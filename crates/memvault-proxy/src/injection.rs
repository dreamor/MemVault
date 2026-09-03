use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::{Notify, RwLock};
use tracing::debug;
use uuid::Uuid;

use memvault_core::models::InjectChannel;
use memvault_core::router::MemoryRouter;

use crate::context::SessionContext;

/// How long a caller may block waiting for the semantic prefetch to land
/// before proceeding with the deterministic baseline (Feature C). Kept short
/// on purpose: the whole point is that the request is never held hostage by
/// embedding latency.
pub const PREFETCH_WAIT_WINDOW: Duration = Duration::from_millis(250);

/// Conversation n-gram window (Feature D): how many recent observed turns
/// build the recency-weighted retrieval key. Overridable via
/// `MEMVAULT_CONTEXT_NGRAM_WINDOW`; invalid/zero values fall back to 5.
pub fn ngram_window_from_env() -> usize {
    std::env::var("MEMVAULT_CONTEXT_NGRAM_WINDOW")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(5)
}

/// Which phase produced the current injection state.
///
/// Feature C (docs/PAPER-INSPIRATIONS.md, paper §2.3): deterministic
/// addressing (MUST rules + namespace rules, zero embedding calls) can be
/// served immediately; the semantic pipeline prefetches in the background
/// and replaces the state when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionPhase {
    /// Fast path only: MUST-level memories resolved by pure rules.
    Deterministic,
    /// Full layered pipeline (semantic/hybrid search) completed.
    Full,
}

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
    /// Which phase produced this state.
    pub phase: InjectionPhase,
}

pub struct InjectionEngine {
    router: Arc<MemoryRouter>,
    context: Arc<SessionContext>,
    state: RwLock<Option<InjectionState>>,
    agent_id: RwLock<String>,
    /// Monotonic refresh generation. A slow prefetch that returns after a
    /// newer refresh started must not overwrite the fresher state.
    generation: AtomicU64,
    /// Wakes `wait_full` waiters whenever the state is (re)placed.
    prefetch_notify: Notify,
}

impl InjectionEngine {
    pub fn new(router: Arc<MemoryRouter>, context: Arc<SessionContext>) -> Arc<Self> {
        Arc::new(Self {
            router,
            context,
            state: RwLock::new(None),
            agent_id: RwLock::new("proxy-client".to_string()),
            generation: AtomicU64::new(0),
            prefetch_notify: Notify::new(),
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
        // Feature F: if this agent's canonical injection channel is not the
        // transparent proxy, skip auto-injection so the same memory is not
        // delivered twice (another channel owns it).
        if !self.router.channel_allows(&agent_id, InjectChannel::Proxy) {
            debug!(agent_id = %agent_id, "proxy auto-injection skipped: not the canonical channel");
            return false;
        }
        // Feature D: condition retrieval on the recent turn window, weighted
        // by recency, rather than a single flat hint.
        let context_hint = self
            .context
            .conversation_ngram(ngram_window_from_env())
            .await;
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
            phase: InjectionPhase::Full,
        });
        self.prefetch_notify.notify_waiters();
        true
    }

    /// Two-phase refresh — Feature C (paper §2.3 deterministic addressing +
    /// async prefetch).
    ///
    /// Phase 1 (deterministic, synchronous): MUST-level memories resolved by
    /// pure rules, zero embedding calls. Stored immediately so a caller can
    /// proceed without waiting on the semantic pipeline.
    ///
    /// Phase 2 (prefetch, async): the full layered pipeline runs in the
    /// background and replaces the state when it lands. If it fails, the
    /// deterministic baseline is kept rather than dropped. A generation
    /// counter stops a slow prefetch from clobbering a fresher refresh.
    ///
    /// Returns `true` when a refresh was kicked off (i.e. the context had
    /// changed), mirroring [`Self::refresh_if_needed`].
    pub async fn refresh_if_needed_two_phase(self: &Arc<Self>) -> bool {
        if !self.context.take_if_changed().await {
            return false;
        }
        self.refresh_two_phase().await
    }

