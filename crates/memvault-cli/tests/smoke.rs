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
