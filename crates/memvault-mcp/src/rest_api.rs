use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Deserializer, Serialize};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};

use memvault_core::agent_adapt::{self, InjectFormat};
use memvault_core::agent_import::AgentMemorySource;
use memvault_core::compliance::ComplianceStore;
use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::doctor::Doctor;
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
    /// Contextual (LLM-based) extractor holder, used by `POST /api/extract`
    /// mode="llm" and to reflect failed task outcomes into lessons
    /// (`POST /api/outcome`). Lazily probed from the environment on first
    /// use and re-probed after failures (see `LazyLlmExtractor`) — a boot-
    /// time Ollama hiccup no longer disables llm extraction permanently.
    llm_extractor: memvault_core::llm_extractor::LazyLlmExtractor,
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
    /// Force insert, skipping delta-write dedup/merge
    /// (docs/PAPER-INSPIRATIONS.md Feature A).
    #[serde(default)]
    force_insert: bool,
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
    /// Drop results whose final (post-rerank) relevance score is below this
    /// threshold. Unset = return the full top_k regardless of score, the
    /// pre-existing behavior. Lets callers distinguish "no confident match"
    /// (empty result) from "weak matches padded to top_k".
    #[serde(default)]
    min_score: Option<f64>,
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
struct SessionQuery {
    /// "plain" returns the formatted instructions as text/plain instead of the
    /// JSON envelope — for hook scripts that only have curl (plugin P1).
    output: Option<String>,
}

