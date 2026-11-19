//! 任务级记忆效果评测基准 —— Feature B（见 `docs/DESIGN.md` §9.7）。
//!
//! 灵感来源：Qwen3.8-Flash-Next 技术报告 §2.3.2 —— n-gram 记忆词表扩大时
//! 训练 loss 单调下降，但下游任务性能**饱和甚至波动**；知识类基准提升最稳定，
//! 推理类收益最小。翻译到记忆产品的语境：**"检索召回率 ≠ Agent 任务成功率"**。
//!
//! 因此本模块把评测分三层，逐层逼近"任务级"：
//!
//! 1. **检索层**：把历史失败任务当作查询，它的教训（lesson）是否进入 top-k；
//! 2. **注入层**：走完整 `session_start` 管线后，教训是否真的会被注入；
//! 3. **裁判层**（可选，需要 LLM）：对同一任务生成"无记忆方案"与"带注入记忆方案"，
//!    让裁判对照已知失败原因判定两者是否避开坑——差值就是记忆的任务级价值。
//!
//! 前两层不依赖外部模型，永远可跑；裁判层是 best-effort（无 LLM 时跳过）。
//! 数据全部来自用户自己的 `outcome`/episode 历史——评测的是"这套记忆是否
//! 防止 Agent 重蹈覆辙"，而不是人造数据集上的检索分数。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::agent_adapt::{self, InjectFormat};
use crate::error::{MemVaultError, Result};
use crate::llm_extractor::{ChatEvidence, ChatEvidenceStatus, LlmExtractor};
use crate::models::{
    AgentProfile, ConflictNotice, EpisodeFilter, EpisodeRecord, HitSource, KeywordTier, Memory,
    MemoryLayer, OutcomeStatus, SearchQuery, SearchResult, SkippedMemory,
};
use crate::router::{MemoryRouter, SessionSelectionTrace};
use crate::storage::MemoryStore;

/// 评测配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchConfig {
    /// 用哪个 agent profile 跑 `session_start`（决定注入规则）。
    pub agent_id: String,
    /// 检索层 top-k。
    pub top_k: usize,
    /// 每个样本注入内容的最大字符数（裁判层喂给 LLM 的上限，防止打爆上下文）。
    pub max_injected_chars: usize,
    /// Allow memories still awaiting human review to participate in benchmark
    /// ranking and injection. Production sessions deliberately hide them, which
    /// can make a task-level benchmark report zero injected lessons while all
    /// candidate lessons are healthy and merely pending review. This flag is a
    /// diagnostic override for default benchmark runs, not a production gate.
    pub include_unreviewed: bool,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            agent_id: "default".to_string(),
            top_k: 10,
            max_injected_chars: 4000,
            include_unreviewed: true,
        }
    }
}

/// 一个评测样本：一次有教训的历史任务。
#[derive(Debug, Clone)]
pub struct BenchSample {
    pub episode: EpisodeRecord,
    /// Intake-time convenience snapshot (None only when missing/no backlink).
    /// Evaluation re-fetches the stable episode backlink; this cache is not
    /// authoritative for existence, archival, review, expiry, or selection.
    pub lesson_memory: Option<Memory>,
}

/// Effective, credential-free settings used for this run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchEffectiveConfig {
    pub requested: BenchConfig,
    pub project: Option<String>,
    pub profile: AgentProfile,
    pub resolved_namespace: Option<String>,
    pub require_review: bool,
    pub strict_must: bool,
    pub corroboration_gate: bool,
    pub corroboration_min_agents: usize,
    pub format: InjectFormat,
    pub retrieval_mode: String,
    pub effective_top_k: usize,
    /// The episode limit is applied BEFORE lesson-bearing episode filtering.
    /// `run_bench` receives caller-supplied samples; their intake limit is unknown.
    pub intake_limit_semantics: String,
    pub character_budget_unit: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LessonState {
    pub exists: bool,
    pub live: bool,
    pub namespace: Option<String>,
    pub layer: Option<MemoryLayer>,
    pub human_reviewed: Option<bool>,
    pub ai_generated: Option<bool>,
    pub trusted_for_instructions: Option<bool>,
    pub archived: bool,
    pub superseded_by: Option<String>,
    pub expired: bool,
    pub pending_proposal: bool,
}

