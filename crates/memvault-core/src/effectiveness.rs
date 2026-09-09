//! Automatic effectiveness judging — Tier-3 "online usage effectiveness"
//! from a memory-eval framework we're adapting: `report_compliance` only
//! answers "did the agent follow this MUST rule", entirely dependent on an
//! external caller reporting it. This closes the other half — "did an
//! injected memory actually help (or hurt) the task it was injected for" —
//! using [`crate::episode::record_outcome`] as the natural point where
//! task-level ground truth (success/failure/cause) becomes available, the
//! same trigger [`crate::reflection::reflect_and_store`] already uses for
//! lesson distillation.
//!
//! Best-effort by design, mirroring [`crate::bench`]'s judge: no LLM
//! configured means no judgment is fabricated (`judge_recent_injections`
//! returns `Ok(0)`) — unlike lesson reflection, there is no safe rule-based
//! fallback here. Guessing "useful" or "harmful" from keyword overlap alone
//! would be low-confidence noise passed off as a measurement; better to
//! leave the row unjudged (retried on the next matching outcome) than to
//! record a fabricated verdict.

use chrono::Utc;
use tracing::{debug, warn};

use crate::bench::parse_field;
use crate::compliance::{ComplianceStore, EffectivenessVerdict};
use crate::episode::OutcomeInput;
use crate::error::Result;
use crate::llm_extractor::LlmExtractor;
use crate::storage::MemoryStore;

/// How far back to look for injections to pair with an outcome. A local
/// single-process tool has no session-boundary signal the way a hosted
/// service would (the memory-eval framework this is adapted from uses a
/// 2-hour Redis TTL between injection and pairing) — the gap between
/// `session_start` and `record_outcome` for the same task is often longer
/// here, so the window is generous rather than tight.
pub const EFFECTIVENESS_MATCH_WINDOW_HOURS: i64 = 24;

/// Upper bound on how many pending injections one `record_outcome` call will
/// judge. Bounded rather than unbounded so one outcome cannot trigger an
/// unbounded burst of LLM calls; truncation is logged, never silent.
pub const EFFECTIVENESS_MAX_JUDGED_PER_OUTCOME: usize = 10;

const EFFECTIVENESS_JUDGE_SYSTEM: &str = "You are evaluating whether a piece of injected memory \
actually helped an AI agent complete a task. You are given the task, its outcome (and failure \
cause when known), and the memory content that was injected into the agent's context before it \
acted. Decide whether the memory was useful (genuinely helped), harmful (misled or distracted \
from the actual issue), neutral (present but made no real difference), or insufficient_context \
(you cannot tell from what's given). Reply with JSON \
{\"verdict\": \"useful\"|\"neutral\"|\"harmful\"|\"insufficient_context\", \"reason\": string}.";

/// Judge one (task outcome, injected memory) pair. `Ok(None)` on any
/// degradation (LLM error, unparseable JSON, unrecognized verdict string) —
/// the caller leaves the row unjudged rather than guessing.
pub async fn judge_one(
    judge: &dyn LlmExtractor,
    task: &str,
    status_str: &str,
    cause: Option<&str>,
    memory_content: &str,
) -> Result<Option<(EffectivenessVerdict, Option<String>)>> {
    let cause_line = cause
        .map(|c| format!("\nFailure cause: {c}"))
        .unwrap_or_default();
    let user = format!(
        "Task: {task}\nOutcome: {status_str}{cause_line}\nInjected memory: {memory_content}"
    );

    let Some(raw) = judge.json_chat(EFFECTIVENESS_JUDGE_SYSTEM, &user).await? else {
        return Ok(None);
    };
    let Some(verdict_str) = parse_field(&raw, "verdict") else {
        return Ok(None);
    };
    let Ok(verdict) = verdict_str.parse::<EffectivenessVerdict>() else {
        return Ok(None);
    };
    Ok(Some((verdict, parse_field(&raw, "reason"))))
}

