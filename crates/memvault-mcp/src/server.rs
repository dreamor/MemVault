use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::service::{MaybeSendFuture, NotificationContext};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use memvault_core::compliance::ComplianceStore;
use memvault_core::embedding::{EMBED_WRITE_WAIT, LazyEmbedder};
use memvault_core::forget::{ForgetConfig, ForgetCriteria, ForgetManager, ForgetOptions};
use memvault_core::hybrid::HybridMerger;
use memvault_core::llm_extractor::{LazyLlmExtractor, LlmExtractor};
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::rerank::{MultiSignalReranker, RerankConfig};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::writer::{MemoryWriter, WriteOutcome};

#[derive(Clone)]
pub struct MemVaultMcp {
    store: Arc<dyn MemoryStore>,
    router: Arc<MemoryRouter>,
    embedder: LazyEmbedder,
    compliance: Option<Arc<ComplianceStore>>,
    /// Deduplicates embedding backfills across every trigger (boot
    /// `on_initialized`, later `session_start`s): concurrent runs would each
    /// re-embed the same vectorless memories. Shared for the server's
    /// lifetime rather than recreated per trigger.
    backfill_guard: Arc<std::sync::atomic::AtomicBool>,
    reranker: MultiSignalReranker,
    /// Contextual (LLM-based) extractor holder for `extract_memories`
    /// mode="llm" and outcome reflection. `new()` defaults to a fixed
    /// `None`; production entrypoints install
    /// [`LazyLlmExtractor::from_env`] so a boot-time probe failure no
    /// longer disables llm extraction for the process lifetime.
    llm_extractor: LazyLlmExtractor,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

// --- Tool parameter structs ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    /// The content of the memory to save
    pub content: String,
    /// Priority level: MUST, REFERENCE, or BACKGROUND
    #[serde(default = "default_priority_str")]
    pub priority: String,
    /// Memory type: preference, fact, episode, entity, or skill
    #[serde(default = "default_type_str")]
    pub r#type: String,
    /// Namespace for the memory (default: global)
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Optional instruction format for the memory
    pub instruction: Option<String>,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
    /// ID of the agent saving this memory
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Type of the agent (e.g., coding-assistant)
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Confidence score (0.0 to 1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Memory layer: L0 (raw), L1 (atom), L2 (scenario), L3 (persona). Auto-assigned if omitted.
    pub layer: Option<String>,
    /// Sharing scope: "scoped" (default, namespace rules) or "shared" (team pool).
    pub visibility: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
    /// Skill trigger pattern (only for type=skill)
    pub skill_trigger: Option<String>,
    /// Force insert, skipping delta-write dedup/merge. By default a save is
    /// checked against similar memories in the same namespace: near-duplicates
    /// are skipped, similar memories absorb the new content's residual
    /// (docs/DESIGN.md §17.1).
    #[serde(default)]
    pub force_insert: bool,
    /// Skill execution steps (only for type=skill)
    #[serde(default)]
    pub skill_steps: Vec<String>,
    /// Skill verification criteria (only for type=skill)
    pub skill_verification: Option<String>,
}

fn default_priority_str() -> String {
    "REFERENCE".to_string()
}
fn default_type_str() -> String {
    "fact".to_string()
}
fn default_namespace() -> String {
    "global".to_string()
}
fn default_agent_id() -> String {
    "unknown".to_string()
}
fn default_agent_type() -> String {
    "general-assistant".to_string()
}
fn default_confidence() -> f64 {
    0.8
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordOutcomeParams {
    /// What task was executed (kept verbatim for retrieval)
    pub task: String,
    /// Outcome status: success, failure, or partial
    pub status: String,
    /// Attribution of the outcome, when known
    pub cause: Option<String>,
    /// Coarse task category for lesson matching (e.g. deploy, debug, refactor)
    pub task_type: Option<String>,
    /// ID of the skill memory the agent followed, if any — attributes this
    /// outcome to the skill's success/failure statistics
    pub skill_id: Option<String>,
    /// Namespace for the episode memory (default: global)
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
    /// ID of the agent reporting this outcome
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Type of the agent (e.g., coding-assistant)
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Session the task ran in, for traceability
    pub session_id: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

/// Host-agnostic outcome evidence (mirrors `memvault_core::outcome_inference::OutcomeEvidence`).
/// For hosts whose event model doesn't expose a Claude-Code-shaped JSONL
/// transcript (e.g. dsh/Cordis), the caller computes these counts itself
/// from whatever structured events it already has (tool-call results, user
/// turns) instead of flattening them into fake transcript text.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutcomeEvidenceParams {
    /// What the agent was trying to do, from the host's own turn history.
    /// `None`/absent always skips inference — there is nothing to anchor an
    /// episode to.
    pub task: Option<String>,
    /// Count of consecutive-tool-call-error retry streaks observed (a
    /// single error does not count; a success resets the streak).
    #[serde(default)]
    pub retry_hits: u32,
    /// Count of tool-call rejection phrases observed.
    #[serde(default)]
    pub rejection_hits: u32,
    /// Count of user correction/interruption turns observed (not counting
    /// the opening prompt).
    #[serde(default)]
    pub correction_hits: u32,
    /// How the session's last recognized event looked: "unresolved_tool_error"
    /// | "unresolved_correction" | "resolved" (default when absent/unrecognized)
    #[serde(default)]
    pub terminal: String,
    /// Detail behind an unresolved terminal state (the specific error or
    /// correction text) — used verbatim as the rule-floor's `cause` when present.
    pub terminal_detail: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InferOutcomeParams {
    /// Structural evidence computed by the caller from its own event stream
    pub evidence: OutcomeEvidenceParams,
    /// Conversation excerpt handed to the LLM judge, if one is configured
    /// (DATA to analyze, not instructions). Optional — when empty, only the
    /// rule-based floor runs.
    #[serde(default)]
    pub judge_context: String,
    /// Namespace for the episode memory (default: global)
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Tags for categorization — "auto-inferred" and "inferred:rule"/"inferred:llm"
    /// are added automatically
    #[serde(default)]
    pub tags: Vec<String>,
    /// ID of the agent reporting this outcome
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Type of the agent (e.g., coding-assistant)
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Session the task ran in, for traceability
    pub session_id: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    /// Search query string
    pub query: String,
    /// Search mode: "hybrid" (default; keyword + vector via RRF), "keyword"
    /// (text-only), or "semantic" (vector-only). Semantic/hybrid fall back to
    /// keyword when embeddings are unavailable.
    #[serde(default = "default_search_mode")]
    pub mode: String,
    /// Maximum number of results to return
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Filter by namespace
    pub namespace: Option<String>,
    /// Filter by memory type
    pub type_filter: Option<String>,
    /// Filter by priority
    pub priority_filter: Option<String>,
    /// ID of the requesting agent
    pub agent_id: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
    /// Attach each result's one-hop relation neighborhood (semantic graph
    /// expansion, C5). Default off.
    #[serde(default)]
    pub expand_relations: bool,
    /// Drop results whose final (post-rerank) relevance score is below this
    /// threshold. Unset = return the full top_k regardless of score, the
    /// pre-existing behavior. Lets callers distinguish "no confident match"
    /// (empty result) from "weak matches padded to top_k".
    #[serde(default)]
    pub min_score: Option<f64>,
}

fn default_search_mode() -> String {
    "hybrid".to_string()
}
fn default_top_k() -> usize {
    10
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionStartParams {
    /// ID of the agent starting the session
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Type of the agent
    #[serde(default = "default_agent_type")]
    #[allow(dead_code)]
    pub agent_type: String,
    /// Hint about the current conversation context
    pub context_hint: Option<String>,
    /// Current project name
    pub project: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewMemoryParams {
    /// ID of the memory to review
    pub memory_id: String,
    /// Action: approve, reject, or edit
    pub action: String,
    /// New content if action is edit
    pub edited_content: Option<String>,
    /// New instruction if action is edit
    pub edited_instruction: Option<String>,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMemoryParams {
    /// ID of the memory to delete
    pub memory_id: String,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetMemoryParams {
    /// ID of a single memory to permanently erase. Mutually exclusive with
    /// the criteria fields below — when set, every criteria field is
    /// ignored.
    pub memory_id: Option<String>,
    /// Bulk: namespace starts with this (e.g. "archived:")
    pub namespace_prefix: Option<String>,
    /// Bulk: exact namespace match
    pub namespace: Option<String>,
    /// Bulk: source agent id
    pub source_agent_id: Option<String>,
    /// Bulk: memories carrying this tag
    pub tag: Option<String>,
    /// Bulk: memory type (preference, fact, episode, entity, skill)
    pub memory_type: Option<String>,
    /// Bulk: not updated in this many days
    pub before_days: Option<i64>,
    /// Explicit override to include Priority::Must memories (otherwise
    /// excluded by default)
    #[serde(default)]
    pub allow_must: bool,
    /// Audit-trail reason recorded in the forgotten-log
    pub reason: Option<String>,
    /// Preview matches without purging anything (default: true — forgetting
    /// is irreversible, so a caller must explicitly set this to false to
    /// execute)
    #[serde(default = "default_forget_dry_run")]
    pub dry_run: bool,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_forget_dry_run() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExtractMemoriesParams {
    /// Text to extract memories from (conversation content, e.g. the user's turn)
    pub text: String,
    /// Extraction mode: "rule" (default, keyword pattern matching) or "llm"
    /// (semantic extraction over the full context; uses the configured LLM
    /// extraction provider, falling back to rule-based when none is
    /// configured or the LLM call fails — see fallback_used in the result).
    pub mode: Option<String>,
    /// Optional paired assistant/response text for mode="llm" — passing
    /// both sides of the exchange lets the model resolve references and
    /// implicit preferences a single text can't.
    pub assistant_text: Option<String>,
    /// Whether to auto-save extracted memories
    #[serde(default)]
    pub auto_save: bool,
    /// Agent ID to attribute saved memories to
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
    /// When the extracted facts actually happened in the source
    /// conversation (RFC 3339), stored as `occurred_at` on saved memories —
    /// distinct from `created_at` (ingestion time). Invalid input is an
    /// error, never a silent drop.
    #[serde(default)]
    pub occurred_at: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportSkillsParams {
    /// Markdown SOP text. Each #/## heading becomes a skill; `trigger:` /
    /// `verification:` lines and list items become the skill metadata.
    pub markdown: String,
    /// Fallback skill title when the document has no headings.
    #[serde(default = "default_sop_fallback")]
    pub fallback_title: String,
    /// Namespace for the imported skills.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Mark imported skills as human-reviewed (skip the inbox). Default false.
    #[serde(default)]
    pub approve: bool,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_sop_fallback() -> String {
    "imported-sop".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDedupParams {
    /// Namespace to scan (null for all)
    pub namespace: Option<String>,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDecayParams {
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfirmReadParams {
    /// List of memory IDs to confirm as read
    pub memory_ids: Vec<String>,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListInboxParams {
    /// Filter by namespace
    pub namespace: Option<String>,
    /// Maximum number of results to return
    #[serde(default = "default_inbox_limit")]
    pub limit: usize,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_inbox_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunPromoteParams {
    /// Namespace to scan (currently the promote pipeline scans all namespaces)
    pub namespace: Option<String>,
    /// Minimum L1 memories to consolidate into L2 (default: 3)
    pub min_l1: Option<usize>,
    /// Minimum L2 memories to promote to L3 (default: 2)
    pub min_l2: Option<usize>,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddEvidenceParams {
    /// The memory this evidence statement is ABOUT, or that the subject
    /// memory supports/contradicts — see `relation` for direction.
    /// For supports/contradicts: the memory being supported/contradicted.
    /// For sourced_from: the memory whose provenance is recorded.
    pub memory_id: String,
    /// Evidence relation: "supports" | "contradicts" | "sourced_from".
    /// supports/contradicts link two memories (`evidence_id` = the memory
    /// providing evidence); sourced_from records an external source text.
    pub relation: String,
    /// The memory providing evidence (required for supports/contradicts).
    pub evidence_id: Option<String>,
    /// External source: URL, document path, conversation reference
    /// (required for sourced_from).
    pub source: Option<String>,
    /// Evidence confidence, 0.0-1.0 (default 0.8).
    pub confidence: Option<f64>,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoryEvidenceParams {
    /// The memory whose raw-evidence chain (L0 trace rows) to expand
    pub memory_id: String,
    /// ID of the requesting agent
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReportComplianceParams {
    /// The inject_session_id from a previous session_start call
    pub inject_session_id: String,
    /// List of compliance reports, one per memory
    pub reports: Vec<ComplianceReportItem>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComplianceReportItem {
    pub memory_id: String,
    /// Status: "followed", "violated", or "unknown"
    pub status: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetComplianceReportParams {
    /// Get report for a specific injection session
    pub inject_session_id: Option<String>,
    /// Filter aggregate summary by agent
    pub agent_id: Option<String>,
    #[serde(default = "default_compliance_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetEffectivenessReportParams {
    /// Filter by agent
    pub agent_id: Option<String>,
    #[serde(default = "default_compliance_limit")]
    pub limit: usize,
}

fn default_compliance_limit() -> usize {
    10
}

// --- MCP Server implementation ---

#[tool_router]
impl MemVaultMcp {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        router: Arc<MemoryRouter>,
        embedder: LazyEmbedder,
        compliance: Option<Arc<ComplianceStore>>,
    ) -> Self {
        Self {
            store,
            router,
            embedder,
            compliance,
            backfill_guard: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reranker: MultiSignalReranker::new(RerankConfig::default()),
            llm_extractor: LazyLlmExtractor::fixed(None),
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_llm_extractor(mut self, extractor: Arc<dyn LlmExtractor>) -> Self {
        self.llm_extractor = LazyLlmExtractor::fixed(Some(extractor));
        self
    }

    /// Production wiring: hand over a lazily-probed holder instead of a
    /// one-shot startup result (see [`LazyLlmExtractor`]).
    pub fn with_lazy_llm_extractor(mut self, extractor: LazyLlmExtractor) -> Self {
        self.llm_extractor = extractor;
        self
    }

    /// Kick the shared embedding backfill after a bulk raw-store write
    /// (SOP import, extract auto-save) that bypassed the router's
    /// embed-on-save path — without it, memories written by those tools
    /// stay vectorless (invisible to semantic search) until the next
    /// handshake or restart. Uses the same `backfill_guard` as
    /// `on_initialized`/`session_start`, so overlapping triggers dedup
    /// instead of re-embedding the same rows concurrently.
    fn spawn_post_write_backfill(&self) {
        let store = self.store.clone();
        let embedder = self.embedder.clone();
        let guard = self.backfill_guard.clone();
        tokio::spawn(async move {
            if let Some(provider) = embedder.get().await {
                MemoryRouter::spawn_embedding_backfill(store, provider, guard);
            }
        });
    }

    #[tool(
        description = "Save a new memory. Memories are persistent user preferences, facts, episodes, or skills that should be recalled in future conversations. Embeddings are generated automatically for semantic search. Delta-write is on by default: near-duplicate saves are skipped and similar existing memories absorb the new content instead of a new row being created (status in the response: saved | merged | skipped). Pass force_insert=true to bypass. Writes persist immediately; requires agent auth when the agent has a registered key.",
        annotations(
            title = "Save Memory",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        // Authenticate the agent, and record whether its agent_id had a
        // registered API key that was actually checked (vs. unauthenticated
        // mode) — feeds the MUST corroboration gate in
        // `router::format::is_trusted` without changing default behavior.
        let (_, identity_verified) = self
            .router
            .authenticate_agent_verified(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let priority = match params.priority.to_uppercase().as_str() {
            "MUST" => Priority::Must,
            "BACKGROUND" => Priority::Background,
            _ => Priority::Reference,
        };

        let memory_type = match params.r#type.to_lowercase().as_str() {
            "preference" => MemoryType::Preference,
            "episode" => MemoryType::Episode,
            "entity" => MemoryType::Entity,
            "skill" => MemoryType::Skill,
            _ => MemoryType::Fact,
        };

        let mut mem = Memory::new(
            memory_type,
            params.content,
            priority,
            SourceAgent {
                id: params.agent_id,
                agent_type: params.agent_type,
                session_id: None,
            },
        );
        mem.identity_verified = identity_verified;
        mem.namespace = params.namespace;
        mem.instruction = params.instruction;
        mem.tags = params.tags;
        mem.confidence = params.confidence;
        if let Some(ref l) = params.layer {
            mem.layer = match l.to_uppercase().as_str() {
                "L0" => MemoryLayer::L0,
                "L2" => MemoryLayer::L2,
                "L3" => MemoryLayer::L3,
                _ => MemoryLayer::L1,
            };
        }
        if let Some(ref v) = params.visibility {
            mem.visibility = memvault_core::models::Visibility::parse(v);
        }

        if params.skill_trigger.is_some()
            || !params.skill_steps.is_empty()
            || params.skill_verification.is_some()
        {
            mem.skill_meta = Some(SkillMeta {
                trigger: params.skill_trigger,
                steps: params.skill_steps,
                verification: params.skill_verification,
                version: 1,
            });
        }

        let embed_text = mem
            .instruction
            .as_deref()
            .unwrap_or(&mem.content)
            .to_string();
        // Wait (briefly) for a warming-up provider rather than sampling it
        // once: the stdio handshake completes before the lazy `native`
        // provider has finished building, so an immediate first save used to
        // persist a vectorless memory even though the model was already cached
        // and landed milliseconds later. `get_timeout` waits for *readiness*
        // (not a fixed sleep) and never blocks a genuinely cold first-launch
        // download — in that case the record is saved without a vector and the
        // boot backfill fills it in once the model lands (see
        // `on_initialized`/`spawn_embedding_backfill`).
        let mut embedding: Option<Vec<f32>> = None;
        let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        if let Some(ref provider) = provider {
            match provider.embed(std::slice::from_ref(&embed_text)).await {
                Ok(embeddings) if !embeddings.is_empty() => {
                    debug!(id = %mem.id, dim = embeddings[0].len(), "auto-embedded memory");
                    embedding = embeddings.into_iter().next();
                }
                Err(e) => warn!("Auto-embedding failed, saving without: {}", e),
                _ => {}
            }
        }
        let embedded = embedding.is_some();
        // A vector omitted while the provider is (still) expected is pending
        // backfill, not a deliberate keyword-only save — surface that instead
        // of reporting a bare `embedded: false` the caller would read as final.
        let vector_pending = !embedded && self.embedder.expects_embedding().await;

        // Delta write (docs/DESIGN.md §17.1): dedup within the
        // same namespace first — near-duplicates skip, similar memories absorb
        // the residual, force_insert bypasses. Skills (SOPs) are procedural
        // knowledge; merging them into facts would not be meaningful, so they
        // always force-insert.
        let force = params.force_insert || mem.skill_meta.is_some();
        // Reuse the provider resolved above for delta-write dedup: a second
        // `try_get` here could miss the very provider we just waited for and
        // silently degrade dedup to keyword-only. `MemoryWriter::save` backfills
        // a missing vector on a skipped near-duplicate using this same
        // `embedding` when the existing row doesn't have one yet (a retry
        // after the embedder comes online must not leave the old record
        // permanently vectorless).
        let writer = MemoryWriter::new(self.store.clone(), provider.clone());
        let outcome = writer
            .save(mem, embedding, force)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let result = match outcome {
            WriteOutcome::Inserted(saved) => serde_json::json!({
                "status": "saved",
                "id": saved.id,
                "priority": format!("{:?}", saved.priority),
                "embedded": embedded,
                "vector_pending": vector_pending,
            }),
            WriteOutcome::Merged {
                memory,
                similarity,
                residual_added,
            } => serde_json::json!({
                "status": "merged",
                "id": memory.id,
                "priority": format!("{:?}", memory.priority),
                "embedded": embedded,
                "vector_pending": vector_pending,
                "similarity": similarity,
                "residual_added": residual_added,
            }),
            WriteOutcome::Skipped { memory, similarity } => serde_json::json!({
                "status": "skipped",
                "id": memory.id,
                "priority": format!("{:?}", memory.priority),
                "embedded": embedded,
                "vector_pending": vector_pending,
                "similarity": similarity,
            }),
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Record the outcome of a task you just executed (episodic memory). Call this after finishing a task so past successes and failures can inform future sessions. Status must be 'success', 'failure', or 'partial'. Failures with a cause are reflected into lessons that get injected into similar future tasks. Repeated calls for the same session/outcome are deduped rather than creating a second episode. Requires agent auth when the agent has a registered key.",
        annotations(
            title = "Record Task Outcome",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn record_outcome(
        &self,
        Parameters(params): Parameters<RecordOutcomeParams>,
    ) -> Result<CallToolResult, McpError> {
        // Authenticate the agent
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let status = OutcomeStatus::parse(&params.status).ok_or_else(|| {
            McpError::internal_error(
                format!(
                    "invalid status '{}' — expected success, failure, or partial",
                    params.status
                ),
                None,
            )
        })?;

        let input = memvault_core::episode::OutcomeInput {
            task: params.task,
            status,
            cause: params.cause,
            task_type: params.task_type,
            skill_id: params.skill_id,
            tags: params.tags,
            namespace: params.namespace,
            source_agent: SourceAgent {
                id: params.agent_id,
                agent_type: params.agent_type,
                session_id: params.session_id,
            },
        };

        // Same boot-race guard as `save_memory`: wait (bounded) for a warming
        // provider so an outcome recorded right after startup keeps its vector.
        // Backfill covers the cold-download case.
        let embedder = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        let recorded = memvault_core::episode::record_outcome(
            self.store.as_ref(),
            input.clone(),
            embedder.as_deref(),
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // A duplicate (same session/task/status/cause already on record) wrote
        // no new episode — skip reflection/effectiveness so the original
        // outcome's lesson is not distilled or judged a second time.
        let duplicate_of = recorded.duplicate_of.clone();
        let is_duplicate = duplicate_of.is_some();
        let llm = self.llm_extractor.get().await;

        // Reflection: failures/partials get a lesson distilled (LLM when
        // configured, conservative rules otherwise) and linked back to the
        // episode. Best-effort — a reflection hiccup must not lose the outcome.
        let mut lesson_json = serde_json::Value::Null;
        if !is_duplicate {
            match memvault_core::reflection::reflect_and_store(
                self.store.as_ref(),
                &recorded.memory.id,
                input.clone().into(),
                llm.as_deref(),
                embedder.as_deref(),
            )
            .await
            {
                Ok(Some(record)) => {
                    lesson_json = serde_json::json!({
                        "lesson": record.lesson,
                        "source": format!("{:?}", record.source).to_lowercase(),
                        "memory_id": record.lesson_memory.id,
                        "escalation_hint": record.escalation_hint,
                        "recurrence_update": record.recurrence_update.as_ref().map(|u| serde_json::json!({
                            "draft_memory_id": u.draft_memory_id,
                            "old_lesson_memory_id": u.old_lesson_memory_id,
                        })),
                    });
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, "lesson reflection failed; outcome kept"),
            }
        }

        // Effectiveness: pair this outcome against recent pending injections
        // for the same agent and let the LLM judge (when configured) decide
        // useful/neutral/harmful/insufficient_context. Best-effort — no
        // judge configured means 0 judged, never a fabricated verdict.
        let mut effectiveness_judged = 0usize;
        if !is_duplicate && let Some(ref cs) = self.compliance {
            match memvault_core::effectiveness::judge_recent_injections(
                cs,
                self.store.as_ref(),
                llm.as_deref(),
                &input,
            )
            .await
            {
                Ok(n) => effectiveness_judged = n,
                Err(e) => warn!(error = %e, "effectiveness judging failed; outcome kept"),
            }
        }

        let result = serde_json::json!({
            "status": if is_duplicate { "duplicate" } else { "recorded" },
            "id": recorded.memory.id,
            "outcome": recorded.memory.content,
            "embedded": recorded.embedded,
            "duplicate_of": duplicate_of,
            "lesson": lesson_json,
            "effectiveness_judged": effectiveness_judged,
            "flagged_skills": recorded.flagged_skills,
            "skill_draft_id": recorded.skill_draft_id,
            "note": if is_duplicate {
                "outcome already recorded for this session — no duplicate written"
            } else {
                match status {
                    OutcomeStatus::Failure | OutcomeStatus::Partial =>
                        "failure recorded — the lesson will be injected into similar future tasks",
                    OutcomeStatus::Success =>
                        "success recorded — repeated successes may consolidate into a reusable skill",
                }
            },
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Classify and record a task outcome from host-provided evidence rather than raw transcript text — for hosts whose event model doesn't produce a Claude-Code-shaped JSONL transcript (e.g. dsh/Cordis session events). Pass pre-computed friction counts and terminal state in `evidence`; this runs the same rule-floor-plus-optional-LLM-judge classification as the Claude Code Stop hook's `outcome --infer`, then records exactly like record_outcome. Returns status 'skipped' (not an error) when nothing was inferred — a clean session, below the friction threshold, or no task evidence at all. Repeated calls for the same session/evidence are deduped. Requires agent auth when the agent has a registered key.",
        annotations(
            title = "Infer Task Outcome",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn infer_outcome(
        &self,
        Parameters(params): Parameters<InferOutcomeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let terminal = match params.evidence.terminal.as_str() {
            "unresolved_tool_error" => {
                memvault_core::outcome_inference::TerminalState::UnresolvedToolError
            }
            "unresolved_correction" => {
                memvault_core::outcome_inference::TerminalState::UnresolvedCorrection
            }
            _ => memvault_core::outcome_inference::TerminalState::Resolved,
        };
        let evidence = memvault_core::outcome_inference::OutcomeEvidence {
            task: params.evidence.task,
            retry_hits: params.evidence.retry_hits,
            rejection_hits: params.evidence.rejection_hits,
            correction_hits: params.evidence.correction_hits,
            terminal,
            terminal_detail: params.evidence.terminal_detail,
        };

        let llm = self.llm_extractor.get().await;
        let Some(inferred) = memvault_core::outcome_inference::infer_outcome(
            &evidence,
            &params.judge_context,
            llm.as_deref(),
        )
        .await
        else {
            let result = serde_json::json!({
                "status": "skipped",
                "note": "no outcome inferred — clean session, below the friction threshold, or no task evidence",
            });
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&result).unwrap_or_default(),
            )]));
        };

        // Provenance tags: an auto-inferred outcome must stay distinguishable
        // from a human/agent-reported one — same review-queue trust level
        // either way (record_outcome never special-cases provenance), but a
        // reviewer or a future bench filter needs to tell them apart.
        let mut tags = params.tags;
        tags.push("auto-inferred".to_string());
        tags.push(
            match inferred.source {
                memvault_core::outcome_inference::InferenceSource::Rule => "inferred:rule",
                memvault_core::outcome_inference::InferenceSource::Llm => "inferred:llm",
            }
            .to_string(),
        );

        let input = memvault_core::episode::OutcomeInput {
            task: inferred.task,
            status: inferred.status,
            cause: inferred.cause,
            task_type: inferred.task_type,
            skill_id: None,
            tags,
            namespace: params.namespace,
            source_agent: SourceAgent {
                id: params.agent_id,
                agent_type: params.agent_type,
                session_id: params.session_id,
            },
        };

        // Same boot-race guard as `save_memory`: wait (bounded) for a warming
        // provider so an outcome recorded right after startup keeps its vector.
        // Backfill covers the cold-download case.
        let embedder = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        let recorded = memvault_core::episode::record_outcome(
            self.store.as_ref(),
            input.clone(),
            embedder.as_deref(),
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // A duplicate (same session/task/status/cause already on record) wrote
        // no new episode — skip reflection/effectiveness so the original
        // outcome's lesson is not distilled a second time. This mirrors the
        // guard in `record_outcome`; without it a repeated `infer_outcome`
        // left the episode id unchanged but appended a byte-identical lesson
        // (and orphaned the first one via `update_episode_lesson`).
        let duplicate_of = recorded.duplicate_of.clone();
        let is_duplicate = duplicate_of.is_some();

        let mut lesson_json = serde_json::Value::Null;
        if !is_duplicate {
            match memvault_core::reflection::reflect_and_store(
                self.store.as_ref(),
                &recorded.memory.id,
                input.clone().into(),
                llm.as_deref(),
                embedder.as_deref(),
            )
            .await
            {
                Ok(Some(record)) => {
                    lesson_json = serde_json::json!({
                        "lesson": record.lesson,
                        "source": format!("{:?}", record.source).to_lowercase(),
                        "memory_id": record.lesson_memory.id,
                        "escalation_hint": record.escalation_hint,
                    });
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, "lesson reflection failed; inferred outcome kept"),
            }
        }

        let mut effectiveness_judged = 0usize;
        if !is_duplicate && let Some(ref cs) = self.compliance {
            match memvault_core::effectiveness::judge_recent_injections(
                cs,
                self.store.as_ref(),
                llm.as_deref(),
                &input,
            )
            .await
            {
                Ok(n) => effectiveness_judged = n,
                Err(e) => warn!(error = %e, "effectiveness judging failed; inferred outcome kept"),
            }
        }

        let result = serde_json::json!({
            "status": if is_duplicate { "duplicate" } else { "recorded" },
            "id": recorded.memory.id,
            "outcome": recorded.memory.content,
            "duplicate_of": duplicate_of,
            "inference_source": format!("{:?}", inferred.source).to_lowercase(),
            "inference_confidence": inferred.confidence,
            "lesson": lesson_json,
            "effectiveness_judged": effectiveness_judged,
            "flagged_skills": recorded.flagged_skills,
            "note": if is_duplicate {
                "outcome already recorded for this session — no duplicate lesson written"
            } else {
                "outcome recorded — a failure/partial distills a lesson for similar future tasks"
            },
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Search stored memories. mode defaults to 'hybrid' (keyword + vector via RRF); 'keyword' is text-only and 'semantic' is vector-only. Semantic/hybrid need an embedding provider — when embeddings are unavailable the query falls back to keyword and each result reports search_mode='keyword'. Supports namespace, type_filter, priority_filter, top_k (default 10), min_score, and optional expand_relations for one-hop relation context. Read-only; does not mutate memories.",
        annotations(
            title = "Search Memories",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        // Authenticate the agent
        if let Some(ref agent_id) = params.agent_id {
            self.router
                .authenticate_agent(agent_id, params.api_key.as_deref())
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }

        let type_filter = params
            .type_filter
            .and_then(|t| match t.to_lowercase().as_str() {
                "preference" => Some(MemoryType::Preference),
                "fact" => Some(MemoryType::Fact),
                "episode" => Some(MemoryType::Episode),
                "entity" => Some(MemoryType::Entity),
                "skill" => Some(MemoryType::Skill),
                _ => None,
            });

        let priority_filter =
            params
                .priority_filter
                .and_then(|p| match p.to_uppercase().as_str() {
                    "MUST" => Some(Priority::Must),
                    "REFERENCE" => Some(Priority::Reference),
                    "BACKGROUND" => Some(Priority::Background),
                    _ => None,
                });

        let mode = params.mode.to_lowercase();

        // Vector modes get the same bounded readiness wait as the write
        // path (`EMBED_WRITE_WAIT`): a semantic/hybrid query landing while
        // the provider is still warming up used to sample readiness once
        // (`try_get`) and fail with -32602 — an error shape that reads as
        // "caller passed bad params" and vanished on retry. Waiting joins
        // the single-flight build; only a genuinely cold first launch
        // (model download) exceeds the budget, and that case degrades
        // honestly instead of failing.
        let embedder_ready = if mode != "keyword" {
            self.embedder.get_timeout(EMBED_WRITE_WAIT).await
        } else {
            None
        };
        let mut actual_mode = mode.as_str();
        if (mode == "semantic" || mode == "hybrid") && embedder_ready.is_none() {
            if mode == "semantic" && !self.embedder.expects_embedding().await {
                // Keyword-only by configuration (provider deliberately off)
                // — a permanent fact worth an explicit error, not a
                // transient race to paper over.
                return Err(McpError::invalid_params(
                    "Semantic search requires an embedding provider. Set OPENAI_API_KEY or use mode='keyword'.".to_string(),
                    None,
                ));
            }
            // Still warming up after the bounded wait: degrade honestly —
            // every result reports `search_mode: "keyword"`.
            actual_mode = "keyword";
        }

        // keyword search
        let keyword_results = if actual_mode != "semantic" {
            let query = SearchQuery {
                query: params.query.clone(),
                agent_id: params.agent_id.clone(),
                type_filter: type_filter.clone(),
                priority_filter: priority_filter.clone(),
                namespace: params.namespace.clone(),
                top_k: params.top_k,
                token_budget: None,
                expand_relations: false,
            };
            self.store
                .search(query)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .results
        } else {
            Vec::new()
        };

        // vector search — `embedder_ready` was resolved (with the bounded
        // warm-up wait) before the mode dispatch above, so a vector mode
        // reaching this branch always has a provider.
        let vector_results = if actual_mode != "keyword" {
            if let Some(ref embedder) = embedder_ready {
                match embedder.embed(std::slice::from_ref(&params.query)).await {
                    Ok(embeddings) if !embeddings.is_empty() => self
                        .store
                        .vector_search(&embeddings[0], params.top_k, params.namespace.as_deref())
                        .await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?,
                    Err(e) => {
                        warn!("Semantic search embedding failed: {}", e);
                        actual_mode = "keyword";
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let results = match actual_mode {
            "keyword" => keyword_results,
            "semantic" => vector_results,
            _ => HybridMerger::merge(keyword_results, vector_results, params.top_k, 0.4, 0.6),
        };

        debug!(mode = actual_mode, count = results.len(), "search complete");

        // Router's session_start already reranks; search_memory used to skip
        // straight from merge to output, so this tool never got the
        // overlap/recency/authority-tier signals — only raw FTS/RRF order.
        let results = self
            .reranker
            .rerank(&params.query, results, &chrono::Utc::now());

        // Confidence floor: applied to the final reranked scores (mirrors
        // REST /api/search min_score), so callers can tell "nothing
        // relevant" from "top_k padded with weak matches".
        let results = match params.min_score {
            Some(min) => results.into_iter().filter(|r| r.score >= min).collect(),
            None => results,
        };

        // C5: optionally expand each result's one-hop relation neighborhood.
        let mut relations_by_id: std::collections::HashMap<String, Vec<serde_json::Value>> =
            std::collections::HashMap::new();
        if params.expand_relations {
            for r in &results {
                let rels =
                    memvault_core::relations::collect_relations(self.store.as_ref(), &r.memory.id)
                        .await;
                if !rels.is_empty() {
                    relations_by_id.insert(
                        r.memory.id.clone(),
                        rels.iter()
                            .map(|rel| {
                                serde_json::json!({
                                    "subject_id": rel.subject_id,
                                    "predicate": rel.predicate,
                                    "object_id": rel.object_id,
                                    "object_text": rel.object_text,
                                    "line": memvault_core::relations::relation_line(
                                        &r.memory.content, rel
                                    ),
                                })
                            })
                            .collect(),
                    );
                }
            }
        }

        let output: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                let mut obj = serde_json::json!({
                    "id": r.memory.id,
                    "content": r.memory.content,
                    "instruction": r.memory.instruction,
                    "priority": format!("{:?}", r.memory.priority),
                    "type": format!("{:?}", r.memory.memory_type),
                    "namespace": r.memory.namespace,
                    "tags": r.memory.tags,
                    "score": r.score,
                    "human_reviewed": r.memory.human_reviewed,
                    "search_mode": actual_mode,
                    // Recall provenance: which path(s) surfaced this memory
                    // and at what rank ("why is this ranked first?").
                    "hit_sources": r.hit_sources.iter().map(|h| h.tag()).collect::<Vec<_>>(),
                });
                if params.expand_relations
                    && let Some(rels) = relations_by_id.get(&r.memory.id)
                {
                    obj["relations"] = serde_json::json!(rels);
                }
                obj
            })
            .collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Start an agent session. Returns relevant memories filtered by agent identity, formatted as MUST/REF instructions ready for injection into the conversation context; overflow memories are summarized with a hint to use search_memory for details. If this agent's canonical inject channel is not MCP, returns a skip explanation instead of injecting. When compliance tracking is enabled, records the injected memories and returns inject_session_id in structured_content for a later report_compliance call. Not read-only; requires agent auth when the agent has a registered key.",
        annotations(
            title = "Start Memory Session",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn session_start(
        &self,
        Parameters(params): Parameters<SessionStartParams>,
    ) -> Result<CallToolResult, McpError> {
        // Authenticate the agent
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Feature F: honor the agent's canonical injection channel. When this
        // agent's memory is delivered by another channel (transparent proxy or
        // synced files), skip the MCP injection so the same memory is not
        // delivered twice.
        if !self
            .router
            .channel_allows(&params.agent_id, InjectChannel::Mcp)
        {
            let canonical = self
                .router
                .inject_channel_for(&params.agent_id)
                .map(|c| c.as_str())
                .unwrap_or("unknown");
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Memory for agent '{}' is injected via the '{}' channel; \
                 skipping MCP session injection to avoid duplication.",
                params.agent_id, canonical
            ))]));
        }

        // Feature D: weight a multi-turn context hint by recency so retrieval
        // is conditioned on the recent context, not a flat string.
        let context_key = params
            .context_hint
            .as_deref()
            .map(memvault_core::query_expand::weight_turns_by_recency);
        let output = self
            .router
            .session_start_layered(
                &params.agent_id,
                context_key.as_deref(),
                params.project.as_deref(),
            )
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Track injected memories for compliance — mirrors the REST
        // `session_start` handler in `rest_api.rs`. `inject_session_id` is
        // returned via `structured_content` (not the text block) so it
        // stays out of the formatted memory context handed to the calling
        // agent's model, while still letting the caller invoke
        // `report_compliance` afterward — the same way REST returns it in
        // the JSON envelope alongside `formatted`.
        let inject_session_id = if let Some(ref cs) = self.compliance {
            let sid = format!("inj_{}", Uuid::new_v4().simple());
            for r in &output.injected {
                let _ = cs
                    .record_injection(&sid, &r.memory.id, &r.memory.priority, &params.agent_id)
                    .await;
            }
            Some(sid)
        } else {
            None
        };

        let formatted = self.router.format_layered_instructions(&output);

        let mut result = if formatted.is_empty() {
            CallToolResult::success(vec![ContentBlock::text(
                "No memories to inject for this session.",
            )])
        } else {
            CallToolResult::success(vec![ContentBlock::text(formatted)])
        };
        if let Some(sid) = inject_session_id {
            result.structured_content = Some(serde_json::json!({ "inject_session_id": sid }));
        }
        Ok(result)
    }

    #[tool(
        description = "Review or edit a memory. action='approve' marks it human-reviewed; action='edit' updates content/instruction and re-embeds or clears the stale vector (this also works on already-approved memories); action='reject' soft-deletes a pending memory and is rejected for already-approved memories (use delete_memory instead). Approved memories rank higher in future injections. Writes immediately; requires agent auth when the agent has a registered key.",
        annotations(
            title = "Review or Edit Memory",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn review_memory(
        &self,
        Parameters(params): Parameters<ReviewMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match params.action.to_lowercase().as_str() {
            "approve" => {
                let mut mem = self
                    .store
                    .get(&params.memory_id)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                mem.human_reviewed = true;
                mem.updated_at = chrono::Utc::now();
                self.store
                    .update(mem)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Memory {} approved.",
                    params.memory_id
                ))]))
            }
            "reject" => {
                let mem = self
                    .store
                    .get(&params.memory_id)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                if mem.human_reviewed {
                    return Err(McpError::invalid_params(
                        format!(
                            "Memory {} is already approved; use delete_memory instead",
                            params.memory_id
                        ),
                        None,
                    ));
                }
                self.store
                    .delete(&params.memory_id)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Memory {} rejected and deleted.",
                    params.memory_id
                ))]))
            }
            "edit" => {
                let mut mem = self
                    .store
                    .get(&params.memory_id)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                let old_text = mem
                    .instruction
                    .clone()
                    .unwrap_or_else(|| mem.content.clone());
                if let Some(content) = params.edited_content {
                    mem.content = content;
                }
                if let Some(instruction) = params.edited_instruction {
                    mem.instruction = Some(instruction);
                }
                mem.human_reviewed = true;
                mem.updated_at = chrono::Utc::now();
                let updated = self
                    .store
                    .update(mem)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                let new_text = updated
                    .instruction
                    .clone()
                    .unwrap_or_else(|| updated.content.clone());
                if new_text != old_text {
                    let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
                    let mut refreshed = false;
                    if let Some(ref provider) = provider
                        && let Ok(vectors) = provider.embed(std::slice::from_ref(&new_text)).await
                        && let Some(emb) = vectors.into_iter().next()
                    {
                        self.store
                            .set_embedding(&updated.id, emb)
                            .await
                            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                        refreshed = true;
                    }
                    if !refreshed {
                        let _ = self.store.clear_embedding(&updated.id).await;
                    }
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Memory {} edited and approved.",
                    params.memory_id
                ))]))
            }
            _ => Err(McpError::invalid_params(
                format!(
                    "Invalid action: {}. Use approve, reject, or edit.",
                    params.action
                ),
                None,
            )),
        }
    }

    #[tool(
        description = "Record evidence for a memory. relation=\"supports\"/\"contradicts\" links evidence_id → memory_id (a memory supporting or contradicting it); relation=\"sourced_from\" records memory_id's external provenance via `source` (URL/document). Writes one relation and returns its relation_id; an identical relation is rejected rather than duplicated. Contradictions accelerate the contradicted memory's decay; superseded or archived evidence stops counting. Requires agent auth when the agent has a registered key.",
        annotations(
            title = "Add Memory Evidence",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn add_evidence(
        &self,
        Parameters(params): Parameters<AddEvidenceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let kind = memvault_core::evidence::EvidenceKind::parse(&params.relation)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

        let (subject_id, object_id) = match kind {
            memvault_core::evidence::EvidenceKind::Supports
            | memvault_core::evidence::EvidenceKind::Contradicts => {
                let Some(evidence_id) = params.evidence_id.as_deref() else {
                    return Err(McpError::invalid_params(
                        format!(
                            "relation '{}' requires evidence_id (the memory providing evidence)",
                            params.relation
                        ),
                        None,
                    ));
                };
                (evidence_id.to_string(), Some(params.memory_id.as_str()))
            }
            memvault_core::evidence::EvidenceKind::SourcedFrom => (params.memory_id.clone(), None),
        };

        let relation = memvault_core::evidence::add_evidence(
            &*self.store,
            &subject_id,
            kind,
            object_id,
            params.source.as_deref(),
            params.confidence.unwrap_or(0.8),
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let object = relation
            .object_id
            .clone()
            .or(relation.object_text.clone())
            .unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Evidence recorded: {} —{}→ {} (relation_id={})",
            relation.subject_id,
            relation.predicate,
            object,
            relation.relation_id.unwrap_or_default()
        ))]))
    }

    #[tool(
        description = "Soft-delete one memory by its ID: removes its live row, FTS entry, and cascaded evidence relations so it is no longer searchable or injected. A full snapshot is written to memory_history first; restore is available through the CLI (`memvault restore`), not through this MCP tool. Use forget_memory for an irreversible GDPR-style purge. Requires agent auth when the agent has a registered key; a second delete of the same ID returns NotFound.",
        annotations(
            title = "Delete Memory (Soft Delete)",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.store
            .delete(&params.memory_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Memory {} deleted.",
            params.memory_id
        ))]))
    }

    #[tool(
        description = "Permanently erase (GDPR-style) one memory by id or a bulk set matched by criteria (namespace/namespace_prefix/source_agent_id/tag/memory_type/before_days). Unlike delete_memory, this also purges memory_history snapshots and any orphaned compliance rows, leaving only a content-hash audit entry in the forgotten-log — irreversible. dry_run defaults to true: call once to preview, then again with dry_run=false to execute. Priority::Must memories are excluded unless allow_must=true. Gated by the same per-agent API-key auth as delete_memory — there is no separate admin tier at the MCP layer.",
        annotations(
            title = "Permanently Forget Memory",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn forget_memory(
        &self,
        Parameters(params): Parameters<ForgetMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Distinguish "absent" (None = no type filter, legitimate) from
        // "present but unknown" (must be rejected). Unknown values used to
        // silently collapse to None — on this hard-delete path a typo'd or
        // model-hallucinated type widened the purge from one type to EVERY
        // type (issue #1). Validation runs before the delete plan is built,
        // so dry_run and execute share the same rejection.
        let memory_type = match params.memory_type.as_deref() {
            None => None,
            Some(t) => match t.trim().to_lowercase().as_str() {
                "preference" => Some(MemoryType::Preference),
                "fact" => Some(MemoryType::Fact),
                "episode" => Some(MemoryType::Episode),
                "entity" => Some(MemoryType::Entity),
                "skill" => Some(MemoryType::Skill),
                other => {
                    return Err(McpError::invalid_params(
                        format!(
                            "unknown memory_type '{other}': expected one of \
                             preference, fact, episode, entity, skill \
                             (omit memory_type to match all types)"
                        ),
                        None,
                    ));
                }
            },
        };

        let mut fm = ForgetManager::new(self.store.clone(), ForgetConfig::default());
        if let Some(compliance) = &self.compliance {
            fm = fm.with_compliance(compliance.clone());
        }

        let scope = if params.memory_id.is_some() {
            ForgetScope::Manual
        } else {
            ForgetScope::Bulk
        };
        let criteria = ForgetCriteria {
            ids: params.memory_id.into_iter().collect(),
            namespace: params.namespace,
            namespace_prefix: params.namespace_prefix,
            source_agent_id: params.source_agent_id,
            tag: params.tag,
            memory_type,
            updated_before: params
                .before_days
                .map(|d| chrono::Utc::now() - chrono::Duration::days(d)),
            archived_before: None,
        };
        let opts = ForgetOptions {
            scope,
            allow_must: params.allow_must,
            allow_shared: true,
            confirmed: !params.dry_run,
            reason: params.reason,
            requested_by: format!("mcp:{}", params.agent_id),
        };

        let output = if params.dry_run {
            let preview = fm
                .preview(&criteria, &opts)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            serde_json::json!({
                "dry_run": true,
                "matched": preview.matched.iter().map(|c| serde_json::json!({
                    "id": c.id,
                    "namespace": c.namespace,
                    "content_preview": c.content_preview,
                })).collect::<Vec<_>>(),
                "excluded": preview.excluded.iter().map(|c| serde_json::json!({
                    "id": c.id,
                    "reason": format!("{:?}", c.excluded_reason),
                })).collect::<Vec<_>>(),
            })
        } else {
            let report = fm
                .execute(&criteria, &opts)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            serde_json::json!({
                "dry_run": false,
                "purged": report.purged,
                // Matched the criteria but were skipped (e.g. Must-protected
                // without allow_must) — surfaced so a single-id forget that
                // hits a protected memory doesn't look like a silent no-op.
                "excluded": report.excluded,
                "failed": report.failed,
                "compliance_rows_purged": report.compliance_rows_purged,
            })
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Extract structured memories from conversation text. mode=\"rule\" (default) detects preferences, facts, and skills via keyword pattern matching. mode=\"llm\" instead understands the full context (optionally pairing assistant_text) semantically — uses the configured LLM extraction provider, and degrades to rule-based extraction (reported via fallback_used/fallback_reason) when no provider is configured or the LLM call fails. Returns extracted items; with auto_save=true it also writes them and requires agent auth when the agent has a registered key. Invalid occurred_at is an error, never a silent drop.",
        annotations(
            title = "Extract Memories from Text",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn extract_memories(
        &self,
        Parameters(params): Parameters<ExtractMemoriesParams>,
    ) -> Result<CallToolResult, McpError> {
        let mode = params.mode.as_deref().unwrap_or("rule");
        // LLM 契约(llm_extractor.rs trait doc):抽取失败必须非致命,降级到
        // 规则抽取而不是把整段输入静默丢掉。降级如实上报,绝不假装是干净
        // 的 llm 结果。与 REST /api/extract 的 fallback 行为对齐。
        let rule_outcome = || {
            let outcome = memvault_core::extractor::Extractor::extract_with_coverage(&params.text);
            let cov = outcome.coverage;
            let coverage_json = serde_json::json!({
                "input_lines": cov.input_lines,
                "extracted_lines": cov.extracted_lines,
                "no_signal_lines": cov.no_signal_lines,
                "empty_lines": cov.empty_lines,
            });
            (outcome.memories, Some(coverage_json))
        };
        let (extracted, coverage_json, fallback_used, fallback_reason): (
            Vec<memvault_core::extractor::ExtractedMemory>,
            Option<serde_json::Value>,
            bool,
            Option<String>,
        ) = match mode {
            "llm" => {
                // Lazy holder: probes the environment on first use and
                // re-probes after failures — see LazyLlmExtractor.
                let holder = self.llm_extractor.get().await;
                match holder.as_deref() {
                    None => {
                        let reason =
                            "no_llm_extractor_configured: set MEMVAULT_LLM_EXTRACTION_PROVIDER \
                                      (and MEMVAULT_LLM_EXTRACTION_API_KEY / _MODEL as needed); \
                                      fell back to rule-based extraction"
                                .to_string();
                        warn!(%reason, "llm extraction unavailable");
                        let (extracted, coverage_json) = rule_outcome();
                        (extracted, coverage_json, true, Some(reason))
                    }
                    Some(llm) => {
                        let mut context = format!("User: {}", params.text);
                        if let Some(assistant_text) = params
                            .assistant_text
                            .as_deref()
                            .filter(|t| !t.trim().is_empty())
                        {
                            context.push_str("\nAssistant: ");
                            context.push_str(assistant_text);
                        }

                        match llm.extract(&context).await {
                            Ok(extracted) => (extracted, None, false, None),
                            Err(e) => {
                                let reason = format!("llm_extraction_error: {e}");
                                warn!(error = %e, "llm extraction failed; falling back to rule-based");
                                let (extracted, coverage_json) = rule_outcome();
                                (extracted, coverage_json, true, Some(reason))
                            }
                        }
                    }
                }
            }
            _ => {
                let (extracted, coverage_json) = rule_outcome();
                (extracted, coverage_json, false, None)
            }
        };
        // Audit trail: 降级路径产出的记忆必须能和主动选择的 rule 调用区分开。
        let method = if fallback_used {
            "llm_fallback_rule"
        } else {
            mode
        };

        if extracted.is_empty() {
            let mut message = match &coverage_json {
                Some(cov) => format!(
                    "No memories extracted from the provided text. (coverage: {} line(s) in, {} no signal, {} empty)",
                    cov["input_lines"], cov["no_signal_lines"], cov["empty_lines"]
                ),
                None => "No memories extracted from the provided text.".to_string(),
            };
            if let Some(reason) = &fallback_reason {
                message.push_str(&format!(" (note: {reason})"));
            }
            return Ok(CallToolResult::success(vec![ContentBlock::text(message)]));
        }

        let mut saved_ids = Vec::new();

        if params.auto_save {
            self.router
                .authenticate_agent(&params.agent_id, params.api_key.as_deref())
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            // Parse the caller-supplied event date once, before any save: an
            // invalid timestamp is an error, never a silent drop (mirrors
            // REST /api/extract).
            let occurred_at = match params.occurred_at.as_deref().map(str::trim) {
                Some(s) if !s.is_empty() => Some(
                    chrono::DateTime::parse_from_rfc3339(s)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .map_err(|e| {
                            McpError::invalid_params(
                                format!(
                                    "occurred_at must be an RFC 3339 timestamp (e.g. 2026-09-09T12:00:00Z): {e}"
                                ),
                                None,
                            )
                        })?,
                ),
                _ => None,
            };
            // Delta write (docs/DESIGN.md §17.1): route auto-saved candidates
            // through `MemoryWriter` like every other save path. The previous
            // direct `store.save` bypassed dedup, so calling
            // `extract_memories(auto_save=true)` twice on the same text piled up
            // byte-identical rows even though the CLI/`ingest` paths deduped.
            let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
            let writer = MemoryWriter::new(self.store.clone(), provider.clone());
            for e in &extracted {
                let mut mem = Memory::new(
                    e.memory_type.clone(),
                    e.content.clone(),
                    e.priority.clone(),
                    SourceAgent {
                        id: params.agent_id.clone(),
                        agent_type: "extractor".to_string(),
                        session_id: None,
                    },
                );
                mem.instruction = e.instruction.clone();
                mem.tags = e.tags.clone();
                mem.tags.push(format!("method:{method}"));
                mem.confidence = e.confidence;
                mem.occurred_at = occurred_at;

                let embed_text = mem
                    .instruction
                    .clone()
                    .unwrap_or_else(|| mem.content.clone());
                let embedding = match &provider {
                    Some(p) => p
                        .embed(std::slice::from_ref(&embed_text))
                        .await
                        .ok()
                        .and_then(|v| v.into_iter().next()),
                    None => None,
                };
                let outcome = writer
                    .save(mem, embedding, false)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                // A near-duplicate resolves to the row already in the store:
                // record that id so `saved_id`/relations point at the memory
                // that actually carries the content.
                saved_ids.push(outcome.memory().id.clone());
            }
            if !saved_ids.is_empty() {
                self.spawn_post_write_backfill();
            }
        }

        // C2 relation extraction (opt-in, LLM mode only): distill stable
        // domain-knowledge triples from the same context. Best-effort — a
        // relation-extraction failure never fails the memory extraction.
        let mut relations_json = serde_json::Value::Null;
        if mode == "llm"
            && !fallback_used
            && params.auto_save
            && std::env::var("MEMVAULT_RELATIONS")
                .map(|v| memvault_core::env_file::parse_bool(&v) == Some(true))
                .unwrap_or(false)
            && let Some(llm) = self.llm_extractor.get().await
        {
            let mut context = format!("User: {}", params.text);
            if let Some(assistant_text) = params
                .assistant_text
                .as_deref()
                .filter(|t| !t.trim().is_empty())
            {
                context.push_str("\nAssistant: ");
                context.push_str(assistant_text);
            }
            match llm.extract_relations(&context).await {
                Ok(triples) if !triples.is_empty() => {
                    let agent = SourceAgent {
                        id: params.agent_id.clone(),
                        agent_type: "extractor".to_string(),
                        session_id: None,
                    };
                    match memvault_core::relations::store_relation_triples(
                        self.store.as_ref(),
                        &triples,
                        "global",
                        &agent,
                        saved_ids.first().map(|s| s.as_str()),
                    )
                    .await
                    {
                        Ok(stored) => {
                            relations_json = serde_json::json!({
                                "extracted": triples.len(),
                                "stored": stored,
                            });
                        }
                        Err(e) => warn!(error = %e, "relation persistence failed; memories kept"),
                    }
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "relation extraction failed; memories kept"),
            }
        }

        let memories_json: Vec<serde_json::Value> = extracted
            .iter()
            .enumerate()
            .map(|(i, e)| {
                serde_json::json!({
                    "content": e.content,
                    "instruction": e.instruction,
                    "type": format!("{:?}", e.memory_type),
                    "priority": format!("{:?}", e.priority),
                    "tags": e.tags,
                    "confidence": e.confidence,
                    "saved_id": saved_ids.get(i),
                })
            })
            .collect();

        // Coverage travels with the result: how much of the input was covered
        // is part of the answer, not a debug detail. Only meaningful for the
        // line-based rule path (incl. llm→rule fallback); null for a clean
        // mode="llm" result.
        let output = serde_json::json!({
            "mode": mode,
            "coverage": coverage_json,
            "memories": memories_json,
            "relations": relations_json,
            "fallback_used": fallback_used,
            "fallback_reason": fallback_reason,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Import skills from a Markdown SOP document. Each #/## heading becomes a skill (trigger:/verification: metadata lines + list-item steps). Sections without steps are skipped. Imported skills enter the review inbox unless approve=true. Writes new skill memories; repeating the same import can create duplicates. Requires agent auth when the agent has a registered key.",
        annotations(
            title = "Import Skills from SOP",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn import_skills(
        &self,
        Parameters(params): Parameters<ImportSkillsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let parsed = memvault_core::sop::parse_sops(&params.markdown, &params.fallback_title);

        let mut imported = Vec::new();
        for skill in &parsed.skills {
            let mut mem = Memory::new(
                MemoryType::Skill,
                skill.title.clone(),
                Priority::Reference,
                SourceAgent {
                    id: params.agent_id.clone(),
                    agent_type: "importer".to_string(),
                    session_id: None,
                },
            );
            mem.namespace = params.namespace.clone();
            mem.tags = vec!["imported-sop".to_string()];
            mem.human_reviewed = params.approve;
            mem.skill_meta = Some(SkillMeta {
                trigger: skill.trigger.clone(),
                steps: skill.steps.clone(),
                verification: skill.verification.clone(),
                version: 1,
            });
            let saved = self
                .store
                .save(mem)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            imported.push(serde_json::json!({
                "title": skill.title,
                "id": saved.id,
                "steps": skill.steps.len(),
            }));
        }

        if !imported.is_empty() {
            self.spawn_post_write_backfill();
        }

        let output = serde_json::json!({
            "imported": imported,
            "skipped_no_steps": parsed.skipped_no_steps,
            "approved": params.approve,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Scan for duplicate memories and report findings. Uses lexical text similarity (Jaccard) to detect near-duplicates in one namespace or all namespaces. Read-only: reports existing_id/new_content/similarity/action but does not merge or delete anything. Returns at most 20 duplicate findings.",
        annotations(
            title = "Scan for Duplicate Memories",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn run_dedup(
        &self,
        Parameters(params): Parameters<RunDedupParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let dedup = memvault_core::dedup::Deduplicator::new(self.store.clone(), None);
        let result = dedup
            .scan(params.namespace.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "unique_count": result.unique_count,
            "duplicate_count": result.duplicates.len(),
            "duplicates": result.duplicates.iter().take(20).map(|d| {
                serde_json::json!({
                    "existing_id": d.existing_id,
                    "new_content": d.new_content,
                    "similarity": format!("{:.0}%", d.similarity * 100.0),
                    "action": format!("{:?}", d.action),
                })
            }).collect::<Vec<_>>(),
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Run a memory decay cycle: reduces decay_score for old memories and archives memories that fall below the threshold. MUST memories are exempt from decay. This is not purely reversible: if MEMVAULT_FORGET_AUTO_ENABLED=1, the same run also permanently forgets archived memories older than MEMVAULT_FORGET_RETENTION_DAYS (default 90); otherwise auto-forget is off. Requires agent auth when the agent has a registered key. Returns updated/archived/contradicted/harmful_flagged/auto_forgotten counts.",
        annotations(
            title = "Run Memory Decay",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn run_decay(
        &self,
        Parameters(params): Parameters<RunDecayParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut dm = memvault_core::decay::DecayManager::new(
            self.store.clone(),
            memvault_core::decay::DecayConfig::default(),
        );
        let mut fm = ForgetManager::new(self.store.clone(), ForgetConfig::default());
        if let Some(compliance) = &self.compliance {
            dm = dm.with_compliance(compliance.clone());
            fm = fm.with_compliance(compliance.clone());
        }
        dm = dm.with_forget(Arc::new(fm));
        let report = dm
            .run_decay()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "updated": report.updated,
            "archived": report.archived,
            "contradicted": report.contradicted,
            "harmful_flagged": report.harmful_flagged,
            "auto_forgotten": report.auto_forgotten,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Confirm that one or more memories have been read by the agent. Increments access_count and sets last_read_at for the specified memory IDs; unknown IDs are skipped and the response reports how many rows were confirmed. Repeated calls increment the counters again (not idempotent). Requires agent auth when the agent has a registered key.",
        annotations(
            title = "Confirm Memories Read",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn confirm_read(
        &self,
        Parameters(params): Parameters<ConfirmReadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if params.memory_ids.is_empty() {
            return Err(McpError::invalid_params(
                "memory_ids must not be empty",
                None,
            ));
        }

        let confirmed = self
            .router
            .confirm_read(&params.memory_ids)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "confirmed": confirmed,
            "memory_ids": params.memory_ids,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List memories pending human review, sorted by created_at ascending (oldest first). Read-only. Supports an optional namespace and limit (default 20); there is no offset/pagination, so a full page silently truncates older pending memories. Requires agent auth when the agent has a registered key.",
        annotations(
            title = "List Review Inbox",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_inbox(
        &self,
        Parameters(params): Parameters<ListInboxParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let memories = self
            .store
            .list_pending(params.namespace.as_deref(), params.limit, 0)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut output: Vec<serde_json::Value> = Vec::with_capacity(memories.len());
        for m in &memories {
            let relations = memvault_core::relations::collect_relations(self.store.as_ref(), &m.id)
                .await
                .iter()
                .map(|rel| memvault_core::relations::relation_line(&m.content, rel))
                .collect::<Vec<_>>();
            output.push(serde_json::json!({
                "id": m.id,
                "content": m.content,
                "instruction": m.instruction,
                "priority": format!("{:?}", m.priority),
                "type": format!("{:?}", m.memory_type),
                "tags": m.tags,
                "namespace": m.namespace,
                "confidence": m.confidence,
                "ai_generated": m.ai_generated,
                "created_at": m.created_at,
                "source_agent": m.source_agent.id,
                "friction_evidence": m.friction_evidence,
                "relations": relations,
            }));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Get the raw-evidence chain a memory was distilled from (its L0 trace rows), plus its evidence profile. Read-only grounding: lets an agent quote the original session text and name its sources. Missing or pruned evidence yields an empty chain rather than a write; requires agent auth when the agent has a registered key.",
        annotations(
            title = "Get Memory Evidence Chain",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_memory_evidence(
        &self,
        Parameters(params): Parameters<GetMemoryEvidenceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let evidence =
            memvault_core::evidence::trace_evidence_chain(self.store.as_ref(), &params.memory_id)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let summary =
            memvault_core::evidence::evidence_summary(self.store.as_ref(), &params.memory_id)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let evidence_json: Vec<serde_json::Value> = evidence
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "content": m.content,
                    "layer": format!("{:?}", m.layer),
                    "agent_id": m.source_agent.id,
                    "agent_type": m.source_agent.agent_type,
                    "session_id": m.source_agent.session_id,
                    "created_at": m.created_at,
                    "tags": m.tags,
                })
            })
            .collect();

        let output = serde_json::json!({
            "memory_id": params.memory_id,
            "evidence_count": evidence_json.len(),
            "evidence": evidence_json,
            "summary": {
                "supports": summary.supports,
                "contradicts": summary.contradicts,
                "sources": summary.sources,
            },
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Run the promote pipeline: consolidates atomic L1 memories into L2 scenario summaries, and promotes stable L2 memories into L3 core persona rules. Source memories are archived to L0 after promotion (not deleted). Mutates the store, is not idempotent, and requires agent auth when the agent has a registered key. Returns promoted/consolidated/merged/source counts.",
        annotations(
            title = "Run Memory Promotion",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn run_promote(
        &self,
        Parameters(params): Parameters<RunPromoteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let _ = params.namespace; // promote pipeline currently scans all namespaces
        let config = PromoteConfig {
            min_l1_for_l2: params.min_l1.unwrap_or(3),
            min_l2_for_l3: params.min_l2.unwrap_or(2),
            ..PromoteConfig::default()
        };
        let promoter = Promoter::new(self.store.clone(), config);
        let result = promoter
            .run()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "promoted_to_l2": result.promoted_to_l2,
            "promoted_to_l3": result.promoted_to_l3,
            "consolidated_facts": result.consolidated_facts,
            "merged_entities": result.merged_entities,
            "source_ids_consumed": result.source_ids_consumed.len(),
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Report compliance for a previous injection session returned by session_start. Call this to mark which injected memories were followed or violated, with optional evidence. Reports are processed item by item: a bad item is logged and skipped while the call still succeeds, so the response's updated/total counts must be checked rather than assuming all reports were stored. Requires a compliance-enabled server; no agent API key is required by this tool.",
        annotations(
            title = "Report Injection Compliance",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn report_compliance(
        &self,
        Parameters(params): Parameters<ReportComplianceParams>,
    ) -> Result<CallToolResult, McpError> {
        let cs = self
            .compliance
            .as_ref()
            .ok_or_else(|| McpError::internal_error("Compliance tracking is not enabled", None))?;

        let mut updated = 0;
        for item in &params.reports {
            let status = match item.status.as_str() {
                "followed" => memvault_core::compliance::ComplianceStatus::Followed,
                "violated" => memvault_core::compliance::ComplianceStatus::Violated,
                _ => memvault_core::compliance::ComplianceStatus::Unknown,
            };
            match cs
                .report(
                    &params.inject_session_id,
                    &item.memory_id,
                    status,
                    item.evidence.as_deref(),
                )
                .await
            {
                Ok(()) => updated += 1,
                Err(e) => {
                    warn!(error = %e, memory_id = %item.memory_id, "compliance report failed")
                }
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Compliance reported: {}/{} items updated for session {}",
            updated,
            params.reports.len(),
            params.inject_session_id
        ))]))
    }

    #[tool(
        description = "Get a compliance report for injection sessions. Shows follow/violation rates for MUST memories. Provide inject_session_id for one session, or omit it for an aggregate summary filtered by optional agent_id and limit (default 10). Read-only; returns JSON and does not change compliance records.",
        annotations(
            title = "Get Compliance Report",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_compliance_report(
        &self,
        Parameters(params): Parameters<GetComplianceReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let cs = self
            .compliance
            .as_ref()
            .ok_or_else(|| McpError::internal_error("Compliance tracking is not enabled", None))?;

        if let Some(sid) = params.inject_session_id {
            let report = cs
                .get_report(&sid)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&report).unwrap_or_default(),
            )]));
        }

        let summary = cs
            .get_summary(params.agent_id.as_deref(), params.limit)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Get automatic effectiveness judgments for injected memories: useful/neutral/harmful/insufficient_context rates, judged automatically from record_outcome — independent of manual report_compliance. Optional agent_id filters the summary and limit defaults to 10. Read-only; returns JSON and does not change records.",
        annotations(
            title = "Get Effectiveness Report",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_effectiveness_report(
        &self,
        Parameters(params): Parameters<GetEffectivenessReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let cs = self
            .compliance
            .as_ref()
            .ok_or_else(|| McpError::internal_error("Compliance tracking is not enabled", None))?;

        let summary = cs
            .get_effectiveness_summary(params.agent_id.as_deref(), params.limit)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for MemVaultMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(
            "MemVault — AI Agent Memory Router. \
             Stores, retrieves, and auto-injects user memories into agent context. \
             Use save_memory to store, search_memory to find (supports keyword/semantic/hybrid modes), \
             session_start to get formatted injection context.",
        )
    }

    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl std::future::Future<Output = ()> + MaybeSendFuture + '_ {
        info!("MCP client initialized — auto-injecting memories via resources");

        // Trigger embedding backfill in background so all memories are vector-searchable.
        // Wait for the provider instead of sampling readiness once: on a fresh
        // install the native model is still downloading when the handshake
        // lands, and a write arriving in that window is persisted without a
        // vector — this task is what closes that gap. `get()` joins the
        // warm-up's single-flight build, so it costs nothing while waiting and
        // never starts a second download; it returns `None` only when the build
        // itself failed, in which case a later `session_start` retries.
        let store = self.store.clone();
        let embedder = self.embedder.clone();
        let backfill_guard = self.backfill_guard.clone();
        tokio::spawn(async move {
            if let Some(provider) = embedder.get().await {
                MemoryRouter::spawn_embedding_backfill(store, provider, backfill_guard);
            } else {
                debug!("embedding backfill skipped — provider unavailable after build attempt");
            }
        });

        // memories are injected via the memory://-resources that the client auto-loads
        std::future::ready(())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = vec![
            Resource::new("memory://user-profile", "User Profile & MUST Rules")
                .with_description(
                    "User's core preferences and mandatory rules. \
                     Loaded at conversation start. All agents share this resource.",
                )
                .with_mime_type("text/plain"),
            Resource::new("memory://project-context", "Current Project Context")
                .with_description(
                    "Reference-level memories about the current project's \
                     tech stack, conventions, and recent context.",
                )
                .with_mime_type("text/plain"),
        ];

        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.as_str();
        let content = self
            .router
            .get_mcp_resource_content(uri)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let text = if content.is_empty() {
            "No memories stored yet.".to_string()
        } else {
            content
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, uri)]).into())
    }
}

/// `MEMVAULT_EMBEDDING_PROVIDER=none|disabled|off` turns embeddings off
/// explicitly — pure keyword mode. Production entrypoints use
/// [`lazy_embedder_from_env`] so the explicit-off switch still skips the
/// background download entirely instead of probing forever.
pub fn embedding_explicitly_disabled() -> bool {
    matches!(
        std::env::var("MEMVAULT_EMBEDDING_PROVIDER")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "none" | "disabled" | "off"
    )
}

/// The embedding holder for long-running servers: a [`LazyEmbedder`] that
/// downloads the native model in the background on first launch (or a fixed
/// `None` when explicitly disabled).
pub fn lazy_embedder_from_env() -> LazyEmbedder {
    if embedding_explicitly_disabled() {
        LazyEmbedder::fixed(None)
    } else {
        LazyEmbedder::from_env()
    }
}

/// Kick the lazy build immediately (a fresh install downloads the native
/// model from HuggingFace into the MemVault home) without delaying the
/// caller — the port binds right away and every consumer hot-swaps the
/// moment the provider lands.
pub fn spawn_embedder_warmup(embedder: &LazyEmbedder) {
    let warmup = embedder.clone();
    tokio::spawn(async move {
        match warmup.get().await {
            Some(_) => info!("Embedding provider ready — semantic/hybrid search enabled"),
            None => info!("Embedding provider unavailable — keyword-only mode"),
        }
    });
}

/// Wire the (possibly lazy) embedding provider into a [`MemoryRouter`].
/// Enabled configs hand the router a `LazyEmbedder` trait object so its
/// hybrid context-hint path and embedding backfill hot-swap once the
/// background download lands; explicit `none` keeps the router provider-free
/// (pure keyword, no probing) exactly like the pre-lazy wiring.
pub async fn build_router_with_embedder(
    store: Arc<dyn MemoryStore>,
    registry_path: &std::path::Path,
    embedder: &LazyEmbedder,
) -> anyhow::Result<Arc<MemoryRouter>> {
    let wire = !embedding_explicitly_disabled();
    let router = if registry_path.exists() {
        info!("Loading agent registry from {}", registry_path.display());
        let r = MemoryRouter::load_registry_from_yaml(store.clone(), registry_path)?;
        if wire {
            Arc::new(r.with_embedder(Arc::new(embedder.clone())))
        } else {
            Arc::new(r)
        }
    } else {
        let r = MemoryRouter::new(store.clone());
        if wire {
            Arc::new(r.with_embedder(Arc::new(embedder.clone())))
        } else {
            Arc::new(r)
        }
    };
    Ok(router)
}

#[allow(dead_code)]
pub async fn run_stdio_server(db_path: PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path
        .parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let embedder = lazy_embedder_from_env();
    spawn_embedder_warmup(&embedder);
    let router = build_router_with_embedder(store.clone(), &registry_path, &embedder).await?;

    let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
    run_stdio_server_with(store, router, embedder, compliance).await
}

pub async fn run_stdio_server_with(
    store: Arc<dyn MemoryStore>,
    router: Arc<MemoryRouter>,
    embedder: LazyEmbedder,
    compliance: Option<Arc<ComplianceStore>>,
) -> anyhow::Result<()> {
    // Lazy env probe (LazyLlmExtractor): resolved on first llm-mode call and
    // re-probed after failures — a boot-time Ollama hiccup no longer
    // disables llm extraction for the lifetime of the process.
    let server = MemVaultMcp::new(store, router, embedder, compliance)
        .with_lazy_llm_extractor(LazyLlmExtractor::from_env());

    info!("MemVault MCP Server starting on stdio...");

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("MCP Server error: {}", e))?;

    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-global `MEMVAULT_FORGET_*`
    /// env vars `run_decay`'s handler reads via `ForgetConfig::default()`.
    static FORGET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn save_params(content: &str) -> SaveMemoryParams {
        SaveMemoryParams {
            content: content.to_string(),
            priority: "REFERENCE".to_string(),
            r#type: "fact".to_string(),
            namespace: "global".to_string(),
            instruction: None,
            tags: Vec::new(),
            agent_id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            confidence: 0.8,
            layer: None,
            visibility: None,
            api_key: None,
            force_insert: false,
            skill_trigger: None,
            skill_steps: Vec::new(),
            skill_verification: None,
        }
    }

    fn build_server(with_compliance: bool) -> (MemVaultMcp, Option<Arc<ComplianceStore>>) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let compliance = if with_compliance {
            let db = std::env::temp_dir().join(format!(
                "memvault_mcp_srv_{}.db",
                uuid::Uuid::new_v4().simple()
            ));
            Some(ComplianceStore::new(&db.to_string_lossy()).expect("compliance store"))
        } else {
            None
        };
        let server = MemVaultMcp::new(store, router, LazyEmbedder::fixed(None), compliance.clone());
        (server, compliance)
    }

    /// Markdown-backed counterpart to `build_server` — proves the
    /// `Arc<dyn MemoryStore>` generalization of `MemVaultMcp.store` actually
    /// works end to end with the markdown backend, not just that it compiles.
    async fn build_server_markdown(dir: &std::path::Path) -> MemVaultMcp {
        let store = Arc::new(
            memvault_core::storage::markdown::MarkdownStore::open(dir)
                .await
                .unwrap(),
        );
        let router = Arc::new(MemoryRouter::new(store.clone()));
        MemVaultMcp::new(store, router, LazyEmbedder::fixed(None), None)
    }

    /// Extract the text payload of a successful tool result.
    fn tool_text(result: Result<CallToolResult, McpError>) -> String {
        match result {
            Ok(r) => match r.content.first() {
                Some(ContentBlock::Text(t)) => t.text.clone(),
                _ => String::new(),
            },
            Err(e) => panic!("unexpected tool error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_tool_save_memory_defaults() {
        let (server, _comp) = build_server(false);
        let result = server
            .save_memory(Parameters(save_params("prefers Emacs")))
            .await;
        let text = tool_text(result);
        assert!(text.contains("saved"));
        assert!(text.contains("mem_"));
        assert!(text.contains("embedded"));
    }

    #[tokio::test]
    async fn test_tool_save_memory_embeds_with_ready_provider() {
        let server = build_server_with_embedder(Arc::new(FakeEmbedder { fail: false }));
        let text = tool_text(
            server
                .save_memory(Parameters(save_params("prefers vim keybindings")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(parsed["embedded"], serde_json::json!(true));
        assert_eq!(
            parsed["vector_pending"],
            serde_json::json!(false),
            "a provider-backed save is not pending backfill: {text}"
        );
    }

    #[tokio::test]
    async fn test_tool_save_memory_waits_for_warming_provider() {
        // Boot-race regression (0.6.0): the stdio handshake completes before the
        // lazy `native` provider is built, so a save arriving immediately used
        // to persist a vectorless memory. `save_memory` must now wait for
        // readiness rather than sampling the provider once.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let embedder = LazyEmbedder::delayed(
            Arc::new(FakeEmbedder { fail: false }),
            std::time::Duration::from_millis(60),
        );
        assert!(
            !embedder.ready().await,
            "precondition: provider not ready yet"
        );
        let server = MemVaultMcp::new(store, router, embedder, None);

        let text = tool_text(
            server
                .save_memory(Parameters(save_params("saved during model warm-up")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(
            parsed["embedded"],
            serde_json::json!(true),
            "a save during provider warm-up must wait and embed: {text}"
        );
        assert_eq!(parsed["vector_pending"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_tool_save_memory_reports_vector_pending_when_embed_fails() {
        // Provider present but the embed call fails: the record lands without a
        // vector, and because a provider *is* expected this must be reported as
        // pending backfill rather than as a deliberate keyword-only save.
        let server = build_server_with_embedder(Arc::new(FakeEmbedder { fail: true }));
        let text = tool_text(
            server
                .save_memory(Parameters(save_params("recall without a vector")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(parsed["embedded"], serde_json::json!(false));
        assert_eq!(parsed["vector_pending"], serde_json::json!(true));
    }

    fn evidence_params(memory_id: &str, relation: &str) -> AddEvidenceParams {
        AddEvidenceParams {
            memory_id: memory_id.to_string(),
            relation: relation.to_string(),
            evidence_id: None,
            source: None,
            confidence: None,
            agent_id: "tester".to_string(),
            api_key: None,
        }
    }

    #[tokio::test]
    async fn test_tool_add_evidence_sourced_from() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("postgres 16 in prod")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        let mut params = evidence_params(&id, "sourced_from");
        params.source = Some("https://wiki/db-stack".to_string());
        let text = tool_text(server.add_evidence(Parameters(params)).await);
        assert!(text.contains("sourced_from"));
        assert!(text.contains("https://wiki/db-stack"));
    }

    #[tokio::test]
    async fn test_tool_add_evidence_supports() {
        let (server, _comp) = build_server(false);
        let claim = extract_id(
            &server
                .save_memory(Parameters(save_params("deploy window is Friday")))
                .await
                .unwrap(),
        );
        let support = extract_id(
            &server
                .save_memory(Parameters(save_params("team agreed on Friday deploys")))
                .await
                .unwrap(),
        );

        let mut params = evidence_params(&claim, "supports");
        params.evidence_id = Some(support.clone());
        let text = tool_text(server.add_evidence(Parameters(params)).await);
        assert!(text.contains("supports"));
        assert!(text.contains(&support));
        assert!(text.contains(&claim));
    }

    #[tokio::test]
    async fn test_tool_add_evidence_rejects_bad_relation() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("some fact")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        let result = server
            .add_evidence(Parameters(evidence_params(&id, "refutes")))
            .await;
        assert!(result.is_err(), "unknown relation must be rejected");
    }

    #[tokio::test]
    async fn test_tool_add_evidence_supports_requires_evidence_id() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("some fact")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        let result = server
            .add_evidence(Parameters(evidence_params(&id, "supports")))
            .await;
        assert!(
            result.is_err(),
            "supports without evidence_id must be rejected"
        );
    }

    #[tokio::test]
    async fn test_tool_add_evidence_sourced_from_requires_source() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("some fact")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        let result = server
            .add_evidence(Parameters(evidence_params(&id, "sourced_from")))
            .await;
        assert!(
            result.is_err(),
            "sourced_from without source text must be rejected"
        );
    }

    #[tokio::test]
    async fn test_tool_add_evidence_rejects_unknown_memory() {
        let (server, _comp) = build_server(false);
        let result = server
            .add_evidence(Parameters(evidence_params("mem_missing", "supports")))
            .await;
        assert!(result.is_err(), "unknown subject memory must be rejected");
    }

    #[tokio::test]
    async fn test_tool_get_memory_evidence_returns_l0_chain() {
        let (server, _comp) = build_server(false);

        // Seed an L0 raw-evidence row directly (trace ingestion writes these).
        let mut raw = Memory::new(
            MemoryType::Fact,
            "raw transcript: the deploy window is Friday".to_string(),
            Priority::Background,
            SourceAgent {
                id: "ingest".to_string(),
                agent_type: "transcript".to_string(),
                session_id: Some("sess-42".to_string()),
            },
        );
        raw.layer = MemoryLayer::L0;
        let raw = server.store.save(raw).await.unwrap();

        // Candidate distilled from that raw row.
        let mut candidate = Memory::new(
            MemoryType::Fact,
            "deploy window is Friday".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        );
        candidate.source_trace_ids = vec![raw.id.clone()];
        let candidate = server.store.save(candidate).await.unwrap();

        let text = tool_text(
            server
                .get_memory_evidence(Parameters(GetMemoryEvidenceParams {
                    memory_id: candidate.id.clone(),
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );

        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["evidence_count"].as_u64(), Some(1));
        assert_eq!(v["evidence"][0]["id"], raw.id);
        assert!(
            v["evidence"][0]["content"]
                .as_str()
                .unwrap()
                .contains("raw transcript: the deploy window is Friday")
        );
        assert_eq!(v["evidence"][0]["layer"], "L0");
        assert_eq!(v["evidence"][0]["session_id"], "sess-42");
        assert_eq!(v["summary"]["supports"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn test_tool_get_memory_evidence_unknown_id_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .get_memory_evidence(Parameters(GetMemoryEvidenceParams {
                memory_id: "mem_missing".to_string(),
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(result.is_err(), "unknown memory id must be a tool error");
    }

    fn outcome_params(task: &str, status: &str) -> RecordOutcomeParams {
        RecordOutcomeParams {
            task: task.to_string(),
            status: status.to_string(),
            cause: None,
            task_type: None,
            skill_id: None,
            namespace: "global".to_string(),
            tags: Vec::new(),
            agent_id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
            api_key: None,
        }
    }

    struct FixedEffectivenessJudge;

    #[async_trait::async_trait]
    impl LlmExtractor for FixedEffectivenessJudge {
        async fn extract(
            &self,
            _context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::extractor::ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(
            &self,
            _system: &str,
            _user: &str,
        ) -> memvault_core::error::Result<Option<String>> {
            Ok(Some(
                "{\"verdict\": \"useful\", \"reason\": \"named the exact fix\"}".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn test_tool_record_outcome_judges_effectiveness_of_prior_injection() {
        // End-to-end Tier-3 loop: session_start records an injection, then
        // record_outcome (with an LLM judge configured) pairs it and judges
        // it — no manual `report_compliance` involved anywhere.
        let (server, comp) = build_server(true);
        let compliance = comp.expect("compliance store must be configured");
        let server = server.with_llm_extractor(Arc::new(FixedEffectivenessJudge));

        server
            .save_memory(Parameters(save_params(
                "always set ENV_VAR before deploying the service",
            )))
            .await
            .unwrap();

        server
            .session_start(Parameters(SessionStartParams {
                agent_id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                context_hint: None,
                project: None,
                api_key: None,
            }))
            .await
            .unwrap();

        let mut params = outcome_params("deploy the api", "failure");
        params.cause = Some("missing env var".to_string());
        let text = tool_text(server.record_outcome(Parameters(params)).await);
        assert!(
            text.contains("\"effectiveness_judged\": 1"),
            "response: {}",
            text
        );

        let summary = compliance
            .get_effectiveness_summary(Some("tester"), 10)
            .await
            .unwrap();
        assert_eq!(summary.useful, 1);

        let text = tool_text(
            server
                .get_effectiveness_report(Parameters(GetEffectivenessReportParams {
                    agent_id: Some("tester".to_string()),
                    limit: 10,
                }))
                .await,
        );
        assert!(text.contains("\"useful\": 1"), "response: {}", text);
    }

    #[tokio::test]
    async fn test_tool_get_effectiveness_report_without_store_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .get_effectiveness_report(Parameters(GetEffectivenessReportParams {
                agent_id: None,
                limit: 10,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_record_outcome_success() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .record_outcome(Parameters(outcome_params("deploy the web", "success")))
                .await,
        );
        assert!(text.contains("recorded"));
        assert!(text.contains("mem_"));
        assert!(text.contains("success"));
    }

    #[tokio::test]
    async fn test_tool_record_outcome_failure_with_cause() {
        let (server, _comp) = build_server(false);
        let mut params = outcome_params("deploy the api", "failure");
        params.cause = Some("missing env var".to_string());
        params.task_type = Some("deploy".to_string());

        let text = tool_text(server.record_outcome(Parameters(params)).await);
        assert!(text.contains("recorded"));
        assert!(text.contains("failure"));
        // Rule-based reflection fires (no LLM in this harness): the cause
        // becomes a lesson linked back to the episode.
        assert!(
            text.contains("missing env var"),
            "lesson in response: {}",
            text
        );
        assert!(text.contains("lesson"));

        // The episode record must carry the lesson + backlink.
        let episodes = server
            .store
            .list_episodes(EpisodeFilter {
                task_type: Some("deploy".to_string()),
                status: Some(OutcomeStatus::Failure),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].task, "deploy the api");
        assert_eq!(episodes[0].cause.as_deref(), Some("missing env var"));
        assert!(episodes[0].lesson.is_some());
        assert!(episodes[0].lesson_memory_id.is_some());
    }

    #[tokio::test]
    async fn test_tool_record_outcome_rejects_invalid_status() {
        let (server, _comp) = build_server(false);
        let result = server
            .record_outcome(Parameters(outcome_params("task", "maybe")))
            .await;
        assert!(result.is_err(), "invalid status must be rejected");
    }

    fn infer_outcome_params(
        evidence: OutcomeEvidenceParams,
        judge_context: &str,
    ) -> InferOutcomeParams {
        InferOutcomeParams {
            evidence,
            judge_context: judge_context.to_string(),
            namespace: "global".to_string(),
            tags: Vec::new(),
            agent_id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
            api_key: None,
        }
    }

    #[tokio::test]
    async fn test_tool_infer_outcome_skips_with_no_evidence() {
        let (server, _comp) = build_server(false);
        let params = infer_outcome_params(
            OutcomeEvidenceParams {
                task: Some("deploy the checkout service".to_string()),
                retry_hits: 0,
                rejection_hits: 0,
                correction_hits: 0,
                terminal: "resolved".to_string(),
                terminal_detail: None,
            },
            "",
        );
        let text = tool_text(server.infer_outcome(Parameters(params)).await);
        assert!(
            text.contains("skipped"),
            "clean evidence must skip: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_tool_infer_outcome_records_failure_from_evidence() {
        let (server, _comp) = build_server(false);
        let params = infer_outcome_params(
            OutcomeEvidenceParams {
                task: Some("deploy the checkout service to staging".to_string()),
                retry_hits: 1,
                rejection_hits: 0,
                correction_hits: 0,
                terminal: "unresolved_tool_error".to_string(),
                terminal_detail: Some("Bash: permission denied".to_string()),
            },
            "",
        );
        let text = tool_text(server.infer_outcome(Parameters(params)).await);
        assert!(text.contains("recorded"), "response: {}", text);
        assert!(text.contains("\"inference_source\": \"rule\""));

        let episodes = server
            .store
            .list_episodes(EpisodeFilter::default())
            .await
            .unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].status, OutcomeStatus::Failure);
        assert!(episodes[0].lesson.is_some());

        let episode_mem = server.store.get(&episodes[0].memory_id).await.unwrap();
        assert!(episode_mem.tags.contains(&"auto-inferred".to_string()));
        assert!(episode_mem.tags.contains(&"inferred:rule".to_string()));
        assert!(!episode_mem.human_reviewed);
    }

    /// 回归:同一 session/证据重复 infer_outcome 时,episode 会去重,但 lesson
    /// 之前仍被二次蒸馏(reflection 走裸 store.save,且本工具漏了
    /// `record_outcome` 里已有的 `is_duplicate` 守卫),还通过
    /// `update_episode_lesson` 把第一条 lesson 变成孤儿。
    #[tokio::test]
    async fn test_infer_outcome_duplicate_does_not_distill_second_lesson() {
        let (server, _comp) = build_server(false);
        let params = || {
            let mut p = infer_outcome_params(
                OutcomeEvidenceParams {
                    task: Some("fix the flaky test".to_string()),
                    retry_hits: 2,
                    rejection_hits: 0,
                    correction_hits: 0,
                    terminal: "unresolved_tool_error".to_string(),
                    terminal_detail: Some("cargo test failed twice".to_string()),
                },
                "",
            );
            p.session_id = Some("sess-dupe".to_string());
            p
        };

        let first = tool_text(server.infer_outcome(Parameters(params())).await);
        assert!(first.contains("recorded"), "first call: {first}");
        let second = tool_text(server.infer_outcome(Parameters(params())).await);
        assert!(second.contains("duplicate"), "second call: {second}");
        assert!(
            second.contains("\"lesson\": null"),
            "duplicate must not distill a lesson: {second}"
        );

        let episodes = server
            .store
            .list_episodes(EpisodeFilter::default())
            .await
            .unwrap();
        assert_eq!(episodes.len(), 1, "episode must stay deduped");

        let lessons = server
            .store
            .list(None, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.tags.iter().any(|t| t == "lesson"))
            .count();
        assert_eq!(
            lessons, 1,
            "duplicate outcome must not distill a second lesson"
        );
    }

    #[tokio::test]
    async fn test_tool_save_memory_must_skill_with_meta() {
        let (server, _comp) = build_server(false);
        let mut params = save_params("deploy runbook");
        params.priority = "MUST".to_string();
        params.r#type = "skill".to_string();
        params.layer = Some("L3".to_string());
        params.skill_trigger = Some("deploy".to_string());
        params.skill_steps = vec!["build".to_string(), "test".to_string()];
        params.skill_verification = Some("health ok".to_string());
        let text = tool_text(server.save_memory(Parameters(params)).await);
        assert!(text.contains("Must"), "save output: {}", text);
    }

    #[tokio::test]
    async fn test_tool_save_rejects_invalid_layer() {
        let (server, _comp) = build_server(false);
        let mut params = save_params("bad layer");
        params.layer = Some("L9".to_string()); // falls back to L1
        let _ = server.save_memory(Parameters(params)).await.unwrap();
    }

    #[tokio::test]
    async fn test_tool_extract_memories_empty() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: String::new(),
                    mode: None,
                    assistant_text: None,
                    auto_save: false,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(text.contains("No memories extracted"));
    }

    #[tokio::test]
    async fn test_tool_extract_memories_with_auto_save() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I always prefer dark mode".to_string(),
                    mode: None,
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(text.contains("Preference"), "extracted text: {}", text);
        assert!(text.contains("saved_id"));
    }

    /// 回归:extract_memories(auto_save) 之前走裸 `store.save`,重复调用会把
    /// 逐字节相同的记忆反复插入(CLI/ingest 走 MemoryWriter 会去重,入口间
    /// 不一致)。现在同样走 MemoryWriter,重复调用不得堆积。
    #[tokio::test]
    async fn test_extract_auto_save_dedups_repeated_call() {
        let (server, _comp) = build_server(false);
        let params = || ExtractMemoriesParams {
            text: "I prefer dark mode".to_string(),
            mode: None,
            assistant_text: None,
            auto_save: true,
            agent_id: "tester".to_string(),
            api_key: None,
            occurred_at: None,
        };
        let _ = tool_text(server.extract_memories(Parameters(params())).await);
        let _ = tool_text(server.extract_memories(Parameters(params())).await);

        let stored = server.store.list(None, 100, 0).await.unwrap();
        let dupes = stored
            .iter()
            .filter(|m| m.content == "I prefer dark mode")
            .count();
        assert_eq!(
            dupes, 1,
            "repeated auto_save must dedup instead of piling up: {stored:?}"
        );
    }

    /// 回归:SOP 导入走裸 store 写入,工具必须踢共享 backfill,导入的技能
    /// 无需等下一次握手/重启即可语义检索。
    #[tokio::test]
    async fn test_import_skills_backfills_embeddings() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let server = MemVaultMcp::new(
            store.clone(),
            router,
            LazyEmbedder::fixed(Some(Arc::new(FakeEmbedder { fail: false }))),
            None,
        );

        let text = tool_text(
            server
                .import_skills(Parameters(ImportSkillsParams {
                    markdown: "# Deploy SOP\ntrigger: deploy\n- step one\n- step two\n".to_string(),
                    fallback_title: "imported-sop".to_string(),
                    namespace: "global".to_string(),
                    approve: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Deploy SOP"), "tool output: {}", text);

        for _ in 0..100 {
            if store.list_without_embedding(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            store.list_without_embedding(10).await.unwrap().is_empty(),
            "import_skills must kick the embedding backfill"
        );
    }

    /// 回归:extract_memories auto_save 同为裸 store 写入,同样必须踢
    /// backfill。
    #[tokio::test]
    async fn test_extract_auto_save_backfills_embeddings() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let server = MemVaultMcp::new(
            store.clone(),
            router,
            LazyEmbedder::fixed(Some(Arc::new(FakeEmbedder { fail: false }))),
            None,
        );

        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I always prefer dark mode".to_string(),
                    mode: None,
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(text.contains("saved_id"), "tool output: {}", text);

        for _ in 0..100 {
            if store.list_without_embedding(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            store.list_without_embedding(10).await.unwrap().is_empty(),
            "extract auto_save must kick the embedding backfill"
        );
    }

    /// 回归(复查发现):启动后立即 semantic 查询撞上模型预热窗口,旧行为
    /// 采样一次就绪度就报 -32602(看似调用方参数错误,重试又消失)。现在
    /// 应有界等待(与写路径同预算 EMBED_WRITE_WAIT)后正常返回向量结果。
    #[tokio::test]
    async fn test_search_semantic_waits_for_warming_embedder() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "vector me".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "t".to_string(),
                agent_type: "g".to_string(),
                session_id: None,
            },
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store.set_embedding(&id, vec![0.1, 0.2, 0.3]).await.unwrap();
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let server = MemVaultMcp::new(
            store,
            router,
            LazyEmbedder::delayed(
                Arc::new(FakeEmbedder { fail: false }),
                std::time::Duration::from_millis(40),
            ),
            None,
        );

        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "vector".to_string(),
                    mode: "semantic".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: None,
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );
        assert!(
            text.contains("\"search_mode\": \"semantic\""),
            "a semantic query during warm-up must wait and stay semantic, got: {text}"
        );
        assert!(text.contains("vector me"), "must return the vector hit");
    }

    /// 配置性关闭 embedding(provider 明确为 None)时,semantic 查询保持
    /// 显式报错 —— 那是永久配置事实,不是预热竞态,不该被静默降级掩盖。
    #[tokio::test]
    async fn test_search_semantic_without_provider_still_errors() {
        let (server, _comp) = build_server(false);
        let err = server
            .search_memory(Parameters(SearchMemoryParams {
                query: "anything".to_string(),
                mode: "semantic".to_string(),
                top_k: 5,
                namespace: None,
                type_filter: None,
                priority_filter: None,
                agent_id: None,
                api_key: None,
                expand_relations: false,
                min_score: None,
            }))
            .await
            .expect_err("semantic search on a keyword-only config must error explicitly");
        assert!(
            format!("{err:?}").contains("embedding provider"),
            "error must explain the missing provider, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_tool_extract_memories_invalid_occurred_at_is_invalid_params() {
        // Parse happens before any save: a bad timestamp must fail the whole
        // call, never silently drop the provenance and persist anyway.
        let (server, _comp) = build_server(false);
        let result = server
            .extract_memories(Parameters(ExtractMemoriesParams {
                text: "I always prefer dark mode".to_string(),
                mode: None,
                assistant_text: None,
                auto_save: true,
                agent_id: "tester".to_string(),
                api_key: None,
                occurred_at: Some("last tuesday".to_string()),
            }))
            .await;
        let err = result.unwrap_err();
        assert_eq!(
            err.code,
            rmcp::model::ErrorCode(-32602),
            "must be invalid_params"
        );

        // Nothing was saved despite auto_save.
        let count = server
            .store
            .search(memvault_core::models::SearchQuery::new(
                "dark mode".to_string(),
            ))
            .await
            .unwrap()
            .results
            .len();
        assert_eq!(count, 0, "invalid occurred_at must prevent any save");
    }

    #[tokio::test]
    async fn test_tool_extract_memories_occurred_at_lands_on_saved_memory() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I always prefer dark mode".to_string(),
                    mode: None,
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: Some("2023-05-07T13:56:00Z".to_string()),
                }))
                .await,
        );
        assert!(text.contains("saved_id"), "extracted text: {}", text);

        // Pull the saved_id out of the response text and verify provenance.
        let saved_id = text
            .split_once("saved_id\": \"")
            .map(|(_, rest)| rest.split('"').next().unwrap())
            .expect("saved_id present in tool output");
        let mem = server.store.get(saved_id).await.unwrap();
        assert_eq!(
            mem.occurred_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2023-05-07T13:56:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
    }

    /// Stub `LlmExtractor` for testing the `mode="llm"` path without a real
    /// network call.
    struct StubLlmExtractor {
        seen_context: std::sync::Mutex<Option<String>>,
    }

    impl StubLlmExtractor {
        fn new() -> Self {
            Self {
                seen_context: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl memvault_core::llm_extractor::LlmExtractor for StubLlmExtractor {
        async fn extract(
            &self,
            context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::extractor::ExtractedMemory>> {
            *self.seen_context.lock().unwrap() = Some(context.to_string());
            Ok(vec![memvault_core::extractor::ExtractedMemory {
                content: "likes concise commit messages".to_string(),
                instruction: None,
                memory_type: MemoryType::Preference,
                priority: Priority::Reference,
                tags: vec!["git".to_string()],
                confidence: 0.85,
            }])
        }
    }

    /// An LLM extractor whose `extract()` always fails — stands in for a
    /// malformed-JSON model response or a dead Ollama daemon. The handler
    /// must degrade to rule-based extraction, never propagate the error.
    struct FailingExtractor;

    #[async_trait::async_trait]
    impl memvault_core::llm_extractor::LlmExtractor for FailingExtractor {
        async fn extract(
            &self,
            _context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::extractor::ExtractedMemory>> {
            Err(memvault_core::error::MemVaultError::LlmExtraction(
                "simulated malformed model output".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn test_tool_extract_memories_mode_llm_without_provider_falls_back() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I prefer concise commit messages".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: None,
                    auto_save: false,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["fallback_used"], true);
        assert!(
            out["fallback_reason"]
                .as_str()
                .unwrap()
                .contains("MEMVAULT_LLM_EXTRACTION_PROVIDER")
        );
        // The degraded path is rule-based, so honest coverage accounting
        // must be reported — it is not a clean llm result.
        assert!(out["coverage"].is_object());
    }

    #[tokio::test]
    async fn test_tool_extract_memories_mode_llm_error_falls_back() {
        let (server, _comp) = build_server(false);
        let server = server.with_llm_extractor(Arc::new(FailingExtractor));
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    // rule path must still find this preference line
                    text: "I prefer concise commit messages".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["fallback_used"], true);
        assert!(
            out["fallback_reason"]
                .as_str()
                .unwrap()
                .contains("simulated malformed model output")
        );
        // Fallback-produced saves are tagged so an audit can tell them apart
        // from an intentional rule call.
        let memories = server.store.list(None, 100, 0).await.unwrap();
        assert!(!memories.is_empty());
        assert!(
            memories
                .iter()
                .all(|m| m.tags.iter().any(|t| t == "method:llm_fallback_rule"))
        );
    }

    #[tokio::test]
    async fn test_tool_extract_memories_mode_llm_success_no_fallback() {
        let (server, _comp) = build_server(false);
        let server = server.with_llm_extractor(Arc::new(StubLlmExtractor::new()));
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I prefer concise commit messages".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: None,
                    auto_save: false,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["fallback_used"], false);
        assert!(out["fallback_reason"].is_null());
        assert_eq!(
            out["memories"][0]["content"],
            "likes concise commit messages"
        );
    }

    #[tokio::test]
    async fn test_tool_extract_memories_mode_llm_pairs_context_and_saves() {
        let (server, _comp) = build_server(false);
        let stub = Arc::new(StubLlmExtractor::new());
        let server = server.with_llm_extractor(stub.clone());

        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "I prefer concise commit messages".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: Some("Got it, I'll keep commits short.".to_string()),
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(text.contains("saved_id"), "extracted text: {}", text);
        assert!(
            text.contains("\"mode\": \"llm\""),
            "extracted text: {}",
            text
        );

        let seen = stub.seen_context.lock().unwrap().clone().unwrap();
        assert!(seen.contains("I prefer concise commit messages"));
        assert!(seen.contains("Got it, I'll keep commits short."));
    }

    #[tokio::test]
    async fn test_tool_search_keyword_finds_saved() {
        let (server, _comp) = build_server(false);
        server
            .save_memory(Parameters(save_params("likes green tea")))
            .await
            .unwrap();
        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "green tea".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: Some("tester".to_string()),
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );
        assert!(text.contains("likes green tea"));
    }

    #[tokio::test]
    async fn test_tool_save_and_search_against_markdown_backend() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_mcp_markdown_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let server = build_server_markdown(&dir).await;

        let save_text = tool_text(
            server
                .save_memory(Parameters(save_params("likes oolong tea")))
                .await,
        );
        assert!(save_text.contains("saved"));

        let search_text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "oolong".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: Some("tester".to_string()),
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );
        assert!(search_text.contains("likes oolong tea"));

        // The write really did land on disk as a markdown file, not just in
        // an in-memory fallback.
        let md_files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(md_files.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_tool_search_min_score_filters_weak_matches() {
        // min_score 置信度下限,与 REST /api/search 行为对齐:
        // 高于所有候选 → 空结果(不是错误);省略 → 行为不变。
        let (server, _comp) = build_server(false);
        server
            .save_memory(Parameters(save_params("likes green tea")))
            .await
            .unwrap();

        let search = |min_score: Option<f64>| {
            let server = &server;
            async move {
                tool_text(
                    server
                        .search_memory(Parameters(SearchMemoryParams {
                            query: "green tea".to_string(),
                            mode: "keyword".to_string(),
                            top_k: 5,
                            namespace: None,
                            type_filter: None,
                            priority_filter: None,
                            agent_id: Some("tester".to_string()),
                            api_key: None,
                            expand_relations: false,
                            min_score,
                        }))
                        .await,
                )
            }
        };

        let baseline = search(None).await;
        assert!(baseline.contains("likes green tea"));

        // Floor far above any possible score → the match is filtered out.
        let filtered = search(Some(1000.0)).await;
        assert!(
            !filtered.contains("likes green tea"),
            "min_score above every candidate must drop it, got: {filtered}"
        );
    }

    #[tokio::test]
    async fn test_tool_search_applies_filters() {
        let (server, _comp) = build_server(false);
        let mut must = save_params("always run lints");
        must.priority = "MUST".to_string();
        server.save_memory(Parameters(must)).await.unwrap();

        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "lints".to_string(),
                    mode: "hybrid".to_string(), // falls back to keyword without embedder
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: Some("MUST".to_string()),
                    agent_id: None,
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );
        assert!(
            text.contains("always run lints"),
            "hybrid falls back to keyword without an embedder: {}",
            text
        );
        assert!(
            text.contains("search_mode"),
            "output carries the resolved mode"
        );
        assert!(text.contains("Must"));
    }

    #[tokio::test]
    async fn test_tool_search_reranks_by_authority_tier() {
        // search_memory used to skip straight from merge to output, so it
        // never got the reranker's overlap/recency/authority-tier signals —
        // only raw FTS order. Two memories tying on every other signal
        // (same content, same priority, saved moments apart) should still
        // come back with the decision-tagged one first.
        let (server, _comp) = build_server(false);

        let plain = save_params("authority signal check");
        server.save_memory(Parameters(plain)).await.unwrap();

        let mut decision = save_params("authority signal check");
        decision.tags = vec!["decision".to_string()];
        // The rerank test needs two rows with identical content; delta-write
        // would (correctly) merge them.
        decision.force_insert = true;
        server.save_memory(Parameters(decision)).await.unwrap();

        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "authority signal check".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: None,
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0]["tags"],
            serde_json::json!(["decision"]),
            "decision-tagged memory should rank first: {}",
            text
        );
    }

    struct FakeEmbedder {
        fail: bool,
    }

    #[async_trait::async_trait]
    impl memvault_core::embedding::EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, _texts: &[String]) -> memvault_core::error::Result<Vec<Vec<f32>>> {
            if self.fail {
                Err(memvault_core::error::MemVaultError::LlmExtraction(
                    "stub embedder failure".to_string(),
                ))
            } else {
                Ok(vec![vec![0.1, 0.2, 0.3]])
            }
        }

        fn dimension(&self) -> usize {
            3
        }
    }

    fn build_server_with_embedder(
        embedder: Arc<dyn memvault_core::embedding::EmbeddingProvider>,
    ) -> MemVaultMcp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        MemVaultMcp::new(store, router, LazyEmbedder::fixed(Some(embedder)), None)
    }

    /// Restore `MEMVAULT_RELATIONS` after the test. Edition 2024 marks
    /// `set_var`/`remove_var` unsafe; this variable is read by no other test
    /// in parallel, so the process-global mutation is confined here.
    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// `extract` returns nothing; `extract_relations` yields one triple so the
    /// C2 relation-persistence branch in `extract_memories` (gated by
    /// `MEMVAULT_RELATIONS=on`) is exercised end to end.
    struct StubRelationsLlmExtractor;

    #[async_trait::async_trait]
    impl memvault_core::llm_extractor::LlmExtractor for StubRelationsLlmExtractor {
        async fn extract(
            &self,
            _context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::extractor::ExtractedMemory>> {
            // `extract_memories` short-circuits on an empty extraction, so the
            // stub must return one memory to reach the relation branch.
            Ok(vec![memvault_core::extractor::ExtractedMemory {
                content: "likes concise commit messages".to_string(),
                instruction: None,
                memory_type: MemoryType::Preference,
                priority: Priority::Reference,
                tags: vec!["git".to_string()],
                confidence: 0.85,
            }])
        }

        async fn extract_relations(
            &self,
            _context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::llm_extractor::ExtractedRelation>>
        {
            Ok(vec![memvault_core::llm_extractor::ExtractedRelation {
                subject: "PostgreSQL".to_string(),
                predicate: "supports".to_string(),
                object: "JSONB".to_string(),
            }])
        }
    }

    #[tokio::test]
    async fn test_tool_search_expand_relations_attaches_neighborhood() {
        // C5 on the MCP tool path: rest_api.rs covers expand_relations, but
        // every server.rs search test passed expand_relations:false, leaving
        // the tool's relation-neighborhood serialization untested.
        let (server, _comp) = build_server(false);
        let claim = extract_id(
            &server
                .save_memory(Parameters(save_params("deploy window is Friday")))
                .await
                .unwrap(),
        );
        let support = extract_id(
            &server
                .save_memory(Parameters(save_params("team agreed on Friday deploys")))
                .await
                .unwrap(),
        );
        let mut ep = evidence_params(&claim, "supports");
        ep.evidence_id = Some(support.clone());
        server.add_evidence(Parameters(ep)).await.unwrap();

        // With expand_relations, the claim carries its supports→support edge.
        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "Friday".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 10,
                    namespace: Some("global".to_string()),
                    type_filter: None,
                    priority_filter: None,
                    agent_id: Some("tester".to_string()),
                    api_key: None,
                    expand_relations: true,
                    min_score: None,
                }))
                .await,
        );
        let results: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        let with_relations: Vec<&serde_json::Value> = results
            .iter()
            .filter(|r| {
                r.get("relations").is_some()
                    && r["relations"].as_array().is_some_and(|a| !a.is_empty())
            })
            .collect();
        assert!(
            !with_relations.is_empty(),
            "expand_relations must attach a non-empty neighborhood: {}",
            text
        );
        let found = with_relations.iter().any(|r| {
            r["relations"].as_array().unwrap().iter().any(|rel| {
                rel["predicate"] == "supports" && rel["object_id"].as_str() == Some(&claim)
            })
        });
        assert!(
            found,
            "expected a supports edge targeting claim {} in {:#}",
            claim,
            serde_json::to_string_pretty(&with_relations).unwrap()
        );

        // Without expand_relations, no relations key leaks out.
        let text = tool_text(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "Friday".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 10,
                    namespace: Some("global".to_string()),
                    type_filter: None,
                    priority_filter: None,
                    agent_id: Some("tester".to_string()),
                    api_key: None,
                    expand_relations: false,
                    min_score: None,
                }))
                .await,
        );
        let results: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert!(results.iter().all(|r| r.get("relations").is_none()));
    }

    #[tokio::test]

    async fn test_tool_save_memory_auto_embeds_with_embedder() {
        // Auto-embed success branch: embedder present + non-empty embeddings.
        let server = build_server_with_embedder(Arc::new(FakeEmbedder { fail: false }));
        let text = tool_text(
            server
                .save_memory(Parameters(save_params("auto-embed me")))
                .await,
        );
        assert!(
            text.contains("\"embedded\": true"),
            "expected embedded:true with embedder, got: {}",
            text
        );

        // Auto-embed fallback branch: embedder errors → still saves, no vector.
        let server = build_server_with_embedder(Arc::new(FakeEmbedder { fail: true }));
        let text = tool_text(
            server
                .save_memory(Parameters(save_params("fallback save")))
                .await,
        );
        assert!(
            text.contains("\"embedded\": false"),
            "expected embedded:false on embedder failure, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_tool_extract_memories_mode_llm_persists_relations_when_enabled() {
        // LLM relation persistence (C2) is gated by MEMVAULT_RELATIONS=on in
        // addition to auto_save + llm extractor; both the on and off paths
        // must be observable in the tool output.
        let stub = Arc::new(StubRelationsLlmExtractor);

        let guard = EnvGuard::set("MEMVAULT_RELATIONS", "on");
        let (server, _comp) = build_server(false);
        let server = server.with_llm_extractor(stub.clone());
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "PostgreSQL supports JSONB".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(
            text.contains("\"extracted\": 1") && text.contains("\"stored\": 1"),
            "expected extracted/stored relation counts, got: {}",
            text
        );
        drop(guard);

        let guard = EnvGuard::set("MEMVAULT_RELATIONS", "off");
        let (server, _comp) = build_server(false);
        let server = server.with_llm_extractor(stub);
        let text = tool_text(
            server
                .extract_memories(Parameters(ExtractMemoriesParams {
                    text: "PostgreSQL supports JSONB".to_string(),
                    mode: Some("llm".to_string()),
                    assistant_text: None,
                    auto_save: true,
                    agent_id: "tester".to_string(),
                    api_key: None,
                    occurred_at: None,
                }))
                .await,
        );
        assert!(
            !text.contains("extracted"),
            "relations must stay off when MEMVAULT_RELATIONS=off, got: {}",
            text
        );
        drop(guard);
    }

    #[tokio::test]
    async fn test_tool_search_semantic_without_embedder_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .search_memory(Parameters(SearchMemoryParams {
                query: "x".to_string(),
                mode: "semantic".to_string(),
                top_k: 5,
                namespace: None,
                type_filter: None,
                priority_filter: None,
                agent_id: None,
                api_key: None,
                expand_relations: false,
                min_score: None,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_session_start_empty() {
        let (server, _comp) = build_server(false);
        let text = tool_text(
            server
                .session_start(Parameters(SessionStartParams {
                    agent_id: "tester".to_string(),
                    agent_type: "coding-assistant".to_string(),
                    context_hint: None,
                    project: None,
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("No memories to inject"));
    }

    #[tokio::test]
    async fn test_tool_session_start_with_memory() {
        let (server, _comp) = build_server(false);
        server
            .save_memory(Parameters(save_params("user prefers vi")))
            .await
            .unwrap();
        let text = tool_text(
            server
                .session_start(Parameters(SessionStartParams {
                    agent_id: "tester".to_string(),
                    agent_type: "coding-assistant".to_string(),
                    context_hint: None,
                    project: None,
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("prefers vi"));
    }

    #[tokio::test]
    async fn test_tool_session_start_records_compliance_injection() {
        // Regression for the stdio/REST parity gap: `session_start` must
        // record injections into `compliance_events` the same way the REST
        // handler already does, so `report_compliance`/effectiveness judging
        // have data to work with for MCP stdio sessions too.
        let (server, compliance) = build_server(true);
        let compliance = compliance.expect("compliance store must be configured");

        server
            .save_memory(Parameters(save_params("user prefers vi")))
            .await
            .unwrap();

        let result = server
            .session_start(Parameters(SessionStartParams {
                agent_id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                context_hint: None,
                project: None,
                api_key: None,
            }))
            .await
            .unwrap();

        let text = match result.content.first() {
            Some(ContentBlock::Text(t)) => t.text.clone(),
            _ => String::new(),
        };
        assert!(text.contains("prefers vi"));

        let summary = compliance.get_summary(Some("tester"), 10).await.unwrap();
        assert_eq!(
            summary.total_sessions, 1,
            "session_start over stdio must create a compliance session"
        );

        // `inject_session_id` travels via `structured_content`, not the text
        // block — the formatted memory context handed to the agent's model
        // must stay free of tool-plumbing IDs, but the caller still needs
        // this to invoke `report_compliance` afterward.
        let sid = result
            .structured_content
            .as_ref()
            .and_then(|v| v.get("inject_session_id"))
            .and_then(|v| v.as_str())
            .expect("inject_session_id must be present in structured_content")
            .to_string();

        let report_text = tool_text(
            server
                .report_compliance(Parameters(ReportComplianceParams {
                    inject_session_id: sid.clone(),
                    reports: vec![ComplianceReportItem {
                        memory_id: "mem_a".to_string(),
                        status: "followed".to_string(),
                        evidence: None,
                    }],
                }))
                .await,
        );
        // The memory id in this fixture is auto-generated, so the specific
        // item won't match — what matters is that the session id itself
        // resolves (no "not enabled"/"no such session" error), proving the
        // manual round-trip is now reachable over stdio.
        assert!(
            !report_text.to_lowercase().contains("not enabled"),
            "report_compliance must work now that stdio returns inject_session_id: {}",
            report_text
        );
    }

    #[tokio::test]
    async fn test_tool_session_start_respects_inject_channel() {
        // Feature F: an agent whose canonical channel is the transparent proxy
        // must NOT also be injected via MCP session_start.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![AgentProfile {
            id: "proxy-owned".to_string(),
            agent_type: "coding-assistant".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: None,
            inject_channel: Some(InjectChannel::Proxy),
        }];
        let router = Arc::new(MemoryRouter::with_registry(store.clone(), registry));
        let server = MemVaultMcp::new(store, router, LazyEmbedder::fixed(None), None);

        server
            .save_memory(Parameters(save_params("user prefers vi")))
            .await
            .unwrap();

        let text = tool_text(
            server
                .session_start(Parameters(SessionStartParams {
                    agent_id: "proxy-owned".to_string(),
                    agent_type: "coding-assistant".to_string(),
                    context_hint: None,
                    project: None,
                    api_key: None,
                }))
                .await,
        );
        assert!(
            text.contains("'proxy' channel"),
            "must explain the canonical channel: {}",
            text
        );
        assert!(
            !text.contains("prefers vi"),
            "memory must not be double-injected: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_tool_review_actions() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("to review")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        // approve
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id.clone(),
                action: "approve".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(tool_text(result).contains("approved"));

        // edit
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id.clone(),
                action: "edit".to_string(),
                edited_content: Some("edited".to_string()),
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(tool_text(result).contains("edited"));

        // reject is only for pending memories; an approved memory
        // must survive and be removed explicitly via delete_memory.
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id.clone(),
                action: "reject".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(result.is_err());
        assert!(server.store.get(&id).await.is_ok());

        // invalid action
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: "mem_x".to_string(),
                action: "bogus".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(result.is_err());
    }

    fn extract_id(result: &CallToolResult) -> String {
        let text = match result.content.first() {
            Some(ContentBlock::Text(t)) => t.text.clone(),
            _ => String::new(),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        v["id"].as_str().unwrap_or("").to_string()
    }

    #[tokio::test]
    async fn test_tool_review_missing_memory_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: "mem_missing".to_string(),
                action: "approve".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_delete_memory() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("delete me")))
            .await
            .unwrap();
        let id = extract_id(&saved);
        let text = tool_text(
            server
                .delete_memory(Parameters(DeleteMemoryParams {
                    memory_id: id,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("deleted"));
    }

    /// Regression: every mutating/admin MCP tool used to skip
    /// `authenticate_agent` entirely while its REST equivalent required it —
    /// a network-exposed auth bypass via the MCP/SSE transport. Once an
    /// agent has a registered key, calling `delete_memory` for that agent
    /// without (or with the wrong) credentials must now be rejected.
    #[tokio::test]
    async fn test_tool_delete_memory_requires_auth_when_agent_has_key() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![AgentProfile {
            id: "admin".to_string(),
            agent_type: "admin".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: Some("s3cr3t".to_string()),
            inject_channel: None,
        }];
        let router = Arc::new(MemoryRouter::with_registry(store.clone(), registry));
        let server = MemVaultMcp::new(store, router, LazyEmbedder::fixed(None), None);

        let saved = server
            .save_memory(Parameters(save_params("protect me")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        // No credentials at all -> rejected.
        let result = server
            .delete_memory(Parameters(DeleteMemoryParams {
                memory_id: id.clone(),
                agent_id: "admin".to_string(),
                api_key: None,
            }))
            .await;
        assert!(
            result.is_err(),
            "delete without api_key must be rejected once the agent has a registered key"
        );

        // Wrong key -> also rejected.
        let result = server
            .delete_memory(Parameters(DeleteMemoryParams {
                memory_id: id.clone(),
                agent_id: "admin".to_string(),
                api_key: Some("wrong".to_string()),
            }))
            .await;
        assert!(result.is_err());

        // Correct key -> succeeds.
        let result = server
            .delete_memory(Parameters(DeleteMemoryParams {
                memory_id: id,
                agent_id: "admin".to_string(),
                api_key: Some("s3cr3t".to_string()),
            }))
            .await;
        assert!(tool_text(result).contains("deleted"));
    }

    fn forget_params(memory_id: Option<&str>, dry_run: bool) -> ForgetMemoryParams {
        ForgetMemoryParams {
            memory_id: memory_id.map(|s| s.to_string()),
            namespace_prefix: None,
            namespace: None,
            source_agent_id: None,
            tag: None,
            memory_type: None,
            before_days: None,
            allow_must: false,
            reason: Some("test".to_string()),
            dry_run,
            agent_id: "tester".to_string(),
            api_key: None,
        }
    }

    #[tokio::test]
    async fn test_tool_forget_memory_dry_run() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("forget me")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        // dry_run defaults to true and must not purge.
        let text = tool_text(
            server
                .forget_memory(Parameters(forget_params(Some(&id), true)))
                .await,
        );
        assert!(text.contains("\"dry_run\": true"));
        assert!(server.store.get(&id).await.is_ok());

        let text = tool_text(
            server
                .forget_memory(Parameters(forget_params(Some(&id), false)))
                .await,
        );
        assert!(text.contains(&id));
        assert!(server.store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_tool_forget_memory_requires_auth() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![AgentProfile {
            id: "admin".to_string(),
            agent_type: "admin".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: Some("s3cr3t".to_string()),
            inject_channel: None,
        }];
        let router = Arc::new(MemoryRouter::with_registry(store.clone(), registry));
        let server = MemVaultMcp::new(store, router, LazyEmbedder::fixed(None), None);

        let saved = server
            .save_memory(Parameters(save_params("protect me from forget")))
            .await
            .unwrap();
        let id = extract_id(&saved);

        let mut params = forget_params(Some(&id), false);
        params.agent_id = "admin".to_string();
        let result = server.forget_memory(Parameters(params)).await;
        assert!(
            result.is_err(),
            "forget without api_key must be rejected once the agent has a registered key"
        );
    }

    #[tokio::test]
    async fn test_tool_forget_memory_must_requires_allow_must() {
        let (server, _comp) = build_server(false);
        let mut params = save_params("must rule");
        params.priority = "MUST".to_string();
        let saved = server.save_memory(Parameters(params)).await.unwrap();
        let id = extract_id(&saved);

        let text = tool_text(
            server
                .forget_memory(Parameters(forget_params(Some(&id), false)))
                .await,
        );
        assert!(
            text.contains("\"purged\": []"),
            "Must-priority memory must not be purged without allow_must: {text}"
        );
        assert!(
            text.contains("must-protected") && text.contains(&id),
            "the response must say WHY nothing was purged, not just '0 purged': {text}"
        );
        assert!(server.store.get(&id).await.is_ok());

        let mut allow_params = forget_params(Some(&id), false);
        allow_params.allow_must = true;
        let text = tool_text(server.forget_memory(Parameters(allow_params)).await);
        assert!(text.contains(&id));
        assert!(server.store.get(&id).await.is_err());
    }

    /// Issue #1 regression: an unknown `memory_type` used to be silently
    /// parsed as `None` ("no type filter"), widening a bulk hard-delete from
    /// the intended type to EVERY type. It must be rejected as invalid params
    /// — on both the dry_run preview and the execute path — while absent
    /// `memory_type` (match all types) and valid values keep working.
    #[tokio::test]
    async fn test_tool_forget_memory_rejects_unknown_memory_type() {
        let (server, _comp) = build_server(false);

        let mut fact = save_params("bulk fact");
        fact.r#type = "fact".to_string();
        fact.namespace = "fixture:issue1".to_string();
        let fact_id = extract_id(&server.save_memory(Parameters(fact)).await.unwrap());

        let mut pref = save_params("bulk preference");
        pref.r#type = "preference".to_string();
        pref.namespace = "fixture:issue1".to_string();
        let pref_id = extract_id(&server.save_memory(Parameters(pref)).await.unwrap());

        // Unknown type → invalid params, for dry_run AND execute.
        for dry_run in [true, false] {
            let mut params = forget_params(None, dry_run);
            params.namespace = Some("fixture:issue1".to_string());
            params.memory_type = Some("INVALID".to_string());
            let err = server
                .forget_memory(Parameters(params))
                .await
                .expect_err("unknown memory_type must be rejected");
            assert_eq!(err.code, rmcp::model::ErrorCode(-32602));
            assert!(
                err.message.contains("unknown memory_type"),
                "error should name the bad value: {}",
                err.message
            );
        }
        // The rejected calls must not have deleted anything.
        assert!(server.store.get(&fact_id).await.is_ok());
        assert!(server.store.get(&pref_id).await.is_ok());

        // A valid type filter (with case/whitespace noise) still narrows the
        // purge to that type only.
        let mut params = forget_params(None, false);
        params.namespace = Some("fixture:issue1".to_string());
        params.memory_type = Some("  FACT ".to_string());
        let text = tool_text(server.forget_memory(Parameters(params)).await);
        assert!(text.contains(&fact_id), "{text}");
        assert!(server.store.get(&fact_id).await.is_err());
        assert!(
            server.store.get(&pref_id).await.is_ok(),
            "other types must survive a typed purge"
        );

        // Omitted memory_type still means "all types" (legitimate bulk wipe
        // of the namespace), unchanged from before.
        let mut params = forget_params(None, false);
        params.namespace = Some("fixture:issue1".to_string());
        let text = tool_text(server.forget_memory(Parameters(params)).await);
        assert!(text.contains(&pref_id), "{text}");
        assert!(server.store.get(&pref_id).await.is_err());
    }

    #[tokio::test]
    async fn test_tool_confirm_read_empty_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .confirm_read(Parameters(ConfirmReadParams {
                memory_ids: Vec::new(),
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_confirm_read_with_ids() {
        let (server, _comp) = build_server(false);
        let saved = server
            .save_memory(Parameters(save_params("read me")))
            .await
            .unwrap();
        let id = extract_id(&saved);
        let text = tool_text(
            server
                .confirm_read(Parameters(ConfirmReadParams {
                    memory_ids: vec![id],
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("confirmed"));
    }

    #[tokio::test]
    async fn test_tool_import_skills_parses_sop() {
        let (server, _comp) = build_server(false);
        // "## No Steps Section" is nested under the "# Deploy Runbook" H1,
        // so sop.rs's H1/H2 nesting rule absorbs it as a subsection of that
        // ONE skill (matching how real Claude/Hermes SKILL.md files are
        // structured — an H1 title with descriptive H2 subsections) rather
        // than splitting it off as a second, separate, empty skill entry.
        // It contributes no steps but also does not count as a skipped
        // section, since it was never its own skill attempt.
        let markdown = "# Deploy Runbook\ntrigger: deploy\nverification: health ok\n1. build\n2. push\n\n## No Steps Section\njust prose\n";
        let text = tool_text(
            server
                .import_skills(Parameters(ImportSkillsParams {
                    markdown: markdown.to_string(),
                    fallback_title: "fb".to_string(),
                    namespace: "global".to_string(),
                    approve: false,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Deploy Runbook"));
        assert!(text.contains("\"skipped_no_steps\": 0"));

        // The skill was saved with meta and lands in the review queue.
        let saved = server.store.list(None, 10, 0).await.unwrap();
        let skill = saved
            .iter()
            .find(|m| m.content == "Deploy Runbook")
            .unwrap();
        assert_eq!(skill.memory_type, MemoryType::Skill);
        assert!(!skill.human_reviewed);
        assert_eq!(
            skill.skill_meta.as_ref().unwrap().trigger.as_deref(),
            Some("deploy")
        );
    }

    #[tokio::test]
    async fn test_tool_import_skills_skips_sibling_section_without_steps() {
        let (server, _comp) = build_server(false);
        // Two flat, sibling H2 sections with no wrapping H1 — the
        // multi-SOP-file shape where each heading really is its own
        // independent skill attempt, so a heading with no list items is
        // still correctly counted as skipped (unlike the nested-under-H1
        // case in `test_tool_import_skills_parses_sop` above).
        let markdown = "## Deploy Runbook\ntrigger: deploy\n1. build\n2. push\n\n## No Steps Section\njust prose\n";
        let text = tool_text(
            server
                .import_skills(Parameters(ImportSkillsParams {
                    markdown: markdown.to_string(),
                    fallback_title: "fb".to_string(),
                    namespace: "global".to_string(),
                    approve: false,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Deploy Runbook"));
        assert!(text.contains("\"skipped_no_steps\": 1"));
    }

    #[tokio::test]
    async fn test_tool_run_dedup_decay_promote() {
        let (server, _comp) = build_server(false);
        server
            .save_memory(Parameters(save_params("dup text")))
            .await
            .unwrap();

        let text = tool_text(
            server
                .run_dedup(Parameters(RunDedupParams {
                    namespace: None,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("unique_count"));

        let text = tool_text(
            server
                .run_decay(Parameters(RunDecayParams {
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("updated"));

        let text = tool_text(
            server
                .run_promote(Parameters(RunPromoteParams {
                    namespace: None,
                    min_l1: Some(1),
                    min_l2: Some(1),
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("promoted_to_l2"));
    }

    /// Regression: the `run_decay` MCP tool must actually attach a
    /// `ForgetManager` via `with_forget` — otherwise enabling
    /// `MEMVAULT_FORGET_AUTO_ENABLED` would silently never purge anything
    /// through this tool.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_tool_run_decay_triggers_auto_forget_when_enabled() {
        let _guard = FORGET_ENV_LOCK.lock().unwrap();
        let (server, _comp) = build_server(false);

        let mut long_archived = Memory::new(
            MemoryType::Fact,
            "ancient archived fact".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "tester".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        );
        long_archived.namespace = "archived:global".to_string();
        long_archived.archived_at = Some(chrono::Utc::now() - chrono::Duration::days(200));
        let id = long_archived.id.clone();
        server.store.save(long_archived).await.unwrap();

        unsafe {
            std::env::set_var("MEMVAULT_FORGET_AUTO_ENABLED", "1");
            std::env::set_var("MEMVAULT_FORGET_RETENTION_DAYS", "90");
        }
        let text = tool_text(
            server
                .run_decay(Parameters(RunDecayParams {
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        unsafe {
            std::env::remove_var("MEMVAULT_FORGET_AUTO_ENABLED");
            std::env::remove_var("MEMVAULT_FORGET_RETENTION_DAYS");
        }

        assert!(text.contains("\"auto_forgotten\": 1"));
        assert!(server.store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_tool_list_inbox() {
        let (server, _comp) = build_server(false);
        server
            .save_memory(Parameters(save_params("needs review")))
            .await
            .unwrap();
        let text = tool_text(
            server
                .list_inbox(Parameters(ListInboxParams {
                    namespace: None,
                    limit: 10,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("needs review"));
    }

    #[tokio::test]
    async fn test_tool_list_inbox_surfaces_contradicts_relation() {
        let (server, _comp) = build_server(false);
        let old_lesson = extract_id(
            &server
                .save_memory(Parameters(save_params("old lesson: retry on timeout")))
                .await
                .unwrap(),
        );
        let draft = extract_id(
            &server
                .save_memory(Parameters(save_params(
                    "updated lesson: back off then retry",
                )))
                .await
                .unwrap(),
        );

        let mut params = evidence_params(&old_lesson, "contradicts");
        params.evidence_id = Some(draft.clone());
        server.add_evidence(Parameters(params)).await.unwrap();

        let text = tool_text(
            server
                .list_inbox(Parameters(ListInboxParams {
                    namespace: None,
                    limit: 10,
                    agent_id: "tester".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(
            text.contains("contradicts"),
            "the draft's contradicts relation must be visible in the inbox listing: {text}"
        );
    }

    #[tokio::test]
    async fn test_tool_compliance_report_without_store_errors() {
        let (server, _comp) = build_server(false);
        let result = server
            .report_compliance(Parameters(ReportComplianceParams {
                inject_session_id: "inj_x".to_string(),
                reports: vec![ComplianceReportItem {
                    memory_id: "mem_x".to_string(),
                    status: "followed".to_string(),
                    evidence: None,
                }],
            }))
            .await;
        assert!(result.is_err());

        let result = server
            .get_compliance_report(Parameters(GetComplianceReportParams {
                inject_session_id: Some("inj_x".to_string()),
                agent_id: None,
                limit: 10,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_compliance_report_with_store() {
        let (server, comp) = build_server(true);
        let comp = comp.expect("compliance store present");
        let sid = "inj_test";

        // Record an injection manually — this test exercises manual
        // `report_compliance` reporting in isolation, independent of the
        // auto-recording `session_start` now also does.
        comp.record_injection(sid, "mem_a", &Priority::Must, "tester")
            .await
            .unwrap();

        let text = tool_text(
            server
                .report_compliance(Parameters(ReportComplianceParams {
                    inject_session_id: sid.to_string(),
                    reports: vec![ComplianceReportItem {
                        memory_id: "mem_a".to_string(),
                        status: "followed".to_string(),
                        evidence: Some("did it".to_string()),
                    }],
                }))
                .await,
        );
        assert!(text.contains("1/1 items updated"));

        let text = tool_text(
            server
                .get_compliance_report(Parameters(GetComplianceReportParams {
                    inject_session_id: Some(sid.to_string()),
                    agent_id: None,
                    limit: 10,
                }))
                .await,
        );
        assert!(text.contains("inj_test"), "report output: {}", text);

        let text = tool_text(
            server
                .get_compliance_report(Parameters(GetComplianceReportParams {
                    inject_session_id: None,
                    agent_id: Some("tester".to_string()),
                    limit: 10,
                }))
                .await,
        );
        assert!(text.contains("overall_rate") || text.contains("total_sessions"));
    }

    #[tokio::test]
    async fn test_server_info_and_resources() {
        let (server, _comp) = build_server(false);
        let info = server.get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "server advertises the tools capability"
        );
        assert!(info.capabilities.resources.is_some());
        let instructions = info.instructions.as_deref().unwrap_or("");
        assert!(instructions.contains("MemVault"));
    }
    // ---- HTTP round-trip: ServerHandler base surface ----
    //
    // `list_resources`/`read_resource`/`on_initialized` take context types that
    // cannot be constructed from outside rmcp, so they are exercised through a
    // real client connection over the streamable-HTTP transport.

    use axum::Router;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    use rmcp::{RoleClient, ServiceExt};

    async fn spawn_mcp_http_server(server: MemVaultMcp) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let session_manager = Arc::new(LocalSessionManager::default());
        let svc = StreamableHttpService::new(
            move || Ok::<_, std::io::Error>(server.clone()),
            session_manager,
            StreamableHttpServerConfig::default(),
        );
        let app = Router::new().route("/mcp", axum::routing::any_service(svc));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}/mcp", addr)
    }

    #[tokio::test]
    async fn test_http_roundtrip_resources_and_read() {
        let (server, _comp) = build_server(false);
        let url = spawn_mcp_http_server(server).await;

        // Keep `service` alive: dropping the RunningService shuts the transport.
        let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.as_str());
        let service = ().serve(transport).await.expect("client connects");
        let peer: rmcp::service::Peer<RoleClient> = service.peer().clone();

        // get_info + initialize handshake, then list_resources over the wire.
        let resources = peer.list_all_resources().await.expect("list resources");
        let uris: Vec<String> = resources.iter().map(|r| r.uri.clone()).collect();
        assert!(
            uris.iter().any(|u| u == "memory://user-profile")
                && uris.iter().any(|u| u == "memory://project-context"),
            "both local resources must be advertised, got {uris:?}"
        );

        // list_all_tools triggers the tool listing (get_info capabilities).
        let tools = peer.list_all_tools().await.expect("list tools");
        assert!(tools.iter().any(|t| t.name == "save_memory"));
        assert_eq!(tools.len(), 20, "unexpected MCP tool count: {tools:?}");
        for tool in &tools {
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("tool {} must advertise annotations", tool.name));
            assert!(
                annotations.title.as_deref().is_some_and(|t| !t.is_empty()),
                "tool {} must have a non-empty title",
                tool.name
            );
            assert_eq!(
                annotations.open_world_hint,
                Some(false),
                "memory tools must be closed-world: {}",
                tool.name
            );
            assert!(
                annotations.read_only_hint.is_some(),
                "tool {} must declare read_only_hint",
                tool.name
            );
        }

        let delete_tool = tools
            .iter()
            .find(|t| t.name == "delete_memory")
            .expect("delete_memory advertised");
        let delete_annotations = delete_tool.annotations.as_ref().expect("annotations");
        assert_eq!(delete_annotations.read_only_hint, Some(false));
        assert_eq!(delete_annotations.destructive_hint, Some(true));
        let delete_description = delete_tool.description.as_deref().unwrap_or_default();
        assert!(
            delete_description.contains("forget_memory")
                && delete_description.contains("memvault restore"),
            "delete_memory must explain the soft-delete/purge distinction: {delete_description}"
        );

        let search_tool = tools
            .iter()
            .find(|t| t.name == "search_memory")
            .expect("search_memory advertised");
        let search_annotations = search_tool.annotations.as_ref().expect("annotations");
        assert_eq!(search_annotations.read_only_hint, Some(true));
        let search_description = search_tool.description.as_deref().unwrap_or_default();
        assert!(
            search_description.contains("defaults to 'hybrid'"),
            "search_memory default must match default_search_mode(): {search_description}"
        );

        // read_resource on an empty store -> friendly empty message.
        let empty = peer
            .read_resource(ReadResourceRequestParams::new("memory://user-profile"))
            .await
            .expect("read empty resource");
        let empty_text = match empty.contents.first() {
            Some(ResourceContents::TextResourceContents { text, .. }) => text.clone(),
            _ => String::new(),
        };
        assert_eq!(empty_text, "No memories stored yet.");

        // Save a MUST memory through the tool router.
        let mut args = serde_json::Map::new();
        args.insert(
            "content".to_string(),
            serde_json::Value::String("server roundtrip memory".to_string()),
        );
        args.insert(
            "priority".to_string(),
            serde_json::Value::String("MUST".to_string()),
        );
        peer.call_tool(CallToolRequestParams::new("save_memory").with_arguments(args))
            .await
            .expect("save_memory over HTTP must succeed");

        // read_resource now reflects the saved memory.
        let filled = peer
            .read_resource(ReadResourceRequestParams::new("memory://user-profile"))
            .await
            .expect("read filled resource");
        let filled_text = match filled.contents.first() {
            Some(ResourceContents::TextResourceContents { text, .. }) => text.clone(),
            _ => String::new(),
        };
        assert!(
            filled_text.contains("server roundtrip memory"),
            "resource must include the saved memory: {filled_text}"
        );
    }

    #[tokio::test]
    async fn test_http_initialize_backfills_vectorless_memories() {
        // Reproduces the stdio boot race end to end: a memory persisted without
        // a vector (as a save arriving during provider warm-up does) must be
        // backfilled once the client's `initialize` handshake fires
        // `on_initialized`.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        store
            .save(Memory::new(
                MemoryType::Fact,
                "python 项目的构建与发布约定".to_string(),
                Priority::Reference,
                SourceAgent {
                    id: "claude-code".to_string(),
                    agent_type: "coding-assistant".to_string(),
                    session_id: None,
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            store.list_without_embedding(10).await.unwrap().len(),
            1,
            "precondition: one vectorless memory"
        );

        let router = Arc::new(MemoryRouter::new(store.clone()));
        // Provider not ready at handshake time — `on_initialized` must wait for
        // it, not sample readiness once (which would skip the backfill and
        // leave the memory vectorless for the process lifetime).
        let embedder = LazyEmbedder::delayed(
            Arc::new(FakeEmbedder { fail: false }),
            std::time::Duration::from_millis(40),
        );
        assert!(
            !embedder.ready().await,
            "precondition: provider not ready yet"
        );
        let server = MemVaultMcp::new(store.clone(), router, embedder, None);
        let url = spawn_mcp_http_server(server).await;

        let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.as_str());
        let service = ().serve(transport).await.expect("client connects");
        let peer: rmcp::service::Peer<RoleClient> = service.peer().clone();
        // The handshake fires `on_initialized`, which schedules the backfill.
        peer.list_all_resources()
            .await
            .expect("initialize handshake");

        for _ in 0..100 {
            if store.list_without_embedding(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            store.list_without_embedding(10).await.unwrap().is_empty(),
            "initialize must backfill memories that were saved without a vector"
        );
    }

    /// Serializes tests that mutate the process-global `MEMVAULT_EMBEDDING_PROVIDER`
    /// env var read by `embedding_explicitly_disabled`/`lazy_embedder_from_env`.
    /// A `tokio::sync::Mutex` (not `std::sync::Mutex`) because several of
    /// these tests hold the guard across an `.await`.
    static EMBEDDING_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn test_embedding_explicitly_disabled_values() {
        let _guard = EMBEDDING_ENV_LOCK.blocking_lock();
        for val in ["none", "disabled", "off", "NONE", "Off"] {
            unsafe { std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", val) };
            assert!(
                embedding_explicitly_disabled(),
                "{val} should disable embeddings"
            );
        }
        for val in ["native", "openai", ""] {
            unsafe { std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", val) };
            assert!(
                !embedding_explicitly_disabled(),
                "{val} should not disable embeddings"
            );
        }
        unsafe { std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER") };
        assert!(
            !embedding_explicitly_disabled(),
            "unset provider should not disable embeddings"
        );
    }

    #[tokio::test]
    async fn test_lazy_embedder_from_env_disabled_returns_fixed_none() {
        let _guard = EMBEDDING_ENV_LOCK.lock().await;
        unsafe { std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", "none") };
        let embedder = lazy_embedder_from_env();
        unsafe { std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER") };
        assert!(
            embedder.get().await.is_none(),
            "explicit `none` must short-circuit to a fixed None provider, no network probe"
        );
    }

    #[tokio::test]
    async fn test_build_router_with_embedder_without_registry_file() {
        let _guard = EMBEDDING_ENV_LOCK.lock().await;
        unsafe { std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", "none") };
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let embedder = lazy_embedder_from_env();
        let registry_path = std::env::temp_dir().join(format!(
            "memvault_mcp_no_registry_{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        let result = build_router_with_embedder(store, &registry_path, &embedder).await;
        unsafe { std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER") };
        assert!(
            result.is_ok(),
            "a missing registry file must fall back to a default router, not error"
        );
    }

    #[tokio::test]
    async fn test_build_router_with_embedder_with_registry_file() {
        let _guard = EMBEDDING_ENV_LOCK.lock().await;
        unsafe { std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", "none") };
        let dir = std::env::temp_dir().join(format!(
            "memvault_mcp_registry_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let registry_path = dir.join("agents.yaml");
        std::fs::write(
            &registry_path,
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

        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let embedder = lazy_embedder_from_env();
        let result = build_router_with_embedder(store, &registry_path, &embedder).await;
        unsafe { std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER") };
        std::fs::remove_dir_all(&dir).ok();

        let router = result.expect("an existing registry file must load successfully");
        let profile = router.get_agent_profile("yaml-agent");
        assert_eq!(profile.inject_rules.max_memories, 3);
    }
}
