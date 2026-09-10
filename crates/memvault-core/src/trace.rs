//! Trace ingestion: turn agent session transcripts into L0 raw-evidence
//! memories plus distilled candidates, advancing a per-(agent, session)
//! watermark so a crashed or re-run ingestion resumes where it left off.
//!
//! Design (L0-evidence chain):
//! - Every transcript *turn* that produces an extraction signal is stored
//!   first as an **L0 evidence row**: the raw turn text (tail-capped at
//!   [`MAX_TURN_EVIDENCE_BYTES`]), in namespace `trace:<agent_key>`, tagged
//!   [`TRACE_SOURCE_TAG`], marked `human_reviewed = true` (raw logs need no
//!   human review — they are evidence, not conclusions) and
//!   `ai_generated = true`.
//! - Each rule-extracted candidate distilled from that turn is then stored
//!   with `source_trace_ids = [<evidence id>]` so the provenance chain stays
//!   restorable: a reviewer can walk from a distilled candidate back to the
//!   exact raw turn that produced it.
//! - Candidates follow the normal layer/namespace derivation (auto layer from
//!   priority, `global` namespace) and land in the human review inbox
//!   (`human_reviewed = false`) unless `approve` is set.
//!
//! Bounded evidence retention: only turns that yielded an extraction signal
//! become evidence. A turn with no signal is watermark-consumed but stored
//! nowhere — transcripts are append-only, so a signalless turn will never
//! signal later, and keeping every turn would balloon the store.
//!
//! Watermark semantics: [`TraceWatermark::last_seq`] is the 0-based *line*
//! index of the last processed transcript turn (see
//! [`crate::transcript::parse_turns`]). Turns with `seq <= last_seq` are
//! skipped; the watermark advances only after a session finished processing
//! successfully, and never in dry-run mode. A mid-session storage error
//! propagates (`?`) before the watermark write, so a failed run can never
//! advance past turns it did not persist.
//!
//! Robustness contract: a session file that cannot be read, or whose content
//! is not a recognized JSONL transcript, is skipped without counting — a
//! hook/CLI must never hard-fail on one corrupt file. Storage failures,
//! however, propagate: partial ingestion must be visible, not silently
//! swallowed. Turns whose text trips the [`crate::sensitive`] content guard
//! are skipped entirely (never persisted, never extracted).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::Utc;

use crate::error::{MemVaultError, Result};
use crate::extractor::Extractor;
use crate::models::{Memory, MemoryLayer, Priority, SourceAgent, TraceWatermark};
use crate::storage::MemoryStore;
use crate::transcript::{TraceTurn, parse_turns};

/// Tag applied to both L0 evidence rows and distilled candidates so trace
/// ingestion output is filterable in one query.
pub const TRACE_SOURCE_TAG: &str = "source:trace";

/// L0 evidence rows live in namespace `trace:<agent_key>` — one namespace per
/// source agent keeps raw evidence out of the retrieval namespaces while
/// remaining addressable for audit.
pub const TRACE_NAMESPACE_PREFIX: &str = "trace:";

/// Per-turn evidence cap (16 KiB). A single turn rarely exceeds this; the cap
/// is a safety valve so one pathological paste cannot balloon the store. The
/// *tail* is kept — the most recent text carries the strongest signals,
/// mirroring [`crate::transcript`]. The same bounded text feeds the extractor
/// and the stored evidence row so both describe the same bytes.
const MAX_TURN_EVIDENCE_BYTES: usize = 16 * 1024;

/// Default cap on how many session files one run discovers/processes
/// (newest-by-mtime first). [`IngestOptions::default`] points here.
const DEFAULT_MAX_SESSIONS: usize = 20;

/// Filesystem recursion depth when probing a transcript root. Claude nests
/// one level (`projects/<slug>/<uuid>.jsonl`); Codex may nest by date
/// (`sessions/YYYY/MM/DD/…`); the cap keeps a hostile layout bounded.
const MAX_SCAN_DEPTH: usize = 4;

/// Source agents whose session transcripts trace ingestion understands.
static SUPPORTED_AGENTS: [&str; 3] = ["claude", "codex", "hermes"];

