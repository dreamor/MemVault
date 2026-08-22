use std::sync::Arc;

use tracing::{debug, info, warn};

use memvault_core::extractor::{ExtractedMemory, Extractor};
use memvault_core::llm_extractor::LlmExtractor;
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
        if let Ok(v) = std::env::var("MEMVAULT_EXTRACT_ASSISTANT")
            && matches!(
                v.to_ascii_lowercase().as_str(),
                "off" | "disabled" | "false" | "0"
            )
        {
            cfg.assistant_policy = AssistantExtractionPolicy::Disabled;
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
    /// Optional contextual (LLM-based) extractor. When absent, extraction
    /// stays 100% rule-based — this mirrors the embedding provider's
    /// optional-provider shape, but with no bundled local fallback: an
    /// LLM call has real cost/latency/hallucination risk, so it must be an
    /// explicit opt-in via `MEMVAULT_LLM_EXTRACTION_PROVIDER`.
    llm_extractor: Option<Arc<dyn LlmExtractor>>,
}

impl ResponseExtractor {
    pub fn new(store: Arc<dyn MemoryStore>, config: ExtractionConfig) -> Self {
        Self {
            store,
            config,
            llm_extractor: None,
        }
    }

    pub fn with_llm_extractor(mut self, extractor: Arc<dyn LlmExtractor>) -> Self {
        self.llm_extractor = Some(extractor);
        self
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
        let role = self.role_for_source(source);

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

        let (saved, skipped) = self
            .save_extracted(&extracted, agent_id, source, "rule")
            .await;

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

    /// Source-role guard: user turns extract normally; agent-produced text
    /// is either refused outright (Disabled) or routed through the
    /// untrusted-provenance path (Downgraded → review:required + lowered
    /// confidence), never through the trusted one.
    fn role_for_source(&self, source: &str) -> memvault_core::extractor::SourceRole {
        if source == "user" {
            memvault_core::extractor::SourceRole::User
        } else {
            match self.config.assistant_policy {
                AssistantExtractionPolicy::Disabled => memvault_core::extractor::SourceRole::Agent,
                AssistantExtractionPolicy::Downgraded => {
                    memvault_core::extractor::SourceRole::Mixed
                }
            }
        }
    }

    /// Convert extracted memories into `Memory` rows and save them,
    /// applying the confidence/type filters. Any provenance downgrade
    /// (review:required + confidence discount) must already be baked into
    /// `extracted` by the caller — see [`Extractor::extract_guarded`] for
    /// the rule-based path and [`Self::downgrade_mixed_provenance`] for the
    /// LLM path — so this stays a pure convert-and-save step shared by
    /// both, instead of each path re-deriving its own persistence logic.
    async fn save_extracted(
        &self,
        extracted: &[ExtractedMemory],
        agent_id: &str,
        source: &str,
        method: &str,
    ) -> (usize, usize) {
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
            mem.tags.push(format!("method:{method}"));
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

        (saved, skipped)
    }

    /// Apply the same review-required downgrade that
    /// [`Extractor::extract_guarded`]'s `Mixed`/`Unknown` branch applies to
    /// rule-based extractions — but to LLM output, since the LLM path calls
    /// the extractor trait directly and never goes through that guard.
    /// "Provenance not confirmed as the user" must never be treated as
    /// "confirmed user", regardless of which extraction method produced it.
    fn downgrade_mixed_provenance(extracted: &mut [ExtractedMemory]) {
        for m in extracted {
            if !m.tags.iter().any(|t| t == "review:required") {
                m.tags.push("review:required".to_string());
            }
            m.confidence *= 0.9;
        }
    }

    /// Extract from a paired user+assistant turn as one piece of
    /// *context*, rather than scanning each text independently for
    /// keyword signals. When an LLM extractor is configured, it sees both
    /// sides of the exchange together — recovering implicit preferences
    /// and cross-sentence references the line-by-line rule engine misses.
    /// Falls back to the rule-based per-text extraction (unchanged
    /// behavior) when no LLM extractor is configured, or if the LLM call
    /// fails — an LLM outage must never break the extraction pipeline.
    pub async fn extract_and_save_contextual(
        &self,
        user_text: Option<&str>,
        response_text: &str,
        agent_id: &str,
    ) -> ExtractionResult {
        // Same per-source policy as the rule-based path: the assistant side
        // is dropped from consideration entirely when the policy is
        // Disabled, while the user side is never affected by that policy.
        let assistant_included = !response_text.trim().is_empty()
            && !matches!(
                self.role_for_source("assistant"),
                memvault_core::extractor::SourceRole::Agent
            );
        let user_included = user_text.is_some_and(|t| !t.trim().is_empty());

        if !assistant_included && !user_included {
            return ExtractionResult {
                extracted: 0,
                saved: 0,
                skipped: 0,
            };
        }

        if let Some(llm) = &self.llm_extractor {
            let mut context = String::new();
            if user_included {
                context.push_str("User: ");
                context.push_str(user_text.unwrap());
                context.push('\n');
            }
            if assistant_included {
                context.push_str("Assistant: ");
                context.push_str(response_text);
            }

            match llm.extract(&context).await {
                Ok(mut extracted) if !extracted.is_empty() => {
                    let contextual_source = if assistant_included { "mixed" } else { "user" };
                    if contextual_source == "mixed" {
                        Self::downgrade_mixed_provenance(&mut extracted);
                    }
                    let (saved, skipped) = self
                        .save_extracted(&extracted, agent_id, contextual_source, "llm")
                        .await;
                    info!(
                        extracted = extracted.len(),
                        saved, skipped, agent_id, "contextual llm extraction complete"
                    );
                    return ExtractionResult {
                        extracted: extracted.len(),
                        saved,
                        skipped,
                    };
                }
                Ok(_) => {
                    return ExtractionResult {
                        extracted: 0,
                        saved: 0,
                        skipped: 0,
                    };
                }
                Err(err) => {
                    warn!(error = %err, "llm extraction failed, falling back to rule-based");
                }
            }
        }

        // No LLM extractor configured, or it failed above: fall back to
        // the original rule-based, per-text extraction (each side keeps
        // its own independent source-role guard, exactly as before).
        let mut result = self
            .extract_and_save_labeled(response_text, agent_id, "assistant")
            .await;
        if let Some(user_text) = user_text.filter(|t| !t.trim().is_empty()) {
            let user_result = self
                .extract_and_save_labeled(user_text, agent_id, "user")
                .await;
            result.extracted += user_result.extracted;
            result.saved += user_result.saved;
            result.skipped += user_result.skipped;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memvault_core::error::Result as CoreResult;
    use memvault_core::storage::sqlite::SqliteStore;

    /// Stub `LlmExtractor` returning a fixed set of memories (or an error),
    /// so the contextual-extraction wiring can be tested without a real
    /// network call.
    struct StubLlmExtractor {
        result: std::sync::Mutex<Option<CoreResult<Vec<ExtractedMemory>>>>,
        seen_context: std::sync::Mutex<Option<String>>,
    }

    impl StubLlmExtractor {
        fn returning(memories: Vec<ExtractedMemory>) -> Self {
            Self {
                result: std::sync::Mutex::new(Some(Ok(memories))),
                seen_context: std::sync::Mutex::new(None),
            }
        }

        fn failing(msg: &str) -> Self {
            Self {
                result: std::sync::Mutex::new(Some(Err(
                    memvault_core::error::MemVaultError::LlmExtraction(msg.to_string()),
                ))),
                seen_context: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl memvault_core::llm_extractor::LlmExtractor for StubLlmExtractor {
        async fn extract(&self, context: &str) -> CoreResult<Vec<ExtractedMemory>> {
            *self.seen_context.lock().unwrap() = Some(context.to_string());
            self.result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()))
        }
    }

    fn stub_memory(content: &str) -> ExtractedMemory {
        ExtractedMemory {
            content: content.to_string(),
            instruction: None,
            memory_type: MemoryType::Preference,
            priority: Priority::Reference,
            tags: vec!["stub".to_string()],
            confidence: 0.9,
        }
    }

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
            all.iter()
                .all(|m| m.tags.contains(&"review:required".to_string())),
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

    /// When an LLM extractor is configured, `extract_and_save_contextual`
    /// must use it (not the rule-based path), pass it both sides of the
    /// exchange, and mark the saved memory `method:llm`.
    #[tokio::test]
    async fn test_contextual_uses_llm_extractor_when_configured() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let llm = Arc::new(StubLlmExtractor::returning(vec![stub_memory("likes tabs")]));
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default())
            .with_llm_extractor(llm);

        let result = extractor
            .extract_and_save_contextual(
                Some("I like tabs, not spaces"),
                "Noted, I'll use tabs.",
                "test-agent",
            )
            .await;
        assert_eq!(result.saved, 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].tags.contains(&"method:llm".to_string()));
        // Both user and assistant text present -> mixed provenance, so the
        // same review-required downgrade as the rule path's Mixed role
        // must apply.
        assert!(all[0].tags.contains(&"review:required".to_string()));
        assert!(all[0].tags.contains(&"source:mixed".to_string()));
    }

    /// User-only context (no assistant text) is trusted provenance, same as
    /// the rule-based user path: no review-required downgrade.
    #[tokio::test]
    async fn test_contextual_llm_user_only_is_trusted() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let llm = Arc::new(StubLlmExtractor::returning(vec![stub_memory("likes tabs")]));
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default())
            .with_llm_extractor(llm);

