use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use memvault_cli::{Cli, run};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // The env file must land before tracing init — RUST_LOG itself may come
    // from it — and before every config reader, which all just read env vars.
    let env_report = memvault_core::env_file::load(cli.env_file.as_deref());

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    memvault_core::env_file::log_report(&env_report);

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Reaching this match means `run` returned and its store —
            // including the Markdown backend's directory lock — was already
            // dropped by normal stack unwinding. Command implementations must
            // never `process::exit` while holding the store (issue #4); exit
            // codes are produced here, at the top level, after cleanup.
            if e.downcast_ref::<memvault_cli::UsageError>().is_some() {
                // Usage errors print their message at the detection site;
                // exit 2, matching the historical `process::exit(2)`.
                return ExitCode::from(2);
            }
            // Everything else keeps the old `main() -> Result` behavior:
            // Debug-print the error chain, exit 1.
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}
