//! Session friction scoring — a cheap, structural signal for "was this
//! session worth summarizing", inspired by TeamAI-cli's friction-gated
//! experience capture: retries, rejected tool calls, and mid-session
//! corrections mark a session as worth distilling; a long-but-smooth
//! session does not.
//!
//! Unlike [`crate::transcript`], which flattens a transcript to prose and
//! discards `tool_use`/`tool_result` blocks, friction scoring reads those
//! blocks directly — the signal lives in exactly what `transcript` throws
//! away. Both parsers stay deliberately tolerant: one bad line must not
//! break scoring, and an unrecognized shape just contributes nothing rather
//! than erroring.
//!
//! See `the (removed) FRICTION-GATED-EXTRACTION-PLAN.md design note, see git history` for the design writeup this
//! module implements (Phase 1).

use std::collections::HashMap;

use serde_json::Value;

/// Equal weight per signal for the first cut — no historical data yet to
/// justify weighting one signal over another. Revisit once inbox quality
/// data exists (see the (removed) FRICTION-GATED-EXTRACTION-PLAN.md design note, see git history §7/§12).
const WEIGHT_RETRY: u32 = 1;
const WEIGHT_REJECTION: u32 = 1;
const WEIGHT_CORRECTION: u32 = 1;

/// Below this many friction points, a hook-triggered extract is skipped.
/// Any single signal is enough to clear the default — a missed learning
/// opportunity costs more than one extra unreviewed inbox draft.
pub const DEFAULT_MIN_FRICTION: u32 = 1;

/// Bound how many JSONL lines are scanned so a pathological transcript
/// cannot blow past the Stop hook's timeout. Mirrors
/// [`crate::transcript::DEFAULT_MAX_BYTES`]'s "the tail carries the
/// strongest signal" reasoning, counted in lines instead of bytes — a
/// byte cap on raw JSONL risks truncating mid-object.
const MAX_SCORED_LINES: usize = 2000;

/// Phrases Claude Code has been observed to emit inside a `tool_result` when
/// the user declines a permission prompt. Not a documented contract —
/// wording can drift across host versions, so a miss here just means the
/// rejection signal silently contributes 0, never a false positive.
const REJECTION_PHRASES: &[&str] = &[
    "doesn't want to proceed",
    "don't want to proceed",
    "tool call was rejected",
    "tool use was rejected",
    "user rejected this",
    "user denied",
];

/// Heuristic correction/interruption markers in a human turn that is not the
/// session's opening prompt. Precision over recall by design — no semantic
/// judgment, no LLM call, so the Stop hook keeps its fast, dependency-free
/// exit path.
const CORRECTION_KEYWORDS: &[&str] = &[
    "no, ",
    "not what i",
    "that's wrong",
    "that's not right",
    "stop, ",
    "stop.",
    "wait, ",
    "actually, ",
    "revert",
    "undo that",
    "roll that back",
    "不对",
    "不是这样",
    "不是这个意思",
    "停一下",
    "等一下",
    "撤销",
    "改回去",
    "别这样",
];

/// One recognized friction signal and how many times it fired, in a stable
/// declared order (retry, rejection, correction) regardless of hit counts —
/// callers rely on this order for debug output.
pub type FrictionSignals = Vec<(&'static str, u32)>;

/// The result of scoring one transcript: a weighted total plus the raw
/// per-signal counts that produced it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrictionScore {
    pub score: u32,
    pub signals: FrictionSignals,
}

impl FrictionScore {
    /// Whether this score clears the given gate.
    pub fn meets(&self, threshold: u32) -> bool {
        self.score >= threshold
    }
}

/// Minimum friction score a hook-triggered extract must reach before it is
/// allowed to save anything. Reads `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION`;
/// `0` disables the gate outright (parity with the pre-gating behavior of
/// extracting on every Stop).
pub fn min_friction_threshold() -> u32 {
    std::env::var("MEMVAULT_HOOK_EXTRACT_MIN_FRICTION")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_MIN_FRICTION)
}

/// Score a raw Claude Code JSONL transcript for friction.
///
/// Tolerant by the same contract as [`crate::transcript::parse_turns`]:
/// unparseable or unrecognized lines are skipped, never fail the scan.
/// Only the tail (`MAX_SCORED_LINES`) is scanned for very long transcripts.
pub fn score(raw: &str) -> FrictionScore {
    // tool_use_id -> tool name, so a later tool_result can be attributed
    // back to the tool that produced it even when calls interleave.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    // tool name -> consecutive is_error streak.
    let mut streaks: HashMap<String, u32> = HashMap::new();
    let mut human_turns_seen: u32 = 0;
    let mut retry_hits = 0u32;
    let mut rejection_hits = 0u32;
    let mut correction_hits = 0u32;

    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.len().saturating_sub(MAX_SCORED_LINES);

    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(role) = value.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
            continue;
        };

        match role {
            "assistant" => record_tool_uses(content, &mut tool_names),
            "user" => match tool_results(content) {
                Some(results) => {
                    for (tool_use_id, is_error, text) in results {
                        let name = tool_names.get(&tool_use_id).cloned().unwrap_or(tool_use_id);
                        let streak = streaks.entry(name).or_insert(0);
                        if is_error {
                            *streak += 1;
                            if *streak >= 2 {
                                retry_hits += 1;
                            }
                        } else {
                            *streak = 0;
                        }
                        if contains_any(&text, REJECTION_PHRASES) {
                            rejection_hits += 1;
                        }
                    }
                }
                None => {
                    // A genuine human turn, not an auto-injected tool result.
                    human_turns_seen += 1;
                    if human_turns_seen > 1
                        && contains_any(&plain_text(content), CORRECTION_KEYWORDS)
                    {
                        correction_hits += 1;
                    }
                }
            },
            _ => {}
        }
    }

    let score = WEIGHT_RETRY * retry_hits
        + WEIGHT_REJECTION * rejection_hits
        + WEIGHT_CORRECTION * correction_hits;

    FrictionScore {
        score,
        signals: vec![
            ("retry", retry_hits),
            ("rejection", rejection_hits),
            ("correction", correction_hits),
        ],
    }
}

