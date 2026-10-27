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
        let mut segments: Vec<String> = content_texts(content);
        if segments.is_empty() {
            continue; // a turn entry with no prose contributes nothing
        }
        // Host-injected context arrives as `role: "user"` rows. Codex prepends
        // the AGENTS.md instructions block and `<environment_context>` ahead of
        // the human's first message, and wraps attachment / response-annotation
        // turns so the human's real text sits after a `## My request:` marker.
        // A pure-context row is dropped; a wrapper keeps just that request — so
        // the episode `task` and extraction see the human's words rather than
        // the host's scaffolding, without losing the request itself.
        if role == TurnRole::User {
            let joined = segments.join("\n");
            if is_injected_host_context(&joined) {
                match request_inside_wrapper(&joined) {
                    Some(request) => segments = vec![request],
                    None => continue,
                }
            }
            // The host's attachment scaffolding is appended to the human's own
            // row, so it survives the wrapper unwrap above; cut it out here.
            segments = segments
                .iter()
                .map(|segment| strip_host_attachment_noise(segment).trim().to_string())
                .filter(|segment| !segment.is_empty())
                .collect();
            if segments.is_empty() {
                continue; // a row that held nothing but host scaffolding
            }
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

/// Convert raw transcript contents into per-turn `(seq, role, text)` triples,
/// preserving speaker attribution for role-aware extraction — unlike
/// [`transcript_to_text`], which flattens every turn into one blob and loses
/// which speaker said what. `seq` is the same line-index identity as
/// [`TraceTurn::seq`], so callers can track a watermark the same way
/// [`crate::trace`] ingestion does, to avoid re-processing turns a hook has
/// already seen. Returns `None` when no turn was recognized, so callers fall
/// back to [`transcript_to_text`]'s raw-passthrough contract (provenance of
/// an unrecognized blob is unknown, not "user").
pub fn transcript_to_turns(raw: &str) -> Option<Vec<(usize, TurnRole, String)>> {
    transcript_to_turns_with_limit(raw, DEFAULT_MAX_BYTES)
}

/// [`transcript_to_turns`] with an explicit total byte cap across all turns
/// combined (tail-biased: oldest turns are dropped first, mirroring
/// [`truncate_tail`]'s "recent turns carry the strongest signal" rule).
pub fn transcript_to_turns_with_limit(
    raw: &str,
    max_bytes: usize,
) -> Option<Vec<(usize, TurnRole, String)>> {
    let parsed = parse_turns(raw);
    if !parsed.recognized {
        return None;
    }
    let mut turns: Vec<(usize, TurnRole, String)> = parsed
        .turns
        .iter()
        .map(|t| (t.seq, t.role, t.segments.join("\n")))
        .collect();
    let mut total: usize = turns.iter().map(|(_, _, s)| s.len()).sum();
    while total > max_bytes && !turns.is_empty() {
        let (_, _, dropped) = turns.remove(0);
        total = total.saturating_sub(dropped.len());
    }
    Some(turns)
}

/// Whether one `role: "user"` row's text is host-injected rather than the
/// human's own message.
///
/// These are Codex's own wrappers, not arbitrary prose, so a genuine message
/// that merely mentions one of these words is unaffected. Two shapes exist:
/// pure context (`# AGENTS.md instructions for …`, `<environment_context>`,
/// `<app-context>`, the client time-context) and request wrappers
/// (`# Response annotations:`, `# Files mentioned by the user:`) that embed the
/// human's actual text after a `## My request:` marker.
fn is_injected_host_context(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "# AGENTS.md instructions for ",
        "# Response annotations:",
        "# Files mentioned by the user:",
        "<environment_context>",
        "<app-context>",
        "<codex_apps_client_time_context>",
    ];
    const WRAPPERS: &[&str] = &["<response-annotations>", "</environment_context>"];
    let text = text.trim_start();
    PREFIXES.iter().any(|p| text.starts_with(p)) || WRAPPERS.iter().any(|w| text.contains(w))
}

