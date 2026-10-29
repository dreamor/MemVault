use std::path::Path;
use std::sync::Arc;

pub(crate) mod format;

use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

use chrono::Utc;

use crate::agent_adapt;
use crate::auth::{AgentAuth, AgentCredentials, hash_key};
use crate::config::default_agent_registry;
use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::hybrid::HybridMerger;
use crate::intent::{self, Intent};
use crate::models::*;
use crate::rerank::{MultiSignalReranker, RerankConfig};
use crate::storage::MemoryStore;

/// What `session_start` injected, plus every candidate dropped on the way
/// and why. Injection decisions must be explainable — a memory silently
/// missing from an agent's context is the exact failure this prevents.
#[derive(Debug, Clone, Default)]
pub struct SessionInjection {
    pub results: Vec<SearchResult>,
    pub skipped: Vec<SkippedMemory>,
}

/// Whether write paths should record `Memory::identity_verified` (reads
/// `MEMVAULT_IDENTITY_VERIFICATION`, default on — recording this signal
/// never by itself changes `is_trusted` output, only `MEMVAULT_CORROBORATION_GATE`
/// does, so it is safe to leave enabled by default).
pub fn identity_verification_enabled() -> bool {
    crate::env_file::parse_bool(
        &std::env::var("MEMVAULT_IDENTITY_VERIFICATION").unwrap_or_default(),
    )
    .unwrap_or(true)
}

/// Hard cap on non-MUST lessons injected per session. A project with a long
/// failure history must not drown the working context in past mistakes — the
/// best-matching lessons win, the rest are reported as skipped.
pub const MAX_LESSONS_PER_INJECTION: usize = 3;

/// Fixed relevance score for a lesson explicitly matched by task_type. Sits
/// above the score floor and mid-range, so a matched lesson competes with
/// ordinary REFERENCE memories but does not outrank strongly matching ones.
const LESSON_MATCH_SCORE: f64 = 0.55;

/// Lessons are stored as ordinary memories tagged `lesson` (see
/// `crate::reflection`), so injection recognizes them by tag.
pub fn is_lesson(memory: &Memory) -> bool {
    memory.tags.iter().any(|t| t == "lesson")
}

/// Hard cap on skills injected per session. Procedures are long (steps +
/// verification); more than a couple would crowd out the working context.
pub const MAX_SKILLS_PER_INJECTION: usize = 2;

/// Phase D team shared pool: max shared memories merged into a session and
/// the score they enter with (mid-range — relevant, but not dominating).
const SHARED_POOL_MAX: usize = 20;
const SHARED_MATCH_SCORE: f64 = 0.5;

/// Upper bound on how many MUST memories the completeness fetch below will
/// pull in. Every earlier step in this pipeline (search top_k, hybrid merge
/// top_k) caps candidates at `max_memories * small constant`, so a project
/// with more MUST rules than that would otherwise never even reach the
/// MUST exemptions applied later against `max_memories`/token budget. Set
/// generously high rather than tied to `max_memories` — MUST completeness
/// must not itself become just another instance of the bug it exists to fix.
const MUST_COMPLETENESS_FETCH_LIMIT: usize = 500;

/// Fixed relevance score for a skill explicitly matched by trigger. Slightly
/// above the lesson score: a matched procedure is directly actionable.
const SKILL_MATCH_SCORE: f64 = 0.6;

/// A skill is a Skill-typed memory carrying `skill_meta` (trigger/steps/
/// verification). Type alone is not enough — a skill without meta has
/// nothing structured to inject.
pub fn is_skill(memory: &Memory) -> bool {
    memory.memory_type == MemoryType::Skill && memory.skill_meta.is_some()
}

/// Whether a memory sits on the archive layer (L0: raw transcript evidence,
/// demoted/archived rows).
///
/// L0 never participates in ANY automatic exposure — injection body,
/// overflow summaries, the deterministic MUST fast path, or MCP resources —
/// regardless of priority, review state, or the strict-MUST gate. Priority
/// and gating are orthogonal dimensions: a MUST row demoted to L0 is
/// archived material and must not resurface as a live instruction just
/// because it is mandatory or reviewed (issue #2). Every exposure path must
/// consult this single predicate so the rule cannot drift apart again.
pub fn is_archived(memory: &Memory) -> bool {
    memory.layer == MemoryLayer::L0
}

/// Normalize a caller-supplied project identifier into a project namespace.
///
/// Callers are inconsistent: the proxy's `SessionContext::get_project()`
/// already returns a `"project:*"`-prefixed namespace, while other call sites
/// pass the bare project name. Prefix exactly once so neither form produces a
/// malformed `"project:project:*"` namespace, then canonicalize the project
/// id (lowercase, trailing slashes/whitespace stripped — see
/// [`canonical_project_namespace`]) so a session running in `MemVault` and a
/// `save --namespace project:memvault` resolve to the same namespace instead
/// of silently missing each other.
pub fn project_namespace(project: Option<&str>) -> Option<String> {
    project.map(|p| {
        let prefixed = if p.starts_with("project:") {
            p.to_string()
        } else {
            format!("project:{}", p)
        };
        crate::models::canonical_project_namespace(&prefixed).into_owned()
    })
}

/// The namespace an injection session reads from: the project namespace when
/// one is active, otherwise the profile's first concrete (non-`project:*`)
/// filter entry, otherwise unfiltered. Single source of truth — the body
/// search in `session_start` and the overflow sweep in
/// `session_start_layered` MUST resolve it identically, or the overflow
/// channel re-widens the scope the body just narrowed (the leak that let
/// `trace:*` raw evidence and foreign-project memories surface as
/// `[MORE - 摘要]` for profiles that never allowed them).
fn resolved_injection_namespace(profile: &AgentProfile, project: Option<&str>) -> Option<String> {
    project_namespace(project).or_else(|| {
        profile
            .inject_rules
            .namespace_filter
            .first()
            .filter(|ns| *ns != "project:*")
            .cloned()
    })
}

/// True when the resolved namespace is project-scoped — the case where
/// `session_start`'s cross-namespace fallback supplements the body from
/// `global`, and hence where the layered overflow sweep must consult
/// `global` too (so budget-trimmed global candidates stay reachable as
/// summaries instead of silently narrowing the overflow universe).
fn project_scoped(namespace: Option<&str>) -> bool {
    namespace.is_some_and(|ns| ns != "global" && ns != "project:*")
}

pub struct MemoryRouter {
    store: Arc<dyn MemoryStore>,
    registry: Vec<AgentProfile>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    backfill_guard: Arc<AtomicBool>,
    auth: AgentAuth,
    reranker: MultiSignalReranker,
}

