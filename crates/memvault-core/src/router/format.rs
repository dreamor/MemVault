//! Formatting helpers for turning retrieved memories into text that gets
//! injected into an Agent's context: MUST/REF instruction blocks, layered
//! overflow summaries, and the token-budget trimming that keeps injected
//! content within a configured size.
//!
//! Extracted out of `router.rs` because these are pure functions over
//! `SearchResult`/`SessionStartOutput` data — they don't touch any
//! `MemoryRouter` state — so they're easiest to read and test in isolation.

use crate::models::{Memory, Priority, SearchResult, SessionStartOutput, SkillStats};

pub(super) fn make_summary(memory: &Memory) -> String {
    let text = memory.instruction.as_deref().unwrap_or(&memory.content);
    if text.len() <= 60 {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(57).collect();
        format!("{}...", truncated)
    }
}

/// Format injection output with layered strategy:
/// - MUST memories: always full text
/// - REF memories within budget: full text
/// - Overflow: append summary hint + count
pub(super) fn format_layered_instructions(output: &SessionStartOutput) -> String {
    if output.injected.is_empty() && output.overflow_count == 0 {
        return String::new();
    }

    let mut result = format_as_instructions(&output.injected);

    if output.overflow_count > 0 {
        if !output.overflow_summaries.is_empty() {
            result.push_str("\n[MORE - 摘要]:\n");
            for summary in &output.overflow_summaries {
                result.push_str(&format!("  • {}\n", summary));
            }
        }
        result.push_str(&format!(
            "\n---\n还有 {} 条相关记忆未展示，使用 search_memory 工具可获取详情。\n",
            output.overflow_count
        ));
    }

    result
}

/// Render one skill memory as the structured instruction block injected into
/// an agent's context (§5.3 of the evolution plan). The success rate is only
/// shown once enough executions back it — a "100% (1 run)" badge would
/// over-sell an unproven skill.
pub(super) fn format_skill_block(memory: &Memory, stats: Option<&SkillStats>) -> String {
    let Some(meta) = memory.skill_meta.as_ref() else {
        // Not really a skill — degrade gracefully to the normal text path.
        return memory
            .instruction
            .clone()
            .unwrap_or_else(|| memory.content.clone());
    };

    let title: String = memory.content.chars().take(40).collect();
    let mut header = format!("[SKILL: {title}] (v{}", meta.version);
    if let Some(stats) = stats {
        if let Some(rate) = stats.success_rate() {
            header.push_str(&format!(" · 成功率 {:.0}%", rate * 100.0));
        }
        if stats.executions() > 0 {
            header.push_str(&format!(" · 基于 {} 次执行", stats.executions()));
        }
    }
    header.push(')');

    let mut out = header;
    if let Some(ref trigger) = meta.trigger {
        out.push_str(&format!("\n触发条件：{trigger}"));
    }
    if !meta.steps.is_empty() {
        out.push_str("\n步骤：");
        for (i, step) in meta.steps.iter().enumerate() {
            out.push_str(&format!("\n  {}. {step}", i + 1));
        }
    }
    if let Some(ref verification) = meta.verification {
        out.push_str(&format!("\n验证：{verification}"));
    }
    out
}

pub(super) fn format_as_instructions(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut must_lines = Vec::new();
    let mut ref_lines = Vec::new();
    let mut bg_lines = Vec::new();

    for r in results {
        let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
        match r.memory.priority {
            Priority::Must => must_lines.push(format!("[MUST] {}", text)),
            Priority::Reference => ref_lines.push(format!("[REF] {}", text)),
            Priority::Background => bg_lines.push(format!("[BG] {}", text)),
        }
    }

    let mut output = String::from("[MEMORY CONTEXT - 必须遵循]:\n");
    for line in must_lines
        .iter()
        .chain(ref_lines.iter())
        .chain(bg_lines.iter())
    {
        output.push_str(line);
        output.push('\n');
    }

    output
}

pub(super) fn estimate_tokens(text: &str) -> usize {
    let ascii_count = text.chars().filter(|c| c.is_ascii()).count();
    let non_ascii_count = text.chars().count() - ascii_count;
    // ~4 chars/token for English, ~1.5 chars/token for CJK
    (ascii_count / 4) + (non_ascii_count * 2 / 3) + 1
}

