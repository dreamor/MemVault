use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use memvault_cli::{Cli, run};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    run(Cli::parse()).await
}
