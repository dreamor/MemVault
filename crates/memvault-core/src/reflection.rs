//! Episodic reflection: distill lessons from tasks that did not fully
//! succeed, and persist them as instruction-form memories linked back to
//! the episode.
//!
//! Flow: `record_outcome` (episode.rs) → reflection here → lesson memory
//! (REFERENCE, instruction form, review-required) + `episodes.lesson`
//! backlink. Success outcomes reflect nothing — only failure/partial do.

use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::llm_extractor::LlmExtractor;
use crate::models::{Memory, MemoryType, OutcomeStatus, Priority, SourceAgent};
use crate::storage::MemoryStore;
use tracing::warn;

/// Confidence for lesson memories. Outcomes are agent self-reports and the
/// lesson is machine-generated, so it starts below the 0.8 default trust
/// and must pass human review before anyone relies on it.
pub const LESSON_CONFIDENCE: f64 = 0.6;

/// Same-type failures with lessons reaching this count trigger an escalation
/// hint: the lesson should be considered for MUST priority — but only a human
/// may make that call (never auto-promoted).
pub const LESSON_ESCALATION_THRESHOLD: usize = 2;

/// Input for reflecting on one recorded outcome.
#[derive(Debug, Clone)]
pub struct LessonInput {
    pub task: String,
    pub task_type: Option<String>,
    pub status: OutcomeStatus,
    pub cause: Option<String>,
    pub namespace: String,
    pub source_agent: SourceAgent,
}

/// Where a lesson came from: the LLM reflection path or the conservative
/// rule fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonSource {
    Llm,
    Rule,
}

impl From<crate::episode::OutcomeInput> for LessonInput {
    fn from(o: crate::episode::OutcomeInput) -> Self {
        Self {
            task: o.task,
            task_type: o.task_type,
            status: o.status,
            cause: o.cause,
            namespace: o.namespace,
            source_agent: o.source_agent,
        }
    }
}

/// A distilled lesson plus the instruction-form memory created from it.
#[derive(Debug)]
pub struct LessonRecord {
    pub lesson: String,
    pub source: LessonSource,
    pub lesson_memory: Memory,
    /// Present when repeated same-type failures suggest the lesson deserves
    /// MUST priority. Promotion always requires human confirmation — this is
    /// a nudge, never an action.
    pub escalation_hint: Option<String>,
}

/// The report handed to the LLM for reflection. Deliberately flat text:
/// the model is told this is data, and the reflection prompt carries the
/// injection-defense stance.
fn reflection_context(input: &LessonInput) -> String {
    let mut ctx = format!("Task: {}\nStatus: {}", input.task, input.status.as_str());
    if let Some(ref tt) = input.task_type {
        ctx.push_str(&format!("\nCategory: {tt}"));
    }
    if let Some(ref cause) = input.cause {
        ctx.push_str(&format!("\nCause: {cause}"));
    }
    ctx
}

/// Rule-based fallback: a lesson strictly rooted in the stated cause —
/// nothing invented. No cause means no lesson (an LLM-less deployment must
/// never fabricate advice).
fn rule_lesson(input: &LessonInput) -> Option<String> {
    let cause = input.cause.as_deref()?.trim();
    if cause.is_empty() {
        return None;
    }
    Some(match &input.task_type {
        Some(tt) => format!("Before '{}' tasks, verify: {}", tt, cause),
        None => format!("Verify before retrying: {}", cause),
    })
}

/// Attempt reflection: LLM path first when available, conservative rule
/// fallback otherwise. Returns `None` when nothing actionable follows —
/// success outcomes, or failures without a cause and without an LLM.
pub async fn reflect_lesson(
    llm: Option<&dyn LlmExtractor>,
    input: &LessonInput,
) -> Option<(String, LessonSource)> {
    if input.status == OutcomeStatus::Success {
        return None;
    }

    if let Some(llm) = llm {
        match llm.reflect_lesson(&reflection_context(input)).await {
            Ok(Some(lesson)) => return Some((lesson, LessonSource::Llm)),
            Ok(None) => {}
            Err(e) => warn!("LLM reflection failed, falling back to rules: {}", e),
        }
    }

    rule_lesson(input).map(|l| (l, LessonSource::Rule))
}

