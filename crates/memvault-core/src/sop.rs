//! External SOP/Markdown skill import (Phase D).
//!
//! Parses Markdown SOP documents into skill definitions. Format (lenient):
//!
//! - An `#` (H1) heading always starts a new skill; the heading text is the
//!   title.
//! - A `##` (H2) heading starts a new skill ONLY when it is not already
//!   inside an H1's section — i.e. a flat document of sibling `##` headings
//!   (no `#` above them) is treated as one skill per heading, exactly like
//!   before. But once an `#` has opened a skill, subsequent `##` headings up
//!   to the next `#` are treated as sub-sections of that SAME skill (their
//!   trigger/verification/list-item content is absorbed into it, not split
//!   off). This distinguishes two real-world shapes that are visually
//!   identical at the `##` line itself: a flat multi-SOP file
//!   (`## Skill One` / `## Skill Two` as siblings) versus a single skill
//!   document with descriptive subsections (`# Skill\n## Usage\n##
//!   Parameters\n## Examples`, the shape used by Claude/Hermes `SKILL.md`
//!   files) — without this distinction, the latter fragments into one
//!   low-value "skill" per subsection instead of one real skill with all its
//!   steps combined. `###` and deeper were already, and remain, sub-structure
//!   rather than a boundary at any level.
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

/// Heading level relevant to skill boundaries. `H3`+ never reaches this far
/// (see [`heading_with_level`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadingLevel {
    H1,
    H2,
}

fn heading_with_level(line: &str) -> Option<(HeadingLevel, &str)> {
    let trimmed = line.trim();

    if let Some(rest) = trimmed.strip_prefix("##") {
        // "###" and deeper are sub-structure, not a boundary at any level.
        if rest.starts_with('#') {
            return None;
        }
        let title = rest.trim();
        return if title.is_empty() {
            None
        } else {
            Some((HeadingLevel::H2, title))
        };
    }

    if let Some(rest) = trimmed.strip_prefix('#') {
        let title = rest.trim();
        return if title.is_empty() {
            None
        } else {
            Some((HeadingLevel::H1, title))
        };
    }

    None
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
    // Which level opened the section we're currently accumulating into —
    // `None` before any heading, otherwise the boundary-widening rule above.
    let mut current_opener: Option<HeadingLevel> = None;

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

        if let Some((level, title)) = heading_with_level(line) {
            // An H2 nested under an already-open H1 is a sub-section of the
            // SAME skill: absorb it (no flush, no title change) so its
            // trigger/verification/list-item content keeps accumulating into
            // the skill the H1 opened.
            if level == HeadingLevel::H2 && current_opener == Some(HeadingLevel::H1) {
                continue;
            }

            // Otherwise this heading is a new skill boundary: flush the
            // previous section and open a new one.
            flush(
                current_title.take(),
                current_trigger.take(),
                std::mem::take(&mut current_steps),
                current_verification.take(),
                &mut result,
                seen_any_heading,
            );
            current_title = Some(title.to_string());
            current_opener = Some(level);
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

    /// A `# Title` followed by `##` subsections (the shape used by
    /// Claude/Hermes `SKILL.md` files) must produce ONE skill with steps
    /// combined across every subsection, not one skill per `##`.
    #[test]
    fn test_h1_absorbs_nested_h2_subsections() {
        let doc = r#"# Board CLI Skill

## Usage
Some prose, no steps here.

## Parameters
- query: search text
- limit: max results

## Examples
1. run a search
2. render results
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1, "H2 subsections must not split off");
        assert_eq!(result.skipped_no_steps, 0);
        let skill = &result.skills[0];
        assert_eq!(skill.title, "Board CLI Skill");
        assert_eq!(
            skill.steps,
            vec![
                "query: search text",
                "limit: max results",
                "run a search",
                "render results"
            ],
            "steps from every absorbed H2 subsection must be combined, in document order"
        );
    }

    /// Two `# Title` skills, each with its own `##` subsections, must stay
    /// two separate skills — absorption only applies within one H1's span.
    #[test]
    fn test_h1_then_h2_then_new_h1_starts_second_skill() {
        let doc = r#"# Skill A
## sub
1. step a

# Skill B
## sub
1. step b
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.skills[0].title, "Skill A");
        assert_eq!(result.skills[0].steps, vec!["step a"]);
        assert_eq!(result.skills[1].title, "Skill B");
        assert_eq!(result.skills[1].steps, vec!["step b"]);
    }

    /// H3+ subsections were already absorbed before this change (only `#`/
    /// `##` are boundary candidates at all) — confirm that still holds when
    /// nested three levels deep under an H1, mirroring the real
    /// `# Skill\n## 核心命令\n### 浏览\n### 搜索` shape.
    #[test]
    fn test_h3_under_absorbed_h2_still_not_a_boundary() {
        let doc = r#"# Skill
## 核心命令
### 浏览
1. list items
### 搜索
1. search items
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].steps, vec!["list items", "search items"]);
    }

    /// A bare `##`-only document (no `#` anywhere) keeps the pre-existing
    /// flat sibling behavior — this is the shape multi-SOP import files use.
    #[test]
    fn test_h2_siblings_without_h1_still_split() {
        let doc = "## First\n1. a\n\n## Second\n1. b\n";
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.skills[0].title, "First");
        assert_eq!(result.skills[1].title, "Second");
    }

    /// trigger:/verification: lines inside an absorbed H2 subsection still
    /// attach to the enclosing H1 skill (first-one-wins, same as before).
    #[test]
    fn test_trigger_inside_absorbed_h2_attaches_to_h1_skill() {
        let doc = r#"# Deploy
## Preconditions
trigger: deploy
1. check env

## Verification
verification: healthz ok
"#;
        let result = parse_sops(doc, "fallback");
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].trigger.as_deref(), Some("deploy"));
        assert_eq!(result.skills[0].verification.as_deref(), Some("healthz ok"));
    }
}