/// Hook-friendly response: plain text body for curl-only consumers that cannot
/// safely unwrap the JSON envelope's embedded string.
fn plain_text_response(body: String) -> axum::response::Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        body,
    )
        .into_response()
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
        "occurred_at": m.occurred_at,
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
    // Authenticate the agent, and record whether its agent_id had a
    // registered API key that was actually checked (vs. unauthenticated
    // mode) — feeds the MUST corroboration gate in
    // `router::format::is_trusted` without changing default behavior.
    let (_, identity_verified) = state
        .router
        .authenticate_agent_verified(&req.agent_id, req.api_key.as_deref())
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
    mem.identity_verified = identity_verified;
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
    let mut embedding: Option<Vec<f32>> = None;
    if let Some(ref embedder) = state.embedder {
        match embedder.embed(&[embed_text]).await {
            Ok(embeddings) if !embeddings.is_empty() => {
                embedding = embeddings.into_iter().next();
            }
            Err(e) => tracing::warn!("Auto-embedding failed, saving without: {}", e),
            _ => {}
        }
    }
    let embedded = embedding.is_some();

    // Delta write (docs/PAPER-INSPIRATIONS.md Feature A): 同命名空间内先查重,
    // 近重复跳过、相似项吸收残差、force_insert 直插。技能(SOP)是过程性知识,
    // 与事实合并语义不成立,一律直插。
    let force = req.force_insert || mem.skill_meta.is_some();
    let writer =
        memvault_core::writer::MemoryWriter::new(state.store.clone(), state.embedder.clone());
    let outcome = writer
        .save(mem, embedding, force)
        .await
        .map_err(http_error)?;

    let result = match outcome {
        memvault_core::writer::WriteOutcome::Inserted(saved) => {
            metrics::counter!("memvault_memories_saved_total").increment(1);
            serde_json::json!({
                "id": saved.id,
                "embedded": embedded,
                "action": "saved",
            })
        }
        memvault_core::writer::WriteOutcome::Merged {
            memory,
            similarity,
            residual_added,
        } => {
            metrics::counter!("memvault_memories_merged_total").increment(1);
            serde_json::json!({
                "id": memory.id,
                "embedded": embedded,
                "action": "merged",
                "similarity": similarity,
                "residual_added": residual_added,
            })
        }
        memvault_core::writer::WriteOutcome::Skipped { memory, similarity } => {
            metrics::counter!("memvault_memories_skipped_total").increment(1);
            serde_json::json!({
                "id": memory.id,
                "embedded": embedded,
                "action": "skipped",
                "similarity": similarity,
            })
        }
    };
    Ok(ApiResponse::success(result))
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
    let llm = state.llm_extractor.get().await;
    match memvault_core::reflection::reflect_and_store(
        state.store.as_ref(),
        &recorded.memory.id,
        input.clone().into(),
        llm.as_deref(),
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

    // Effectiveness: pair this outcome against recent pending injections for
    // the same agent (see `crates/memvault-core/src/effectiveness.rs`).
    // Best-effort — no judge configured means 0 judged, never fabricated.
    let mut effectiveness_judged = 0usize;
    if let Some(ref cs) = state.compliance {
        match memvault_core::effectiveness::judge_recent_injections(
            cs,
            state.store.as_ref(),
            llm.as_deref(),
            &input,
        )
        .await
        {
            Ok(n) => effectiveness_judged = n,
            Err(e) => warn!(error = %e, "effectiveness judging failed; outcome kept"),
        }
    }

    metrics::counter!("memvault_outcomes_recorded_total").increment(1);
    Ok(ApiResponse::success(serde_json::json!({
        "id": recorded.memory.id,
        "outcome": recorded.memory.content,
        "embedded": recorded.embedded,
        "lesson": lesson_json,
        "effectiveness_judged": effectiveness_judged,
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

    // Confidence floor: applied to the final reranked scores, so callers can
    // tell "nothing relevant" (empty list) from "top_k padded with weak
    // matches". Unset keeps the pre-existing return-everything behavior.
    let results = match req.min_score {
        Some(min) => results.into_iter().filter(|r| r.score >= min).collect(),
        None => results,
    };

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
    Query(q): Query<SessionQuery>,
    Json(req): Json<SessionRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiResponse<()>>)> {
    let plain = q.output.as_deref() == Some("plain");

    // Authenticate the agent
    state
        .router
        .authenticate_agent(&req.agent_id, req.api_key.as_deref())
        .map_err(http_error)?;

    // Feature F: honor the agent's canonical injection channel — when memory
    // for this agent is delivered by another channel, skip the session
    // injection so the same memory is not delivered twice.
    if !state
        .router
        .channel_allows(&req.agent_id, InjectChannel::Mcp)
    {
        if plain {
            return Ok(plain_text_response(String::new()));
        }
        let canonical = state
            .router
            .inject_channel_for(&req.agent_id)
            .map(|c| c.as_str())
            .unwrap_or("unknown");
        return Ok(ApiResponse::success(serde_json::json!({
            "formatted": String::new(),
            "count": 0,
            "format": "none",
            "agent_profile": req.agent_id,
            "skipped": [],
            "skipped_channel": canonical,
            "note": format!(
                "Memory for agent '{}' is injected via the '{}' channel; skipping session injection to avoid duplication.",
                req.agent_id, canonical
            ),
        }))
        .into_response());
    }

    // Feature D: weight a multi-turn context hint by recency so retrieval is
    // conditioned on the recent context, not a flat string.
    let context_key = req
        .context_hint
        .as_deref()
        .map(memvault_core::query_expand::weight_turns_by_recency);
    let injection = state
        .router
        .session_start(
            &req.agent_id,
            context_key.as_deref(),
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

    // Hook scripts (plugin P1) consume the formatted text directly — same
    // side effects as the JSON path, different envelope.
    if plain {
        return Ok(plain_text_response(formatted));
    }

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

    Ok(ApiResponse::success(response).into_response())
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

#[derive(Debug, Deserialize)]
struct ExtractRequest {
    text: String,
    /// "rule" (default, keyword pattern matching) or "llm" (semantic, uses
    /// the configured LLM extraction provider; degrades to rule-based with
    /// `fallback_used`/`fallback_reason` reported when no provider is
    /// configured or the LLM call fails).
    mode: Option<String>,
    /// Optional paired assistant/response text for mode="llm".
    assistant_text: Option<String>,
    /// Save extracted candidates straight to the store (always unreviewed —
    /// same review-inbox trust boundary as `import-skills`/`import-agent`).
    #[serde(default)]
    auto_save: bool,
    /// Who produced `text` (agent id / speaker name), recorded as
    /// `source_agent.id` on saved memories. Defaults to "dashboard" — the
    /// pre-existing hardcoded value — when absent.
    #[serde(default)]
    source_id: Option<String>,
    /// When the extracted facts actually happened in the source
    /// conversation (RFC 3339), stored as `occurred_at` on saved memories —
    /// distinct from `created_at` (ingestion time). Invalid input is a 400,
    /// never a silent drop.
    #[serde(default)]
    occurred_at: Option<String>,
}

fn extracted_memory_to_json(e: &memvault_core::extractor::ExtractedMemory) -> serde_json::Value {
    serde_json::json!({
        "content": e.content,
        "instruction": e.instruction,
        "type": format!("{:?}", e.memory_type),
        "priority": format!("{:?}", e.priority),
        "tags": e.tags,
        "confidence": e.confidence,
    })
}

async fn extract_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExtractRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let mode = req.mode.as_deref().unwrap_or("rule");
    // 带覆盖面记账:rule 模式返回四桶计数,与 CLI/MCP 对齐,避免"偷偷丢段"。
    // llm 模式没有逐行覆盖率概念,coverage 为 null——除非降级到 rule(见下)。
    // LLM 契约(llm_extractor.rs trait doc):抽取失败必须非致命,降级到
    // 规则抽取而不是把整段输入静默丢掉。降级如实上报:fallback_used +
    // fallback_reason + coverage,绝不假装是干净的 llm 结果。
    let rule_outcome = || {
        let outcome = Extractor::extract_with_coverage(&req.text);
        let cov = outcome.coverage;
        let coverage_json = serde_json::json!({
            "input_lines": cov.input_lines,
            "empty_lines": cov.empty_lines,
            "extracted_lines": cov.extracted_lines,
            "no_signal_lines": cov.no_signal_lines,
        });
        (outcome.memories, Some(coverage_json))
    };
    let (extracted, coverage_json, fallback_used, fallback_reason): (
        Vec<memvault_core::extractor::ExtractedMemory>,
        Option<serde_json::Value>,
        bool,
        Option<String>,
    ) = match mode {
        "llm" => {
            // Lazy holder: probes the environment on first use and re-probes
            // after failures — see LazyLlmExtractor.
            let llm = state.llm_extractor.get().await;
            match llm.as_deref() {
                None => {
                    let reason =
                        "no_llm_extractor_configured: set MEMVAULT_LLM_EXTRACTION_PROVIDER \
                                  (and MEMVAULT_LLM_EXTRACTION_API_KEY / _MODEL as needed); \
                                  fell back to rule-based extraction"
                            .to_string();
                    warn!(%reason, "llm extraction unavailable");
                    let (extracted, coverage_json) = rule_outcome();
                    (extracted, coverage_json, true, Some(reason))
                }
                Some(llm) => {
                    let mut context = format!("User: {}", req.text);
                    if let Some(assistant_text) = req
                        .assistant_text
                        .as_deref()
                        .filter(|t| !t.trim().is_empty())
                    {
                        context.push_str("\nAssistant: ");
                        context.push_str(assistant_text);
                    }

                    match llm.extract(&context).await {
                        Ok(extracted) => (extracted, None, false, None),
                        Err(e) => {
                            let reason = format!("llm_extraction_error: {e}");
                            warn!(error = %e, "llm extraction failed; falling back to rule-based");
                            let (extracted, coverage_json) = rule_outcome();
                            (extracted, coverage_json, true, Some(reason))
                        }
                    }
                }
            }
        }
        _ => {
            let (extracted, coverage_json) = rule_outcome();
            (extracted, coverage_json, false, None)
        }
    };
    // Audit trail: a save that came out of the llm-requested-but-degraded
    // path must be distinguishable from an intentional rule-based call.
    let method = if fallback_used {
        "llm_fallback_rule"
    } else {
        mode
    };

    let memories: Vec<serde_json::Value> = extracted.iter().map(extracted_memory_to_json).collect();

    // Parse the caller-supplied event date once, before any save: an invalid
    // timestamp is a 400, never a silent drop (the caller believes their
    // provenance landed).
    let occurred_at = match req.occurred_at.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => Some(
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    http_error(MemVaultError::InvalidInput(format!(
                        "occurred_at must be an RFC 3339 timestamp (e.g. 2026-09-09T12:00:00Z): {e}"
                    )))
                })?,
        ),
        _ => None,
    };

    let mut saved_ids = Vec::new();
    if req.auto_save {
        for e in &extracted {
            let mut mem = Memory::new(
                e.memory_type.clone(),
                e.content.clone(),
                e.priority.clone(),
                SourceAgent {
                    // Caller-declared provenance instead of the old
                    // hardcoded "dashboard": batch ingestions (transcripts,
                    // imports) must be able to say who produced the text.
                    id: req
                        .source_id
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| "dashboard".to_string()),
                    agent_type: "web-dashboard".to_string(),
                    session_id: None,
                },
            );
            mem.instruction = e.instruction.clone();
            mem.tags = e.tags.clone();
            mem.tags.push(format!("method:{method}"));
            mem.confidence = e.confidence;
            mem.occurred_at = occurred_at;
            let saved = state.store.save(mem).await.map_err(http_error)?;
            saved_ids.push(saved.id);
        }
    }

    Ok(ApiResponse::success(serde_json::json!({
        "memories": memories,
        "coverage": coverage_json,
        "saved_ids": saved_ids,
        "fallback_used": fallback_used,
        "fallback_reason": fallback_reason,
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

/// Which features degrade without an embedding provider configured — cheap,
/// no store access, instant (no `Doctor`-style scan needed).
async fn get_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let report = memvault_core::capabilities::capability_report(&state.embedder);
    Ok(ApiResponse::success(report))
}

/// Full read-only hygiene scan (dangling supersede/lesson pointers, stale
/// unarchived memories, live contradictions, near-duplicates, review
/// backlog, skills flagged for revision). Deterministic, no writes — but an
/// O(n) scan over the whole store, so it's a button the dashboard triggers
/// on demand rather than something polled on every page load.
async fn run_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let report = Doctor::new(state.store).run().await.map_err(http_error)?;
    Ok(ApiResponse::success(report))
}

// --- Cold-start cross-agent memory import ---
//
// Wraps `memvault_core::agent_import` (same module the `memvault import-agent`
// CLI command uses) for the dashboard. Detection reads files on *this
// server's* local filesystem — meaningful only when the dashboard is
// pointed at a `memvault-mcp` running on the same machine as the agent
// being imported from, same trust boundary as `/api/backup`.

#[derive(Deserialize)]
struct AgentImportRequest {
    agent: String,
    path: Option<String>,
    namespace: Option<String>,
}

type AdapterAndSource = (
    Box<dyn AgentMemorySource>,
    memvault_core::agent_import::DetectedSource,
);

/// Resolve `req.agent` to its adapter and run detection. Shared by preview/run.
fn detect_for_agent(
    req: &AgentImportRequest,
) -> Result<AdapterAndSource, (StatusCode, Json<ApiResponse<()>>)> {
    let adapter = memvault_core::agent_import::all_adapters()
        .into_iter()
        .find(|a| a.agent_key().eq_ignore_ascii_case(&req.agent))
        .ok_or_else(|| bad_request(format!("unknown agent: {}", req.agent)))?;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let override_path = req.path.as_ref().map(PathBuf::from);

    let detected = adapter
        .detect(&home, &cwd, override_path.as_deref())
        .ok_or_else(|| {
            bad_request(format!(
                "{}: not detected on this machine (pass `path` to point at a memory file/dir manually)",
                adapter.display_name()
            ))
        })?;
    Ok((adapter, detected))
}

fn skipped_files_json(files_skipped: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    files_skipped
        .iter()
        .map(|(p, reason)| serde_json::json!({ "path": p.display().to_string(), "reason": reason }))
        .collect()
}

/// Probe every known agent's default locations; read-only, no parsing, no writes.
async fn agent_import_scan(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let results: Vec<serde_json::Value> = memvault_core::agent_import::all_adapters()
        .iter()
        .map(|adapter| {
            let detected = adapter.detect(&home, &cwd, None);
            serde_json::json!({
                "agent_key": adapter.agent_key(),
                "display_name": adapter.display_name(),
                "found": detected.is_some(),
                "paths": detected
                    .map(|d| d.paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            })
        })
        .collect();

    Ok(ApiResponse::success(results))
}

/// Detect + parse + dedup-check one agent's memory files. Never writes.
async fn agent_import_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AgentImportRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let (adapter, detected) = detect_for_agent(&req)?;
    let outcome = adapter.parse(&detected);
    let dedup = Deduplicator::new(state.store.clone(), state.embedder.clone());

    let mut candidates = Vec::new();
    for c in outcome.candidates {
        let mut mem = c.memory;
        if let Some(ns) = &req.namespace {
            mem.namespace = ns.clone();
        }
        let dup = dedup
            .check_duplicate(&mem.content, Some(&mem.namespace))
            .await
            .map_err(http_error)?;
        candidates.push(serde_json::json!({
            "content": mem.content,
            "instruction": mem.instruction,
            "type": format!("{:?}", mem.memory_type),
            "priority": format!("{:?}", mem.priority),
            "tags": mem.tags,
            "confidence": mem.confidence,
            "namespace": mem.namespace,
            "raw_excerpt": c.raw_excerpt,
            "parse_confidence": format!("{:?}", c.confidence),
            "duplicate_of": dup.map(|d| d.existing_id),
        }));
    }

    Ok(ApiResponse::success(serde_json::json!({
        "agent_key": adapter.agent_key(),
        "display_name": adapter.display_name(),
        "files_scanned": outcome.files_scanned,
        "files_skipped": skipped_files_json(&outcome.files_skipped),
        "candidates": candidates,
    })))
}

/// Detect + parse + dedup-check + save. Duplicates are skipped; everything
/// else lands at `priority=Reference`, `human_reviewed=false` — same trust
/// boundary as `import-skills`/`extract`, and there is deliberately no
/// `approve` param here (the CLI's `--approve` bypass is for the machine's
/// own operator, not exposed over the web).
async fn agent_import_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AgentImportRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let (adapter, detected) = detect_for_agent(&req)?;
    let outcome = adapter.parse(&detected);
    let dedup = Deduplicator::new(state.store.clone(), state.embedder.clone());

    let mut imported = Vec::new();
    let mut duplicates_skipped = 0usize;
    for c in outcome.candidates {
        let mut mem = c.memory;
        if let Some(ns) = &req.namespace {
            mem.namespace = ns.clone();
        }
        let dup = dedup
            .check_duplicate(&mem.content, Some(&mem.namespace))
            .await
            .map_err(http_error)?;
        if dup.is_some() {
            duplicates_skipped += 1;
            continue;
        }
        let saved = state.store.save(mem).await.map_err(http_error)?;
        metrics::counter!("memvault_memories_saved_total").increment(1);
        imported.push(serde_json::json!({ "id": saved.id, "content": saved.content }));
    }

    Ok(ApiResponse::success(serde_json::json!({
        "agent_key": adapter.agent_key(),
        "display_name": adapter.display_name(),
        "files_scanned": outcome.files_scanned,
        "files_skipped": skipped_files_json(&outcome.files_skipped),
        "imported": imported,
        "duplicates_skipped": duplicates_skipped,
    })))
}

// --- Export / Import ---
//
// One-shot, in-memory (no chunking/streaming) — same 100k-row cap as
// `Exporter`/`Importer` elsewhere. Fine for the vault sizes this tool
// targets; a very large vault would need a streaming variant, not built
// here.

fn default_export_format() -> String {
    "json".to_string()
}

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default = "default_export_format")]
    format: String,
    namespace: Option<String>,
}

