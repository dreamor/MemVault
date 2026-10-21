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
/// bound (may differ from the preferred port under `--auto-port`) and its URL.
#[derive(Debug, Clone)]
pub struct RunningServer {
    pub port: u16,
    pub url: String,
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

fn read_pid_file() -> Option<u32> {
    let path = local_server::pid_file()?;
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
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
        if local_server::probe_health(port).await.is_some() {
            return Some(RunningServer {
                port,
                url: url_for(port),
            });
        }
    }
    None
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
pub async fn ensure_running(preferred: u16, opts: &SpawnOptions) -> Result<RunningServer> {
    if let Some(running) = running_server(preferred).await {
        return Ok(running);
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
        let mut ports = vec![preferred];
        if let Some(p) = read_port_file() {
            ports.push(p);
        }
        for port in ports {
            if local_server::probe_health(port).await.is_some() {
                return Ok(RunningServer {
                    port,
                    url: url_for(port),
                });
            }
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

fn kill_process(pid: u32) -> Result<()> {
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

/// Stop a running auto-managed server (via its pid file), waiting for it to
/// leave the health endpoint.
pub async fn stop(preferred: u16) -> Result<()> {
    if running_server(preferred).await.is_none() {
        println!("MemVault server is not running");
        return Ok(());
    }
    let pid = read_pid_file().ok_or_else(|| {
        anyhow::anyhow!(
            "server is running but its pid file is missing; stop the memvault-mcp process manually"
        )
    })?;
    kill_process(pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if running_server(preferred).await.is_none() {
            for path in [local_server::pid_file(), local_server::port_file()]
                .into_iter()
                .flatten()
            {
                let _ = std::fs::remove_file(path);
            }
            println!("MemVault server stopped (pid {pid})");
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn port_and_pid_files_roundtrip_under_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("mv_cli_runtime_{:x}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("MEMVAULT_HOME");
        unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
        std::fs::write(local_server::port_file().unwrap(), "3888\n").unwrap();
        std::fs::write(local_server::pid_file().unwrap(), "1234\n").unwrap();
        assert_eq!(read_port_file(), Some(3888));
        assert_eq!(read_pid_file(), Some(1234));
        match original {
            Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
            None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn running_server_detects_existing_instance() {
        // Listener first (it awaits); the env-mutating part runs under the
        // lock with no awaits, and the guard is gone before the probes below.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // A fake MemVault health endpoint on an ephemeral port.
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Ok((mut stream, _)) = listener.accept().await {
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
        let dir = std::env::temp_dir().join(format!("mv_cli_detect_{:x}", std::process::id()));
        let original = {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            std::fs::create_dir_all(&dir).unwrap();
            let original = std::env::var_os("MEMVAULT_HOME");
            unsafe { std::env::set_var("MEMVAULT_HOME", &dir) }
            std::fs::write(local_server::port_file().unwrap(), port.to_string()).unwrap();
            original
        };

        let running = running_server(1).await.expect("should detect the instance");
        assert_eq!(running.port, port);
        assert_eq!(running.url, url_for(port));

        {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            match original {
                Some(v) => unsafe { std::env::set_var("MEMVAULT_HOME", v) },
                None => unsafe { std::env::remove_var("MEMVAULT_HOME") },
            }
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
