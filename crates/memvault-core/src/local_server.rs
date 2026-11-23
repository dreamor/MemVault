//! Local MemVault REST server lifecycle helpers.
//!
//! Shared by `memvault-mcp` (the server itself), `memvault-cli` (the
//! `server start/status/stop` / `dashboard` commands) and the installers:
//! well-known file paths under the MemVault home directory, the default REST
//! port, and a health probe that can positively identify a running MemVault
//! server (as opposed to an unrelated process that happens to own the port).

use anyhow::Context;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

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
    user_home_dir_with_env(&|name| std::env::var_os(name)).map(|home| home.join(".memvault"))
}

/// Native account home, shared by runtime paths and strict journal resolution.
/// The injectable lookup keeps platform path tests independent of process env.
pub(crate) fn user_home_dir_with_env(
    env: &impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    #[cfg(windows)]
    let home = env("USERPROFILE")?;
    #[cfg(not(windows))]
    let home = env("HOME")?;
    if home.is_empty() || home.as_encoded_bytes().contains(&0) {
        return None;
    }
    Some(PathBuf::from(home))
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

/// A parsed `major.minor.patch` release triple.
pub type Version = (u64, u64, u64);

/// Parse the release triple out of a version string, dropping any
/// pre-release/build tail. `None` when the result cannot be ranked.
pub fn parse_version(raw: &str) -> Option<Version> {
    let core = raw.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next().unwrap_or("0").trim().parse().ok()?;
    let patch = parts.next().unwrap_or("0").trim().parse().ok()?;
    Some((major, minor, patch))
}

/// Component-wise numeric comparison, so `0.9.0` ranks below `0.10.0` (a
/// string compare would call the first one newer).
pub fn is_older(server: Version, current: Version) -> bool {
    server < current
}

/// Whether a healthy MemVault instance should be replaced by this build.
///
/// Only a *strictly older* build is replaced. Same, newer, and unparseable
/// versions are adopted: a newer hand-built server must not be downgraded, and
/// an unrankable version is never a reason to kill a working process.
pub fn should_replace_older(existing: Option<&str>, current: &str) -> bool {
    let (Some(existing), Some(current)) =
        (existing.and_then(parse_version), parse_version(current))
    else {
        return false;
    };
    is_older(existing, current)
}

/// PID recorded by the auto-managed (`--auto-port`) server, if any.
pub fn read_pid_file() -> Option<u32> {
    let path = pid_file()?;
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Full command line for `pid`, used to verify a pid file before signalling
/// it. `None` when the process is gone or the platform cannot inspect it.
#[cfg(target_os = "linux")]
pub fn process_command_line(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let text = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn process_command_line(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(windows)]
pub fn process_command_line(pid: u32) -> Option<String> {
    let script = format!("(Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine");
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Whether a command line starts with the managed server binary. Matching the
/// executable (not a substring anywhere in the arguments) avoids treating a
/// `grep memvault-mcp` or editor invocation as our server.
pub fn command_looks_like_memvault(command: &str) -> bool {
    let command = command.trim();
    let executable = if let Some(rest) = command.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else if let Some(rest) = command.strip_prefix('\'') {
        rest.split('\'').next().unwrap_or(rest)
    } else {
        command.split_whitespace().next().unwrap_or_default()
    };
    let file = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    let stem = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file);
    stem.eq_ignore_ascii_case("memvault-mcp")
}

/// A stale or attacker-controlled pid file must never turn `server stop` into
/// an arbitrary-process killer. Fail closed when the pid cannot be inspected.
pub fn ensure_pid_belongs_to_memvault(pid: u32) -> anyhow::Result<()> {
    let command = process_command_line(pid).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot verify that PID {pid} is memvault-mcp; refusing to kill it. \
             Remove the stale pid file or stop the server manually"
        )
    })?;
    anyhow::ensure!(
        command_looks_like_memvault(&command),
        "PID file points to a different process (pid {pid}: {command}); \
         refusing to kill it. Remove the stale pid file or stop the server manually"
    );
    Ok(())
}

/// Send SIGTERM/taskkill to a process already verified by
/// [`ensure_pid_belongs_to_memvault`].
pub fn kill_process(pid: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .context("run taskkill")?;
        anyhow::ensure!(status.success(), "taskkill failed for PID {pid}");
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .context("run kill")?;
        anyhow::ensure!(status.success(), "kill failed for PID {pid}");
    }
    Ok(())
}

