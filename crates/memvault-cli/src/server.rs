//! Local REST server lifecycle for `memvault-cli server …` and
//! `memvault-cli dashboard`: locate the `memvault-mcp` binary, health-probe
//! it, spawn it detached when missing, and stop it again. The REST server is
//! the host of the embedded Web Dashboard, so "start the server" and "open
//! the dashboard" are the same operation.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Subcommand;

use memvault_core::local_server;

/// `memvault-cli server …` subcommands.
#[derive(Subcommand, Debug, Clone, Copy)]
pub enum ServerCommand {
    /// Ensure the REST server (the Web Dashboard host) is running; start it
    /// detached when it is not (idempotent — safe for hooks and installers)
    Start {
        /// Preferred port. If it is owned by a foreign process the server
        /// auto-moves to the next free port (see: `memvault-mcp --auto-port`).
        #[arg(long, default_value_t = local_server::DEFAULT_REST_PORT)]
        port: u16,
        /// Print only the active URL (for scripts, installers and hooks)
        #[arg(long)]
        quiet: bool,
    },
    /// Show whether the REST server is running and its URL
    Status {
        #[arg(long, default_value_t = local_server::DEFAULT_REST_PORT)]
        port: u16,
    },
    /// Stop the running auto-managed server
    Stop {
        #[arg(long, default_value_t = local_server::DEFAULT_REST_PORT)]
        port: u16,
    },
}

fn spawn_options(
    db: Option<&str>,
    markdown_dir: Option<&str>,
    env_file: Option<&str>,
) -> SpawnOptions {
    SpawnOptions {
        db: db.map(String::from),
        markdown_dir: markdown_dir.map(String::from),
        env_file: env_file.map(String::from),
    }
}

/// Dispatch `memvault-cli server …`.
pub async fn dispatch(
    command: ServerCommand,
    db: Option<&str>,
    markdown_dir: Option<&str>,
    env_file: Option<&str>,
) -> Result<()> {
    let opts = spawn_options(db, markdown_dir, env_file);
    match command {
        ServerCommand::Start { port, quiet } => {
            let running = ensure_running(port, &opts).await?;
            if quiet {
                println!("{}", running.url);
            } else {
                println!(
                    "MemVault server running: {} (port {})",
                    running.url, running.port
                );
                println!("Web Dashboard: {}", running.url);
            }
            Ok(())
        }
        ServerCommand::Status { port } => match running_server(port).await {
            Some(running) => {
                println!("running: {}", running.url);
                Ok(())
            }
            None => {
                println!("not running");
                Ok(())
            }
        },
        ServerCommand::Stop { port } => stop(port).await,
    }
}

/// `memvault-cli dashboard`: ensure the server is running, then open it.
pub async fn dashboard(
    preferred: u16,
    db: Option<&str>,
    markdown_dir: Option<&str>,
    env_file: Option<&str>,
) -> Result<()> {
    let opts = spawn_options(db, markdown_dir, env_file);
    let running = ensure_running(preferred, &opts).await?;
    println!("MemVault Dashboard: {}", running.url);
    open_browser(&running.url);
    Ok(())
}

/// What `memvault-cli server start` returns: the port the server actually
/// bound (may differ from the preferred port under `--auto-port`), its URL,
/// and the build version it reported on `/api/health` (older servers predate
/// that field, so it is optional).
#[derive(Debug, Clone)]
pub struct RunningServer {
    pub port: u16,
    pub url: String,
    pub version: Option<String>,
}

/// Forwarded to the spawned server so it opens the same database/backend the
/// CLI would have used.
#[derive(Debug, Default, Clone)]
pub struct SpawnOptions {
    pub db: Option<String>,
    pub markdown_dir: Option<String>,
    pub env_file: Option<String>,
}

fn server_exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "memvault-mcp.exe"
    }
    #[cfg(not(windows))]
    {
        "memvault-mcp"
    }
}

/// Resolve the `memvault-mcp` binary: `$MEMVAULT_BIN`, then the sibling of
/// the running CLI (the installers put them side by side), then a PATH lookup.
pub fn server_binary() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("MEMVAULT_BIN") {
        return Some(PathBuf::from(bin));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && dir.join(server_exe_name()).is_file()
    {
        return Some(dir.join(server_exe_name()));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(server_exe_name()))
            .find(|p| p.is_file())
    })
}

