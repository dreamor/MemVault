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
    // Capacity bounded by MAX_TOP_K — the same cap every process entrance
    // applies to `top_k` (CWE-789, code-scanning #16). Production callers all
    // hand this an already-bounded result set, so the clamp changes nothing
    // observable; it makes the invariant local and explicit instead of a
    // fact about distant callers that neither a reader nor a scanner can see
    // from the allocation site.
    let mut out = Vec::with_capacity(results.len().min(crate::models::MAX_TOP_K));
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

/// The result of [`MemoryStore::approve_supersede`]. Exactly one of the three
/// happens; in the two refusal cases *nothing at all* was written.
#[derive(Debug)]
pub enum ApproveSupersedeOutcome {
    /// The target is retired into the proposal; the approved row is
    /// returned. Boxed: `Memory` is ~500 bytes and the refusal variants
    /// would otherwise pay for it on every stack frame.
    Approved(Box<Memory>),
    /// The target no longer exists; the proposal was left untouched.
    TargetGone,
    /// A different memory already retired the target; the proposal was left
    /// untouched. `by` is the winner's id.
    AlreadyClaimed { by: String },
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
    /// Return a lightweight hash of the current store state for change
    /// detection. Deliberately time-sensitive: it folds in how many memories
    /// have crossed their `expires_at` bound as of now, because expiry
    /// writes nothing — a `COUNT`/`MAX(updated_at)`-only hash never moves on
    /// it and `sync --watch` keeps serving a rules file full of retired
    /// rules (audit #18). Stable between expiry crossings, so watchers do
    /// not churn on every poll.
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

