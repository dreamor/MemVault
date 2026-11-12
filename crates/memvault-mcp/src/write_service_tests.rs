use super::*;
use chrono::Datelike;
use memvault_core::embedding::EmbeddingProvider;
use memvault_core::error::{MemVaultError, Result};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    mcp: MemVaultMcp,
    store: Arc<SqliteStore>,
    base: String,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture(embedder: LazyEmbedder, keyed: bool) -> Fixture {
    fixture_with_llm(embedder, keyed, None).await
}

async fn fixture_with_llm(
    embedder: LazyEmbedder,
    keyed: bool,
    llm: Option<Arc<dyn LlmExtractor>>,
) -> Fixture {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let router = if keyed {
        MemoryRouter::with_registry(
            store.clone(),
            ["alice", "bob"]
                .iter()
                .map(|id| AgentProfile {
                    id: id.to_string(),
                    agent_type: "coding-assistant".to_string(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: Some(format!("test-{id}-key")),
                    inject_channel: None,
                })
                .collect(),
        )
    } else {
        MemoryRouter::new(store.clone())
    };
    let router = Arc::new(router);
    let mcp = MemVaultMcp::new(store.clone(), router.clone(), embedder.clone(), None)
        .with_lazy_llm_extractor(LazyLlmExtractor::fixed(llm.clone()));
    let app = crate::rest_api::build_rest_router(
        store.clone(),
        Some(store.clone()),
        router,
        None,
        crate::metrics_setup::install_recorder(),
        embedder,
        LazyLlmExtractor::fixed(llm),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture {
        mcp,
        store,
        base,
        client: reqwest::Client::new(),
        task,
    }
}

async fn rest(
    f: &Fixture,
    route: &str,
    body: Value,
    actor: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let mut request = f.client.post(format!("{}{route}", f.base)).json(&body);
    if let Some(actor) = actor {
        request = request
            .header("x-memvault-agent-id", actor)
            .header("x-memvault-api-key", format!("test-{actor}-key"));
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

fn tool_json(result: CallToolResult) -> Value {
    match &result.content[0] {
        ContentBlock::Text(text) => serde_json::from_str(&text.text).unwrap(),
        _ => panic!("expected JSON text"),
    }
}

async fn mcp_save(f: &Fixture, body: Value) -> Value {
    tool_json(
        f.mcp
            .save_memory(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .unwrap(),
    )
}

async fn mcp_extract(f: &Fixture, body: Value) -> Value {
    tool_json(
        f.mcp
            .extract_memories(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .unwrap(),
    )
}

#[tokio::test]
async fn phase2_save_parity_dedup_force_and_metadata() {
    for transport in ["rest", "mcp"] {
        let f = fixture(LazyEmbedder::fixed(None), false).await;
        let body = json!({
            "content": "release tooling uses signed reproducible artifacts",
            "priority": "MUST", "namespace": "project:MemVault/", "agent_id": "alice",
            "agent_type": "coding-assistant", "instruction": "Always sign release artifacts",
            "tags": ["release"], "visibility": "shared",
            "occurred_at": "2026-09-09T12:00:00Z", "expires_at": "2030-01-01T00:00:00Z",
        });
        let write = async |body: Value| {
            if transport == "rest" {
                let (status, response) = rest(&f, "/api/memories", body, None).await;
                assert_eq!(status, 200);
                response["data"].clone()
            } else {
                mcp_save(&f, body).await
            }
        };
        let first = write(body.clone()).await;
        let second = write(body.clone()).await;
        let status_key = if transport == "rest" {
            "action"
        } else {
            "status"
        };
        assert_eq!(first[status_key], "saved");
        assert_eq!(second[status_key], "skipped");
        assert_eq!(first["id"], second["id"]);
        assert_eq!(second["embedded"], false);
        assert_eq!(second["vector_pending"], false);
        let mut metadata = body.clone();
        metadata["tags"] = json!(["release", "audit"]);
        assert_eq!(write(metadata).await[status_key], "merged");
        let row = f.store.get(first["id"].as_str().unwrap()).await.unwrap();
        assert_eq!(row.namespace, "project:memvault");
        assert_eq!(row.tags, ["release", "audit"]);
        assert_eq!(row.source_agent.id, "alice");
        assert!(!row.identity_verified);
        assert!(!row.human_reviewed);
        assert!(row.ai_generated);
        assert_eq!(row.visibility, Visibility::Shared);
        assert_eq!(
            row.occurred_at.unwrap().to_rfc3339(),
            "2026-09-09T12:00:00+00:00"
        );
        assert!(row.expires_at.is_some());
        let mut force = body;
        force["force_insert"] = json!(true);
        assert_eq!(write(force).await[status_key], "saved");
        assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn phase2_skill_type_without_metadata_always_inserts() {
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let body = json!({ "content": "Run the release checklist", "type": "skill" });
    let (_, rest_result) = rest(&f, "/api/memories", body.clone(), None).await;
    let mcp_result = mcp_save(&f, body).await;
    assert_ne!(rest_result["data"]["id"], mcp_result["id"]);
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 2);
}

#[tokio::test]
async fn phase2_rest_header_actor_cannot_corroborate_spoofed_body() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let body = json!({ "content": "Release artifacts require approval", "priority": "MUST", "agent_id": "bob" });
    let (_, first) = rest(&f, "/api/memories", body.clone(), Some("alice")).await;
    let id = first["data"]["id"].as_str().unwrap();
    let row = f.store.get(id).await.unwrap();
    assert_eq!(row.source_agent.id, "alice");
    assert!(row.identity_verified);
    let mut body = body;
    body["tags"] = json!(["repeat"]);
    let (_, repeat) = rest(&f, "/api/memories", body, Some("alice")).await;
    assert_eq!(repeat["data"]["id"], id);
    let row = f.store.get(id).await.unwrap();
    assert_eq!(row.corroborating_agents, ["alice"]);
}

#[tokio::test]
async fn phase2_extract_parity_repeats_dedup_and_retains_origin() {
    for transport in ["rest", "mcp"] {
        let f = fixture(LazyEmbedder::fixed(None), false).await;
        let body = json!({ "text": "I prefer Rust for tooling\nI always use dark mode", "auto_save": true,
            "source_id": "speaker", "agent_id": "speaker", "occurred_at": "2026-01-01T00:00:00Z" });
        let extract = async |body: Value| {
            if transport == "rest" {
                let (status, response) = rest(&f, "/api/extract", body, None).await;
                assert_eq!(status, 200);
                response["data"].clone()
            } else {
                mcp_extract(&f, body).await
            }
        };
        let first = extract(body.clone()).await;
        let repeat = extract(body).await;
        assert_eq!(first["writes"].as_array().unwrap().len(), 2);
        assert_eq!(first["writes"][0]["id"], repeat["writes"][0]["id"]);
        assert_eq!(first["writes"][1]["id"], repeat["writes"][1]["id"]);
        assert!(
            repeat["writes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["status"] == "skipped")
        );
        let rows = f.store.list(None, 10, 0).await.unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.source_agent.id, "speaker");
            assert_eq!(row.namespace, "global");
            assert!(!row.identity_verified);
            assert!(!row.human_reviewed);
            assert!(row.ai_generated);
            assert!(row.tags.contains(&"method:rule".to_string()));
            assert_eq!(row.occurred_at.unwrap().year(), 2026);
        }
    }
}

#[tokio::test]
async fn phase2_rest_extract_expired_duplicate_creates_fresh_visible_id() {
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let body = json!({
        "text": "I prefer Rust for tooling", "auto_save": true, "source_id": "fresh-speaker",
    });
    let mut readonly = body.clone();
    readonly["auto_save"] = json!(false);
    let (status, preview) = rest(&f, "/api/extract", readonly, None).await;
    assert_eq!(status, 200);
    assert_eq!(preview["data"]["memories"].as_array().unwrap().len(), 1);
    let content = preview["data"]["memories"][0]["content"].as_str().unwrap();
    let mut expired = Memory::new(
        MemoryType::Preference,
        content.to_string(),
        Priority::Reference,
        SourceAgent {
            id: "old-speaker".into(),
            agent_type: "general".into(),
            session_id: None,
        },
    );
    let bound = chrono::DateTime::parse_from_rfc3339("2020-01-02T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    expired.created_at = bound - chrono::Duration::days(1);
    expired.updated_at = expired.created_at;
    expired.expires_at = Some(bound);
    let expired = f.store.save(expired).await.unwrap();
    let old_before = f.store.get(&expired.id).await.unwrap();
    let search = json!({ "query": content, "namespace": "global" });
    let (status, before) = rest(&f, "/api/search", search.clone(), None).await;
    assert_eq!(status, 200);
    assert!(before["data"].as_array().unwrap().is_empty());

    let (status, response) = rest(&f, "/api/extract", body.clone(), None).await;
    assert_eq!(status, 200);
    let data = &response["data"];
    assert_eq!(data["writes"].as_array().unwrap().len(), 1);
    assert_eq!(data["writes"][0]["status"], "saved");
    let fresh_id = data["saved_ids"][0].as_str().unwrap();
    assert_eq!(data["writes"][0]["id"], fresh_id);
    assert_ne!(fresh_id, expired.id);
    let fresh = f.store.get(fresh_id).await.unwrap();
    assert_eq!(fresh.content, content);
    assert_eq!(fresh.namespace, "global");
    assert_eq!(fresh.source_agent.id, "fresh-speaker");
    assert!(fresh.expires_at.is_none());
    let (status, visible) = rest(&f, "/api/search", search, None).await;
    assert_eq!(status, 200);
    assert_eq!(visible["data"].as_array().unwrap().len(), 1);
    assert_eq!(visible["data"][0]["memory"]["id"], fresh_id);

    // The identical retry dedups to the new live row, never the expired one.
    let (status, repeat) = rest(&f, "/api/extract", body, None).await;
    assert_eq!(status, 200);
    assert_eq!(repeat["data"]["saved_ids"][0], fresh_id);
    assert_eq!(repeat["data"]["writes"][0]["status"], "skipped");
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 2);
    let old_after = f.store.get(&expired.id).await.unwrap();
    assert_eq!(old_after.expires_at, Some(bound));
    assert_eq!(
        serde_json::to_value(old_after).unwrap(),
        serde_json::to_value(old_before).unwrap()
    );
}

#[tokio::test]
async fn phase2_extract_verification_is_bound_to_checked_actor() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let (_, response) = rest(&f, "/api/extract", json!({
        "text": "I prefer Rust for tooling", "auto_save": true, "source_id": "bob", "identity_verified": true,
    }), Some("alice")).await;
    let id = response["data"]["saved_ids"][0].as_str().unwrap();
    let row = f.store.get(id).await.unwrap();
    assert_eq!(row.source_agent.id, "bob");
    assert!(!row.identity_verified);
    assert!(row.corroborating_agents.is_empty());
    let (_, same_actor) = rest(
        &f,
        "/api/extract",
        json!({
            "text": "I prefer green terminal themes", "auto_save": true, "source_id": "alice",
        }),
        Some("alice"),
    )
    .await;
    assert!(
        f.store
            .get(same_actor["data"]["saved_ids"][0].as_str().unwrap())
            .await
            .unwrap()
            .identity_verified
    );
    let response = mcp_extract(&f, json!({
        "text": "I prefer keyboard navigation", "auto_save": true, "agent_id": "alice", "api_key": "test-alice-key",
    })).await;
    assert!(
        f.store
            .get(response["writes"][0]["id"].as_str().unwrap())
            .await
            .unwrap()
            .identity_verified
    );
}

struct FakeEmbedding {
    calls: AtomicUsize,
    success_calls: usize,
    empty: bool,
}

#[async_trait::async_trait]
impl EmbeddingProvider for FakeEmbedding {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.empty {
            Ok(Vec::new())
        } else if call < self.success_calls {
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        } else {
            Err(MemVaultError::Embedding("offline fake failure".to_string()))
        }
    }

    fn dimension(&self) -> usize {
        3
    }
}

fn fake(success_calls: usize, empty: bool) -> Arc<FakeEmbedding> {
    Arc::new(FakeEmbedding {
        calls: AtomicUsize::new(0),
        success_calls,
        empty,
    })
}

#[tokio::test]
async fn phase2_readonly_extraction_does_not_warm_or_write() {
    let provider = fake(usize::MAX, false);
    let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
    let f = fixture(lazy.clone(), false).await;
    let body = json!({ "text": "I prefer Rust", "auto_save": false });
    rest(&f, "/api/extract", body.clone(), None).await;
    mcp_extract(&f, body).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!lazy.ready().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn phase2_invalid_requests_neither_warm_nor_write() {
    let provider = fake(usize::MAX, false);
    let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
    let f = fixture(lazy.clone(), false).await;
    for body in [
        json!({ "content": " " }),
        json!({ "content": "valid text", "occurred_at": "wrong" }),
        json!({ "content": "valid text", "expires_at": "wrong" }),
    ] {
        assert_eq!(rest(&f, "/api/memories", body.clone(), None).await.0, 400);
        assert!(
            f.mcp
                .save_memory(Parameters(serde_json::from_value(body).unwrap()))
                .await
                .is_err()
        );
    }
    let body = json!({ "text": "I prefer Rust", "auto_save": true, "occurred_at": "wrong" });
    assert_eq!(rest(&f, "/api/extract", body.clone(), None).await.0, 400);
    assert!(
        f.mcp
            .extract_memories(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .is_err()
    );
    assert!(!lazy.ready().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn phase2_embedding_failure_and_empty_report_pending() {
    for empty in [false, true] {
        let f = fixture(LazyEmbedder::fixed(Some(fake(0, empty))), false).await;
        let (_, response) = rest(
            &f,
            "/api/memories",
            json!({ "content": "first vectorless payload" }),
            None,
        )
        .await;
        assert_eq!(response["data"]["embedded"], false);
        assert_eq!(response["data"]["vector_pending"], true);
        let response = mcp_save(
            &f,
            json!({ "content": "second unrelated payload", "force_insert": true }),
        )
        .await;
        assert_eq!(response["embedded"], false);
        assert_eq!(response["vector_pending"], true);
        for row in f.store.list(None, 10, 0).await.unwrap() {
            assert!(f.store.get_embedding(&row.id).await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn phase2_duplicate_reports_existing_vector_when_provider_fails() {
    let f = fixture(LazyEmbedder::fixed(Some(fake(0, false))), false).await;
    let row = f
        .store
        .save_with_embedding(
            Memory::new(
                MemoryType::Fact,
                "retained embedding on repeated content".to_string(),
                Priority::Reference,
                SourceAgent {
                    id: "unknown".to_string(),
                    agent_type: "general-assistant".to_string(),
                    session_id: None,
                },
            ),
            vec![0.1, 0.2, 0.3],
        )
        .await
        .unwrap();
    let (_, response) = rest(&f, "/api/memories", json!({ "content": row.content }), None).await;
    assert_eq!(response["data"]["action"], "skipped");
    assert_eq!(response["data"]["embedded"], true);
    assert_eq!(response["data"]["vector_pending"], false);
    let response = mcp_save(&f, json!({ "content": row.content })).await;
    assert_eq!(response["status"], "skipped");
    assert_eq!(response["embedded"], true);
    assert_eq!(response["id"], row.id);
}

#[tokio::test]
async fn phase2_duplicate_backfills_actual_outcome_row() {
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let (_, response) = rest(
        &f,
        "/api/memories",
        json!({ "content": "retry this vectorless payload" }),
        None,
    )
    .await;
    let id = response["data"]["id"].as_str().unwrap();
    let mcp = MemVaultMcp::new(
        f.store.clone(),
        f.mcp.router.clone(),
        LazyEmbedder::fixed(Some(fake(usize::MAX, false))),
        None,
    );
    let result = tool_json(
        mcp.save_memory(Parameters(
            serde_json::from_value(json!({ "content": "retry this vectorless payload" })).unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(result["status"], "skipped");
    assert_eq!(result["id"], id);
    assert_eq!(result["embedded"], true);
    assert!(f.store.get_embedding(id).await.unwrap().is_some());
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn phase2_merge_diagnostics_ignore_discarded_precomputed_vector() {
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let first = mcp_save(&f, json!({ "content": "metadata refresh with no vector" })).await;
    let provider = fake(1, false);
    let mcp = MemVaultMcp::new(
        f.store.clone(),
        f.mcp.router.clone(),
        LazyEmbedder::fixed(Some(provider)),
        None,
    );
    let response = tool_json(
        mcp.save_memory(Parameters(
            serde_json::from_value(json!({
                "content": "metadata refresh with no vector", "tags": ["updated"],
            }))
            .unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(response["status"], "merged");
    assert_eq!(response["id"], first["id"]);
    assert_eq!(response["embedded"], false);
    assert_eq!(response["vector_pending"], true);
    assert!(
        f.store
            .get_embedding(first["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn phase2_extraction_embeds_both_transports_and_duplicate_results() {
    let f = fixture(LazyEmbedder::fixed(Some(fake(usize::MAX, false))), false).await;
    let body = json!({ "text": "I prefer reproducible builds", "auto_save": true,
        "agent_id": "unknown", "source_id": "unknown" });
    let (_, response) = rest(&f, "/api/extract", body.clone(), None).await;
    let first = &response["data"]["writes"][0];
    assert_eq!(first["embedded"], true);
    let repeat = mcp_extract(&f, body).await;
    assert_eq!(repeat["writes"][0]["id"], first["id"]);
    assert_eq!(repeat["writes"][0]["status"], "skipped");
    assert_eq!(repeat["writes"][0]["embedded"], true);
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
}

struct RelationExtractor {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmExtractor for RelationExtractor {
    async fn extract(&self, _: &str) -> Result<Vec<memvault_core::extractor::ExtractedMemory>> {
        Ok(vec![memvault_core::extractor::ExtractedMemory {
            content: "PostgreSQL supports JSONB".to_string(),
            instruction: None,
            memory_type: MemoryType::Fact,
            priority: Priority::Reference,
            tags: Vec::new(),
            confidence: 0.8,
        }])
    }

    async fn extract_relations(
        &self,
        _: &str,
    ) -> Result<Vec<memvault_core::llm_extractor::ExtractedRelation>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![memvault_core::llm_extractor::ExtractedRelation {
            subject: "PostgreSQL".to_string(),
            predicate: "supports".to_string(),
            object: "JSONB".to_string(),
        }])
    }
}

struct RelationsEnv(Option<std::ffi::OsString>);

impl Drop for RelationsEnv {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(value) => std::env::set_var("MEMVAULT_RELATIONS", value),
                None => std::env::remove_var("MEMVAULT_RELATIONS"),
            }
        }
    }
}

#[tokio::test]
async fn phase2_optional_relations_use_actual_dedup_target_on_both_transports() {
    let _lock = RELATIONS_TEST_LOCK.lock().await;
    let _env = RelationsEnv(std::env::var_os("MEMVAULT_RELATIONS"));
    unsafe { std::env::set_var("MEMVAULT_RELATIONS", "on") };
    let llm = Arc::new(RelationExtractor {
        calls: AtomicUsize::new(0),
    });
    let f = fixture_with_llm(LazyEmbedder::fixed(None), false, Some(llm.clone())).await;
    let source = mcp_save(&f, json!({ "content": "PostgreSQL supports JSONB" })).await;
    let body = json!({ "text": "relation context", "mode": "llm", "auto_save": true });
    let (_, response) = rest(&f, "/api/extract", body.clone(), None).await;
    assert_eq!(response["data"]["writes"][0]["id"], source["id"]);
    assert_eq!(response["data"]["relations"]["stored"], 1);
    let result = mcp_extract(&f, body).await;
    assert_eq!(result["writes"][0]["id"], source["id"]);
    assert_eq!(result["relations"]["stored"], 0);
    let entity = f
        .store
        .list(None, 10, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.memory_type == MemoryType::Entity)
        .unwrap();
    let relations = f.store.relations_of_subject(&entity.id).await.unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(
        relations[0].source_memory_id.as_deref(),
        source["id"].as_str()
    );
    let readonly = json!({ "text": "relation context", "mode": "llm", "auto_save": false });
    rest(&f, "/api/extract", readonly.clone(), None).await;
    mcp_extract(&f, readonly).await;
    assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
}

struct InvalidBatch;

#[async_trait::async_trait]
impl LlmExtractor for InvalidBatch {
    async fn extract(&self, _: &str) -> Result<Vec<memvault_core::extractor::ExtractedMemory>> {
        Ok(["valid first candidate", " "]
            .iter()
            .map(|content| memvault_core::extractor::ExtractedMemory {
                content: content.to_string(),
                instruction: None,
                memory_type: MemoryType::Fact,
                priority: Priority::Reference,
                tags: Vec::new(),
                confidence: 0.8,
            })
            .collect())
    }
}

#[tokio::test]
async fn phase2_invalid_extracted_batch_preflight_prevents_partial_write() {
    let provider = fake(usize::MAX, false);
    let f = fixture_with_llm(
        LazyEmbedder::fixed(Some(provider.clone())),
        false,
        Some(Arc::new(InvalidBatch)),
    )
    .await;
    let body = json!({ "text": "batch", "mode": "llm", "auto_save": true });
    assert!(
        f.mcp
            .extract_memories(Parameters(serde_json::from_value(body.clone()).unwrap()))
            .await
            .is_err()
    );
    assert_eq!(rest(&f, "/api/extract", body, None).await.0, 400);
    assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn phase2_search_diagnostics_preserve_empty_arrays_and_defaults() {
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let response = f
        .client
        .post(format!("{}/api/search", f.base))
        .json(&json!({ "query": "no hits" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["x-memvault-search-mode"], "keyword");
    assert_eq!(response.json::<Value>().await.unwrap()["data"], json!([]));
    let result = f
        .mcp
        .search_memory(Parameters(
            serde_json::from_value(json!({ "query": "no hits" })).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(tool_json(result.clone()), json!([]));
    let diagnostic = result.structured_content.unwrap();
    assert_eq!(diagnostic["search_mode"], "keyword");
    assert_eq!(
        diagnostic["fallback_reason"],
        "embedding_provider_unavailable"
    );
}

#[tokio::test]
async fn phase2_query_embedding_failure_really_falls_back() {
    for empty in [false, true] {
        let f = fixture(LazyEmbedder::fixed(Some(fake(0, empty))), false).await;
        mcp_save(&f, json!({ "content": "keyword fallback evidence" })).await;
        let response = f
            .client
            .post(format!("{}/api/search", f.base))
            .json(&json!({
                "query": "fallback evidence", "mode": "semantic",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["x-memvault-search-mode"], "keyword");
        let expected_reason = if empty {
            "query_embedding_empty"
        } else {
            "query_embedding_failed"
        };
        assert_eq!(
            response.headers()["x-memvault-search-fallback"],
            expected_reason
        );
        assert!(
            !response.json::<Value>().await.unwrap()["data"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let result = f
            .mcp
            .search_memory(Parameters(
                serde_json::from_value(json!({
                    "query": "fallback evidence", "mode": "semantic",
                }))
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(
            result.structured_content.as_ref().unwrap()["fallback_reason"],
            expected_reason
        );
        assert!(!tool_json(result).as_array().unwrap().is_empty());
    }
}

fn assertion(key: &str, value: &str, order: u64) -> Value {
    json!({ "key": key, "value": value, "source_namespace": "profile:v1",
        "source_order": order, "cardinality": "single" })
}

fn fact_body(value: &str, order: u64) -> Value {
    json!({ "content": format!("The current quota is {value}"), "agent_id": "alice",
        "fact_assertion": assertion("account:alice/quota", value, order), "fact_write": "resolve" })
}

/// Assert the real wire decision/error boundary while leaving transport-specific
/// legacy envelopes intact. Returned JSON is always the server's decision.
async fn fact_write(f: &Fixture, transport: &str, mut body: Value, actor: &str) -> (Value, bool) {
    if transport == "rest" {
        let (status, response) = rest(f, "/api/memories", body, Some(actor)).await;
        let refused = response["ok"] == false;
        if refused {
            assert_eq!(
                status.as_u16(),
                if response["data"]["fact_outcome"] == "unsupported" {
                    400
                } else {
                    409
                },
                "{response}"
            );
        } else {
            assert_eq!(status, 200, "{response}");
        }
        (response["data"].clone(), refused)
    } else {
        body["agent_id"] = json!(actor);
        body["api_key"] = json!(format!("test-{actor}-key"));
        let result = f
            .mcp
            .save_memory(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .unwrap();
        let refused = result.is_error == Some(true);
        (tool_json(result), refused)
    }
}

#[test]
fn phase3_local_schema_and_defaults_are_explicit_and_non_authoritative() {
    let params: SaveMemoryParams = serde_json::from_value(json!({"content":"legacy"})).unwrap();
    assert!(params.fact_assertion.is_none());
    assert!(matches!(params.fact_write, FactWriteInput::Preserve));
    let schema = serde_json::to_value(schemars::schema_for!(SaveMemoryParams)).unwrap();
    let properties = schema["properties"].as_object().unwrap();
    assert!(properties.contains_key("fact_assertion"));
    assert!(properties.contains_key("fact_write"));
    for forbidden in [
        "fact_management",
        "fact_context",
        "identity_verified",
        "producer_id",
    ] {
        assert!(!properties.contains_key(forbidden));
    }
    for bad in [
        json!({"content":"valid", "fact_write":"replace"}),
        json!({"content":"valid", "fact_assertion":{"key":"x", "value":"y", "source_namespace":"s", "source_order":-1, "cardinality":"single"}}),
        json!({"content":"valid", "fact_assertion":{"key":"x", "value":"y", "source_namespace":"s", "source_order":1, "cardinality":"single", "producer_id":"alice"}}),
    ] {
        assert!(serde_json::from_value::<SaveMemoryParams>(bad).is_err());
    }
}

#[tokio::test]
async fn phase3_resolve_parity_current_history_retry_stale_and_conflict() {
    for transport in ["rest", "mcp"] {
        let f = fixture(LazyEmbedder::fixed(None), true).await;
        let (first, error) = fact_write(&f, transport, fact_body("5", 5), "alice").await;
        assert!(!error);
        assert_eq!(first["fact_outcome"], "inserted");
        let status_key = if transport == "rest" {
            "action"
        } else {
            "status"
        };
        assert_eq!(first[status_key], "saved");
        assert_eq!(first["identity_verified"], true);
        assert_eq!(first["memory"]["identity_verified"], true);
        assert_eq!(first["written"], true);
        assert_eq!(first["fact_management"]["producer_id"], "alice");
        assert_eq!(first["fact_management"]["state"], "active");
        let (second, error) = fact_write(&f, transport, fact_body("9", 9), "alice").await;
        assert!(!error);
        assert_eq!(second["fact_outcome"], "superseded");
        assert_eq!(second[status_key], "saved");
        assert_eq!(second["superseded_id"], first["id"]);
        assert_ne!(second["id"], first["id"]);
        assert_eq!(second["memory"]["fact_assertion"]["value"], "9");
        let old = f.store.get(first["id"].as_str().unwrap()).await.unwrap();
        assert_eq!(old.superseded_by.as_deref(), second["id"].as_str());
        assert!(
            f.store
                .get_history_snapshot(&old.id)
                .await
                .unwrap()
                .is_some()
        );
        let current = f.store.get(second["id"].as_str().unwrap()).await.unwrap();
        let before = serde_json::to_value(&current).unwrap();
        let (retry, error) = fact_write(&f, transport, fact_body("9", 9), "alice").await;
        assert!(!error);
        assert_eq!(retry["fact_outcome"], "idempotent");
        assert_eq!(retry[status_key], "skipped");
        assert_eq!(retry["written"], false);
        assert_eq!(retry["id"], second["id"]);
        let (stale, error) = fact_write(&f, transport, fact_body("5", 5), "alice").await;
        assert!(!error);
        assert_eq!(stale["fact_outcome"], "stale");
        assert_eq!(stale[status_key], "skipped");
        assert_eq!(stale["written"], false);
        assert_eq!(stale["id"], second["id"]);
        assert_eq!(stale["memory"]["fact_assertion"]["value"], "9");
        let (conflict, error) = fact_write(&f, transport, fact_body("10", 9), "alice").await;
        assert!(error);
        assert_eq!(conflict["fact_outcome"], "conflict");
        assert_eq!(conflict["written"], false);
        assert_eq!(conflict["id"], second["id"]);
        assert_eq!(conflict["memory"]["fact_assertion"]["value"], "9");
        assert_eq!(
            serde_json::to_value(f.store.get(&current.id).await.unwrap()).unwrap(),
            before
        );
        let (status, search) = rest(
            &f,
            "/api/search",
            json!({"query":"current quota", "mode":"keyword"}),
            Some("alice"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(search["data"].as_array().unwrap().len(), 1);
        assert_eq!(search["data"][0]["memory"]["id"], second["id"]);
        assert_eq!(search["data"][0]["memory"]["identity_verified"], true);
        assert_eq!(
            search["data"][0]["memory"]["fact_management"]["state"],
            "active"
        );
    }
}

#[tokio::test]
async fn phase3_preserve_default_and_force_resolve_are_literal_unmanaged() {
    for transport in ["rest", "mcp"] {
        let f = fixture(LazyEmbedder::fixed(None), true).await;
        let mut body = fact_body("5", 5);
        body.as_object_mut().unwrap().remove("fact_write");
        let (first, _) = fact_write(&f, transport, body.clone(), "alice").await;
        let (repeat, _) = fact_write(&f, transport, body, "alice").await;
        assert_eq!(first["fact_outcome"], "inserted");
        assert_ne!(first["id"], repeat["id"]);
        assert!(first["fact_management"].is_null());
        let mut forced = fact_body("9", 9);
        forced["force_insert"] = json!(true);
        forced["fact_management"] = json!({"producer_id":"alice", "state":"active"});
        forced["identity_verified"] = json!(true);
        let (forced, error) = fact_write(&f, transport, forced, "alice").await;
        assert!(!error);
        assert_eq!(forced["fact_outcome"], "inserted");
        assert!(forced["fact_management"].is_null());
        let rows = f.store.list(None, 10, 0).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .all(|m| m.superseded_by.is_none() && m.fact_management.is_none())
        );
        let (ambiguous, error) = fact_write(&f, transport, fact_body("12", 12), "alice").await;
        assert!(error);
        assert_eq!(ambiguous["fact_outcome"], "ambiguity");
        assert!(ambiguous["id"].is_null());
        assert!(ambiguous["memory"].is_null());
    }
}

#[tokio::test]
async fn phase3_body_flags_and_unknown_or_unverified_ids_never_authorize_resolve() {
    for keyed in [false, true] {
        let provider = fake(usize::MAX, false);
        let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
        let f = fixture(lazy.clone(), keyed).await;
        let mut body = fact_body("9", 9);
        body["agent_id"] = json!("unregistered");
        body["api_key"] = json!("unverified-arbitrary-key");
        body["identity_verified"] = json!(true);
        body["producer_id"] = json!("alice");
        body["fact_context"] = json!({"producer_id":"alice"});
        body["fact_management"] = json!({"producer_id":"alice","state":"active"});
        let (status, response) = rest(&f, "/api/memories", body.clone(), None).await;
        assert_eq!(status, 409);
        assert_eq!(response["ok"], false);
        assert_eq!(response["data"]["fact_outcome"], "review_required");
        assert!(response["data"]["id"].is_null());
        let result = f
            .mcp
            .save_memory(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(tool_json(result)["fact_outcome"], "review_required");
        assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
        assert!(!lazy.ready().await);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let mut body = fact_body("5", 5);
    body["api_key"] = json!("wrong-key");
    assert_eq!(rest(&f, "/api/memories", body.clone(), None).await.0, 401);
    assert!(
        f.mcp
            .save_memory(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .is_err()
    );
    // A currently registered producer does not retroactively verify a legacy
    // historical write. Every output reports the stored flag, not the registry.
    let historical = f
        .store
        .save(Memory::new(
            MemoryType::Fact,
            "historical producer identity evidence".into(),
            Priority::Reference,
            SourceAgent {
                id: "alice".into(),
                agent_type: "coding-assistant".into(),
                session_id: None,
            },
        ))
        .await
        .unwrap();
    assert!(!historical.identity_verified);
    let (status, search) = rest(
        &f,
        "/api/search",
        json!({"query":"historical producer identity", "mode":"keyword"}),
        Some("alice"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(search["data"][0]["memory"]["identity_verified"], false);
    let response = f
        .client
        .get(format!("{}/api/memories", f.base))
        .header("x-memvault-agent-id", "alice")
        .header("x-memvault-api-key", "test-alice-key")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["data"][0]["identity_verified"],
        false
    );
    let search = f.mcp.search_memory(Parameters(serde_json::from_value(json!({
        "query":"historical producer identity", "mode":"keyword", "agent_id":"alice", "api_key":"test-alice-key",
    })).unwrap())).await.unwrap();
    assert_eq!(tool_json(search)[0]["identity_verified"], false);
}

#[tokio::test]
async fn phase3_header_actor_overrides_body_and_cannot_replace_another_producer() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let (first, _) = fact_write(&f, "rest", fact_body("5", 5), "bob").await;
    assert_eq!(first["memory"]["source_agent"], "bob");
    assert_eq!(first["fact_management"]["producer_id"], "bob");
    let mut spoof = fact_body("9", 9);
    spoof["agent_id"] = json!("bob");
    spoof["identity_verified"] = json!(true);
    spoof["fact_management"] = first["fact_management"].clone();
    let (refused, error) = fact_write(&f, "rest", spoof, "alice").await;
    assert!(error);
    assert_eq!(refused["fact_outcome"], "conflict");
    assert_eq!(refused["id"], first["id"]);
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
    let (legitimate, error) = fact_write(&f, "rest", fact_body("9", 9), "bob").await;
    assert!(!error);
    assert_eq!(legitimate["fact_outcome"], "superseded");
    assert_eq!(legitimate["fact_management"]["producer_id"], "bob");
}

#[tokio::test]
async fn phase3_protected_and_non_single_streams_refuse_without_provider_work() {
    for patch in [
        json!({"priority":"MUST"}),
        json!({"human_reviewed":true}),
        json!({"ai_generated":false}),
        json!({"instruction":"Keep the manually selected quota"}),
        json!({"type":"skill"}),
        json!({"visibility":"shared"}),
        json!({"type":"episode"}),
    ] {
        let f = fixture(LazyEmbedder::fixed(None), true).await;
        let mut body = fact_body("5", 5);
        body["fact_write"] = json!("preserve");
        for (key, value) in patch.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (first, _) = fact_write(&f, "rest", body, "alice").await;
        let provider = fake(usize::MAX, false);
        let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
        let mcp = MemVaultMcp::new(f.store.clone(), f.mcp.router.clone(), lazy.clone(), None);
        let mut body = fact_body("9", 9);
        body["api_key"] = json!("test-alice-key");
        let result = mcp
            .save_memory(Parameters(serde_json::from_value(body).unwrap()))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true), "{patch}");
        let result = tool_json(result);
        assert_eq!(result["fact_outcome"], "review_required", "{patch}");
        assert_eq!(result["id"], first["id"]);
        assert!(!lazy.ready().await);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
    }
    for cardinality in ["unknown", "multi", "event"] {
        let f = fixture(LazyEmbedder::fixed(None), true).await;
        let mut body = fact_body("9", 9);
        body["fact_assertion"]["cardinality"] = json!(cardinality);
        for transport in ["rest", "mcp"] {
            let (result, error) = fact_write(&f, transport, body.clone(), "alice").await;
            assert!(error);
            assert_eq!(result["fact_outcome"], "review_required");
            assert!(result["id"].is_null());
        }
        assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn phase3_namespace_source_namespace_and_qualifiers_scope_fact_identity() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let mut ids = Vec::new();
    for (namespace, source_namespace, key) in [
        (
            "project:alpha",
            "profile:v1",
            "account:alice/quota;region=eu",
        ),
        (
            "project:beta",
            "profile:v1",
            "account:alice/quota;region=eu",
        ),
        (
            "project:alpha",
            "profile:v2",
            "account:alice/quota;region=eu",
        ),
        (
            "project:alpha",
            "profile:v1",
            "account:alice/quota;region=us",
        ),
    ] {
        let mut body = fact_body("5", 5);
        body["namespace"] = json!(namespace);
        body["fact_assertion"]["source_namespace"] = json!(source_namespace);
        body["fact_assertion"]["key"] = json!(key);
        let (first, error) = fact_write(&f, "mcp", body, "alice").await;
        assert!(!error);
        assert_eq!(first["fact_outcome"], "inserted");
        ids.push(first["id"].as_str().unwrap().to_string());
    }
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        4
    );
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 4);
}

#[tokio::test]
async fn phase3_semantic_rest_edits_clear_assertion_and_suspend_server_guard() {
    for edit in [
        json!({"content":"Human selected a different quota"}),
        json!({"instruction":"Use the manual quota"}),
    ] {
        let f = fixture(LazyEmbedder::fixed(None), true).await;
        let (first, _) = fact_write(&f, "rest", fact_body("5", 5), "alice").await;
        let id = first["id"].as_str().unwrap();
        let response = f
            .client
            .put(format!("{}/api/memories/{id}", f.base))
            .header("x-memvault-agent-id", "alice")
            .header("x-memvault-api-key", "test-alice-key")
            .json(&edit)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let edited: Value = response.json().await.unwrap();
        assert!(edited["data"]["fact_assertion"].is_null());
        assert_eq!(edited["data"]["fact_management"]["state"], "suspended");
        let (refused, error) = fact_write(&f, "rest", fact_body("9", 9), "alice").await;
        assert!(error);
        assert_eq!(refused["fact_outcome"], "review_required");
        assert_eq!(refused["id"], id);
        assert_eq!(refused["memory"]["content"], edited["data"]["content"]);
        let response = f.client.put(format!("{}/api/memories/{id}", f.base))
            .header("x-memvault-agent-id", "alice").header("x-memvault-api-key", "test-alice-key")
            .json(&json!({"fact_assertion":assertion("account:alice/quota", "9", 9), "fact_management":first["fact_management"]}))
            .send().await.unwrap();
        assert_eq!(response.status(), 200);
        let updated: Value = response.json().await.unwrap();
        assert_eq!(updated["data"]["fact_management"]["state"], "suspended");
        let response = f
            .client
            .put(format!("{}/api/memories/{id}", f.base))
            .header("x-memvault-agent-id", "alice")
            .header("x-memvault-api-key", "test-alice-key")
            .json(&json!({"fact_assertion":assertion("", "9", 9)}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn phase3_invalid_assertions_fail_before_embedder_warmup() {
    let provider = fake(usize::MAX, false);
    let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
    let f = fixture(lazy.clone(), true).await;
    let mut cases = vec![json!({"content":"valid", "fact_write":"resolve"})];
    for (field, value) in [
        ("key", "".to_string()),
        ("value", "x".repeat(8193)),
        ("source_namespace", "x".repeat(257)),
        ("value", "api_key=offline-fake-credential".to_string()),
    ] {
        let mut body = fact_body("9", 9);
        body["fact_assertion"][field] = json!(value);
        cases.push(body);
    }
    for body in cases {
        assert_eq!(
            rest(&f, "/api/memories", body.clone(), Some("alice"))
                .await
                .0,
            400,
            "{body}"
        );
        let mut body = body;
        body["agent_id"] = json!("alice");
        body["api_key"] = json!("test-alice-key");
        assert!(
            f.mcp
                .save_memory(Parameters(serde_json::from_value(body).unwrap()))
                .await
                .is_err()
        );
    }
    assert!(!lazy.ready().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn phase3_superseded_diagnostics_use_stored_winner_and_readonly_retries_do_not_embed() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let (first, _) = fact_write(&f, "mcp", fact_body("5", 5), "alice").await;
    let provider = fake(usize::MAX, false);
    let mcp = MemVaultMcp::new(
        f.store.clone(),
        f.mcp.router.clone(),
        LazyEmbedder::fixed(Some(provider.clone())),
        None,
    );
    let write = async |mut body: Value| {
        body["api_key"] = json!("test-alice-key");
        tool_json(
            mcp.save_memory(Parameters(serde_json::from_value(body).unwrap()))
                .await
                .unwrap(),
        )
    };
    let second = write(fact_body("9", 9)).await;
    assert_eq!(second["fact_outcome"], "superseded");
    assert_eq!(second["embedded"], true);
    assert_eq!(second["vector_pending"], false);
    assert_ne!(second["id"], first["id"]);
    assert!(
        f.store
            .get_embedding(second["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        f.store
            .get_embedding(first["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let count = provider.calls.load(Ordering::SeqCst);
    for (value, order, outcome) in [
        ("9", 9, "idempotent"),
        ("5", 5, "stale"),
        ("10", 9, "conflict"),
    ] {
        let result = write(fact_body(value, order)).await;
        assert_eq!(result["fact_outcome"], outcome);
        assert_eq!(result["id"], second["id"]);
        assert_eq!(result["embedded"], true);
        assert_eq!(result["written"], false);
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), count);
}

#[tokio::test]
async fn phase3_markdown_unsupported_neither_creates_memory_files_nor_warms_embedder() {
    let dir = std::env::temp_dir().join(format!("memvault-mcp-facts-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(
        memvault_core::storage::markdown::MarkdownStore::open(&dir)
            .await
            .unwrap(),
    );
    let provider = fake(usize::MAX, false);
    let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
    let service = crate::write_service::WriteService::new(
        store.clone(),
        lazy.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let mut memory = Memory::new(
        MemoryType::Fact,
        "The current quota is 9".into(),
        Priority::Reference,
        SourceAgent {
            id: "alice".into(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        },
    );
    memory.fact_assertion =
        Some(serde_json::from_value(assertion("account:alice/quota", "9", 9)).unwrap());
    let mut before: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    before.sort();
    let report = service
        .save(
            memory,
            &crate::write_service::WriteActor::authenticated("alice", true),
            false,
            memvault_core::facts::FactWriteMode::Resolve,
        )
        .await
        .unwrap();
    assert!(report.unsupported());
    let json = report.to_json();
    assert_eq!(json["fact_outcome"], "unsupported");
    assert!(json["id"].is_null());
    assert!(json["memory"].is_null());
    assert!(store.list(None, 10, 0).await.unwrap().is_empty());
    let mut after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    after.sort();
    assert_eq!(before, after);
    assert!(!lazy.ready().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    drop(service);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn phase3_explicit_extraction_mapping_refuses_alias_and_never_calls_relations_on_refusal() {
    let _lock = RELATIONS_TEST_LOCK.lock().await;
    let _env = RelationsEnv(std::env::var_os("MEMVAULT_RELATIONS"));
    unsafe { std::env::set_var("MEMVAULT_RELATIONS", "on") };
    for transport in ["rest", "mcp"] {
        let llm = Arc::new(RelationExtractor {
            calls: AtomicUsize::new(0),
        });
        let provider = fake(usize::MAX, false);
        let lazy = LazyEmbedder::delayed(provider.clone(), std::time::Duration::from_millis(1));
        let f = fixture_with_llm(lazy.clone(), true, Some(llm.clone())).await;
        let mut body = json!({"text":"explicit fact extraction", "mode":"llm", "auto_save":true,
            "fact_write":"resolve", "fact_assertions":[assertion("database/supports", "JSONB", 1)]});
        if transport == "rest" {
            body["source_id"] = json!("bob");
            let (status, result) = rest(&f, "/api/extract", body, Some("alice")).await;
            assert_eq!(status, 409);
            assert_eq!(
                result["data"]["writes"][0]["fact_outcome"],
                "review_required"
            );
            assert!(result["data"]["saved_ids"][0].is_null());
            assert!(result["data"]["relations"].is_null());
        } else {
            body["agent_id"] = json!("unregistered");
            body["identity_verified"] = json!(true);
            let result = f
                .mcp
                .extract_memories(Parameters(serde_json::from_value(body).unwrap()))
                .await
                .unwrap();
            assert_eq!(result.is_error, Some(true));
            let result = tool_json(result);
            assert_eq!(result["writes"][0]["fact_outcome"], "review_required");
            assert!(result["memories"][0]["saved_id"].is_null());
            assert!(result["relations"].is_null());
        }
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(!lazy.ready().await);
        assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn phase3_extraction_assertions_are_optional_and_never_generated_from_text() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let body = json!({"text":"I prefer Rust for tooling\nI always use dark mode", "auto_save":true,
        "source_id":"alice", "agent_id":"alice", "api_key":"test-alice-key"});
    let (status, result) = rest(&f, "/api/extract", body.clone(), Some("alice")).await;
    assert_eq!(status, 200);
    assert_eq!(result["data"]["writes"].as_array().unwrap().len(), 2);
    let repeated = mcp_extract(&f, body.clone()).await;
    assert_eq!(repeated["writes"].as_array().unwrap().len(), 2);
    for row in f.store.list(None, 10, 0).await.unwrap() {
        assert!(row.fact_assertion.is_none());
        assert!(row.fact_management.is_none());
    }
    let mut invalid = body;
    invalid["fact_write"] = json!("resolve");
    assert_eq!(
        rest(&f, "/api/extract", invalid.clone(), Some("alice"))
            .await
            .0,
        400
    );
    assert!(
        f.mcp
            .extract_memories(Parameters(serde_json::from_value(invalid).unwrap()))
            .await
            .is_err()
    );
    let readonly = json!({"text":"I prefer Rust for tooling", "auto_save":false, "fact_write":"resolve",
        "fact_assertions":[assertion("preference/language", "Rust", 1)]});
    let (status, result) = rest(&f, "/api/extract", readonly, Some("alice")).await;
    assert_eq!(status, 200);
    assert!(result["data"]["saved_ids"].as_array().unwrap().is_empty());
    assert_eq!(
        result["data"]["memories"][0]["fact_assertion"]["value"],
        "Rust"
    );
}

#[tokio::test]
async fn phase3_verified_explicit_extraction_resolves_only_supplied_fact_stream() {
    let _lock = RELATIONS_TEST_LOCK.lock().await;
    let _env = RelationsEnv(std::env::var_os("MEMVAULT_RELATIONS"));
    unsafe { std::env::set_var("MEMVAULT_RELATIONS", "off") };
    for transport in ["rest", "mcp"] {
        let llm = Arc::new(RelationExtractor {
            calls: AtomicUsize::new(0),
        });
        let f = fixture_with_llm(LazyEmbedder::fixed(None), true, Some(llm.clone())).await;
        let extract = async |order: u64| {
            let body = json!({"text":"explicit database fact", "mode":"llm", "auto_save":true,
                "source_id":"alice", "agent_id":"alice", "api_key":"test-alice-key", "fact_write":"resolve",
                "fact_assertions":[assertion("database/supports", "JSONB", order)]});
            if transport == "rest" {
                let (status, result) = rest(&f, "/api/extract", body, Some("alice")).await;
                assert_eq!(status, 200);
                result["data"].clone()
            } else {
                mcp_extract(&f, body).await
            }
        };
        let first = extract(5).await;
        assert_eq!(first["writes"][0]["fact_outcome"], "inserted");
        let second = extract(9).await;
        assert_eq!(second["writes"][0]["fact_outcome"], "superseded");
        assert_eq!(
            second["writes"][0]["superseded_id"],
            first["writes"][0]["id"]
        );
        assert_eq!(second["writes"][0]["fact_assertion"]["source_order"], 9);
        let retry = extract(9).await;
        assert_eq!(retry["writes"][0]["fact_outcome"], "idempotent");
        assert_eq!(retry["writes"][0]["id"], second["writes"][0]["id"]);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        let rows = f.store.list(None, 10, 0).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().filter(|m| m.superseded_by.is_none()).count(), 1);
    }
}

struct IdentityRecordingEnv(Option<std::ffi::OsString>);

impl Drop for IdentityRecordingEnv {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(value) => std::env::set_var("MEMVAULT_IDENTITY_VERIFICATION", value),
                None => std::env::remove_var("MEMVAULT_IDENTITY_VERIFICATION"),
            }
        }
    }
}

#[tokio::test]
async fn phase3_key_proof_is_independent_of_identity_recording_policy() {
    // Environment-mutating regressions run with --test-threads=1 and share the
    // extraction environment lock. Restore both flags even after a panic.
    let _lock = RELATIONS_TEST_LOCK.lock().await;
    let _identity_env = IdentityRecordingEnv(std::env::var_os("MEMVAULT_IDENTITY_VERIFICATION"));
    let _relations_env = RelationsEnv(std::env::var_os("MEMVAULT_RELATIONS"));
    unsafe {
        std::env::set_var("MEMVAULT_RELATIONS", "off");
        std::env::set_var("MEMVAULT_IDENTITY_VERIFICATION", "off");
    }
    for transport in ["rest", "mcp"] {
        let llm = Arc::new(RelationExtractor {
            calls: AtomicUsize::new(0),
        });
        let f = fixture_with_llm(LazyEmbedder::fixed(None), true, Some(llm)).await;
        assert!(
            f.mcp
                .router
                .authenticate_agent_key_checked("alice", Some("test-alice-key"))
                .unwrap()
                .1
        );
        assert!(
            !f.mcp
                .router
                .authenticate_agent_verified("alice", Some("test-alice-key"))
                .unwrap()
                .1
        );
        assert!(
            !f.mcp
                .router
                .authenticate_agent_key_checked("unregistered", Some("test-alice-key"))
                .unwrap()
                .1
        );
        let (first, error) = fact_write(&f, transport, fact_body("5", 5), "alice").await;
        assert!(!error, "{first}");
        assert_eq!(first["fact_outcome"], "inserted");
        assert_eq!(first["identity_verified"], false);
        assert_eq!(first["memory"]["identity_verified"], false);
        assert_eq!(first["fact_management"]["producer_id"], "alice");
        assert_eq!(first["fact_management"]["state"], "active");
        let (second, error) = fact_write(&f, transport, fact_body("9", 9), "alice").await;
        assert!(!error, "{second}");
        assert_eq!(second["fact_outcome"], "superseded");
        assert_eq!(second["identity_verified"], false);
        assert_eq!(second["fact_management"]["state"], "active");
        let before =
            serde_json::to_value(f.store.get(second["id"].as_str().unwrap()).await.unwrap())
                .unwrap();

        let (status, search) = rest(
            &f,
            "/api/search",
            json!({"query":"current quota", "mode":"keyword"}),
            Some("alice"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(search["data"][0]["memory"]["id"], second["id"]);
        assert_eq!(search["data"][0]["memory"]["identity_verified"], false);
        assert_eq!(
            search["data"][0]["memory"]["fact_management"]["state"],
            "active"
        );
        let search = f.mcp.search_memory(Parameters(serde_json::from_value(json!({
            "query":"current quota", "mode":"keyword", "agent_id":"alice", "api_key":"test-alice-key",
        })).unwrap())).await.unwrap();
        assert_eq!(tool_json(search)[0]["identity_verified"], false);
        let response = f
            .client
            .get(format!("{}/api/memories", f.base))
            .header("x-memvault-agent-id", "alice")
            .header("x-memvault-api-key", "test-alice-key")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let rows = response.json::<Value>().await.unwrap();
        assert!(
            rows["data"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["identity_verified"] == false)
        );

        // Enabling recording cannot retrospectively attest an idempotent head.
        unsafe { std::env::set_var("MEMVAULT_IDENTITY_VERIFICATION", "on") };
        let (retry, error) = fact_write(&f, transport, fact_body("9", 9), "alice").await;
        assert!(!error);
        assert_eq!(retry["fact_outcome"], "idempotent");
        assert_eq!(retry["written"], false);
        assert_eq!(retry["identity_verified"], false);
        assert_eq!(
            serde_json::to_value(f.store.get(second["id"].as_str().unwrap()).await.unwrap())
                .unwrap(),
            before
        );
        let (recorded, error) = fact_write(&f, transport, fact_body("12", 12), "alice").await;
        assert!(!error);
        assert_eq!(recorded["fact_outcome"], "superseded");
        assert_eq!(recorded["identity_verified"], true);
        assert!(
            !f.store
                .get(first["id"].as_str().unwrap())
                .await
                .unwrap()
                .identity_verified
        );
        assert!(
            !f.store
                .get(second["id"].as_str().unwrap())
                .await
                .unwrap()
                .identity_verified
        );
        unsafe { std::env::set_var("MEMVAULT_IDENTITY_VERIFICATION", "off") };
        let (retry, error) = fact_write(&f, transport, fact_body("12", 12), "alice").await;
        assert!(!error);
        assert_eq!(retry["fact_outcome"], "idempotent");
        assert_eq!(retry["identity_verified"], true);
        let (unrecorded, error) = fact_write(&f, transport, fact_body("15", 15), "alice").await;
        assert!(!error);
        assert_eq!(unrecorded["fact_outcome"], "superseded");
        assert_eq!(unrecorded["identity_verified"], false);
        assert_eq!(unrecorded["fact_management"]["state"], "active");
        assert!(
            f.store
                .get(recorded["id"].as_str().unwrap())
                .await
                .unwrap()
                .identity_verified
        );

        // Ordinary writes keep the legacy public recording policy.
        let ordinary = json!({"content":"ordinary keyed write without public attestation", "agent_id":"alice",
            "api_key":"test-alice-key", "force_insert":true, "identity_verified":true});
        let ordinary = if transport == "rest" {
            rest(&f, "/api/memories", ordinary, Some("alice")).await.1["data"].clone()
        } else {
            mcp_save(&f, ordinary).await
        };
        let row = f.store.get(ordinary["id"].as_str().unwrap()).await.unwrap();
        assert!(!row.identity_verified);
        assert!(row.corroborating_agents.is_empty());
        assert!(row.fact_management.is_none());

        let extraction = json!({"text":"explicit database fact", "mode":"llm", "auto_save":true,
            "source_id":"alice", "agent_id":"alice", "api_key":"test-alice-key", "fact_write":"resolve",
            "fact_assertions":[assertion("database/supports", "JSONB", 1)]});
        let extract = async |body: Value| {
            if transport == "rest" {
                let (status, result) = rest(&f, "/api/extract", body, Some("alice")).await;
                assert_eq!(status, 200, "{result}");
                result["data"].clone()
            } else {
                mcp_extract(&f, body).await
            }
        };
        let first_extracted = extract(extraction.clone()).await;
        assert_eq!(first_extracted["writes"][0]["fact_outcome"], "inserted");
        assert_eq!(first_extracted["writes"][0]["identity_verified"], false);
        assert_eq!(
            first_extracted["writes"][0]["fact_management"]["state"],
            "active"
        );
        let mut next_extraction = extraction;
        next_extraction["fact_assertions"][0]["source_order"] = json!(2);
        let next_extracted = extract(next_extraction).await;
        assert_eq!(next_extracted["writes"][0]["fact_outcome"], "superseded");
        assert_eq!(next_extracted["writes"][0]["identity_verified"], false);
        assert_eq!(
            next_extracted["writes"][0]["fact_management"]["producer_id"],
            "alice"
        );

        let count = f.store.list(None, 100, 0).await.unwrap().len();
        for key in [None, Some("wrong-key")] {
            let mut invalid = fact_body("20", 20);
            invalid["identity_verified"] = json!(true);
            invalid["fact_management"] = unrecorded["fact_management"].clone();
            if let Some(key) = key {
                invalid["api_key"] = json!(key);
            }
            assert_eq!(
                rest(&f, "/api/memories", invalid.clone(), None).await.0,
                401
            );
            assert!(
                f.mcp
                    .save_memory(Parameters(serde_json::from_value(invalid.clone()).unwrap()))
                    .await
                    .is_err()
            );
            invalid["text"] = json!("explicit database fact");
            invalid["mode"] = json!("llm");
            invalid["auto_save"] = json!(true);
            invalid["fact_assertions"] = json!([assertion("database/supports", "JSONB", 3)]);
            let mut request = f
                .client
                .post(format!("{}/api/extract", f.base))
                .header("x-memvault-agent-id", "alice")
                .json(&invalid);
            if let Some(key) = key {
                request = request.header("x-memvault-api-key", key);
            }
            assert_eq!(request.send().await.unwrap().status(), 401);
            assert!(
                f.mcp
                    .extract_memories(Parameters(serde_json::from_value(invalid).unwrap()))
                    .await
                    .is_err()
            );
        }
        let mut spoof = fact_body("20", 20);
        spoof["agent_id"] = json!("unregistered");
        spoof["identity_verified"] = json!(true);
        spoof["key_authenticated"] = json!(true);
        spoof["record_identity"] = json!(true);
        spoof["producer_id"] = json!("alice");
        spoof["fact_context"] = json!({"producer_id":"alice"});
        spoof["fact_management"] = unrecorded["fact_management"].clone();
        let (status, refusal) = rest(&f, "/api/memories", spoof.clone(), None).await;
        assert_eq!(status, 409);
        assert_eq!(refusal["data"]["fact_outcome"], "review_required");
        let refusal = f
            .mcp
            .save_memory(Parameters(serde_json::from_value(spoof).unwrap()))
            .await
            .unwrap();
        assert_eq!(refusal.is_error, Some(true));
        assert_eq!(tool_json(refusal)["fact_outcome"], "review_required");
        assert_eq!(f.store.list(None, 100, 0).await.unwrap().len(), count);
    }
    let f = fixture(LazyEmbedder::fixed(None), false).await;
    let mut body = fact_body("5", 5);
    body["identity_verified"] = json!(true);
    body["fact_context"] = json!({"producer_id":"alice"});
    assert_eq!(rest(&f, "/api/memories", body.clone(), None).await.0, 409);
    let refusal = f
        .mcp
        .save_memory(Parameters(serde_json::from_value(body).unwrap()))
        .await
        .unwrap();
    assert_eq!(refusal.is_error, Some(true));
    assert!(f.store.list(None, 10, 0).await.unwrap().is_empty());
}

struct RequestFactExtractor;

#[async_trait::async_trait]
impl LlmExtractor for RequestFactExtractor {
    async fn extract(
        &self,
        context: &str,
    ) -> Result<Vec<memvault_core::extractor::ExtractedMemory>> {
        Ok(vec![memvault_core::extractor::ExtractedMemory {
            content: context.to_string(),
            instruction: None,
            memory_type: MemoryType::Fact,
            priority: Priority::Reference,
            tags: Vec::new(),
            confidence: 0.8,
        }])
    }
}

#[tokio::test]
async fn phase3_resolved_extraction_cross_transport_retry_preserves_strict_payload() {
    let _lock = RELATIONS_TEST_LOCK.lock().await;
    let _env = RelationsEnv(std::env::var_os("MEMVAULT_RELATIONS"));
    unsafe { std::env::set_var("MEMVAULT_RELATIONS", "off") };
    for transports in [["rest", "mcp"], ["mcp", "rest"]] {
        let f = fixture_with_llm(
            LazyEmbedder::fixed(None),
            true,
            Some(Arc::new(RequestFactExtractor)),
        )
        .await;
        let body = json!({"text":"PostgreSQL supports JSONB", "mode":"llm", "auto_save":true,
            "source_id":"alice", "agent_id":"alice", "api_key":"test-alice-key", "fact_write":"resolve",
            "fact_assertions":[assertion("database/supports", "JSONB", 5)],
            "agent_type":"untrusted-client-type", "source_agent":{"id":"bob", "agent_type":"forged"}});
        let extract = async |transport: &str, body: Value| {
            if transport == "rest" {
                let (status, result) = rest(&f, "/api/extract", body, Some("alice")).await;
                assert_eq!(
                    status.as_u16(),
                    if result["ok"] == false { 409 } else { 200 }
                );
                (result["data"].clone(), result["ok"] == false)
            } else {
                let result = f
                    .mcp
                    .extract_memories(Parameters(serde_json::from_value(body).unwrap()))
                    .await
                    .unwrap();
                let refused = result.is_error == Some(true);
                (tool_json(result), refused)
            }
        };
        let (first, error) = extract(transports[0], body.clone()).await;
        assert!(!error);
        assert_eq!(first["writes"][0]["fact_outcome"], "inserted");
        let id = first["writes"][0]["id"].as_str().unwrap();
        let row = f.store.get(id).await.unwrap();
        assert_eq!(row.source_agent.id, "alice");
        assert_eq!(row.fact_management.as_ref().unwrap().producer_id(), "alice");
        let before = serde_json::to_value(&row).unwrap();
        let (retry, error) = extract(transports[1], body.clone()).await;
        assert!(!error, "{retry}");
        assert_eq!(retry["writes"][0]["fact_outcome"], "idempotent");
        assert_eq!(retry["writes"][0]["status"], "skipped");
        assert_eq!(retry["writes"][0]["written"], false);
        assert_eq!(retry["writes"][0]["id"], id);
        if transports[1] == "rest" {
            assert_eq!(retry["saved_ids"][0], id);
        } else {
            assert_eq!(retry["memories"][0]["saved_id"], id);
        }
        assert_eq!(
            f.store.get(id).await.unwrap().source_agent.agent_type,
            "extractor"
        );
        let mut changed = body.clone();
        changed["text"] = json!("PostgreSQL supports JSONB with a different body");
        let (conflict, error) = extract(transports[1], changed).await;
        assert!(error);
        assert_eq!(conflict["writes"][0]["fact_outcome"], "conflict");
        assert_eq!(conflict["writes"][0]["written"], false);
        assert_eq!(conflict["writes"][0]["id"], id);
        assert_eq!(
            serde_json::to_value(f.store.get(id).await.unwrap()).unwrap(),
            before
        );

        // Canonicalization is opt-in for resolved structured extraction only.
        for transport in ["rest", "mcp"] {
            for structured in [false, true] {
                let mut preserve = body.clone();
                preserve["text"] = json!(format!(
                    "legacy {transport} extraction structured={structured}"
                ));
                preserve.as_object_mut().unwrap().remove("fact_write");
                if structured {
                    preserve["fact_assertions"][0]["key"] = json!(format!("legacy/{transport}"));
                } else {
                    preserve.as_object_mut().unwrap().remove("fact_assertions");
                }
                // Isolate ordinary intake so legacy fuzzy dedup cannot return
                // another transport's already-persisted source type.
                let legacy = fixture_with_llm(
                    LazyEmbedder::fixed(None),
                    true,
                    Some(Arc::new(RequestFactExtractor)),
                )
                .await;
                let saved = if transport == "rest" {
                    let (status, saved) =
                        rest(&legacy, "/api/extract", preserve, Some("alice")).await;
                    assert_eq!(status, 200);
                    saved["data"].clone()
                } else {
                    mcp_extract(&legacy, preserve).await
                };
                let row = legacy
                    .store
                    .get(saved["writes"][0]["id"].as_str().unwrap())
                    .await
                    .unwrap();
                assert_eq!(row.source_agent.id, "alice");
                assert_eq!(
                    row.source_agent.agent_type,
                    if transport == "rest" {
                        "web-dashboard"
                    } else {
                        "extractor"
                    }
                );
                assert!(row.fact_management.is_none());
                assert_eq!(row.fact_assertion.is_some(), structured);
            }
        }
    }
}

#[tokio::test]
async fn phase3_reviewing_managed_fact_suspends_guard_before_later_resolve() {
    let f = fixture(LazyEmbedder::fixed(None), true).await;
    let (first, _) = fact_write(&f, "rest", fact_body("5", 5), "alice").await;
    let id = first["id"].as_str().unwrap();
    let (status, _) = rest(
        &f,
        &format!("/api/inbox/{id}/approve"),
        json!({}),
        Some("alice"),
    )
    .await;
    assert_eq!(status, 200);
    let reviewed = f.store.get(id).await.unwrap();
    assert!(reviewed.human_reviewed);
    assert!(reviewed.fact_assertion.is_none());
    assert_eq!(
        reviewed.fact_management.as_ref().unwrap().state(),
        memvault_core::facts::FactManagementState::Suspended
    );
    let (result, error) = fact_write(&f, "mcp", fact_body("9", 9), "alice").await;
    assert!(error);
    assert_eq!(result["fact_outcome"], "review_required");
    assert_eq!(result["id"], id);
    assert_eq!(f.store.list(None, 10, 0).await.unwrap().len(), 1);
}
