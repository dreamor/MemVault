use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tracing::info;

use crate::server::MemVaultMcp;

/// Run the MCP server over SSE/HTTP transport.
///
/// Multiple MCP clients can connect simultaneously via HTTP.
/// The endpoint is available at `http://<host>:<port>/mcp`.
pub async fn run_sse_server(server: MemVaultMcp, port: u16) -> anyhow::Result<()> {
    let session_manager = Arc::new(LocalSessionManager::default());

    let config =
        StreamableHttpServerConfig::default().with_sse_keep_alive(Some(Duration::from_secs(15)));

    let svc = StreamableHttpService::new(move || Ok(server.clone()), session_manager, config);

    let app = Router::new().route("/mcp", axum::routing::any_service(svc));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MCP SSE Server listening on http://{}/mcp", addr);
    info!("Connect any MCP client with:");
    info!("  url: http://127.0.0.1:{}/mcp", port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
