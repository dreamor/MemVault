use async_trait::async_trait;
use rusqlite::Connection;
use std::sync::Mutex;
use std::path::Path;
use tracing::debug;

use crate::embedding::cosine_similarity;
use crate::error::{MemVaultError, Result};
use crate::models::*;
use super::MemoryStore;

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.execute_batch("
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
        ")?;

        // Migration: add embedding column if missing (for existing databases)
        let has_embedding: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name='embedding'")
            .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
            .map(|c| c > 0)
            .unwrap_or(false);

        if !has_embedding {
            let _ = conn.execute("ALTER TABLE memories ADD COLUMN embedding BLOB", []);
        }

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

    fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
        let tags_str: String = row.get("tags")?;
        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();

        let memory_type_str: String = row.get("memory_type")?;
        let memory_type: MemoryType = serde_json::from_str(&format!("\"{}\"", memory_type_str))
            .unwrap_or(MemoryType::Fact);

        let priority_str: String = row.get("priority")?;
        let priority: Priority = serde_json::from_str(&format!("\"{}\"", priority_str))
            .unwrap_or(Priority::Reference);

        let created_str: String = row.get("created_at")?;
        let updated_str: String = row.get("updated_at")?;

        Ok(Memory {
            id: row.get("id")?,
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
            created_at: created_str.parse().unwrap_or_default(),
            updated_at: updated_str.parse().unwrap_or_default(),
            ai_generated: row.get::<_, bool>("ai_generated")?,
            human_reviewed: row.get::<_, bool>("human_reviewed")?,
            decay_score: row.get("decay_score")?,
            access_count: row.get("access_count")?,
        })
    }
}

#[async_trait]
impl MemoryStore for SqliteStore {
    async fn save(&self, memory: Memory) -> Result<Memory> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        conn.execute(
            "INSERT INTO memories (id, memory_type, content, instruction, priority,
             source_agent_id, source_agent_type, source_session_id,
             namespace, confidence, tags, created_at, updated_at,
             ai_generated, human_reviewed, decay_score, access_count, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, NULL)",
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
            ],
        )?;

        Ok(memory)
    }

    async fn get(&self, id: &str) -> Result<Memory> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.query_row(
            "SELECT * FROM memories WHERE id = ?1",
            rusqlite::params![id],
            Self::row_to_memory,
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(id.to_string()),
            other => MemVaultError::Sqlite(other),
        })
    }

    async fn update(&self, memory: Memory) -> Result<Memory> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let tags_json = serde_json::to_string(&memory.tags)?;
        let type_str = serde_json::to_string(&memory.memory_type)?;
        let type_str = type_str.trim_matches('"');
        let priority_str = serde_json::to_string(&memory.priority)?;
        let priority_str = priority_str.trim_matches('"');

        let rows = conn.execute(
            "UPDATE memories SET memory_type=?2, content=?3, instruction=?4, priority=?5,
             namespace=?6, confidence=?7, tags=?8, updated_at=?9,
             human_reviewed=?10, decay_score=?11, access_count=?12
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
            ],
        )?;

        if rows == 0 {
            return Err(MemVaultError::NotFound(memory.id.clone()));
        }

        Ok(memory)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let rows = conn.execute("DELETE FROM memories WHERE id = ?1", rusqlite::params![id])?;
        if rows == 0 {
            return Err(MemVaultError::NotFound(id.to_string()));
        }

        Ok(())
    }

    async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

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

        // keyword search in content
        if !query.query.is_empty() {
            sql.push_str(&format!(" AND content LIKE ?{}", param_idx));
            params.push(Box::new(format!("%{}%", query.query)));
            param_idx += 1;
        }

        let _ = param_idx;

        sql.push_str(" ORDER BY CASE priority WHEN 'MUST' THEN 0 WHEN 'REFERENCE' THEN 1 ELSE 2 END, decay_score DESC, updated_at DESC");
        sql.push_str(&format!(" LIMIT {}", query.top_k));

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), Self::row_to_memory)?;

        let mut results = Vec::new();
        for row in rows {
            let memory = row?;
            let score = if memory.priority == Priority::Must { 1.0 } else { memory.decay_score * memory.confidence };
            results.push(SearchResult { memory, score });
        }

        Ok(results)
    }

    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;
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
             ai_generated, human_reviewed, decay_score, access_count, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                blob,
            ],
        )?;

        Ok(memory)
    }

    async fn vector_search(&self, query_embedding: &[f32], top_k: usize, namespace: Option<&str>) -> Result<Vec<SearchResult>> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ns) = namespace {
            (
                "SELECT * FROM memories WHERE embedding IS NOT NULL AND namespace = ?1".to_string(),
                vec![Box::new(ns.to_string())],
            )
        } else {
            (
                "SELECT * FROM memories WHERE embedding IS NOT NULL".to_string(),
                vec![],
            )
        };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

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
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let result: Option<Vec<u8>> = conn.query_row(
            "SELECT embedding FROM memories WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MemVaultError::NotFound(id.to_string()),
            other => MemVaultError::Sqlite(other),
        })?;

        Ok(result.map(|blob| Self::blob_to_embedding(&blob)))
    }

    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;
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

    async fn list(&self, namespace: Option<&str>, limit: usize, offset: usize) -> Result<Vec<Memory>> {
        let conn = self.conn.lock().map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ns) = namespace {
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
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
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

        let m1 = Memory::new(MemoryType::Preference, "user prefers Python".to_string(), Priority::Must, test_agent());
        let m2 = Memory::new(MemoryType::Fact, "project uses PostgreSQL".to_string(), Priority::Reference, test_agent());
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let results = store.search(SearchQuery::new("Python".to_string())).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].memory.content.contains("Python"));
    }

    #[tokio::test]
    async fn test_delete() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(MemoryType::Fact, "temp".to_string(), Priority::Background, test_agent());
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        store.delete(&id).await.unwrap();
        assert!(store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let store = SqliteStore::in_memory().unwrap();

        let m1 = Memory::new(MemoryType::Fact, "low priority fact".to_string(), Priority::Reference, test_agent());
        let m2 = Memory::new(MemoryType::Preference, "must rule".to_string(), Priority::Must, test_agent());
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let results = store.search(SearchQuery { query: String::new(), top_k: 10, ..SearchQuery::new(String::new()) }).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_save_with_embedding() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = Memory::new(MemoryType::Fact, "test embedding".to_string(), Priority::Reference, test_agent());
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

        let m1 = Memory::new(MemoryType::Fact, "python coding".to_string(), Priority::Reference, test_agent());
        let m2 = Memory::new(MemoryType::Fact, "rust systems".to_string(), Priority::Reference, test_agent());
        let m3 = Memory::new(MemoryType::Fact, "no embedding".to_string(), Priority::Reference, test_agent());

        // similar embeddings for python and query, different for rust
        store.save_with_embedding(m1, vec![0.9, 0.1, 0.0]).await.unwrap();
        store.save_with_embedding(m2, vec![0.1, 0.9, 0.0]).await.unwrap();
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
        let mem = Memory::new(MemoryType::Fact, "delayed embedding".to_string(), Priority::Reference, test_agent());
        let id = mem.id.clone();

        store.save(mem).await.unwrap();
        assert!(store.get_embedding(&id).await.unwrap().is_none());

        store.set_embedding(&id, vec![1.0, 2.0, 3.0]).await.unwrap();
        let emb = store.get_embedding(&id).await.unwrap().unwrap();
        assert_eq!(emb.len(), 3);
    }
}
