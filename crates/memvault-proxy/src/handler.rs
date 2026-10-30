use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use memvault_core::compliance::{ComplianceStatus, ComplianceStore};
use memvault_core::embedding::{EMBED_WRITE_WAIT, LazyEmbedder};
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::writer::{MemoryWriter, WriteOutcome};

use crate::context::SessionContext;
use crate::extraction::{ExtractionConfig, ResponseExtractor};
use crate::injection::InjectionEngine;
use crate::merge;
use crate::upstream::UpstreamManager;

// --- Parameter structs ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    pub content: String,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default = "default_type")]
    pub memory_type: String,
    pub instruction: Option<String>,
    pub tags: Option<String>,
    /// Namespace for the memory (default: global)
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// ID of the agent saving this memory (default: proxy-client)
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
    /// Skill trigger pattern (only for memory_type=skill)
    pub skill_trigger: Option<String>,
    /// Skill execution steps (only for memory_type=skill)
    #[serde(default)]
    pub skill_steps: Vec<String>,
    /// Skill verification criteria (only for memory_type=skill)
    pub skill_verification: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_priority() -> String {
    "REFERENCE".to_string()
}
fn default_type() -> String {
    "fact".to_string()
}
fn default_namespace() -> String {
    "global".to_string()
}
fn default_agent_type() -> String {
    "proxy".to_string()
}
fn default_confidence() -> f64 {
    0.8
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    pub query: String,
    #[allow(dead_code)]
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Filter by namespace
    pub namespace: Option<String>,
    /// Filter by memory type
    pub type_filter: Option<String>,
    /// Filter by priority
    pub priority_filter: Option<String>,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_mode() -> String {
    "hybrid".to_string()
}
fn default_top_k() -> usize {
    10
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionStartParams {
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    pub context_hint: Option<String>,
    pub project: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_agent_id() -> String {
    "proxy-client".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReportComplianceParams {
    pub inject_session_id: String,
    pub reports: Vec<ComplianceReportItem>,
    /// ID of the agent reporting compliance (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComplianceReportItem {
    pub memory_id: String,
    pub status: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetComplianceReportParams {
    pub inject_session_id: Option<String>,
    pub agent_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NotifyResponseParams {
    /// The agent's response text to extract memories from
    pub response_text: String,
    /// Optional: the user's own message text for this turn. First-person
    /// signal words ("我偏好"/"我喜欢") match a user's own statements far
    /// more reliably than an assistant's restatement of them, so passing
    /// this alongside response_text substantially improves extraction
    /// recall. Extracted independently and merged into the same result.
    pub user_text: Option<String>,
    /// Agent ID that produced this response
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

/// Host-agnostic outcome evidence (mirrors
/// `memvault_core::outcome_inference::OutcomeEvidence`, same shape as
/// memvault-mcp's `OutcomeEvidenceParams`). dsh/Cordis sessions never
/// produce a Claude-Code-shaped JSONL transcript, so the plugin computes
/// these counts itself from the `tool/result`/`user/message` events it
/// already receives, instead of flattening them into fake transcript text.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutcomeEvidenceParams {
    /// What the agent was trying to do, from the host's own turn history.
    /// Absent always skips inference — there is nothing to anchor an
    /// episode to.
    pub task: Option<String>,
    #[serde(default)]
    pub retry_hits: u32,
    #[serde(default)]
    pub rejection_hits: u32,
    #[serde(default)]
    pub correction_hits: u32,
    /// "unresolved_tool_error" | "unresolved_correction" | "resolved" (default)
    #[serde(default)]
    pub terminal: String,
    pub terminal_detail: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InferOutcomeParams {
    pub evidence: OutcomeEvidenceParams,
    /// Conversation excerpt handed to the LLM judge, if one is configured
    /// (DATA to analyze, not instructions). Empty means only the rule-based
    /// floor runs.
    #[serde(default)]
    pub judge_context: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunPromoteParams {
    pub namespace: Option<String>,
    pub min_l1: Option<usize>,
    pub min_l2: Option<usize>,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDedupParams {
    pub namespace: Option<String>,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDecayParams {
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfirmReadParams {
    pub memory_ids: Vec<String>,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListInboxParams {
    pub namespace: Option<String>,
    #[serde(default = "default_inbox_limit")]
    pub limit: usize,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

fn default_inbox_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewMemoryParams {
    pub memory_id: String,
    /// Action: approve, reject, or edit
    pub action: String,
    pub edited_content: Option<String>,
    pub edited_instruction: Option<String>,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMemoryParams {
    pub memory_id: String,
    /// ID of the requesting agent (default: proxy-client)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// API key for agent authentication (required if agent has a registered key)
    pub api_key: Option<String>,
}

// --- Handler ---

#[derive(Clone)]
pub struct ProxyHandler {
    store: Arc<dyn MemoryStore>,
    router: Arc<MemoryRouter>,
    /// Lazy holder shared with the router; `save_memory` uses it to embed new
    /// memories exactly like the MCP/REST paths (see `save_memory`).
    embedder: LazyEmbedder,
    upstreams: Arc<UpstreamManager>,
    #[allow(dead_code)]
    context: Arc<SessionContext>,
    injection: Arc<InjectionEngine>,
    compliance: Arc<ComplianceStore>,
    extractor: Arc<ResponseExtractor>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ProxyHandler {
    pub async fn new(
        store: Arc<dyn MemoryStore>,
        router: Arc<MemoryRouter>,
        embedder: LazyEmbedder,
        upstreams: Arc<UpstreamManager>,
        context: Arc<SessionContext>,
        injection: Arc<InjectionEngine>,
        compliance: Arc<ComplianceStore>,
    ) -> Self {
        // Share the handler's lazily-probed embedder so auto-extracted
        // memories get vectors + vector-aware dedup, exactly like the
        // manual `save_memory` path (rather than persisting vectorless rows
        // during the provider warm-up window).
        let extraction_builder =
            ResponseExtractor::new(store.clone(), ExtractionConfig::from_env())
                .with_embedder(embedder.clone());
        // Lazy env probe: resolved on first extraction and re-probed after
        // failures — a boot-time Ollama hiccup no longer disables contextual
        // extraction for the lifetime of the proxy.
        let extraction_builder = extraction_builder
            .with_lazy_llm_extractor(memvault_core::llm_extractor::LazyLlmExtractor::from_env());
        let extractor = Arc::new(extraction_builder);
        Self {
            store,
            router,
            embedder,
            upstreams,
            context,
            injection,
            compliance,
            extractor,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Save a new memory. Memories persist across sessions and are injected into future conversations."
    )]
    async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let (_, identity_verified) = self
            .router
            .authenticate_agent_verified(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let priority = match params.priority.to_uppercase().as_str() {
            "MUST" => Priority::Must,
            "BACKGROUND" => Priority::Background,
            _ => Priority::Reference,
        };
        let mem_type = match params.memory_type.as_str() {
            "preference" => MemoryType::Preference,
            "skill" => MemoryType::Skill,
            "episode" => MemoryType::Episode,
            "entity" => MemoryType::Entity,
            _ => MemoryType::Fact,
        };
        let tags_vec: Vec<String> = params
            .tags
            .map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let agent = SourceAgent {
            id: params.agent_id,
            agent_type: params.agent_type,
            session_id: None,
        };

        let mut memory = Memory::new(mem_type, params.content, priority, agent);
        memory.instruction = params.instruction;
        memory.tags = tags_vec;
        memory.namespace = params.namespace;
        memory.confidence = params.confidence;
        memory.identity_verified = identity_verified;
        if let Some(ref l) = params.layer {
            memory.layer = match l.to_uppercase().as_str() {
                "L0" => MemoryLayer::L0,
                "L2" => MemoryLayer::L2,
                "L3" => MemoryLayer::L3,
                _ => MemoryLayer::L1,
            };
        }
        if params.skill_trigger.is_some()
            || !params.skill_steps.is_empty()
            || params.skill_verification.is_some()
        {
            memory.skill_meta = Some(SkillMeta {
                trigger: params.skill_trigger,
                steps: params.skill_steps,
                verification: params.skill_verification,
                version: 1,
            });
        }

        // Parity with the MCP/REST save paths. This tool previously called
        // `store.save` directly, silently skipping *both* auto-embedding (so
        // every proxy save was vectorless until a background backfill happened
        // to run) and the delta-write dedup/merge step.
        let embed_text = memory
            .instruction
            .as_deref()
            .unwrap_or(&memory.content)
            .to_string();
        let mut embedding: Option<Vec<f32>> = None;
        let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        if let Some(ref provider) = provider {
            match provider.embed(std::slice::from_ref(&embed_text)).await {
                Ok(embeddings) if !embeddings.is_empty() => {
                    embedding = embeddings.into_iter().next();
                }
                Err(e) => warn!("Auto-embedding failed, saving without: {}", e),
                _ => {}
            }
        }
        let embedded = embedding.is_some();
        let vector_pending = !embedded && self.embedder.expects_embedding().await;

        // Skills (SOPs) are procedural knowledge; merging them into facts would
        // not be meaningful, so they always force-insert (mirrors MCP/REST).
        let force = memory.skill_meta.is_some();
        let writer = MemoryWriter::new(self.store.clone(), provider);
        let outcome = writer
            .save(memory, embedding, force)
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

    #[tool(description = "Search memories. Modes: 'keyword', 'semantic', 'hybrid' (default).")]
    async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

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

        let search_query = SearchQuery {
            query: params.query,
            top_k: params.top_k,
            namespace: params.namespace,
            type_filter,
            priority_filter,
            agent_id: Some(params.agent_id),
            ..SearchQuery::new(String::new())
        };

        let results = self
            .store
            .search(search_query)
            .await
            .map_err(|e| McpError::internal_error(format!("search failed: {}", e), None))?
            .results;

        let output = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "{}. [{:?}] (score:{:.2}) {}",
                    i + 1,
                    r.memory.priority,
                    r.score,
                    r.memory.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(CallToolResult::success(vec![ContentBlock::text(
            if output.is_empty() {
                "No memories found.".to_string()
            } else {
                output
            },
        )]))
    }

    #[tool(
        description = "Start a session and get relevant memories for injection. Returns formatted MUST/REF instructions with an inject_session_id for compliance tracking."
    )]
    async fn session_start(
        &self,
        Parameters(params): Parameters<SessionStartParams>,
    ) -> Result<CallToolResult, McpError> {
        // Authenticate the agent
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let injection = self
            .router
            .session_start(
                &params.agent_id,
                params.context_hint.as_deref(),
                params.project.as_deref(),
            )
            .await
            .map_err(|e| McpError::internal_error(format!("session_start failed: {}", e), None))?;
        let results = injection.results;
        if !injection.skipped.is_empty() {
            tracing::debug!(
                agent_id = %params.agent_id,
                skipped = ?injection.skipped,
                "session_start dropped candidates with reasons"
            );
        }

        let formatted = self.router.format_as_instructions(&results);
        let session_id = format!("inj_{}", Uuid::new_v4().simple());

        for r in &results {
            let _ = self
                .compliance
                .record_injection(
                    &session_id,
                    &r.memory.id,
                    &r.memory.priority,
                    &params.agent_id,
                )
                .await;
        }

        let output = format!("[inject_session_id: {}]\n{}", session_id, formatted);
        Ok(CallToolResult::success(vec![ContentBlock::text(output)]))
    }

    #[tool(
        description = "Report compliance for a previous injection session. Call this to indicate which injected memories you followed or violated."
    )]
    async fn report_compliance(
        &self,
        Parameters(params): Parameters<ReportComplianceParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut updated = 0;
        for item in &params.reports {
            let status = match item.status.as_str() {
                "followed" => ComplianceStatus::Followed,
                "violated" => ComplianceStatus::Violated,
                _ => ComplianceStatus::Unknown,
            };
            match self
                .compliance
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
        description = "Get compliance report for injection sessions. Shows follow/violation rates for MUST memories; rates are null when no real verdict exists yet (never a fake 100%)."
    )]
    async fn get_compliance_report(
        &self,
        Parameters(params): Parameters<GetComplianceReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let caller = params.agent_id.clone().unwrap_or_else(default_agent_id);
        self.router
            .authenticate_agent(&caller, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if let Some(sid) = params.inject_session_id {
            let report = self
                .compliance
                .get_report(&sid)
                .await
                .map_err(|e| McpError::internal_error(format!("report failed: {}", e), None))?;
            let json = serde_json::to_string_pretty(&report).unwrap_or_default();
            return Ok(CallToolResult::success(vec![ContentBlock::text(json)]));
        }

        let summary = self
            .compliance
            .get_summary(params.agent_id.as_deref(), params.limit)
            .await
            .map_err(|e| McpError::internal_error(format!("summary failed: {}", e), None))?;
        let json = serde_json::to_string_pretty(&summary).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    #[tool(
        description = "Notify the proxy of an agent's response text (and optionally the user's own turn text) for automatic memory extraction. Extracts preferences, facts, and skills from both and saves them to Inbox (unreviewed). Passing user_text substantially improves recall — first-person signal words match a user's own statements more reliably than an assistant's restatement of them. Call this after each agent turn to enable the extraction loop."
    )]
    async fn notify_response(
        &self,
        Parameters(params): Parameters<NotifyResponseParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let result = self
            .extractor
            .extract_and_save_contextual(
                params.user_text.as_deref(),
                &params.response_text,
                &params.agent_id,
            )
            .await;

        if result.extracted == 0 {
            Ok(CallToolResult::success(vec![ContentBlock::text(
                "No extractable memories found in this response.",
            )]))
        } else {
            let pending = if result.vector_pending > 0 {
                format!(", {} awaiting vector backfill", result.vector_pending)
            } else {
                String::new()
            };
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Extraction complete: {} found, {} saved to Inbox, {} skipped (below threshold){}.",
                result.extracted, result.saved, result.skipped, pending
            ))]))
        }
    }

    #[tool(
        description = "Classify and record a task outcome from host-provided evidence rather than raw transcript text — for hosts (e.g. dsh/Cordis) whose event model doesn't produce a Claude-Code-shaped JSONL transcript. Pass pre-computed friction counts and terminal state in `evidence`; runs the same rule-floor-plus-optional-LLM-judge classification as the Claude Code Stop hook, then records the outcome (episode memory + lesson reflection) exactly like a manual outcome report. Returns status 'skipped' (not an error) when nothing was inferred."
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

        // Reuse the extractor's own lazily-probed LLM instance rather than
        // standing up a second probe/cache for the same environment.
        let llm = self.extractor.llm_extractor().await;
        let Some(inferred) = memvault_core::outcome_inference::infer_outcome(
            &evidence,
            &params.judge_context,
            llm.as_deref(),
        )
        .await
        else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "No outcome inferred (clean session, below the friction threshold, or no task evidence).",
            )]));
        };

        let tags = vec![
            "auto-inferred".to_string(),
            match inferred.source {
                memvault_core::outcome_inference::InferenceSource::Rule => "inferred:rule",
                memvault_core::outcome_inference::InferenceSource::Llm => "inferred:llm",
            }
            .to_string(),
        ];

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
                agent_type: "dsh".to_string(),
                session_id: None,
            },
        };

        let recorded =
            memvault_core::episode::record_outcome(self.store.as_ref(), input.clone(), None)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mut lesson_text: Option<String> = None;
        match memvault_core::reflection::reflect_and_store(
            self.store.as_ref(),
            &recorded.memory.id,
            input.into(),
            llm.as_deref(),
            None,
        )
        .await
        {
            Ok(Some(record)) => lesson_text = Some(record.lesson),
            Ok(None) => {}
            Err(e) => warn!(error = %e, "lesson reflection failed; inferred outcome kept"),
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Outcome recorded ({}, source={:?}): {}{}",
            recorded.memory.id,
            inferred.source,
            recorded.memory.content,
            lesson_text
                .map(|l| format!(" — lesson: {l}"))
                .unwrap_or_default(),
        ))]))
    }

    #[tool(
        description = "Run the promote pipeline: consolidates atomic L1 memories into L2 scenario summaries, and promotes stable L2 memories into L3 core persona rules."
    )]
    async fn run_promote(
        &self,
        Parameters(params): Parameters<RunPromoteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let _ = params.namespace;
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

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Promote: {} consolidated to L2, {} promoted to L3 ({} sources consumed)",
            result.promoted_to_l2,
            result.promoted_to_l3,
            result.source_ids_consumed.len()
        ))]))
    }

    #[tool(description = "Scan for duplicate memories and report findings.")]
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

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Dedup: {} unique, {} duplicates found",
            result.unique_count,
            result.duplicates.len()
        ))]))
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
        )
        .with_compliance(self.compliance.clone());
        let report = dm
            .run_decay()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Decay: {} updated, {} archived, {} harmful-flagged",
            report.updated, report.archived, report.harmful_flagged
        ))]))
    }

    #[tool(
        description = "Confirm that one or more memories have been read by the agent. Updates access_count and last_read_at."
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

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Confirmed {} memories as read",
            params.memory_ids.len()
        ))]))
    }

    #[tool(description = "List memories pending human review, sorted oldest first.")]
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
                    "priority": format!("{:?}", m.priority),
                    "type": format!("{:?}", m.memory_type),
                    "tags": m.tags,
                    "namespace": m.namespace,
                    "created_at": m.created_at,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Review a pending memory: approve it, reject it, or edit its content.")]
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
}

