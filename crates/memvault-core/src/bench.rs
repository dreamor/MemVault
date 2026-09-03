//! 任务级记忆效果评测基准 —— Feature B（见 `docs/PAPER-INSPIRATIONS.md`）。
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

use crate::error::Result;
use crate::llm_extractor::LlmExtractor;
use crate::models::{EpisodeFilter, EpisodeRecord, Memory, OutcomeStatus, SearchQuery};
use crate::router::MemoryRouter;
use crate::storage::MemoryStore;

/// 评测配置。
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// 用哪个 agent profile 跑 `session_start`（决定注入规则）。
    pub agent_id: String,
    /// 检索层 top-k。
    pub top_k: usize,
    /// 每个样本注入内容的最大字符数（裁判层喂给 LLM 的上限，防止打爆上下文）。
    pub max_injected_chars: usize,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            agent_id: "default".to_string(),
            top_k: 10,
            max_injected_chars: 4000,
        }
    }
}

/// 一个评测样本：一次有教训的历史任务。
#[derive(Debug, Clone)]
pub struct BenchSample {
    pub episode: EpisodeRecord,
    /// 教训对应的记忆实体（可能已被删除/归档 → None）。
    pub lesson_memory: Option<Memory>,
}

/// 单个样本的评测结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleResult {
    pub task: String,
    pub task_type: Option<String>,
    pub status: OutcomeStatus,
    pub lesson_id: Option<String>,
    pub lesson: Option<String>,
    /// 检索层：教训是否进入 top-k。
    pub retrieved: bool,
    /// 注入层：完整管线后教训是否被注入。
    pub injected: bool,
    /// 本次注入的估算成本（字符数，约 4 字符/token）。
    pub injected_chars: usize,
    /// 裁判层：无记忆方案是否避开已知失败原因（None = 未评测）。
    pub pass_without: Option<bool>,
    /// 裁判层：带注入记忆方案是否避开已知失败原因。
    pub pass_with: Option<bool>,
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
    pub rows: Vec<SampleResult>,
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
            limit: max_episodes,
        })
        .await?;

    let mut samples = Vec::new();
    for episode in episodes {
        if episode.lesson.is_none() {
            continue;
        }
        // 记忆已删除/归档时返回 None：样本保留，检索层会体现缺口。
        let lesson_memory = match episode.lesson_memory_id {
            Some(ref id) => store.get(id).await.ok(),
            None => None,
        };
        samples.push(BenchSample {
            episode,
            lesson_memory,
        });
    }
    Ok(samples)
}

