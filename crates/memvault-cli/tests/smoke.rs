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
