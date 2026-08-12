mod rest_api;
mod server;
mod sse_server;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use memvault_core::compliance::ComplianceStore;
use memvault_core::embedding::{EmbeddingProvider, OpenAIEmbedding};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;

#[derive(Parser)]
#[command(
    name = "memvault-mcp",
    about = "MemVault MCP + REST Server — AI Agent Memory Router"
)]
struct Args {
    #[arg(long, default_value = "~/.memvault/data.db")]
    db: String,

    /// Transport mode: "stdio" for MCP stdio, "sse" for MCP over HTTP/SSE, "http" for REST API
    #[arg(long, default_value = "stdio")]
    transport: String,

    /// HTTP port (only used with --transport sse or --transport http)
    #[arg(long, default_value = "3777")]
    port: u16,
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
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let db_path = resolve_path(&args.db);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path
        .parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let embedder: Option<Arc<dyn EmbeddingProvider>> = if std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("MEMVAULT_EMBEDDING_MODEL").is_ok()
    {
        info!("Embedding provider initialized from env");
        Some(Arc::new(OpenAIEmbedding::from_env()))
    } else {
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

    match args.transport.as_str() {
        "http" | "rest" => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            rest_api::run_rest_server(store, router, compliance, args.port).await?;
        }
        "sse" => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            let mcp_server = server::MemVaultMcp::new(store, router, embedder, compliance);
            sse_server::run_sse_server(mcp_server, args.port).await?;
        }
        _ => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            server::run_stdio_server_with(store, router, embedder, compliance).await?;
        }
    }

    Ok(())
}
