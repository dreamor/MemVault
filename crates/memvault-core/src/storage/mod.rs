pub mod schema_checksum;
pub mod sqlite;

use crate::error::Result;
use crate::models::{
    EpisodeFilter, EpisodeRecord, Memory, SearchOutcome, SearchQuery, SearchResult,
};
use async_trait::async_trait;

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save(&self, memory: Memory) -> Result<Memory>;
    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory>;
    async fn get(&self, id: &str) -> Result<Memory>;
    async fn update(&self, memory: Memory) -> Result<Memory>;
    async fn delete(&self, id: &str) -> Result<()>;
    /// Keyword search. The outcome reports which match tier was used —
    /// callers must surface relaxations rather than present them as exact
    /// matches.
    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome>;
    async fn vector_search(
        &self,
        embedding: &[f32],
        top_k: usize,
        namespace: Option<&str>,
    ) -> Result<Vec<SearchResult>>;
    async fn list(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>>;
    async fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>>;
    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()>;
    async fn record_access(&self, ids: &[String]) -> Result<()>;
    /// List memories that have no embedding (NULL), for auto-backfill.
    async fn list_without_embedding(&self, limit: usize) -> Result<Vec<Memory>>;
    /// List memories pending human review (human_reviewed = false).
    async fn list_pending(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>>;
    /// Return a lightweight hash of the current store state for change detection.
    async fn sync_state_hash(&self) -> Result<u64>;

    // --- Episodic memory (episodes table) ---

    /// Attach a structured outcome record to an existing episode memory.
    /// The memory row itself must already be saved; the record links 1:1.
    async fn record_episode(&self, episode: EpisodeRecord) -> Result<()>;
    /// Fetch the outcome record attached to an episode memory.
    async fn get_episode(&self, memory_id: &str) -> Result<EpisodeRecord>;
    /// List episode records, newest first, optionally filtered.
    async fn list_episodes(&self, filter: EpisodeFilter) -> Result<Vec<EpisodeRecord>>;
    /// Write back the reflection result: the distilled lesson and the id of
    /// the instruction-form memory created from it (if one was created).
    async fn update_episode_lesson(
        &self,
        memory_id: &str,
        lesson: &str,
        lesson_memory_id: Option<&str>,
    ) -> Result<()>;
}
