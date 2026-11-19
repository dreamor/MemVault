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
            let Some(text) = normalize_user_row(&value) else {
                continue;
            };
            segments = vec![text];
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

/// The same row-aware human normalizer is used by extraction, friction and
/// terminal inference. Tool blocks are NOT human text; callers still account
/// for their results separately, including mixed prose/tool-result rows.
///
/// Policy is structural, not semantic intent detection: only complete known
/// host envelopes at unquoted line boundaries are removed. Inline examples,
/// fenced/indented code, blockquotes, malformed and unknown lookalikes remain
/// human text (their intent is uncertain). A literal unquoted canonical host
/// envelope is structurally indistinguishable from host output.
pub fn normalize_user_row(value: &serde_json::Value) -> Option<String> {
    let content = if is_claude_turn(value) && value["type"].as_str() == Some("user") {
        &value["message"]["content"]
    } else if is_codex_turn(value) && value["payload"]["role"].as_str() == Some("user") {
        &value["payload"]["content"]
    } else {
        return None;
    };
    // Do not trim each block before classifying its line boundaries: that
    // would turn an indented literal example into an unquoted host envelope.
    let texts = match content {
        serde_json::Value::String(text) => vec![text.as_str()],
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("text") | Some("input_text") | Some("output_text")
                )
            })
            .filter_map(|block| block["text"].as_str())
            .collect(),
        _ => Vec::new(),
    };
    let normalized = normalize_user_text(&texts.join("\n"));
    (!normalized.trim().is_empty()).then_some(normalized)
}

/// One structural human request. Codex may split it across consecutive rows.
#[derive(Debug)]
pub(crate) struct RequestGroup {
    pub seqs: Vec<usize>,
    pub texts: Vec<String>,
    pub format: &'static str,
}

/// Ephemeral ownership, shared by scoring and outcome selection. Call/result
/// associations are replayed forward; no transcript or new L0 row is retained.
#[derive(Debug, Default)]
pub(crate) struct RequestGroups {
    pub groups: Vec<RequestGroup>,
    prose_groups: std::collections::HashMap<usize, usize>,
    result_groups: std::collections::HashMap<usize, std::collections::HashMap<String, usize>>,
    row_groups: std::collections::HashMap<usize, Vec<usize>>,
    active: Option<usize>,
    pending: Option<usize>,
    last_result: Option<usize>,
    last_seq: Option<usize>,
}

impl RequestGroups {
    fn note_row(&mut self, seq: usize, group: usize) {
        let groups = self.row_groups.entry(seq).or_default();
        if !groups.contains(&group) {
            groups.push(group);
        }
    }

    pub fn prose_group(&self, seq: usize) -> Option<usize> {
        self.prose_groups.get(&seq).copied()
    }

    pub fn result_group(&self, seq: usize, call_id: &str) -> Option<usize> {
        self.result_groups.get(&seq)?.get(call_id).copied()
    }

    fn unanswered_feedback(&self) -> Option<usize> {
        let pending = self.pending?;
        (self.last_result.is_none()
            && self.active.is_some()
            && crate::friction::contains_any(
                &self.groups[pending].texts.join("\n"),
                crate::friction::CORRECTION_KEYWORDS,
            ))
        .then_some(pending)
    }

    pub fn selected_group(&self) -> Option<usize> {
        self.last_result.or_else(|| {
            if self.unanswered_feedback().is_some() {
                self.active
            } else {
                self.pending.or(self.active)
            }
        })
    }

    pub fn includes_prose(&self, seq: usize, group: usize) -> bool {
        self.prose_group(seq) == Some(group)
            || (self.active == Some(group)
                && self.prose_group(seq) == self.unanswered_feedback()
                && self.unanswered_feedback().is_some())
    }

    pub fn selected_rows(&self, group: usize) -> Vec<usize> {
        let mut rows: Vec<_> = self
            .row_groups
            .iter()
            .filter(|(seq, groups)| {
                self.last_seq.is_some_and(|end| **seq <= end)
                    && (groups.contains(&group) || self.includes_prose(**seq, group))
            })
            .map(|(seq, _)| *seq)
            .collect();
        rows.sort_unstable();
        rows
    }
}

