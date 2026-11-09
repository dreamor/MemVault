use async_trait::async_trait;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::path::Path;
use tracing::{debug, info, warn};

use super::{ApproveSupersedeOutcome, MemoryStore};
use crate::embedding::cosine_similarity;
use crate::error::{MemVaultError, Result};
use crate::models::*;

type Pool = r2d2::Pool<SqliteConnectionManager>;

/// Append namespace/type/priority filter clauses to a search statement.
/// `prefix` qualifies the column names when the query joins memories under an
/// alias (the FTS path selects `m.*`).
fn append_filter_clauses(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    prefix: &str,
    query: &SearchQuery,
) {
    if let Some(ref ns) = query.namespace {
        // Canonicalize on the read side too, so a caller-supplied spelling
        // (`project:MemVault` vs the canonical `project:memvault`) matches
        // what the write path stored.
        let idx = params.len() + 1;
        sql.push_str(&format!(" AND {prefix}namespace = ?{idx}"));
        params.push(Box::new(
            crate::models::canonical_project_namespace(ns).into_owned(),
        ));
    }

    if let Some(ref mt) = query.type_filter {
        let type_str = serde_json::to_string(mt).unwrap_or_default();
        let type_str = type_str.trim_matches('"').to_string();
        let idx = params.len() + 1;
        sql.push_str(&format!(" AND {prefix}memory_type = ?{idx}"));
        params.push(Box::new(type_str));
    }

    if let Some(ref pf) = query.priority_filter {
        let p_str = serde_json::to_string(pf).unwrap_or_default();
        let p_str = p_str.trim_matches('"').to_string();
        let idx = params.len() + 1;
        sql.push_str(&format!(" AND {prefix}priority = ?{idx}"));
        params.push(Box::new(p_str));
    }

    // Superseded memories are archived knowledge (C4): retrieval serves the
    // current truth only. They stay visible via list/checkpoints/restore.
    sql.push_str(&format!(" AND {prefix}superseded_by IS NULL"));

    // A pending edit proposal is not current truth either — it is a change
    // awaiting a human decision (`crate::proposal`). Excluded for the same
    // reason as a superseded memory, and for the same reason it stays visible
    // through `list_pending` / the Inbox, which is where it is meant to be read.
    // Without this, a search would return both the live memory and its near-identical
    // pending edit, and the agent would have no way to tell which one is true.
    sql.push_str(&format!(" AND {prefix}proposes_supersede_of IS NULL"));

    // Expired memories retire exactly the way superseded ones do: out of
    // retrieval and injection, still visible via list/checkpoints/restore, and
    // never physically removed by expiry alone (erasure is forget's job, §9.5).
    //
    // The comparison instant is the query's `as_of` when one is set, so a
    // point-in-time search sees what was in effect *then* — a memory that has
    // since expired was still live at T and must come back.
    let idx = params.len() + 1;
    sql.push_str(&format!(
        " AND ({prefix}expires_at IS NULL OR {prefix}expires_at > ?{idx})"
    ));
    params.push(Box::new(expiry_reference(query)));

    // Event-time window — a different axis from `as_of` above, and the two are
    // never combined into one range (§9.6).
    //
    // `occurred_at IS NOT NULL` is required, not incidental: an undated memory
    // cannot be said to have happened inside a range, and admitting it would
    // return the same rows for every window.
    //
    // Compared through `datetime()` rather than as raw text, unlike `expires_at`.
    // The difference is deliberate. `expires_at` is written by one helper in a
    // fixed-width form precisely so that plain lexicographic comparison is
    // chronological and `idx_memories_expires_at` stays usable; `occurred_at` is
    // written by `to_rfc3339()` with variable fractional precision and has no
    // index, so there is nothing to preserve by avoiding the function — and
    // normalising both sides is what makes a mixed-precision column compare
    // correctly. Second precision is ample for an event window.
    if let Some(from) = query.occurred_from {
        let idx = params.len() + 1;
        sql.push_str(&format!(
            " AND {prefix}occurred_at IS NOT NULL AND datetime({prefix}occurred_at) >= datetime(?{idx})"
        ));
        params.push(Box::new(from.to_rfc3339()));
    }
    if let Some(to) = query.occurred_to {
        let idx = params.len() + 1;
        sql.push_str(&format!(
            " AND {prefix}occurred_at IS NOT NULL AND datetime({prefix}occurred_at) <= datetime(?{idx})"
        ));
        params.push(Box::new(to.to_rfc3339()));
    }
}

/// The instant an expiry comparison is made against: the query's `as_of` when
/// it set one, otherwise now.
fn expiry_instant(query: &SearchQuery) -> chrono::DateTime<chrono::Utc> {
    query.as_of.unwrap_or_else(chrono::Utc::now)
}

/// Canonical on-disk form for `expires_at`.
///
/// Fixed microsecond precision with a `Z` suffix (`2026-11-05T14:04:45.123456Z`)
/// rather than `to_rfc3339()`, which emits a variable number of fractional
/// digits and a `+00:00` offset. Fixed width is what makes plain lexicographic
/// comparison in SQL (`expires_at > ?`) exactly equivalent to chronological
/// comparison — and therefore what lets `idx_memories_expires_at` be used
/// instead of wrapping the column in `datetime()` and forcing a scan. Every
/// writer goes through this helper, so the column can only ever hold one shape.
fn expiry_stamp(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Bound parameter form of [`expiry_instant`]: the stamp the SQL filter compares
/// `expires_at` against.
fn expiry_reference(query: &SearchQuery) -> String {
    expiry_stamp(expiry_instant(query))
}

pub struct SqliteStore {
    pool: Pool,
}

/// One row of `memory_history`, without the full JSON snapshot — used for
/// `list_checkpoints` listings. Fetch the snapshot itself via
/// `restore_checkpoint`.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub history_id: i64,
    pub memory_id: String,
    pub operation: String,
    pub changed_at: String,
}

