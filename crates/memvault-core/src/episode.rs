//! Episodic memory: record structured task outcomes as episode memories.
//!
//! One recorded outcome = one episode `Memory` row (searchable content, tags,
//! namespace) plus one `episodes` row (task/status/cause/lesson). The two
//! are linked 1:1 by memory id; deleting the memory cascades the record.

use crate::embedding::EmbeddingProvider;
use crate::error::{MemVaultError, Result};
use crate::intent::trigger_matches_context;
use crate::models::{
    EpisodeFilter, Memory, MemoryLayer, MemoryType, NEEDS_REVISION_TAG, OutcomeStatus, Priority,
    SKILL_DRAFT_TAG, SKILL_DRAFT_THRESHOLD, SkillMeta, SourceAgent,
};
use crate::outcome_inference::FRICTION_CAUSE_PREFIX;
use crate::storage::MemoryStore;
use tracing::{debug, warn};

/// Everything needed to record one task outcome.
#[derive(Debug, Clone)]
pub struct OutcomeInput {
    /// What the agent was trying to do.
    pub task: String,
    pub status: OutcomeStatus,
    /// Attribution of the outcome, when known.
    pub cause: Option<String>,
    /// Coarse task category (deploy/debug/refactor/...) used later for
    /// lesson matching at session start.
    pub task_type: Option<String>,
    /// The skill the agent followed, if any — attributes this outcome to the
    /// skill's success/failure statistics (procedural memory tracking).
    pub skill_id: Option<String>,
    pub tags: Vec<String>,
    pub namespace: String,
    pub source_agent: SourceAgent,
    pub outcome_provenance: Option<crate::models::OutcomeProvenance>,
}

/// A recorded outcome: the saved episode memory and whether it got a vector.
#[derive(Debug, Clone)]
pub struct RecordedOutcome {
    pub memory: Memory,
    pub embedded: bool,
    /// The skill this outcome was attributed to, if attribution happened.
    pub attributed_skill: Option<String>,
    /// Skills flagged `needs-revision` because this failure's task_type
    /// matched their trigger — their procedure may be at fault.
    pub flagged_skills: Vec<String>,
    /// A newly created skill-draft memory id, when repeated same-type
    /// successes crossed the distillation threshold for the first time.
    pub skill_draft_id: Option<String>,
    /// `Some(id)` when this outcome was already on record for the agent's
    /// session (same task/status/cause): no episode row was written and the
    /// returned `memory` is the pre-existing one. Session-scoped idempotency
    /// — a host Stop hook fires once per assistant turn, so without this the
    /// same inferred outcome would be appended over and over. Callers must
    /// not re-run downstream reflection/effectiveness on a duplicate.
    pub duplicate_of: Option<String>,
}

/// Human-searchable content for an episode memory: status tag + task, with
/// the cause appended when present. Keeps the raw task text verbatim so
/// keyword search over "what failed" works without parsing structure.
fn episode_content(input: &OutcomeInput) -> String {
    let mut content = format!("[{}] {}", input.status.as_str(), input.task);
    if let Some(ref cause) = input.cause {
        content.push_str(&format!(" — cause: {cause}"));
    }
    content
}

/// Tags for the episode memory: caller tags plus status and task_type, so
/// keyword retrieval can hit both without structured filters.
fn episode_tags(input: &OutcomeInput) -> Vec<String> {
    let mut tags = input.tags.clone();
    let status_tag = input.status.as_str().to_string();
    if !tags.iter().any(|t| t == &status_tag) {
        tags.push(status_tag);
    }
    if let Some(ref tt) = input.task_type
        && !tags.iter().any(|t| t == tt)
    {
        tags.push(tt.clone());
    }
    tags
}

