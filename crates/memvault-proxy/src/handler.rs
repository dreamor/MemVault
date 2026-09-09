use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use memvault_core::compliance::{ComplianceStatus, ComplianceStore};
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;

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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunPromoteParams {
    pub namespace: Option<String>,
    pub min_l1: Option<usize>,
    pub min_l2: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDedupParams {
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfirmReadParams {
    pub memory_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListInboxParams {
    pub namespace: Option<String>,
    #[serde(default = "default_inbox_limit")]
    pub limit: usize,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMemoryParams {
    pub memory_id: String,
}

// --- Handler ---

#[derive(Clone)]
pub struct ProxyHandler {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
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
        store: Arc<SqliteStore>,
        router: Arc<MemoryRouter>,
        upstreams: Arc<UpstreamManager>,
        context: Arc<SessionContext>,
        injection: Arc<InjectionEngine>,
        compliance: Arc<ComplianceStore>,
    ) -> Self {
        let extraction_builder = ResponseExtractor::new(
            store.clone() as Arc<dyn MemoryStore>,
            ExtractionConfig::from_env(),
        );
        // Lazy env probe: resolved on first extraction and re-probed after
        // failures — a boot-time Ollama hiccup no longer disables contextual
        // extraction for the lifetime of the proxy.
        let extraction_builder = extraction_builder
            .with_lazy_llm_extractor(memvault_core::llm_extractor::LazyLlmExtractor::from_env());
        let extractor = Arc::new(extraction_builder);
        Self {
            store,
            router,
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
        self.router
            .authenticate_agent(&params.agent_id, params.api_key.as_deref())
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

        let saved = self
            .store
            .save(memory)
            .await
            .map_err(|e| McpError::internal_error(format!("save failed: {}", e), None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Memory saved: id={}, priority={:?}",
            saved.id, saved.priority
        ))]))
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
        description = "Get compliance report for injection sessions. Shows follow/violation rates for MUST memories."
    )]
    async fn get_compliance_report(
        &self,
        Parameters(params): Parameters<GetComplianceReportParams>,
    ) -> Result<CallToolResult, McpError> {
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
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Extraction complete: {} found, {} saved to Inbox, {} skipped (below threshold).",
                result.extracted, result.saved, result.skipped
            ))]))
        }
    }

    #[tool(
        description = "Run the promote pipeline: consolidates atomic L1 memories into L2 scenario summaries, and promotes stable L2 memories into L3 core persona rules."
    )]
    async fn run_promote(
        &self,
        Parameters(params): Parameters<RunPromoteParams>,
    ) -> Result<CallToolResult, McpError> {
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
    async fn run_decay(&self) -> Result<CallToolResult, McpError> {
        let dm = memvault_core::decay::DecayManager::new(
            self.store.clone(),
            memvault_core::decay::DecayConfig::default(),
        );
        let report = dm
            .run_decay()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Decay: {} updated, {} archived",
            report.updated, report.archived
        ))]))
    }

    #[tool(
        description = "Confirm that one or more memories have been read by the agent. Updates access_count and last_read_at."
    )]
    async fn confirm_read(
        &self,
        Parameters(params): Parameters<ConfirmReadParams>,
    ) -> Result<CallToolResult, McpError> {
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

    #[tool(description = "Delete a memory by its ID.")]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
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
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
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
            upstreams,
            context,
            injection,
            compliance.clone(),
        )
        .await;
        (handler, compliance, store)
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
        // "Memory saved: id=mem_xxx, priority=..."
        save_text
            .split("id=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
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
        assert!(text.contains("Memory saved: id=mem_"), "{}", text);
        assert!(text.contains("priority"));
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
        let (server, _comp, _store) = build_server().await;
        let save_text = text_of(
            server
                .save_memory(Parameters(save_params("must rule")))
                .await,
        );
        let id = saved_id(&save_text);
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
                .run_dedup(Parameters(RunDedupParams { namespace: None }))
                .await,
        );
        assert!(text.contains("Dedup:"), "{}", text);

        let text = text_of(server.run_decay().await);
        assert!(text.contains("Decay:"), "{}", text);

        let text = text_of(
            server
                .run_promote(Parameters(RunPromoteParams {
                    namespace: None,
                    min_l1: Some(1),
                    min_l2: Some(1),
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
            }))
            .await
            .expect_err("empty memory_ids must be rejected");
        assert!(!err.to_string().is_empty());

        let text = text_of(
            server
                .confirm_read(Parameters(ConfirmReadParams {
                    memory_ids: vec![id.clone()],
                }))
                .await,
        );
        assert!(text.contains("Confirmed 1 memories"), "{}", text);

        let text = text_of(
            server
                .list_inbox(Parameters(ListInboxParams {
                    namespace: None,
                    limit: 10,
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
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid action"), "{}", err);

        let text = text_of(
            server
                .review_memory(Parameters(ReviewMemoryParams {
                    memory_id: id,
                    action: "reject".to_string(),
                    edited_content: None,
                    edited_instruction: None,
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
                .delete_memory(Parameters(DeleteMemoryParams { memory_id: id }))
                .await,
        );
        assert!(text.contains("deleted"), "{}", text);
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

    async fn spawn_proxy_http_server(server: ProxyHandler) -> String {
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
            result
                .content
                .iter()
                .any(|c| matches!(c, ContentBlock::Text(t) if t.text.contains("Memory saved"))),
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
}