impl LessonState {
    fn current(memory: Option<&Memory>) -> Self {
        let Some(memory) = memory else {
            return Self::default();
        };
        let archived = crate::router::is_archived(memory);
        let expired = memory
            .expires_at
            .is_some_and(|expiry| expiry <= chrono::Utc::now());
        let pending_proposal = memory.proposes_supersede_of.is_some();
        Self {
            exists: true,
            live: !archived && !expired && !pending_proposal && memory.superseded_by.is_none(),
            namespace: Some(memory.namespace.clone()),
            layer: Some(memory.layer.clone()),
            human_reviewed: Some(memory.human_reviewed),
            ai_generated: Some(memory.ai_generated),
            trusted_for_instructions: Some(crate::router::format::is_trusted(memory)),
            archived,
            superseded_by: memory.superseded_by.clone(),
            expired,
            pending_proposal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchRetrievalHit {
    pub id: String,
    /// One-based returned rank, independently of source-local ranks.
    pub rank: usize,
    pub score: f64,
    pub hit_sources: Vec<HitSource>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    #[default]
    RetrievalInjectionOnly,
    NativeFourStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchStage {
    PlanControl,
    PlanExperimental,
    JudgeControl,
    JudgeExperimental,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageValidation {
    #[default]
    NotAttempted,
    Unavailable,
    IncompleteResponse,
    InvalidJson,
    Valid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchVerdict {
    pub pass: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchStageResult {
    pub stage: BenchStage,
    pub evidence: ChatEvidence,
    pub validation: StageValidation,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub verdict: Option<BenchVerdict>,
    /// Unicode-character heuristic for the provided system + user prompt;
    /// this is not evidence that a provider sent or billed the request.
    #[serde(default)]
    pub estimated_prompt_tokens: Option<usize>,
    #[serde(default)]
    pub estimated_output_tokens: Option<usize>,
    #[serde(default)]
    pub estimated_plan_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenObservation {
    pub observed_stages: usize,
    /// None means unavailable, not zero. Totals cover observed stages ONLY.
    pub observed_total: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchUsageCoverage {
    pub attempted_stages: usize,
    pub prompt_tokens: TokenObservation,
    pub completion_tokens: TokenObservation,
    pub total_tokens: TokenObservation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchCohort {
    pub name: String,
    pub samples: usize,
    /// Exact task-text identity, not episode count or semantic deduplication.
    pub distinct_tasks: usize,
}

/// 单个样本的评测结果。 Existing fields retain their wire names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleResult {
    pub task: String,
    pub task_type: Option<String>,
    pub status: OutcomeStatus,
    pub lesson_id: Option<String>,
    pub lesson: Option<String>,
    /// 检索层：教训是否进入 top-k。
    pub retrieved: bool,
    /// Legacy injection-layer field: target was SELECTED by the production
    /// pipeline. Judge delivery is separate (`target_delivery_state`).
    pub injected: bool,
    /// Complete judge-delivered formatted context size (Unicode characters),
    /// including trust wrappers/XML and conflict warnings.
    pub injected_chars: usize,
    /// 裁判层：无记忆方案是否避开已知失败原因（None = 未评测）。
    pub pass_without: Option<bool>,
    /// 裁判层：带注入记忆方案是否避开已知失败原因。
    pub pass_with: Option<bool>,
    /// 无记忆方案的估算 token 成本（任务提示词 + 方案文本，约 4 字符/token，
    /// 和 `injected_chars` 用同一种粗估方式——不是真实 API usage）。
    pub tokens_without: Option<usize>,
    /// 带注入记忆方案的估算 token 成本（任务提示词 + 注入上下文 + 方案文本）。
    pub tokens_with: Option<usize>,
    #[serde(default)]
    pub episode_id: String,
    /// Stable episode backlink, even when its memory no longer exists.
    #[serde(default)]
    pub lesson_memory_id: Option<String>,
    #[serde(default)]
    pub outcome_provenance: Option<crate::models::OutcomeProvenance>,
    #[serde(default)]
    pub lesson_state: LessonState,
    #[serde(default)]
    pub retrieval_results: Vec<BenchRetrievalHit>,
    #[serde(default)]
    pub retrieval_keyword_tier: KeywordTier,
    #[serde(default)]
    pub retrieval_rank: Option<usize>,
    #[serde(default)]
    pub selection_trace: SessionSelectionTrace,
    #[serde(default)]
    pub skipped: Vec<SkippedMemory>,
    #[serde(default)]
    pub degraded: Vec<crate::router::format::DegradedMemory>,
    #[serde(default)]
    pub selected_ids: Vec<String>,
    #[serde(default)]
    pub delivered_ids: Vec<String>,
    #[serde(default)]
    pub selected_chars: usize,
    #[serde(default)]
    pub selected_bytes: usize,
    #[serde(default)]
    pub delivered_chars: usize,
    #[serde(default)]
    pub delivered_bytes: usize,
    #[serde(default)]
    pub estimated_selected_tokens: usize,
    #[serde(default)]
    pub estimated_delivered_tokens: usize,
    #[serde(default)]
    pub delivered_text: String,
    #[serde(default)]
    pub conflicts: Vec<ConflictNotice>,
    #[serde(default)]
    pub character_cap_applied: bool,
    #[serde(default)]
    pub character_cap_excluded_ids: Vec<String>,
    #[serde(default)]
    pub formatter_excluded_ids: Vec<String>,
    #[serde(default)]
    pub target_delivery_state: String,
    /// Unknown when the target was never observed by the selector; do not
    /// invent a namespace/rank/gate exclusion reason for that case.
    #[serde(default)]
    pub target_selection_reason: Option<String>,
    #[serde(default)]
    pub stages: Vec<BenchStageResult>,
    #[serde(default)]
    pub formal_pair_valid: bool,
    #[serde(default)]
    pub paired_pass_delta: Option<i8>,
    #[serde(default)]
    pub usage_coverage: BenchUsageCoverage,
}

/// 汇总报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub total: usize,
    /// 教训记忆仍然存在的样本数（检索/注入率的分母）。
    pub with_lesson_memory: usize,
    pub retrieved: usize,
    pub injected: usize,
    /// 裁判层实际评测的样本数。
    pub judged: usize,
    pub pass_without: usize,
    pub pass_with: usize,
    pub avg_injected_chars: f64,
    /// 裁判层样本的平均估算 token 成本（见 [`SampleResult::tokens_without`]）。
    pub avg_tokens_without: f64,
    pub avg_tokens_with: f64,
    /// 按成功率加权的 token 节省比例：对比"无记忆"和"带记忆"各自达成一次
    /// 通过方案所需的期望 token 成本（成本 / 通过率），而不是单次提示词大小——
    /// 带记忆的提示词本身更大，但更可能一次通过，省下的是重试轮次的成本。
    /// `judged == 0` 时为 `None`；"无记忆"通过率为 0 时视为需要无限次重试，
    /// 节省比例封顶在 100%。
    pub token_savings_pct: Option<f64>,
    pub rows: Vec<SampleResult>,
    #[serde(default = "legacy_protocol_version")]
    pub protocol_version: u32,
    #[serde(default)]
    pub effective_config: Option<BenchEffectiveConfig>,
    #[serde(default)]
    pub mode: BenchMode,
    #[serde(default)]
    pub cohorts: Vec<BenchCohort>,
    #[serde(default)]
    pub distinct_tasks: usize,
    #[serde(default)]
    pub delivered: usize,
    #[serde(default)]
    pub paired_pass_rate_delta_pct: Option<f64>,
    #[serde(default)]
    pub usage_coverage: BenchUsageCoverage,
    #[serde(default)]
    pub estimate_label: String,
    #[serde(default)]
    pub token_savings_label: String,
}

fn legacy_protocol_version() -> u32 {
    1
}

/// 从 episode 历史构造评测样本：只保留**有教训**的任务（有 ground truth）。
pub async fn collect_samples(
    store: &dyn MemoryStore,
    max_episodes: usize,
) -> Result<Vec<BenchSample>> {
    let episodes = store
        .list_episodes(EpisodeFilter {
            task_type: None,
            status: None,
            namespace: None,
            session_id: None,
            limit: max_episodes,
        })
        .await?;

    let mut samples = Vec::new();
    for episode in episodes {
        if episode.lesson.is_none() {
            continue;
        }
        // Missing backlinks are diagnostic data. Other storage failures must
        // remain explicit errors, not masquerade as a deleted lesson.
        let lesson_memory = current_lesson(store, episode.lesson_memory_id.as_deref()).await?;
        samples.push(BenchSample {
            episode,
            lesson_memory,
        });
    }
    Ok(samples)
}

async fn current_lesson(store: &dyn MemoryStore, id: Option<&str>) -> Result<Option<Memory>> {
    match id {
        Some(id) => match store.get(id).await {
            Ok(memory) => Ok(Some(memory)),
            Err(MemVaultError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        },
        None => Ok(None),
    }
}

/// Read-only benchmark. No judge runs only keyword retrieval + injection preview.
pub async fn run_bench(
    router: &MemoryRouter,
    store: &dyn MemoryStore,
    samples: &[BenchSample],
    config: &BenchConfig,
    judge: Option<&Arc<dyn LlmExtractor>>,
) -> Result<BenchReport> {
    run_bench_with_project(router, store, samples, config, judge, None).await
}

/// Project-aware variant; the existing config and `run_bench` API stay compatible.
pub async fn run_bench_with_project(
    router: &MemoryRouter,
    store: &dyn MemoryStore,
    samples: &[BenchSample],
    config: &BenchConfig,
    judge: Option<&Arc<dyn LlmExtractor>>,
    project: Option<&str>,
) -> Result<BenchReport> {
    let require_review =
        !config.include_unreviewed && crate::router::format::require_review_enabled();
    let strict_must =
        !config.include_unreviewed && crate::router::format::require_review_for_must();
    let mut profile = router.get_agent_profile(&config.agent_id);
    profile.api_key = None;
    let format = agent_adapt::best_format_for_agent(&profile.agent_type);
    let resolved_namespace = crate::router::resolved_injection_namespace(&profile, project);
    let mut effective_config = BenchEffectiveConfig {
        requested: config.clone(),
        project: project.map(str::to_string),
        profile,
        resolved_namespace,
        require_review,
        strict_must,
        // Record the production formatter's existing policy without changing
        // it or overriding process-global gates for a benchmark.
        corroboration_gate: crate::env_file::parse_bool(
            &std::env::var("MEMVAULT_CORROBORATION_GATE").unwrap_or_default(),
        )
        .unwrap_or(false),
        corroboration_min_agents: std::env::var("MEMVAULT_CORROBORATION_MIN_AGENTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(2),
        format,
        retrieval_mode: "keyword_only".into(),
        effective_top_k: config.top_k.min(crate::models::MAX_TOP_K),
        intake_limit_semantics:
            "newest_episode_limit_before_lesson_filter; caller_sample_limit_unknown".into(),
        character_budget_unit: "Unicode scalar values in complete formatted body plus conflicts"
            .into(),
    };
    let mut rows = Vec::with_capacity(samples.len());

    for sample in samples {
        // The episode backlink is the target, never a cached memory snapshot.
        let lesson_id = sample.episode.lesson_memory_id.clone();
        let current = current_lesson(store, lesson_id.as_deref()).await?;
        let lesson_state = LessonState::current(current.as_ref());
        let outcome = store
            .search(SearchQuery {
                top_k: effective_config.effective_top_k,
                ..SearchQuery::new(sample.episode.task.clone())
            })
            .await?;
        let retrieval_results: Vec<BenchRetrievalHit> = outcome
            .results
            .iter()
            .enumerate()
            .map(|(i, r)| BenchRetrievalHit {
                id: r.memory.id.clone(),
                rank: i + 1,
                score: r.score,
                hit_sources: r.hit_sources.clone(),
            })
            .collect();
        let retrieval_rank = retrieval_results
            .iter()
            .find(|r| Some(&r.id) == lesson_id.as_ref())
            .map(|r| r.rank);

        let mut trace = SessionSelectionTrace::default();
        let injection = router
            .session_preview_with_gates(
                &config.agent_id,
                Some(&sample.episode.task),
                project,
                strict_must,
                require_review,
                Some(&mut trace),
            )
            .await?;
        effective_config.resolved_namespace = trace.resolved_namespace.clone();
        let selected_ids: Vec<String> = injection
            .results
            .iter()
            .map(|r| r.memory.id.clone())
            .collect();
        let injected = lesson_id
            .as_ref()
            .is_some_and(|id| selected_ids.contains(id));
        let conflicts = router.collect_conflicts(&injection.results).await;
        let delivery = format_delivery(
            router,
            &injection.results,
            &conflicts,
            format,
            strict_must,
            config.max_injected_chars,
        );
        let delivered = lesson_id
            .as_ref()
            .is_some_and(|id| delivery.ids.contains(id));
        let target_delivery_state = if lesson_id.is_none() {
            "no_backlink"
        } else if !injected {
            "not_selected"
        } else if delivered {
            "delivered"
        } else if lesson_id
            .as_ref()
            .is_some_and(|id| delivery.formatter_excluded.contains(id))
        {
            "formatter_excluded"
        } else {
            "character_cap_excluded"
        }
        .to_string();
        let target_selection_reason = injection
            .skipped
            .iter()
            .find(|s| Some(&s.id) == lesson_id.as_ref())
            .map(|s| s.reason.to_string());
        let stages = judge_sample(judge.map(|j| j.as_ref()), sample, &delivery.text).await;
        let formal_pair_valid = stages.len() == 4
            && stages
                .iter()
                .all(|s| s.validation == StageValidation::Valid && s.evidence.is_complete());
        let pass_without = formal_pair_valid
            .then(|| stages[2].verdict.as_ref().map(|v| v.pass))
            .flatten();
        let pass_with = formal_pair_valid
            .then(|| stages[3].verdict.as_ref().map(|v| v.pass))
            .flatten();
        // Legacy costs remain explicitly estimated, never substituted for API usage.
        let tokens_without = formal_pair_valid
            .then_some(stages[0].estimated_plan_tokens)
            .flatten()
            .map(|plan| estimate_tokens(&sample.episode.task) + plan);
        let tokens_with = formal_pair_valid
            .then_some(stages[1].estimated_plan_tokens)
            .flatten()
            .map(|plan| {
                estimate_tokens(&sample.episode.task) + estimate_tokens(&delivery.text) + plan
            });
        let usage_coverage = usage_coverage(stages.iter());
        let paired_pass_delta = pass_without
            .zip(pass_with)
            .map(|(c, e)| i8::from(e) - i8::from(c));
        let selected_chars = delivery.full_text.chars().count();
        let delivered_chars = delivery.text.chars().count();
        rows.push(SampleResult {
            task: sample.episode.task.clone(),
            task_type: sample.episode.task_type.clone(),
            status: sample.episode.status,
            lesson_id: lesson_id.clone(),
            lesson: sample.episode.lesson.clone(),
            retrieved: retrieval_rank.is_some(),
            injected,
            injected_chars: delivered_chars,
            pass_without,
            pass_with,
            tokens_without,
            tokens_with,
            episode_id: sample.episode.memory_id.clone(),
            lesson_memory_id: lesson_id,
            outcome_provenance: sample.episode.outcome_provenance.clone(),
            lesson_state,
            retrieval_results,
            retrieval_keyword_tier: outcome.keyword_tier,
            retrieval_rank,
            selection_trace: trace,
            skipped: injection.skipped,
            degraded: injection.degraded,
            selected_ids,
            delivered_ids: delivery.ids,
            selected_chars,
            selected_bytes: delivery.full_text.len(),
            delivered_chars,
            delivered_bytes: delivery.text.len(),
            estimated_selected_tokens: estimate_tokens(&delivery.full_text),
            estimated_delivered_tokens: estimate_tokens(&delivery.text),
            delivered_text: delivery.text,
            conflicts: delivery.conflicts,
            character_cap_applied: !delivery.cap_excluded.is_empty(),
            character_cap_excluded_ids: delivery.cap_excluded,
            formatter_excluded_ids: delivery.formatter_excluded,
            target_delivery_state,
            target_selection_reason,
            stages,
            formal_pair_valid,
            paired_pass_delta,
            usage_coverage,
        });
    }

    let with_lesson_memory = rows.iter().filter(|r| r.lesson_state.exists).count();
    let retrieved = rows.iter().filter(|r| r.retrieved).count();
    let injected = rows.iter().filter(|r| r.injected).count();
    let delivered = rows
        .iter()
        .filter(|r| r.target_delivery_state == "delivered")
        .count();
    let judged = rows.iter().filter(|r| r.formal_pair_valid).count();
    let pass_without = rows.iter().filter(|r| r.pass_without == Some(true)).count();
    let pass_with = rows.iter().filter(|r| r.pass_with == Some(true)).count();
    let avg_injected_chars = if rows.is_empty() {
        0.0
    } else {
        rows.iter().map(|r| r.injected_chars).sum::<usize>() as f64 / rows.len() as f64
    };

    let avg_tokens_without = avg_of(rows.iter().filter_map(|r| r.tokens_without));
    let avg_tokens_with = avg_of(rows.iter().filter_map(|r| r.tokens_with));
    let token_savings_pct = if judged == 0 {
        None
    } else {
        let pass_rate_without = pass_without as f64 / judged as f64;
        let pass_rate_with = pass_with as f64 / judged as f64;
        // Expected token cost to reach one passing plan = cost-per-attempt /
        // pass-rate (geometric-retry approximation). A 0 pass rate means
        // "never succeeds on its own" — treat as needing unboundedly many
        // retries, so savings cap at 100% rather than divide by zero.
        let expected_without = if pass_rate_without > 0.0 {
            Some(avg_tokens_without / pass_rate_without)
        } else {
            None
        };
        let expected_with = if pass_rate_with > 0.0 {
            avg_tokens_with / pass_rate_with
        } else {
            f64::INFINITY
        };
        match expected_without {
            Some(expected_without) if expected_without > 0.0 => Some(
                ((expected_without - expected_with) / expected_without * 100.0)
                    .clamp(-100.0, 100.0),
            ),
            Some(_) => None,
            None => {
                if pass_rate_with > 0.0 {
                    Some(100.0)
                } else {
                    None
                }
            }
        }
    };

    let cohort = |name: &str, predicate: fn(&SampleResult) -> bool| {
        let members: Vec<&SampleResult> = rows.iter().filter(|r| predicate(r)).collect();
        BenchCohort {
            name: name.into(),
            samples: members.len(),
            distinct_tasks: members
                .iter()
                .map(|r| r.task.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
        }
    };
    let cohorts = vec![
        cohort("all_samples", |_| true),
        cohort("current_lesson_memory", |r| r.lesson_state.exists),
        cohort("live_lesson_memory", |r| r.lesson_state.live),
        cohort("retrieved_target", |r| r.retrieved),
        cohort("selected_target", |r| r.injected),
        cohort("delivered_target", |r| {
            r.target_delivery_state == "delivered"
        }),
        cohort("formal_native_pairs", |r| r.formal_pair_valid),
    ];
    let distinct_tasks = cohorts[0].distinct_tasks;
    let usage_coverage = usage_coverage(rows.iter().flat_map(|r| r.stages.iter()));
    let paired_pass_rate_delta_pct =
        (judged > 0).then(|| (pass_with as f64 - pass_without as f64) / judged as f64 * 100.0);
    Ok(BenchReport {
        total: rows.len(),
        with_lesson_memory,
        retrieved,
        injected,
        judged,
        pass_without,
        pass_with,
        avg_injected_chars,
        avg_tokens_without,
        avg_tokens_with,
        token_savings_pct,
        rows,
        protocol_version: 2,
        effective_config: Some(effective_config),
        mode: if judge.is_some() { BenchMode::NativeFourStage } else { BenchMode::RetrievalInjectionOnly },
        cohorts, distinct_tasks, delivered, paired_pass_rate_delta_pct, usage_coverage,
        estimate_label: "heuristic Unicode characters / 4; legacy tokens_without/with are task + plan (+ delivered context), not API usage".into(),
        token_savings_label: "legacy heuristic geometric-retry estimate; not observed billing, retries, or token savings".into(),
    })
}

/// Mean of an iterator of `usize`, or `0.0` for an empty iterator.
fn avg_of(values: impl Iterator<Item = usize>) -> f64 {
    let (sum, count) = values.fold((0usize, 0usize), |(s, c), v| (s + v, c + 1));
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

/// Char-count estimate of token cost (~4 chars/token), matching the existing
/// `injected_chars` heuristic — not a real API usage figure.
fn estimate_tokens(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        (s.chars().count() / 4).max(1)
    }
}

struct Delivery {
    full_text: String,
    text: String,
    ids: Vec<String>,
    conflicts: Vec<ConflictNotice>,
    cap_excluded: Vec<String>,
    formatter_excluded: Vec<String>,
}

/// Fit the largest whole-memory PREFIX, re-rendering complete trust/XML and
/// conflict blocks. Never slice a UTF-8 string or deliver a partial rule.
fn format_delivery(
    router: &MemoryRouter,
    results: &[SearchResult],
    conflicts: &[ConflictNotice],
    format: InjectFormat,
    strict_must: bool,
    max_chars: usize,
) -> Delivery {
    let visible_ids: std::collections::HashSet<&str> = results
        .iter()
        .filter(|r| {
            !agent_adapt::format_memories_with(std::slice::from_ref(r), format, strict_must)
                .is_empty()
        })
        .map(|r| r.memory.id.as_str())
        .collect();
    let render = |prefix: &[SearchResult]| {
        let mut text = agent_adapt::format_memories_with(prefix, format, strict_must);
        let prefix_ids: std::collections::HashSet<&str> =
            prefix.iter().map(|r| r.memory.id.as_str()).collect();
        // The production conflict formatter expects prevalidated pairs. A
        // capped prefix must not receive a dangling header or hidden endpoint.
        let active_conflicts: Vec<ConflictNotice> = conflicts
            .iter()
            .filter(|c| {
                prefix_ids.contains(c.memory_id.as_str())
                    && prefix_ids.contains(c.conflicting_with.as_str())
                    && visible_ids.contains(c.memory_id.as_str())
                    && visible_ids.contains(c.conflicting_with.as_str())
            })
            .cloned()
            .collect();
        text.push_str(&router.render_conflicts(prefix, &active_conflicts));
        text
    };
    let full_text = render(results);
    let (mut text, mut count) = if full_text.chars().count() <= max_chars {
        (full_text.clone(), results.len())
    } else {
        (String::new(), 0)
    };
    if count < results.len() {
        for end in 1..=results.len() {
            let candidate = render(&results[..end]);
            if candidate.chars().count() > max_chars {
                break;
            }
            text = candidate;
            count = end;
        }
    }
    let visible = |r: &SearchResult| visible_ids.contains(r.memory.id.as_str());
    let ids: Vec<String> = results[..count]
        .iter()
        .filter(|r| visible(r))
        .map(|r| r.memory.id.clone())
        .collect();
    let formatter_excluded = results
        .iter()
        .filter(|r| !visible(r))
        .map(|r| r.memory.id.clone())
        .collect();
    let cap_excluded = results[count..]
        .iter()
        .filter(|r| visible(r))
        .map(|r| r.memory.id.clone())
        .collect();
    let conflicts = conflicts
        .iter()
        .filter(|c| ids.contains(&c.memory_id) && ids.contains(&c.conflicting_with))
        .cloned()
        .collect();
    Delivery {
        full_text,
        text,
        ids,
        conflicts,
        cap_excluded,
        formatter_excluded,
    }
}

fn usage_coverage<'a>(stages: impl Iterator<Item = &'a BenchStageResult>) -> BenchUsageCoverage {
    let mut coverage = BenchUsageCoverage::default();
    fn observe(observation: &mut TokenObservation, tokens: Option<u64>) {
        if let Some(tokens) = tokens {
            observation.observed_stages += 1;
            observation.observed_total = if observation.observed_stages == 1 {
                Some(tokens)
            } else {
                // An unrepresentable total is unknown, never a saturated or
                // restarted guessed value. Coverage still records the inputs.
                observation
                    .observed_total
                    .and_then(|sum| sum.checked_add(tokens))
            };
        }
    }
    for stage in stages {
        if !matches!(
            stage.evidence.status,
            ChatEvidenceStatus::NotAttempted | ChatEvidenceStatus::Unsupported
        ) {
            coverage.attempted_stages += 1;
        }
        observe(&mut coverage.prompt_tokens, stage.evidence.prompt_tokens);
        observe(
            &mut coverage.completion_tokens,
            stage.evidence.completion_tokens,
        );
        observe(&mut coverage.total_tokens, stage.evidence.total_tokens);
    }
    coverage
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictPlan {
    plan: String,
}

fn strict_plan(raw: &str) -> Option<String> {
    let plan: StrictPlan = serde_json::from_str(raw).ok()?;
    (!plan.plan.trim().is_empty()).then_some(plan.plan)
}

fn strict_verdict(raw: &str) -> Option<BenchVerdict> {
    let verdict: BenchVerdict = serde_json::from_str(raw).ok()?;
    (!verdict.reason.trim().is_empty()).then_some(verdict)
}

/// Native protocol: plan C -> plan E -> judge C -> judge E, one request each.
/// A broken stage stops the sequence but preserves earlier evidence. None is
/// unknown/unassessed, never an adverse task verdict.
async fn judge_sample(
    judge: Option<&dyn LlmExtractor>,
    sample: &BenchSample,
    injected: &str,
) -> Vec<BenchStageResult> {
    let mut stages: Vec<BenchStageResult> = [
        BenchStage::PlanControl,
        BenchStage::PlanExperimental,
        BenchStage::JudgeControl,
        BenchStage::JudgeExperimental,
    ]
    .into_iter()
    .map(|stage| BenchStageResult {
        stage,
        evidence: ChatEvidence::not_attempted("previous_stage_not_complete"),
        validation: StageValidation::NotAttempted,
        plan: None,
        verdict: None,
        estimated_prompt_tokens: None,
        estimated_output_tokens: None,
        estimated_plan_tokens: None,
    })
    .collect();
    let Some(judge) = judge else {
        for stage in &mut stages {
            stage.evidence = ChatEvidence::not_attempted("judge_not_configured");
        }
        return stages;
    };
    if sample
        .episode
        .cause
        .as_deref()
        .is_none_or(|s| s.trim().is_empty())
        && sample
            .episode
            .lesson
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
    {
        for stage in &mut stages {
            stage.evidence = ChatEvidence::not_attempted("no_nonempty_pitfall_ground_truth");
        }
        return stages;
    }
    // Original plans drive subsequent requests but never enter the report.
    let mut operational_plans: [Option<String>; 2] = [None, None];
    for (index, result) in stages.iter_mut().enumerate() {
        let (system, user) = if index < 2 {
            (
                PLAN_SYSTEM,
                plan_prompt(&sample.episode.task, (index == 1).then_some(injected)),
            )
        } else {
            let plan = operational_plans[index - 2].as_deref().unwrap_or("");
            (
                JUDGE_SYSTEM,
                verdict_prompt(&sample.episode.task, &sample.episode, plan),
            )
        };
        let evidence = judge.json_chat_evidence(system, &user).await;
        let raw = evidence.text_for_validation();
        let plan = if index < 2 {
            raw.and_then(strict_plan)
        } else {
            None
        };
        let verdict = if index >= 2 {
            raw.and_then(strict_verdict)
        } else {
            None
        };
        let validation = match evidence.status {
            ChatEvidenceStatus::NotAttempted => StageValidation::NotAttempted,
            ChatEvidenceStatus::Unsupported | ChatEvidenceStatus::Error => {
                StageValidation::Unavailable
            }
            ChatEvidenceStatus::Response if !evidence.is_complete() => {
                StageValidation::IncompleteResponse
            }
            ChatEvidenceStatus::Response if plan.is_none() && verdict.is_none() => {
                StageValidation::InvalidJson
            }
            ChatEvidenceStatus::Response => StageValidation::Valid,
        };
        let estimated_output_tokens = raw.map(estimate_tokens);
        let estimated_plan_tokens = plan.as_deref().map(estimate_tokens);
        if index < 2 {
            operational_plans[index] = plan;
        }
        // Validate/execute original text; redact only cloned report values.
        // Recording cannot upgrade schema validity or alter a judge request.
        let record_text = |text: &str| {
            let mut value = serde_json::Value::String(text.to_string());
            evidence.redact_record_value(&mut value);
            value
                .as_str()
                .unwrap_or("[record text redacted]")
                .to_string()
        };
        let plan = if index < 2 {
            operational_plans[index].as_deref().map(record_text)
        } else {
            None
        };
        let verdict = verdict.map(|original| BenchVerdict {
            pass: original.pass,
            reason: record_text(&original.reason),
        });
        *result = BenchStageResult {
            stage: result.stage,
            evidence,
            validation,
            plan,
            verdict,
            estimated_prompt_tokens: Some(estimate_tokens(system) + estimate_tokens(&user)),
            estimated_output_tokens,
            estimated_plan_tokens,
        };
        if validation != StageValidation::Valid {
            warn!(stage = ?result.stage, ?validation, "bench native stage not valid; retaining evidence and stopping pair");
            break;
        }
    }
    stages
}

const PLAN_SYSTEM: &str = "You are an AI agent planning how to execute a task. \
Reply with exactly JSON {\"plan\": string} listing concrete steps in a nonempty string. \
Be brief and specific. No code fences, duplicate keys, or extra fields.";

const JUDGE_SYSTEM: &str = "You are a strict reviewer. A previous attempt at this exact task \
did not fully succeed. You are given the known failure attribution and/or the lesson learned, \
plus a newly proposed plan. Decide whether the plan AVOIDS the known pitfall \
(actually prevents it, not merely mentions it). Reply with exactly JSON {\"pass\": boolean, \"reason\": string} \
with a nonempty reason. No code fences, duplicate keys, coercions, or extra fields.";

fn plan_prompt(task: &str, injected: Option<&str>) -> String {
    match injected {
        Some(mem) if !mem.trim().is_empty() => format!(
            "Relevant memories from past experience:\n{}\n\nTask: {}",
            mem, task
        ),
        _ => format!("Task: {}", task),
    }
}

fn verdict_prompt(task: &str, episode: &EpisodeRecord, plan: &str) -> String {
    let mut known = String::new();
    if let Some(cause) = episode.cause.as_deref().filter(|s| !s.trim().is_empty()) {
        known.push_str(&format!("Known failure cause: {}\n", cause));
    }
    if let Some(lesson) = episode.lesson.as_deref().filter(|s| !s.trim().is_empty()) {
        known.push_str(&format!("Lesson learned: {}\n", lesson));
    }
    format!("Task: {}\n{}Proposed plan: {}", task, known, plan)
}

/// 宽容解析 JSON 字符串字段（允许代码栅栏包裹）。
/// Shared with [`crate::effectiveness`]'s judge, which follows the same
/// best-effort JSON-verdict pattern.
pub(crate) fn parse_field(raw: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(strip_fences(raw)).ok()?;
    value.get(field)?.as_str().map(str::to_string)
}

pub(crate) fn strip_fences(raw: &str) -> &str {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim();
        }
        return rest.trim();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::{OutcomeInput, record_outcome};
    use crate::models::{MemoryType, Priority, SourceAgent};
    use crate::reflection::reflect_and_store;
    use crate::storage::sqlite::SqliteStore;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "bench".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    /// Record a failed deploy outcome AND run rule-based reflection on it, so
    /// the episode carries a lesson + lesson memory (real CLI/MCP flow).
    async fn store_with_failed_episode() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let input = OutcomeInput {
            task: "deploy the checkout service to production".to_string(),
            status: OutcomeStatus::Failure,
            cause: Some("disk space insufficient on target host".to_string()),
            task_type: Some("deploy".to_string()),
            skill_id: None,
            tags: vec!["deploy".to_string()],
            namespace: "global".to_string(),
            source_agent: agent(),
            outcome_provenance: None,
        };
        let recorded = record_outcome(store.as_ref(), input.clone(), None)
            .await
            .expect("record outcome");
        let lesson = reflect_and_store(
            store.as_ref(),
            &recorded.memory.id,
            input.into(),
            None,
            None,
        )
        .await
        .expect("reflect lesson")
        .expect("failure with cause reflects a lesson");
        // The benchmark fixture exercises retrieval/injection, not the review
        // gate: mark the auto-generated lesson reviewed as a human would.
        let mut lesson_memory = lesson.lesson_memory;
        lesson_memory.human_reviewed = true;
        store.update(lesson_memory).await.expect("review lesson");
        store
    }

    #[tokio::test]
    async fn test_collect_samples_keeps_only_lesson_episodes() {
        let store = store_with_failed_episode().await;
        // A success episode adds no benchmark ground truth.
        record_outcome(
            store.as_ref(),
            OutcomeInput {
                task: "run unit tests".to_string(),
                status: OutcomeStatus::Success,
                cause: None,
                task_type: Some("test".to_string()),
                skill_id: None,
                tags: vec![],
                namespace: "global".to_string(),
                source_agent: agent(),
                outcome_provenance: None,
            },
            None,
        )
        .await
        .unwrap();

        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        assert_eq!(samples.len(), 1, "only the failure-with-lesson counts");
        assert!(samples[0].lesson_memory.is_some(), "lesson memory resolved");
        assert_eq!(
            samples[0].episode.task,
            "deploy the checkout service to production"
        );
    }

    #[tokio::test]
    async fn test_run_bench_retrieval_and_injection_layers() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let router = MemoryRouter::new(store.clone());

        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.total, 1);
        assert_eq!(report.with_lesson_memory, 1);
        // The lesson is stored with task_type matching; session_start must
        // surface it for the same task — that is the whole point of the
        // outcome→lesson→injection loop.
        assert!(report.injected >= report.retrieved);
        assert!(
            report.rows[0].injected,
            "lesson for the exact same task must be injected: {:?}",
            report.rows[0]
        );
        // Without a judge these stay None.
        assert!(report.rows[0].pass_without.is_none());
        assert!(report.rows[0].pass_with.is_none());
        assert_eq!(report.judged, 0);
    }

    #[tokio::test]
    async fn test_run_bench_defaults_include_unreviewed_lessons() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let lesson_id = samples[0]
            .lesson_memory
            .as_ref()
            .expect("fixture stores a live lesson memory")
            .id
            .clone();
        let mut lesson = store.get(&lesson_id).await.unwrap();
        lesson.human_reviewed = false;
        store.update(lesson).await.expect("stage the lesson draft");

        let router = MemoryRouter::new(store.clone());
        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert!(
            report.rows[0].retrieved,
            "the lesson is still in search results"
        );
        assert!(
            report.rows[0].injected,
            "the benchmark default must not hide a draft lesson behind the review backlog"
        );
    }

    #[tokio::test]
    async fn test_sample_without_lesson_memory_still_reported() {
        let store = store_with_failed_episode().await;
        let mut samples = collect_samples(store.as_ref(), 50).await.unwrap();
        // Simulate the lesson memory having been deleted meanwhile.
        let lesson_id = samples[0].lesson_memory.as_ref().unwrap().id.clone();
        store.delete(&lesson_id).await.unwrap();
        samples[0].lesson_memory = None;

        let router = MemoryRouter::new(store.clone());
        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.with_lesson_memory, 0);
        assert!(!report.rows[0].retrieved);
        assert!(!report.rows[0].injected);
    }

    /// Judge stub that always claims the plan passes.
    struct AlwaysPassJudge;
    #[async_trait::async_trait]
    impl LlmExtractor for AlwaysPassJudge {
        async fn extract(
            &self,
            _context: &str,
        ) -> crate::error::Result<Vec<crate::extractor::ExtractedMemory>> {
            Ok(vec![])
        }
        async fn json_chat_evidence(&self, system: &str, _user: &str) -> ChatEvidence {
            let mut evidence = ChatEvidence::not_attempted("explicit offline fixture");
            evidence.status = ChatEvidenceStatus::Response;
            evidence.finish_reason = Some("stop".into());
            evidence.text = Some(if system.starts_with("You are an AI agent planning") {
                "{\"plan\": \"check disk space first, then deploy\"}".into()
            } else {
                "{\"pass\": true, \"reason\": \"avoids the pitfall\"}".into()
            });
            evidence
        }
    }

    #[tokio::test]
    async fn test_judge_layer_populates_pass_fields() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let router = MemoryRouter::new(store.clone());
        let judge: Arc<dyn LlmExtractor> = Arc::new(AlwaysPassJudge);

        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            Some(&judge),
        )
        .await
        .unwrap();

        assert_eq!(report.judged, 1);
        assert_eq!(report.pass_without, 1);
        assert_eq!(report.pass_with, 1);
        assert_eq!(report.rows[0].pass_without, Some(true));
        assert_eq!(report.rows[0].pass_with, Some(true));
        assert!(report.rows[0].tokens_without.is_some());
        assert!(report.rows[0].tokens_with.is_some());
        assert!(report.avg_tokens_without > 0.0);
        assert!(report.avg_tokens_with > 0.0);
        // Both conditions pass equally in this stub, so savings is defined
        // (not None) even though it needn't be positive — the "with" prompt
        // carries extra injected-context tokens for the same pass rate.
        assert!(report.token_savings_pct.is_some());
    }

    /// Judge that cannot produce plans → judging degrades to None, no error.
    struct SilentJudge;
    #[async_trait::async_trait]
    impl LlmExtractor for SilentJudge {
        async fn extract(
            &self,
            _context: &str,
        ) -> crate::error::Result<Vec<crate::extractor::ExtractedMemory>> {
            Ok(vec![])
        }
        async fn json_chat(&self, _system: &str, _user: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn test_judge_degrades_gracefully() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let router = MemoryRouter::new(store.clone());
        let judge: Arc<dyn LlmExtractor> = Arc::new(SilentJudge);

        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            Some(&judge),
        )
        .await
        .unwrap();

        assert_eq!(report.judged, 0, "silent judge must degrade, not fail");
        assert!(report.rows[0].tokens_without.is_none());
        assert!(report.rows[0].tokens_with.is_none());
        assert_eq!(report.avg_tokens_without, 0.0);
        assert_eq!(report.avg_tokens_with, 0.0);
        assert!(
            report.token_savings_pct.is_none(),
            "no judged samples means no savings figure"
        );
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("ab"), 1, "non-empty text rounds up to >=1");
        assert_eq!(estimate_tokens(&"x".repeat(40)), 10);
    }

    #[test]
    fn test_avg_of() {
        assert_eq!(avg_of(std::iter::empty()), 0.0);
        assert_eq!(avg_of(vec![10usize, 20, 30].into_iter()), 20.0);
    }

    #[test]
    fn test_parse_field_and_bool() {
        assert_eq!(
            parse_field("{\"plan\": \"do it\"}", "plan").as_deref(),
            Some("do it")
        );
        assert_eq!(
            parse_field("```json\n{\"plan\": \"do it\"}\n```", "plan").as_deref(),
            Some("do it")
        );
        assert_eq!(
            strict_verdict("{\"pass\": true, \"reason\": \"clear\"}").map(|v| v.pass),
            Some(true)
        );
        assert!(strict_verdict("not json").is_none());
        assert_eq!(parse_field("{\"other\": 1}", "plan"), None);
    }

    #[test]
    fn test_injection_text_respects_budget() {
        let memory = Memory::new(
            MemoryType::Fact,
            "a very long fact ".repeat(100),
            Priority::Reference,
            agent(),
        );
        let results = vec![crate::models::SearchResult {
            memory,
            score: 1.0,
            hit_sources: vec![],
            as_of_source: None,
        }];
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let delivery = format_delivery(&router, &results, &[], InjectFormat::Xml, false, 50);
        assert!(delivery.text.chars().count() <= 50);
        assert!(
            delivery.text.is_empty(),
            "oversized first memory is excluded whole"
        );
    }

    #[test]
    fn test_injection_text_prefers_instruction() {
        let mut memory = Memory::new(
            MemoryType::Fact,
            "raw content".to_string(),
            Priority::Must,
            agent(),
        );
        memory.instruction = Some("Always check disk space".to_string());
        memory.human_reviewed = true;
        let results = vec![crate::models::SearchResult {
            memory,
            score: 1.0,
            hit_sources: vec![],
            as_of_source: None,
        }];
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let router = MemoryRouter::new(store);
        let delivery = format_delivery(&router, &results, &[], InjectFormat::MustRef, false, 1000);
        assert!(delivery.text.contains("[MUST] Always check disk space"));
        assert!(!delivery.text.contains("raw content"));
    }

    fn response(text: &str) -> ChatEvidence {
        let mut evidence = ChatEvidence::not_attempted("explicit offline fixture");
        evidence.status = ChatEvidenceStatus::Response;
        evidence.text = Some(text.into());
        evidence.finish_reason = Some("stop".into());
        evidence
    }

    struct ScriptedJudge {
        responses: std::sync::Mutex<std::collections::VecDeque<ChatEvidence>>,
        calls: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl ScriptedJudge {
        fn new(responses: Vec<ChatEvidence>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                calls: std::sync::Mutex::new(vec![]),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmExtractor for ScriptedJudge {
        async fn extract(&self, _: &str) -> Result<Vec<crate::extractor::ExtractedMemory>> {
            Ok(vec![])
        }
        async fn json_chat_evidence(&self, system: &str, user: &str) -> ChatEvidence {
            self.calls
                .lock()
                .unwrap()
                .push((system.into(), user.into()));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("at most four scripted requests")
        }
    }

    #[test]
    fn native_json_is_strict_and_legacy_parser_is_unchanged() {
        assert_eq!(
            strict_plan("{\"plan\":\"check first\"}").as_deref(),
            Some("check first")
        );
        for raw in [
            "{\"plan\":\"\"}",
            "{\"plan\":\"   \"}",
            "{\"plan\":null}",
            "{\"plan\":1}",
            "{\"plan\":\"a\",\"plan\":\"b\"}",
            "{\"plan\":\"a\",\"extra\":true}",
            "```json\n{\"plan\":\"a\"}\n```",
        ] {
            assert!(strict_plan(raw).is_none(), "reject {raw}");
        }
        for raw in [
            "{\"pass\":true}",
            "{\"pass\":true,\"reason\":\"\"}",
            "{\"pass\":\"true\",\"reason\":\"a\"}",
            "{\"pass\":1,\"reason\":\"a\"}",
            "{\"pass\":true,\"pass\":false,\"reason\":\"a\"}",
            "{\"pass\":true,\"reason\":\"a\",\"reason\":\"b\"}",
            "```json\n{\"pass\":true,\"reason\":\"a\"}\n```",
        ] {
            assert!(strict_verdict(raw).is_none(), "reject {raw}");
        }
        assert_eq!(
            parse_field("```json\n{\"plan\":\"legacy\"}\n```", "plan").as_deref(),
            Some("legacy")
        );
    }

    #[tokio::test]
    async fn native_stops_at_every_invalid_stage_and_retains_earlier_records() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        for failed in 0..4 {
            let mut responses = vec![
                response("{\"plan\":\"control\"}"),
                response("{\"plan\":\"experimental\"}"),
                response("{\"pass\":true,\"reason\":\"control clear\"}"),
                response("{\"pass\":false,\"reason\":\"experimental fails\"}"),
            ];
            responses[failed].text = Some("{\"not_the_schema\":true}".into());
            let judge = ScriptedJudge::new(responses);
            let stages = judge_sample(Some(&judge), &samples[0], "injected context").await;
            assert_eq!(judge.calls.lock().unwrap().len(), failed + 1);
            assert_eq!(stages.len(), 4);
            assert!(
                stages[..failed]
                    .iter()
                    .all(|s| s.validation == StageValidation::Valid)
            );
            assert_eq!(stages[failed].validation, StageValidation::InvalidJson);
            assert!(
                stages[failed + 1..]
                    .iter()
                    .all(|s| s.evidence.status == ChatEvidenceStatus::NotAttempted)
            );
            if failed == 3 {
                assert_eq!(stages[2].verdict.as_ref().map(|v| v.pass), Some(true));
            }
        }
    }

    #[tokio::test]
    async fn native_unknown_or_truncated_finish_never_counts_as_a_failure_verdict() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        for finish in [None, Some("length"), Some("content_filter")] {
            let mut evidence = response("{\"plan\":\"valid JSON is not sufficient\"}");
            evidence.finish_reason = finish.map(str::to_string);
            let judge = ScriptedJudge::new(vec![evidence]);
            let stages = judge_sample(Some(&judge), &samples[0], "").await;
            assert_eq!(judge.calls.lock().unwrap().len(), 1);
            assert_eq!(stages[0].validation, StageValidation::IncompleteResponse);
            assert!(stages[0].plan.is_some(), "diagnostic parsing is preserved");
            assert!(
                stages[1..]
                    .iter()
                    .all(|s| s.validation == StageValidation::NotAttempted)
            );
        }
    }

    #[tokio::test]
    async fn native_pair_requires_all_four_stages_and_reports_optional_usage_coverage() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let mut first = response("{\"plan\":\"control\"}");
        first.prompt_tokens = Some(17);
        first.completion_tokens = Some(3);
        first.total_tokens = Some(20);
        let mut last = response("{\"pass\":false,\"reason\":\"bad outcome\"}");
        last.prompt_tokens = Some(23);
        let scripted = Arc::new(ScriptedJudge::new(vec![
            first,
            response("{\"plan\":\"experimental\"}"),
            response("{\"pass\":true,\"reason\":\"control clear\"}"),
            last,
        ]));
        let judge: Arc<dyn LlmExtractor> = scripted.clone();
        let router = MemoryRouter::new(store.clone());
        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            Some(&judge),
        )
        .await
        .unwrap();
        assert_eq!(report.judged, 1);
        assert_eq!(report.rows[0].paired_pass_delta, Some(-1));
        assert_eq!(report.paired_pass_rate_delta_pct, Some(-100.0));
        assert_eq!(report.usage_coverage.attempted_stages, 4);
        assert_eq!(report.usage_coverage.prompt_tokens.observed_stages, 2);
        assert_eq!(report.usage_coverage.prompt_tokens.observed_total, Some(40));
        assert_eq!(report.usage_coverage.completion_tokens.observed_stages, 1);
        assert_eq!(report.usage_coverage.total_tokens.observed_total, Some(20));
        {
            let calls = scripted.calls.lock().unwrap();
            assert_eq!(calls.len(), 4);
            assert!(calls[0].0 == PLAN_SYSTEM && calls[1].0 == PLAN_SYSTEM);
            assert!(calls[2].0 == JUDGE_SYSTEM && calls[3].0 == JUDGE_SYSTEM);
            assert!(!calls[0].1.contains("Relevant memories"));
            assert!(calls[1].1.contains(&report.rows[0].delivered_text));
            assert!(calls[2].1.contains("Proposed plan: control"));
            assert!(calls[3].1.contains("Proposed plan: experimental"));
        }
        let partial: Arc<dyn LlmExtractor> = Arc::new(ScriptedJudge::new(vec![
            response("{\"plan\":\"c\"}"),
            response("{\"plan\":\"e\"}"),
            response("{\"pass\":true,\"reason\":\"clear\"}"),
            response("{\"pass\":true}"),
        ]));
        let report = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            Some(&partial),
        )
        .await
        .unwrap();
        assert_eq!(report.judged, 0);
        assert_eq!(report.rows[0].pass_without, None);
        assert_eq!(report.rows[0].pass_with, None);
        assert_eq!(
            report.rows[0].stages[2].verdict.as_ref().map(|v| v.pass),
            Some(true)
        );
        assert_eq!(report.paired_pass_rate_delta_pct, None);
        assert_eq!(report.usage_coverage.total_tokens.observed_total, None);
    }

