use async_trait::async_trait;
use chrono::SubsecRound;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::{debug, info, warn};

use super::approval_journal::{ApprovalPair, revision_fingerprint};
use super::{
    ApproveSupersedeOutcome, MAX_VECTOR_SCAN_CANDIDATES, MemoryStore, SyncPage, VectorSearchOutcome,
};
use crate::embedding::cosine_similarity;
use crate::error::{MemVaultError, Result};
use crate::models::*;

type Pool = r2d2::Pool<SqliteConnectionManager>;

fn optional_fact_json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    name: &str,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(name)?
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    row.as_ref().column_index(name).unwrap_or(0),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

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

fn sync_cursor_for_id(id: &str) -> String {
    format!("v1:{}", hex::encode(Sha256::digest(id.as_bytes())))
}

fn parse_sync_cursor(cursor: Option<&str>) -> Result<Option<&str>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let digest = cursor.strip_prefix("v1:");
    if cursor.len() > 1024
        || cursor.chars().any(char::is_control)
        || !digest.is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(MemVaultError::InvalidInput(
            "invalid sync page cursor".into(),
        ));
    }
    Ok(digest)
}

fn claim_sync_cursor_anchor(anchor: &mut Option<String>, id: String) -> Result<()> {
    if anchor.is_some() {
        // A hash token must identify exactly one current scoped member. Even
        // an ambiguous SHA-256 collision must not silently choose an anchor.
        return Err(MemVaultError::InvalidInput(
            "ambiguous sync page cursor; restart enumeration".into(),
        ));
    }
    *anchor = Some(id);
    Ok(())
}

pub struct SqliteStore {
    pool: Pool,
    #[cfg(test)]
    history_search_pause: std::sync::Mutex<Option<HistorySearchPause>>,
    #[cfg(test)]
    sync_page_pause: std::sync::Mutex<Option<SyncPagePause>>,
}

#[cfg(test)]
struct HistorySearchPause {
    projected: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
}

#[cfg(test)]
struct SyncPagePause {
    membership_ready: std::sync::Arc<std::sync::Barrier>,
    resume: std::sync::Arc<std::sync::Barrier>,
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
        let store = Self {
            pool,
            #[cfg(test)]
            history_search_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            sync_page_pause: std::sync::Mutex::new(None),
        };
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
        let store = Self {
            pool,
            #[cfg(test)]
            history_search_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            sync_page_pause: std::sync::Mutex::new(None),
        };
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

    /// Connection-scoped insert shared by literal saves and the atomic fact
    /// resolver. The caller owns validation, authority, transaction and commit.
    /// Both embedding paths use the same column list and int8 codec.
    fn write_insert_in_tx(
        conn: &Connection,
        memory: &Memory,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        Self::write_frozen_insert_in_tx(conn, memory, embedding)?;
        Self::suspend_proposed_target_in_tx(conn, memory)
    }

