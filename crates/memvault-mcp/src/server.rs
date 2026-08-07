use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use memvault_core::models::*;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::storage::MemoryStore;

#[derive(Clone)]
pub struct MemVaultMcp {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
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
}

fn default_priority_str() -> String { "REFERENCE".to_string() }
fn default_type_str() -> String { "fact".to_string() }
fn default_namespace() -> String { "global".to_string() }
fn default_agent_id() -> String { "unknown".to_string() }
fn default_agent_type() -> String { "general-assistant".to_string() }
fn default_confidence() -> f64 { 0.8 }

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchMemoryParams {
    /// Search query string
    pub query: String,
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
}

fn default_top_k() -> usize { 10 }

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionStartParams {
    /// ID of the agent starting the session
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Type of the agent
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Hint about the current conversation context
    pub context_hint: Option<String>,
    /// Current project name
    pub project: Option<String>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMemoryParams {
    /// ID of the memory to delete
    pub memory_id: String,
}

// --- MCP Server implementation ---

#[tool_router]
impl MemVaultMcp {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        let router = Arc::new(MemoryRouter::new(store.clone()));
        Self {
            store,
            router,
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_router(store: Arc<SqliteStore>, router: Arc<MemoryRouter>) -> Self {
        Self {
            store,
            router,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Save a new memory. Memories are persistent user preferences, facts, episodes, or skills that should be recalled in future conversations.")]
    async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
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
        mem.namespace = params.namespace;
        mem.instruction = params.instruction;
        mem.tags = params.tags;
        mem.confidence = params.confidence;

        let saved = self.store.save(mem).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let result = serde_json::json!({
            "status": "saved",
            "id": saved.id,
            "priority": format!("{:?}", saved.priority),
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Search user memories by keyword, with optional filters by namespace, type, and priority. Results are ordered by priority (MUST first) and relevance.")]
    async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let type_filter = params.type_filter.and_then(|t| {
            match t.to_lowercase().as_str() {
                "preference" => Some(MemoryType::Preference),
                "fact" => Some(MemoryType::Fact),
                "episode" => Some(MemoryType::Episode),
                "entity" => Some(MemoryType::Entity),
                "skill" => Some(MemoryType::Skill),
                _ => None,
            }
        });

        let priority_filter = params.priority_filter.and_then(|p| {
            match p.to_uppercase().as_str() {
                "MUST" => Some(Priority::Must),
                "REFERENCE" => Some(Priority::Reference),
                "BACKGROUND" => Some(Priority::Background),
                _ => None,
            }
        });

        let query = SearchQuery {
            query: params.query,
            agent_id: params.agent_id,
            type_filter,
            priority_filter,
            namespace: params.namespace,
            top_k: params.top_k,
            token_budget: None,
        };

        let results = self.store.search(query).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output: Vec<serde_json::Value> = results.iter().map(|r| {
            serde_json::json!({
                "id": r.memory.id,
                "content": r.memory.content,
                "instruction": r.memory.instruction,
                "priority": format!("{:?}", r.memory.priority),
                "type": format!("{:?}", r.memory.memory_type),
                "namespace": r.memory.namespace,
                "tags": r.memory.tags,
                "score": r.score,
                "human_reviewed": r.memory.human_reviewed,
            })
        }).collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Start a new session. Returns relevant memories filtered by agent identity, formatted as MUST/REF instructions ready for injection into the conversation context.")]
    async fn session_start(
        &self,
        Parameters(params): Parameters<SessionStartParams>,
    ) -> Result<CallToolResult, McpError> {
        let results = self.router
            .session_start(&params.agent_id, params.context_hint.as_deref(), params.project.as_deref())
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let formatted = self.router.format_as_instructions(&results);

        if formatted.is_empty() {
            Ok(CallToolResult::success(vec![ContentBlock::text(
                "No memories to inject for this session.",
            )]))
        } else {
            Ok(CallToolResult::success(vec![ContentBlock::text(formatted)]))
        }
    }

    #[tool(description = "Review a pending memory: approve it, reject it, or edit its content. Approved memories get higher priority in future injections.")]
    async fn review_memory(
        &self,
        Parameters(params): Parameters<ReviewMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        match params.action.to_lowercase().as_str() {
            "approve" => {
                let mut mem = self.store.get(&params.memory_id).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                mem.human_reviewed = true;
                mem.updated_at = chrono::Utc::now();
                self.store.update(mem).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    format!("Memory {} approved.", params.memory_id),
                )]))
            }
            "reject" => {
                self.store.delete(&params.memory_id).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    format!("Memory {} rejected and deleted.", params.memory_id),
                )]))
            }
            "edit" => {
                let mut mem = self.store.get(&params.memory_id).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                if let Some(content) = params.edited_content {
                    mem.content = content;
                }
                if let Some(instruction) = params.edited_instruction {
                    mem.instruction = Some(instruction);
                }
                mem.human_reviewed = true;
                mem.updated_at = chrono::Utc::now();
                self.store.update(mem).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    format!("Memory {} edited and approved.", params.memory_id),
                )]))
            }
            _ => Err(McpError::invalid_params(
                format!("Invalid action: {}. Use approve, reject, or edit.", params.action),
                None,
            )),
        }
    }

    #[tool(description = "Delete a memory by its ID.")]
    async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.store.delete(&params.memory_id).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            format!("Memory {} deleted.", params.memory_id),
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
             Use save_memory to store, search_memory to find, \
             session_start to get formatted injection context.",
        )
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
        let content = self.router.get_mcp_resource_content(uri).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let text = if content.is_empty() {
            "No memories stored yet.".to_string()
        } else {
            content
        };

        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri),
        ]).into())
    }
}

pub async fn run_stdio_server(db_path: PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path.parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let router = if registry_path.exists() {
        tracing::info!("Loading agent registry from {}", registry_path.display());
        Arc::new(MemoryRouter::load_registry_from_yaml(store.clone(), &registry_path)?)
    } else {
        Arc::new(MemoryRouter::new(store.clone()))
    };

    let server = MemVaultMcp::with_router(store, router);

    tracing::info!("MemVault MCP Server starting on stdio...");

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("MCP Server error: {}", e))?;

    service.waiting().await?;
    Ok(())
}