/// Tag every skill whose trigger matches a failed task's type as
/// `needs-revision`. A procedure that was followed and still failed is the
/// prime suspect — flag it for human revision (never auto-edit; the version
/// bump happens when a person actually changes the skill).
async fn flag_skills_for_revision(
    store: &dyn MemoryStore,
    task_type: &str,
    namespace: &str,
) -> Vec<String> {
    let mut flagged = Vec::new();

    let mut namespaces: Vec<String> = vec![namespace.to_string()];
    if namespace != "global" {
        namespaces.push("global".to_string());
    }

    for ns in namespaces {
        let query = crate::models::SearchQuery {
            query: String::new(),
            namespace: Some(ns.clone()),
            type_filter: Some(MemoryType::Skill),
            top_k: 100,
            ..crate::models::SearchQuery::new(String::new())
        };
        let candidates = match store.search(query).await {
            Ok(o) => o.results,
            Err(e) => {
                warn!(error = %e, "failed to list skills for revision flagging");
                continue;
            }
        };
        for r in candidates {
            let mut mem = r.memory;
            if mem.superseded_by.is_some() || mem.layer == MemoryLayer::L0 {
                continue;
            }
            let Some(ref meta) = mem.skill_meta else {
                continue;
            };
            let Some(ref trigger) = meta.trigger else {
                continue;
            };
            if !trigger_matches_context(trigger, task_type) {
                continue;
            }
            if mem.tags.iter().any(|t| t == NEEDS_REVISION_TAG) {
                // Already flagged — still report it so callers can surface
                // the hint, but don't write again.
                if !flagged.contains(&mem.id) {
                    flagged.push(mem.id.clone());
                }
                continue;
            }
            mem.tags.push(NEEDS_REVISION_TAG.to_string());
            mem.updated_at = chrono::Utc::now();
            match store.update(mem.clone()).await {
                Ok(_) => {
                    if !flagged.contains(&mem.id) {
                        flagged.push(mem.id.clone());
                    }
                }
                Err(e) => {
                    warn!(skill_id = %mem.id, error = %e, "failed to flag skill for revision")
                }
            }
        }
    }
    flagged
}

/// Repeated same-type success may be a reusable procedure in disguise. Once
/// the count of successes for a task_type reaches [`SKILL_DRAFT_THRESHOLD`],
/// propose ONE skill draft (trigger = task_type, steps seeded from the task
/// descriptions) and put it in the review queue — distillation proposes,
/// humans dispose. Returns the draft's id, or `None` when the threshold
/// isn't reached or a draft already exists for that task_type.
async fn maybe_draft_skill(store: &dyn MemoryStore, input: &OutcomeInput) -> Option<String> {
    let task_type = input.task_type.as_deref()?;

    let filter = EpisodeFilter {
        task_type: Some(task_type.to_string()),
        status: Some(OutcomeStatus::Success),
        namespace: Some(input.namespace.clone()),
        session_id: None,
        limit: 200,
    };
    let successes = match store.list_episodes(filter).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "failed to count successes for skill drafting");
            return None;
        }
    };
    if successes.len() < SKILL_DRAFT_THRESHOLD {
        return None;
    }

    // Already drafted for this task_type? Don't spam the review queue.
    let existing = store
        .search(crate::models::SearchQuery {
            query: String::new(),
            namespace: Some(input.namespace.clone()),
            type_filter: Some(MemoryType::Skill),
            top_k: 100,
            ..crate::models::SearchQuery::new(String::new())
        })
        .await
        .map(|o| o.results)
        .unwrap_or_default();
    let already_drafted = existing.iter().any(|r| {
        r.memory.tags.iter().any(|t| t == SKILL_DRAFT_TAG)
            && r.memory.tags.iter().any(|t| t == task_type)
    });
    if already_drafted {
        return None;
    }

    let steps: Vec<String> = successes
        .iter()
        .take(SKILL_DRAFT_THRESHOLD)
        .map(|e| e.task.clone())
        .collect();

    if let Some(step) = steps
        .iter()
        .find(|step| crate::transcript::internal_contamination(step).is_some())
    {
        warn!(reason = "canonical codex internal goal envelope", source_hash = %crate::transcript::source_hash(step.as_bytes()), "skill drafting skipped internal scaffolding in prior episode task");
        return None;
    }
    let mut draft = Memory::new(
        MemoryType::Skill,
        format!(
            "Skill draft: '{}' workflow (distilled from {} successes) — steps need human review",
            task_type,
            successes.len()
        ),
        Priority::Reference,
        input.source_agent.clone(),
    );
    draft.namespace = input.namespace.clone();
    draft.tags = vec![SKILL_DRAFT_TAG.to_string(), task_type.to_string()];
    draft.layer = MemoryLayer::L1;
    draft.skill_meta = Some(SkillMeta {
        trigger: Some(task_type.to_string()),
        steps,
        verification: None,
        version: 1,
    });
    // ai_generated=true + human_reviewed=false (Memory::new defaults) → the
    // draft lands in the review queue rather than being trusted outright.

    match store.save(draft.clone()).await {
        Ok(saved) => Some(saved.id),
        Err(e) => {
            warn!(error = %e, "failed to save skill draft");
            None
        }
    }
}

