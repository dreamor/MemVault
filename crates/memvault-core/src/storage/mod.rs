pub mod backend;
pub mod markdown;
pub mod schema_checksum;
pub mod sqlite;

use crate::error::Result;
use crate::models::{
    EpisodeFilter, EpisodeRecord, ForgottenLogEntry, Memory, MemoryAtTime, MemoryRelation,
    SearchOutcome, SearchQuery, SearchResult, SkillStats, TraceWatermark,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Project already-retrieved results onto a point-in-time (valid-time) view.
///
/// Needed by the composition paths — MCP `search_memory` and REST `/api/search`
/// call [`MemoryStore::vector_search`] directly and merge it with the keyword
/// path themselves, so they bypass the projection that
/// [`MemoryStore::search`] applies internally. Without this, a semantic or
/// hybrid `as_of` query would hand back current truth while a keyword one handed
/// back history.
///
/// Apply **after** merging and ranking, **before** truncating to `top_k`, so the
/// returned page is filled with memories that existed at `as_of` rather than
/// padded with current-only ones.
///
/// `score` is left untouched: it was computed against the current index text
/// (see the limitation on `SearchQuery::as_of`), and re-scoring against
/// reconstructed text would misrepresent which path recalled the hit.
pub async fn project_results_as_of<S: MemoryStore + ?Sized>(
    store: &S,
    results: Vec<SearchResult>,
    as_of: DateTime<Utc>,
) -> crate::error::Result<Vec<SearchResult>> {
    let mut out = Vec::with_capacity(results.len());
    for mut hit in results {
        let id = hit.memory.id.clone();
        if let Some(state) = store.get_as_of(&id, as_of).await? {
            hit.memory = state.memory;
            hit.as_of_source = Some(state.source);
            out.push(hit);
        }
        // Not in effect at `as_of` — dropped, never silently replaced by the
        // current state.
    }
    Ok(out)
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn save(&self, memory: Memory) -> Result<Memory>;
    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory>;
    async fn get(&self, id: &str) -> Result<Memory>;
    async fn update(&self, memory: Memory) -> Result<Memory>;
    /// Soft delete: removes the live row but writes a full snapshot to the
    /// history/undo log first (restorable via the sqlite backend's
    /// `restore_checkpoint`). For a true, unrecoverable erasure use
    /// [`MemoryStore::purge`] instead.
    async fn delete(&self, id: &str) -> Result<()>;
    /// Latest `memory_history` snapshot for an id, used to erase a
    /// soft-deleted memory with the same protection/audit/cleanup treatment
    /// as a live one (`forget::ForgetManager`). Returns `None` when the id
    /// has no history at all; a storage-level read failure is an error, not
    /// an absence.
    async fn get_history_snapshot(&self, id: &str) -> Result<Option<Memory>>;
    /// The backend's persisted source-of-truth snapshot for `id`, even when
    /// neither a live row nor a history snapshot exists. The Markdown backend
    /// uses this to erase a stale `{id}.md` left behind after the disposable
    /// cache was rebuilt; the SQLite backend has no separate file source, so
    /// it returns `None`. A storage-level read failure is an error, not an
    /// absence.
    async fn get_persisted_snapshot(&self, id: &str) -> Result<Option<Memory>>;
    /// Permanent erasure (GDPR-style "forget"): removes the live row, its
    /// FTS entry, AND every existing history/undo snapshot for this id.
    /// Unlike [`MemoryStore::delete`], never writes a new history entry —
    /// nothing is left restorable. Callers are expected to have already
    /// recorded a [`ForgottenLogEntry`] via [`MemoryStore::record_forgotten`]
    /// before purging, since the content is gone afterward.
    async fn purge(&self, id: &str) -> Result<()>;
    /// Append one forget audit-trail row. Not linked by foreign key to
    /// `memories` — the row must survive the memory it describes being
    /// purged.
    async fn record_forgotten(&self, entry: &ForgottenLogEntry) -> Result<()>;
    /// List the forget audit trail, most recent first; `memory_id`
    /// optionally scopes to one memory.
    async fn list_forgotten_log(
        &self,
        memory_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ForgottenLogEntry>>;
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
    // --- Point-in-time (valid-time) reads ---

    /// The state `id` held at `as_of`, or `None` when there was no such state:
    /// not created yet, already deleted, or its `expires_at` had passed.
    /// Reconstructed from the `memory_history` snapshot chain, falling back to
    /// the live row. `MemoryAtTime::source` reports which of the two produced
    /// the answer.
    async fn get_as_of(&self, id: &str, as_of: DateTime<Utc>) -> Result<Option<MemoryAtTime>>;
    /// Every memory in effect at `as_of`, each reconstructed to the state it
    /// held then. Includes memories that have since been superseded, expired or
    /// soft-deleted — at `as_of` those were still the current truth — and
    /// excludes ones that did not exist yet.
    ///
    /// Deliberately **not** a hot path and not used by injection: it enumerates
    /// the union of live ids and `memory_history` ids, so cost scales with vault
    /// history rather than with `limit`.
    async fn list_as_of(
        &self,
        as_of: DateTime<Utc>,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MemoryAtTime>>;
    async fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>>;
    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()>;
    /// Drop a stale vector (e.g. after a body edit while the embedder is
    /// unavailable) so semantic search cannot return the old text's vector.
    /// A later backfill re-embeds from the current content.
    async fn clear_embedding(&self, id: &str) -> Result<()>;
    async fn record_access(&self, ids: &[String]) -> Result<usize>;
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

    // --- Procedural memory (skill_stats table) ---

    /// Count one injection of a skill (called when a skill actually enters
    /// a session's injected context).
    async fn record_skill_injection(&self, skill_memory_id: &str) -> Result<()>;
    /// Attribute one task outcome to a skill (record_outcome with a
    /// `skill_id`). Success and failure accumulate separately so the rate
    /// can distinguish "never ran" from "ran and failed".
    async fn record_skill_outcome(&self, skill_memory_id: &str, success: bool) -> Result<()>;
    /// Fetch accumulated stats for one skill; `None` when never tracked.
    async fn get_skill_stats(&self, skill_memory_id: &str) -> Result<Option<SkillStats>>;
    /// Mark `old_id` as superseded by `new_id` (human-confirmed knowledge
    /// replacement, C4): the old memory is archived to L0 with a
    /// `superseded_by` pointer — never deleted, always restorable.
    async fn supersede(&self, old_id: &str, new_id: &str) -> Result<()>;

    /// List team-pool memories (`visibility = 'shared'`), most recent first
    /// (Phase D). Used by session_start to inject shared knowledge into
    /// every session regardless of namespace.
    async fn list_shared(&self, limit: usize) -> Result<Vec<Memory>>;

    // --- Semantic memory (memory_relations table) ---

    /// Insert one relation triple. Validates that the subject (and
    /// `object_id`, when given) exist and that exactly one of
    /// `object_id`/`object_text` is set. Returns the new relation id.
    async fn add_relation(&self, relation: MemoryRelation) -> Result<i64>;
    /// Outgoing relations of a memory (as subject).
    async fn relations_of_subject(&self, memory_id: &str) -> Result<Vec<MemoryRelation>>;
    /// Incoming relations of a memory (as object).
    async fn relations_of_object(&self, memory_id: &str) -> Result<Vec<MemoryRelation>>;
    /// Delete one relation by id.
    async fn delete_relation(&self, relation_id: i64) -> Result<()>;

    // --- Trace ingestion watermark ---

    /// Fetch the ingestion watermark for one (agent, session); `None` when
    /// that session has never been ingested.
    async fn get_trace_watermark(
        &self,
        agent_key: &str,
        session_id: &str,
    ) -> Result<Option<TraceWatermark>>;
    /// Upsert the ingestion watermark for one (agent, session).
    async fn set_trace_watermark(&self, watermark: &TraceWatermark) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AsOfSource, MemoryType, Priority, SourceAgent};
    use crate::storage::sqlite::SqliteStore;
    use std::sync::Arc;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    /// A memory created two hours ago whose content was replaced just now, so
    /// there is a deterministic window in which only the snapshot describes it.
    async fn store_with_one_update() -> (SqliteStore, SearchResult, DateTime<Utc>) {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = Utc::now() - chrono::Duration::hours(2);

        let mut mem = Memory::new(
            MemoryType::Fact,
            "the retry budget is 3".to_string(),
            Priority::Reference,
            agent(),
        );
        mem.created_at = t0;
        mem.updated_at = t0;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let mut updated = store.get(&id).await.unwrap();
        updated.content = "the retry budget is 5".to_string();
        store.update(updated.clone()).await.unwrap();

        let hit = SearchResult {
            memory: updated,
            score: 0.9,
            hit_sources: vec![],
            as_of_source: None,
        };
        (store, hit, t0)
    }

    #[tokio::test]
    async fn test_project_rewrites_a_hit_to_its_point_in_time_state() {
        let (store, hit, t0) = store_with_one_update().await;
        let at = t0 + chrono::Duration::hours(1);

        let projected = project_results_as_of(&store, vec![hit], at).await.unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].memory.content, "the retry budget is 3");
        assert!(
            matches!(projected[0].as_of_source, Some(AsOfSource::History { .. })),
            "must report that the state was reconstructed, got {:?}",
            projected[0].as_of_source
        );
    }

    #[tokio::test]
    async fn test_project_keeps_score_and_provenance_from_the_original_recall() {
        let (store, mut hit, t0) = store_with_one_update().await;
        hit.score = 0.4242;
        hit.hit_sources = vec![crate::models::HitSource::Keyword { rank: 3 }];

        let projected = project_results_as_of(&store, vec![hit], t0 + chrono::Duration::hours(1))
            .await
            .unwrap();
        // Re-scoring against reconstructed text would misrepresent which path
        // recalled the hit, so both are carried over untouched.
        assert_eq!(projected[0].score, 0.4242);
        assert_eq!(
            projected[0].hit_sources,
            vec![crate::models::HitSource::Keyword { rank: 3 }]
        );
    }

    #[tokio::test]
    async fn test_project_drops_a_hit_that_did_not_exist_then() {
        let (store, hit, _t0) = store_with_one_update().await;
        let projected =
            project_results_as_of(&store, vec![hit], Utc::now() - chrono::Duration::hours(9))
                .await
                .unwrap();
        assert!(
            projected.is_empty(),
            "a hit created after `as_of` must be dropped, not served in its current form"
        );
    }

    #[tokio::test]
    async fn test_project_drops_a_hit_deleted_before_as_of() {
        let (store, hit, _t0) = store_with_one_update().await;
        let id = hit.memory.id.clone();
        store.delete(&id).await.unwrap();

        let projected =
            project_results_as_of(&store, vec![hit], Utc::now() + chrono::Duration::hours(1))
                .await
                .unwrap();
        assert!(
            projected.is_empty(),
            "deleted before as_of: no state to serve"
        );
    }

    #[tokio::test]
    async fn test_project_reports_live_when_nothing_changed_since() {
        let (store, hit, _t0) = store_with_one_update().await;
        let projected =
            project_results_as_of(&store, vec![hit], Utc::now() + chrono::Duration::seconds(1))
                .await
                .unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].memory.content, "the retry budget is 5");
        assert_eq!(projected[0].as_of_source, Some(AsOfSource::Live));
    }

    #[tokio::test]
    async fn test_project_filters_independently_per_hit() {
        // Two hits, only one of which existed at `as_of`: the projection must
        // decide per hit rather than all-or-nothing.
        let (store, hit, t0) = store_with_one_update().await;
        let newer = Memory::new(
            MemoryType::Fact,
            "added after the fact".to_string(),
            Priority::Reference,
            agent(),
        );
        store.save(newer.clone()).await.unwrap();
        let newer_hit = SearchResult {
            memory: newer,
            score: 0.5,
            hit_sources: vec![],
            as_of_source: None,
        };

        let projected = project_results_as_of(
            &store,
            vec![hit, newer_hit],
            t0 + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].memory.content, "the retry budget is 3");
    }

    #[tokio::test]
    async fn test_project_is_usable_through_a_trait_object() {
        // MCP and REST hold `Arc<dyn MemoryStore>`, so the `?Sized` bound is
        // what makes the helper callable at all — assert it, not just assume it.
        let (store, hit, t0) = store_with_one_update().await;
        let dynamic: Arc<dyn MemoryStore> = Arc::new(store);
        let projected =
            project_results_as_of(dynamic.as_ref(), vec![hit], t0 + chrono::Duration::hours(1))
                .await
                .unwrap();
        assert_eq!(projected.len(), 1);
    }
}
