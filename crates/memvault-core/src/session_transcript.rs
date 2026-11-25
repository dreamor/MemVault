//! Host session transcript resolution.
//!
//! Hosts hand MemVault a `transcript_path` in their hook payload, but that
//! path is not stable for the lifetime of the session: Codex Desktop moves
//! rollouts from `sessions/` to `archived_sessions/` when a conversation is
//! archived, while the `SessionEnd` payload keeps referring to the old path.
//! Memorising the payload path only is therefore not enough — a durable
//! compliance job may run after the move, or after the host process is gone.
//!
//! This module keeps the host-specific discovery rules in one agent-agnostic
//! place: given an agent id, the payload path, and the host session id it
//! tries the original path first and then each agent's known transcript
//! roots. Callers (the enqueue hook, the compliance worker) never need to
//! hard-code `.codex` or `.claude` themselves.
//!
//! It also reads a bounded tail snapshot at enqueue time. The snapshot is the
//! primary defence: a job can be judged even if the file is moved or deleted
//! before the worker runs, and no agent-specific behaviour leaks into the
//! worker.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of the transcript tail is snapshotted at enqueue time.
///
/// The judge only reads the last
/// [`MAX_TRANSCRIPT_CHARS`](crate::compliance_judge::MAX_TRANSCRIPT_CHARS)
/// characters (12 000), so 64 KiB of UTF-8 JSONL always covers it with room
/// for multi-byte text. Reading more would bloat every job row for no gain.
pub const DEFAULT_TAIL_BYTES: usize = 64 * 1024;

/// Bound the fallback directory walk. Transcript trees are shallow (Codex:
/// `sessions/YYYY/MM/DD/`, Claude: `projects/<project>/`); this only stops a
/// pathological tree from stalling a hook.
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SCAN_ENTRIES: usize = 4_000;

/// Directories that hold host transcripts. Tests construct these explicitly;
/// production callers use [`TranscriptRoots::from_env`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptRoots {
    /// Native user home (`$HOME`, `%USERPROFILE%`).
    pub user_home: Option<PathBuf>,
    /// Codex config home (`$CODEX_HOME`, else `<user_home>/.codex`).
    pub codex_home: Option<PathBuf>,
    /// Claude Code config home (`$CLAUDE_CONFIG_DIR`, else
    /// `<user_home>/.claude`).
    pub claude_config_dir: Option<PathBuf>,
}

