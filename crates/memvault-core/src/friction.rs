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
pub(crate) const REJECTION_PHRASES: &[&str] = &[
    "doesn't want to proceed",
    "don't want to proceed",
    "tool call was rejected",
    "tool use was rejected",
    "user rejected this",
    "user denied",
];

/// Codex refusal wording inside a host-level failure line. Observed as
/// `exec_command failed: CreateProcess { message: "Rejected(…)" }` when the
/// host declines to spawn the command; wording can drift across Codex
/// versions, and a miss only costs one signal, never a false positive.
pub(crate) const CODEX_REFUSAL_PHRASES: &[&str] = &["rejected("];

/// Heuristic correction/interruption markers in a human turn that is not the
/// session's opening prompt. Precision over recall by design — no semantic
/// judgment, no LLM call, so the Stop hook keeps its fast, dependency-free
/// exit path.
pub(crate) const CORRECTION_KEYWORDS: &[&str] = &[
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

/// Score a raw host JSONL transcript for friction.
///
/// Two host shapes are recognized, the same pair
/// [`crate::transcript::parse_turns`] reads: Claude Code's top-level
/// `user`/`assistant` rows, and Codex's `response_item` rows
/// (`function_call`, `function_call_output`, and user `message`). A Codex
/// rollout therefore scores on the same three signals as a Claude Code
/// transcript — retries, refusals, mid-session corrections — instead of
/// silently scoring zero and leaving the extract/outcome gates permanently
/// closed on that host.
///
/// A correction is only recognized as feedback on something the agent has
/// already produced. Claude Code rows are counted ordinally (its JSONL has one
/// row per human turn and no host-injected `user` rows), whereas a Codex
/// `user` row counts only after an assistant turn has come first — Codex
/// prepends its own `role: "user"` context and can split one human turn across
/// rows, so an ordinal count would let that injected context consume the
/// opening-prompt exemption.
///
/// Tolerant by the same contract as [`crate::transcript::parse_turns`]:
/// unparseable or unrecognized lines are skipped, never fail the scan.
/// Only the tail (`MAX_SCORED_LINES`) is scanned for very long transcripts.
pub fn score(raw: &str) -> FrictionScore {
    // tool_use_id / call_id -> tool name, so a later tool result can be
    // attributed back to the tool that produced it even when calls
    // interleave.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    // tool name -> consecutive is_error streak.
    let mut streaks: HashMap<String, u32> = HashMap::new();
    // Claude Code turns are counted ordinally — its JSONL carries one row per
    // human turn and never a host-injected `user` row. Codex instead tracks
    // whether the agent has produced a turn yet: a Codex rollout prepends
    // host-generated `role: "user"` context (the AGENTS.md instructions block,
    // `<environment_context>`, response annotations) and can split one human
    // turn across rows, so an ordinal count would consume the opening-prompt
    // exemption and score the task statement itself as a correction.
    let mut human_turns_seen: u32 = 0;
    let mut codex_assistant_seen = false;
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
        // Codex rows are matched first: a `response_item` row nests its
        // speaker in `payload`, so the Claude-shaped checks below could never
        // see it and every Codex session would score a flat zero.
        if let Some(payload) = codex_payload(&value) {
            match payload.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    codex_assistant_seen = true;
                    if let Some((call_id, name)) = codex_tool_call(payload) {
                        tool_names.insert(call_id, name);
                    }
                }
                Some("function_call_output") => {
                    codex_assistant_seen = true;
                    if let Some((call_id, is_error, text)) = codex_tool_result(payload) {
                        note_tool_result(
                            &tool_names,
                            &mut streaks,
                            &mut retry_hits,
                            &mut rejection_hits,
                            &call_id,
                            is_error,
                            &text,
                        );
                    }
                }
                Some("message") if payload.get("role").and_then(Value::as_str) == Some("user") => {
                    // Only feedback on something the agent already produced is
                    // a correction: leading host-injected context and the whole
                    // opening prompt (however many rows it spans) precede the
                    // first assistant turn and are therefore exempt.
                    if codex_assistant_seen
                        && contains_any(&codex_plain_text(&payload["content"]), CORRECTION_KEYWORDS)
                    {
                        correction_hits += 1;
                    }
                }
                Some("message")
                    if payload.get("role").and_then(Value::as_str) == Some("assistant") =>
                {
                    codex_assistant_seen = true;
                }
                _ => {}
            }
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
                        note_tool_result(
                            &tool_names,
                            &mut streaks,
                            &mut retry_hits,
                            &mut rejection_hits,
                            &tool_use_id,
                            is_error,
                            &text,
                        );
                    }
                }
                None => {
                    // A genuine human turn, not an auto-injected tool result.
                    // Ordinal counting is safe here: Claude Code's JSONL has
                    // one row per human turn and no host-injected `user` rows,
                    // so the second human turn really is the first correction.
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

/// Fold one tool result into the retry/rejection accumulators.
///
/// Shared by the Claude Code and Codex paths so both hosts score the same
/// way: a second consecutive failure from the same tool is a retry, a refusal
/// is a rejection, and any success resets that tool's error streak.
fn note_tool_result(
    tool_names: &HashMap<String, String>,
    streaks: &mut HashMap<String, u32>,
    retry_hits: &mut u32,
    rejection_hits: &mut u32,
    tool_id: &str,
    is_error: bool,
    text: &str,
) {
    let name = tool_names
        .get(tool_id)
        .cloned()
        .unwrap_or_else(|| tool_id.to_string());
    let streak = streaks.entry(name).or_insert(0);
    if is_error {
        *streak += 1;
        if *streak >= 2 {
            *retry_hits += 1;
        }
    } else {
        *streak = 0;
    }
    if is_rejection(text) {
        *rejection_hits += 1;
    }
}

/// The nested `payload` object of a Codex rollout `response_item` row, i.e.
/// `{"type":"response_item","payload":{…}}`.
///
/// `None` for every other line shape — Claude Code rows, and Codex's
/// `event_msg` / `session_meta` / `turn_context` / `world_state` metadata
/// rows — which makes this the "is this line Codex?" test.
pub(crate) fn codex_payload(value: &Value) -> Option<&Value> {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    value.get("payload").filter(|p| p.is_object())
}

/// Codex tool invocation → `(call_id, tool name)`: the Codex counterpart of
/// [`record_tool_uses`]. The id is what the later `function_call_output`
/// refers back to.
pub(crate) fn codex_tool_call(payload: &Value) -> Option<(String, String)> {
    if payload.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let name = payload.get("name").and_then(Value::as_str)?;
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id").and_then(Value::as_str))?;
    Some((call_id.to_string(), name.to_string()))
}

/// Codex tool result → `(call_id, is_error, output text)`: the Codex
/// counterpart of [`tool_results`].
///
/// A refusal is deliberately *not* an error here, mirroring the Claude path
/// (where a declined tool call comes back with `is_error` false and scores as
/// a rejection instead of feeding the retry streak).
pub(crate) fn codex_tool_result(payload: &Value) -> Option<(String, bool, String)> {
    if payload.get("type").and_then(Value::as_str) != Some("function_call_output") {
        return None;
    }
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("id").and_then(Value::as_str))?;
    let output = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_error = codex_output_is_error(output) && !codex_refusal(output);
    Some((call_id.to_string(), is_error, output.to_string()))
}