    #[tokio::test]
    async fn legacy_text_only_judge_cannot_create_a_formal_native_pair() {
        struct LegacyJudge;
        #[async_trait::async_trait]
        impl LlmExtractor for LegacyJudge {
            async fn extract(&self, _: &str) -> Result<Vec<crate::extractor::ExtractedMemory>> {
                Ok(vec![])
            }
            async fn json_chat(&self, _: &str, _: &str) -> Result<Option<String>> {
                Ok(Some("{\"plan\":\"legacy response\"}".into()))
            }
        }
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let stages = judge_sample(Some(&LegacyJudge), &samples[0], "").await;
        assert_eq!(stages[0].validation, StageValidation::IncompleteResponse);
        assert_eq!(stages[0].evidence.finish_reason, None);
        assert_eq!(stages[0].evidence.total_tokens, None);
        assert!(
            stages[1..]
                .iter()
                .all(|s| s.validation == StageValidation::NotAttempted)
        );
    }

    #[test]
    fn whole_prefix_character_cap_keeps_xml_trust_and_conflicts_complete() {
        let router = MemoryRouter::new(Arc::new(SqliteStore::in_memory().unwrap()));
        let mut first = Memory::new(
            MemoryType::Fact,
            "规则<&😀中文".into(),
            Priority::Reference,
            agent(),
        );
        first.human_reviewed = true;
        first.instruction = Some("引用<&😀中文".into());
        let second = Memory::new(MemoryType::Fact, "待审<&😀".into(), Priority::Must, agent());
        let results = vec![
            SearchResult {
                memory: first,
                score: 1.0,
                hit_sources: vec![],
                as_of_source: None,
            },
            SearchResult {
                memory: second,
                score: 0.9,
                hit_sources: vec![],
                as_of_source: None,
            },
        ];
        let conflicts = vec![ConflictNotice {
            memory_id: results[0].memory.id.clone(),
            conflicting_with: results[1].memory.id.clone(),
        }];
        for format in [
            InjectFormat::Xml,
            InjectFormat::MustRef,
            InjectFormat::SystemPrompt,
            InjectFormat::Markdown,
        ] {
            let all = format_delivery(&router, &results, &conflicts, format, false, usize::MAX);
            let exact = format_delivery(
                &router,
                &results,
                &conflicts,
                format,
                false,
                all.full_text.chars().count(),
            );
            assert_eq!(exact.text, all.full_text);
            assert_eq!(exact.ids.len(), 2);
            assert!(exact.text.contains("MEMORY CONFLICT"));
            assert!(
                exact.text.len() > exact.text.chars().count(),
                "Unicode budget is not byte length"
            );
            let one = format_delivery(
                &router,
                &results[..1],
                &conflicts,
                format,
                false,
                usize::MAX,
            );
            let limited = format_delivery(
                &router,
                &results,
                &conflicts,
                format,
                false,
                one.text.chars().count(),
            );
            assert_eq!(limited.text, one.text);
            assert_eq!(limited.ids, vec![results[0].memory.id.clone()]);
            assert_eq!(limited.cap_excluded, vec![results[1].memory.id.clone()]);
            assert!(!limited.text.contains("MEMORY CONFLICT"));
            if format == InjectFormat::Xml {
                assert!(limited.text.ends_with("</memory_context>\n"));
                assert!(all.text.contains("</unvetted_reference>"));
                assert!(all.text.contains("&lt;&amp;"));
                assert!(!all.text.contains("priority=\"must\""));
            }
            let zero = format_delivery(&router, &results, &conflicts, format, false, 0);
            assert!(zero.text.is_empty());
            assert!(zero.ids.is_empty());
        }
    }

