//! Flat configuration via a dot-env file (`~/.memvault/.env`).
//!
//! Resolution order — later layers never override earlier ones:
//!
//! 1. CLI flag (`--env-file <path>`, per binary)
//! 2. Real process environment (mcpServers `env` maps, shell exports, CI)
//! 3. The env file (see [`default_path`])
//! 4. Built-in defaults inside each reader
//!
//! The file is loaded once, at the very top of every binary's `main`,
//! *before* tracing init — so `RUST_LOG` set in the file also works. Values
//! are pushed into real process env via [`std::env::set_var`] only for keys
//! that are not already present, which is why the many lazy read sites
//! ([[`crate::writer`]] delta-write, [[`crate::router`]] corroboration, the
//! [`crate::llm_extractor`] re-probe) need no changes at all: they keep
//! reading plain env vars and transparently pick up file values.
//!
//! This file holds *server* configuration (the keys read by crates code).
//! It deliberately excludes two groups: the host installation contract
//! (`MEMVAULT_AGENT_ID`, `MEMVAULT_HOOK_EXTRACT`, … — per-agent values
//! authored in each host's own plugin/mcpServers config) and the proxy's
//! `upstreams` list (structured data; lives in `proxy.yaml`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tracing::{info, warn};

/// The configuration knobs crates code actually reads — one row each in the
/// `memvault status` provenance table. Legacy `OPENAI_*` fallbacks included
/// so "where did this key come from" always has an answer.
pub const CONFIG_KEYS: &[&str] = &[
    "MEMVAULT_HOME",
    "MEMVAULT_DB",
    "MEMVAULT_DB_POOL_SIZE",
    "MEMVAULT_CORS_ORIGIN",
    "RUST_LOG",
    "MEMVAULT_EMBEDDING_PROVIDER",
    "MEMVAULT_EMBEDDING_MODEL",
    "MEMVAULT_EMBEDDING_API_BASE",
    "MEMVAULT_EMBEDDING_API_KEY",
    "MEMVAULT_EMBEDDING_DIM",
    "OPENAI_API_KEY",
    "MEMVAULT_LLM_EXTRACTION_PROVIDER",
    "MEMVAULT_LLM_EXTRACTION_API_BASE",
    "MEMVAULT_LLM_EXTRACTION_API_KEY",
    "MEMVAULT_LLM_EXTRACTION_MODEL",
    "MEMVAULT_RELATIONS",
    "MEMVAULT_EXTRACT_ASSISTANT",
    "MEMVAULT_DELTA_WRITE",
    "MEMVAULT_CONTEXT_NGRAM_WINDOW",
    "MEMVAULT_IDENTITY_VERIFICATION",
    "MEMVAULT_CORROBORATION_GATE",
    "MEMVAULT_CORROBORATION_MIN_AGENTS",
];

/// Where a configuration value came from, as reported by [`provenance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Not set anywhere — the reader's built-in default is in effect.
    Default,
    /// Set in the process environment (or CLI context) before load.
    Env,
    /// Applied from an env file at startup (payload: the file path).
    File(String),
}

/// Report of one [`load`] run.
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    /// The file that was loaded; `None` when absent (silent, zero-config).
    pub path: Option<PathBuf>,
    /// Keys the file introduced into the environment.
    pub applied: usize,
    /// Keys the file listed but a real env var already held — env wins.
    pub skipped: usize,
    /// Lines that could not be parsed (skipped; the rest of the file loads).
    pub warnings: Vec<String>,
}

/// Keys this process took from the env file, mapped to the file path they
/// came from. Populated only by [`load`] (i.e. real binaries), so library
/// tests never see file-provenance state.
fn file_applied() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse one env-file-body string into ordered KEY/VALUE pairs plus the
/// lines that could not be parsed. Pure — no environment access.
///
/// Rules: `#` starts a comment line (whole-line comments only; values run to
/// end of line); an optional `export ` prefix is accepted; values are
/// trimmed and one layer of matching `'`/`"` quotes is stripped; there is no
/// variable interpolation.
pub fn parse(content: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut pairs = Vec::new();
    let mut warnings = Vec::new();

    for (idx, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("line {}: not KEY=VALUE: {}", idx + 1, raw.trim()));
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            warnings.push(format!("line {}: invalid key: {}", idx + 1, raw.trim()));
            continue;
        }
        pairs.push((key.to_string(), strip_quotes(value.trim())));
    }
    (pairs, warnings)
}

