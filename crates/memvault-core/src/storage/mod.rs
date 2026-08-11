pub mod sqlite;

use crate::error::Result;
use crate::models::{Memory, SearchQuery, SearchResult};
use async_trait::async_trait;

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save(&self, memory: Memory) -> Result<Memory>;
    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory>;
    async fn get(&self, id: &str) -> Result<Memory>;
    async fn update(&self, memory: Memory) -> Result<Memory>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>>;
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
    /// Return a lightweight hash of the current store state for change detection.
    async fn sync_state_hash(&self) -> Result<u64>;
}
