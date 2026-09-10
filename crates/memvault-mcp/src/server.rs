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
use memvault_core::embedding::{EmbeddingProvider, build_embedder_from_env};
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
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    compliance: Option<Arc<ComplianceStore>>,
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
    /// (docs/PAPER-INSPIRATIONS.md Feature A).
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    /// Search query string
    pub query: String,
    /// Search mode: "keyword" (default), "semantic" (vector), or "hybrid" (both)
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
        store: Arc<SqliteStore>,
        router: Arc<MemoryRouter>,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        compliance: Option<Arc<ComplianceStore>>,
    ) -> Self {
        Self {
            store,
            router,
            embedder,
            compliance,
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

    #[tool(
        description = "Save a new memory. Memories are persistent user preferences, facts, episodes, or skills that should be recalled in future conversations. Embeddings are generated automatically for semantic search. Delta-write is on by default: near-duplicate saves are skipped and similar existing memories absorb the new content instead of a new row being created (status in the response: saved | merged | skipped). Pass force_insert=true to bypass."
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
        let mut embedding: Option<Vec<f32>> = None;
        if let Some(ref embedder) = self.embedder {
            match embedder.embed(&[embed_text]).await {
                Ok(embeddings) if !embeddings.is_empty() => {
                    debug!(id = %mem.id, dim = embeddings[0].len(), "auto-embedded memory");
                    embedding = embeddings.into_iter().next();
                }
                Err(e) => warn!("Auto-embedding failed, saving without: {}", e),
                _ => {}
            }
        }
        let embedded = embedding.is_some();

        // Delta write (docs/PAPER-INSPIRATIONS.md Feature A): dedup within the
        // same namespace first — near-duplicates skip, similar memories absorb
        // the residual, force_insert bypasses. Skills (SOPs) are procedural
        // knowledge; merging them into facts would not be meaningful, so they
        // always force-insert.
        let force = params.force_insert || mem.skill_meta.is_some();
        let writer = MemoryWriter::new(self.store.clone(), self.embedder.clone());
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
                "similarity": similarity,
                "residual_added": residual_added,
            }),
            WriteOutcome::Skipped { memory, similarity } => serde_json::json!({
                "status": "skipped",
                "id": memory.id,
                "priority": format!("{:?}", memory.priority),
                "embedded": embedded,
                "similarity": similarity,
            }),
        };

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Record the outcome of a task you just executed (episodic memory). Call this after finishing a task so past successes and failures can inform future sessions. Status must be 'success', 'failure', or 'partial'. Failures with a cause are reflected into lessons that get injected into similar future tasks."
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

        let recorded = memvault_core::episode::record_outcome(
            self.store.as_ref(),
            input.clone(),
            self.embedder.as_deref(),
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Reflection: failures/partials get a lesson distilled (LLM when
        // configured, conservative rules otherwise) and linked back to the
        // episode. Best-effort — a reflection hiccup must not lose the outcome.
        let mut lesson_json = serde_json::Value::Null;
        let llm = self.llm_extractor.get().await;
        match memvault_core::reflection::reflect_and_store(
            self.store.as_ref(),
            &recorded.memory.id,
            input.clone().into(),
            llm.as_deref(),
            self.embedder.as_deref(),
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
            Err(e) => warn!(error = %e, "lesson reflection failed; outcome kept"),
        }

        // Effectiveness: pair this outcome against recent pending injections
        // for the same agent and let the LLM judge (when configured) decide
        // useful/neutral/harmful/insufficient_context. Best-effort — no
        // judge configured means 0 judged, never a fabricated verdict.
        let mut effectiveness_judged = 0usize;
        if let Some(ref cs) = self.compliance {
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
            "status": "recorded",
            "id": recorded.memory.id,
            "outcome": recorded.memory.content,
            "embedded": recorded.embedded,
            "lesson": lesson_json,
            "effectiveness_judged": effectiveness_judged,
            "flagged_skills": recorded.flagged_skills,
            "skill_draft_id": recorded.skill_draft_id,
            "note": match status {
                OutcomeStatus::Failure | OutcomeStatus::Partial =>
                    "failure recorded — the lesson will be injected into similar future tasks",
                OutcomeStatus::Success =>
                    "success recorded — repeated successes may consolidate into a reusable skill",
            },
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Search user memories. Supports three modes: 'keyword' (text match), 'semantic' (vector similarity), or 'hybrid' (both combined with RRF). Default is 'hybrid' when embeddings are available."
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
        let mut actual_mode = mode.as_str();

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

        // vector search
        let vector_results = if actual_mode != "keyword" {
            if let Some(ref embedder) = self.embedder {
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
                if actual_mode == "semantic" {
                    return Err(McpError::invalid_params(
                        "Semantic search requires an embedding provider. Set OPENAI_API_KEY or use mode='keyword'.".to_string(),
                        None,
                    ));
                }
                actual_mode = "keyword";
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
        description = "Start a new session. Returns relevant memories filtered by agent identity, formatted as MUST/REF instructions ready for injection into the conversation context. Overflow memories are shown as summaries with a hint to use search_memory for details."
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
        description = "Review a pending memory: approve it, reject it, or edit its content. Approved memories get higher priority in future injections."
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
                if let Some(content) = params.edited_content {
                    mem.content = content;
                }
                if let Some(instruction) = params.edited_instruction {
                    mem.instruction = Some(instruction);
                }
                mem.human_reviewed = true;
                mem.updated_at = chrono::Utc::now();
                self.store
                    .update(mem)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
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
        description = "Record evidence for a memory. relation=\"supports\"/\"contradicts\" links evidence_id → memory_id (a memory supporting or contradicting it); relation=\"sourced_from\" records memory_id's external provenance via `source` (URL/document). Contradictions accelerate the contradicted memory's decay; superseded or archived evidence stops counting."
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

    #[tool(description = "Delete a memory by its ID.")]
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
        description = "Extract structured memories from conversation text. mode=\"rule\" (default) detects preferences, facts, and skills via keyword pattern matching. mode=\"llm\" instead understands the full context (optionally pairing assistant_text) semantically — uses the configured LLM extraction provider, and degrades to rule-based extraction (reported via fallback_used/fallback_reason) when no provider is configured or the LLM call fails. Returns extracted items; optionally saves them."
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

                let saved = self
                    .store
                    .save(mem)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                saved_ids.push(saved.id);
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
                .map(|v| v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("1"))
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
        description = "Import skills from a Markdown SOP document. Each #/## heading becomes a skill (trigger:/verification: metadata lines + list-item steps). Sections without steps are skipped. Imported skills enter the review inbox unless approve=true."
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
        description = "Scan for duplicate memories and report findings. Uses text similarity (Jaccard) to detect near-duplicates."
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
        description = "Run memory decay cycle. Reduces decay_score for old memories, archives memories below threshold. MUST memories are exempt."
    )]
    async fn run_decay(
        &self,
        Parameters(params): Parameters<RunDecayParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let dm = memvault_core::decay::DecayManager::new(
            self.store.clone(),
            memvault_core::decay::DecayConfig::default(),
        );
        let report = dm
            .run_decay()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "updated": report.updated,
            "archived": report.archived,
            "contradicted": report.contradicted,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Confirm that one or more memories have been read by the agent. Updates access_count and last_read_at for the specified memory IDs."
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

        self.router
            .confirm_read(&params.memory_ids)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "confirmed": params.memory_ids.len(),
            "memory_ids": params.memory_ids,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List memories pending human review. Shows memories that have not been reviewed yet, sorted by created_at ascending (oldest first)."
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

        let output: Vec<serde_json::Value> = memories
            .iter()
            .map(|m| {
                serde_json::json!({
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
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Get the raw-evidence chain a memory was distilled from (its L0 trace rows), plus its evidence profile. Read-only grounding: lets an agent quote the original session text and name its sources."
    )]
    async fn get_memory_evidence(
        &self,
        Parameters(params): Parameters<GetMemoryEvidenceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let evidence = memvault_core::evidence::trace_evidence_chain(
            self.store.as_ref(),
            &params.memory_id,
        )
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
        description = "Run the promote pipeline: consolidates atomic L1 memories into L2 scenario summaries, and promotes stable L2 memories into L3 core persona rules. Source memories are archived to L0 after promotion."
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
        description = "Report compliance for a previous injection session. Call this to indicate which injected memories you followed or violated."
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
        description = "Get compliance report for injection sessions. Shows follow/violation rates for MUST memories. Provide inject_session_id for a specific session, or omit for an aggregate summary."
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
        description = "Get automatic effectiveness judgments for injected memories: useful/neutral/harmful/insufficient_context rates, judged automatically from record_outcome — independent of manual report_compliance."
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
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
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

        // Trigger embedding backfill in background so all memories are vector-searchable
        if let Some(ref embedder) = self.embedder {
            MemoryRouter::spawn_embedding_backfill(
                self.store.clone(),
                embedder.clone(),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
        }

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

    let embedder: Option<Arc<dyn EmbeddingProvider>> = build_embedder_from_env().await;

    let router = if registry_path.exists() {
        info!("Loading agent registry from {}", registry_path.display());
        let r = MemoryRouter::load_registry_from_yaml(store.clone(), &registry_path)?;
        if let Some(ref emb) = embedder {
            Arc::new(r.with_embedder(emb.clone()))
        } else {
            Arc::new(r)
        }
    } else {
        let r = MemoryRouter::new(store.clone());
        if let Some(ref emb) = embedder {
            Arc::new(r.with_embedder(emb.clone()))
        } else {
            Arc::new(r)
        }
    };

    let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
    run_stdio_server_with(store, router, embedder, compliance).await
}

pub async fn run_stdio_server_with(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
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
        let server = MemVaultMcp::new(store, router, None, compliance.clone());
        (server, compliance)
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
        assert!(v["evidence"][0]["content"]
            .as_str()
            .unwrap()
            .contains("raw transcript: the deploy window is Friday"));
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
        MemVaultMcp::new(store, router, Some(embedder), None)
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
        let server = MemVaultMcp::new(store, router, None, None);

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

        // reject
        let result = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id,
                action: "reject".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "tester".to_string(),
                api_key: None,
            }))
            .await;
        assert!(tool_text(result).contains("rejected"));

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
        let server = MemVaultMcp::new(store, router, None, None);

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
}
