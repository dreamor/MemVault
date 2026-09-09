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

/// Coverage accounting for one extraction run.
///
/// Batch operations must report HOW MUCH of the input they covered, not just
/// success/failure: an extraction that silently processed half the text is
/// indistinguishable from a complete one without this. Counts are mutually
/// exclusive and sum to `input_lines`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionCoverage {
    pub input_lines: usize,
    pub empty_lines: usize,
    pub extracted_lines: usize,
    pub no_signal_lines: usize,
}

/// Extraction result plus its coverage report.
#[derive(Debug, Clone)]
pub struct ExtractionOutcome {
    pub memories: Vec<ExtractedMemory>,
    pub coverage: ExtractionCoverage,
}

/// Who produced the text being extracted.
///
/// Agent-produced text must not flow into memory unchecked: after a few
/// dedup/decay cycles the agent's own phrasing (and its mistakes) comes to
/// dominate the store — self-reinforcing drift, gradual, and it never looks
/// like an error at any single moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    /// The user's own words — the primary memory material.
    #[default]
    User,
    /// Output produced by an agent itself.
    Agent,
    /// User and agent turns mixed together, unlabeled.
    Mixed,
    /// Provenance unknown.
    Unknown,
}

/// Why a guarded extraction was rejected outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionRejectReason {
    /// The text is an agent's own output; feeding it back into memory
    /// causes self-reinforcing drift and is therefore refused.
    SelfGenerated,
    /// The text itself contains a credential-shaped pattern (see
    /// [`crate::sensitive`]) — content-based, independent of who said it.
    SensitiveContent(crate::sensitive::SensitiveKind),
}

/// Result of [`Extractor::extract_guarded`].
#[derive(Debug, Clone)]
pub struct GuardedExtraction {
    pub outcome: ExtractionOutcome,
    /// Set when the whole input was rejected (`memories` is then empty).
    pub rejection: Option<ExtractionRejectReason>,
    /// True when results were downgraded (tagged `review:required`, lowered
    /// confidence) because provenance was not confirmed to be the user.
    pub downgraded: bool,
}

pub struct Extractor;

impl Extractor {
    /// Extract structured memories from a conversation turn.
    /// Uses rule-based pattern matching (no LLM dependency).
    pub fn extract(text: &str) -> Vec<ExtractedMemory> {
        Self::extract_with_coverage(text).memories
    }

    /// Like [`extract`], but also reporting how much of the input was
    /// covered — callers showing progress to users must say how many lines
    /// produced nothing and why, not just what was found.
    pub fn extract_with_coverage(text: &str) -> ExtractionOutcome {
        let mut memories = Vec::new();
        let mut coverage = ExtractionCoverage::default();

        for line in text.lines() {
            coverage.input_lines += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                coverage.empty_lines += 1;
                continue;
            }

            // Questions and assistant acknowledgment echoes carry no memory
            // content (see `is_interrogative` / `is_echo_ack`); classifying
            // them as no-signal keeps the coverage partition exact.
            if Self::is_interrogative(trimmed) || Self::is_echo_ack(trimmed) {
                coverage.no_signal_lines += 1;
                continue;
            }

            if let Some(mem) = Self::extract_preference(trimmed) {
                memories.push(mem);
                coverage.extracted_lines += 1;
            } else if let Some(mem) = Self::extract_fact(trimmed) {
                memories.push(mem);
                coverage.extracted_lines += 1;
            } else if let Some(mem) = Self::extract_skill(trimmed) {
                memories.push(mem);
                coverage.extracted_lines += 1;
            } else {
                coverage.no_signal_lines += 1;
            }
        }