/// Supported source-agent keys (validate `agent_key` against this before
/// probing the filesystem).
pub fn supported_agent_keys() -> &'static [&'static str] {
    &SUPPORTED_AGENTS
}

/// One discovered session transcript file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSession {
    pub agent_key: &'static str,
    /// Session id = transcript file stem (e.g. the Claude Code session uuid).
    pub session_id: String,
    pub path: PathBuf,
}

/// Options for one trace-ingestion run.
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Directory treated as the agent home (`~`) when probing transcript
    /// directories. Tests point this at a temp dir; the CLI passes the real
    /// home. Default: `$HOME`, falling back to `.` when unset.
    pub home: PathBuf,
    /// Mark distilled candidates `human_reviewed = true` (skip the review
    /// inbox). L0 evidence rows are always pre-approved — they are raw logs.
    pub approve: bool,
    /// Count what WOULD be saved and write nothing — no memories, no
    /// watermarks.
    pub dry_run: bool,
    /// At most this many sessions are processed per run (newest first).
    pub max_sessions: usize,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            home: default_home(),
            approve: false,
            dry_run: false,
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }
}

/// Counts reported by one [`ingest_for_agent`] run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestStats {
    /// Sessions whose transcript was read and recognized.
    pub sessions_scanned: usize,
    /// Turns encountered past the watermark (new in this run), whether or
    /// not they later produced evidence.
    pub turns_new: usize,
    /// Turns skipped because their seq was already consumed (`<= last_seq`).
    pub turns_skipped: usize,
    /// L0 raw-evidence rows saved (counted even in dry-run).
    pub evidence_saved: usize,
    /// Distilled candidates saved (counted even in dry-run).
    pub candidates_saved: usize,
}

/// Discover the newest session transcripts for one agent under `home`.
///
/// - `claude` → `~/.claude/projects/*/<session-id>.jsonl`
/// - `codex`  → `~/.codex/sessions/**/<session-id>.jsonl`
/// - `hermes` → best-effort `~/.hermes/sessions/**/<session-id>.jsonl`; the
///   Hermes adapter (see [`crate::agent_import::hermes`]) has no reliable
///   session-transcript convention, so this normally yields empty.
///
/// Read-only and never panics: a missing directory, an unknown agent key, or
/// an unreadable subtree all yield an empty vec. Results are newest-by-mtime
/// first, capped at `limit` (a `limit` of 0 returns nothing).
pub fn discover_sessions(agent_key: &str, home: &Path, limit: usize) -> Vec<TraceSession> {
    // Unknown keys are not discoverable — not an error either.
    let agent_key: &'static str = match agent_key {
        "claude" => "claude",
        "codex" => "codex",
        "hermes" => "hermes",
        _ => return Vec::new(),
    };

    let root = transcript_root(agent_key, home);
    let mut found = Vec::new();
    collect_jsonl(&root, MAX_SCAN_DEPTH, &mut found);

    // Newest first: the most recent sessions carry the freshest signals, and
    // the per-run cap prefers them.
    let mut by_mtime: Vec<(SystemTime, PathBuf)> = found
        .into_iter()
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    by_mtime.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    by_mtime.truncate(limit);

    by_mtime
        .into_iter()
        .map(|(_, path)| TraceSession {
            agent_key,
            session_id: path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            path,
        })
        .collect()
}

/// The transcript root for one agent under `home`.
fn transcript_root(agent_key: &str, home: &Path) -> PathBuf {
    match agent_key {
        "claude" => home.join(".claude").join("projects"),
        "codex" => home.join(".codex").join("sessions"),
        _ => home.join(".hermes").join("sessions"),
    }
}

/// Collect `*.jsonl` files under `dir` (recursing up to `depth` levels).
/// Missing/unreadable directories contribute nothing.
fn collect_jsonl(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push(path);
            }
        } else if path.is_dir() && depth > 0 {
            collect_jsonl(&path, depth - 1, out);
        }
    }
}

