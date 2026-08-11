use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::info;

use memvault_core::agent_adapt::{self, InjectFormat};
use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::extractor::Extractor;
use memvault_core::models::*;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;

#[derive(Clone)]
struct AppState {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
}

// --- Request/Response types ---

#[derive(Deserialize)]
struct SaveRequest {
    content: String,
    #[serde(default = "default_ref")]
    priority: String,
    #[serde(default = "default_fact")]
    r#type: String,
    #[serde(default = "default_global")]
    namespace: String,
    instruction: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_unknown")]
    agent_id: String,
    #[serde(default = "default_general")]
    agent_type: String,
}

fn default_ref() -> String {
    "REFERENCE".into()
}
fn default_fact() -> String {
    "fact".into()
}
fn default_global() -> String {
    "global".into()
}
fn default_unknown() -> String {
    "unknown".into()
}
fn default_general() -> String {
    "general-assistant".into()
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_10")]
    top_k: usize,
    namespace: Option<String>,
    agent_id: Option<String>,
}

fn default_10() -> usize {
    10
}

#[derive(Deserialize)]
struct SessionRequest {
    agent_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    agent_type: Option<String>,
    context_hint: Option<String>,
    project: Option<String>,
    /// Override injection format: "must_ref", "xml", "system_prompt", "markdown"
    format: Option<String>,
}

#[derive(Deserialize)]
struct ListQuery {
    namespace: Option<String>,
    #[serde(default = "default_100")]
    limit: usize,
}

fn default_100() -> usize {
    100
}

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    fn success(data: T) -> Json<Self> {
        Json(Self {
            ok: true,
            data: Some(data),
            error: None,
        })
    }
}

fn api_error(msg: impl ToString) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.to_string()),
        }),
    )
}

// --- Handlers ---

async fn health() -> &'static str {
    "ok"
}

async fn save_memory(
    State(state): State<AppState>,
    Json(req): Json<SaveRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let priority = match req.priority.to_uppercase().as_str() {
        "MUST" => Priority::Must,
        "BACKGROUND" => Priority::Background,
        _ => Priority::Reference,
    };
    let memory_type = match req.r#type.to_lowercase().as_str() {
        "preference" => MemoryType::Preference,
        "episode" => MemoryType::Episode,
        "entity" => MemoryType::Entity,
        "skill" => MemoryType::Skill,
        _ => MemoryType::Fact,
    };

    let mut mem = Memory::new(
        memory_type,
        req.content,
        priority,
        SourceAgent {
            id: req.agent_id,
            agent_type: req.agent_type,
            session_id: None,
        },
    );
    mem.namespace = req.namespace;
    mem.instruction = req.instruction;
    mem.tags = req.tags;

    let saved = state.store.save(mem).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "id": saved.id })))
}

async fn search_memories(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let results = state
        .store
        .search(SearchQuery {
            query: req.query,
            top_k: req.top_k,
            namespace: req.namespace,
            agent_id: req.agent_id,
            ..SearchQuery::new(String::new())
        })
        .await
        .map_err(api_error)?;

    let output: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.memory.id,
                "content": r.memory.content,
                "instruction": r.memory.instruction,
                "priority": format!("{:?}", r.memory.priority),
                "tags": r.memory.tags,
                "score": r.score,
            })
        })
        .collect();

    Ok(ApiResponse::success(output))
}

async fn session_start(
    State(state): State<AppState>,
    Json(req): Json<SessionRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let results = state
        .router
        .session_start(
            &req.agent_id,
            req.context_hint.as_deref(),
            req.project.as_deref(),
        )
        .await
        .map_err(api_error)?;

    // Determine injection format
    let profile = state.router.get_agent_profile(&req.agent_id);
    let format = req
        .format
        .as_deref()
        .map(|f| match f {
            "xml" => InjectFormat::Xml,
            "system_prompt" => InjectFormat::SystemPrompt,
            "markdown" => InjectFormat::Markdown,
            _ => InjectFormat::MustRef,
        })
        .unwrap_or_else(|| agent_adapt::best_format_for_agent(&profile.agent_type));

    let formatted = agent_adapt::format_memories(&results, format);

    Ok(ApiResponse::success(serde_json::json!({
        "formatted": formatted,
        "count": results.len(),
        "format": format!("{:?}", format),
        "agent_profile": profile.id,
    })))
}

async fn list_memories(
    State(state): State<AppState>,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let memories = state
        .store
        .list(params.namespace.as_deref(), params.limit, 0)
        .await
        .map_err(api_error)?;

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
                "human_reviewed": m.human_reviewed,
            })
        })
        .collect();

    Ok(ApiResponse::success(output))
}

async fn delete_memory(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    state.store.delete(&id).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "deleted": id })))
}

async fn extract_memories(Json(body): Json<serde_json::Value>) -> impl IntoResponse {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let extracted = Extractor::extract(text);
    let output: Vec<serde_json::Value> = extracted
        .iter()
        .map(|e| {
            serde_json::json!({
                "content": e.content,
                "instruction": e.instruction,
                "type": format!("{:?}", e.memory_type),
                "priority": format!("{:?}", e.priority),
                "tags": e.tags,
                "confidence": e.confidence,
            })
        })
        .collect();
    ApiResponse::success(output)
}

async fn run_dedup(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let dedup = Deduplicator::new(state.store, None);
    let result = dedup.scan(None).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "unique": result.unique_count,
        "duplicates": result.duplicates.len(),
    })))
}

async fn run_decay(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let dm = DecayManager::new(state.store, DecayConfig::default());
    let report = dm.run_decay().await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "updated": report.updated,
        "archived": report.archived,
    })))
}

/// Build the REST API router.
pub fn build_rest_router(store: Arc<SqliteStore>, router: Arc<MemoryRouter>) -> Router {
    let state = AppState { store, router };

    Router::new()
        .route("/health", get(health))
        .route("/api/memories", get(list_memories))
        .route("/api/memories", post(save_memory))
        .route("/api/memories/{id}", delete(delete_memory))
        .route("/api/search", post(search_memories))
        .route("/api/session", post(session_start))
        .route("/api/extract", post(extract_memories))
        .route("/api/dedup", post(run_dedup))
        .route("/api/decay", post(run_decay))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Run the REST API server.
pub async fn run_rest_server(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    port: u16,
) -> anyhow::Result<()> {
    let app = build_rest_router(store, router);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MemVault REST API listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
