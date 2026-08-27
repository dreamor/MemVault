use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Deserializer, Serialize};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

use memvault_core::agent_adapt::{self, InjectFormat};
use memvault_core::compliance::ComplianceStore;
use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::embedding::EmbeddingProvider;
use memvault_core::error::MemVaultError;
use memvault_core::extractor::Extractor;
use memvault_core::models::*;
use memvault_core::promote::{PromoteConfig, Promoter};
use memvault_core::rerank::{MultiSignalReranker, RerankConfig};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use metrics_exporter_prometheus::PrometheusHandle;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    metrics_handle: PrometheusHandle,
    /// Optional embedding provider. When present, saves embed (int8) and
    /// `semantic`/`hybrid` search modes become available.
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    /// Same reranker MCP's `search_memory` already applies after merge —
    /// REST used to return raw merge order, giving REST/MCP callers
    /// different rankings for identical queries.
    reranker: MultiSignalReranker,
    /// Optional contextual (LLM-based) extractor, used to reflect failed
    /// task outcomes into lessons (`POST /api/outcome`). Absent → rule-based
    /// reflection only.
    llm_extractor: Option<Arc<dyn memvault_core::llm_extractor::LlmExtractor>>,
}

// --- Request/Response types ---

#[derive(Deserialize)]
struct SaveRequest {
    content: String,
    #[serde(default = "default_ref")]
    priority: String,
    #[serde(default = "default_fact")]
    r#type: String,
    #[serde(default = "default_global")]
    namespace: String,
    instruction: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_unknown")]
    agent_id: String,
    #[serde(default = "default_general")]
    agent_type: String,
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
    /// Override the review flag for a direct human save (Web Dashboard etc.).
    /// Absent → keep `Memory::new` default (`human_reviewed=false`).
    #[serde(default)]
    human_reviewed: Option<bool>,
    /// Override the AI-generated flag. Absent → keep default (`ai_generated=true`).
    #[serde(default)]
    ai_generated: Option<bool>,
    /// Skill fields (type=skill): trigger pattern, execution steps, verification.
    skill_trigger: Option<String>,
    #[serde(default)]
    skill_steps: Vec<String>,
    skill_verification: Option<String>,
    /// Sharing scope: "scoped" (default) or "shared" (team pool).
    visibility: Option<String>,
}

#[derive(Deserialize)]
struct SupersedeRequest {
    replacement_id: String,
}

#[derive(Deserialize)]
struct OutcomeRequest {
    task: String,
    /// success | failure | partial
    status: String,
    cause: Option<String>,
    task_type: Option<String>,
    /// Skill memory followed during the task — attributes the outcome to the
    /// skill's success/failure statistics.
    skill_id: Option<String>,
    #[serde(default = "default_global")]
    namespace: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_unknown")]
    agent_id: String,
    #[serde(default = "default_general")]
    agent_type: String,
    session_id: Option<String>,
    api_key: Option<String>,
}

fn default_ref() -> String {
    "REFERENCE".into()
}
fn default_fact() -> String {
    "fact".into()
}
fn default_global() -> String {
    "global".into()
}
fn default_unknown() -> String {
    "unknown".into()
}
fn default_general() -> String {
    "general-assistant".into()
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    /// Search mode: "keyword" (default), "semantic", or "hybrid"
    #[serde(default = "default_keyword")]
    mode: String,
    #[serde(default = "default_10")]
    top_k: usize,
    namespace: Option<String>,
    agent_id: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
    /// Attach each result's one-hop relation neighborhood (C5). Default off.
    #[serde(default)]
    expand_relations: bool,
}

fn default_keyword() -> String {
    "keyword".into()
}

fn default_10() -> usize {
    10
}

#[derive(Deserialize)]
struct SessionRequest {
    agent_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    agent_type: Option<String>,
    context_hint: Option<String>,
    project: Option<String>,
    /// Override injection format: "must_ref", "xml", "system_prompt", "markdown"
    format: Option<String>,
    /// API key for agent authentication (required if agent has a registered key)
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct ListQuery {
    namespace: Option<String>,
    #[serde(default = "default_100")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_100() -> usize {
    100
}

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    fn success(data: T) -> Json<Self> {
        Json(Self {
            ok: true,
            data: Some(data),
            error: None,
        })
    }
}

fn api_error(msg: impl ToString) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.to_string()),
        }),
    )
}

/// Map a domain error to a meaningful HTTP status: auth failures are 401,
/// not-found is 404, everything else stays 500. Previously every error was
/// flattened to 500, hiding auth failures from clients.
fn http_error(err: MemVaultError) -> (StatusCode, Json<ApiResponse<()>>) {
    let status = match &err {
        MemVaultError::NotFound(_) => StatusCode::NOT_FOUND,
        MemVaultError::Auth(_) => StatusCode::UNAUTHORIZED,
        MemVaultError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(err.to_string()),
        }),
    )
}

/// Header carrying the agent id for admin-style routes that have no natural
/// agent_id in their body/query (delete, dedup/decay/promote, compliance, ...).
/// Falls back to `DEFAULT_ADMIN_AGENT` so unconfigured deployments keep working.
const ADMIN_AGENT_HEADER: &str = "x-memvault-agent-id";
const API_KEY_HEADER: &str = "x-memvault-api-key";
const DEFAULT_ADMIN_AGENT: &str = "admin";

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Authenticate an admin-style request against the agent identified by
/// `X-MemVault-Agent-Id` (default `"admin"`) using `X-MemVault-Api-Key`.
/// A no-op if that agent has no registered key (back-compat default).
fn authenticate_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<ApiResponse<()>>)> {
    let agent_id = header_str(headers, ADMIN_AGENT_HEADER).unwrap_or(DEFAULT_ADMIN_AGENT);
    let api_key = header_str(headers, API_KEY_HEADER);
    state
        .router
        .authenticate_agent(agent_id, api_key)
        .map_err(http_error)?;
    Ok(())
}

/// Full JSON representation of a memory, shared by every handler that returns
/// memory records (list/search/update) so clients see a consistent shape —
/// list/search previously returned partial, inconsistent field sets.
fn memory_to_json(m: &Memory) -> serde_json::Value {
    serde_json::json!({
        "id": m.id,
        "content": m.content,
        "instruction": m.instruction,
        "priority": format!("{:?}", m.priority),
        "type": format!("{:?}", m.memory_type),
        "tags": m.tags,
        "namespace": m.namespace,
        "layer": format!("{:?}", m.layer),
        "human_reviewed": m.human_reviewed,
        "ai_generated": m.ai_generated,
        "confidence": m.confidence,
        "access_count": m.access_count,
        "decay_score": m.decay_score,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
        "source_agent": m.source_agent.id,
        "visibility": m.visibility.as_str(),
        "superseded_by": m.superseded_by,
        "skill_meta": m.skill_meta.as_ref().map(|sm| serde_json::json!({
            "trigger": sm.trigger,
            "steps": sm.steps,
            "verification": sm.verification,
            "version": sm.version,
        })),
    })
}

// --- Handlers ---

async fn health() -> &'static str {
    "ok"
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics_handle.render()
}

