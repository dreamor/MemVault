use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::models::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub content: String,
    pub instruction: Option<String>,
    pub memory_type: MemoryType,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub confidence: f64,
}

pub struct Extractor;

impl Extractor {
    /// Extract structured memories from a conversation turn.
    /// Uses rule-based pattern matching (no LLM dependency).
    pub fn extract(text: &str) -> Vec<ExtractedMemory> {
        let mut results = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(mem) = Self::extract_preference(trimmed) {
                results.push(mem);
            } else if let Some(mem) = Self::extract_fact(trimmed) {
                results.push(mem);
            } else if let Some(mem) = Self::extract_skill(trimmed) {
                results.push(mem);
            }
        }

        debug!(extracted = results.len(), "extractor complete");
        results
    }

    fn extract_preference(text: &str) -> Option<ExtractedMemory> {
        let lower = text.to_lowercase();

        let preference_signals = [
            "i prefer",
            "i like",
            "i want",
            "i always",
            "i never",
            "i don't like",
            "please always",
            "please never",
            "please don't",
            "don't ever",
            "我喜欢",
            "我偏好",
            "我习惯",
            "我不喜欢",
            "我不要",
            "我希望",
            "请总是",
            "请不要",
            "请永远",
            "以后都",
        ];

        if !preference_signals.iter().any(|s| lower.contains(s)) {
            return None;
        }

        let must_signals = [
            "always",
            "never",
            "must",
            "don't ever",
            "请总是",
            "请永远",
            "请不要",
            "必须",
        ];
        let priority = if must_signals.iter().any(|s| lower.contains(s)) {
            Priority::Must
        } else {
            Priority::Reference
        };

        let tags = Self::infer_tags(&lower);

        Some(ExtractedMemory {
            content: text.to_string(),
            instruction: Some(Self::to_instruction(text)),
            memory_type: MemoryType::Preference,
            priority,
            tags,
            confidence: 0.75,
        })
    }

    fn extract_fact(text: &str) -> Option<ExtractedMemory> {
        let lower = text.to_lowercase();

        let fact_signals = [
            "i am a",
            "i'm a",
            "i work",
            "my name is",
            "i use",
            "we use",
            "our project",
            "our team",
            "the project",
            "tech stack",
            "我是",
            "我在",
            "我们用",
            "我们的项目",
            "项目使用",
            "技术栈",
            "目前在做",
            "正在开发",
        ];

        if !fact_signals.iter().any(|s| lower.contains(s)) {
            return None;
        }

        let tags = Self::infer_tags(&lower);

        Some(ExtractedMemory {
            content: text.to_string(),
            instruction: None,
            memory_type: MemoryType::Fact,
            priority: Priority::Reference,
            tags,
            confidence: 0.7,
        })
    }

    fn extract_skill(text: &str) -> Option<ExtractedMemory> {
        let lower = text.to_lowercase();

        let skill_signals = [
            "to deploy",
            "to build",
            "the process",
            "the workflow",
            "steps to",
            "how to",
            "run the",
            "the command",
            "部署流程",
            "构建步骤",
            "操作步骤",
            "使用方法",
            "运行命令",
        ];

        if !skill_signals.iter().any(|s| lower.contains(s)) {
            return None;
        }

        let tags = Self::infer_tags(&lower);

        Some(ExtractedMemory {
            content: text.to_string(),
            instruction: None,
            memory_type: MemoryType::Skill,
            priority: Priority::Reference,
            tags,
            confidence: 0.65,
        })
    }

    fn to_instruction(text: &str) -> String {
        const PREFIXES: &[(&str, &str)] = &[
            ("i prefer ", ""),
            ("i like ", ""),
            ("i always ", "Always "),
            ("i never ", "Never "),
            ("please always ", "Always "),
            ("please never ", "Never "),
            ("我喜欢", ""),
            ("我偏好", ""),
            ("请总是", "总是"),
            ("请不要", "不要"),
        ];
        for (prefix, replacement) in PREFIXES {
            if let Some(rest) = Self::strip_prefix_ci(text, prefix) {
                return format!("{}{}", replacement, rest.trim()).trim().to_string();
            }
        }
        text.trim().to_string()
    }

    /// Case-insensitive prefix strip that is safe on UTF-8 boundaries.
    fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
        let head = s.get(..prefix.len())?;
        if head.eq_ignore_ascii_case(prefix) {
            s.get(prefix.len()..)
        } else {
            None
        }
    }

    fn infer_tags(lower: &str) -> Vec<String> {
        let mut tags = Vec::new();

        let tag_map: &[(&[&str], &str)] = &[
            (
                &[
                    "python",
                    "rust",
                    "javascript",
                    "typescript",
                    "java",
                    "go",
                    "code",
                    "function",
                    "api",
                    "database",
                    "sql",
                    "git",
                    "deploy",
                    "编程",
                    "代码",
                    "函数",
                ],
                "coding",
            ),
            (
                &[
                    "write", "article", "blog", "document", "文章", "写作", "文档",
                ],
                "writing",
            ),
            (
                &["design", "ui", "ux", "layout", "css", "设计", "界面"],
                "design",
            ),
            (
                &["project", "team", "sprint", "deadline", "项目", "团队"],
                "project",
            ),
            (
                &["style", "format", "convention", "风格", "格式", "规范"],
                "style",
            ),
        ];

        for (keywords, tag) in tag_map {
            if keywords.iter().any(|kw| lower.contains(kw)) {
                tags.push(tag.to_string());
            }
        }

        if tags.is_empty() {
            tags.push("general".to_string());
        }

        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preference() {
        let results = Extractor::extract("I prefer Python over Java for backend work");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Preference);
        assert!(results[0].tags.contains(&"coding".to_string()));
    }

    #[test]
    fn test_extract_must_preference() {
        let results = Extractor::extract("Please never add comments to code");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].priority, Priority::Must);
    }

    #[test]
    fn test_extract_fact() {
        let results =
            Extractor::extract("I'm a senior Rust developer working on distributed systems");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Fact);
    }

    #[test]
    fn test_extract_skill() {
        let results = Extractor::extract("The command to deploy is: cargo build --release && scp");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Skill);
    }

    #[test]
    fn test_extract_chinese() {
        let results = Extractor::extract("我偏好使用 Python 编程");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Preference);
    }

    #[test]
    fn test_no_extraction() {
        let results = Extractor::extract("What's the weather today?");
        assert!(results.is_empty());
    }

    #[test]
    fn test_multi_line() {
        let text = "I prefer dark mode for all editors\nOur project uses FastAPI and PostgreSQL";
        let results = Extractor::extract(text);
        assert_eq!(results.len(), 2);
    }

    /// Regression: the old prefix strip used case-sensitive `replace`, so
    /// lowercase input ("i always …", "i prefer …") kept its prefix verbatim.
    #[test]
    fn test_to_instruction_strips_prefix_case_insensitively() {
        for (input, expected) in [
            ("I prefer vim", "vim"),
            ("i prefer vim", "vim"),
            ("i always write tests", "Always write tests"),
            ("I NEVER debug on weekends", "Never debug on weekends"),
            ("Please always review the PRs", "Always review the PRs"),
            ("please never use tabs", "Never use tabs"),
        ] {
            assert_eq!(Extractor::to_instruction(input), expected, "input: {input}");
        }
    }

    /// No recognised prefix → text is passed through cleanly.
    #[test]
    fn test_to_instruction_passthrough_when_no_prefix() {
        assert_eq!(
            Extractor::to_instruction("use tabs for indentation"),
            "use tabs for indentation"
        );
        assert_eq!(Extractor::to_instruction(""), "");
    }

    /// CJK prefixes still strip on an exact (equal) match.
    #[test]
    fn test_to_instruction_strips_cjk_prefix() {
        assert_eq!(Extractor::to_instruction("我喜欢咖啡"), "咖啡");
        assert_eq!(Extractor::to_instruction("我偏好简洁风格"), "简洁风格");
    }
}