        debug!(
            extracted = memories.len(),
            no_signal = coverage.no_signal_lines,
            "extractor complete"
        );
        ExtractionOutcome { memories, coverage }
    }

    /// Ends with a terminal question mark. A question asks, it does not
    /// assert — extracting a question as a fact has been a real
    /// false-positive source (the substring "你是" matches the
    /// "你是否" in "你检查一下你是否正常注册了 memvault 插件了？",
    /// which was stored as a fact).
    fn is_interrogative(text: &str) -> bool {
        let trimmed = text.trim();
        trimmed.ends_with('?') || trimmed.ends_with('\u{ff1f}')
    }

    /// Assistant acknowledgment echoes: lines that merely confirm a user's
    /// statement ("好的，我记住了：你偏好……", "收到，用户偏好……") add no
    /// durable information of their own — the underlying statement is already
    /// captured (tagged `source:user`) from the user's own turn, so saving the
    /// echo duplicates it (observed in the wild as `mem_270a` / `mem_15b6`).
    /// Guarded to ack-prefix + mirror-signal so a bare "好的" alone does not
    /// suppress genuinely new content on the same line.
    fn is_echo_ack(text: &str) -> bool {
        let lower = text.to_lowercase();
        const ACK_PREFIXES: &[&str] = &[
            "好的，我记住了",
            "好的，我记下了",
            "好的，明白了",
            "好的，没问题",
            "我记住了",
            "我记下了",
            "收到了",
            "收到，",
            "明白了，",
            "好的，",
            "ok, i'll remember",
            "ok i'll remember",
            "got it",
            "no problem",
        ];
        if !ACK_PREFIXES.iter().any(|p| lower.starts_with(p)) {
            return false;
        }
        const MIRROR_SIGNALS: &[&str] = &[
            "你偏好",
            "你喜欢",
            "你习惯",
            "你希望",
            "你觉得",
            "你使用",
            "用户偏好",
            "用户喜欢",
            "用户习惯",
            "用户希望",
            "用户使用",
        ];
        lower.contains("记住了") || MIRROR_SIGNALS.iter().any(|s| lower.contains(s))
    }

    /// Extraction with a source-role guard.
    ///
    /// - [`SourceRole::User`] — normal extraction.
    /// - [`SourceRole::Agent`] — refused outright: an agent's own output must
    ///   not become memory without a human in the loop (self-reinforcing
    ///   drift). Coverage is still reported so callers see the input was
    ///   processed, not lost.
    /// - [`SourceRole::Mixed`] / [`SourceRole::Unknown`] — extracted, but
    ///   every memory is tagged `review:required` and its confidence lowered:
    ///   "provenance not confirmed" is not the same as "provenance is the
    ///   user", and treating it as such would silently launder agent text.
    pub fn extract_guarded(text: &str, role: SourceRole) -> GuardedExtraction {
        // Content-based guard runs first and independent of provenance —
        // a credential is refused whether the user or the agent typed it.
        if let Some(m) = crate::sensitive::scan(text).into_iter().next() {
            let mut outcome = Self::extract_with_coverage(text);
            let rejected_hits = outcome.coverage.extracted_lines;
            outcome.memories.clear();
            outcome.coverage.extracted_lines = 0;
            outcome.coverage.no_signal_lines += rejected_hits;
            debug!(kind = ?m.kind, "sensitive content refused by extraction guard");
            return GuardedExtraction {
                outcome,
                rejection: Some(ExtractionRejectReason::SensitiveContent(m.kind)),
                downgraded: false,
            };
        }
        match role {
            SourceRole::Agent => {
                let mut outcome = Self::extract_with_coverage(text);
                let rejected_hits = outcome.coverage.extracted_lines;
                outcome.memories.clear();
                // The lines that WOULD have extracted are reclassified: from
                // the store's point of view they produced nothing.
                outcome.coverage.extracted_lines = 0;
                outcome.coverage.no_signal_lines += rejected_hits;
                debug!(
                    rejected_lines = rejected_hits,
                    "agent-produced text refused by extraction guard"
                );
                GuardedExtraction {
                    outcome,
                    rejection: Some(ExtractionRejectReason::SelfGenerated),
                    downgraded: false,
                }
            }
            SourceRole::User => GuardedExtraction {
                outcome: Self::extract_with_coverage(text),
                rejection: None,
                downgraded: false,
            },
            SourceRole::Mixed | SourceRole::Unknown => {
                let mut outcome = Self::extract_with_coverage(text);
                for m in &mut outcome.memories {
                    if !m.tags.iter().any(|t| t == "review:required") {
                        m.tags.push("review:required".to_string());
                    }
                    m.confidence *= 0.9;
                }
                GuardedExtraction {
                    outcome,
                    rejection: None,
                    downgraded: true,
                }
            }
        }
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
            // Second/third-person mirrors: an assistant restating a user's
            // stated preference, or a caller passing the user's own turn
            // text separately, typically phrases it this way rather than in
            // first person.
            "你喜欢",
            "你偏好",
            "你习惯",
            "你不喜欢",
            "你不要",
            "你希望",
            "用户喜欢",
            "用户偏好",
            "用户习惯",
            "用户不喜欢",
            "用户希望",
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

        // A "你是否/你是不是" construction is interrogative or hypothetical —
        // "你是" substring matching must not claim it as a stated fact.
        if lower.contains("你是否") || lower.contains("你是不是") {
            return None;
        }

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
            // Second/third-person mirrors, same rationale as extract_preference.
            "你是",
            "你在",
            "你的项目",
            "你们的项目",
            "你用",
            "你使用",
            "用户是",
            "用户在",
            "用户的项目",
            "用户使用",
            "用户的技术栈",
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
            ("你喜欢", ""),
            ("你偏好", ""),
            ("用户喜欢", ""),
            ("用户偏好", ""),
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

    #[test]
    fn test_extract_skips_interrogative_lines() {
        for text in [
            "你检查一下你是否正常注册了 memvault 插件了？",
            "你检查一下你是否正常注册了 memvault 插件了",
            "你喜欢 Rust 吗？",
            "What is the weather today?",
        ] {
            assert!(
                Extractor::extract(text).is_empty(),
                "extracted from: {text}"
            );
        }
    }

    #[test]
    fn test_extract_skips_echo_acknowledgments() {
        for text in [
            "好的，我记住了：你偏好使用 Rust 而不是 Go 来写后端服务。",
            "收到，用户偏好深色主题的编辑器。",
            "好的，我记下了：你习惯用 4 个空格缩进。",
        ] {
            assert!(
                Extractor::extract(text).is_empty(),
                "extracted from: {text}"
            );
        }
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

    /// An assistant restating a user's preference in second person should be
    /// just as extractable as a first-person statement.
    #[test]
    fn test_extract_preference_second_person() {
        let results = Extractor::extract("你偏好使用 Rust 而不是 Go");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Preference);
    }

    /// Third-person "用户……" restatement (matches how existing seed memories
    /// in this repo are phrased) should also be extractable.
    #[test]
    fn test_extract_preference_user_prefix() {
        let results = Extractor::extract("用户喜欢在周末写技术博客");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Preference);
    }

    #[test]
    fn test_extract_fact_second_person() {
        let results = Extractor::extract("你们的项目使用 Kubernetes 部署");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Fact);
    }

    #[test]
    fn test_to_instruction_strips_second_person_prefix() {
        assert_eq!(Extractor::to_instruction("你偏好 vim"), "vim");
        assert_eq!(Extractor::to_instruction("用户喜欢深色主题"), "深色主题");
    }

    /// Coverage must partition the input exactly: every line lands in exactly
    /// one bucket, so a half-processed input can never look like a full run.
    #[test]
    fn test_extract_with_coverage_counts_partition_input() {
        let text = "I prefer dark mode\n\njust a plain sentence\nI am a backend engineer\n   \n";
        let outcome = Extractor::extract_with_coverage(text);
        let cov = &outcome.coverage;

        assert_eq!(cov.input_lines, 5);
        assert_eq!(cov.empty_lines, 2);
        assert_eq!(cov.extracted_lines, 2);
        assert_eq!(cov.no_signal_lines, 1);
        assert_eq!(
            cov.input_lines,
            cov.empty_lines + cov.extracted_lines + cov.no_signal_lines
        );
        assert_eq!(outcome.memories.len(), cov.extracted_lines);
    }

    /// `extract()` stays a thin wrapper over the coverage-aware path.
    #[test]
    fn test_extract_matches_outcome_memories() {
        let text = "我喜欢简洁的注释\nnothing to see here";
        assert_eq!(
            Extractor::extract(text).len(),
            Extractor::extract_with_coverage(text).memories.len()
        );
    }

    /// Agent-produced text is refused outright — the guard must not let an
    /// agent's own output become memory (self-reinforcing drift).
    #[test]
    fn test_extract_guarded_refuses_agent_text() {
        let text = "I prefer dark mode\nI am a backend engineer";
        let guarded = Extractor::extract_guarded(text, SourceRole::Agent);

        assert_eq!(
            guarded.rejection,
            Some(ExtractionRejectReason::SelfGenerated)
        );
        assert!(guarded.outcome.memories.is_empty());
        // Coverage still accounts for every line: refused, not lost.
        let cov = &guarded.outcome.coverage;
        assert_eq!(cov.input_lines, 2);
        assert_eq!(cov.extracted_lines, 0);
        assert_eq!(cov.no_signal_lines, 2);
    }

    /// Unconfirmed provenance (mixed/unknown) extracts, but every memory is
    /// marked for review and downgraded — "not confirmed user" must never be
    /// treated as "user".
    #[test]
    fn test_extract_guarded_downgrades_unknown_provenance() {
        let text = "I prefer dark mode";
        for role in [SourceRole::Mixed, SourceRole::Unknown] {
            let guarded = Extractor::extract_guarded(text, role);
            assert!(guarded.rejection.is_none());
            assert!(guarded.downgraded);
            assert_eq!(guarded.outcome.memories.len(), 1);
            let mem = &guarded.outcome.memories[0];
            assert!(mem.tags.contains(&"review:required".to_string()));
            assert!(
                mem.confidence < 0.75,
                "downgraded confidence must be below the base 0.75"
            );
        }
    }

    /// User text extracts unchanged: no rejection, no downgrade.
    #[test]
    fn test_extract_guarded_user_text_is_trusted() {
        let text = "I prefer dark mode";
        let guarded = Extractor::extract_guarded(text, SourceRole::User);
        assert!(guarded.rejection.is_none());
        assert!(!guarded.downgraded);
        assert_eq!(guarded.outcome.memories.len(), 1);
        let mem = &guarded.outcome.memories[0];
        assert!(!mem.tags.contains(&"review:required".to_string()));
        assert!((mem.confidence - 0.75).abs() < 1e-9);
    }
}