async fn save_memory(
    State(state): State<AppState>,
    Json(req): Json<SaveRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    // Authenticate the agent
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(http_error)?;

    let priority = match req.priority.to_uppercase().as_str() {
        "MUST" => Priority::Must,
        "BACKGROUND" => Priority::Background,
        _ => Priority::Reference,
    };
    let memory_type = match req.r#type.to_lowercase().as_str() {
        "preference" => MemoryType::Preference,
        "episode" => MemoryType::Episode,
        "entity" => MemoryType::Entity,
        "skill" => MemoryType::Skill,
        _ => MemoryType::Fact,
    };

    let mut mem = Memory::new(
        memory_type,
        req.content,
        priority,
        SourceAgent {
            id: req.agent_id,
            agent_type: req.agent_type,
            session_id: None,
        },
    );
    mem.namespace = req.namespace;
    mem.instruction = req.instruction;
    mem.tags = req.tags;
    // Allow a direct human save (e.g. Web Dashboard "New Memory") to skip the
    // review queue. Absent fields keep `Memory::new` defaults.
    mem.human_reviewed = req.human_reviewed.unwrap_or(mem.human_reviewed);
    mem.ai_generated = req.ai_generated.unwrap_or(mem.ai_generated);

    // Skill metadata (parity with the MCP save_memory tool): a skill saved
    // without its trigger/steps is inert — trigger matching has nothing to
    // match and injection has no procedure to render.
    if req.skill_trigger.is_some()
        || !req.skill_steps.is_empty()
        || req.skill_verification.is_some()
    {
        mem.skill_meta = Some(memvault_core::models::SkillMeta {
            trigger: req.skill_trigger,
            steps: req.skill_steps,
            verification: req.skill_verification,
            version: 1,
        });
    }
    if let Some(ref v) = req.visibility {
        mem.visibility = memvault_core::models::Visibility::parse(v);
    }

    // 保存时优先嵌入 (与 MCP 路径 / proxy 一致):embedder 可用时
    // 生成 int8 向量写入,保证该记忆能被 feature 路径召回;失败则降级无向量保存。
    let embed_text = mem
        .instruction
        .clone()
        .unwrap_or_else(|| mem.content.clone())
        .to_string();
    let mut embedded = false;

    let saved = if let Some(ref embedder) = state.embedder {
        match embedder.embed(&[embed_text]).await {
            Ok(embeddings) if !embeddings.is_empty() => {
                embedded = true;
                state
                    .store
                    .save_with_embedding(mem, embeddings.into_iter().next().unwrap())
                    .await
                    .map_err(http_error)?
            }
            Err(e) => {
                tracing::warn!("Auto-embedding failed, saving without: {}", e);
                state.store.save(mem).await.map_err(http_error)?
            }
            _ => state.store.save(mem).await.map_err(http_error)?,
        }
    } else {
        state.store.save(mem).await.map_err(http_error)?
    };

    metrics::counter!("memvault_memories_saved_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({
        "id": saved.id,
        "embedded": embedded,
    })))
}

async fn record_outcome(
    State(state): State<AppState>,
    Json(req): Json<OutcomeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(http_error)?;

    let status = match OutcomeStatus::parse(&req.status) {
        Some(s) => s,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    ok: false,
                    data: None,
                    error: Some(format!(
                        "invalid status '{}' — expected success, failure, or partial",
                        req.status
                    )),
                }),
            ));
        }
    };

    let input = memvault_core::episode::OutcomeInput {
        task: req.task,
        status,
        cause: req.cause,
        task_type: req.task_type,
        skill_id: req.skill_id,
        tags: req.tags,
        namespace: req.namespace,
        source_agent: SourceAgent {
            id: req.agent_id,
            agent_type: req.agent_type,
            session_id: req.session_id,
        },
    };

    let recorded = memvault_core::episode::record_outcome(
        state.store.as_ref(),
        input.clone(),
        state.embedder.as_deref(),
    )
    .await
    .map_err(http_error)?;

    // Reflection is best-effort: a failed lesson must not lose the outcome.
    let mut lesson_json = serde_json::Value::Null;
    match memvault_core::reflection::reflect_and_store(
        state.store.as_ref(),
        &recorded.memory.id,
        input.into(),
        state.llm_extractor.as_deref(),
        state.embedder.as_deref(),
    )
    .await
    {
        Ok(Some(record)) => {
            lesson_json = serde_json::json!({
                "lesson": record.lesson,
                "source": format!("{:?}", record.source).to_lowercase(),
                "memory_id": record.lesson_memory.id,
                "escalation_hint": record.escalation_hint,
            });
        }
        Ok(None) => {}
        Err(e) => warn!(error = %e, "lesson reflection failed; outcome kept"),
    }

    metrics::counter!("memvault_outcomes_recorded_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({
        "id": recorded.memory.id,
        "outcome": recorded.memory.content,
        "embedded": recorded.embedded,
        "lesson": lesson_json,
        "flagged_skills": recorded.flagged_skills,
        "skill_draft_id": recorded.skill_draft_id,
    })))
}

async fn supersede_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<SupersedeRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;
    state
        .store
        .supersede(&id, &req.replacement_id)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "superseded": id,
        "replacement_id": req.replacement_id,
    })))
}

async fn list_episodes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let status = match params.get("status") {
        Some(s) => match OutcomeStatus::parse(s) {
            Some(st) => Some(st),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse {
                        ok: false,
                        data: None,
                        error: Some(format!(
                            "invalid status '{}' — expected success, failure, or partial",
                            s
                        )),
                    }),
                ));
            }
        },
        None => None,
    };

    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(50)
        .min(500);

    let filter = EpisodeFilter {
        task_type: params.get("task_type").cloned(),
        status,
        namespace: params.get("namespace").cloned(),
        limit,
    };

    let episodes = state
        .store
        .list_episodes(filter)
        .await
        .map_err(http_error)?;

    let data: Vec<serde_json::Value> = episodes
        .iter()
        .map(|e| {
            serde_json::json!({
                "memory_id": e.memory_id,
                "task": e.task,
                "task_type": e.task_type,
                "status": e.status.as_str(),
                "cause": e.cause,
                "lesson": e.lesson,
                "lesson_memory_id": e.lesson_memory_id,
                "occurred_at": e.occurred_at,
            })
        })
        .collect();

    Ok(ApiResponse::success(serde_json::json!({
        "episodes": data,
        "count": data.len(),
    })))
}

async fn search_memories(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    // Authenticate the agent if agent_id was provided
    if let Some(ref agent_id) = req.agent_id {
        state
            .router
            .authenticate_agent(agent_id, req.api_key.as_deref())
            .map_err(http_error)?;
    }

    // 检索模式分发对齐 MCP search_memory:keyword / semantic / hybrid。
    // 先决定最终模式:semantic/hybrid 需要 embedder,不可用时**真正**降级执行
    // keyword 检索(而非返回空结果),并在响应中如实上报 search_mode。
    let mode = req.mode.to_lowercase();
    let has_embedder = state.embedder.is_some();
    let actual_mode = if (mode == "semantic" || mode == "hybrid") && !has_embedder {
        "keyword"
    } else {
        mode.as_str()
    };

    let keyword_results = if actual_mode != "semantic" {
        state
            .store
            .search(SearchQuery {
                query: req.query.clone(),
                top_k: req.top_k,
                namespace: req.namespace.clone(),
                agent_id: req.agent_id.clone(),
                ..SearchQuery::new(String::new())
            })
            .await
            .map_err(http_error)?
            .results
    } else {
        Vec::new()
    };

    let vector_results = if actual_mode != "keyword" {
        if let Some(ref embedder) = state.embedder {
            match embedder.embed(std::slice::from_ref(&req.query)).await {
                Ok(embeddings) if !embeddings.is_empty() => state
                    .store
                    .vector_search(&embeddings[0], req.top_k, req.namespace.as_deref())
                    .await
                    .map_err(http_error)?,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let results = match actual_mode {
        "keyword" => keyword_results,
        "semantic" => vector_results,
        _ => memvault_core::hybrid::HybridMerger::merge(
            keyword_results,
            vector_results,
            req.top_k,
            0.4,
            0.6,
        ),
    };

    // Mirror MCP's search_memory: apply the same overlap/recency/authority
    // signals after merge instead of returning raw FTS/RRF order.
    let results = state
        .reranker
        .rerank(&req.query, results, &chrono::Utc::now());

    metrics::counter!("memvault_searches_total").increment(1);

    // C5: optionally expand each result's one-hop relation neighborhood.
    let mut relations_by_id: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    if req.expand_relations {
        for r in &results {
            let rels =
                memvault_core::relations::collect_relations(state.store.as_ref(), &r.memory.id)
                    .await;
            if !rels.is_empty() {
                relations_by_id.insert(
                    r.memory.id.clone(),
                    rels.iter()
                        .map(|rel| {
                            serde_json::json!({
                                "subject_id": rel.subject_id,
                                "predicate": rel.predicate,
                                "object_id": rel.object_id,
                                "object_text": rel.object_text,
                                "line": memvault_core::relations::relation_line(
                                    &r.memory.content, rel
                                ),
                            })
                        })
                        .collect(),
                );
            }
        }
    }

    let output: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let sources = r.hit_sources.iter().map(|h| h.tag()).collect::<Vec<_>>();
            let mut obj = serde_json::json!({
                "memory": memory_to_json(&r.memory),
                "score": r.score,
                "search_mode": actual_mode,
                "hit_sources": sources,
            });
            if req.expand_relations
                && let Some(rels) = relations_by_id.get(&r.memory.id)
            {
                obj["relations"] = serde_json::json!(rels);
            }
            obj
        })
        .collect();

    Ok(ApiResponse::success(output))
}

async fn session_start(
    State(state): State<AppState>,
    Json(req): Json<SessionRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    // Authenticate the agent
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(http_error)?;

    let injection = state
        .router
        .session_start(
            &req.agent_id,
            req.context_hint.as_deref(),
            req.project.as_deref(),
        )
        .await
        .map_err(http_error)?;
    let results = injection.results;

    metrics::counter!("memvault_sessions_started_total").increment(1);

    // Determine injection format
    let profile = state.router.get_agent_profile(&req.agent_id);
    let format = req
        .format
        .as_deref()
        .map(|f| match f {
            "xml" => InjectFormat::Xml,
            "system_prompt" => InjectFormat::SystemPrompt,
            "markdown" => InjectFormat::Markdown,
            _ => InjectFormat::MustRef,
        })
        .unwrap_or_else(|| agent_adapt::best_format_for_agent(&profile.agent_type));

    let formatted = agent_adapt::format_memories(&results, format);

    // Track injected memories for compliance
    let inject_session_id = if let Some(ref cs) = state.compliance {
        let sid = format!("inj_{}", Uuid::new_v4().simple());
        for r in &results {
            let _ = cs
                .record_injection(&sid, &r.memory.id, &r.memory.priority, &req.agent_id)
                .await;
        }
        Some(sid)
    } else {
        None
    };

    let mut response = serde_json::json!({
        "formatted": formatted,
        "count": results.len(),
        "format": format!("{:?}", format),
        "agent_profile": profile.id,
        // Why candidates were NOT injected — auditable injection decisions.
        "skipped": injection
            .skipped
            .iter()
            .map(|s| serde_json::json!({ "id": s.id, "reason": format!("{}", s.reason) }))
            .collect::<Vec<_>>(),
    });
    if let Some(sid) = inject_session_id {
        response["inject_session_id"] = serde_json::json!(sid);
    }

    Ok(ApiResponse::success(response))
}

async fn list_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let memories = state
        .store
        .list(params.namespace.as_deref(), params.limit, params.offset)
        .await
        .map_err(http_error)?;

    let output: Vec<serde_json::Value> = memories.iter().map(memory_to_json).collect();

    Ok(ApiResponse::success(output))
}