/// Normalize a recorded cause for the session-scoped idempotency comparison.
///
/// The rule floor renders a bumpy-but-cleanly-ended session as
/// `session had friction before ending cleanly (retry×1, rejection×0,
/// correction×6)`, and those counters accumulate over the life of a session.
/// Re-inferring the same session on a later Stop therefore yields a textually
/// different cause for what is still one outcome — 16 episodes were recorded
/// for a single Codex session this way. Collapsing every counter variant onto
/// the shared prefix keeps the guard working, while any other cause (an
/// LLM-authored one, a terminal detail, a human-supplied one) is still
/// compared verbatim so genuinely different outcomes keep recording.
fn normalize_outcome_cause(cause: Option<&str>) -> Option<&str> {
    cause.map(|c| {
        if c.starts_with(FRICTION_CAUSE_PREFIX) {
            FRICTION_CAUSE_PREFIX
        } else {
            c
        }
    })
}

/// Verify supplied trace claims against actual available L0 rows. No scanning,
/// no synthetic ids and no new evidence retention. Exact content matching is
/// intentionally conservative: truncated/grouped inputs may have no link.
pub(crate) async fn verified_source_trace_ids(
    store: &dyn MemoryStore,
    provenance: Option<&crate::models::OutcomeProvenance>,
    agent: &SourceAgent,
    task: &str,
    cause: Option<&str>,
) -> Vec<String> {
    let Some(provenance) = provenance else {
        return Vec::new();
    };
    let Some(session_id) = agent.session_id.as_ref() else {
        return Vec::new();
    };
    let task_hash = crate::transcript::source_hash(task.as_bytes());
    let cause_hash = cause.map(|c| crate::transcript::source_hash(c.as_bytes()));
    let mut ids = Vec::new();
    for id in &provenance.source_trace_ids {
        if ids.contains(id) {
            continue;
        }
        let Ok(memory) = store.get(id).await else {
            continue;
        };
        let hash = crate::transcript::source_hash(memory.content.as_bytes());
        if memory.id == *id
            && memory.layer == MemoryLayer::L0
            && memory
                .namespace
                .starts_with(crate::trace::TRACE_NAMESPACE_PREFIX)
            && memory
                .tags
                .iter()
                .any(|t| t == crate::trace::TRACE_SOURCE_TAG)
            && memory.source_agent.id == agent.id
            && memory.source_agent.session_id.as_ref() == Some(session_id)
            && (hash == task_hash || cause_hash.as_ref() == Some(&hash))
        {
            ids.push(memory.id);
        }
    }
    ids
}

/// Known request row identities distinguish repeated executions with identical
/// text. Legacy/manual None (or partial metadata with no group) retains the
/// historical session/task/status/cause comparison; append/hash changes within
/// one known group do not create a new execution.
fn same_request_group(
    previous: Option<&crate::models::OutcomeProvenance>,
    current: Option<&crate::models::OutcomeProvenance>,
) -> bool {
    match (previous, current) {
        (Some(previous), Some(current))
            if !previous.task_seq_group.is_empty() && !current.task_seq_group.is_empty() =>
        {
            previous.task_seq_group == current.task_seq_group
        }
        _ => true,
    }
}

