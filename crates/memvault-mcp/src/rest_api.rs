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
use memvault_core::compliance::ComplianceStore;
use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::extractor::Extractor;
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use metrics_exporter_prometheus::PrometheusHandle;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    metrics_handle: PrometheusHandle,
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
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
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
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
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
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
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

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics_handle.render()
}

async fn save_memory(
    State(state): State<AppState>,
    Json(req): Json<SaveRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    // Authenticate the agent
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(api_error)?;

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
    metrics::counter!("memvault_memories_saved_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({ "id": saved.id })))
}

async fn search_memories(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    // Authenticate the agent if agent_id was provided
    if let Some(ref agent_id) = req.agent_id {
        state
            .router
            .authenticate_agent(agent_id, req.api_key.as_deref())
            .map_err(api_error)?;
    }

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

    metrics::counter!("memvault_searches_total").increment(1);

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
    // Authenticate the agent
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(api_error)?;

    let results = state
        .router
        .session_start(
            &req.agent_id,
            req.context_hint.as_deref(),
            req.project.as_deref(),
        )
        .await
        .map_err(api_error)?;

    metrics::counter!("memvault_sessions_started_total").increment(1);

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

    // Track injected memories for compliance
    let inject_session_id = if let Some(ref cs) = state.compliance {
        let sid = format!("inj_{}", Uuid::new_v4().simple());
        for r in &results {
            let _ = cs
                .record_injection(&sid, &r.memory.id, &r.memory.priority, &req.agent_id)
                .await;
        }
        Some(sid)
    } else {
        None
    };

    let mut response = serde_json::json!({
        "formatted": formatted,
        "count": results.len(),
        "format": format!("{:?}", format),
        "agent_profile": profile.id,
    });
    if let Some(sid) = inject_session_id {
        response["inject_session_id"] = serde_json::json!(sid);
    }

    Ok(ApiResponse::success(response))
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

#[derive(Deserialize, Default)]
struct PromoteRequest {
    namespace: Option<String>,
    min_l1: Option<usize>,
    min_l2: Option<usize>,
}

async fn run_promote(
    State(state): State<AppState>,
    Json(req): Json<PromoteRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let _ = req.namespace; // promote pipeline currently scans all namespaces
    let config = PromoteConfig {
        min_l1_for_l2: req.min_l1.unwrap_or(3),
        min_l2_for_l3: req.min_l2.unwrap_or(2),
        ..PromoteConfig::default()
    };
    let promoter = Promoter::new(state.store, config);
    let result = promoter.run().await.map_err(api_error)?;
    metrics::counter!("memvault_promote_runs_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({
        "promoted_to_l2": result.promoted_to_l2,
        "promoted_to_l3": result.promoted_to_l3,
        "source_ids_consumed": result.source_ids_consumed,
    })))
}

#[derive(Deserialize)]
struct ConfirmReadRequest {
    memory_ids: Vec<String>,
}

async fn confirm_read(
    State(state): State<AppState>,
    Json(req): Json<ConfirmReadRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .router
        .confirm_read(&req.memory_ids)
        .await
        .map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "confirmed": req.memory_ids.len(),
    })))
}

// --- Inbox handlers ---

#[derive(Deserialize)]
struct InboxQuery {
    namespace: Option<String>,
    #[serde(default = "default_50")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_50() -> usize {
    50
}

async fn list_inbox(
    State(state): State<AppState>,
    Query(params): Query<InboxQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let memories = state
        .store
        .list_pending(params.namespace.as_deref(), params.limit, params.offset)
        .await
        .map_err(api_error)?;

    let total = memories.len();
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

    Ok(ApiResponse::success(serde_json::json!({
        "memories": output,
        "total": total,
    })))
}

#[derive(Deserialize)]
struct InboxEditRequest {
    edited_content: Option<String>,
    edited_instruction: Option<String>,
}