/// True when a Codex tool result reports failure.
///
/// Two shapes exist. `exec_command` returns the process exit code in the
/// host's own output header (`Process exited with code 1`), while a refused
/// spawn or sandbox denial never reaches a process at all and comes back as a
/// host-level `<tool> failed: …` line instead.
pub(crate) fn codex_output_is_error(output: &str) -> bool {
    match codex_exit_code(output) {
        Some(code) => code != 0,
        None => codex_host_failure(output),
    }
}

/// The process exit code from a Codex `exec_command` output header, when the
/// output has one.
///
/// Only the first lines are scanned: this marker belongs to the host's own
/// framing, so a command that merely *echoes* the phrase — reading another
/// session's log, running a test suite that prints it — must not be read as
/// its own failure.
fn codex_exit_code(output: &str) -> Option<i32> {
    const MARKER: &str = "Process exited with code ";
    let head = output.lines().take(3).collect::<Vec<_>>().join("\n");
    let at = head.find(MARKER)? + MARKER.len();
    head[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// A Codex host-level failure line: `<tool> failed: <detail>` as the output's
/// first line, e.g. `exec_command failed: CreateProcess { … }`.
fn codex_host_failure(output: &str) -> bool {
    host_failure_head(output.lines().next().unwrap_or_default())
}

/// Whether `head` is a `<tool> failed: <detail>` host-level failure line.
///
/// The guard requires the marker to sit behind a single bare token: prose that
/// merely contains " failed: " ("the build failed: …") is not a host failure,
/// and only the output's first line is ever consulted.
fn host_failure_head(head: &str) -> bool {
    const MARKER: &str = " failed:";
    let Some((tool, _)) = head.split_once(MARKER) else {
        return false;
    };
    !tool.is_empty() && tool.len() <= 32 && !tool.contains(char::is_whitespace)
}

/// True when a Codex tool result is a *refusal* rather than an ordinary
/// failure: the host declined to run the call, reported as
/// `exec_command failed: CreateProcess { message: "Rejected(…)" }`.
///
/// Both the host-failure prefix and the word are required on the output's
/// first line, so a command that merely prints the word "rejected(" — reading
/// a log file, `cat`ing source that mentions it — is not read as a refusal.
pub(crate) fn codex_refusal(output: &str) -> bool {
    let head = output.lines().next().unwrap_or_default();
    host_failure_head(head) && contains_any(head, CODEX_REFUSAL_PHRASES)
}

/// Flatten a Codex message payload's `content` — a plain string, or the
/// `input_text` / `output_text` blocks a rollout actually writes — into plain
/// text for keyword matching: the Codex counterpart of [`plain_text`].
pub(crate) fn codex_plain_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(Value::as_str),
                    Some("input_text") | Some("output_text") | Some("text")
                )
            })
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Record every `tool_use` block's `id -> name` mapping from an assistant
/// turn's content blocks.
pub(crate) fn record_tool_uses(content: &Value, tool_names: &mut HashMap<String, String>) {
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
pub(crate) fn tool_results(content: &Value) -> Option<Vec<(String, bool, String)>> {
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
pub(crate) fn plain_text(content: &Value) -> String {
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
         lesson: `memvault-cli outcome --status failure --task <task> --cause <cause>`.",
        friction.score
    )
}

/// Whether `text` carries a refusal marker from either host: Claude Code's
/// observed permission-prompt wording, or a Codex declined spawn.
pub(crate) fn is_rejection(text: &str) -> bool {
    contains_any(text, REJECTION_PHRASES) || codex_refusal(text)
}

/// Case-insensitive substring match against any of `needles`.
pub(crate) fn contains_any(haystack: &str, needles: &[&str]) -> bool {
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
    fn claude_tool_result_matching_codex_refusal_shape_also_scores_rejection() {
        // `is_rejection` is shared across both hosts by design (see its doc
        // comment above its definition): a Claude Code tool_result whose text
        // happens to be shaped like Codex's host-level refusal (`<tool>
        // failed: ...Rejected(...)`) scores as a rejection too, not just
        // through Codex's own `function_call_output` path. Pins the
        // cross-host overlap so a future change can't silently narrow
        // `is_rejection` back to Claude-only wording without a test noticing.
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"mytool failed: Rejected(by policy)"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "rejection"), 1);
    }

    #[test]
    fn claude_ordinary_failure_prose_is_not_misread_as_codex_refusal() {
        // Ordinary multi-word Claude Code tool failure text must not satisfy
        // `host_failure_head`'s single-bare-token guard, so sharing
        // `is_rejection` across hosts does not turn a plain failure into a
        // rejection just because the word "rejected(" shows up in prose.
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"the build failed: dependency rejected(incompatible version)"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "rejection"), 0);
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

    // --- Codex rollout shape (`response_item` rows) ---

    #[test]
    fn codex_smooth_session_scores_zero() {
        let raw = concat!(
            r#"{"type":"session_meta","payload":{"id":"rollout-1"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>desktop</app-context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a login page"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Chunk ID: 1\nWall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nok"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(result.score, 0);
        assert_eq!(signal(&result, "retry"), 0);
        assert_eq!(signal(&result, "rejection"), 0);
        assert_eq!(signal(&result, "correction"), 0);
    }

    #[test]
    fn codex_consecutive_command_failures_score_retry_friction() {
        // Two consecutive non-zero exits from the same tool name, reported in
        // the exec_command header rather than an `is_error` flag.
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Chunk ID: 1\nWall time: 0.1 seconds\nProcess exited with code 1\nOutput:\nfail"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c2","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"Chunk ID: 2\nWall time: 0.1 seconds\nProcess exited with code 1\nOutput:\nfail again"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "retry"), 1);
        assert!(result.score >= 1);
    }

    #[test]
    fn codex_declined_spawn_scores_rejection_but_not_retry() {
        // `exec_command failed: CreateProcess { message: "Rejected(…)" }` is
        // the host refusing to run the command — a rejection, mirroring how a
        // declined Claude Code tool call scores, and not a retry streak.
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"clean up the temp files"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"exec_command failed: CreateProcess { message: \"Rejected(`/bin/zsh -lc rm -f /tmp/x`)\" }"}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "rejection"), 1);
        assert_eq!(signal(&result, "retry"), 0);
    }

    #[test]
    fn codex_correction_in_later_user_turn_scores_correction_friction() {
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a login page"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"等一下，不对，不是这样，改回去"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 1);
    }

    #[test]
    fn codex_developer_rows_are_not_human_turns() {
        // Injected app/environment context can arrive as a `developer` row: it
        // is not a human turn and must not consume the opening-prompt
        // exemption. (Context injected as a `role: "user"` row is covered by
        // `codex_injected_user_context_does_not_consume_the_opening_prompt_exemption`.)
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>不对，等一下</app-context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a login page"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 0);
    }

    #[test]
    fn codex_injected_user_context_does_not_consume_the_opening_prompt_exemption() {
        // Real Codex rollouts prepend host-generated `role: "user"` context —
        // the AGENTS.md instructions block and the `<environment_context>`
        // block — ahead of the human's first message. Counting those rows as
        // human turns (or counting ordinally at all) would push the opening
        // prompt past the "first turn" exemption, so a task statement that
        // merely contains a word like "revert" would score as a correction and
        // open the extract gate on an otherwise friction-free session.
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>desktop</app-context>"}]}}"#,
            "\n",
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nBuild, Test, and Development Commands\n</INSTRUCTIONS>"}]}}"##,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/repo</cwd>\n</environment_context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"revert the last commit, then no-op if it fails"}]}}"#,
            "\n",
        );
        let result = score(raw);
        assert_eq!(signal(&result, "correction"), 0);
        assert_eq!(result.score, 0);
    }

    #[test]
    fn codex_correction_still_scores_after_injected_context() {
        // The assistant-turn gate must not hide a real correction: once the
        // agent has responded, a later user turn with a correction keyword is
        // still counted even with injected context up front.
        let raw = concat!(
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /repo\n<INSTRUCTIONS>\n</INSTRUCTIONS>"}]}}"##,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a login page"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"wait, that's not right"}]}}"#,
            "\n",
        );
        assert_eq!(signal(&score(raw), "correction"), 1);
    }

    #[test]
    fn codex_exit_code_echoed_by_the_command_is_not_a_failure() {
        // The marker belongs to the host's own header. A command that prints
        // it — greping another session's log — succeeded and must not be read
        // as failing here.
        let ok = "Chunk ID: 1\nWall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nProcess exited with code 1";
        assert!(!codex_output_is_error(ok));
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Chunk ID: 1\nWall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nProcess exited with code 1"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c2","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"Chunk ID: 2\nWall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nProcess exited with code 1"}}"#,
            "\n",
        );
        assert_eq!(signal(&score(raw), "retry"), 0);
    }

    #[test]
    fn codex_refusal_needs_the_host_failure_prefix_and_the_first_line() {
        // Echoing the word must not count: the refusal marker is only
        // meaningful on a host-level `<tool> failed:` first line.
        let echoed = "Chunk ID: 1\nWall time: 0.1 seconds\nProcess exited with code 0\nOutput:\npub(crate) const CODEX_REFUSAL_PHRASES: &[&str] = &[\"rejected(\"];";
        assert!(!codex_refusal(echoed));
        assert!(!is_rejection(echoed));
        assert!(codex_output_is_error(
            "exec_command failed: CreateProcess { message: \"spawn ENOENT\" }"
        ));
        assert!(host_failure_head(
            "exec_command failed: CreateProcess { … }"
        ));
        // A bare `failed:` inside prose is not a host failure.
        assert!(!host_failure_head("the build failed: boom"));
        assert!(!host_failure_head("Output:"));
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
        assert!(note.contains("memvault-cli outcome --status failure"));
    }

    #[test]
    fn codex_shape_helpers_tolerate_missing_or_odd_fields() {
        // Schema drift is expected: `id` is the documented fallback when
        // `call_id` is absent, and non-Codex values must read as "not this
        // shape" rather than panicking or matching by accident.
        let call = serde_json::json!({"type":"function_call","name":"exec","id":"x1"});
        assert_eq!(
            codex_tool_call(&call),
            Some(("x1".to_string(), "exec".to_string()))
        );
        assert_eq!(
            codex_tool_call(&serde_json::json!({"type":"message"})),
            None
        );
        assert_eq!(codex_payload(&serde_json::json!({"type":"user"})), None);
        assert_eq!(
            codex_payload(&serde_json::json!({"type":"response_item"})),
            None
        );

        let result = serde_json::json!({"type":"function_call_output","id":"x1","output":"ok"});
        assert_eq!(
            codex_tool_result(&result),
            Some(("x1".to_string(), false, "ok".to_string()))
        );
        assert_eq!(
            codex_tool_result(&serde_json::json!({"type":"other"})),
            None
        );
        // No `output` at all is an empty result, not an error.
        let empty = serde_json::json!({"type":"function_call_output","call_id":"y","output":""});
        assert_eq!(
            codex_tool_result(&empty),
            Some(("y".to_string(), false, String::new()))
        );

        // `content` arrives as a bare string in some writers and as typed
        // blocks in the desktop app's rollouts.
        assert_eq!(codex_plain_text(&serde_json::json!("plain")), "plain");
        assert_eq!(
            codex_plain_text(&serde_json::json!([
                {"type":"input_text","text":"a"},
                {"type":"output_text","text":"b"}
            ])),
            "a b"
        );
        assert_eq!(codex_plain_text(&serde_json::json!({"not":"blocks"})), "");

        // No exit-code header at all is not an error, and a host failure line
        // without the refusal word is a failure but not a rejection.
        assert_eq!(codex_exit_code("no header here"), None);
        assert!(!codex_output_is_error(""));
        let spawn_error = "exec_command failed: CreateProcess { message: \"spawn ENOENT\" }";
        assert!(!codex_refusal(spawn_error));
        assert!(codex_output_is_error(spawn_error));
    }

    #[test]
    fn min_friction_threshold_defaults_when_unset() {
        // No env var set in the default test environment.
        if std::env::var("MEMVAULT_HOOK_EXTRACT_MIN_FRICTION").is_err() {
            assert_eq!(min_friction_threshold(), DEFAULT_MIN_FRICTION);
        }
    }
}
