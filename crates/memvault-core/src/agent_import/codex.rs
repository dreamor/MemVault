//! OpenAI Codex CLI memory adapter.
//!
//! Codex has no auto-memory database of its own — the whole "memory" surface
//! is `AGENTS.md`, an open Markdown standard (no required fields) read at
//! two layers: global (`~/.codex/AGENTS.md`, personal defaults) and project
//! root (`./AGENTS.md`, team conventions). Module-level `AGENTS.md` files
//! nested under subdirectories are a future enhancement, not covered here.

use std::path::Path;

use super::{
    AgentMemorySource, DetectedSource, ImportCandidate, ImportParseOutcome, ParseConfidence,
    infer_namespace_from_parent, split_by_heading,
};
use crate::models::{Memory, MemoryType, Priority, SourceAgent};

pub struct CodexSource;

impl AgentMemorySource for CodexSource {
    fn agent_key(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "OpenAI Codex CLI"
    }

    fn detect(
        &self,
        home: &Path,
        cwd: &Path,
        override_path: Option<&Path>,
    ) -> Option<DetectedSource> {
        if let Some(p) = override_path {
            return probe_override(p);
        }

        let mut paths = Vec::new();

        let global_agents_md = home.join(".codex").join("AGENTS.md");
        if global_agents_md.is_file() {
            paths.push(global_agents_md);
        }

        let project_agents_md = cwd.join("AGENTS.md");
        if project_agents_md.is_file() {
            paths.push(project_agents_md);
        }

        if paths.is_empty() {
            None
        } else {
            Some(DetectedSource {
                agent_key: "codex",
                paths,
            })
        }
    }

    fn parse(&self, source: &DetectedSource) -> ImportParseOutcome {
        let mut outcome = ImportParseOutcome::default();

        for path in &source.paths {
            outcome.files_scanned += 1;
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    outcome
                        .files_skipped
                        .push((path.clone(), format!("could not read file: {e}")));
                    continue;
                }
            };

            let sections = split_by_heading(&content);
            if sections.is_empty() {
                outcome
                    .files_skipped
                    .push((path.clone(), "empty file".to_string()));
                continue;
            }

            let namespace = infer_namespace_from_parent(path, ".codex");
            for (title, body) in sections {
                let text = match &title {
                    Some(t) => format!("{t}\n\n{body}"),
                    None => body.clone(),
                };
                let mut mem = Memory::new(
                    MemoryType::Fact,
                    text,
                    Priority::Reference,
                    SourceAgent {
                        id: "import-codex".to_string(),
                        agent_type: "imported-from-codex".to_string(),
                        session_id: None,
                    },
                );
                mem.namespace = namespace.clone();
                mem.tags = vec!["imported-agents-md".to_string()];
                mem.confidence = 0.7;
                mem.ai_generated = false;
                outcome.candidates.push(ImportCandidate {
                    memory: mem,
                    raw_excerpt: body,
                    confidence: ParseConfidence::SectionSplit,
                });
            }
        }

        outcome
    }
}

/// A user-supplied override may be a single `AGENTS.md`-like file, or a
/// directory containing one (checked non-recursively).
fn probe_override(p: &Path) -> Option<DetectedSource> {
    if p.is_file() {
        return Some(DetectedSource {
            agent_key: "codex",
            paths: vec![p.to_path_buf()],
        });
    }
    if p.is_dir() {
        let candidate = p.join("AGENTS.md");
        if candidate.is_file() {
            return Some(DetectedSource {
                agent_key: "codex",
                paths: vec![candidate],
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_agent_import_codex_{name}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_finds_global_and_project_agents_md() {
        let home = tmp_dir("home");
        let cwd = tmp_dir("cwd");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex").join("AGENTS.md"),
            "# Personal\nuse tabs\n",
        )
        .unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "# Team\nuse rustfmt\n").unwrap();

        let source = CodexSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_returns_none_when_nothing_found() {
        let home = tmp_dir("home2");
        let cwd = tmp_dir("cwd2");
        let source = CodexSource;
        assert!(source.detect(&home, &cwd, None).is_none());
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_splits_sections_and_sets_namespace() {
        let home = tmp_dir("home3");
        let cwd = tmp_dir("cwd3");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex").join("AGENTS.md"),
            "# Style\nUse 4-space indent\n\n# Tests\nAlways add unit tests\n",
        )
        .unwrap();

        let source = CodexSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 2);
        for c in &outcome.candidates {
            assert_eq!(c.memory.namespace, "global");
            assert_eq!(c.memory.priority, Priority::Reference);
            assert_eq!(c.memory.memory_type, MemoryType::Fact);
            assert_eq!(c.confidence, ParseConfidence::SectionSplit);
        }

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_project_agents_md_gets_project_namespace() {
        let home = tmp_dir("home4");
        let cwd = tmp_dir("cwd4");
        std::fs::write(cwd.join("AGENTS.md"), "# Rules\nno unwrap()\n").unwrap();

        let source = CodexSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert!(
            outcome.candidates[0]
                .memory
                .namespace
                .starts_with("project:")
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_override_path_to_directory() {
        let dir = tmp_dir("override");
        std::fs::write(dir.join("AGENTS.md"), "# X\nsome rule\n").unwrap();

        let source = CodexSource;
        let detected = source
            .detect(
                &PathBuf::from("/nonexistent"),
                &PathBuf::from("/nonexistent"),
                Some(&dir),
            )
            .unwrap();
        assert_eq!(detected.paths, vec![dir.join("AGENTS.md")]);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_empty_file_is_skipped() {
        let home = tmp_dir("home5");
        let cwd = tmp_dir("cwd5");
        std::fs::write(cwd.join("AGENTS.md"), "   \n").unwrap();

        let source = CodexSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.files_skipped.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }
}
