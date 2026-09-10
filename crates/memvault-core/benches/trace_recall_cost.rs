//! Offline half of the P3 handoff-vs-recall cost benchmark.
//!
//! The full protocol (see `docs/experiments/TRACE-RECALL-BENCH.md`) compares
//! weighted tokens per successful task across three channels:
//!
//! 1. handoff   — agent writes a handoff doc, a new session continues from it
//! 2. compaction — a long session is compacted and continued
//! 3. recall    — MemVault `session_start` injection + `get_memory_evidence`
//!
//! Channels 1 and 2 need a real LLM (and a judge) to produce/compress the
//! handoff text, so they cannot be measured here. This bench measures the
//! only offline-measurable part of channel 3: on a deterministic synthetic
//! transcript it reports
//!
//! - ingest throughput (turns per session ingested), and
//! - the recall payload size (bytes/tokens of injected memory text) versus
//!   the full-transcript size, so the compression ratio is visible.
//!
//! ## Token accounting
//!
//! Tokens are counted with [`MemoryRouter::estimate_tokens`], the repo's
//! existing helper (`router/format.rs`): ASCII counts as ~4 chars/token and
//! CJK as ~1.5 chars/token. For ASCII text this is exactly `bytes / 4`, the
//! same rough approximation funes' benchmark uses; we reuse the helper rather
//! than inventing a second estimator.
//!
//! ## Determinism
//!
//! The transcript content is a fixed constant; no wall-clock dates or random
//! data feed the measurements or any assertion. Only the temp directory name
//! varies (by process id) and it does not affect the numbers. Criterion owns
//! all timing.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::{Path, PathBuf};

use memvault_core::config::default_agent_registry;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::trace::{IngestOptions, ingest_for_agent};
use memvault_core::transcript::transcript_to_text;

/// Turns that carry extractor-detectable signals (stated preferences, an
/// identity, a standing instruction) — these become L0 evidence, not filler.
const SIGNAL_TURNS: [&str; 4] = [
    r#"{"type":"user","message":{"content":"I always prefer dark mode when editing code"}}"#,
    r#"{"type":"user","message":{"content":"I prefer vim over emacs for quick edits"}}"#,
    r#"{"type":"user","message":{"content":"I am a backend engineer working on Rust services"}}"#,
    r#"{"type":"user","message":{"content":"Always run cargo fmt before committing changes"}}"#,
];

/// Total turns in the synthetic session. Only a few carry signals; the rest
/// are signalless filler so the session is long enough for the recall-vs-
/// transcript ratio to be meaningful, and so ingest exercises the "bounded
/// evidence retention" branch (signalless turns are consumed, not stored).
const FILLER_TURNS: usize = 40;

/// A turn with no extractor trigger words, alternating speaker.
fn filler_turn(i: usize) -> String {
    if i % 2 == 0 {
        format!(
            r#"{{"type":"user","message":{{"content":"Continuing the implementation of component {i}."}}}}"#
        )
    } else {
        r#"{"type":"assistant","message":{"content":"Sounds good, moving on."}}"#.to_string()
    }
}

/// Build the synthetic session: a signal turn every 10th slot, filler between.
/// Deterministic — no randomness or wall-clock input.
fn synthetic_raw() -> String {
    let mut raw = String::new();
    let mut signals = SIGNAL_TURNS.iter();
    for i in 0..FILLER_TURNS {
        let turn = if i % 10 == 0 {
            signals
                .next()
                .map_or_else(|| filler_turn(i), |s| (*s).to_string())
        } else {
            filler_turn(i)
        };
        raw.push_str(&turn);
        raw.push('\n');
    }
    raw
}

/// Write the synthetic transcript to a home dir under `base` in the layout
/// `claude` discovery expects: `~/.claude/projects/<slug>/<session>.jsonl`.
/// Each `n` gets a distinct session id, so a shared store never sees a
/// consumed watermark — no per-iteration store/pool is needed.
fn fixture_home(base: &Path, n: usize) -> PathBuf {
    let home = base.join(format!("h{n}"));
    let dir = home.join(".claude").join("projects").join("bench-slug");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("bench-session-{n}.jsonl")),
        synthetic_raw(),
    )
    .unwrap();
    home
}

/// A process-unique parent dir for fixtures (never affects the numbers).
fn fixture_base(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("memvault_trace_bench_{tag}_{}", std::process::id()))
}

