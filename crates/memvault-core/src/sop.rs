//! External SOP/Markdown skill import (Phase D).
//!
//! Parses Markdown SOP documents into skill definitions. Format (lenient):
//!
//! - Each `#`/`##` heading starts a new skill; the heading text is the title.
//!   A document without headings is treated as a single skill titled by the
//!   caller-provided fallback.
//! - `trigger: <text>` (or `触发条件：<text>`) sets the trigger phrase.
//! - `verification: <text>` (or `验证：<text>`) sets the verification.
//! - Ordered/unordered list items (outside code fences) become the steps,
//!   in document order.
//!
//! Sections without any steps are skipped — a skill without a procedure is
//! nothing to inject.

use serde::{Deserialize, Serialize};

/// One skill parsed from an SOP document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedSkill {
    pub title: String,
    pub trigger: Option<String>,
    pub steps: Vec<String>,
    pub verification: Option<String>,
}

/// Parse outcome: the parsed skills plus how many sections were skipped
/// (had a heading but no steps).
#[derive(Debug, Clone, Default)]
pub struct SopParseResult {
    pub skills: Vec<ParsedSkill>,
    pub skipped_no_steps: usize,
}

fn is_trigger_line(line: &str) -> Option<&str> {
    let lower = line.trim().to_lowercase();
    for key in ["trigger:", "触发条件:", "触发条件："] {
        if let Some(rest) = lower.strip_prefix(key) {
            // Preserve the ORIGINAL case of the value: re-slice from the
            // original line using the byte length of the matched prefix.
            let start = line.trim().len() - rest.len();
            return Some(line.trim()[start..].trim());
        }
    }
    None
}

fn is_verification_line(line: &str) -> Option<&str> {
    let lower = line.trim().to_lowercase();
    for key in ["verification:", "verify:", "验证:", "验证："] {
        if let Some(rest) = lower.strip_prefix(key) {
            let start = line.trim().len() - rest.len();
            return Some(line.trim()[start..].trim());
        }
    }
    None
}

fn heading_title(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("##")
        .or_else(|| trimmed.strip_prefix('#'))?;
    // "###" and deeper are sub-structure, not skill boundaries.
    if rest.starts_with('#') {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() { None } else { Some(title) }
}

fn list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let bytes = trimmed.as_bytes();

    // Ordered: "1." / "1)" / "12." — digits are ASCII, so byte offsets are
    // char offsets within the prefix.
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0 && idx < bytes.len() && (bytes[idx] == b'.' || bytes[idx] == b')') {
        let text = trimmed[idx + 1..].trim();
        return if text.is_empty() { None } else { Some(text) };
    }

    // Unordered: "- text" / "* text" (not "---" rules: needs whitespace after).
    if let Some(rest) = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        && let Some(text) = rest.strip_prefix(char::is_whitespace)
    {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// Parse an SOP markdown document into skill definitions.
pub fn parse_sops(markdown: &str, fallback_title: &str) -> SopParseResult {
    let mut result = SopParseResult::default();

    let mut current_title: Option<String> = None;
    let mut current_trigger: Option<String> = None;
    let mut current_steps: Vec<String> = Vec::new();
    let mut current_verification: Option<String> = None;
    let mut seen_any_heading = false;
    let mut in_code_fence = false;

    let flush = |title: Option<String>,
                 trigger: Option<String>,
                 steps: Vec<String>,
                 verification: Option<String>,
                 result: &mut SopParseResult,
                 had_heading: bool| {
        if steps.is_empty() {
            if had_heading {
                result.skipped_no_steps += 1;
            }
            return;
        }
        result.skills.push(ParsedSkill {
            title: title.unwrap_or_else(|| fallback_title.to_string()),
            trigger,
            steps,
            verification,
        });
    };

    for line in markdown.lines() {
        if line.trim().starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        if let Some(title) = heading_title(line) {
            // Flush the previous section.
            flush(
                current_title.take(),
                current_trigger.take(),
                std::mem::take(&mut current_steps),
                current_verification.take(),
                &mut result,
                seen_any_heading,
            );
            current_title = Some(title.to_string());
            seen_any_heading = true;
            continue;
        }

        if let Some(trigger) = is_trigger_line(line) {
            if current_trigger.is_none() {
                current_trigger = Some(trigger.to_string());
            }
            continue;
        }
        if let Some(verification) = is_verification_line(line) {
            if current_verification.is_none() {
                current_verification = Some(verification.to_string());
            }
            continue;
        }
        if let Some(item) = list_item(line) {
            current_steps.push(item.to_string());
        }
    }

    // Flush the final section (or the whole heading-less document).
    flush(
        current_title,
        current_trigger,
        current_steps,
        current_verification,
        &mut result,
        seen_any_heading,
    );

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_skill_full_metadata() {
        let doc = r#"# Deploy Dashboard

trigger: deploy dashboard
verification: curl /healthz returns ok

1. Check environment variables
2. Build the bundle
3. Push to CDN
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skipped_no_steps, 0);
        let skill = &result.skills[0];
        assert_eq!(skill.title, "Deploy Dashboard");
        assert_eq!(skill.trigger.as_deref(), Some("deploy dashboard"));
        assert_eq!(skill.steps.len(), 3);
        assert_eq!(skill.steps[0], "Check environment variables");
        assert_eq!(
            skill.verification.as_deref(),
            Some("curl /healthz returns ok")
        );
    }

    #[test]
    fn test_parse_multiple_skills_h2() {
        let doc = r#"Intro text ignored.

## Skill One
trigger: one
1. step a
2. step b

## Skill Two
- bullet step
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.skills[0].title, "Skill One");
        assert_eq!(result.skills[0].steps, vec!["step a", "step b"]);
        assert_eq!(result.skills[1].title, "Skill Two");
        assert_eq!(result.skills[1].steps, vec!["bullet step"]);
    }

    #[test]
    fn test_parse_headingless_document_uses_fallback() {
        let doc = "1. do this\n2. do that\n";
        let result = parse_sops(doc, "runbook-from-filename");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].title, "runbook-from-filename");
        assert_eq!(result.skills[0].steps.len(), 2);
    }

    #[test]
    fn test_skip_section_without_steps() {
        let doc = r#"## No Steps Here
just prose, no procedure.

## Real Skill
1. real step
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skipped_no_steps, 1);
        assert_eq!(result.skills[0].title, "Real Skill");
    }

    #[test]
    fn test_code_fences_are_ignored() {
        let doc = r#"# Skill
```bash
1. not a step
```
1. real step
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].steps, vec!["real step"]);
    }

    #[test]
    fn test_cjk_metadata_lines() {
        let doc = "# 部署流程\n触发条件：部署静态站点\n验证：健康检查通过\n1. 构建\n2. 推送\n";
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        let skill = &result.skills[0];
        assert_eq!(skill.trigger.as_deref(), Some("部署静态站点"));
        assert_eq!(skill.verification.as_deref(), Some("健康检查通过"));
        assert_eq!(skill.steps, vec!["构建", "推送"]);
    }

    #[test]
    fn test_first_trigger_wins_and_h3_not_a_boundary() {
        let doc = r#"# Skill
trigger: first
trigger: second
### sub note
1. step
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].trigger.as_deref(), Some("first"));
        assert_eq!(result.skills[0].steps, vec!["step"]);
    }

    #[test]
    fn test_empty_document() {
        let result = parse_sops("", "fallback");
        assert!(result.skills.is_empty());
        assert_eq!(result.skipped_no_steps, 0);
    }
}
