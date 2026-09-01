use crate::models::MemoryType;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Coding,
    Writing,
    Design,
    Research,
    Project,
    General,
}

#[derive(Debug, Clone)]
pub struct IntentResult {
    pub primary: Intent,
    pub domains: Vec<String>,
    pub confidence: f64,
}

struct Rule {
    intent: Intent,
    keywords: &'static [&'static str],
    domains: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule {
        intent: Intent::Coding,
        keywords: &[
            "code",
            "coding",
            "program",
            "function",
            "class",
            "method",
            "bug",
            "fix",
            "refactor",
            "test",
            "debug",
            "compile",
            "build",
            "deploy",
            "api",
            "endpoint",
            "database",
            "sql",
            "query",
            "schema",
            "migration",
            "git",
            "commit",
            "branch",
            "merge",
            "pr",
            "pull request",
            "review",
            "lint",
            "format",
            "type",
            "interface",
            "struct",
            "enum",
            "trait",
            "impl",
            "async",
            "await",
            "error",
            "exception",
            "import",
            "module",
            "package",
            "dependency",
            "cargo",
            "npm",
            "pip",
            "python",
            "rust",
            "javascript",
            "typescript",
            "java",
            "go",
            "c++",
            "代码",
            "编程",
            "函数",
            "类",
            "方法",
            "调试",
            "编译",
            "构建",
            "部署",
            "接口",
            "数据库",
            "重构",
            "测试",
        ],
        domains: &["coding", "programming", "development"],
    },
    Rule {
        intent: Intent::Writing,
        keywords: &[
            "write",
            "writing",
            "article",
            "blog",
            "post",
            "essay",
            "document",
            "documentation",
            "readme",
            "changelog",
            "copy",
            "content",
            "draft",
            "edit",
            "proofread",
            "grammar",
            "tone",
            "style guide",
            "markdown",
            "文章",
            "写作",
            "文档",
            "博客",
            "草稿",
            "校对",
        ],
        domains: &["writing", "content", "documentation"],
    },
    Rule {
        intent: Intent::Design,
        keywords: &[
            "design",
            "architecture",
            "ui",
            "ux",
            "layout",
            "wireframe",
            "mockup",
            "component",
            "figma",
            "sketch",
            "color",
            "font",
            "responsive",
            "css",
            "style",
            "animation",
            "prototype",
            "设计",
            "架构",
            "布局",
            "原型",
            "组件",
        ],
        domains: &["design", "ui", "ux"],
    },
    Rule {
        intent: Intent::Research,
        keywords: &[
            "research",
            "investigate",
            "analyze",
            "compare",
            "benchmark",
            "evaluate",
            "study",
            "survey",
            "review",
            "paper",
            "finding",
            "conclusion",
            "调研",
            "分析",
            "比较",
            "评估",
            "研究",
        ],
        domains: &["research", "analysis"],
    },
    Rule {
        intent: Intent::Project,
        keywords: &[
            "project",
            "milestone",
            "deadline",
            "sprint",
            "roadmap",
            "plan",
            "schedule",
            "timeline",
            "priority",
            "task",
            "issue",
            "ticket",
            "项目",
            "里程碑",
            "截止",
            "计划",
            "进度",
            "任务",
        ],
        domains: &["project", "management"],
    },
];

pub fn analyze_intent(message: &str) -> IntentResult {
    let lower = message.to_lowercase();
    let mut scores: Vec<(Intent, usize, Vec<String>)> = Vec::new();

    for rule in RULES {
        let mut hits = 0;
        let mut matched_domains = Vec::new();

        for &kw in rule.keywords {
            if lower.contains(kw) {
                hits += 1;
            }
        }

        if hits > 0 {
            matched_domains.extend(rule.domains.iter().map(|d| d.to_string()));
            scores.push((rule.intent.clone(), hits, matched_domains));
        }
    }

    scores.sort_by_key(|b| std::cmp::Reverse(b.1));

    if let Some((intent, hits, domains)) = scores.into_iter().next() {
        let confidence = (hits as f64 / 3.0).min(1.0);
        IntentResult {
            primary: intent,
            domains,
            confidence,
        }
    } else {
        IntentResult {
            primary: Intent::General,
            domains: vec!["general".to_string()],
            confidence: 0.5,
        }
    }
}

/// Match a skill trigger phrase against session context.
///
/// Two deterministic rules (no fuzziness that couldn't be regression-tested):
/// 1. Whole-trigger containment — the context mentions the full trigger
///    ("deploy" ⊂ "please deploy the dashboard").
/// 2. Token overlap — for multi-word triggers, at least half of the
///    significant tokens (len ≥ 2) appear in the context, so a trigger like
///    "部署 静态 站点" still fires on "帮我部署站点" even without "静态".
///
/// A single-token trigger falls back to rule 1 only.
pub fn trigger_matches_context(trigger: &str, context: &str) -> bool {
    let trigger_lower = trigger.to_lowercase();
    let context_lower = context.to_lowercase();
    let trigger_trimmed = trigger_lower.trim();
    if trigger_trimmed.is_empty() || context_lower.trim().is_empty() {
        return false;
    }
    if context_lower.contains(trigger_trimmed) {
        return true;
    }

    let tokens: Vec<&str> = trigger_trimmed
        .split_whitespace()
        .filter(|t| t.chars().count() >= 2)
        .collect();
    if tokens.is_empty() {
        return false;
    }
    let hits = tokens.iter().filter(|t| context_lower.contains(*t)).count();
    let required = tokens.len().div_ceil(2);
    hits >= required
}

