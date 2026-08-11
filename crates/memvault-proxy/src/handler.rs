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
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;

use crate::context::SessionContext;
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
}

fn default_priority() -> String {
    "REFERENCE".to_string()
}
fn default_type() -> String {
    "fact".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    pub query: String,
    #[allow(dead_code)]
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
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
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ProxyHandler {
    pub fn new(
        store: Arc<SqliteStore>,
        router: Arc<MemoryRouter>,
        upstreams: Arc<UpstreamManager>,
        context: Arc<SessionContext>,
        injection: Arc<InjectionEngine>,
        compliance: Arc<ComplianceStore>,
    ) -> Self {
        Self {
            store,
            router,
            upstreams,
            context,
            injection,
            compliance,
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
        let priority = match params.priority.to_uppercase().as_str() {
            "MUST" => Priority::Must,
            "BACKGROUND" => Priority::Background,
            _ => Priority::Reference,
        };
        let mem_type = match params.memory_type.as_str() {
            "preference" => MemoryType::Preference,
            "skill" => MemoryType::Skill,
            "episode" => MemoryType::Episode,
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
            id: "proxy-client".to_string(),
            agent_type: "proxy".to_string(),
            session_id: None,
        };

        let mut memory = Memory::new(mem_type, params.content, priority, agent);
        memory.instruction = params.instruction;
        memory.tags = tags_vec;

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
        let search_query = SearchQuery {
            query: params.query,
            top_k: params.top_k,
            ..SearchQuery::new(String::new())
        };

        let results = self
            .store
            .search(search_query)
            .await
            .map_err(|e| McpError::internal_error(format!("search failed: {}", e), None))?;

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
        let results = self
            .router
            .session_start(
                &params.agent_id,
                params.context_hint.as_deref(),
                params.project.as_deref(),
            )
            .await
            .map_err(|e| McpError::internal_error(format!("session_start failed: {}", e), None))?;

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
                    self.injection.refresh_if_needed().await;
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

            let results = self
                .router
                .session_start("proxy-client", context_hint, None)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            let formatted = self.router.format_as_instructions(&results);

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