/// Structural request ownership, not semantic task/intent classification.
/// Human-only rows open requests; mixed tool-result/prose rows retain their
/// text but cannot invent a new task or lose their result's call association.
pub(crate) fn request_groups(raw: &str) -> RequestGroups {
    use std::collections::HashMap;

    let mut out = RequestGroups::default();
    let mut calls: HashMap<String, usize> = HashMap::new();
    for (seq, line) in raw.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let codex = crate::friction::codex_payload(&value);
        let results = if let Some(payload) = codex {
            crate::friction::codex_tool_result(payload).map(|result| vec![result])
        } else if value["type"].as_str() == Some("user") {
            crate::friction::tool_results(&value["message"]["content"])
        } else {
            None
        };
        if let Some(results) = results {
            let mut last = None;
            for (id, _, _) in results {
                let owner = calls.get(&id).copied().or(out.active).or(out.pending);
                if let Some(group) = owner {
                    out.result_groups.entry(seq).or_default().insert(id, group);
                    out.note_row(seq, group);
                }
                last = owner;
            }
            if normalize_user_row(&value).is_some()
                && let Some(group) = out.active.or(last).or(out.pending)
            {
                out.prose_groups.insert(seq, group);
                out.note_row(seq, group);
            }
            out.last_result = last;
            out.last_seq = Some(seq);
            continue;
        }
        if let Some(text) = normalize_user_row(&value) {
            let format = if codex.is_some() {
                "codex_rollout"
            } else {
                "claude_code"
            };
            let group = out
                .pending
                .filter(|pending| codex.is_some() && out.groups[*pending].format == "codex_rollout")
                .unwrap_or_else(|| {
                    out.groups.push(RequestGroup {
                        seqs: Vec::new(),
                        texts: Vec::new(),
                        format,
                    });
                    out.groups.len() - 1
                });
            out.groups[group].seqs.push(seq);
            out.groups[group].texts.push(text);
            out.pending = Some(group);
            out.prose_groups.insert(seq, group);
            out.note_row(seq, group);
            out.last_result = None;
            out.last_seq = Some(seq);
            continue;
        }
        let work = if let Some(payload) = codex {
            payload["type"].as_str() == Some("function_call")
                || (payload["type"].as_str() == Some("message")
                    && payload["role"].as_str() == Some("assistant"))
        } else {
            is_claude_turn(&value) && value["type"].as_str() == Some("assistant")
        };
        if !work {
            continue;
        }
        out.active = out.pending.take().or(out.active);
        if let Some(group) = out.active {
            out.prose_groups.insert(seq, group);
            out.note_row(seq, group);
            if let Some(payload) = codex {
                if let Some((id, _)) = crate::friction::codex_tool_call(payload) {
                    calls.insert(id, group);
                }
            } else if let Some(blocks) = value["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"].as_str() == Some("tool_use")
                        && let Some(id) = block["id"].as_str()
                    {
                        calls.insert(id.to_string(), group);
                    }
                }
            }
        }
        out.last_result = None;
        out.last_seq = Some(seq);
    }
    out
}

/// Which lines may be structural host framing. Markdown quotations and code
/// are preserved even when their literal bytes match a known envelope.
fn structural_lines(lines: &[&str]) -> Vec<bool> {
    let mut fence: Option<(char, usize)> = None;
    lines
        .iter()
        .map(|line| {
            let trimmed = line.trim_start();
            let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
            let run = marker
                .map(|m| trimmed.chars().take_while(|c| *c == m).count())
                .unwrap_or(0);
            let indent = &line[..line.len() - trimmed.len()];
            let fence_indent = indent.len() <= 3 && indent.bytes().all(|byte| byte == b' ');
            if let Some((m, count)) = fence {
                if fence_indent
                    && marker == Some(m)
                    && run >= count
                    && trimmed[run..]
                        .bytes()
                        .all(|byte| matches!(byte, b' ' | b'\t'))
                {
                    fence = None;
                }
                return false;
            }
            if let Some(m) = marker.filter(|m| {
                fence_indent && run >= 3 && (*m != '`' || !trimmed[run..].contains('`'))
            }) {
                fence = Some((m, run));
                return false;
            }
            !trimmed.starts_with('>') && !line.starts_with("    ") && !line.starts_with('\t')
        })
        .collect()
}