/// Record every `tool_use` block's `id -> name` mapping from an assistant
/// turn's content blocks.
fn record_tool_uses(content: &Value, tool_names: &mut HashMap<String, String>) {
    let Value::Array(blocks) = content else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        if let (Some(id), Some(name)) = (
            block.get("id").and_then(Value::as_str),
            block.get("name").and_then(Value::as_str),
        ) {
            tool_names.insert(id.to_string(), name.to_string());
        }
    }
}

/// If `content` carries one or more `tool_result` blocks, return each as
/// `(tool_use_id, is_error, flattened text)`. Returns `None` when the
/// content is not tool-result-shaped at all — the caller's signal that this
/// is a genuine human turn instead.
fn tool_results(content: &Value) -> Option<Vec<(String, bool, String)>> {
    let Value::Array(blocks) = content else {
        return None;
    };
    let mut out = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let tool_use_id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = block_text(block.get("content"));
        out.push((tool_use_id, is_error, text));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Flatten a `tool_result` block's `content` field (string or array of
/// typed blocks) into plain text for phrase matching.
fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Flatten a human turn's `content` field (string or array of `text`
/// blocks) into plain text for keyword matching.
fn plain_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Render a human-readable evidence note for a friction score that already
/// cleared the gate — meant for a reviewer glancing at an inbox draft, so
/// they don't have to re-read the transcript to see why it's there. Plain
/// text, not JSON: the only consumer is a person reading
/// `Memory.friction_evidence` in the dashboard/CLI, not another program.
pub fn evidence_note(friction: &FrictionScore) -> String {
    let detail = friction
        .signals
        .iter()
        .map(|(name, count)| format!("{name}×{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Friction score {} ({detail}). If this task actually failed, consider recording the \
         lesson: `memvault outcome --status failure --task <task> --cause <cause>`.",
        friction.score
    )
}

/// Case-insensitive substring match against any of `needles`.
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_lowercase();
    needles
        .iter()
        .any(|n| lower.contains(n.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(score: &FrictionScore, name: &str) -> u32 {
        score
            .signals
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    }

    #[test]
    fn smooth_session_scores_zero() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"add a login page"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Sure, here's the plan."}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"looks good, ship it"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(result.score, 0);
        assert_eq!(signal(&result, "retry"), 0);
        assert_eq!(signal(&result, "rejection"), 0);
        assert_eq!(signal(&result, "correction"), 0);
    }

    #[test]
    fn consecutive_tool_errors_score_retry_friction() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"run the tests"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"permission denied"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"permission denied"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "retry"), 1);
        assert!(result.score >= 1);
    }

    #[test]
    fn single_tool_error_does_not_score_retry_friction() {
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"transient network blip"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "retry"), 0);
    }

    #[test]
    fn success_resets_the_error_streak() {
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"nope"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":false,"content":"ok"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3","is_error":true,"content":"nope again"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(
            signal(&result, "retry"),
            0,
            "a success in between must reset the streak"
        );
    }

    #[test]
    fn rejection_phrase_in_tool_result_scores_rejection_friction() {
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"The user doesn't want to proceed with this tool use."}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "rejection"), 1);
    }

    #[test]
    fn correction_keyword_in_later_human_turn_scores_correction_friction() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"add a login page"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"wait, that's not right, use OAuth instead"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 1);
    }

    #[test]
    fn correction_keyword_in_opening_prompt_is_not_counted() {
        // The first human turn is the task itself, not a correction, even if
        // it happens to contain "wait" or "no".
        let raw = concat!(
            r#"{"type":"user","message":{"content":"wait for the build, then no-op if it fails"}}"#,
            "\n"
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 0);
    }

    #[test]
    fn sidechain_entries_are_ignored() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"add a login page"}}"#,
            "\n",
            r#"{"type":"user","isSidechain":true,"message":{"content":"wait, that's wrong"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 0);
    }

    #[test]
    fn garbage_lines_do_not_break_scoring() {
        let raw = concat!(
            "not json at all\n",
            r#"{"type":"user","message":{"content":"add a login page"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"wait, that's wrong"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 1);
    }

    #[test]
    fn evidence_note_renders_score_signals_and_outcome_suggestion() {
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"nope"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"nope again"}]}}"#,
            "\n",
        );
        let result = score(raw);
        let note = evidence_note(&result);
        assert!(note.contains("Friction score 1"));
        assert!(note.contains("retry×1"));
        assert!(note.contains("memvault outcome --status failure"));
    }

    #[test]
    fn min_friction_threshold_defaults_when_unset() {
        // No env var set in the default test environment.
        if std::env::var("MEMVAULT_HOOK_EXTRACT_MIN_FRICTION").is_err() {
            assert_eq!(min_friction_threshold(), DEFAULT_MIN_FRICTION);
        }
    }
}
