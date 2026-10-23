//! End-to-end accuracy eval harness for the rule-based extractor
//! (`Extractor::extract_guarded`).
//!
//! Fixture-driven: each case states what the extractor SHOULD do, not just
//! what it currently does, so a future change to `extractor.rs` that
//! reopens a previously-fixed false positive/negative (or a source-role
//! leak) shows up as a failing test here instead of shipping silently.
//! Categories mirror the failure modes found in production audits: explicit
//! preference, implicit imperative, quoted/cited material, explicit
//! negation, chit-chat, assistant-authored paraphrase, correction, and
//! unconfirmed provenance.
//!
//! This only covers the extractor itself, not the full retrieval/injection
//! path (`router::format`) — that would need a live `MemoryStore` and is a
//! reasonable next step if this harness proves useful, not included here.

use crate::extractor::{Extractor, SourceRole};

/// What a fixture case expects `extract_guarded` to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOutcome {
    /// Genuine user signal — must produce at least one memory.
    Extracted,
    /// Must NOT produce any memory: chit-chat, a question, quoted/cited
    /// material, an explicit negation, or agent-authored text that the
    /// source-role guard refuses.
    Rejected,
}

#[derive(Debug, Clone, Copy)]
pub struct EvalCase {
    pub label: &'static str,
    pub input: &'static str,
    pub role: SourceRole,
    pub expected: ExpectedOutcome,
}

pub const FIXTURES: &[EvalCase] = &[
    // --- explicit preference: must extract ---
    EvalCase {
        label: "explicit_preference_en",
        input: "I prefer concise Chinese answers",
        role: SourceRole::User,
        expected: ExpectedOutcome::Extracted,
    },
    EvalCase {
        label: "explicit_preference_zh",
        input: "我偏好使用 Rust 编程",
        role: SourceRole::User,
        expected: ExpectedOutcome::Extracted,
    },
    // --- implicit future-tense imperative: must extract (previously a
    //     false negative — no signal in the preference list matched it) ---
    EvalCase {
        label: "implicit_imperative_zh",
        input: "以后请用中文简洁回答我",
        role: SourceRole::User,
        expected: ExpectedOutcome::Extracted,
    },
    // --- quoted/cited material: must NOT extract even though it contains
    //     the same keyword as a real preference ---
    EvalCase {
        label: "citation_zh",
        input: "引用:我喜欢深色模式(摘录自某篇博客)",
        role: SourceRole::User,
        expected: ExpectedOutcome::Rejected,
    },
    // --- explicit negation: must NOT extract (previously a false
    //     positive, observed in production at 75% confidence) ---
    EvalCase {
        label: "negation_zh",
        input: "这不是我的偏好,只是文档中的一句引用",
        role: SourceRole::User,
        expected: ExpectedOutcome::Rejected,
    },
    // --- chit-chat / questions: must NOT extract ---
    EvalCase {
        label: "chitchat_en",
        input: "The weather is fine today.",
        role: SourceRole::User,
        expected: ExpectedOutcome::Rejected,
    },
    EvalCase {
        label: "chitchat_question_zh",
        input: "你喜欢 Rust 吗?",
        role: SourceRole::User,
        expected: ExpectedOutcome::Rejected,
    },
    // --- assistant-authored text, even phrased as a first-person
    //     preference: must NOT extract (source-role boundary) ---
    EvalCase {
        label: "assistant_paraphrase_zh",
        input: "我喜欢深色模式",
        role: SourceRole::Agent,
        expected: ExpectedOutcome::Rejected,
    },
    // --- correction mid-conversation: the corrective statement is still a
    //     real user preference and must extract ---
    EvalCase {
        label: "correction_zh",
        input: "以后请不要用 sudo,直接修权限",
        role: SourceRole::User,
        expected: ExpectedOutcome::Extracted,
    },
    // --- unconfirmed provenance: still extracted, but downgraded by the
    //     guard rather than trusted outright (see `downgraded_fixtures`) ---
    EvalCase {
        label: "unknown_provenance_preference_zh",
        input: "我偏好使用 Python 编程",
        role: SourceRole::Unknown,
        expected: ExpectedOutcome::Extracted,
    },
];

