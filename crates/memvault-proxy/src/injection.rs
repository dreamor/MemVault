use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use tracing::debug;
use uuid::Uuid;

use memvault_core::router::MemoryRouter;

use crate::context::SessionContext;

pub struct InjectionState {
    pub session_id: String,
    pub formatted_text: String,
    #[allow(dead_code)]
    pub injected_memory_ids: Vec<String>,
    #[allow(dead_code)]
    pub updated_at: chrono::DateTime<Utc>,
    /// Candidates dropped during this injection, with reasons — kept so the
    /// proxy can answer "why didn't memory X reach the model?" after the fact.
    #[allow(dead_code)]
    pub skipped: Vec<memvault_core::models::SkippedMemory>,
}

pub struct InjectionEngine {
    router: Arc<MemoryRouter>,
    context: Arc<SessionContext>,
    state: RwLock<Option<InjectionState>>,
    agent_id: RwLock<String>,
}

impl InjectionEngine {
    pub fn new(router: Arc<MemoryRouter>, context: Arc<SessionContext>) -> Arc<Self> {
        Arc::new(Self {
            router,
            context,
            state: RwLock::new(None),
            agent_id: RwLock::new("proxy-client".to_string()),
        })
    }

    #[allow(dead_code)]
    pub async fn set_agent_id(&self, agent_id: &str) {
        *self.agent_id.write().await = agent_id.to_string();
    }

    pub async fn refresh_if_needed(&self) -> bool {
        if !self.context.take_if_changed().await {
            return false;
        }
        self.refresh().await
    }

    /// Returns `true` when the injection state was successfully replaced, so
    /// callers can tell a real refresh from a failed one instead of re-using
    /// stale instructions for the current session.
    pub async fn refresh(&self) -> bool {
        let agent_id = self.agent_id.read().await.clone();
        let context_hint = self.context.get_context_hint().await;
        let project = self.context.get_project().await;

        let output = match self
            .router
            .session_start_layered(&agent_id, context_hint.as_deref(), project.as_deref())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "injection refresh failed");
                return false;
            }
        };

        let formatted = self.router.format_layered_instructions(&output);
        let session_id = format!("inj_{}", Uuid::new_v4().simple());
        let memory_ids: Vec<String> = output
            .injected
            .iter()
            .map(|r| r.memory.id.clone())
            .collect();

        let formatted_with_session =
            format!("[MEMORY CONTEXT - session: {}]\n{}", session_id, formatted);

        debug!(
            session_id = %session_id,
            memories = memory_ids.len(),
            overflow = output.overflow_count,
            "injection refreshed (layered)"
        );

        *self.state.write().await = Some(InjectionState {
            session_id,
            formatted_text: formatted_with_session,
            injected_memory_ids: memory_ids,
            updated_at: Utc::now(),
            skipped: output.skipped.clone(),
        });
        true
    }

    pub async fn get_current_injection(&self) -> Option<String> {
        let state = self.state.read().await;
        state.as_ref().map(|s| s.formatted_text.clone())
    }

    #[allow(dead_code)]
    pub async fn get_session_id(&self) -> Option<String> {
        let state = self.state.read().await;
        state.as_ref().map(|s| s.session_id.clone())
    }

    #[allow(dead_code)]
    pub async fn get_injected_memory_ids(&self) -> Vec<String> {
        let state = self.state.read().await;
        state
            .as_ref()
            .map(|s| s.injected_memory_ids.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memvault_core::models::{Memory, MemoryType, Priority, SourceAgent};
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;

    async fn make_engine() -> (Arc<InjectionEngine>, Arc<SessionContext>, Arc<SqliteStore>) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let context = SessionContext::new();
        let engine = InjectionEngine::new(router, context.clone());
        (engine, context, store)
    }

    #[tokio::test]
    async fn test_new_engine_has_no_state() {
        let (engine, _ctx, _store) = make_engine().await;
        assert!(engine.get_current_injection().await.is_none());
        assert!(engine.get_session_id().await.is_none());
        assert!(engine.get_injected_memory_ids().await.is_empty());
    }

    #[tokio::test]
    async fn test_refresh_without_changes_does_not_run() {
        let (engine, _ctx, _store) = make_engine().await;
        let ran = engine.refresh_if_needed().await;
        assert!(!ran);
        assert!(engine.get_current_injection().await.is_none());
    }

    #[tokio::test]
    async fn test_refresh_if_needed_after_context_change() {
        let (engine, ctx, _store) = make_engine().await;
        ctx.observe_tool_call(
            "read",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;
        let ran = engine.refresh_if_needed().await;
        assert!(ran, "context changed -> refresh runs");
        let injection = engine.get_current_injection().await.expect("state present");
        assert!(injection.contains("MEMORY CONTEXT"));
        assert!(engine.get_session_id().await.unwrap().starts_with("inj_"));
    }

    #[tokio::test]
    async fn test_refresh_after_change_only_once() {
        let (engine, ctx, _store) = make_engine().await;
        ctx.observe_tool_call(
            "read",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;
        assert!(engine.refresh_if_needed().await);
        assert!(
            !engine.refresh_if_needed().await,
            "second refresh is a no-op until the next change"
        );
    }

    #[tokio::test]
    async fn test_refresh_saves_injection_with_memories() {
        let (engine, ctx, store) = make_engine().await;
        store
            .save(Memory::new(
                MemoryType::Preference,
                "prefers vim".to_string(),
                Priority::Must,
                SourceAgent {
                    id: "proxy-client".to_string(),
                    agent_type: "general".to_string(),
                    session_id: None,
                },
            ))
            .await
            .unwrap();

        ctx.observe_tool_call(
            "edit",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;
        assert!(engine.refresh_if_needed().await);
        let ids = engine.get_injected_memory_ids().await;
        assert!(!ids.is_empty());
        let text = engine.get_current_injection().await.unwrap();
        assert!(text.contains("prefers vim"));
    }

    #[tokio::test]
    async fn test_set_agent_id_changes_scope() {
        let (engine, _ctx, _store) = make_engine().await;
        engine.set_agent_id("other-agent").await;
        let refreshed = engine.refresh().await;
        assert!(refreshed, "refresh should report success");
        // refresh stores state (possibly empty injection) and a fresh session
        let _ = engine.get_current_injection().await;
        assert!(engine.get_session_id().await.is_some());
    }
}