    /// Unconditional two-phase refresh. See [`Self::refresh_if_needed_two_phase`].
    pub async fn refresh_two_phase(self: &Arc<Self>) -> bool {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // One session id spans both phases of a single refresh, so compliance
        // tracking stays coherent as the state upgrades Deterministic -> Full.
        let session_id = format!("inj_{}", Uuid::new_v4().simple());

        let agent_id = self.agent_id.read().await.clone();
        // Feature F: skip auto-injection when another channel owns this agent.
        if !self.router.channel_allows(&agent_id, InjectChannel::Proxy) {
            debug!(agent_id = %agent_id, "proxy auto-injection skipped: not the canonical channel");
            return false;
        }
        // Feature D: recency-weighted conversation n-gram as the retrieval key.
        let context_hint = self
            .context
            .conversation_ngram(ngram_window_from_env())
            .await;
        let project = self.context.get_project().await;

        // —— Phase 1: deterministic fast path. Never fails the refresh: an
        // error here only means we fall through to phase 2 with no baseline.
        match self
            .router
            .deterministic_injection(&agent_id, project.as_deref())
            .await
        {
            Ok(results) if !results.is_empty() => {
                let formatted = self.router.format_as_instructions(&results);
                let formatted_with_session =
                    format!("[MEMORY CONTEXT - session: {}]\n{}", session_id, formatted);
                let memory_ids: Vec<String> = results.iter().map(|r| r.memory.id.clone()).collect();
                debug!(
                    session_id = %session_id,
                    memories = memory_ids.len(),
                    "deterministic fast-path injection ready"
                );
                *self.state.write().await = Some(InjectionState {
                    session_id: session_id.clone(),
                    formatted_text: formatted_with_session,
                    injected_memory_ids: memory_ids,
                    updated_at: Utc::now(),
                    skipped: Vec::new(),
                    phase: InjectionPhase::Deterministic,
                });
                self.prefetch_notify.notify_waiters();
            }
            Ok(_) => {
                debug!("no MUST memories for deterministic fast path");
            }
            Err(e) => {
                tracing::warn!(error = %e, "deterministic fast path failed; waiting for prefetch only");
            }
        }

        // —— Phase 2: full pipeline in the background.
        let engine = Arc::clone(self);
        let hint = context_hint.clone();
        let proj = project.clone();
        tokio::spawn(async move {
            let output = engine
                .router
                .session_start_layered(&agent_id, hint.as_deref(), proj.as_deref())
                .await;

            // A newer refresh superseded this one — drop the stale result.
            if engine.generation.load(Ordering::SeqCst) != generation {
                debug!(generation, "prefetch superseded by a newer refresh");
                return;
            }

            let output = match output {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "injection prefetch failed; keeping deterministic baseline");
                    return;
                }
            };

            let formatted = engine.router.format_layered_instructions(&output);
            let formatted_with_session =
                format!("[MEMORY CONTEXT - session: {}]\n{}", session_id, formatted);
            let memory_ids: Vec<String> = output
                .injected
                .iter()
                .map(|r| r.memory.id.clone())
                .collect();
            debug!(
                session_id = %session_id,
                memories = memory_ids.len(),
                overflow = output.overflow_count,
                "injection prefetch complete (layered)"
            );
            *engine.state.write().await = Some(InjectionState {
                session_id,
                formatted_text: formatted_with_session,
                injected_memory_ids: memory_ids,
                updated_at: Utc::now(),
                skipped: output.skipped.clone(),
                phase: InjectionPhase::Full,
            });
            engine.prefetch_notify.notify_waiters();
        });

        true
    }

    /// Wait until the injection state was produced by the full pipeline, or
    /// the timeout elapses. Returns `true` when the state is `Full`. A
    /// deterministic baseline (or no state at all) returns `false` after the
    /// timeout — callers must treat that as "proceed with what is available",
    /// never as an error.
    pub async fn wait_full(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let notified = self.prefetch_notify.notified();
            // Check AFTER arming the waiter so a completion between check and
            // await cannot be missed.
            {
                let state = self.state.read().await;
                if let Some(s) = state.as_ref()
                    && s.phase == InjectionPhase::Full
                {
                    return true;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return false;
            }
        }
    }

    /// Current injection phase, if any state exists.
    pub async fn get_phase(&self) -> Option<InjectionPhase> {
        self.state.read().await.as_ref().map(|s| s.phase)
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

    // —— Feature C: two-phase refresh (deterministic fast path + prefetch) ——

    /// Embedder whose `embed` blocks until the test flips a watch to `true`.
    /// Lets us freeze the semantic pipeline in phase 2 deterministically.
    struct GatedEmbedder {
        rx: tokio::sync::watch::Receiver<bool>,
    }
    #[async_trait::async_trait]
    impl memvault_core::embedding::EmbeddingProvider for GatedEmbedder {
        async fn embed(&self, texts: &[String]) -> memvault_core::error::Result<Vec<Vec<f32>>> {
            let mut rx = self.rx.clone();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
            Ok(texts.iter().map(|_| vec![0.1_f32, 0.2]).collect())
        }
        fn dimension(&self) -> usize {
            2
        }
    }

    async fn make_gated_engine() -> (
        Arc<InjectionEngine>,
        Arc<SessionContext>,
        Arc<SqliteStore>,
        tokio::sync::watch::Sender<bool>,
    ) {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let (tx, rx) = tokio::sync::watch::channel(false);
        let router = Arc::new(
            MemoryRouter::new(store.clone()).with_embedder(Arc::new(GatedEmbedder { rx })),
        );
        let context = SessionContext::new();
        let engine = InjectionEngine::new(router, context.clone());
        (engine, context, store, tx)
    }

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "proxy-client".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn test_two_phase_serves_deterministic_then_full() {
        let (engine, ctx, store, gate) = make_gated_engine().await;
        // A MUST rule (deterministic fast path) plus a plain reference memory
        // (only reachable through the full search pipeline).
        store
            .save(Memory::new(
                MemoryType::Preference,
                "always use Rust".to_string(),
                Priority::Must,
                agent(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "the checkout service uses PostgreSQL".to_string(),
                Priority::Reference,
                agent(),
            ))
            .await
            .unwrap();

        ctx.observe_tool_call(
            "edit",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;

        assert!(engine.refresh_if_needed_two_phase().await);

        // Phase 1 lands synchronously: MUST rule present, still not Full
        // because the semantic pipeline is frozen inside embed().
        let text = engine
            .get_current_injection()
            .await
            .expect("deterministic baseline must be available immediately");
        assert!(text.contains("always use Rust"), "baseline: {}", text);
        assert_eq!(
            engine.get_phase().await,
            Some(InjectionPhase::Deterministic)
        );
        assert!(
            !engine.wait_full(Duration::from_millis(50)).await,
            "prefetch is still blocked; wait_full must time out, not hang"
        );

        // Release the embedder: the prefetch completes and upgrades the state.
        gate.send(true).unwrap();
        assert!(
            engine.wait_full(Duration::from_secs(5)).await,
            "prefetch should land once the embedder unblocks"
        );
        assert_eq!(engine.get_phase().await, Some(InjectionPhase::Full));
        let full = engine.get_current_injection().await.unwrap();
        assert!(full.contains("always use Rust"), "full: {}", full);
    }

    #[tokio::test]
    async fn test_two_phase_without_must_memories_still_reaches_full() {
        let (engine, ctx, store, gate) = make_gated_engine().await;
        store
            .save(Memory::new(
                MemoryType::Fact,
                "the checkout service uses PostgreSQL".to_string(),
                Priority::Reference,
                agent(),
            ))
            .await
            .unwrap();

        ctx.observe_tool_call(
            "edit",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;

        assert!(engine.refresh_if_needed_two_phase().await);
        // No MUST memories -> no deterministic baseline state yet.
        assert!(engine.get_phase().await.is_none());

        gate.send(true).unwrap();
        assert!(engine.wait_full(Duration::from_secs(5)).await);
        assert_eq!(engine.get_phase().await, Some(InjectionPhase::Full));
    }

    #[tokio::test]
    async fn test_two_phase_no_change_is_noop() {
        let (engine, _ctx, _store, _gate) = make_gated_engine().await;
        assert!(!engine.refresh_if_needed_two_phase().await);
        assert!(engine.get_phase().await.is_none());
    }

    // —— Feature F: canonical injection channel ——

    #[tokio::test]
    async fn test_refresh_skipped_when_proxy_not_canonical() {
        // The engine's agent ("proxy-client") is owned by the MCP channel, so
        // the transparent proxy must not auto-inject for it.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let registry = vec![memvault_core::models::AgentProfile {
            id: "proxy-client".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: memvault_core::models::InjectRules::default(),
            api_key: None,
            inject_channel: Some(InjectChannel::Mcp),
        }];
        let router = Arc::new(MemoryRouter::with_registry(store.clone(), registry));
        let context = SessionContext::new();
        let engine = InjectionEngine::new(router, context.clone());

        context
            .observe_tool_call(
                "edit",
                &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
            )
            .await;

        assert!(
            !engine.refresh_if_needed().await,
            "proxy auto-injection must be skipped when another channel is canonical"
        );
        assert!(engine.get_current_injection().await.is_none());
        assert!(engine.get_phase().await.is_none());
        // Two-phase refresh is gated the same way.
        context
            .observe_tool_call(
                "read",
                &serde_json::json!({ "path": "/repo/myapp/src/x.rs" }),
            )
            .await;
        assert!(!engine.refresh_if_needed_two_phase().await);
        assert!(engine.get_phase().await.is_none());
    }

    #[tokio::test]
    async fn test_refresh_allowed_when_proxy_canonical_or_unset() {
        // No registry entry -> unrestricted -> proxy auto-injection proceeds.
        let (engine, ctx, _store) = make_engine().await;
        ctx.observe_tool_call(
            "edit",
            &serde_json::json!({ "path": "/repo/myapp/src/main.rs" }),
        )
        .await;
        assert!(engine.refresh_if_needed().await);
        assert!(engine.get_current_injection().await.is_some());
    }
}
