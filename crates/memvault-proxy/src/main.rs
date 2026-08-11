mod config;
mod context;
pub mod extraction;
mod handler;
mod injection;
mod merge;
mod upstream;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use rmcp::ServiceExt;
use tracing::info;

use memvault_core::compliance::ComplianceStore;
use memvault_core::embedding::{EmbeddingProvider, OpenAIEmbedding};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;

use crate::config::{TransportMode, load_config};
use crate::context::SessionContext;
use crate::handler::ProxyHandler;
use crate::injection::InjectionEngine;
use crate::upstream::UpstreamManager;

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

    // Initialize embedding provider (optional)
    let embedder: Option<Arc<dyn EmbeddingProvider>> = if std::env::var("OPENAI_API_KEY").is_ok() {
        info!("Embedding provider initialized");
        Some(Arc::new(OpenAIEmbedding::from_env()))
    } else {
        None
    };

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
    let proxy_handler = ProxyHandler::new(store, router, upstreams, context, injection, compliance);

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
    use axum::Router;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let session_manager = Arc::new(LocalSessionManager::default());
    let config =
        StreamableHttpServerConfig::default().with_sse_keep_alive(Some(Duration::from_secs(15)));

    let svc = StreamableHttpService::new(move || Ok(handler.clone()), session_manager, config);

    let app = Router::new().route("/mcp", axum::routing::any_service(svc));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MCP Proxy (SSE) listening on http://{}/mcp", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
