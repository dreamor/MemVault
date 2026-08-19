use std::sync::Arc;

use tracing::{debug, info, warn};

use memvault_core::extractor::Extractor;
use memvault_core::models::*;
use memvault_core::storage::MemoryStore;

/// What to do with agent-produced text on the assistant extraction path.
///
/// Agent output fed back into memory unchecked causes self-reinforcing
/// drift (the store converges on the model's own phrasing and errors), so
/// even the permissive default keeps such memories in a review-pending
/// state instead of trusting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssistantExtractionPolicy {
    /// Save, but downgraded: tagged `review:required` with lowered
    /// confidence. Default for backwards compatibility — extraction stays
    /// on, but nothing agent-produced is trusted before human review.
    #[default]
    Downgraded,
    /// Do not save agent-produced extractions at all
    /// (`MEMVAULT_EXTRACT_ASSISTANT=off`).
    Disabled,
}

pub struct ExtractionConfig {
    /// Only extract memories with confidence >= this threshold
    pub min_confidence: f64,
    /// Maximum memories to extract per response
    pub max_per_response: usize,
    /// Types allowed for extraction (whitelist)
    pub allowed_types: Vec<MemoryType>,
    /// Treatment of agent-produced text (see type docs).
    pub assistant_policy: AssistantExtractionPolicy,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.6,
            max_per_response: 5,
            allowed_types: vec![MemoryType::Preference, MemoryType::Fact, MemoryType::Skill],
            assistant_policy: AssistantExtractionPolicy::default(),
        }
    }
}

impl ExtractionConfig {
    /// Default config with environment overrides:
    /// `MEMVAULT_EXTRACT_ASSISTANT=off|disabled|false|0` disables extracting
    /// from agent responses entirely.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("MEMVAULT_EXTRACT_ASSISTANT") {
            if matches!(v.to_ascii_lowercase().as_str(), "off" | "disabled" | "false" | "0") {
                cfg.assistant_policy = AssistantExtractionPolicy::Disabled;
            }
        }
        cfg
    }
}

pub struct ExtractionResult {
    pub extracted: usize,
    pub saved: usize,
    pub skipped: usize,
}

pub struct ResponseExtractor {
    store: Arc<dyn MemoryStore>,
    config: ExtractionConfig,
}

impl ResponseExtractor {
    pub fn new(store: Arc<dyn MemoryStore>, config: ExtractionConfig) -> Self {
        Self { store, config }
    }

    /// Extract memories from an agent's own response text and save to store as unreviewed (Inbox).
    pub async fn extract_and_save(&self, response_text: &str, agent_id: &str) -> ExtractionResult {
        self.extract_and_save_labeled(response_text, agent_id, "assistant")
            .await
    }

    /// Extract memories directly from the user's own turn text. First-person
    /// signal words ("我偏好"/"我喜欢") match a user's own statements about
    /// themselves far more reliably than an assistant's restatement of them,
    /// so this path typically has higher recall than `extract_and_save`.
    pub async fn extract_and_save_from_user(
        &self,
        user_text: &str,
        agent_id: &str,
    ) -> ExtractionResult {
        self.extract_and_save_labeled(user_text, agent_id, "user")
            .await
    }

