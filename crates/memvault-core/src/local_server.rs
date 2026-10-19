//! Local MemVault REST server lifecycle helpers.
//!
//! Shared by `memvault-mcp` (the server itself), `memvault-cli` (the
//! `server start/status/stop` / `dashboard` commands) and the installers:
//! well-known file paths under the MemVault home directory, the default REST
//! port, and a health probe that can positively identify a running MemVault
//! server (as opposed to an unrelated process that happens to own the port).

use std::path::PathBuf;
use std::time::Duration;

/// Default REST port. Also the Vite dev-server proxy target in
/// `dashboard/vite.config.ts` and the documented default for `--port`.
pub const DEFAULT_REST_PORT: u16 = 3777;

/// How many extra ports `--auto-port` tries before giving up, when the
/// preferred port is owned by a process that is not MemVault.
pub const PORT_SEARCH_RANGE: u16 = 100;

/// Health probe timeout; a probe that does not answer inside this window is
/// treated as "not a running MemVault server".
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(700);

/// MemVault home directory: `$MEMVAULT_HOME`, else `~/.memvault` (Unix) or
/// `%USERPROFILE%\.memvault` (Windows). `None` when no home can be resolved.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("MEMVAULT_HOME") {
        return Some(PathBuf::from(home));
    }
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE")?;
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".memvault"))
}

/// Where the auto-started REST server records its PID, so `server stop` can
/// find it across platforms.
pub fn pid_file() -> Option<PathBuf> {
    home_dir().map(|h| h.join("mcp.pid"))
}

/// Where the auto-started REST server records the port it actually bound
/// (may differ from the preferred port under `--auto-port`). Clients that
/// cannot use the default port (Vite dev proxy, integrations) read this file.
pub fn port_file() -> Option<PathBuf> {
    home_dir().map(|h| h.join("mcp.port"))
}

/// Directory for daemon log files.
pub fn logs_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("logs"))
}

/// Identity payload served by `GET /api/health` on every MemVault REST
/// server. Probes use `service == "memvault-mcp"` to positively identify a
/// MemVault instance before adopting a busy port.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HealthInfo {
    pub status: String,
    pub service: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Probe `127.0.0.1:<port>/api/health`. Returns the health payload only when
/// a MemVault server answers; any other response (different service, error,
/// connection refused, timeout) yields `None`.
pub async fn probe_health(port: u16) -> Option<HealthInfo> {
    let url = format!("http://127.0.0.1:{port}/api/health");
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let info: HealthInfo = resp.json().await.ok()?;
    (info.service == "memvault-mcp").then_some(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate process env vars (cargo test runs them in
    /// parallel threads on the same process).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn home_dir_honors_memvault_home_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", "/custom/mv-home") }
        assert_eq!(home_dir(), Some(PathBuf::from("/custom/mv-home")));
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
    }

    #[test]
    fn home_dir_falls_back_to_user_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mh = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::remove_var("MEMVAULT_HOME") }
        #[cfg(windows)]
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        #[cfg(not(windows))]
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let expected = home.map(|h| h.join(".memvault"));
        assert_eq!(home_dir(), expected);
        if let Some(v) = mh {
            unsafe { std::env::set_var("MEMVAULT_HOME", v) }
        }
    }

    #[test]
    fn runtime_files_live_under_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", "/tmp/mv-runtime") }
        assert_eq!(pid_file(), Some(PathBuf::from("/tmp/mv-runtime/mcp.pid")));
        assert_eq!(port_file(), Some(PathBuf::from("/tmp/mv-runtime/mcp.port")));
        assert_eq!(logs_dir(), Some(PathBuf::from("/tmp/mv-runtime/logs")));
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
    }

    /// Serve a canned HTTP response on an ephemeral localhost port.
    async fn serve_once(body: &'static str, status_line: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let response = format!(
                    "{status_line}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        port
    }

    #[tokio::test]
    async fn probe_health_identifies_memvault() {
        let port = serve_once(
            r#"{"status":"ok","service":"memvault-mcp","version":"0.5.0"}"#,
            "HTTP/1.1 200 OK",
        )
        .await;
        let info = probe_health(port).await.expect("memvault health");
        assert_eq!(info.service, "memvault-mcp");
        assert_eq!(info.version.as_deref(), Some("0.5.0"));
    }

    #[tokio::test]
    async fn probe_health_rejects_foreign_service() {
        let port = serve_once(
            r#"{"status":"ok","service":"not-memvault"}"#,
            "HTTP/1.1 200 OK",
        )
        .await;
        assert!(probe_health(port).await.is_none());
    }

    #[tokio::test]
    async fn probe_health_rejects_bad_status() {
        let port = serve_once("nope", "HTTP/1.1 404 Not Found").await;
        assert!(probe_health(port).await.is_none());
    }

    #[tokio::test]
    async fn probe_health_connection_refused_is_none() {
        // Grab a free port by binding then dropping the listener.
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(probe_health(port).await.is_none());
    }
}