/// Ingest new turns of every discovered session for `agent_key`.
///
/// Pipeline per session file:
/// 1. Read + [`parse_turns`]; a read error or an unrecognized (non-transcript)
///    file is skipped without counting.
/// 2. Load the (agent, session) watermark; turns with `seq <= last_seq` are
///    already consumed and skipped.
/// 3. Per new turn: join segments, trim, and skip turns that are empty,
///    contain sensitive (credential-shaped) content, or yield no extraction
///    signal — the "bounded evidence retention" rule.
/// 4. Save the L0 evidence row first, then one candidate per extracted
///    memory, each pointing its `source_trace_ids` at that evidence row.
/// 5. After the whole session processed successfully, advance the watermark
///    to the last processed turn's seq (never in dry-run). A storage error
///    earlier returns before this write, so the watermark cannot advance past
///    unprocessed turns.
///
/// File-level problems are tolerated (counted/skipped); storage failures are
/// hard errors so partial ingestion is visible. Dedup is deliberately NOT
/// run here — the review inbox and later dedup/promote cycles own that.
pub async fn ingest_for_agent(
    store: &(impl MemoryStore + ?Sized),
    agent_key: &str,
    opts: &IngestOptions,
) -> Result<IngestStats> {
    if !supported_agent_keys().contains(&agent_key) {
        return Err(MemVaultError::InvalidInput(format!(
            "unsupported agent key for trace ingestion: {agent_key:?} \
             (supported: {supported:?})",
            supported = supported_agent_keys()
        )));
    }

    let mut stats = IngestStats::default();
    for session in discover_sessions(agent_key, &opts.home, opts.max_sessions) {
        ingest_session(store, agent_key, &session, opts, &mut stats).await?;
    }
    Ok(stats)
}

/// Read, parse, and ingest one session transcript, updating `stats`.
///
/// Returns `Ok(())` for every file-level problem (unreadable, unrecognized);
/// only storage errors propagate.
async fn ingest_session(
    store: &(impl MemoryStore + ?Sized),
    agent_key: &str,
    session: &TraceSession,
    opts: &IngestOptions,
    stats: &mut IngestStats,
) -> Result<()> {
    // A corrupt/unreadable session file must never hard-fail the run.
    let Ok(raw) = std::fs::read_to_string(&session.path) else {
        tracing::warn!(
            agent_key = %agent_key,
            session_id = %session.session_id,
            path = %session.path.display(),
            "trace ingestion: could not read session transcript; skipping"
        );
        return Ok(());
    };

    let parsed = parse_turns(&raw);
    if !parsed.recognized {
        // Not a JSONL transcript (or empty) — nothing to consume.
        tracing::debug!(
            agent_key = %agent_key,
            session_id = %session.session_id,
            "trace ingestion: session file is not a recognized transcript; skipping"
        );
        return Ok(());
    }
    stats.sessions_scanned += 1;

    let watermark = store
        .get_trace_watermark(agent_key, &session.session_id)
        .await?;
    let prior_last_seq = watermark.as_ref().map(|w| w.last_seq);
    let new_turns: Vec<&TraceTurn> = parsed
        .turns
        .iter()
        .filter(|t| prior_last_seq.is_none_or(|seq| t.seq > seq))
        .collect();
    stats.turns_skipped += parsed.turns.len() - new_turns.len();
    stats.turns_new += new_turns.len();

    // `last_seq` is the max seq actually processed; with nothing new it stays
    // at the prior value (and no watermark exists to fall back to only when
    // the file had no turns at all, which `recognized` already ruled out).
    let last_processed_seq = match (new_turns.last(), prior_last_seq) {
        (Some(turn), _) => turn.seq,
        (None, Some(seq)) => seq,
        (None, None) => return Ok(()),
    };

    let source_agent = SourceAgent {
        id: format!("trace:{agent_key}"),
        agent_type: agent_key.to_string(),
        session_id: Some(session.session_id.clone()),
    };

    let session_turns_new = new_turns.len();
    let mut session_evidence = 0;
    let mut session_candidates = 0;
    for turn in new_turns {
        let (evidence, candidates) =
            process_turn(store, agent_key, turn, &source_agent, opts, stats).await?;
        session_evidence += evidence;
        session_candidates += candidates;
    }

    // Whole session processed: persist the watermark so a later run resumes
    // past it. Never in dry-run — dry-run must leave no trace.
    if !opts.dry_run {
        store
            .set_trace_watermark(&TraceWatermark {
                agent_key: agent_key.to_string(),
                session_id: session.session_id.clone(),
                last_seq: last_processed_seq,
                updated_at: Utc::now(),
            })
            .await?;
    }

    tracing::info!(
        agent_key = %agent_key,
        session_id = %session.session_id,
        turns_new = session_turns_new,
        evidence = session_evidence,
        candidates = session_candidates,
        "trace ingestion: session processed"
    );
    Ok(())
}