async fn delete_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    state.store.delete(&id).await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "deleted": id })))
}

/// General-purpose patch for an existing memory. Every field is optional; omitted fields
/// are left untouched. Unlike `/api/inbox/{id}/edit`, this is not scoped to the review flow
/// and does not affect `human_reviewed`.
/// Distinguish three states per field:
/// - field absent        → `None`      (leave untouched)
/// - field set to `null` → `Some(None)` (clear the field, when clearable)
/// - field set          → `Some(Some(v))` (update with `v`)
///
/// Plain `Option<T>` cannot tell `null` apart from "absent", so clients like
/// the Obsidian plugin that send `instruction: patch.instruction || null`
/// silently failed to clear fields — the old value was retained forever.
fn deserialize_clearable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    Option<T>: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

#[derive(Deserialize)]
struct UpdateRequest {
    #[serde(default, deserialize_with = "deserialize_clearable")]
    content: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    instruction: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    priority: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    r#type: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    tags: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    namespace: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    layer: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    skill_trigger: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    skill_steps: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    skill_verification: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    visibility: Option<Option<String>>,
}

fn bad_request(msg: String) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            ok: false,
            data: None,
            error: Some(msg),
        }),
    )
}

/// Parse a priority string, rejecting unknown values with a 400 instead of
/// silently degrading a MUST memory to Reference.
fn parse_priority(v: &str) -> Result<Priority, (StatusCode, Json<ApiResponse<()>>)> {
    match v.to_uppercase().as_str() {
        "MUST" => Ok(Priority::Must),
        "REFERENCE" => Ok(Priority::Reference),
        "BACKGROUND" => Ok(Priority::Background),
        _ => Err(bad_request(format!("invalid priority: {v}"))),
    }
}

/// Parse a memory-type field, rejecting unknown values with a 400 instead of
/// silently turning a typed memory into a Fact.
fn parse_memory_type(v: &str) -> Result<MemoryType, (StatusCode, Json<ApiResponse<()>>)> {
    match v.to_lowercase().as_str() {
        "preference" => Ok(MemoryType::Preference),
        "episode" => Ok(MemoryType::Episode),
        "entity" => Ok(MemoryType::Entity),
        "skill" => Ok(MemoryType::Skill),
        "fact" => Ok(MemoryType::Fact),
        _ => Err(bad_request(format!("invalid memory type: {v}"))),
    }
}

fn parse_memory_layer(v: &str) -> Result<MemoryLayer, (StatusCode, Json<ApiResponse<()>>)> {
    match v.to_uppercase().as_str() {
        "L0" => Ok(MemoryLayer::L0),
        "L1" => Ok(MemoryLayer::L1),
        "L2" => Ok(MemoryLayer::L2),
        "L3" => Ok(MemoryLayer::L3),
        _ => Err(bad_request(format!("invalid layer: {v}"))),
    }
}

async fn update_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UpdateRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let mut mem = state.store.get(&id).await.map_err(http_error)?;

    // Inner `Some(v)` updates a field; outer `Some(None)` clears it (when the
    // field is clearable); `None` (absent) leaves it untouched. `content` is
    // a non-nullable String, so an explicit null is a bad request, not a
    // silent no-op.
    match req.content {
        Some(Some(content)) => mem.content = content,
        Some(None) => return Err(bad_request("content cannot be cleared".to_string())),
        None => {}
    }
    if let Some(instruction) = req.instruction {
        mem.instruction = instruction;
    }
    if let Some(Some(priority)) = req.priority {
        mem.priority = parse_priority(&priority)?;
    }
    if let Some(Some(t)) = req.r#type {
        mem.memory_type = parse_memory_type(&t)?;
    }
    if let Some(Some(tags)) = req.tags {
        mem.tags = tags;
    }
    if let Some(Some(namespace)) = req.namespace {
        mem.namespace = namespace;
    }
    if let Some(Some(layer)) = req.layer {
        mem.layer = parse_memory_layer(&layer)?;
    }
    if req.skill_trigger.is_some() || req.skill_steps.is_some() || req.skill_verification.is_some()
    {
        let had_meta = mem.skill_meta.is_some();
        let old_meta = mem.skill_meta.clone().unwrap_or_default();
        let mut meta = mem.skill_meta.take().unwrap_or_default();
        if let Some(trigger) = req.skill_trigger {
            meta.trigger = trigger;
        }
        if let Some(Some(steps)) = req.skill_steps {
            meta.steps = steps;
        }
        if let Some(verification) = req.skill_verification {
            meta.verification = verification;
        }
        // Version evolution (B4): a real change to an EXISTING procedure
        // bumps the version and clears the needs-revision flag. First-time
        // skill definition stays at v1. memory_history already snapshots the
        // pre-edit row, so older versions stay restorable.
        if had_meta
            && (meta.trigger != old_meta.trigger
                || meta.steps != old_meta.steps
                || meta.verification != old_meta.verification)
        {
            meta.version = old_meta.version.saturating_add(1);
            mem.tags
                .retain(|t| t != memvault_core::models::NEEDS_REVISION_TAG);
        } else if !had_meta {
            meta.version = 1;
        }
        mem.skill_meta = Some(meta);
    }

    // Visibility: Some(Some(v)) sets it; Some(None) resets to scoped.
    if let Some(v) = req.visibility {
        mem.visibility = v
            .map(|s| memvault_core::models::Visibility::parse(&s))
            .unwrap_or_default();
    }

    mem.updated_at = chrono::Utc::now();
    let updated = state.store.update(mem).await.map_err(http_error)?;
    Ok(ApiResponse::success(memory_to_json(&updated)))
}

async fn extract_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    // 带覆盖面记账:返回四桶计数,与 CLI/MCP 对齐,避免"偷偷丢段"。
    let outcome = Extractor::extract_with_coverage(text);
    let memories: Vec<serde_json::Value> = outcome
        .memories
        .iter()
        .map(|e| {
            serde_json::json!({
                "content": e.content,
                "instruction": e.instruction,
                "type": format!("{:?}", e.memory_type),
                "priority": format!("{:?}", e.priority),
                "tags": e.tags,
                "confidence": e.confidence,
            })
        })
        .collect();
    let cov = outcome.coverage;
    Ok(ApiResponse::success(serde_json::json!({
        "memories": memories,
        "coverage": {
            "input_lines": cov.input_lines,
            "empty_lines": cov.empty_lines,
            "extracted_lines": cov.extracted_lines,
            "no_signal_lines": cov.no_signal_lines,
        },
    })))
}