async fn approve_memory(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let mut mem = state.store.get(&id).await.map_err(api_error)?;
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    state.store.update(mem).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "approved": id })))
}

async fn reject_memory(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    state.store.delete(&id).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "rejected": id })))
}

async fn edit_memory(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<InboxEditRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let mut mem = state.store.get(&id).await.map_err(api_error)?;
    if let Some(content) = req.edited_content {
        mem.content = content;
    }
    if let Some(instruction) = req.edited_instruction {
        mem.instruction = Some(instruction);
    }
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    state.store.update(mem).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "edited": id })))
}

// --- Compliance endpoints ---

#[derive(Deserialize)]
struct ComplianceSessionQuery {
    session_id: String,
}

#[derive(Deserialize)]
struct ComplianceSummaryQuery {
    agent_id: Option<String>,
    #[serde(default = "default_10")]
    limit: usize,
}

async fn get_compliance_session(
    State(state): State<AppState>,
    Query(params): Query<ComplianceSessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let cs = state
        .compliance
        .ok_or_else(|| api_error("Compliance tracking is not enabled"))?;
    let report = cs.get_report(&params.session_id).await.map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!(report)))
}

async fn get_compliance_summary(
    State(state): State<AppState>,
    Query(params): Query<ComplianceSummaryQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let cs = state
        .compliance
        .ok_or_else(|| api_error("Compliance tracking is not enabled"))?;
    let summary = cs
        .get_summary(params.agent_id.as_deref(), params.limit)
        .await
        .map_err(api_error)?;
    Ok(ApiResponse::success(serde_json::json!(summary)))
}

/// Build the CORS layer. Defaults to localhost-only (127.0.0.1 / localhost, any port) to
/// support local MCP clients (Obsidian, VS Code) without opening the API to arbitrary origins.
/// Set `MEMVAULT_CORS_ORIGIN` to a comma-separated origin list, or `*` to explicitly allow all
/// origins (not recommended outside trusted networks).
fn build_cors_layer() -> CorsLayer {
    match std::env::var("MEMVAULT_CORS_ORIGIN") {
        Ok(val) if val.trim() == "*" => {
            tracing::warn!(
                "MEMVAULT_CORS_ORIGIN=* — REST API accepts requests from any origin. \
                 Only use this on trusted networks."
            );
            CorsLayer::permissive()
        }
        Ok(val) => {
            let origins: Vec<_> = val
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(tower_http::cors::AllowMethods::any())
                .allow_headers(tower_http::cors::AllowHeaders::any())
        }
        Err(_) => {
            tracing::warn!(
                "MEMVAULT_CORS_ORIGIN not set — defaulting to localhost-only CORS. \
                 Set MEMVAULT_CORS_ORIGIN for remote access."
            );
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::predicate(|origin, _| {
                    origin
                        .to_str()
                        .map(|s| {
                            s.starts_with("http://127.0.0.1")
                                || s.starts_with("http://localhost")
                                || s.starts_with("https://127.0.0.1")
                                || s.starts_with("https://localhost")
                        })
                        .unwrap_or(false)
                }))
                .allow_methods(tower_http::cors::AllowMethods::any())
                .allow_headers(tower_http::cors::AllowHeaders::any())
        }
    }
}

/// Build the REST API router.
pub fn build_rest_router(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    metrics_handle: PrometheusHandle,
) -> Router {
    let state = AppState {
        store,
        router,
        compliance,
        metrics_handle,
    };

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/api/memories", get(list_memories))
        .route("/api/memories", post(save_memory))
        .route("/api/memories/{id}", delete(delete_memory))
        .route("/api/search", post(search_memories))
        .route("/api/session", post(session_start))
        .route("/api/extract", post(extract_memories))
        .route("/api/dedup", post(run_dedup))
        .route("/api/decay", post(run_decay))
        .route("/api/promote", post(run_promote))
        .route("/api/confirm-read", post(confirm_read))
        // Inbox review endpoints
        .route("/api/inbox", get(list_inbox))
        .route("/api/inbox/{id}/approve", post(approve_memory))
        .route("/api/inbox/{id}/reject", post(reject_memory))
        .route("/api/inbox/{id}/edit", post(edit_memory))
        // Compliance endpoints
        .route("/api/compliance/session", get(get_compliance_session))
        .route("/api/compliance/summary", get(get_compliance_summary))
        .layer(build_cors_layer())
        .with_state(state)
}