impl TranscriptRoots {
    /// Resolve roots from the process environment. Missing variables and an
    /// unset user home simply leave the corresponding root `None`; every
    /// resolution path treats that as "not discoverable", never as an error.
    pub fn from_env() -> Self {
        let user_home = env_path("HOME").or_else(|| env_path("USERPROFILE"));
        let codex_home =
            env_path("CODEX_HOME").or_else(|| user_home.as_ref().map(|h| h.join(".codex")));
        let claude_config_dir =
            env_path("CLAUDE_CONFIG_DIR").or_else(|| user_home.as_ref().map(|h| h.join(".claude")));
        Self {
            user_home,
            codex_home,
            claude_config_dir,
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolve the current on-disk path of a host transcript.
///
/// The original payload path wins when it still exists, so the common case
/// does no directory scanning. Otherwise:
///
/// * `codex` — `<codex_home>/archived_sessions/<basename>`, then a bounded
///   walk of `<codex_home>/sessions` matching the rollout filename or the
///   session id suffix.
/// * `claude` / `claude-code` — a bounded walk of
///   `<claude_config_dir>/projects` matching `<session_id>.jsonl`.
/// * any other agent — `None` unless the original path exists.
///
/// Returns `None` rather than guessing when nothing matches.
pub fn resolve_transcript_path(
    agent_id: &str,
    transcript_path: Option<&str>,
    session_id: Option<&str>,
) -> Option<PathBuf> {
    resolve_with_roots(
        agent_id,
        transcript_path,
        session_id,
        &TranscriptRoots::from_env(),
    )
}

/// Testable form of [`resolve_transcript_path`] with explicit roots.
pub fn resolve_with_roots(
    agent_id: &str,
    transcript_path: Option<&str>,
    session_id: Option<&str>,
    roots: &TranscriptRoots,
) -> Option<PathBuf> {
    let original = transcript_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(path) = original.as_ref().filter(|path| path.is_file()) {
        return Some(path.clone());
    }

    let session_id = session_id.map(str::trim).filter(|id| !id.is_empty());
    let basename = original
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_os_string());
    if basename.is_none() && session_id.is_none() {
        return None;
    }

    match agent_id.trim().to_ascii_lowercase().as_str() {
        "codex" => {
            let root = roots.codex_home.as_ref()?;
            let archived = root.join("archived_sessions");
            if let Some(path) = match_in_tree(&archived, basename.as_deref(), session_id, "codex") {
                return Some(path);
            }
            match_in_tree(
                &root.join("sessions"),
                basename.as_deref(),
                session_id,
                "codex",
            )
        }
        "claude" | "claude-code" => {
            let root = roots.claude_config_dir.as_ref()?;
            match_in_tree(
                &root.join("projects"),
                basename.as_deref(),
                session_id,
                "claude",
            )
        }
        _ => None,
    }
}

/// Read a UTF-8-safe tail snapshot of `path`.
///
/// When the file is longer than `max_bytes` the cut lands in the middle of a
/// line, so everything before the first newline is dropped: the judge must
/// never receive the tail half of a JSONL row and interpret it as history.
/// A file with no newline in the retained window yields `None` (there is no
/// complete evidence row to keep). Empty files and read errors also yield
/// `None`.
pub fn read_tail(path: &Path, max_bytes: usize) -> Option<String> {
    if max_bytes == 0 {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start > 0 {
        let newline = text.find('\n')?;
        &text[newline + 1..]
    } else {
        &text[..]
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(text.to_string())
}

fn match_in_tree(
    root: &Path,
    basename: Option<&std::ffi::OsStr>,
    session_id: Option<&str>,
    agent: &str,
) -> Option<PathBuf> {
    let mut matches = Vec::new();
    let mut budget = MAX_SCAN_ENTRIES;
    collect_matches(
        root,
        0,
        basename,
        session_id,
        agent,
        &mut budget,
        &mut matches,
    );
    matches
        .into_iter()
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path)
}

fn collect_matches(
    dir: &Path,
    depth: usize,
    basename: Option<&std::ffi::OsStr>,
    session_id: Option<&str>,
    agent: &str,
    budget: &mut usize,
    out: &mut Vec<(SystemTime, PathBuf)>,
) {
    if depth > MAX_SCAN_DEPTH || *budget == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = entry.path();
        if path.is_file() {
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let name_matches = basename.is_some_and(|name| path.file_name() == Some(name));
            let id_matches = session_id.is_some_and(|id| session_id_matches(&path, id, agent));
            if name_matches || id_matches {
                let mtime = path
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((mtime, path));
            }
        } else if path.is_dir() {
            collect_matches(&path, depth + 1, basename, session_id, agent, budget, out);
        }
    }
}

fn session_id_matches(path: &Path, session_id: &str, agent: &str) -> bool {
    let Some(stem) = path.file_stem().map(|stem| stem.to_string_lossy()) else {
        return false;
    };
    match agent {
        // Codex rollouts are `rollout-<timestamp>-<thread-id>.jsonl`; the
        // thread id is the SessionEnd `session_id`, so a suffix match finds
        // the file even after a rename/move that changed its directory.
        "codex" => stem.ends_with(session_id),
        // Claude Code names the file exactly `<session-id>.jsonl`.
        _ => stem == session_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault-transcript-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn roots(home: &Path) -> TranscriptRoots {
        TranscriptRoots {
            user_home: Some(home.to_path_buf()),
            codex_home: Some(home.join(".codex")),
            claude_config_dir: Some(home.join(".claude")),
        }
    }

    #[test]
    fn keeps_an_existing_original_path_without_scanning() {
        let home = temp_dir("existing");
        let transcript = home.join("anywhere").join("rollout.jsonl");
        write(&transcript, "{}\n");

        let resolved = resolve_with_roots(
            "codex",
            Some(transcript.to_string_lossy().as_ref()),
            Some("sess-1"),
            &roots(&home),
        );
        assert_eq!(resolved, Some(transcript));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn finds_a_codex_rollout_moved_to_archived_sessions() {
        let home = temp_dir("codex-archived");
        let original = home
            .join(".codex")
            .join("sessions")
            .join("2026/11/25")
            .join("rollout-2026-11-25T15-11-18-sess-abc.jsonl");
        write(&original, "{}\n");
        let archived = home
            .join(".codex")
            .join("archived_sessions")
            .join(original.file_name().unwrap());
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::rename(&original, &archived).unwrap();

        let resolved = resolve_with_roots(
            "codex",
            Some(original.to_string_lossy().as_ref()),
            Some("sess-abc"),
            &roots(&home),
        );
        assert_eq!(resolved, Some(archived));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn finds_a_codex_rollout_by_session_id_in_the_dated_tree() {
        let home = temp_dir("codex-session-id");
        let rollout = home
            .join(".codex")
            .join("sessions")
            .join("2026/11/25")
            .join("rollout-2026-11-25T15-11-18-sess-xyz.jsonl");
        write(&rollout, "{}\n");

        // The payload path is gone and its basename no longer exists, but the
        // session id suffix still identifies the rollout.
        let resolved = resolve_with_roots(
            "codex",
            Some("/gone/rollout-old.jsonl"),
            Some("sess-xyz"),
            &roots(&home),
        );
        assert_eq!(resolved, Some(rollout));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn finds_a_claude_code_session_by_id() {
        let home = temp_dir("claude");
        let transcript = home
            .join(".claude")
            .join("projects")
            .join("-Users-demo-project")
            .join("sess-claude.jsonl");
        write(&transcript, "{}\n");

        let resolved = resolve_with_roots(
            "claude-code",
            Some("/moved/away/sess-claude.jsonl"),
            Some("sess-claude"),
            &roots(&home),
        );
        assert_eq!(resolved, Some(transcript));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn unknown_agents_and_unmatched_sessions_yield_none() {
        let home = temp_dir("unknown");
        let roots = roots(&home);
        assert_eq!(
            resolve_with_roots("cursor", Some("/gone/a.jsonl"), Some("sess-1"), &roots),
            None
        );
        assert_eq!(
            resolve_with_roots("codex", Some("/gone/a.jsonl"), Some("missing"), &roots),
            None
        );
        assert_eq!(resolve_with_roots("codex", None, None, &roots), None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn read_tail_drops_the_partial_first_line() {
        let home = temp_dir("tail-partial");
        let transcript = home.join("rollout.jsonl");
        let long = format!(
            "{}\n{}\n{}\n",
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64)
        );
        write(&transcript, &long);

        let tail = read_tail(&transcript, 80).unwrap();
        assert!(tail.ends_with(&format!("{}\n", "c".repeat(64))));
        assert!(!tail.contains(&"b".repeat(64)));
        assert!(tail.lines().all(|line| line.len() == 64));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn read_tail_keeps_small_files_and_rejects_empty_ones() {
        let home = temp_dir("tail-small");
        let transcript = home.join("rollout.jsonl");
        write(&transcript, "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(
            read_tail(&transcript, DEFAULT_TAIL_BYTES).as_deref(),
            Some("{\"a\":1}\n{\"b\":2}\n")
        );

        write(&transcript, "");
        assert_eq!(read_tail(&transcript, DEFAULT_TAIL_BYTES), None);
        assert_eq!(read_tail(&transcript, 0), None);
        std::fs::remove_dir_all(home).ok();
    }
}