/// Replace an older MemVault instance already serving `port`.
///
/// The pid file is trusted only after its command line is verified as
/// `memvault-mcp`. This is the `--auto-port` takeover path: the caller has
/// already probed `/api/health` and decided the running build is older.
pub async fn stop_managed_instance_on_port(port: u16) -> anyhow::Result<()> {
    if probe_health(port).await.is_none() {
        return Ok(());
    }
    let pid = read_pid_file().ok_or_else(|| {
        anyhow::anyhow!(
            "server is running but its pid file is missing; cannot replace the older build"
        )
    })?;
    ensure_pid_belongs_to_memvault(pid)?;
    kill_process(pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if probe_health(port).await.is_none() {
            for path in [pid_file(), port_file()].into_iter().flatten() {
                let _ = std::fs::remove_file(path);
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("server did not stop within 5s (pid {pid})")
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
    ///
    /// The request is drained before the response is written: closing a socket
    /// while the client's request is still unread makes the kernel send a RST,
    /// so the client reports `Canceled(UnexpectedMessage)` for a probe that was
    /// in fact answered.
    async fn serve_once(body: &'static str, status_line: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
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
    #[test]
    fn parse_version_accepts_triples_and_drops_tails() {
        assert_eq!(parse_version("0.6.4"), Some((0, 6, 4)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("0.6.4-rc.1"), Some((0, 6, 4)));
        assert_eq!(parse_version("0.6.4+build.9"), Some((0, 6, 4)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn parse_version_rejects_anything_unrankable() {
        for raw in ["", "unknown", "v0.6.4", "0.6.x", "dev"] {
            assert_eq!(parse_version(raw), None, "{raw:?}");
        }
        // Trailing components/tails are ignored rather than treated as a
        // parse failure — ranking the release triple is what matters.
        assert_eq!(parse_version("0.6.4.5"), Some((0, 6, 4)));
        assert_eq!(parse_version("0.6.4.5-beta"), Some((0, 6, 4)));
    }

    #[test]
    fn is_older_compares_components_numerically() {
        assert!(is_older((0, 9, 0), (0, 10, 0)));
        assert!(is_older((0, 6, 3), (0, 6, 4)));
        assert!(is_older((1, 0, 0), (2, 0, 0)));
        assert!(!is_older((0, 6, 4), (0, 6, 4)));
        assert!(!is_older((0, 6, 5), (0, 6, 4)));
    }

    #[test]
    fn only_a_strictly_older_server_is_replaced() {
        assert!(should_replace_older(Some("0.7.4"), "0.7.5"));
        assert!(!should_replace_older(Some("0.7.5"), "0.7.5"));
        assert!(!should_replace_older(Some("0.7.6"), "0.7.5"));
        assert!(!should_replace_older(None, "0.7.5"));
        assert!(!should_replace_older(Some("dev"), "0.7.5"));
    }

    #[test]
    fn command_identity_accepts_only_the_server_executable() {
        assert!(command_looks_like_memvault(
            "/usr/local/bin/memvault-mcp --transport http --port 3777"
        ));
        assert!(command_looks_like_memvault(
            r#""C:\Program Files\MemVault\memvault-mcp.exe" --transport http"#
        ));
        assert!(!command_looks_like_memvault("sleep 60"));
        assert!(!command_looks_like_memvault("grep memvault-mcp"));
        assert!(!command_looks_like_memvault("vim memvault-mcp.log"));
    }

    #[test]
    fn pid_identity_rejects_a_non_server_process() {
        // The test process itself is definitely not memvault-mcp, so the
        // guard must refuse to signal it instead of trusting a stale pid file.
        let err = ensure_pid_belongs_to_memvault(std::process::id())
            .expect_err("the test binary must not look like memvault-mcp");
        assert!(err.to_string().contains("refusing to kill"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn kill_process_terminates_a_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        kill_process(child.id()).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success(), "SIGTERM must end the child");
    }

    #[test]
    fn read_pid_file_roundtrips_under_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mv_pid_{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
        std::fs::write(pid_file().unwrap(), "1234\n").unwrap();
        assert_eq!(read_pid_file(), Some(1234));
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stopping_an_absent_instance_is_a_noop() {
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        stop_managed_instance_on_port(port)
            .await
            .expect("nothing running means nothing to stop");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn replacement_refuses_an_unverified_pid() {
        let port = serve_once(
            r#"{"status":"ok","service":"memvault-mcp","version":"0.7.4"}"#,
            "HTTP/1.1 200 OK",
        )
        .await;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mv_stop_{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
        std::fs::write(pid_file().unwrap(), format!("{}\n", std::process::id())).unwrap();
        let err = stop_managed_instance_on_port(port)
            .await
            .expect_err("a pid file for the test process must be refused");
        assert!(err.to_string().contains("refusing to kill"), "{err}");
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
