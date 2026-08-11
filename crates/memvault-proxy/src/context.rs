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