fn lesson_tags(input: &LessonInput) -> Vec<String> {
    let mut tags = vec!["lesson".to_string()];
    if let Some(ref tt) = input.task_type {
        tags.push(tt.clone());
    }
    tags.push(input.status.as_str().to_string());
    tags
}

/// Reflect on a recorded episode and persist the results: the lesson text on
/// the episode record, plus a new instruction-form memory scoped to the task
/// type.
///
/// The lesson memory is created with default review flags
/// (`ai_generated=true`, `human_reviewed=false`) so it lands in the review
/// queue — agent self-reported failures must not silently become trusted
/// instructions.
pub async fn reflect_and_store(
    store: &impl MemoryStore,
    episode_memory_id: &str,
    input: LessonInput,
    llm: Option<&dyn LlmExtractor>,
    embedder: Option<&dyn EmbeddingProvider>,
) -> Result<Option<LessonRecord>> {
    let Some((lesson, source)) = reflect_lesson(llm, &input).await else {
        return Ok(None);
    };

    // Instruction form: imperative, scoped to the task type when known.
    let instruction = match &input.task_type {
        Some(tt) => format!("When working on '{}' tasks: {}", tt, lesson),
        None => lesson.clone(),
    };

    let mut mem = Memory::new(
        MemoryType::Fact,
        format!("Lesson from failed task '{}': {}", input.task, lesson),
        Priority::Reference,
        input.source_agent.clone(),
    );
    mem.namespace = input.namespace.clone();
    mem.instruction = Some(instruction.clone());
    mem.tags = lesson_tags(&input);
    mem.confidence = LESSON_CONFIDENCE;

    let saved = if let Some(embedder) = embedder {
        match embedder.embed(&[instruction]).await {
            Ok(embeddings) if !embeddings.is_empty() => {
                store
                    .save_with_embedding(mem, embeddings.into_iter().next().unwrap())
                    .await?
            }
            Ok(_) => store.save(mem).await?,
            Err(e) => {
                warn!("Lesson embedding failed, saving without: {}", e);
                store.save(mem).await?
            }
        }
    } else {
        store.save(mem).await?
    };

    if let Err(e) = store
        .update_episode_lesson(episode_memory_id, &lesson, Some(&saved.id))
        .await
    {
        // Best-effort cleanup: without the backlink the lesson floats free
        // of the episode that produced it.
        if let Err(cleanup_err) = store.delete(&saved.id).await {
            warn!(
                id = %saved.id,
                error = %cleanup_err,
                "failed to roll back orphan lesson memory"
            );
        }
        return Err(e);
    }

    // Escalation nudge: repeated same-type failures with lessons suggest the
    // lesson deserves MUST priority. Count failures of this task_type that
    // already carry a lesson; reaching the threshold only HINTS — promoting a
    // machine-generated lesson to a mandatory rule is a human decision.
    let escalation_hint = match &input.task_type {
        Some(tt) => {
            let filter = crate::models::EpisodeFilter {
                task_type: Some(tt.clone()),
                status: Some(OutcomeStatus::Failure),
                namespace: Some(input.namespace.clone()),
                limit: 200,
            };
            match store.list_episodes(filter).await {
                Ok(episodes) => {
                    let with_lesson = episodes.iter().filter(|e| e.lesson.is_some()).count();
                    if with_lesson >= LESSON_ESCALATION_THRESHOLD {
                        Some(format!(
                            "{} failures of type '{}' now carry lessons — consider promoting a lesson to MUST priority (human review required)",
                            with_lesson, tt
                        ))
                    } else {
                        None
                    }
                }
                Err(e) => {
                    warn!(error = %e, "escalation check failed; lesson kept");
                    None
                }
            }
        }
        None => None,
    };

    Ok(Some(LessonRecord {
        lesson,
        source,
        lesson_memory: saved,
        escalation_hint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MemVaultError;
    use crate::extractor::ExtractedMemory;
    use crate::storage::sqlite::SqliteStore;
    use async_trait::async_trait;

    struct StubLlm {
        lesson: Result<Option<String>>,
    }

    #[async_trait]
    impl LlmExtractor for StubLlm {
        async fn extract(&self, _context: &str) -> Result<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }
        async fn reflect_lesson(&self, _context: &str) -> Result<Option<String>> {
            match &self.lesson {
                Ok(l) => Ok(l.clone()),
                Err(e) => Err(MemVaultError::LlmExtraction(e.to_string())),
            }
        }
    }

    fn input(status: OutcomeStatus, cause: Option<&str>) -> LessonInput {
        LessonInput {
            task: "deploy the dashboard".into(),
            task_type: Some("deploy".into()),
            status,
            cause: cause.map(str::to_string),
            namespace: "global".into(),
            source_agent: SourceAgent {
                id: "tester".into(),
                agent_type: "coding-assistant".into(),
                session_id: None,
            },
        }
    }

    async fn save_episode(store: &SqliteStore, task: &str) -> String {
        let recorded = crate::episode::record_outcome(
            store,
            crate::episode::OutcomeInput {
                task: task.into(),
                status: OutcomeStatus::Failure,
                cause: None,
                task_type: Some("deploy".into()),
                skill_id: None,
                tags: Vec::new(),
                namespace: "global".into(),
                source_agent: SourceAgent {
                    id: "tester".into(),
                    agent_type: "coding-assistant".into(),
                    session_id: None,
                },
            },
            None,
        )
        .await
        .unwrap();
        recorded.memory.id
    }

    #[tokio::test]
    async fn test_success_never_reflects() {
        let input = input(OutcomeStatus::Success, Some("ignored"));
        assert!(reflect_lesson(None, &input).await.is_none());
    }

    #[tokio::test]
    async fn test_rule_lesson_from_cause() {
        let input = input(OutcomeStatus::Failure, Some("missing env var"));
        let (lesson, source) = reflect_lesson(None, &input).await.unwrap();
        assert_eq!(source, LessonSource::Rule);
        assert!(lesson.contains("missing env var"));
        assert!(lesson.contains("deploy"));
    }

    #[tokio::test]
    async fn test_rule_lesson_requires_cause() {
        let input = input(OutcomeStatus::Failure, None);
        assert!(reflect_lesson(None, &input).await.is_none());
    }

    #[tokio::test]
    async fn test_llm_lesson_preferred_over_rule() {
        let stub = StubLlm {
            lesson: Ok(Some("Always run migrations before deploy".into())),
        };
        let input = input(OutcomeStatus::Failure, Some("missing env var"));
        let (lesson, source) = reflect_lesson(Some(&stub), &input).await.unwrap();
        assert_eq!(source, LessonSource::Llm);
        assert_eq!(lesson, "Always run migrations before deploy");
    }

    #[tokio::test]
    async fn test_llm_error_falls_back_to_rule() {
        let stub = StubLlm {
            lesson: Err(MemVaultError::LlmExtraction("down".into())),
        };
        let input = input(OutcomeStatus::Failure, Some("disk full"));
        let (lesson, source) = reflect_lesson(Some(&stub), &input).await.unwrap();
        assert_eq!(source, LessonSource::Rule);
        assert!(lesson.contains("disk full"));
    }

    #[tokio::test]
    async fn test_llm_none_result_falls_back_to_rule() {
        let stub = StubLlm { lesson: Ok(None) };
        let input = input(OutcomeStatus::Failure, Some("disk full"));
        let (lesson, source) = reflect_lesson(Some(&stub), &input).await.unwrap();
        assert_eq!(source, LessonSource::Rule);
        assert!(lesson.contains("disk full"));
    }

    #[tokio::test]
    async fn test_reflect_and_store_links_lesson_to_episode() {
        let store = SqliteStore::in_memory().unwrap();
        let episode_id = save_episode(&store, "deploy the dashboard").await;

        let input = input(OutcomeStatus::Failure, Some("missing env var"));
        let record = reflect_and_store(&store, &episode_id, input, None, None)
            .await
            .unwrap()
            .expect("failure with cause must produce a lesson");

        assert_eq!(record.source, LessonSource::Rule);
        assert!(
            record
                .lesson_memory
                .content
                .contains("Lesson from failed task")
        );
        assert!(record.lesson_memory.instruction.is_some());
        assert!(
            record
                .lesson_memory
                .instruction
                .unwrap()
                .contains("'deploy' tasks")
        );
        assert_eq!(record.lesson_memory.confidence, LESSON_CONFIDENCE);
        assert!(!record.lesson_memory.human_reviewed, "needs review");
        assert!(record.lesson_memory.tags.contains(&"lesson".to_string()));

        let episode = store.get_episode(&episode_id).await.unwrap();
        assert_eq!(episode.lesson.as_deref(), Some(record.lesson.as_str()));
        assert_eq!(
            episode.lesson_memory_id.as_deref(),
            Some(record.lesson_memory.id.as_str())
        );
    }

    #[tokio::test]
    async fn test_reflect_and_store_no_cause_no_llm_yields_nothing() {
        let store = SqliteStore::in_memory().unwrap();
        let episode_id = save_episode(&store, "deploy the dashboard").await;

        let input = input(OutcomeStatus::Failure, None);
        let record = reflect_and_store(&store, &episode_id, input, None, None)
            .await
            .unwrap();
        assert!(record.is_none());

        let episode = store.get_episode(&episode_id).await.unwrap();
        assert!(episode.lesson.is_none());
    }

    #[tokio::test]
    async fn test_reflect_and_store_missing_episode_errors() {
        let store = SqliteStore::in_memory().unwrap();
        let input = input(OutcomeStatus::Failure, Some("cause"));
        let err = reflect_and_store(&store, "mem_missing", input, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, MemVaultError::NotFound(_)));
    }

    async fn record_failure(store: &SqliteStore, task_type: &str, idx: usize) -> String {
        let recorded = crate::episode::record_outcome(
            store,
            crate::episode::OutcomeInput {
                task: format!("task {idx}"),
                status: OutcomeStatus::Failure,
                cause: Some(format!("cause {idx}")),
                task_type: Some(task_type.to_string()),
                skill_id: None,
                tags: Vec::new(),
                namespace: "global".to_string(),
                source_agent: SourceAgent {
                    id: "tester".to_string(),
                    agent_type: "coding-assistant".to_string(),
                    session_id: None,
                },
            },
            None,
        )
        .await
        .unwrap();
        recorded.memory.id
    }

    #[tokio::test]
    async fn test_escalation_hint_after_repeated_failures() {
        let store = SqliteStore::in_memory().unwrap();

        // First failure: no hint yet.
        let ep1 = record_failure(&store, "deploy", 1).await;
        let input1 = LessonInput {
            task: "task 1".to_string(),
            task_type: Some("deploy".to_string()),
            status: OutcomeStatus::Failure,
            cause: Some("cause 1".to_string()),
            namespace: "global".to_string(),
            source_agent: SourceAgent {
                id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        };
        let first = reflect_and_store(&store, &ep1, input1, None, None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            first.escalation_hint.is_none(),
            "a single failure must not escalate"
        );

        // Second failure of the same type: hint appears.
        let ep2 = record_failure(&store, "deploy", 2).await;
        let input2 = LessonInput {
            task: "task 2".to_string(),
            task_type: Some("deploy".to_string()),
            status: OutcomeStatus::Failure,
            cause: Some("cause 2".to_string()),
            namespace: "global".to_string(),
            source_agent: SourceAgent {
                id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        };
        let second = reflect_and_store(&store, &ep2, input2, None, None)
            .await
            .unwrap()
            .unwrap();
        let hint = second
            .escalation_hint
            .expect("second failure must escalate");
        assert!(hint.contains("MUST"));
        assert!(hint.contains("deploy"));
    }

    #[test]
    fn test_reflection_context_shape() {
        let input = input(OutcomeStatus::Failure, Some("missing env var"));
        let ctx = reflection_context(&input);
        assert!(ctx.contains("Task: deploy the dashboard"));
        assert!(ctx.contains("Status: failure"));
        assert!(ctx.contains("Category: deploy"));
        assert!(ctx.contains("Cause: missing env var"));
    }
}