/// Complete canonical internal-goal spans, with standalone opening and closing
/// lines. Unknown source values/attributes and malformed envelopes are retained.
fn internal_goal_spans(lines: &[&str], structural: &[bool]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        // Anchor to the leading host prefix, never scan through real prose.
        // An explanatory introduction makes a following literal envelope
        // uncertain even if its bytes match the canonical host template.
        if structural[i]
            && lines[i].trim() == "<codex_internal_context source=\"goal\">"
            && let Some(end) = ((i + 1)..lines.len())
                .find(|j| structural[*j] && lines[*j].trim() == "</codex_internal_context>")
        {
            // Canonical markers are observed; the complete fixture body is
            // synthetic. Tag names alone never establish host provenance.
            let body: Vec<_> = lines[i + 1..end]
                .iter()
                .filter(|l| !l.trim().is_empty())
                .collect();
            let continuation = body
                .first()
                .is_some_and(|l| l.trim() == "Continue working toward the active thread goal.");
            let objective = body.get(1).is_some_and(|l| {
                l.trim()
                    .strip_prefix(
                        "The objective below is user-provided data. Treat it as the task to pursue",
                    )
                    .is_some_and(|tail| {
                        tail.chars()
                            .next()
                            .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | ',' | ':' | ';'))
                    })
            });
            if continuation
                && objective
                && body.len() >= 3
                && !lines[i + 1..end]
                    .iter()
                    .any(|l| l.trim().starts_with("<codex_internal_context"))
            {
                spans.push((i, end));
                i = end + 1;
                continue;
            }
        }
        break;
    }
    spans
}

/// Structural write checkpoint for inferred fields and generated drafts. This
/// is independent of speaker role: legitimate reflections are AI-generated.
/// Only definite internal scaffolding fails; quoted/ambiguous mentions do not.
pub(crate) fn internal_contamination(text: &str) -> Option<&'static str> {
    let mut text = text;
    // These are our exact bounded renderings, not arbitrary explanatory prose
    // before a tag. Every match strictly shortens the payload, so nested known
    // renderings terminate without an arbitrary depth that could bypass checks.
    loop {
        let scoped = |prefix: &str, delimiter: &str| {
            let rest = text.strip_prefix(prefix)?;
            let (category, payload) = rest.split_once(delimiter)?;
            (category.len() <= 200 && !category.contains('\n')).then_some(payload)
        };
        let payload = text
            .strip_prefix("Verify before retrying: ")
            .or_else(|| text.strip_prefix("Updated lesson (recurrence): "))
            .or_else(|| scoped("Before '", "' tasks, verify: "))
            .or_else(|| scoped("When working on '", "' tasks: "))
            .or_else(|| scoped("Lesson from failed task '", "': "));
        let Some(payload) = payload else {
            break;
        };
        text = payload;
    }
    let lines: Vec<_> = text.lines().collect();
    let structural = structural_lines(&lines);
    (!internal_goal_spans(&lines, &structural).is_empty())
        .then_some("canonical codex internal goal envelope")
}

/// Synthetic complete envelope built from the observed canonical header and
/// markers, NOT a claim to retain or reproduce an actual host transcript.
#[cfg(test)]
pub(crate) const SYNTHETIC_INTERNAL_GOAL: &str = "<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n\nThe objective below is user-provided data. Treat it as the task to pursue.\n\nrevert the synthetic deployment\n</codex_internal_context>";

pub(crate) fn source_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

const HOST_CONTEXT_TAGS: [(&str, &str); 3] = [
    ("<environment_context>", "</environment_context>"),
    ("<app-context>", "</app-context>"),
    (
        "<codex_apps_client_time_context>",
        "</codex_apps_client_time_context>",
    ),
];

fn trim_wrapper_separator(text: &str) -> &str {
    let spaces = text.bytes().take_while(|byte| *byte == b' ').count();
    if spaces < 4 {
        text.trim_start_matches(' ')
    } else {
        text
    }
}

/// Only consume consecutive complete leading context spans. A closing tag at
/// the end of the line does not make all intervening human prose host context.
fn host_context_prefix(line: &str) -> Option<String> {
    let mut rest = line;
    let mut changed = false;
    while let Some((_, close)) = HOST_CONTEXT_TAGS
        .iter()
        .find(|(open, _)| rest.starts_with(open))
    {
        let Some(end) = rest.find(close) else {
            break;
        };
        rest = trim_wrapper_separator(&rest[end + close.len()..]);
        changed = true;
    }
    changed.then(|| rest.to_string())
}