/// Arguments for the spawned server process.
pub fn build_args(preferred: u16, opts: &SpawnOptions) -> Vec<String> {
    let mut args = vec![
        "--transport".to_string(),
        "http".to_string(),
        "--port".to_string(),
        preferred.to_string(),
        "--auto-port".to_string(),
    ];
    if let Some(db) = &opts.db {
        args.push("--db".to_string());
        args.push(db.clone());
    }
    if let Some(dir) = &opts.markdown_dir {
        args.push("--markdown-dir".to_string());
        args.push(dir.clone());
    }
    if let Some(env_file) = &opts.env_file {
        args.push("--env-file".to_string());
        args.push(env_file.clone());
    }
    args
}

/// Read the port recorded by the last auto-managed server run, if any.
pub fn read_port_file() -> Option<u16> {
    let path = local_server::port_file()?;
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()
}

pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Health-check a running server. `None` when nothing answers.
pub async fn running_server(preferred: u16) -> Option<RunningServer> {
    let mut ports = vec![preferred];
    if let Some(p) = read_port_file()
        && !ports.contains(&p)
    {
        ports.push(p);
    }
    for port in ports {
        if let Some(health) = local_server::probe_health(port).await {
            return Some(RunningServer {
                port,
                url: url_for(port),
                version: health.version,
            });
        }
    }
    None
}

/// This CLI's own build version — the build a freshly spawned server runs.
fn cli_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// What to do with a server that is already answering `/api/health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingServer {
    /// Same or newer build, or a version we cannot rank: leave it running.
    Adopt,
    /// Strictly older build: stop it and start the one this CLI ships.
    Replace,
}

/// Version comparison behind `ExistingServer`.
///
/// Adopting any healthy server is why an upgrade used to leave the old build
/// running forever: `brew upgrade` replaces the binary on disk, the detached
/// server keeps its old inode, `/api/health` keeps answering, and every later
/// `server start` returned that stale process. Only a *strictly older* build is
/// replaced — a newer one (a hand-built server, a second install) is adopted,
/// and an unparsable version is never a reason to kill a working server.
fn classify(running: &RunningServer) -> ExistingServer {
    let (Some(server), Some(cli)) = (
        running
            .version
            .as_deref()
            .and_then(local_server::parse_version),
        local_server::parse_version(cli_version()),
    ) else {
        return ExistingServer::Adopt;
    };
    if local_server::is_older(server, cli) {
        ExistingServer::Replace
    } else {
        ExistingServer::Adopt
    }
}

fn log_file_path() -> Result<PathBuf> {
    let dir = local_server::logs_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve the MemVault home directory"))?;
    std::fs::create_dir_all(&dir).context("create the MemVault logs directory")?;
    Ok(dir.join(format!(
        "mcp-{}.log",
        chrono::Local::now().format("%Y-%m-%d")
    )))
}

fn open_log_file() -> Result<std::fs::File> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path()?)?;
    writeln!(file, "--- memvault-mcp server start ---")?;
    Ok(file)
}

/// Detach the child from this process/terminal so it survives the CLI exit.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // New process group: avoids shell job control and terminal signals.
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Ensure a MemVault REST server is running, starting one detached when it is
/// not. Idempotent: concurrent callers converge on one instance thanks to the
/// probe-and-adopt logic in the server itself.
///
/// A *strictly older* server than this CLI is stopped and replaced rather than
/// adopted — an upgraded package otherwise kept serving from the process that
/// was already running (see `classify`).
pub async fn ensure_running(preferred: u16, opts: &SpawnOptions) -> Result<RunningServer> {
    if let Some(running) = running_server(preferred).await {
        if classify(&running) == ExistingServer::Adopt {
            return Ok(running);
        }
        // stderr, not stdout: `server start --quiet` promises the URL and
        // nothing else on stdout (installers and hooks parse it).
        eprintln!(
            "MemVault: replacing the running server (port {}, version {}) with this CLI's {}",
            running.port,
            running.version.as_deref().unwrap_or("unknown"),
            cli_version()
        );
        replace_stale(running.port).await?;
    }

    let bin = server_binary()
        .with_context(|| "cannot find memvault-mcp (set MEMVAULT_BIN or add it to PATH)")?;
    let log_file = open_log_file().context("open the server log file")?;
    let mut cmd = Command::new(&bin);
    cmd.args(build_args(preferred, opts))
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log_file.try_clone().context("dup the log stdout")?,
        ))
        .stderr(Stdio::from(log_file));
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("failed to start {}", bin.display()))?;
    // Fire-and-forget: the child is detached; wait for it to become healthy.

    // The port binds immediately even on a fresh install — the native
    // embedding model downloads silently in the background (the server
    // hot-swaps to semantic search once it lands), so a first-run model
    // download no longer lengthens this wait. MEMVAULT_SERVER_START_TIMEOUT
    // (seconds) remains tunable for CI/scripting.
    let timeout_secs: u64 = std::env::var("MEMVAULT_SERVER_START_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if let Some(running) = running_server(preferred).await {
            return Ok(running);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    anyhow::bail!(
        "the server did not become healthy within {timeout_secs}s; check {}",
        log_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "the server log".to_string())
    )
}

