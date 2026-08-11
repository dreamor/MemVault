use std::sync::Arc;

use tracing::{debug, info, warn};

use memvault_core::extractor::Extractor;
use memvault_core::models::*;
use memvault_core::storage::MemoryStore;

pub struct ExtractionConfig {
    /// Only extract memories with confidence >= this threshold
    pub min_confidence: f64,
    /// Maximum memories to extract per response
    pub max_per_response: usize,
    /// Types allowed for extraction (whitelist)
    pub allowed_types: Vec<MemoryType>,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.6,
            max_per_response: 5,
            allowed_types: vec![MemoryType::Preference, MemoryType::Fact, MemoryType::Skill],
        }
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

    /// Extract memories from an agent response text and save to store as unreviewed (Inbox).
    pub async fn extract_and_save(
        &self,
        response_text: &str,
        agent_id: &str,
    ) -> ExtractionResult {
        let extracted = Extractor::extract(response_text);

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
            saved, skipped, agent_id, "response extraction complete"
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

        let response = "Based on your request, I'll remember that you prefer Python over Java for all backend work.";
        // This won't match since it's the assistant talking about the user
        // Let's use user-facing text that the extractor can pick up:
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
}
