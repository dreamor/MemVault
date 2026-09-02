//! Cold-start import: read another agent's native memory files (CLAUDE.md,
//! AGENTS.md, auto-memory frontmatter, ...) and normalize them into
//! [`Memory`] candidates so a first-time MemVault user does not start from
//! zero.
//!
//! Each supported agent is one [`AgentMemorySource`] implementation in its
//! own submodule; adding a new agent means adding a new file and registering
//! it in [`all_adapters`] — the rest of the pipeline (CLI, dedup, review
//! inbox) is shared.
//!
//! Detection is always read-only and never panics: an agent that is not
//! installed, or whose files are in an unexpected shape, is reported via
//! [`AgentMemorySource::detect`] returning `None` or via
//! [`ImportParseOutcome::files_skipped`] — never a hard error.

pub mod claude;
pub mod codex;
pub mod hermes;
pub mod openclaw;
pub mod qoder;

use std::path::{Path, PathBuf};

use crate::models::Memory;

/// One agent's memory-file adapter.
pub trait AgentMemorySource: Send + Sync {
    /// Short, stable identifier used on the CLI (`--agent claude`) and as
    /// part of the imported memory's `source_agent.id`.
    fn agent_key(&self) -> &'static str;

    /// Human-readable name for CLI output.
    fn display_name(&self) -> &'static str;

    /// Probe this agent's default locations under `home`/`cwd`, or check
    /// `override_path` if the caller supplied one. Returns `None` when
    /// nothing is found — that is a normal outcome, not an error.
    fn detect(
        &self,
        home: &Path,
        cwd: &Path,
        override_path: Option<&Path>,
    ) -> Option<DetectedSource>;

    /// Parse a previously detected source into normalized candidates.
    fn parse(&self, source: &DetectedSource) -> ImportParseOutcome;
}

/// Where this agent's memory files were found.
#[derive(Debug, Clone)]
pub struct DetectedSource {
    pub agent_key: &'static str,
    pub paths: Vec<PathBuf>,
}

/// How much to trust a parsed candidate before it reaches the review inbox.
/// Purely informational (surfaced in CLI/dry-run output) — every candidate
/// still lands at `Priority::Reference` / unreviewed regardless of tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseConfidence {
    /// Mapped directly from a structured field (e.g. YAML frontmatter).
    Structured,
    /// Derived by splitting free-form prose into sections.
    SectionSplit,
    /// Best-effort guess against an unconfirmed/unstable source format
    /// (e.g. OpenClaw's memory store, whose on-disk schema is not
    /// documented). Never assume this candidate's shape is reliable.
    Heuristic,
}

/// One normalized candidate, not yet saved.
#[derive(Debug, Clone)]
pub struct ImportCandidate {
    pub memory: Memory,
    /// Original source text this candidate was derived from, kept for
    /// audit / dry-run display.
    pub raw_excerpt: String,
    pub confidence: ParseConfidence,
}

/// Result of parsing one detected source.
#[derive(Debug, Clone, Default)]
pub struct ImportParseOutcome {
    pub candidates: Vec<ImportCandidate>,
    pub files_scanned: usize,
    /// Files that were looked at but produced nothing, with a human-readable
    /// reason — skipped files are always accounted for, never silently
    /// dropped.
    pub files_skipped: Vec<(PathBuf, String)>,
}

/// All built-in adapters. This is the one place to register a new agent.
pub fn all_adapters() -> Vec<Box<dyn AgentMemorySource>> {
    vec![
        Box::new(claude::ClaudeSource),
        Box::new(codex::CodexSource),
        Box::new(hermes::HermesSource),
        Box::new(qoder::QoderSource),
        Box::new(openclaw::OpenClawSource),
    ]
}

/// Split a free-form Markdown document into `(title, body)` sections on
/// top-level (`#`/`##`) headings. A document with no headings becomes one
/// section with `title: None`. Content inside fenced code blocks is never
/// mistaken for a heading. Sections with an empty body are dropped.
pub(crate) fn split_by_heading(markdown: &str) -> Vec<(Option<String>, String)> {
    let mut sections = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();
    let mut in_code_fence = false;
    let mut seen_any_heading = false;

    let flush =
        |title: Option<String>, body: String, sections: &mut Vec<(Option<String>, String)>| {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                sections.push((title, trimmed.to_string()));
            }
        };

    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_code_fence = !in_code_fence;
            current_body.push_str(line);
            current_body.push('\n');
            continue;
        }
        if in_code_fence {
            current_body.push_str(line);
            current_body.push('\n');
            continue;
        }

        if let Some(title) = heading_title(line) {
            flush(
                current_title.take(),
                std::mem::take(&mut current_body),
                &mut sections,
            );
            current_title = Some(title.to_string());
            seen_any_heading = true;
            continue;
        }

        current_body.push_str(line);
        current_body.push('\n');
    }
    flush(current_title, current_body, &mut sections);

    // A single heading-less document still counts as one section.
    if sections.is_empty() && !seen_any_heading && !markdown.trim().is_empty() {
        sections.push((None, markdown.trim().to_string()));
    }
    sections
}

/// Top-level (`#`/`##`) heading text, or `None` for deeper headings
/// (`###`+, which are sub-structure, not section boundaries) or non-headings.
fn heading_title(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("##")
        .or_else(|| trimmed.strip_prefix('#'))?;
    if rest.starts_with('#') {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() { None } else { Some(title) }
}

/// Infer a memory namespace from where a rules-style file lives: a file that
/// sits directly inside the agent's home config directory (parent dir name
/// equals `home_marker`, e.g. `.claude`/`.codex`) is global; anything else is
/// assumed to be a project root and namespaced by its directory name.
pub(crate) fn infer_namespace_from_parent(path: &Path, home_marker: &str) -> String {
    match path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        Some(name) if name == home_marker => "global".to_string(),
        Some(name) if !name.is_empty() && name != "." => format!("project:{name}"),
        _ => "global".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_by_heading_basic() {
        let doc = "# One\nbody one\n\n## Two\nbody two\n";
        let sections = split_by_heading(doc);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0.as_deref(), Some("One"));
        assert_eq!(sections[0].1, "body one");
        assert_eq!(sections[1].0.as_deref(), Some("Two"));
        assert_eq!(sections[1].1, "body two");
    }

    #[test]
    fn test_split_by_heading_no_headings() {
        let doc = "just some prose\nacross two lines\n";
        let sections = split_by_heading(doc);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, None);
    }

    #[test]
    fn test_split_by_heading_ignores_code_fence_hashes() {
        let doc = "# Real Heading\n```\n# not a heading\n```\nbody\n";
        let sections = split_by_heading(doc);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0.as_deref(), Some("Real Heading"));
        assert!(sections[0].1.contains("# not a heading"));
    }

    #[test]
    fn test_split_by_heading_drops_empty_sections() {
        let doc = "# Empty\n\n# Real\nsomething\n";
        let sections = split_by_heading(doc);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0.as_deref(), Some("Real"));
    }

    #[test]
    fn test_split_by_heading_empty_input() {
        assert!(split_by_heading("").is_empty());
        assert!(split_by_heading("   \n  \n").is_empty());
    }
}