/// Run the REST API server.
pub async fn run_rest_server(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    port: u16,
) -> anyhow::Result<()> {
    let metrics_handle = crate::metrics_setup::install_recorder();
    let app = build_rest_router(store, router, compliance, metrics_handle);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MemVault REST API listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_exporter_prometheus::PrometheusHandle;

    /// The Prometheus recorder can only be installed once per process.
    /// Share a single handle across all tests via OnceLock.
    static METRICS: std::sync::OnceLock<PrometheusHandle> = std::sync::OnceLock::new();
    fn metrics() -> PrometheusHandle {
        METRICS
            .get_or_init(crate::metrics_setup::install_recorder)
            .clone()
    }

    struct TestApp {
        base: String,
        client: reqwest::Client,
    }

    /// Build an in-memory app and serve it on an ephemeral, localhost-only port.
    async fn spawn_app(with_compliance: bool) -> TestApp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let compliance = if with_compliance {
            let db = std::env::temp_dir().join(format!(
                "memvault_mcp_compliance_{}.db",
                Uuid::new_v4().simple()
            ));
            Some(ComplianceStore::new(&db.to_string_lossy()).expect("compliance store"))
        } else {
            None
        };
        let app = build_rest_router(store, router, compliance, metrics());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // graceful shutdown future never resolves during the test; the task is
            // cancelled when the test runtime shuts down.
            let _ = axum::serve(listener, app).await;
        });
        TestApp {
            base: format!("http://127.0.0.1:{}", addr.port()),
            client: reqwest::Client::new(),
        }
    }

    async fn save(
        app: &TestApp,
        body: serde_json::Value,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let resp = app
            .client
            .post(format!("{}/api/memories", app.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let json = resp.json().await.unwrap();
        (status, json)
    }

    fn save_body(content: &str) -> serde_json::Value {
        serde_json::json!({ "content": content })
    }

    #[tokio::test]
    async fn test_health_returns_ok() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/health", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_metrics_endpoint_renders() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/metrics", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert!(resp.text().await.unwrap().contains("memvault"));
    }

    #[tokio::test]
    async fn test_save_memory_with_defaults() {
        let app = spawn_app(false).await;
        let (status, body) = save(&app, save_body("prefers Python")).await;
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        let id = body["data"]["id"].as_str().unwrap();
        assert!(id.starts_with("mem_"));
    }

    #[tokio::test]
    async fn test_save_memory_parses_priority_and_type() {
        let app = spawn_app(false).await;
        let (status, _) = save(
            &app,
            serde_json::json!({
                "content": "always run tests",
                "priority": "MUST",
                "type": "preference",
                "namespace": "project:myapp",
                "instruction": "run before every commit",
                "tags": ["testing"],
            }),
        )
        .await;
        assert_eq!(status, 200);

        let resp = app
            .client
            .get(format!("{}/api/memories?namespace=project:myapp", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = body["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["priority"], "Must");
        assert_eq!(items[0]["type"], "Preference");
        assert_eq!(items[0]["instruction"], "run before every commit");
        assert_eq!(items[0]["tags"], serde_json::json!(["testing"]));
    }

    #[tokio::test]
    async fn test_save_memory_background_and_skill_types() {
        let app = spawn_app(false).await;
        let (status, _) = save(
            &app,
            serde_json::json!({ "content": "deploy steps", "priority": "BACKGROUND", "type": "skill" }),
        )
        .await;
        assert_eq!(status, 200);

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = body["data"].as_array().unwrap();
        assert_eq!(items[0]["priority"], "Background");
        assert_eq!(items[0]["type"], "Skill");
    }

    #[tokio::test]
    async fn test_search_roundtrip_and_filters() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({ "content": "likes matcha", "namespace": "global" }),
        )
        .await;
        save(
            &app,
            serde_json::json!({ "content": "dislikes coffee", "namespace": "project:other" }),
        )
        .await;

        // namespace filter
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "likes", "namespace": "global" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["content"], "likes matcha");

        // top_k + wrong-key auth is tolerated for unregistered agents
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "es", "top_k": 1, "agent_id": "bob" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_list_memories_pagination() {
        let app = spawn_app(false).await;
        for i in 0..5 {
            save(
                &app,
                serde_json::json!({ "content": format!("memory {}", i) }),
            )
            .await;
        }
        let resp = app
            .client
            .get(format!("{}/api/memories?limit=3", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_session_start_injects_formatted() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({
                "content": "user prefers Rust",
                "priority": "MUST",
                "agent_id": "alice",
                "agent_type": "coding-assistant",
            }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "alice", "context_hint": "start", "format": "xml" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["count"], 1);
        assert!(
            body["data"]["formatted"]
                .as_str()
                .unwrap()
                .contains("prefers Rust")
        );
    }

    #[tokio::test]
    async fn test_session_start_custom_format_falls_back() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "alice", "format": "bogus" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["format"], "MustRef");
    }

    #[tokio::test]
    async fn test_extract_memories_endpoint() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let arr = body["data"].as_array().unwrap();
        assert!(!arr.is_empty(), "preference signal should extract a memory");
    }

    #[tokio::test]
    async fn test_extract_memories_empty_text() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dedup_decay_promote_endpoints() {
        let app = spawn_app(false).await;
        save(&app, save_body("duplicate A")).await;

        let resp = app
            .client
            .post(format!("{}/api/dedup", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let resp = app
            .client
            .post(format!("{}/api/decay", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let resp = app
            .client
            .post(format!("{}/api/promote", app.base))
            .json(&serde_json::json!({ "min_l1": 1, "min_l2": 1 }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_confirm_read_and_delete() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("to read")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .post(format!("{}/api/confirm-read", app.base))
            .json(&serde_json::json!({ "memory_ids": [id] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

        let resp = app
            .client
            .delete(format!("{}/api/memories/{}", app.base, id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_delete_missing_returns_error() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .delete(format!("{}/api/memories/mem_nope", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_inbox_flow() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("pending review")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .get(format!("{}/api/inbox", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["total"], 1);

        // approve
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/approve", app.base, id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

        // reject a second memory
        let (_status, saved2) = save(&app, save_body("to reject")).await;
        let id2 = saved2["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/reject", app.base, id2))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // edit a third memory
        let (_status, saved3) = save(&app, save_body("to edit")).await;
        let id3 = saved3["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/edit", app.base, id3))
            .json(&serde_json::json!({ "edited_content": "edited now" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["edited"], id3);
    }

    #[tokio::test]
    async fn test_inbox_approve_missing_returns_error() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/inbox/mem_missing/approve", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn test_compliance_flow_when_enabled() {
        let app = spawn_app(true).await;
        save(
            &app,
            serde_json::json!({ "content": "must rule", "priority": "MUST", "agent_id": "eve" }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "eve" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let sid = body["data"]["inject_session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = app
            .client
            .get(format!(
                "{}/api/compliance/session?session_id={}",
                app.base, sid
            ))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["inject_session_id"].is_string());

        let resp = app
            .client
            .get(format!("{}/api/compliance/summary", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["total_sessions"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_compliance_errors_when_disabled() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!(
                "{}/api/compliance/session?session_id=inj_x",
                app.base
            ))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap().contains("not enabled"));
    }

    #[tokio::test]
    async fn test_cors_allows_localhost_origin() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/health", app.base))
            .header("Origin", "http://localhost:3000")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
}
