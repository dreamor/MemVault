use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use memvault_cli::{Cli, run};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // The env file must land before tracing init — RUST_LOG itself may come
    // from it — and before every config reader, which all just read env vars.
    let env_report = memvault_core::env_file::load(cli.env_file.as_deref());

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    memvault_core::env_file::log_report(&env_report);

    run(cli).await
}
