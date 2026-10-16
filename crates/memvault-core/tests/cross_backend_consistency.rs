//! Phase 5 (see docs/DESIGN.md §18): cascade-consistency regression tests
//! that run the SAME scenario against both `MemoryStore` implementations —
//! `SqliteStore` and `MarkdownStore` — so dedup/decay/supersede/export-import
//! keep FTS, embeddings, and history behaving identically regardless of
//! which backend is active. `SqliteStore` keeps this guarantee by
//! construction (every write is one transaction); `MarkdownStore` delegates
//! almost everything to an internal `SqliteStore` cache, but its five
//! filesystem-touching overrides (`save`/`update`/`delete`/
//! `save_with_embedding`/`supersede`) are exactly the risk `storage::backend`
//! docs call out — a second backend added without replicating the sqlite
//! implementation's discipline. These tests catch that regression.

use std::path::PathBuf;
use std::sync::Arc;

use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::io::{Exporter, Importer};
use memvault_core::models::*;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::markdown::MarkdownStore;
use memvault_core::storage::sqlite::SqliteStore;

fn agent() -> SourceAgent {
    SourceAgent {
        id: "cross-backend-test".to_string(),
        agent_type: "test".to_string(),
        session_id: None,
    }
}

async fn sqlite_store() -> Arc<dyn MemoryStore> {
    Arc::new(SqliteStore::in_memory().unwrap())
}

async fn markdown_store(label: &str) -> (Arc<dyn MemoryStore>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "memvault_cross_backend_{label}_{}",
        uuid::Uuid::new_v4().simple()
    ));
    let store: Arc<dyn MemoryStore> = Arc::new(MarkdownStore::open(&dir).await.unwrap());
    (store, dir)
}

/// save → keyword search finds it → update changes the indexed text →
/// delete removes it from both search and `get`. The same assertions must
/// hold verbatim on both backends; a backend that forgets to update/clear
/// its FTS row on write would pass `save` but fail here.
async fn scenario_save_update_delete_keeps_fts_consistent(store: Arc<dyn MemoryStore>) {
    let mem = Memory::new(
        MemoryType::Fact,
        "the deploy pipeline uses blue-green releases".to_string(),
        Priority::Reference,
        agent(),
    );
    let id = mem.id.clone();
    store.save(mem).await.unwrap();

    let found = store
        .search(SearchQuery::new("blue-green".to_string()))
        .await
        .unwrap();
    assert!(
        found.results.iter().any(|r| r.memory.id == id),
        "freshly saved content must be keyword-searchable"
    );

    let mut updated = store.get(&id).await.unwrap();
    updated.content = "the deploy pipeline uses canary releases now".to_string();
    store.update(updated).await.unwrap();

    let stale = store
        .search(SearchQuery::new("blue-green".to_string()))
        .await
        .unwrap();
    assert!(
        !stale.results.iter().any(|r| r.memory.id == id),
        "FTS must not still match the pre-update text"
    );
    let fresh = store
        .search(SearchQuery::new("canary".to_string()))
        .await
        .unwrap();
    assert!(
        fresh.results.iter().any(|r| r.memory.id == id),
        "FTS must match the post-update text"
    );

    store.delete(&id).await.unwrap();
    assert!(store.get(&id).await.is_err(), "deleted row must be gone");
    let after_delete = store
        .search(SearchQuery::new("canary".to_string()))
        .await
        .unwrap();
    assert!(
        !after_delete.results.iter().any(|r| r.memory.id == id),
        "deleted row must not still be searchable"
    );
}

/// `supersede`: the old memory is archived (never deleted) with
/// `superseded_by` set, excluded from `search`, but still visible via
/// `list`. `MarkdownStore::supersede` is a hand-written override (not a
/// pass-through) specifically to rewrite the markdown file — this is the
/// scenario most likely to drift between backends.
async fn scenario_supersede_archives_without_deleting(store: Arc<dyn MemoryStore>) {
    let old = Memory::new(
        MemoryType::Fact,
        "the API lives at /v1".to_string(),
        Priority::Reference,
        agent(),
    );
    let new = Memory::new(
        MemoryType::Fact,
        "the API lives at /v2".to_string(),
        Priority::Reference,
        agent(),
    );
    let old_id = old.id.clone();
    let new_id = new.id.clone();
    store.save(old).await.unwrap();
    store.save(new).await.unwrap();

    store.supersede(&old_id, &new_id).await.unwrap();

    let old_after = store.get(&old_id).await.unwrap();
    assert_eq!(old_after.superseded_by, Some(new_id.clone()));
    assert_eq!(old_after.layer, MemoryLayer::L0);

    let search_results = store
        .search(SearchQuery::new("/v1".to_string()))
        .await
        .unwrap();
    assert!(
        !search_results.results.iter().any(|r| r.memory.id == old_id),
        "a superseded memory must not surface in search"
    );

    let listed = store.list(None, 100, 0).await.unwrap();
    assert!(
        listed.iter().any(|m| m.id == old_id),
        "a superseded memory must still appear in list (archived, not deleted)"
    );
}

