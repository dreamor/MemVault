use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{debug, info, warn};

use memvault_core::embedding::{EmbeddingProvider, OpenAIEmbedding};
use memvault_core::hybrid::HybridMerger;
use memvault_core::models::*;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::storage::MemoryStore;

#[derive(Clone)]
pub struct MemVaultMcp {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
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
}

fn default_search_mode() -> String { "hybrid".to_string() }
fn default_top_k() -> usize { 10 }

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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExtractMemoriesParams {
    /// Text to extract memories from (conversation content)
    pub text: String,
    /// Whether to auto-save extracted memories
    #[serde(default)]
    pub auto_save: bool,
    /// Agent ID to attribute saved memories to
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDedupParams {
    /// Namespace to scan (null for all)
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfirmReadParams {
    /// List of memory IDs to confirm as read
    pub memory_ids: Vec<String>,
}

// --- MCP Server implementation ---

#[tool_router]
impl MemVaultMcp {
    pub fn new(store: Arc<SqliteStore>, router: Arc<MemoryRouter>, embedder: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        Self {
            store,
            router,
            embedder,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Save a new memory. Memories are persistent user preferences, facts, episodes, or skills that should be recalled in future conversations. Embeddings are generated automatically for semantic search.")]
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

        let embed_text = mem.instruction.as_deref().unwrap_or(&mem.content).to_string();
        let mut embedded = false;

        let saved = if let Some(ref embedder) = self.embedder {
            match embedder.embed(&[embed_text]).await {
                Ok(embeddings) if !embeddings.is_empty() => {
                    debug!(id = %mem.id, dim = embeddings[0].len(), "auto-embedded memory");
                    embedded = true;
                    self.store.save_with_embedding(mem, embeddings.into_iter().next().unwrap()).await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?
                }
                Err(e) => {
                    warn!("Auto-embedding failed, saving without: {}", e);
                    self.store.save(mem).await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?
                }
                _ => {
                    self.store.save(mem).await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?
                }
            }
        } else {
            self.store.save(mem).await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        };

        let result = serde_json::json!({
            "status": "saved",
            "id": saved.id,
            "priority": format!("{:?}", saved.priority),
            "embedded": embedded,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Search user memories. Supports three modes: 'keyword' (text match), 'semantic' (vector similarity), or 'hybrid' (both combined with RRF). Default is 'hybrid' when embeddings are available.")]
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
            };
            self.store.search(query).await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        } else {
            Vec::new()
        };

        // vector search
        let vector_results = if actual_mode != "keyword" {
            if let Some(ref embedder) = self.embedder {
                match embedder.embed(&[params.query.clone()]).await {
                    Ok(embeddings) if !embeddings.is_empty() => {
                        self.store.vector_search(&embeddings[0], params.top_k, params.namespace.as_deref()).await
                            .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    }
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
                "search_mode": actual_mode,
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

    #[tool(description = "Extract structured memories from conversation text. Detects preferences, facts, and skills using pattern matching. Returns extracted items; optionally saves them.")]
    async fn extract_memories(
        &self,
        Parameters(params): Parameters<ExtractMemoriesParams>,
    ) -> Result<CallToolResult, McpError> {
        let extracted = memvault_core::extractor::Extractor::extract(&params.text);

        if extracted.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "No memories extracted from the provided text.",
            )]));
        }

        let mut saved_ids = Vec::new();

        if params.auto_save {
            for e in &extracted {
                let mut mem = Memory::new(
                    e.memory_type.clone(), e.content.clone(), e.priority.clone(),
                    SourceAgent { id: params.agent_id.clone(), agent_type: "extractor".to_string(), session_id: None },
                );
                mem.instruction = e.instruction.clone();
                mem.tags = e.tags.clone();
                mem.confidence = e.confidence;

                let saved = self.store.save(mem).await
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                saved_ids.push(saved.id);
            }
        }

        let output: Vec<serde_json::Value> = extracted.iter().enumerate().map(|(i, e)| {
            serde_json::json!({
                "content": e.content,
                "instruction": e.instruction,
                "type": format!("{:?}", e.memory_type),
                "priority": format!("{:?}", e.priority),
                "tags": e.tags,
                "confidence": e.confidence,
                "saved_id": saved_ids.get(i),
            })
        }).collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Scan for duplicate memories and report findings. Uses text similarity (Jaccard) to detect near-duplicates.")]
    async fn run_dedup(
        &self,
        Parameters(params): Parameters<RunDedupParams>,
    ) -> Result<CallToolResult, McpError> {
        let dedup = memvault_core::dedup::Deduplicator::new(self.store.clone(), None);
        let result = dedup.scan(params.namespace.as_deref()).await
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

    #[tool(description = "Run memory decay cycle. Reduces decay_score for old memories, archives memories below threshold. MUST memories are exempt.")]
    async fn run_decay(&self) -> Result<CallToolResult, McpError> {
        let dm = memvault_core::decay::DecayManager::new(
            self.store.clone(),
            memvault_core::decay::DecayConfig::default(),
        );
        let report = dm.run_decay().await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "updated": report.updated,
            "archived": report.archived,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    #[tool(description = "Confirm that one or more memories have been read by the agent. Updates access_count and last_read_at for the specified memory IDs.")]
    async fn confirm_read(
        &self,
        Parameters(params): Parameters<ConfirmReadParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.memory_ids.is_empty() {
            return Err(McpError::invalid_params("memory_ids must not be empty", None));
        }

        self.router.confirm_read(&params.memory_ids).await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "confirmed": params.memory_ids.len(),
            "memory_ids": params.memory_ids,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
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

#[allow(dead_code)]
pub async fn run_stdio_server(db_path: PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path.parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let embedder: Option<Arc<dyn EmbeddingProvider>> = if std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("MEMVAULT_EMBEDDING_MODEL").is_ok()
    {
        let e = OpenAIEmbedding::from_env();
        info!(model = %"from_env", "Embedding provider initialized");
        Some(Arc::new(e))
    } else {
        info!("No embedding provider configured (set OPENAI_API_KEY for semantic search)");
        None
    };

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

    run_stdio_server_with(store, router, embedder).await
}

pub async fn run_stdio_server_with(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
) -> anyhow::Result<()> {
    let server = MemVaultMcp::new(store, router, embedder);

    info!("MemVault MCP Server starting on stdio...");

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("MCP Server error: {}", e))?;

    service.waiting().await?;
    Ok(())
}
