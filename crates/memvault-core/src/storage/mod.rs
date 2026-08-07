pub mod sqlite;

use async_trait::async_trait;
use crate::error::Result;
use crate::models::{Memory, SearchQuery, SearchResult};

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save(&self, memory: Memory) -> Result<Memory>;
    async fn get(&self, id: &str) -> Result<Memory>;
    async fn update(&self, memory: Memory) -> Result<Memory>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>>;
    async fn list(&self, namespace: Option<&str>, limit: usize, offset: usize) -> Result<Vec<Memory>>;
}
