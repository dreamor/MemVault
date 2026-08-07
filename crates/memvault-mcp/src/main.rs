mod server;

use std::path::PathBuf;
use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "memvault-mcp", about = "MemVault MCP Server — AI Agent Memory Router")]
struct Args {
    #[arg(long, default_value = "~/.memvault/data.db")]
    db: String,
}

fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            return home.join(&raw[2..]);
        }
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

    server::run_stdio_server(db_path).await
}