    async fn extract_and_save_labeled(
        &self,
        text: &str,
        agent_id: &str,
        source: &str,
    ) -> ExtractionResult {
        // Source-role guard: user turns extract normally; agent-produced
        // text is either refused outright (Disabled) or routed through the
        // untrusted-provenance path (Downgraded → review:required + lowered
        // confidence), never through the trusted one.
        let role = if source == "user" {
            memvault_core::extractor::SourceRole::User
        } else {
            match self.config.assistant_policy {
                AssistantExtractionPolicy::Disabled => {
                    memvault_core::extractor::SourceRole::Agent
                }
                AssistantExtractionPolicy::Downgraded => {
                    memvault_core::extractor::SourceRole::Mixed
                }
            }
        };

        let guarded = Extractor::extract_guarded(text, role);
        if let Some(reason) = guarded.rejection {
            info!(
                agent_id,
                source,
                reason = ?reason,
                "extraction refused by source-role guard"
            );
            return ExtractionResult {
                extracted: 0,
                saved: 0,
                skipped: 0,
            };
        }
        let extracted = guarded.outcome.memories;

        if extracted.is_empty() {
            return ExtractionResult {
                extracted: 0,
                saved: 0,
                skipped: 0,
            };
        }

        let mut saved = 0;
        let mut skipped = 0;

        for e in extracted.iter().take(self.config.max_per_response) {
            if e.confidence < self.config.min_confidence {
                skipped += 1;
                continue;
            }
            if !self.config.allowed_types.contains(&e.memory_type) {
                skipped += 1;
                continue;
            }

            let mut mem = Memory::new(
                e.memory_type.clone(),
                e.content.clone(),
                e.priority.clone(),
                SourceAgent {
                    id: agent_id.to_string(),
                    agent_type: "extraction-pipeline".to_string(),
                    session_id: None,
                },
            );
            mem.instruction = e.instruction.clone();
            mem.tags = e.tags.clone();
            mem.tags.push(format!("source:{source}"));
            mem.confidence = e.confidence;
            mem.ai_generated = true;
            mem.human_reviewed = false;
            mem.layer = match e.priority {
                Priority::Must => MemoryLayer::L1,
                _ => MemoryLayer::L1,
            };

            match self.store.save(mem).await {
                Ok(m) => {
                    debug!(id = %m.id, "extraction saved to inbox");
                    saved += 1;
                }
                Err(err) => {
                    warn!(error = %err, "extraction save failed");
                    skipped += 1;
                }
            }
        }

        info!(
            extracted = extracted.len(),
            saved, skipped, agent_id, source, "extraction complete"
        );

        ExtractionResult {
            extracted: extracted.len(),
            saved,
            skipped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memvault_core::storage::sqlite::SqliteStore;

    #[tokio::test]
    async fn test_extract_from_preference_response() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        // User-facing text that the extractor can pick up:
        let user_text = "I prefer Python over Java. Our project uses FastAPI.";

        let result = extractor.extract_and_save(user_text, "test-agent").await;
        assert!(result.extracted >= 1);
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(!all.is_empty());
        assert!(!all[0].human_reviewed);
    }

    #[tokio::test]
    async fn test_extract_nothing_from_irrelevant() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        let text = "The weather is nice today. Let me help you with that code.";
        let result = extractor.extract_and_save(text, "test-agent").await;
        assert_eq!(result.extracted, 0);
        assert_eq!(result.saved, 0);
    }

    #[tokio::test]
    async fn test_confidence_threshold() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let config = ExtractionConfig {
            min_confidence: 0.99, // very high threshold
            ..ExtractionConfig::default()
        };
        let extractor = ResponseExtractor::new(store.clone(), config);

        let text = "I prefer dark mode for all editors";
        let result = extractor.extract_and_save(text, "test-agent").await;
        // Extracted but not saved due to high threshold
        assert!(result.extracted >= 1);
        assert_eq!(result.saved, 0);
    }

    #[tokio::test]
    async fn test_max_per_response_limit() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let config = ExtractionConfig {
            max_per_response: 1,
            ..ExtractionConfig::default()
        };
        let extractor = ResponseExtractor::new(store.clone(), config);

        let text = "I prefer Python\nI always use dark mode\nI never add comments";
        let result = extractor.extract_and_save(text, "test-agent").await;
        assert!(result.extracted >= 2);
        assert_eq!(result.saved, 1);
    }

    #[tokio::test]
    async fn test_extract_and_save_tags_source_assistant() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        let result = extractor
            .extract_and_save("I prefer dark mode", "test-agent")
            .await;
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter()
                .any(|m| m.tags.contains(&"source:assistant".to_string()))
        );
    }

    #[tokio::test]
    async fn test_extract_and_save_from_user_tags_source_user() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        let result = extractor
            .extract_and_save_from_user("我偏好使用 tabs 缩进", "test-agent")
            .await;
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter()
                .any(|m| m.tags.contains(&"source:user".to_string()))
        );
        // User text is trusted provenance: no review-required downgrade.
        assert!(
            all.iter()
                .all(|m| !m.tags.contains(&"review:required".to_string()))
        );
    }

    /// Default assistant policy keeps extraction on but nothing it saves is
    /// trusted: every memory carries review:required.
    #[tokio::test]
    async fn test_assistant_extraction_default_is_downgraded() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        let result = extractor
            .extract_and_save("I prefer dark mode", "test-agent")
            .await;
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter().all(|m| m
                .tags
                .contains(&"review:required".to_string())),
            "agent-produced memories must wait for human review"
        );
    }

    /// With the Disabled policy, agent output never reaches the store — the
    /// guard refuses it before any save.
    #[tokio::test]
    async fn test_assistant_extraction_disabled_saves_nothing() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let config = ExtractionConfig {
            assistant_policy: AssistantExtractionPolicy::Disabled,
            ..ExtractionConfig::default()
        };
        let extractor = ResponseExtractor::new(store.clone(), config);

        let result = extractor
            .extract_and_save("I prefer dark mode", "test-agent")
            .await;
        assert_eq!(result.saved, 0);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(all.is_empty());

        // The user path is unaffected by the assistant policy.
        let user_result = extractor
            .extract_and_save_from_user("I prefer dark mode", "test-agent")
            .await;
        assert!(user_result.saved >= 1);
    }
}
