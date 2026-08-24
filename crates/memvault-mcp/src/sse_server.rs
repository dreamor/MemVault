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
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::shutdown_signal())
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memvault_core::router::MemoryRouter;
    use memvault_core::storage::sqlite::SqliteStore;

    fn test_server() -> MemVaultMcp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        MemVaultMcp::new(store, router, None, None)
    }

    /// Start the real SSE/HTTP server on an ephemeral port and confirm the
    /// `/mcp` endpoint answers. Exercises the bind + serve + SSE-accept path
    /// that otherwise only runs in the binary.
    #[tokio::test]
    async fn test_run_sse_server_serves_mcp_endpoint() {
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let handle = tokio::spawn(run_sse_server(test_server(), port));
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/mcp");

        // The server task is async; poll until it accepts requests.
        let mut response = None;
        for _ in 0..40 {
            if let Ok(res) = client
                .get(&url)
                .header("Accept", "text/event-stream")
                .send()
                .await
            {
                response = Some(res);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let response = response.expect("server must accept a GET /mcp request");
        // Streamable-HTTP MCP answers a GET with an SSE stream (200), a 405
        // for a bad method, or a 400 for a malformed handshake — any non-404
        // response proves the `/mcp` route is mounted and the listener alive.
        assert!(
            response.status() != reqwest::StatusCode::NOT_FOUND,
            "/mcp route must be mounted, got 404"
        );
        handle.abort();
    }
}