    #[test]
    fn all_fit_delivery_preserves_large_complete_rendering() {
        let router = MemoryRouter::new(Arc::new(SqliteStore::in_memory().unwrap()));
        let results: Vec<SearchResult> = (0..128)
            .map(|index| {
                let mut memory = Memory::new(
                    MemoryType::Fact,
                    format!("Rule {index}: {}", "检查<&🙂".repeat(128)),
                    Priority::Must,
                    agent(),
                );
                memory.human_reviewed = true;
                memory.instruction = Some(memory.content.clone());
                SearchResult {
                    memory,
                    score: 1.0,
                    hit_sources: vec![],
                    as_of_source: None,
                }
            })
            .collect();
        let conflicts = vec![ConflictNotice {
            memory_id: results[0].memory.id.clone(),
            conflicting_with: results[127].memory.id.clone(),
        }];
        for format in [
            InjectFormat::Xml,
            InjectFormat::MustRef,
            InjectFormat::SystemPrompt,
            InjectFormat::Markdown,
        ] {
            let delivery =
                format_delivery(&router, &results, &conflicts, format, false, usize::MAX);
            assert_eq!(delivery.text, delivery.full_text);
            assert_eq!(delivery.ids.len(), results.len());
            assert!(delivery.text.contains("MEMORY CONFLICT"));
            assert!(delivery.cap_excluded.is_empty());
            assert!(delivery.formatter_excluded.is_empty());
            let exact = format_delivery(
                &router,
                &results,
                &conflicts,
                format,
                false,
                delivery.text.chars().count(),
            );
            assert_eq!(exact.text, delivery.text);
            assert_eq!(exact.ids, delivery.ids);
        }
    }

