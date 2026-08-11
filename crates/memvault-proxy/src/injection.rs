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
        self.refresh().await;
        true
    }

    pub async fn refresh(&self) {
        let agent_id = self.agent_id.read().await.clone();
        let context_hint = self.context.get_context_hint().await;
        let project = self.context.get_project().await;

        let results = match self
            .router
            .session_start(&agent_id, context_hint.as_deref(), project.as_deref())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "injection refresh failed");
                return;
            }
        };

        let formatted = self.router.format_as_instructions(&results);
        let session_id = format!("inj_{}", Uuid::new_v4().simple());
        let memory_ids: Vec<String> = results.iter().map(|r| r.memory.id.clone()).collect();

        let formatted_with_session =
            format!("[MEMORY CONTEXT - session: {}]\n{}", session_id, formatted);

        debug!(
            session_id = %session_id,
            memories = memory_ids.len(),
            "injection refreshed"
        );

        *self.state.write().await = Some(InjectionState {
            session_id,
            formatted_text: formatted_with_session,
            injected_memory_ids: memory_ids,
            updated_at: Utc::now(),
        });
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