/// 跑评测。`judge` 为 None 时只跑检索层 + 注入层。
pub async fn run_bench(
    router: &MemoryRouter,
    store: &dyn MemoryStore,
    samples: &[BenchSample],
    config: &BenchConfig,
    judge: Option<&Arc<dyn LlmExtractor>>,
) -> Result<BenchReport> {
    let mut rows = Vec::with_capacity(samples.len());

    for sample in samples {
        let lesson_id = sample.lesson_memory.as_ref().map(|m| m.id.clone());

        // —— 检索层：任务文本直接当查询。
        let retrieved = if lesson_id.is_some() {
            let outcome = store
                .search(SearchQuery {
                    query: sample.episode.task.clone(),
                    top_k: config.top_k,
                    ..SearchQuery::new(sample.episode.task.clone())
                })
                .await?;
            outcome
                .results
                .iter()
                .any(|r| Some(&r.memory.id) == lesson_id.as_ref())
        } else {
            false
        };

        // —— 注入层：完整 session_start 管线（意图分析、预算、规则过滤）。
        let injection = router
            .session_start(&config.agent_id, Some(&sample.episode.task), None)
            .await?;
        let injected = injection
            .results
            .iter()
            .any(|r| Some(&r.memory.id) == lesson_id.as_ref());
        let injected_chars: usize = injection
            .results
            .iter()
            .map(|r| r.memory.content.len() + r.memory.instruction.as_deref().map_or(0, str::len))
            .sum::<usize>()
            .min(config.max_injected_chars);

        // —— 裁判层（可选）：无记忆 vs 带注入记忆，对照已知失败原因判定。
        let (pass_without, pass_with) = match judge {
            Some(j) => {
                judge_sample(
                    j.as_ref(),
                    sample,
                    &injection_text(&injection.results, config.max_injected_chars),
                )
                .await
            }
            None => (None, None),
        };

        rows.push(SampleResult {
            task: sample.episode.task.clone(),
            task_type: sample.episode.task_type.clone(),
            status: sample.episode.status,
            lesson_id,
            lesson: sample.episode.lesson.clone(),
            retrieved,
            injected,
            injected_chars,
            pass_without,
            pass_with,
        });
    }

    let with_lesson_memory = rows.iter().filter(|r| r.lesson_id.is_some()).count();
    let retrieved = rows.iter().filter(|r| r.retrieved).count();
    let injected = rows.iter().filter(|r| r.injected).count();
    let judged = rows.iter().filter(|r| r.pass_with.is_some()).count();
    let pass_without = rows.iter().filter(|r| r.pass_without == Some(true)).count();
    let pass_with = rows.iter().filter(|r| r.pass_with == Some(true)).count();
    let avg_injected_chars = if rows.is_empty() {
        0.0
    } else {
        rows.iter().map(|r| r.injected_chars).sum::<usize>() as f64 / rows.len() as f64
    };

    Ok(BenchReport {
        total: rows.len(),
        with_lesson_memory,
        retrieved,
        injected,
        judged,
        pass_without,
        pass_with,
        avg_injected_chars,
        rows,
    })
}

/// 把注入结果渲染成纯文本（裁判层的"带记忆"上下文）。
fn injection_text(results: &[crate::models::SearchResult], max_chars: usize) -> String {
    let mut text = String::new();
    for r in results {
        if let Some(ref inst) = r.memory.instruction {
            text.push_str("- [MUST] ");
            text.push_str(inst);
        } else {
            text.push_str("- ");
            text.push_str(&r.memory.content);
        }
        text.push('\n');
        if text.len() >= max_chars {
            text.truncate(max_chars);
            break;
        }
    }
    text
}

/// 裁判层：对一个样本跑"无记忆 / 带记忆"双方案 + 失败原因对照判定。
///
/// 任何 LLM 环节失败都降级为 (None, None)——评测是 best-effort，
/// 不能因为裁判不可用就让整个基准失败。
async fn judge_sample(
    judge: &dyn LlmExtractor,
    sample: &BenchSample,
    injected: &str,
) -> (Option<bool>, Option<bool>) {
    // 判定需要一个已知的坑：失败原因或教训至少有一个。
    if sample.episode.cause.is_none() && sample.episode.lesson.is_none() {
        return (None, None);
    }

    let task = &sample.episode.task;

    // 任何一步失败/不支持都整体降级为未评测，而不是让基准报错。
    let plan_without = match plan_task(judge, task, None).await {
        Ok(Some(p)) => p,
        Ok(None) => return (None, None),
        Err(e) => {
            warn!("bench judge: planning failed: {}", e);
            return (None, None);
        }
    };
    let plan_with = match plan_task(judge, task, Some(injected)).await {
        Ok(Some(p)) => p,
        Ok(None) => return (None, None),
        Err(e) => {
            warn!("bench judge: planning failed: {}", e);
            return (None, None);
        }
    };

    let pass_without = match judge_avoids_pitfall(judge, task, &sample.episode, &plan_without).await
    {
        Ok(v) => v,
        Err(e) => {
            warn!("bench judge: verdict failed: {}", e);
            return (None, None);
        }
    };
    let pass_with = match judge_avoids_pitfall(judge, task, &sample.episode, &plan_with).await {
        Ok(v) => v,
        Err(e) => {
            warn!("bench judge: verdict failed: {}", e);
            return (None, None);
        }
    };

    (pass_without, pass_with)
}