/// `format=json` → `{"format":"json","content":"<json text>"}` (the exact
/// bytes `POST /api/import` with `format=json` expects back).
/// `format=markdown` → `{"format":"markdown","files":[{filename,content}]}`
/// (one file per memory, YAML-frontmatter format `Importer::parse_markdown` reads).
async fn export_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ExportQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let exporter = memvault_core::io::Exporter::new(state.store);
    match params.format.as_str() {
        "markdown" | "md" => {
            let files = exporter
                .export_markdown(params.namespace.as_deref())
                .await
                .map_err(http_error)?;
            let files_json: Vec<_> = files
                .into_iter()
                .map(|(filename, content)| serde_json::json!({ "filename": filename, "content": content }))
                .collect();
            Ok(ApiResponse::success(
                serde_json::json!({ "format": "markdown", "files": files_json }),
            ))
        }
        _ => {
            let content = exporter
                .export_json(params.namespace.as_deref())
                .await
                .map_err(http_error)?;
            Ok(ApiResponse::success(
                serde_json::json!({ "format": "json", "content": content }),
            ))
        }
    }
}

#[derive(Deserialize)]
struct ImportFileInput {
    filename: String,
    content: String,
}

#[derive(Deserialize)]
struct ImportRequest {
    format: String,
    /// Required for `format=json` — the exact text `export` returned.
    content: Option<String>,
    /// Required for `format=markdown` — one entry per exported `.md` file.
    files: Option<Vec<ImportFileInput>>,
}

