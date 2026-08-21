use async_trait::async_trait;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::Path;
use tracing::{debug, info, warn};

use super::MemoryStore;
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
        let idx = params.len() + 1;
        sql.push_str(&format!(" AND {prefix}namespace = ?{idx}"));
        params.push(Box::new(ns.clone()));
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
}

pub struct SqliteStore {
    pool: Pool,
}

/// One row of `memory_history`, without the full JSON snapshot — used for
/// `list_checkpoints` listings. Fetch the snapshot itself via
/// `restore_checkpoint`.
#[derive(Debug, Clone)]
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
        Self::ensure_fts_populated(&conn)?;

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
        Ok(())
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
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
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
        })
    }
}

#[async_trait]
impl MemoryStore for SqliteStore {
    async fn save(&self, memory: Memory) -> Result<Memory> {
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

        tx.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at, embedding, layer, skill_meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL, ?19, ?20)",
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

    async fn update(&self, memory: Memory) -> Result<Memory> {
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

        // Snapshot the pre-update row so it can be restored later. If the
        // row doesn't exist, skip the snapshot and let the UPDATE below
        // affect 0 rows and return NotFound, same as before this change.
        if let Ok(old) = tx.query_row(
            "SELECT * FROM memories WHERE id = ?1",
            rusqlite::params![memory.id],
            Self::row_to_memory,
        ) {
            Self::write_history(&tx, &old, "update")?;
        }

        let rows = tx.execute(
            "UPDATE memories SET memory_type=?2, content=?3, instruction=?4, priority=?5,
             namespace=?6, confidence=?7, tags=?8, updated_at=?9,
             human_reviewed=?10, decay_score=?11, access_count=?12, last_read_at=?13, layer=?14, skill_meta=?15
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
            ],
        )?;

        if rows == 0 {
            return Err(MemVaultError::NotFound(memory.id.clone()));
        }

        // Same transaction as the row update: either the new text and its
        // index entry both become visible, or neither does.
        Self::fts_replace(&tx, &memory)?;

        tx.commit()?;
        Ok(memory)
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

    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Clamp top_k so LIMIT is neither 0 (empty result) nor unbounded.
        let top_k = query.top_k.clamp(1, 1000);
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
        for (idx, r) in results.iter_mut().enumerate() {
            r.hit_sources = vec![crate::models::HitSource::Keyword { rank: idx + 1 }];
        }
        Ok(SearchOutcome {
            results,
            keyword_tier: used_tier,
        })
    }

    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory> {
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

        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at, embedding, embedding_fmt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, 1)",
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
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Cap the number of candidate rows pulled into memory for cosine-similarity
        // scoring. Without this, an unfiltered vector_search on a large table does
        // a full unbounded scan + per-row deserialization before any ranking happens.
        const MAX_VECTOR_SCAN_CANDIDATES: i64 = 2000;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(ns) = namespace {
                (
                    "SELECT * FROM memories WHERE embedding IS NOT NULL AND namespace = ?1 \
                 ORDER BY updated_at DESC LIMIT ?2"
                        .to_string(),
                    vec![
                        Box::new(ns.to_string()),
                        Box::new(MAX_VECTOR_SCAN_CANDIDATES),
                    ],
                )
            } else {
                (
                    "SELECT * FROM memories WHERE embedding IS NOT NULL \
                 ORDER BY updated_at DESC LIMIT ?1"
                        .to_string(),
                    vec![Box::new(MAX_VECTOR_SCAN_CANDIDATES)],
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

    async fn record_access(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
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
        conn.execute(&sql, param_refs.as_slice())?;

        debug!(count = ids.len(), "record_access batch complete");
        Ok(())
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
            namespace
        {
            (
                "SELECT * FROM memories WHERE namespace = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(ns.to_string()), Box::new(limit as i64), Box::new(offset as i64)],
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
            namespace
        {
            (
                "SELECT * FROM memories WHERE human_reviewed = 0 AND namespace = ?1 ORDER BY created_at ASC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(ns.to_string()), Box::new(limit as i64), Box::new(offset as i64)],
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
            version, 5,
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

    /// Regression: `LIMIT top_k*3` was unbounded on the top (`top_k=0` → no
    /// results, enormous values → huge scans). Clamp to a sane range.
    #[tokio::test]
    async fn test_search_clamps_top_k() {
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

        // top_k = 0 must not silently return zero rows (defensive clamp to >= 1).
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
            !results.is_empty(),
            "top_k=0 should clamp to at least one row, got {}",
            results.len()
        );

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
}