/// Stop a running auto-managed server (via its pid file), waiting for it to
/// leave the health endpoint.
pub async fn stop(preferred: u16) -> Result<()> {
    stop_quiet(preferred, false).await
}

/// Stop a server that answered a health probe but runs the wrong build, then
/// let the caller spawn this CLI's. Another session (its own SessionStart hook)
/// may have stopped it in the meantime — that is the same outcome, so a kill
/// that lost the race is only an error when the endpoint still answers.
async fn replace_stale(port: u16) -> Result<()> {
    match stop_quiet(port, true).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if running_server(port).await.is_none() {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// `stop`, with `quiet` suppressing the progress lines so an internal caller
/// cannot pollute stdout.
async fn stop_quiet(preferred: u16, quiet: bool) -> Result<()> {
    if running_server(preferred).await.is_none() {
        if !quiet {
            println!("MemVault server is not running");
        }
        return Ok(());
    }
    let pid = local_server::read_pid_file().ok_or_else(|| {
        anyhow::anyhow!(
            "server is running but its pid file is missing; stop the memvault-mcp process manually"
        )
    })?;
    local_server::ensure_pid_belongs_to_memvault(pid)?;
    local_server::kill_process(pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if running_server(preferred).await.is_none() {
            for path in [local_server::pid_file(), local_server::port_file()]
                .into_iter()
                .flatten()
            {
                let _ = std::fs::remove_file(path);
            }
            if !quiet {
                println!("MemVault server stopped (pid {pid})");
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("server did not stop within 5s (pid {pid})");
}

/// Open the dashboard URL in the system browser; failures are non-fatal (the
/// URL is printed either way).
pub fn open_browser(url: &str) {
    let opened = {
        #[cfg(target_os = "macos")]
        {
            Command::new("open").arg(url).status().is_ok()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Command::new("xdg-open").arg(url).status().is_ok()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "start", "", url])
                .status()
                .is_ok()
        }
    };
    if !opened {
        eprintln!("could not open a browser automatically; open {url} manually");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_defaults() {
        let opts = SpawnOptions::default();
        assert_eq!(
            build_args(3777, &opts),
            vec!["--transport", "http", "--port", "3777", "--auto-port"]
        );
    }

    #[test]
    fn build_args_forwards_db_markdown_and_env_file() {
        let opts = SpawnOptions {
            db: Some("/tmp/m.db".into()),
            markdown_dir: Some("/tmp/md".into()),
            env_file: Some("/tmp/env".into()),
        };
        let args = build_args(4000, &opts);
        assert_eq!(
            args,
            vec![
                "--transport",
                "http",
                "--port",
                "4000",
                "--auto-port",
                "--db",
                "/tmp/m.db",
                "--markdown-dir",
                "/tmp/md",
                "--env-file",
                "/tmp/env"
            ]
        );
    }

    #[test]
    fn url_for_formats_localhost() {
        assert_eq!(url_for(3777), "http://127.0.0.1:3777");
    }

    #[test]
    fn server_exe_name_is_the_mcp_binary() {
        // Platform constant used by `server_binary` for the sibling / PATH
        // lookup; a rename here would silently break server auto-start.
        #[cfg(windows)]
        assert_eq!(server_exe_name(), "memvault-mcp.exe");
        #[cfg(not(windows))]
        assert_eq!(server_exe_name(), "memvault-mcp");
    }

    #[test]
    fn server_binary_honors_memvault_bin_override() {
        // `MEMVAULT_BIN` short-circuits sibling/PATH discovery (used by the
        // smoke tests and by odd install layouts). The override branch returns
        // before any filesystem/PATH probe, so this is deterministic. Guarded
        // by TEST_ENV_LOCK so it cannot race any env-sensitive test here.
        let _guard = crate::TEST_ENV_LOCK.blocking_lock();
        let original = std::env::var_os("MEMVAULT_BIN");
        unsafe { std::env::set_var("MEMVAULT_BIN", "/custom/memvault-mcp") }
        assert_eq!(
            server_binary(),
            Some(PathBuf::from("/custom/memvault-mcp")),
            "MEMVAULT_BIN must win over sibling/PATH lookup"
        );
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_BIN", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_BIN") },
        }
    }

    #[test]
    fn port_and_pid_files_roundtrip_under_home() {
        let _guard = crate::TEST_ENV_LOCK.blocking_lock();
        let dir = std::env::temp_dir().join(format!("mv_cli_runtime_{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
        std::fs::write(local_server::port_file().unwrap(), "3888\n").unwrap();
        std::fs::write(local_server::pid_file().unwrap(), "1234\n").unwrap();
        assert_eq!(read_port_file(), Some(3888));
        assert_eq!(local_server::read_pid_file(), Some(1234));
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
        std::fs::remove_dir_all(dir).ok();
    }

    fn running_with_version(version: Option<&str>) -> RunningServer {
        RunningServer {
            port: 3777,
            url: url_for(3777),
            version: version.map(str::to_string),
        }
    }

    /// The previous release before `(major, minor, patch)`, borrowing from the
    /// minor (then the major) when the patch is already 0.
    ///
    /// Deriving this with `patch.saturating_sub(1)` instead — which is what this
    /// test used to do — silently yields the *current* version whenever the patch
    /// is 0, so the "older server" case asserted against an equal version and the
    /// test failed. That is not a corner case: it is every minor and major
    /// release, i.e. exactly the releases where this test matters most. 0.7.0 is
    /// what caught it.
    fn previous_release(v: (u64, u64, u64)) -> String {
        match v {
            (major, minor, 0) if minor > 0 => format!("{major}.{}.0", minor - 1),
            (major, 0, 0) => format!("{}.0.0", major.saturating_sub(1)),
            (major, minor, patch) => format!("{major}.{minor}.{}", patch - 1),
        }
    }

    #[test]
    fn previous_release_borrows_when_the_patch_is_zero() {
        assert_eq!(previous_release((0, 7, 0)), "0.6.0");
        assert_eq!(previous_release((0, 6, 12)), "0.6.11");
        assert_eq!(previous_release((1, 0, 0)), "0.0.0");
        assert_eq!(previous_release((0, 0, 0)), "0.0.0");
        assert_eq!(previous_release((2, 3, 1)), "2.3.0");
    }

    #[test]
    fn older_server_is_replaced_and_the_rest_are_adopted() {
        let cli =
            local_server::parse_version(env!("CARGO_PKG_VERSION")).expect("CLI version parses");
        let older = previous_release(cli);
        let newer = format!("{}.{}.{}", cli.0, cli.1, cli.2 + 1);

        // Guard the premise before asserting on it: if `older` ever came out equal
        // to the current version the assertions below would test nothing and fail
        // confusingly, which is exactly how the 0.7.0 bump surfaced this.
        assert!(
            local_server::is_older(
                local_server::parse_version(&older).expect("derived version parses"),
                cli
            ),
            "the derived previous release {older} must actually be older than {cli:?}"
        );

        // The upgrade case: a server left over from the previous release.
        assert_eq!(
            classify(&running_with_version(Some(&older))),
            ExistingServer::Replace
        );
        assert_eq!(
            classify(&running_with_version(Some(env!("CARGO_PKG_VERSION")))),
            ExistingServer::Adopt
        );
        assert_eq!(
            classify(&running_with_version(Some(&newer))),
            ExistingServer::Adopt,
            "a newer server must not be downgraded"
        );
        assert_eq!(classify(&running_with_version(None)), ExistingServer::Adopt);
        assert_eq!(
            classify(&running_with_version(Some("dev"))),
            ExistingServer::Adopt,
            "an unrankable version is never a reason to kill a server"
        );
    }

    #[tokio::test]
    async fn running_server_detects_existing_instance() {
        // A fake MemVault health endpoint on an ephemeral port. It keeps
        // accepting and drains the request before replying, like a real server:
        // closing a socket while the client's request is still unread makes the
        // kernel send a RST, and hyper then reports the just-answered probe as
        // `Canceled(UnexpectedMessage)` — the flake this test used to hit.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let body = r#"{"status":"ok","service":"memvault-mcp","version":"0.5.0"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        // `running_server` learns the recorded port by reading the port file
        // through `MEMVAULT_HOME`, so the lock has to span the probes too:
        // `port_and_pid_files_roundtrip_under_home` points `MEMVAULT_HOME` at its
        // own directory while it runs, and a probe that observed that would look
        // for the wrong recorded port and find nothing.
        let _guard = crate::TEST_ENV_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("mv_cli_detect_{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
        std::fs::write(local_server::port_file().unwrap(), port.to_string()).unwrap();

        let running = running_server(1).await.expect("should detect the instance");
        assert_eq!(running.port, port);
        assert_eq!(running.url, url_for(port));
        // The advertised build rides along: it is what tells an upgraded CLI
        // that the process answering here is the one it replaced on disk.
        assert_eq!(running.version.as_deref(), Some("0.5.0"));

        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