/// Record one task outcome: save the episode memory (embedding when an
/// embedder is available), then attach the structured record.
///
/// Episodes are L1 raw experience at BACKGROUND priority — promotion to
/// lessons/instructions happens in the reflection step, not here.
pub async fn record_outcome(
    store: &dyn MemoryStore,
    mut input: OutcomeInput,
    embedder: Option<&dyn EmbeddingProvider>,
) -> Result<RecordedOutcome> {
    for (field, text) in [
        ("task", Some(input.task.as_str())),
        ("task_type", input.task_type.as_deref()),
        ("cause", input.cause.as_deref()),
    ] {
        if let Some(text) = text
            && let Some(reason) = crate::transcript::internal_contamination(text)
        {
            let hash = crate::transcript::source_hash(text.as_bytes());
            warn!(field, reason, source_hash = %hash, "episode skipped internal scaffolding");
            return Err(MemVaultError::InvalidInput(format!(
                "internal scaffolding in {field}: {reason} (sha256:{hash})"
            )));
        }
    }
    let verified_ids = verified_source_trace_ids(
        store,
        input.outcome_provenance.as_ref(),
        &input.source_agent,
        &input.task,
        input.cause.as_deref(),
    )
    .await;
    if let Some(provenance) = input.outcome_provenance.as_mut() {
        provenance.source_trace_ids = verified_ids;
    }
    // Session-scoped idempotency (docs/DESIGN.md §9.1.1, extended from delta
    // write): a Stop hook fires once per assistant turn and derives the
    // session's `task` from the same opening turn each time, so a naive
    // re-record appends byte-identical episodes forever. When this exact
    // outcome (same agent session, task, status, cause) is already on record,
    // return it instead of inserting a duplicate; a genuinely different
    // outcome for the session still records. `cause` is compared through
    // `normalize_outcome_cause`, because the rule floor embeds the session's
    // *cumulative* friction counters in it — without that, a long session
    // re-inferred on every Stop looks like a new outcome every time.
    if let Some(ref session_id) = input.source_agent.session_id {
        let existing = store
            .list_episodes(EpisodeFilter {
                session_id: Some(session_id.clone()),
                limit: 200,
                ..EpisodeFilter::default()
            })
            .await?;
        if let Some(dup) = existing.iter().find(|e| {
            e.task == input.task
                && same_request_group(
                    e.outcome_provenance.as_ref(),
                    input.outcome_provenance.as_ref(),
                )
                && e.status == input.status
                && normalize_outcome_cause(e.cause.as_deref())
                    == normalize_outcome_cause(input.cause.as_deref())
        }) {
            let memory = store.get(&dup.memory_id).await?;
            debug!(
                id = %dup.memory_id,
                session_id = %session_id,
                "episode already recorded for this session; skipping duplicate"
            );
            return Ok(RecordedOutcome {
                memory,
                embedded: false,
                attributed_skill: None,
                flagged_skills: Vec::new(),
                skill_draft_id: None,
                duplicate_of: Some(dup.memory_id.clone()),
            });
        }
    }

    let mut mem = Memory::new(
        MemoryType::Episode,
        episode_content(&input),
        Priority::Background,
        input.source_agent.clone(),
    );
    mem.namespace = input.namespace.clone();
    mem.tags = episode_tags(&input);
    mem.layer = MemoryLayer::L1;
    mem.source_trace_ids = input
        .outcome_provenance
        .as_ref()
        .map(|p| p.source_trace_ids.clone())
        .unwrap_or_default();
    if let Some(reason) = crate::transcript::internal_contamination(&mem.content) {
        return Err(MemVaultError::InvalidInput(format!(
            "internal scaffolding in episode content: {reason} (sha256:{})",
            crate::transcript::source_hash(mem.content.as_bytes())
        )));
    }

    let embed_text = mem.content.clone();
    let mut embedded = false;

    let saved = if let Some(embedder) = embedder {
        match embedder.embed(&[embed_text]).await {
            Ok(embeddings) if !embeddings.is_empty() => {
                embedded = true;
                store
                    .save_with_embedding(mem, embeddings.into_iter().next().unwrap())
                    .await?
            }
            Ok(_) => store.save(mem).await?,
            Err(e) => {
                warn!("Episode auto-embedding failed, saving without: {}", e);
                store.save(mem).await?
            }
        }
    } else {
        store.save(mem).await?
    };

    let record = crate::models::EpisodeRecord {
        memory_id: saved.id.clone(),
        task: input.task.clone(),
        task_type: input.task_type.clone(),
        status: input.status,
        cause: input.cause.clone(),
        lesson: None,
        lesson_memory_id: None,
        outcome_provenance: input.outcome_provenance.clone(),
        occurred_at: saved.created_at,
    };

    if let Err(e) = store.record_episode(record).await {
        // Best-effort cleanup: an episode memory without its outcome record is
        // an orphan the rest of the pipeline cannot interpret.
        if let Err(cleanup_err) = store.delete(&saved.id).await {
            warn!(
                id = %saved.id,
                error = %cleanup_err,
                "failed to roll back orphan episode memory"
            );
        }
        return Err(e);
    }

    // Skill attribution: feed this outcome into the followed skill's stats.
    // A bad skill_id is a caller error — fail loudly rather than silently
    // drop the attribution.
    let mut attributed_skill = None;
    if let Some(ref skill_id) = input.skill_id {
        let skill = store.get(skill_id).await?;
        if skill.memory_type != MemoryType::Skill {
            return Err(MemVaultError::InvalidInput(format!(
                "skill_id '{}' is not a skill memory (type: {:?})",
                skill_id, skill.memory_type
            )));
        }
        store
            .record_skill_outcome(skill_id, input.status == OutcomeStatus::Success)
            .await?;
        attributed_skill = Some(skill_id.clone());
    }

    // Version-evolution signal: a failed/partial task whose type matches a
    // skill trigger flags that skill for human revision.
    let mut flagged_skills = Vec::new();
    if input.status != OutcomeStatus::Success
        && let Some(ref task_type) = input.task_type
    {
        flagged_skills = flag_skills_for_revision(store, task_type, &input.namespace).await;
    }

    // Distillation signal: repeated same-type success may propose a skill.
    let mut skill_draft_id = None;
    if input.status == OutcomeStatus::Success {
        skill_draft_id = maybe_draft_skill(store, &input).await;
    }

    Ok(RecordedOutcome {
        memory: saved,
        embedded,
        attributed_skill,
        flagged_skills,
        skill_draft_id,
        duplicate_of: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OutcomeStatus;
    use crate::storage::sqlite::SqliteStore;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "test-agent".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: Some("sess_1".to_string()),
        }
    }

    fn input(task: &str, status: OutcomeStatus) -> OutcomeInput {
        OutcomeInput {
            task: task.into(),
            status,
            cause: None,
            task_type: None,
            skill_id: None,
            tags: vec!["project-x".into()],
            namespace: "global".into(),
            source_agent: agent(),
            outcome_provenance: None,
        }
    }

    struct NeverEmbed;
    #[async_trait::async_trait]
    impl EmbeddingProvider for NeverEmbed {
        async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
            panic!("contaminated episode must be rejected before embedding")
        }
        fn dimension(&self) -> usize {
            2
        }
    }

    #[tokio::test]
    async fn contaminated_outcome_rejected_before_embedding_save_and_error_is_hash_only() {
        let store = SqliteStore::in_memory().unwrap();
        let mut candidate = input("synthetic task", OutcomeStatus::Failure);
        candidate.cause = Some(crate::transcript::SYNTHETIC_INTERNAL_GOAL.into());
        let err = record_outcome(&store, candidate, Some(&NeverEmbed))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("sha256:"));
        assert!(!err.contains("Continue working toward"));
        assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 0);
        assert_eq!(
            store
                .list_episodes(EpisodeFilter::default())
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn metadata_changes_do_not_break_outcome_dedup_or_overwrite_original_provenance() {
        let store = SqliteStore::in_memory().unwrap();
        let mut candidate = input("synthetic deploy", OutcomeStatus::Failure);
        candidate.cause = Some("disk full".into());
        candidate.outcome_provenance = Some(crate::models::OutcomeProvenance {
            transcript_format: "codex_rollout".into(),
            selector_version: "active-request-v1".into(),
            task_seq_group: vec![2],
            evidence_seq_group: Vec::new(),
            evidence_start_seq: 2,
            evidence_end_seq: 5,
            source_hash: "a".repeat(64),
            window_hash: "b".repeat(64),
            source_trace_ids: Vec::new(),
        });
        let first = record_outcome(&store, candidate.clone(), None)
            .await
            .unwrap();
        candidate
            .outcome_provenance
            .as_mut()
            .unwrap()
            .evidence_end_seq = 99;
        candidate.outcome_provenance.as_mut().unwrap().source_hash = "c".repeat(64);
        let again = record_outcome(&store, candidate, Some(&NeverEmbed))
            .await
            .unwrap();
        assert_eq!(
            again.duplicate_of.as_deref(),
            Some(first.memory.id.as_str())
        );
        assert_eq!(
            store
                .get_episode(&first.memory.id)
                .await
                .unwrap()
                .outcome_provenance
                .unwrap()
                .evidence_end_seq,
            5
        );
        assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_record_outcome_creates_memory_and_record() {
        let store = SqliteStore::in_memory().unwrap();
        let mut input = input("deploy the api", OutcomeStatus::Failure);
        input.cause = Some("missing env var".into());
        input.task_type = Some("deploy".into());

        let recorded = record_outcome(&store, input, None).await.unwrap();

        assert!(recorded.memory.id.starts_with("mem_"));
        assert!(!recorded.embedded);
        assert_eq!(recorded.memory.memory_type, MemoryType::Episode);
        assert_eq!(recorded.memory.priority, Priority::Background);
        assert_eq!(recorded.memory.layer, MemoryLayer::L1);
        assert!(recorded.memory.content.contains("deploy the api"));
        assert!(recorded.memory.content.contains("[failure]"));
        assert!(recorded.memory.tags.contains(&"failure".to_string()));
        assert!(recorded.memory.tags.contains(&"deploy".to_string()));
        assert!(recorded.memory.tags.contains(&"project-x".to_string()));

        let episode = store.get_episode(&recorded.memory.id).await.unwrap();
        assert_eq!(episode.task, "deploy the api");
        assert_eq!(episode.status, OutcomeStatus::Failure);
        assert_eq!(episode.cause.as_deref(), Some("missing env var"));
        assert_eq!(episode.task_type.as_deref(), Some("deploy"));
        assert!(episode.lesson.is_none());
    }

    #[tokio::test]
    async fn test_record_outcome_success_keeps_task_verbatim() {
        let store = SqliteStore::in_memory().unwrap();
        let recorded = record_outcome(
            &store,
            input("fix the flaky test", OutcomeStatus::Success),
            None,
        )
        .await
        .unwrap();
        assert_eq!(recorded.memory.content, "[success] fix the flaky test");
    }

    #[tokio::test]
    async fn test_episode_content_includes_cause() {
        let mut input = input("build the image", OutcomeStatus::Failure);
        input.cause = Some("registry timeout".into());
        let content = episode_content(&input);
        assert!(content.contains("build the image"));
        assert!(content.contains("registry timeout"));
    }

    #[tokio::test]
    async fn test_episode_tags_no_duplicates() {
        let mut input = input("task", OutcomeStatus::Success);
        input.tags = vec!["success".into(), "deploy".into()];
        input.task_type = Some("deploy".into());
        let tags = episode_tags(&input);
        assert_eq!(tags.iter().filter(|t| *t == "success").count(), 1);
        assert_eq!(tags.iter().filter(|t| *t == "deploy").count(), 1);
    }

    async fn make_skill(store: &SqliteStore) -> String {
        let mut mem = Memory::new(
            MemoryType::Skill,
            "deploy runbook".to_string(),
            Priority::Reference,
            agent(),
        );
        mem.skill_meta = Some(crate::models::SkillMeta {
            trigger: Some("deploy".into()),
            steps: vec!["check env".into()],
            verification: None,
            version: 1,
        });
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        id
    }

    #[tokio::test]
    async fn test_skill_attribution_success_and_failure() {
        let store = SqliteStore::in_memory().unwrap();
        let skill_id = make_skill(&store).await;

        let mut ok = input("deploy the dashboard", OutcomeStatus::Success);
        ok.skill_id = Some(skill_id.clone());
        let recorded = record_outcome(&store, ok, None).await.unwrap();
        assert_eq!(
            recorded.attributed_skill.as_deref(),
            Some(skill_id.as_str())
        );

        let mut bad = input("deploy the api", OutcomeStatus::Failure);
        bad.skill_id = Some(skill_id.clone());
        record_outcome(&store, bad, None).await.unwrap();

        let stats = store
            .get_skill_stats(&skill_id)
            .await
            .unwrap()
            .expect("tracked");
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.failure_count, 1);
    }

    #[tokio::test]
    async fn test_skill_attribution_missing_skill_errors() {
        let store = SqliteStore::in_memory().unwrap();
        let mut input = input("deploy", OutcomeStatus::Success);
        input.skill_id = Some("mem_nonexistent".into());
        let err = record_outcome(&store, input, None).await.unwrap_err();
        assert!(matches!(err, crate::error::MemVaultError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_skill_attribution_non_skill_memory_rejected() {
        let store = SqliteStore::in_memory().unwrap();
        // A plain fact memory, not a skill.
        let fact = Memory::new(
            MemoryType::Fact,
            "not a skill".to_string(),
            Priority::Reference,
            agent(),
        );
        let fact_id = fact.id.clone();
        store.save(fact).await.unwrap();

        let mut input = input("deploy", OutcomeStatus::Success);
        input.skill_id = Some(fact_id);
        let err = record_outcome(&store, input, None).await.unwrap_err();
        assert!(matches!(err, crate::error::MemVaultError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_failure_flags_matching_skill_for_revision() {
        let store = SqliteStore::in_memory().unwrap();
        let skill_id = make_skill(&store).await; // trigger: "deploy"

        let mut fail = input("deploy the dashboard", OutcomeStatus::Failure);
        fail.cause = Some("bad step".into());
        fail.task_type = Some("deploy".into());
        let recorded = record_outcome(&store, fail, None).await.unwrap();

        assert_eq!(recorded.flagged_skills, vec![skill_id.clone()]);
        let skill = store.get(&skill_id).await.unwrap();
        assert!(
            skill
                .tags
                .iter()
                .any(|t| t == crate::models::NEEDS_REVISION_TAG),
            "matching skill must be tagged needs-revision after a failure"
        );
    }

    #[tokio::test]
    async fn test_success_does_not_flag_skills() {
        let store = SqliteStore::in_memory().unwrap();
        let skill_id = make_skill(&store).await;

        let mut ok = input("deploy the dashboard", OutcomeStatus::Success);
        ok.task_type = Some("deploy".into());
        let recorded = record_outcome(&store, ok, None).await.unwrap();

        assert!(recorded.flagged_skills.is_empty());
        let skill = store.get(&skill_id).await.unwrap();
        assert!(
            !skill
                .tags
                .iter()
                .any(|t| t == crate::models::NEEDS_REVISION_TAG)
        );
    }

    #[tokio::test]
    async fn test_failure_without_matching_trigger_flags_nothing() {
        let store = SqliteStore::in_memory().unwrap();
        let skill_id = make_skill(&store).await; // trigger: "deploy"

        let mut fail = input("refactor the auth module", OutcomeStatus::Failure);
        fail.cause = Some("broke tests".into());
        fail.task_type = Some("refactor".into());
        let recorded = record_outcome(&store, fail, None).await.unwrap();

        assert!(recorded.flagged_skills.is_empty());
        let skill = store.get(&skill_id).await.unwrap();
        assert!(
            !skill
                .tags
                .iter()
                .any(|t| t == crate::models::NEEDS_REVISION_TAG)
        );
    }

    fn success_input(task: &str, task_type: &str) -> OutcomeInput {
        let mut i = input(task, OutcomeStatus::Success);
        i.task_type = Some(task_type.to_string());
        i
    }

    #[tokio::test]
    async fn test_third_success_drafts_a_skill() {
        let store = SqliteStore::in_memory().unwrap();

        for (n, task) in [
            (1, "deploy the dashboard"),
            (2, "deploy the api"),
            (3, "deploy the worker"),
        ] {
            let recorded = record_outcome(&store, success_input(task, "deploy"), None)
                .await
                .unwrap();
            if n < 3 {
                assert!(
                    recorded.skill_draft_id.is_none(),
                    "no draft before the threshold"
                );
            } else {
                let draft_id = recorded
                    .skill_draft_id
                    .as_ref()
                    .expect("draft at the threshold");
                let draft = store.get(draft_id).await.unwrap();
                assert_eq!(draft.memory_type, MemoryType::Skill);
                assert!(
                    draft
                        .tags
                        .iter()
                        .any(|t| t == crate::models::SKILL_DRAFT_TAG)
                );
                assert!(draft.tags.iter().any(|t| t == "deploy"));
                assert!(!draft.human_reviewed, "draft must enter the review queue");
                let meta = draft.skill_meta.as_ref().expect("draft has meta");
                assert_eq!(meta.trigger.as_deref(), Some("deploy"));
                assert_eq!(meta.steps.len(), 3, "steps seeded from the successes");
            }
        }
    }

    #[tokio::test]
    async fn test_draft_created_only_once_per_task_type() {
        let store = SqliteStore::in_memory().unwrap();
        for task in [
            "deploy a", "deploy b", "deploy c",
            "deploy d", // 4th success — draft already exists
        ] {
            record_outcome(&store, success_input(task, "deploy"), None)
                .await
                .unwrap();
        }
        let fourth = record_outcome(&store, success_input("deploy e", "deploy"), None)
            .await
            .unwrap();
        assert!(
            fourth.skill_draft_id.is_none(),
            "no duplicate draft once one exists"
        );
    }

    #[tokio::test]
    async fn test_no_draft_without_task_type() {
        let store = SqliteStore::in_memory().unwrap();
        for task in ["task a", "task b", "task c"] {
            let recorded = record_outcome(&store, input(task, OutcomeStatus::Success), None)
                .await
                .unwrap();
            assert!(recorded.skill_draft_id.is_none());
        }
    }

    #[tokio::test]
    async fn test_same_session_identical_outcome_not_recorded_twice() {
        // Regression: a Stop hook fires once per assistant turn and derives
        // the session's task from the same opening turn, so without the
        // guard the identical outcome is appended on every Stop.
        let store = SqliteStore::in_memory().unwrap();
        let mut outcome = input("deploy the api", OutcomeStatus::Failure);
        outcome.cause = Some("missing env var".into());
        outcome.task_type = Some("deploy".into());

        let first = record_outcome(&store, outcome.clone(), None).await.unwrap();
        assert!(
            first.duplicate_of.is_none(),
            "first record is not a duplicate"
        );

        let again = record_outcome(&store, outcome.clone(), None).await.unwrap();
        assert_eq!(
            again.duplicate_of.as_deref(),
            Some(first.memory.id.as_str()),
            "the second identical record must point back at the first"
        );
        assert_eq!(again.memory.id, first.memory.id);
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 1);
        assert_eq!(
            store
                .list_episodes(EpisodeFilter::default())
                .await
                .unwrap()
                .len(),
            1,
            "exactly one episode row, not two"
        );
    }

    #[tokio::test]
    async fn test_same_session_distinct_outcomes_both_recorded() {
        // The guard is scoped to an *identical* outcome — genuinely different
        // outcomes in one session still record.
        let store = SqliteStore::in_memory().unwrap();
        let a = record_outcome(
            &store,
            input("deploy the api", OutcomeStatus::Failure),
            None,
        )
        .await
        .unwrap();
        let b = record_outcome(
            &store,
            input("deploy the worker", OutcomeStatus::Failure),
            None,
        )
        .await
        .unwrap();
        assert!(a.duplicate_of.is_none());
        assert!(b.duplicate_of.is_none());
        assert_ne!(a.memory.id, b.memory.id);
        assert_eq!(
            store
                .list_episodes(EpisodeFilter::default())
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn test_counter_only_cause_change_folds_into_existing_episode() {
        // Regression: the rule floor embeds the session's cumulative friction
        // counters in the cause, so a long session re-inferred on every Stop
        // recorded one episode per Stop — 16 rows for a single Codex session.
        let store = SqliteStore::in_memory().unwrap();

        let mut first = input("review the plugin", OutcomeStatus::Partial);
        first.cause = Some(
            "session had friction before ending cleanly (retry×0, rejection×0, correction×1)"
                .into(),
        );
        let a = record_outcome(&store, first, None).await.unwrap();
        assert!(a.duplicate_of.is_none());

        let mut later = input("review the plugin", OutcomeStatus::Partial);
        later.cause = Some(
            "session had friction before ending cleanly (retry×1, rejection×1, correction×8)"
                .into(),
        );
        let b = record_outcome(&store, later, None).await.unwrap();
        assert_eq!(
            b.duplicate_of.as_deref(),
            Some(a.memory.id.as_str()),
            "a counter-only cause change must fold into the existing episode"
        );
        assert_eq!(
            store
                .list_episodes(EpisodeFilter::default())
                .await
                .unwrap()
                .len(),
            1,
            "exactly one episode row, not one per Stop"
        );
    }

    #[tokio::test]
    async fn test_same_session_different_cause_still_records() {
        // Normalization must not over-merge: a genuinely different cause in the
        // same session for the same task is still a different outcome.
        let store = SqliteStore::in_memory().unwrap();

        let mut first = input("deploy the api", OutcomeStatus::Failure);
        first.cause = Some("missing env var".into());
        let a = record_outcome(&store, first, None).await.unwrap();

        let mut second = input("deploy the api", OutcomeStatus::Failure);
        second.cause = Some("port already in use".into());
        let b = record_outcome(&store, second, None).await.unwrap();

        assert!(a.duplicate_of.is_none());
        assert!(
            b.duplicate_of.is_none(),
            "a different cause is a different outcome"
        );
        assert_ne!(a.memory.id, b.memory.id);
    }

    #[tokio::test]
    async fn test_different_sessions_record_same_outcome_separately() {
        let store = SqliteStore::in_memory().unwrap();
        let mut a = input("deploy the api", OutcomeStatus::Failure);
        a.source_agent.session_id = Some("sess_a".into());
        let mut b = input("deploy the api", OutcomeStatus::Failure);
        b.source_agent.session_id = Some("sess_b".into());

        let first = record_outcome(&store, a, None).await.unwrap();
        let second = record_outcome(&store, b, None).await.unwrap();
        assert!(second.duplicate_of.is_none());
        assert_ne!(first.memory.id, second.memory.id);
    }

    #[tokio::test]
    async fn test_sessionless_outcomes_keep_appending() {
        // Manual callers (no session id) keep the historical append behavior;
        // only session-tagged outcomes are deduped.
        let store = SqliteStore::in_memory().unwrap();
        let mut outcome = input("deploy the api", OutcomeStatus::Failure);
        outcome.source_agent.session_id = None;

        let first = record_outcome(&store, outcome.clone(), None).await.unwrap();
        let second = record_outcome(&store, outcome, None).await.unwrap();
        assert!(second.duplicate_of.is_none());
        assert_ne!(first.memory.id, second.memory.id);
    }
}