const PLAN_SYSTEM: &str = "You are an AI agent planning how to execute a task. \
Reply with JSON {\"plan\": string} listing concrete steps. Be brief and specific.";

const JUDGE_SYSTEM: &str = "You are a strict reviewer. A previous attempt at this exact task \
did not fully succeed. You are given the known failure attribution and/or the lesson learned, \
plus a newly proposed plan. Decide whether the plan AVOIDS the known pitfall \
(actually prevents it, not merely mentions it). Reply with JSON {\"pass\": boolean, \"reason\": string}.";

/// 生成任务方案；返回 JSON 里的 `plan` 文本。
async fn plan_task(
    judge: &dyn LlmExtractor,
    task: &str,
    injected: Option<&str>,
) -> Result<Option<String>> {
    let user = match injected {
        Some(mem) if !mem.trim().is_empty() => format!(
            "Relevant memories from past experience:\n{}\n\nTask: {}",
            mem, task
        ),
        _ => format!("Task: {}", task),
    };
    let Some(raw) = judge.json_chat(PLAN_SYSTEM, &user).await? else {
        return Ok(None);
    };
    Ok(parse_field(&raw, "plan"))
}

/// 裁判判定：方案是否避开已知坑。
async fn judge_avoids_pitfall(
    judge: &dyn LlmExtractor,
    task: &str,
    episode: &EpisodeRecord,
    plan: &str,
) -> Result<Option<bool>> {
    let Some(plan) = non_empty(Some(plan)) else {
        return Ok(None);
    };
    let mut known = String::new();
    if let Some(ref cause) = episode.cause {
        known.push_str(&format!("Known failure cause: {}\n", cause));
    }
    if let Some(ref lesson) = episode.lesson {
        known.push_str(&format!("Lesson learned: {}\n", lesson));
    }
    let user = format!("Task: {}\n{}Proposed plan: {}", task, known, plan);
    let Some(raw) = judge.json_chat(JUDGE_SYSTEM, &user).await? else {
        return Ok(None);
    };
    Ok(parse_bool(&raw, "pass"))
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.trim().is_empty())
}

/// 宽容解析 JSON 字符串字段（允许代码栅栏包裹）。
fn parse_field(raw: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(strip_fences(raw)).ok()?;
    value.get(field)?.as_str().map(str::to_string)
}

fn parse_bool(raw: &str, field: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(strip_fences(raw)).ok()?;
    value.get(field)?.as_bool()
}

fn strip_fences(raw: &str) -> &str {
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
        };
        let recorded = record_outcome(store.as_ref(), input.clone(), None)
            .await
            .expect("record outcome");
        reflect_and_store(
            store.as_ref(),
            &recorded.memory.id,
            input.into(),
            None,
            None,
        )
        .await
        .expect("reflect lesson");
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
        async fn json_chat(&self, system: &str, _user: &str) -> Result<Option<String>> {
            if system.starts_with("You are an AI agent planning") {
                Ok(Some(
                    "{\"plan\": \"check disk space first, then deploy\"}".to_string(),
                ))
            } else {
                Ok(Some(
                    "{\"pass\": true, \"reason\": \"avoids the pitfall\"}".to_string(),
                ))
            }
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
        assert_eq!(parse_bool("{\"pass\": true}", "pass"), Some(true));
        assert_eq!(parse_bool("not json", "pass"), None);
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
        }];
        let text = injection_text(&results, 50);
        assert!(text.len() <= 50);
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
        let results = vec![crate::models::SearchResult {
            memory,
            score: 1.0,
            hit_sources: vec![],
        }];
        let text = injection_text(&results, 1000);
        assert!(text.contains("[MUST] Always check disk space"));
    }
}