#[derive(Serialize)]
struct DashboardLayers {
    l0: usize,
    l1: usize,
    l2: usize,
    l3: usize,
}

#[derive(Serialize)]
struct DashboardStats {
    total: usize,
    must_count: usize,
    reference_count: usize,
    reviewed_count: usize,
    agents: Vec<String>,
    namespaces: Vec<String>,
    layers: DashboardLayers,
    skills: usize,
}

/// Aggregate stats for the Web Dashboard (aligns with the desktop view it replaces).
async fn get_dashboard_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let all = state.store.list(None, 10000, 0).await.map_err(http_error)?;

    let total = all.len();
    let must_count = all.iter().filter(|m| m.priority == Priority::Must).count();
    let reference_count = all
        .iter()
        .filter(|m| m.priority == Priority::Reference)
        .count();
    let reviewed_count = all.iter().filter(|m| m.human_reviewed).count();
    let skills = all.iter().filter(|m| m.skill_meta.is_some()).count();

    let layers = DashboardLayers {
        l0: all.iter().filter(|m| m.layer == MemoryLayer::L0).count(),
        l1: all.iter().filter(|m| m.layer == MemoryLayer::L1).count(),
        l2: all.iter().filter(|m| m.layer == MemoryLayer::L2).count(),
        l3: all.iter().filter(|m| m.layer == MemoryLayer::L3).count(),
    };

    let mut agents: Vec<String> = all.iter().map(|m| m.source_agent.id.clone()).collect();
    agents.sort();
    agents.dedup();

    let mut namespaces: Vec<String> = all.iter().map(|m| m.namespace.clone()).collect();
    namespaces.sort();
    namespaces.dedup();

    Ok(ApiResponse::success(DashboardStats {
        total,
        must_count,
        reference_count,
        reviewed_count,
        agents,
        namespaces,
        layers,
        skills,
    }))
}

async fn run_dedup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let dedup = Deduplicator::new(state.store, None);
    let result = dedup.scan(None).await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "unique": result.unique_count,
        "duplicates": result.duplicates.len(),
    })))
}

async fn run_decay(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let dm = DecayManager::new(state.store, DecayConfig::default());
    let report = dm.run_decay().await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "updated": report.updated,
        "archived": report.archived,
    })))
}

#[derive(Deserialize, Default)]
struct PromoteRequest {
    namespace: Option<String>,
    min_l1: Option<usize>,
    min_l2: Option<usize>,
}

async fn run_promote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PromoteRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let _ = req.namespace; // promote pipeline currently scans all namespaces
    let config = PromoteConfig {
        min_l1_for_l2: req.min_l1.unwrap_or(3),
        min_l2_for_l3: req.min_l2.unwrap_or(2),
        ..PromoteConfig::default()
    };
    let promoter = Promoter::new(state.store, config);
    let result = promoter.run().await.map_err(http_error)?;
    metrics::counter!("memvault_promote_runs_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({
        "promoted_to_l2": result.promoted_to_l2,
        "promoted_to_l3": result.promoted_to_l3,
        "consolidated_facts": result.consolidated_facts,
        "merged_entities": result.merged_entities,
        "source_ids_consumed": result.source_ids_consumed,
    })))
}

#[derive(Deserialize)]
struct ConfirmReadRequest {
    memory_ids: Vec<String>,
}

async fn confirm_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ConfirmReadRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    state
        .router
        .confirm_read(&req.memory_ids)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({
        "confirmed": req.memory_ids.len(),
    })))
}

// --- Inbox handlers ---

#[derive(Deserialize)]
struct InboxQuery {
    namespace: Option<String>,
    #[serde(default = "default_50")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_50() -> usize {
    50
}

async fn list_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<InboxQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let memories = state
        .store
        .list_pending(params.namespace.as_deref(), params.limit, params.offset)
        .await
        .map_err(http_error)?;

    let total = memories.len();
    let output: Vec<serde_json::Value> = memories.iter().map(memory_to_json).collect();

    Ok(ApiResponse::success(serde_json::json!({
        "memories": output,
        "total": total,
    })))
}

#[derive(Deserialize)]
struct InboxEditRequest {
    edited_content: Option<String>,
    edited_instruction: Option<String>,
}

async fn approve_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let mut mem = state.store.get(&id).await.map_err(http_error)?;
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    state.store.update(mem).await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "approved": id })))
}

async fn reject_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    state.store.delete(&id).await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "rejected": id })))
}

async fn edit_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<InboxEditRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let mut mem = state.store.get(&id).await.map_err(http_error)?;
    if let Some(content) = req.edited_content {
        mem.content = content;
    }
    if let Some(instruction) = req.edited_instruction {
        mem.instruction = Some(instruction);
    }
    mem.human_reviewed = true;
    mem.updated_at = chrono::Utc::now();
    state.store.update(mem).await.map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!({ "edited": id })))
}

// --- Compliance endpoints ---

#[derive(Deserialize)]
struct ComplianceSessionQuery {
    session_id: String,
}

#[derive(Deserialize)]
struct ComplianceSummaryQuery {
    agent_id: Option<String>,
    #[serde(default = "default_10")]
    limit: usize,
}

async fn get_compliance_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ComplianceSessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let cs = state
        .compliance
        .ok_or_else(|| api_error("Compliance tracking is not enabled"))?;
    let report = cs
        .get_report(&params.session_id)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!(report)))
}

async fn get_compliance_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ComplianceSummaryQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let cs = state
        .compliance
        .ok_or_else(|| api_error("Compliance tracking is not enabled"))?;
    let summary = cs
        .get_summary(params.agent_id.as_deref(), params.limit)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(serde_json::json!(summary)))
}

/// Build the CORS layer. Defaults to localhost-only (127.0.0.1 / localhost, any port) to
/// support local MCP clients (Obsidian, VS Code) without opening the API to arbitrary origins.
/// Set `MEMVAULT_CORS_ORIGIN` to a comma-separated origin list, or `*` to explicitly allow all
/// origins (not recommended outside trusted networks).
fn build_cors_layer() -> CorsLayer {
    match std::env::var("MEMVAULT_CORS_ORIGIN") {
        Ok(val) if val.trim() == "*" => {
            tracing::warn!(
                "MEMVAULT_CORS_ORIGIN=* — REST API accepts requests from any origin. \
                 Only use this on trusted networks."
            );
            CorsLayer::permissive()
        }
        Ok(val) => {
            let origins: Vec<_> = val
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(tower_http::cors::AllowMethods::any())
                .allow_headers(tower_http::cors::AllowHeaders::any())
        }
        Err(_) => {
            tracing::warn!(
                "MEMVAULT_CORS_ORIGIN not set — defaulting to localhost-only CORS. \
                 Set MEMVAULT_CORS_ORIGIN for remote access."
            );
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::predicate(|origin, _| {
                    origin
                        .to_str()
                        .map(|s| {
                            s.starts_with("http://127.0.0.1")
                                || s.starts_with("http://localhost")
                                || s.starts_with("https://127.0.0.1")
                                || s.starts_with("https://localhost")
                        })
                        .unwrap_or(false)
                }))
                .allow_methods(tower_http::cors::AllowMethods::any())
                .allow_headers(tower_http::cors::AllowHeaders::any())
        }
    }
}

/// Build the REST API router.
pub fn build_rest_router(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    metrics_handle: PrometheusHandle,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    llm_extractor: Option<Arc<dyn memvault_core::llm_extractor::LlmExtractor>>,
) -> Router {
    let state = AppState {
        store,
        router,
        compliance,
        metrics_handle,
        embedder,
        reranker: MultiSignalReranker::new(RerankConfig::default()),
        llm_extractor,
    };

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/api/memories", get(list_memories))
        .route("/api/memories", post(save_memory))
        .route("/api/outcome", post(record_outcome))
        .route("/api/episodes", get(list_episodes))
        .route("/api/memories/{id}/supersede", post(supersede_memory))
        .route(
            "/api/memories/{id}",
            delete(delete_memory).put(update_memory),
        )
        .route("/api/search", post(search_memories))
        .route("/api/session", post(session_start))
        .route("/api/extract", post(extract_memories))
        .route("/api/dedup", post(run_dedup))
        .route("/api/decay", post(run_decay))
        .route("/api/promote", post(run_promote))
        .route("/api/stats", get(get_dashboard_stats))
        .route("/api/confirm-read", post(confirm_read))
        // Inbox review endpoints
        .route("/api/inbox", get(list_inbox))
        .route("/api/inbox/{id}/approve", post(approve_memory))
        .route("/api/inbox/{id}/reject", post(reject_memory))
        .route("/api/inbox/{id}/edit", post(edit_memory))
        // Compliance endpoints
        .route("/api/compliance/session", get(get_compliance_session))
        .route("/api/compliance/summary", get(get_compliance_summary))
        .layer(build_cors_layer())
        .with_state(state)
}