    #[tokio::test]
    async fn benchmark_is_read_only_and_current_state_ignores_cached_lesson() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let lesson_id = samples[0].episode.lesson_memory_id.as_ref().unwrap();
        let router = MemoryRouter::new(store.clone());
        let before = serde_json::to_value(store.list(None, 100, 0).await.unwrap()).unwrap();
        let config = BenchConfig {
            max_injected_chars: 0,
            ..BenchConfig::default()
        };
        for _ in 0..2 {
            let report = run_bench(&router, store.as_ref(), &samples, &config, None)
                .await
                .unwrap();
            assert_eq!(report.protocol_version, 2);
            assert!(report.rows[0].injected);
            assert_eq!(
                report.rows[0].target_delivery_state,
                "character_cap_excluded"
            );
            assert_eq!(report.rows[0].delivered_chars, 0);
            assert_eq!(report.delivered, 0);
        }
        assert_eq!(
            before,
            serde_json::to_value(store.list(None, 100, 0).await.unwrap()).unwrap()
        );
        let mut retired = store.get(lesson_id).await.unwrap();
        retired.layer = MemoryLayer::L0;
        store.update(retired).await.unwrap();
        let archived = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(archived.rows[0].lesson_state.layer, Some(MemoryLayer::L0));
        assert!(!archived.rows[0].lesson_state.live);
        assert!(!archived.rows[0].injected);
        assert_eq!(
            archived.rows[0].target_selection_reason.as_deref(),
            Some("archived-layer")
        );
        store.delete(lesson_id).await.unwrap();
        let missing = run_bench(
            &router,
            store.as_ref(),
            &samples,
            &BenchConfig::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!missing.rows[0].lesson_state.exists);
        assert_eq!(missing.rows[0].lesson_id.as_ref(), Some(lesson_id));
        assert_eq!(missing.rows[0].lesson_memory_id.as_ref(), Some(lesson_id));
        assert_eq!(missing.with_lesson_memory, 0);
        assert_eq!(
            missing.rows[0].target_selection_reason, None,
            "missing pool members have no invented exclusion reason"
        );
    }

    #[tokio::test]
    async fn current_lesson_not_found_is_distinct_from_storage_corruption() {
        struct TestDir(std::path::PathBuf);
        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TestDir(
            std::env::temp_dir().join(format!("memvault-bench-test-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("isolated-benchmark-test.db");
        let store = SqliteStore::new(&path).unwrap();
        assert!(
            current_lesson(&store, Some("absent"))
                .await
                .unwrap()
                .is_none()
        );
        let memory = store
            .save(Memory::new(
                MemoryType::Fact,
                "test row".into(),
                Priority::Reference,
                agent(),
            ))
            .await
            .unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // Unknown type strings intentionally default to Fact in the backend;
        // remove the isolated test table to exercise an actual read failure.
        conn.execute("DROP TABLE memories", []).unwrap();
        let error = current_lesson(&store, Some(&memory.id)).await.unwrap_err();
        assert!(
            !matches!(error, MemVaultError::NotFound(_)),
            "corruption is not deleted-memory evidence"
        );
    }

    #[tokio::test]
    async fn compatible_project_api_and_distinct_task_cohorts_report_actual_scope() {
        let store = store_with_failed_episode().await;
        let samples = collect_samples(store.as_ref(), 50).await.unwrap();
        let mut memory = store
            .get(samples[0].episode.lesson_memory_id.as_ref().unwrap())
            .await
            .unwrap();
        memory.namespace = "project:alpha".into();
        store.update(memory).await.unwrap();
        let router = MemoryRouter::new(store.clone());
        let repeated = vec![samples[0].clone(), samples[0].clone()];
        let report = run_bench_with_project(
            &router,
            store.as_ref(),
            &repeated,
            &BenchConfig::default(),
            None,
            Some("Alpha/"),
        )
        .await
        .unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.distinct_tasks, 1);
        assert_eq!(
            report
                .effective_config
                .as_ref()
                .unwrap()
                .resolved_namespace
                .as_deref(),
            Some("project:alpha")
        );
        assert!(
            report
                .effective_config
                .as_ref()
                .unwrap()
                .profile
                .api_key
                .is_none()
        );
        assert!(report.rows.iter().all(|r| r.injected));
        assert!(
            report.rows[0]
                .selection_trace
                .prefilter_ids
                .contains(report.rows[0].lesson_id.as_ref().unwrap())
        );
        let serialized = serde_json::to_string(&report).unwrap();
        let restored: BenchReport = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.rows[0].stages.len(), 4);
        assert_eq!(restored.protocol_version, 2);
        let legacy: BenchReport = serde_json::from_value(serde_json::json!({
            "total":1,"with_lesson_memory":0,"retrieved":0,"injected":0,"judged":0,"pass_without":0,"pass_with":0,
            "avg_injected_chars":0.0,"avg_tokens_without":0.0,"avg_tokens_with":0.0,"token_savings_pct":null,
            "rows":[{"task":"old","task_type":null,"status":"failure","lesson_id":null,"lesson":null,"retrieved":false,"injected":false,"injected_chars":0,"pass_without":null,"pass_with":null,"tokens_without":null,"tokens_with":null}]
        })).unwrap();
        assert_eq!(legacy.protocol_version, 1);
        assert!(legacy.effective_config.is_none());
        assert!(legacy.rows[0].stages.is_empty());
    }
}
