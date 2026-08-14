use std::path::PathBuf;
use std::sync::Arc;

use memvault_core::compliance::ComplianceStore;
use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::storage::MemoryStore;
use serde::{Deserialize, Serialize};
use tauri::State;

struct AppState {
    store: Arc<SqliteStore>,
    compliance: Option<Arc<ComplianceStore>>,
    /// The DB path this running instance actually opened (changing it via
    /// `set_db_path` only updates the config file for the *next* launch).
    active_db_path: PathBuf,
}

fn home_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
}

fn config_path() -> PathBuf {
    home_dir().join(".memvault/dashboard-config.json")
}

#[derive(Serialize, Deserialize, Default)]
struct DashboardConfig {
    db_path: Option<String>,
}

fn load_dashboard_config() -> DashboardConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Resolve the DB path to open: an explicit override in
/// `~/.memvault/dashboard-config.json` if present, else the default.
fn db_path() -> PathBuf {
    if let Some(p) = load_dashboard_config().db_path {
        return PathBuf::from(p);
    }
    home_dir().join(".memvault/data.db")
}

#[derive(Serialize)]
struct SkillMetaView {
    trigger: Option<String>,
    steps: Vec<String>,
    verification: Option<String>,
    version: u32,
}

impl From<SkillMeta> for SkillMetaView {
    fn from(sm: SkillMeta) -> Self {
        Self {
            trigger: sm.trigger,
            steps: sm.steps,
            verification: sm.verification,
            version: sm.version,
        }
    }
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
    layer: String,
    skill_meta: Option<SkillMetaView>,
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
            layer: format!("{:?}", m.layer),
            skill_meta: m.skill_meta.map(SkillMetaView::from),
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
struct LayersView {
    l0: usize,
    l1: usize,
    l2: usize,
    l3: usize,
}

#[derive(Serialize)]
struct StatsView {
    total: usize,
    must_count: usize,
    reference_count: usize,
    reviewed_count: usize,
    agents: Vec<String>,
    namespaces: Vec<String>,
    layers: LayersView,
    skills: usize,
}

#[derive(Serialize)]
struct PromoteResultView {
    promoted_to_l2: usize,
    promoted_to_l3: usize,
}

#[derive(Serialize)]
struct DecayResultView {
    updated: usize,
    archived: usize,
}

#[derive(Serialize)]
struct DedupResultView {
    unique_count: usize,
    duplicate_count: usize,
}

#[derive(Deserialize)]
struct CreateMemoryRequest {
    content: String,
    instruction: Option<String>,
    #[serde(default = "default_priority_str")]
    priority: String,
    #[serde(default = "default_type_str")]
    memory_type: String,
    namespace: Option<String>,
    tags: Option<Vec<String>>,
}

fn default_priority_str() -> String {
    "REFERENCE".into()
}
fn default_type_str() -> String {
    "fact".into()
}

fn parse_priority(s: &str) -> Priority {
    match s.to_uppercase().as_str() {
        "MUST" => Priority::Must,
        "BACKGROUND" => Priority::Background,
        _ => Priority::Reference,
    }
}

fn parse_memory_type(s: &str) -> MemoryType {
    match s.to_lowercase().as_str() {
        "preference" => MemoryType::Preference,
        "episode" => MemoryType::Episode,
        "entity" => MemoryType::Entity,
        "skill" => MemoryType::Skill,
        _ => MemoryType::Fact,
    }
}

fn parse_layer(s: &str, fallback: MemoryLayer) -> MemoryLayer {
    match s.to_uppercase().as_str() {
        "L0" => MemoryLayer::L0,
        "L1" => MemoryLayer::L1,
        "L2" => MemoryLayer::L2,
        "L3" => MemoryLayer::L3,
        _ => fallback,
    }
}

/// Patch for an existing memory. Every field is optional; omitted fields are left untouched.
#[derive(Deserialize, Default)]
struct UpdateMemoryPatch {
    content: Option<String>,
    instruction: Option<String>,
    priority: Option<String>,
    memory_type: Option<String>,
    tags: Option<Vec<String>>,
    namespace: Option<String>,
    layer: Option<String>,
    skill_trigger: Option<String>,
    skill_steps: Option<Vec<String>>,
    skill_verification: Option<String>,
}

#[tauri::command]
async fn list_memories(
    state: State<'_, AppState>,
    namespace: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<MemoryView>, String> {
    let memories = state
        .store
        .list(
            namespace.as_deref(),
            limit.unwrap_or(100),
            offset.unwrap_or(0),
        )
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
    let results = state
        .store
        .search(SearchQuery {
            query,
            top_k: top_k.unwrap_or(20),
            ..SearchQuery::new(String::new())
        })
        .await
        .map_err(|e| e.to_string())?;

    Ok(results
        .into_iter()
        .map(|r| SearchResultView {
            score: r.score,
            memory: MemoryView::from(r.memory),
        })
        .collect())
}

#[tauri::command]
async fn get_memory(state: State<'_, AppState>, id: String) -> Result<MemoryView, String> {
    let mem = state.store.get(&id).await.map_err(|e| e.to_string())?;
    Ok(MemoryView::from(mem))
}

#[tauri::command]
async fn create_memory(
    state: State<'_, AppState>,
    req: CreateMemoryRequest,
) -> Result<MemoryView, String> {
    let mut mem = Memory::new(
        parse_memory_type(&req.memory_type),
        req.content,
        parse_priority(&req.priority),
        SourceAgent {
            id: "dashboard".to_string(),
            agent_type: "desktop-gui".to_string(),
            session_id: None,
        },
    );
    mem.instruction = req.instruction;
    if let Some(ns) = req.namespace {
        mem.namespace = ns;
    }
    if let Some(tags) = req.tags {
        mem.tags = tags;
    }
    // A memory typed directly by a human via the GUI doesn't need inbox review.
    mem.ai_generated = false;
    mem.human_reviewed = true;

    let saved = state.store.save(mem).await.map_err(|e| e.to_string())?;
    Ok(MemoryView::from(saved))
}

#[tauri::command]
async fn update_memory(
    state: State<'_, AppState>,
    id: String,
    patch: UpdateMemoryPatch,
) -> Result<MemoryView, String> {
    let mut mem = state.store.get(&id).await.map_err(|e| e.to_string())?;

    if let Some(content) = patch.content {
        mem.content = content;
    }
    if let Some(instruction) = patch.instruction {
        mem.instruction = Some(instruction);
    }
    if let Some(priority) = patch.priority {
        mem.priority = parse_priority(&priority);
    }
    if let Some(memory_type) = patch.memory_type {
        mem.memory_type = parse_memory_type(&memory_type);
    }
    if let Some(tags) = patch.tags {
        mem.tags = tags;
    }
    if let Some(namespace) = patch.namespace {
        mem.namespace = namespace;
    }
    if let Some(layer) = patch.layer {
        mem.layer = parse_layer(&layer, mem.layer);
    }
    if patch.skill_trigger.is_some()
        || patch.skill_steps.is_some()
        || patch.skill_verification.is_some()
    {
        let mut meta = mem.skill_meta.take().unwrap_or_default();
        if let Some(trigger) = patch.skill_trigger {
            meta.trigger = Some(trigger);
        }
        if let Some(steps) = patch.skill_steps {
            meta.steps = steps;
        }
        if let Some(verification) = patch.skill_verification {
            meta.verification = Some(verification);
        }
        mem.skill_meta = Some(meta);
    }

    mem.updated_at = chrono::Utc::now();
    let updated = state.store.update(mem).await.map_err(|e| e.to_string())?;
    Ok(MemoryView::from(updated))
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
    let all = state
        .store
        .list(None, 10000, 0)
        .await
        .map_err(|e| e.to_string())?;

    let total = all.len();
    let must_count = all.iter().filter(|m| m.priority == Priority::Must).count();
    let reference_count = all
        .iter()
        .filter(|m| m.priority == Priority::Reference)
        .count();
    let reviewed_count = all.iter().filter(|m| m.human_reviewed).count();
    let skills = all.iter().filter(|m| m.skill_meta.is_some()).count();

    let layers = LayersView {
        l0: all.iter().filter(|m| m.layer == MemoryLayer::L0).count(),
        l1: all.iter().filter(|m| m.layer == MemoryLayer::L1).count(),
        l2: all.iter().filter(|m| m.layer == MemoryLayer::L2).count(),
        l3: all.iter().filter(|m| m.layer == MemoryLayer::L3).count(),
    };

    let mut agents: Vec<String> = all.iter().map(|m| m.source_agent.id.clone()).collect();
    agents.sort();
    agents.dedup();

    let mut namespaces: Vec<String> = all.iter().map(|m| m.namespace.clone()).collect();
    namespaces.sort();
    namespaces.dedup();

    Ok(StatsView {
        total,
        must_count,
        reference_count,
        reviewed_count,
        agents,
        namespaces,
        layers,
        skills,
    })
}

#[tauri::command]
async fn run_promote(state: State<'_, AppState>) -> Result<PromoteResultView, String> {
    let promoter = Promoter::new(state.store.clone(), PromoteConfig::default());
    let result = promoter.run().await.map_err(|e| e.to_string())?;
    Ok(PromoteResultView {
        promoted_to_l2: result.promoted_to_l2,
        promoted_to_l3: result.promoted_to_l3,
    })
}

#[tauri::command]
async fn run_decay(state: State<'_, AppState>) -> Result<DecayResultView, String> {
    let dm = DecayManager::new(state.store.clone(), DecayConfig::default());
    let report = dm.run_decay().await.map_err(|e| e.to_string())?;
    Ok(DecayResultView {
        updated: report.updated,
        archived: report.archived,
    })
}

#[tauri::command]
async fn run_dedup(state: State<'_, AppState>) -> Result<DedupResultView, String> {
    let dedup = Deduplicator::new(state.store.clone(), None);
    let result = dedup.scan(None).await.map_err(|e| e.to_string())?;
    Ok(DedupResultView {
        unique_count: result.unique_count,
        duplicate_count: result.duplicates.len(),
    })
}

/// The DB path this running instance actually opened. Kept separate from
/// `set_db_path`, which only stages a path for the *next* launch — Tauri
/// state is built once in `run()` and the store can't be hot-swapped.
#[tauri::command]
fn get_db_path(state: State<'_, AppState>) -> String {
    state.active_db_path.to_string_lossy().to_string()
}

#[tauri::command]
fn set_db_path(new_path: String) -> Result<(), String> {
    let config = DashboardConfig {
        db_path: Some(new_path),
    };
    let content = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    if let Some(parent) = config_path().parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(config_path(), content).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_compliance_summary(
    state: State<'_, AppState>,
    agent_id: Option<String>,
    limit: Option<usize>,
) -> Result<memvault_core::compliance::AggregateSummary, String> {
    let cs = state
        .compliance
        .clone()
        .ok_or_else(|| "Compliance tracking is not enabled".to_string())?;
    cs.get_summary(agent_id.as_deref(), limit.unwrap_or(10))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_compliance_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<memvault_core::compliance::ComplianceReport, String> {
    let cs = state
        .compliance
        .clone()
        .ok_or_else(|| "Compliance tracking is not enabled".to_string())?;
    cs.get_report(&session_id).await.map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let path = db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let store = Arc::new(SqliteStore::new(&path).expect("Failed to open database"));
    let compliance = ComplianceStore::new(&path.to_string_lossy()).ok();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            store,
            compliance,
            active_db_path: path,
        })
        .invoke_handler(tauri::generate_handler![
            list_memories,
            search_memories,
            get_memory,
            create_memory,
            update_memory,
            approve_memory,
            reject_memory,
            get_stats,
            run_promote,
            run_decay,
            run_dedup,
            get_db_path,
            set_db_path,
            get_compliance_summary,
            get_compliance_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_priority_known_and_unknown() {
        assert_eq!(parse_priority("MUST"), Priority::Must);
        assert_eq!(parse_priority("must"), Priority::Must);
        assert_eq!(parse_priority("BACKGROUND"), Priority::Background);
        assert_eq!(parse_priority("REFERENCE"), Priority::Reference);
        // Unknown values were previously silently coerced to Reference.
        assert_eq!(parse_priority("MUSTT"), Priority::Reference);
        assert_eq!(parse_priority(""), Priority::Reference);
    }

    #[test]
    fn parse_memory_type_known_and_unknown() {
        assert_eq!(parse_memory_type("preference"), MemoryType::Preference);
        assert_eq!(parse_memory_type("PREFERENCE"), MemoryType::Preference);
        assert_eq!(parse_memory_type("episode"), MemoryType::Episode);
        assert_eq!(parse_memory_type("entity"), MemoryType::Entity);
        assert_eq!(parse_memory_type("skill"), MemoryType::Skill);
        assert_eq!(parse_memory_type("fact"), MemoryType::Fact);
        assert_eq!(parse_memory_type("bogus"), MemoryType::Fact);
    }

    #[test]
    fn parse_layer_uses_fallback_for_unknown() {
        for l in [
            MemoryLayer::L0,
            MemoryLayer::L1,
            MemoryLayer::L2,
            MemoryLayer::L3,
        ] {
            assert_eq!(parse_layer(&format!("{l:?}"), MemoryLayer::L1), l);
        }
        assert_eq!(parse_layer("L9", MemoryLayer::L2), MemoryLayer::L2);
        assert_eq!(parse_layer("", MemoryLayer::L1), MemoryLayer::L1);
    }
}
