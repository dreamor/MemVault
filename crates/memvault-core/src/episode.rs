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
use crate::storage::MemoryStore;
use tracing::warn;

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
    store: &impl MemoryStore,
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
async fn maybe_draft_skill(store: &impl MemoryStore, input: &OutcomeInput) -> Option<String> {
    let task_type = input.task_type.as_deref()?;

    let filter = EpisodeFilter {
        task_type: Some(task_type.to_string()),
        status: Some(OutcomeStatus::Success),
        namespace: Some(input.namespace.clone()),
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

/// Record one task outcome: save the episode memory (embedding when an
/// embedder is available), then attach the structured record.
///
/// Episodes are L1 raw experience at BACKGROUND priority — promotion to
/// lessons/instructions happens in the reflection step, not here.
pub async fn record_outcome(
    store: &impl MemoryStore,
    input: OutcomeInput,
    embedder: Option<&dyn EmbeddingProvider>,
) -> Result<RecordedOutcome> {
    let mut mem = Memory::new(
        MemoryType::Episode,
        episode_content(&input),
        Priority::Background,
        input.source_agent.clone(),
    );
    mem.namespace = input.namespace.clone();
    mem.tags = episode_tags(&input);
    mem.layer = MemoryLayer::L1;

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
        }
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
}
