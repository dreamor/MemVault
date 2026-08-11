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
}
