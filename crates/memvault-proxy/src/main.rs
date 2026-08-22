use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tracing::info;

use memvault_core::compliance::ComplianceStore;
use memvault_core::embedding::{EmbeddingProvider, build_embedder_from_env};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;

use memvault_proxy::config::{TransportMode, load_config};
use memvault_proxy::context::SessionContext;
use memvault_proxy::handler::ProxyHandler;
use memvault_proxy::injection::InjectionEngine;
use memvault_proxy::upstream::UpstreamManager;

#[derive(Parser)]
#[command(
    name = "memvault-proxy",
    about = "MemVault MCP Proxy — transparent proxy with memory injection and compliance tracking"
)]
struct Args {
    /// Path to proxy config file
    #[arg(long, default_value = "~/.memvault/proxy.yaml")]
    config: String,

    /// Override transport mode (stdio or sse)
    #[arg(long)]
    transport: Option<String>,

    /// Override port (for SSE mode)
    #[arg(long, default_value = "3778")]
    port: u16,

    /// Override database path
    #[arg(long)]
    db: Option<String>,
}

fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/")
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(&raw[2..]);
    }
    PathBuf::from(raw)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("memvault_proxy=debug".parse()?)
                .add_directive("memvault_core=debug".parse()?),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let mut proxy_config = load_config(&args.config)?;

    if let Some(ref t) = args.transport {
        proxy_config.proxy.transport = match t.as_str() {
            "sse" => TransportMode::Sse,
            _ => TransportMode::Stdio,
        };
    }
    proxy_config.proxy.port = args.port;
    if let Some(ref db) = args.db {
        proxy_config.proxy.db = db.clone();
    }

    let db_path = resolve_path(&proxy_config.proxy.db);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Initialize core storage
    let store = Arc::new(SqliteStore::new(&db_path)?);

    // Initialize embedding provider(默认本地 Ollama,可选任意 OpenAI 兼容 API)
    let embedder: Option<Arc<dyn EmbeddingProvider>> = build_embedder_from_env().await;

    // Initialize router with optional agent registry
    let registry_path = db_path
        .parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let router = if registry_path.exists() {
        info!(path = %registry_path.display(), "loading agent registry");
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

    // Initialize compliance store (same db)
    let compliance = ComplianceStore::new(db_path.to_str().unwrap_or(":memory:"))?;

    // Connect to upstream MCP servers
    info!(
        count = proxy_config.proxy.upstreams.len(),
        "connecting to upstream MCP servers"
    );
    let upstreams = UpstreamManager::connect_all(&proxy_config.proxy.upstreams).await?;

    // Session context and injection engine
    let context = SessionContext::new();
    let injection = InjectionEngine::new(router.clone(), context.clone());
    injection.refresh().await;

    // Build proxy handler
    let proxy_handler =
        ProxyHandler::new(store, router, upstreams, context, injection, compliance).await;

    match proxy_config.proxy.transport {
        TransportMode::Stdio => {
            info!("starting MCP proxy (stdio mode)");
            let transport = rmcp::transport::stdio();
            let service = proxy_handler.serve(transport).await?;
            service.waiting().await?;
        }
        TransportMode::Sse => {
            run_sse_proxy(proxy_handler, proxy_config.proxy.port).await?;
        }
    }

    Ok(())
}

async fn run_sse_proxy(handler: ProxyHandler, port: u16) -> anyhow::Result<()> {
    let session_manager = Arc::new(LocalSessionManager::default());
    let config =
        StreamableHttpServerConfig::default().with_sse_keep_alive(Some(Duration::from_secs(15)));

    let svc = StreamableHttpService::new(move || Ok(handler.clone()), session_manager, config);
    let app = sse_app(svc);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MCP Proxy (SSE) listening on http://{}/mcp", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(memvault_proxy::shutdown::shutdown_signal())
        .await?;

    Ok(())
}

/// SSE-mode axum router: `/mcp` is the streamable-HTTP MCP endpoint, `/health`
/// a standalone liveness probe. Extracted as a function so the tests exercise
/// the *merged* router — a lone `/health` route would never catch a `/mcp` ↔
/// `/health` routing conflict.
fn sse_app(svc: StreamableHttpService<ProxyHandler, LocalSessionManager>) -> axum::Router {
    Router::new()
        .route("/mcp", axum::routing::any_service(svc))
        .route("/health", axum::routing::get(health))
}

/// Minimum liveness probe for the SSE server. Returns 200 without touching
/// the database or MCP session state, so the dsh bridge plugin can poll it
/// safely during process-readiness detection. See docs/DSH-BRIDGE-DESIGN.md
/// §1 (the optional Rust-side addition that design doc called out).
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "status": "ok", "service": "memvault-proxy" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Build the production SSE router backed by a real `ProxyHandler`, so the
    /// `/mcp` + `/health` routes are exercised together (coexistence was
    /// previously confirmed by manual curl only).
    async fn proxy_app() -> axum::Router {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let upstreams = UpstreamManager::connect_all(&[]).await.unwrap();
        let context = SessionContext::new();
        let injection = InjectionEngine::new(router.clone(), context.clone());
        let db = std::env::temp_dir().join(format!(
            "memvault_proxy_health_{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        let compliance = ComplianceStore::new(&db.to_string_lossy()).expect("compliance store");
        let handler =
            ProxyHandler::new(store, router, upstreams, context, injection, compliance).await;

        let session = Arc::new(LocalSessionManager::default());
        let svc = StreamableHttpService::new(
            move || Ok::<_, std::io::Error>(handler.clone()),
            session,
            StreamableHttpServerConfig::default(),
        );
        sse_app(svc)
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let response = proxy_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "memvault-proxy");
    }

    #[tokio::test]
    async fn health_endpoint_rejects_other_methods() {
        let response = proxy_app()
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
