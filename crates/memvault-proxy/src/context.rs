use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use tracing::debug;

use memvault_core::intent::{self, Intent, IntentResult};

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct ObservedToolCall {
    tool_name: String,
    arguments_hint: Option<String>,
    timestamp: chrono::DateTime<Utc>,
}

pub struct SessionContext {
    tool_calls: RwLock<Vec<ObservedToolCall>>,
    inferred_project: RwLock<Option<String>>,
    #[allow(dead_code)]
    inferred_intent: RwLock<IntentResult>,
    context_changed: RwLock<bool>,
}

impl SessionContext {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tool_calls: RwLock::new(Vec::new()),
            inferred_project: RwLock::new(None),
            inferred_intent: RwLock::new(IntentResult {
                primary: Intent::General,
                domains: vec!["general".to_string()],
                confidence: 0.5,
            }),
            context_changed: RwLock::new(false),
        })
    }

    #[allow(dead_code)]
    pub async fn observe_tool_call(&self, tool_name: &str, arguments: &serde_json::Value) {
        let hint = Self::extract_context_hint(tool_name, arguments);

        self.tool_calls.write().await.push(ObservedToolCall {
            tool_name: tool_name.to_string(),
            arguments_hint: hint.clone(),
            timestamp: Utc::now(),
        });

        if let Some(project) = Self::infer_project_from_args(arguments) {
            let mut proj = self.inferred_project.write().await;
            if proj.as_deref() != Some(&project) {
                *proj = Some(project.clone());
                *self.context_changed.write().await = true;
                debug!(project = %project, "inferred project from tool call");
            }
        }

        if let Some(ref h) = hint {
            let new_intent = intent::analyze_intent(h);
            if new_intent.confidence > 0.3 {
                let mut current = self.inferred_intent.write().await;
                if current.primary != new_intent.primary {
                    *current = new_intent;
                    *self.context_changed.write().await = true;
                }
            }
        }
    }

    pub async fn take_if_changed(&self) -> bool {
        let mut changed = self.context_changed.write().await;
        if *changed {
            *changed = false;
            true
        } else {
            false
        }
    }

    pub async fn get_context_hint(&self) -> Option<String> {
        let calls = self.tool_calls.read().await;
        let recent: Vec<&ObservedToolCall> = calls.iter().rev().take(5).collect();
        if recent.is_empty() {
            return None;
        }
        let hints: Vec<String> = recent
            .iter()
            .filter_map(|c| c.arguments_hint.clone())
            .collect();
        if hints.is_empty() {
            Some(
                recent
                    .iter()
                    .map(|c| c.tool_name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        } else {
            Some(hints.join("; "))
        }
    }

    /// Conversation n-gram retrieval key — Feature D (docs/PAPER-INSPIRATIONS.md,
    /// paper §2.3 "conditional memory").
    ///
    /// Instead of keying retrieval on a single query sentence (unigram-style),
    /// condition it on the recent window of observed turns — and weight that
    /// window by recency, so the turn the agent is acting on *right now*
    /// dominates retrieval while slightly older turns still shape it. The
    /// newest turn is repeated most, decaying linearly with age, which biases
    /// both the keyword tier (term frequency) and the embedding toward the
    /// current focus. Output is length-bounded so a long session cannot bloat
    /// the query unboundedly.
    ///
    /// Returns `None` when nothing has been observed yet.
    pub async fn conversation_ngram(&self, window: usize) -> Option<String> {
        /// Hard cap on the assembled key; retrieval quality saturates long
        /// before this and the embedding/FTS input must stay bounded.
        const MAX_KEY_LEN: usize = 600;

        let window = window.max(1);
        let calls = self.tool_calls.read().await;
        if calls.is_empty() {
            return None;
        }

        // Newest first: index 0 is the most recent observation.
        let recent: Vec<&ObservedToolCall> = calls.iter().rev().take(window).collect();
        let n = recent.len();

        let mut segments: Vec<String> = Vec::new();
        let mut total_len = 0usize;
        'outer: for (i, call) in recent.iter().enumerate() {
            let text = call
                .arguments_hint
                .clone()
                .unwrap_or_else(|| call.tool_name.clone());
            // Recency weight: newest turn repeated `n` times, decaying to 1
            // for the oldest turn in the window.
            let weight = n - i;
            for _ in 0..weight {
                if total_len + text.len() > MAX_KEY_LEN {
                    break 'outer;
                }
                segments.push(text.clone());
                total_len += text.len();
            }
        }

        if segments.is_empty() {
            None
        } else {
            Some(segments.join(" "))
        }
    }

    pub async fn get_project(&self) -> Option<String> {
        self.inferred_project.read().await.clone()
    }

    #[allow(dead_code)]
    pub async fn get_intent(&self) -> IntentResult {
        self.inferred_intent.read().await.clone()
    }

    #[allow(dead_code)]
    fn extract_context_hint(tool_name: &str, arguments: &serde_json::Value) -> Option<String> {
        let mut parts = vec![tool_name.to_string()];
        if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
            parts.push(path.to_string());
        }
        if let Some(query) = arguments.get("query").and_then(|v| v.as_str()) {
            parts.push(query.to_string());
        }
        if let Some(content) = arguments.get("content").and_then(|v| v.as_str())
            && content.len() < 200
        {
            parts.push(content.to_string());
        }
        if parts.len() > 1 {
            Some(parts.join(" "))
        } else {
            None
        }
    }

    #[allow(dead_code)]
    fn infer_project_from_args(arguments: &serde_json::Value) -> Option<String> {
        let path = arguments.get("path").and_then(|v| v.as_str())?;
        let parts: Vec<&str> = path.split('/').collect();
        // Look for common project directory patterns
        for (i, part) in parts.iter().enumerate() {
            if (*part == "src" || *part == "lib" || *part == "crates" || *part == "packages")
                && i > 0
            {
                return Some(format!("project:{}", parts[i - 1]));
            }
        }
        // Fallback: use the first non-home directory component
        if parts.len() >= 4 {
            return Some(format!("project:{}", parts[3]));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_take_if_changed_starts_false() {
        let ctx = SessionContext::new();
        assert!(!ctx.take_if_changed().await);
    }

    #[tokio::test]
    async fn test_observe_tool_call_sets_project_and_changed() {
        let ctx = SessionContext::new();
        ctx.observe_tool_call(
            "read_file",
            &serde_json::json!({ "path": "/Users/me/work/app/src/main.rs" }),
        )
        .await;
        assert!(ctx.take_if_changed().await);
        assert_eq!(ctx.get_project().await.as_deref(), Some("project:app"));
    }

    #[tokio::test]
    async fn test_take_changed_resets_after_read() {
        let ctx = SessionContext::new();
        ctx.observe_tool_call(
            "read_file",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;
        assert!(
            ctx.take_if_changed().await,
            "project inference marks change"
        );
        assert!(!ctx.take_if_changed().await, "flag resets to false");
    }

    #[tokio::test]
    async fn test_get_context_hint_builds_hint() {
        let ctx = SessionContext::new();
        ctx.observe_tool_call(
            "edit_file",
            &serde_json::json!({ "path": "/a/b/c.rs", "query": "fix bug" }),
        )
        .await;
        let hint = ctx.get_context_hint().await.unwrap();
        assert!(hint.contains("edit_file"));
        assert!(hint.contains("fix bug"));
    }

    #[tokio::test]
    async fn test_get_context_hint_empty() {
        let ctx = SessionContext::new();
        assert!(ctx.get_context_hint().await.is_none());
    }

    #[tokio::test]
    async fn test_get_context_hint_falls_back_to_tool_names() {
        let ctx = SessionContext::new();
        ctx.observe_tool_call("bash", &serde_json::json!({})).await;
        let hint = ctx.get_context_hint().await.unwrap();
        assert_eq!(hint, "bash");
    }

    // —— Feature D: recency-weighted conversation n-gram ——

    #[tokio::test]
    async fn test_conversation_ngram_empty() {
        let ctx = SessionContext::new();
        assert!(ctx.conversation_ngram(5).await.is_none());
    }

    #[tokio::test]
    async fn test_conversation_ngram_weights_recent_turns_more() {
        let ctx = SessionContext::new();
        // Oldest -> newest: A, B, C.
        ctx.observe_tool_call("a", &serde_json::json!({ "query": "alpha" }))
            .await;
        ctx.observe_tool_call("b", &serde_json::json!({ "query": "beta" }))
            .await;
        ctx.observe_tool_call("c", &serde_json::json!({ "query": "gamma" }))
            .await;

        // window=3 -> weights newest 3, next 2, oldest 1.
        let key = ctx.conversation_ngram(3).await.unwrap();
        let count = |needle: &str| key.matches(needle).count();
        assert_eq!(count("gamma"), 3, "newest turn repeats most: {}", key);
        assert_eq!(count("beta"), 2, "middle turn: {}", key);
        assert_eq!(count("alpha"), 1, "oldest turn: {}", key);
        // Newest first.
        assert!(
            key.find("gamma").unwrap() < key.find("alpha").unwrap(),
            "newest turn must lead the key: {}",
            key
        );
    }

    #[tokio::test]
    async fn test_conversation_ngram_respects_window() {
        let ctx = SessionContext::new();
        for name in ["one", "two", "three", "four"] {
            ctx.observe_tool_call(name, &serde_json::json!({ "query": name }))
                .await;
        }
        // window=2 keeps only the two most recent turns.
        let key = ctx.conversation_ngram(2).await.unwrap();
        assert!(key.contains("four"));
        assert!(key.contains("three"));
        assert!(!key.contains("two"), "older turns drop out: {}", key);
        assert!(!key.contains("one"), "older turns drop out: {}", key);
    }

    #[tokio::test]
    async fn test_conversation_ngram_bounded_length() {
        let ctx = SessionContext::new();
        // Many long turns: the assembled key must stay bounded.
        for i in 0..20 {
            ctx.observe_tool_call(
                "edit",
                &serde_json::json!({ "content": format!("turn content number {} padding padding", i) }),
            )
            .await;
        }
        let key = ctx.conversation_ngram(10).await.unwrap();
        assert!(
            key.len() <= 600 + 60,
            "key must stay bounded: len={}",
            key.len()
        );
    }

    #[tokio::test]
    async fn test_conversation_ngram_falls_back_to_tool_name() {
        let ctx = SessionContext::new();
        ctx.observe_tool_call("bash", &serde_json::json!({})).await;
        let key = ctx.conversation_ngram(5).await.unwrap();
        assert!(key.contains("bash"), "tool name used when no hint: {}", key);
    }

    #[test]
    fn test_extract_context_hint_parts() {
        let args = serde_json::json!({ "path": "/x", "query": "q", "content": "c" });
        let hint = SessionContext::extract_context_hint("tool", &args);
        assert_eq!(hint.as_deref(), Some("tool /x q c"));

        // content longer than 200 chars is omitted but path/query remain
        let long = "x".repeat(250);
        let args = serde_json::json!({ "path": "/x", "query": "q", "content": long });
        let hint = SessionContext::extract_context_hint("tool", &args);
        assert_eq!(hint.as_deref(), Some("tool /x q"));

        // only tool name -> None
        let hint = SessionContext::extract_context_hint("tool", &serde_json::json!({}));
        assert!(hint.is_none());
    }

    #[test]
    fn test_infer_project_from_src_path() {
        let args = serde_json::json!({ "path": "/repo/app/src/main.rs" });
        assert_eq!(
            SessionContext::infer_project_from_args(&args),
            Some("project:app".to_string())
        );
    }

    #[test]
    fn test_infer_project_from_crates_path() {
        let args = serde_json::json!({ "path": "/workspace/backend/crates/api/lib.rs" });
        // the directory BEFORE "crates" is taken as the project root
        assert_eq!(
            SessionContext::infer_project_from_args(&args),
            Some("project:backend".to_string())
        );
    }

    #[test]
    fn test_infer_project_fallback_deep_path() {
        let args = serde_json::json!({ "path": "/Users/x/Projects/webshop/frontend/" });
        // no src/lib/crates/packages marker -> first non-home component
        assert_eq!(
            SessionContext::infer_project_from_args(&args),
            Some("project:Projects".to_string())
        );
    }

    #[test]
    fn test_infer_project_missing_path() {
        let args = serde_json::json!({});
        assert_eq!(SessionContext::infer_project_from_args(&args), None);
    }
}