    /// Atomically retire `target_id` into `proposal` and persist `proposal`
    /// as approved — one indivisible operation, the storage-layer primitive
    /// behind `proposal::approve_memory`.
    ///
    /// The check ("is the target still unclaimed?"), the target's retirement
    /// (`superseded_by` + L0), and the proposal's approval
    /// (`human_reviewed`, link consumed) must not be separable:
    ///
    /// - a crash between the two writes otherwise strands the vault with the
    ///   old rule retired and nothing approved in its place (audit #5);
    /// - two concurrent approvals of two proposals for one target otherwise
    ///   both read "unclaimed" and both land, leaving two live replacements
    ///   of the same fact (audit #6).
    ///
    /// Contract for implementations:
    ///
    /// - **SQLite**: one `BEGIN IMMEDIATE` transaction — read-check and both
    ///   row writes hold the write lock throughout, which is what makes it a
    ///   compare-and-swap rather than a check-then-act.
    /// - **Markdown**: a critical section serialized in-process (the vault's
    ///   cross-process file lock is already held for the store's lifetime).
    ///   The two file writes are not crash-atomic; the retry arm is the
    ///   documented recovery, and files remain the source of truth.
    /// - **Idempotent retry**: if the target is already superseded by *this*
    ///   proposal (a previous attempt retired it, then failed to persist the
    ///   approval), the approval completes rather than being refused —
    ///   recovery must be the one path that can always run.
    /// - `TargetGone` and `AlreadyClaimed` write nothing at all.
    /// - The implementation stamps `human_reviewed = true` and `updated_at`,
    ///   and consumes `proposes_supersede_of` on the approved row.
    /// - Self-supersede (`proposal.id == target_id`) is `InvalidInput`.
    async fn approve_supersede(
        &self,
        target_id: &str,
        proposal: Memory,
    ) -> Result<ApproveSupersedeOutcome>;

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

/// The non-text query predicates, evaluated against a `Memory` in Rust.
///
/// `append_filter_clauses` (sqlite.rs) expresses these in SQL because the live
/// keyword path can. Two paths cannot: a point-in-time state reconstructed from
/// `memory_history` may have no live row at all, and a vector hit arrives from
/// [`MemoryStore::vector_search`] — which takes only an embedding, a `top_k` and
/// a namespace — already merged with keyword hits that *were* filtered in SQL.
/// Both evaluate the same predicates here, in one function.
///
/// **This and `append_filter_clauses` must be kept in step.** A predicate added
/// to one and not the other silently widens the scope of semantic and historical
/// search relative to ordinary keyword search, and the result looks like a search
/// bug rather than a filter bug. That has happened three times now — `as_of`
/// alone, then the event-time window, then `type_filter`/`priority_filter` — so
/// the Rust side is deliberately collapsed to a single shared copy. The SQL side
/// stays SQL because it has to.
///
/// `at` is the instant the expiry bound is compared against: the query's
/// `as_of` when one is set, otherwise now. A memory that has since expired was
/// still in effect then.
pub fn memory_matches_filters(memory: &Memory, query: &SearchQuery, at: DateTime<Utc>) -> bool {
    if let Some(ref ns) = query.namespace {
        // Canonicalized on the read side too, so a caller-supplied spelling
        // (`project:MemVault` vs the canonical `project:memvault`) matches what
        // the write path stored — same rule as the SQL clause.
        let want = crate::models::canonical_project_namespace(ns);
        let got = crate::models::canonical_project_namespace(&memory.namespace);
        if got.as_ref() != want.as_ref() {
            return false;
        }
    }
    if let Some(ref mt) = query.type_filter
        && &memory.memory_type != mt
    {
        return false;
    }
    if let Some(ref pf) = query.priority_filter
        && &memory.priority != pf
    {
        return false;
    }
    // Superseded memories and pending proposals are not current truth; both stay
    // visible through `list`/`list_pending`/the Inbox, which is where they are
    // meant to be read. Read off the possibly-reconstructed state, so a memory
    // superseded *after* `at` correctly still counts as current truth then.
    if memory.superseded_by.is_some() || memory.proposes_supersede_of.is_some() {
        return false;
    }
    if memory.is_expired_at(at) {
        return false;
    }
    // Event-time window. An undated memory never matches: it cannot be said to
    // have happened inside a range, and admitting it would return the same rows
    // for every window — noise dressed as a result.
    if query.occurred_from.is_some() || query.occurred_to.is_some() {
        let Some(occurred) = memory.occurred_at else {
            return false;
        };
        if let Some(from) = query.occurred_from
            && occurred < from
        {
            return false;
        }
        if let Some(to) = query.occurred_to
            && occurred > to
        {
            return false;
        }
    }
    true
}

/// Apply the query-level filters that `MemoryStore::vector_search` cannot express.
///
/// `vector_search` takes only an embedding, a `top_k` and a namespace, so the
/// semantic and hybrid composition paths in the MCP server and REST API merge its
/// results with keyword results that *were* filtered in SQL. Without this, every
/// other predicate constrains keyword hits only — a semantic search would still
/// return memories from outside the event-time window, of the wrong type or
/// priority, and undated ones the window is defined to exclude. Same for `as_of`,
/// which needs both the in-effect filter and the point-in-time reconstruction.
///
/// Shared rather than duplicated because the two callers already drifted once on
/// `as_of` alone, and a filter that only one channel applies is not a filter.
///
/// Filtering happens **after** the merge and **before** reranking, so the reranker
/// scores what will actually be returned and `top_k` is filled with in-window
/// memories rather than padded with out-of-window ones.
///
/// Safe to call unconditionally on any non-keyword result set: predicates the
/// vector SQL already applied (`namespace`, superseded, expired) are idempotent
/// here, and a query carrying no filters at all is an O(n) pass that drops
/// nothing.
pub async fn apply_query_filters<S: MemoryStore + ?Sized>(
    store: &S,
    results: Vec<SearchResult>,
    query: &SearchQuery,
) -> Result<Vec<SearchResult>> {
    let mut out = results;

    // Project first, then filter, so the predicates see the state that was in
    // effect at `as_of` rather than today's. The other order drops a memory that
    // was superseded or expired *since* `as_of` and was therefore legitimately
    // current then — and it is what the historical keyword path in
    // `search_history_at` already does, so the two Rust paths agree.
    if let Some(as_of) = query.as_of {
        out = project_results_as_of(store, out, as_of).await?;
    }

    let at = query.as_of.unwrap_or_else(Utc::now);
    out.retain(|r| memory_matches_filters(&r.memory, query, at));

    Ok(out)
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

    /// A hit exactly as the vector branch hands it to the merger: score set, no
    /// provenance, and — the point — reached by no SQL predicate at all.
    fn hit(memory: Memory) -> SearchResult {
        SearchResult {
            memory,
            score: 0.9,
            hit_sources: vec![],
            as_of_source: None,
        }
    }

    fn fact(content: &str, namespace: &str) -> Memory {
        let mut m = Memory::new(
            MemoryType::Fact,
            content.to_string(),
            Priority::Reference,
            agent(),
        );
        m.namespace = namespace.to_string();
        m
    }

    /// The regression this whole pass exists for.
    ///
    /// `vector_search` takes only an embedding, a `top_k` and a namespace, so on
    /// the semantic and hybrid paths the merged result set carries hits that no
    /// SQL predicate ever saw. Each of the filters below bound on the keyword
    /// branch alone until `apply_query_filters` was handed the *whole* query
    /// rather than a time-axes-only copy of it — a search scoped to one type or
    /// priority returned all of them, and looked like a ranking problem rather
    /// than a missing filter.
    #[tokio::test]
    async fn test_post_merge_filters_apply_every_predicate_the_sql_path_does() {
        let store = SqliteStore::in_memory().unwrap();

        let mut preference = Memory::new(
            MemoryType::Preference,
            "marker zqx".to_string(),
            Priority::Must,
            agent(),
        );
        preference.namespace = "project:alpha".to_string();
        let alpha_fact = fact("marker zqx", "project:alpha");
        let beta_fact = fact("marker zqx", "project:beta");

        // What the merger produces: all three, unfiltered. Rebuilt per case
        // because `apply_query_filters` takes ownership.
        let merged = || {
            vec![
                hit(preference.clone()),
                hit(alpha_fact.clone()),
                hit(beta_fact.clone()),
            ]
        };

        /// The namespaces a filter leaves behind, as the observable.
        async fn surviving(
            store: &SqliteStore,
            q: SearchQuery,
            merged: Vec<SearchResult>,
        ) -> Vec<String> {
            apply_query_filters(store, merged, &q)
                .await
                .unwrap()
                .into_iter()
                .map(|r| format!("{}/{:?}", r.memory.namespace, r.memory.memory_type))
                .collect()
        }

        assert_eq!(
            surviving(
                &store,
                SearchQuery {
                    type_filter: Some(MemoryType::Preference),
                    ..SearchQuery::new(String::new())
                },
                merged(),
            )
            .await,
            vec!["project:alpha/Preference"],
            "type_filter must bind on the post-merge path"
        );
        assert_eq!(
            surviving(
                &store,
                SearchQuery {
                    priority_filter: Some(Priority::Must),
                    ..SearchQuery::new(String::new())
                },
                merged(),
            )
            .await,
            vec!["project:alpha/Preference"],
            "priority_filter must bind on the post-merge path"
        );
        assert_eq!(
            surviving(
                &store,
                SearchQuery {
                    namespace: Some("project:beta".to_string()),
                    ..SearchQuery::new(String::new())
                },
                merged(),
            )
            .await,
            vec!["project:beta/Fact"],
            "namespace must bind on the post-merge path"
        );
        // Canonicalized exactly as the SQL clause does, so a caller-supplied
        // spelling matches what the write path stored.
        assert_eq!(
            surviving(
                &store,
                SearchQuery {
                    namespace: Some("project:Alpha".to_string()),
                    ..SearchQuery::new(String::new())
                },
                merged(),
            )
            .await,
            vec!["project:alpha/Preference", "project:alpha/Fact"],
            "a non-canonical spelling must still match the canonical namespace"
        );
    }

    /// An undated memory cannot be said to have happened inside a range, and
    /// admitting it would return the same rows for every window.
    #[tokio::test]
    async fn test_undated_memory_never_matches_an_event_time_window() {
        let store = SqliteStore::in_memory().unwrap();
        let t0 = Utc::now() - chrono::Duration::days(1);

        let mut dated = fact("the incident was on the east cluster", "project:alpha");
        dated.occurred_at = Some(t0);
        let undated = fact("the east cluster exists", "project:alpha");

        let q = SearchQuery {
            occurred_from: Some(t0 - chrono::Duration::hours(1)),
            occurred_to: Some(t0 + chrono::Duration::hours(1)),
            ..SearchQuery::new(String::new())
        };
        let out = apply_query_filters(&store, vec![hit(dated.clone()), hit(undated)], &q)
            .await
            .unwrap();
        assert_eq!(out.len(), 1, "only the dated memory can match a window");
        assert_eq!(out[0].memory.id, dated.id);

        // And one outside the window is excluded, which is the reported symptom.
        let q = SearchQuery {
            occurred_from: Some(t0 + chrono::Duration::days(30)),
            occurred_to: Some(t0 + chrono::Duration::days(31)),
            ..SearchQuery::new(String::new())
        };
        let out = apply_query_filters(&store, vec![hit(dated)], &q)
            .await
            .unwrap();
        assert!(out.is_empty(), "an out-of-window memory must not survive");
    }

    /// Order matters, and it is the half of the fix that is easy to get wrong:
    /// the predicates have to see the state that was in effect at `as_of`.
    /// Filtering first would drop a memory superseded *since* then, which was
    /// legitimately current truth at the queried instant — and it is what the
    /// historical keyword path in `search_history_at` already does, so the two
    /// Rust paths agree only if this one projects first too.
    #[tokio::test]
    async fn test_as_of_filters_the_reconstructed_state_not_todays() {
        let (store, before, t0) = store_with_one_update().await;
        let at = t0 + chrono::Duration::hours(1);

        // Supersede it now, well after `at`.
        let successor = store
            .save(fact("the retry budget is 9", "global"))
            .await
            .unwrap();
        store
            .supersede(&before.memory.id, &successor.id)
            .await
            .unwrap();
        assert!(
            store
                .get(&before.memory.id)
                .await
                .unwrap()
                .superseded_by
                .is_some(),
            "precondition: superseded as of now"
        );

        let q = SearchQuery {
            as_of: Some(at),
            ..SearchQuery::new(String::new())
        };
        let out = apply_query_filters(&store, vec![before], &q).await.unwrap();
        assert_eq!(
            out.len(),
            1,
            "superseded after `as_of`, so it was still current truth then"
        );
        assert_eq!(out[0].memory.content, "the retry budget is 3");

        // The same memory, queried as of now, is correctly gone.
        let q = SearchQuery {
            as_of: Some(Utc::now()),
            ..SearchQuery::new(String::new())
        };
        let out = apply_query_filters(&store, vec![], &q).await.unwrap();
        assert!(out.is_empty());
    }

    /// Called unconditionally on every non-keyword search, so a query carrying
    /// no filters has to be a genuine pass-through — otherwise the simplification
    /// that lets both call sites drop their `has_time_filter` guard quietly
    /// narrows every semantic search.
    #[tokio::test]
    async fn test_unfiltered_query_passes_results_through() {
        let (store, before, _) = store_with_one_update().await;
        let out = apply_query_filters(
            &store,
            vec![before.clone()],
            &SearchQuery::new(String::new()),
        )
        .await
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].memory.id, before.memory.id);
    }
}
