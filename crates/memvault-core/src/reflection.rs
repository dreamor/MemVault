//! Episodic reflection: distill lessons from tasks that did not fully
//! succeed, and persist them as instruction-form memories linked back to
//! the episode.
//!
//! Flow: `record_outcome` (episode.rs) → reflection here → lesson memory
//! (REFERENCE, instruction form, review-required) + `episodes.lesson`
//! backlink. Success outcomes reflect nothing — only failure/partial do.

use crate::dedup::Deduplicator;
use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::evidence::{EvidenceKind, add_evidence};
use crate::llm_extractor::LlmExtractor;
use crate::models::{EpisodeRecord, Memory, MemoryType, OutcomeStatus, Priority, SourceAgent};
use crate::storage::MemoryStore;
use tracing::warn;

/// Confidence for lesson memories. Outcomes are agent self-reports and the
/// lesson is machine-generated, so it starts below the 0.8 default trust
/// and must pass human review before anyone relies on it.
pub const LESSON_CONFIDENCE: f64 = 0.6;

/// Recurrences (failures whose `cause` text matches an existing lesson's
/// `cause`, see [`is_recurrence`]) reaching this count trigger an escalation
/// hint: the lesson should be considered for MUST priority — but only a human
/// may make that call (never auto-promoted). Also gates the recurrence
/// update proposal (see [`propose_lesson_update`]) — both nudges fire off
/// the same "this keeps happening" signal.
pub const LESSON_ESCALATION_THRESHOLD: usize = 2;

/// Word-overlap (Jaccard) similarity a new failure's cause must reach
/// against a past failure's cause to count as the SAME recurring problem,
/// rather than merely another failure of the same task_type. Lower than
/// [`crate::dedup::Deduplicator`]'s 0.7 near-duplicate-memory bar — failure
/// causes recur with much more varied wording than near-identical memory
/// content, so a stricter bar would miss real recurrences. First-cut
/// heuristic; no historical data yet to calibrate against (see
/// docs/FRICTION-GATED-EXTRACTION-PLAN.md §12).
pub const RECURRENCE_SIMILARITY_THRESHOLD: f32 = 0.3;

/// Whether `new_cause` describes the same underlying problem as
/// `past_cause` — word-overlap only, no LLM call, so escalation stays cheap
/// even when checked against up to 200 past episodes per failure recorded.
fn is_recurrence(new_cause: &str, past_cause: &str) -> bool {
    let a = Deduplicator::tokenize(new_cause);
    let b = Deduplicator::tokenize(past_cause);
    Deduplicator::jaccard_similarity(&a, &b) >= RECURRENCE_SIMILARITY_THRESHOLD
}

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
    /// Present when a recurrence against an existing lesson produced an LLM
    /// proposal for how that lesson should change — a new unreviewed draft
    /// linked to the old lesson via a `contradicts` relation, never applied
    /// automatically. `None` whenever no LLM was configured, no recurrence
    /// matched, or the LLM judged the old lesson still correct.
    pub recurrence_update: Option<RecurrenceUpdate>,
}

/// A proposed revision to an existing lesson, produced when a new failure
/// recurs against it (see [`is_recurrence`]). Purely informational until a
/// human reviews the draft and runs `memvault supersede`.
#[derive(Debug)]
pub struct RecurrenceUpdate {
    /// The new unreviewed draft memory holding the LLM's proposed revision.
    pub draft_memory_id: String,
    /// The existing lesson memory the draft proposes to replace.
    pub old_lesson_memory_id: String,
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