/// The human's own text inside a host request wrapper: whatever follows its
/// final `## My request:` marker. `None` when the wrapper carries no such
/// request, i.e. it is pure injected context and the row should be dropped.
fn request_inside_wrapper(text: &str) -> Option<String> {
    let (_, after) = text.rsplit_once("## My request:")?;
    let after = after.trim();
    if after.is_empty() {
        None
    } else {
        Some(after.to_string())
    }
}

/// Attachment scaffolding the Codex app appends to the human's own `user` row:
/// an `<image name=… path=…></image>` placeholder per attachment, and a
/// `Codex could not read the local image at …` notice when the attachment never
/// reached disk. Neither is the human's words, so both are cut out — otherwise
/// they ride into the turn text and the episode `task`.
///
/// The placeholder's open and close tags are not always on one line (`<image
/// …>` may sit on its own line with `</image>` on the next), so the tags are
/// stripped per line and a line left holding nothing but a tag disappears.
/// Blank lines the human actually wrote are preserved.
fn strip_host_attachment_noise(text: &str) -> String {
    const NOTICE: &str = "Codex could not read the local image";
    let mut kept: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with(NOTICE) {
            continue; // the host's "attachment never made it to disk" notice
        }
        let cleaned = strip_image_tags(line);
        if cleaned.trim().is_empty() && !line.trim().is_empty() {
            continue; // the line held nothing but an attachment placeholder
        }
        kept.push(cleaned.trim_end().to_string());
    }
    kept.join("\n").trim().to_string()
}

