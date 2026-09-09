use std::path::Path;
use std::sync::Arc;

mod format;

use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

use chrono::Utc;

use crate::agent_adapt;
use crate::auth::{AgentAuth, AgentCredentials};
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
    !matches!(
        std::env::var("MEMVAULT_IDENTITY_VERIFICATION")
            .as_deref()
            .map(str::to_lowercase),
        Ok(v) if v == "off" || v == "0" || v == "false" || v == "disabled"
    )
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

/// Normalize a caller-supplied project identifier into a project namespace.
///
/// Callers are inconsistent: the proxy's `SessionContext::get_project()`
/// already returns a `"project:*"`-prefixed namespace, while other call sites
/// pass the bare project name. Prefix exactly once so neither form produces a
/// malformed `"project:project:*"` namespace.
pub fn project_namespace(project: Option<&str>) -> Option<String> {
    project.map(|p| {
        if p.starts_with("project:") {
            p.to_string()
        } else {
            format!("project:{}", p)
        }
    })
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

    pub fn load_registry_from_yaml(store: Arc<dyn MemoryStore>, path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::error::MemVaultError::Storage(format!("Failed to read agent registry: {}", e))
        })?;

        let mut config: AgentRegistryConfig = serde_yaml::from_str(&content).map_err(|e| {
            crate::error::MemVaultError::InvalidInput(format!("Invalid agent registry YAML: {}", e))
        })?;

        // Hash api_keys for secure in-memory storage
        config.hash_api_keys_in_place();

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
    /// Feature F (docs/PAPER-INSPIRATIONS.md). `None` means the agent placed
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

    pub async fn session_start(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
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

        let namespace = project_namespace(project).or_else(|| {
            profile
                .inject_rules
                .namespace_filter
                .first()
                .filter(|ns| *ns != "project:*")
                .cloned()
        });

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
        if namespace
            .as_deref()
            .is_some_and(|ns| ns != "global" && ns != "project:*")
        {
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
    /// (docs/PAPER-INSPIRATIONS.md): the agent-visible MUST-level memories,
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
        Ok(out)
    }

    pub async fn confirm_read(&self, ids: &[String]) -> Result<()> {
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
        let profile = self.get_agent_profile(agent_id);

        // Fetch more candidates than needed for layered selection
        let injection = self.session_start(agent_id, context_hint, project).await?;
        let all_results = injection.results;

        // Also fetch overflow candidates that were trimmed
        let extended_query = SearchQuery {
            query: context_hint.unwrap_or("").to_string(),
            agent_id: Some(agent_id.to_string()),
            namespace: project_namespace(project),
            top_k: profile.inject_rules.max_memories * 3,
            ..SearchQuery::new(String::new())
        };
        let extended = self.store.search(extended_query).await?.results;

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
            if r.score > 0.1 {
                overflow.push(r);
            } else if !already_skipped.contains(&r.memory.id) {
                skipped.push(SkippedMemory {
                    id: r.memory.id.clone(),
                    reason: InjectSkipReason::BelowScoreFloor,
                });
            }
        }

        let overflow_count = overflow.len();
        let overflow_summaries: Vec<String> = overflow
            .iter()
            .take(5)
            .map(|r| Self::make_summary(&r.memory))
            .collect();

        let injected_ids: Vec<String> = all_results.iter().map(|r| r.memory.id.clone()).collect();
        let conflicts = crate::evidence::contradictions_among(&*self.store, &injected_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(memory_id, conflicting_with)| ConflictNotice {
                memory_id,
                conflicting_with,
            })
            .collect();

        Ok(SessionStartOutput {
            injected: all_results,
            overflow_count,
            overflow_summaries,
            skipped,
            conflicts,
        })
    }

    fn make_summary(memory: &Memory) -> String {
        format::make_summary(memory)
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
        assert!(formatted.contains("[MUST]"));
        assert!(formatted.contains("[MEMORY CONTEXT"));
    }

    #[tokio::test]
    async fn test_mcp_resource() {
        let router = setup().await;
        let content = router
            .get_mcp_resource_content("memory://user-profile")
            .await
            .unwrap();
        assert!(content.contains("[MUST]"));
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

        assert!(formatted.contains("[MUST] must rule"));
        assert!(formatted.contains("[REF] ref content"));
        assert!(formatted.contains("[BG] bg info"));
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
        assert!(content.contains("[REF]"));
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
        assert!(formatted.contains("[MUST]"));
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
        assert!(formatted.contains("[MUST]"));
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
}
