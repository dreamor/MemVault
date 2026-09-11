//! Host conversation transcript → plain turn text.
//!
//! Plugin hooks forward a host-written transcript file (today: Claude Code's
//! JSONL and Codex's `rollout.jsonl`). Parsing lives in Rust instead of the
//! hook shell scripts so every
//! host shares one tolerant reader and the scripts stay dependency-free.
//!
//! The parser is deliberately forgiving: transcript schemas drift between
//! host versions, and a hook must never fail the host session because of one
//! unparseable line. Worst case we extract from noise, bounded by the byte
//! cap (tail is kept — the most recent turns carry the strongest signals).

/// Upper bound applied to the extracted text before it reaches the extractor.
pub const DEFAULT_MAX_BYTES: usize = 200_000;

/// The speaker side of one recognized transcript turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
}

/// One recognized conversation turn, keeping enough structure for
/// incremental (watermark-based) ingestion.
///
/// `seq` is the 0-based line index of the entry in the raw file. Line
/// indexes are the only order that survives an append-only log: later runs
/// of a session keep appending, so "process from seq N on" is well-defined
/// even when earlier lines were malformed or non-turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceTurn {
    /// 0-based index of the JSONL line that produced this turn.
    pub seq: usize,
    pub role: TurnRole,
    /// The turn's prose blocks, in order. Sidechain and tool blocks are
    /// already excluded.
    pub segments: Vec<String>,
}

/// Result of [`parse_turns`]: the recognized turns plus whether *any* entry
/// was recognized (mirrors the fallback contract of
/// [`transcript_to_text`] — unknown content is passed through, not dropped).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnParse {
    pub turns: Vec<TraceTurn>,
    pub recognized: bool,
}

/// Parse raw transcript file contents into structured turns.
///
/// Same tolerant contract as [`transcript_to_text`]: one unparseable line
/// must not fail the parse. Non-turn lines (sidechain, summary headers,
/// tool-only assistant turns) are skipped without breaking `seq` — the
/// index stays the *line* index so later appends remain addressable.
pub fn parse_turns(raw: &str) -> TurnParse {
    let mut out = TurnParse::default();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        // Two host shapes are recognized: Claude Code's top-level
        // user/assistant rows and Codex's `response_item` rows wrapping a
        // message payload. Everything else — sidechain entries, Codex
        // `developer` rows (injected app/environment context), tool-call and
        // compaction rows — is skipped without breaking `seq`.
        let claude = is_claude_turn(&value);
        if !claude && !is_codex_turn(&value) {
            continue;
        }
        out.recognized = true;
        let speaker = if claude {
            value["type"].as_str()
        } else {
            value["payload"]["role"].as_str()
        };
        let role = match speaker {
            Some("user") => TurnRole::User,
            Some("assistant") => TurnRole::Assistant,
            // The recognized shapes already restrict the speaker; treat
            // anything else as unknown rather than inventing a third role.
            _ => continue,
        };
        let content = if claude {
            &value["message"]["content"]
        } else {
            &value["payload"]["content"]
        };
        let segments: Vec<String> = content_texts(content);
        if segments.is_empty() {
            continue; // a turn entry with no prose contributes nothing
        }
        out.turns.push(TraceTurn {
            seq: idx,
            role,
            segments,
        });
    }
    out
}

/// Convert raw transcript file contents into `role: text` lines.
///
/// - Recognized JSONL entries (`user`/`assistant` with a `message` payload)
///   contribute their text blocks; sidechain (subagent) entries and unknown
///   lines are skipped.
/// - When nothing is recognized, the raw contents are passed through as-is so
///   callers can pipe plain-text session logs and still get extraction.
pub fn transcript_to_text(raw: &str) -> String {
    transcript_to_text_with_limit(raw, DEFAULT_MAX_BYTES)
}

/// [`transcript_to_text`] with an explicit output size cap.
pub fn transcript_to_text_with_limit(raw: &str, max_bytes: usize) -> String {
    let parsed = parse_turns(raw);
    if !parsed.recognized {
        return truncate_tail(raw, max_bytes);
    }
    let mut lines: Vec<String> = Vec::new();
    for turn in &parsed.turns {
        let role = match turn.role {
            TurnRole::User => "user",
            TurnRole::Assistant => "assistant",
        };
        for segment in &turn.segments {
            lines.push(format!("{role}: {segment}"));
        }
    }
    truncate_tail(&lines.join("\n"), max_bytes)
}

/// A Claude Code conversation turn: a top-level user/assistant entry carrying
/// a `message` payload, excluding sidechain (subagent) explorations.
fn is_claude_turn(value: &serde_json::Value) -> bool {
    if value.get("message").is_none() {
        return false;
    }
    matches!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("user") | Some("assistant")
    ) && value
        .get("isSidechain")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
}

/// A Codex rollout conversation turn: a `response_item` row wrapping a
/// message payload. Only user/assistant rows are prose — `developer` rows
/// carry injected app/environment context, and `function_call`,
/// `function_call_output`, `compacted` and `turn_context` rows are not
/// conversation turns.
fn is_codex_turn(value: &serde_json::Value) -> bool {
    value.get("type").and_then(serde_json::Value::as_str) == Some("response_item")
        && value["payload"]["type"].as_str() == Some("message")
        && matches!(
            value["payload"]["role"].as_str(),
            Some("user") | Some("assistant")
        )
}