/// Ingest one turn. Returns `(evidence_saved, candidates_saved)` for this
/// turn (0 for turns with no signal, empty text, or sensitive content).
async fn process_turn(
    store: &(impl MemoryStore + ?Sized),
    agent_key: &str,
    turn: &TraceTurn,
    source_agent: &SourceAgent,
    opts: &IngestOptions,
    stats: &mut IngestStats,
) -> Result<(usize, usize)> {
    let text = turn.segments.join("\n");
    let text = text.trim();
    if text.is_empty() {
        return Ok((0, 0));
    }
    // The content guard runs on the WHOLE turn before any truncation, so a
    // secret anywhere in it disqualifies the turn, not just one in the kept
    // tail. When in doubt, skip.
    if crate::sensitive::is_sensitive(text) {
        tracing::debug!(
            agent_key = %agent_key,
            session_id = %source_agent.session_id.as_deref().unwrap_or_default(),
            seq = turn.seq,
            "trace ingestion: turn skipped (sensitive content)"
        );
        return Ok((0, 0));
    }

    let bounded = cap_tail(text, MAX_TURN_EVIDENCE_BYTES);
    let outcome = Extractor::extract_with_coverage(&bounded);
    if outcome.memories.is_empty() {
        // Bounded evidence retention: a signalless turn becomes no evidence
        // block. It is still watermark-consumed — append-only transcripts
        // mean it will never signal later.
        return Ok((0, 0));
    }

    // L0 raw-evidence row FIRST so candidates can point back at it. Its type
    // mirrors the first distilled candidate (raw evidence of a preference
    // reads as a preference, not a generic fact).
    let first_type = outcome.memories[0].memory_type.clone();
    let mut evidence = Memory::new(
        first_type,
        bounded,
        Priority::Background,
        source_agent.clone(),
    );
    evidence.namespace = format!("{TRACE_NAMESPACE_PREFIX}{agent_key}");
    evidence.layer = MemoryLayer::L0;
    evidence.human_reviewed = true; // raw logs need no human review
    evidence.ai_generated = true;
    evidence.tags = vec![
        TRACE_SOURCE_TAG.to_string(),
        format!("{TRACE_NAMESPACE_PREFIX}{agent_key}"),
    ];
    stats.evidence_saved += 1;
    let evidence_id = if opts.dry_run {
        evidence.id.clone()
    } else {
        store.save(evidence).await?.id
    };

    let mut candidates = 0;
    for extracted in outcome.memories {
        let mut candidate = Memory::new(
            extracted.memory_type,
            extracted.content,
            extracted.priority,
            source_agent.clone(),
        );
        candidate.instruction = extracted.instruction;
        candidate.tags = extracted.tags;
        if !candidate.tags.iter().any(|t| t == TRACE_SOURCE_TAG) {
            candidate.tags.push(TRACE_SOURCE_TAG.to_string());
        }
        candidate.confidence = extracted.confidence;
        candidate.source_trace_ids = vec![evidence_id.clone()];
        candidate.human_reviewed = opts.approve;
        candidates += 1;
        stats.candidates_saved += 1;
        if !opts.dry_run {
            store.save(candidate).await?;
        }
    }
    Ok((1, candidates))
}