/// Strip one layer of matching surrounding quotes ('…' or "…"). No escape
/// processing — a value needing literal quotes at both ends can use the
/// other quote kind.
fn strip_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    let quoted =
        bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[bytes.len() - 1];
    if quoted {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Default env-file location: `$MEMVAULT_HOME/.env`, falling back to
/// `~/.memvault/.env`. `MEMVAULT_HOME` is intentionally only read from the
/// real environment here — the file cannot define the directory it lives in.
pub fn default_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("MEMVAULT_HOME") {
        return Some(PathBuf::from(home).join(".env"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".memvault").join(".env"))
}

/// Resolve the file to load: explicit path > `MEMVAULT_ENV_FILE` >
/// [`default_path`]. `None` only when no HOME is available at all.
pub fn resolve_path(cli_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = cli_path {
        return Some(PathBuf::from(shellexpand_home(p)));
    }
    if let Ok(p) = std::env::var("MEMVAULT_ENV_FILE")
        && !p.trim().is_empty()
    {
        return Some(PathBuf::from(shellexpand_home(&p)));
    }
    default_path()
}

/// Expand a leading `~/` like every binary's `resolve_path` does.
fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return format!("{}/{}", home.to_string_lossy(), rest);
    }
    path.to_string()
}

/// Resolve the SQLite database path — the full precedence chain `.env.example`
/// documents: CLI `--db` flag > `MEMVAULT_DB` (real process environment, or an
/// env-file value, since binaries run [`load`] before resolving the db; an
/// empty value counts as unset) > `$MEMVAULT_HOME/data.db` >
/// `~/.memvault/data.db`.
pub fn resolve_db(cli_db: Option<&str>) -> PathBuf {
    if let Some(p) = cli_db {
        return PathBuf::from(shellexpand_home(p));
    }
    if let Ok(p) = std::env::var("MEMVAULT_DB")
        && !p.trim().is_empty()
    {
        return PathBuf::from(shellexpand_home(&p));
    }
    if let Some(home) = std::env::var_os("MEMVAULT_HOME") {
        return PathBuf::from(home).join("data.db");
    }
    PathBuf::from(shellexpand_home("~/.memvault/data.db"))
}

/// Load the env file into process environment. Missing file is silent
/// (zero-config default); a malformed line is skipped and surfaced as a
/// warning in the report. Call exactly once, at the top of `main`, before
/// tracing init and before any thread or async task exists.
pub fn load(cli_path: Option<&str>) -> LoadReport {
    let Some(path) = resolve_path(cli_path) else {
        return LoadReport::default();
    };
    if !path.exists() {
        return LoadReport::default();
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "env file unreadable; skipping");
            return LoadReport::default();
        }
    };
    let (pairs, warnings) = parse(&content);

    let mut report = LoadReport {
        path: Some(path.clone()),
        warnings,
        ..LoadReport::default()
    };

    for (key, value) in pairs {
        if std::env::var_os(&key).is_some() {
            report.skipped += 1;
            continue;
        }
        // SAFETY: `load` is documented to run at the top of `main`, before
        // any thread, async task, or other code can read the environment —
        // the single-threaded window `set_var` requires.
        unsafe {
            std::env::set_var(&key, &value);
        }
        file_applied()
            .lock()
            .unwrap()
            .insert(key, path.display().to_string());
        report.applied += 1;
    }
    report
}

/// Log non-default findings from a [`load`] report. Called by binaries right
/// after tracing init, so users see why the fileload mattered (or broke).
pub fn log_report(report: &LoadReport) {
    if let Some(path) = &report.path {
        info!(
            path = %path.display(),
            applied = report.applied,
            skipped = report.skipped,
            "env file loaded"
        );
    }
    for w in &report.warnings {
        warn!(target: "env_file", "{}", w);
    }
}

/// Config-source provenance for one key: what layer supplied the value a
/// reader will see. Answers "where did this value come from" for diagnostics.
pub fn provenance(key: &str) -> Source {
    if let Some(path) = file_applied().lock().unwrap().get(key) {
        return Source::File(path.clone());
    }
    if std::env::var_os(key).is_some() {
        return Source::Env;
    }
    Source::Default
}

/// One row of the [`provenance_table`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedEntry {
    pub key: String,
    /// Current value; API keys are masked to a fixed stub.
    pub value: String,
    pub source: Source,
}

fn masked(key: &str) -> bool {
    key.ends_with("API_KEY")
}

