//! Binary smoke test: run the compiled `memvault-cli` executable end to end.
//!
//! Exercises the real `main()` path (argument parsing + dispatch) which the
//! unit tests in `lib.rs` cannot reach.

use std::path::PathBuf;
use std::process::Command;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_memvault-cli")
}

fn temp_db() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "memvault_cli_bin_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("data.db")
}

fn run(args: &[&str]) -> (String, String) {
    let out = Command::new(exe())
        .args(args)
        .output()
        .expect("spawn memvault-cli binary");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn run_env(args: &[&str], envs: &[(&str, &str)]) -> (String, String, bool) {
    let out = Command::new(exe())
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .expect("spawn memvault-cli binary");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

fn temp_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "memvault_cli_home_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(target_os = "linux")]
struct ContentAccessWatch(std::fs::File);

#[cfg(target_os = "linux")]
impl ContentAccessWatch {
    fn new(paths: &[&std::path::Path]) -> Self {
        use std::os::fd::FromRawFd;
        unsafe extern "C" {
            fn inotify_init1(flags: std::os::raw::c_int) -> std::os::raw::c_int;
            fn inotify_add_watch(
                fd: std::os::raw::c_int,
                pathname: *const std::os::raw::c_char,
                mask: u32,
            ) -> std::os::raw::c_int;
        }
        // Linux O_NONBLOCK | O_CLOEXEC, IN_OPEN | IN_ACCESS. Metadata
        // detection is permitted; opening/reading source/config is not.
        let fd = unsafe { inotify_init1(0x800 | 0x80000) };
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        let watcher = Self(unsafe { std::fs::File::from_raw_fd(fd) });
        for path in paths {
            let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            let watch = unsafe { inotify_add_watch(fd, path.as_ptr(), 0x20 | 0x01) };
            assert!(watch >= 0, "{}", std::io::Error::last_os_error());
        }
        watcher
    }

    fn accessed(&mut self) -> bool {
        use std::io::Read;
        let mut events = [0u8; 1024];
        match self.0.read(&mut events) {
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => false,
            Ok(bytes) if bytes > 0 => true,
            other => panic!("unexpected inotify read: {other:?}"),
        }
    }
}

fn isolated_command(home: &std::path::Path, cwd: &std::path::Path) -> Command {
    let mut command = Command::new(exe());
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .env("MEMVAULT_EMBEDDING_PROVIDER", "off")
        .env("MEMVAULT_LLM_EXTRACTION_PROVIDER", "off")
        .env("RUST_LOG", "warn")
        .current_dir(cwd);
    command
}

fn scan_binary_contract(explicit_env: bool) {
    let root = temp_home();
    let home = root.join("home");
    let cwd = root.join("project");
    let appdata = home.join(".memvault");
    std::fs::create_dir_all(&appdata).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let source = cwd.join("AGENTS.md");
    std::fs::write(
        &source,
        "# Owned fixture\nPrefer deterministic owned-source-contents\n",
    )
    .unwrap();
    let default_env = appdata.join(".env");
    let explicit = root.join("explicit.env");
    // Deliberately incomplete backend configuration: scans must not consume
    // it, while the non-scan positive control below still does.
    std::fs::write(&default_env, "MEMVAULT_STORAGE_BACKEND=markdown\n").unwrap();
    std::fs::write(&explicit, "MEMVAULT_STORAGE_BACKEND=markdown\n").unwrap();
    let db = root.join("untouched-vault").join("data.db");
    #[cfg(target_os = "linux")]
    let mut watcher = ContentAccessWatch::new(&[&source, &default_env, &explicit]);
    let mut command = isolated_command(&home, &cwd);
    command.arg("--db").arg(&db);
    if explicit_env {
        command.arg("--env-file").arg(&explicit);
    }
    let output = command
        .args(["import-agent", "--agent", "CoDeX", "--path"])
        .arg(&source)
        .arg("--scan")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}; stderr: {stderr}"
    );
    assert!(stdout.contains("detected"), "{stdout}");
    assert!(!stdout.contains("candidate(s) parsed"), "{stdout}");
    assert!(!stdout.contains("owned-source-contents"), "{stdout}");
    assert!(!db.exists(), "scan must not create a database");
    assert!(
        !db.parent().unwrap().exists(),
        "scan must not initialize a vault"
    );
    #[cfg(target_os = "linux")]
    assert!(
        !watcher.accessed(),
        "scan opened/accessed source or env configuration"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scan_binary_skips_default_env_source_reads_and_database() {
    scan_binary_contract(false);
}

#[test]
fn scan_binary_skips_explicit_env_source_reads_and_database() {
    scan_binary_contract(true);
}

#[test]
fn non_scan_binary_still_loads_default_and_explicit_configuration() {
    for explicit_env in [false, true] {
        let root = temp_home();
        let appdata = root.join(".memvault");
        std::fs::create_dir_all(&appdata).unwrap();
        let env_file = if explicit_env {
            root.join("explicit.env")
        } else {
            appdata.join(".env")
        };
        std::fs::write(&env_file, "MEMVAULT_STORAGE_BACKEND=markdown\n").unwrap();
        #[cfg(target_os = "linux")]
        let mut watcher = ContentAccessWatch::new(&[&env_file]);
        let mut command = isolated_command(&root, &root);
        command.arg("--db").arg(root.join("untouched.db"));
        if explicit_env {
            command.arg("--env-file").arg(&env_file);
        }
        let output = command.arg("list").output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "non-scan commands must load their configuration"
        );
        assert!(stderr.contains("requires --markdown-dir"), "{stderr}");
        #[cfg(target_os = "linux")]
        assert!(
            watcher.accessed(),
            "positive control must load the env file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn fixture_bench_binary_skips_user_configuration_vault_and_providers() {
    for explicit_env in [false, true] {
        let root = temp_home();
        let appdata = root.join(".memvault");
        let markdown = root.join("markdown");
        std::fs::create_dir_all(&appdata).unwrap();
        std::fs::create_dir_all(&markdown).unwrap();
        let default_env = appdata.join(".env");
        let explicit = root.join("explicit.env");
        let agents = appdata.join("agents.yaml");
        let db = appdata.join("data.db");
        let note = markdown.join("owned.md");
        for env_file in [&default_env, &explicit] {
            std::fs::write(env_file, "MEMVAULT_STORAGE_BACKEND=markdown\n").unwrap();
        }
        std::fs::write(&agents, "invalid: [\n").unwrap();
        std::fs::write(&db, "not a SQLite vault").unwrap();
        std::fs::write(&note, "untouched 中文🙂\n").unwrap();
        let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let provider_url = format!("http://{}", provider.local_addr().unwrap());
        #[cfg(target_os = "linux")]
        let mut watcher = ContentAccessWatch::new(&[&default_env, &explicit, &agents, &db, &note]);
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/experiments/fixtures/task_bench_v1.json");
        let mut command = isolated_command(&root, &root);
        command
            .arg("--db")
            .arg(&db)
            .env("MEMVAULT_STORAGE_BACKEND", "invalid")
            .env("MEMVAULT_LLM_EXTRACTION_PROVIDER", "openai")
            .env("MEMVAULT_LLM_EXTRACTION_API_BASE", &provider_url)
            .env("MEMVAULT_LLM_EXTRACTION_API_KEY", "local-test-not-a-key")
            .env("MEMVAULT_EMBEDDING_PROVIDER", "openai")
            .env("MEMVAULT_EMBEDDING_API_BASE", &provider_url)
            .env("MEMVAULT_EMBEDDING_API_KEY", "local-test-not-a-key");
        if explicit_env {
            command.arg("--env-file").arg(&explicit);
        }
        let output = command
            .args(["bench", "--fixture"])
            .arg(&fixture)
            .args(["--agent-id", "fixture", "--compare-modes", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["judging_requested"], false);
        assert_eq!(report["embeddings_enabled"], false);
        assert_eq!(report["history_persisted"], false);
        assert_eq!(report["runs"].as_array().unwrap().len(), 2);
        for run in report["runs"].as_array().unwrap() {
            assert_eq!(run["report"]["total"], 6);
            assert_eq!(run["report"]["distinct_tasks"], 6);
            assert_eq!(run["report"]["judged"], 0);
            assert!(run["report"]["paired_pass_rate_delta_pct"].is_null());
        }
        #[cfg(target_os = "linux")]
        assert!(
            !watcher.accessed(),
            "fixture opened user configuration/vault"
        );
        assert_eq!(std::fs::read_to_string(&db).unwrap(), "not a SQLite vault");
        assert_eq!(
            std::fs::read_to_string(&note).unwrap(),
            "untouched 中文🙂\n"
        );
        assert!(!appdata.join("data.db-wal").exists());
        assert!(!appdata.join("eval_history.jsonl").exists());
        assert_eq!(std::fs::read_dir(&appdata).unwrap().count(), 3);
        assert!(
            matches!(provider.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// The local-server commands must not open the store: they dispatch before
/// the database is constructed, so a broken/locked DB can never block
/// `server status` / `server stop`.
#[test]
fn server_commands_do_not_touch_the_store() {
    let home = temp_home();
    let home_str = home.to_string_lossy().to_string();
    let envs = [("MEMVAULT_HOME", home_str.as_str())];

    let (out, _, ok) = run_env(&["server", "status", "--port", "1"], &envs);
    assert!(ok && out.contains("not running"), "stdout: {out}");

    let (out, _, ok) = run_env(&["server", "stop", "--port", "1"], &envs);
    assert!(ok && out.contains("is not running"), "stdout: {out}");

    assert!(
        !home.join("data.db").exists(),
        "server commands must not create the database"
    );
    std::fs::remove_dir_all(home).ok();
}

/// `dashboard` is `server start` + open browser; with the server binary
/// missing it must fail the same clean way (and never open a browser or
/// touch the store).
#[test]
fn dashboard_reports_missing_binary_cleanly() {
    let home = temp_home();
    let home_str = home.to_string_lossy().to_string();
    let envs = [
        ("MEMVAULT_HOME", home_str.as_str()),
        ("MEMVAULT_BIN", "/nonexistent/memvault-mcp"),
    ];
    let (out, err, ok) = run_env(&["dashboard", "--port", "1"], &envs);
    assert!(!ok, "dashboard with a missing binary must fail");
    let msg = format!("{out}{err}");
    assert!(
        msg.contains("failed to start") || msg.contains("cannot find memvault-mcp"),
        "message: {msg}"
    );
    assert!(
        !home.join("data.db").exists(),
        "dashboard must not create the database"
    );
    std::fs::remove_dir_all(home).ok();
}

/// `server start` must fail with a clear message (not a panic) when the
/// memvault-mcp binary cannot be found.
#[test]
fn server_start_reports_missing_binary_cleanly() {
    let home = temp_home();
    let home_str = home.to_string_lossy().to_string();
    let envs = [
        ("MEMVAULT_HOME", home_str.as_str()),
        ("MEMVAULT_BIN", "/nonexistent/memvault-mcp"),
    ];
    let (out, err, ok) = run_env(&["server", "start", "--port", "47777"], &envs);
    assert!(!ok, "start with a missing binary must fail");
    let msg = format!("{out}{err}");
    assert!(msg.contains("failed to start"), "message: {msg}");
    std::fs::remove_dir_all(home).ok();
}

#[test]
fn smoke_save_search_list_delete() {
    let db = temp_db().to_string_lossy().to_string();
    let db_flag = format!("--db={}", db);

    // save
    let (out, _) = run(&[&db_flag, "save", "--content", "binary smoke memory"]);
    assert!(out.contains("Saved:"), "stdout: {}", out);

    // search finds it
    let (out, _) = run(&[&db_flag, "search", "--query", "smoke"]);
    assert!(out.contains("binary smoke memory"), "stdout: {}", out);

    // list non-empty
    let (out, _) = run(&[&db_flag, "list", "--limit", "10"]);
    assert!(out.contains("Total:"), "stdout: {}", out);

    // session start does not error on empty context
    let (_, err) = run(&[&db_flag, "session-start", "--agent-id", "cli"]);
    assert!(err.is_empty(), "stderr: {}", err);

    // dedup / decay / promote / backup tolerate data
    let (out, _) = run(&[&db_flag, "dedup"]);
    assert!(out.contains("Unique:"), "stdout: {}", out);
    let (out, _) = run(&[&db_flag, "decay"]);
    assert!(out.contains("Decay cycle:"), "stdout: {}", out);
    let (out, _) = run(&[&db_flag, "promote"]);
    assert!(out.contains("Promote:"), "stdout: {}", out);

    let backup = std::env::temp_dir()
        .join(format!(
            "memvault_cli_bin_bak_{}.db",
            uuid::Uuid::new_v4().simple()
        ))
        .to_string_lossy()
        .to_string();
    let (out, _) = run(&[&db_flag, "backup", "--output", &backup]);
    assert!(out.contains("Backup written"), "stdout: {}", out);
    assert!(PathBuf::from(&backup).exists());
    std::fs::remove_file(&backup).ok();

    // export & import round trip
    let json_out = std::env::temp_dir()
        .join(format!(
            "memvault_cli_bin_{}.json",
            uuid::Uuid::new_v4().simple()
        ))
        .to_string_lossy()
        .to_string();
    let (out, _) = run(&[
        &db_flag, "export", "--format", "json", "--output", &json_out,
    ]);
    assert!(out.contains("Exported to"), "stdout: {}", out);

    let db2 = temp_db().to_string_lossy().to_string();
    let db2_flag = format!("--db={}", db2);
    let (out, _) = run(&[
        &db2_flag, "import", "--format", "json", "--input", &json_out,
    ]);
    assert!(out.contains("Imported 1 memories"), "stdout: {}", out);

    // extract without save
    let (out, _) = run(&[&db_flag, "extract", "--text", "I always prefer dark mode"]);
    assert!(out.contains("Extracted"), "stdout: {}", out);

    std::fs::remove_file(&json_out).ok();
}

fn saved_id(out: &str) -> String {
    out.lines()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("Saved: ")
                .or_else(|| {
                    // "  imported: title (mem_xxx)" used by import-skills
                    l.find("imported:").map(|_| l)
                })
                .and_then(|rest| {
                    rest.split('(')
                        .next()
                        .and_then(|t| t.split_whitespace().next())
                        .map(str::to_string)
                })
        })
        .expect("no Saved: line in output")
}

fn pending_id(out: &str) -> String {
    // review list lines look like: `[Reference|L2] mem_xxx — content`
    out.lines()
        .find_map(|l| {
            let l = l.trim();
            if l.starts_with('[') && l.contains(']') {
                let after = &l[l.find(']').unwrap() + 1..];
                after.split_whitespace().next().map(str::to_string)
            } else {
                None
            }
        })
        .expect("no pending line in review output")
}

#[test]
fn smoke_outcome_review_supersede_import_doctor() {
    let db = temp_db().to_string_lossy().to_string();
    let db_flag = format!("--db={}", db);

    // Record a failed outcome -> episode + rule-based lesson.
    let (out, err) = run(&[
        &db_flag,
        "outcome",
        "--task",
        "deploy checkout",
        "--status",
        "failure",
        "--cause",
        "disk full",
        "--task-type",
        "deploy",
    ]);
    assert!(out.contains("Recorded:"), "stdout: {}", out);
    assert!(
        out.contains("Lesson"),
        "lesson should be distilled: {}",
        out
    );
    // Embedding/LLM builds may log to stderr in some environments; the
    // functional contract lives on stdout.
    let _ = err;

    // Save two facts and supersede the stale one.
    let (out, _) = run(&[&db_flag, "save", "--content", "old fact: api at /v1"]);
    assert!(out.contains("Saved:"), "stdout: {}", out);
    let old_id = saved_id(&out);

    let (out, _) = run(&[&db_flag, "save", "--content", "new fact: api at /v2"]);
    assert!(out.contains("Saved:"), "stdout: {}", out);
    let new_id = saved_id(&out);

    let (out, _) = run(&[&db_flag, "supersede", "--old", &old_id, "--new", &new_id]);
    assert!(out.contains("Superseded:"), "stdout: {}", out);
    assert!(
        out.contains(&old_id) && out.contains(&new_id),
        "stdout: {}",
        out
    );

    // Review queue lists the pending lesson; approve it.
    let (out, _) = run(&[&db_flag, "review"]);
    assert!(
        out.contains("Pending:"),
        "review should show a queue: {}",
        out
    );
    let pending = pending_id(&out);
    let (out, _) = run(&[&db_flag, "review", "--approve", &pending]);
    assert!(out.contains("Approved:"), "stdout: {}", out);

    // Import a SOP skill file (marked reviewed).
    let sop = std::env::temp_dir().join(format!(
        "memvault_cli_sop_{}.md",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(
        &sop,
        "# Deploy Runbook\n\ntrigger: deploy\nverification: health ok\n\n- check env\n- push\n",
    )
    .unwrap();
    let sop_s = sop.to_string_lossy().to_string();
    let (out, _) = run(&[&db_flag, "import-skills", "--file", &sop_s, "--approve"]);
    assert!(out.contains("Imported 1 skill(s)"), "stdout: {}", out);
    assert!(out.contains("marked reviewed"), "stdout: {}", out);

    std::fs::remove_file(&sop).ok();
}

#[test]
fn smoke_session_start_respects_inject_channel() {
    // Feature F: an agent whose canonical inject channel is "sync" must NOT
    // receive session_start injection through the CLI (backed by the Claude
    // Code SessionStart hook) — same dedup contract as REST /api/session and
    // the MCP session_start tool.
    let dir = std::env::temp_dir().join(format!(
        "memvault_cli_bin_chan_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agents.yaml"),
        r#"
agents:
  - id: sync-agent
    agent_type: coding-assistant
    description: "sync only"
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global"]
      exclude_types: []
    inject_channel: sync
"#,
    )
    .unwrap();
    let db = dir.join("data.db");
    let db_flag = format!("--db={}", db.to_string_lossy());

    // Seed a MUST memory that would be injected if the gate were bypassed.
    let (_, err) = run(&[
        &db_flag,
        "save",
        "--content",
        "always run the full suite before merging",
        "--priority",
        "MUST",
    ]);
    assert!(err.is_empty(), "stderr: {}", err);

    let (out, _) = run(&[
        &db_flag,
        "session-start",
        "--agent-id",
        "sync-agent",
        "--context",
        "begin",
    ]);
    assert!(
        out.contains("injected via the 'sync' channel"),
        "expected channel-skip note, got: {}",
        out
    );
    assert!(
        !out.contains("[MUST]"),
        "sync-gated agent must not receive CLI injection: {}",
        out
    );

    // hook-json envelope path must still emit a valid (empty) envelope.
    let (out, _) = run(&[
        &db_flag,
        "session-start",
        "--agent-id",
        "sync-agent",
        "--context",
        "begin",
        "--format",
        "hook-json",
    ]);
    assert!(
        out.trim_start().starts_with("{\"hookSpecificOutput\""),
        "hook-json should still be a valid envelope, got: {}",
        out
    );
    assert!(!out.contains("[MUST]"), "envelope must be empty: {}", out);

    let _ = std::fs::remove_dir_all(dir);
}