/// Mount a built Web Dashboard (Vite `dist/`) at the server root with SPA
/// fallback to `index.html`. Kept as a separate helper (not part of
/// `build_rest_router`) so existing route tests stay untouched; `/api/*`,
/// `/health` and `/metrics` specific routes always take precedence.
pub fn attach_web_assets(app: Router, dir: PathBuf) -> Router {
    if !dir.is_dir() {
        warn!(
            "--serve-web: {} is not a directory; web dashboard not served",
            dir.display()
        );
    }
    // axum 0.8 disallows nest_service at "/" — use fallback_service instead:
    // matched API routes win, everything else falls through to ServeDir, and
    // unknown paths (SPA client routes) fall back to index.html (ServeDir.fallback
    // serves index.html for any unmatched request, unlike not_found_service which
    // only triggers for file-not-found).
    let serve = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
    app.fallback_service(serve)
}

/// Run the REST API server.
pub async fn run_rest_server(
    store: Arc<SqliteStore>,
    router: Arc<MemoryRouter>,
    compliance: Option<Arc<ComplianceStore>>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    llm_extractor: Option<Arc<dyn memvault_core::llm_extractor::LlmExtractor>>,
    port: u16,
    serve_web: Option<PathBuf>,
) -> anyhow::Result<()> {
    let metrics_handle = crate::metrics_setup::install_recorder();
    let app = build_rest_router(
        store,
        router,
        compliance,
        metrics_handle,
        embedder,
        llm_extractor,
    );
    let app = if let Some(ref dir) = serve_web {
        attach_web_assets(app, dir.clone())
    } else {
        app
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    info!("MemVault REST API listening on http://{}", addr);
    if serve_web.is_some() {
        info!("Web dashboard served at http://{}", addr);
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_exporter_prometheus::PrometheusHandle;

    /// The Prometheus recorder can only be installed once per process.
    /// Share a single handle across all tests via OnceLock.
    static METRICS: std::sync::OnceLock<PrometheusHandle> = std::sync::OnceLock::new();
    fn metrics() -> PrometheusHandle {
        METRICS
            .get_or_init(crate::metrics_setup::install_recorder)
            .clone()
    }

    struct TestApp {
        base: String,
        client: reqwest::Client,
    }

    /// Build an in-memory app and serve it on an ephemeral, localhost-only port.
    async fn spawn_app(with_compliance: bool) -> TestApp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let compliance = if with_compliance {
            let db = std::env::temp_dir().join(format!(
                "memvault_mcp_compliance_{}.db",
                Uuid::new_v4().simple()
            ));
            Some(ComplianceStore::new(&db.to_string_lossy()).expect("compliance store"))
        } else {
            None
        };
        let app = build_rest_router(store, router, compliance, metrics(), None, None);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // graceful shutdown future never resolves during the test; the task is
            // cancelled when the test runtime shuts down.
            let _ = axum::serve(listener, app).await;
        });
        TestApp {
            base: format!("http://127.0.0.1:{}", addr.port()),
            client: reqwest::Client::new(),
        }
    }

    /// Same as `spawn_app`, but registers an "admin" agent with the given API key,
    /// so admin-only routes (list/delete/update/inbox/dedup/decay/promote/compliance)
    /// require `X-MemVault-Api-Key` to match.
    #[tokio::test]
    async fn test_save_reports_embedded_flag() {
        // P1 回归:POST /api/memories 响应必须含 `embedded`(无 embedder 时为 false)。
        let app = spawn_app(false).await;
        let (status, body) = save(
            &app,
            serde_json::json!({ "content": "embedded flag check" }),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["data"]["embedded"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn test_search_emits_search_mode_and_hit_sources() {
        // P2 回归:REST 检索必须带 `search_mode` + `hit_sources`(vs:VS Code 等客户端依赖)。
        let app = spawn_app(false).await;
        save(&app, serde_json::json!({ "content": "沙箱环境部署完成了" })).await;

        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "沙箱" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0]["search_mode"], "keyword");
        let hit_sources = results[0]["hit_sources"].as_array().unwrap();
        assert!(
            hit_sources
                .iter()
                .any(|h| h.as_str().unwrap().starts_with("kw#")),
            "keyword search must tag a kw# source, got {:?}",
            hit_sources
        );
    }

    #[tokio::test]
    async fn test_search_expand_relations_after_consolidation() {
        // End-to-end C3→C5: consolidate two near-identical facts (creating
        // consolidated_from provenance relations), then verify search with
        // expand_relations surfaces them.
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({ "content": "the checkout service uses PostgreSQL 15" }),
        )
        .await;
        save(
            &app,
            serde_json::json!({ "content": "the checkout service uses PostgreSQL 15." }),
        )
        .await;

        // Promote consolidates the two facts and records provenance relations.
        let resp = app
            .client
            .post(format!("{}/api/promote", app.base))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["consolidated_facts"], 1);

        // Search without expand_relations → no relations key.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "checkout PostgreSQL" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert!(results.iter().all(|r| r.get("relations").is_none()));

        // Search with expand_relations → the consolidated fact carries its
        // consolidated_from provenance.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "checkout PostgreSQL", "expand_relations": true }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        let with_relations: Vec<&serde_json::Value> = results
            .iter()
            .filter(|r| r.get("relations").is_some())
            .collect();
        assert!(
            !with_relations.is_empty(),
            "the consolidated fact must expose its relations"
        );
        let rels = with_relations[0]["relations"].as_array().unwrap();
        assert!(
            rels.iter()
                .any(|rel| rel["predicate"] == "consolidated_from")
        );
    }

    #[tokio::test]
    async fn test_search_hybrid_without_provider_falls_back_to_keyword() {
        // 语义/混合模式在无 embedder 时必须优雅降级为 keyword 并如实上报 search_mode,
        // 而不是报错或假装做了语义检索(MCP 同规则)。
        let app = spawn_app(false).await;
        save(&app, serde_json::json!({ "content": "深色主题界面" })).await;

        for mode in ["semantic", "hybrid"] {
            let resp = app
                .client
                .post(format!("{}/api/search", app.base))
                .json(&serde_json::json!({ "query": "深色", "mode": mode }))
                .send()
                .await
                .unwrap();
            let body: serde_json::Value = resp.json().await.unwrap();
            assert!(body["ok"].as_bool().unwrap());
            let results = body["data"].as_array().unwrap();
            assert!(!results.is_empty());
            // 降级:实际走 keyword,并在每条结果上如实标注
            for r in results {
                assert_eq!(r["search_mode"], "keyword");
                assert!(r["hit_sources"].is_array());
            }
        }
    }

    async fn spawn_app_with_admin_key(admin_key: &str) -> TestApp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::with_registry(
            store.clone(),
            vec![AgentProfile {
                id: "admin".to_string(),
                agent_type: "general-assistant".to_string(),
                description: "admin".to_string(),
                inject_rules: InjectRules::default(),
                api_key: Some(admin_key.to_string()),
            }],
        ));
        let app = build_rest_router(store, router, None, metrics(), None, None);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        TestApp {
            base: format!("http://127.0.0.1:{}", addr.port()),
            client: reqwest::Client::new(),
        }
    }

    async fn save(
        app: &TestApp,
        body: serde_json::Value,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let resp = app
            .client
            .post(format!("{}/api/memories", app.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let json = resp.json().await.unwrap();
        (status, json)
    }

    fn save_body(content: &str) -> serde_json::Value {
        serde_json::json!({ "content": content })
    }

    #[tokio::test]
    async fn test_outcome_records_episode() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/outcome", app.base))
            .json(&serde_json::json!({
                "task": "deploy the dashboard",
                "status": "failure",
                "cause": "missing env var",
                "task_type": "deploy",
                "agent_id": "tester",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["embedded"], false);
        assert!(
            body["data"]["outcome"]
                .to_string()
                .contains("deploy the dashboard")
        );
        // Rule-based reflection (no LLM configured in tests): the stated
        // cause yields a lesson with source "rule".
        assert_eq!(body["data"]["lesson"]["source"], "rule");
        assert!(
            body["data"]["lesson"]["lesson"]
                .to_string()
                .contains("missing env var")
        );
    }

    #[tokio::test]
    async fn test_outcome_rejects_invalid_status() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/outcome", app.base))
            .json(&serde_json::json!({
                "task": "deploy",
                "status": "maybe",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn test_list_episodes_returns_recorded_outcomes() {
        let app = spawn_app(false).await;
        for (task, status) in [("deploy a", "failure"), ("deploy b", "success")] {
            let resp = app
                .client
                .post(format!("{}/api/outcome", app.base))
                .json(&serde_json::json!({
                    "task": task,
                    "status": status,
                    "task_type": "deploy",
                    "cause": "test cause",
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }

        // All episodes.
        let resp = app
            .client
            .get(format!("{}/api/episodes", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["count"], 2);

        // Filter by status.
        let resp = app
            .client
            .get(format!("{}/api/episodes?status=failure", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["count"], 1);
        assert_eq!(body["data"]["episodes"][0]["task"], "deploy a");
        // The failure had a cause, so rule reflection attached a lesson.
        assert!(body["data"]["episodes"][0]["lesson"].is_string());
    }

    #[tokio::test]
    async fn test_list_episodes_rejects_invalid_status() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/episodes?status=bogus", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_health_returns_ok() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/health", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn test_metrics_endpoint_renders() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/metrics", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert!(resp.text().await.unwrap().contains("memvault"));
    }

    #[tokio::test]
    async fn test_save_memory_with_defaults() {
        let app = spawn_app(false).await;
        let (status, body) = save(&app, save_body("prefers Python")).await;
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        let id = body["data"]["id"].as_str().unwrap();
        assert!(id.starts_with("mem_"));
    }

    #[tokio::test]
    async fn test_save_memory_parses_priority_and_type() {
        let app = spawn_app(false).await;
        let (status, _) = save(
            &app,
            serde_json::json!({
                "content": "always run tests",
                "priority": "MUST",
                "type": "preference",
                "namespace": "project:myapp",
                "instruction": "run before every commit",
                "tags": ["testing"],
            }),
        )
        .await;
        assert_eq!(status, 200);

        let resp = app
            .client
            .get(format!("{}/api/memories?namespace=project:myapp", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = body["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["priority"], "Must");
        assert_eq!(items[0]["type"], "Preference");
        assert_eq!(items[0]["instruction"], "run before every commit");
        assert_eq!(items[0]["tags"], serde_json::json!(["testing"]));
    }

    #[tokio::test]
    async fn test_save_memory_background_and_skill_types() {
        let app = spawn_app(false).await;
        let (status, _) = save(
            &app,
            serde_json::json!({ "content": "deploy steps", "priority": "BACKGROUND", "type": "skill" }),
        )
        .await;
        assert_eq!(status, 200);

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = body["data"].as_array().unwrap();
        assert_eq!(items[0]["priority"], "Background");
        assert_eq!(items[0]["type"], "Skill");
    }

    #[tokio::test]
    async fn test_search_roundtrip_and_filters() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({ "content": "likes matcha", "namespace": "global" }),
        )
        .await;
        save(
            &app,
            serde_json::json!({ "content": "dislikes coffee", "namespace": "project:other" }),
        )
        .await;

        // namespace filter
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "likes", "namespace": "global" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["memory"]["content"], "likes matcha");

        // top_k + wrong-key auth is tolerated for unregistered agents
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "es", "top_k": 1, "agent_id": "bob" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_search_response_nests_memory_with_full_fields() {
        // Regression test: VS Code / Obsidian both expect `{ memory: {...}, score }`,
        // not a flat record — and `memory` needs layer/skill_meta/namespace/etc,
        // not just content/instruction/priority/tags.
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({
                "content": "deploy checklist",
                "priority": "MUST",
                "type": "skill",
                "namespace": "project:x",
            }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "deploy" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        let mem = &results[0]["memory"];
        assert_eq!(mem["content"], "deploy checklist");
        assert_eq!(mem["priority"], "Must");
        assert_eq!(mem["type"], "Skill");
        assert_eq!(mem["namespace"], "project:x");
        assert_eq!(mem["layer"], "L3");
        assert!(mem["human_reviewed"].is_boolean());
        assert!(mem["updated_at"].is_string());
        assert!(results[0]["score"].is_number());
    }

    /// Regression: REST used to return raw merge order while MCP's
    /// search_memory applied the reranker's authority-tier signal — same
    /// query, different ranking depending on transport.
    #[tokio::test]
    async fn test_search_reranks_by_authority_tier() {
        let app = spawn_app(false).await;
        save(&app, save_body("authority signal check")).await;
        save(
            &app,
            serde_json::json!({
                "content": "authority signal check",
                "tags": ["decision"],
            }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "authority signal check" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0]["memory"]["tags"],
            serde_json::json!(["decision"]),
            "decision-tagged memory should rank first: {}",
            body
        );
    }

    #[tokio::test]
    async fn test_list_memories_returns_full_fields() {
        // Regression test: list previously omitted layer/skill_meta/access_count/
        // decay_score/created_at/updated_at, which every client's `Memory` type expects.
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({ "content": "full fields check", "priority": "REFERENCE" }),
        )
        .await;

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = body["data"].as_array().unwrap();
        let mem = &items[0];
        assert_eq!(mem["layer"], "L2");
        assert!(mem["access_count"].is_number());
        assert!(mem["decay_score"].is_number());
        assert!(mem["created_at"].is_string());
        assert!(mem["updated_at"].is_string());
        assert!(mem["skill_meta"].is_null());
    }

    #[tokio::test]
    async fn test_list_memories_pagination() {
        let app = spawn_app(false).await;
        for i in 0..5 {
            save(
                &app,
                serde_json::json!({ "content": format!("memory {}", i) }),
            )
            .await;
        }
        let resp = app
            .client
            .get(format!("{}/api/memories?limit=3", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_session_start_injects_formatted() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({
                "content": "user prefers Rust",
                "priority": "MUST",
                "agent_id": "alice",
                "agent_type": "coding-assistant",
            }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "alice", "context_hint": "start", "format": "xml" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["count"], 1);
        assert!(
            body["data"]["formatted"]
                .as_str()
                .unwrap()
                .contains("prefers Rust")
        );
    }

    #[tokio::test]
    async fn test_session_start_custom_format_falls_back() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "alice", "format": "bogus" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["format"], "MustRef");
    }

    #[tokio::test]
    async fn test_extract_memories_endpoint() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let mems = body["data"]["memories"].as_array().unwrap();
        assert!(
            !mems.is_empty(),
            "preference signal should extract a memory"
        );
        // coverage 四桶必须存在且互斥求和 = 输入行数
        let cov = &body["data"]["coverage"];
        assert!(cov["input_lines"].as_u64().unwrap() >= 1);
        let n = cov["empty_lines"].as_u64().unwrap()
            + cov["extracted_lines"].as_u64().unwrap()
            + cov["no_signal_lines"].as_u64().unwrap();
        assert_eq!(n, cov["input_lines"].as_u64().unwrap());
    }

    #[tokio::test]
    async fn test_extract_memories_empty_text() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dedup_decay_promote_endpoints() {
        let app = spawn_app(false).await;
        save(&app, save_body("duplicate A")).await;

        let resp = app
            .client
            .post(format!("{}/api/dedup", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let resp = app
            .client
            .post(format!("{}/api/decay", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let resp = app
            .client
            .post(format!("{}/api/promote", app.base))
            .json(&serde_json::json!({ "min_l1": 1, "min_l2": 1 }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_confirm_read_and_delete() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("to read")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .post(format!("{}/api/confirm-read", app.base))
            .json(&serde_json::json!({ "memory_ids": [id] }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

        let resp = app
            .client
            .delete(format!("{}/api/memories/{}", app.base, id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_update_memory_partial_patch_preserves_other_fields() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(
            &app,
            serde_json::json!({
                "content": "original content",
                "priority": "REFERENCE",
                "tags": ["a", "b"],
            }),
        )
        .await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "priority": "MUST" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["priority"], "Must");
        // untouched fields survive the patch
        assert_eq!(body["data"]["content"], "original content");
        assert_eq!(body["data"]["tags"], serde_json::json!(["a", "b"]));
    }

    #[tokio::test]
    async fn test_save_memory_with_skill_meta() {
        let app = spawn_app(false).await;
        let (status, saved) = save(
            &app,
            serde_json::json!({
                "content": "deploy runbook",
                "type": "skill",
                "skill_trigger": "deploy",
                "skill_steps": ["check env", "push"],
                "skill_verification": "health check",
            }),
        )
        .await;
        assert_eq!(status, 200);
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        // Save responses are minimal — refetch via list to verify skill_meta.
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let list: serde_json::Value = resp.json().await.unwrap();
        let mem = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .expect("saved skill must be listed")
            .clone();
        assert_eq!(mem["skill_meta"]["trigger"], "deploy");
        assert_eq!(mem["skill_meta"]["version"], 1);
        assert_eq!(mem["skill_meta"]["steps"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_save_memory_visibility_roundtrip_and_default() {
        let app = spawn_app(false).await;
        // Default → scoped.
        let (_s, saved) = save(&app, serde_json::json!({ "content": "scoped fact" })).await;
        let scoped_id = saved["data"]["id"].as_str().unwrap().to_string();
        // Explicit → shared.
        let (_s, saved) = save(
            &app,
            serde_json::json!({ "content": "team pool fact", "visibility": "shared" }),
        )
        .await;
        let shared_id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let list: serde_json::Value = resp.json().await.unwrap();
        let arr = list["data"].as_array().unwrap();
        let scoped = arr.iter().find(|m| m["id"] == scoped_id).unwrap();
        let shared = arr.iter().find(|m| m["id"] == shared_id).unwrap();
        assert_eq!(scoped["visibility"], "scoped");
        assert_eq!(shared["visibility"], "shared");
    }

    #[tokio::test]
    async fn test_update_memory_sets_and_clears_skill_meta() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("deploy process")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({
                "type": "skill",
                "skill_trigger": "deploy",
                "skill_steps": ["build", "test", "push"],
                "skill_verification": "health check passes",
            }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["type"], "Skill");

        // fetch via list to confirm the memory still resolves after the patch
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(list["data"][0]["id"], id);
    }

    #[tokio::test]
    async fn test_update_memory_skill_version_evolution() {
        let app = spawn_app(false).await;
        // Create a skill with the needs-revision flag (as a failed task would).
        let (_status, saved) = save(&app, save_body("deploy process")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({
                "type": "skill",
                "tags": ["needs-revision", "deploy"],
                "skill_trigger": "deploy",
                "skill_steps": ["build", "push"],
            }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["data"]["skill_meta"]["version"], 1,
            "first definition is v1"
        );

        // Human revises the steps → version bumps, flag clears.
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({
                "skill_steps": ["build", "test", "push"],
            }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["skill_meta"]["version"], 2);
        let tags: Vec<&str> = body["data"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(!tags.contains(&"needs-revision"), "edit clears the flag");
        assert!(tags.contains(&"deploy"), "other tags preserved");

        // No-op edit (same steps) → no bump.
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({
                "skill_steps": ["build", "test", "push"],
            }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["data"]["skill_meta"]["version"], 2,
            "unchanged edit must not bump"
        );
    }

    #[tokio::test]
    async fn test_update_memory_layer_and_namespace() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("layer test")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "layer": "L3", "namespace": "project:foo" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["layer"], "L3");
        assert_eq!(body["data"]["namespace"], "project:foo");
    }

    #[tokio::test]
    async fn test_update_memory_missing_returns_error() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .put(format!("{}/api/memories/mem_nope", app.base))
            .json(&serde_json::json!({ "content": "x" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    /// Regression: `UpdateRequest` used plain `Option`, so `instruction: null`
    /// was indistinguishable from the field being absent — the Obsidian plugin
    /// sends `instruction: patch.instruction || null`, which silently kept the
    /// old instruction when the user cleared it.
    #[tokio::test]
    async fn test_update_memory_can_clear_instruction_with_null() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("clear me")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        // 1. set an instruction
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "instruction": "do the thing" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["instruction"], "do the thing");

        // 2. clear it with null
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "instruction": null }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        let inst = body["data"]["instruction"].clone();
        assert!(inst.is_null(), "instruction should be cleared, got {inst}");
    }

    /// Regression: unknown enum strings used to be silently coerced (MUST → Reference,
    /// any type → Fact). They must now surface as a 400.
    #[tokio::test]
    async fn test_update_memory_rejects_invalid_values_with_400() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("base")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        for (field, value) in [
            ("priority", "MUSTT"),
            ("type", "not-a-type"),
            ("layer", "L9"),
        ] {
            let resp = app
                .client
                .put(format!("{}/api/memories/{}", app.base, id))
                .json(&serde_json::json!({ field: value }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "field {field}={value} should be rejected"
            );
            let body: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(body["ok"], false);
        }
    }

    #[tokio::test]
    async fn test_update_memory_content_null_returns_400() {
        // `content` is a non-nullable String; an explicit null used to be a
        // silent no-op instead of a 400, unlike priority/type/layer.
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("base")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "content": null }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn test_delete_missing_returns_error() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .delete(format!("{}/api/memories/mem_nope", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_inbox_flow() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("pending review")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .get(format!("{}/api/inbox", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["total"], 1);

        // approve
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/approve", app.base, id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

        // reject a second memory
        let (_status, saved2) = save(&app, save_body("to reject")).await;
        let id2 = saved2["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/reject", app.base, id2))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // edit a third memory
        let (_status, saved3) = save(&app, save_body("to edit")).await;
        let id3 = saved3["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .post(format!("{}/api/inbox/{}/edit", app.base, id3))
            .json(&serde_json::json!({ "edited_content": "edited now" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["edited"], id3);
    }

    #[tokio::test]
    async fn test_inbox_approve_missing_returns_error() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/inbox/mem_missing/approve", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn test_compliance_flow_when_enabled() {
        let app = spawn_app(true).await;
        save(
            &app,
            serde_json::json!({ "content": "must rule", "priority": "MUST", "agent_id": "eve" }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "eve" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let sid = body["data"]["inject_session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = app
            .client
            .get(format!(
                "{}/api/compliance/session?session_id={}",
                app.base, sid
            ))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["inject_session_id"].is_string());

        let resp = app
            .client
            .get(format!("{}/api/compliance/summary", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["total_sessions"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_compliance_errors_when_disabled() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!(
                "{}/api/compliance/session?session_id=inj_x",
                app.base
            ))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap().contains("not enabled"));
    }

    #[tokio::test]
    async fn test_admin_route_rejects_missing_or_wrong_key_when_registered() {
        let app = spawn_app_with_admin_key("s3cr3t").await;

        // no header at all
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);

        // wrong key
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .header(API_KEY_HEADER, "wrong-key")
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
    }

    #[tokio::test]
    async fn test_admin_route_accepts_correct_key() {
        let app = spawn_app_with_admin_key("s3cr3t").await;

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .header(API_KEY_HEADER, "s3cr3t")
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_admin_route_dedup_requires_key_when_registered() {
        let app = spawn_app_with_admin_key("s3cr3t").await;

        let resp = app
            .client
            .post(format!("{}/api/dedup", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);

        let resp = app
            .client
            .post(format!("{}/api/dedup", app.base))
            .header(API_KEY_HEADER, "s3cr3t")
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_confirm_read_requires_admin_key_when_registered() {
        let app = spawn_app_with_admin_key("s3cr3t").await;
        let (_status, saved) = save(&app, save_body("read me")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        let resp = app
            .client
            .post(format!("{}/api/confirm-read", app.base))
            .json(&serde_json::json!({ "memory_ids": [id] }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["ok"], false,
            "confirm-read without a key must be rejected"
        );
    }

    #[tokio::test]
    async fn test_extract_requires_admin_key_when_registered() {
        let app = spawn_app_with_admin_key("s3cr3t").await;

        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false, "extract without a key must be rejected");

        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .header(API_KEY_HEADER, "s3cr3t")
            .json(&serde_json::json!({ "text": "I always prefer dark mode" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_admin_routes_stay_open_when_no_admin_key_registered() {
        // Default registry (used by every other test in this module) has no
        // "admin" agent key configured — admin routes must keep working
        // unauthenticated, preserving back-compat with existing deployments.
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_cors_allows_localhost_origin() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/health", app.base))
            .header("Origin", "http://localhost:3000")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert!(
            resp.headers().contains_key("access-control-allow-origin"),
            "allowed origin must receive a CORS header"
        );
    }

    // CORS layer reads MEMVAULT_CORS_ORIGIN from a process-global env var at
    // router-build time, so these tests must not run concurrently and must
    // clean up the env var afterwards.
    static CORS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn assert_cors_origin(origin: &str, expected: Option<&str>) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let app = spawn_app(false).await;
            let resp = app
                .client
                .get(format!("{}/health", app.base))
                .header("Origin", origin)
                .send()
                .await
                .unwrap();
            match expected {
                Some(ok) => assert_eq!(
                    resp.headers()
                        .get("access-control-allow-origin")
                        .and_then(|v| v.to_str().ok()),
                    Some(ok),
                    "origin {origin} should be allowed with header {ok}"
                ),
                None => assert!(
                    !resp.headers().contains_key("access-control-allow-origin"),
                    "origin {origin} must NOT get a CORS header"
                ),
            }
        });
    }

    // `std::env::set_var`/`remove_var` are `unsafe` on current toolchains; these
    // tests own the env var exclusively via `CORS_LOCK` and always clean up.
    fn set_cors_env(v: Option<&str>) {
        unsafe {
            match v {
                Some(val) => std::env::set_var("MEMVAULT_CORS_ORIGIN", val),
                None => std::env::remove_var("MEMVAULT_CORS_ORIGIN"),
            }
        }
    }

    #[test]
    fn test_cors_default_allows_only_localhost() {
        let _g = CORS_LOCK.lock().unwrap();
        set_cors_env(None);
        assert_cors_origin("http://localhost:3000", Some("http://localhost:3000"));
        assert_cors_origin("http://127.0.0.1:8080", Some("http://127.0.0.1:8080"));
        assert_cors_origin("http://evil.example.com", None);
        set_cors_env(None);
    }

    #[test]
    fn test_cors_wildcard_allows_any_origin() {
        let _g = CORS_LOCK.lock().unwrap();
        set_cors_env(Some("*"));
        assert_cors_origin("http://localhost:5173", Some("*"));
        assert_cors_origin("https://evil.example.com", Some("*"));
        set_cors_env(None);
    }

    #[test]
    fn test_cors_origin_list_allowlist_only() {
        let _g = CORS_LOCK.lock().unwrap();
        set_cors_env(Some("http://app.example.com, http://localhost:3000"));
        assert_cors_origin("http://app.example.com", Some("http://app.example.com"));
        assert_cors_origin("http://localhost:3000", Some("http://localhost:3000"));
        assert_cors_origin("http://not-listed.example.com", None);
        set_cors_env(None);
    }

    /// Regression: write routes with a registered admin key must reject a
    /// missing/wrong key with 401 (was 500 before http_error mapping).
    #[tokio::test]
    async fn test_put_delete_reject_wrong_admin_key_with_401() {
        let app = spawn_app_with_admin_key("s3cr3t").await;
        let (_status, saved) = save(&app, save_body("protect me")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        // wrong key -> 401
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .header("X-MemVault-Api-Key", "wrong")
            .json(&serde_json::json!({ "priority": "MUST" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        let resp = app
            .client
            .delete(format!("{}/api/memories/{}", app.base, id))
            .header("X-MemVault-Api-Key", "wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    /// NotFound on update/delete now surfaces as 404 (was 500).
    #[tokio::test]
    async fn test_update_missing_returns_404() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .put(format!("{}/api/memories/mem_nope", app.base))
            .json(&serde_json::json!({ "content": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    /// /api/stats aggregates MUST/REFERENCE counts and layer distribution.
    #[tokio::test]
    async fn test_stats_endpoint_counts() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({ "content": "must rule", "priority": "MUST" }),
        )
        .await;
        save(
            &app,
            serde_json::json!({ "content": "ref fact", "priority": "REFERENCE" }),
        )
        .await;

        let resp = app
            .client
            .get(format!("{}/api/stats", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let d = &body["data"];
        assert_eq!(d["total"], 2);
        assert_eq!(d["must_count"], 1);
        assert_eq!(d["reference_count"], 1);
        assert_eq!(d["skills"], 0);
        // MUST maps to L3, REFERENCE maps to L2 via Memory::new
        assert!(d["layers"]["l2"].as_u64().unwrap() >= 1);
        assert!(d["layers"]["l3"].as_u64().unwrap() >= 1);
        assert!(
            d["namespaces"]
                .as_array()
                .unwrap()
                .contains(&"global".into())
        );
    }

    /// User-created memories can skip the review queue via human_reviewed=true.
    #[tokio::test]
    async fn test_save_human_reviewed_override_skips_inbox() {
        let app = spawn_app(false).await;
        let (status, body) = save(
            &app,
            serde_json::json!({
                "content": "hand-entered",
                "human_reviewed": true,
                "ai_generated": false,
            }),
        )
        .await;
        assert_eq!(status, 200);
        let id = body["data"]["id"].as_str().unwrap();

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let json: serde_json::Value = resp.json().await.unwrap();
        let mem = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap();
        assert_eq!(mem["human_reviewed"], true);
        assert_eq!(mem["ai_generated"], false);
    }

    /// GET /api/memories honors the offset parameter for pagination.
    #[tokio::test]
    async fn test_list_memories_respects_offset() {
        let app = spawn_app(false).await;
        for i in 0..5 {
            save(&app, serde_json::json!({ "content": format!("mem {}", i) })).await;
        }

        let first = app
            .client
            .get(format!("{}/api/memories?limit=3", app.base))
            .send()
            .await
            .unwrap();
        let first_json: serde_json::Value = first.json().await.unwrap();
        let page0 = first_json["data"].as_array().unwrap().clone();
        assert_eq!(page0.len(), 3);

        let second = app
            .client
            .get(format!("{}/api/memories?limit=3&offset=3", app.base))
            .send()
            .await
            .unwrap();
        let second_json: serde_json::Value = second.json().await.unwrap();
        let page1 = second_json["data"].as_array().unwrap().clone();
        assert_eq!(page1.len(), 2);

        let id0: std::collections::HashSet<&str> =
            page0.iter().map(|m| m["id"].as_str().unwrap()).collect();
        for m in &page1 {
            assert!(!id0.contains(m["id"].as_str().unwrap()), "pages overlap");
        }
    }

    /// /api/stats requires the registered admin API key when one is configured.
    #[tokio::test]
    async fn test_stats_requires_admin_key_when_registered() {
        let app = spawn_app_with_admin_key("s3cr3t").await;

        let unauth = app
            .client
            .get(format!("{}/api/stats", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);
        let unauth_body: serde_json::Value = unauth.json().await.unwrap();
        assert_eq!(unauth_body["ok"], false);

        let auth = app
            .client
            .get(format!("{}/api/stats", app.base))
            .header("X-MemVault-Api-Key", "s3cr3t")
            .send()
            .await
            .unwrap();
        assert_eq!(auth.status(), 200);
        let auth_body: serde_json::Value = auth.json().await.unwrap();
        assert_eq!(auth_body["ok"], true);
    }

    /// A built SPA (dist/) is served at / with fallback to index.html while
    /// /api/* routes keep taking precedence.
    #[tokio::test]
    async fn test_web_assets_served_with_api_routes_alive() {
        // temp dir with an index.html behaving like a Vite build
        let dir =
            std::env::temp_dir().join(format!("memvault_mcp_web_{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<div id=root>memvault</div>").unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let app = build_rest_router(store, router, None, metrics(), None, None);
        let app = attach_web_assets(app, dir.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", addr.port());

        // / returns index

        let root = client.get(&base).send().await.unwrap();
        assert_eq!(root.status(), 200);
        assert!(root.text().await.unwrap().contains("memvault"));

        // SPA fallback: unknown routes return index.html

        let deep = client
            .get(format!("{}/memories/xyz", base))
            .send()
            .await
            .unwrap();
        assert_eq!(deep.status(), 200);
        assert!(deep.text().await.unwrap().contains("memvault"));

        // API routes still take precedence

        let api = client
            .get(format!("{}/api/stats", base))
            .send()
            .await
            .unwrap();
        let api_json: serde_json::Value = api.json().await.unwrap();
        assert_eq!(api_json["ok"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
