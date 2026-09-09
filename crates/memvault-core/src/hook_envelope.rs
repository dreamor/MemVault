//! Host hook payloads and envelopes for the plugin adapters (P1).
//!
//! Hooks receive a JSON document on stdin and (for SessionStart) must answer
//! with an `additionalContext` envelope. Both directions are handled here, in
//! Rust, so the hook shell scripts never touch JSON: hand-rolled concatenation
//! is exactly how hooks end up emitting invalid JSON, and macOS ships without
//! `jq`. Envelope shape per the Claude Code hooks reference:
//! `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"…"}}`.

use serde_json::Value;

/// The subset of a host hook payload MemVault consumes. Every field is
/// optional — hosts evolve these payloads between versions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HookInput {
    /// Host session id, threaded into `SourceAgent.session_id` for extraction.
    pub session_id: Option<String>,
    /// Stop-hook conversations: the transcript file the host wrote for this
    /// session (Claude Code).
    pub transcript_path: Option<String>,
    /// SessionStart hint: the working directory, used as the project scope.
    pub cwd: Option<String>,
    /// Why the hook fired (startup / resume / clear / comfy…). Not load
    /// bearing today; exposed for parity checks and debugging.
    pub source: Option<String>,
    /// UserPromptSubmit-only: the submitted prompt, usable as a context hint.
    pub prompt: Option<String>,
}

impl HookInput {
    /// Tolerant parse: unrecognized input yields the all-`None` default rather
    /// than an error — a hook must never fail the host session because of a
    /// payload it does not understand.
    pub fn parse(raw: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return Self::default();
        };
        let field = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        Self {
            session_id: field("session_id"),
            transcript_path: field("transcript_path"),
            cwd: field("cwd"),
            source: field("source"),
            prompt: field("prompt"),
        }
    }
}

/// Build the Claude Code SessionStart hook envelope around the formatted
/// instruction text. Serialized with serde_json — never string-concatenated.
pub fn session_start_hook_json(additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_fields_and_ignores_unknown_ones() {
        let raw = concat!(
            r#"{"session_id":"abc-123","transcript_path":"/tmp/x.jsonl","cwd":"/work/memvault","#,
            r#""source":"startup","hook_event_name":"SessionStart","extra":{"nested":true}}"#,
        );
        let input = HookInput::parse(raw);
        assert_eq!(input.session_id.as_deref(), Some("abc-123"));
        assert_eq!(input.cwd.as_deref(), Some("/work/memvault"));
        assert_eq!(input.source.as_deref(), Some("startup"));
        assert_eq!(input.prompt, None);
    }

    #[test]
    fn garbage_payload_falls_back_to_default() {
        for raw in ["", "not json", "[1,2]", "{\"session_id\":42}"] {
            assert_eq!(HookInput::parse(raw), HookInput::default());
        }
    }

    #[test]
    fn empty_strings_are_treated_as_absent() {
        assert_eq!(HookInput::parse(r#"{"cwd":""}"#), HookInput::default());
    }

    #[test]
    fn envelope_escapes_quotes_and_newlines() {
        let json = session_start_hook_json("rule one\nsay \"hi\"");
        let value: Value = serde_json::from_str(&json).expect("envelope must be valid JSON");
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "rule one\nsay \"hi\""
        );
    }

    #[test]
    fn empty_context_still_produces_a_valid_envelope() {
        let value: Value = serde_json::from_str(&session_start_hook_json("")).expect("valid JSON");
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap(),
            ""
        );
    }
}
