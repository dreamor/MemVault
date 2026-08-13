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

pub struct SqliteStore {
    pool: Pool,
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
        ",
        )?;

        Self::run_migrations(&conn)?;

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
            return Ok(());
        }

        for (version, sql) in Self::MIGRATIONS {
            if *version > current_version {
                conn.execute(sql, [])?;
                conn.execute(&format!("PRAGMA user_version = {}", version), [])?;
                info!(version, "applied schema migration");
            }
        }

        Ok(())
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

    fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
        embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
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
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        conn.execute(
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
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        let rows = conn.execute(
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

        Ok(memory)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let rows = conn.execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(id.to_string()));
        }

        Ok(())
    }

    async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let mut sql = String::from("SELECT * FROM memories WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref ns) = query.namespace {
            sql.push_str(&format!(" AND namespace = ?{}", param_idx));
            params.push(Box::new(ns.clone()));
            param_idx += 1;
        }

        if let Some(ref mt) = query.type_filter {
            let type_str = serde_json::to_string(mt).unwrap_or_default();
            let type_str = type_str.trim_matches('"').to_string();
            sql.push_str(&format!(" AND memory_type = ?{}", param_idx));
            params.push(Box::new(type_str));
            param_idx += 1;
        }

        if let Some(ref pf) = query.priority_filter {
            let p_str = serde_json::to_string(pf).unwrap_or_default();
            let p_str = p_str.trim_matches('"').to_string();
            sql.push_str(&format!(" AND priority = ?{}", param_idx));
            params.push(Box::new(p_str));
            param_idx += 1;
        }

        // Word-level multi-field search with query expansion
        let search_words = if !query.query.is_empty() {
            crate::query_expand::expand_query(&query.query)
        } else {
            Vec::new()
        };

        if !search_words.is_empty() {
            let mut word_clauses = Vec::new();
            for word in &search_words {
                let clause = format!(
                    "(content LIKE ?{p} OR instruction LIKE ?{p} OR tags LIKE ?{p})",
                    p = param_idx
                );
                word_clauses.push(clause);
                params.push(Box::new(format!("%{}%", word)));
                param_idx += 1;
            }
            sql.push_str(&format!(" AND ({})", word_clauses.join(" OR ")));
        }

        let _ = param_idx;

        sql.push_str(" ORDER BY CASE priority WHEN 'MUST' THEN 0 WHEN 'REFERENCE' THEN 1 ELSE 2 END, decay_score DESC, updated_at DESC");
        sql.push_str(&format!(" LIMIT {}", query.top_k * 3)); // fetch more for re-scoring

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_memory)?;

        let now = chrono::Utc::now();
        let mut results = Vec::new();

        for row in rows {
            let memory = row?;
            let score = Self::compute_relevance_score(&memory, &search_words, now);
            results.push(SearchResult { memory, score });
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

        results.truncate(query.top_k);
        Ok(results)
    }

    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');
        let blob = Self::embedding_to_blob(&embedding);

        conn.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, last_read_at, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
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

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let memory = Self::row_to_memory(row)?;
            let blob: Vec<u8> = row.get("embedding")?;
            Ok((memory, blob))
        })?;

        for row in rows {
            let (memory, blob) = row?;
            let emb = Self::blob_to_embedding(&blob);
            let sim = cosine_similarity(query_embedding, &emb);
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
            .map(|(memory, sim)| SearchResult {
                score: sim as f64,
                memory,
            })
            .collect())
    }

    async fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT embedding FROM memories WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(id.to_string()),
                other => MemVaultError::Sqlite(other),
            })?;

        Ok(result.map(|blob| Self::blob_to_embedding(&blob)))
    }

    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        let conn = self
            .pool
            .get()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let blob = Self::embedding_to_blob(&embedding);

        let rows = conn.execute(
            "UPDATE memories SET embedding = ?2 WHERE id = ?1",
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
            .unwrap();
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
            .unwrap();
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
        assert!((retrieved_emb[0] - 0.1).abs() < 1e-6);
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
            version, 4,
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
            .unwrap();
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
            .unwrap();
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
            .unwrap();
        assert!(wrong_ns.is_empty());

        let right_ns = store
            .search(SearchQuery {
                query: String::new(),
                namespace: Some("project:beta".to_string()),
                top_k: 10,
                ..SearchQuery::new(String::new())
            })
            .await
            .unwrap();
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
            .unwrap();
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
            .unwrap();
        assert_eq!(results.len(), 2);
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