/// Decay crosses the archive threshold for an old, low-score memory:
/// `namespace` gets the `archived:` prefix, the row survives (never
/// deleted), and a MUST-priority memory is exempt regardless of age.
async fn scenario_decay_archives_old_low_score_memory(store: Arc<dyn MemoryStore>) {
    let mut old = Memory::new(
        MemoryType::Fact,
        "a fact nobody has touched in a month".to_string(),
        Priority::Reference,
        agent(),
    );
    old.decay_score = 0.25; // just above the 0.2 default archive_threshold
    old.updated_at = chrono::Utc::now() - chrono::Duration::days(30);
    let old_id = old.id.clone();
    store.save(old).await.unwrap();

    let must = Memory::new(
        MemoryType::Preference,
        "must rule, exempt from decay".to_string(),
        Priority::Must,
        agent(),
    );
    let mut must = must;
    must.decay_score = 0.25;
    must.updated_at = chrono::Utc::now() - chrono::Duration::days(365);
    let must_id = must.id.clone();
    store.save(must).await.unwrap();

    let manager = DecayManager::new(store.clone(), DecayConfig::default());
    let report = manager.run_decay().await.unwrap();
    assert!(report.archived >= 1, "the old low-score fact must archive");

    let archived = store.get(&old_id).await.unwrap();
    assert!(
        archived.namespace.starts_with("archived:"),
        "archived memory must carry the archived: namespace prefix, got {}",
        archived.namespace
    );

    let must_after = store.get(&must_id).await.unwrap();
    assert!(
        !must_after.namespace.starts_with("archived:"),
        "MUST-priority memories are exempt from decay by default"
    );
}

/// `Deduplicator::check_duplicate` must detect the same near-duplicate with
/// the same action regardless of backend — it reads purely through `list`,
/// but this pins the behavior so a future backend-specific `list` quirk
/// (ordering, filtering) doesn't silently change dedup detection.
async fn scenario_dedup_detects_near_duplicate_consistently(store: Arc<dyn MemoryStore>) {
    let original = Memory::new(
        MemoryType::Fact,
        "the staging database runs on port 5432".to_string(),
        Priority::Reference,
        agent(),
    );
    store.save(original).await.unwrap();

    let dedup = Deduplicator::new(store.clone(), None);
    let near_dup = dedup
        .check_duplicate("the staging database runs on port 5432 now", None)
        .await
        .unwrap();
    assert!(
        near_dup.is_some(),
        "near-identical content must be flagged as a duplicate"
    );

    let unrelated = dedup
        .check_duplicate("the frontend uses React and TypeScript", None)
        .await
        .unwrap();
    assert!(
        unrelated.is_none(),
        "unrelated content must not be flagged as a duplicate"
    );
}

/// Export every memory from `source`, import into a fresh `target` of the
/// SAME backend type, and confirm the imported row is keyword-searchable —
/// not just present as a raw row. This is the real cascade check: import
/// must re-establish the index, not merely re-insert data the index then
/// silently disagrees with.
async fn scenario_export_import_rebuilds_search_index(
    source: Arc<dyn MemoryStore>,
    target: Arc<dyn MemoryStore>,
) {
    let mem = Memory::new(
        MemoryType::Fact,
        "the on-call rotation uses PagerDuty escalation policies".to_string(),
        Priority::Reference,
        agent(),
    );
    let id = mem.id.clone();
    source.save(mem).await.unwrap();

    let exporter = Exporter::new(source.clone());
    let json = exporter.export_json(None).await.unwrap();

    let importer = Importer::new(target.clone());
    let report = importer.import_json(&json).await.unwrap();
    assert_eq!(report.imported, 1);

    let found = target
        .search(SearchQuery::new("PagerDuty".to_string()))
        .await
        .unwrap();
    assert!(
        found.results.iter().any(|r| r.memory.id == id),
        "an imported memory must be keyword-searchable in the target store, \
         not just present via `get`"
    );
}

#[tokio::test]
async fn test_save_update_delete_consistent_on_sqlite() {
    scenario_save_update_delete_keeps_fts_consistent(sqlite_store().await).await;
}

#[tokio::test]
async fn test_save_update_delete_consistent_on_markdown() {
    let (store, dir) = markdown_store("save_update_delete").await;
    scenario_save_update_delete_keeps_fts_consistent(store).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_supersede_consistent_on_sqlite() {
    scenario_supersede_archives_without_deleting(sqlite_store().await).await;
}

#[tokio::test]
async fn test_supersede_consistent_on_markdown() {
    let (store, dir) = markdown_store("supersede").await;
    scenario_supersede_archives_without_deleting(store).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_decay_consistent_on_sqlite() {
    scenario_decay_archives_old_low_score_memory(sqlite_store().await).await;
}

#[tokio::test]
async fn test_decay_consistent_on_markdown() {
    let (store, dir) = markdown_store("decay").await;
    scenario_decay_archives_old_low_score_memory(store).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_dedup_detection_consistent_on_sqlite() {
    scenario_dedup_detects_near_duplicate_consistently(sqlite_store().await).await;
}

#[tokio::test]
async fn test_dedup_detection_consistent_on_markdown() {
    let (store, dir) = markdown_store("dedup").await;
    scenario_dedup_detects_near_duplicate_consistently(store).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_export_import_rebuilds_index_sqlite_to_sqlite() {
    scenario_export_import_rebuilds_search_index(sqlite_store().await, sqlite_store().await).await;
}

#[tokio::test]
async fn test_export_import_rebuilds_index_markdown_to_markdown() {
    let (source, source_dir) = markdown_store("export_src").await;
    let (target, target_dir) = markdown_store("export_dst").await;
    scenario_export_import_rebuilds_search_index(source, target).await;
    std::fs::remove_dir_all(source_dir).ok();
    std::fs::remove_dir_all(target_dir).ok();
}

/// Cross-backend direction too: migrating sqlite → markdown (the exact
/// `memvault-cli migrate-to-markdown` CLI command's underlying operation) must
/// also leave the target searchable, not just the same-backend case above.
#[tokio::test]
async fn test_export_import_rebuilds_index_sqlite_to_markdown() {
    let (target, target_dir) = markdown_store("export_from_sqlite").await;
    scenario_export_import_rebuilds_search_index(sqlite_store().await, target).await;
    std::fs::remove_dir_all(target_dir).ok();
}