/// Judge as many of `input.source_agent.id`'s pending injections as fit
/// under [`EFFECTIVENESS_MAX_JUDGED_PER_OUTCOME`], against this outcome.
/// Returns how many rows were successfully judged. A single row's failure
/// (deleted memory, judge error, store error) is logged and skipped — it
/// must never abort the rest of the batch or the outcome recording itself.
pub async fn judge_recent_injections(
    compliance: &ComplianceStore,
    store: &dyn MemoryStore,
    judge: Option<&dyn LlmExtractor>,
    input: &OutcomeInput,
) -> Result<usize> {
    let Some(judge) = judge else {
        return Ok(0);
    };

    let since = Utc::now() - chrono::Duration::hours(EFFECTIVENESS_MATCH_WINDOW_HOURS);
    let pending = compliance
        .pending_since(
            &input.source_agent.id,
            since,
            EFFECTIVENESS_MAX_JUDGED_PER_OUTCOME + 1,
        )
        .await?;

    let overflow = pending
        .len()
        .saturating_sub(EFFECTIVENESS_MAX_JUDGED_PER_OUTCOME);
    if overflow > 0 {
        debug!(
            overflow,
            agent_id = %input.source_agent.id,
            "more pending injections than this outcome will judge; the rest wait for the next one"
        );
    }

    let status_str = input.status.as_str();
    let mut judged = 0usize;
    for row in pending
        .into_iter()
        .take(EFFECTIVENESS_MAX_JUDGED_PER_OUTCOME)
    {
        let memory = match store.get(&row.memory_id).await {
            Ok(m) => m,
            Err(e) => {
                debug!(memory_id = %row.memory_id, error = %e, "skipping effectiveness judging; memory unavailable");
                continue;
            }
        };
        let content = memory.instruction.as_deref().unwrap_or(&memory.content);

        match judge_one(
            judge,
            &input.task,
            status_str,
            input.cause.as_deref(),
            content,
        )
        .await
        {
            Ok(Some((verdict, reason))) => {
                if let Err(e) = compliance
                    .record_effectiveness(&row.id, verdict, reason.as_deref())
                    .await
                {
                    warn!(error = %e, id = %row.id, "failed to record effectiveness verdict");
                    continue;
                }
                judged += 1;
            }
            Ok(None) => {} // left unjudged; retried against a future outcome
            Err(e) => warn!(error = %e, id = %row.id, "effectiveness judge call failed"),
        }
    }

    Ok(judged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{OutcomeStatus, Priority, SourceAgent};
    use crate::storage::sqlite::SqliteStore;
    use std::sync::Arc;

    struct FixedJudge(&'static str);

    #[async_trait::async_trait]
    impl LlmExtractor for FixedJudge {
        async fn extract(&self, _context: &str) -> Result<Vec<crate::extractor::ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(&self, _system: &str, _user: &str) -> Result<Option<String>> {
            Ok(Some(self.0.to_string()))
        }
    }

    struct ErroringJudge;

    #[async_trait::async_trait]
    impl LlmExtractor for ErroringJudge {
        async fn extract(&self, _context: &str) -> Result<Vec<crate::extractor::ExtractedMemory>> {
            Ok(Vec::new())
        }

        async fn json_chat(&self, _system: &str, _user: &str) -> Result<Option<String>> {
            Ok(None)
        }
    }

    fn outcome_input(agent_id: &str) -> OutcomeInput {
        OutcomeInput {
            task: "deploy the dashboard".to_string(),
            status: OutcomeStatus::Failure,
            cause: Some("missing env var".to_string()),
            task_type: Some("deploy".to_string()),
            skill_id: None,
            tags: vec![],
            namespace: "global".to_string(),
            source_agent: SourceAgent {
                id: agent_id.to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        }
    }

    #[tokio::test]
    async fn judge_one_parses_a_valid_verdict() {
        let judge = FixedJudge("{\"verdict\": \"useful\", \"reason\": \"pinpointed the cause\"}");
        let result = judge_one(
            &judge,
            "deploy",
            "failure",
            Some("missing env var"),
            "always set ENV_VAR before deploy",
        )
        .await
        .unwrap();
        let (verdict, reason) = result.expect("must parse a valid verdict");
        assert_eq!(verdict, EffectivenessVerdict::Useful);
        assert_eq!(reason.as_deref(), Some("pinpointed the cause"));
    }

    #[tokio::test]
    async fn judge_one_degrades_to_none_on_unparseable_json() {
        let judge = FixedJudge("not json at all");
        let result = judge_one(&judge, "deploy", "failure", None, "some memory")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn judge_one_degrades_to_none_on_unknown_verdict_string() {
        let judge = FixedJudge("{\"verdict\": \"very useful indeed\"}");
        let result = judge_one(&judge, "deploy", "failure", None, "some memory")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn judge_recent_injections_returns_zero_without_a_judge() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let compliance = ComplianceStore::new(":memory:").unwrap();
        let n =
            judge_recent_injections(&compliance, store.as_ref(), None, &outcome_input("claude"))
                .await
                .unwrap();
        assert_eq!(n, 0, "no LLM configured must never fabricate a judgment");
    }

    #[tokio::test]
    async fn judge_recent_injections_judges_pending_rows_for_the_same_agent() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = crate::models::Memory::new(
            crate::models::MemoryType::Preference,
            "always set ENV_VAR before deploy".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "claude".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        );
        let mem = store.save(mem).await.unwrap();

        let compliance = ComplianceStore::new(":memory:").unwrap();
        compliance
            .record_injection("inj_1", &mem.id, &Priority::Reference, "claude")
            .await
            .unwrap();

        let judge = FixedJudge("{\"verdict\": \"useful\", \"reason\": \"named the exact fix\"}");
        let n = judge_recent_injections(
            &compliance,
            store.as_ref(),
            Some(&judge),
            &outcome_input("claude"),
        )
        .await
        .unwrap();
        assert_eq!(n, 1);

        let summary = compliance
            .get_effectiveness_summary(Some("claude"), 10)
            .await
            .unwrap();
        assert_eq!(summary.useful, 1);
        assert_eq!(summary.unjudged, 0);
    }

    #[tokio::test]
    async fn judge_recent_injections_leaves_row_unjudged_when_judge_returns_none() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = crate::models::Memory::new(
            crate::models::MemoryType::Preference,
            "some unrelated memory".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "claude".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        );
        let mem = store.save(mem).await.unwrap();

        let compliance = ComplianceStore::new(":memory:").unwrap();
        compliance
            .record_injection("inj_1", &mem.id, &Priority::Reference, "claude")
            .await
            .unwrap();

        let judge = ErroringJudge;
        let n = judge_recent_injections(
            &compliance,
            store.as_ref(),
            Some(&judge),
            &outcome_input("claude"),
        )
        .await
        .unwrap();
        assert_eq!(n, 0);

        // Left unjudged, not fabricated — must still be pending for retry.
        let since = Utc::now() - chrono::Duration::hours(1);
        let pending = compliance.pending_since("claude", since, 10).await.unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn judge_recent_injections_ignores_a_different_agents_injections() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = crate::models::Memory::new(
            crate::models::MemoryType::Preference,
            "memory for someone else".to_string(),
            Priority::Reference,
            SourceAgent {
                id: "other-agent".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
        );
        let mem = store.save(mem).await.unwrap();

        let compliance = ComplianceStore::new(":memory:").unwrap();
        compliance
            .record_injection("inj_1", &mem.id, &Priority::Reference, "other-agent")
            .await
            .unwrap();

        let judge = FixedJudge("{\"verdict\": \"useful\"}");
        let n = judge_recent_injections(
            &compliance,
            store.as_ref(),
            Some(&judge),
            &outcome_input("claude"),
        )
        .await
        .unwrap();
        assert_eq!(n, 0, "must not judge another agent's injections");
    }
}
