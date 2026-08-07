use std::path::PathBuf;
use std::sync::Arc;

use memvault_core::models::*;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::storage::MemoryStore;
use serde::{Deserialize, Serialize};
use tauri::State;

struct AppState {
    store: Arc<SqliteStore>,
}

fn db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".memvault/data.db")
}

#[derive(Serialize)]
struct MemoryView {
    id: String,
    memory_type: String,
    content: String,
    instruction: Option<String>,
    priority: String,
    namespace: String,
    tags: Vec<String>,
    source_agent_id: String,
    confidence: f64,
    human_reviewed: bool,
    decay_score: f64,
    access_count: u32,
    created_at: String,
    updated_at: String,
}

impl From<Memory> for MemoryView {
    fn from(m: Memory) -> Self {
        Self {
            id: m.id,
            memory_type: format!("{:?}", m.memory_type),
            content: m.content,
            instruction: m.instruction,
            priority: format!("{:?}", m.priority),
            namespace: m.namespace,
            tags: m.tags,
            source_agent_id: m.source_agent.id,
            confidence: m.confidence,
            human_reviewed: m.human_reviewed,
            decay_score: m.decay_score,
            access_count: m.access_count,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize)]
struct SearchResultView {
    memory: MemoryView,
    score: f64,
}

#[derive(Serialize)]
struct StatsView {
    total: usize,
    must_count: usize,
    reference_count: usize,
    reviewed_count: usize,
    agents: Vec<String>,
}

#[tauri::command]
async fn list_memories(
    state: State<'_, AppState>,
    namespace: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<MemoryView>, String> {
    let memories = state.store
        .list(namespace.as_deref(), limit.unwrap_or(100), 0)
        .await
        .map_err(|e| e.to_string())?;

    Ok(memories.into_iter().map(MemoryView::from).collect())
}

#[tauri::command]
async fn search_memories(
    state: State<'_, AppState>,
    query: String,
    top_k: Option<usize>,
) -> Result<Vec<SearchResultView>, String> {
    let results = state.store
        .search(SearchQuery {
            query,
            top_k: top_k.unwrap_or(20),
            ..SearchQuery::new(String::new())
        })
        .await
        .map_err(|e| e.to_string())?;

    Ok(results.into_iter().map(|r| SearchResultView {
        score: r.score,
        memory: MemoryView::from(r.memory),
    }).collect())
}

#[tauri::command]
async fn get_memory(state: State<'_, AppState>, id: String) -> Result<MemoryView, String> {
    let mem = state.store.get(&id).await.map_err(|e| e.to_string())?;
    Ok(MemoryView::from(mem))
}

#[tauri::command]
async fn approve_memory(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let mut mem = state.store.get(&id).await.map_err(|e| e.to_string())?;
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    state.store.update(mem).await.map_err(|e| e.to_string())?;
    Ok(format!("Approved: {}", id))
}

#[tauri::command]
async fn reject_memory(state: State<'_, AppState>, id: String) -> Result<String, String> {
    state.store.delete(&id).await.map_err(|e| e.to_string())?;
    Ok(format!("Rejected: {}", id))
}

#[tauri::command]
async fn get_stats(state: State<'_, AppState>) -> Result<StatsView, String> {
    let all = state.store.list(None, 10000, 0).await.map_err(|e| e.to_string())?;

    let total = all.len();
    let must_count = all.iter().filter(|m| m.priority == Priority::Must).count();
    let reference_count = all.iter().filter(|m| m.priority == Priority::Reference).count();
    let reviewed_count = all.iter().filter(|m| m.human_reviewed).count();

    let mut agents: Vec<String> = all.iter().map(|m| m.source_agent.id.clone()).collect();
    agents.sort();
    agents.dedup();

    Ok(StatsView {
        total,
        must_count,
        reference_count,
        reviewed_count,
        agents,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let path = db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let store = Arc::new(SqliteStore::new(&path).expect("Failed to open database"));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState { store })
        .invoke_handler(tauri::generate_handler![
            list_memories,
            search_memories,
            get_memory,
            approve_memory,
            reject_memory,
            get_stats,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
