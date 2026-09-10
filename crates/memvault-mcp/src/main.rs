use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use memvault_core::compliance::ComplianceStore;
use memvault_core::embedding::{EmbeddingProvider, build_embedder_from_env};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_mcp::{rest_api, server, sse_server};

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

    /// Serve a built Web Dashboard (dist/) at the REST server root.
    /// Only used with --transport http/rest.
    #[arg(long)]
    serve_web: Option<String>,

    /// Env file to load at startup (default: $MEMVAULT_HOME/.env or
    /// ~/.memvault/.env; an absent file is silently skipped). Values already
    /// set in the environment always win.
    #[arg(long)]
    env_file: Option<String>,
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
    let args = Args::parse();

    // The env file must land before tracing init — RUST_LOG itself may come
    // from it — and before every config reader, which all just read env vars.
    let env_report = memvault_core::env_file::load(args.env_file.as_deref());

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    memvault_core::env_file::log_report(&env_report);

    let db_path = resolve_path(&args.db);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path
        .parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let embedder: Option<Arc<dyn EmbeddingProvider>> = build_embedder_from_env().await;

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

    let is_http = matches!(args.transport.as_str(), "http" | "rest");
    if args.serve_web.is_some() && !is_http {
        tracing::warn!("--serve-web is only used with --transport http/rest; ignoring it");
    }

    match args.transport.as_str() {
        "http" | "rest" => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            let web_dir = args.serve_web.as_ref().map(|d| resolve_path(d));
            // Lazy env probe: first llm-mode call resolves the extractor and
            // failures re-probe (rate-limited) — a boot-time Ollama hiccup
            // no longer disables llm extraction for the process lifetime.
            let llm = memvault_core::llm_extractor::LazyLlmExtractor::from_env();
            rest_api::run_rest_server(store, router, compliance, embedder, llm, args.port, web_dir)
                .await?;
        }
        "sse" => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            let mcp_server =
                server::MemVaultMcp::new(store, router, embedder, compliance)
                    .with_lazy_llm_extractor(
                        memvault_core::llm_extractor::LazyLlmExtractor::from_env(),
                    );
            sse_server::run_sse_server(mcp_server, args.port).await?;
        }
        _ => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            server::run_stdio_server_with(store, router, embedder, compliance).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_expands_tilde_with_home() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("HOME set");
        let expanded = resolve_path("~/.memvault/data.db");
        let expected = home.join(".memvault/data.db");
        assert_eq!(expanded, expected);
    }

    #[test]
    fn resolve_path_keeps_absolute_and_relative() {
        assert_eq!(
            resolve_path("/tmp/x.db"),
            std::path::PathBuf::from("/tmp/x.db")
        );
        assert_eq!(resolve_path("data.db"), std::path::PathBuf::from("data.db"));
    }

    #[test]
    fn args_parse_defaults() {
        let args = Args::try_parse_from(["memvault-mcp"]).expect("defaults parse");
        assert_eq!(args.db, "~/.memvault/data.db");
        assert_eq!(args.transport, "stdio");
        assert_eq!(args.port, 3777);
        assert_eq!(args.serve_web, None);
    }

    #[test]
    fn args_parse_overrides() {
        let args = Args::try_parse_from([
            "memvault-mcp",
            "--db",
            "/tmp/m.db",
            "--transport",
            "http",
            "--port",
            "4000",
            "--serve-web",
            "/tmp/dist",
        ])
        .expect("overrides parse");
        assert_eq!(args.db, "/tmp/m.db");
        assert_eq!(args.transport, "http");
        assert_eq!(args.port, 4000);
        assert_eq!(args.serve_web.as_deref(), Some("/tmp/dist"));
    }
}
