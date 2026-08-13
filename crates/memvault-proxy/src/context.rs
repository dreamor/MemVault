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