/// Pull the text viewport out of a message `content` value, which may be a
/// plain string or an array of typed blocks. Claude Code puts prose in
/// `text` blocks; Codex rollout messages use `input_text`/`output_text` —
/// anything else in a block array (tool_use, reasoning, ...) is not prose.
fn content_texts(content: &serde_json::Value) -> Vec<String> {
    match content {
        serde_json::Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|block| {
                matches!(
                    block.get("type").and_then(serde_json::Value::as_str),
                    Some("text") | Some("input_text") | Some("output_text")
                )
            })
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Keep the tail of the input so a cap does not silently drop the most recent
/// turns. A `…` prefix marks the cut; the start index is nudged forward to a
/// UTF-8 char boundary so the kept text stays valid.
fn truncate_tail(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    let mut start = input.len() - max_bytes;
    while start < input.len() && !input.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &input[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_user_and_assistant_text_from_jsonl() {
        let raw = concat!(
            r#"{"type":"user","message":{"role":"user","content":"I always prefer dark mode"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Noted. "},{"type":"tool_use","name":"Read","input":{}}]}}"#,
            "\n",
        );
        let text = transcript_to_text(raw);
        assert!(text.contains("user: I always prefer dark mode"));
        assert!(text.contains("assistant: Noted."));
        // Tool blocks are not prose — they must not leak into the payload.
        assert!(!text.contains("tool_use"));
    }

    #[test]
    fn skips_sidechain_and_unknown_entries() {
        let raw = concat!(
            r#"{"type":"user","isSidechain":true,"message":{"content":"subagent noise"}}"#,
            "\n",
            r#"{"type":"summary","summary":"unrelated heading"}"#,
            "\n",
            r#"{"type":"user","message":{"content":"real signal"}}"#,
            "\n",
            "not json at all\n",
        );
        let text = transcript_to_text(raw);
        assert!(!text.contains("subagent noise"));
        assert!(!text.contains("unrelated heading"));
        assert!(text.contains("user: real signal"));
    }

    #[test]
    fn falls_back_to_raw_text_when_nothing_is_recognized() {
        let raw = "plain session log line one\nplain session log line two\n";
        assert_eq!(transcript_to_text(raw), raw);
    }

    #[test]
    fn tail_truncation_prefers_recent_turns_and_stays_utf8_safe() {
        let turn = r#"{"type":"user","message":{"content":"keep me"}}"#;
        let mut raw =
            String::from("{\"type\":\"user\",\"message\":{\"content\":\"老记忆被丢弃\"}}\n");
        for _ in 0..50 {
            raw.push_str(turn);
            raw.push('\n');
        }
        let text = transcript_to_text_with_limit(&raw, 200);
        assert!(text.starts_with('…'));
        assert!(text.contains("keep me"));
        // A valid String implies no char boundary was violated mid-cut.
        assert!(!text.contains("老记忆"));
    }

    #[test]
    fn empty_and_degenerate_inputs() {
        assert_eq!(transcript_to_text(""), "");
        assert_eq!(transcript_to_text_with_limit("hello", 0), "");
        assert_eq!(transcript_to_text_with_limit("hello", 10), "hello");
    }

    #[test]
    fn parse_turns_keeps_line_indexes_across_skipped_lines() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"first"}}"#,
            "\n",
            "not json at all\n",
            r#"{"type":"assistant","message":{"content":"second"}}"#,
            "\n",
            r#"{"type":"user","isSidechain":true,"message":{"content":"skip"}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert!(parsed.recognized);
        assert_eq!(parsed.turns.len(), 2);
        // seq is the LINE index, so the non-JSON line between must not
        // renumber the assistant turn — watermarks stay stable on append.
        assert_eq!(parsed.turns[0].seq, 0);
        assert_eq!(parsed.turns[0].role, TurnRole::User);
        assert_eq!(parsed.turns[1].seq, 2);
        assert_eq!(parsed.turns[1].role, TurnRole::Assistant);
    }

    #[test]
    fn parse_turns_merges_multi_block_text_per_turn() {
        let raw = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a"},{"type":"tool_use","name":"Read","input":{}},{"type":"text","text":"b"}]}}"#;
        let parsed = parse_turns(raw);
        assert_eq!(parsed.turns.len(), 1);
        // tool blocks are excluded, prose segments kept in order.
        assert_eq!(
            parsed.turns[0].segments,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn parse_turns_unrecognized_marks_not_recognized() {
        let parsed = parse_turns("plain log\nwith no jsonl turns\n");
        assert!(!parsed.recognized);
        assert!(parsed.turns.is_empty());
    }

    #[test]
    fn parses_codex_rollout_message_rows() {
        // Rows faithful to a real Codex rollout.jsonl: injected developer
        // context, tool calls, compaction markers and UI event duplicates
        // must all stay out of the extraction payload.
        let raw = concat!(
            r#"{"timestamp":"2026-09-10T11:34:25.229Z","ordinal":1,"type":"session_meta","payload":{"id":"rollout-1"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context> desktop chrome"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"记住我们用 Rust,不用 Go"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"已记录。"}]}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"UI 通知里的重复文本"}}"#,
            "\n",
            r#"{"type":"turn_context","cwd":"/tmp","model":"gpt-5.2"}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert!(parsed.recognized);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].role, TurnRole::User);
        assert_eq!(parsed.turns[0].seq, 2);
        assert_eq!(parsed.turns[1].role, TurnRole::Assistant);
        let text = transcript_to_text(raw);
        assert!(text.contains("user: 记住我们用 Rust,不用 Go"));
        assert!(text.contains("assistant: 已记录。"));
        // Injected developer context and tool/UI rows never leak in.
        assert!(!text.contains("app-context"));
        assert!(!text.contains("shell"));
        assert!(!text.contains("UI 通知里的重复文本"));
    }

    #[test]
    fn codex_rollout_seq_stays_stable_on_append() {
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","output":"noise"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"second"}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(parsed.turns[0].seq, 0);
        assert_eq!(parsed.turns[1].seq, 2);
    }
}