/// Build the provenance table [`crate::CONFIG_KEYS`]-style diagnostics print
/// (see `memvault status`). Secrets are masked regardless of source.
pub fn provenance_table() -> Vec<TrackedEntry> {
    CONFIG_KEYS
        .iter()
        .map(|&key| {
            let value = match (masked(key), std::env::var(key)) {
                (true, Ok(_)) => "(set, masked)".to_string(),
                (false, Ok(v)) => v,
                (_, Err(_)) => "(unset)".to_string(),
            };
            TrackedEntry {
                key: key.to_string(),
                value,
                source: provenance(key),
            }
        })
        .collect()
}

/// Unified boolean env parsing. Canonical spellings documented in
/// `.env.example` are `true`/`false`; the other accepted aliases exist so
/// hand-written values still do the obvious thing.
pub fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "enabled" => Some(true),
        "0" | "false" | "off" | "no" | "disabled" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize the tests in this module that mutate process env, mirroring
    /// the ENV_LOCK pattern used by embedding/llm_extractor tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_parse_comments_blank_and_pairs() {
        let content =
            "\n# a comment\nFOO=bar\n\n  SPACED  =  value  \nexport BAZ=qux\n#X=commented-out\n";
        let (pairs, warnings) = parse(content);
        assert_eq!(
            pairs,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("SPACED".to_string(), "value".to_string()),
                ("BAZ".to_string(), "qux".to_string()),
            ]
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_parse_strips_one_layer_of_matching_quotes() {
        let (pairs, _) = parse("A=\"double quoted\"\nB='single'\nC=\"unbalanced'\nD=no-quote\"\n");
        assert_eq!(pairs[0], ("A".to_string(), "double quoted".to_string()));
        assert_eq!(pairs[1], ("B".to_string(), "single".to_string()));
        // No matching pair → kept verbatim, quotes included.
        assert_eq!(pairs[2], ("C".to_string(), "\"unbalanced'".to_string()));
        assert_eq!(pairs[3], ("D".to_string(), "no-quote\"".to_string()));
    }

    #[test]
    fn test_parse_reports_malformed_lines_and_bad_keys() {
        let (pairs, warnings) = parse("GOOD=1\nNO_EQUALS_SIGN\n= empty-key\nBAD KEY=v\n");
        assert_eq!(pairs, vec![("GOOD".to_string(), "1".to_string())]);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("line 2"));
    }

    #[test]
    fn test_parse_value_runs_to_end_of_line_no_inline_comments() {
        // Documented behavior: no inline-comment stripping.
        let (pairs, _) = parse("KEY=value # trailing\n");
        assert_eq!(pairs[0].1, "value # trailing");
    }

    #[test]
    fn test_load_applies_file_values_and_reports_provenance() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("memvault_envfile_applies");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "MEMVAULT_TEST_ENVFILE_FRESH=file-value\n").unwrap();

        let report = load(Some(&path.display().to_string()));
        assert_eq!(report.path.as_deref(), Some(path.as_path()));
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 0);
        assert_eq!(
            std::env::var("MEMVAULT_TEST_ENVFILE_FRESH").unwrap(),
            "file-value"
        );
        assert_eq!(
            provenance("MEMVAULT_TEST_ENVFILE_FRESH"),
            Source::File(path.display().to_string())
        );
        assert_eq!(
            provenance("MEMVAULT_TEST_ENVFILE_NEVER_SET_ANYWHERE"),
            Source::Default
        );

        unsafe {
            std::env::remove_var("MEMVAULT_TEST_ENVFILE_FRESH");
        }
    }

    #[test]
    fn test_load_real_env_wins_over_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("memvault_envfile_precedence");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "MEMVAULT_TEST_ENVFILE_DUAL=file\nMEMVAULT_TEST_ENVFILE_ONLY=file\n",
        )
        .unwrap();

        unsafe {
            std::env::set_var("MEMVAULT_TEST_ENVFILE_DUAL", "env");
        }
        let report = load(Some(&path.display().to_string()));
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(std::env::var("MEMVAULT_TEST_ENVFILE_DUAL").unwrap(), "env");
        assert_eq!(provenance("MEMVAULT_TEST_ENVFILE_DUAL"), Source::Env);
        assert_eq!(
            provenance("MEMVAULT_TEST_ENVFILE_ONLY"),
            Source::File(path.display().to_string())
        );

        unsafe {
            std::env::remove_var("MEMVAULT_TEST_ENVFILE_DUAL");
            std::env::remove_var("MEMVAULT_TEST_ENVFILE_ONLY");
        }
    }

    #[test]
    fn test_load_missing_file_is_silent_noop() {
        let report = load(Some("/tmp/memvault_envfile_no_such_dir_xyz/.env"));
        assert_eq!(report.path, None);
        assert_eq!(report.applied, 0);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn test_resolve_path_prefers_cli_then_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // CLI path wins.
        assert_eq!(
            resolve_path(Some("/tmp/a.env")),
            Some(PathBuf::from("/tmp/a.env"))
        );
        unsafe {
            std::env::set_var("MEMVAULT_ENV_FILE", "~/from-env-var.env");
        }
        let expanded = resolve_path(None).unwrap();
        assert!(
            expanded
                .display()
                .to_string()
                .ends_with("/from-env-var.env")
        );
        unsafe {
            std::env::set_var("MEMVAULT_ENV_FILE", "/abs/from-env-var.env");
        }
        assert_eq!(
            resolve_path(None),
            Some(PathBuf::from("/abs/from-env-var.env"))
        );
        unsafe {
            std::env::remove_var("MEMVAULT_ENV_FILE");
        }
        // Falls back to the default location.
        let fallback = resolve_path(None).unwrap();
        assert!(fallback.display().to_string().ends_with(".env"));
    }

    #[test]
    fn test_default_path_honors_memvault_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("MEMVAULT_HOME", "/custom/memvault-home");
        }
        assert_eq!(
            default_path(),
            Some(PathBuf::from("/custom/memvault-home/.env"))
        );
        unsafe {
            std::env::remove_var("MEMVAULT_HOME");
        }
    }

    #[test]
    fn test_resolve_db_follows_precedence_chain() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("MEMVAULT_DB");
            std::env::remove_var("MEMVAULT_HOME");
        }

        // 1. Explicit --db flag wins, with `~/` expanded.
        assert_eq!(
            resolve_db(Some("/tmp/flag.db")),
            PathBuf::from("/tmp/flag.db")
        );
        assert!(resolve_db(Some("~/flag.db")).is_absolute());

        // 2. MEMVAULT_DB next (real env or env-file value — load() has already
        //    applied file values to process env by the time binaries call this).
        unsafe {
            std::env::set_var("MEMVAULT_DB", "/tmp/env.db");
        }
        assert_eq!(resolve_db(None), PathBuf::from("/tmp/env.db"));
        // An empty/blank value counts as unset and falls through.
        unsafe {
            std::env::set_var("MEMVAULT_DB", "  ");
        }
        unsafe {
            std::env::set_var("MEMVAULT_HOME", "/tmp/mv-home");
        }
        // 3. $MEMVAULT_HOME/data.db.
        assert_eq!(resolve_db(None), PathBuf::from("/tmp/mv-home/data.db"));

        unsafe {
            std::env::remove_var("MEMVAULT_DB");
            std::env::remove_var("MEMVAULT_HOME");
        }
        // 4. Default: ~/.memvault/data.db, expanded via HOME.
        let fallback = resolve_db(None);
        assert!(fallback.is_absolute());
        assert!(fallback.to_string_lossy().ends_with("/.memvault/data.db"));
    }

    #[test]
    fn test_provenance_table_masks_api_keys() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("MEMVAULT_EMBEDDING_API_KEY", "sk-secret-value");
        }
        let table = provenance_table();
        let key_row = table.iter().find(|e| e.key == "MEMVAULT_EMBEDDING_API_KEY");
        assert_eq!(key_row.unwrap().value, "(set, masked)");
        assert_eq!(key_row.unwrap().source, Source::Env);
        assert!(table.iter().any(|e| e.key == "RUST_LOG"));
        assert_eq!(table.len(), CONFIG_KEYS.len());
        unsafe {
            std::env::remove_var("MEMVAULT_EMBEDDING_API_KEY");
        }
    }

    #[test]
    fn test_parse_bool_covers_documented_and_legacy_spellings() {
        for v in ["1", "true", "on", "yes", "enabled", "TRUE", "On"] {
            assert_eq!(parse_bool(v), Some(true), "input {v}");
        }
        for v in ["0", "false", "off", "no", "disabled", "Off", "DISABLED"] {
            assert_eq!(parse_bool(v), Some(false), "input {v}");
        }
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_bool(""), None);
    }
}