    // Escalation nudge: repeated RECURRENCES of the same underlying problem
    // (not just any failure of this task_type) suggest the lesson deserves
    // MUST priority. Reaching the threshold only HINTS — promoting a
    // machine-generated lesson to a mandatory rule is a human decision.
    let new_cause = input.cause.as_deref().unwrap_or(&input.task);
    let mut best_match: Option<EpisodeRecord> = None;
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
                    let mut recurrence_count = 0usize;
                    for ep in episodes {
                        if ep.lesson.is_none() {
                            continue;
                        }
                        let past_cause = ep.cause.as_deref().unwrap_or(&ep.task);
                        if !is_recurrence(new_cause, past_cause) {
                            continue;
                        }
                        recurrence_count += 1;
                        if best_match.is_none() {
                            best_match = Some(ep);
                        }
                    }
                    if recurrence_count >= LESSON_ESCALATION_THRESHOLD {
                        Some(format!(
                            "{} recurrence(s) of a similar failure in '{}' now carry lessons — consider promoting a lesson to MUST priority (human review required)",
                            recurrence_count, tt
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

    // Recurrence update proposal: only once escalation-worthy AND an LLM is
    // configured — best-effort, mirrors crate::effectiveness's contract of
    // never fabricating a judgment when there's no model to make one.
    let mut recurrence_update = None;
    if let (true, Some(llm), Some(old_lesson), Some(old_lesson_id)) = (
        escalation_hint.is_some(),
        llm,
        best_match.as_ref().and_then(|ep| ep.lesson.as_ref()),
        best_match
            .as_ref()
            .and_then(|ep| ep.lesson_memory_id.as_ref()),
    ) {
        match propose_lesson_update(llm, old_lesson, &input, &lesson).await {
            Ok(Some(updated_text)) => {
                match save_recurrence_draft(store, &input, &updated_text, old_lesson_id).await {
                    Ok(update) => recurrence_update = Some(update),
                    Err(e) => warn!(error = %e, "failed to save recurrence update draft"),
                }
            }
            Ok(None) => {}
            Err(e) => warn!(error = %e, "recurrence update proposal failed"),
        }
    }

    Ok(Some(LessonRecord {
        lesson,
        source,
        lesson_memory: saved,
        escalation_hint,
        recurrence_update,
    }))
}

/// Ask the LLM whether a recurring failure means an existing lesson needs
/// updating. `Ok(None)` when the LLM has nothing to add (old lesson still
/// holds) or no LLM is configured — never fabricates a revision.
async fn propose_lesson_update(
    llm: &dyn LlmExtractor,
    old_lesson: &str,
    input: &LessonInput,
    new_lesson: &str,
) -> Result<Option<String>> {
    let system = "You maintain a lesson-learned knowledge base for an AI coding agent. A \
                  previously recorded lesson has recurred with a new, similar failure. Decide \
                  whether the old lesson needs updating to account for the new failure. If yes, \
                  reply with ONLY the full replacement lesson text (imperative, standalone, no \
                  preamble or markdown). If the old lesson already covers this case and needs no \
                  change, reply with exactly: NONE";
    let user = format!(
        "Old lesson: {old_lesson}\nNew failure task: {}\nNew failure cause: {}\nLesson just \
         reflected from this new failure: {new_lesson}",
        input.task,
        input.cause.as_deref().unwrap_or("(unknown)")
    );
    let Some(text) = llm.json_chat(system, &user).await? else {
        return Ok(None);
    };
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        Ok(Some(text.to_string()))
    }
}

/// Save the LLM's proposed lesson revision as a new unreviewed draft and
/// link it to the lesson it proposes to replace via a `contradicts`
/// relation — a human reviews the draft and runs `memvault supersede` to
/// actually apply it; nothing here mutates the old lesson.
async fn save_recurrence_draft(
    store: &impl MemoryStore,
    input: &LessonInput,
    updated_text: &str,
    old_lesson_id: &str,
) -> Result<RecurrenceUpdate> {
    let instruction = match &input.task_type {
        Some(tt) => format!("When working on '{}' tasks: {}", tt, updated_text),
        None => updated_text.to_string(),
    };
    let mut draft = Memory::new(
        MemoryType::Fact,
        format!("Updated lesson (recurrence): {}", updated_text),
        Priority::Reference,
        input.source_agent.clone(),
    );
    draft.namespace = input.namespace.clone();
    draft.instruction = Some(instruction);
    draft.tags = {
        let mut tags = lesson_tags(input);
        tags.push("lesson-update".to_string());
        tags
    };
    draft.confidence = LESSON_CONFIDENCE;
    let saved = store.save(draft).await?;

    add_evidence(
        store,
        &saved.id,
        EvidenceKind::Contradicts,
        Some(old_lesson_id),
        None,
        LESSON_CONFIDENCE,
    )
    .await?;

    Ok(RecurrenceUpdate {
        draft_memory_id: saved.id,
        old_lesson_memory_id: old_lesson_id.to_string(),
    })
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

    struct StubLlmFull {
        lesson: Option<String>,
        update: Option<String>,
    }

    #[async_trait]
    impl LlmExtractor for StubLlmFull {
        async fn extract(&self, _context: &str) -> Result<Vec<ExtractedMemory>> {
            Ok(Vec::new())
        }
        async fn reflect_lesson(&self, _context: &str) -> Result<Option<String>> {
            Ok(self.lesson.clone())
        }
        async fn json_chat(&self, _system: &str, _user: &str) -> Result<Option<String>> {
            Ok(self.update.clone())
        }
    }

    async fn record_failure_with_cause(
        store: &SqliteStore,
        task_type: &str,
        cause: &str,
    ) -> String {
        let recorded = crate::episode::record_outcome(
            store,
            crate::episode::OutcomeInput {
                task: format!("task with cause: {cause}"),
                status: OutcomeStatus::Failure,
                cause: Some(cause.to_string()),
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

    fn input_with_cause(task_type: &str, cause: &str) -> LessonInput {
        LessonInput {
            task: format!("task with cause: {cause}"),
            task_type: Some(task_type.to_string()),
            status: OutcomeStatus::Failure,
            cause: Some(cause.to_string()),
            namespace: "global".to_string(),
            source_agent: SourceAgent {
                id: "tester".to_string(),
                agent_type: "coding-assistant".to_string(),
                session_id: None,
            },
        }
    }

    #[tokio::test]
    async fn test_unrelated_causes_of_same_task_type_do_not_escalate() {
        // The bug §10.1 fixes: two failures of the same task_type used to
        // escalate regardless of whether they were the same underlying
        // problem. These two share nothing but the task_type.
        let store = SqliteStore::in_memory().unwrap();

        let ep1 = record_failure_with_cause(&store, "deploy", "disk space full").await;
        reflect_and_store(
            &store,
            &ep1,
            input_with_cause("deploy", "disk space full"),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        let ep2 = record_failure_with_cause(&store, "deploy", "authentication token expired").await;
        let second = reflect_and_store(
            &store,
            &ep2,
            input_with_cause("deploy", "authentication token expired"),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(
            second.escalation_hint.is_none(),
            "unrelated causes of the same task_type must not escalate"
        );
        assert!(second.recurrence_update.is_none());
    }

    #[tokio::test]
    async fn test_recurrence_escalates_and_proposes_lesson_update() {
        let store = SqliteStore::in_memory().unwrap();

        let ep1 = record_failure_with_cause(&store, "deploy", "disk space full").await;
        reflect_and_store(
            &store,
            &ep1,
            input_with_cause("deploy", "disk space full"),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        let stub = StubLlmFull {
            lesson: Some("Check disk space before deploy".to_string()),
            update: Some(
                "Check disk space and clear old build artifacts before deploy".to_string(),
            ),
        };
        let ep2 = record_failure_with_cause(&store, "deploy", "disk space is full again").await;
        let second = reflect_and_store(
            &store,
            &ep2,
            input_with_cause("deploy", "disk space is full again"),
            Some(&stub),
            None,
        )
        .await
        .unwrap()
        .unwrap();

        let hint = second
            .escalation_hint
            .expect("paraphrased recurrence of the same problem must escalate");
        assert!(hint.contains("MUST"));

        let update = second
            .recurrence_update
            .expect("LLM-configured recurrence must propose an update");
        let draft = store.get(&update.draft_memory_id).await.unwrap();
        assert!(
            !draft.human_reviewed,
            "update draft lands in the review inbox"
        );
        assert!(draft.tags.contains(&"lesson-update".to_string()));
        assert!(draft.content.contains("clear old build artifacts"));

        let summary = crate::evidence::evidence_summary(&store, &update.old_lesson_memory_id)
            .await
            .unwrap();
        assert_eq!(
            summary.contradicts, 1,
            "the draft must link back to the old lesson via a contradicts relation"
        );
    }

    #[tokio::test]
    async fn test_recurrence_without_llm_skips_update_proposal() {
        let store = SqliteStore::in_memory().unwrap();

        let ep1 = record_failure_with_cause(&store, "deploy", "disk space full").await;
        reflect_and_store(
            &store,
            &ep1,
            input_with_cause("deploy", "disk space full"),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        let ep2 = record_failure_with_cause(&store, "deploy", "disk space is full again").await;
        let second = reflect_and_store(
            &store,
            &ep2,
            input_with_cause("deploy", "disk space is full again"),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(
            second.escalation_hint.is_some(),
            "recurrence still escalates without an LLM"
        );
        assert!(
            second.recurrence_update.is_none(),
            "no LLM configured means no update is ever fabricated"
        );
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