/// A single bad markdown file is reported in `skipped`, not a hard failure
/// for the whole batch — same "never silently drop, never all-or-nothing"
/// stance as `agent_import`/`extract`.
async fn import_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    match req.format.as_str() {
        "markdown" | "md" => {
            let files = req
                .files
                .ok_or_else(|| bad_request("markdown import requires `files`".to_string()))?;
            let mut imported = 0usize;
            let mut skipped = Vec::new();
            for f in files {
                match memvault_core::io::Importer::parse_markdown(&f.content) {
                    Ok(mem) => {
                        state.store.save(mem).await.map_err(http_error)?;
                        imported += 1;
                    }
                    Err(e) => skipped.push(
                        serde_json::json!({ "filename": f.filename, "reason": e.to_string() }),
                    ),
                }
            }
            Ok(ApiResponse::success(
                serde_json::json!({ "imported": imported, "skipped": skipped }),
            ))
        }
        _ => {
            let content = req
                .content
                .ok_or_else(|| bad_request("json import requires `content`".to_string()))?;
            let importer = memvault_core::io::Importer::new(state.store);
            let count = importer.import_json(&content).await.map_err(http_error)?;
            Ok(ApiResponse::success(
                serde_json::json!({ "imported": count }),
            ))
        }
    }
}

/// Point-in-time SQLite snapshot (`VACUUM INTO`, safe against a live
/// WAL-mode DB) streamed back as a file download. Writes to a scratch temp
/// path on *this server's* filesystem and deletes it immediately after
/// reading it back — never left on disk, no accumulating backups dir to
/// manage.
async fn create_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let dest = std::env::temp_dir().join(format!("memvault-backup-{}.db", Uuid::new_v4().simple()));
    state.store.backup_to(&dest).await.map_err(http_error)?;

    let bytes = tokio::fs::read(&dest).await.map_err(|e| {
        http_error(MemVaultError::Storage(format!(
            "failed to read backup file: {e}"
        )))
    })?;
    let _ = tokio::fs::remove_file(&dest).await;

    let filename = format!(
        "memvault-backup-{}.db",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    let content_disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|e| http_error(MemVaultError::Storage(e.to_string())))?;

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    resp_headers.insert(axum::http::header::CONTENT_DISPOSITION, content_disposition);

    Ok((resp_headers, bytes))
}

// --- Checkpoints / Restore ---

#[derive(Deserialize)]
struct CheckpointsQuery {
    #[serde(default = "default_checkpoints_limit")]
    limit: usize,
}
fn default_checkpoints_limit() -> usize {
    20
}

/// Recent edit history across the whole store (most recent first).
async fn list_all_checkpoints(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<CheckpointsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;
    let entries = state
        .store
        .list_checkpoints(None, params.limit)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(entries))
}

/// Edit history for one memory (most recent first).
async fn list_memory_checkpoints(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<CheckpointsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;
    let entries = state
        .store
        .list_checkpoints(Some(&id), params.limit)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(entries))
}

/// Revert to a prior snapshot. Targeted by `history_id` (not `memory_id`) —
/// the revert itself is captured as a new history entry, so "undo the undo"
/// stays possible.
async fn restore_checkpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(history_id): axum::extract::Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;
    let restored = state
        .store
        .restore_checkpoint(history_id)
        .await
        .map_err(http_error)?;
    Ok(ApiResponse::success(memory_to_json(&restored)))
}

// --- SOP skill import ---

fn default_sop_fallback_title() -> String {
    "imported-sop".to_string()
}

