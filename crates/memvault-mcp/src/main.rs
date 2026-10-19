use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use memvault_core::compliance::ComplianceStore;
use memvault_mcp::{rest_api, server, sse_server};

#[derive(Parser)]
#[command(
    name = "memvault-mcp",
    version,
    about = "MemVault MCP + REST Server — AI Agent Memory Router"
)]
struct Args {
    /// SQLite database path (default: $MEMVAULT_DB, then
    /// $MEMVAULT_HOME/data.db, then ~/.memvault/data.db). Ignored when the
    /// markdown backend is active (see `--markdown-dir`).
    #[arg(long, value_name = "PATH")]
    db: Option<String>,

    /// Directory for the markdown storage backend (default:
    /// $MEMVAULT_MARKDOWN_DIR). Only used when
    /// MEMVAULT_STORAGE_BACKEND=markdown; ignored otherwise.
    #[arg(long, value_name = "DIR")]
    markdown_dir: Option<String>,

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

    /// Auto-managed mode for background/dashboard startup: if the preferred
    /// port is already serving MemVault, adopt it instead of failing; if it
    /// is owned by a foreign process, try the next ports (up to +100). Writes
    /// runtime files (~/.memvault/mcp.pid and mcp.port) so clients can find
    /// the active port. Only used with --transport http/rest.
    #[arg(long)]
    auto_port: bool,

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

/// CLI `--serve-web` wins over the `MEMVAULT_SERVE_WEB` env (the Docker image
/// sets the env to the dashboard baked in at /srv/dashboard). Returns the raw
/// path; `~/` expansion happens later in `resolve_path`.
fn merge_serve_web(flag: Option<String>, env: Option<std::ffi::OsString>) -> Option<String> {
    flag.or_else(|| env.map(|v| v.to_string_lossy().into_owned()))
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

    let sqlite_db_path = memvault_core::env_file::resolve_db(args.db.as_deref());
    let markdown_dir = args.markdown_dir.as_deref().map(resolve_path);

    let opened =
        memvault_core::storage::backend::open(&sqlite_db_path, markdown_dir.as_deref()).await?;
    let store = opened.store;
    let sqlite_store = opened.sqlite_store;
    // Shared file for ComplianceStore and the agents.yaml registry — the
    // sqlite data file, or the markdown backend's internal cache file.
    let db_path = opened.side_db_path;

    let registry_path = db_path
        .parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    // Lazy embedding (first-run UX): the `native` provider downloads its model
    // from HuggingFace on first launch — that must not gate the port bind or
    // the dashboard. The build is deferred to first use; a background warm-up
    // task starts the download immediately and consumers hot-swap the moment
    // the provider lands (keyword mode in the meantime).
    let embedder = server::lazy_embedder_from_env();
    server::spawn_embedder_warmup(&embedder);
    let router =
        server::build_router_with_embedder(store.clone(), &registry_path, &embedder).await?;

    let is_http = matches!(args.transport.as_str(), "http" | "rest");
    // An explicit --serve-web on a non-http transport is a user mistake worth
    // flagging; the env default (set by the Docker image) is silent instead —
    // stdio/sse containers carry the dashboard harmlessly.
    if args.serve_web.is_some() && !is_http {
        tracing::warn!("--serve-web is only used with --transport http/rest; ignoring it");
    }

    match args.transport.as_str() {
        "http" | "rest" => {
            let compliance = ComplianceStore::new(&db_path.to_string_lossy()).ok();
            // Priority: --serve-web flag > MEMVAULT_SERVE_WEB env > dist/
            // embedded in this binary (synced from dashboard/).
            let web_source =
                match merge_serve_web(args.serve_web, std::env::var_os("MEMVAULT_SERVE_WEB")) {
                    Some(d) => rest_api::WebSource::Dir(resolve_path(&d)),
                    None => rest_api::WebSource::Embedded,
                };
            // Lazy env probe: first llm-mode call resolves the extractor and
            // failures re-probe (rate-limited) — a boot-time Ollama hiccup
            // no longer disables llm extraction for the process lifetime.
            let llm = memvault_core::llm_extractor::LazyLlmExtractor::from_env();
            // --auto-port owns the runtime files (pid/port) so
            // `memvault-cli server status|stop` and the installers can find
            // the active instance. A plain foreground server stays passive.
            let runtime = if args.auto_port {
                let home = memvault_core::local_server::home_dir();
                match (
                    memvault_core::local_server::pid_file(),
                    memvault_core::local_server::port_file(),
                ) {
                    (Some(pid_file), Some(port_file)) => {
                        if let Some(ref h) = home {
                            std::fs::create_dir_all(h)?;
                        }
                        Some(rest_api::RuntimeFiles {
                            pid_file,
                            port_file,
                        })
                    }
                    _ => {
                        tracing::warn!(
                            "--auto-port: no home dir; continuing without pid/port files"
                        );
                        None
                    }
                }
            } else {
                None
            };
            rest_api::run_rest_server(
                store,
                sqlite_store,
                router,
                compliance,
                embedder,
                llm,
                args.port,
                web_source,
                args.auto_port,
                runtime,
            )
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
        // db stays None at parse time — the flag > MEMVAULT_DB >
        // $MEMVAULT_HOME > ~/.memvault/data.db chain is applied later by
        // memvault_core::env_file::resolve_db, after the env file loads.
        assert_eq!(args.db, None);
        assert_eq!(args.transport, "stdio");
        assert_eq!(args.port, 3777);
        assert_eq!(args.serve_web, None);
        assert!(!args.auto_port);
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
        assert_eq!(args.db.as_deref(), Some("/tmp/m.db"));
        assert_eq!(args.transport, "http");
        assert_eq!(args.port, 4000);
        assert_eq!(args.serve_web.as_deref(), Some("/tmp/dist"));
    }

    #[test]
    fn merge_serve_web_flag_wins_over_env() {
        assert_eq!(
            merge_serve_web(
                Some("/cli/dist".to_string()),
                Some(std::ffi::OsString::from("/env/dist"))
            ),
            Some("/cli/dist".to_string())
        );
    }

    #[test]
    fn merge_serve_web_falls_back_to_env_then_none() {
        assert_eq!(
            merge_serve_web(None, Some(std::ffi::OsString::from("/srv/dashboard"))),
            Some("/srv/dashboard".to_string())
        );
        assert_eq!(merge_serve_web(None, None), None);
    }

    #[test]
    fn version_flag_reports_crate_version() {
        // `--version` must short-circuit like every other clap binary, so
        // operators can audit which server build is on PATH (see
        // docs/TROUBLESHOOTING.md §5.3, multiple installs).
        let err = Args::try_parse_from(["memvault-mcp", "--version"])
            .err()
            .expect("--version must short-circuit parsing");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(
            err.to_string().contains(env!("CARGO_PKG_VERSION")),
            "version output must carry the crate version: {err}"
        );
    }
}