        let result = extractor
            .extract_and_save_contextual(Some("I like tabs, not spaces"), "", "test-agent")
            .await;
        assert_eq!(result.saved, 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(all[0].tags.contains(&"source:user".to_string()));
        assert!(!all[0].tags.contains(&"review:required".to_string()));
    }

    /// An LLM failure must not break extraction: it falls back to the
    /// original rule-based, per-text path and still saves what the rule
    /// engine can find.
    #[tokio::test]
    async fn test_contextual_falls_back_to_rule_based_on_llm_error() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let llm = Arc::new(StubLlmExtractor::failing("network down"));
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default())
            .with_llm_extractor(llm);

        let result = extractor
            .extract_and_save_contextual(None, "I always prefer dark mode", "test-agent")
            .await;
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter()
                .any(|m| m.tags.contains(&"method:rule".to_string()))
        );
    }

    /// With no LLM extractor configured at all, contextual extraction must
    /// behave exactly like the pre-existing rule-based two-call path.
    #[tokio::test]
    async fn test_contextual_without_llm_extractor_uses_rule_based() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let extractor = ResponseExtractor::new(store.clone(), ExtractionConfig::default());

        let result = extractor
            .extract_and_save_contextual(Some("我偏好用 tabs 缩进"), "好的", "test-agent")
            .await;
        assert!(result.saved >= 1);

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(
            all.iter()
                .any(|m| m.tags.contains(&"source:user".to_string())
                    && m.tags.contains(&"method:rule".to_string()))
        );
    }

    /// Disabled assistant policy: the assistant side must be excluded from
    /// the LLM context entirely, while user text still gets extracted.
    #[tokio::test]
    async fn test_contextual_disabled_policy_excludes_assistant_from_llm_context() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let llm = Arc::new(StubLlmExtractor::returning(vec![stub_memory("likes tabs")]));
        let config = ExtractionConfig {
            assistant_policy: AssistantExtractionPolicy::Disabled,
            ..ExtractionConfig::default()
        };
        let extractor =
            ResponseExtractor::new(store.clone(), config).with_llm_extractor(llm.clone());

        let result = extractor
            .extract_and_save_contextual(
                Some("I like tabs, not spaces"),
                "I always prefer dark mode too",
                "test-agent",
            )
            .await;
        assert_eq!(result.saved, 1);

        let seen = llm.seen_context.lock().unwrap().clone().unwrap();
        assert!(seen.contains("I like tabs"));
        assert!(
            !seen.contains("Assistant:"),
            "disabled assistant policy must exclude assistant text from the LLM context, got: {seen}"
        );

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(all[0].tags.contains(&"source:user".to_string()));
        assert!(!all[0].tags.contains(&"review:required".to_string()));
    }
}