/// Ingest the fixture once, assert it actually produced survivors, and return
/// a router over the resulting store. `approve: true` marks distilled
/// candidates reviewed so recall treats them as curated memories.
fn ingested_router(home: &Path) -> MemoryRouter {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = SqliteStore::in_memory().unwrap();
    let opts = IngestOptions {
        home: home.to_path_buf(),
        approve: true,
        ..IngestOptions::default()
    };
    let stats = rt
        .block_on(ingest_for_agent(&store, "claude", &opts))
        .unwrap();
    assert!(
        stats.evidence_saved >= 1,
        "fixture must yield at least one signal turn, got {stats:?}"
    );
    assert!(
        stats.candidates_saved >= 1,
        "fixture must yield at least one distilled candidate, got {stats:?}"
    );
    MemoryRouter::with_registry(std::sync::Arc::new(store), default_agent_registry())
}

/// Print the channel-3 offline ratio (injected recall payload vs. the full
/// transcript). Runs once per bench invocation, before timing.
fn report_payload_ratio(router: &MemoryRouter, raw: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let injected = rt
        .block_on(router.session_start("claude-desktop", None, None))
        .unwrap()
        .results;
    assert!(
        !injected.is_empty(),
        "recall must inject at least one memory for the ratio to mean anything"
    );

    let injected_text = router.format_as_instructions(&injected);
    // `transcript_to_text` is the extractor's own view of the session, so the
    // baseline is "what a channel-2 continuation would have to carry", not
    // raw JSONL overhead.
    let transcript_text = transcript_to_text(raw);

    let recall_bytes = injected_text.len();
    let recall_tokens = MemoryRouter::estimate_tokens(&injected_text);
    let full_bytes = transcript_text.len();
    let full_tokens = MemoryRouter::estimate_tokens(&transcript_text);
    let byte_ratio = full_bytes as f64 / recall_bytes as f64;
    let token_ratio = full_tokens as f64 / recall_tokens as f64;

    eprintln!("[trace-recall] injected memories: {}", injected.len());
    eprintln!(
        "[trace-recall] recall payload: {recall_bytes} B / {recall_tokens} tokens \
         (estimate_tokens: ASCII ~4 chars/token)"
    );
    eprintln!("[trace-recall] full transcript: {full_bytes} B / {full_tokens} tokens");
    eprintln!(
        "[trace-recall] compression ratio (full / recall): {byte_ratio:.2}x bytes, \
         {token_ratio:.2}x tokens"
    );
}

/// Ingest throughput: turns ingested per synthetic session. One store and one
/// connection pool are shared across iterations; each iteration gets a fresh
/// session id (via `fixture_home`), so the watermark never short-circuits the
/// run and no per-iteration pool is spawned (that exhausted OS threads).
fn bench_trace_ingest(c: &mut Criterion) {
    let base = fixture_base("ingest");
    let store = std::sync::Arc::new(SqliteStore::in_memory().unwrap());
    let rt = tokio::runtime::Runtime::new().unwrap();

    // One-off report so the "turns/session" figure is visible next to timing.
    {
        let home = fixture_home(&base, 0);
        let opts = IngestOptions {
            home,
            approve: true,
            max_sessions: 1,
            ..IngestOptions::default()
        };
        let stats = rt
            .block_on(ingest_for_agent(&*store, "claude", &opts))
            .unwrap();
        eprintln!(
            "[trace-recall] ingest: {} turns new, {} evidence, {} candidates per session",
            stats.turns_new, stats.evidence_saved, stats.candidates_saved
        );
    }

    let mut session = 1usize;
    c.bench_function("trace_ingest_synthetic_session", |b| {
        b.to_async(&rt).iter_batched(
            || {
                session += 1;
                fixture_home(&base, session)
            },
            |home| {
                let store = store.clone();
                async move {
                    let opts = IngestOptions {
                        home,
                        approve: true,
                        max_sessions: 1,
                        ..IngestOptions::default()
                    };
                    let stats = ingest_for_agent(&*store, "claude", &opts).await.unwrap();
                    black_box(stats)
                }
            },
            BatchSize::SmallInput,
        )
    });

    std::fs::remove_dir_all(&base).ok();
}

/// Recall cost: `session_start` over the ingested store. The payload ratio is
/// printed by [`report_payload_ratio`]; criterion times the injection work.
fn bench_recall_injection(c: &mut Criterion) {
    let base = fixture_base("recall");
    let home = fixture_home(&base, 0);
    let router = ingested_router(&home);
    report_payload_ratio(&router, &synthetic_raw());
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("recall_session_start_injected", |b| {
        b.to_async(&rt).iter(|| async {
            let results = router
                .session_start(
                    black_box("claude-desktop"),
                    black_box(None),
                    black_box(None),
                )
                .await
                .unwrap()
                .results;
            black_box(results)
        })
    });

    std::fs::remove_dir_all(&base).ok();
}

criterion_group!(
    name = trace_recall;
    config = Criterion::default().sample_size(30);
    targets = bench_trace_ingest, bench_recall_injection
);
criterion_main!(trace_recall);