#[derive(Deserialize)]
struct ImportSkillsRequest {
    markdown: String,
    #[serde(default = "default_sop_fallback_title")]
    fallback_title: String,
    #[serde(default = "default_global")]
    namespace: String,
    /// Mark imported skills as human-reviewed (skip the inbox). Default false.
    #[serde(default)]
    approve: bool,
}

/// Bulk skill onboarding from a Markdown SOP document — same parser/shape
/// as the MCP `import_skills` tool (`sop::parse_sops`), ported 1:1 for the
/// dashboard rather than reimplemented.
async fn import_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ImportSkillsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let parsed = memvault_core::sop::parse_sops(&req.markdown, &req.fallback_title);

    let mut imported = Vec::new();
    for skill in &parsed.skills {
        let mut mem = Memory::new(
            MemoryType::Skill,
            skill.title.clone(),
            Priority::Reference,
            SourceAgent {
                id: "dashboard".to_string(),
                agent_type: "importer".to_string(),
                session_id: None,
            },
        );
        mem.namespace = req.namespace.clone();
        mem.tags = vec!["imported-sop".to_string()];
        mem.human_reviewed = req.approve;
        mem.skill_meta = Some(memvault_core::models::SkillMeta {
            trigger: skill.trigger.clone(),
            steps: skill.steps.clone(),
            verification: skill.verification.clone(),
            version: 1,
        });
        let saved = state.store.save(mem).await.map_err(http_error)?;
        imported.push(serde_json::json!({
            "title": skill.title,
            "id": saved.id,
            "steps": skill.steps.len(),
        }));
    }

    Ok(ApiResponse::success(serde_json::json!({
        "imported": imported,
        "skipped_no_steps": parsed.skipped_no_steps,
    })))
}

/// Read-only view of the agent registry (`agents.yaml` or the built-in
/// defaults) — injection rules per agent, for ops visibility. `api_key` is
/// never returned, not even hashed; only whether one is configured.
async fn list_agents(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let profiles: Vec<serde_json::Value> = state
        .router
        .list_agent_profiles()
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "agent_type": p.agent_type,
                "description": p.description,
                "inject_rules": {
                    "max_memories": p.inject_rules.max_memories,
                    "token_budget": p.inject_rules.token_budget,
                    "priority_order": p.inject_rules.priority_order,
                    "namespace_filter": p.inject_rules.namespace_filter,
                    "exclude_types": p.inject_rules.exclude_types,
                },
                "has_api_key": p.api_key.is_some(),
            })
        })
        .collect();

    Ok(ApiResponse::success(profiles))
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

#[derive(Deserialize)]
struct EffectivenessQuery {
    agent_id: Option<String>,
    #[serde(default = "default_10")]
    limit: usize,
}