fn default_pool_size() -> u32 {
    std::env::var("MEMVAULT_DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
}

/// Per-connection setup applied to every connection the pool creates,
/// mirroring the PRAGMAs previously set once on the single shared connection.
fn init_connection(conn: &mut Connection) -> std::result::Result<(), rusqlite::Error> {
    // busy_timeout must be set before journal_mode=WAL: switching journal
    // modes needs a brief exclusive lock, and if the pool is eagerly opening
    // several connections against a brand-new database file at once, that
    // switch is exactly where contention happens.
    conn.execute_batch(
        "PRAGMA busy_timeout=5000; \
         PRAGMA journal_mode=WAL; \
         PRAGMA foreign_keys=ON;",
    )
}

impl SqliteStore {
    pub fn new(path: &Path) -> Result<Self> {
        let manager = SqliteConnectionManager::file(path).with_init(init_connection);
        let pool = r2d2::Pool::builder()
            .max_size(default_pool_size())
            // Don't eagerly pre-warm connections: r2d2 defaults to filling
            // the pool to max_size immediately, which opens several
            // connections in parallel against a possibly-brand-new database
            // file, each racing to flip journal_mode to WAL. Creating
            // connections lazily (only when a caller actually needs one)
            // avoids that startup contention.
            .min_idle(Some(0))
            .build(manager)
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let store = Self { pool };
        store.init_schema()?;
        // Best-effort: `~/.memvault/data.db` holds every memory, so keep it
        // (and SQLite's -wal/-shm siblings) owner-only. Done after the first
        // connection has created the file and switched to WAL.
        crate::private_fs::restrict_sqlite_files(path);
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        // SQLite ":memory:" databases are per-connection; pooling more than
        // one connection would give each caller an independent, empty
        // database. Force a single-connection pool so in_memory() behaves
        // like one shared database, as it did with the old Mutex<Connection>.
        let manager = SqliteConnectionManager::memory().with_init(init_connection);
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .min_idle(Some(1))
            .build(manager)
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let store = Self { pool };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS memories (
                id              TEXT PRIMARY KEY,
                memory_type     TEXT NOT NULL,
                content         TEXT NOT NULL,
                instruction     TEXT,
                priority        TEXT NOT NULL DEFAULT 'REFERENCE',
                source_agent_id   TEXT NOT NULL,
                source_agent_type TEXT NOT NULL,
                source_session_id TEXT,
                namespace       TEXT NOT NULL DEFAULT 'global',
                confidence      REAL NOT NULL DEFAULT 0.8,
                tags            TEXT NOT NULL DEFAULT '[]',
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                ai_generated    INTEGER NOT NULL DEFAULT 1,
                human_reviewed  INTEGER NOT NULL DEFAULT 0,
                decay_score     REAL NOT NULL DEFAULT 1.0,
                access_count    INTEGER NOT NULL DEFAULT 0,
                last_read_at    TEXT,
                embedding       BLOB
            );

            CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace);
            CREATE INDEX IF NOT EXISTS idx_memories_priority ON memories(priority);
            CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
            CREATE INDEX IF NOT EXISTS idx_memories_agent ON memories(source_agent_id);
            CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);

            CREATE TABLE IF NOT EXISTS agent_registry (
                id              TEXT PRIMARY KEY,
                agent_type      TEXT NOT NULL,
                description     TEXT NOT NULL DEFAULT '',
                inject_rules    TEXT NOT NULL DEFAULT '{}',
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memory_history (
                history_id  INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_id   TEXT NOT NULL,
                operation   TEXT NOT NULL,
                snapshot    TEXT NOT NULL,
                changed_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_memory_history_memory_id ON memory_history(memory_id);

            -- Per-migration checksums: guards against the schema being
            -- quietly changed underneath migrated databases (see
            -- storage::schema_checksum for why the hash ignores comments).
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                checksum   TEXT NOT NULL,
                algorithm  INTEGER NOT NULL,
                applied_at TEXT NOT NULL
            );

            -- FTS5 keyword index over content/instruction/tags. Columns hold
            -- pre-tokenized text (CJK unigrams + bigrams, see crate::fts):
            -- the bundled FTS5 tokenizers do not segment CJK, so tokenization
            -- happens on the write path and the query path uses the SAME
            -- tokenizer. memory_id is UNINDEXED: stored for joining back to
            -- memories, never searchable itself.
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                instruction,
                tags,
                memory_id UNINDEXED
            );
        ",
        )?;

        Self::run_migrations(&conn)?;
        // One-time, idempotent data fix: rewrite pre-existing project
        // namespaces to their canonical spelling so rows written before
        // normalization (e.g. `project:MemVault`) stay reachable by the
        // canonical read filters.
        Self::backfill_canonical_project_namespaces(&conn)?;
        Self::ensure_fts_populated(&conn)?;

        Ok(())
    }

    /// Rewrite `project:*` namespaces to their canonical form (lowercase,
    /// trailing slashes stripped) in place. Idempotent: rows already
    /// canonical are untouched, so this converges on the first open and is a
    /// cheap `SELECT … LIKE` scan afterwards.
    fn backfill_canonical_project_namespaces(conn: &Connection) -> Result<()> {
        let mut stmt =
            conn.prepare("SELECT id, namespace FROM memories WHERE namespace LIKE 'project:%'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut rewrites: Vec<(String, String)> = Vec::new();
        for row in rows {
            let (id, ns) = row?;
            let canonical = crate::models::canonical_project_namespace(&ns).into_owned();
            if canonical != ns {
                rewrites.push((id, canonical));
            }
        }
        for (id, canonical) in rewrites {
            conn.execute(
                "UPDATE memories SET namespace = ?1 WHERE id = ?2",
                rusqlite::params![canonical, id],
            )?;
        }
        Ok(())
    }

    /// Keep the FTS index in sync with the memories table.
    ///
    /// Runs on startup: if the two row counts disagree (fresh index over an
    /// existing database, or any partial failure mid-maintenance), rebuild
    /// the whole index. A rebuild is cheap for a local-first store and makes
    /// "index silently half-populated" impossible to observe from outside.
    fn ensure_fts_populated(conn: &Connection) -> Result<()> {
        let memories_count: i64 =
            conn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
        let fts_count: i64 =
            conn.query_row("SELECT count(*) FROM memories_fts", [], |r| r.get(0))?;

        if memories_count != fts_count {
            info!(
                memories = memories_count,
                fts = fts_count,
                "FTS index out of sync with memories table, rebuilding"
            );
            Self::rebuild_fts_index(conn)?;
        }
        // Historical index for `as_of` search. Checked on every open, like the
        // live index above: it is derived data, so making it self-heal costs one
        // count query and removes any need for a separate repair command.
        Self::backfill_history_fts(conn)?;
        Ok(())
    }

    /// Drop and re-populate the FTS index from the memories table.
    pub(crate) fn rebuild_fts_index(conn: &Connection) -> Result<()> {
        conn.execute("DELETE FROM memories_fts", [])?;
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM memories")?
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        for id in ids {
            let memory = conn.query_row(
                "SELECT * FROM memories WHERE id = ?1",
                rusqlite::params![id],
                Self::row_to_memory,
            )?;
            Self::fts_insert(conn, &memory)?;
        }
        Ok(())
    }

    /// Tokenized-text columns for one memory, ready for FTS insertion.
    fn fts_columns(memory: &Memory) -> (String, String, String) {
        let content = crate::fts::tokenize(&memory.content).join(" ");
        let instruction = memory
            .instruction
            .as_deref()
            .map(crate::fts::tokenize)
            .unwrap_or_default()
            .join(" ");
        let tags = crate::fts::tokenize(&memory.tags.join(" ")).join(" ");
        (content, instruction, tags)
    }

    fn fts_insert(conn: &Connection, memory: &Memory) -> Result<()> {
        let (content, instruction, tags) = Self::fts_columns(memory);
        conn.execute(
            "INSERT INTO memories_fts (content, instruction, tags, memory_id) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![content, instruction, tags, memory.id],
        )?;
        Ok(())
    }

    fn fts_delete(conn: &Connection, memory_id: &str) -> Result<()> {
        conn.execute(
            "DELETE FROM memories_fts WHERE memory_id = ?1",
            rusqlite::params![memory_id],
        )?;
        Ok(())
    }

    /// Replace a memory's FTS row after its searchable text changed.
    fn fts_replace(conn: &Connection, memory: &Memory) -> Result<()> {
        Self::fts_delete(conn, &memory.id)?;
        Self::fts_insert(conn, memory)?;
        Ok(())
    }

    /// Versioned schema migrations, tracked via `PRAGMA user_version`.
    ///
    /// Each entry is (version, sql). Migrations with version > current
    /// `user_version` are applied in order, then `user_version` is bumped
    /// to the highest version applied.
    const MIGRATIONS: &'static [(u32, &'static str)] = &[
        (1, "ALTER TABLE memories ADD COLUMN embedding BLOB"),
        (2, "ALTER TABLE memories ADD COLUMN last_read_at TEXT"),
        (
            3,
            "ALTER TABLE memories ADD COLUMN layer TEXT NOT NULL DEFAULT 'L1'",
        ),
        (4, "ALTER TABLE memories ADD COLUMN skill_meta TEXT"),
        // 0 = legacy f32 blob, 1 = int8 quantized (scale f32 LE + i8 data).
        // New writes always use fmt 1; readers handle both so a corpus can
        // migrate (or switch embedding models) gradually.
        (
            5,
            "ALTER TABLE memories ADD COLUMN embedding_fmt INTEGER NOT NULL DEFAULT 0",
        ),
        // Episodic memory: structured task outcomes attached 1:1 to episode
        // memories. ON DELETE CASCADE keeps the row from outliving its memory
        // when that memory is deleted (foreign_keys is ON on every connection).
        (
            6,
            "CREATE TABLE IF NOT EXISTS episodes (
                memory_id        TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                task             TEXT NOT NULL,
                task_type        TEXT,
                status           TEXT NOT NULL,
                cause            TEXT,
                lesson           TEXT,
                lesson_memory_id TEXT,
                occurred_at      TEXT NOT NULL
            )",
        ),
        // Semantic versioning: points a superseded fact at its replacement.
        (7, "ALTER TABLE memories ADD COLUMN superseded_by TEXT"),
        (
            8,
            "CREATE INDEX IF NOT EXISTS idx_episodes_task_type ON episodes(task_type)",
        ),
        (
            9,
            "CREATE INDEX IF NOT EXISTS idx_episodes_status ON episodes(status)",
        ),
        // Procedural memory: per-skill execution statistics (injection count
        // + attributed outcomes) driving the success-rate display. Cascade
        // keeps stats from outliving a deleted skill.
        (
            10,
            "CREATE TABLE IF NOT EXISTS skill_stats (
                skill_memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                injected_count  INTEGER NOT NULL DEFAULT 0,
                success_count   INTEGER NOT NULL DEFAULT 0,
                failure_count   INTEGER NOT NULL DEFAULT 0,
                updated_at      TEXT NOT NULL
            )",
        ),
        // Semantic memory: lightweight relation triples (subject → predicate
        // → object). Subject is always a memory; object is a memory id or
        // free text. Deleting an endpoint memory cascades its relations;
        // deleting a provenance source nulls the backlink.
        (
            11,
            "CREATE TABLE IF NOT EXISTS memory_relations (
                relation_id      INTEGER PRIMARY KEY AUTOINCREMENT,
                subject_id       TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                predicate        TEXT NOT NULL,
                object_id        TEXT REFERENCES memories(id) ON DELETE CASCADE,
                object_text      TEXT,
                confidence       REAL NOT NULL DEFAULT 0.8,
                source_memory_id TEXT REFERENCES memories(id) ON DELETE SET NULL,
                created_at       TEXT NOT NULL
            )",
        ),
        (
            12,
            "CREATE INDEX IF NOT EXISTS idx_relations_subject ON memory_relations(subject_id)",
        ),
        (
            13,
            "CREATE INDEX IF NOT EXISTS idx_relations_object ON memory_relations(object_id)",
        ),
        // Phase D team shared pool: visibility = 'scoped' (default, namespace
        // rules) or 'shared' (injected into every session).
        (
            14,
            "ALTER TABLE memories ADD COLUMN visibility TEXT NOT NULL DEFAULT 'scoped'",
        ),
        // Identity/corroboration signals for the MUST trust gate
        // (router::format::is_trusted): whether this write's agent_id had a
        // registered+matched API key, and which distinct verified agents'
        // writes have merged into this memory's content.
        (
            15,
            "ALTER TABLE memories ADD COLUMN identity_verified INTEGER NOT NULL DEFAULT 0",
        ),
        (
            16,
            "ALTER TABLE memories ADD COLUMN corroborating_agents TEXT NOT NULL DEFAULT '[]'",
        ),
        // Event provenance: when the remembered fact actually happened in
        // the source conversation (caller-supplied), distinct from
        // created_at (= ingestion time). Nullable; NULL for memories whose
        // caller supplied no date.
        (17, "ALTER TABLE memories ADD COLUMN occurred_at TEXT"),
        // Trace ingestion watermark: per (agent_key, session_id) pointer to
        // the last transcript turn already ingested, so a crashed/resumed
        // ingestion run continues where it left off instead of duplicating
        // (or skipping) raw evidence turns.
        (
            18,
            "CREATE TABLE IF NOT EXISTS trace_watermark (
                agent_key   TEXT NOT NULL,
                session_id  TEXT NOT NULL,
                last_seq    INTEGER NOT NULL DEFAULT 0,
                updated_at  TEXT NOT NULL,
                PRIMARY KEY (agent_key, session_id)
            )",
        ),
        // Trace provenance chain: which L0 raw-evidence memory ids this
        // distilled memory was derived from. JSON array of memory ids; empty
        // for memories that were not distilled from raw evidence.
        (
            19,
            "ALTER TABLE memories ADD COLUMN source_trace_ids TEXT NOT NULL DEFAULT '[]'",
        ),
        // Friction-gate provenance: human-readable note (signal counts +
        // outcome-backfill suggestion) attached when a Stop-hook-triggered
        // extract passed the friction gate (see crate::friction). Nullable;
        // NULL for memories not produced through that gate.
        (20, "ALTER TABLE memories ADD COLUMN friction_evidence TEXT"),
        // Forget feature: when a memory first transitioned into an
        // `archived:*` namespace (set once by decay::run_decay, never
        // reset). Nullable; NULL for memories never archived. This is what
        // ForgetManager::auto_forget measures retention against —
        // updated_at is bumped by record_access even while archived, so it
        // cannot be used as a "time since archived" signal.
        (21, "ALTER TABLE memories ADD COLUMN archived_at TEXT"),
        // Forget feature audit trail: proof an erasure happened, without
        // retaining the erased content (content_hash only). Deliberately
        // has NO foreign key to memories(id) — this row must survive
        // purge() deleting the memory it describes; a CASCADE FK here
        // would delete the audit entry the instant it's written.
        (
            22,
            "CREATE TABLE IF NOT EXISTS forgotten_log (
                log_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_id     TEXT NOT NULL,
                content_hash  TEXT NOT NULL,
                namespace     TEXT NOT NULL,
                priority      TEXT NOT NULL,
                visibility    TEXT NOT NULL,
                scope         TEXT NOT NULL,
                reason        TEXT,
                requested_by  TEXT NOT NULL,
                must_override INTEGER NOT NULL DEFAULT 0,
                erased_at     TEXT NOT NULL
            )",
        ),
        (
            23,
            "CREATE INDEX IF NOT EXISTS idx_forgotten_log_memory_id ON forgotten_log(memory_id)",
        ),
        // Valid-time upper bound: when a memory stops being true. Third and
        // deliberately separate time axis alongside occurred_at (event-time,
        // migration 17) and created_at (ingest-time). NULL = never expires.
        // Retrieval filters on `expires_at IS NULL OR expires_at > <ref>`;
        // expiry alone never deletes anything (erasure is forget's job).
        (24, "ALTER TABLE memories ADD COLUMN expires_at TEXT"),
        // Partial index for the expiry filter. Most rows never expire, so a
        // plain index would be nearly all NULLs; the partial form indexes only
        // the rows the filter actually has to compare.
        (
            25,
            "CREATE INDEX IF NOT EXISTS idx_memories_expires_at ON memories(expires_at) WHERE expires_at IS NOT NULL",
        ),
        // Pending edit proposals (crate::proposal). Points a proposed replacement
        // at the memory it would supersede once a human approves it. The link is
        // a real column rather than a tag because approving it triggers a
        // destructive action on another row — that should be keyed on a validated
        // field, not a string prefix in a user-editable list.
        (
            26,
            "ALTER TABLE memories ADD COLUMN proposes_supersede_of TEXT",
        ),
        (
            27,
            "CREATE INDEX IF NOT EXISTS idx_memories_proposal ON memories(proposes_supersede_of) WHERE proposes_supersede_of IS NOT NULL",
        ),
        // Full-text index over `memory_history` snapshots, so an `as_of` search
        // can recall a memory by the text it had *then* and not only by its
        // current text. Without this, "what did we believe about X in August"
        // misses every memory whose wording has since changed — the limitation
        // §9.6 documented when `as_of` first shipped.
        //
        // Indexed columns mirror `memories_fts` exactly and are populated through
        // the same `fts_columns` helper, so tokenization cannot drift between the
        // live index and the historical one. `history_id` is carried alongside
        // `memory_id` because the query has to know *which* version matched, not
        // just which memory — a match on a version that was not in effect at
        // `as_of` must not produce a result.
        (
            28,
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_history_fts USING fts5(
                content,
                instruction,
                tags,
                history_id UNINDEXED,
                memory_id UNINDEXED
            )",
        ),
    ];

    fn run_migrations(conn: &Connection) -> Result<()> {
        let current_version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        if current_version == 0 {
            // Either a brand-new database, or a pre-versioning database that
            // may already have some/all migration columns applied via the
            // old ad-hoc ALTER-and-ignore-errors approach. Reconcile by
            // checking actual column presence rather than trusting
            // user_version, then jump straight to the latest version.
            Self::reconcile_legacy_schema(conn)?;
            let latest = Self::MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0);
            conn.execute(&format!("PRAGMA user_version = {}", latest), [])?;
            // First checksum-enabled open: baseline every migration so later
            // opens have something to verify against.
            Self::ensure_migrations_recorded(conn, latest)?;
            return Ok(());
        }

        let mut applied_up_to = current_version;
        for (version, sql) in Self::MIGRATIONS {
            if *version > current_version {
                conn.execute(sql, [])?;
                conn.execute(&format!("PRAGMA user_version = {}", version), [])?;
                applied_up_to = *version;
                info!(version, "applied schema migration");
            }
        }

        // Verify (and, on first checksum-enabled open, record) the checksum
        // of every migration applied to this database. A mismatch means the
        // migration text changed after it ran — schema drift — and failing
        // closed is deliberate: a silently-wrong schema is worse than a loud
        // startup refusal.
        Self::ensure_migrations_recorded(conn, applied_up_to)?;

        Ok(())
    }

    /// Record/verify checksums for all migrations up to `applied_up_to`.
    ///
    /// - No row yet → record the current checksum (baseline). This is how
    ///   databases created before checksum support get adopted: the CURRENT
    ///   migration text becomes the trusted baseline (any older drift is
    ///   forgiven once, by design).
    /// - Row recorded under a different checksum algorithm → re-baseline
    ///   under the new algorithm (warn), since old-algorithm hashes are
    ///   incomparable rather than contradictory.
    /// - Row recorded under the SAME algorithm but a different checksum →
    ///   the migration text changed after it was applied: hard error.
    fn ensure_migrations_recorded(conn: &Connection, applied_up_to: u32) -> Result<()> {
        use crate::storage::schema_checksum::{CHECKSUM_ALGORITHM, schema_checksum};

        let now = chrono::Utc::now().to_rfc3339();
        for (version, sql) in Self::MIGRATIONS {
            if *version > applied_up_to {
                break;
            }
            let expected = schema_checksum(sql);

            let existing: Option<(String, i64)> = match conn.query_row(
                "SELECT checksum, algorithm FROM schema_migrations WHERE version = ?1",
                rusqlite::params![version],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            ) {
                Ok(pair) => Some(pair),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(MemVaultError::Sqlite(e)),
            };

            match existing {
                None => {
                    conn.execute(
                        "INSERT INTO schema_migrations (version, checksum, algorithm, applied_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![version, expected, CHECKSUM_ALGORITHM, now],
                    )?;
                    debug!(version, "migration checksum baselined");
                }
                Some((_stored, algorithm)) if algorithm != CHECKSUM_ALGORITHM => {
                    warn!(
                        version,
                        old_algorithm = algorithm,
                        "re-baselining migration checksum under new algorithm"
                    );
                    conn.execute(
                        "UPDATE schema_migrations SET checksum = ?2, algorithm = ?3 WHERE version = ?1",
                        rusqlite::params![version, expected, CHECKSUM_ALGORITHM],
                    )?;
                }
                Some((stored, _)) if stored != expected => {
                    return Err(MemVaultError::SchemaDrift(format!(
                        "migration v{} stored checksum {} but current definition hashes to {} \
                         — the migration text changed after it was applied",
                        version, stored, expected
                    )));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Schema fingerprint for diagnostics: user_version plus the checksum of
    /// the newest recorded migration (short prefix — enough to compare two
    /// databases, not enough to fake).
    pub fn schema_fingerprint(&self) -> Result<(u32, String)> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let checksum: String = match conn.query_row(
            "SELECT checksum FROM schema_migrations WHERE version = \
             (SELECT MAX(version) FROM schema_migrations)",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(cs) => cs.chars().take(12).collect(),
            Err(rusqlite::Error::QueryReturnedNoRows) => "none".to_string(),
            Err(e) => return Err(MemVaultError::Sqlite(e)),
        };
        Ok((version, checksum))
    }

    /// For databases at user_version=0 (fresh or legacy), add any of the
    /// migration columns that are missing, tolerating "duplicate column"
    /// errors from columns a legacy ad-hoc migration already added.
    fn reconcile_legacy_schema(conn: &Connection) -> Result<()> {
        let existing_columns: std::collections::HashSet<String> = conn
            .prepare("SELECT name FROM pragma_table_info('memories')")?
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        let column_of = |sql: &str| -> Option<String> {
            // "ALTER TABLE memories ADD COLUMN <name> ..." → extract <name>
            sql.split("ADD COLUMN")
                .nth(1)?
                .split_whitespace()
                .next()
                .map(str::to_string)
        };

        for (version, sql) in Self::MIGRATIONS {
            let already_present = column_of(sql)
                .map(|col| existing_columns.contains(&col))
                .unwrap_or(false);
            if already_present {
                debug!(version, "legacy column already present, skipping");
                continue;
            }
            if let Err(e) = conn.execute(sql, []) {
                warn!(version, error = %e, "legacy schema reconciliation step failed (continuing)");
            }
        }
        Ok(())
    }

    /// Create a consistent point-in-time snapshot of the database at `dest`
    /// using SQLite's `VACUUM INTO`, which is safe to run against a live
    /// WAL-mode database (it reads a transactionally consistent snapshot
    /// without blocking concurrent readers/writers for long).
    pub async fn backup_to(&self, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Err(MemVaultError::InvalidInput(format!(
                "backup destination already exists: {}",
                dest.display()
            )));
        }
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let dest_str = dest.to_string_lossy().to_string();
        conn.execute("VACUUM INTO ?1", rusqlite::params![dest_str])?;
        info!(dest = %dest.display(), "database backup created");
        Ok(())
    }

    /// Snapshot `memory` into `memory_history` before an update/delete
    /// overwrites or removes its row. Stored as a single JSON blob (rather
    /// than mirroring columns) so the history table never needs its own
    /// migration when the `memories` schema grows a column.
    fn write_history(conn: &Connection, memory: &Memory, operation: &str) -> Result<()> {
        let snapshot = serde_json::to_string(memory)?;
        conn.execute(
            "INSERT INTO memory_history (memory_id, operation, snapshot, changed_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                memory.id,
                operation,
                snapshot,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        // Same transaction as the snapshot row: a history entry that is not
        // searchable would make `as_of` recall silently incomplete, and the
        // failure would be invisible (the query would just return fewer results).
        let history_id = conn.last_insert_rowid();
        Self::history_fts_insert(conn, history_id, memory)?;
        Ok(())
    }

    /// Index one snapshot's searchable text. Shares [`Self::fts_columns`] with the
    /// live index so the two can never tokenize the same memory differently.
    fn history_fts_insert(conn: &Connection, history_id: i64, memory: &Memory) -> Result<()> {
        let (content, instruction, tags) = Self::fts_columns(memory);
        conn.execute(
            "INSERT INTO memory_history_fts (content, instruction, tags, history_id, memory_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![content, instruction, tags, history_id, memory.id],
        )?;
        Ok(())
    }

    /// One full row update inside an already-open transaction: pre-update
    /// history snapshot, the row UPDATE, and the live-FTS replace. Extracted
    /// verbatim from [`MemoryStore::update`] so `approve_supersede` can run
    /// *two* of them — target retirement plus proposal approval — under a
    /// single write lock without duplicating this (drift-prone: see the
    /// `ai_generated` and `expires_at` notes) column list. The caller owns
    /// validate/canonicalize/commit.
    fn write_update_in_tx(conn: &Connection, memory: &Memory) -> Result<()> {
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        // Snapshot the pre-update row so it can be restored later. If the
        // row doesn't exist, skip the snapshot and let the UPDATE below
        // affect 0 rows and return NotFound, same as before this change.
        if let Ok(old) = conn.query_row(
            "SELECT * FROM memories WHERE id = ?1",
            rusqlite::params![memory.id],
            Self::row_to_memory,
        ) {
            Self::write_history(conn, &old, "update")?;
        }

        let corroborating_agents_json = serde_json::to_string(&memory.corroborating_agents)?;
        let source_trace_ids_json = serde_json::to_string(&memory.source_trace_ids)?;

        // `ai_generated` MUST stay in this column list: trust revocations
        // flip it in memory (writer::merge_memory absorbing unreviewed
        // content, markdown quarantine) and is_trusted() accepts
        // `!ai_generated` as the human-authorship shortcut — an UPDATE that
        // drops the column silently re-trusts revoked content on the next
        // read (the column once went missing exactly this way).
        let rows = conn.execute(
            "UPDATE memories SET memory_type=?2, content=?3, instruction=?4, priority=?5,
             namespace=?6, confidence=?7, tags=?8, updated_at=?9,
             human_reviewed=?10, decay_score=?11, access_count=?12, last_read_at=?13, layer=?14, skill_meta=?15, superseded_by=?16, visibility=?17,
             identity_verified=?18, corroborating_agents=?19, occurred_at=?20, source_trace_ids=?21, friction_evidence=?22, archived_at=?23,
             ai_generated=?24, expires_at=?25, proposes_supersede_of=?26
             WHERE id=?1",
            rusqlite::params![
                memory.id,
                type_str,
                memory.content,
                memory.instruction,
                priority_str,
                memory.namespace,
                memory.confidence,
                tags_json,
                memory.updated_at.to_rfc3339(),
                memory.human_reviewed,
                memory.decay_score,
                memory.access_count,
                memory.last_read_at.map(|dt| dt.to_rfc3339()),
                serde_json::to_string(&memory.layer).unwrap_or_default().trim_matches('"').to_string(),
                memory.skill_meta.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()),
                memory.superseded_by,
                memory.visibility.as_str(),
                memory.identity_verified,
                corroborating_agents_json,
                memory.occurred_at.map(|dt| dt.to_rfc3339()),
                source_trace_ids_json,
                memory.friction_evidence,
                memory.archived_at.map(|dt| dt.to_rfc3339()),
                memory.ai_generated,
                // `expires_at` MUST stay in this column list for the same
                // reason as `ai_generated` above: an UPDATE that omits it would
                // reset a temporary fact's valid-time bound to NULL, silently
                // making it permanent and re-injectable after it should have
                // retired.
                memory.expires_at.map(expiry_stamp),
                memory.proposes_supersede_of,
            ],
        )?;

        if rows == 0 {
            return Err(MemVaultError::NotFound(memory.id.clone()));
        }

        // Same transaction as the row update: either the new text and its
        // index entry both become visible, or neither does.
        Self::fts_replace(conn, memory)?;
        Ok(())
    }

    /// Index snapshots written before migration 28 existed.
    ///
    /// Idempotent and self-healing, following the same shape as
    /// [`Self::rebuild_fts_index`]: compare row counts, fill only the gap.
    ///
    /// A snapshot whose JSON no longer parses is logged and skipped rather than
    /// treated as an error. One corrupt row must not prevent the store from
    /// opening, and it must not make the count comparison disagree forever — the
    /// `NOT IN` predicate drives the work list, so a skipped row is simply
    /// retried on the next open and skipped again. It stays unsearchable at
    /// `as_of`, which is a smaller loss than a vault that will not open.
    fn backfill_history_fts(conn: &Connection) -> Result<()> {
        let indexed: i64 =
            conn.query_row("SELECT count(*) FROM memory_history_fts", [], |r| r.get(0))?;
        let total: i64 = conn.query_row("SELECT count(*) FROM memory_history", [], |r| r.get(0))?;
        if indexed >= total {
            return Ok(());
        }

        let mut stmt = conn.prepare(
            "SELECT h.history_id, h.memory_id, h.snapshot FROM memory_history h
             WHERE h.history_id NOT IN (SELECT history_id FROM memory_history_fts)
             ORDER BY h.history_id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let mut added = 0usize;
        let mut unparseable = 0usize;
        for (history_id, memory_id, snapshot) in rows {
            let Ok(memory) = serde_json::from_str::<Memory>(&snapshot) else {
                // Skipped, not fatal: one corrupt snapshot must not block the
                // index for every other memory. It stays unsearchable at `as_of`,
                // which is a smaller loss than a store that will not open.
                unparseable += 1;
                warn!(
                    history_id,
                    memory_id = %memory_id,
                    "history snapshot does not parse; skipping it in the historical index"
                );
                continue;
            };
            Self::history_fts_insert(conn, history_id, &memory)?;
            added += 1;
        }
        if added > 0 || unparseable > 0 {
            info!(
                added,
                unparseable, "backfilled memory_history_fts (historical as_of search)"
            );
        }
        Ok(())
    }

    /// Reconstruct the state `id` held at `as_of`, or `None` when there was no
    /// such state (not created yet, already deleted, or its validity window had
    /// closed).
    ///
    /// `write_history` snapshots the **pre-operation** row, so the chain reads:
    /// snapshot 1 was current during `[created_at, t1)`, snapshot 2 during
    /// `[t1, t2)`, … and the live row during `[tn, now)`. The state at `as_of`
    /// is therefore the **first snapshot whose `changed_at` postdates `as_of`**;
    /// if none does, it is the live row — unless that row is gone, meaning the
    /// memory had already been deleted by `as_of`.
    ///
    /// This needs no new schema: the version chain already exists
    /// (`memory_history` for content, `superseded_by` for knowledge
    /// replacement), it just was not queryable by time. A superseded memory
    /// queried at an `as_of` before the supersede correctly comes back with
    /// `superseded_by: None`, because that is what the snapshot holds.
    fn reconstruct_at(
        conn: &Connection,
        id: &str,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<MemoryAtTime>> {
        let mut stmt = conn.prepare(
            "SELECT history_id, snapshot, changed_at FROM memory_history
             WHERE memory_id = ?1 ORDER BY changed_at ASC, history_id ASC",
        )?;
        let snapshots = stmt
            .query_map(rusqlite::params![id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        for (history_id, snapshot, changed_at) in snapshots {
            let Some(replaced_at) = chrono::DateTime::parse_from_rfc3339(&changed_at)
                .inspect_err(|e| warn!(id = %id, raw = %changed_at, error = %e, "unparseable history changed_at; snapshot cannot be ordered, skipping"))
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
            else {
                continue;
            };
            if as_of >= replaced_at {
                // This snapshot had already been replaced by `as_of`; a later
                // one (or the live row) describes that instant.
                continue;
            }
            let memory: Memory = serde_json::from_str(&snapshot)?;
            // The snapshot existed as a version, but the memory itself may not
            // have been in effect: created after `as_of` cannot happen (the
            // snapshot predates it) but an `expires_at` already past can.
            if !memory.was_in_effect_at(as_of) {
                return Ok(None);
            }
            return Ok(Some(MemoryAtTime {
                memory,
                as_of,
                source: AsOfSource::History {
                    history_id,
                    replaced_at,
                },
            }));
        }

        // No snapshot postdates `as_of`, so the live row is the state then —
        // provided the memory still has one and was in effect.
        let live = conn
            .query_row(
                "SELECT * FROM memories WHERE id = ?1",
                rusqlite::params![id],
                Self::row_to_memory,
            )
            .optional()?;
        Ok(live
            .filter(|m| m.was_in_effect_at(as_of))
            .map(|memory| MemoryAtTime {
                memory,
                as_of,
                source: AsOfSource::Live,
            }))
    }

    /// Rewrite each hit to the state it held at `as_of`, dropping hits that were
    /// not in effect then. Applied **before** re-ranking and truncation, so the
    /// returned `top_k` is filled with memories that actually existed at `as_of`
    /// rather than padded with current-only ones.
    ///
    /// `score` is deliberately left untouched: it was computed against the
    /// current index text, and re-scoring against reconstructed text would
    /// misrepresent which path recalled the hit. `SearchResult::as_of_source`
    /// carries the caveat instead.
    fn project_results_as_of(
        conn: &Connection,
        results: Vec<SearchResult>,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<SearchResult>> {
        let mut out = Vec::with_capacity(results.len());
        for mut hit in results {
            let id = hit.memory.id.clone();
            if let Some(at) = Self::reconstruct_at(conn, &id, as_of)? {
                hit.memory = at.memory;
                hit.as_of_source = Some(at.source);
                out.push(hit);
            }
            // Not in effect at `as_of` — dropped, never silently replaced by
            // the current state.
        }
        Ok(out)
    }

    /// Recall memories whose text **at `as_of`** matched the query.
    ///
    /// Uses `memory_history_fts` (migration 28). This closes the limitation §9.6
    /// documented when `as_of` first shipped: matching ran against the *current*
    /// index text, so a memory reworded since `as_of` could not be found by the
    /// words it had then.
    ///
    /// A match is kept only when the matched snapshot is the one actually in
    /// effect at `as_of`. Without that check the query would return text the
    /// memory did not hold at the requested instant — a memory edited three times
    /// has three historical versions and only one of them was current then.
    ///
    /// Keyword-only, deliberately. Covering the vector path would mean embedding
    /// every snapshot and keeping those vectors in step with the history table,
    /// which is a much larger change for a narrower gain: the documented failure
    /// was "the wording changed", which is a lexical-recall problem.
    fn search_history_at(
        conn: &Connection,
        query: &SearchQuery,
        as_of: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let query_text = query.query.trim();
        if query_text.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        // Strict -> relaxed unigram -> any-token OR: the same ladder the live
        // keyword path climbs, so a historical query does not silently recall
        // more loosely than a current one would. A tier whose expression cannot
        // be built is skipped rather than fatal — the next tier is still worth
        // trying, and the fallback is "no historical hits", not an error.
        let tiers = crate::fts::query_token_tiers(query_text);
        let mut attempts: Vec<String> = Vec::new();
        if let Some(strict) = tiers.first()
            && let Ok(expr) = crate::fts::build_match_expr(strict)
        {
            attempts.push(expr);
        }
        if let Some(relaxed) = tiers.get(1)
            && let Ok(expr) = crate::fts::build_match_expr(relaxed)
        {
            attempts.push(expr);
        }
        if let Some(all) = tiers.last()
            && let Ok(expr) = crate::fts::build_match_expr_or(all)
        {
            attempts.push(expr);
        }

        let mut matched: Vec<(i64, String)> = Vec::new();
        for expr in attempts {
            let mut stmt = conn.prepare(
                "SELECT history_id, memory_id FROM memory_history_fts
                 WHERE memory_history_fts MATCH ?1 ORDER BY bm25(memory_history_fts) ASC
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![expr, limit as i64 * 3], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            if !rows.is_empty() {
                matched = rows;
                break;
            }
        }
        if matched.is_empty() {
            return Ok(Vec::new());
        }

        let search_words = crate::query_expand::expand_query(query_text);
        let now = chrono::Utc::now();
        let mut out: Vec<SearchResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (rank, (history_id, memory_id)) in matched.into_iter().enumerate() {
            if !seen.insert(memory_id.clone()) {
                continue;
            }
            // Only the version that was current at `as_of` may answer. A match on
            // some other historical version means the words existed at *some*
            // point, not at the point being asked about.
            let Some(at) = Self::reconstruct_at(conn, &memory_id, as_of)? else {
                continue;
            };
            let in_effect = matches!(
                at.source,
                AsOfSource::History { history_id: h, .. } if h == history_id
            );
            if !in_effect {
                continue;
            }
            // Re-apply the query's non-text filters to the reconstructed state.
            // The FTS query above can only match text; it has no way to express
            // namespace, type, priority or the event-time window. Without this a
            // search scoped to one project would happily return another project's
            // history — a cross-namespace leak that the live keyword path does not
            // have, because `append_filter_clauses` puts those predicates in SQL.
            if !crate::storage::memory_matches_filters(&at.memory, query, as_of) {
                continue;
            }
            let score = Self::compute_relevance_score(&at.memory, &search_words, now);
            out.push(SearchResult {
                memory: at.memory,
                score,
                hit_sources: vec![HitSource::HistoryKeyword { rank: rank + 1 }],
                as_of_source: Some(at.source),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// List history entries, most recent first. Filters to one memory when
    /// `memory_id` is given, otherwise lists recent changes across all
    /// memories.
    pub async fn list_checkpoints(
        &self,
        memory_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let mut stmt;
        let rows = if let Some(id) = memory_id {
            stmt = conn.prepare(
                "SELECT history_id, memory_id, operation, changed_at FROM memory_history
                 WHERE memory_id = ?1 ORDER BY history_id DESC LIMIT ?2",
            )?;
            stmt.query_map(
                rusqlite::params![id, limit as i64],
                Self::row_to_history_entry,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt = conn.prepare(
                "SELECT history_id, memory_id, operation, changed_at FROM memory_history
                 ORDER BY history_id DESC LIMIT ?1",
            )?;
            stmt.query_map(rusqlite::params![limit as i64], Self::row_to_history_entry)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        Ok(rows)
    }

    /// Restore a memory to the state captured in the given history entry.
    /// If the memory still exists, this is an `update` back to the snapshot
    /// (itself recorded as a new history entry — undoing an undo works for
    /// free). If the memory was deleted, the snapshot is re-inserted.
    pub async fn restore_checkpoint(&self, history_id: i64) -> Result<Memory> {
        let snapshot: String = {
            let conn = self
                .pool
                .get()
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            conn.query_row(
                "SELECT snapshot FROM memory_history WHERE history_id = ?1",
                rusqlite::params![history_id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    MemVaultError::NotFound(format!("history_id {history_id}"))
                }
                other => MemVaultError::Sqlite(other),
            })?
        };

        let memory: Memory = serde_json::from_str(&snapshot)?;

        match MemoryStore::get(self, &memory.id).await {
            Ok(_) => MemoryStore::update(self, memory).await,
            Err(MemVaultError::NotFound(_)) => MemoryStore::save(self, memory).await,
            Err(e) => Err(e),
        }
    }

    fn row_to_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            history_id: row.get("history_id")?,
            memory_id: row.get("memory_id")?,
            operation: row.get("operation")?,
            changed_at: row.get("changed_at")?,
        })
    }

    fn row_to_forgotten_log_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForgottenLogEntry> {
        let memory_id: String = row.get("memory_id")?;
        let priority_str: String = row.get("priority")?;
        let priority = serde_json::from_str(&format!("\"{}\"", priority_str))
            .unwrap_or_else(|e| {
                warn!(memory_id = %memory_id, raw = %priority_str, error = %e, "failed to parse forgotten_log priority, defaulting to Reference");
                Priority::Reference
            });
        let visibility_str: String = row.get("visibility")?;
        let scope_str: String = row.get("scope")?;
        let scope = ForgetScope::parse(&scope_str).unwrap_or_else(|| {
            warn!(memory_id = %memory_id, raw = %scope_str, "unknown forget scope, defaulting to Manual");
            ForgetScope::Manual
        });
        let erased_str: String = row.get("erased_at")?;
        Ok(ForgottenLogEntry {
            log_id: Some(row.get("log_id")?),
            memory_id,
            content_hash: row.get("content_hash")?,
            namespace: row.get("namespace")?,
            priority,
            visibility: Visibility::parse(&visibility_str),
            scope,
            reason: row.get("reason")?,
            requested_by: row.get("requested_by")?,
            must_override: row.get::<_, bool>("must_override")?,
            erased_at: erased_str.parse().unwrap_or_else(|e| {
                warn!(raw = %erased_str, error = %e, "failed to parse erased_at, defaulting to epoch");
                Default::default()
            }),
        })
    }

    /// int8-quantized blob (migration format 1): per-row scale (f32 LE)
    /// followed by one byte per dimension.
    fn embedding_to_int8_blob(embedding: &[f32]) -> Vec<u8> {
        let q = crate::embedding::quantize_int8(embedding);
        let mut blob = Vec::with_capacity(4 + q.data.len());
        blob.extend_from_slice(&q.scale.to_le_bytes());
        blob.extend(q.data.iter().map(|&v| v as u8));
        blob
    }

    fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
        let (words, _) = blob.as_chunks::<4>();
        words.iter().map(|&c| f32::from_le_bytes(c)).collect()
    }

    /// Decode an embedding blob according to its stored format. int8 rows
    /// dequantize back to floats (their L2-normalized approximation).
    fn decode_embedding(blob: &[u8], fmt: i64) -> Vec<f32> {
        match fmt {
            1 if blob.len() >= 4 => {
                let scale = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
                let q = crate::embedding::QuantizedVector {
                    data: blob[4..].iter().map(|&b| b as i8).collect(),
                    scale,
                    dim: blob.len() - 4,
                };
                crate::embedding::dequantize_int8(&q)
            }
            _ => Self::blob_to_embedding(blob),
        }
    }

    fn compute_relevance_score(
        memory: &Memory,
        search_words: &[String],
        now: chrono::DateTime<chrono::Utc>,
    ) -> f64 {
        if memory.priority == Priority::Must {
            return 1.0;
        }

        // match_score: how many search words appear in content/instruction/tags
        let match_score = if search_words.is_empty() {
            0.5 // no query = neutral
        } else {
            let searchable = format!(
                "{} {} {}",
                memory.content.to_lowercase(),
                memory.instruction.as_deref().unwrap_or("").to_lowercase(),
                memory.tags.join(" ").to_lowercase(),
            );

            let hits = search_words
                .iter()
                .filter(|w| searchable.contains(&w.to_lowercase()))
                .count();

            (hits as f64 / search_words.len() as f64).min(1.0)
        };

        // recency_score: 30-day half-life
        let days = (now - memory.updated_at).num_hours() as f64 / 24.0;
        let recency_score = 1.0 / (1.0 + days / 30.0);

        // priority_score
        let priority_score = match memory.priority {
            Priority::Must => 1.0,
            Priority::Reference => 0.5,
            Priority::Background => 0.2,
        };

        // access_score
        let access_score = (memory.access_count as f64 / 10.0).min(1.0);

        // combined
        match_score * 0.4
            + recency_score * 0.2
            + priority_score * 0.2
            + access_score * 0.1
            + memory.decay_score * 0.1
    }

    fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
        let id: String = row.get("id")?;

        let tags_str: String = row.get("tags")?;
        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_else(|e| {
            warn!(id = %id, error = %e, "failed to parse tags JSON, defaulting to empty");
            Vec::new()
        });

        let corroborating_agents_str: String = row.get("corroborating_agents")?;
        let corroborating_agents: Vec<String> = serde_json::from_str(&corroborating_agents_str)
            .unwrap_or_else(|e| {
                warn!(id = %id, error = %e, "failed to parse corroborating_agents JSON, defaulting to empty");
                Vec::new()
            });

        let source_trace_ids_str: String = row.get("source_trace_ids")?;
        let source_trace_ids: Vec<String> = serde_json::from_str(&source_trace_ids_str)
            .unwrap_or_else(|e| {
                warn!(id = %id, error = %e, "failed to parse source_trace_ids JSON, defaulting to empty");
                Vec::new()
            });

        let memory_type_str: String = row.get("memory_type")?;
        let memory_type: MemoryType = serde_json::from_str(&format!("\"{}\"", memory_type_str))
            .unwrap_or_else(|e| {
                warn!(id = %id, raw = %memory_type_str, error = %e, "failed to parse memory_type, defaulting to Fact");
                MemoryType::Fact
            });

        let priority_str: String = row.get("priority")?;
        let priority: Priority = serde_json::from_str(&format!("\"{}\"", priority_str))
            .unwrap_or_else(|e| {
                warn!(id = %id, raw = %priority_str, error = %e, "failed to parse priority, defaulting to Reference");
                Priority::Reference
            });

        let created_str: String = row.get("created_at")?;
        let updated_str: String = row.get("updated_at")?;

        Ok(Memory {
            id: id.clone(),
            memory_type,
            content: row.get("content")?,
            instruction: row.get("instruction")?,
            priority,
            source_agent: SourceAgent {
                id: row.get("source_agent_id")?,
                agent_type: row.get("source_agent_type")?,
                session_id: row.get("source_session_id")?,
            },
            namespace: row.get("namespace")?,
            confidence: row.get("confidence")?,
            tags,
            created_at: created_str.parse().unwrap_or_else(|e| {
                warn!(id = %id, raw = %created_str, error = %e, "failed to parse created_at, defaulting to epoch");
                Default::default()
            }),
            updated_at: updated_str.parse().unwrap_or_else(|e| {
                warn!(id = %id, raw = %updated_str, error = %e, "failed to parse updated_at, defaulting to epoch");
                Default::default()
            }),
            ai_generated: row.get::<_, bool>("ai_generated")?,
            human_reviewed: row.get::<_, bool>("human_reviewed")?,
            decay_score: row.get("decay_score")?,
            access_count: row.get("access_count")?,
            last_read_at: row.get::<_, Option<String>>("last_read_at")?.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .inspect_err(|e| warn!(id = %id, raw = %s, error = %e, "failed to parse last_read_at"))
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
            occurred_at: row.get::<_, Option<String>>("occurred_at")?.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .inspect_err(|e| warn!(id = %id, raw = %s, error = %e, "failed to parse occurred_at"))
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
            layer: row
                .get::<_, Option<String>>("layer")?
                .and_then(|s| {
                    serde_json::from_str(&format!("\"{}\"", s))
                        .inspect_err(|e: &serde_json::Error| {
                            warn!(id = %id, raw = %s, error = %e, "failed to parse layer, defaulting to L1")
                        })
                        .ok()
                })
                .unwrap_or(MemoryLayer::L1),
            skill_meta: row
                .get::<_, Option<String>>("skill_meta")?
                .and_then(|s| {
                    serde_json::from_str(&s)
                        .inspect_err(|e: &serde_json::Error| {
                            warn!(id = %id, error = %e, "failed to parse skill_meta, defaulting to None")
                        })
                        .ok()
                }),
            visibility: row
                .get::<_, Option<String>>("visibility")?
                .map(|s| Visibility::parse(&s))
                .unwrap_or_default(),
            superseded_by: row.get("superseded_by")?,
            identity_verified: row.get::<_, bool>("identity_verified")?,
            corroborating_agents,
            source_trace_ids,
            friction_evidence: row.get("friction_evidence")?,
            archived_at: row.get::<_, Option<String>>("archived_at")?.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .inspect_err(|e| warn!(id = %id, raw = %s, error = %e, "failed to parse archived_at"))
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
            // An unparseable stamp degrades to `None` (= never expires) rather
            // than failing the read, matching every other optional timestamp
            // here. That is the safe direction for retrieval: a corrupt stamp
            // keeps the memory visible instead of silently retiring it.
            expires_at: row.get::<_, Option<String>>("expires_at")?.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .inspect_err(|e| warn!(id = %id, raw = %s, error = %e, "failed to parse expires_at, treating as non-expiring"))
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
            proposes_supersede_of: row.get("proposes_supersede_of")?,
        })
    }

    fn row_to_episode(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeRecord> {
        let memory_id: String = row.get("memory_id")?;
        let status_str: String = row.get("status")?;
        let status = OutcomeStatus::parse(&status_str).unwrap_or_else(|| {
            warn!(memory_id = %memory_id, raw = %status_str, "unknown episode status, defaulting to partial");
            OutcomeStatus::Partial
        });
        let occurred_str: String = row.get("occurred_at")?;
        Ok(EpisodeRecord {
            memory_id,
            task: row.get("task")?,
            task_type: row.get("task_type")?,
            status,
            cause: row.get("cause")?,
            lesson: row.get("lesson")?,
            lesson_memory_id: row.get("lesson_memory_id")?,
            occurred_at: occurred_str.parse().unwrap_or_else(|e| {
                warn!(raw = %occurred_str, error = %e, "failed to parse occurred_at, defaulting to epoch");
                Default::default()
            }),
        })
    }
}

fn row_to_relation(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRelation> {
    let relation_id: i64 = row.get("relation_id")?;
    let created_str: String = row.get("created_at")?;
    Ok(MemoryRelation {
        relation_id: Some(relation_id),
        subject_id: row.get("subject_id")?,
        predicate: row.get("predicate")?,
        object_id: row.get("object_id")?,
        object_text: row.get("object_text")?,
        confidence: row.get("confidence")?,
        source_memory_id: row.get("source_memory_id")?,
        created_at: created_str.parse().unwrap_or_default(),
    })
}

/// Storage-layer backstop for [`crate::sensitive`]: `MemoryWriter::save` and
/// `Extractor::extract_guarded` already check earlier in the pipeline, but
/// several callers (imports, restores, review-approve re-saves, agent
/// import) call `MemoryStore::save`/`save_with_embedding` directly,
/// bypassing both. Checking here, in the only `MemoryStore` implementation,
/// means no future caller can add a new bypass path by construction.
fn check_not_sensitive(memory: &Memory) -> Result<()> {
    if crate::sensitive::is_sensitive(&memory.content)
        || memory
            .instruction
            .as_deref()
            .is_some_and(crate::sensitive::is_sensitive)
    {
        return Err(MemVaultError::InvalidInput(
            "content appears to contain sensitive credentials; refusing to save".to_string(),
        ));
    }
    Ok(())
}

#[async_trait]
impl MemoryStore for SqliteStore {
    async fn save(&self, memory: Memory) -> Result<Memory> {
        memory.validate_content()?;
        check_not_sensitive(&memory)?;
        // Canonicalize caller-supplied project spellings before they hit the
        // row (only `project:*` is touched; `global`/`archived:*` pass through).
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();

        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        let tx = conn.transaction()?;

        let corroborating_agents_json = serde_json::to_string(&memory.corroborating_agents)?;
        let source_trace_ids_json = serde_json::to_string(&memory.source_trace_ids)?;

        tx.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at, embedding, layer, skill_meta, superseded_by, visibility,
             identity_verified, corroborating_agents, occurred_at, source_trace_ids, friction_evidence, archived_at, expires_at, proposes_supersede_of)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)",
            rusqlite::params![
                memory.id,
                type_str,
                memory.content,
                memory.instruction,
                priority_str,
                memory.source_agent.id,
                memory.source_agent.agent_type,
                memory.source_agent.session_id,
                memory.namespace,
                memory.confidence,
                tags_json,
                memory.created_at.to_rfc3339(),
                memory.updated_at.to_rfc3339(),
                memory.ai_generated,
                memory.human_reviewed,
                memory.decay_score,
                memory.access_count,
                memory.last_read_at.map(|dt| dt.to_rfc3339()),
                serde_json::to_string(&memory.layer).unwrap_or_default().trim_matches('"').to_string(),
                memory.skill_meta.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()),
                memory.superseded_by,
                memory.visibility.as_str(),
                memory.identity_verified,
                corroborating_agents_json,
                memory.occurred_at.map(|dt| dt.to_rfc3339()),
                source_trace_ids_json,
                memory.friction_evidence,
                memory.archived_at.map(|dt| dt.to_rfc3339()),
                memory.expires_at.map(expiry_stamp),
                memory.proposes_supersede_of,
            ],
        )?;
        // Same transaction as the row insert: either the memory and its FTS
        // index entry both become visible, or neither does — otherwise a
        // crash between the two statements leaves it unsearchable by
        // keyword until the next full-index rebuild.
        Self::fts_insert(&tx, &memory)?;

        tx.commit()?;
        Ok(memory)
    }

    async fn get(&self, id: &str) -> Result<Memory> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.query_row(
            "SELECT * FROM memories WHERE id = ?1",
            rusqlite::params![id],
            Self::row_to_memory,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(id.to_string()),
            other => MemVaultError::Sqlite(other),
        })
    }

    async fn get_history_snapshot(&self, id: &str) -> Result<Option<Memory>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let snapshot: Option<String> = conn
            .query_row(
                "SELECT snapshot FROM memory_history
                 WHERE memory_id = ?1 ORDER BY history_id DESC LIMIT 1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(MemVaultError::Sqlite)?;

        match snapshot {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    async fn get_persisted_snapshot(&self, _id: &str) -> Result<Option<Memory>> {
        // SQLite has no second, file-backed source of truth: the live row and
        // memory_history are already covered by `get`/`get_history_snapshot`.
        Ok(None)
    }

    async fn update(&self, memory: Memory) -> Result<Memory> {
        memory.validate_content()?;
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();

        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction()?;
        Self::write_update_in_tx(&tx, &memory)?;
        tx.commit()?;
        Ok(memory)
    }

    async fn approve_supersede(
        &self,
        target_id: &str,
        proposal: Memory,
    ) -> Result<ApproveSupersedeOutcome> {
        proposal.validate_content()?;
        if proposal.id == target_id {
            return Err(MemVaultError::InvalidInput(
                "a memory cannot supersede itself".to_string(),
            ));
        }
        let mut proposal = proposal;
        proposal.namespace =
            crate::models::canonical_project_namespace(&proposal.namespace).into_owned();

        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        // IMMEDIATE: take the write lock up front. The target check below
        // reads *inside* the transaction, so holding the lock across
        // read-check-both-writes is what makes this a compare-and-swap; with
        // a deferred transaction two concurrent approvals could both read
        // "unclaimed" from pre-lock snapshots and degrade this to the very
        // check-then-act it replaces (audit #6).
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let target = tx
            .query_row(
                "SELECT * FROM memories WHERE id = ?1",
                rusqlite::params![target_id],
                Self::row_to_memory,
            )
            .optional()?;
        let Some(mut target) = target else {
            // Nothing written; the tx drops uncommitted.
            return Ok(ApproveSupersedeOutcome::TargetGone);
        };
        match target.superseded_by.as_deref() {
            // Retry arm: a previous attempt retired the target and then
            // failed to persist the approval. Complete rather than refuse —
            // recovery must be the one path that can always run. The target
            // already points at this proposal, so it is not rewritten.
            Some(already) if already == proposal.id => {}
            Some(other) => {
                return Ok(ApproveSupersedeOutcome::AlreadyClaimed {
                    by: other.to_string(),
                });
            }
            None => {
                target.superseded_by = Some(proposal.id.clone());
                target.layer = MemoryLayer::L0;
                target.updated_at = chrono::Utc::now();
                Self::write_update_in_tx(&tx, &target)?;
            }
        }

        proposal.human_reviewed = true;
        proposal.updated_at = chrono::Utc::now();
        // Consume the link defensively: `proposal::approve_memory` takes it
        // before calling, but the storage contract stands on its own — an
        // approved row is a live memory, never a pending proposal.
        proposal.proposes_supersede_of = None;
        // If the proposal row vanished mid-flight this is NotFound, and the
        // whole transaction — the target's retirement included — rolls back.
        Self::write_update_in_tx(&tx, &proposal)?;

        tx.commit()?;
        Ok(ApproveSupersedeOutcome::Approved(Box::new(proposal)))
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let tx = conn.transaction()?;

        if let Ok(old) = tx.query_row(
            "SELECT * FROM memories WHERE id = ?1",
            rusqlite::params![id],
            Self::row_to_memory,
        ) {
            Self::write_history(&tx, &old, "delete")?;
        }

        let rows = tx.execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(id.to_string()));
        }

        Self::fts_delete(&tx, id)?;

        tx.commit()?;
        Ok(())
    }

    async fn purge(&self, id: &str) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let tx = conn.transaction()?;

        // Unlike delete(), no pre-DELETE snapshot into memory_history —
        // purge() is the path that must leave nothing restorable. History is
        // erased FIRST so that an already-soft-deleted id (live row gone,
        // snapshot still restorable) still gets its checkpoints purged —
        // otherwise the early NotFound on the live row used to roll back the
        // whole transaction and leave "erased" content restorable (H6).
        let history_rows = tx.execute(
            "DELETE FROM memory_history WHERE memory_id = ?1",
            rusqlite::params![id],
        )?;
        // The historical full-text index holds the *text* of every snapshot, so
        // erasing the snapshots without erasing their index entries would leave
        // the forgotten content searchable — a direct violation of the guarantee
        // this function's own comment makes, and of §9.5's promise that only a
        // content hash survives in `forgotten_log`. Same transaction, so the two
        // can never disagree.
        tx.execute(
            "DELETE FROM memory_history_fts WHERE memory_id = ?1",
            rusqlite::params![id],
        )?;
        let rows = tx.execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
        if rows == 0 {
            // Idempotent erasure: a missing live row is only an error when
            // there was never anything at all (no row, no history). A row
            // that was previously soft-deleted had its snapshots erased
            // above, so the erasure is complete.
            tx.commit()?;
            return if history_rows == 0 {
                Err(MemVaultError::NotFound(id.to_string()))
            } else {
                Ok(())
            };
        }

        Self::fts_delete(&tx, id)?;

        tx.commit()?;
        Ok(())
    }

    async fn record_forgotten(&self, entry: &ForgottenLogEntry) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let priority_str = serde_json::to_string(&entry.priority)?;
        let priority_str = priority_str.trim_matches('"');
        conn.execute(
            "INSERT INTO forgotten_log (memory_id, content_hash, namespace, priority, visibility, scope, reason, requested_by, must_override, erased_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                entry.memory_id,
                entry.content_hash,
                entry.namespace,
                priority_str,
                entry.visibility.as_str(),
                entry.scope.as_str(),
                entry.reason,
                entry.requested_by,
                entry.must_override,
                entry.erased_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    async fn list_forgotten_log(
        &self,
        memory_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ForgottenLogEntry>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let mut stmt;
        let rows = if let Some(id) = memory_id {
            stmt = conn.prepare(
                "SELECT log_id, memory_id, content_hash, namespace, priority, visibility, scope, reason, requested_by, must_override, erased_at
                 FROM forgotten_log WHERE memory_id = ?1 ORDER BY log_id DESC LIMIT ?2",
            )?;
            stmt.query_map(
                rusqlite::params![id, limit as i64],
                Self::row_to_forgotten_log_entry,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt = conn.prepare(
                "SELECT log_id, memory_id, content_hash, namespace, priority, visibility, scope, reason, requested_by, must_override, erased_at
                 FROM forgotten_log ORDER BY log_id DESC LIMIT ?1",
            )?;
            stmt.query_map(
                rusqlite::params![limit as i64],
                Self::row_to_forgotten_log_entry,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        Ok(rows)
    }

    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // top_k=0 is the caller's explicit "zero results" cap — every search
        // mode must honor it identically, so short-circuit before any FTS
        // work. (It used to be clamped up to 1, which made keyword mode leak
        // a single row where semantic/hybrid returned [].) The upper bound
        // still keeps LIMIT bounded for enormous values.
        if query.top_k == 0 {
            return Ok(SearchOutcome {
                results: Vec::new(),
                keyword_tier: KeywordTier::None,
            });
        }
        let top_k = query.top_k.min(crate::models::MAX_TOP_K);
        // Fetch more candidates than requested for post-retrieval relevance
        // re-scoring (same multiplier as the old LIKE implementation).
        let candidate_limit = top_k * 3;

        let query_text = query.query.trim();

        let (rows, used_tier): (Vec<Memory>, KeywordTier) = if query_text.is_empty() {
            // No keyword constraint: list by priority/decay/recency as before.
            let mut sql = String::from("SELECT * FROM memories WHERE 1=1");
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            append_filter_clauses(&mut sql, &mut params, "", &query);
            sql.push_str(" ORDER BY CASE priority WHEN 'MUST' THEN 0 WHEN 'REFERENCE' THEN 1 ELSE 2 END, decay_score DESC, updated_at DESC");
            sql.push_str(&format!(" LIMIT {candidate_limit}"));

            let mut stmt = conn.prepare(&sql)?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), Self::row_to_memory)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            (rows, KeywordTier::None)
        } else {
            // Tiered FTS5 recall. Tiers go from most precise to most relaxed;
            // the first tier that returns anything wins, and WHICH tier won is
            // reported to the caller — a silently relaxed match would be
            // mistaken for an exact one.
            let tiers = crate::fts::query_token_tiers(query_text);
            let mut attempts: Vec<(KeywordTier, String)> = Vec::new();
            if let Some(strict_tokens) = tiers.first() {
                attempts.push((
                    KeywordTier::Strict,
                    crate::fts::build_match_expr(strict_tokens)?,
                ));
            }
            if let Some(relaxed_tokens) = tiers.get(1) {
                attempts.push((
                    KeywordTier::RelaxedUnigram,
                    crate::fts::build_match_expr(relaxed_tokens)?,
                ));
            }
            // Last resort: OR over all query tokens plus tokenized synonym
            // expansions. Any token matching counts, mirroring the old
            // word-OR semantics — but only after both AND tiers came up empty.
            let mut fallback_tokens: Vec<String> = tiers.iter().flatten().cloned().collect();
            for word in crate::query_expand::expand_query(query_text) {
                fallback_tokens.extend(crate::fts::tokenize(&word));
            }
            if !fallback_tokens.is_empty() {
                attempts.push((
                    KeywordTier::SynonymFallback,
                    crate::fts::build_match_expr_or(&fallback_tokens)?,
                ));
            }

            let mut found: Vec<Memory> = Vec::new();
            let mut used = KeywordTier::None;
            for (tier, match_expr) in attempts {
                let mut sql = String::from(
                    "SELECT m.*, bm25(memories_fts) AS fts_rank \
                     FROM memories_fts \
                     JOIN memories m ON m.id = memories_fts.memory_id \
                     WHERE memories_fts MATCH ?1",
                );
                let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(match_expr)];
                append_filter_clauses(&mut sql, &mut params, "m.", &query);
                // bm25() is cost: lower = better match.
                sql.push_str(" ORDER BY fts_rank ASC");
                sql.push_str(&format!(" LIMIT {candidate_limit}"));

                let mut stmt = conn.prepare(&sql)?;
                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    params.iter().map(|p| p.as_ref()).collect();
                let fetched = stmt
                    .query_map(param_refs.as_slice(), Self::row_to_memory)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;

                if !fetched.is_empty() {
                    found = fetched;
                    used = tier;
                    break;
                }
            }
            if used != KeywordTier::Strict {
                debug!(tier = ?used, query = %query_text, "keyword search used a relaxed match tier");
            }
            (found, used)
        };

        // Re-score candidates. The FTS path already ordered by bm25, but the
        // composite score below is what downstream reranking and MUST-first
        // sorting consume, so it is computed for every path.
        let search_words = if query_text.is_empty() {
            Vec::new()
        } else {
            crate::query_expand::expand_query(query_text)
        };

        let now = chrono::Utc::now();
        let mut results = Vec::new();

        for memory in rows {
            let score = Self::compute_relevance_score(&memory, &search_words, now);
            results.push(SearchResult {
                memory,
                score,
                hit_sources: Vec::new(),
                as_of_source: None,
            });
        }

        // Point-in-time (valid-time) projection, applied before re-ranking so
        // `top_k` is filled with memories that existed at `as_of` rather than
        // padded with current-only ones. The recall above ran against the
        // *current* FTS index, so this step decides which hits were in effect
        // then and rewrites each to the state it held.
        if let Some(as_of) = query.as_of {
            results = Self::project_results_as_of(&conn, results, as_of)?;
            // Then widen recall with the historical index: a memory reworded
            // since `as_of` matches neither the current FTS text nor anything
            // above, and without this pass "what did we believe about X in
            // August" would miss exactly the memories that have changed since —
            // which are the interesting ones. See `search_history_at`.
            let historical = Self::search_history_at(&conn, &query, as_of, top_k)?;
            if !historical.is_empty() {
                let mut present: std::collections::HashMap<String, usize> = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| (r.memory.id.clone(), i))
                    .collect();
                for hit in historical {
                    match present.get(&hit.memory.id) {
                        // Already recalled from current text and projected to the
                        // same state. Merge the provenance rather than adding a
                        // second row: two entries for one memory would double its
                        // weight downstream and misreport what recalled it.
                        Some(&i) => {
                            results[i].hit_sources.extend(hit.hit_sources);
                        }
                        None => {
                            present.insert(hit.memory.id.clone(), results.len());
                            results.push(hit);
                        }
                    }
                }
            }
        }

        // Re-sort by computed relevance score (MUST still first)
        results.sort_by(|a, b| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        results.truncate(top_k);
        // Provenance for the single-path (keyword) list: rank as returned.
        // Entries that already carry provenance are left alone — an `as_of` search
        // may have recalled them from the *historical* index, and overwriting that
        // with `kw#N` would report a match on current text that never happened,
        // erasing the one signal that distinguishes the two.
        for (idx, r) in results.iter_mut().enumerate() {
            if r.hit_sources.is_empty() {
                r.hit_sources = vec![crate::models::HitSource::Keyword { rank: idx + 1 }];
            }
        }
        Ok(SearchOutcome {
            results,
            keyword_tier: used_tier,
        })
    }

    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory> {
        memory.validate_content()?;
        check_not_sensitive(&memory)?;
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();

        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');
        // New embeddings are stored int8-quantized (~1/4 of the f32 size at
        // near-identical ranking quality); the fmt column tells readers apart.
        let blob = Self::embedding_to_int8_blob(&embedding);

        let corroborating_agents_json = serde_json::to_string(&memory.corroborating_agents)?;
        let source_trace_ids_json = serde_json::to_string(&memory.source_trace_ids)?;

        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at, embedding, embedding_fmt, layer, skill_meta,
             identity_verified, corroborating_agents, occurred_at, source_trace_ids, friction_evidence, archived_at, expires_at, proposes_supersede_of)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, 1, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
            rusqlite::params![
                memory.id,
                type_str,
                memory.content,
                memory.instruction,
                priority_str,
                memory.source_agent.id,
                memory.source_agent.agent_type,
                memory.source_agent.session_id,
                memory.namespace,
                memory.confidence,
                tags_json,
                memory.created_at.to_rfc3339(),
                memory.updated_at.to_rfc3339(),
                memory.ai_generated,
                memory.human_reviewed,
                memory.decay_score,
                memory.access_count,
                memory.last_read_at.map(|dt| dt.to_rfc3339()),
                blob,
                serde_json::to_string(&memory.layer)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string(),
                memory
                    .skill_meta
                    .as_ref()
                    .map(|sm| serde_json::to_string(sm).unwrap_or_default()),
                memory.identity_verified,
                corroborating_agents_json,
                memory.occurred_at.map(|dt| dt.to_rfc3339()),
                source_trace_ids_json,
                memory.friction_evidence,
                memory.archived_at.map(|dt| dt.to_rfc3339()),
                memory.expires_at.map(expiry_stamp),
                memory.proposes_supersede_of,
            ],
        )?;
        Self::fts_insert(&tx, &memory)?;

        tx.commit()?;
        Ok(memory)
    }

    async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        namespace: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        // top_k=0 → the caller asked for zero results; honor it without
        // scanning (same contract as `search`).
        if top_k == 0 {
            return Ok(Vec::new());
        }
        // Defense in depth: API boundaries already clamp, but a huge top_k
        // reaching this trait method directly must not be able to size the
        // truncated result set (CWE-770). Same bound as `search`.
        let top_k = top_k.min(crate::models::MAX_TOP_K);

        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Cap the number of candidate rows pulled into memory for cosine-similarity
        // scoring. Without this, an unfiltered vector_search on a large table does
        // a full unbounded scan + per-row deserialization before any ranking happens.
        const MAX_VECTOR_SCAN_CANDIDATES: i64 = 2000;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(ns) = namespace.map(crate::models::canonical_project_namespace) {
                (
                    "SELECT * FROM memories WHERE embedding IS NOT NULL AND namespace = ?1 \
                     AND superseded_by IS NULL AND proposes_supersede_of IS NULL \
                     AND (expires_at IS NULL OR expires_at > ?2) \
                 ORDER BY updated_at DESC LIMIT ?3"
                        .to_string(),
                    vec![
                        Box::new(ns.as_ref().to_string()),
                        Box::new(expiry_stamp(chrono::Utc::now())),
                        Box::new(MAX_VECTOR_SCAN_CANDIDATES),
                    ],
                )
            } else {
                (
                    "SELECT * FROM memories WHERE embedding IS NOT NULL \
                     AND superseded_by IS NULL AND proposes_supersede_of IS NULL \
                     AND (expires_at IS NULL OR expires_at > ?1) \
                 ORDER BY updated_at DESC LIMIT ?2"
                        .to_string(),
                    vec![
                        Box::new(expiry_stamp(chrono::Utc::now())),
                        Box::new(MAX_VECTOR_SCAN_CANDIDATES),
                    ],
                )
            };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut scored: Vec<(Memory, f32)> = Vec::new();

        // Quantize the query once for all int8 rows.
        let query_quantized = crate::embedding::quantize_int8(query_embedding);

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let memory = Self::row_to_memory(row)?;
            let blob: Vec<u8> = row.get("embedding")?;
            let fmt: i64 = row.get("embedding_fmt")?;
            Ok((memory, blob, fmt))
        })?;

        for row in rows {
            let (memory, blob, fmt) = row?;
            let sim = if fmt == 1 && blob.len() >= 4 {
                // int8 row: compare in quantized space (both sides were
                // L2-normalized before quantization, so inner product with
                // the two scales is the cosine).
                let row_dim = blob.len() - 4;
                if row_dim != query_quantized.dim {
                    // Dimension mismatch means the embedding model changed.
                    // Skip the row rather than erroring so re-embedding can
                    // migrate the corpus gradually.
                    debug!(
                        id = %memory.id,
                        row_dim,
                        query_dim = query_quantized.dim,
                        "skipping embedding with mismatched dimension"
                    );
                    continue;
                }
                let scale = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
                let row_q = crate::embedding::QuantizedVector {
                    data: blob[4..].iter().map(|&b| b as i8).collect(),
                    scale,
                    dim: row_dim,
                };
                crate::embedding::cosine_int8(&query_quantized, &row_q)
            } else {
                // Legacy f32 row: full-precision cosine (also tolerates
                // mismatched dims by returning 0 — see cosine_similarity).
                let emb = Self::blob_to_embedding(&blob);
                cosine_similarity(query_embedding, &emb)
            };
            scored.push((memory, sim));
        }

        if scored.len() as i64 >= MAX_VECTOR_SCAN_CANDIDATES {
            warn!(
                namespace = ?namespace,
                cap = MAX_VECTOR_SCAN_CANDIDATES,
                "vector_search candidate set hit the scan cap; results may miss \
                 older embedded memories beyond this window. Consider a namespace \
                 filter or a proper vector index for large datasets."
            );
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        debug!(candidates = scored.len(), "vector search complete");

        Ok(scored
            .into_iter()
            .enumerate()
            .map(|(idx, (memory, sim))| SearchResult {
                score: sim as f64,
                memory,
                hit_sources: vec![crate::models::HitSource::Vector { rank: idx + 1 }],
                as_of_source: None,
            })
            .collect())
    }

    async fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let result: (Option<Vec<u8>>, i64) = conn
            .query_row(
                "SELECT embedding, embedding_fmt FROM memories WHERE id = ?1",
                rusqlite::params![id],
                |row| -> rusqlite::Result<(Option<Vec<u8>>, i64)> {
                    Ok((row.get(0)?, row.get(1)?))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(id.to_string()),
                other => MemVaultError::Sqlite(other),
            })?;

        Ok(result.0.map(|blob| Self::decode_embedding(&blob, result.1)))
    }

    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let blob = Self::embedding_to_int8_blob(&embedding);

        let rows = conn.execute(
            "UPDATE memories SET embedding = ?2, embedding_fmt = 1 WHERE id = ?1",
            rusqlite::params![id, blob],
        )?;

        if rows == 0 {
            return Err(MemVaultError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn clear_embedding(&self, id: &str) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let rows = conn.execute(
            "UPDATE memories SET embedding = NULL, embedding_fmt = 0 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn sync_state_hash(&self) -> Result<u64> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let (count, max_updated): (i64, Option<String>) = conn.query_row(
            "SELECT COUNT(*), MAX(updated_at) FROM memories",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        // Simple hash: xor count with a hash of the max_updated string
        let updated_hash: u64 = max_updated
            .as_deref()
            .map(|s| {
                s.bytes()
                    .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64))
            })
            .unwrap_or(0);
        Ok((count as u64)
            .wrapping_mul(100_003)
            .wrapping_add(updated_hash))
    }

    async fn list_without_embedding(&self, limit: usize) -> Result<Vec<Memory>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT * FROM memories WHERE embedding IS NULL LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], Self::row_to_memory)?;
        let mut memories = Vec::new();
        for row in rows {
            memories.push(row?);
        }
        Ok(memories)
    }

    async fn record_access(&self, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }

        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();

        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "UPDATE memories SET access_count = access_count + 1, last_read_at = ?1 WHERE id IN ({})",
            placeholders.join(",")
        );

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(ids.len() + 1);
        params.push(Box::new(now));
        for id in ids {
            params.push(Box::new(id.clone()));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let changed = conn.execute(&sql, param_refs.as_slice())?;

        // Ground truth for callers: ids that do not exist match zero rows, so
        // reporting `ids.len()` (the old behavior) overstated the update.
        debug!(
            requested = ids.len(),
            changed, "record_access batch complete"
        );
        Ok(changed)
    }

    async fn list(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ns) =
            namespace.map(crate::models::canonical_project_namespace)
        {
            (
                "SELECT * FROM memories WHERE namespace = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(ns.as_ref().to_string()), Box::new(limit as i64), Box::new(offset as i64)],
            )
        } else {
            (
                "SELECT * FROM memories ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2".to_string(),
                vec![Box::new(limit as i64), Box::new(offset as i64)],
            )
        };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_memory)?;

        let mut memories = Vec::new();
        for row in rows {
            memories.push(row?);
        }

        Ok(memories)
    }

    async fn get_as_of(
        &self,
        id: &str,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<MemoryAtTime>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Self::reconstruct_at(&conn, id, as_of)
    }

    async fn list_as_of(
        &self,
        as_of: chrono::DateTime<chrono::Utc>,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryAtTime>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let want_ns = namespace.map(crate::models::canonical_project_namespace);

        // Candidate ids are the union of everything live and everything that
        // survives only in history. The union is unavoidable: a memory deleted
        // *after* `as_of` was still current truth *at* `as_of`, and its
        // namespace is only recoverable from the snapshot — so namespace
        // filtering has to happen after reconstruction, not in SQL.
        let mut ids: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT id FROM memories")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                ids.push(row?);
            }
        }
        {
            let mut stmt = conn.prepare("SELECT DISTINCT memory_id FROM memory_history")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                ids.push(row?);
            }
        }
        ids.sort();
        ids.dedup();

        let mut at_time: Vec<MemoryAtTime> = Vec::new();
        for id in ids {
            let Some(state) = Self::reconstruct_at(&conn, &id, as_of)? else {
                continue;
            };
            if let Some(ref want) = want_ns {
                let got = crate::models::canonical_project_namespace(&state.memory.namespace);
                if got.as_ref() != want.as_ref() {
                    continue;
                }
            }
            at_time.push(state);
        }

        // Newest-first by the *reconstructed* state's own `updated_at`, so the
        // ordering describes the past rather than the present. Sorted in memory
        // because rows come from two tables with no shared ordering key.
        at_time.sort_by_key(|m| std::cmp::Reverse(m.memory.updated_at));
        Ok(at_time.into_iter().skip(offset).take(limit).collect())
    }

    async fn list_pending(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ns) =
            namespace.map(crate::models::canonical_project_namespace)
        {
            (
                "SELECT * FROM memories WHERE human_reviewed = 0 AND namespace = ?1 ORDER BY created_at ASC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(ns.as_ref().to_string()), Box::new(limit as i64), Box::new(offset as i64)],
            )
        } else {
            (
                "SELECT * FROM memories WHERE human_reviewed = 0 ORDER BY created_at ASC LIMIT ?1 OFFSET ?2".to_string(),
                vec![Box::new(limit as i64), Box::new(offset as i64)],
            )
        };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_memory)?;

        let mut memories = Vec::new();
        for row in rows {
            memories.push(row?);
        }

        Ok(memories)
    }

    async fn record_episode(&self, episode: EpisodeRecord) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // The episode memory must exist first: the FK (with cascade) would
        // reject the insert otherwise, and a record without a memory row
        // would be unsearchable. Fail loudly rather than drop the record.
        let exists: bool = conn
            .query_row(
                "SELECT count(*) FROM memories WHERE id = ?1",
                rusqlite::params![episode.memory_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)?;
        if !exists {
            return Err(MemVaultError::NotFound(episode.memory_id));
        }

        conn.execute(
            "INSERT INTO episodes (memory_id, task, task_type, status, cause, lesson, lesson_memory_id, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                episode.memory_id,
                episode.task,
                episode.task_type,
                episode.status.as_str(),
                episode.cause,
                episode.lesson,
                episode.lesson_memory_id,
                episode.occurred_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    async fn get_episode(&self, memory_id: &str) -> Result<EpisodeRecord> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.query_row(
            "SELECT * FROM episodes WHERE memory_id = ?1",
            rusqlite::params![memory_id],
            Self::row_to_episode,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(memory_id.to_string()),
            other => MemVaultError::Sqlite(other),
        })
    }

    async fn list_episodes(&self, filter: EpisodeFilter) -> Result<Vec<EpisodeRecord>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Join memories for the namespace filter; episodes alone carry no
        // namespace. Newest first — callers want the freshest experiences.
        let mut sql =
            String::from("SELECT e.* FROM episodes e JOIN memories m ON m.id = e.memory_id");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut clauses: Vec<String> = Vec::new();

        if let Some(ref tt) = filter.task_type {
            params.push(Box::new(tt.clone()));
            clauses.push(format!("e.task_type = ?{}", params.len()));
        }
        if let Some(status) = filter.status {
            params.push(Box::new(status.as_str().to_string()));
            clauses.push(format!("e.status = ?{}", params.len()));
        }
        if let Some(ref ns) = filter.namespace {
            params.push(Box::new(ns.clone()));
            clauses.push(format!("m.namespace = ?{}", params.len()));
        }
        if let Some(ref sid) = filter.session_id {
            params.push(Box::new(sid.clone()));
            clauses.push(format!("m.source_session_id = ?{}", params.len()));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        params.push(Box::new(filter.limit.max(1) as i64));
        sql.push_str(&format!(
            " ORDER BY e.occurred_at DESC LIMIT ?{}",
            params.len()
        ));

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_episode)?;

        let mut episodes = Vec::new();
        for row in rows {
            episodes.push(row?);
        }
        Ok(episodes)
    }

    async fn update_episode_lesson(
        &self,
        memory_id: &str,
        lesson: &str,
        lesson_memory_id: Option<&str>,
    ) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let rows = conn.execute(
            "UPDATE episodes SET lesson = ?2, lesson_memory_id = ?3 WHERE memory_id = ?1",
            rusqlite::params![memory_id, lesson, lesson_memory_id],
        )?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(memory_id.to_string()));
        }
        Ok(())
    }

    async fn record_skill_injection(&self, skill_memory_id: &str) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO skill_stats (skill_memory_id, injected_count, success_count, failure_count, updated_at)
             VALUES (?1, 1, 0, 0, ?2)
             ON CONFLICT(skill_memory_id) DO UPDATE SET
                injected_count = injected_count + 1,
                updated_at = excluded.updated_at",
            rusqlite::params![skill_memory_id, now],
        )?;
        Ok(())
    }

    async fn record_skill_outcome(&self, skill_memory_id: &str, success: bool) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO skill_stats (skill_memory_id, injected_count, success_count, failure_count, updated_at)
             VALUES (?1, 0, ?2, ?3, ?4)
             ON CONFLICT(skill_memory_id) DO UPDATE SET
                success_count = success_count + ?2,
                failure_count = failure_count + ?3,
                updated_at = excluded.updated_at",
            rusqlite::params![skill_memory_id, success as i64, !success as i64, now],
        )?;
        Ok(())
    }

    async fn get_skill_stats(&self, skill_memory_id: &str) -> Result<Option<SkillStats>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let stats = conn
            .query_row(
                "SELECT skill_memory_id, injected_count, success_count, failure_count
                 FROM skill_stats WHERE skill_memory_id = ?1",
                rusqlite::params![skill_memory_id],
                |row| {
                    Ok(SkillStats {
                        skill_memory_id: row.get(0)?,
                        injected_count: row.get::<_, i64>(1)? as u32,
                        success_count: row.get::<_, i64>(2)? as u32,
                        failure_count: row.get::<_, i64>(3)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(stats)
    }

    async fn add_relation(&self, relation: MemoryRelation) -> Result<i64> {
        // Exactly one object form: an object_id pointing at a memory, or
        // free text — never both, never neither.
        match (&relation.object_id, &relation.object_text) {
            (Some(_), Some(_)) => {
                return Err(MemVaultError::InvalidInput(
                    "relation object must be either object_id or object_text, not both".to_string(),
                ));
            }
            (None, None) => {
                return Err(MemVaultError::InvalidInput(
                    "relation needs an object_id or object_text".to_string(),
                ));
            }
            _ => {}
        }
        if relation.predicate.trim().is_empty() {
            return Err(MemVaultError::InvalidInput(
                "relation predicate must not be empty".to_string(),
            ));
        }

        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Endpoint existence checks turn FK violations into readable errors.
        let subject_exists: bool = conn.query_row(
            "SELECT count(*) FROM memories WHERE id = ?1",
            rusqlite::params![relation.subject_id],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if !subject_exists {
            return Err(MemVaultError::NotFound(relation.subject_id));
        }
        if let Some(ref object_id) = relation.object_id {
            let object_exists: bool = conn.query_row(
                "SELECT count(*) FROM memories WHERE id = ?1",
                rusqlite::params![object_id],
                |r| r.get::<_, i64>(0),
            )? > 0;
            if !object_exists {
                return Err(MemVaultError::NotFound(object_id.clone()));
            }
        }

        conn.execute(
            "INSERT INTO memory_relations (subject_id, predicate, object_id, object_text, confidence, source_memory_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                relation.subject_id,
                relation.predicate,
                relation.object_id,
                relation.object_text,
                relation.confidence,
                relation.source_memory_id,
                relation.created_at.to_rfc3339(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn relations_of_subject(&self, memory_id: &str) -> Result<Vec<MemoryRelation>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT relation_id, subject_id, predicate, object_id, object_text, confidence, source_memory_id, created_at
             FROM memory_relations WHERE subject_id = ?1 ORDER BY relation_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![memory_id], row_to_relation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(MemVaultError::Sqlite)
    }

    async fn relations_of_object(&self, memory_id: &str) -> Result<Vec<MemoryRelation>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT relation_id, subject_id, predicate, object_id, object_text, confidence, source_memory_id, created_at
             FROM memory_relations WHERE object_id = ?1 ORDER BY relation_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![memory_id], row_to_relation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(MemVaultError::Sqlite)
    }

    async fn delete_relation(&self, relation_id: i64) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let rows = conn.execute(
            "DELETE FROM memory_relations WHERE relation_id = ?1",
            rusqlite::params![relation_id],
        )?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(relation_id.to_string()));
        }
        Ok(())
    }

    async fn supersede(&self, old_id: &str, new_id: &str) -> Result<()> {
        if old_id == new_id {
            return Err(MemVaultError::InvalidInput(
                "a memory cannot supersede itself".to_string(),
            ));
        }
        let old = MemoryStore::get(self, old_id).await?;
        // get(new_id) validates existence (NotFound otherwise).
        MemoryStore::get(self, new_id).await?;

        let mut updated = old;
        updated.superseded_by = Some(new_id.to_string());
        updated.layer = MemoryLayer::L0;
        updated.updated_at = chrono::Utc::now();
        MemoryStore::update(self, updated).await?;
        Ok(())
    }

    async fn list_shared(&self, limit: usize) -> Result<Vec<Memory>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let limit = limit.clamp(1, 1000);
        // Expired shared memories must not reach a session: this path feeds
        // injection into *every* session regardless of namespace, so a
        // temporary team fact that outlived its window would otherwise be
        // re-injected forever. Filtered against now — the team pool is always
        // the live view, never a point-in-time one.
        let mut stmt = conn.prepare(
            "SELECT * FROM memories WHERE visibility = 'shared' AND superseded_by IS NULL
             AND proposes_supersede_of IS NULL
             AND (expires_at IS NULL OR expires_at > ?2)
             ORDER BY CASE priority WHEN 'MUST' THEN 0 WHEN 'REFERENCE' THEN 1 ELSE 2 END, updated_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![limit as i64, expiry_stamp(chrono::Utc::now())],
            Self::row_to_memory,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(MemVaultError::Sqlite)
    }

    async fn get_trace_watermark(
        &self,
        agent_key: &str,
        session_id: &str,
    ) -> Result<Option<TraceWatermark>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let watermark = conn
            .query_row(
                "SELECT agent_key, session_id, last_seq, updated_at
                 FROM trace_watermark
                 WHERE agent_key = ?1 AND session_id = ?2",
                rusqlite::params![agent_key, session_id],
                |row| {
                    let updated_str: String = row.get("updated_at")?;
                    Ok(TraceWatermark {
                        agent_key: row.get("agent_key")?,
                        session_id: row.get("session_id")?,
                        last_seq: row.get::<_, i64>("last_seq")? as usize,
                        updated_at: updated_str.parse().unwrap_or_else(|e| {
                            warn!(agent_key = %agent_key, session_id = %session_id, raw = %updated_str, error = %e, "failed to parse trace_watermark updated_at, defaulting to epoch");
                            Default::default()
                        }),
                    })
                },
            )
            .optional()?;
        Ok(watermark)
    }

    async fn set_trace_watermark(&self, watermark: &TraceWatermark) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT INTO trace_watermark (agent_key, session_id, last_seq, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_key, session_id) DO UPDATE SET
                last_seq = excluded.last_seq,
                updated_at = excluded.updated_at",
            rusqlite::params![
                watermark.agent_key,
                watermark.session_id,
                watermark.last_seq as i64,
                watermark.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent() -> SourceAgent {
        SourceAgent {
            id: "test-agent".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: Some("sess_test".to_string()),
        }
    }

    #[tokio::test]
    async fn test_save_and_get() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Preference,
            "user prefers Python".to_string(),
            Priority::Must,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.content, "user prefers Python");
        assert_eq!(retrieved.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_occurred_at_roundtrips_through_save_update_and_get() {
        // occurred_at (event time, caller-supplied) is distinct from
        // created_at (ingestion time) and must survive save → get and
        // update → get; None must stay None (legacy rows / callers without
        // a date).
        let store = SqliteStore::in_memory().unwrap();
        let occurred = chrono::DateTime::parse_from_rfc3339("2023-05-07T13:56:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let mut mem = Memory::new(
            MemoryType::Episode,
            "Caroline went to a support group".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.occurred_at = Some(occurred);
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.occurred_at, Some(occurred));
        assert_ne!(
            retrieved.created_at, occurred,
            "created_at stays ingestion time, not event time"
        );

        // update() must not silently drop it
        store.update(retrieved.clone()).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap().occurred_at, Some(occurred));

        // A memory without occurred_at reads back as None
        let plain = Memory::new(
            MemoryType::Fact,
            "no date supplied".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let plain_id = plain.id.clone();
        store.save(plain).await.unwrap();
        assert_eq!(store.get(&plain_id).await.unwrap().occurred_at, None);
    }

    #[tokio::test]
    async fn test_occurred_at_malformed_db_value_degrades_to_none() {
        // A corrupted (non-RFC3339) occurred_at in the DB must degrade to
        // None with a warning — never panic, never poison the row.
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "row with corrupted provenance".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        {
            let conn = store.pool.get().unwrap();
            conn.execute(
                "UPDATE memories SET occurred_at = 'not-a-date' WHERE id = ?1",
                [&id],
            )
            .unwrap();
        }

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.occurred_at, None);
    }

    #[tokio::test]
    async fn test_update_clears_occurred_at_to_null() {
        // update() with None must write NULL back — Some() must not be a
        // one-way ratchet.
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "provenance can be retracted".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.occurred_at = Some(
            chrono::DateTime::parse_from_rfc3339("2023-05-07T13:56:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        assert!(store.get(&id).await.unwrap().occurred_at.is_some());

        let mut cleared = store.get(&id).await.unwrap();
        cleared.occurred_at = None;
        store.update(cleared).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap().occurred_at, None);
    }

    #[tokio::test]
    async fn test_save_rejects_sensitive_content_bypassing_memory_writer() {
        // Import/restore/review-approve call `store.save` directly, skipping
        // `MemoryWriter`'s own check — this is the storage-layer backstop.
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "db password=Sup3rSecret!42".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        let result = store.save(mem).await;
        assert!(result.is_err());
        assert!(store.get(&id).await.is_err(), "nothing must be persisted");
    }

    #[tokio::test]
    async fn test_save_with_embedding_rejects_sensitive_content() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "api_key=sk-abcdefghijklmno".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        let result = store.save_with_embedding(mem, vec![0.1, 0.2, 0.3]).await;
        assert!(result.is_err());
        assert!(store.get(&id).await.is_err(), "nothing must be persisted");
    }

    #[tokio::test]
    async fn test_search_by_keyword() {
        let store = SqliteStore::in_memory().unwrap();

        let m1 = Memory::new(
            MemoryType::Preference,
            "user prefers Python".to_string(),
            Priority::Must,
            test_agent(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "project uses PostgreSQL".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let results = store
            .search(SearchQuery::new("Python".to_string()))
            .await
            .unwrap()
            .results;
        assert_eq!(results.len(), 1);
        assert!(results[0].memory.content.contains("Python"));
    }

    #[tokio::test]
    async fn test_delete() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "temp".to_string(),
            Priority::Background,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        store.delete(&id).await.unwrap();
        assert!(store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let store = SqliteStore::in_memory().unwrap();

        let m1 = Memory::new(
            MemoryType::Fact,
            "low priority fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let m2 = Memory::new(
            MemoryType::Preference,
            "must rule".to_string(),
            Priority::Must,
            test_agent(),
        );
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let results = store
            .search(SearchQuery {
                query: String::new(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_save_with_embedding() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "test embedding".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        let emb = vec![0.1, 0.2, 0.3, 0.4];

        store.save_with_embedding(mem, emb.clone()).await.unwrap();

        let retrieved = store.get_embedding(&id).await.unwrap();
        assert!(retrieved.is_some());
        let retrieved_emb = retrieved.unwrap();
        assert_eq!(retrieved_emb.len(), 4);
        // Embeddings are stored int8-quantized from an L2-normalized copy, so
        // exact f32 values do not roundtrip — direction must (that is what
        // cosine-based retrieval consumes).
        let sim = crate::embedding::cosine_similarity(&emb, &retrieved_emb);
        assert!(
            sim > 0.99,
            "quantized roundtrip lost direction (cosine {sim})"
        );
    }

    #[tokio::test]
    async fn test_vector_search() {
        let store = SqliteStore::in_memory().unwrap();

        let m1 = Memory::new(
            MemoryType::Fact,
            "python coding".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "rust systems".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let m3 = Memory::new(
            MemoryType::Fact,
            "no embedding".to_string(),
            Priority::Reference,
            test_agent(),
        );

        // similar embeddings for python and query, different for rust
        store
            .save_with_embedding(m1, vec![0.9, 0.1, 0.0])
            .await
            .unwrap();
        store
            .save_with_embedding(m2, vec![0.1, 0.9, 0.0])
            .await
            .unwrap();
        store.save(m3).await.unwrap(); // no embedding

        let query_emb = vec![0.85, 0.15, 0.0]; // similar to python
        let results = store.vector_search(&query_emb, 10, None).await.unwrap();

        assert_eq!(results.len(), 2); // only 2 have embeddings
        assert!(results[0].memory.content.contains("python")); // python should rank first
        assert!(results[0].score > results[1].score);
    }

    #[tokio::test]
    async fn test_set_embedding_after_save() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "delayed embedding".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();

        store.save(mem).await.unwrap();
        assert!(store.get_embedding(&id).await.unwrap().is_none());

        store.set_embedding(&id, vec![1.0, 2.0, 3.0]).await.unwrap();
        let emb = store.get_embedding(&id).await.unwrap().unwrap();
        assert_eq!(emb.len(), 3);
    }

    #[tokio::test]
    async fn test_list_pending() {
        let store = SqliteStore::in_memory().unwrap();

        // A not-yet-reviewed memory (ai_generated=true, human_reviewed=false)
        let mut m1 = Memory::new(
            MemoryType::Fact,
            "needs review".to_string(),
            Priority::Reference,
            test_agent(),
        );
        m1.ai_generated = true;
        m1.human_reviewed = false;

        // An already-reviewed memory
        let mut m2 = Memory::new(
            MemoryType::Fact,
            "already reviewed".to_string(),
            Priority::Reference,
            test_agent(),
        );
        m2.human_reviewed = true;

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let pending = store.list_pending(None, 100, 0).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].content, "needs review");
        assert!(!pending[0].human_reviewed);
    }

    #[tokio::test]
    async fn test_list_pending_with_namespace() {
        let store = SqliteStore::in_memory().unwrap();

        let mut m1 = Memory::new(
            MemoryType::Fact,
            "project pending".to_string(),
            Priority::Reference,
            test_agent(),
        );
        m1.namespace = "project:alpha".to_string();
        m1.ai_generated = true;

        let mut m2 = Memory::new(
            MemoryType::Fact,
            "global pending".to_string(),
            Priority::Reference,
            test_agent(),
        );
        m2.ai_generated = true;

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let pending = store
            .list_pending(Some("project:alpha"), 100, 0)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].content.contains("project"));
    }

    #[test]
    fn test_legacy_schema_reconciliation_sets_user_version() {
        let tmp =
            std::env::temp_dir().join(format!("memvault-legacy-test-{}.db", uuid::Uuid::new_v4()));
        {
            let conn = Connection::open(&tmp).unwrap();
            conn.execute_batch(
                "CREATE TABLE memories (
                    id TEXT PRIMARY KEY, memory_type TEXT NOT NULL, content TEXT NOT NULL,
                    instruction TEXT, priority TEXT NOT NULL DEFAULT 'REFERENCE',
                    source_agent_id TEXT NOT NULL, source_agent_type TEXT NOT NULL,
                    source_session_id TEXT, namespace TEXT NOT NULL DEFAULT 'global',
                    confidence REAL NOT NULL DEFAULT 0.8, tags TEXT NOT NULL DEFAULT '[]',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    ai_generated INTEGER NOT NULL DEFAULT 1, human_reviewed INTEGER NOT NULL DEFAULT 0,
                    decay_score REAL NOT NULL DEFAULT 1.0, access_count INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO memories (id, memory_type, content, source_agent_id, source_agent_type, created_at, updated_at)
                 VALUES ('mem_legacy', 'fact', 'legacy row', 'a', 'b', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        let store = SqliteStore::new(&tmp).unwrap();
        let conn = store.pool.get().unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version as usize,
            SqliteStore::MIGRATIONS.len(),
            "legacy db should be reconciled to latest schema version"
        );

        let has_layer: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name='layer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_layer, 1);

        let conn = store.pool.get().unwrap();
        let has_superseded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name='superseded_by'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_superseded, 1);
        let has_episodes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='episodes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            has_episodes, 1,
            "legacy path must create the episodes table"
        );
        drop(conn);

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(tmp.with_extension("db-wal"));
        let _ = std::fs::remove_file(tmp.with_extension("db-shm"));
    }

    #[tokio::test]
    async fn test_backup_to_creates_consistent_snapshot() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();
        let mem = Memory::new(
            MemoryType::Fact,
            "backup me".to_string(),
            Priority::Reference,
            agent,
        );
        store.save(mem).await.unwrap();

        let dest =
            std::env::temp_dir().join(format!("memvault-backup-test-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&dest);

        store.backup_to(&dest).await.unwrap();
        assert!(dest.exists());

        let restored = SqliteStore::new(&dest).unwrap();
        let memories = restored.list(None, 10, 0).await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "backup me");

        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(dest.with_extension("db-wal"));
        let _ = std::fs::remove_file(dest.with_extension("db-shm"));
    }

    #[tokio::test]
    async fn test_backup_to_refuses_existing_destination() {
        let store = SqliteStore::in_memory().unwrap();
        let dest = std::env::temp_dir().join(format!(
            "memvault-backup-exists-{}.db",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&dest, b"not a real db").unwrap();

        let result = store.backup_to(&dest).await;
        assert!(result.is_err());

        let _ = std::fs::remove_file(&dest);
    }

    #[tokio::test]
    async fn test_update_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "original".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        let mut updated = mem;
        updated.content = "updated content".to_string();
        updated.priority = Priority::Must;
        updated.tags = vec!["newtag".to_string()];
        store.update(updated).await.unwrap();

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.content, "updated content");
        assert_eq!(retrieved.priority, Priority::Must);
        assert_eq!(retrieved.tags, vec!["newtag".to_string()]);
    }

    /// C1 persistence regression: `update` must write `ai_generated`. The
    /// column was once missing from the UPDATE list, so trust revocations
    /// (writer::merge_memory flips ai_generated to true when a merge absorbs
    /// unreviewed content; markdown quarantine does the same) existed only
    /// in memory — the row kept `ai_generated=false` and `is_trusted()`'s
    /// human-authorship shortcut re-trusted the revoked content on the next
    /// read. Every other trust-relevant column round-trips through this test
    /// via the save → update → get cycle.
    #[tokio::test]
    async fn test_update_persists_ai_generated() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Preference,
            "deploy only after explicit approval".to_string(),
            Priority::Must,
            test_agent(),
        );
        mem.ai_generated = false; // human-authored
        mem.human_reviewed = true;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // The revocation shape writer::merge_memory produces.
        let mut updated = store.get(&id).await.unwrap();
        updated.human_reviewed = false;
        updated.ai_generated = true;
        store.update(updated).await.unwrap();

        let reloaded = store.get(&id).await.unwrap();
        assert!(!reloaded.human_reviewed);
        assert!(
            reloaded.ai_generated,
            "UPDATE must persist ai_generated — a lost flip silently re-trusts revoked content"
        );
    }

    #[tokio::test]
    async fn test_friction_evidence_roundtrips_through_save_update_get() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Preference,
            "prefers dark mode".to_string(),
            Priority::Reference,
            test_agent(),
        );
        assert_eq!(mem.friction_evidence, None, "unset by default");
        mem.friction_evidence =
            Some("Friction score 2 (retry×2, rejection×0, correction×0).".to_string());
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(
            retrieved.friction_evidence.as_deref(),
            Some("Friction score 2 (retry×2, rejection×0, correction×0).")
        );

        let mut updated = retrieved;
        updated.friction_evidence = None;
        store.update(updated).await.unwrap();
        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(
            retrieved.friction_evidence, None,
            "update can clear it back to null"
        );
    }

    #[tokio::test]
    async fn test_update_writes_history_snapshot() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "before edit".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        let mut updated = mem;
        updated.content = "after edit".to_string();
        store.update(updated).await.unwrap();

        let history = store.list_checkpoints(Some(&id), 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].operation, "update");
        assert_eq!(history[0].memory_id, id);
    }

    #[tokio::test]
    async fn test_delete_writes_history_snapshot() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "about to be deleted".to_string(),
            Priority::Background,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        store.delete(&id).await.unwrap();

        let history = store.list_checkpoints(Some(&id), 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].operation, "delete");
    }

    #[tokio::test]
    async fn test_get_history_snapshot_returns_latest_for_soft_deleted_id() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "state before edits".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        // Two updates before the delete: the latest snapshot is the
        // pre-delete state, not the oldest one.
        let mut first = mem.clone();
        first.content = "first edit".to_string();
        store.update(first).await.unwrap();
        let mut second = mem;
        second.content = "second edit".to_string();
        store.update(second).await.unwrap();
        store.delete(&id).await.unwrap();

        let snapshot = store.get_history_snapshot(&id).await.unwrap().unwrap();
        assert_eq!(snapshot.content, "second edit");
        assert_eq!(snapshot.id, id);

        // An id that never had any history reads as None, not an error.
        assert!(
            store
                .get_history_snapshot("mem_never_existed")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_purge_removes_row_and_leaves_nothing_restorable() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "about to be purged".to_string(),
            Priority::Background,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        // An update first, so there is a pre-existing history snapshot that
        // purge must also erase, not just skip writing a new one.
        let mut updated = mem;
        updated.content = "edited before purge".to_string();
        store.update(updated).await.unwrap();
        assert_eq!(
            store.list_checkpoints(Some(&id), 10).await.unwrap().len(),
            1
        );

        store.purge(&id).await.unwrap();

        assert!(store.get(&id).await.is_err());
        assert!(
            store
                .list_checkpoints(Some(&id), 10)
                .await
                .unwrap()
                .is_empty(),
            "purge must erase prior history snapshots too, unlike delete()"
        );
    }

    #[tokio::test]
    async fn test_purge_also_erases_the_historical_full_text_index() {
        // `memory_history_fts` (migration 28) indexes the TEXT of every snapshot
        // so `as_of` search can recall a memory by the words it had then. That
        // makes it a fourth place forgotten content can survive, and `purge`'s own
        // contract is "leave nothing restorable" — §9.5 promises only a content
        // hash survives in `forgotten_log`. Erasing the snapshots without their
        // index entries left the erased text MATCH-able in the index.
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "the secret codeword is lavender".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        // An edit, so a snapshot exists and its text is indexed.
        let mut edited = mem;
        edited.content = "the secret codeword is rosemary".to_string();
        store.update(edited).await.unwrap();

        // `memories` keys on `id`; the history table and both FTS indexes carry
        // `memory_id`. One helper, explicit column, because assuming they agree is
        // how this test first failed.
        let count = |table: &str, column: &str, id: &str| -> i64 {
            let conn = store.pool.get().unwrap();
            conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column} = ?1"),
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        // Precondition: all four locations hold the memory, including both texts.
        assert_eq!(count("memories", "id", &id), 1);
        assert_eq!(count("memory_history", "memory_id", &id), 1);
        assert_eq!(count("memories_fts", "memory_id", &id), 1);
        assert_eq!(count("memory_history_fts", "memory_id", &id), 1);
        {
            let conn = store.pool.get().unwrap();
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM memory_history_fts WHERE memory_history_fts MATCH 'lavender'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "the retired wording is searchable before the purge");
        }

        store.purge(&id).await.unwrap();

        // Every location must be empty. Checking only the row and the history
        // table is what let this through: both were already correct.
        assert_eq!(count("memories", "id", &id), 0, "live row");
        assert_eq!(
            count("memory_history", "memory_id", &id),
            0,
            "history snapshots"
        );
        assert_eq!(
            count("memories_fts", "memory_id", &id),
            0,
            "live full-text index"
        );
        assert_eq!(
            count("memory_history_fts", "memory_id", &id),
            0,
            "historical full-text index must be erased too"
        );
        for word in ["lavender", "rosemary"] {
            let conn = store.pool.get().unwrap();
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM memory_history_fts WHERE memory_history_fts MATCH '{word}'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                n, 0,
                "{word} must not be MATCH-able after a permanent forget"
            );
        }
    }

    #[tokio::test]
    async fn test_purge_on_missing_id_returns_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let err = store.purge("mem_does_not_exist").await;
        assert!(matches!(err, Err(MemVaultError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_record_and_list_forgotten_log() {
        let store = SqliteStore::in_memory().unwrap();
        let entry = ForgottenLogEntry {
            log_id: None,
            memory_id: "mem_erased".to_string(),
            content_hash: "abc123hash".to_string(),
            namespace: "archived:global".to_string(),
            priority: Priority::Reference,
            visibility: Visibility::Scoped,
            scope: ForgetScope::Auto,
            reason: Some("auto-forget: archived retention exceeded".to_string()),
            requested_by: "system:auto-forget".to_string(),
            must_override: false,
            erased_at: chrono::Utc::now(),
        };
        store.record_forgotten(&entry).await.unwrap();

        let all = store.list_forgotten_log(None, 10).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].memory_id, "mem_erased");
        assert_eq!(all[0].content_hash, "abc123hash");
        assert_eq!(all[0].scope, ForgetScope::Auto);

        let scoped = store
            .list_forgotten_log(Some("mem_erased"), 10)
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);

        let none = store
            .list_forgotten_log(Some("mem_other"), 10)
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn test_archived_at_roundtrips_through_save_and_update() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "will be archived".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        assert!(mem.archived_at.is_none());
        store.save(mem.clone()).await.unwrap();
        assert!(store.get(&id).await.unwrap().archived_at.is_none());

        let archived_at = chrono::Utc::now();
        mem.archived_at = Some(archived_at);
        mem.namespace = "archived:global".to_string();
        store.update(mem).await.unwrap();

        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(
            retrieved.archived_at.map(|d| d.timestamp()),
            Some(archived_at.timestamp())
        );
    }

    // ---------------------------------------------------------------------
    // expires_at (valid-time upper bound)
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn test_expires_at_roundtrips_through_save_and_update() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "deploy freeze until friday".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        assert!(mem.expires_at.is_none());
        store.save(mem.clone()).await.unwrap();
        assert!(store.get(&id).await.unwrap().expires_at.is_none());

        // Far enough in the future that the retrieval filters below cannot
        // race the clock and treat it as already expired.
        let expires = chrono::Utc::now() + chrono::Duration::days(3);
        mem.expires_at = Some(expires);
        store.update(mem).await.unwrap();

        let got = store.get(&id).await.unwrap();
        assert_eq!(
            got.expires_at.map(|d| d.timestamp_micros()),
            Some(expires.timestamp_micros()),
            "expires_at must survive the update path — an UPDATE that dropped \
             the column would silently make a temporary fact permanent"
        );
    }

    #[tokio::test]
    async fn test_expired_memory_is_excluded_from_keyword_search() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the oncall rotation is alice".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        store.save(mem).await.unwrap();

        let mut q = SearchQuery::new("oncall rotation".to_string());
        q.top_k = 10;
        let outcome = store.search(q).await.unwrap();
        assert!(
            outcome.results.is_empty(),
            "an expired memory must not be retrieved, got {:?}",
            outcome
                .results
                .iter()
                .map(|r| &r.memory.content)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_expired_memory_is_excluded_from_vector_search_and_shared_pool() {
        let store = SqliteStore::in_memory().unwrap();

        let mut vec_mem = Memory::new(
            MemoryType::Fact,
            "vector candidate".to_string(),
            Priority::Reference,
            test_agent(),
        );
        vec_mem.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        store
            .save_with_embedding(vec_mem, vec![1.0, 0.0, 0.0])
            .await
            .unwrap();
        let hits = store
            .vector_search(&[1.0, 0.0, 0.0], 10, None)
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "expired memory must not be vector-recalled"
        );

        // The shared pool feeds injection into *every* session, so an expired
        // team fact leaking through here is the most damaging place it could
        // surface.
        let mut shared = Memory::new(
            MemoryType::Fact,
            "shared team fact".to_string(),
            Priority::Must,
            test_agent(),
        );
        shared.visibility = Visibility::Shared;
        shared.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        store.save(shared).await.unwrap();
        assert!(
            store.list_shared(20).await.unwrap().is_empty(),
            "an expired shared memory must not be injected into sessions"
        );
    }

    #[tokio::test]
    async fn test_expired_memory_stays_visible_through_inspection_paths() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "expired but not erased".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // Same contract as superseded memories: out of retrieval, still
        // inspectable and restorable. Expiry retires a fact; only `forget`
        // erases one (§9.5).
        assert!(store.get(&id).await.unwrap().expires_at.is_some());
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 1);
    }

    #[test]
    fn test_is_expired_at_bound_is_exclusive() {
        let now = chrono::Utc::now();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "x".to_string(),
            Priority::Reference,
            test_agent(),
        );
        assert!(!mem.is_expired_at(now), "no expiry = never expires");

        mem.expires_at = Some(now);
        assert!(
            mem.is_expired_at(now),
            "expiring at exactly `now` is already expired — matches the \
             `expires_at > ?` form in SQL so the two can never disagree"
        );
        assert!(!mem.is_expired_at(now - chrono::Duration::seconds(1)));

        mem.expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(mem.is_expired_at(now));
    }

    #[test]
    fn test_was_in_effect_at_covers_both_bounds() {
        let t0 = chrono::Utc::now() - chrono::Duration::hours(2);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "x".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.expires_at = Some(t0 + chrono::Duration::hours(1));

        assert!(
            !mem.was_in_effect_at(t0 - chrono::Duration::seconds(1)),
            "not created yet"
        );
        assert!(mem.was_in_effect_at(t0), "created_at is inclusive");
        assert!(mem.was_in_effect_at(t0 + chrono::Duration::minutes(30)));
        assert!(
            !mem.was_in_effect_at(t0 + chrono::Duration::hours(1)),
            "expired"
        );

        mem.expires_at = None;
        assert!(mem.was_in_effect_at(t0 + chrono::Duration::days(365)));
    }

    // ---------------------------------------------------------------------
    // as_of (valid-time point-in-time reads)
    // ---------------------------------------------------------------------

    /// A memory created two hours ago whose content was replaced just now.
    /// Returns (store, id, original content, t0).
    async fn store_with_one_update() -> (SqliteStore, String, String, chrono::DateTime<chrono::Utc>)
    {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(2);
        let original = "the staging database is postgres 14".to_string();
        let mut mem = Memory::new(
            MemoryType::Fact,
            original.clone(),
            Priority::Reference,
            test_agent(),
        );
        // Pin created_at into the past so there is a deterministic window
        // between creation and the update below; `write_history` stamps
        // `changed_at` with the real current time.
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let mut updated = store.get(&id).await.unwrap();
        updated.content = "the staging database is postgres 16".to_string();
        store.update(updated).await.unwrap();

        (store, id, original, t0)
    }

    #[tokio::test]
    async fn test_search_filters_by_event_time_window() {
        let store = SqliteStore::in_memory().unwrap();
        let march = chrono::DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let august = chrono::DateTime::parse_from_rfc3339("2026-08-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        for (content, when) in [
            ("the march oncall rotation is alice", Some(march)),
            ("the august oncall rotation is bob", Some(august)),
            // Undated: cannot be said to have happened in any window.
            ("the oncall rotation is unspecified", None),
        ] {
            let mut mem = Memory::new(
                MemoryType::Fact,
                content.to_string(),
                Priority::Reference,
                test_agent(),
            );
            mem.occurred_at = when;
            store.save(mem).await.unwrap();
        }

        let mut q = SearchQuery::new("oncall rotation".to_string());
        q.occurred_from = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        q.occurred_to = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-31T23:59:59Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let hits = store.search(q).await.unwrap();
        let contents: Vec<&str> = hits
            .results
            .iter()
            .map(|r| r.memory.content.as_str())
            .collect();
        assert_eq!(
            contents,
            vec!["the march oncall rotation is alice"],
            "the window must select on event-time alone"
        );
    }

    #[tokio::test]
    async fn test_event_time_window_never_matches_undated_memories() {
        // Deliberate, and the reason is worth pinning: an undated memory admitted
        // to every window would make the filter return the same rows regardless of
        // the range, which is noise dressed as a result.
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "an undated oncall note".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.occurred_at = None;
        store.save(mem).await.unwrap();

        for (from, to) in [
            ("1970-01-01T00:00:00Z", "2999-12-31T23:59:59Z"),
            ("2026-01-01T00:00:00Z", "2026-12-31T23:59:59Z"),
        ] {
            let mut q = SearchQuery::new("oncall".to_string());
            q.occurred_from = Some(
                chrono::DateTime::parse_from_rfc3339(from)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            );
            q.occurred_to = Some(
                chrono::DateTime::parse_from_rfc3339(to)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            );
            assert!(
                store.search(q).await.unwrap().results.is_empty(),
                "an undated memory must not match the window {from}..{to}"
            );
        }

        // …and without a window it is found, so the memory is not simply broken.
        assert_eq!(
            store
                .search(SearchQuery::new("oncall".to_string()))
                .await
                .unwrap()
                .results
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_event_time_bounds_are_inclusive() {
        let store = SqliteStore::in_memory().unwrap();
        let exactly = chrono::DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "a boundary timed note".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.occurred_at = Some(exactly);
        store.save(mem).await.unwrap();

        // A degenerate window [t, t] must still match a memory at exactly t.
        let mut q = SearchQuery::new("boundary timed".to_string());
        q.occurred_from = Some(exactly);
        q.occurred_to = Some(exactly);
        assert_eq!(
            store.search(q).await.unwrap().results.len(),
            1,
            "both bounds are inclusive"
        );
    }

    #[tokio::test]
    async fn test_event_time_and_valid_time_axes_stay_independent() {
        // The whole reason there are two parameters rather than one range: a
        // memory can have happened in March and still have been true in August.
        // Folding the axes would make one of the two questions unanswerable.
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the database was migrated in march".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.occurred_at = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-10T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        // Created (ingested) much later than the event.
        mem.created_at = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // Event-time in March: found.
        let mut q = SearchQuery::new("database migrated".to_string());
        q.occurred_from = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        q.occurred_to = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-31T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(store.search(q).await.unwrap().results.len(), 1);

        // Valid-time in March: NOT found, because MemVault did not hold it yet —
        // it was ingested in August. Same month, opposite answer, which is exactly
        // why the two cannot share a parameter.
        let mut q2 = SearchQuery::new("database migrated".to_string());
        q2.as_of = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-15T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert!(
            store.search(q2).await.unwrap().results.is_empty(),
            "as_of is about belief, occurred_at is about the event"
        );
        assert!(
            store
                .get_as_of(
                    &id,
                    chrono::DateTime::parse_from_rfc3339("2026-03-15T00:00:00Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc)
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_as_of_search_honours_the_same_filters_as_live_search() {
        // Guard for the drift risk `storage::memory_matches_filters` documents:
        // the live
        // keyword path filters in SQL via `append_filter_clauses`, the historical
        // path filters in Rust. A filter present in one and absent in the other
        // makes `as_of` searches silently widen their scope — which is how a
        // namespace-scoped query came to return another project's history.
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let as_of = t0 + chrono::Duration::hours(1);

        // One memory per namespace, each edited after `as_of` so the recall has to
        // come from the historical index rather than the live one.
        let mut ids = std::collections::HashMap::new();
        for (ns, mtype, occurred) in [
            ("project:alpha", MemoryType::Fact, Some(t0)),
            ("project:beta", MemoryType::Fact, Some(t0)),
            ("project:alpha", MemoryType::Preference, None),
        ] {
            let key = (ns.to_string(), format!("{mtype:?}"));
            let mut mem = Memory::new(
                mtype,
                format!("the shared codeword is quartz in {ns}"),
                Priority::Reference,
                test_agent(),
            );
            mem.namespace = ns.to_string();
            mem.created_at = t0;
            mem.updated_at = t0;
            mem.occurred_at = occurred;
            let saved = store.save(mem).await.unwrap();
            ids.insert(key, saved.id.clone());
            let mut edited = store.get(&saved.id).await.unwrap();
            edited.content = format!("the codeword was rotated in {ns}");
            store.update(edited).await.unwrap();
        }

        // Baseline: the retired wording is reachable historically at all.
        let mut all = SearchQuery::new("quartz".to_string());
        all.as_of = Some(as_of);
        let hits = store.search(all).await.unwrap();
        assert_eq!(
            hits.results.len(),
            3,
            "test premise: all three are recallable"
        );

        // Namespace scoping must hold on the historical path too.
        let mut scoped = SearchQuery::new("quartz".to_string());
        scoped.as_of = Some(as_of);
        scoped.namespace = Some("project:alpha".to_string());
        let hits = store.search(scoped).await.unwrap();
        assert_eq!(hits.results.len(), 2, "alpha has two of the three");
        assert!(
            hits.results
                .iter()
                .all(|r| r.memory.namespace == "project:alpha"),
            "a namespace-scoped as_of search must not return another project's \
             history, got {:?}",
            hits.results
                .iter()
                .map(|r| &r.memory.namespace)
                .collect::<Vec<_>>()
        );

        // Type filter.
        let mut typed = SearchQuery::new("quartz".to_string());
        typed.as_of = Some(as_of);
        typed.type_filter = Some(MemoryType::Preference);
        let hits = store.search(typed).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.memory_type, MemoryType::Preference);

        // Priority filter.
        let mut prioritised = SearchQuery::new("quartz".to_string());
        prioritised.as_of = Some(as_of);
        prioritised.priority_filter = Some(Priority::Must);
        assert!(
            store.search(prioritised).await.unwrap().results.is_empty(),
            "no MUST memories exist, so none may be returned"
        );

        // Event-time window: the third memory is undated and must never match.
        let mut windowed = SearchQuery::new("quartz".to_string());
        windowed.as_of = Some(as_of);
        windowed.occurred_from = Some(t0 - chrono::Duration::days(1));
        windowed.occurred_to = Some(t0 + chrono::Duration::days(1));
        let hits = store.search(windowed).await.unwrap();
        assert_eq!(
            hits.results.len(),
            2,
            "the undated memory must not match an event-time window"
        );
        assert!(hits.results.iter().all(|r| r.memory.occurred_at.is_some()));
    }

    #[tokio::test]
    async fn test_search_as_of_recalls_a_memory_by_the_text_it_had_then() {
        // The limitation §9.6 documented when `as_of` shipped: matching ran
        // against current index text, so a memory reworded since `as_of` could
        // not be found by the words it had then. This is the case that closes it.
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the staging database is postgres 14".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let as_of = t0 + chrono::Duration::hours(1);
        let mut updated = store.get(&id).await.unwrap();
        updated.content = "the staging database is a managed mysql cluster".to_string();
        store.update(updated).await.unwrap();

        // Current-text search no longer finds it by the old wording.
        let now_q = SearchQuery::new("postgres".to_string());
        assert!(
            store.search(now_q).await.unwrap().results.is_empty(),
            "the word is gone from the current text"
        );

        // Point-in-time search does, via the historical index.
        let mut past_q = SearchQuery::new("postgres".to_string());
        past_q.as_of = Some(as_of);
        let past = store.search(past_q).await.unwrap();
        assert_eq!(past.results.len(), 1, "the historical index must recall it");
        assert_eq!(
            past.results[0].memory.content,
            "the staging database is postgres 14"
        );
        assert!(
            past.results[0]
                .hit_sources
                .iter()
                .any(|h| matches!(h, HitSource::HistoryKeyword { .. })),
            "provenance must say the recall came from historical text, got {:?}",
            past.results[0].hit_sources
        );
        assert!(matches!(
            past.results[0].as_of_source,
            Some(AsOfSource::History { .. })
        ));
    }

    #[tokio::test]
    async fn test_historical_recall_rejects_a_version_that_was_not_in_effect() {
        // A memory edited twice has two historical versions. Only the one current
        // at `as_of` may answer — otherwise the query would return text the memory
        // did not hold at the instant being asked about.
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(6);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "v1 the retry budget is three".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // v1 -> v2 (snapshot of v1 written), then v2 -> v3 (snapshot of v2).
        let mut v2 = store.get(&id).await.unwrap();
        v2.content = "v2 the retry budget is five".to_string();
        store.update(v2).await.unwrap();

        // Captured explicitly rather than derived as "now minus a few
        // milliseconds" at assertion time. The original form depended on the v3
        // update landing inside that window, which held when the test ran alone
        // and flaked under the full suite's parallel load — a test whose answer
        // depends on CPU contention is not testing the code.
        let while_v2_current = chrono::Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(5));

        let mut v3 = store.get(&id).await.unwrap();
        v3.content = "v3 the retry budget is nine".to_string();
        store.update(v3).await.unwrap();

        // Ask about a moment when v2 was current, using a word only v1 had.
        let mut q = SearchQuery::new("three".to_string());
        q.as_of = Some(while_v2_current);
        let hits = store.search(q).await.unwrap();
        assert!(
            hits.results.iter().all(|r| r.memory.content.contains("v2")),
            "v1 matched the historical index but was not in effect then; \
             only the v2 state may be returned, got {:?}",
            hits.results
                .iter()
                .map(|r| &r.memory.content)
                .collect::<Vec<_>>()
        );

        // And a word only v2 had does resolve, to the v2 state.
        let mut q2 = SearchQuery::new("five".to_string());
        q2.as_of = Some(while_v2_current);
        let hits2 = store.search(q2).await.unwrap();
        assert_eq!(hits2.results.len(), 1);
        assert!(hits2.results[0].memory.content.contains("v2"));
    }

    #[tokio::test]
    async fn test_history_fts_backfills_pre_existing_snapshots() {
        // Migration 28 created an empty index; snapshots written before it must
        // still become searchable, or every existing vault would have an
        // `as_of` search that silently returns nothing until its next edit.
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the oncall pager belongs to zephyr".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // Wipe the historical index to simulate a vault upgraded in place, then
        // reopen: the count comparison in `ensure_fts_populated` must refill it.
        {
            let conn = store.pool.get().unwrap();
            conn.execute("DELETE FROM memory_history_fts", []).unwrap();
            let n: i64 = conn
                .query_row("SELECT count(*) FROM memory_history_fts", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "index emptied for the test");
        }

        let mut updated = store.get(&id).await.unwrap();
        updated.content = "the oncall pager belongs to atlas".to_string();
        let as_of = t0 + chrono::Duration::hours(1);
        store.update(updated).await.unwrap();

        // Re-running the backfill is what a restart does; call it directly since
        // an in-memory store cannot be reopened.
        {
            let conn = store.pool.get().unwrap();
            SqliteStore::backfill_history_fts(&conn).unwrap();
        }

        let mut q = SearchQuery::new("zephyr".to_string());
        q.as_of = Some(as_of);
        let hits = store.search(q).await.unwrap();
        assert_eq!(
            hits.results.len(),
            1,
            "a snapshot written before the index existed must still be searchable"
        );
        assert!(hits.results[0].memory.content.contains("zephyr"));
    }

    #[tokio::test]
    async fn test_search_without_as_of_never_uses_the_historical_index() {
        // A current-truth search must not surface wording that is no longer true.
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "the api base url is quartz".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        let mut updated = store.get(&id).await.unwrap();
        updated.content = "the api base url is basalt".to_string();
        store.update(updated).await.unwrap();

        let hits = store
            .search(SearchQuery::new("quartz".to_string()))
            .await
            .unwrap();
        assert!(
            hits.results.is_empty(),
            "without as_of, retired wording must not be recalled"
        );
        assert!(hits.results.iter().all(|r| {
            !r.hit_sources
                .iter()
                .any(|h| matches!(h, HitSource::HistoryKeyword { .. }))
        }));
    }

    #[tokio::test]
    async fn test_get_as_of_returns_the_pre_update_snapshot() {
        let (store, id, original, t0) = store_with_one_update().await;
        let as_of = t0 + chrono::Duration::hours(1);

        let at = store
            .get_as_of(&id, as_of)
            .await
            .unwrap()
            .expect("the memory existed an hour after creation");
        assert_eq!(at.memory.content, original);
        assert_eq!(at.as_of, as_of);
        match at.source {
            AsOfSource::History { replaced_at, .. } => {
                assert!(
                    replaced_at > as_of,
                    "the snapshot must be one that was still current at as_of"
                );
            }
            other => panic!("expected a reconstructed snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_as_of_returns_live_row_when_nothing_changed_since() {
        let (store, id, _original, _t0) = store_with_one_update().await;
        let at = store
            .get_as_of(&id, chrono::Utc::now() + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .expect("the memory is live now");
        assert_eq!(at.memory.content, "the staging database is postgres 16");
        assert_eq!(at.source, AsOfSource::Live);
    }

    #[tokio::test]
    async fn test_get_as_of_returns_none_before_the_memory_existed() {
        let (store, id, _original, t0) = store_with_one_update().await;
        assert!(
            store
                .get_as_of(&id, t0 - chrono::Duration::hours(1))
                .await
                .unwrap()
                .is_none(),
            "a time before created_at must yield no state, not the earliest snapshot"
        );
    }

    #[tokio::test]
    async fn test_get_as_of_spans_a_delete() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(2);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "deleted later".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store.delete(&id).await.unwrap();

        // Before the delete: the snapshot holds the live-at-the-time state.
        let at = store
            .get_as_of(&id, t0 + chrono::Duration::hours(1))
            .await
            .unwrap()
            .expect("still existed an hour after creation");
        assert_eq!(at.memory.content, "deleted later");
        assert!(matches!(at.source, AsOfSource::History { .. }));

        // After the delete: there is no state, and the answer must be None
        // rather than a resurrected row.
        assert!(
            store
                .get_as_of(&id, chrono::Utc::now() + chrono::Duration::hours(1))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_get_as_of_sees_a_superseded_memory_as_current_before_the_supersede() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(2);

        let mut old = Memory::new(
            MemoryType::Fact,
            "the api base url is v1".to_string(),
            Priority::Reference,
            test_agent(),
        );
        old.created_at = t0;
        old.updated_at = t0;
        let old_id = old.id.clone();
        store.save(old).await.unwrap();

        let new = Memory::new(
            MemoryType::Fact,
            "the api base url is v2".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let new_id = new.id.clone();
        store.save(new).await.unwrap();

        store.supersede(&old_id, &new_id).await.unwrap();
        assert_eq!(
            store.get(&old_id).await.unwrap().superseded_by.as_deref(),
            Some(new_id.as_str())
        );

        // At a point before the supersede, v1 was the current truth and was
        // not superseded — the reconstructed state must say so.
        let at = store
            .get_as_of(&old_id, t0 + chrono::Duration::hours(1))
            .await
            .unwrap()
            .expect("v1 existed before being superseded");
        assert_eq!(at.memory.content, "the api base url is v1");
        assert!(
            at.memory.superseded_by.is_none(),
            "supersession had not happened at as_of, so the snapshot must not carry it"
        );

        // Now: the live row carries the supersede pointer.
        let now_at = store
            .get_as_of(&old_id, chrono::Utc::now() + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(now_at.source, AsOfSource::Live);
        assert_eq!(
            now_at.memory.superseded_by.as_deref(),
            Some(new_id.as_str())
        );
    }

    #[tokio::test]
    async fn test_get_as_of_respects_the_expiry_window_at_that_instant() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the freeze runs until friday".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        mem.expires_at = Some(t0 + chrono::Duration::hours(1));
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        assert!(
            store
                .get_as_of(&id, t0 + chrono::Duration::minutes(30))
                .await
                .unwrap()
                .is_some(),
            "still inside its validity window at that instant"
        );
        assert!(
            store
                .get_as_of(&id, t0 + chrono::Duration::hours(2))
                .await
                .unwrap()
                .is_none(),
            "past its validity window: there was no in-effect state then"
        );
    }

    #[tokio::test]
    async fn test_search_as_of_revives_a_memory_that_has_since_expired() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the freeze window is thursday".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        mem.expires_at = Some(t0 + chrono::Duration::hours(1));
        store.save(mem).await.unwrap();

        // Present-day search: gone.
        let now_q = SearchQuery::new("freeze window".to_string());
        assert!(store.search(now_q).await.unwrap().results.is_empty());

        // Point-in-time search inside the window: found, and the result says
        // where its state came from.
        let mut past_q = SearchQuery::new("freeze window".to_string());
        past_q.as_of = Some(t0 + chrono::Duration::minutes(30));
        let past = store.search(past_q).await.unwrap();
        assert_eq!(past.results.len(), 1, "was in effect at as_of");
        assert_eq!(
            past.results[0].memory.content,
            "the freeze window is thursday"
        );
        assert!(
            past.results[0].as_of_source.is_some(),
            "an as_of search must label every result as time-travelled"
        );
    }

    #[tokio::test]
    async fn test_search_as_of_drops_a_memory_created_afterwards() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "brand new rotation schedule".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(mem).await.unwrap();

        let mut q = SearchQuery::new("rotation schedule".to_string());
        q.as_of = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        let outcome = store.search(q).await.unwrap();
        assert!(
            outcome.results.is_empty(),
            "a memory that did not exist at as_of must be dropped, not served \
             in its current form"
        );
    }

    #[tokio::test]
    async fn test_search_without_as_of_leaves_the_provenance_field_unset() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "plain current memory".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(mem).await.unwrap();

        let outcome = store
            .search(SearchQuery::new("plain current".to_string()))
            .await
            .unwrap();
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(
            outcome.results[0].as_of_source, None,
            "as_of_source doubles as 'this result was time-travelled'"
        );
    }

    #[tokio::test]
    async fn test_list_as_of_includes_deleted_and_filters_by_namespace() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(2);

        // Survives to the present.
        let mut live = Memory::new(
            MemoryType::Fact,
            "still here".to_string(),
            Priority::Reference,
            test_agent(),
        );
        live.created_at = t0;
        live.updated_at = t0;
        store.save(live).await.unwrap();

        // Deleted after as_of, so it was current truth at as_of and must appear
        // even though it has no live row.
        let mut gone = Memory::new(
            MemoryType::Fact,
            "gone later".to_string(),
            Priority::Reference,
            test_agent(),
        );
        gone.created_at = t0;
        gone.updated_at = t0;
        let gone_id = gone.id.clone();
        store.save(gone).await.unwrap();
        store.delete(&gone_id).await.unwrap();

        // Different namespace: must be filtered out.
        let mut other = Memory::new(
            MemoryType::Fact,
            "other namespace".to_string(),
            Priority::Reference,
            test_agent(),
        );
        other.created_at = t0;
        other.updated_at = t0;
        other.namespace = "project:alpha".to_string();
        store.save(other).await.unwrap();

        let as_of = t0 + chrono::Duration::hours(1);
        let all = store.list_as_of(as_of, None, 50, 0).await.unwrap();
        assert_eq!(
            all.len(),
            3,
            "no namespace filter: live + deleted-but-then-current + the alpha one"
        );
        assert!(all.iter().all(|m| m.as_of == as_of));
        assert!(all.iter().any(|m| m.memory.content == "gone later"));

        // Namespace filtering happens after reconstruction, because a deleted
        // memory's namespace is only recoverable from its snapshot.
        let scoped = store
            .list_as_of(as_of, Some("project:alpha"), 50, 0)
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].memory.content, "other namespace");

        // Pagination applies to the reconstructed set.
        let paged = store.list_as_of(as_of, None, 1, 1).await.unwrap();
        assert_eq!(paged.len(), 1);
    }

    #[tokio::test]
    async fn test_list_as_of_orders_by_the_reconstructed_state() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);

        for (content, updated) in [
            ("older edit", t0 + chrono::Duration::minutes(10)),
            ("newer edit", t0 + chrono::Duration::minutes(50)),
        ] {
            let mut mem = Memory::new(
                MemoryType::Fact,
                content.to_string(),
                Priority::Reference,
                test_agent(),
            );
            mem.created_at = t0;
            mem.updated_at = updated;
            store.save(mem).await.unwrap();
        }

        let list = store
            .list_as_of(t0 + chrono::Duration::hours(1), None, 50, 0)
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0].memory.content, "newer edit",
            "newest-first by the reconstructed state's own updated_at"
        );
    }

    #[tokio::test]
    async fn test_restore_after_update_reverts_content() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "original content".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        let mut updated = mem;
        updated.content = "accidentally overwritten".to_string();
        store.update(updated).await.unwrap();

        let history = store.list_checkpoints(Some(&id), 10).await.unwrap();
        let history_id = history[0].history_id;

        let restored = store.restore_checkpoint(history_id).await.unwrap();
        assert_eq!(restored.content, "original content");

        let refetched = store.get(&id).await.unwrap();
        assert_eq!(refetched.content, "original content");

        // Restoring is itself an update, so it produces one more history entry.
        let history_after = store.list_checkpoints(Some(&id), 10).await.unwrap();
        assert_eq!(history_after.len(), 2);
    }

    #[tokio::test]
    async fn test_restore_after_delete_reinserts_row() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "will be deleted then restored".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store.delete(&id).await.unwrap();
        assert!(store.get(&id).await.is_err());

        let history = store.list_checkpoints(Some(&id), 10).await.unwrap();
        let history_id = history[0].history_id;

        let restored = store.restore_checkpoint(history_id).await.unwrap();
        assert_eq!(restored.id, id);

        let refetched = store.get(&id).await.unwrap();
        assert_eq!(refetched.content, "will be deleted then restored");
    }

    #[tokio::test]
    async fn test_update_missing_returns_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "ghost".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let err = store.update(mem).await.unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_get_missing_returns_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let err = store.get("no-such-memory").await.unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_search_type_and_priority_filters() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();

        let mut m1 = Memory::new(
            MemoryType::Preference,
            "must preference".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m1.tags = vec!["coding".to_string()];

        let m2 = Memory::new(
            MemoryType::Fact,
            "fact reference".to_string(),
            Priority::Reference,
            agent,
        );

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let by_type = store
            .search(SearchQuery {
                query: String::new(),
                type_filter: Some(MemoryType::Fact),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(by_type.len(), 1);
        assert_eq!(by_type[0].memory.memory_type, MemoryType::Fact);

        let by_priority = store
            .search(SearchQuery {
                query: String::new(),
                priority_filter: Some(Priority::Must),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(by_priority.len(), 1);
        assert_eq!(by_priority[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_search_namespace_filter() {
        let store = SqliteStore::in_memory().unwrap();
        let mut m = Memory::new(
            MemoryType::Fact,
            "scoped memory".to_string(),
            Priority::Reference,
            test_agent(),
        );
        m.namespace = "project:beta".to_string();
        store.save(m).await.unwrap();

        let wrong_ns = store
            .search(SearchQuery {
                query: String::new(),
                namespace: Some("project:alpha".to_string()),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert!(wrong_ns.is_empty());

        let right_ns = store
            .search(SearchQuery {
                query: String::new(),
                namespace: Some("project:beta".to_string()),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(right_ns.len(), 1);
    }

    #[tokio::test]
    async fn test_search_no_results() {
        let store = SqliteStore::in_memory().unwrap();
        let results = store
            .search(SearchQuery {
                query: "definitely-not-present".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_search_top_k_honored() {
        let store = SqliteStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .save(Memory::new(
                    MemoryType::Fact,
                    format!("shared keyword {}", i),
                    Priority::Reference,
                    test_agent(),
                ))
                .await
                .unwrap();
        }
        let results = store
            .search(SearchQuery {
                query: "shared".to_string(),
                top_k: 2,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(results.len(), 2);
    }

    /// Regression: user input must match literally. Under the old LIKE path,
    /// `_` acted as a single-char wildcard so `a_b` matched `acb`. Under FTS5
    /// the same guarantee comes from quoting every token via
    /// `fts::build_match_expr`; `_` is an ASCII word character, so `a_b`
    /// stays one literal token and only the `a_b` row matches.
    #[tokio::test]
    async fn test_search_escapes_like_wildcards() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "the a_b identifier".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "the acb identifier".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        let results = store
            .search(SearchQuery {
                query: "a_b".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;

        assert_eq!(
            results.len(),
            1,
            "literal underscore must not act as wildcard"
        );
        assert!(results[0].memory.content.contains("a_b"));
    }

    // ------------------------------------------------------------------
    // FTS5 + CJK bigram search behavior
    // ------------------------------------------------------------------

    /// The motivating case for the CJK tokenizer: two-character words are the
    /// dominant query shape in Chinese, and the bundled FTS5 tokenizers do not
    /// segment CJK at all (verified: unicode61 indexes the whole run as one
    /// token). Searching `沙箱` must hit a sentence containing it.
    #[tokio::test]
    async fn test_search_cjk_two_char_word() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "沙箱环境部署完成了".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        let outcome = store
            .search(SearchQuery {
                query: "沙箱".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.keyword_tier, KeywordTier::Strict);

        // A two-char word that never appears must not match via bigram noise.
        let miss = store
            .search(SearchQuery {
                query: "游泳".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert!(miss.results.is_empty());
    }

    /// Reordered queries produce cross-word bigrams that the document does not
    /// contain; the strict tier misses and the relaxed unigram tier must catch
    /// it — and report that it did.
    #[tokio::test]
    async fn test_search_cjk_reordered_query_relaxes_tier() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "沙箱环境部署完成了".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        // 「部署沙箱」 strict tokens include 署沙, absent from the document.
        let outcome = store
            .search(SearchQuery {
                query: "部署沙箱".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.results.len(),
            1,
            "relaxed unigram tier must recover reordered CJK queries"
        );
        assert_eq!(outcome.keyword_tier, KeywordTier::RelaxedUnigram);
    }

    /// FTS5 metacharacters in user input must not error and must not be
    /// executed as query syntax — they are quoted into literals.
    #[tokio::test]
    async fn test_search_fts_metacharacters_are_literal() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "sandbox ring mid:SECRET".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        // Each of these is FTS5 syntax if passed raw (column filter, boolean
        // op, negation, prefix query) and used to break or silently change
        // queries. Quoted, they are inert literals and simply match nothing.
        for evil in [
            "环 OR mid:SECRET",
            "-沙箱",
            "NEAR(沙 箱)",
            "sandbox*",
            "\"unclosed",
        ] {
            let outcome = store
                .search(SearchQuery {
                    query: evil.to_string(),
                    top_k: 10,
                    ..SearchQuery::new(String::new())
                })
                .await
                .unwrap_or_else(|e| panic!("query {evil:?} must not error: {e}"));
            // No panic and no syntax error is the assertion; content matches
            // are allowed only for genuinely contained tokens.
            for r in &outcome.results {
                assert!(
                    r.memory.content.contains("sandbox"),
                    "unexpected hit for {evil:?}"
                );
            }
        }

        // A literal token that IS present still matches (sandbox → 'sandbox').
        let hit = store
            .search(SearchQuery {
                query: "sandbox\"injection".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert!(!hit.results.is_empty());
    }

    /// Synonym-expansion fallback: strict tokenization misses, but an expanded
    /// synonym token matches — reported as the fallback tier, never silently.
    #[tokio::test]
    async fn test_search_synonym_fallback_tier() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "our stack is javascript only".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        // 「js」 is not in the content; query_expand maps js → javascript.
        let outcome = store
            .search(SearchQuery {
                query: "js".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.keyword_tier, KeywordTier::SynonymFallback);
    }

    /// Updating searchable text must refresh the FTS row: the new content is
    /// found, the old content is not.
    #[tokio::test]
    async fn test_search_update_refreshes_fts() {
        let store = SqliteStore::in_memory().unwrap();
        let mut m = Memory::new(
            MemoryType::Fact,
            "original kubernetes notes".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(m.clone()).await.unwrap();

        m.content = "rewritten terraform notes".to_string();
        m.updated_at = chrono::Utc::now();
        store.update(m).await.unwrap();

        let new = store
            .search(SearchQuery {
                query: "terraform".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert_eq!(new.results.len(), 1);

        let old = store
            .search(SearchQuery {
                query: "kubernetes".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert!(
            old.results.is_empty(),
            "stale FTS row would resurrect deleted text"
        );
    }

    /// Deleting a memory must drop its FTS row too.
    #[tokio::test]
    async fn test_search_delete_removes_fts() {
        let store = SqliteStore::in_memory().unwrap();
        let m = Memory::new(
            MemoryType::Fact,
            "ephemeral rollup cache".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(m.clone()).await.unwrap();
        store.delete(&m.id).await.unwrap();

        let outcome = store
            .search(SearchQuery {
                query: "rollup".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert!(outcome.results.is_empty());
    }

    /// A fresh database baselines one checksum row per migration, and the
    /// fingerprint reports them.
    #[tokio::test]
    async fn test_schema_migrations_baselined_on_fresh_db() {
        let store = SqliteStore::in_memory().unwrap();
        let conn = store.pool.get().unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows as usize, super::SqliteStore::MIGRATIONS.len());
        drop(conn);

        let (version, fingerprint) = store.schema_fingerprint().unwrap();
        assert_eq!(version as usize, super::SqliteStore::MIGRATIONS.len());
        assert!(!fingerprint.is_empty() && fingerprint != "none");
    }

    /// If a recorded migration's text changes after it was applied, opening
    /// the database must fail closed — silent schema drift is exactly what
    /// this guard exists to catch.
    #[tokio::test]
    async fn test_schema_drift_fails_closed() {
        let tmp = std::env::temp_dir().join(format!("memvault-drift-{}.db", uuid::Uuid::new_v4()));
        {
            // Normal open: baselines checksums.
            let _store = SqliteStore::new(&tmp).unwrap();
        }
        {
            // Tamper with a recorded checksum behind the store's back.
            let conn = Connection::open(&tmp).unwrap();
            conn.execute(
                "UPDATE schema_migrations SET checksum = 'deadbeef' WHERE version = 1",
                [],
            )
            .unwrap();
        }
        // Reopen: the mismatch must surface as SchemaDrift, not a silent pass.
        match SqliteStore::new(&tmp) {
            Err(MemVaultError::SchemaDrift(msg)) => {
                assert!(msg.contains("migration v1"), "unexpected message: {msg}");
            }
            Err(other) => panic!("expected SchemaDrift, got {other:?}"),
            Ok(_) => panic!("tampered checksum must fail the open"),
        }
        std::fs::remove_file(&tmp).ok();
    }

    /// Reopening an untouched database verifies cleanly (checksums stable
    /// across opens).
    #[tokio::test]
    async fn test_schema_checksum_stable_across_reopens() {
        let tmp = std::env::temp_dir().join(format!("memvault-reopen-{}.db", uuid::Uuid::new_v4()));
        {
            let _store = SqliteStore::new(&tmp).unwrap();
        }
        // Second and third opens must not report drift.
        let store = SqliteStore::new(&tmp).unwrap();
        let (version, _) = store.schema_fingerprint().unwrap();
        assert!(version > 0);
        drop(store);
        let _store = SqliteStore::new(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();
    }

    /// Upgrade path: rows written straight into `memories` by an older
    /// version (before the FTS index existed) get indexed on next open.
    #[tokio::test]
    async fn test_fts_backfill_for_preexisting_rows() {
        let tmp =
            std::env::temp_dir().join(format!("memvault-fts-backfill-{}.db", uuid::Uuid::new_v4()));
        {
            let store = SqliteStore::new(&tmp).unwrap();
            store
                .save(Memory::new(
                    MemoryType::Fact,
                    "preexisting graphite dashboard".to_string(),
                    Priority::Reference,
                    test_agent(),
                ))
                .await
                .unwrap();
        }
        // Simulate an index-less legacy state: wipe the FTS table behind the
        // store's back, leaving the memories row intact.
        {
            let conn = Connection::open(&tmp).unwrap();
            conn.execute("DELETE FROM memories_fts", []).unwrap();
        }
        // Reopen: count mismatch must trigger a rebuild.
        let store = SqliteStore::new(&tmp).unwrap();
        let outcome = store
            .search(SearchQuery {
                query: "graphite".to_string(),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.results.len(),
            1,
            "reopen must rebuild the FTS index from memories"
        );
        std::fs::remove_file(&tmp).ok();
    }

    /// Regression: `LIMIT top_k*3` was unbounded on the top (enormous values
    /// → huge scans), so top_k is clamped to 1000. And `top_k=0` is the
    /// caller's explicit "zero results" cap: keyword search must return
    /// empty, NOT clamp up to 1 — the old `clamp(1, 1000)` leaked a single
    /// row in keyword mode where semantic/hybrid returned [].
    #[tokio::test]
    async fn test_search_top_k_zero_is_empty_and_upper_clamped() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "shared keyword".to_string(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();

        // top_k = 0 → zero rows, uniformly with the other search modes.
        let results = store
            .search(SearchQuery {
                query: "shared".to_string(),
                top_k: 0,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert!(
            results.is_empty(),
            "top_k=0 must return zero rows, got {}",
            results.len()
        );

        // top_k = 1 → exactly one row: the smallest positive cap, and every
        // ordinary positive value, must be unaffected by the zero handling.
        let results = store
            .search(SearchQuery {
                query: "shared".to_string(),
                top_k: 1,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert_eq!(results.len(), 1, "top_k=1 must return exactly one row");

        // Enormous top_k must not blow up the LIMIT.
        let results = store
            .search(SearchQuery {
                query: "shared".to_string(),
                top_k: usize::MAX,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap()
            .results;
        assert!(!results.is_empty());
    }

    /// `vector_search` must honor top_k=0 as "zero results" too — the
    /// semantic/hybrid modes observed empty on zero while keyword leaked a
    /// row; this pins the vector side of that contract.
    #[tokio::test]
    async fn test_vector_search_top_k_zero_is_empty() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save_with_embedding(
                Memory::new(
                    MemoryType::Fact,
                    "embedded fact".to_string(),
                    Priority::Reference,
                    test_agent(),
                ),
                vec![0.9, 0.1, 0.0],
            )
            .await
            .unwrap();

        let results = store
            .vector_search(&[0.9, 0.1, 0.0], 0, None)
            .await
            .unwrap();
        assert!(
            results.is_empty(),
            "vector_search(top_k=0) must return zero rows, got {}",
            results.len()
        );

        // Positive control: the same query with top_k=1 still finds it.
        let results = store
            .vector_search(&[0.9, 0.1, 0.0], 1, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1, "top_k=1 must still return the match");
    }

    /// New embeddings are persisted int8-quantized; `get_embedding`
    /// dequantizes back to a vector that preserves the original direction.
    #[tokio::test]
    async fn test_embedding_stored_as_int8_and_roundtrips() {
        let store = SqliteStore::in_memory().unwrap();
        let m = Memory::new(
            MemoryType::Fact,
            "quantized".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let original = vec![0.6f32, 0.8, 0.0, -0.2, 0.1];
        store
            .save_with_embedding(m.clone(), original.clone())
            .await
            .unwrap();

        let conn = store.pool.get().unwrap();
        let (fmt, blob_len): (i64, i64) = conn
            .query_row(
                "SELECT embedding_fmt, length(embedding) FROM memories WHERE id = ?1",
                rusqlite::params![m.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(fmt, 1, "new embeddings must use the int8 format");
        assert_eq!(
            blob_len as usize,
            4 + original.len(),
            "int8 blob = 4-byte scale + one byte per dimension"
        );
        drop(conn);

        let back = store.get_embedding(&m.id).await.unwrap().unwrap();
        assert_eq!(back.len(), original.len());
        // Dequantized vectors are L2-normalized; compare direction, not scale.
        let sim = crate::embedding::cosine_similarity(&original, &back);
        assert!(sim > 0.99, "roundtrip lost direction (cosine {sim})");
    }

    /// Legacy f32 rows and new int8 rows coexist in one corpus and are scored
    /// comparably (the migration path for existing databases).
    #[tokio::test]
    async fn test_vector_search_mixed_formats() {
        let store = SqliteStore::in_memory().unwrap();

        let m_new = Memory::new(
            MemoryType::Fact,
            "int8 row".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store
            .save_with_embedding(m_new.clone(), vec![0.9, 0.1, 0.0])
            .await
            .unwrap();

        let m_legacy = Memory::new(
            MemoryType::Fact,
            "legacy f32 row".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(m_legacy.clone()).await.unwrap();
        {
            // Rewrite one row in the legacy f32 format behind the API.
            let conn = store.pool.get().unwrap();
            let blob: Vec<u8> = [0.1f32, 0.9, 0.0]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            conn.execute(
                "UPDATE memories SET embedding = ?2, embedding_fmt = 0 WHERE id = ?1",
                rusqlite::params![m_legacy.id, blob],
            )
            .unwrap();
        }

        let results = store
            .vector_search(&[0.85, 0.15, 0.0], 10, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].memory.content.contains("int8"));
        assert!(results[0].score > results[1].score);
    }

    /// A dimension mismatch means the embedding model changed: the row is
    /// skipped (searchable again once re-embedded), not an error.
    #[tokio::test]
    async fn test_vector_search_skips_mismatched_dimensions() {
        let store = SqliteStore::in_memory().unwrap();
        store
            .save_with_embedding(
                Memory::new(
                    MemoryType::Fact,
                    "old-model embedding".to_string(),
                    Priority::Reference,
                    test_agent(),
                ),
                vec![0.9, 0.1, 0.0], // 3 dims
            )
            .await
            .unwrap();

        // Query with a different dimensionality: no error, row skipped.
        let results = store
            .vector_search(&[0.9, 0.1, 0.0, 0.0, 0.0], 10, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_vector_search_namespace_scope() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();

        let mut m_ns = Memory::new(
            MemoryType::Fact,
            "embedded in ns".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m_ns.namespace = "project:gamma".to_string();

        let m_global = Memory::new(
            MemoryType::Fact,
            "embedded global".to_string(),
            Priority::Reference,
            agent,
        );

        store
            .save_with_embedding(m_ns, vec![1.0, 0.0])
            .await
            .unwrap();
        store
            .save_with_embedding(m_global, vec![1.0, 0.0])
            .await
            .unwrap();

        let in_alpha = store
            .vector_search(&[0.99, 0.0], 10, Some("project:alpha"))
            .await
            .unwrap();
        assert!(in_alpha.is_empty(), "no embedded memories in project:alpha");

        let in_gamma = store
            .vector_search(&[0.99, 0.0], 10, Some("project:gamma"))
            .await
            .unwrap();
        assert_eq!(in_gamma.len(), 1);
        assert!(in_gamma[0].memory.content.contains("embedded in ns"));

        let all = store.vector_search(&[0.99, 0.0], 10, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_get_embedding_missing_returns_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let err = store.get_embedding("no-such").await.unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_set_embedding_missing_returns_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let err = store.set_embedding("no-such", vec![1.0]).await.unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_list_pagination() {
        let store = SqliteStore::in_memory().unwrap();
        for i in 0..5 {
            store
                .save(Memory::new(
                    MemoryType::Fact,
                    format!("paged {}", i),
                    Priority::Reference,
                    test_agent(),
                ))
                .await
                .unwrap();
        }
        let page1 = store.list(None, 2, 0).await.unwrap();
        let page3 = store.list(None, 2, 4).await.unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page3.len(), 1);
    }

    #[tokio::test]
    async fn test_list_namespace_filter() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();
        let mut m = Memory::new(
            MemoryType::Fact,
            "scoped".to_string(),
            Priority::Reference,
            agent,
        );
        m.namespace = "project:gamma".to_string();
        store.save(m).await.unwrap();

        assert!(
            store
                .list(Some("project:gamma"), 10, 0)
                .await
                .unwrap()
                .len()
                == 1
        );
        assert!(store.list(Some("global"), 10, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_record_access_empty_noop() {
        let store = SqliteStore::in_memory().unwrap();
        store.record_access(&[]).await.unwrap();
    }

    #[tokio::test]
    async fn test_record_access_batch_increments() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();
        let m1 = Memory::new(
            MemoryType::Fact,
            "m1".into(),
            Priority::Reference,
            agent.clone(),
        );
        let m2 = Memory::new(MemoryType::Fact, "m2".into(), Priority::Reference, agent);
        let id1 = m1.id.clone();
        let id2 = m2.id.clone();
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        store
            .record_access(&[id1.clone(), id2.clone()])
            .await
            .unwrap();

        assert_eq!(store.get(&id1).await.unwrap().access_count, 1);
        assert_eq!(store.get(&id2).await.unwrap().access_count, 1);
        assert!(store.get(&id1).await.unwrap().last_read_at.is_some());
    }

    /// A batch mixing real and nonexistent ids must report how many rows
    /// actually changed, not `ids.len()` — a caller (CLI `confirm-read`,
    /// REST/MCP `confirm_read`) that echoes the input length back as
    /// "confirmed" overstates the result for every bad id in the batch.
    #[tokio::test]
    async fn test_record_access_reports_only_rows_actually_changed() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = test_agent();
        let m1 = Memory::new(
            MemoryType::Fact,
            "m1".into(),
            Priority::Reference,
            agent.clone(),
        );
        let m2 = Memory::new(MemoryType::Fact, "m2".into(), Priority::Reference, agent);
        let id1 = m1.id.clone();
        let id2 = m2.id.clone();
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let changed = store
            .record_access(&[id1.clone(), id2.clone(), "does-not-exist".to_string()])
            .await
            .unwrap();

        assert_eq!(
            changed, 2,
            "only the 2 real ids should count, not the full batch of 3"
        );
    }

    #[tokio::test]
    async fn test_list_without_embedding() {
        let store = SqliteStore::in_memory().unwrap();
        let no_emb = Memory::new(
            MemoryType::Fact,
            "needs embedding".into(),
            Priority::Reference,
            test_agent(),
        );
        let has_emb = Memory::new(
            MemoryType::Fact,
            "has embedding".into(),
            Priority::Reference,
            test_agent(),
        );
        store.save(no_emb).await.unwrap();
        store
            .save_with_embedding(has_emb, vec![0.1, 0.1])
            .await
            .unwrap();

        let missing = store.list_without_embedding(10).await.unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].content.contains("needs"));
    }

    #[tokio::test]
    async fn test_sync_state_hash_reflects_changes() {
        let store = SqliteStore::in_memory().unwrap();
        let empty = store.sync_state_hash().await.unwrap();

        store
            .save(Memory::new(
                MemoryType::Fact,
                "state change".into(),
                Priority::Reference,
                test_agent(),
            ))
            .await
            .unwrap();
        let after_insert = store.sync_state_hash().await.unwrap();
        assert_ne!(
            empty, after_insert,
            "hash must change after adding a memory"
        );

        // Deleting back should not necessarily restore the original hash, but
        // the value must be stable across reads of the same state.
        let again = store.sync_state_hash().await.unwrap();
        assert_eq!(after_insert, again);
        assert!(store.list(None, 10, 0).await.unwrap().len() == 1);
    }

    #[tokio::test]
    async fn test_duplicate_save_errors() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "dup key".into(),
            Priority::Reference,
            test_agent(),
        );
        store.save(mem.clone()).await.unwrap();
        let err = store.save(mem).await.unwrap_err();
        assert!(
            matches!(err, MemVaultError::Sqlite(_)),
            "inserting a duplicate primary key must surface a storage error"
        );
    }

    fn episode_memory(task: &str) -> Memory {
        let mut mem = Memory::new(
            MemoryType::Episode,
            task.to_string(),
            Priority::Background,
            test_agent(),
        );
        mem.layer = MemoryLayer::L1;
        mem
    }

    #[tokio::test]
    async fn test_episode_record_roundtrip() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = episode_memory("deployed the dashboard");
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        store
            .record_episode(EpisodeRecord {
                memory_id: id.clone(),
                task: "deploy the dashboard".into(),
                task_type: Some("deploy".into()),
                status: OutcomeStatus::Failure,
                cause: Some("missing env var".into()),
                lesson: None,
                lesson_memory_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        let got = store.get_episode(&id).await.unwrap();
        assert_eq!(got.task, "deploy the dashboard");
        assert_eq!(got.task_type.as_deref(), Some("deploy"));
        assert_eq!(got.status, OutcomeStatus::Failure);
        assert_eq!(got.cause.as_deref(), Some("missing env var"));
        assert!(got.lesson.is_none());
    }

    #[tokio::test]
    async fn test_episode_requires_existing_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let err = store
            .record_episode(EpisodeRecord {
                memory_id: "mem_missing".into(),
                task: "anything".into(),
                task_type: None,
                status: OutcomeStatus::Success,
                cause: None,
                lesson: None,
                lesson_memory_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
        assert!(matches!(
            store.get_episode("mem_missing").await.unwrap_err(),
            MemVaultError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn test_list_episodes_filters() {
        let store = SqliteStore::in_memory().unwrap();

        for (task, task_type, status) in [
            ("deploy a", "deploy", OutcomeStatus::Failure),
            ("deploy b", "deploy", OutcomeStatus::Success),
            ("debug c", "debug", OutcomeStatus::Failure),
        ] {
            let mem = episode_memory(task);
            let id = mem.id.clone();
            store.save(mem).await.unwrap();
            store
                .record_episode(EpisodeRecord {
                    memory_id: id,
                    task: task.into(),
                    task_type: Some(task_type.into()),
                    status,
                    cause: None,
                    lesson: None,
                    lesson_memory_id: None,
                    occurred_at: chrono::Utc::now(),
                })
                .await
                .unwrap();
        }

        let all = store.list_episodes(EpisodeFilter::default()).await.unwrap();
        assert_eq!(all.len(), 3);

        let deploys = store
            .list_episodes(EpisodeFilter {
                task_type: Some("deploy".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(deploys.len(), 2);

        let failed = store
            .list_episodes(EpisodeFilter {
                status: Some(OutcomeStatus::Failure),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(failed.len(), 2);

        let failed_deploys = store
            .list_episodes(EpisodeFilter {
                task_type: Some("deploy".into()),
                status: Some(OutcomeStatus::Failure),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(failed_deploys.len(), 1);
        assert_eq!(failed_deploys[0].task, "deploy a");

        let bounded = store
            .list_episodes(EpisodeFilter {
                limit: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(bounded.len(), 1);
    }

    #[tokio::test]
    async fn test_update_episode_lesson() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = episode_memory("deployed the api");
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store
            .record_episode(EpisodeRecord {
                memory_id: id.clone(),
                task: "deploy the api".into(),
                task_type: Some("deploy".into()),
                status: OutcomeStatus::Failure,
                cause: None,
                lesson: None,
                lesson_memory_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        store
            .update_episode_lesson(&id, "always check env vars first", Some("mem_lesson_1"))
            .await
            .unwrap();
        let got = store.get_episode(&id).await.unwrap();
        assert_eq!(got.lesson.as_deref(), Some("always check env vars first"));
        assert_eq!(got.lesson_memory_id.as_deref(), Some("mem_lesson_1"));

        assert!(matches!(
            store
                .update_episode_lesson("mem_missing", "x", None)
                .await
                .unwrap_err(),
            MemVaultError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn test_delete_memory_cascades_episode() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = episode_memory("deployed the web");
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store
            .record_episode(EpisodeRecord {
                memory_id: id.clone(),
                task: "deploy the web".into(),
                task_type: None,
                status: OutcomeStatus::Success,
                cause: None,
                lesson: None,
                lesson_memory_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        MemoryStore::delete(&store, &id).await.unwrap();
        assert!(
            matches!(
                store.get_episode(&id).await.unwrap_err(),
                MemVaultError::NotFound(_)
            ),
            "episode row must not outlive its memory"
        );
    }

    #[tokio::test]
    async fn test_superseded_by_roundtrip() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "old fact".into(),
            Priority::Reference,
            test_agent(),
        );
        mem.superseded_by = Some("mem_newer".into());
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let mut got = store.get(&id).await.unwrap();
        assert_eq!(got.superseded_by.as_deref(), Some("mem_newer"));

        // Clear it again via update.
        got.superseded_by = None;
        store.update(got).await.unwrap();
        let again = store.get(&id).await.unwrap();
        assert!(again.superseded_by.is_none());
    }

    fn skill_memory(name: &str) -> Memory {
        let mut mem = Memory::new(
            MemoryType::Skill,
            name.to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.skill_meta = Some(crate::models::SkillMeta {
            trigger: Some("deploy".into()),
            steps: vec!["step".into()],
            verification: None,
            version: 1,
        });
        mem
    }

    #[tokio::test]
    async fn test_skill_stats_none_when_never_tracked() {
        let store = SqliteStore::in_memory().unwrap();
        assert!(store.get_skill_stats("mem_absent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_skill_stats_injection_and_outcomes_accumulate() {
        let store = SqliteStore::in_memory().unwrap();
        let skill = skill_memory("deploy runbook");
        let id = skill.id.clone();
        store.save(skill).await.unwrap();

        store.record_skill_injection(&id).await.unwrap();
        store.record_skill_injection(&id).await.unwrap();
        store.record_skill_outcome(&id, true).await.unwrap();
        store.record_skill_outcome(&id, true).await.unwrap();
        store.record_skill_outcome(&id, false).await.unwrap();

        let stats = store.get_skill_stats(&id).await.unwrap().expect("tracked");
        assert_eq!(stats.injected_count, 2);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.failure_count, 1);
        assert_eq!(stats.executions(), 3);
        let rate = stats.success_rate().expect(">=3 samples shows a rate");
        assert!((rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_skill_rate_hidden_below_min_samples() {
        let store = SqliteStore::in_memory().unwrap();
        let skill = skill_memory("deploy runbook");
        let id = skill.id.clone();
        store.save(skill).await.unwrap();

        store.record_skill_outcome(&id, true).await.unwrap();
        store.record_skill_outcome(&id, true).await.unwrap();

        let stats = store.get_skill_stats(&id).await.unwrap().unwrap();
        assert_eq!(stats.executions(), 2);
        assert!(
            stats.success_rate().is_none(),
            "rate must stay hidden below {} samples",
            crate::models::SKILL_RATE_MIN_SAMPLES
        );
    }

    #[tokio::test]
    async fn test_delete_skill_cascades_stats() {
        let store = SqliteStore::in_memory().unwrap();
        let skill = skill_memory("deploy runbook");
        let id = skill.id.clone();
        store.save(skill).await.unwrap();
        store.record_skill_injection(&id).await.unwrap();

        MemoryStore::delete(&store, &id).await.unwrap();
        assert!(
            store.get_skill_stats(&id).await.unwrap().is_none(),
            "stats must not outlive the skill"
        );
    }

    fn fact(content: &str) -> Memory {
        Memory::new(
            MemoryType::Fact,
            content.to_string(),
            Priority::Reference,
            test_agent(),
        )
    }

    fn relation(subject: &str, predicate: &str, object_text: &str) -> MemoryRelation {
        MemoryRelation {
            relation_id: None,
            subject_id: subject.to_string(),
            predicate: predicate.to_string(),
            object_id: None,
            object_text: Some(object_text.to_string()),
            confidence: 0.8,
            source_memory_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_relation_roundtrip_and_lookup() {
        let store = SqliteStore::in_memory().unwrap();
        let dashboard = fact("dashboard service");
        let postgres = fact("PostgreSQL");
        let d_id = dashboard.id.clone();
        let p_id = postgres.id.clone();
        store.save(dashboard).await.unwrap();
        store.save(postgres).await.unwrap();

        // object as memory id
        let rid = store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: d_id.clone(),
                predicate: "depends_on".to_string(),
                object_id: Some(p_id.clone()),
                object_text: None,
                confidence: 0.9,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        assert!(rid >= 1);

        // object as free text
        store
            .add_relation(relation(&d_id, "located_in", "prod-cluster-a"))
            .await
            .unwrap();

        let out = store.relations_of_subject(&d_id).await.unwrap();
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .any(|r| r.predicate == "depends_on"
                    && r.object_id.as_deref() == Some(p_id.as_str()))
        );
        assert!(
            out.iter().any(|r| r.predicate == "located_in"
                && r.object_text.as_deref() == Some("prod-cluster-a"))
        );

        let inbound = store.relations_of_object(&p_id).await.unwrap();
        assert_eq!(inbound.len(), 1);
        assert_eq!(inbound[0].subject_id, d_id);
    }

    #[tokio::test]
    async fn test_relation_validation() {
        let store = SqliteStore::in_memory().unwrap();
        let a = fact("alpha");
        let a_id = a.id.clone();
        store.save(a).await.unwrap();

        // both object forms
        let err = store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: a_id.clone(),
                predicate: "uses".to_string(),
                object_id: Some(a_id.clone()),
                object_text: Some("x".to_string()),
                confidence: 0.8,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::InvalidInput(_)));

        // neither object form
        let err = store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: a_id.clone(),
                predicate: "uses".to_string(),
                object_id: None,
                object_text: None,
                confidence: 0.8,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::InvalidInput(_)));

        // missing subject
        let err = store
            .add_relation(relation("mem_missing", "uses", "x"))
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));

        // missing object_id target
        let err = store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: a_id.clone(),
                predicate: "uses".to_string(),
                object_id: Some("mem_missing".to_string()),
                object_text: None,
                confidence: 0.8,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_delete_memory_cascades_relations() {
        let store = SqliteStore::in_memory().unwrap();
        let a = fact("alpha");
        let a_id = a.id.clone();
        store.save(a).await.unwrap();
        store
            .add_relation(relation(&a_id, "uses", "libfoo"))
            .await
            .unwrap();

        MemoryStore::delete(&store, &a_id).await.unwrap();
        assert!(
            store.relations_of_subject(&a_id).await.unwrap().is_empty(),
            "relations must not outlive their subject memory"
        );
    }

    #[tokio::test]
    async fn test_delete_relation() {
        let store = SqliteStore::in_memory().unwrap();
        let a = fact("alpha");
        let a_id = a.id.clone();
        store.save(a).await.unwrap();
        let rid = store
            .add_relation(relation(&a_id, "uses", "libfoo"))
            .await
            .unwrap();

        store.delete_relation(rid).await.unwrap();
        assert!(store.relations_of_subject(&a_id).await.unwrap().is_empty());
        assert!(matches!(
            store.delete_relation(rid).await.unwrap_err(),
            MemVaultError::NotFound(_)
        ));
    }

    #[tokio::test]
    async fn test_supersede_archives_old_and_hides_from_search() {
        let store = SqliteStore::in_memory().unwrap();
        let old = fact("the api runs on port 8080");
        let new = fact("the api runs on port 9090");
        let old_id = old.id.clone();
        let new_id = new.id.clone();
        store.save(old).await.unwrap();
        store.save(new).await.unwrap();

        store.supersede(&old_id, &new_id).await.unwrap();

        let archived = store.get(&old_id).await.unwrap();
        assert_eq!(archived.superseded_by.as_deref(), Some(new_id.as_str()));
        assert_eq!(archived.layer, MemoryLayer::L0);

        // Search serves the current truth only.
        let hits = store
            .search(SearchQuery {
                query: "port".to_string(),
                top_k: 10,
                ..SearchQuery::new("port".to_string())
            })
            .await
            .unwrap()
            .results;
        assert!(hits.iter().any(|r| r.memory.id == new_id));
        assert!(!hits.iter().any(|r| r.memory.id == old_id));

        // list() still surfaces the archived memory (history stays visible).
        let all = store.list(None, 100, 0).await.unwrap();
        assert!(all.iter().any(|m| m.id == old_id));
    }

    #[tokio::test]
    async fn test_supersede_validation() {
        let store = SqliteStore::in_memory().unwrap();
        let a = fact("one");
        let a_id = a.id.clone();
        store.save(a).await.unwrap();

        // self-supersede rejected
        assert!(matches!(
            store.supersede(&a_id, &a_id).await.unwrap_err(),
            MemVaultError::InvalidInput(_)
        ));
        // missing replacement rejected
        assert!(matches!(
            store.supersede(&a_id, "mem_missing").await.unwrap_err(),
            MemVaultError::NotFound(_)
        ));
        // missing old rejected
        assert!(matches!(
            store.supersede("mem_missing", &a_id).await.unwrap_err(),
            MemVaultError::NotFound(_)
        ));
    }
    #[tokio::test]
    async fn test_save_canonicalizes_project_namespace() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "canonical namespace".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.namespace = "project:MemVault/".to_string();
        let id = mem.id.clone();
        let saved = store.save(mem).await.unwrap();
        // Write path stores (and returns) the canonical spelling.
        assert_eq!(saved.namespace, "project:memvault");

        // Read paths agree regardless of the caller-supplied spelling.
        for spelling in ["project:MemVault/", "project:memvault"] {
            let listed = store.list(Some(spelling), 10, 0).await.unwrap();
            assert_eq!(listed.len(), 1, "list({spelling}) must match");
            assert_eq!(listed[0].id, id);
            assert_eq!(listed[0].namespace, "project:memvault");

            let pending = store.list_pending(Some(spelling), 10, 0).await.unwrap();
            assert_eq!(pending.len(), 1, "list_pending({spelling}) must match");
        }

        let searched = store
            .search(SearchQuery {
                query: "canonical".to_string(),
                namespace: Some("project:MemVault".to_string()),
                ..SearchQuery::new("canonical".to_string())
            })
            .await
            .unwrap();
        assert_eq!(searched.results.len(), 1, "search must match too");

        // Non-project namespaces pass through untouched.
        let mut g = Memory::new(
            MemoryType::Fact,
            "global".to_string(),
            Priority::Reference,
            test_agent(),
        );
        g.namespace = "archived:OldProject/".to_string();
        let saved_g = store.save(g).await.unwrap();
        assert_eq!(saved_g.namespace, "archived:OldProject/");
    }

    #[tokio::test]
    async fn test_backfill_canonicalizes_existing_project_namespaces() {
        let store = SqliteStore::in_memory().unwrap();
        // Simulate rows written before normalization by inserting directly.
        {
            let conn = store.pool.get().unwrap();
            for (id, ns) in [
                ("legacy_mixed", "project:MemVault"),
                ("legacy_slash", "project:my-app/"),
                ("legacy_canon", "project:my-app"),
                ("legacy_other", "archived:OldProject/"),
            ] {
                collect(&conn, id, ns);
            }
        }
        fn collect(conn: &rusqlite::Connection, id: &str, ns: &str) {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO memories (id, memory_type, content, priority, source_agent_id, source_agent_type, namespace, created_at, updated_at)
                 VALUES (?1, 'fact', 'legacy', 'REFERENCE', 'test-agent', 'coding-assistant', ?2, ?3, ?3)",
                rusqlite::params![id, ns, now],
            )
            .unwrap();
        }

        {
            let conn = store.pool.get().unwrap();
            SqliteStore::backfill_canonical_project_namespaces(&conn).unwrap();
            // Idempotent: a second pass changes nothing.
            SqliteStore::backfill_canonical_project_namespaces(&conn).unwrap();
        }

        let mixed = store.list(Some("project:memvault"), 10, 0).await.unwrap();
        assert_eq!(mixed.len(), 1, "legacy mixed-case row migrated");
        assert_eq!(mixed[0].id, "legacy_mixed");

        let my_app = store.list(Some("project:my-app/"), 10, 0).await.unwrap();
        let ids: Vec<String> = my_app.iter().map(|m| m.id.clone()).collect();
        assert!(
            ids.contains(&"legacy_slash".to_string()),
            "legacy trailing-slash row migrated"
        );
        assert!(
            ids.contains(&"legacy_canon".to_string()),
            "already-canonical row untouched"
        );
        assert_eq!(my_app.len(), 2, "backfill is idempotent");

        // Unrelated namespace kinds are never migrated.
        let archived = store
            .list(Some("archived:OldProject/"), 10, 0)
            .await
            .unwrap();
        assert_eq!(archived.len(), 1);
    }
}
