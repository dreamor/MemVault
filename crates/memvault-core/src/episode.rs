//! Episodic memory: record structured task outcomes as episode memories.
//!
//! One recorded outcome = one episode `Memory` row (searchable content, tags,
//! namespace) plus one `episodes` row (task/status/cause/lesson). The two
//! are linked 1:1 by memory id; deleting the memory cascades the record.

use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::models::{Memory, MemoryLayer, MemoryType, OutcomeStatus, Priority, SourceAgent};
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
    pub tags: Vec<String>,
    pub namespace: String,
    pub source_agent: SourceAgent,
}

/// A recorded outcome: the saved episode memory and whether it got a vector.
#[derive(Debug, Clone)]
pub struct RecordedOutcome {
    pub memory: Memory,
    pub embedded: bool,
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

    Ok(RecordedOutcome {
        memory: saved,
        embedded,
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
}