    /// Exact trusted insert, including record-bound fact management. Frozen
    /// journal publication/compensation must not re-normalize either participant
    /// or suspend a proposal target as a side effect of rebuilding cache rows.
    /// Public saves and fact resolution share this codec through the helper above.
    fn write_frozen_insert_in_tx(
        conn: &Connection,
        memory: &Memory,
        embedding: Option<&[f32]>,
    ) -> Result<()> {
        memory.validate_content()?;
        memory.validate_fact_metadata()?;
        check_not_sensitive(memory)?;
        let type_json = serde_json::to_string(&memory.memory_type)?;
        let priority_json = serde_json::to_string(&memory.priority)?;
        let layer_json = serde_json::to_string(&memory.layer)?;
        let (group_namespace, source_namespace, key) = crate::facts::group_fields(memory);
        let assertion_json = memory
            .fact_assertion
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let management_json = memory
            .fact_management
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let embedding_blob = embedding.map(Self::embedding_to_int8_blob);
        conn.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at,
             embedding, embedding_fmt, layer, skill_meta, superseded_by, visibility,
             identity_verified, corroborating_agents, occurred_at, source_trace_ids,
             friction_evidence, archived_at, expires_at, proposes_supersede_of,
             fact_assertion, fact_management, fact_group_namespace, fact_source_namespace, fact_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28,
             ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37)",
            rusqlite::params![
                memory.id,
                type_json.trim_matches('"'),
                memory.content,
                memory.instruction,
                priority_json.trim_matches('"'),
                memory.source_agent.id,
                memory.source_agent.agent_type,
                memory.source_agent.session_id,
                memory.namespace,
                memory.confidence,
                serde_json::to_string(&memory.tags)?,
                memory.created_at.to_rfc3339(),
                memory.updated_at.to_rfc3339(),
                memory.ai_generated,
                memory.human_reviewed,
                memory.decay_score,
                memory.access_count,
                memory.last_read_at.map(|dt| dt.to_rfc3339()),
                embedding_blob,
                i64::from(embedding.is_some()),
                layer_json.trim_matches('"'),
                memory
                    .skill_meta
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                memory.superseded_by,
                memory.visibility.as_str(),
                memory.identity_verified,
                serde_json::to_string(&memory.corroborating_agents)?,
                memory.occurred_at.map(|dt| dt.to_rfc3339()),
                serde_json::to_string(&memory.source_trace_ids)?,
                memory.friction_evidence,
                memory.archived_at.map(|dt| dt.to_rfc3339()),
                memory.expires_at.map(expiry_stamp),
                memory.proposes_supersede_of,
                assertion_json,
                management_json,
                group_namespace,
                source_namespace,
                key,
            ],
        )?;
        Self::fts_insert(conn, memory)
    }

    /// A pending human edit freezes the target immediately, not only on
    /// approval. Reused for both proposal creation and retargeting by update.
    fn suspend_proposed_target_in_tx(conn: &Connection, memory: &Memory) -> Result<()> {
        if let Some(target_id) = memory.proposes_supersede_of.as_deref()
            && let Some(mut target) = conn
                .query_row(
                    "SELECT * FROM memories WHERE id=?1",
                    rusqlite::params![target_id],
                    Self::row_to_memory,
                )
                .optional()?
            && target
                .fact_management
                .as_ref()
                .is_some_and(|mark| mark.state() == crate::facts::FactManagementState::Active)
        {
            target.fact_assertion = None;
            if let Some(mark) = target.fact_management.as_mut() {
                mark.suspend();
            }
            target.updated_at = chrono::Utc::now();
            Self::write_update_in_tx(conn, &target)?;
        }
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
        // Explicit assertion JSON and record-bound server management. Separate
        // nullable columns keep version-zero reconciliation statement-scoped.
        (29, "ALTER TABLE memories ADD COLUMN fact_assertion TEXT"),
        (30, "ALTER TABLE memories ADD COLUMN fact_management TEXT"),
        (
            31,
            "ALTER TABLE memories ADD COLUMN fact_group_namespace TEXT",
        ),
        (
            32,
            "ALTER TABLE memories ADD COLUMN fact_source_namespace TEXT",
        ),
        (33, "ALTER TABLE memories ADD COLUMN fact_key TEXT"),
        // Nonunique: force/preserve/import must remain literal inserts. A
        // suspended mark indexes its ORIGINAL namespace, keeping its guard
        // visible even after a human namespace edit. No independent heads.
        (
            34,
            "CREATE INDEX IF NOT EXISTS idx_memories_fact_group ON memories(fact_group_namespace, fact_source_namespace, fact_key) WHERE fact_key IS NOT NULL",
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
        Self::write_history_at(conn, memory, operation, chrono::Utc::now())
    }

    fn write_history_at(
        conn: &Connection,
        memory: &Memory,
        operation: &str,
        changed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let snapshot = serde_json::to_string(memory)?;
        conn.execute(
            "INSERT INTO memory_history (memory_id, operation, snapshot, changed_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                memory.id,
                operation,
                snapshot,
                changed_at.to_rfc3339()
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

    /// Reindex an already policy-normalized Markdown snapshot with an existing
    /// cache row. The caller holds the Markdown mutation locks and must never
    /// route raw/public intake here. Keep delete/reinsert history, cascades and
    /// embedding invalidation while preserving exact trusted fact guard metadata.
    pub(crate) fn resync_trusted_snapshot(&self, memory: &Memory) -> Result<()> {
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old = tx
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                rusqlite::params![memory.id],
                Self::row_to_memory,
            )
            .optional()?
            .ok_or_else(|| MemVaultError::NotFound(memory.id.clone()))?;
        Self::write_history(&tx, &old, "delete")?;
        tx.execute(
            "DELETE FROM memories WHERE id=?1",
            rusqlite::params![memory.id],
        )?;
        Self::fts_delete(&tx, &memory.id)?;
        Self::write_frozen_insert_in_tx(&tx, memory, None)?;
        tx.commit()?;
        Ok(())
    }

    /// Publish or compensate a frozen journal pair in one write transaction.
    /// The Markdown caller has already validated private authority, canonical
    /// files and tombstones. Missing cache rows are allowed after cache loss;
    /// later semantic revisions are never overwritten. This is not a public
    /// approval entry point and does not infer trust from either relationship.
    pub(super) fn publish_journal_pair(
        &self,
        expected: &ApprovalPair,
        desired: &ApprovalPair,
        excluded_ids: &std::collections::HashSet<String>,
    ) -> Result<()> {
        if expected.target.id != desired.target.id
            || expected.proposal.id != desired.proposal.id
            || expected.target.id == expected.proposal.id
        {
            return Err(MemVaultError::InvalidInput(
                "journal pair identity mismatch".to_string(),
            ));
        }
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut updates = Vec::new();
        for (before, after) in [
            (&expected.target, &desired.target),
            (&expected.proposal, &desired.proposal),
        ] {
            if excluded_ids.contains(&after.id) {
                continue;
            }
            after.validate_content()?;
            after.validate_fact_metadata()?;
            check_not_sensitive(after)?;
            if before.created_at != after.created_at
                || serde_json::to_value(&before.source_agent)?
                    != serde_json::to_value(&after.source_agent)?
            {
                return Err(MemVaultError::InvalidInput(
                    "journal changes immutable memory identity".to_string(),
                ));
            }
            let current = tx
                .query_row(
                    "SELECT * FROM memories WHERE id = ?1",
                    rusqlite::params![after.id],
                    Self::row_to_memory,
                )
                .optional()?;
            let mut frozen = after.clone();
            if let Some(current) = &current {
                let revision = revision_fingerprint(current)?;
                if revision == revision_fingerprint(after)? {
                    continue;
                }
                if revision != revision_fingerprint(before)? {
                    return Err(MemVaultError::Storage(format!(
                        "memory '{}' changed after approval was recorded; refusing stale replay",
                        after.id
                    )));
                }
                // Access bookkeeping is not part of the approved payload and
                // may legitimately have advanced while file writes were pending.
                frozen.access_count = current.access_count;
                frozen.last_read_at = current.last_read_at;
                frozen.decay_score = current.decay_score;
            }
            updates.push((current, frozen));
        }
        for (current, frozen) in updates {
            if let Some(current) = current {
                // This helper validates and writes exact metadata; ordinary
                // update() sanitization already happened before journal freeze.
                Self::write_update_in_tx(&tx, &frozen)?;
                let old_input = current.instruction.as_deref().unwrap_or(&current.content);
                let new_input = frozen.instruction.as_deref().unwrap_or(&frozen.content);
                if old_input != new_input {
                    tx.execute(
                        "UPDATE memories SET embedding = NULL WHERE id = ?1",
                        rusqlite::params![frozen.id],
                    )?;
                }
            } else {
                Self::write_frozen_insert_in_tx(&tx, &frozen, None)?;
            }
        }
        tx.commit()?;
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
        Self::write_update_in_tx_at(conn, memory, None)
    }

    /// An explicit history boundary lets atomic fact replacement share exactly
    /// one system transition instant with its successor. Other updates retain
    /// their existing history clock, independent of the supplied row timestamps.
    fn write_update_in_tx_at(
        conn: &Connection,
        memory: &Memory,
        changed_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        memory.validate_content()?;
        memory.validate_fact_metadata()?;
        check_not_sensitive(memory)?;
        let (group_namespace, source_namespace, key) = crate::facts::group_fields(memory);
        let assertion_json = memory
            .fact_assertion
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let management_json = memory
            .fact_management
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        // Snapshot the pre-update row so it can be restored later. If the
        // row doesn't exist, skip the snapshot and let the UPDATE below
        // affect 0 rows and return NotFound, same as before this change.
        if let Some(old) = conn
            .query_row(
                "SELECT * FROM memories WHERE id = ?1",
                rusqlite::params![memory.id],
                Self::row_to_memory,
            )
            .optional()?
        {
            if let Some(changed_at) = changed_at {
                Self::write_history_at(conn, &old, "update", changed_at)?;
            } else {
                Self::write_history(conn, &old, "update")?;
            }
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
             ai_generated=?24, expires_at=?25, proposes_supersede_of=?26,
             fact_assertion=?27, fact_management=?28, fact_group_namespace=?29,
             fact_source_namespace=?30, fact_key=?31,
             source_agent_id=?32, source_agent_type=?33, source_session_id=?34
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
                assertion_json, management_json, group_namespace, source_namespace, key,
                memory.source_agent.id, memory.source_agent.agent_type, memory.source_agent.session_id,
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
        let snapshots = stmt.query_map(rusqlite::params![id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        // Stream version chains: one memory with many revisions must not
        // allocate every historical body merely to select its effective state.
        for snapshot in snapshots {
            let (history_id, snapshot, changed_at) = snapshot?;
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

    fn search_result_order(a: &SearchResult, b: &SearchResult) -> std::cmp::Ordering {
        let a_must = a.memory.priority == Priority::Must;
        let b_must = b.memory.priority == Priority::Must;
        b_must
            .cmp(&a_must)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.memory.id.cmp(&b.memory.id))
    }

    /// Historical blank queries enumerate the full live/history ID union, but
    /// retain only the best eligible page. Text recall streams BM25 order until
    /// the existing relevance-reranking budget is filled by ELIGIBLE rows.
    /// Rejections have no hidden raw-candidate cap and never spend result slots.
    fn search_at(
        &self,
        conn: &Connection,
        query: &SearchQuery,
        as_of: chrono::DateTime<chrono::Utc>,
        top_k: usize,
    ) -> Result<SearchOutcome> {
        let text = query.query.trim();
        let now = chrono::Utc::now();
        if text.is_empty() {
            let mut results = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT id FROM memories UNION SELECT memory_id AS id FROM memory_history ORDER BY id",
            )?;
            let ids = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for id in ids {
                let Some(at) = Self::reconstruct_at(conn, &id?, as_of)? else {
                    continue;
                };
                if !super::memory_matches_filters(&at.memory, query, as_of) {
                    continue;
                }
                let hit = SearchResult {
                    score: Self::compute_relevance_score(&at.memory, &[], now),
                    memory: at.memory,
                    hit_sources: Vec::new(),
                    as_of_source: Some(at.source),
                };
                let position = results
                    .binary_search_by(|existing| Self::search_result_order(existing, &hit))
                    .unwrap_or_else(|position| position);
                if position < top_k {
                    results.insert(position, hit);
                    results.truncate(top_k);
                }
            }
            Self::finish_search_results(&mut results, top_k);
            return Ok(SearchOutcome {
                results,
                keyword_tier: KeywordTier::None,
            });
        }

        let tiers = crate::fts::query_token_tiers(text);
        let mut attempts = Vec::new();
        if let Some(strict) = tiers.first() {
            attempts.push((KeywordTier::Strict, crate::fts::build_match_expr(strict)?));
        }
        if let Some(relaxed) = tiers.get(1) {
            attempts.push((
                KeywordTier::RelaxedUnigram,
                crate::fts::build_match_expr(relaxed)?,
            ));
        }
        let search_words = crate::query_expand::expand_query(text);
        let mut fallback_tokens: Vec<String> = tiers.iter().flatten().cloned().collect();
        for word in &search_words {
            fallback_tokens.extend(crate::fts::tokenize(word));
        }
        if !fallback_tokens.is_empty() {
            attempts.push((
                KeywordTier::SynonymFallback,
                crate::fts::build_match_expr_or(&fallback_tokens)?,
            ));
        }
        for (tier, expr) in attempts {
            let mut results = Vec::new();
            {
                // No present-state predicates: a moved, retired or newly dated
                // live row can still reconstruct to an eligible historical row.
                // No raw LIMIT: inspect matches in deterministic BM25 order and
                // apply the candidate budget only AFTER reconstruction/filtering.
                let mut stmt = conn.prepare(
                    "SELECT m.* FROM memories_fts
                     JOIN memories m ON m.id = memories_fts.memory_id
                     WHERE memories_fts MATCH ?1
                     ORDER BY bm25(memories_fts) ASC, m.id ASC",
                )?;
                let rows = stmt.query_map(rusqlite::params![expr], Self::row_to_memory)?;
                for row in rows {
                    let live = row?;
                    let Some(at) = Self::reconstruct_at(conn, &live.id, as_of)? else {
                        continue;
                    };
                    if !super::memory_matches_filters(&at.memory, query, as_of) {
                        continue;
                    }
                    results.push(SearchResult {
                        // Preserve the score of the current-text recall path.
                        score: Self::compute_relevance_score(&live, &search_words, now),
                        memory: at.memory,
                        hit_sources: Vec::new(),
                        as_of_source: Some(at.source),
                    });
                    if results.len() >= top_k * 3 {
                        break;
                    }
                }
            }
            #[cfg(test)]
            self.pause_history_search()?;
            let historical = Self::search_history_at_expr(conn, query, as_of, top_k, &expr)?;
            let mut present: std::collections::HashMap<String, usize> = results
                .iter()
                .enumerate()
                .map(|(i, hit)| (hit.memory.id.clone(), i))
                .collect();
            for hit in historical {
                if let Some(&i) = present.get(&hit.memory.id) {
                    results[i].hit_sources.extend(hit.hit_sources);
                } else {
                    present.insert(hit.memory.id.clone(), results.len());
                    results.push(hit);
                }
            }
            // A raw current-only match does not choose the tier. Both channels
            // must qualify before any relaxation can be reported as answered.
            if !results.is_empty() {
                Self::finish_search_results(&mut results, top_k);
                return Ok(SearchOutcome {
                    results,
                    keyword_tier: tier,
                });
            }
        }
        Ok(SearchOutcome {
            results: Vec::new(),
            keyword_tier: KeywordTier::None,
        })
    }

    fn finish_search_results(results: &mut Vec<SearchResult>, top_k: usize) {
        results.sort_by(Self::search_result_order);
        results.truncate(top_k);
        for (idx, hit) in results.iter_mut().enumerate() {
            if hit.hit_sources.is_empty() {
                hit.hit_sources = vec![HitSource::Keyword { rank: idx + 1 }];
            }
        }
    }

    #[cfg(test)]
    fn pause_history_search(&self) -> Result<()> {
        let pause = self
            .history_search_pause
            .lock()
            .map_err(|_| MemVaultError::Storage("history search test hook poisoned".into()))?
            .take();
        if let Some(pause) = pause {
            pause.projected.wait();
            pause.resume.wait();
        }
        Ok(())
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
    ///
    /// Discover matching memory IDs without a raw candidate cap, reconstruct
    /// each once, and restrict the ranked FTS query to eligible snapshot IDs
    /// before paging. Wrong versions and out-of-filter memories must not spend
    /// result slots or make recall depend on the requested `top_k`.
    fn search_history_at_expr(
        conn: &Connection,
        query: &SearchQuery,
        as_of: chrono::DateTime<chrono::Utc>,
        limit: usize,
        match_expr: &str,
    ) -> Result<Vec<SearchResult>> {
        /// Eligible history rows pulled per ranked FTS page.
        const PAGE: i64 = 64;

        let query_text = query.query.trim();
        if query_text.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        // The caller chooses a tier only after BOTH live and historical
        // candidates have been projected and qualified. Keep the existing
        // snapshot-ID qualification and ranked historical paging here.

        let search_words = crate::query_expand::expand_query(query_text);
        let now = chrono::Utc::now();
        let mut out: Vec<SearchResult> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Cache eligible states and rejections for ranked paging. Matching N
        // wrong versions of one memory must not reconstruct its chain N times.
        let mut reconstructed: std::collections::HashMap<String, Option<MemoryAtTime>> =
            std::collections::HashMap::new();

        let expr = match_expr;
        let mut eligible_history_ids = Vec::new();
        {
            // Discovery is distinct by MEMORY, not by version, and has no
            // top_k-derived LIMIT. reconstruct_at remains the authority for
            // the one snapshot in effect; SQL must not approximate it.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT memory_id FROM memory_history_fts
                     WHERE memory_history_fts MATCH ?1",
            )?;
            let matching_ids =
                stmt.query_map(rusqlite::params![expr], |row| row.get::<_, String>(0))?;
            for memory_id in matching_ids {
                let memory_id = memory_id?;
                if seen.contains(&memory_id) {
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    reconstructed.entry(memory_id.clone())
                {
                    let eligible = Self::reconstruct_at(conn, &memory_id, as_of)?.filter(|at| {
                        matches!(at.source, AsOfSource::History { .. })
                            && crate::storage::memory_matches_filters(&at.memory, query, as_of)
                    });
                    entry.insert(eligible);
                }
                if let Some(Some(at)) = reconstructed.get(&memory_id)
                    && let AsOfSource::History { history_id, .. } = at.source
                {
                    eligible_history_ids.push(history_id);
                }
            }
        }
        if eligible_history_ids.is_empty() {
            return Ok(Vec::new());
        }
        // One bound JSON parameter avoids an SQL placeholder per eligible
        // ID (and SQLite's variable limit), with no table or migration.
        // MATCH is still applied to the eligible snapshot's own text: a
        // different matching version cannot make this version answer.
        let eligible_json = serde_json::to_string(&eligible_history_ids)?;
        let mut offset = 0i64;
        let mut rank = 0usize;

        loop {
            let mut stmt = conn.prepare(
                "SELECT history_id, memory_id FROM memory_history_fts
                     WHERE memory_history_fts MATCH ?1
                       AND history_id IN (SELECT value FROM json_each(?2))
                     ORDER BY bm25(memory_history_fts) ASC, history_id ASC
                     LIMIT ?3 OFFSET ?4",
            )?;
            let rows = stmt
                .query_map(
                    rusqlite::params![expr, eligible_json, PAGE, offset],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            if rows.is_empty() {
                break;
            }
            // Eligibility is applied BEFORE BM25 LIMIT/paging, so rejected
            // snapshots cannot spend result slots. history_id breaks BM25
            // ties deterministically across pages of the same read snapshot.
            let fetched = rows.len() as i64;

            for (history_id, memory_id) in rows {
                rank += 1;
                if seen.contains(&memory_id) {
                    continue;
                }
                let Some(Some(at)) = reconstructed.get(&memory_id) else {
                    continue;
                };
                if !matches!(
                    at.source,
                    AsOfSource::History { history_id: h, .. } if h == history_id
                ) {
                    continue;
                }
                // Claim the ID only after its selected row is validated.
                seen.insert(memory_id);
                let score = Self::compute_relevance_score(&at.memory, &search_words, now);
                out.push(SearchResult {
                    memory: at.memory.clone(),
                    score,
                    hit_sources: vec![HitSource::HistoryKeyword { rank }],
                    as_of_source: Some(at.source),
                });
                if out.len() >= limit {
                    break;
                }
            }

            if out.len() >= limit {
                break;
            }
            offset += fetched;
            if fetched < PAGE {
                // Actual exhaustion, not an arbitrary candidate budget.
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

        let mut memory: Memory = serde_json::from_str(&snapshot)?;
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                rusqlite::params![memory.id],
                Self::row_to_memory,
            )
            .optional()?;
        // Restoring content cannot erase or reassign a live record's stream
        // binding, even if its id was reused since this checkpoint. None is also
        // authoritative: only a genuinely absent row may inherit history's mark.
        if let Some(current) = current.as_ref() {
            memory.fact_management = current.fact_management.clone();
        }
        // A restore is a manual semantic decision, never an ownership grant.
        // Keep the persisted producer/order/scope as a suspended record guard;
        // the ordinary persistence helper derives its index from that binding.
        if let Some(mark) = memory.fact_management.as_mut() {
            mark.suspend();
            memory.fact_assertion = None;
        }
        if current.is_some() {
            Self::write_update_in_tx(&tx, &memory)?;
        } else {
            Self::write_insert_in_tx(&tx, &memory, None)?;
        }
        tx.commit()?;
        Ok(memory)
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

        let memory = Memory {
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
            fact_assertion: optional_fact_json(row, "fact_assertion")?,
            fact_management: optional_fact_json(row, "fact_management")?,
        };
        memory.validate_fact_metadata().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                row.as_ref().column_index("fact_assertion").unwrap_or(0),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(memory)
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
        let mut memory = memory;
        memory.sanitize_public_intake();
        memory.namespace = canonical_project_namespace(&memory.namespace).into_owned();
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction()?;
        Self::write_insert_in_tx(&tx, &memory, None)?;
        tx.commit()?;
        Ok(memory)
    }

    async fn resolve_fact(
        &self,
        memory: Memory,
        context: &crate::facts::FactWriteContext,
    ) -> Result<crate::facts::FactWriteResult> {
        use crate::facts::{
            FactManagement, FactManagementState, FactWriteOutcome, FactWriteResult,
        };
        let mut memory = memory;
        memory.sanitize_public_intake();
        memory.namespace = canonical_project_namespace(&memory.namespace).into_owned();
        memory.validate_content()?;
        memory.validate_fact_metadata()?;
        check_not_sensitive(&memory)?;
        // Match the existing SQLite codec's microsecond expiry precision for
        // equal-ordinal payload comparison and the returned persisted snapshot.
        memory.expires_at = memory.expires_at.map(|dt| dt.trunc_subsecs(6));
        if let Some(reason) = crate::facts::resolve_eligibility(&memory, context) {
            return Ok(FactWriteResult::new(
                FactWriteOutcome::ReviewRequired,
                None,
                Some(reason),
            ));
        }
        let producer = context
            .producer_id()
            .ok_or_else(|| MemVaultError::InvalidInput("verified producer required".into()))?;
        let assertion = memory
            .fact_assertion
            .as_ref()
            .ok_or_else(|| MemVaultError::InvalidInput("fact_assertion required".into()))?;
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        // Lookup AND first insert hold the same lock as retirement/replacement.
        // Never ignore expired heads or pick a head by updated_at. A manually
        // retired suspended guard also participates until deletion/forget.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let heads = tx.prepare(
            "SELECT * FROM memories WHERE fact_group_namespace=?1
             AND fact_source_namespace=?2 AND fact_key=?3
             AND (superseded_by IS NULL OR json_extract(fact_management, '$.state')='suspended') LIMIT 2",
        )?.query_map(rusqlite::params![memory.namespace, assertion.source_namespace, assertion.key], Self::row_to_memory)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if heads.len() > 1 {
            return Ok(FactWriteResult::new(
                FactWriteOutcome::Ambiguity,
                None,
                Some("multiple asserted heads or suspended guards; no automatic choice"),
            ));
        }
        if let Some(head) = heads.first() {
            let Some(management) = head.fact_management.as_ref() else {
                return Ok(FactWriteResult::new(
                    FactWriteOutcome::ReviewRequired,
                    Some(head.clone()),
                    Some("literal/imported/legacy head is not managed"),
                ));
            };
            if management.state() != FactManagementState::Active {
                return Ok(FactWriteResult::new(
                    FactWriteOutcome::ReviewRequired,
                    Some(head.clone()),
                    Some("stream management was suspended by an edit or review"),
                ));
            }
            if management.producer_id() != producer {
                return Ok(FactWriteResult::new(
                    FactWriteOutcome::Conflict,
                    Some(head.clone()),
                    Some("stream belongs to another verified producer"),
                ));
            }
            // Active record-bound management plus the current producer's key
            // proof establishes ownership, not the optional public identity flag.
            if crate::facts::resolve_eligibility(head, context).is_some()
                || head.fact_assertion.as_ref().is_none_or(|a| {
                    a.key != management.key()
                        || a.source_namespace != management.source_namespace()
                        || a.source_order != management.source_order()
                })
                || head.namespace != management.namespace()
            {
                return Ok(FactWriteResult::new(
                    FactWriteOutcome::ReviewRequired,
                    Some(head.clone()),
                    Some("head is no longer an eligible managed fact"),
                ));
            }
            match assertion.source_order.cmp(&management.source_order()) {
                std::cmp::Ordering::Less => {
                    return Ok(FactWriteResult::new(
                        FactWriteOutcome::Stale,
                        Some(head.clone()),
                        Some("source ordinal is older than the current head"),
                    ));
                }
                std::cmp::Ordering::Equal => {
                    let status = if crate::facts::same_semantic_payload(head, &memory) {
                        FactWriteOutcome::Idempotent
                    } else {
                        FactWriteOutcome::Conflict
                    };
                    return Ok(FactWriteResult::new(
                        status,
                        Some(head.clone()),
                        if status == FactWriteOutcome::Conflict {
                            Some("equal source ordinal carries a different semantic payload")
                        } else {
                            None
                        },
                    ));
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        // Stamp only this new row from the server context's recording policy.
        // Read-only retries and retirement never re-attest historical rows.
        memory.identity_verified = context.records_identity();
        // Body-provided corroboration is not authentication evidence.
        memory.corroborating_agents.clear();
        memory.fact_management = Some(FactManagement::for_memory(&memory, producer)?);
        // System time starts only after the write lock and resolution decision,
        // not when a caller constructed a Memory before waiting for that lock.
        // Creation, retirement and its history boundary are one exact instant.
        let transition_at = chrono::Utc::now();
        memory.created_at = transition_at;
        memory.updated_at = transition_at;
        Self::write_insert_in_tx(&tx, &memory, None)?;
        let mut result =
            FactWriteResult::new(FactWriteOutcome::Inserted, Some(memory.clone()), None);
        if let Some(mut head) = heads.into_iter().next() {
            let old_id = head.id.clone();
            head.superseded_by = Some(memory.id.clone());
            head.layer = MemoryLayer::L0;
            head.updated_at = transition_at;
            // Trusted automatic retirement intentionally keeps the old assertion,
            // management and vector intact. History/FTS writes share this tx.
            Self::write_update_in_tx_at(&tx, &head, Some(transition_at))?;
            result.outcome = FactWriteOutcome::Superseded;
            result.superseded_id = Some(old_id);
        }
        tx.commit()?;
        Ok(result)
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
        // Reject a malformed declaration before edit normalization would clear
        // it. Incoming management claims are ignored in favor of the stored mark.
        if let Some(assertion) = memory.fact_assertion.as_ref() {
            assertion.validate()?;
        }
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();

        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                rusqlite::params![memory.id],
                Self::row_to_memory,
            )
            .optional()?
            .ok_or_else(|| MemVaultError::NotFound(memory.id.clone()))?;
        crate::facts::sanitize_update(&existing, &mut memory);
        Self::write_update_in_tx(&tx, &memory)?;
        Self::suspend_proposed_target_in_tx(&tx, &memory)?;
        tx.commit()?;
        Ok(memory)
    }

    async fn approve_supersede(
        &self,
        target_id: &str,
        proposal: Memory,
    ) -> Result<ApproveSupersedeOutcome> {
        proposal.validate_content()?;
        if let Some(assertion) = proposal.fact_assertion.as_ref() {
            assertion.validate()?;
        }
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
                let previous = target.clone();
                target.superseded_by = Some(proposal.id.clone());
                target.layer = MemoryLayer::L0;
                target.updated_at = chrono::Utc::now();
                crate::facts::sanitize_update(&previous, &mut target);
                Self::write_update_in_tx(&tx, &target)?;
            }
        }

        let persisted_proposal = tx
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                rusqlite::params![proposal.id],
                Self::row_to_memory,
            )
            .optional()?
            .ok_or_else(|| MemVaultError::NotFound(proposal.id.clone()))?;
        proposal.human_reviewed = true;
        proposal.updated_at = chrono::Utc::now();
        // Consume the link defensively: `proposal::approve_memory` takes it
        // before calling, but the storage contract stands on its own — an
        // approved row is a live memory, never a pending proposal.
        proposal.proposes_supersede_of = None;
        crate::facts::sanitize_update(&persisted_proposal, &mut proposal);
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
        // Historical discovery, reconstruction, and ranked selection must see
        // the same SQLite snapshot, including the current-text projection and
        // merge. The deferred transaction is read-only; non-as_of searches keep
        // their existing connection path. Errors roll back before pool return.
        let read_tx = if query.as_of.is_some() {
            Some(conn.unchecked_transaction()?)
        } else {
            None
        };
        let conn = read_tx.as_deref().unwrap_or(&conn);

        let top_k = query.top_k.min(crate::models::MAX_TOP_K);
        if let Some(as_of) = query.as_of {
            let outcome = self.search_at(conn, &query, as_of, top_k)?;
            if let Some(tx) = read_tx {
                tx.commit()?;
            }
            return Ok(outcome);
        }
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
        let mut memory = memory;
        memory.sanitize_public_intake();
        memory.namespace = canonical_project_namespace(&memory.namespace).into_owned();
        let mut conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.transaction()?;
        Self::write_insert_in_tx(&tx, &memory, Some(&embedding))?;
        tx.commit()?;
        Ok(memory)
    }

    async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        namespace: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let mut query = SearchQuery::new(String::new());
        query.top_k = top_k;
        query.namespace = namespace.map(str::to_string);
        Ok(self
            .vector_search_query(query_embedding, &query)
            .await?
            .results)
    }

    async fn vector_search_query(
        &self,
        query_embedding: &[f32],
        query: &SearchQuery,
    ) -> Result<VectorSearchOutcome> {
        let mut outcome = VectorSearchOutcome {
            results: Vec::new(),
            requested_top_k: query.top_k,
            processed_candidates: 0,
            scan_limit: MAX_VECTOR_SCAN_CANDIDATES,
            scan_incomplete: false,
        };
        if query.top_k == 0 {
            return Ok(outcome);
        }
        let top_k = query.top_k.min(MAX_TOP_K);
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        // The vector read and historical reconstruction see one read snapshot.
        let tx = conn.unchecked_transaction()?;
        let mut sql = String::from("SELECT * FROM memories WHERE embedding IS NOT NULL");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if query.as_of.is_none() {
            // Current predicates are safe before the work cap. Historical
            // queries cannot use today's namespace/type/expiry/links here.
            append_filter_clauses(&mut sql, &mut params, "", query);
        }
        sql.push_str(" ORDER BY updated_at DESC, id ASC");
        let idx = params.len() + 1;
        sql.push_str(&format!(" LIMIT ?{idx}"));
        // One cheap lookahead row distinguishes actual exhaustion from exactly
        // 2000 candidates. It is not reconstructed or scored beyond the cap.
        params.push(Box::new((MAX_VECTOR_SCAN_CANDIDATES + 1) as i64));
        let mut stmt = tx.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        // Quantize the query once for all int8 rows.
        let query_quantized = crate::embedding::quantize_int8(query_embedding);

        let mut rows = stmt.query(param_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            if outcome.processed_candidates == MAX_VECTOR_SCAN_CANDIDATES {
                outcome.scan_incomplete = true;
                break;
            }
            outcome.processed_candidates += 1;
            let live = Self::row_to_memory(row)?;
            let (memory, as_of_source) = if let Some(as_of) = query.as_of {
                let Some(at) = Self::reconstruct_at(&tx, &live.id, as_of)? else {
                    continue;
                };
                (at.memory, Some(at.source))
            } else {
                (live, None)
            };
            if !super::memory_matches_filters(
                &memory,
                query,
                query.as_of.unwrap_or_else(chrono::Utc::now),
            ) {
                continue;
            }
            let blob: Vec<u8> = row.get("embedding")?;
            let fmt: i64 = row.get("embedding_fmt")?;
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
            let hit = SearchResult {
                score: sim as f64,
                memory,
                hit_sources: Vec::new(),
                as_of_source,
            };
            let position = outcome
                .results
                .binary_search_by(|existing| {
                    hit.score
                        .partial_cmp(&existing.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| existing.memory.id.cmp(&hit.memory.id))
                })
                .unwrap_or_else(|position| position);
            if position < top_k {
                outcome.results.insert(position, hit);
                outcome.results.truncate(top_k);
            }
        }
        drop(rows);
        drop(stmt);
        tx.commit()?;
        if outcome.scan_incomplete {
            warn!(
                cap = MAX_VECTOR_SCAN_CANDIDATES,
                "vector search scan window is incomplete"
            );
        }
        for (idx, hit) in outcome.results.iter_mut().enumerate() {
            hit.hit_sources = vec![HitSource::Vector { rank: idx + 1 }];
        }
        debug!(
            processed = outcome.processed_candidates,
            returned = outcome.results.len(),
            "query-aware vector search complete"
        );
        Ok(outcome)
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
        // Third component (audit #18): how many memories have crossed their
        // valid-time bound as of *now*. Expiry writes nothing — no row
        // changes, `updated_at` does not move — so a COUNT/MAX(updated_at)
        // hash never changes when a memory expires, and `sync --watch` kept
        // serving a generated rules file containing rules that had retired.
        // Folding in the live expired-count makes the hash time-aware on
        // purpose: it flips at the expiry instant and triggers the
        // regeneration that drops the rule (`sync` already excludes expired
        // memories at generation). Between crossings it is stable, so the
        // watch loop does not rewrite files on every poll. COALESCE keeps
        // the SUM off NULL on an empty table.
        let (count, max_updated, expired_count): (i64, Option<String>, i64) = conn.query_row(
            "SELECT COUNT(*), MAX(updated_at),
                    COALESCE(SUM(CASE WHEN expires_at IS NOT NULL AND expires_at <= ?1 THEN 1 ELSE 0 END), 0)
             FROM memories",
            rusqlite::params![expiry_stamp(chrono::Utc::now())],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
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
            .wrapping_add(updated_hash)
            .wrapping_add((expired_count as u64).wrapping_mul(999_983)))
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

    async fn sync_page(
        &self,
        namespace: Option<&str>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<SyncPage> {
        if limit == 0 {
            return Err(MemVaultError::InvalidInput(
                "sync page limit must be nonzero".into(),
            ));
        }
        let cursor_digest = parse_sync_cursor(cursor)?;
        let limit = limit.min(MAX_TOP_K);
        let namespace = namespace
            .map(canonical_project_namespace)
            .map(|ns| ns.into_owned());
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tx = conn.unchecked_transaction()?;
        // Same membership as list: do not auto-hide retired or pending records.
        // Stream the complete ID set, independent of the page/output limit.
        let mut hash = Sha256::new();
        hash.update(b"memvault-sync-ids-v1\0");
        let mut total_count = 0u64;
        let mut cursor_anchor = None;
        {
            let mut stmt = tx.prepare(
                "SELECT id FROM memories WHERE (?1 IS NULL OR namespace = ?1)
                 ORDER BY id COLLATE BINARY ASC",
            )?;
            let ids =
                stmt.query_map(rusqlite::params![namespace], |row| row.get::<_, String>(0))?;
            for id in ids {
                let id = id?;
                hash.update((id.len() as u64).to_be_bytes());
                hash.update(id.as_bytes());
                total_count += 1;
                if cursor_digest
                    .is_some_and(|digest| digest == hex::encode(Sha256::digest(id.as_bytes())))
                {
                    claim_sync_cursor_anchor(&mut cursor_anchor, id)?;
                }
            }
        }
        if cursor_digest.is_some() && cursor_anchor.is_none() {
            return Err(MemVaultError::InvalidInput(
                "sync page cursor is not in this membership; restart enumeration".into(),
            ));
        }
        let membership_digest = hex::encode(hash.finalize());
        #[cfg(test)]
        {
            let pause = self
                .sync_page_pause
                .lock()
                .map_err(|_| MemVaultError::Storage("sync page test hook poisoned".into()))?
                .take();
            if let Some(pause) = pause {
                pause.membership_ready.wait();
                pause.resume.wait();
            }
        }
        let mut records = Vec::new();
        let mut has_more = false;
        {
            let mut stmt = tx.prepare(
                "SELECT * FROM memories WHERE (?1 IS NULL OR namespace = ?1)
                 AND (?2 IS NULL OR id COLLATE BINARY > ?2)
                 ORDER BY id COLLATE BINARY ASC LIMIT ?3",
            )?;
            let mut rows = stmt.query(rusqlite::params![
                namespace,
                cursor_anchor.as_deref(),
                (limit + 1) as i64
            ])?;
            while let Some(row) = rows.next()? {
                if records.len() == limit {
                    has_more = true;
                    break;
                }
                records.push(Self::row_to_memory(row)?);
            }
        }
        let next_cursor = if has_more {
            records.last().map(|memory| sync_cursor_for_id(&memory.id))
        } else {
            None
        };
        tx.commit()?;
        Ok(SyncPage {
            records,
            next_cursor,
            has_more,
            total_count,
            membership_digest,
        })
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

    fn frozen_journal_pairs() -> (ApprovalPair, ApprovalPair) {
        use crate::io::Exporter;
        let mut target = Memory::new(
            MemoryType::Fact,
            "the approval policy".to_string(),
            Priority::Must,
            test_agent(),
        );
        target.human_reviewed = true;
        let mut proposal = Memory::new(
            MemoryType::Fact,
            "old proposal text".to_string(),
            Priority::Must,
            test_agent(),
        );
        proposal.instruction = Some("old instruction".to_string());
        proposal.proposes_supersede_of = Some(target.id.clone());
        let before = ApprovalPair {
            target_file: Exporter::memory_to_markdown(&target),
            proposal_file: Exporter::memory_to_markdown(&proposal),
            target: target.clone(),
            proposal: proposal.clone(),
        };
        target.superseded_by = Some(proposal.id.clone());
        target.layer = MemoryLayer::L0;
        proposal.content = "updated proposal text".to_string();
        proposal.instruction = Some("require approval".to_string());
        proposal.human_reviewed = true;
        proposal.proposes_supersede_of = None;
        let after = ApprovalPair {
            target_file: Exporter::memory_to_markdown(&target),
            proposal_file: Exporter::memory_to_markdown(&proposal),
            target,
            proposal,
        };
        (before, after)
    }

    #[tokio::test]
    async fn test_journal_pair_preserves_reads_and_invalidates_changed_embeddings() {
        let store = SqliteStore::in_memory().unwrap();
        let (before, after) = frozen_journal_pairs();
        store.save(before.target.clone()).await.unwrap();
        store
            .save_with_embedding(before.proposal.clone(), vec![0.2, 0.4, 0.6])
            .await
            .unwrap();
        store
            .record_access(&[before.target.id.clone(), before.proposal.id.clone()])
            .await
            .unwrap();
        store
            .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
            .unwrap();
        let target = store.get(&after.target.id).await.unwrap();
        let approved = store.get(&after.proposal.id).await.unwrap();
        assert_eq!(target.superseded_by, Some(approved.id.clone()));
        assert_eq!(target.access_count, 1);
        assert_eq!(approved.access_count, 1);
        assert_eq!(approved.instruction.as_deref(), Some("require approval"));
        assert!(approved.human_reviewed);
        assert!(approved.proposes_supersede_of.is_none());
        assert!(store.get_embedding(&approved.id).await.unwrap().is_none());
        let count = || {
            store
                .pool
                .get()
                .unwrap()
                .query_row("SELECT count(*) FROM memory_history", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        let history = count();
        store
            .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
            .unwrap();
        assert_eq!(count(), history, "idempotent replay cannot invent history");
    }

    #[tokio::test]
    async fn test_journal_pair_instruction_only_edit_invalidates_embedding() {
        let store = SqliteStore::in_memory().unwrap();
        let (before, mut after) = frozen_journal_pairs();
        after.proposal.content = before.proposal.content.clone();
        after.proposal_file = crate::io::Exporter::memory_to_markdown(&after.proposal);
        store.save(before.target.clone()).await.unwrap();
        store
            .save_with_embedding(before.proposal.clone(), vec![0.2, 0.4, 0.6])
            .await
            .unwrap();
        store
            .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
            .unwrap();
        assert!(
            store
                .get_embedding(&after.proposal.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.get(&after.proposal.id).await.unwrap().instruction,
            after.proposal.instruction
        );
    }

    #[tokio::test]
    async fn test_journal_pair_second_update_failure_rolls_back_target() {
        let store = SqliteStore::in_memory().unwrap();
        let (before, after) = frozen_journal_pairs();
        store.save(before.target.clone()).await.unwrap();
        store.save(before.proposal.clone()).await.unwrap();
        store
            .pool
            .get()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER reject_journal_proposal BEFORE UPDATE ON memories
             WHEN NEW.id = '{}' BEGIN
             SELECT RAISE(ABORT, 'injected proposal write failure'); END;",
                before.proposal.id,
            ))
            .unwrap();
        assert!(
            store
                .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
                .is_err()
        );
        assert!(
            store
                .get(&before.target.id)
                .await
                .unwrap()
                .superseded_by
                .is_none()
        );
        assert!(!store.get(&before.proposal.id).await.unwrap().human_reviewed);
        let count: i64 = store
            .pool
            .get()
            .unwrap()
            .query_row("SELECT count(*) FROM memory_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_journal_pair_recovers_missing_cache_rows_atomically() {
        for seed in 0..4 {
            let store = SqliteStore::in_memory().unwrap();
            let (before, after) = frozen_journal_pairs();
            if seed & 1 != 0 {
                store.save(before.target.clone()).await.unwrap();
            }
            if seed & 2 != 0 {
                store.save(before.proposal.clone()).await.unwrap();
            }
            store
                .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
                .unwrap();
            assert_eq!(
                store.get(&after.target.id).await.unwrap().layer,
                MemoryLayer::L0
            );
            assert_eq!(
                store.get(&after.proposal.id).await.unwrap().content,
                after.proposal.content
            );
        }
    }

    #[tokio::test]
    async fn test_journal_pair_rejects_later_edits_before_either_update() {
        let store = SqliteStore::in_memory().unwrap();
        let (before, after) = frozen_journal_pairs();
        store.save(before.target.clone()).await.unwrap();
        store.save(before.proposal.clone()).await.unwrap();
        let mut later = before.proposal.clone();
        later.content = "a later human correction".to_string();
        store.update(later.clone()).await.unwrap();
        assert!(
            store
                .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
                .is_err()
        );
        assert!(
            store
                .get(&before.target.id)
                .await
                .unwrap()
                .superseded_by
                .is_none()
        );
        assert_eq!(store.get(&later.id).await.unwrap().content, later.content);
    }

    #[tokio::test]
    async fn test_journal_compensation_does_not_recreate_erased_participant() {
        let store = SqliteStore::in_memory().unwrap();
        let (before, after) = frozen_journal_pairs();
        store
            .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
            .unwrap();
        store.purge(&after.proposal.id).await.unwrap();
        let excluded = std::collections::HashSet::from([after.proposal.id.clone()]);
        store
            .publish_journal_pair(&after, &before, &excluded)
            .unwrap();
        assert!(matches!(
            store.get(&after.proposal.id).await,
            Err(MemVaultError::NotFound(_))
        ));
        assert!(
            store
                .get(&before.target.id)
                .await
                .unwrap()
                .superseded_by
                .is_none()
        );
    }

    fn journal_fact_memory(value: &str, key: &str) -> Memory {
        use crate::facts::{FactAssertion, FactCardinality, FactManagement};
        let mut memory = Memory::new(
            MemoryType::Fact,
            value.to_string(),
            Priority::Reference,
            test_agent(),
        );
        memory.namespace = "project:fact-cache".to_string();
        memory.fact_assertion = Some(FactAssertion {
            key: key.to_string(),
            value: value.to_string(),
            source_namespace: "producer:cache-test".to_string(),
            source_order: 7,
            cardinality: FactCardinality::Single,
        });
        memory.fact_management =
            Some(FactManagement::for_memory(&memory, &memory.source_agent.id).unwrap());
        memory
    }

    fn frozen_fact_journal_pairs() -> (ApprovalPair, ApprovalPair) {
        use crate::io::Exporter;
        let target = journal_fact_memory("the original status", "status");
        let mut proposal = journal_fact_memory("the proposed status", "proposal-status");
        let original_proposal = proposal.clone();
        proposal.proposes_supersede_of = Some(target.id.clone());
        crate::facts::sanitize_update(&original_proposal, &mut proposal);
        let before = ApprovalPair {
            target_file: Exporter::memory_to_markdown(&target),
            proposal_file: Exporter::memory_to_markdown(&proposal),
            target: target.clone(),
            proposal: proposal.clone(),
        };
        let mut target = target;
        target.superseded_by = Some(proposal.id.clone());
        target.layer = MemoryLayer::L0;
        target.namespace = "project:human-edited".to_string();
        target.updated_at += chrono::Duration::seconds(1);
        crate::facts::sanitize_update(&before.target, &mut target);
        proposal.content = "the human corrected status".to_string();
        proposal.namespace = "project:human-edited".to_string();
        proposal.human_reviewed = true;
        proposal.proposes_supersede_of = None;
        proposal.updated_at += chrono::Duration::seconds(1);
        crate::facts::sanitize_update(&before.proposal, &mut proposal);
        let after = ApprovalPair {
            target_file: Exporter::memory_to_markdown(&target),
            proposal_file: Exporter::memory_to_markdown(&proposal),
            target,
            proposal,
        };
        (before, after)
    }

    fn assert_cached_fact_snapshot(store: &SqliteStore, expected: &Memory) {
        let conn = store.pool.get().unwrap();
        let actual = conn
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                rusqlite::params![expected.id],
                SqliteStore::row_to_memory,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "frozen payload, timestamps and management must round-trip exactly"
        );
        let indexed = conn
            .query_row(
                "SELECT fact_group_namespace, fact_source_namespace, fact_key
                 FROM memories WHERE id=?1",
                rusqlite::params![expected.id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        let (namespace, source, key) = crate::facts::group_fields(expected);
        assert_eq!(
            indexed,
            (
                namespace.map(str::to_string),
                source.map(str::to_string),
                key.map(str::to_string),
            )
        );
        let fts = conn
            .query_row(
                "SELECT content, instruction, tags FROM memories_fts WHERE memory_id=?1",
                rusqlite::params![expected.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(fts, SqliteStore::fts_columns(expected));
    }

    #[tokio::test]
    async fn test_journal_fact_metadata_and_group_roundtrip_existing_and_missing_cache() {
        for seed in 0..4 {
            let store = SqliteStore::in_memory().unwrap();
            let (before, after) = frozen_fact_journal_pairs();
            {
                let mut conn = store.pool.get().unwrap();
                let tx = conn.transaction().unwrap();
                for (bit, memory) in [(1, &before.target), (2, &before.proposal)] {
                    if seed & bit != 0 {
                        SqliteStore::write_frozen_insert_in_tx(&tx, memory, None).unwrap();
                    }
                }
                tx.commit().unwrap();
            }
            store
                .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
                .unwrap();
            for (bit, previous, desired) in [
                (1, &before.target, &after.target),
                (2, &before.proposal, &after.proposal),
            ] {
                assert!(desired.fact_assertion.is_none());
                assert_eq!(
                    desired.fact_management.as_ref().unwrap().state(),
                    crate::facts::FactManagementState::Suspended
                );
                assert_cached_fact_snapshot(&store, desired);
                if seed & bit != 0 {
                    let history = store
                        .get_history_snapshot(&desired.id)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        serde_json::to_value(&history).unwrap(),
                        serde_json::to_value(previous).unwrap()
                    );
                }
            }
            // Simulate cache loss independently for each participant, then undo.
            // Inserting a pending BEFORE proposal must not change the restored
            // target's exact original active management/assertion snapshot.
            {
                let mut conn = store.pool.get().unwrap();
                let tx = conn.transaction().unwrap();
                for (bit, memory) in [(1, &after.target), (2, &after.proposal)] {
                    if seed & bit == 0 {
                        SqliteStore::fts_delete(&tx, &memory.id).unwrap();
                        tx.execute(
                            "DELETE FROM memories WHERE id=?1",
                            rusqlite::params![memory.id],
                        )
                        .unwrap();
                    }
                }
                tx.commit().unwrap();
            }
            store
                .publish_journal_pair(&after, &before, &std::collections::HashSet::new())
                .unwrap();
            assert_cached_fact_snapshot(&store, &before.target);
            assert_cached_fact_snapshot(&store, &before.proposal);
            // Both assertion and private record binding survived compensation.
            assert_eq!(
                store
                    .get(&before.target.id)
                    .await
                    .unwrap()
                    .fact_management
                    .unwrap()
                    .state(),
                crate::facts::FactManagementState::Active
            );
        }
    }

    #[tokio::test]
    async fn test_journal_invalid_fact_metadata_fails_closed_before_either_write() {
        for seed in [0, 3] {
            for invalid_binding in [false, true] {
                let store = SqliteStore::in_memory().unwrap();
                let (before, mut after) = frozen_fact_journal_pairs();
                if seed != 0 {
                    let mut conn = store.pool.get().unwrap();
                    let tx = conn.transaction().unwrap();
                    SqliteStore::write_frozen_insert_in_tx(&tx, &before.target, None).unwrap();
                    SqliteStore::write_frozen_insert_in_tx(&tx, &before.proposal, None).unwrap();
                    tx.commit().unwrap();
                }
                if invalid_binding {
                    after.proposal.fact_management = before.target.fact_management.clone();
                } else {
                    after.proposal.fact_assertion = before.target.fact_assertion.clone();
                    after.proposal.fact_assertion.as_mut().unwrap().key.clear();
                }
                assert!(
                    store
                        .publish_journal_pair(&before, &after, &std::collections::HashSet::new())
                        .is_err()
                );
                if seed != 0 {
                    assert_cached_fact_snapshot(&store, &before.target);
                    assert_cached_fact_snapshot(&store, &before.proposal);
                } else {
                    assert!(store.get(&before.target.id).await.is_err());
                    assert!(store.get(&before.proposal.id).await.is_err());
                }
                let conn = store.pool.get().unwrap();
                let history: i64 = conn
                    .query_row("SELECT count(*) FROM memory_history", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(history, 0);
                let fts: i64 = conn
                    .query_row("SELECT count(*) FROM memories_fts", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(fts, if seed == 0 { 0 } else { 2 });
            }
        }
    }

    #[tokio::test]
    async fn test_frozen_cache_insert_does_not_mutate_unrelated_proposal_target() {
        let store = SqliteStore::in_memory().unwrap();
        let unrelated = journal_fact_memory("unrelated current status", "unrelated-status");
        let mut pending = journal_fact_memory("a frozen pending edit", "pending-status");
        pending.proposes_supersede_of = Some(unrelated.id.clone());
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            SqliteStore::write_insert_in_tx(&tx, &unrelated, None).unwrap();
            SqliteStore::write_frozen_insert_in_tx(&tx, &pending, None).unwrap();
            tx.commit().unwrap();
        }
        assert_cached_fact_snapshot(&store, &unrelated);
        assert_cached_fact_snapshot(&store, &pending);
        assert!(
            store
                .get_history_snapshot(&unrelated.id)
                .await
                .unwrap()
                .is_none()
        );
        // The ordinary insert API must still perform upstream suspension.
        let mut ordinary = pending;
        ordinary.id = uuid::Uuid::new_v4().to_string();
        ordinary.fact_management = None;
        store.save(ordinary).await.unwrap();
        let suspended = store.get(&unrelated.id).await.unwrap();
        assert!(suspended.fact_assertion.is_none());
        assert_eq!(
            suspended.fact_management.as_ref().unwrap().state(),
            crate::facts::FactManagementState::Suspended
        );
    }

    #[tokio::test]
    async fn test_trusted_reindex_preserves_normalized_fact_guard_and_group() {
        let store = SqliteStore::in_memory().unwrap();
        let cached = journal_fact_memory("cached managed status", "cached-status");
        let unrelated = journal_fact_memory("unrelated managed status", "unrelated-status");
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            SqliteStore::write_insert_in_tx(&tx, &cached, Some(&[0.2, 0.4, 0.6])).unwrap();
            SqliteStore::write_insert_in_tx(&tx, &unrelated, None).unwrap();
            tx.commit().unwrap();
        }
        let mut normalized = cached.clone();
        normalized.content = "human corrected cached status".to_string();
        normalized.namespace = "project:human-edited".to_string();
        normalized.source_agent.session_id = Some("reindexed-session".to_string());
        normalized.proposes_supersede_of = Some(unrelated.id.clone());
        crate::facts::sanitize_update(&cached, &mut normalized);
        store.resync_trusted_snapshot(&normalized).unwrap();
        assert_cached_fact_snapshot(&store, &normalized);
        assert_cached_fact_snapshot(&store, &unrelated);
        assert!(normalized.fact_assertion.is_none());
        let guard = normalized.fact_management.as_ref().unwrap();
        assert_eq!(guard.state(), crate::facts::FactManagementState::Suspended);
        assert_eq!(guard.namespace(), "project:fact-cache");
        assert!(store.get_embedding(&normalized.id).await.unwrap().is_none());
        let history = store
            .get_history_snapshot(&normalized.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&history).unwrap(),
            serde_json::to_value(&cached).unwrap()
        );
        // Invalid replacement rolls back its deletion, history and FTS changes.
        let mut invalid = normalized.clone();
        invalid.fact_management = unrelated.fact_management.clone();
        assert!(store.resync_trusted_snapshot(&invalid).is_err());
        assert_cached_fact_snapshot(&store, &normalized);
        let missing = journal_fact_memory("never cached", "missing-status");
        assert!(matches!(
            store.resync_trusted_snapshot(&missing),
            Err(MemVaultError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn test_fact_assertion_literal_insert_int8_and_update_share_group_codec() {
        for embedded in [false, true] {
            let store = SqliteStore::in_memory().unwrap();
            let mut literal = journal_fact_memory("a literal status", "literal-status");
            literal.namespace = "project:Fact-Cache/".to_string();
            let stored = if embedded {
                store
                    .save_with_embedding(literal, vec![0.2, 0.4, 0.6])
                    .await
                    .unwrap()
            } else {
                store.save(literal).await.unwrap()
            };
            assert!(stored.fact_assertion.is_some());
            assert!(
                stored.fact_management.is_none(),
                "public intake is not trusted recovery"
            );
            assert_eq!(stored.namespace, "project:fact-cache");
            assert_cached_fact_snapshot(&store, &stored);
            assert_eq!(
                store.get_embedding(&stored.id).await.unwrap().is_some(),
                embedded
            );
            let mut updated = stored;
            updated.access_count = 4;
            updated.source_agent.session_id = Some("human-session".to_string());
            let updated = store.update(updated).await.unwrap();
            assert!(
                updated.fact_assertion.is_none(),
                "source edits clear stale assertions"
            );
            assert_cached_fact_snapshot(&store, &updated);
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

    /// Audit #16: this INSERT drifted from `save`'s — `visibility` and
    /// `superseded_by` were missing from the column list, so a save that
    /// happened to compute an embedding landed on the column DEFAULTs
    /// instead of the caller's values. `visibility` was the damaging half:
    /// a `shared` request became `scoped`, the same business save behaved
    /// differently depending on whether the embedder was up, and
    /// `list_shared` never saw the row. This round-trips EVERY persisted
    /// field so any future column drift fails here instead of silently in
    /// production.
    #[tokio::test]
    async fn test_save_with_embedding_persists_every_field() {
        let store = SqliteStore::in_memory().unwrap();
        // Whole-second instants: some timestamp columns round-trip at second
        // precision, and the point of this test is column coverage, not
        // sub-second fidelity.
        let occurred =
            chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() - 3600, 0).unwrap();
        let expires = occurred + chrono::Duration::days(2);

        let mut mem = Memory::new(
            MemoryType::Preference,
            "team stands up at nine".to_string(),
            Priority::Must,
            test_agent(),
        );
        mem.instruction = Some("attend the standup".to_string());
        mem.namespace = "project:atlas".to_string();
        mem.visibility = Visibility::Shared;
        mem.identity_verified = true;
        mem.corroborating_agents = vec!["agent-a".to_string()];
        mem.occurred_at = Some(occurred);
        mem.expires_at = Some(expires);
        mem.tags = vec!["standup".to_string()];
        mem.confidence = 0.9;
        mem.skill_meta = Some(SkillMeta {
            trigger: Some("mornings".to_string()),
            steps: vec!["read the board".to_string()],
            verification: Some("board is green".to_string()),
            version: 1,
        });
        mem.friction_evidence = Some("missed three standups".to_string());
        mem.source_trace_ids = vec!["trace-1".to_string()];
        let id = mem.id.clone();
        let expected = mem.clone();

        store
            .save_with_embedding(mem, vec![0.1, 0.2, 0.3])
            .await
            .unwrap();
        let got = store.get(&id).await.unwrap();

        assert_eq!(
            got.visibility,
            Visibility::Shared,
            "shared must survive the embedding path — it used to land scoped"
        );
        assert_eq!(got.priority, Priority::Must);
        assert_eq!(got.memory_type, MemoryType::Preference);
        assert_eq!(got.instruction, expected.instruction);
        assert_eq!(got.namespace, "project:atlas");
        assert!(got.identity_verified);
        assert_eq!(got.corroborating_agents, expected.corroborating_agents);
        assert_eq!(got.occurred_at, expected.occurred_at);
        assert_eq!(got.expires_at, expected.expires_at);
        assert_eq!(got.tags, expected.tags);
        assert!((got.confidence - 0.9).abs() < 1e-9);
        assert_eq!(got.skill_meta, expected.skill_meta);
        assert_eq!(got.friction_evidence, expected.friction_evidence);
        assert_eq!(got.source_trace_ids, expected.source_trace_ids);
        assert_eq!(got.created_at, expected.created_at);
        assert!(got.human_reviewed == expected.human_reviewed);
        assert!(got.ai_generated == expected.ai_generated);

        // The user-visible symptom: the team pool must see the row.
        let shared = store.list_shared(10).await.unwrap();
        assert!(
            shared.iter().any(|m| m.id == id),
            "list_shared must see an embedding-path shared save"
        );

        // `superseded_by` parity, on its own row (a superseded memory is
        // excluded from list_shared, so it cannot share the one above).
        let mut retired = Memory::new(
            MemoryType::Fact,
            "the old standup time was ten".to_string(),
            Priority::Reference,
            test_agent(),
        );
        retired.superseded_by = Some("mem_predecessor".to_string());
        let retired_id = retired.id.clone();
        store
            .save_with_embedding(retired, vec![0.4, 0.5])
            .await
            .unwrap();
        assert_eq!(
            store
                .get(&retired_id)
                .await
                .unwrap()
                .superseded_by
                .as_deref(),
            Some("mem_predecessor"),
            "superseded_by must survive the embedding path — dropping it would resurrect a retired memory"
        );
    }

    /// Audit #18: expiry writes nothing — no row changes, `updated_at` does
    /// not move — so a COUNT/MAX(updated_at)-only hash never moves when a
    /// memory expires, and `sync --watch` keeps serving a generated rules
    /// file containing rules that retired. The expired-count component makes
    /// the hash flip at the expiry instant, and stay stable between crossings
    /// so watchers do not rewrite files on every poll.
    #[tokio::test]
    async fn test_sync_state_hash_moves_when_a_memory_expires() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "the staging key rotates weekly".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.expires_at = Some(chrono::Utc::now() + chrono::Duration::milliseconds(300));
        store.save(mem).await.unwrap();

        let before = store.sync_state_hash().await.unwrap();
        let stable = store.sync_state_hash().await.unwrap();
        assert_eq!(
            before, stable,
            "nothing crossed a bound yet: the hash must be stable"
        );

        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let after = store.sync_state_hash().await.unwrap();
        assert_ne!(
            before, after,
            "expiry alone — no write of any kind — must be visible to the change detector"
        );
        let after_again = store.sync_state_hash().await.unwrap();
        assert_eq!(after, after_again, "stable again between crossings");
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

    /// Audit #19, second half: deferring the `seen` claim fixed *which*
    /// snapshot shadows which, but the candidate pool itself was still cut to
    /// `top_k * 3` rows BEFORE any validity check ran. BM25 ranks every
    /// historical version independently, and a rejected version does not claim
    /// `seen` — so each one burns a slot outright. Memories that did not exist
    /// at `as_of` are the worst offenders: every snapshot they have matches the
    /// text query and *none* can ever be accepted, so a handful of them
    /// exhausts a small budget on their own and the memory that should have
    /// answered is never examined.
    ///
    /// This is not a marginal case. A memory needs the historical index only
    /// when its text has changed since `as_of`, and changing repeatedly is what
    /// produces many snapshots — so the old cap was tightest exactly where the
    /// recall problem is worst.
    #[tokio::test]
    async fn test_search_as_of_history_scan_is_not_truncated_before_validation() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let as_of = t0 + chrono::Duration::hours(1);
        let after_as_of = t0 + chrono::Duration::hours(2);

        // The memory that SHOULD answer: in effect at `as_of`, matching the
        // query then, and reworded afterwards so its live text no longer
        // matches. Only the historical index can recall it — which is the whole
        // reason `search_history_at` exists.
        let mut answer = Memory::new(
            MemoryType::Fact,
            format!(
                "the shared codeword is quartz, and here is enough surrounding \
                 prose that a single occurrence of the term does not out-rank a \
                 document that repeats it: {}",
                "padding ".repeat(60)
            ),
            Priority::Reference,
            test_agent(),
        );
        answer.created_at = t0;
        answer.updated_at = t0;
        let answer = store.save(answer).await.unwrap();
        let answer_id = answer.id.clone();
        let mut reworded = store.get(&answer_id).await.unwrap();
        reworded.content = "the codeword has been rotated and no longer appears here".to_string();
        reworded.updated_at = chrono::Utc::now();
        store.update(reworded).await.unwrap();

        // Decoys: created strictly AFTER `as_of`, so no version of them was ever
        // in effect then and every candidate they contribute is rejected. Each
        // repeats the term so BM25 ranks them above the answer's single
        // occurrence — five of them is more than the old `top_k * 3` budget of
        // three could look past.
        let mut decoy_ids = Vec::new();
        for i in 0..5 {
            let mut decoy = Memory::new(
                MemoryType::Fact,
                format!("quartz {i} {}", "quartz ".repeat(20)),
                Priority::Reference,
                test_agent(),
            );
            decoy.created_at = after_as_of;
            decoy.updated_at = after_as_of;
            let saved = store.save(decoy).await.unwrap();
            // An update, because `save` alone writes no history snapshot — the
            // historical index only sees versions that were replaced.
            let mut edited = store.get(&saved.id).await.unwrap();
            edited.content = format!("quartz {i} revised {}", "quartz ".repeat(20));
            store.update(edited).await.unwrap();
            decoy_ids.push(saved.id);
        }

        // Test premises, asserted rather than assumed: a decoy was not in effect
        // at `as_of`, and the answer was.
        assert!(
            store
                .get_as_of(&decoy_ids[0], as_of)
                .await
                .unwrap()
                .is_none(),
            "premise: a decoy created after as_of has no state to reconstruct"
        );
        assert!(
            store.get_as_of(&answer_id, as_of).await.unwrap().is_some(),
            "premise: the answer was in effect at as_of"
        );

        // A generous top_k finds it: the historical index does contain the
        // retired wording. This is the half that already passed before the fix,
        // which is why the truncation went unnoticed.
        let mut wide = SearchQuery::new("quartz".to_string());
        wide.as_of = Some(as_of);
        wide.top_k = 50;
        let hits = store.search(wide).await.unwrap();
        assert!(
            hits.results.iter().any(|r| r.memory.id == answer_id),
            "premise: the answer is recallable historically at a wide top_k"
        );
        assert!(
            hits.results
                .iter()
                .all(|r| !decoy_ids.contains(&r.memory.id)),
            "premise: no decoy was in effect at as_of, so none may be returned"
        );

        // The actual regression: `top_k = 1` gave the scan a budget of three
        // candidates, the decoys took all three, and the answer was never
        // examined. A narrow request must not recall less than a wide one.
        let mut narrow = SearchQuery::new("quartz".to_string());
        narrow.as_of = Some(as_of);
        narrow.top_k = 1;
        let hits = store.search(narrow).await.unwrap();
        assert_eq!(
            hits.results.len(),
            1,
            "top_k=1 must still return the one memory that answers, not give up \
             after three rejected decoy snapshots"
        );
        assert_eq!(
            hits.results[0].memory.id, answer_id,
            "and it must be the memory that was in effect at as_of"
        );
        assert!(
            hits.results[0]
                .hit_sources
                .iter()
                .any(|s| matches!(s, HitSource::HistoryKeyword { .. })),
            "the answer came from the historical index, not the live one"
        );
    }

    #[tokio::test]
    async fn test_search_as_of_live_fts_qualifies_before_bm25_limit_without_history() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        // Distinct filler tokens are important: repeated padding distorts BM25
        // enough to conceal the actual four-live-row counterexample.
        let filler = (1..=99)
            .map(|i| format!("filler{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut answer = history_fixture_memory(format!("quartz {filler}"));
        answer.id = "old-quartz-answer".into();
        store.save(answer.clone()).await.unwrap();
        for i in 0..3 {
            let mut newer = history_fixture_memory("quartz".into());
            newer.id = format!("new-quartz-{i}");
            newer.created_at = t0 + chrono::Duration::hours(2);
            newer.updated_at = newer.created_at;
            store.save(newer).await.unwrap();
        }
        {
            let conn = store.pool.get().unwrap();
            assert_eq!(
                conn.query_row("SELECT count(*) FROM memory_history", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            let mut stmt = conn.prepare("SELECT memory_id FROM memories_fts WHERE memories_fts MATCH 'quartz' ORDER BY bm25(memories_fts), memory_id LIMIT 3").unwrap();
            let first = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(first.len(), 3);
            assert!(
                !first.contains(&answer.id),
                "premise: all raw top3 candidates are newer"
            );
        }
        for top_k in [1, MAX_TOP_K] {
            let mut query = SearchQuery::new("quartz".into());
            query.as_of = Some(as_of);
            query.top_k = top_k;
            let out = store.search(query).await.unwrap();
            assert_eq!(out.keyword_tier, KeywordTier::Strict);
            assert_eq!(out.results.len(), 1);
            assert_eq!(out.results[0].memory.id, answer.id);
            assert_eq!(out.results[0].as_of_source, Some(AsOfSource::Live));
        }
    }

    #[tokio::test]
    async fn test_search_as_of_blank_qualifies_full_live_and_deleted_id_union() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let mut old = history_fixture_memory("fourth old memory".into());
        old.id = "old-fourth".into();
        store.save(old.clone()).await.unwrap();
        for i in 0..3 {
            let mut newer = history_fixture_memory("new memory".into());
            newer.id = format!("new-blank-{i}");
            newer.created_at = t0 + chrono::Duration::hours(2);
            newer.updated_at = newer.created_at;
            store.save(newer).await.unwrap();
        }
        let mut query = SearchQuery::new("   ".into());
        query.as_of = Some(as_of);
        query.top_k = 1;
        let out = store.search(query.clone()).await.unwrap();
        assert_eq!(out.keyword_tier, KeywordTier::None);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].memory.id, old.id);
        let deleted = history_fixture_memory("deleted after requested instant".into());
        let history_id;
        {
            let conn = store.pool.get().unwrap();
            history_id = history_fixture_snapshot(&conn, &deleted, t0 + chrono::Duration::hours(2));
        }
        query.top_k = 10;
        let out = store.search(query).await.unwrap();
        assert_eq!(out.results.len(), 2);
        let hit = out
            .results
            .iter()
            .find(|hit| hit.memory.id == deleted.id)
            .unwrap();
        assert!(
            matches!(hit.as_of_source, Some(AsOfSource::History { history_id: h, .. }) if h == history_id)
        );
    }

    #[tokio::test]
    async fn test_search_as_of_live_projection_filters_old_state_before_candidate_budget() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let mut answer = history_fixture_memory("quartz".into());
        answer.namespace = "project:past".into();
        answer.occurred_at = Some(t0);
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            // Current rows match the query, but their effective versions fail
            // independently. None may spend a live candidate slot.
            for i in 0..8 {
                let mut past = answer.clone();
                past.id = format!("decoy-filter-{i}");
                match i {
                    0 => past.namespace = "project:wrong".into(),
                    1 => past.memory_type = MemoryType::Skill,
                    2 => past.priority = Priority::Must,
                    3 => past.occurred_at = None,
                    4 => past.occurred_at = Some(t0 + chrono::Duration::days(1)),
                    5 => past.created_at = t0 + chrono::Duration::hours(2),
                    6 => past.expires_at = Some(as_of),
                    _ => past.superseded_by = Some(answer.id.clone()),
                }
                history_fixture_snapshot(&tx, &past, t0 + chrono::Duration::hours(2));
                let mut current = answer.clone();
                current.id = past.id;
                SqliteStore::write_frozen_insert_in_tx(&tx, &current, None).unwrap();
            }
            // Today this row fails every filter; only reconstruction can admit it.
            let mut current = answer.clone();
            current.namespace = "project:today".into();
            current.memory_type = MemoryType::Skill;
            current.priority = Priority::Must;
            current.occurred_at = None;
            current.superseded_by = Some("successor".into());
            current.expires_at = Some(as_of);
            SqliteStore::write_frozen_insert_in_tx(&tx, &current, None).unwrap();
            history_fixture_snapshot(&tx, &answer, t0 + chrono::Duration::hours(2));
            tx.commit().unwrap();
        }
        for text in ["quartz", ""] {
            let mut query = SearchQuery::new(text.into());
            query.top_k = 1;
            query.as_of = Some(as_of);
            query.namespace = Some("project:Past".into());
            query.type_filter = Some(MemoryType::Fact);
            query.priority_filter = Some(Priority::Reference);
            query.occurred_from = Some(t0);
            query.occurred_to = Some(t0);
            let out = store.search(query).await.unwrap();
            assert_eq!(out.results.len(), 1);
            assert_eq!(out.results[0].memory.id, answer.id);
            assert_eq!(out.results[0].memory.namespace, answer.namespace);
        }
    }

    #[tokio::test]
    async fn test_search_as_of_selects_tier_after_live_and_history_eligibility() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let answer = history_fixture_memory("quartz".into());
        store.save(answer.clone()).await.unwrap();
        let mut newer = history_fixture_memory("quartz zebra".into());
        newer.created_at = t0 + chrono::Duration::hours(2);
        store.save(newer).await.unwrap();
        let mut query = SearchQuery::new("quartz zebra".into());
        query.top_k = 1;
        query.as_of = Some(as_of);
        let out = store.search(query.clone()).await.unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].memory.id, answer.id);
        assert_eq!(out.keyword_tier, KeywordTier::SynonymFallback);
        // A history-only strict answer must beat a live relaxed answer, and
        // must report Strict rather than the old live-path None/fallback label.
        let historical = history_fixture_memory("quartz zebra".into());
        {
            let conn = store.pool.get().unwrap();
            history_fixture_snapshot(&conn, &historical, t0 + chrono::Duration::hours(2));
        }
        let out = store.search(query).await.unwrap();
        assert_eq!(out.keyword_tier, KeywordTier::Strict);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].memory.id, historical.id);
        assert!(matches!(
            out.results[0].hit_sources.as_slice(),
            [HitSource::HistoryKeyword { rank: 1 }]
        ));
    }

    #[tokio::test]
    async fn test_vector_query_filters_before_top_k_and_current_sql_scan_cap() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let mut answer = history_fixture_memory("valid fact".into());
        answer.id = "zz-valid-fact".into();
        answer.namespace = "project:target".into();
        answer.occurred_at = Some(t0);
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            // More closest Skills than the vector work cap. Safe current SQL
            // filters must keep them from hiding one later, lower-similarity Fact.
            for i in 0..2001 {
                let mut skill = answer.clone();
                skill.id = format!("skill-{i:04}");
                skill.memory_type = MemoryType::Skill;
                SqliteStore::write_frozen_insert_in_tx(&tx, &skill, Some(&[1.0, 0.0])).unwrap();
            }
            SqliteStore::write_frozen_insert_in_tx(&tx, &answer, Some(&[0.7, 0.7])).unwrap();
            tx.commit().unwrap();
        }
        let mut query = SearchQuery::new(String::new());
        query.top_k = 1;
        query.namespace = Some("project:Target".into());
        query.type_filter = Some(MemoryType::Fact);
        query.priority_filter = Some(Priority::Reference);
        query.occurred_from = Some(t0);
        query.occurred_to = Some(t0);
        let out = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert_eq!(out.requested_top_k, 1);
        assert_eq!(out.processed_candidates, 1);
        assert!(!out.scan_incomplete);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].memory.id, answer.id);
        assert_eq!(
            out.results[0].hit_sources,
            vec![HitSource::Vector { rank: 1 }]
        );
        query.occurred_to = Some(t0 - chrono::Duration::seconds(1));
        assert!(
            store
                .vector_search_query(&[1.0, 0.0], &query)
                .await
                .unwrap()
                .results
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_vector_query_historical_eligibility_before_top_k() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let mut answer = history_fixture_memory("old fact wording".into());
        answer.namespace = "project:past".into();
        answer.occurred_at = Some(t0);
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..24 {
                let mut past = answer.clone();
                past.id = format!("historical-vector-decoy-{i:02}");
                match i % 8 {
                    0 => past.namespace = "project:wrong".into(),
                    1 => past.memory_type = MemoryType::Skill,
                    2 => past.priority = Priority::Must,
                    3 => past.occurred_at = None,
                    4 => past.created_at = t0 + chrono::Duration::hours(2),
                    5 => past.expires_at = Some(as_of),
                    6 => past.superseded_by = Some(answer.id.clone()),
                    _ => past.proposes_supersede_of = Some(answer.id.clone()),
                }
                history_fixture_snapshot(&tx, &past, t0 + chrono::Duration::hours(2));
                let mut live = answer.clone();
                live.id = past.id;
                SqliteStore::write_frozen_insert_in_tx(&tx, &live, Some(&[1.0, 0.0])).unwrap();
            }
            let mut today = answer.clone();
            today.namespace = "project:today".into();
            today.memory_type = MemoryType::Skill;
            today.priority = Priority::Must;
            today.occurred_at = None;
            today.expires_at = Some(as_of);
            today.superseded_by = Some("replacement".into());
            today.proposes_supersede_of = Some("proposal-target".into());
            today.content = "current text has a different vector".into();
            SqliteStore::write_frozen_insert_in_tx(&tx, &today, Some(&[0.7, 0.7])).unwrap();
            history_fixture_snapshot(&tx, &answer, t0 + chrono::Duration::hours(2));
            tx.commit().unwrap();
        }
        let mut query = SearchQuery::new("old fact wording".into());
        query.top_k = 1;
        query.as_of = Some(as_of);
        query.namespace = Some("project:Past".into());
        query.type_filter = Some(MemoryType::Fact);
        query.priority_filter = Some(Priority::Reference);
        query.occurred_from = Some(t0);
        query.occurred_to = Some(t0);
        let out = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert_eq!(out.processed_candidates, 25);
        assert!(!out.scan_incomplete);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].memory.content, answer.content);
        assert_eq!(out.results[0].memory.id, answer.id);
        assert!(matches!(
            out.results[0].as_of_source,
            Some(AsOfSource::History { .. })
        ));
        // A tombstone-only history row has no current vector. Keyword history
        // can still recall it; do not promise frozen historical semantic recall.
        let deleted = history_fixture_memory("old deleted lexical codeword".into());
        {
            let conn = store.pool.get().unwrap();
            history_fixture_snapshot(&conn, &deleted, t0 + chrono::Duration::hours(2));
        }
        let vector = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert!(vector.results.iter().all(|hit| hit.memory.id != deleted.id));
        query.query = "deleted lexical codeword".into();
        query.namespace = None;
        query.occurred_from = None;
        query.occurred_to = None;
        assert_eq!(
            store.search(query).await.unwrap().results[0].memory.id,
            deleted.id
        );
    }

    #[tokio::test]
    async fn test_vector_query_scan_diagnostics_exact_exhaustion_zero_and_clamp() {
        let store = SqliteStore::in_memory().unwrap();
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..MAX_VECTOR_SCAN_CANDIDATES {
                let mut row = history_fixture_memory("vector scan fixture".into());
                row.id = format!("vector-{i:04}");
                SqliteStore::write_frozen_insert_in_tx(&tx, &row, Some(&[1.0, 0.0])).unwrap();
            }
            tx.commit().unwrap();
        }
        let mut query = SearchQuery::new(String::new());
        query.top_k = usize::MAX;
        let out = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert_eq!(out.requested_top_k, usize::MAX);
        assert_eq!(out.scan_limit, 2000);
        assert_eq!(out.processed_candidates, 2000);
        assert!(
            !out.scan_incomplete,
            "exactly 2000 rows is complete, not a warning heuristic"
        );
        assert_eq!(out.results.len(), MAX_TOP_K);
        assert_eq!(
            out.results[0].memory.id, "vector-0000",
            "stable score/updated_at ties use ID"
        );
        let mut beyond = history_fixture_memory("one more vector".into());
        beyond.id = "zz-vector-beyond-cap".into();
        store
            .save_with_embedding(beyond, vec![1.0, 0.0])
            .await
            .unwrap();
        query.top_k = 1;
        let out = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert_eq!(out.processed_candidates, 2000);
        assert!(out.scan_incomplete);
        let legacy = store.vector_search(&[1.0, 0.0], 1, None).await.unwrap();
        assert_eq!(legacy[0].memory.id, out.results[0].memory.id);
        query.top_k = 0;
        let zero = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert_eq!(zero.processed_candidates, 0);
        assert!(!zero.scan_incomplete);
        assert!(zero.results.is_empty());
    }

    #[tokio::test]
    async fn test_vector_query_scan_diagnostics_count_dimension_and_historical_rejections() {
        let store = SqliteStore::in_memory().unwrap();
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..=MAX_VECTOR_SCAN_CANDIDATES {
                let mut row = history_fixture_memory("incompatible vector fixture".into());
                row.id = format!("incompatible-{i:04}");
                SqliteStore::write_frozen_insert_in_tx(&tx, &row, Some(&[1.0, 0.0, 0.0])).unwrap();
            }
            tx.commit().unwrap();
        }
        let mut query = SearchQuery::new(String::new());
        query.top_k = 1;
        let current = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert!(current.results.is_empty());
        assert_eq!(current.processed_candidates, 2000);
        assert!(
            current.scan_incomplete,
            "dimension rejects still spend bounded scan work"
        );
        query.as_of = Some(history_fixture_time() - chrono::Duration::seconds(1));
        let historical = store
            .vector_search_query(&[1.0, 0.0], &query)
            .await
            .unwrap();
        assert!(historical.results.is_empty());
        assert_eq!(historical.processed_candidates, 2000);
        assert!(
            historical.scan_incomplete,
            "historical eligibility rejects must not fake scan completeness"
        );
    }

    async fn assert_sync_page_opaque_cursor_roundtrip(anchor_id: &str) {
        let store = SqliteStore::in_memory().unwrap();
        let mut anchor = history_fixture_memory("legacy ID anchor".into());
        anchor.id = anchor_id.to_string();
        anchor.namespace = "project:opaque".into();
        let mut next = history_fixture_memory("record after legacy anchor".into());
        next.id = format!("{anchor_id}\u{10ffff}");
        next.namespace = anchor.namespace.clone();
        // Use public persistence: existing IDs remain legal, not test-only SQL.
        store.save(anchor.clone()).await.unwrap();
        store.save(next.clone()).await.unwrap();
        let first = store
            .sync_page(Some("project:Opaque"), 1, None)
            .await
            .unwrap();
        assert_eq!(first.total_count, 2);
        assert_eq!(first.records.len(), 1);
        assert_eq!(first.records[0].id, anchor.id);
        assert!(first.has_more);
        let token = first.next_cursor.as_deref().unwrap();
        // Before the repair, long/control anchor IDs emitted by page one fail
        // their own continuation request with InvalidInput.
        let second = store
            .sync_page(Some("project:opaque"), 1, Some(token))
            .await
            .unwrap();
        assert_eq!(second.total_count, first.total_count);
        assert_eq!(second.membership_digest, first.membership_digest);
        assert_eq!(second.records.len(), 1);
        assert_eq!(second.records[0].id, next.id);
        assert!(second.records[0].id > first.records[0].id);
        assert!(!second.has_more);
        assert!(second.next_cursor.is_none());
        assert_eq!(token.len(), 67);
        assert!(token.starts_with("v1:"));
        assert!(
            token[3..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(
            token,
            format!("v1:{}", hex::encode(Sha256::digest(anchor.id.as_bytes())))
        );
    }

    #[test]
    fn test_sync_page_opaque_cursor_rejects_ambiguous_anchor() {
        let mut anchor = None;
        claim_sync_cursor_anchor(&mut anchor, "first matching ID".into()).unwrap();
        assert!(matches!(
            claim_sync_cursor_anchor(&mut anchor, "second matching ID".into()),
            Err(MemVaultError::InvalidInput(_))
        ));
        assert_eq!(anchor.as_deref(), Some("first matching ID"));
    }

    #[tokio::test]
    async fn test_sync_page_opaque_cursor_rejects_noncanonical_and_absent_tokens() {
        let store = SqliteStore::in_memory().unwrap();
        let mut anchor = history_fixture_memory("canonical cursor anchor".into());
        anchor.id = "canonical-anchor".into();
        store.save(anchor).await.unwrap();
        for malformed in [
            String::new(),
            "canonical-anchor".into(),
            format!("v2:{}", "a".repeat(64)),
            format!("v1:{}", "A".repeat(64)),
            format!("v1:{}", "g".repeat(64)),
            format!("v1:{}", "a".repeat(63)),
            format!("v1:{}", "a".repeat(65)),
            format!("v1:{}", "a".repeat(1022)),
            format!("v1:{}\n", "a".repeat(64)),
            format!(" v1:{}", "a".repeat(64)),
            sync_cursor_for_id("absent-anchor"),
        ] {
            assert!(matches!(
                store.sync_page(None, 1, Some(&malformed)).await,
                Err(MemVaultError::InvalidInput(_))
            ));
        }
        assert_eq!(store.sync_page(None, 1, None).await.unwrap().total_count, 1);
    }

    #[tokio::test]
    async fn test_sync_page_opaque_cursor_supports_long_legacy_id() {
        assert_sync_page_opaque_cursor_roundtrip(&"a".repeat(1025)).await;
    }

    #[tokio::test]
    async fn test_sync_page_opaque_cursor_supports_control_legacy_id() {
        assert_sync_page_opaque_cursor_roundtrip("\0\u{0001}legacy\nanchor").await;
    }

    #[tokio::test]
    async fn test_sync_page_opaque_cursor_supports_unicode_legacy_id() {
        assert_sync_page_opaque_cursor_roundtrip("雪だるま-é-λ").await;
    }

    #[tokio::test]
    async fn test_sync_page_opaque_cursor_supports_empty_legacy_id() {
        assert_sync_page_opaque_cursor_roundtrip("").await;
    }

    #[tokio::test]
    async fn test_sync_page_full_10001_membership_and_stable_keysets() {
        let store = SqliteStore::in_memory().unwrap();
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..10001 {
                let mut row = history_fixture_memory("sync fixture".into());
                row.id = format!("sync-{i:05}");
                row.namespace = "project:sync".into();
                if i == 10000 {
                    row.expires_at = Some(history_fixture_time());
                    row.superseded_by = Some("replacement".into());
                    row.proposes_supersede_of = Some("target".into());
                }
                SqliteStore::write_frozen_insert_in_tx(&tx, &row, None).unwrap();
            }
            tx.commit().unwrap();
        }
        let mut cursor = None;
        let mut digest = None;
        let mut ids = Vec::new();
        let mut previous_page_anchor = None;
        let mut seen_cursors = std::collections::HashSet::new();
        loop {
            let page = store
                .sync_page(Some("project:Sync"), usize::MAX, cursor.as_deref())
                .await
                .unwrap();
            assert_eq!(page.total_count, 10001);
            assert!(page.records.len() <= MAX_TOP_K);
            assert_eq!(page.membership_digest.len(), 64);
            assert_eq!(page.next_cursor.is_some(), page.has_more);
            if let Some(expected) = &digest {
                assert_eq!(&page.membership_digest, expected);
            } else {
                digest = Some(page.membership_digest.clone());
            }
            for row in &page.records {
                assert!(
                    previous_page_anchor
                        .as_ref()
                        .is_none_or(|last| &row.id > last)
                );
                assert!(ids.last().is_none_or(|last| &row.id > last));
                ids.push(row.id.clone());
            }
            if !page.has_more {
                assert_eq!(page.records.len(), 1);
                assert_eq!(
                    page.records[0].id, "sync-10000",
                    "10001st retired record is still canonical list membership"
                );
                break;
            }
            assert_ne!(page.next_cursor, cursor);
            let token = page.next_cursor.as_ref().unwrap();
            assert_eq!(token.len(), 67);
            assert!(token.is_ascii());
            assert!(
                seen_cursors.insert(token.clone()),
                "continuations must never repeat or cycle"
            );
            previous_page_anchor = page.records.last().map(|memory| memory.id.clone());
            cursor = page.next_cursor;
        }
        assert_eq!(ids.len(), 10001);
        let mut hash = Sha256::new();
        hash.update(b"memvault-sync-ids-v1\0");
        for id in &ids {
            hash.update((id.len() as u64).to_be_bytes());
            hash.update(id.as_bytes());
        }
        assert_eq!(digest.unwrap(), hex::encode(hash.finalize()));
        // Obtain the last-ID token from a real response, then remove a later
        // sentinel so the still-existing anchor legitimately points at EOF.
        let mut sentinel = history_fixture_memory("temporary later sync member".into());
        sentinel.id = "sync-10001".into();
        sentinel.namespace = "project:sync".into();
        store.save(sentinel).await.unwrap();
        let last_anchor = store
            .sync_page(Some("project:sync"), 1, cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(last_anchor.records[0].id, "sync-10000");
        assert!(last_anchor.has_more);
        store.delete("sync-10001").await.unwrap();
        let eof = store
            .sync_page(Some("project:sync"), 1, last_anchor.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(eof.total_count, 10001);
        assert!(eof.records.is_empty());
        assert!(!eof.has_more);
        assert!(eof.next_cursor.is_none());
    }

    #[tokio::test]
    async fn test_sync_page_membership_digest_changes_not_body_and_cursor_validation() {
        let store = SqliteStore::in_memory().unwrap();
        let mut a = history_fixture_memory("original body".into());
        a.id = "a".into();
        let mut b = history_fixture_memory("second body".into());
        b.id = "b".into();
        store.save(a.clone()).await.unwrap();
        store.save(b).await.unwrap();
        let first = store.sync_page(None, 1, None).await.unwrap();
        assert_eq!(first.total_count, 2);
        assert_eq!(first.records[0].id, "a");
        assert_eq!(first.next_cursor.as_ref().unwrap().len(), 67);
        a.content = "edited body".into();
        store.update(a).await.unwrap();
        assert_eq!(
            store
                .sync_page(None, 1, None)
                .await
                .unwrap()
                .membership_digest,
            first.membership_digest
        );
        let mut c = history_fixture_memory("inserted body".into());
        c.id = "c".into();
        store.save(c).await.unwrap();
        let inserted = store
            .sync_page(None, 1, first.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(inserted.total_count, 3);
        assert_ne!(inserted.membership_digest, first.membership_digest);
        assert_eq!(
            inserted.records[0].id, "b",
            "returned first-page token must resolve its actual ID anchor"
        );
        assert!(inserted.has_more);
        let deleted_anchor_token = inserted.next_cursor.clone().unwrap();
        store.delete("b").await.unwrap();
        assert!(
            matches!(
                store.sync_page(None, 1, Some(&deleted_anchor_token)).await,
                Err(MemVaultError::InvalidInput(_))
            ),
            "a formerly returned token must reject its now-deleted anchor"
        );
        let deleted = store
            .sync_page(None, 1, first.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(deleted.total_count, 2);
        assert_ne!(
            deleted.membership_digest, first.membership_digest,
            "same count, different IDs must still restart"
        );
        assert!(matches!(
            store.sync_page(None, 0, None).await,
            Err(MemVaultError::InvalidInput(_))
        ));
        for bad in ["", "\0", "missing", "' OR 1=1 --", "b"] {
            assert!(matches!(
                store.sync_page(None, 1, Some(bad)).await,
                Err(MemVaultError::InvalidInput(_))
            ));
        }
        let scoped = store
            .sync_page(Some("project:empty"), 1, None)
            .await
            .unwrap();
        assert_eq!(scoped.total_count, 0);
        assert!(scoped.records.is_empty());
        assert!(!scoped.has_more);
        assert!(
            store
                .sync_page(Some("project:empty"), 1, first.next_cursor.as_deref())
                .await
                .is_err()
        );
        let json = serde_json::to_value(&deleted).unwrap();
        assert_eq!(json["total_count"], 2);
        assert!(json["has_more"].is_boolean());
        assert!(json["membership_digest"].is_string());
    }

    #[tokio::test]
    async fn test_sync_page_count_digest_and_page_share_one_read_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "memvault-sync-page-snapshot-{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = std::sync::Arc::new(SqliteStore::new(&path).unwrap());
        let writer = SqliteStore::new(&path).unwrap();
        let mut a = history_fixture_memory("atomic sync fixture".into());
        a.id = "atomic-a".into();
        store.save(a).await.unwrap();
        let expected = store.sync_page(None, 10, None).await.unwrap();
        let membership_ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
        *store.sync_page_pause.lock().unwrap() = Some(SyncPagePause {
            membership_ready: membership_ready.clone(),
            resume: resume.clone(),
        });
        let reader = store.clone();
        let paused = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(reader.sync_page(None, 10, None))
        });
        membership_ready.wait();
        let mut b = history_fixture_memory("committed during reader snapshot".into());
        b.id = "atomic-b".into();
        let inserted = writer.save(b).await;
        resume.wait();
        let page = paused.join().unwrap().unwrap();
        inserted.unwrap();
        assert_eq!(page.total_count, expected.total_count);
        assert_eq!(page.membership_digest, expected.membership_digest);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].id, "atomic-a");
        let fresh = store.sync_page(None, 10, None).await.unwrap();
        assert_eq!(fresh.total_count, 2);
        assert_eq!(fresh.records.len(), 2);
        assert_ne!(fresh.membership_digest, page.membership_digest);
        drop(writer);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    fn history_fixture_time() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn history_fixture_memory(content: String) -> Memory {
        let mut memory = Memory::new(MemoryType::Fact, content, Priority::Reference, test_agent());
        memory.created_at = history_fixture_time();
        memory.updated_at = memory.created_at;
        memory
    }

    // Fixed-time snapshots use the production FTS tokenizer. No wall-clock
    // updates or sleeps are needed to describe a version chain in these tests.
    fn history_fixture_snapshot(
        conn: &Connection,
        memory: &Memory,
        replaced_at: chrono::DateTime<chrono::Utc>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO memory_history (memory_id, operation, snapshot, changed_at)
             VALUES (?1, 'update', ?2, ?3)",
            rusqlite::params![
                memory.id,
                serde_json::to_string(memory).unwrap(),
                replaced_at.to_rfc3339(),
            ],
        )
        .unwrap();
        let history_id = conn.last_insert_rowid();
        SqliteStore::history_fts_insert(conn, history_id, memory).unwrap();
        history_id
    }

    async fn assert_history_decoys_do_not_consume_limit(decoy_count: usize) {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let replaced_at = t0 + chrono::Duration::hours(2);
        let mut answer = history_fixture_memory(format!("quartz {}", "padding ".repeat(60)));
        answer.namespace = "project:historytarget".to_string();
        answer.occurred_at = Some(t0);
        let answer_history_id;
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            // Each distinct memory fails exactly one historical predicate;
            // all its other fields match. Every decoy outranks the answer.
            for i in 0..decoy_count {
                let mut decoy = history_fixture_memory("quartz ".repeat(20));
                decoy.namespace = answer.namespace.clone();
                decoy.occurred_at = answer.occurred_at;
                match i % 8 {
                    0 => decoy.namespace = "project:elsewhere".to_string(),
                    1 => decoy.memory_type = MemoryType::Preference,
                    2 => decoy.occurred_at = None,
                    3 => decoy.created_at = replaced_at,
                    4 => decoy.priority = Priority::Must,
                    5 => decoy.superseded_by = Some(answer.id.clone()),
                    6 => decoy.proposes_supersede_of = Some(answer.id.clone()),
                    _ => decoy.expires_at = Some(as_of),
                }
                history_fixture_snapshot(&tx, &decoy, replaced_at);
            }
            answer_history_id = history_fixture_snapshot(&tx, &answer, replaced_at);
            let ranked = {
                let mut stmt = tx
                    .prepare(
                        "SELECT history_id FROM memory_history_fts
                         WHERE memory_history_fts MATCH 'quartz'
                         ORDER BY bm25(memory_history_fts), history_id",
                    )
                    .unwrap();
                stmt.query_map([], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
            };
            assert_eq!(ranked.len(), decoy_count + 1);
            assert_eq!(ranked.last(), Some(&answer_history_id));
            tx.commit().unwrap();
        }

        for top_k in [crate::models::MAX_TOP_K, 1] {
            let mut query = SearchQuery::new("quartz".to_string());
            query.as_of = Some(as_of);
            query.top_k = top_k;
            query.namespace = Some(answer.namespace.clone());
            query.type_filter = Some(MemoryType::Fact);
            query.priority_filter = Some(Priority::Reference);
            query.occurred_from = Some(t0);
            query.occurred_to = Some(t0);
            let hits = store.search(query).await.unwrap();
            assert_eq!(
                hits.results.len(),
                1,
                "{decoy_count} invalid snapshots must not hide the sole answer at top_k={top_k}"
            );
            let hit = &hits.results[0];
            assert_eq!(hit.memory.id, answer.id);
            assert_eq!(hit.memory.content, answer.content);
            assert!(matches!(
                hit.as_of_source,
                Some(AsOfSource::History { history_id, .. }) if history_id == answer_history_id
            ));
            assert_eq!(
                hit.hit_sources,
                vec![HitSource::HistoryKeyword { rank: 1 }],
                "rejected snapshots must be excluded before the ranked FTS selection"
            );
        }
    }

    #[tokio::test]
    async fn test_search_as_of_history_filters_before_the_256_row_floor() {
        assert_history_decoys_do_not_consume_limit(300).await;
    }

    #[tokio::test]
    async fn test_search_as_of_history_filters_before_the_2048_row_ceiling() {
        assert_history_decoys_do_not_consume_limit(2100).await;
    }

    #[tokio::test]
    async fn test_search_as_of_history_many_wrong_versions_do_not_consume_limit() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let answer = history_fixture_memory(format!("quartz {}", "padding ".repeat(60)));
        let answer_history_id;
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            answer_history_id =
                history_fixture_snapshot(&tx, &answer, t0 + chrono::Duration::hours(2));
            let mut wrong = answer.clone();
            wrong.content = "quartz ".repeat(20);
            for i in 0..2100 {
                history_fixture_snapshot(
                    &tx,
                    &wrong,
                    t0 + chrono::Duration::hours(3) + chrono::Duration::seconds(i),
                );
            }
            tx.commit().unwrap();
        }
        for top_k in [1, crate::models::MAX_TOP_K] {
            let mut query = SearchQuery::new("quartz".to_string());
            query.as_of = Some(as_of);
            query.top_k = top_k;
            let hits = store.search(query).await.unwrap();
            assert_eq!(hits.results.len(), 1, "top_k={top_k}");
            assert_eq!(hits.results[0].memory.content, answer.content);
            assert!(matches!(
                hits.results[0].as_of_source,
                Some(AsOfSource::History { history_id, .. }) if history_id == answer_history_id
            ));
            assert_eq!(
                hits.results[0].hit_sources,
                vec![HitSource::HistoryKeyword { rank: 1 }]
            );
        }
    }

    #[tokio::test]
    async fn test_search_as_of_history_json_bound_ids_support_multiple_pages() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let mut ids = std::collections::HashSet::new();
        {
            let mut conn = store.pool.get().unwrap();
            let tx = conn.transaction().unwrap();
            // More than SQLite's traditional 999-variable limit: eligibility
            // must be one JSON-bound parameter, not one placeholder per ID.
            for _ in 0..1100 {
                let memory = history_fixture_memory("quartz".to_string());
                ids.insert(memory.id.clone());
                history_fixture_snapshot(&tx, &memory, t0 + chrono::Duration::hours(2));
            }
            tx.commit().unwrap();
        }
        let mut query = SearchQuery::new("quartz".to_string());
        query.as_of = Some(as_of);
        query.top_k = 131; // More than two 64-row selection pages.
        let hits = store.search(query).await.unwrap();
        assert_eq!(hits.results.len(), 131);
        let mut ranks = Vec::new();
        let mut returned_ids = std::collections::HashSet::new();
        for hit in hits.results {
            assert!(ids.contains(&hit.memory.id));
            assert!(returned_ids.insert(hit.memory.id));
            assert!(matches!(hit.as_of_source, Some(AsOfSource::History { .. })));
            match hit.hit_sources.as_slice() {
                [HitSource::HistoryKeyword { rank }] => ranks.push(*rank),
                other => panic!("expected historical keyword provenance, got {other:?}"),
            }
        }
        ranks.sort_unstable();
        assert_eq!(ranks, (1..=131).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn test_search_as_of_history_no_matching_effective_version_and_zero() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let mut memory = history_fixture_memory("quartz".to_string());
        {
            let conn = store.pool.get().unwrap();
            history_fixture_snapshot(&conn, &memory, t0 + chrono::Duration::minutes(30));
            memory.content = "retired wording".to_string();
            history_fixture_snapshot(&conn, &memory, t0 + chrono::Duration::hours(2));
        }
        for requested_at in [
            as_of,                             // Only a nonmatching version was in effect.
            t0 - chrono::Duration::seconds(1), // Not created yet.
            t0 + chrono::Duration::hours(2),   // No live row after the last snapshot.
        ] {
            let mut query = SearchQuery::new("quartz".to_string());
            query.as_of = Some(requested_at);
            query.top_k = 1;
            assert!(store.search(query).await.unwrap().results.is_empty());
        }
        // An effective matching version still cannot override top_k=0.
        let mut zero = SearchQuery::new("retired wording".to_string());
        zero.as_of = Some(as_of);
        zero.top_k = 0;
        let outcome = store.search(zero).await.unwrap();
        assert!(outcome.results.is_empty());
        assert_eq!(outcome.keyword_tier, KeywordTier::None);
    }

    #[tokio::test]
    async fn test_search_as_of_history_read_transaction_keeps_one_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "memvault-history-read-snapshot-{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = std::sync::Arc::new(SqliteStore::new(&path).unwrap());
        // A separate pool guarantees a writer connection even if the configured
        // pool size is one. Open it before establishing the read snapshot.
        let writer = SqliteStore::new(&path).unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let snapshot = history_fixture_memory("quartz".to_string());
        let mut live = snapshot.clone();
        // The live text must NOT recall the answer: without the production read
        // transaction, the purge below would leave historical recall empty.
        live.content = "retired wording".to_string();
        store.save(live).await.unwrap();
        {
            let conn = store.pool.get().unwrap();
            history_fixture_snapshot(&conn, &snapshot, t0 + chrono::Duration::hours(2));
        }
        let mut query = SearchQuery::new("quartz".to_string());
        query.as_of = Some(as_of);
        query.top_k = 1;
        let projected = std::sync::Arc::new(std::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
        *store.history_search_pause.lock().unwrap() = Some(HistorySearchPause {
            projected: projected.clone(),
            resume: resume.clone(),
        });
        let search_store = store.clone();
        let paused_query = query.clone();
        let search = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(search_store.search(paused_query))
        });
        // The real search has finished current-text recall/projection and has
        // read its SQLite snapshot. Hold it there until the writer commits.
        projected.wait();
        let purged = writer.purge(&snapshot.id).await;
        resume.wait();
        let hits = search.join().unwrap().unwrap();
        purged.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, snapshot.id);
        assert_eq!(hits.results[0].memory.content, snapshot.content);
        assert!(matches!(
            hits.results[0].as_of_source,
            Some(AsOfSource::History { .. })
        ));
        // A fresh production search sees the committed erasure, not that snapshot.
        assert!(store.search(query).await.unwrap().results.is_empty());
        drop(writer);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn test_search_as_of_history_fallback_after_only_ineligible_strict_matches() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = history_fixture_time();
        let as_of = t0 + chrono::Duration::hours(1);
        let answer = history_fixture_memory("quartz".to_string());
        {
            let conn = store.pool.get().unwrap();
            let mut decoy = history_fixture_memory("quartz zebra".to_string());
            decoy.created_at = t0 + chrono::Duration::hours(2);
            history_fixture_snapshot(&conn, &decoy, t0 + chrono::Duration::hours(3));
            history_fixture_snapshot(&conn, &answer, t0 + chrono::Duration::hours(2));
        }
        let mut query = SearchQuery::new("quartz zebra".to_string());
        query.as_of = Some(as_of);
        query.top_k = 1;
        let hits = store.search(query).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, answer.id);
        assert_eq!(
            hits.results[0].hit_sources,
            vec![HitSource::HistoryKeyword { rank: 1 }]
        );
    }

    /// Audit #2: the SQL predicates run against the CURRENT row; an `as_of`
    /// query then reconstructs a PAST row. Passing the former says nothing
    /// about the latter — a memory moved from project:alpha to project:beta
    /// with its text untouched is recalled by a "project:beta + old instant"
    /// query through the live index and projected back to its project:alpha
    /// state, so returning it would leak one project's history into another's
    /// results. Every recall path must re-run the non-text predicates on the
    /// reconstructed state. Each field below is changed *after* `as_of` and
    /// probed in both directions: the new value must return nothing, the old
    /// value must return the projected state.
    #[tokio::test]
    async fn test_search_as_of_refilters_the_projected_state() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let as_of = t0 + chrono::Duration::hours(1);

        // Create the memory at t0, then edit it *now* — strictly after
        // `as_of` — so the reconstruction at `as_of` sees the original state.
        async fn edited_after_as_of(
            store: &SqliteStore,
            codeword: &str,
            t0: chrono::DateTime<chrono::Utc>,
            setup: impl FnOnce(&mut Memory),
            edit: impl FnOnce(&mut Memory),
        ) -> String {
            let mut mem = Memory::new(
                MemoryType::Fact,
                format!("the codeword is {codeword}"),
                Priority::Reference,
                test_agent(),
            );
            mem.created_at = t0;
            mem.updated_at = t0;
            setup(&mut mem);
            let saved = store.save(mem).await.unwrap();
            let mut edited = store.get(&saved.id).await.unwrap();
            edit(&mut edited);
            store.update(edited).await.unwrap();
            saved.id
        }

        let ns_id = edited_after_as_of(
            &store,
            "quartz",
            t0,
            |m| m.namespace = "project:alpha".to_string(),
            |m| m.namespace = "project:beta".to_string(),
        )
        .await;
        let type_id = edited_after_as_of(
            &store,
            "basalt",
            t0,
            |_| {},
            |m| m.memory_type = MemoryType::Preference,
        )
        .await;
        let prio_id = edited_after_as_of(
            &store,
            "granite",
            t0,
            |_| {},
            |m| m.priority = Priority::Must,
        )
        .await;
        let occurred_id =
            edited_after_as_of(&store, "marble", t0, |_| {}, |m| m.occurred_at = Some(t0)).await;

        // Baseline: every memory is recallable at `as_of` unfiltered.
        for codeword in ["quartz", "basalt", "granite", "marble"] {
            let mut q = SearchQuery::new(codeword.to_string());
            q.as_of = Some(as_of);
            let hits = store.search(q).await.unwrap();
            assert_eq!(
                hits.results.len(),
                1,
                "test premise: {codeword} is recallable at as_of"
            );
        }

        // Namespace, both directions.
        let mut q = SearchQuery::new("quartz".to_string());
        q.as_of = Some(as_of);
        q.namespace = Some("project:beta".to_string());
        assert!(
            store.search(q).await.unwrap().results.is_empty(),
            "the beta-scoped query must not receive the alpha-era history"
        );
        let mut q = SearchQuery::new("quartz".to_string());
        q.as_of = Some(as_of);
        q.namespace = Some("project:alpha".to_string());
        let hits = store.search(q).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, ns_id);
        assert_eq!(
            hits.results[0].memory.namespace, "project:alpha",
            "the projected state must carry the namespace it had at as_of"
        );

        // Type, both directions.
        let mut q = SearchQuery::new("basalt".to_string());
        q.as_of = Some(as_of);
        q.type_filter = Some(MemoryType::Preference);
        assert!(
            store.search(q).await.unwrap().results.is_empty(),
            "the memory was still a Fact at as_of"
        );
        let mut q = SearchQuery::new("basalt".to_string());
        q.as_of = Some(as_of);
        q.type_filter = Some(MemoryType::Fact);
        let hits = store.search(q).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, type_id);
        assert_eq!(hits.results[0].memory.memory_type, MemoryType::Fact);

        // Priority, both directions.
        let mut q = SearchQuery::new("granite".to_string());
        q.as_of = Some(as_of);
        q.priority_filter = Some(Priority::Must);
        assert!(
            store.search(q).await.unwrap().results.is_empty(),
            "the escalation to MUST happened after as_of"
        );
        let mut q = SearchQuery::new("granite".to_string());
        q.as_of = Some(as_of);
        q.priority_filter = Some(Priority::Reference);
        let hits = store.search(q).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, prio_id);
        assert_eq!(hits.results[0].memory.priority, Priority::Reference);

        // Event-time window: undated at as_of, dated now. An undated memory
        // never matches a window, so the projection must drop it even though
        // the current row satisfies the SQL predicate.
        let mut q = SearchQuery::new("marble".to_string());
        q.as_of = Some(as_of);
        q.occurred_from = Some(t0 - chrono::Duration::days(1));
        q.occurred_to = Some(t0 + chrono::Duration::days(1));
        assert!(
            store.search(q).await.unwrap().results.is_empty(),
            "the memory had no event date at as_of; it cannot match a window"
        );
        let mut q = SearchQuery::new("marble".to_string());
        q.as_of = Some(as_of);
        let hits = store.search(q).await.unwrap();
        assert_eq!(hits.results.len(), 1);
        assert_eq!(hits.results[0].memory.id, occurred_id);
        assert!(hits.results[0].memory.occurred_at.is_none());
    }

    /// Audit #19: BM25 ranks every historical snapshot independently, so a
    /// match on the WRONG version — words the memory had at *some* point,
    /// just not at `as_of` — can outrank the snapshot actually in effect.
    /// `seen.insert` used to run BEFORE the version check: the wrong snapshot
    /// claimed the memory_id, was discarded, and permanently shadowed the
    /// right one. The claim must happen only after acceptance.
    #[tokio::test]
    async fn test_search_as_of_wrong_snapshot_does_not_shadow_the_right_one() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);

        // v1: dense in the query word, so its snapshot outranks v2 on BM25.
        let mut mem = Memory::new(
            MemoryType::Fact,
            "alpha alpha alpha alpha".to_string(),
            Priority::Reference,
            test_agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let saved = store.save(mem).await.unwrap();
        let id = saved.id.clone();

        // v2: the state in effect at `as_of` — one lonely "alpha".
        let mut v2 = store.get(&id).await.unwrap();
        v2.content = "alpha beta".to_string();
        store.update(v2).await.unwrap();

        // `as_of` lands strictly between the two updates below.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let as_of = chrono::Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;

        // v3: retires the word entirely, so the live index contributes
        // nothing and the recall has to come from the historical one.
        let mut v3 = store.get(&id).await.unwrap();
        v3.content = "zebra crossing".to_string();
        store.update(v3).await.unwrap();

        let mut q = SearchQuery::new("alpha".to_string());
        q.as_of = Some(as_of);
        let hits = store.search(q).await.unwrap();
        let got: Vec<(&str, &str)> = hits
            .results
            .iter()
            .map(|r| (r.memory.id.as_str(), r.memory.content.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![(id.as_str(), "alpha beta")],
            "the in-effect snapshot must survive its higher-ranked wrong sibling"
        );
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