#[tool_handler]
impl ServerHandler for ProxyHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_instructions(
            "MemVault Proxy: transparent MCP proxy with automatic memory injection and compliance tracking. \
             All upstream tools are available alongside MemVault memory tools."
        )
    }

    /// Aggregate local + upstream tools. Local tools win name conflicts;
    /// across upstreams the first connection providing a name wins — the
    /// same first-wins rule the upstream forwarding index (`register_index`)
    /// uses, so `tools/list` never advertises a tool `tools/call` would route
    /// somewhere else. These manual impls override the `#[tool_handler]`
    /// generated ones, which only saw the local router — the reason upstream
    /// tools were previously neither listed nor callable (issue #3).
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let upstream_tools = self.upstreams.all_tools().await;
        let merged = merge::merge_tools(self.tool_router.list_all(), upstream_tools);
        for conflict in &merged.conflicts {
            warn!(%conflict);
        }
        Ok(ListToolsResult::with_all_items(merged.items))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        // Local first: a local tool shadows any same-named upstream tool,
        // mirroring the merge conflict policy above.
        if self.tool_router.get(&name).is_some() {
            let tcc = ToolCallContext::new(self, request, context);
            return self.tool_router.call(tcc).await;
        }
        if self.upstreams.has_tool(&name).await {
            return self
                .upstreams
                .forward_tool_call(&name, request)
                .await
                .map(CallToolResponse::from)
                .map_err(|e| McpError::internal_error(e.to_string(), None));
        }
        Err(McpError::invalid_params(
            format!("tool not found: {name}"),
            None,
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let local_resources = vec![
            Resource::new("memory://user-profile", "User Profile & MUST Rules")
                .with_description("Core user preferences and mandatory rules.")
                .with_mime_type("text/plain"),
            Resource::new("memory://project-context", "Current Project Context")
                .with_description("Project-specific reference memories.")
                .with_mime_type("text/plain"),
            Resource::new("memory://session-inject", "Dynamic Session Injection")
                .with_description(
                    "Context-aware memory injection that updates based on conversation flow.",
                )
                .with_mime_type("text/plain"),
        ];

        let upstream_resources = self.upstreams.all_resources().await;
        let merged = merge::merge_resources(local_resources, upstream_resources);
        for conflict in &merged.conflicts {
            warn!(%conflict);
        }

        Ok(ListResourcesResult::with_all_items(merged.items))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.as_str();

        if uri.starts_with("memory://") {
            let content = match uri {
                "memory://session-inject" => {
                    // Two-phase (Feature C): MUST rules resolve deterministically
                    // and are served immediately; the semantic pipeline prefetches
                    // in the background. We wait at most PREFETCH_WAIT_WINDOW for
                    // the richer result, then proceed with whatever is ready —
                    // the request is never blocked on embedding latency.
                    self.injection.refresh_if_needed_two_phase().await;
                    self.injection
                        .wait_full(crate::injection::PREFETCH_WAIT_WINDOW)
                        .await;
                    self.injection.get_current_injection().await
                        .unwrap_or_else(|| "No injection context available yet. Use session_start or interact with tools to build context.".to_string())
                }
                _ => self
                    .router
                    .get_mcp_resource_content(uri)
                    .await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?,
            };
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(content, uri)]).into());
        }

        let uri_owned = uri.to_string();
        let result = self
            .upstreams
            .forward_read_resource(&uri_owned, request)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(result.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let local_prompts = vec![Prompt::new(
            "memvault-context",
            Some("Get memory-injected context for your current task"),
            Some(vec![PromptArgument::new("context_hint").with_description(
                "Describe your current task for better memory retrieval",
            )]),
        )];

        let upstream_prompts = self.upstreams.all_prompts().await;
        let merged = merge::merge_prompts(local_prompts, upstream_prompts);
        Ok(ListPromptsResult::with_all_items(merged.items))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        if request.name.as_ref() as &str == "memvault-context" {
            let context_hint = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("context_hint"))
                .and_then(|v| v.as_str());

            let injection = self
                .router
                .session_start("proxy-client", context_hint, None)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            let formatted = self.router.format_as_instructions(&injection.results);

            return Ok(
                GetPromptResult::new(vec![PromptMessage::new_text(Role::User, formatted)]).into(),
            );
        }

        let name = request.name.to_string();
        self.upstreams
            .forward_get_prompt(&name, request)
            .await
            .map(|r| r.into())
            .map_err(|e| McpError::internal_error(e.to_string(), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UpstreamDef;
    use crate::context::SessionContext;
    use crate::injection::InjectionEngine;
    use crate::upstream::UpstreamManager;
    use memvault_core::compliance::ComplianceStore;
    use memvault_core::router::MemoryRouter;
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;
    use uuid::Uuid;

    /// Serializes env-var mutation for handler tests that must be hermetic
    /// (mirrors memvault-core's `llm_extractor` ENV_LOCK pattern). The
    /// notify_response tests force LLM extraction off so they exercise the
    /// rule-based path deterministically, independent of whether a local
    /// Ollama daemon happens to be running.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    async fn build_server() -> (ProxyHandler, Arc<ComplianceStore>, Arc<SqliteStore>) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let upstreams = UpstreamManager::connect_all(&[]).await.unwrap();
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db =
            std::env::temp_dir().join(format!("memvault_proxy_srv_{}.db", Uuid::new_v4().simple()));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        let handler = ProxyHandler::new(
            store.clone(),
            router.clone(),
            memvault_core::embedding::LazyEmbedder::fixed(None),
            upstreams,
            context,
            injection,
            compliance.clone(),
        )
        .await;
        (handler, compliance, store)
    }

    /// Markdown-backed counterpart to `build_server` — proves
    /// `ProxyHandler.store`'s `Arc<dyn MemoryStore>` generalization actually
    /// works end to end with the markdown backend, not just that it compiles.
    async fn build_server_markdown(dir: &std::path::Path) -> ProxyHandler {
        let store: Arc<dyn MemoryStore> = Arc::new(
            memvault_core::storage::markdown::MarkdownStore::open(dir)
                .await
                .unwrap(),
        );
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let upstreams = UpstreamManager::connect_all(&[]).await.unwrap();
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db = std::env::temp_dir().join(format!(
            "memvault_proxy_srv_md_{}.db",
            Uuid::new_v4().simple()
        ));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        ProxyHandler::new(
            store,
            router,
            memvault_core::embedding::LazyEmbedder::fixed(None),
            upstreams,
            context,
            injection,
            compliance,
        )
        .await
    }

    fn save_params(content: &str) -> SaveMemoryParams {
        SaveMemoryParams {
            content: content.to_string(),
            priority: "REFERENCE".to_string(),
            memory_type: "fact".to_string(),
            instruction: None,
            tags: None,
            namespace: "global".to_string(),
            agent_id: "proxy-client".to_string(),
            agent_type: "proxy-agent".to_string(),
            confidence: 0.8,
            layer: None,
            skill_trigger: None,
            skill_steps: Vec::new(),
            skill_verification: None,
            api_key: None,
        }
    }

    fn text_of(result: Result<CallToolResult, McpError>) -> String {
        match result {
            Ok(r) => match r.content.first() {
                Some(ContentBlock::Text(t)) => t.text.clone(),
                _ => String::new(),
            },
            Err(e) => panic!("unexpected tool error: {}", e),
        }
    }

    fn saved_id(save_text: &str) -> String {
        // `save_memory` returns the MCP/REST JSON shape:
        // {"status": "saved", "id": "mem_xxx", ...}
        serde_json::from_str::<serde_json::Value>(save_text)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Build a handler whose save path uses `embedder` (default `build_server`
    /// disables embeddings with a fixed `None` holder).
    async fn build_server_with_embedder(
        embedder: memvault_core::embedding::LazyEmbedder,
    ) -> (ProxyHandler, Arc<SqliteStore>) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let upstreams = UpstreamManager::connect_all(&[]).await.unwrap();
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db =
            std::env::temp_dir().join(format!("memvault_proxy_emb_{}.db", Uuid::new_v4().simple()));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        let handler = ProxyHandler::new(
            store.clone(),
            router,
            embedder,
            upstreams,
            context,
            injection,
            compliance,
        )
        .await;
        (handler, store)
    }

    /// Minimal in-process provider for proxy save-path tests.
    struct FakeEmbedder;

    #[async_trait::async_trait]
    impl memvault_core::embedding::EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, texts: &[String]) -> memvault_core::error::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        }
        fn dimension(&self) -> usize {
            3
        }
    }

    fn session_id(text: &str) -> String {
        text.split("inj_")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .map(|s| format!("inj_{}", s))
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn test_tool_save_memory_defaults() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .save_memory(Parameters(save_params("prefers Go")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(parsed["status"], serde_json::json!("saved"));
        assert!(
            parsed["id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("mem_"),
            "{}",
            text
        );
        assert_eq!(parsed["embedded"], serde_json::json!(false));
        assert_eq!(parsed["vector_pending"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_tool_save_memory_must_skill_with_meta() {
        let (server, _comp, _store) = build_server().await;
        let mut params = save_params("deploy procedure");
        params.priority = "MUST".to_string();
        params.memory_type = "skill".to_string();
        params.tags = Some("devops,  deploy".to_string());
        params.layer = Some("L2".to_string());
        params.skill_trigger = Some("deploy".to_string());
        params.skill_steps = vec!["build".to_string()];
        params.skill_verification = Some("ok".to_string());
        let text = text_of(server.save_memory(Parameters(params)).await);
        assert!(text.contains("Must"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_save_memory_invalid_layer_falls_back() {
        let (server, _comp, _store) = build_server().await;
        let mut params = save_params("bad layer");
        params.layer = Some("L9".to_string());
        let _ = server.save_memory(Parameters(params)).await.unwrap();
    }

    #[tokio::test]
    async fn test_tool_save_memory_embeds_with_ready_provider() {
        let (server, _store) = build_server_with_embedder(
            memvault_core::embedding::LazyEmbedder::fixed(Some(Arc::new(FakeEmbedder))),
        )
        .await;
        let text = text_of(
            server
                .save_memory(Parameters(save_params("prefers tabs")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(parsed["status"], serde_json::json!("saved"));
        assert_eq!(parsed["embedded"], serde_json::json!(true), "{text}");
        assert_eq!(parsed["vector_pending"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_tool_save_memory_waits_for_warming_provider() {
        // Parity with MCP/REST: a save arriving before the lazy provider is
        // ready must wait for readiness (bounded) instead of persisting a
        // vectorless memory.
        let embedder = memvault_core::embedding::LazyEmbedder::delayed(
            Arc::new(FakeEmbedder),
            std::time::Duration::from_millis(60),
        );
        assert!(
            !embedder.ready().await,
            "precondition: provider not ready yet"
        );
        let (server, _store) = build_server_with_embedder(embedder).await;

        let text = text_of(
            server
                .save_memory(Parameters(save_params("saved during warm-up")))
                .await,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("save returns JSON");
        assert_eq!(parsed["embedded"], serde_json::json!(true), "{text}");
        assert_eq!(parsed["vector_pending"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_tool_save_memory_dedupes_repeat_content() {
        // Regression: this tool used to call `store.save` directly, bypassing
        // the delta-write dedup every other save path applies.
        let (server, store) =
            build_server_with_embedder(memvault_core::embedding::LazyEmbedder::fixed(None)).await;

        let first = text_of(
            server
                .save_memory(Parameters(save_params("prefers Go over Rust")))
                .await,
        );
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["status"], serde_json::json!("saved"));

        let second = text_of(
            server
                .save_memory(Parameters(save_params("prefers Go over Rust")))
                .await,
        );
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(
            second["status"],
            serde_json::json!("skipped"),
            "an exact repeat must be deduped: {second}"
        );
        assert_eq!(
            store.list(None, 100, 0).await.unwrap().len(),
            1,
            "delta write must not append a second row"
        );
    }

    #[tokio::test]
    async fn test_tool_search_memory_no_results() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "nothing".to_string(),
                    mode: "hybrid".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("No memories found."));
    }

    #[tokio::test]
    async fn test_tool_save_and_search_against_markdown_backend() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_proxy_markdown_{}",
            Uuid::new_v4().simple()
        ));
        let server = build_server_markdown(&dir).await;

        server
            .save_memory(Parameters(save_params("prefers Rust over Go")))
            .await
            .unwrap();

        let text = text_of(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "Rust".to_string(),
                    mode: "keyword".to_string(),
                    top_k: 5,
                    namespace: None,
                    type_filter: None,
                    priority_filter: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("prefers Rust over Go"));

        let md_files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        assert_eq!(md_files.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_tool_search_memory_finds_and_applies_filters() {
        let (server, _comp, _store) = build_server().await;
        let mut must = save_params("always lint first");
        must.priority = "MUST".to_string();
        server.save_memory(Parameters(must)).await.unwrap();

        let text = text_of(
            server
                .search_memory(Parameters(SearchMemoryParams {
                    query: "lint".to_string(),
                    mode: "hybrid".to_string(),
                    top_k: 10,
                    namespace: None,
                    type_filter: None,
                    priority_filter: Some("MUST".to_string()),
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("always lint first"), "{}", text);
        assert!(text.contains("Must"));
    }

    #[tokio::test]
    async fn test_tool_session_start_empty() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .session_start(Parameters(SessionStartParams {
                    agent_id: "proxy-client".to_string(),
                    context_hint: None,
                    project: None,
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("[inject_session_id:"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_session_start_records_compliance_and_report() {
        let (server, _comp, store) = build_server().await;
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("must rule")))
                .await,
        );
        let id = saved_id(&save_text);
        // Approve the saved memory: the default injection gate only lets
        // human-reviewed memories into a session.
        let mut memory = store.get(&id).await.unwrap();
        memory.human_reviewed = true;
        store.update(memory).await.unwrap();
        let sid_text = text_of(
            server
                .session_start(Parameters(SessionStartParams {
                    agent_id: "proxy-client".to_string(),
                    context_hint: Some("begin".to_string()),
                    project: None,
                    api_key: None,
                }))
                .await,
        );
        let sid = session_id(&sid_text);

        assert!(!sid.is_empty(), "session id parsed from: {}", sid_text);

        let text = text_of(
            server
                .report_compliance(Parameters(ReportComplianceParams {
                    inject_session_id: sid,
                    reports: vec![ComplianceReportItem {
                        memory_id: id,
                        status: "followed".to_string(),
                        evidence: Some("done".to_string()),
                    }],
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("1/1 items updated"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_report_compliance_unknown_item_counts_zero() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .report_compliance(Parameters(ReportComplianceParams {
                    inject_session_id: "inj_unknown".to_string(),
                    reports: vec![ComplianceReportItem {
                        memory_id: "mem_x".to_string(),
                        status: "violated".to_string(),
                        evidence: None,
                    }],
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("0/1 items updated"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_get_compliance_report_session_and_summary() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .get_compliance_report(Parameters(GetComplianceReportParams {
                    inject_session_id: Some("inj_ghost".to_string()),
                    agent_id: None,
                    limit: 10,
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("inj_ghost"), "{}", text);

        let text = text_of(
            server
                .get_compliance_report(Parameters(GetComplianceReportParams {
                    inject_session_id: None,
                    agent_id: Some("proxy-client".to_string()),
                    limit: 10,
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("total_sessions"), "{}", text);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_tool_notify_response_extracts_and_saves() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("MEMVAULT_LLM_EXTRACTION_PROVIDER", "off");
        }
        let (server, _comp, store) = build_server().await;
        let text = text_of(
            server
                .notify_response(Parameters(NotifyResponseParams {
                    response_text: "I always prefer dark mode".to_string(),
                    user_text: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Extraction complete"), "{}", text);
        let all = store.list(None, 100, 0).await.unwrap();
        assert!(!all.is_empty());

        let text = text_of(
            server
                .notify_response(Parameters(NotifyResponseParams {
                    response_text: "nothing useful here".to_string(),
                    user_text: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("No extractable memories"), "{}", text);
        unsafe {
            std::env::remove_var("MEMVAULT_LLM_EXTRACTION_PROVIDER");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_tool_notify_response_extracts_from_user_text_too() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("MEMVAULT_LLM_EXTRACTION_PROVIDER", "off");
        }
        let (server, _comp, store) = build_server().await;

        // response_text alone has nothing extractable; user_text does.
        let text = text_of(
            server
                .notify_response(Parameters(NotifyResponseParams {
                    response_text: "好的".to_string(),
                    user_text: Some("我偏好用 tabs 缩进".to_string()),
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Extraction complete"), "{}", text);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter()
                .any(|m| m.tags.contains(&"source:user".to_string())),
            "expected a memory tagged source:user, got: {:?}",
            all.iter().map(|m| &m.tags).collect::<Vec<_>>()
        );

        // A blank user_text must not error or double-count.
        let text = text_of(
            server
                .notify_response(Parameters(NotifyResponseParams {
                    response_text: "好的".to_string(),
                    user_text: Some("   ".to_string()),
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("No extractable memories"), "{}", text);
        unsafe {
            std::env::remove_var("MEMVAULT_LLM_EXTRACTION_PROVIDER");
        }
    }

    #[tokio::test]
    async fn test_tool_run_promote_dedup_decay() {
        let (server, _comp, _store) = build_server().await;
        server
            .save_memory(Parameters(save_params("dup a")))
            .await
            .unwrap();

        let text = text_of(
            server
                .run_dedup(Parameters(RunDedupParams {
                    namespace: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Dedup:"), "{}", text);

        let text = text_of(
            server
                .run_decay(Parameters(RunDecayParams {
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Decay:"), "{}", text);

        let text = text_of(
            server
                .run_promote(Parameters(RunPromoteParams {
                    namespace: None,
                    min_l1: Some(1),
                    min_l2: Some(1),
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Promote:"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_confirm_read_and_list_inbox() {
        let (server, _comp, _store) = build_server().await;
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("inbox item")))
                .await,
        );
        let id = saved_id(&save_text);

        let err = server
            .confirm_read(Parameters(ConfirmReadParams {
                memory_ids: Vec::new(),
                agent_id: "proxy-client".to_string(),
                api_key: None,
            }))
            .await
            .expect_err("empty memory_ids must be rejected");
        assert!(!err.to_string().is_empty());

        let text = text_of(
            server
                .confirm_read(Parameters(ConfirmReadParams {
                    memory_ids: vec![id.clone()],
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("Confirmed 1 memories"), "{}", text);

        let text = text_of(
            server
                .list_inbox(Parameters(ListInboxParams {
                    namespace: None,
                    limit: 10,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("inbox item"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_review_memory_actions() {
        let (server, _comp, _store) = build_server().await;
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("review me")))
                .await,
        );
        let id = saved_id(&save_text);

        let text = text_of(
            server
                .review_memory(Parameters(ReviewMemoryParams {
                    memory_id: id.clone(),
                    action: "approve".to_string(),
                    edited_content: None,
                    edited_instruction: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("approved"), "{}", text);

        let text = text_of(
            server
                .review_memory(Parameters(ReviewMemoryParams {
                    memory_id: id.clone(),
                    action: "edit".to_string(),
                    edited_content: Some("edited".to_string()),
                    edited_instruction: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("edited"), "{}", text);

        let err = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id.clone(),
                action: "bogus".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "proxy-client".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid action"), "{}", err);

        // `id` is already approved (and edited, which also marks it
        // reviewed) — rejecting it now must be refused, not silently delete
        // an already-trusted memory.
        let err = server
            .review_memory(Parameters(ReviewMemoryParams {
                memory_id: id,
                action: "reject".to_string(),
                edited_content: None,
                edited_instruction: None,
                agent_id: "proxy-client".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already approved"), "{}", err);

        // Reject still works on a genuinely pending memory.
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("pending, never approved")))
                .await,
        );
        let pending_id = saved_id(&save_text);
        let text = text_of(
            server
                .review_memory(Parameters(ReviewMemoryParams {
                    memory_id: pending_id,
                    action: "reject".to_string(),
                    edited_content: None,
                    edited_instruction: None,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("rejected"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_delete_memory() {
        let (server, _comp, _store) = build_server().await;
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("remove me")))
                .await,
        );
        let id = saved_id(&save_text);
        let text = text_of(
            server
                .delete_memory(Parameters(DeleteMemoryParams {
                    memory_id: id,
                    agent_id: "proxy-client".to_string(),
                    api_key: None,
                }))
                .await,
        );
        assert!(text.contains("deleted"), "{}", text);
    }

    /// Build a handler whose router has one registered, keyed agent — the
    /// same setup that must fail closed on the write/read paths, matching
    /// the MCP server's per-tool `authenticate_agent` posture.
    async fn build_server_keyed() -> (ProxyHandler, Arc<ComplianceStore>, Arc<SqliteStore>) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![AgentProfile {
            id: "keyed-agent".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules::default(),
            api_key: Some("s3cret".to_string()),
            inject_channel: None,
        }];
        let router = Arc::new(MemoryRouter::with_registry(store.clone(), registry));
        let upstreams = UpstreamManager::connect_all(&[]).await.unwrap();
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db = std::env::temp_dir().join(format!(
            "memvault_proxy_keyed_{}.db",
            Uuid::new_v4().simple()
        ));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        let handler = ProxyHandler::new(
            store.clone(),
            router,
            memvault_core::embedding::LazyEmbedder::fixed(None),
            upstreams,
            context,
            injection,
            compliance.clone(),
        )
        .await;
        (handler, compliance, store)
    }

    #[tokio::test]
    async fn test_tool_write_and_read_paths_require_auth_once_any_key_registered() {
        let (server, _comp, _store) = build_server_keyed().await;

        // Anonymous callers must no longer be able to tamper with compliance
        // records (H4/P1a), read compliance summaries (H4/P1b), or trigger
        // extractions under an arbitrary agent id (H4/P1c).
        let err = server
            .report_compliance(Parameters(ReportComplianceParams {
                inject_session_id: "inj_ghost".to_string(),
                reports: vec![ComplianceReportItem {
                    memory_id: "mem_x".to_string(),
                    status: "followed".to_string(),
                    evidence: None,
                }],
                agent_id: "keyed-agent".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "anonymous compliance write must fail"
        );

        let err = server
            .get_compliance_report(Parameters(GetComplianceReportParams {
                inject_session_id: None,
                agent_id: Some("keyed-agent".to_string()),
                limit: 10,
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "anonymous compliance read must fail"
        );

        let err = server
            .notify_response(Parameters(NotifyResponseParams {
                response_text: "I always prefer dark mode".to_string(),
                user_text: None,
                agent_id: "keyed-agent".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "anonymous notify_response must fail"
        );

        // The same-class tools that previously skipped auth must too.
        let err = server
            .delete_memory(Parameters(DeleteMemoryParams {
                memory_id: "mem_whatever".to_string(),
                agent_id: "keyed-agent".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "anonymous delete_memory must fail"
        );

        let err = server
            .list_inbox(Parameters(ListInboxParams {
                namespace: None,
                limit: 10,
                agent_id: "keyed-agent".to_string(),
                api_key: None,
            }))
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "anonymous list_inbox must fail"
        );
    }

    #[tokio::test]
    async fn test_tool_save_memory_stamps_identity_verified_with_key() {
        let (server, _comp, store) = build_server_keyed().await;
        let text = text_of(
            server
                .save_memory(Parameters(SaveMemoryParams {
                    content: "keyed save stamps verification".to_string(),
                    priority: "MUST".to_string(),
                    memory_type: "fact".to_string(),
                    instruction: None,
                    tags: None,
                    namespace: "global".to_string(),
                    agent_id: "keyed-agent".to_string(),
                    agent_type: "general".to_string(),
                    confidence: 0.8,
                    layer: None,
                    skill_trigger: None,
                    skill_steps: Vec::new(),
                    skill_verification: None,
                    api_key: Some("s3cret".to_string()),
                }))
                .await,
        );
        assert!(text.contains("\"status\": \"saved\""), "{}", text);

        let id = saved_id(&text);
        let saved = store.get(&id).await.unwrap();
        assert!(
            saved.identity_verified,
            "a key-authenticated save must stamp identity_verified (M2)"
        );

        // An unregistered agent id in keyed-deployment mode still falls back
        // to the documented unauthenticated path (same as MCP/REST), but its
        // save must NOT be stamped verified.
        let text = text_of(
            server
                .save_memory(Parameters(save_params("no key here")))
                .await,
        );
        let keyless_id = saved_id(&text);
        let keyless = store.get(&keyless_id).await.unwrap();
        assert!(
            !keyless.identity_verified,
            "a keyless save must not stamp identity_verified"
        );
    }

    #[tokio::test]
    async fn test_server_info() {
        let (server, _comp, _store) = build_server().await;
        let info = server.get_info();
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.prompts.is_some());
        let instructions = info.instructions.as_deref().unwrap_or("");
        assert!(instructions.contains("MemVault"));
    }
    // ---- HTTP round-trip: ServerHandler base surface + upstream forwarding ----
    //
    // `list_resources`/`read_resource`/`list_prompts`/`get_prompt` take a
    // `RequestContext<RoleServer>` that cannot be constructed from outside rmcp,
    // so they are exercised through a real client connection: the ProxyHandler
    // is served over HTTP and driven by `UpstreamManager` (the production client).

    use axum::Router;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    async fn spawn_http_server<H>(server: H) -> String
    where
        H: ServerHandler + Clone + Send + Sync + 'static,
    {
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

    async fn spawn_proxy_http_server(server: ProxyHandler) -> String {
        spawn_http_server(server).await
    }

    /// A fake upstream exposing exactly one unique tool (`fixture_echo`) —
    /// drives the issue #3 regression: the proxy must aggregate upstream
    /// tools into its own `tools/list` and forward `tools/call` to the
    /// upstream that provides them.
    #[derive(Clone)]
    struct EchoUpstreamServer;

    #[tool_router(allow_empty)]
    impl EchoUpstreamServer {}

    #[tool_handler]
    impl ServerHandler for EchoUpstreamServer {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, McpError> {
            let mut schema = serde_json::Map::new();
            schema.insert(
                "type".to_string(),
                serde_json::Value::String("object".to_string()),
            );
            schema.insert(
                "properties".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
            Ok(ListToolsResult::with_all_items(vec![Tool::new(
                "fixture_echo",
                "echo test tool",
                std::sync::Arc::new(schema),
            )]))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, McpError> {
            if request.name.as_ref() == "fixture_echo" {
                Ok(CallToolResult::success(vec![ContentBlock::text("ECHO_FROM_UPSTREAM")]).into())
            } else {
                Err(McpError::invalid_params(
                    format!("tool not found: {}", request.name),
                    None,
                ))
            }
        }
    }

    fn http_upstream_def(name: &str, url: String) -> UpstreamDef {
        UpstreamDef {
            name: name.to_string(),
            command: None,
            args: None,
            env: Default::default(),
            url: Some(url),
        }
    }

    async fn build_server_with_upstreams(upstreams: Arc<UpstreamManager>) -> ProxyHandler {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db =
            std::env::temp_dir().join(format!("memvault_proxy_up_{}.db", Uuid::new_v4().simple()));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        ProxyHandler::new(
            store,
            router,
            memvault_core::embedding::LazyEmbedder::fixed(None),
            upstreams,
            context,
            injection,
            compliance,
        )
        .await
    }

    #[tokio::test]
    async fn test_proxy_lists_and_forwards_upstream_tools() {
        // Full chain over the wire: production client → proxy → fake upstream.
        let upstream_url = spawn_http_server(EchoUpstreamServer).await;
        let upstreams =
            UpstreamManager::connect_all(&[http_upstream_def("echo-upstream", upstream_url)])
                .await
                .expect("connect fake upstream");

        let proxy = build_server_with_upstreams(upstreams).await;
        let proxy_url = spawn_http_server(proxy).await;
        let client = UpstreamManager::connect_all(&[http_upstream_def("proxy", proxy_url)])
            .await
            .expect("connect to proxy");

        // tools/list must aggregate: upstream's fixture_echo alongside the
        // proxy's own local tools.
        let tools = client.all_tools().await;
        assert!(
            tools.iter().any(|t| t.name == "fixture_echo"),
            "upstream tool must be exposed through the proxy: {:?}",
            tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>()
        );
        assert!(
            tools.iter().any(|t| t.name == "save_memory"),
            "local tools must still be listed"
        );

        // tools/call on the upstream tool forwards and returns its result.
        let result = client
            .forward_tool_call("fixture_echo", CallToolRequestParams::new("fixture_echo"))
            .await
            .expect("fixture_echo must be forwarded to the upstream");
        assert!(
            result.content.iter().any(
                |c| matches!(c, ContentBlock::Text(t) if t.text.contains("ECHO_FROM_UPSTREAM"))
            ),
            "forwarded result: {result:?}"
        );

        // Local tools keep working through the same connection.
        let mut args = serde_json::Map::new();
        args.insert(
            "content".to_string(),
            serde_json::Value::String("local still works".to_string()),
        );
        args.insert(
            "priority".to_string(),
            serde_json::Value::String("MUST".to_string()),
        );
        let result = client
            .forward_tool_call(
                "save_memory",
                CallToolRequestParams::new("save_memory").with_arguments(args),
            )
            .await
            .expect("local save_memory must still work");
        assert!(
            result.content.iter().any(
                |c| matches!(c, ContentBlock::Text(t) if t.text.contains("\"status\": \"saved\""))
            ),
            "save_memory output: {result:?}"
        );

        // A tool provided by nobody still errors with "not found".
        let err = client
            .forward_tool_call(
                "no_such_tool_anywhere",
                CallToolRequestParams::new("no_such_tool_anywhere"),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn test_http_roundtrip_resources_prompts_and_tools() {
        let (server, _comp, _store) = build_server().await;
        let url = spawn_proxy_http_server(server).await;

        let upstream = UpstreamManager::connect_all(&[UpstreamDef {
            name: "self".to_string(),
            command: None,
            args: None,
            env: Default::default(),
            url: Some(url),
        }])
        .await
        .expect("connect to the in-process proxy server");

        // get_info + list_resources are exercised during the client handshake.
        let resources = upstream.all_resources().await;
        assert!(
            resources.iter().any(|r| r.uri == "memory://user-profile"),
            "local resources must be listed: {:?}",
            resources.iter().map(|r| r.uri.clone()).collect::<Vec<_>>()
        );
        assert!(resources.iter().any(|r| r.uri == "memory://session-inject"));

        let prompts = upstream.all_prompts().await;
        assert!(
            prompts.iter().any(|p| p.name == "memvault-context"),
            "local prompt must be listed"
        );

        // forward_tool_call success path: save_memory through the real tool router.
        let mut args = serde_json::Map::new();
        args.insert(
            "content".to_string(),
            serde_json::Value::String("roundtrip memory".to_string()),
        );
        args.insert(
            "priority".to_string(),
            serde_json::Value::String("MUST".to_string()),
        );
        let result = upstream
            .forward_tool_call(
                "save_memory",
                CallToolRequestParams::new("save_memory").with_arguments(args),
            )
            .await
            .expect("tool call over HTTP must succeed");
        assert!(
            result.content.iter().any(
                |c| matches!(c, ContentBlock::Text(t) if t.text.contains("\"status\": \"saved\""))
            ),
            "save_memory output: {result:?}"
        );

        // read_resource (memory:// branch) over the wire now reflects the save.
        let resource = upstream
            .forward_read_resource(
                "memory://user-profile",
                ReadResourceRequestParams::new("memory://user-profile"),
            )
            .await
            .expect("read_resource over HTTP must succeed");
        assert!(
            matches!(
                resource.contents.first(),
                Some(ResourceContents::TextResourceContents { text, .. }) if text.contains("roundtrip memory")
            ),
            "user-profile resource must reflect the saved memory: {resource:?}"
        );

        // get_prompt (local "memvault-context" branch) over the wire.
        let prompt = upstream
            .forward_get_prompt(
                "memvault-context",
                GetPromptRequestParams::new("memvault-context"),
            )
            .await
            .expect("get_prompt over HTTP must succeed");
        assert!(
            !prompt.messages.is_empty(),
            "memvault-context prompt must return at least one message"
        );
    }

    fn infer_outcome_params(evidence: OutcomeEvidenceParams) -> InferOutcomeParams {
        InferOutcomeParams {
            evidence,
            judge_context: String::new(),
            namespace: "global".to_string(),
            agent_id: "dsh".to_string(),
            api_key: None,
        }
    }

    #[tokio::test]
    async fn test_tool_infer_outcome_skips_clean_evidence() {
        let (server, _comp, _store) = build_server().await;
        let text = text_of(
            server
                .infer_outcome(Parameters(infer_outcome_params(OutcomeEvidenceParams {
                    task: Some("deploy the checkout service".to_string()),
                    retry_hits: 0,
                    rejection_hits: 0,
                    correction_hits: 0,
                    terminal: "resolved".to_string(),
                    terminal_detail: None,
                })))
                .await,
        );
        assert!(text.contains("No outcome inferred"), "{}", text);
    }

    #[tokio::test]
    async fn test_tool_infer_outcome_records_failure_from_evidence() {
        let (server, _comp, store) = build_server().await;
        let text = text_of(
            server
                .infer_outcome(Parameters(infer_outcome_params(OutcomeEvidenceParams {
                    task: Some("deploy the checkout service to staging".to_string()),
                    retry_hits: 1,
                    rejection_hits: 0,
                    correction_hits: 0,
                    terminal: "unresolved_tool_error".to_string(),
                    terminal_detail: Some("Bash: permission denied".to_string()),
                })))
                .await,
        );
        assert!(text.contains("Outcome recorded"), "{}", text);
        assert!(text.contains("source=Rule"), "{}", text);

        let episodes = store.list_episodes(EpisodeFilter::default()).await.unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].status, OutcomeStatus::Failure);
        assert!(episodes[0].lesson.is_some());

        let episode_mem = store.get(&episodes[0].memory_id).await.unwrap();
        assert!(episode_mem.tags.contains(&"auto-inferred".to_string()));
        assert!(episode_mem.tags.contains(&"inferred:rule".to_string()));
        assert!(!episode_mem.human_reviewed);
    }
}