/// Positive routing complement to [`should_exclude_for_intent`]: rather than
/// only penalizing the wrong memory type for a task, boost the type that
/// task usually needs — e.g. a coding task wants procedural `Skill`/`Fact`
/// memories ranked above transient `Episode` context. Returns `1.0` (no-op)
/// when the intent has no preferred type or the memory type isn't in its
/// preference list.
pub fn intent_type_boost(intent: &Intent, memory_type: &MemoryType) -> f64 {
    let preferred: &[MemoryType] = match intent {
        Intent::Coding => &[MemoryType::Skill, MemoryType::Fact],
        Intent::Writing => &[MemoryType::Preference],
        Intent::Design => &[MemoryType::Preference, MemoryType::Fact],
        Intent::Research => &[MemoryType::Fact, MemoryType::Entity],
        Intent::Project => &[MemoryType::Episode],
        Intent::General => &[],
    };
    if preferred.contains(memory_type) {
        1.3
    } else {
        1.0
    }
}

pub fn should_exclude_for_intent(
    intent: &Intent,
    memory_tags: &[String],
    exclude_types: &[String],
) -> bool {
    if exclude_types.is_empty() {
        return false;
    }

    let intent_domains: &[&str] = match intent {
        Intent::Coding => &["writing", "design"],
        Intent::Writing => &["coding"],
        Intent::Design => &["coding"],
        _ => &[],
    };

    for tag in memory_tags {
        let tag_lower = tag.to_lowercase();
        if intent_domains.iter().any(|d| tag_lower.contains(d)) {
            return true;
        }
        if exclude_types
            .iter()
            .any(|et| tag_lower.contains(&et.to_lowercase()))
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coding_intent() {
        let result = analyze_intent("帮我写一个 Python 函数来处理 API 请求");
        assert_eq!(result.primary, Intent::Coding);
        assert!(result.confidence > 0.0);
    }

    #[test]
    fn test_writing_intent() {
        let result = analyze_intent("帮我写一篇关于 AI 的博客文章");
        assert_eq!(result.primary, Intent::Writing);
    }

    #[test]
    fn test_general_intent() {
        let result = analyze_intent("今天天气怎么样");
        assert_eq!(result.primary, Intent::General);
    }

    #[test]
    fn test_exclude_for_intent() {
        assert!(should_exclude_for_intent(
            &Intent::Coding,
            &["writing".to_string(), "blog".to_string()],
            &["writing".to_string()],
        ));

        assert!(!should_exclude_for_intent(
            &Intent::Coding,
            &["python".to_string(), "coding".to_string()],
            &["writing".to_string()],
        ));
    }

    #[test]
    fn test_trigger_match_whole_containment() {
        assert!(trigger_matches_context(
            "deploy",
            "please deploy the dashboard"
        ));
        assert!(trigger_matches_context("Deploy", "DEPLOY it now"));
        assert!(!trigger_matches_context("deploy", "write a poem"));
    }

    #[test]
    fn test_trigger_match_token_overlap() {
        // Multi-word trigger: half-or-more tokens present.
        assert!(trigger_matches_context(
            "部署 静态 站点",
            "帮我部署站点到生产"
        ));
        // Only 1 of 3 tokens present — below the half threshold.
        assert!(!trigger_matches_context(
            "部署 静态 站点",
            "帮我写一个静态分析工具"
        ));
    }

    #[test]
    fn test_trigger_match_empty_inputs() {
        assert!(!trigger_matches_context("", "deploy"));
        assert!(!trigger_matches_context("   ", "deploy"));
        assert!(!trigger_matches_context("deploy", ""));
        assert!(!trigger_matches_context("deploy", "   "));
    }

    #[test]
    fn test_intent_type_boost_matches_preferred_type() {
        assert_eq!(intent_type_boost(&Intent::Coding, &MemoryType::Skill), 1.3);
        assert_eq!(intent_type_boost(&Intent::Coding, &MemoryType::Fact), 1.3);
        assert_eq!(
            intent_type_boost(&Intent::Writing, &MemoryType::Preference),
            1.3
        );
        assert_eq!(
            intent_type_boost(&Intent::Project, &MemoryType::Episode),
            1.3
        );
    }

    #[test]
    fn test_intent_type_boost_no_op_for_unrelated_type() {
        assert_eq!(
            intent_type_boost(&Intent::Coding, &MemoryType::Episode),
            1.0
        );
        assert_eq!(intent_type_boost(&Intent::General, &MemoryType::Skill), 1.0);
        assert_eq!(
            intent_type_boost(&Intent::Writing, &MemoryType::Entity),
            1.0
        );
    }

    #[test]
    fn test_intent_type_boost_design_and_research_branches() {
        assert_eq!(
            intent_type_boost(&Intent::Design, &MemoryType::Preference),
            1.3
        );
        assert_eq!(intent_type_boost(&Intent::Design, &MemoryType::Fact), 1.3);
        assert_eq!(intent_type_boost(&Intent::Design, &MemoryType::Skill), 1.0);
        assert_eq!(intent_type_boost(&Intent::Research, &MemoryType::Fact), 1.3);
        assert_eq!(
            intent_type_boost(&Intent::Research, &MemoryType::Entity),
            1.3
        );
        assert_eq!(
            intent_type_boost(&Intent::Research, &MemoryType::Episode),
            1.0
        );
    }
    #[test]
    fn test_trigger_match_single_token_no_spurious_overlap() {
        // Single-token trigger: containment only, no fuzzy token math.
        assert!(!trigger_matches_context("deploy", "dep"));
        assert!(trigger_matches_context("部署", "现在部署"));
    }
}