/// Trim `results` in place to the token budget, RETURNING the trimmed tail.
/// Callers must account for the cut items (skip-reason tracking); silently
/// discarding them here would make "why wasn't X injected?" unanswerable.
pub(super) fn trim_to_budget(
    results: &mut Vec<SearchResult>,
    token_budget: usize,
) -> Vec<SearchResult> {
    let mut total = 0;
    let mut keep = 0;
    for r in results.iter() {
        let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
        let tokens = estimate_tokens(text) + 15; // overhead for [MUST]/[REF] tag + newline
        if total + tokens > token_budget && r.memory.priority != Priority::Must {
            break;
        }
        total += tokens;
        keep += 1;
    }
    results.split_off(keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, SkillMeta, SourceAgent};

    fn make_result(priority: Priority, content: &str) -> SearchResult {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let memory = Memory::new(MemoryType::Fact, content.to_string(), priority, agent);
        SearchResult {
            score: 1.0,
            memory,
            hit_sources: Vec::new(),
        }
    }

    fn make_skill(steps: usize, with_verification: bool) -> Memory {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mut memory = Memory::new(
            MemoryType::Skill,
            "deploy runbook".to_string(),
            Priority::Reference,
            agent,
        );
        memory.skill_meta = Some(SkillMeta {
            trigger: Some("deploy".to_string()),
            steps: (0..steps).map(|i| format!("step {}", i + 1)).collect(),
            verification: with_verification.then(|| "health check passes".to_string()),
            version: 3,
        });
        memory
    }

    #[test]
    fn test_format_skill_block_full() {
        let skill = make_skill(2, true);
        let block = format_skill_block(&skill, None);
        assert!(block.contains("[SKILL: deploy runbook] (v3)"));
        assert!(block.contains("触发条件：deploy"));
        assert!(block.contains("1. step 1"));
        assert!(block.contains("2. step 2"));
        assert!(block.contains("验证：health check passes"));
        // No stats → no rate or execution count.
        assert!(!block.contains("成功率"));
        assert!(!block.contains("次执行"));
    }

    #[test]
    fn test_format_skill_block_shows_rate_with_enough_samples() {
        let skill = make_skill(1, false);
        let stats = crate::models::SkillStats {
            skill_memory_id: skill.id.clone(),
            injected_count: 8,
            success_count: 7,
            failure_count: 1,
        };
        let block = format_skill_block(&skill, Some(&stats));
        assert!(block.contains("成功率 88%"));
        assert!(block.contains("基于 8 次执行"));
        assert!(!block.contains("验证："));
    }

    #[test]
    fn test_format_skill_block_hides_rate_below_threshold() {
        let skill = make_skill(1, false);
        let stats = crate::models::SkillStats {
            skill_memory_id: skill.id.clone(),
            injected_count: 2,
            success_count: 2,
            failure_count: 0,
        };
        let block = format_skill_block(&skill, Some(&stats));
        assert!(
            !block.contains("成功率"),
            "small sample must not show a rate"
        );
        assert!(block.contains("基于 2 次执行"));
    }

    #[test]
    fn test_format_skill_block_without_meta_falls_back() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let mut memory = Memory::new(
            MemoryType::Skill,
            "bare skill".to_string(),
            Priority::Reference,
            agent,
        );
        memory.instruction = Some("plain instruction".to_string());
        assert_eq!(format_skill_block(&memory, None), "plain instruction");
    }

    #[test]
    fn test_make_summary_short() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "short".to_string(),
            Priority::Reference,
            agent,
        );
        assert_eq!(make_summary(&mem), "short");
    }

    #[test]
    fn test_make_summary_truncates_long_text() {
        let agent = SourceAgent {
            id: "t".to_string(),
            agent_type: "g".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "x".repeat(100),
            Priority::Reference,
            agent,
        );
        let summary = make_summary(&mem);
        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 61);
    }

    #[test]
    fn test_format_as_instructions_empty() {
        assert_eq!(format_as_instructions(&[]), "");
    }

    #[test]
    fn test_format_as_instructions_orders_by_priority() {
        let results = vec![
            make_result(Priority::Background, "bg item"),
            make_result(Priority::Must, "must item"),
            make_result(Priority::Reference, "ref item"),
        ];
        let output = format_as_instructions(&results);
        let must_pos = output.find("[MUST]").unwrap();
        let ref_pos = output.find("[REF]").unwrap();
        let bg_pos = output.find("[BG]").unwrap();
        assert!(must_pos < ref_pos);
        assert!(ref_pos < bg_pos);
    }

    #[test]
    fn test_estimate_tokens_scales_with_length() {
        let short = estimate_tokens("hi");
        let long = estimate_tokens(&"word ".repeat(50));
        assert!(long > short);
    }

    #[test]
    fn test_trim_to_budget_keeps_must_even_over_budget() {
        let mut results = vec![make_result(Priority::Must, &"x".repeat(500))];
        trim_to_budget(&mut results, 1);
        assert_eq!(
            results.len(),
            1,
            "MUST must survive even if it exceeds the budget"
        );
    }

    #[test]
    fn test_trim_to_budget_drops_reference_when_over_budget() {
        let mut results = vec![
            make_result(Priority::Reference, &"x".repeat(50)),
            make_result(Priority::Reference, &"y".repeat(50)),
            make_result(Priority::Reference, &"z".repeat(50)),
        ];
        trim_to_budget(&mut results, 20);
        assert!(results.len() < 3);
    }

    #[test]
    fn test_format_layered_instructions_no_overflow() {
        let output = SessionStartOutput {
            injected: vec![make_result(Priority::Must, "rule")],
            overflow_count: 0,
            overflow_summaries: vec![],
            skipped: vec![],
        };
        let formatted = format_layered_instructions(&output);
        assert!(formatted.contains("[MUST]"));
        assert!(!formatted.contains("search_memory"));
    }

    #[test]
    fn test_format_layered_instructions_with_overflow() {
        let output = SessionStartOutput {
            injected: vec![make_result(Priority::Must, "rule")],
            overflow_count: 3,
            overflow_summaries: vec!["extra one".to_string()],
            skipped: vec![],
        };
        let formatted = format_layered_instructions(&output);
        assert!(formatted.contains("[MUST]"));
        assert!(formatted.contains("extra one"));
        assert!(formatted.contains("search_memory"));
        assert!(formatted.contains("3"));
    }

    #[test]
    fn test_format_layered_instructions_empty() {
        let output = SessionStartOutput {
            injected: vec![],
            overflow_count: 0,
            overflow_summaries: vec![],
            skipped: vec![],
        };
        assert_eq!(format_layered_instructions(&output), "");
    }
}