/// The process home dir: `$HOME`, falling back to `.` when unset.
fn default_home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Keep the tail of `input` within `max_bytes`, nudging the cut to a UTF-8
/// char boundary so the kept text stays valid. A `…` prefix marks the cut so
/// a truncated evidence row can never be mistaken for the full turn.
fn cap_tail(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    let mut start = input.len() - max_bytes;
    while start < input.len() && !input.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &input[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    fn tmp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_trace_{tag}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Path of a Claude Code session transcript under a (fake) home:
    /// `~/.claude/projects/<slug>/<session-id>.jsonl`.
    fn claude_session_path(home: &Path, session_id: &str) -> PathBuf {
        home.join(".claude")
            .join("projects")
            .join("test-slug")
            .join(format!("{session_id}.jsonl"))
    }

    fn write_session(path: &Path, lines: &[&str]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut raw = String::new();
        for line in lines {
            raw.push_str(line);
            raw.push('\n');
        }
        std::fs::write(path, raw).unwrap();
    }

    fn default_opts(home: PathBuf) -> IngestOptions {
        IngestOptions {
            home,
            ..IngestOptions::default()
        }
    }

    #[test]
    fn discover_finds_claude_session_and_returns_empty_for_others() {
        let home = tmp_home("discover");
        let path = claude_session_path(&home, "sess-123");
        write_session(
            &path,
            &[r#"{"type":"user","message":{"content":"I always prefer dark mode"}}"#],
        );

        let sessions = discover_sessions("claude", &home, 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_key, "claude");
        assert_eq!(sessions[0].session_id, "sess-123");
        assert_eq!(sessions[0].path, path);

        // Hermes has no upstream session-transcript convention; unknown keys
        // are equally un-discoverable. Neither is an error or a panic.
        assert!(discover_sessions("hermes", &home, 10).is_empty());
        assert!(discover_sessions("no-such-agent", &home, 10).is_empty());
        // A limit of 0 discovers nothing.
        assert!(discover_sessions("claude", &home, 0).is_empty());

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn discover_missing_dir_returns_empty() {
        let home = tmp_home("discover-empty");
        assert!(discover_sessions("claude", &home, 10).is_empty());
        assert!(discover_sessions("codex", &home, 10).is_empty());
        assert!(discover_sessions("hermes", &home, 10).is_empty());
        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn first_run_saves_evidence_and_candidates_with_provenance() {
        let home = tmp_home("pipeline");
        let session_id = "sess-full";
        let path = claude_session_path(&home, session_id);
        // Three turns; only turn 0 carries an extraction signal, so it (and
        // only it) becomes evidence.
        write_session(
            &path,
            &[
                r#"{"type":"user","message":{"content":"I always prefer dark mode"}}"#,
                r#"{"type":"user","message":{"content":"thanks"}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Understood."}]}}"#,
            ],
        );

        let store = SqliteStore::in_memory().unwrap();
        let stats = ingest_for_agent(&store, "claude", &default_opts(home.clone()))
            .await
            .unwrap();

        assert_eq!(stats.sessions_scanned, 1);
        assert_eq!(stats.turns_new, 3);
        assert_eq!(stats.turns_skipped, 0);
        assert_eq!(
            stats.evidence_saved, 1,
            "only the signal turn becomes evidence"
        );
        assert!(
            stats.candidates_saved >= 1,
            "the signal turn yields a candidate"
        );

        let all = store.list(None, 100, 0).await.unwrap();
        assert_eq!(
            all.len() as usize,
            stats.evidence_saved + stats.candidates_saved
        );

        let evidence = all
            .iter()
            .find(|m| m.layer == MemoryLayer::L0)
            .expect("evidence row exists");
        assert_eq!(evidence.priority, Priority::Background);
        assert_eq!(evidence.namespace, "trace:claude");
        assert!(evidence.human_reviewed, "raw logs are pre-approved");
        assert!(evidence.ai_generated);
        assert!(evidence.content.contains("dark mode"));
        assert!(evidence.tags.contains(&TRACE_SOURCE_TAG.to_string()));
        assert!(evidence.tags.contains(&"trace:claude".to_string()));

        let candidate = all
            .iter()
            .find(|m| m.layer != MemoryLayer::L0)
            .expect("candidate exists");
        assert!(
            !candidate.source_trace_ids.is_empty(),
            "provenance is recorded"
        );
        assert_eq!(
            candidate.source_trace_ids,
            vec![evidence.id.clone()],
            "candidate provenance points at the saved L0 evidence row"
        );
        assert!(
            !candidate.human_reviewed,
            "approve=false lands in the review inbox"
        );
        assert!(candidate.tags.contains(&TRACE_SOURCE_TAG.to_string()));

        // Watermark advanced past the last processed turn (line index 2).
        let wm = store
            .get_trace_watermark("claude", session_id)
            .await
            .unwrap()
            .expect("watermark written");
        assert_eq!(wm.last_seq, 2);

        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn second_run_is_watermarked_and_writes_nothing() {
        let home = tmp_home("idem");
        let session_id = "sess-idem";
        let path = claude_session_path(&home, session_id);
        write_session(
            &path,
            &[
                r#"{"type":"user","message":{"content":"I prefer vim over emacs"}}"#,
                r#"{"type":"user","message":{"content":"thanks"}}"#,
                r#"{"type":"assistant","message":{"content":"ok"}}"#,
            ],
        );

        let store = SqliteStore::in_memory().unwrap();
        let opts = default_opts(home.clone());

        let first = ingest_for_agent(&store, "claude", &opts).await.unwrap();
        assert!(first.evidence_saved >= 1);
        let rows1 = store.list(None, 100, 0).await.unwrap().len();

        let second = ingest_for_agent(&store, "claude", &opts).await.unwrap();
        assert_eq!(second.turns_new, 0, "all turns already consumed");
        assert_eq!(second.turns_skipped, 3);
        assert_eq!(second.evidence_saved, 0);
        assert_eq!(second.candidates_saved, 0);
        assert_eq!(
            store.list(None, 100, 0).await.unwrap().len(),
            rows1,
            "second run wrote no rows"
        );

        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn dry_run_counts_but_writes_nothing() {
        let home = tmp_home("dry-run");
        let session_id = "sess-dry";
        let path = claude_session_path(&home, session_id);
        write_session(
            &path,
            &[
                r#"{"type":"user","message":{"content":"I prefer dark mode"}}"#,
                r#"{"type":"user","message":{"content":"thanks"}}"#,
                r#"{"type":"assistant","message":{"content":"ok"}}"#,
            ],
        );

        let store = SqliteStore::in_memory().unwrap();
        let opts = IngestOptions {
            home: home.clone(),
            dry_run: true,
            ..IngestOptions::default()
        };
        let stats = ingest_for_agent(&store, "claude", &opts).await.unwrap();

        assert_eq!(
            stats.evidence_saved, 1,
            "dry-run still counts what WOULD save"
        );
        assert!(stats.candidates_saved >= 1);
        assert!(
            store.list(None, 100, 0).await.unwrap().is_empty(),
            "dry-run wrote no rows"
        );
        assert!(
            store
                .get_trace_watermark("claude", session_id)
                .await
                .unwrap()
                .is_none(),
            "dry-run never writes watermarks"
        );

        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn sensitive_turn_is_never_persisted() {
        let home = tmp_home("sensitive");
        let path = claude_session_path(&home, "sess-secret");
        // "I always …" would extract as a preference — but the credential
        // pattern must disqualify the whole turn before extraction.
        write_session(
            &path,
            &[
                r#"{"type":"user","message":{"content":"I always rotate api_key = sk-1234567890abcdef1234567890"}}"#,
            ],
        );

        let store = SqliteStore::in_memory().unwrap();
        let stats = ingest_for_agent(&store, "claude", &default_opts(home.clone()))
            .await
            .unwrap();

        assert_eq!(stats.turns_new, 1);
        assert_eq!(stats.evidence_saved, 0);
        assert_eq!(stats.candidates_saved, 0);
        assert!(store.list(None, 100, 0).await.unwrap().is_empty());

        std::fs::remove_dir_all(&home).ok();
    }

    #[tokio::test]
    async fn unknown_agent_key_is_an_invalid_input_error() {
        let store = SqliteStore::in_memory().unwrap();
        let err = ingest_for_agent(&store, "not-an-agent", &IngestOptions::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, MemVaultError::InvalidInput(_)),
            "unknown agent key must be InvalidInput, got: {err:?}"
        );
    }
}