/// Automatic effectiveness judgments (useful/neutral/harmful/insufficient_context),
/// judged from `record_outcome` pairing — independent of manual `report_compliance`.
async fn get_effectiveness_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<EffectivenessQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    authenticate_admin(&state, &headers)?;

    let cs = state
        .compliance
        .ok_or_else(|| api_error("Compliance tracking is not enabled"))?;
    let summary = cs
        .get_effectiveness_summary(params.agent_id.as_deref(), params.limit)
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
    llm_extractor: memvault_core::llm_extractor::LazyLlmExtractor,
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
        .route("/api/doctor", get(run_doctor))
        .route("/api/capabilities", get(get_capabilities))
        .route("/api/agents/import/scan", get(agent_import_scan))
        .route("/api/agents/import/preview", post(agent_import_preview))
        .route("/api/agents/import/run", post(agent_import_run))
        .route("/api/export", get(export_memories))
        .route("/api/import", post(import_memories))
        .route("/api/backup", post(create_backup))
        .route("/api/checkpoints", get(list_all_checkpoints))
        .route(
            "/api/checkpoints/{history_id}/restore",
            post(restore_checkpoint),
        )
        .route(
            "/api/memories/{id}/checkpoints",
            get(list_memory_checkpoints),
        )
        .route("/api/skills/import", post(import_skills))
        .route("/api/agents", get(list_agents))
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
        .route("/api/effectiveness", get(get_effectiveness_report))
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
    llm_extractor: memvault_core::llm_extractor::LazyLlmExtractor,
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
        let app = build_rest_router(
            store,
            router,
            compliance,
            metrics(),
            None,
            memvault_core::llm_extractor::LazyLlmExtractor::fixed(None),
        );
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
    async fn test_search_min_score_filters_weak_matches() {
        // min_score 是置信度下限:高于所有候选 → 空列表(不是错误);
        // 省略 → 行为不变(back-compat)。
        let app = spawn_app(false).await;
        save(&app, serde_json::json!({ "content": "沙箱环境部署完成了" })).await;

        // Unset: unchanged behavior, result present with a score.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "沙箱" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        let score = results[0]["score"].as_f64().unwrap();

        // Floor below the candidate: still returned.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "沙箱", "min_score": score / 2.0 }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 1);

        // Floor above every candidate: empty list, not an error.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "沙箱", "min_score": score + 0.5 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert!(body["data"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_search_expand_relations_after_consolidation() {
        // End-to-end C3→C5: consolidate two near-identical facts (creating
        // consolidated_from provenance relations), then verify search with
        // expand_relations surfaces them.
        let app = spawn_app(false).await;
        // force_insert: this test deliberately seeds near-identical rows for
        // consolidation — delta-write would (correctly) merge them.
        save(
            &app,
            serde_json::json!({ "content": "the checkout service uses PostgreSQL 15", "force_insert": true }),
        )
        .await;
        save(
            &app,
            serde_json::json!({ "content": "the checkout service uses PostgreSQL 15.", "force_insert": true }),
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
                inject_channel: None,
            }],
        ));
        let app = build_rest_router(
            store,
            router,
            None,
            metrics(),
            None,
            memvault_core::llm_extractor::LazyLlmExtractor::fixed(None),
        );
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
        // force_insert: the rerank test needs two rows with identical content.
        save(
            &app,
            serde_json::json!({
                "content": "authority signal check",
                "tags": ["decision"],
                "force_insert": true,
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
            // force_insert: after tokenize these contents are identical
            // (single digits dropped); pagination needs five distinct rows.
            save(
                &app,
                serde_json::json!({ "content": format!("memory {}", i), "force_insert": true }),
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
    async fn test_session_start_plain_output_for_hooks() {
        let app = spawn_app(false).await;
        save(
            &app,
            serde_json::json!({
                "content": "user prefers Rust",
                "priority": "MUST",
                "agent_id": "alice",
            }),
        )
        .await;

        let resp = app
            .client
            .post(format!("{}/api/session?output=plain", app.base))
            .json(&serde_json::json!({ "agent_id": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("text/plain"),
            "unexpected content-type: {content_type}"
        );
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("prefers Rust"),
            "plain body should carry the instructions, got: {body}"
        );
        assert!(
            !body.trim_start().starts_with('{'),
            "plain body must not be the JSON envelope"
        );
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

    /// Same as `spawn_app`, but wires a caller-provided LLM extractor into
    /// the router (the extract-fallback tests need a failing double).
    async fn spawn_app_with_llm(
        llm: Option<Arc<dyn memvault_core::llm_extractor::LlmExtractor>>,
    ) -> TestApp {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = Arc::new(MemoryRouter::new(store.clone()));
        let app = build_rest_router(
            store,
            router,
            None,
            metrics(),
            None,
            memvault_core::llm_extractor::LazyLlmExtractor::fixed(llm),
        );
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

    /// An LLM extractor whose `extract()` always fails — stands in for a
    /// malformed-JSON model response or a dead Ollama daemon. The handler
    /// must degrade to rule-based extraction, never propagate the error.
    struct FailingExtractor;

    #[async_trait::async_trait]
    impl memvault_core::llm_extractor::LlmExtractor for FailingExtractor {
        async fn extract(
            &self,
            _context: &str,
        ) -> memvault_core::error::Result<Vec<memvault_core::extractor::ExtractedMemory>> {
            Err(memvault_core::error::MemVaultError::LlmExtraction(
                "simulated malformed model output".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn test_extract_memories_llm_mode_without_provider_falls_back() {
        // LLM 契约:无 provider 时 mode=llm 不再 400,而是降级到 rule 并如实上报。
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode", "mode": "llm" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["fallback_used"], true);
        assert!(
            body["data"]["fallback_reason"]
                .as_str()
                .unwrap()
                .contains("MEMVAULT_LLM_EXTRACTION_PROVIDER")
        );
        // 降级路径是 rule 抽取,必须带覆盖面记账——不是干净的 llm 结果。
        assert!(body["data"]["coverage"].is_object());
        assert!(!body["data"]["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_extract_memories_llm_error_falls_back_and_tags() {
        let app = spawn_app_with_llm(Some(Arc::new(FailingExtractor))).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({
                "text": "I always prefer dark mode",
                "mode": "llm",
                "auto_save": true,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["fallback_used"], true);
        assert!(
            body["data"]["fallback_reason"]
                .as_str()
                .unwrap()
                .contains("simulated malformed model output")
        );
        assert!(!body["data"]["saved_ids"].as_array().unwrap().is_empty());

        // 降级产出的记忆必须带 method:llm_fallback_rule 标签,审计时可区分。
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let memories = body["data"].as_array().unwrap();
        assert!(!memories.is_empty());
        assert!(memories.iter().all(|m| {
            m["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "method:llm_fallback_rule")
        }));
    }

    #[tokio::test]
    async fn test_extract_source_id_and_occurred_at_land_on_saved_memory() {
        // 批量摄取的历史对话必须能声明"谁说的"和"何时发生"——
        // source_agent.id 不再被硬编码成 dashboard,occurred_at 独立于
        // created_at(摄取时间)保存。
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({
                "text": "I always prefer dark mode",
                "auto_save": true,
                "source_id": "caroline",
                "occurred_at": "2023-05-07T13:56:00Z",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let saved_id = body["data"]["saved_ids"][0].as_str().unwrap().to_string();

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let mem = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == saved_id)
            .expect("saved memory listed");
        assert_eq!(mem["source_agent"], "caroline");
        assert_eq!(mem["occurred_at"], "2023-05-07T13:56:00Z");
    }

    #[tokio::test]
    async fn test_extract_invalid_occurred_at_is_400_not_silent_drop() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({
                "text": "I always prefer dark mode",
                "auto_save": true,
                "occurred_at": "last tuesday",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("RFC 3339"));
    }

    #[tokio::test]
    async fn test_extract_memories_llm_mode_clean_success_no_fallback_flag() {
        // rule 模式(以及未来干净的 llm 成功路径)fallback_used 必须为 false,
        // 字段恒定存在,客户端无需区分"字段缺失"与"未降级"。
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode" }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["fallback_used"], false);
        assert!(body["data"]["fallback_reason"].is_null());
    }

    #[tokio::test]
    async fn test_extract_memories_auto_save_persists_unreviewed() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/extract", app.base))
            .json(&serde_json::json!({ "text": "I always prefer dark mode", "auto_save": true }))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let saved_ids = body["data"]["saved_ids"].as_array().unwrap();
        assert!(!saved_ids.is_empty());

        let resp = app
            .client
            .get(format!("{}/api/inbox?limit=100", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let inbox_ids: Vec<&str> = body["data"]["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        let saved_id = saved_ids[0].as_str().unwrap();
        assert!(
            inbox_ids.contains(&saved_id),
            "auto-saved extraction must land in the review inbox, unreviewed"
        );
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
    async fn test_agent_import_scan_endpoint_lists_all_adapters() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/agents/import/scan", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().any(|r| r["agent_key"] == "codex"));
    }

    #[tokio::test]
    async fn test_agent_import_preview_does_not_write() {
        let app = spawn_app(false).await;
        let dir = std::env::temp_dir().join(format!(
            "memvault_rest_import_preview_{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        let resp = app
            .client
            .post(format!("{}/api/agents/import/preview", app.base))
            .json(&serde_json::json!({ "agent": "codex", "path": dir.to_string_lossy() }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let candidates = body["data"]["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["duplicate_of"], serde_json::Value::Null);

        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["data"].as_array().unwrap().is_empty(),
            "preview must not write to the store"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_agent_import_run_saves_unreviewed_and_skips_duplicates_on_rerun() {
        let app = spawn_app(false).await;
        let dir = std::env::temp_dir().join(format!(
            "memvault_rest_import_run_{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("AGENTS.md"),
            "# Style\nAlways use four space indentation everywhere\n",
        )
        .unwrap();

        let run_body = serde_json::json!({
            "agent": "codex",
            "path": dir.to_string_lossy(),
            "namespace": "project:custom",
        });

        let resp = app
            .client
            .post(format!("{}/api/agents/import/run", app.base))
            .json(&run_body)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["imported"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"]["duplicates_skipped"], 0);

        let resp = app
            .client
            .get(format!("{}/api/inbox?limit=10", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let inbox = body["data"]["memories"].as_array().unwrap();
        assert_eq!(
            inbox.len(),
            1,
            "imported memory must land unreviewed in the inbox"
        );
        assert_eq!(inbox[0]["namespace"], "project:custom");

        // Re-running against the same file must skip the now-existing duplicate.
        let resp = app
            .client
            .post(format!("{}/api/agents/import/run", app.base))
            .json(&run_body)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["imported"].as_array().unwrap().len(), 0);
        assert_eq!(body["data"]["duplicates_skipped"], 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_agent_import_unknown_agent_is_bad_request() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/agents/import/run", app.base))
            .json(&serde_json::json!({ "agent": "no-such-agent" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_agent_import_not_detected_is_bad_request() {
        let app = spawn_app(false).await;
        let empty_dir = std::env::temp_dir().join(format!(
            "memvault_rest_import_empty_{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&empty_dir).unwrap();
        let resp = app
            .client
            .post(format!("{}/api/agents/import/preview", app.base))
            .json(&serde_json::json!({ "agent": "codex", "path": empty_dir.to_string_lossy() }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        std::fs::remove_dir_all(empty_dir).ok();
    }

    #[tokio::test]
    async fn test_export_import_json_roundtrip() {
        let app = spawn_app(false).await;
        save(&app, save_body("roundtrip me")).await;

        let resp = app
            .client
            .get(format!("{}/api/export?format=json", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["format"], "json");
        let content = body["data"]["content"].as_str().unwrap().to_string();
        assert!(content.contains("roundtrip me"));

        // Importing the exact export output into a fresh store must restore it.
        let app2 = spawn_app(false).await;
        let resp = app2
            .client
            .post(format!("{}/api/import", app2.base))
            .json(&serde_json::json!({ "format": "json", "content": content }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["imported"], 1);

        let resp = app2
            .client
            .get(format!("{}/api/memories", app2.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"][0]["content"], "roundtrip me");
    }

    #[tokio::test]
    async fn test_export_import_markdown_roundtrip() {
        let app = spawn_app(false).await;
        save(&app, save_body("markdown roundtrip")).await;

        let resp = app
            .client
            .get(format!("{}/api/export?format=markdown", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let files = body["data"]["files"].as_array().unwrap().clone();
        assert_eq!(files.len(), 1);

        let app2 = spawn_app(false).await;
        let resp = app2
            .client
            .post(format!("{}/api/import", app2.base))
            .json(&serde_json::json!({ "format": "markdown", "files": files }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["imported"], 1);
        assert!(body["data"]["skipped"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_import_markdown_reports_bad_file_without_failing_the_batch() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!("{}/api/import", app.base))
            .json(&serde_json::json!({
                "format": "markdown",
                "files": [{ "filename": "broken.md", "content": "no frontmatter here" }],
            }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["imported"], 0);
        let skipped = body["data"]["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["filename"], "broken.md");
    }

    #[tokio::test]
    async fn test_backup_endpoint_returns_a_valid_sqlite_file() {
        let app = spawn_app(false).await;
        save(&app, save_body("back me up")).await;

        let resp = app
            .client
            .post(format!("{}/api/backup", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/octet-stream"
        );
        let disposition = resp
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains("memvault-backup-") && disposition.contains(".db"));

        let bytes = resp.bytes().await.unwrap();
        assert!(!bytes.is_empty());
        // SQLite files start with this fixed 16-byte magic header.
        assert_eq!(&bytes[0..16], b"SQLite format 3\0");
    }

    #[tokio::test]
    async fn test_checkpoints_and_restore_roundtrip() {
        let app = spawn_app(false).await;
        let (_status, saved) = save(&app, save_body("version 1")).await;
        let id = saved["data"]["id"].as_str().unwrap().to_string();

        // Edit it once so a history row exists to restore back to.
        let resp = app
            .client
            .put(format!("{}/api/memories/{}", app.base, id))
            .json(&serde_json::json!({ "content": "version 2" }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        let resp = app
            .client
            .get(format!("{}/api/memories/{}/checkpoints", app.base, id))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let entries = body["data"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "one history row snapshotting version 1 before the edit"
        );
        assert_eq!(entries[0]["memory_id"], id);
        let history_id = entries[0]["history_id"].as_i64().unwrap();

        let resp = app
            .client
            .get(format!("{}/api/checkpoints", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(!body["data"].as_array().unwrap().is_empty());

        let resp = app
            .client
            .post(format!(
                "{}/api/checkpoints/{}/restore",
                app.base, history_id
            ))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["content"], "version 1");

        // Confirm the store itself was actually reverted, not just the response.
        let resp = app
            .client
            .get(format!("{}/api/memories", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"][0]["content"], "version 1");
    }

    #[tokio::test]
    async fn test_restore_unknown_history_id_is_not_found() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .post(format!(
                "{}/api/checkpoints/{}/restore",
                app.base, 999_999_999
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_import_skills_endpoint_parses_and_saves() {
        let app = spawn_app(false).await;
        let markdown = "# Deploy the dashboard\n\
trigger: user asks to deploy\n\
1. Build the frontend\n\
2. Run the release script\n\
verification: check the health endpoint\n";

        let resp = app
            .client
            .post(format!("{}/api/skills/import", app.base))
            .json(&serde_json::json!({ "markdown": markdown }))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let imported = body["data"]["imported"].as_array().unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0]["title"], "Deploy the dashboard");
        assert_eq!(imported[0]["steps"], 2);

        // Default `approve=false` lands the skill in the review inbox.
        let resp = app
            .client
            .get(format!("{}/api/inbox?limit=10", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        let inbox = body["data"]["memories"].as_array().unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0]["type"], "Skill");
    }

    #[tokio::test]
    async fn test_list_agents_redacts_api_key() {
        let app = spawn_app_with_admin_key("s3cr3t-plaintext").await;

        let resp = app
            .client
            .get(format!("{}/api/agents", app.base))
            .header(API_KEY_HEADER, "s3cr3t-plaintext")
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let raw = resp.text().await.unwrap();
        assert!(
            !raw.contains("s3cr3t-plaintext"),
            "the api_key value must never appear in the response body"
        );
        assert!(
            !raw.contains("\"api_key\""),
            "the raw api_key field must be omitted entirely (only the derived has_api_key is allowed)"
        );

        let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let profiles = body["data"].as_array().unwrap();
        let admin = profiles.iter().find(|p| p["id"] == "admin").unwrap();
        assert_eq!(admin["has_api_key"], true);
        assert!(admin["inject_rules"]["max_memories"].is_number());
    }

    #[tokio::test]
    async fn test_list_agents_includes_default_profile_without_key() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/agents", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let profiles = body["data"].as_array().unwrap();
        assert!(!profiles.is_empty());
        assert!(profiles.iter().all(|p| p["has_api_key"] == false));
    }

    #[tokio::test]
    async fn test_doctor_endpoint_reports_pending_review() {
        let app = spawn_app(false).await;
        // A freshly saved memory (no human_reviewed override) lands pending review.
        save(&app, save_body("needs a look")).await;

        let resp = app
            .client
            .get(format!("{}/api/doctor", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["data"]["total_memories"].as_u64().unwrap() >= 1);
        let findings = body["data"]["findings"].as_array().unwrap();
        let pending = findings
            .iter()
            .find(|f| f["check"] == "pending_review")
            .expect("pending_review finding must be present");
        assert!(pending["count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_capabilities_endpoint_without_embedder() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/capabilities", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().any(|r| r["available"] == true));
        assert!(rows.iter().any(|r| r["available"] == false));
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
    async fn test_effectiveness_report_reachable_when_enabled() {
        let app = spawn_app(true).await;
        save(
            &app,
            serde_json::json!({ "content": "always set ENV_VAR before deploy", "agent_id": "eve" }),
        )
        .await;
        app.client
            .post(format!("{}/api/session", app.base))
            .json(&serde_json::json!({ "agent_id": "eve" }))
            .send()
            .await
            .unwrap();

        // No LLM judge configured in this harness, so nothing gets judged —
        // this checks the endpoint itself, not the judging logic (that's
        // covered by memvault_core::effectiveness's own unit tests).
        let resp = app
            .client
            .get(format!("{}/api/effectiveness?agent_id=eve", app.base))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["unjudged"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_effectiveness_report_errors_when_disabled() {
        let app = spawn_app(false).await;
        let resp = app
            .client
            .get(format!("{}/api/effectiveness", app.base))
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
            // force_insert: identical after tokenize; offset test needs rows.
            save(
                &app,
                serde_json::json!({ "content": format!("mem {}", i), "force_insert": true }),
            )
            .await;
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
        let app = build_rest_router(
            store,
            router,
            None,
            metrics(),
            None,
            memvault_core::llm_extractor::LazyLlmExtractor::fixed(None),
        );
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

    /// Full system lifecycle over real HTTP: save → failed outcome (rule
    /// lesson) → episode listing → supersede a stale fact → search excludes
    /// the superseded memory → decay cycle → stats stay consistent.
    #[tokio::test]
    async fn test_e2e_full_memory_lifecycle() {
        let app = spawn_app(false).await;

        // 1. Save a fact that will later be superseded.
        let (_status, saved) = save(
            &app,
            serde_json::json!({
                "content": "the checkout service uses PostgreSQL 15",
                "priority": "REFERENCE",
                "namespace": "global",
            }),
        )
        .await;
        assert_eq!(saved["ok"], true);
        let old_id = saved["data"]["id"].as_str().unwrap().to_string();

        // 2. Record a failed outcome → rule-based lesson is distilled.
        let resp = app
            .client
            .post(format!("{}/api/outcome", app.base))
            .json(&serde_json::json!({
                "task": "deploy the checkout service",
                "status": "failure",
                "cause": "DB migration ran out of disk space",
                "task_type": "deploy",
                "agent_id": "tester",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["lesson"]["source"], "rule");
        let lesson_id = body["data"]["lesson"]["memory_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            !lesson_id.is_empty(),
            "a failure with cause must yield a lesson"
        );

        // 3. The episode (with lesson) is queryable.
        let resp = app
            .client
            .get(format!("{}/api/episodes?task_type=deploy", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["count"], 1);
        assert_eq!(
            body["data"]["episodes"][0]["task"],
            "deploy the checkout service"
        );
        assert!(body["data"]["episodes"][0]["lesson"].is_string());

        // 4. Supersede the stale fact with a corrected one.
        // force_insert: "PostgreSQL 16" vs "PostgreSQL 15" is a corrected
        // FACT, not a residual — supersede is the right flow for it, so the
        // new row must not be absorbed by delta-write.
        let (_status, saved) = save(
            &app,
            serde_json::json!({ "content": "the checkout service uses PostgreSQL 16", "force_insert": true }),
        )
        .await;
        let new_id = saved["data"]["id"].as_str().unwrap().to_string();
        let resp = app
            .client
            .post(format!("{}/api/memories/{}/supersede", app.base, old_id))
            .json(&serde_json::json!({ "replacement_id": new_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // 5. Search surfaces the replacement, never the superseded fact.
        let resp = app
            .client
            .post(format!("{}/api/search", app.base))
            .json(&serde_json::json!({ "query": "checkout service", "top_k": 10 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let results = body["data"].as_array().unwrap();
        assert!(
            !results.iter().any(|r| r["memory"]["id"] == old_id),
            "superseded memory must be excluded from search: {}",
            body
        );
        assert!(
            results.iter().any(|r| r["memory"]["id"] == new_id),
            "replacement memory must be searchable"
        );

        // 6. Decay cycle runs over the store.
        let resp = app
            .client
            .post(format!("{}/api/decay", app.base))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());

        // 7. Stats remain consistent after the full flow.
        let resp = app
            .client
            .get(format!("{}/api/stats", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let total = body["data"]["total"].as_u64().unwrap();
        assert!(
            total >= 3,
            "expected fact+episode+lesson(+replacement), got {total}"
        );
    }
}