impl MemoryRouter {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        let registry = default_agent_registry();
        let auth = AgentAuth::from_profiles(&registry);
        Self {
            store,
            registry,
            embedder: None,
            backfill_guard: Arc::new(AtomicBool::new(false)),
            auth,
            reranker: MultiSignalReranker::new(RerankConfig::default()),
        }
    }

    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    pub fn with_registry(store: Arc<dyn MemoryStore>, mut registry: Vec<AgentProfile>) -> Self {
        if !registry.iter().any(|a| a.id == "default") {
            registry.push(AgentProfile {
                id: "default".to_string(),
                agent_type: "general-assistant".to_string(),
                description: "Default agent profile".to_string(),
                inject_rules: InjectRules::default(),
                api_key: None,
                inject_channel: None,
            });
        }
        // Hash agent API keys in place (once) so plaintext never lingers in
        // memory; the auth store then holds the already-hashed value. Double
        // hashing (from_profiles hashes the key again) would make the agent's
        // configured plaintext key fail authentication everywhere.
        for agent in &mut registry {
            if let Some(ref key) = agent.api_key.clone()
                && !key.is_empty()
            {
                agent.api_key = Some(hash_key(key));
            }
        }
        let auth = AgentAuth::from_hashed_profiles(&registry);
        Self {
            store,
            registry,
            embedder: None,
            backfill_guard: Arc::new(AtomicBool::new(false)),
            auth,
            reranker: MultiSignalReranker::new(RerankConfig::default()),
        }
    }

    pub fn load_registry_from_yaml(store: Arc<dyn MemoryStore>, path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::error::MemVaultError::Storage(format!("Failed to read agent registry: {}", e))
        })?;

        let config: AgentRegistryConfig = serde_yaml::from_str(&content).map_err(|e| {
            crate::error::MemVaultError::InvalidInput(format!("Invalid agent registry YAML: {}", e))
        })?;

        info!(
            "Loaded {} agent profiles from {}",
            config.agents.len(),
            path.display()
        );
        Ok(Self::with_registry(store, config.agents))
    }

    /// All registered agent profiles (dashboard/ops visibility). `api_key`
    /// is whatever's in-memory (a SHA-256 hash once loaded from YAML, never
    /// plaintext) — callers displaying this outside a trusted process
    /// should still redact it before rendering.
    pub fn list_agent_profiles(&self) -> Vec<AgentProfile> {
        self.registry.clone()
    }

    pub fn get_agent_profile(&self, agent_id: &str) -> AgentProfile {
        // 1. Exact match in registry
        if let Some(p) = self.registry.iter().find(|a| a.id == agent_id) {
            return p.clone();
        }

        // 2. Partial type match
        let agent_type = agent_id.split('-').next().unwrap_or("");
        if let Some(p) = self
            .registry
            .iter()
            .find(|a| a.agent_type.contains(agent_type))
        {
            return p.clone();
        }

        // 3. Auto-detect from fingerprints
        if let Some(p) = agent_adapt::identify_agent(agent_id, None) {
            return p;
        }

        // 4. Default
        self.registry
            .iter()
            .find(|a| a.id == "default")
            .cloned()
            .unwrap_or_else(|| AgentProfile {
                id: "default".to_string(),
                agent_type: "general-assistant".to_string(),
                description: "Default".to_string(),
                inject_rules: InjectRules::default(),
                api_key: None,
                inject_channel: None,
            })
    }

    /// The canonical injection channel configured for an agent, if any —
    /// Feature F (docs/DESIGN.md §17.5). `None` means the agent placed
    /// no restriction, so every channel may inject (pre-Feature-F behavior).
    pub fn inject_channel_for(&self, agent_id: &str) -> Option<InjectChannel> {
        self.get_agent_profile(agent_id).inject_channel
    }

    /// Whether `channel` is allowed to inject for `agent_id`. An agent with no
    /// configured [`InjectChannel`] allows every channel; otherwise only the
    /// canonical one does. Callers on a non-canonical channel must skip
    /// automatic injection so the same memory is not delivered twice.
    pub fn channel_allows(&self, agent_id: &str, channel: InjectChannel) -> bool {
        match self.inject_channel_for(agent_id) {
            None => true,
            Some(canonical) => canonical == channel,
        }
    }

    /// Spawn a background task that finds memories without embedding
    /// and generates them asynchronously. Uses a guard to prevent concurrent backfill runs.
    pub fn spawn_embedding_backfill(
        store: Arc<dyn MemoryStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        running: Arc<AtomicBool>,
    ) {
        if running.swap(true, Ordering::Relaxed) {
            debug!("embedding backfill already in progress, skipping");
            return;
        }

        tokio::spawn(async move {
            debug!("starting embedding backfill");
            match Self::do_backfill(&*store, &*embedder).await {
                Ok(count) => {
                    if count > 0 {
                        info!(count, "embedding backfill complete");
                    }
                }
                Err(e) => warn!(error = %e, "embedding backfill failed"),
            }
            running.store(false, Ordering::Relaxed);
        });
    }

    async fn do_backfill(
        store: &dyn MemoryStore,
        embedder: &dyn EmbeddingProvider,
    ) -> Result<usize> {
        let candidates = store.list_without_embedding(50).await?;
        if candidates.is_empty() {
            return Ok(0);
        }

        let texts: Vec<String> = candidates
            .iter()
            .map(|m| {
                let inst = m.instruction.as_deref().unwrap_or("");
                if inst.is_empty() {
                    m.content.clone()
                } else {
                    format!("{} — {}", inst, m.content)
                }
            })
            .collect();

        let embeddings = embedder.embed(&texts).await?;

        for (mem, emb) in candidates.into_iter().zip(embeddings) {
            if let Err(e) = store.set_embedding(&mem.id, emb).await {
                warn!(id = %mem.id, error = %e, "failed to set backfill embedding");
            }
        }

        Ok(texts.len())
    }

    /// Synchronously drain every vectorless memory, not just one batch —
    /// for one-shot callers (`memvault-cli backfill-embeddings`) that need a
    /// final count rather than a fire-and-forget background task. Covers a
    /// SQLite-backend gap the background `spawn_embedding_backfill` doesn't:
    /// there was no way to force a backfill on demand (e.g. after a JSON/
    /// Markdown import that carried no vectors, or after enabling the
    /// embedder for a store that was previously keyword-only).
    pub async fn backfill_all_embeddings(
        store: &dyn MemoryStore,
        embedder: &dyn EmbeddingProvider,
    ) -> Result<usize> {
        let mut total = 0;
        loop {
            let count = Self::do_backfill(store, embedder).await?;
            if count == 0 {
                break;
            }
            total += count;
        }
        Ok(total)
    }

    /// Identify agent with optional client_info (from MCP handshake).
    pub fn get_agent_profile_with_client_info(
        &self,
        agent_id: &str,
        client_info: Option<&str>,
    ) -> AgentProfile {
        // Try registry first
        if let Some(p) = self.registry.iter().find(|a| a.id == agent_id) {
            return p.clone();
        }

        // Try fingerprint with client_info
        if let Some(p) = agent_adapt::identify_agent(agent_id, client_info) {
            return p;
        }

        self.get_agent_profile(agent_id)
    }

    /// Authenticate an agent by verifying its credentials.
    ///
    /// Returns the agent's profile on success.
    /// If the agent has no registered key, unauthenticated access is allowed.
    pub fn authenticate_agent(
        &self,
        agent_id: &str,
        api_key: Option<&str>,
    ) -> std::result::Result<AgentProfile, crate::error::MemVaultError> {
        let creds = api_key.map(|k| AgentCredentials::new(agent_id, k));
        self.auth.authenticate(agent_id, creds.as_ref())?;
        Ok(self.get_agent_profile(agent_id))
    }

    /// Same as [`Router::authenticate_agent`], plus whether `agent_id` had a
    /// registered API key that was actually checked (as opposed to running
    /// in unauthenticated mode, where any caller-supplied `agent_id` is
    /// accepted unchallenged). Write paths use the returned bool to stamp
    /// `Memory::identity_verified`, which feeds the MUST corroboration gate
    /// in `router::format::is_trusted`.
    pub fn authenticate_agent_verified(
        &self,
        agent_id: &str,
        api_key: Option<&str>,
    ) -> std::result::Result<(AgentProfile, bool), crate::error::MemVaultError> {
        let profile = self.authenticate_agent(agent_id, api_key)?;
        let verified = identity_verification_enabled() && self.auth.requires_auth(agent_id);
        Ok((profile, verified))
    }

    /// Whether any agent in the registry has a registered API key. When this
    /// is false the whole deployment runs in the documented unauthenticated
    /// local mode; once it is true, admin routes must fail closed.
    pub fn has_registered_keys(&self) -> bool {
        self.auth.registered_count() > 0
    }

    /// Authentication for admin/operational routes.
    ///
    /// Stricter than [`Self::authenticate_agent`]: the plain method treats an
    /// agent id with no registered key as "unauthenticated mode, allowed" —
    /// which is fine when *nobody* has a key, but once any key is configured
    /// it lets any caller bypass the admin key by inventing an agent id (the
    /// id arrives from the request: `X-MemVault-Agent-Id` on REST, the tool
    /// params on MCP). Once at least one key exists, an admin caller must name
    /// a registered, keyed agent and present its key.
    pub fn authenticate_admin(
        &self,
        agent_id: &str,
        api_key: Option<&str>,
    ) -> std::result::Result<AgentProfile, crate::error::MemVaultError> {
        if self.has_registered_keys() && !self.auth.requires_auth(agent_id) {
            return Err(crate::error::MemVaultError::Auth(format!(
                "agent '{agent_id}' is not registered with an api_key; once any agent \
                 key is configured, admin endpoints require a registered agent id and key"
            )));
        }
        self.authenticate_agent(agent_id, api_key)
    }

    pub async fn session_start(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
    ) -> Result<SessionInjection> {
        self.session_start_gated(
            agent_id,
            context_hint,
            project,
            format::require_review_for_must(),
        )
        .await
    }

    /// [`session_start`] with the strict-MUST-gate flag passed explicitly
    /// (rather than read from env) so tests can exercise the gated selection
    /// deterministically — same rationale as the `_with` helpers in
    /// `router::format`.
    async fn session_start_gated(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
        strict_must: bool,
    ) -> Result<SessionInjection> {
        let profile = self.get_agent_profile(agent_id);
        debug!(agent_id, agent_type = %profile.agent_type, "session_start");

        let intent =
            context_hint
                .map(intent::analyze_intent)
                .unwrap_or_else(|| intent::IntentResult {
                    primary: Intent::General,
                    domains: vec!["general".to_string()],
                    confidence: 0.5,
                });
        debug!(intent = ?intent.primary, confidence = intent.confidence, "intent analyzed");

        let namespace = resolved_injection_namespace(&profile, project);

        let query = SearchQuery {
            query: String::new(),
            agent_id: Some(agent_id.to_string()),
            namespace: namespace.clone(),
            top_k: profile.inject_rules.max_memories * 2,
            ..SearchQuery::new(String::new())
        };

        let mut results = self.store.search(query).await?.results;

        // If embedder is available and there's a context hint, do hybrid search
        if let (Some(embedder), Some(hint)) = (&self.embedder, context_hint)
            && !hint.is_empty()
        {
            match embedder.embed(&[hint.to_string()]).await {
                Ok(embeddings) if !embeddings.is_empty() => {
                    let vector_results = self
                        .store
                        .vector_search(
                            &embeddings[0],
                            profile.inject_rules.max_memories * 2,
                            namespace.as_deref(),
                        )
                        .await?;

                    debug!(
                        keyword = results.len(),
                        vector = vector_results.len(),
                        "merging hybrid results"
                    );

                    results = HybridMerger::merge(
                        results,
                        vector_results,
                        profile.inject_rules.max_memories * 2,
                        0.4,
                        0.6,
                    );
                }
                Err(e) => {
                    warn!("Embedding failed, falling back to keyword search: {}", e);
                }
                _ => {}
            }
        }

        // Rerank results using multi-signal scoring
        let query_str = context_hint.unwrap_or("");
        results = self.reranker.rerank(query_str, results, &Utc::now());

        let mut skipped: Vec<SkippedMemory> = Vec::new();
        // Penalty attribution: when a penalized candidate later falls below
        // the score floor, the skip reason names the penalty that caused it,
        // not the generic floor.
        let mut type_penalized: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut intent_penalized: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // filter by agent's exclude_types (checks both memory_type and tags)
        // MUST memories are never excluded; non-MUST get score penalty instead of hard exclude
        if !profile.inject_rules.exclude_types.is_empty() {
            for r in results.iter_mut() {
                if r.memory.priority == Priority::Must {
                    continue;
                }
                let type_str = serde_json::to_string(&r.memory.memory_type).unwrap_or_default();
                let type_str = type_str.trim_matches('"');
                let mut penalty = false;
                if profile
                    .inject_rules
                    .exclude_types
                    .iter()
                    .any(|et| et.eq_ignore_ascii_case(type_str))
                {
                    penalty = true;
                }
                if !penalty {
                    for tag in &r.memory.tags {
                        if profile
                            .inject_rules
                            .exclude_types
                            .iter()
                            .any(|et| et.eq_ignore_ascii_case(tag))
                        {
                            penalty = true;
                            break;
                        }
                    }
                }
                if penalty {
                    r.score *= 0.3; // soft penalty instead of hard exclude
                    type_penalized.insert(r.memory.id.clone());
                }
            }
        }

        // filter by intent (soft penalty instead of hard exclude)
        if intent.primary != Intent::General {
            for r in results.iter_mut() {
                if r.memory.priority == Priority::Must {
                    continue;
                }
                if intent::should_exclude_for_intent(
                    &intent.primary,
                    &r.memory.tags,
                    &profile.inject_rules.exclude_types,
                ) {
                    r.score *= 0.4;
                    intent_penalized.insert(r.memory.id.clone());
                }
            }

            // Positive complement to the exclude-only penalty above: boost
            // the memory type this task usually needs (e.g. Coding wants
            // Skill/Fact ranked above transient Episode context), instead of
            // relying purely on relevance score to get the type mix right.
            for r in results.iter_mut() {
                if r.memory.priority == Priority::Must {
                    continue;
                }
                r.score *= intent::intent_type_boost(&intent.primary, &r.memory.memory_type);
            }
        }

        // Episodic lessons: pull in lessons whose task_type matches the
        // session context, regardless of whether the generic search ranked
        // them — "about to do X" is exactly when X's failure lessons matter.
        // Procedural skills: likewise pull in skills whose trigger matches —
        // "about to do X" is exactly when X's procedure matters. A skill
        // already present from the generic search gets REPLACED by the
        // trigger-matched copy, which carries the structured procedure block
        // (the generic copy has none).
        if let Some(hint) = context_hint
            && !hint.is_empty()
        {
            let mut existing_ids: std::collections::HashSet<String> =
                results.iter().map(|r| r.memory.id.clone()).collect();
            let lessons = self.matching_lessons(hint, namespace.as_deref()).await;
            for lesson in lessons {
                if !existing_ids.contains(&lesson.memory.id) {
                    existing_ids.insert(lesson.memory.id.clone());
                    results.push(lesson);
                }
            }
            let skills = self.matching_skills(hint, namespace.as_deref()).await;
            for skill in skills {
                if existing_ids.contains(&skill.memory.id) {
                    if let Some(slot) = results.iter_mut().find(|r| r.memory.id == skill.memory.id)
                    {
                        *slot = skill;
                    }
                } else {
                    existing_ids.insert(skill.memory.id.clone());
                    results.push(skill);
                }
            }
        }

        // Phase D team shared pool: memories marked `visibility='shared'` are
        // injected into every session regardless of namespace.
        {
            let mut existing_ids: std::collections::HashSet<String> =
                results.iter().map(|r| r.memory.id.clone()).collect();
            if let Ok(shared) = self.store.list_shared(SHARED_POOL_MAX).await {
                for mem in shared {
                    if existing_ids.insert(mem.id.clone()) {
                        results.push(SearchResult {
                            memory: mem,
                            score: SHARED_MATCH_SCORE,
                            hit_sources: Vec::new(),
                        });
                    }
                }
            }
        }

        // MUST completeness: union in the full MUST set directly, bypassing
        // every top_k cap the fetch/hybrid steps above applied for their own
        // reasons (none of them know "MUST must never be missing"). Without
        // this, a project with more MUST rules than those caps allow would
        // silently lose some before the MUST exemptions below ever run.
        {
            let must_query = SearchQuery {
                query: String::new(),
                agent_id: Some(agent_id.to_string()),
                namespace: namespace.clone(),
                priority_filter: Some(Priority::Must),
                top_k: MUST_COMPLETENESS_FETCH_LIMIT,
                ..SearchQuery::new(String::new())
            };
            if let Ok(outcome) = self.store.search(must_query).await {
                let existing_ids: std::collections::HashSet<String> =
                    results.iter().map(|r| r.memory.id.clone()).collect();
                for r in outcome.results {
                    if !existing_ids.contains(&r.memory.id) {
                        results.push(r);
                    }
                }
            }
        }

        // re-sort after score adjustments
        results.sort_by(|a, b| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        // remove extremely low scoring results — each drop gets a reason,
        // attributed to the penalty that caused it when one applied
        let mut kept: Vec<SearchResult> = Vec::with_capacity(results.len());
        for r in results.drain(..) {
            if r.memory.priority == Priority::Must || r.score > 0.05 {
                kept.push(r);
            } else {
                let reason = if intent_penalized.contains(&r.memory.id) {
                    InjectSkipReason::IntentFiltered
                } else if type_penalized.contains(&r.memory.id) {
                    InjectSkipReason::TypeExcluded
                } else {
                    InjectSkipReason::BelowScoreFloor
                };
                skipped.push(SkippedMemory {
                    id: r.memory.id,
                    reason,
                });
            }
        }
        results = kept;

        // Cross-namespace fallback: if project namespace has few results, supplement from global
        if project_scoped(namespace.as_deref()) {
            let non_must_count = results
                .iter()
                .filter(|r| r.memory.priority != Priority::Must)
                .count();
            if non_must_count < profile.inject_rules.max_memories / 2 {
                let global_query = SearchQuery {
                    query: String::new(),
                    agent_id: Some(agent_id.to_string()),
                    namespace: Some("global".to_string()),
                    top_k: profile.inject_rules.max_memories,
                    ..SearchQuery::new(String::new())
                };
                let global_results = self.store.search(global_query).await?.results;
                let existing_ids: std::collections::HashSet<String> =
                    results.iter().map(|r| r.memory.id.clone()).collect();
                for gr in global_results {
                    if !existing_ids.contains(&gr.memory.id) {
                        results.push(gr);
                    }
                }
                debug!(
                    supplemented = results.len(),
                    "cross-namespace fallback applied"
                );
            }
        }

        // Archive-layer exclusion (issue #2): L0 rows (raw evidence,
        // demoted/archived memories) never enter the injected body, whatever
        // their priority or review state. The MUST-completeness union above
        // deliberately bypasses the top_k caps and can pull in L0 MUST rows;
        // lessons/skills fetchers skip L0 themselves. This is the single
        // choke point where every merged source is present — the same rule
        // `overflow_admits` enforces for the summary channel. Dropped
        // silently, consistent with the other L0 paths: archived material is
        // not a "skipped candidate", it is not injectable at all.
        results.retain(|r| !is_archived(&r.memory));

        // Lesson + skill quotas: a long failure history or a big skill
        // library must not crowd out the working context. Explicitly matched
        // items (task_type / trigger hits) reserve quota slots FIRST — a
        // matched procedure must never be bumped by items that merely
        // floated in through the generic search; leftovers fill remaining
        // slots in score order. MUST lessons are exempt — mandatory rules
        // never yield to a quota. Every dropped candidate is reported.
        fn is_explicit(r: &SearchResult) -> bool {
            r.hit_sources
                .iter()
                .any(|h| matches!(h, HitSource::ExplicitMatch))
        }
        fn quota_class(r: &SearchResult) -> Option<&'static str> {
            if is_lesson(&r.memory) && r.memory.priority != Priority::Must {
                Some("lesson")
            } else if is_skill(&r.memory) {
                Some("skill")
            } else {
                None
            }
        }

        let mut lesson_slots = MAX_LESSONS_PER_INJECTION;
        let mut skill_slots = MAX_SKILLS_PER_INJECTION;
        let mut drop: std::collections::HashMap<String, InjectSkipReason> =
            std::collections::HashMap::new();

        for pass in [true, false] {
            for r in results.iter() {
                let Some(class) = quota_class(r) else {
                    continue;
                };
                if is_explicit(r) != pass || drop.contains_key(&r.memory.id) {
                    continue;
                }
                let slots = if class == "lesson" {
                    &mut lesson_slots
                } else {
                    &mut skill_slots
                };
                if *slots > 0 {
                    *slots -= 1;
                } else {
                    drop.insert(
                        r.memory.id.clone(),
                        if class == "lesson" {
                            InjectSkipReason::LessonQuotaExceeded
                        } else {
                            InjectSkipReason::SkillQuotaExceeded
                        },
                    );
                }
            }
        }

        if !drop.is_empty() {
            for (id, reason) in &drop {
                skipped.push(SkippedMemory {
                    id: id.clone(),
                    reason: *reason,
                });
            }
            results.retain(|r| !drop.contains_key(&r.memory.id));
        }

        // trim to token budget — the cut tail is reported, not dropped
        let before_trim = results.len();
        let budget_cut = Self::trim_to_budget(&mut results, profile.inject_rules.token_budget);
        for r in budget_cut {
            skipped.push(SkippedMemory {
                id: r.memory.id,
                reason: InjectSkipReason::TokenBudgetExceeded,
            });
        }

        // final cap on count — likewise reported. MUST is exempt: a count
        // cap must not silently drop mandatory rules just because there
        // happen to be more of them than `max_memories` — the same
        // guarantee `trim_to_budget` already gives MUST against the token
        // budget above. Partitioning by priority (rather than trusting
        // position) is deliberate: the cross-namespace fallback above can
        // append a MUST result after the earlier MUST-first sort, so a
        // positional `split_off` is not safe here.
        if results.len() > profile.inject_rules.max_memories {
            let must_count = results
                .iter()
                .filter(|r| r.memory.priority == Priority::Must)
                .count();
            let non_must_budget = profile.inject_rules.max_memories.saturating_sub(must_count);
            let mut kept = Vec::with_capacity(results.len());
            let mut non_must_kept = 0usize;
            for r in results.drain(..) {
                if r.memory.priority == Priority::Must {
                    kept.push(r);
                } else if non_must_kept < non_must_budget {
                    non_must_kept += 1;
                    kept.push(r);
                } else {
                    skipped.push(SkippedMemory {
                        id: r.memory.id,
                        reason: InjectSkipReason::MaxMemoriesExceeded,
                    });
                }
            }
            results = kept;
        }

        // Strict MUST gate (`MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST`):
        // applied at SELECTION time so the layered overflow channel, the
        // compliance `record_injection` calls in MCP/REST, and the injected
        // counts all agree with what the body renderer will actually show.
        // The render-time filter in `format_as_instructions_with` stays as
        // defense in depth.
        format::apply_strict_must_gate(&mut results, &mut skipped, strict_must);

        // passive tracking: record access for injected memories
        let result_ids: Vec<String> = results.iter().map(|r| r.memory.id.clone()).collect();
        if let Err(e) = self.store.record_access(&result_ids).await {
            warn!(error = %e, "failed to record access for session_start results");
        }

        // Skill exposure tracking: count every skill that actually entered
        // the injected context (drives the success-rate denominator). Only
        // injected skills count — a quota-skipped skill was never seen.
        for r in results.iter().filter(|r| is_skill(&r.memory)) {
            if let Err(e) = self.store.record_skill_injection(&r.memory.id).await {
                warn!(skill_id = %r.memory.id, error = %e, "failed to record skill injection");
            }
        }

        // background: auto-backfill missing embeddings
        if let Some(ref embedder) = self.embedder {
            Self::spawn_embedding_backfill(
                self.store.clone(),
                embedder.clone(),
                self.backfill_guard.clone(),
            );
        }

        let must_count = results
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .count();
        let ref_count = results.len() - must_count;
        debug!(
            agent_id,
            before_filter = before_trim,
            after = results.len(),
            must = must_count,
            r#ref = ref_count,
            token_budget = profile.inject_rules.token_budget,
            skipped = skipped.len(),
            "session_start complete"
        );

        Ok(SessionInjection { results, skipped })
    }

    /// Deterministic (zero-embedding-call) injection baseline — Feature C
    /// (docs/DESIGN.md §17.3): the agent-visible MUST-level memories,
    /// resolved by pure rules (priority + namespace), no semantic search.
    ///
    /// Deterministic addressing is exactly what makes this safe to serve
    /// synchronously as a fast path while the semantic pipeline is still
    /// prefetching in the background (paper §2.3: deterministic addressing
    /// is what enables host-memory offloading + async prefetch).
    pub async fn deterministic_injection(
        &self,
        agent_id: &str,
        project: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let profile = self.get_agent_profile(agent_id);

        // MUST rules are mandatory, and the full pipeline supplements from
        // `global` whenever a project namespace is active (cross-namespace
        // fallback in `session_start`). Mirror that here so the fast path
        // never drops a MUST rule the full path would have injected: scan the
        // resolved project namespace (when any) plus `global`, dedup by id.
        let mut namespaces: Vec<String> = Vec::new();
        if let Some(ns) = project_namespace(project) {
            namespaces.push(ns);
        } else if let Some(first) = profile
            .inject_rules
            .namespace_filter
            .first()
            .filter(|ns| *ns != "project:*" && *ns != "global")
        {
            namespaces.push(first.clone());
        }
        namespaces.push("global".to_string());

        let mut out: Vec<SearchResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ns in namespaces {
            let query = SearchQuery {
                query: String::new(),
                agent_id: Some(agent_id.to_string()),
                priority_filter: Some(Priority::Must),
                namespace: Some(ns),
                top_k: profile.inject_rules.max_memories,
                token_budget: Some(profile.inject_rules.token_budget),
                ..SearchQuery::new(String::new())
            };
            for r in self.store.search(query).await?.results {
                if seen.insert(r.memory.id.clone()) {
                    out.push(r);
                }
            }
        }
        // Archive-layer exclusion (issue #2), same rule as the full
        // session_start body: a MUST row demoted to L0 is archived material
        // — MUST priority alone must not resurface it on the fast path.
        out.retain(|r| !is_archived(&r.memory));
        // The strict MUST gate applies to the fast path as well. The proxy
        // records `injected_memory_ids` from this set for compliance — a
        // gated candidate left in here would be hidden from the body by the
        // render-time filter while the audit trail still claimed it was
        // injected. This query only fetches MUST rows, so dropping every
        // untrusted one is exactly the gate.
        if format::require_review_for_must() {
            out.retain(|r| format::is_trusted(&r.memory));
        }
        Ok(out)
    }

    pub async fn confirm_read(&self, ids: &[String]) -> Result<usize> {
        self.store.record_access(ids).await
    }

    /// Find lessons whose episode task_type matches the session context.
    ///
    /// The episodes table is the source of truth for task_type (lesson
    /// memories only carry it as one tag among several); a lesson qualifies
    /// when its episode's task_type appears in the context text. Checks the
    /// session namespace first, then `global` — lessons are most often
    /// recorded globally while sessions run in project namespaces.
    async fn matching_lessons(
        &self,
        context_hint: &str,
        namespace: Option<&str>,
    ) -> Vec<SearchResult> {
        let context_lower = context_hint.to_lowercase();

        let mut namespaces: Vec<Option<String>> = vec![namespace.map(str::to_string)];
        if namespace != Some("global") {
            namespaces.push(Some("global".to_string()));
        }

        let mut out: Vec<SearchResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for ns in namespaces {
            let filter = EpisodeFilter {
                namespace: ns.clone(),
                limit: 100,
                ..Default::default()
            };
            let episodes = match self.store.list_episodes(filter).await {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "failed to list episodes for lesson matching");
                    continue;
                }
            };
            for ep in episodes {
                let Some(ref lesson_id) = ep.lesson_memory_id else {
                    continue;
                };
                let Some(ref task_type) = ep.task_type else {
                    continue;
                };
                if seen.contains(lesson_id) {
                    continue;
                }
                if !context_lower.contains(&task_type.to_lowercase()) {
                    continue;
                }
                match self.store.get(lesson_id).await {
                    Ok(memory) => {
                        // A superseded or demoted-to-L0 lesson no longer
                        // represents current knowledge — skip quietly.
                        if memory.superseded_by.is_some() || memory.layer == MemoryLayer::L0 {
                            continue;
                        }
                        seen.insert(lesson_id.clone());
                        out.push(SearchResult {
                            memory,
                            score: LESSON_MATCH_SCORE,
                            hit_sources: vec![HitSource::ExplicitMatch],
                        });
                        debug!(
                            lesson_id = %lesson_id,
                            task_type = %task_type,
                            "matched lesson for session context"
                        );
                    }
                    Err(e) => {
                        warn!(lesson_id = %lesson_id, error = %e, "lesson memory vanished; episode backlink stale");
                    }
                }
            }
        }
        out
    }

    /// Find skills whose trigger matches the session context, across the
    /// session namespace and `global`. Each match is returned with its
    /// structured procedure already rendered into `instruction` (so the
    /// generic formatting pipeline injects the full steps), scored at a
    /// fixed [`SKILL_MATCH_SCORE`]. Superseded or archived (L0) skills are
    /// skipped, as are skills without a trigger (nothing to match on).
    async fn matching_skills(
        &self,
        context_hint: &str,
        namespace: Option<&str>,
    ) -> Vec<SearchResult> {
        let mut namespaces: Vec<Option<String>> = vec![namespace.map(str::to_string)];
        if namespace != Some("global") {
            namespaces.push(Some("global".to_string()));
        }

        let mut out: Vec<SearchResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for ns in namespaces {
            let query = SearchQuery {
                query: String::new(),
                namespace: ns.clone(),
                type_filter: Some(MemoryType::Skill),
                top_k: 100,
                ..SearchQuery::new(String::new())
            };
            let candidates = match self.store.search(query).await {
                Ok(o) => o.results,
                Err(e) => {
                    warn!(error = %e, "failed to list skills for trigger matching");
                    continue;
                }
            };
            for r in candidates {
                let mut mem = r.memory;
                if seen.contains(&mem.id) {
                    continue;
                }
                if mem.superseded_by.is_some() || mem.layer == MemoryLayer::L0 {
                    continue;
                }
                let Some(ref meta) = mem.skill_meta else {
                    continue;
                };
                let Some(ref trigger) = meta.trigger else {
                    continue;
                };
                if !intent::trigger_matches_context(trigger, context_hint) {
                    continue;
                }
                seen.insert(mem.id.clone());

                // Render the structured block into `instruction`, lift a
                // BACKGROUND skill to REFERENCE so it reads as guidance, and
                // fetch stats for the success-rate display.
                let stats = self.store.get_skill_stats(&mem.id).await.ok().flatten();
                mem.instruction = Some(format::format_skill_block(&mem, stats.as_ref()));
                if mem.priority == Priority::Background {
                    mem.priority = Priority::Reference;
                }

                debug!(
                    skill_id = %mem.id,
                    trigger = %trigger,
                    "matched skill for session context"
                );
                out.push(SearchResult {
                    memory: mem,
                    score: SKILL_MATCH_SCORE,
                    hit_sources: vec![HitSource::ExplicitMatch],
                });
            }
        }
        out
    }

    /// Layered session start: returns full injection for MUST/high-priority memories,
    /// and summaries for overflow memories that exceed token budget.
    pub async fn session_start_layered(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
    ) -> Result<SessionStartOutput> {
        self.session_start_layered_gated(
            agent_id,
            context_hint,
            project,
            format::require_review_for_must(),
        )
        .await
    }

    /// [`session_start_layered`] with the strict-MUST-gate flag passed
    /// explicitly, so tests can exercise both the gated selection and the
    /// gated overflow channel without mutating process-global env.
    async fn session_start_layered_gated(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
        strict_must: bool,
    ) -> Result<SessionStartOutput> {
        let profile = self.get_agent_profile(agent_id);

        // Fetch more candidates than needed for layered selection
        let injection = self
            .session_start_gated(agent_id, context_hint, project, strict_must)
            .await?;
        let all_results = injection.results;

        // Also fetch overflow candidates that were trimmed — from the SAME
        // namespace universe the body draws from (`resolved_injection_namespace`,
        // plus a `global` sweep when project-scoped, mirroring the body's
        // cross-namespace fallback). This query used to filter by `project`
        // alone: with no project it swept EVERY namespace in the store, so
        // `trace:*` raw evidence and other projects' memories could surface
        // as `[MORE - 摘要]` even for profiles that never allowed them (a
        // global-only profile like copilot's saw project-scoped summaries).
        let namespace = resolved_injection_namespace(&profile, project);
        let mut sweep: Vec<Option<String>> = vec![namespace.clone()];
        if project_scoped(namespace.as_deref()) {
            sweep.push(Some("global".to_string()));
        }

        let mut extended: Vec<SearchResult> = Vec::new();
        let mut seen_extended: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ns in sweep {
            let extended_query = SearchQuery {
                query: context_hint.unwrap_or("").to_string(),
                agent_id: Some(agent_id.to_string()),
                namespace: ns,
                top_k: profile.inject_rules.max_memories * 3,
                ..SearchQuery::new(String::new())
            };
            for r in self.store.search(extended_query).await?.results {
                if seen_extended.insert(r.memory.id.clone()) {
                    extended.push(r);
                }
            }
        }

        // Injected are what session_start already selected
        let injected_ids: std::collections::HashSet<&str> =
            all_results.iter().map(|r| r.memory.id.as_str()).collect();

        let mut skipped = injection.skipped;
        let already_skipped: std::collections::HashSet<String> =
            skipped.iter().map(|s| s.id.clone()).collect();

        // Overflow: memories that exist, weren't injected, and clear the
        // relevance floor. Candidates below the floor are not injected
        // either, but every dropped candidate must carry a reason — record
        // them as BelowScoreFloor instead of silently vanishing here.
        let mut overflow: Vec<&SearchResult> = Vec::new();
        for r in &extended {
            if injected_ids.contains(r.memory.id.as_str()) {
                continue;
            }
            if !Self::overflow_admits(&r.memory, strict_must) {
                // Not an injection candidate at all (raw L0 evidence, or an
                // unreviewed MUST held back by the strict gate) — excluded
                // from BOTH the summaries and the skip list: a gated MUST
                // already carries `UnreviewedMustGated` from the selection
                // above, and L0 rows are archive material that no injection
                // path surfaces (lessons/skills/episodes skip them too).
                continue;
            }
            if r.score > 0.1 {
                overflow.push(r);
            } else if !already_skipped.contains(&r.memory.id) {
                skipped.push(SkippedMemory {
                    id: r.memory.id.clone(),
                    reason: InjectSkipReason::BelowScoreFloor,
                });
            }
        }

        // The sweeps (session namespace, then `global`) arrive concatenated,
        // so order by relevance before the top-5 summary cut — otherwise a
        // full session-namespace sweep would crowd out stronger global
        // candidates regardless of score.
        overflow.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let overflow_count = overflow.len();
        let overflow_summaries: Vec<String> = overflow
            .iter()
            .take(5)
            .map(|r| Self::make_summary(&r.memory))
            .collect();

        let conflicts = self.collect_conflicts(&all_results).await;

        Ok(SessionStartOutput {
            injected: all_results,
            overflow_count,
            overflow_summaries,
            skipped,
            conflicts,
        })
    }

    /// Whether a non-injected search hit may enter the layered overflow
    /// channel (`[MORE - 摘要]` summaries).
    ///
    /// Two shapes are never admitted:
    /// - **L0 rows** (raw transcript evidence, demoted/archived memories).
    ///   Every other injection path skips L0 deliberately; without this check
    ///   the overflow summary was the one place a raw transcript excerpt —
    ///   quoted or negated sentences included — got auto-injected verbatim
    ///   (a 57-char echo via `make_summary`), on every deployment.
    /// - **Unreviewed MUST candidates when the strict gate
    ///   (`MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST`) is on.** Selection-time
    ///   gating keeps them out of the body, but they are still search
    ///   hits — without this check `make_summary` would echo their raw text
    ///   back as a "summary" and the gate's "not shown at all until
    ///   reviewed" contract would hold for the body only.
    ///
    /// Note the gate alone cannot cover L0 evidence rows: trace ingestion
    /// marks them `human_reviewed = true` ("raw logs need no human review"),
    /// so `is_trusted` says yes — hence the separate layer check.
    fn overflow_admits(memory: &Memory, strict_must: bool) -> bool {
        if is_archived(memory) {
            return false;
        }
        !(strict_must && memory.priority == Priority::Must && !format::is_trusted(memory))
    }

    fn make_summary(memory: &Memory) -> String {
        format::make_summary(memory)
    }

    /// Active `contradicts` pairs among a set of memories about to be
    /// injected — the single detection behind `session_start_layered`
    /// (MCP), [`Self::conflict_warnings`] (CLI session-start) and the REST
    /// `/api/session` handler (which exposes the pairs both structured and
    /// rendered). Both endpoints must be inside `injected`, pairs are
    /// deduplicated regardless of relation direction, and inactive evidence
    /// memories are ignored (all enforced by
    /// [`crate::evidence::contradictions_among`]).
    pub async fn collect_conflicts(&self, injected: &[SearchResult]) -> Vec<ConflictNotice> {
        let injected_ids: Vec<String> = injected.iter().map(|r| r.memory.id.clone()).collect();
        crate::evidence::contradictions_among(&*self.store, &injected_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(memory_id, conflicting_with)| ConflictNotice {
                memory_id,
                conflicting_with,
            })
            .collect()
    }

    /// The `[MEMORY CONFLICT - 需要你决定]` warning block for an injected
    /// set — the CLI `session-start` entry point into the same conflict
    /// capability the MCP layered renderer uses. The CLI used to format
    /// through `format_as_instructions` alone, so the CLI-backed
    /// SessionStart hook (plugins/memvault's `hooks/session-start.sh`
    /// prefers the CLI binary) injected contradicting MUST memories without
    /// the "don't auto-pick" notice MCP already emitted. Returns an empty
    /// string when no conflict pair is fully inside `injected`, so excluded
    /// memories never leak through a warning. Rendering is byte-identical to
    /// the layered block (one shared `format_conflict_block`).
    pub async fn conflict_warnings(&self, injected: &[SearchResult]) -> String {
        self.render_conflicts(injected, &self.collect_conflicts(injected).await)
    }

    /// Render the `[MEMORY CONFLICT]` block for precomputed conflict pairs —
    /// the companion to [`Self::collect_conflicts`] for callers that expose
    /// both the structured pairs and the rendered warning (REST
    /// `/api/session`) without running detection twice. Byte-identical to
    /// the block the MCP layered renderer and the CLI emit.
    pub fn render_conflicts(
        &self,
        injected: &[SearchResult],
        conflicts: &[ConflictNotice],
    ) -> String {
        format::format_conflict_block(injected, conflicts)
    }

    /// Format injection output with layered strategy:
    /// - MUST memories: always full text
    /// - REF memories within budget: full text
    /// - Overflow: append summary hint + count
    pub fn format_layered_instructions(&self, output: &SessionStartOutput) -> String {
        format::format_layered_instructions(output)
    }

    /// Format the injection plus a `[RELATIONS]` appendix carrying the
    /// one-hop graph neighborhood of the injected memories (C5). Bounded:
    /// only the first [`RELATION_EXPANSION_MAX_MEMORIES`] memories are
    /// expanded, at most [`RELATION_EXPANSION_MAX_LINES`] lines each, so the
    /// graph can never blow up the context budget.
    pub async fn format_injection_with_relations(&self, output: &SessionStartOutput) -> String {
        let mut result = format::format_layered_instructions(output);

        const RELATION_EXPANSION_MAX_MEMORIES: usize = 8;
        const RELATION_EXPANSION_MAX_LINES: usize = 5;

        let mut lines: Vec<String> = Vec::new();
        for r in output.injected.iter().take(RELATION_EXPANSION_MAX_MEMORIES) {
            let rels = crate::relations::collect_relations(&*self.store, &r.memory.id).await;
            if rels.is_empty() {
                continue;
            }
            for rel in rels.iter().take(RELATION_EXPANSION_MAX_LINES) {
                lines.push(crate::relations::relation_line(&r.memory.content, rel));
            }
        }

        if !lines.is_empty() {
            result.push_str("\n[RELATIONS]:\n");
            for line in lines {
                result.push_str(&format!("  {line}\n"));
            }
        }
        result
    }

    pub fn format_as_instructions(&self, results: &[SearchResult]) -> String {
        format::format_as_instructions(results)
    }

    pub fn estimate_tokens(text: &str) -> usize {
        format::estimate_tokens(text)
    }

    /// Trim to budget, returning the cut tail so callers can account for it.
    pub fn trim_to_budget(
        results: &mut Vec<SearchResult>,
        token_budget: usize,
    ) -> Vec<SearchResult> {
        format::trim_to_budget(results, token_budget)
    }

    pub async fn get_mcp_resource_content(&self, uri: &str) -> Result<String> {
        debug!(uri, "reading MCP resource");
        match uri {
            "memory://user-profile" => {
                let query = SearchQuery {
                    query: String::new(),
                    priority_filter: Some(Priority::Must),
                    top_k: 20,
                    ..SearchQuery::new(String::new())
                };
                let mut results = self.store.search(query).await?.results;
                // Archive-layer exclusion (issue #2): reviewed L0 MUST rows
                // must not resurface through the resource either.
                results.retain(|r| !is_archived(&r.memory));
                Self::trim_to_budget(&mut results, 800);
                Ok(self.format_as_instructions(&results))
            }
            "memory://project-context" => {
                let query = SearchQuery {
                    query: String::new(),
                    priority_filter: Some(Priority::Reference),
                    top_k: 10,
                    ..SearchQuery::new(String::new())
                };
                let mut results = self.store.search(query).await?.results;
                // Archive-layer exclusion (issue #2), same rule as above.
                results.retain(|r| !is_archived(&r.memory));
                Self::trim_to_budget(&mut results, 700);
                Ok(self.format_as_instructions(&results))
            }
            _ => Ok(String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;
    use std::path::Path;

    async fn setup() -> MemoryRouter {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        let agent = SourceAgent {
            id: "claude-desktop".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Preference,
            "user prefers Python".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m1.instruction = Some("代码使用 Python，不用 Java".to_string());

        let mut m2 = Memory::new(
            MemoryType::Fact,
            "project uses FastAPI".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m2.tags = vec!["coding".to_string(), "project".to_string()];

        let mut m3 = Memory::new(
            MemoryType::Preference,
            "writing style: concise".to_string(),
            Priority::Reference,
            agent,
        );
        m3.tags = vec!["writing".to_string(), "style".to_string()];

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();
        store.save(m3).await.unwrap();

        MemoryRouter::new(store)
    }

    fn router_with_keyed_agent() -> MemoryRouter {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![AgentProfile {
            id: "agent-with-key".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: Some("s3cret".to_string()),
            inject_channel: None,
        }];
        MemoryRouter::with_registry(store, registry)
    }

    #[test]
    fn test_authenticate_agent_verified_true_with_matching_key() {
        let router = router_with_keyed_agent();
        let (_, verified) = router
            .authenticate_agent_verified("agent-with-key", Some("s3cret"))
            .unwrap();
        assert!(verified);
    }

    #[test]
    fn test_authenticate_agent_verified_rejects_wrong_key() {
        let router = router_with_keyed_agent();
        assert!(
            router
                .authenticate_agent_verified("agent-with-key", Some("wrong"))
                .is_err()
        );
    }

    #[test]
    fn test_authenticate_agent_verified_false_when_unauthenticated_mode() {
        let router = router_with_keyed_agent();
        // "unregistered-agent" has no key in the registry — unauthenticated
        // mode is allowed, but must never be reported as identity-verified.
        let (_, verified) = router
            .authenticate_agent_verified("unregistered-agent", None)
            .unwrap();
        assert!(!verified);
    }

    #[test]
    fn test_authenticate_admin_allows_unknown_id_when_no_keys_registered() {
        // Keyless default deployment: admin routes stay open (back-compat).
        let router = MemoryRouter::new(Arc::new(SqliteStore::in_memory().unwrap()));
        assert!(!router.has_registered_keys());
        assert!(router.authenticate_admin("whoever", None).is_ok());
    }

    #[test]
    fn test_authenticate_admin_rejects_unknown_id_once_any_key_registered() {
        let router = router_with_keyed_agent();
        assert!(router.has_registered_keys());

        // Inventing an agent id must not skip authentication, with or without
        // a (necessarily wrong) key.
        assert!(router.authenticate_admin("invented-agent", None).is_err());
        assert!(
            router
                .authenticate_admin("invented-agent", Some("s3cret"))
                .is_err()
        );

        // The keyed agent itself still authenticates.
        assert!(
            router
                .authenticate_admin("agent-with-key", Some("s3cret"))
                .is_ok()
        );
        assert!(
            router
                .authenticate_admin("agent-with-key", Some("wrong"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_session_start() {
        let router = setup().await;
        let results = router
            .session_start("claude-desktop", None, None)
            .await
            .unwrap()
            .results;
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_format_instructions() {
        let router = setup().await;
        let results = router
            .session_start("claude-desktop", None, None)
            .await
            .unwrap()
            .results;
        let formatted = router.format_as_instructions(&results);
        assert!(formatted.contains("[未审核]"));
        assert!(formatted.contains("[MEMORY CONTEXT"));
    }

    #[tokio::test]
    async fn test_mcp_resource() {
        let router = setup().await;
        let content = router
            .get_mcp_resource_content("memory://user-profile")
            .await
            .unwrap();
        assert!(content.contains("[未审核]"));
    }

    #[tokio::test]
    async fn test_token_budget_trims() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "This is memory number {} with some content to consume tokens",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", None, None)
            .await
            .unwrap()
            .results;
        assert!(results.len() <= 8);
    }

    #[tokio::test]
    async fn test_must_always_included() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let m1 = Memory::new(
            MemoryType::Preference,
            "critical rule".to_string(),
            Priority::Must,
            agent.clone(),
        );
        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "Filler memory {} with lots of content to fill the token budget up quickly",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }
        store.save(m1).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", None, None)
            .await
            .unwrap()
            .results;
        assert!(results.iter().any(|r| r.memory.priority == Priority::Must));
    }

    /// Every candidate that does not get injected must appear in `skipped`
    /// with a reason — silent drops are the failure mode this tracking
    /// exists to eliminate.
    #[tokio::test]
    async fn test_session_start_reports_skip_reasons() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        for i in 0..6 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "Budget filler memory number {} with enough text to cost tokens",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        // Tight budget and cap force drops.
        let profile = AgentProfile {
            id: "tight".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules {
                max_memories: 2,
                token_budget: 60,
                priority_order: vec![Priority::Must, Priority::Reference],
                namespace_filter: vec!["global".to_string()],
                exclude_types: Vec::new(),
            },
            api_key: None,
            inject_channel: None,
        };

        let router = MemoryRouter::with_registry(store, vec![profile]);
        let injection = router.session_start("tight", None, None).await.unwrap();

        assert!(!injection.results.is_empty());
        assert!(
            !injection.skipped.is_empty(),
            "tight budget/cap must produce skipped candidates"
        );

        let injected_ids: std::collections::HashSet<&str> = injection
            .results
            .iter()
            .map(|r| r.memory.id.as_str())
            .collect();
        for s in &injection.skipped {
            assert!(
                !injected_ids.contains(s.id.as_str()),
                "a memory cannot be both injected and skipped"
            );
            assert!(
                matches!(
                    s.reason,
                    InjectSkipReason::TokenBudgetExceeded | InjectSkipReason::MaxMemoriesExceeded
                ),
                "unexpected reason {:?} for budget/cap drops",
                s.reason
            );
        }
        assert!(injection.results.len() <= 2, "max_memories cap must hold");
    }

    /// The layered output must carry the same skip accounting.
    #[tokio::test]
    async fn test_session_start_layered_propagates_skipped() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        for i in 0..5 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "Layered filler {} with some amount of token weight behind it",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let profile = AgentProfile {
            id: "tight".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules {
                max_memories: 1,
                token_budget: 40,
                priority_order: vec![Priority::Reference],
                namespace_filter: vec!["global".to_string()],
                exclude_types: Vec::new(),
            },
            api_key: None,
            inject_channel: None,
        };
        let router = MemoryRouter::with_registry(store, vec![profile]);
        let output = router
            .session_start_layered("tight", None, None)
            .await
            .unwrap();
        assert!(!output.skipped.is_empty());
    }

    #[test]
    fn test_estimate_tokens() {
        assert!(MemoryRouter::estimate_tokens("hello world") < 10);
        assert!(MemoryRouter::estimate_tokens("代码使用 Python，不用 Java") > 5);
    }

    #[test]
    fn test_yaml_registry_parse() {
        let yaml = r#"
agents:
  - id: my-agent
    agent_type: coding-assistant
    description: "Custom coding agent"
    inject_rules:
      max_memories: 5
      token_budget: 1000
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: ["writing"]
"#;
        let config: AgentRegistryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].id, "my-agent");
        assert_eq!(config.agents[0].inject_rules.token_budget, 1000);
    }

    // --- format_as_instructions ---

    #[test]
    fn test_format_instructions_empty() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let formatted = router.format_as_instructions(&[]);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_instructions_all_priorities() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut m_must = Memory::new(
            MemoryType::Preference,
            "must content".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m_must.instruction = Some("must rule".to_string());

        let m_ref = Memory::new(
            MemoryType::Fact,
            "ref content".to_string(),
            Priority::Reference,
            agent.clone(),
        );

        let mut m_bg = Memory::new(
            MemoryType::Fact,
            "bg content".to_string(),
            Priority::Background,
            agent,
        );
        m_bg.instruction = Some("bg info".to_string());

        let results = vec![
            SearchResult {
                score: 1.0,
                memory: m_must,
                hit_sources: Vec::new(),
            },
            SearchResult {
                score: 0.5,
                memory: m_ref,
                hit_sources: Vec::new(),
            },
            SearchResult {
                score: 0.2,
                memory: m_bg,
                hit_sources: Vec::new(),
            },
        ];

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let formatted = router.format_as_instructions(&results);

        assert!(formatted.contains("[未审核] must rule"));
        assert!(formatted.contains("[未审核] ref content"));
        assert!(formatted.contains("[未审核] bg info"));
    }

    // --- estimate_tokens ---

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(MemoryRouter::estimate_tokens(""), 1);
    }

    #[test]
    fn test_estimate_tokens_ascii_only() {
        let t =
            MemoryRouter::estimate_tokens("hello world this is a test message with several words");
        assert!(t > 5);
        assert!(t < 20);
    }

    #[test]
    fn test_estimate_tokens_cjk_only() {
        let t = MemoryRouter::estimate_tokens("代码使用Python不用Java这是测试");
        assert!(t > 5);
    }

    // --- trim_to_budget ---

    #[test]
    fn test_trim_to_budget_empty() {
        let mut results: Vec<SearchResult> = Vec::new();
        MemoryRouter::trim_to_budget(&mut results, 1000);
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_trim_to_budget_must_exceeds() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        // One MUST with a huge content that exceeds budget
        let mut must = Memory::new(
            MemoryType::Preference,
            "x".repeat(5000),
            Priority::Must,
            agent,
        );
        must.instruction =
            Some("MUST rule with very long content that exceeds even generous budget".to_string());
        store.save(must).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", None, None)
            .await
            .unwrap()
            .results;
        // MUST must survive even if it exceeds budget
        assert!(results.iter().any(|r| r.memory.priority == Priority::Must));
    }

    // --- get_mcp_resource_content ---

    #[tokio::test]
    async fn test_mcp_resource_project_context() {
        let router = setup().await;
        let content = router
            .get_mcp_resource_content("memory://project-context")
            .await
            .unwrap();
        assert!(content.contains("[未审核]"));
    }

    #[tokio::test]
    async fn test_mcp_resource_unknown() {
        let router = setup().await;
        let content = router
            .get_mcp_resource_content("memory://nonexistent")
            .await
            .unwrap();
        assert!(content.is_empty());
    }

    // --- get_agent_profile ---

    #[test]
    fn test_get_agent_profile_exact_match() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::with_registry(
            store,
            vec![AgentProfile {
                id: "my-agent".to_string(),
                agent_type: "coding-assistant".to_string(),
                description: "Custom agent".to_string(),
                inject_rules: InjectRules {
                    max_memories: 3,
                    ..InjectRules::default()
                },
                api_key: None,
                inject_channel: None,
            }],
        );
        let profile = router.get_agent_profile("my-agent");
        assert_eq!(profile.id, "my-agent");
        assert_eq!(profile.inject_rules.max_memories, 3);
    }

    #[test]
    fn test_get_agent_profile_fallback_to_default() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let profile = router.get_agent_profile("completely-unknown-agent");
        assert_eq!(profile.id, "default");
    }

    #[test]
    fn test_get_agent_profile_with_client_info() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let profile = router.get_agent_profile_with_client_info("unknown", Some("claude-code"));
        assert_eq!(profile.agent_type, "coding-assistant");
    }

    // --- session_start with context ---

    #[tokio::test]
    async fn test_session_start_with_context_hint() {
        let router = setup().await;
        let results = router
            .session_start("claude-desktop", Some("帮我写一个 API"), None)
            .await
            .unwrap()
            .results;
        assert!(!results.is_empty());
        // MUST always comes first
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_session_start_project_namespace() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Fact,
            "project memory".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m1.namespace = "project:my-app".to_string();
        m1.tags = vec!["coding".to_string()];

        let m2 = Memory::new(
            MemoryType::Fact,
            "global memory".to_string(),
            Priority::Reference,
            agent,
        );

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        // Create a router with project-namespace filter
        let registry = vec![AgentProfile {
            id: "project-agent".to_string(),
            agent_type: "coding-assistant".to_string(),
            description: "Project agent".to_string(),
            inject_rules: InjectRules {
                namespace_filter: vec!["project:my-app".to_string()],
                ..InjectRules::default()
            },
            api_key: None,
            inject_channel: None,
        }];
        let router = MemoryRouter::with_registry(store, registry);

        let results = router
            .session_start("project-agent", Some("build my app"), Some("my-app"))
            .await
            .unwrap()
            .results;
        // Should find project-scoped memories
        assert!(results.iter().any(|r| r.memory.content == "project memory"));
    }

    // --- confirm_read ---

    #[tokio::test]
    async fn test_confirm_read() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "test confirm".to_string(),
            Priority::Reference,
            agent,
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let router = MemoryRouter::new(store.clone());
        router
            .confirm_read(std::slice::from_ref(&id))
            .await
            .unwrap();

        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.access_count, 1);
        assert!(updated.last_read_at.is_some());
    }

    // --- load_registry_from_yaml ---

    #[test]
    fn test_load_registry_from_yaml_invalid_path() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let result =
            MemoryRouter::load_registry_from_yaml(store, Path::new("/nonexistent/agents.yaml"));
        assert!(result.is_err());
    }

    // --- session_start_layered ---

    #[tokio::test]
    async fn test_session_start_layered_basic() {
        let router = setup().await;
        let output = router
            .session_start_layered("claude-desktop", None, None)
            .await
            .unwrap();
        assert!(!output.injected.is_empty());
        assert_eq!(output.injected[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_format_layered_no_overflow() {
        let router = setup().await;
        let output = router
            .session_start_layered("claude-desktop", None, None)
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&output);
        assert!(formatted.contains("[未审核]"));
        // With only 3 memories, no overflow expected
        if output.overflow_count == 0 {
            assert!(!formatted.contains("search_memory"));
        }
    }

    #[tokio::test]
    async fn test_format_layered_with_overflow() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        // Create many memories to trigger overflow
        let must = Memory::new(
            MemoryType::Preference,
            "critical rule".to_string(),
            Priority::Must,
            agent.clone(),
        );
        store.save(must).await.unwrap();

        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "Reference memory number {} with enough content to take tokens",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let router = MemoryRouter::new(store);
        let output = router
            .session_start_layered("default", None, None)
            .await
            .unwrap();

        assert!(output.overflow_count > 0);
        let formatted = router.format_layered_instructions(&output);
        assert!(formatted.contains("[未审核]"));
        assert!(formatted.contains("search_memory"));
        assert!(formatted.contains("还有"));
    }

    #[tokio::test]
    async fn test_session_start_layered_records_below_score_floor_for_overflow() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let must = Memory::new(
            MemoryType::Preference,
            "critical rule".to_string(),
            Priority::Must,
            agent.clone(),
        );
        store.save(must).await.unwrap();

        // Enough recent, higher-scoring Reference memories to push a weak
        // candidate out of session_start's own top-N window entirely.
        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!("Reference memory number {}", i),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        // Stale, never-accessed, Background-priority, and only overlapping
        // one word out of many in the context hint below — its composite
        // relevance score falls under the 0.1 injection floor, but it's
        // still a real (if weak) FTS keyword hit that session_start_layered
        // independently re-fetches via its broader `extended` query. It
        // must be accounted for as skipped, not silently vanish.
        let mut weak = Memory::new(
            MemoryType::Fact,
            "gizmo".to_string(),
            Priority::Background,
            agent.clone(),
        );
        weak.decay_score = 0.0;
        weak.updated_at = Utc::now() - chrono::Duration::days(400);
        let weak_id = store.save(weak).await.unwrap().id;

        let router = MemoryRouter::new(store);
        let hint = "gizmo apple banana cherry date eggplant fig grape honeydew iris jackfruit kiwi lemon mango nectarine";
        let output = router
            .session_start_layered("test-agent", Some(hint), None)
            .await
            .unwrap();

        assert!(
            !output.injected.iter().any(|r| r.memory.id == weak_id),
            "weak memory's score should not be strong enough to be injected"
        );
        let skipped = output.skipped.iter().find(|s| s.id == weak_id);
        assert!(
            skipped.is_some(),
            "weak memory must be accounted for in `skipped`, not silently dropped from overflow accounting"
        );
        assert_eq!(skipped.unwrap().reason, InjectSkipReason::BelowScoreFloor);
    }

    /// The store state of the reported gate bypass: `ingest` writes TWO
    /// related rows — the distilled unreviewed MUST candidate, and its L0
    /// raw-evidence twin carrying the original utterance. The twin is
    /// `human_reviewed = true` by design ("raw logs need no human review",
    /// `trace.rs`), so `is_trusted` says yes and a gate keyed on trust alone
    /// can never catch it — the overflow channel needs the explicit L0
    /// exclusion.
    async fn midnight_blue_store() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let candidate = Memory::new(
            MemoryType::Preference,
            "Always use midnight blue themes in every interface".to_string(),
            Priority::Must,
            agent.clone(),
        );
        store.save(candidate).await.unwrap();

        let mut evidence = Memory::new(
            MemoryType::Preference,
            "I always prefer midnight blue themes in every interface.".to_string(),
            Priority::Background,
            agent.clone(),
        );
        evidence.layer = MemoryLayer::L0;
        evidence.human_reviewed = true;
        evidence.namespace = "trace:claude-code".to_string();
        store.save(evidence).await.unwrap();

        store
    }

    /// Regression for the reported bypass: with the strict MUST gate on, the
    /// CLI body dropped the unreviewed candidate, but MCP's layered overflow
    /// still echoed the same sentence into `[MORE - 摘要]` via the L0
    /// evidence row. Neither the candidate nor its raw-evidence twin may
    /// reach the injection through ANY channel while the gate is on.
    #[tokio::test]
    async fn test_layered_strict_gate_keeps_unreviewed_must_and_its_l0_evidence_out() {
        let router = MemoryRouter::new(midnight_blue_store().await);
        let hint = "midnight blue themes";

        let output = router
            .session_start_layered_gated("claude-code", Some(hint), None, true)
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&output);

        assert!(
            !formatted.contains("midnight blue"),
            "gated candidate or its L0 evidence leaked into the injection: {formatted:?}"
        );
        assert!(
            output
                .overflow_summaries
                .iter()
                .all(|s| !s.contains("midnight blue")),
            "overflow summaries echoed gated content: {:?}",
            output.overflow_summaries
        );
        assert!(
            output
                .skipped
                .iter()
                .any(|s| s.reason == InjectSkipReason::UnreviewedMustGated),
            "the gated candidate must stay auditable in `skipped`, got: {:?}",
            output.skipped
        );
        assert!(
            !output
                .injected
                .iter()
                .any(|r| r.memory.priority == Priority::Must),
            "gated candidate must leave the selection set — compliance records this list"
        );
    }

    /// Gate OFF (the default): the unreviewed candidate still renders as
    /// `[未审核]` reference data, never `[MUST]` — and the L0 raw-evidence
    /// twin stays out of the overflow summaries regardless of the gate, on
    /// every deployment.
    #[tokio::test]
    async fn test_layered_overflow_never_echoes_l0_raw_evidence() {
        let router = MemoryRouter::new(midnight_blue_store().await);
        let hint = "midnight blue themes";

        let output = router
            .session_start_layered_gated("claude-code", Some(hint), None, false)
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&output);

        assert!(
            formatted.contains("[未审核]"),
            "gate off: the unreviewed candidate belongs in the reference-data block, got {formatted:?}"
        );
        assert!(!formatted.contains("[MUST]"));
        assert!(
            output
                .overflow_summaries
                .iter()
                .all(|s| !s.contains("midnight blue")),
            "L0 evidence echoed into overflow summaries: {:?}",
            output.overflow_summaries
        );
        assert!(
            !formatted.contains("I always prefer"),
            "raw L0 transcript text must never be auto-injected: {formatted:?}"
        );
    }

    #[test]
    fn test_overflow_admits_excludes_l0_and_gated_unreviewed_must() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };

        let mut l0 = Memory::new(
            MemoryType::Fact,
            "raw transcript line".to_string(),
            Priority::Background,
            agent.clone(),
        );
        l0.layer = MemoryLayer::L0;
        l0.human_reviewed = true; // trace ingestion marks raw logs reviewed
        assert!(!MemoryRouter::overflow_admits(&l0, false));
        assert!(!MemoryRouter::overflow_admits(&l0, true));

        let unvetted_must = Memory::new(
            MemoryType::Preference,
            "always X".to_string(),
            Priority::Must,
            agent.clone(),
        );
        assert!(
            !MemoryRouter::overflow_admits(&unvetted_must, true),
            "strict gate must cover the overflow channel"
        );
        assert!(MemoryRouter::overflow_admits(&unvetted_must, false));

        let mut trusted_must = Memory::new(
            MemoryType::Preference,
            "always Y".to_string(),
            Priority::Must,
            agent.clone(),
        );
        trusted_must.human_reviewed = true;
        assert!(MemoryRouter::overflow_admits(&trusted_must, true));

        let unvetted_ref = Memory::new(
            MemoryType::Fact,
            "some fact".to_string(),
            Priority::Reference,
            agent,
        );
        assert!(
            MemoryRouter::overflow_admits(&unvetted_ref, true),
            "the gate is MUST-only; unreviewed REF/BG keep their existing treatment"
        );
    }

    /// Issue #2 regression: an L0 (archive-layer) row with MUST priority
    /// re-entered live context through the paths the overflow L0 exclusion
    /// never covered — the injection BODY (the MUST-completeness union pulls
    /// every MUST row in, and the strict gate only checks review state, so
    /// gate-on + reviewed leaked into `[MUST]`, gate-off leaked into
    /// `[MUST]`/`[未审核]`), the `memory://user-profile` resource, and the
    /// deterministic fast path. L0 exclusion is orthogonal to priority,
    /// review state, and gating: it must hold for all four combinations on
    /// every exposure path.
    #[tokio::test]
    async fn test_l0_must_never_enters_body_resource_or_fast_path() {
        const SENTINEL: &str = "ARCHIVE_MUST_STAY_HIDDEN_VERIFY";

        for reviewed in [false, true] {
            let store = Arc::new(SqliteStore::in_memory().unwrap());
            let agent = SourceAgent {
                id: "claude-code".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            };

            let mut archived = Memory::new(
                MemoryType::Fact,
                SENTINEL.to_string(),
                Priority::Must,
                agent.clone(),
            );
            archived.layer = MemoryLayer::L0;
            archived.human_reviewed = reviewed;
            store.save(archived).await.unwrap();

            // Control: a live (non-L0) reviewed MUST rule must stay visible
            // everywhere — the fix excludes the archive layer, not MUST.
            let mut live = Memory::new(
                MemoryType::Preference,
                "live must rule".to_string(),
                Priority::Must,
                agent.clone(),
            );
            live.human_reviewed = true;
            store.save(live).await.unwrap();

            let router = MemoryRouter::new(store);

            for strict in [false, true] {
                let injection = router
                    .session_start_gated("claude-code", None, None, strict)
                    .await
                    .unwrap();
                assert!(
                    !injection
                        .results
                        .iter()
                        .any(|r| r.memory.content.contains(SENTINEL)),
                    "L0 MUST (reviewed={reviewed}) leaked into the injection body (strict={strict})"
                );
                assert!(
                    injection
                        .results
                        .iter()
                        .any(|r| r.memory.content.contains("live must rule")),
                    "live MUST rule must still be injected (reviewed={reviewed}, strict={strict})"
                );

                // The MCP-visible layered rendering must not contain the
                // sentinel in ANY block ([MUST], [未审核], [MORE - 摘要]).
                let layered = router
                    .session_start_layered_gated("claude-code", None, None, strict)
                    .await
                    .unwrap();
                let formatted = router.format_layered_instructions(&layered);
                assert!(
                    !formatted.contains(SENTINEL),
                    "L0 MUST (reviewed={reviewed}) leaked into formatted injection (strict={strict}): {formatted:?}"
                );
            }

            // Resource path (reported leak: reviewed L0 MUST content).
            let profile_res = router
                .get_mcp_resource_content("memory://user-profile")
                .await
                .unwrap();
            assert!(
                !profile_res.contains(SENTINEL),
                "L0 MUST (reviewed={reviewed}) leaked into memory://user-profile"
            );
            assert!(
                profile_res.contains("live must rule"),
                "live MUST rule must still surface in the resource"
            );

            // Deterministic fast path.
            let fast = router
                .deterministic_injection("claude-code", None)
                .await
                .unwrap();
            assert!(
                !fast.iter().any(|r| r.memory.content.contains(SENTINEL)),
                "L0 MUST (reviewed={reviewed}) leaked into the deterministic fast path"
            );
        }
    }

    /// CLI/MCP parity regression (v0.6.8 report): the conflict warning for
    /// injected memories must come from ONE shared implementation. The CLI's
    /// plain session-start path (`session_start` + `format_as_instructions`)
    /// skipped it entirely, so the CLI-backed SessionStart hook injected
    /// contradicting MUST memories without the "don't auto-pick" notice the
    /// MCP layered renderer already emitted. Covers the report's acceptance
    /// matrix: warning present on both paths (byte-identical block),
    /// supports-only never warns, a bidirectional relation renders one
    /// bullet, and a memory outside the injected set (gated / L0 /
    /// out-of-scope) cannot leak through the warning.
    #[tokio::test]
    async fn test_conflict_warnings_shared_across_entry_points() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-desktop".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let mut tabs = Memory::new(
            MemoryType::Preference,
            "I prefer tabs for indentation.".to_string(),
            Priority::Must,
            agent.clone(),
        );
        tabs.human_reviewed = true;
        let tabs_id = tabs.id.clone();
        store.save(tabs).await.unwrap();

        let mut spaces = Memory::new(
            MemoryType::Preference,
            "I prefer spaces for indentation.".to_string(),
            Priority::Must,
            agent.clone(),
        );
        spaces.human_reviewed = true;
        let spaces_id = spaces.id.clone();
        store.save(spaces).await.unwrap();

        // Explicit contradicts evidence: spaces —contradicts→ tabs
        // (subject = the evidence memory, object = the contradicted one).
        crate::evidence::add_evidence(
            &*store,
            &spaces_id,
            crate::evidence::EvidenceKind::Contradicts,
            Some(&tabs_id),
            None,
            0.8,
        )
        .await
        .unwrap();

        let router = MemoryRouter::new(store.clone());
        let injection = router
            .session_start_gated("claude-desktop", None, None, false)
            .await
            .unwrap();
        assert_eq!(
            injection.results.len(),
            2,
            "both reviewed MUST memories are recalled"
        );

        // The CLI-side entry point renders the warning...
        let warnings = router.conflict_warnings(&injection.results).await;
        assert!(warnings.contains("MEMORY CONFLICT"), "{warnings}");
        assert!(
            warnings.contains("tabs") && warnings.contains("spaces"),
            "{warnings}"
        );
        assert!(warnings.contains("不要自动二选一"), "{warnings}");

        // ...and the MCP-side layered renderer emits the byte-identical block.
        let layered = router
            .session_start_layered_gated("claude-desktop", None, None, false)
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&layered);
        assert!(
            formatted.contains(&warnings),
            "layered output must contain the identical conflict block: {formatted:?}"
        );

        // A reverse relation must not duplicate the pair.
        crate::evidence::add_evidence(
            &*store,
            &tabs_id,
            crate::evidence::EvidenceKind::Contradicts,
            Some(&spaces_id),
            None,
            0.8,
        )
        .await
        .unwrap();
        let warnings2 = router.conflict_warnings(&injection.results).await;
        assert_eq!(
            warnings2.matches("←→").count(),
            1,
            "bidirectional relation renders exactly one bullet: {warnings2}"
        );

        // A memory outside the injected set cannot leak through the warning:
        // with only `tabs` injected, the pair has no resolvable other side.
        let only_tabs: Vec<SearchResult> = injection
            .results
            .iter()
            .filter(|r| r.memory.id == tabs_id)
            .cloned()
            .collect();
        assert_eq!(
            router.conflict_warnings(&only_tabs).await,
            "",
            "excluded memories must not surface via the conflict block"
        );

        // Supports-only evidence never warns.
        let store2 = Arc::new(SqliteStore::in_memory().unwrap());
        let mut x = Memory::new(
            MemoryType::Fact,
            "deploy window is Friday".to_string(),
            Priority::Must,
            agent.clone(),
        );
        x.human_reviewed = true;
        let x_id = x.id.clone();
        store2.save(x).await.unwrap();
        let mut y = Memory::new(
            MemoryType::Fact,
            "release trains leave Friday".to_string(),
            Priority::Must,
            agent.clone(),
        );
        y.human_reviewed = true;
        let y_id = y.id.clone();
        store2.save(y).await.unwrap();
        crate::evidence::add_evidence(
            &*store2,
            &y_id,
            crate::evidence::EvidenceKind::Supports,
            Some(&x_id),
            None,
            0.8,
        )
        .await
        .unwrap();
        let router2 = MemoryRouter::new(store2);
        let injection2 = router2
            .session_start_gated("claude-desktop", None, None, false)
            .await
            .unwrap();
        assert_eq!(injection2.results.len(), 2);
        assert_eq!(
            router2.conflict_warnings(&injection2.results).await,
            "",
            "supports relations must not produce a conflict warning"
        );
    }

    /// The extended overflow query used to filter by `project` alone — with
    /// no project it swept EVERY namespace, so another project's memories
    /// (and `trace:*` evidence) surfaced as `[MORE - 摘要]` even for a
    /// session whose profile resolves to `global`. Overflow must stay
    /// within the same namespace universe as the body.
    #[tokio::test]
    async fn test_layered_overflow_stays_within_profile_namespace_scope() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let global_note = Memory::new(
            MemoryType::Fact,
            "zebra zephyr global note".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        store.save(global_note).await.unwrap();

        let mut foreign = Memory::new(
            MemoryType::Fact,
            "zebra zephyr foreign secret".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        foreign.namespace = "project:secret".to_string();
        store.save(foreign).await.unwrap();

        let router = MemoryRouter::new(store);
        let output = router
            .session_start_layered("claude-code", Some("zebra zephyr"), None)
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&output);

        assert!(
            !formatted.contains("foreign secret"),
            "another project's memory leaked into the injection: {formatted:?}"
        );
        assert!(
            output
                .overflow_summaries
                .iter()
                .all(|s| !s.contains("foreign")),
            "another project's memory leaked into overflow summaries: {:?}",
            output.overflow_summaries
        );
    }

    /// With a project active, the body supplements from `global`
    /// (cross-namespace fallback) — the overflow sweep must mirror that, or
    /// budget-trimmed global candidates silently vanish from the summaries.
    /// Foreign projects stay excluded.
    #[tokio::test]
    async fn test_layered_overflow_sweeps_global_when_project_scoped() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        // Enough in-scope project memories that the body's cross-namespace
        // fallback (fires only when the project namespace has fewer than
        // max_memories/2 non-MUST results) stays dormant — the global row
        // below must arrive via the OVERFLOW sweep, not the body.
        for i in 0..4 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("zebra zephyr demo note {i}"),
                Priority::Reference,
                agent.clone(),
            );
            m.namespace = "project:demo".to_string();
            store.save(m).await.unwrap();
        }

        let global_echo = Memory::new(
            MemoryType::Fact,
            "zebra zephyr global echo".to_string(),
            Priority::Background,
            agent.clone(),
        );
        store.save(global_echo).await.unwrap();

        let mut foreign = Memory::new(
            MemoryType::Fact,
            "zebra zephyr foreign secret".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        foreign.namespace = "project:other".to_string();
        store.save(foreign).await.unwrap();

        let router = MemoryRouter::new(store);
        let output = router
            .session_start_layered("claude-code", Some("zebra zephyr"), Some("demo"))
            .await
            .unwrap();
        let formatted = router.format_layered_instructions(&output);

        assert!(
            output
                .overflow_summaries
                .iter()
                .any(|s| s.contains("zebra zephyr global echo")),
            "global candidates must stay reachable from overflow when a project is active: {:?}",
            output.overflow_summaries
        );
        assert!(
            !formatted.contains("foreign secret"),
            "a foreign project's memory leaked into a project-scoped session: {formatted:?}"
        );
    }

    #[test]
    fn test_make_summary_short() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Fact,
            "short".to_string(),
            Priority::Reference,
            agent,
        );
        mem.instruction = Some("brief".to_string());
        let summary = MemoryRouter::make_summary(&mem);
        assert_eq!(summary, "brief");
    }

    #[test]
    fn test_make_summary_long_truncates() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "a".repeat(100),
            Priority::Reference,
            agent,
        );
        let summary = MemoryRouter::make_summary(&mem);
        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 63);
    }

    // --- layer field ---

    #[test]
    fn test_memory_layer_default_from_priority() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let must = Memory::new(
            MemoryType::Preference,
            "x".into(),
            Priority::Must,
            agent.clone(),
        );
        assert_eq!(must.layer, MemoryLayer::L3);

        let reference = Memory::new(
            MemoryType::Fact,
            "x".into(),
            Priority::Reference,
            agent.clone(),
        );
        assert_eq!(reference.layer, MemoryLayer::L2);

        let bg = Memory::new(MemoryType::Fact, "x".into(), Priority::Background, agent);
        assert_eq!(bg.layer, MemoryLayer::L1);
    }

    // --- load_registry_from_yaml ---

    #[test]
    fn test_load_registry_from_yaml_success() {
        let dir = std::env::temp_dir().join("memvault_test_registry");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agents.yaml");
        std::fs::write(
            &path,
            r#"
agents:
  - id: yaml-agent
    agent_type: coding-assistant
    description: "from yaml"
    inject_rules:
      max_memories: 3
      token_budget: 500
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: ["writing"]
"#,
        )
        .unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::load_registry_from_yaml(store, &path).unwrap();
        let profile = router.get_agent_profile("yaml-agent");
        assert_eq!(profile.inject_rules.max_memories, 3);
        assert_eq!(profile.inject_rules.token_budget, 500);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_load_registry_from_yaml_invalid_yaml() {
        let dir = std::env::temp_dir().join("ratings_test_bad_yaml");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agents.yaml");
        std::fs::write(&path, "agents: [not: valid: yaml: {{{").unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let result = MemoryRouter::load_registry_from_yaml(store, &path);
        assert!(result.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_load_registry_from_yaml_keyed_agent_authenticates() {
        // Regression: agents.yaml keys were hashed twice (once in the YAML
        // loader, again inside AgentAuth::from_profiles), so the configured
        // plaintext key never authenticated over REST/MCP. Exactly one hash
        // must be applied, and the in-memory profile must still hold a hash.
        let dir = std::env::temp_dir().join("memvault_test_registry_keyed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agents.yaml");
        std::fs::write(
            &path,
            r#"
agents:
  - id: keyed-agent
    agent_type: coding-assistant
    description: "keyed"
    inject_rules:
      max_memories: 3
      token_budget: 500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global"]
      exclude_types: []
    api_key: s3cret-plaintext
"#,
        )
        .unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::load_registry_from_yaml(store, &path).unwrap();
        let (_, verified) = router
            .authenticate_agent_verified("keyed-agent", Some("s3cret-plaintext"))
            .unwrap();
        assert!(verified);
        assert!(
            router
                .authenticate_agent("keyed-agent", Some("wrong-key"))
                .is_err()
        );
        let profile = router.get_agent_profile("keyed-agent");
        assert_ne!(
            profile.api_key.as_deref(),
            Some("s3cret-plaintext"),
            "plaintext key must never be kept in memory"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_list_agent_profiles_includes_the_injected_default() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::with_registry(
            store,
            vec![AgentProfile {
                id: "only-agent".to_string(),
                agent_type: "coding-assistant".to_string(),
                description: String::new(),
                inject_rules: InjectRules::default(),
                api_key: None,
                inject_channel: None,
            }],
        );
        let profiles = router.list_agent_profiles();
        assert_eq!(profiles.len(), 2);
        assert!(profiles.iter().any(|p| p.id == "only-agent"));
        assert!(profiles.iter().any(|p| p.id == "default"));
    }

    // --- get_agent_profile resolution ---

    #[test]
    fn test_with_registry_injects_default_profile() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::with_registry(
            store,
            vec![AgentProfile {
                id: "only-agent".to_string(),
                agent_type: "coding-assistant".to_string(),
                description: String::new(),
                inject_rules: InjectRules::default(),
                api_key: None,
                inject_channel: None,
            }],
        );
        let default = router.get_agent_profile("default");
        assert_eq!(default.id, "default");
    }

    #[test]
    fn test_get_agent_profile_partial_type_match() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::with_registry(
            store,
            vec![AgentProfile {
                id: "coding-prime".to_string(),
                agent_type: "coding-assistant".to_string(),
                description: String::new(),
                inject_rules: InjectRules {
                    max_memories: 3,
                    ..InjectRules::default()
                },
                api_key: None,
                inject_channel: None,
            }],
        );
        // agent_id "coding-…" should partial-match against "coding-assistant"
        let profile = router.get_agent_profile("coding-helper");
        assert_eq!(profile.id, "coding-prime");
    }

    // --- session_start scoring branches ---

    #[tokio::test]
    async fn test_session_start_unknown_agent_falls_back_to_default() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let results = router
            .session_start("totally-unknown-agent", None, None)
            .await
            .unwrap()
            .results;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_session_start_exclude_types_soft_penalty() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "a".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        let mut m = Memory::new(
            MemoryType::Fact,
            "some writing content".into(),
            Priority::Reference,
            agent,
        );
        m.tags = vec!["writing".to_string()];
        store.save(m).await.unwrap();

        let router = MemoryRouter::with_registry(
            store,
            vec![AgentProfile {
                id: "project-agent".to_string(),
                agent_type: "coding-assistant".to_string(),
                description: String::new(),
                inject_rules: InjectRules {
                    exclude_types: vec!["writing".to_string()],
                    ..InjectRules::default()
                },
                api_key: None,
                inject_channel: None,
            }],
        );
        // Must not panic; excluded-type memory must be soft-penalized, not crash.
        let results = router
            .session_start("project-agent", None, None)
            .await
            .unwrap()
            .results;
        assert!(results.iter().any(|r| r.memory.content.contains("writing")));
    }

    #[tokio::test]
    async fn test_session_start_intent_soft_penalty() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "a".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        let mut m = Memory::new(
            MemoryType::Preference,
            "keep the blog tone friendly".into(),
            Priority::Reference,
            agent,
        );
        m.tags = vec!["writing".to_string()];
        store.save(m).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", Some("帮我写一段营销文案"), None)
            .await
            .unwrap()
            .results;
        assert!(
            results
                .iter()
                .any(|r| r.memory.tags.contains(&"writing".to_string()))
        );
    }

    #[tokio::test]
    async fn test_session_start_intent_boost_ranks_preferred_type_higher() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "a".into(),
            agent_type: "g".into(),
            session_id: None,
        };

        let skill = Memory::new(
            MemoryType::Skill,
            "deploy runbook steps".into(),
            Priority::Reference,
            agent.clone(),
        );
        let skill_id = skill.id.clone();
        store.save(skill).await.unwrap();

        let episode = Memory::new(
            MemoryType::Episode,
            "yesterday's standup notes".into(),
            Priority::Reference,
            agent,
        );
        let episode_id = episode.id.clone();
        store.save(episode).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", Some("帮我调试这段代码"), None)
            .await
            .unwrap()
            .results;

        let skill_pos = results
            .iter()
            .position(|r| r.memory.id == skill_id)
            .expect("skill must be injected");
        let episode_pos = results
            .iter()
            .position(|r| r.memory.id == episode_id)
            .expect("episode must be injected");
        assert!(
            skill_pos < episode_pos,
            "Coding intent must rank Skill above Episode when base relevance ties"
        );
    }

    #[tokio::test]
    async fn test_session_start_cross_namespace_fallback() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "a".into(),
            agent_type: "g".into(),
            session_id: None,
        };

        // Project namespace holds only a MUST (doesn't count toward ref quota).
        let mut must = Memory::new(
            MemoryType::Preference,
            "project rule".into(),
            Priority::Must,
            agent.clone(),
        );
        must.namespace = "project:alpha".to_string();

        let global = Memory::new(
            MemoryType::Fact,
            "global filler".into(),
            Priority::Reference,
            agent,
        );

        store.save(must).await.unwrap();
        store.save(global).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .session_start("default", None, Some("alpha"))
            .await
            .unwrap()
            .results;
        // Fallback has pulled the global memory in to meet the ref quota.
        assert!(results.iter().any(|r| r.memory.content == "global filler"));
    }

    // --- embedder-driven hybrid search ---

    struct FakeEmbedder;
    #[async_trait::async_trait]
    impl crate::embedding::EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, texts: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.9, 0.1, 0.0]).collect())
        }
        fn dimension(&self) -> usize {
            3
        }
    }

    #[tokio::test]
    async fn test_session_start_hybrid_with_embedder() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".into(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "python project conventions".into(),
            Priority::Reference,
            agent,
        );
        store
            .save_with_embedding(mem, vec![0.85, 0.15, 0.0])
            .await
            .unwrap();

        let router = MemoryRouter::new(store).with_embedder(Arc::new(FakeEmbedder));
        let results = router
            .session_start("claude-code", Some("python project"), None)
            .await
            .unwrap()
            .results;
        assert!(results.iter().any(|r| r.memory.content.contains("python")));
    }

    // --- embedding backfill(缺失向量自动回填,独立覆盖) ---

    #[tokio::test]
    async fn test_embedding_backfill_populates_missing_vectors() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".into(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "python 项目的构建与发布约定".into(),
            Priority::Reference,
            agent,
        );
        store.save(mem).await.unwrap();

        let pending = store.list_without_embedding(20).await.unwrap();
        assert_eq!(pending.len(), 1, "保存无向量记忆后应产生 1 条待回填");

        let embedder = Arc::new(FakeEmbedder);
        let count = MemoryRouter::do_backfill(&*store, &*embedder)
            .await
            .unwrap();
        assert_eq!(count, 1, "应回填 1 条记忆");

        let pending2 = store.list_without_embedding(20).await.unwrap();
        assert!(pending2.is_empty(), "回填后不应再有待回填记忆");
    }

    /// `backfill_all_embeddings` is the one-shot counterpart to the
    /// server's background `spawn_embedding_backfill`, for a store a
    /// long-running server never touched — e.g. right after a JSON/Markdown
    /// import (which carries no vectors) that `memvault-cli
    /// backfill-embeddings` drives. It must drain every vectorless row, not
    /// just the first internal batch, and report the true total.
    #[tokio::test]
    async fn test_backfill_all_embeddings_drains_every_vectorless_memory() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".into(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        };
        for i in 0..3 {
            store
                .save(Memory::new(
                    MemoryType::Fact,
                    format!("vectorless fact {i}"),
                    Priority::Reference,
                    agent.clone(),
                ))
                .await
                .unwrap();
        }
        assert_eq!(store.list_without_embedding(20).await.unwrap().len(), 3);

        let embedder = FakeEmbedder;
        let total = MemoryRouter::backfill_all_embeddings(&*store, &embedder)
            .await
            .unwrap();

        assert_eq!(total, 3, "all 3 vectorless memories must be backfilled");
        assert!(store.list_without_embedding(20).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_embedding_backfill_skips_when_all_embedded() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "claude-code".into(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "已回填的 python 约定".into(),
            Priority::Reference,
            agent,
        );
        store
            .save_with_embedding(mem, vec![0.9, 0.1, 0.0])
            .await
            .unwrap();

        let count = MemoryRouter::do_backfill(&*store, &*Arc::new(FakeEmbedder))
            .await
            .unwrap();
        assert_eq!(count, 0, "无缺向量记忆时回填计数应为 0");
    }

    // --- Episodic lesson injection (A4) ---

    /// Record a failed task with a cause and reflect it into a lesson,
    /// returning the lesson memory id. Rule-based reflection (no LLM) fires
    /// because a cause is present.
    async fn make_lesson(store: &SqliteStore, task_type: &str, idx: usize) -> String {
        let input = crate::episode::OutcomeInput {
            task: format!("task {idx}"),
            status: OutcomeStatus::Failure,
            cause: Some(format!("root cause {idx}")),
            task_type: Some(task_type.to_string()),
            skill_id: None,
            tags: Vec::new(),
            namespace: "global".to_string(),
            source_agent: SourceAgent {
                id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        };
        let recorded = crate::episode::record_outcome(store, input.clone(), None)
            .await
            .unwrap();
        let lesson = crate::reflection::reflect_and_store(
            store,
            &recorded.memory.id,
            input.into(),
            None,
            None,
        )
        .await
        .unwrap()
        .expect("failure with cause must reflect into a lesson");
        lesson.lesson_memory.id
    }

    #[tokio::test]
    async fn test_session_start_injects_matching_lesson() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        // Four project memories so the cross-namespace fallback does not
        // fire — the lesson must arrive via task_type matching, not luck.
        for i in 0..4 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("project fact {i}"),
                Priority::Reference,
                agent.clone(),
            );
            m.namespace = "project:alpha".to_string();
            store.save(m).await.unwrap();
        }

        let lesson_id = make_lesson(&store, "deploy", 1).await;
        let router = MemoryRouter::new(store);

        let injection = router
            .session_start(
                "claude-desktop",
                Some("please deploy the dashboard"),
                Some("alpha"),
            )
            .await
            .unwrap();
        assert!(
            injection.results.iter().any(|r| r.memory.id == lesson_id),
            "lesson for task_type 'deploy' must be injected when the context mentions deploying"
        );
    }

    #[tokio::test]
    async fn test_session_start_skips_lesson_without_task_type_match() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };
        for i in 0..4 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("project fact {i}"),
                Priority::Reference,
                agent.clone(),
            );
            m.namespace = "project:alpha".to_string();
            store.save(m).await.unwrap();
        }

        let lesson_id = make_lesson(&store, "deploy", 1).await;
        let router = MemoryRouter::new(store);

        let injection = router
            .session_start(
                "claude-desktop",
                Some("write the release notes"),
                Some("alpha"),
            )
            .await
            .unwrap();
        assert!(
            !injection.results.iter().any(|r| r.memory.id == lesson_id),
            "a 'deploy' lesson must not be injected into an unrelated writing session"
        );
    }

    #[tokio::test]
    async fn test_lesson_quota_caps_at_three() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        for i in 0..5 {
            make_lesson(&store, "deploy", i).await;
        }
        let router = MemoryRouter::new(store.clone());

        let injection = router
            .session_start("claude-desktop", Some("deploy all the services"), None)
            .await
            .unwrap();

        let injected_lessons: Vec<_> = injection
            .results
            .iter()
            .filter(|r| is_lesson(&r.memory))
            .collect();
        assert_eq!(
            injected_lessons.len(),
            MAX_LESSONS_PER_INJECTION,
            "quota must cap non-MUST lessons at {MAX_LESSONS_PER_INJECTION}"
        );

        let quota_skips = injection
            .skipped
            .iter()
            .filter(|s| s.reason == InjectSkipReason::LessonQuotaExceeded)
            .count();
        assert_eq!(quota_skips, 2, "5 lessons - quota 3 = 2 reported skips");
    }

    #[tokio::test]
    async fn test_lesson_quota_exempts_must() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut lesson_ids = Vec::new();
        for i in 0..5 {
            lesson_ids.push(make_lesson(&store, "deploy", i).await);
        }
        // Promote one lesson to MUST (simulating confirmed human escalation).
        let mut must_lesson = store.get(&lesson_ids[0]).await.unwrap();
        must_lesson.priority = Priority::Must;
        store.update(must_lesson).await.unwrap();

        let router = MemoryRouter::new(store.clone());
        let injection = router
            .session_start("claude-desktop", Some("deploy all the services"), None)
            .await
            .unwrap();

        let injected_lessons: Vec<_> = injection
            .results
            .iter()
            .filter(|r| is_lesson(&r.memory))
            .collect();
        assert_eq!(
            injected_lessons.len(),
            MAX_LESSONS_PER_INJECTION + 1,
            "MUST lesson bypasses the quota: 3 capped + 1 MUST"
        );
        assert!(
            injected_lessons
                .iter()
                .any(|r| r.memory.id == lesson_ids[0]),
            "the MUST lesson must always be injected"
        );
    }

    // --- Procedural skill injection (B1/B2) ---

    async fn make_skill(store: &SqliteStore, name: &str, trigger: &str) -> String {
        let agent = SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Skill,
            name.to_string(),
            Priority::Reference,
            agent,
        );
        mem.skill_meta = Some(SkillMeta {
            trigger: Some(trigger.to_string()),
            steps: vec!["prepare".to_string(), "execute".to_string()],
            verification: Some("verify it works".to_string()),
            version: 2,
        });
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        id
    }

    #[tokio::test]
    async fn test_session_start_injects_matching_skill() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let skill_id = make_skill(&store, "deploy runbook", "deploy").await;
        let router = MemoryRouter::new(store.clone());

        let injection = router
            .session_start(
                "claude-desktop",
                Some("please deploy the new service"),
                None,
            )
            .await
            .unwrap();

        let skill_result = injection.results.iter().find(|r| r.memory.id == skill_id);
        let Some(r) = skill_result else {
            panic!("matching skill must be injected");
        };
        // Structured block rendered into the instruction.
        let block = r.memory.instruction.as_deref().unwrap();
        assert!(block.contains("[SKILL: deploy runbook] (v2)"));
        assert!(block.contains("1. prepare"));
        assert!(block.contains("2. execute"));
        assert!(block.contains("verify it works"));
        // Injection was counted for the success-rate denominator.
        let stats = store
            .get_skill_stats(&skill_id)
            .await
            .unwrap()
            .expect("tracked");
        assert_eq!(stats.injected_count, 1);
    }

    #[tokio::test]
    async fn test_session_start_skips_skill_without_trigger_match() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };
        // Fill the project namespace so the cross-namespace fallback does not
        // pull the global skill in — only trigger matching could surface it.
        for i in 0..4 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("project fact {i}"),
                Priority::Reference,
                agent.clone(),
            );
            m.namespace = "project:alpha".to_string();
            store.save(m).await.unwrap();
        }
        let skill_id = make_skill(&store, "deploy runbook", "deploy").await;
        let router = MemoryRouter::new(store.clone());

        let injection = router
            .session_start(
                "claude-desktop",
                Some("write a poem about spring"),
                Some("alpha"),
            )
            .await
            .unwrap();

        assert!(
            !injection.results.iter().any(|r| r.memory.id == skill_id),
            "a 'deploy' skill must not be injected into an unrelated session"
        );
        assert!(
            store.get_skill_stats(&skill_id).await.unwrap().is_none(),
            "a never-injected skill must have no stats row"
        );
    }

    #[tokio::test]
    async fn test_skill_quota_caps_at_two() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        for i in 0..3 {
            make_skill(&store, &format!("deploy skill {i}"), "deploy").await;
        }
        let router = MemoryRouter::new(store.clone());

        let injection = router
            .session_start("claude-desktop", Some("deploy everything"), None)
            .await
            .unwrap();

        let injected_skills: Vec<_> = injection
            .results
            .iter()
            .filter(|r| is_skill(&r.memory))
            .collect();
        assert_eq!(
            injected_skills.len(),
            MAX_SKILLS_PER_INJECTION,
            "quota must cap skills at {MAX_SKILLS_PER_INJECTION}"
        );
        let quota_skips = injection
            .skipped
            .iter()
            .filter(|s| s.reason == InjectSkipReason::SkillQuotaExceeded)
            .count();
        assert_eq!(quota_skips, 1, "3 skills - quota 2 = 1 reported skip");
    }

    /// Regression: in a small store every skill also floats in through the
    /// generic search; the quota must not let those floats bump the ONE
    /// trigger-matched skill — explicit matches reserve quota slots first.
    #[tokio::test]
    async fn test_explicit_skill_survives_quota_over_generic_floats() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        make_skill(&store, "deploy runbook", "deploy").await;
        make_skill(&store, "migrate runbook", "migrate").await;
        make_skill(&store, "upgrade runbook", "upgrade").await;
        let router = MemoryRouter::new(store.clone());

        let injection = router
            .session_start(
                "claude-desktop",
                Some("migrate the users table to the new schema"),
                None,
            )
            .await
            .unwrap();

        let injected_skills: Vec<_> = injection
            .results
            .iter()
            .filter(|r| is_skill(&r.memory))
            .collect();
        assert!(
            injected_skills.len() <= MAX_SKILLS_PER_INJECTION,
            "quota must still cap total skills"
        );
        assert!(
            injected_skills.iter().any(|r| {
                r.memory.content == "migrate runbook"
                    && r.memory
                        .instruction
                        .as_deref()
                        .is_some_and(|i| i.contains("[SKILL:"))
            }),
            "the trigger-matched skill must survive the quota with its structured block"
        );
    }

    #[tokio::test]
    async fn test_skill_without_trigger_never_matches() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Skill,
            "triggerless skill".to_string(),
            Priority::Reference,
            agent,
        );
        mem.skill_meta = Some(SkillMeta {
            trigger: None,
            steps: vec!["x".to_string()],
            verification: None,
            version: 1,
        });
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let router = MemoryRouter::new(store.clone());
        let injection = router
            .session_start("claude-desktop", Some("deploy everything"), None)
            .await
            .unwrap();
        // The skill may still surface via generic search, but its instruction
        // must NOT be the structured SKILL block (no trigger to match on).
        if let Some(r) = injection.results.iter().find(|r| r.memory.id == id) {
            assert!(
                !r.memory
                    .instruction
                    .as_deref()
                    .unwrap_or("")
                    .contains("[SKILL:")
            );
        }
    }

    // --- C5 relation expansion on injection ---

    #[tokio::test]
    async fn test_format_injection_appends_relations() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // An entity memory with one outgoing relation.
        let mut entity = Memory::new(
            MemoryType::Entity,
            "dashboard service".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "t".to_string(),
                agent_type: "g".to_string(),
                session_id: None,
            },
        );
        entity.tags = vec!["entity".to_string()];
        let entity_id = entity.id.clone();
        store.save(entity).await.unwrap();
        store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: entity_id.clone(),
                predicate: "depends_on".to_string(),
                object_id: None,
                object_text: Some("PostgreSQL".to_string()),
                confidence: 0.8,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        let router = MemoryRouter::new(store.clone());
        let output = SessionStartOutput {
            injected: vec![SearchResult {
                memory: store.get(&entity_id).await.unwrap(),
                score: 1.0,
                hit_sources: Vec::new(),
            }],
            overflow_count: 0,
            overflow_summaries: vec![],
            skipped: vec![],
            conflicts: vec![],
        };

        let with_relations = router.format_injection_with_relations(&output).await;
        assert!(with_relations.contains("[RELATIONS]"));
        assert!(with_relations.contains("dashboard service —depends_on→ PostgreSQL"));

        // Without relations the plain formatter has no [RELATIONS] block.
        let plain = format::format_layered_instructions(&output);
        assert!(!plain.contains("[RELATIONS]"));
    }

    // --- Phase D team shared pool ---

    #[tokio::test]
    async fn test_shared_memory_injected_across_namespaces() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // A team-pool memory living in a namespace that is neither the
        // session's project namespace nor "global".
        let mut shared = Memory::new(
            MemoryType::Fact,
            "all services must use the internal registry".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "t".to_string(),
                agent_type: "g".to_string(),
                session_id: None,
            },
        );
        shared.namespace = "team:infra".to_string();
        shared.visibility = Visibility::Shared;
        store.save(shared).await.unwrap();

        // A project-scoped memory that must NOT leak into another project.
        let mut other_project = Memory::new(
            MemoryType::Fact,
            "project beta secret".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "t".to_string(),
                agent_type: "g".to_string(),
                session_id: None,
            },
        );
        other_project.namespace = "project:beta".to_string();
        store.save(other_project).await.unwrap();

        let router = MemoryRouter::new(store.clone());
        let injection = router
            .session_start(
                "claude-desktop",
                Some("help me with project alpha"),
                Some("alpha"),
            )
            .await
            .unwrap();

        let injected_contents: Vec<&str> = injection
            .results
            .iter()
            .map(|r| r.memory.content.as_str())
            .collect();
        assert!(
            injected_contents.contains(&"all services must use the internal registry"),
            "shared memory must be injected into any session"
        );
        assert!(
            !injected_contents.contains(&"project beta secret"),
            "a scoped memory from another project must not leak"
        );
    }

    #[tokio::test]
    async fn test_scoped_memory_not_in_shared_list() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut scoped = Memory::new(
            MemoryType::Fact,
            "scoped fact".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "t".to_string(),
                agent_type: "g".to_string(),
                session_id: None,
            },
        );
        scoped.visibility = Visibility::Scoped;
        store.save(scoped).await.unwrap();

        assert!(
            store.list_shared(10).await.unwrap().is_empty(),
            "scoped memories must not appear in the shared pool"
        );
    }

    #[tokio::test]
    async fn test_deterministic_injection_returns_only_must() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        store
            .save(Memory::new(
                MemoryType::Preference,
                "always use Rust".to_string(),
                Priority::Must,
                agent.clone(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "some reference fact".to_string(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let router = MemoryRouter::new(store);
        let results = router
            .deterministic_injection("some-agent", None)
            .await
            .unwrap();

        assert!(
            !results.is_empty(),
            "MUST memories must be resolved deterministically"
        );
        assert!(
            results.iter().all(|r| r.memory.priority == Priority::Must),
            "only MUST memories belong on the deterministic fast path"
        );
        assert!(
            results
                .iter()
                .any(|r| r.memory.content == "always use Rust")
        );
    }

    // —— Feature F: canonical injection channel ——

    fn profile_with_channel(id: &str, channel: Option<InjectChannel>) -> AgentProfile {
        AgentProfile {
            id: id.to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: None,
            inject_channel: channel,
        }
    }

    #[tokio::test]
    async fn test_channel_unset_allows_every_channel() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router =
            MemoryRouter::with_registry(store, vec![profile_with_channel("open-agent", None)]);
        assert!(router.channel_allows("open-agent", InjectChannel::Mcp));
        assert!(router.channel_allows("open-agent", InjectChannel::Proxy));
        assert!(router.channel_allows("open-agent", InjectChannel::Sync));
        assert_eq!(router.inject_channel_for("open-agent"), None);
    }

    #[tokio::test]
    async fn test_channel_set_restricts_to_canonical() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::with_registry(
            store,
            vec![profile_with_channel(
                "proxy-agent",
                Some(InjectChannel::Proxy),
            )],
        );
        assert!(!router.channel_allows("proxy-agent", InjectChannel::Mcp));
        assert!(router.channel_allows("proxy-agent", InjectChannel::Proxy));
        assert!(!router.channel_allows("proxy-agent", InjectChannel::Sync));
    }

    #[tokio::test]
    async fn test_unknown_agent_defaults_to_unrestricted() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        // No registry entry -> default profile -> no channel restriction.
        assert!(router.channel_allows("any-agent", InjectChannel::Mcp));
        assert!(router.channel_allows("any-agent", InjectChannel::Proxy));
    }

    #[test]
    fn test_inject_channel_parse_roundtrip() {
        assert_eq!(InjectChannel::parse("mcp"), Some(InjectChannel::Mcp));
        assert_eq!(InjectChannel::parse("proxy"), Some(InjectChannel::Proxy));
        assert_eq!(InjectChannel::parse("sync"), Some(InjectChannel::Sync));
        assert_eq!(InjectChannel::parse("file"), Some(InjectChannel::Sync));
        assert_eq!(InjectChannel::parse("nope"), None);
        assert_eq!(InjectChannel::Mcp.as_str(), "mcp");
        assert_eq!(InjectChannel::Proxy.as_str(), "proxy");
        assert_eq!(InjectChannel::Sync.as_str(), "sync");
    }

    #[test]
    fn test_registry_yaml_parses_inject_channel() {
        let yaml = r#"
agents:
  - id: proxy-agent
    agent_type: coding-assistant
    description: "injected via the transparent proxy"
    inject_channel: proxy
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global"]
      exclude_types: []
  - id: open-agent
    agent_type: coding-assistant
    description: "no channel restriction"
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global"]
      exclude_types: []
"#;
        let config: AgentRegistryConfig = serde_yaml::from_str(yaml).unwrap();
        let proxy_agent = config
            .agents
            .iter()
            .find(|a| a.id == "proxy-agent")
            .unwrap();
        assert_eq!(proxy_agent.inject_channel, Some(InjectChannel::Proxy));
        let open_agent = config.agents.iter().find(|a| a.id == "open-agent").unwrap();
        assert_eq!(open_agent.inject_channel, None);
    }
    #[test]
    fn project_namespace_canonicalizes_spelling() {
        assert_eq!(
            project_namespace(Some("MemVault")),
            Some("project:memvault".to_string())
        );
        assert_eq!(
            project_namespace(Some("project:MemVault/")),
            Some("project:memvault".to_string())
        );
        assert_eq!(project_namespace(None), None);
        // Already-canonical spellings are returned unchanged.
        assert_eq!(
            project_namespace(Some("project:my-app")),
            Some("project:my-app".to_string())
        );
    }

    #[tokio::test]
    async fn test_session_start_matches_canonical_project_spelling() {
        // Regression: a session in directory `MemVault` (canonical project
        // namespace `project:memvault`) must inject memories that were saved
        // under `project:MemVault`/`project:memvault` alike — a spelling
        // mismatch used to silently orphan the memory from injection.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Fact,
            "saved under mixed case".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m1.namespace = "project:MemVault".to_string();
        m1.tags = vec!["coding".to_string()];

        let mut m2 = Memory::new(
            MemoryType::Fact,
            "saved under canonical".to_string(),
            Priority::Reference,
            agent,
        );
        m2.namespace = "project:memvault".to_string();
        m2.tags = vec!["coding".to_string()];

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let router = MemoryRouter::with_registry(store, Vec::new());

        let output = router
            .session_start("project-agent", None, Some("MemVault"))
            .await
            .unwrap();
        assert_eq!(
            output.results.len(),
            2,
            "both spellings must inject together"
        );
        assert!(
            output
                .results
                .iter()
                .any(|r| r.memory.content == "saved under mixed case"),
            "mixed-case save must be retrievable from a canonical session"
        );
    }
}