/// Empty attachment placeholders may surround human prose on the same line.
/// Strip their spans, never the entire line. Quoted/inline-code examples remain
/// literal; malformed or nonempty image elements are not host placeholders.
fn host_image_placeholders(line: &str) -> Option<String> {
    let first = line.trim_start();
    if !first.starts_with("<image name=") || line.contains('`') {
        return None;
    }
    let end = first.find('>')?;
    if !first[..end].contains(" path=") || !first[end + 1..].starts_with("</image>") {
        return None;
    }
    let mut rest = line;
    let mut out = String::new();
    let mut literal_tail = false;
    let mut changed = false;
    while !rest.is_empty() {
        if !literal_tail
            && rest.starts_with("<image name=")
            && let Some(end) = rest.find('>')
            && rest[..end].contains(" path=")
            && rest[end + 1..].starts_with("</image>")
        {
            rest = &rest[end + 1 + "</image>".len()..];
            changed = true;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if matches!(ch, '\'' | '"') {
            // Outside an already-recognized placeholder, a quoted literal
            // makes later tag-shaped text uncertain. Retain that whole tail.
            literal_tail = true;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    changed.then(|| trim_wrapper_separator(&out).trim_end().to_string())
}

/// Known host IO diagnostics, not arbitrary prose starting with the notice.
/// Keep an unknown diagnostic or explanatory request intact; when a complete
/// known diagnostic has trailing real text, consume only the diagnostic span.
fn host_image_notice(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Codex could not read the local image at `")?;
    let (_, diagnostic) = rest.split_once("`: ")?;
    for message in [
        "No such file or directory",
        "Permission denied",
        "Not a directory",
        "Is a directory",
        "Input/output error",
        "Invalid argument",
    ] {
        let Some(code) = diagnostic
            .strip_prefix(message)
            .and_then(|text| text.strip_prefix(" (os error "))
        else {
            continue;
        };
        let Some((number, tail)) = code.split_once(')') else {
            continue;
        };
        if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(trim_wrapper_separator(tail).to_string());
        }
    }
    None
}

/// Strip only validated host ranges, preserving real text before/after them.
/// `## My request:` is structural ONLY inside a known leading request wrapper;
/// the first marker ends that wrapper, so later literal headings stay intact.
fn normalize_user_text(text: &str) -> String {
    let lines: Vec<_> = text.lines().collect();
    let structural = structural_lines(&lines);
    let mut removed = vec![false; lines.len()];
    let mut replacements: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    if let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) {
        if structural[start]
            && lines[start]
                .trim()
                .starts_with("# AGENTS.md instructions for ")
            && let Some(open) = ((start + 1)..lines.len())
                .find(|j| structural[*j] && lines[*j].trim() == "<INSTRUCTIONS>")
            && let Some(end) = ((open + 1)..lines.len())
                .find(|j| structural[*j] && lines[*j].trim() == "</INSTRUCTIONS>")
        {
            removed[start..=end].fill(true);
        }
        let heading = lines[start].trim();
        let request = ((start + 1)..lines.len())
            .find(|j| structural[*j] && lines[*j].trim() == "## My request:");
        let wrapper_end = request.unwrap_or(lines.len());
        let response = heading == "# Response annotations:"
            && ((start + 1)..wrapper_end)
                .any(|j| structural[j] && lines[j].trim() == "<response-annotations>")
            && ((start + 1)..wrapper_end)
                .any(|j| structural[j] && lines[j].trim() == "</response-annotations>");
        let files = heading == "# Files mentioned by the user:"
            && ((start + 1)..wrapper_end).any(|j| {
                structural[j] && lines[j].trim().starts_with("## ") && lines[j].contains(": ")
            });
        if structural[start] && (response || files) {
            if let Some(request) = request {
                removed[start..=request].fill(true);
            } else if response
                && let Some(end) = ((start + 1)..lines.len())
                    .find(|j| structural[*j] && lines[*j].trim() == "</response-annotations>")
            {
                removed[start..=end].fill(true);
            }
            // A files preamble without the request boundary has no known end;
            // preserve it rather than deleting potentially real outside text.
        }
        if structural[start]
            && heading == "<response-annotations>"
            && let Some(end) = ((start + 1)..lines.len()).find(|index| {
                structural[*index] && lines[*index].trim() == "</response-annotations>"
            })
        {
            let request = ((end + 1)..lines.len())
                .find(|index| structural[*index] && lines[*index].trim() == "## My request:");
            removed[start..=request.unwrap_or(end)].fill(true);
        }
    }
    for start in 0..lines.len() {
        if removed[start] || lines[start].trim().is_empty() {
            continue;
        }
        if !structural[start] {
            break;
        }
        let trimmed = lines[start].trim();
        let Some((open, close)) = HOST_CONTEXT_TAGS
            .into_iter()
            .find(|(open, _)| trimmed.starts_with(open))
        else {
            break;
        };
        if let Some(tail) = host_context_prefix(trimmed) {
            if tail.trim().is_empty() {
                removed[start] = true;
            } else {
                replacements.insert(start, tail);
                break;
            }
        } else if trimmed == open {
            if let Some(end) =
                ((start + 1)..lines.len()).find(|j| structural[*j] && lines[*j].trim() == close)
            {
                removed[start..=end].fill(true);
            } else {
                break;
            }
        } else {
            break;
        }
    }
    let visible: Vec<_> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if removed[i] {
                ""
            } else {
                replacements.get(&i).map(String::as_str).unwrap_or(line)
            }
        })
        .collect();
    for (start, end) in internal_goal_spans(&visible, &structural_lines(&visible)) {
        removed[start..=end].fill(true);
    }
    let attachment_structural = {
        let updated: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if removed[i] {
                    ""
                } else {
                    replacements.get(&i).map(String::as_str).unwrap_or(line)
                }
            })
            .collect();
        structural_lines(&updated)
    };
    for i in 0..lines.len() {
        if !attachment_structural[i] || removed[i] {
            continue;
        }
        let line = replacements
            .get(&i)
            .map(String::as_str)
            .unwrap_or(lines[i])
            .trim();
        if let Some(tail) = host_image_notice(line) {
            if tail.trim().is_empty() {
                removed[i] = true;
            } else {
                replacements.insert(i, tail);
            }
            continue;
        }
        if let Some(text) = host_image_placeholders(line) {
            if text.trim().is_empty() {
                removed[i] = true;
            } else {
                replacements.insert(i, text);
            }
            continue;
        }
        if line.starts_with("<image name=")
            && line.contains(" path=")
            && line.find('>') == Some(line.len().saturating_sub(1))
            && i + 1 < lines.len()
            && attachment_structural[i + 1]
            && lines[i + 1].trim() == "</image>"
        {
            removed[i] = true;
            removed[i + 1] = true;
        }
    }
    let kept: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !removed[*i])
        .map(|(i, line)| replacements.get(&i).map(String::as_str).unwrap_or(line))
        .collect();
    let Some(start) = kept.iter().position(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let end = kept
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(start);
    kept[start..=end].join("\n")
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

    fn codex_user(text: &str) -> String {
        serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}).to_string()
    }

    #[test]
    fn synthetic_internal_goal_is_structural_and_preserves_outside_text_and_seq() {
        let text = format!("real request before\n{SYNTHETIC_INTERNAL_GOAL}\nreal request after");
        let prefixed = format!("{SYNTHETIC_INTERNAL_GOAL}\nreal request after");
        let raw = format!(
            "not a turn\n{}\n{}\n{}\n",
            codex_user(SYNTHETIC_INTERNAL_GOAL),
            codex_user(&text),
            codex_user(&prefixed)
        );
        let parsed = parse_turns(&raw);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].seq, 2);
        assert_eq!(
            parsed.turns[0].segments[0], text,
            "real introduction makes the literal envelope uncertain"
        );
        assert_eq!(parsed.turns[1].seq, 3);
        assert_eq!(parsed.turns[1].segments[0], "real request after");
        assert!(internal_contamination(SYNTHETIC_INTERNAL_GOAL).is_some());
        assert!(internal_contamination(&text).is_none());
    }

    #[test]
    fn explanatory_introduction_preserves_a_complete_literal_canonical_envelope() {
        let text = format!("Explain the following metadata:\n{SYNTHETIC_INTERNAL_GOAL}");
        let parsed = parse_turns(&codex_user(&text));
        assert_eq!(parsed.turns[0].segments[0], text);
        assert!(
            internal_contamination(&text).is_none(),
            "template presence after human prose does not prove host origin"
        );
    }

    #[test]
    fn quoted_fenced_inline_and_malformed_goal_lookalikes_remain_uncertain() {
        let quoted = SYNTHETIC_INTERNAL_GOAL
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        for text in [
            format!("```xml\n{SYNTHETIC_INTERNAL_GOAL}\n```"),
            format!("~~~\n{SYNTHETIC_INTERNAL_GOAL}\n~~~"),
            quoted,
            format!(
                "Here is the syntax: `{}`",
                SYNTHETIC_INTERNAL_GOAL.replace('\n', " ")
            ),
            SYNTHETIC_INTERNAL_GOAL.replace("source=\"goal\"", "source=\"example\""),
            SYNTHETIC_INTERNAL_GOAL.replace(
                "Continue working toward the active thread goal.",
                "This is a user's example, not a host goal.",
            ),
            SYNTHETIC_INTERNAL_GOAL.replace("</codex_internal_context>", ""),
            "Please parse </environment_context> literally.".into(),
            "# AGENTS.md instructions for my article, not an INSTRUCTIONS wrapper".into(),
            "Use `<image name=x path=\"demo\"></image>` in the documentation.".into(),
        ] {
            let parsed = parse_turns(&codex_user(&text));
            assert_eq!(parsed.turns.len(), 1, "retained lookalike: {text}");
            assert_eq!(parsed.turns[0].segments[0], text.trim());
            assert!(
                internal_contamination(&text).is_none(),
                "not definite: {text}"
            );
        }
    }

    #[test]
    fn indented_literal_goal_remains_indented_and_is_not_reclassified_at_a_checkpoint() {
        let text = SYNTHETIC_INTERNAL_GOAL
            .lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_turns(&codex_user(&text));
        assert_eq!(parsed.turns[0].segments[0], text);
        assert!(internal_contamination(&parsed.turns[0].segments[0]).is_none());
    }

    #[test]
    fn wrapper_uses_first_structural_request_marker_not_last_literal_heading() {
        let text = "# Files mentioned by the user:\n## demo.txt: /synthetic/demo.txt\n## My request:\nDocument this heading:\n## My request:\nretain the second section";
        let parsed = parse_turns(&codex_user(text));
        assert_eq!(
            parsed.turns[0].segments[0],
            "Document this heading:\n## My request:\nretain the second section"
        );
        let ordinary = "## My request:\nreal text\n## My request:\nstill real";
        assert_eq!(
            parse_turns(&codex_user(ordinary)).turns[0].segments[0],
            ordinary
        );
        let response = "# Response annotations:\n<response-annotations>\n[]\n</response-annotations>\nreal outside request";
        assert_eq!(
            parse_turns(&codex_user(response)).turns[0].segments[0],
            "real outside request"
        );
    }

    #[test]
    fn checkpoint_recognizes_exact_rule_rendering_without_blanket_tag_rejection() {
        assert!(
            internal_contamination(&format!(
                "Before 'deploy' tasks, verify: {SYNTHETIC_INTERNAL_GOAL}"
            ))
            .is_some()
        );
        assert!(
            internal_contamination(&format!(
                "Verify before retrying: ```xml\n{SYNTHETIC_INTERNAL_GOAL}\n```"
            ))
            .is_none()
        );
        assert!(
            internal_contamination(
                "Discuss <codex_internal_context source=\"goal\"> as literal syntax."
            )
            .is_none()
        );
    }

    #[test]
    fn attachment_spans_preserve_inline_requests_and_original_seq() {
        let text = "<image name=a path=\"a\"></image> Describe these differences <image name=b path=\"b\"></image>";
        let raw = format!("synthetic metadata\n{}", codex_user(text));
        let parsed = parse_turns(&raw);
        assert_eq!(parsed.turns[0].seq, 1);
        assert_eq!(parsed.turns[0].segments[0], "Describe these differences");
        for literal in [
            "Use `<image name=a path=\"a\"></image>` literally.",
            "<image name=a path=\"a\"></image> Describe `literal <image name=b path=\"b\"></image>` syntax.",
            "<image name=a path=\"a\">real element body</image>",
        ] {
            assert_eq!(
                parse_turns(&codex_user(literal)).turns[0].segments[0],
                literal
            );
        }
        let quoted = "<image name=a path=\"a\"></image> Describe \"<image name=b path=\"b\"></image>\" literally";
        assert_eq!(
            parse_turns(&codex_user(quoted)).turns[0].segments[0],
            "Describe \"<image name=b path=\"b\"></image>\" literally"
        );
    }

    #[test]
    fn inline_context_prefix_preserves_all_real_text_outside_the_first_span() {
        let text = "<environment_context>a</environment_context> Explain schema <environment_context>b</environment_context>";
        assert_eq!(
            parse_turns(&codex_user(text)).turns[0].segments[0],
            "Explain schema <environment_context>b</environment_context>"
        );
        let prefixed = "<environment_context>a</environment_context><app-context>b</app-context> Explain schema";
        assert_eq!(
            parse_turns(&codex_user(prefixed)).turns[0].segments[0],
            "Explain schema"
        );
        let explanatory =
            "Explain this literal <environment_context>a</environment_context> schema";
        assert_eq!(
            parse_turns(&codex_user(explanatory)).turns[0].segments[0],
            explanatory
        );
    }

    #[test]
    fn image_notice_is_a_narrow_diagnostic_span_not_an_arbitrary_request_prefix() {
        let request = "Codex could not read the local image at `a.png`: explain why this happens and fix the reader.";
        assert_eq!(
            parse_turns(&codex_user(request)).turns[0].segments[0],
            request
        );
        let mixed = "Codex could not read the local image at `a.png`: No such file or directory (os error 2) Explain why and fix the reader.";
        assert_eq!(
            parse_turns(&codex_user(mixed)).turns[0].segments[0],
            "Explain why and fix the reader."
        );
        let literal = "```text\nCodex could not read the local image at `a.png`: No such file or directory (os error 2)\n```";
        assert_eq!(
            parse_turns(&codex_user(literal)).turns[0].segments[0],
            literal
        );
    }

    #[test]
    fn bare_response_annotations_unwrap_the_first_request_after_the_closing_boundary() {
        let text = "<response-annotations>\nannotation noise\n## My request:\nnot a real request inside the annotations\n</response-annotations>\n## My request:\nFix parser\n## My request:\nKeep this later literal heading";
        assert_eq!(
            parse_turns(&codex_user(text)).turns[0].segments[0],
            "Fix parser\n## My request:\nKeep this later literal heading"
        );
        let literal = format!("Explain these annotations:\n{text}");
        assert_eq!(
            parse_turns(&codex_user(&literal)).turns[0].segments[0],
            literal
        );
    }

    #[test]
    fn a_fence_with_nonwhitespace_suffix_cannot_expose_literal_markup() {
        for false_close in ["```not-a-closing-fence", "    ```", "``` \\t not a closer"] {
            let text = format!(
                "```text\n{false_close}\n<image name=a path=\"a\"></image>\n```\nDescribe the example"
            );
            assert_eq!(parse_turns(&codex_user(&text)).turns[0].segments[0], text);
        }
    }

    #[test]
    fn known_renderer_prefixes_strictly_shrink_without_a_bypass_depth() {
        for count in [5, 12] {
            let text = format!(
                "{}{SYNTHETIC_INTERNAL_GOAL}",
                "Verify before retrying: ".repeat(count)
            );
            assert!(internal_contamination(&text).is_some());
            let literal = format!("Explain this rendering: {text}");
            assert!(internal_contamination(&literal).is_none());
        }
        let truncated = SYNTHETIC_INTERNAL_GOAL.replace("</codex_internal_context>", "");
        assert!(
            internal_contamination(&truncated).is_none(),
            "a truncated lookalike is not proof of complete host framing"
        );
    }

    #[test]
    fn mixed_tool_prose_does_not_open_a_new_request_group() {
        let rows = [
            serde_json::json!({"type":"user","message":{"content":"Deploy service A"}}),
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Bash"}]}}),
            serde_json::json!({"type":"user","message":{"content":[{"type":"text","text":"The command output follows."},{"type":"tool_result","tool_use_id":"a","is_error":true,"content":"disk full"}]}}),
        ];
        let raw = rows
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let groups = request_groups(&raw);
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(groups.selected_group(), Some(0));
        assert_eq!(groups.selected_rows(0), vec![0, 1, 2]);
        assert_eq!(groups.result_group(2, "a"), Some(0));
        assert_eq!(
            parse_turns(&raw).turns[1].segments[0],
            "The command output follows."
        );
    }

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