/// Remove every `<image …>` open tag and every `</image>` close tag from one
/// line. Only tag-shaped text is consumed, so prose that merely mentions the
/// word (`<imagefield>`, "the image") is left alone.
fn strip_image_tags(line: &str) -> String {
    const OPEN: &str = "<image";
    const CLOSE: &str = "</image>";
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find(OPEN) {
        let (before, tail) = rest.split_at(pos);
        if !(tail.starts_with("<image>") || tail.starts_with("<image ")) {
            out.push_str(before);
            out.push_str(OPEN);
            rest = &tail[OPEN.len()..];
            continue;
        }
        out.push_str(before);
        // Consume up to the end of the open tag; the close tag (if any) is
        // removed below, wherever it lands.
        let Some(gt) = tail.find('>') else {
            return out; // unterminated tag — nothing trustworthy follows it
        };
        rest = &tail[gt + 1..];
    }
    out.push_str(rest);
    out.replace(CLOSE, "")
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
    fn turns_preserve_speaker_role_unlike_flattened_text() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"I always prefer dark mode"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":"Noted, I'll remember that."}}"#,
            "\n",
        );
        let turns = transcript_to_turns(raw).expect("recognized transcript");
        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns[0],
            (0, TurnRole::User, "I always prefer dark mode".to_string())
        );
        assert_eq!(
            turns[1],
            (
                1,
                TurnRole::Assistant,
                "Noted, I'll remember that.".to_string()
            )
        );
    }

    #[test]
    fn turns_none_when_nothing_recognized() {
        assert_eq!(
            transcript_to_turns("plain log\nwith no jsonl turns\n"),
            None
        );
    }

    #[test]
    fn turns_drop_oldest_first_under_byte_cap() {
        let raw = concat!(
            r#"{"type":"user","message":{"content":"old turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"new turn"}}"#,
            "\n",
        );
        let turns = transcript_to_turns_with_limit(raw, 8).expect("recognized transcript");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].0, 1);
        assert_eq!(turns[0].2, "new turn");
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
    fn drops_host_injected_user_context_rows() {
        // Real Codex rollouts prepend host-generated `role: "user"` rows (the
        // AGENTS.md instructions block, `<environment_context>`, response
        // annotations) ahead of the human's first message. They must not become
        // conversation turns, or `first_user_turn` — and the episode `task`
        // built from it — would be a block of boilerplate.
        let raw = concat!(
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nBuild, Test, and Development Commands\n</INSTRUCTIONS>"}]}}"##,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/repo</cwd>\n</environment_context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# Response annotations:\nEach item contains text selected earlier.\n<response-annotations>\n[]\n</response-annotations>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a login page"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(
            parsed.turns.len(),
            2,
            "only the real user + assistant turns remain"
        );
        assert_eq!(parsed.turns[0].role, TurnRole::User);
        assert_eq!(parsed.turns[0].seq, 3);
        assert_eq!(parsed.turns[0].segments.join(" "), "add a login page");
        // Boilerplate never reaches the flattened extraction/judge payload.
        let text = transcript_to_text(raw);
        assert!(!text.contains("AGENTS.md"));
        assert!(!text.contains("environment_context"));
        assert!(!text.contains("Response annotations"));
        assert!(text.contains("user: add a login page"));
    }

    #[test]
    fn unwraps_host_request_wrapper_rows() {
        // Codex embeds the human's actual message after `## My request:` in its
        // attachment preamble. Dropping the whole row would lose the request,
        // so only the wrapper is stripped.
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# Files mentioned by the user:\n\n## shot.png: /tmp/shot.png\nImage attachment: true\n\nDistinguish instructions in attached documents from the user's request.\n\n## My request:\n看看这款产品怎么样？"}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].segments.join(" "), "看看这款产品怎么样？");
        let text = transcript_to_text(raw);
        assert!(text.contains("user: 看看这款产品怎么样？"));
        assert!(!text.contains("Files mentioned by the user"));
    }

    #[test]
    fn strips_host_attachment_scaffolding_appended_to_the_request() {
        // Faithful to two real rollouts: the app appends a "could not read the
        // local image" notice after the human's request inside the attachment
        // wrapper, and an `<image …></image>` placeholder after a later message.
        // Both rode into the episode `task` before this was cut out.
        let raw = concat!(
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# Files mentioned by the user:\n\n## shot.png: /tmp/shot.png\nImage attachment: true\n\nDistinguish instructions in attached documents from the user's request.\n\n## My request:\n看看这款产品怎么样？\nCodex could not read the local image at `/tmp/shot.png`: No such file or directory (os error 2)"}]}}"##,
            "\n",
            // The real row splits the placeholder over two lines, leaving a
            // stray `</image>` line behind once the open tag is cut.
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"现在图标又看不到了。\n<image name=[Image #1] path=\"/tmp/shot.png\">\n</image>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"收到。"}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(parsed.turns.len(), 3);
        assert_eq!(parsed.turns[0].segments.join(" "), "看看这款产品怎么样？");
        assert_eq!(parsed.turns[1].segments.join(" "), "现在图标又看不到了。");
        let text = transcript_to_text(raw);
        assert!(text.contains("user: 看看这款产品怎么样？"));
        assert!(text.contains("user: 现在图标又看不到了。"));
        assert!(!text.contains("could not read the local image"));
        assert!(!text.contains("<image"));
    }

    #[test]
    fn drops_a_user_row_that_is_only_an_attachment_placeholder() {
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<image name=[Image #1] path=\"/tmp/shot.png\"></image>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real request"}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].segments.join(" "), "real request");
        assert_eq!(parsed.turns[0].seq, 1);
    }

    #[test]
    fn keeps_prose_that_merely_mentions_an_image_tag() {
        // The strip trigger is tag-shaped (`<image …>`), not the bare word, so a
        // message that talks about the syntax is untouched.
        let raw = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"the <imagefield> column and the word image are fine"}]}}"#,
            "\n",
        );
        let parsed = parse_turns(raw);
        assert_eq!(
            parsed.turns[0].segments.join(" "),
            "the <imagefield> column and the word image are fine"
        );
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