/// Result of running [`FIXTURES`] through `Extractor::extract_guarded`.
#[derive(Debug, Clone, Default)]
pub struct EvalReport {
    pub total: usize,
    /// Expected [`ExpectedOutcome::Rejected`] but something was extracted.
    pub false_positives: Vec<&'static str>,
    /// Expected [`ExpectedOutcome::Extracted`] but nothing came out.
    pub false_negatives: Vec<&'static str>,
    /// An [`SourceRole::Agent`] case whose content was not refused — the
    /// one failure mode that is a source-attribution bug, not just a
    /// precision/recall miss.
    pub source_attribution_failures: Vec<&'static str>,
}

impl EvalReport {
    /// Fraction of `Rejected`-expected cases that leaked a false positive.
    pub fn false_store_rate(&self) -> f64 {
        let rejected_cases = FIXTURES
            .iter()
            .filter(|c| c.expected == ExpectedOutcome::Rejected)
            .count();
        if rejected_cases == 0 {
            return 0.0;
        }
        self.false_positives.len() as f64 / rejected_cases as f64
    }

    /// Fraction of `Extracted`-expected cases that were missed.
    pub fn false_negative_rate(&self) -> f64 {
        let extracted_cases = FIXTURES
            .iter()
            .filter(|c| c.expected == ExpectedOutcome::Extracted)
            .count();
        if extracted_cases == 0 {
            return 0.0;
        }
        self.false_negatives.len() as f64 / extracted_cases as f64
    }

    pub fn is_clean(&self) -> bool {
        self.false_positives.is_empty()
            && self.false_negatives.is_empty()
            && self.source_attribution_failures.is_empty()
    }
}

/// Run every fixture through `Extractor::extract_guarded` and score it
/// against its expected outcome.
pub fn run_eval() -> EvalReport {
    let mut report = EvalReport {
        total: FIXTURES.len(),
        ..Default::default()
    };
    for case in FIXTURES {
        let guarded = Extractor::extract_guarded(case.input, case.role);
        let extracted_something = !guarded.outcome.memories.is_empty();
        match case.expected {
            ExpectedOutcome::Extracted if !extracted_something => {
                report.false_negatives.push(case.label);
            }
            ExpectedOutcome::Rejected if extracted_something => {
                report.false_positives.push(case.label);
            }
            _ => {}
        }
        if matches!(case.role, SourceRole::Agent) && extracted_something {
            report.source_attribution_failures.push(case.label);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_fixtures_are_clean() {
        let report = run_eval();
        assert!(
            report.false_positives.is_empty(),
            "false positives (extracted but should not have): {:?}",
            report.false_positives
        );
        assert!(
            report.false_negatives.is_empty(),
            "false negatives (missed but should have extracted): {:?}",
            report.false_negatives
        );
        assert!(
            report.source_attribution_failures.is_empty(),
            "agent-authored content was not refused: {:?}",
            report.source_attribution_failures
        );
        assert_eq!(report.false_store_rate(), 0.0);
        assert_eq!(report.false_negative_rate(), 0.0);
        assert!(report.is_clean());
    }

    #[test]
    fn unknown_provenance_preference_is_downgraded_not_trusted() {
        let case = FIXTURES
            .iter()
            .find(|c| c.label == "unknown_provenance_preference_zh")
            .expect("fixture present");
        let guarded = Extractor::extract_guarded(case.input, case.role);
        assert!(guarded.downgraded, "unknown provenance must be downgraded");
        assert!(
            guarded
                .outcome
                .memories
                .iter()
                .all(|m| m.tags.iter().any(|t| t == "review:required")),
            "downgraded memories must be tagged review:required"
        );
    }
}
