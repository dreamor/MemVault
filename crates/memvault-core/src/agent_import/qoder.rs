//! Qoder (Alibaba Cloud AI IDE/CLI) memory adapter.
//!
//! Qoder's project-level rules (`.qoder/rules/**/*.md`, human-authored
//! coding conventions and known-bugs notes, Git-shareable, may be grouped
//! into subdirectories like `backend/`/`frontend/`) are the only reliably
//! importable surface. Qoder's own "Memory" feature — preferences and
//! project context the IDE learns automatically — lives inside the IDE's
//! internal database with no documented file path or export format, so it
//! is out of scope here: `detect()` never looks for it, and the CLI surfaces
//! this as a known limitation rather than silently missing it.

use std::path::{Path, PathBuf};

use super::{
    AgentMemorySource, DetectedSource, ImportCandidate, ImportParseOutcome, ParseConfidence,
    split_by_heading,
};
use crate::models::{Memory, MemoryType, Priority, SourceAgent};

pub struct QoderSource;

impl AgentMemorySource for QoderSource {
    fn agent_key(&self) -> &'static str {
        "qoder"
    }

    fn display_name(&self) -> &'static str {
        "Qoder (project rules only — IDE-internal Memory DB is not supported)"
    }

    fn detect(
        &self,
        _home: &Path,
        cwd: &Path,
        override_path: Option<&Path>,
    ) -> Option<DetectedSource> {
        if let Some(p) = override_path {
            return probe_override(p);
        }

        let rules_dir = cwd.join(".qoder").join("rules");
        let mut paths = collect_md_files_recursive(&rules_dir, 4);
        paths.sort();

        if paths.is_empty() {
            None
        } else {
            Some(DetectedSource {
                agent_key: "qoder",
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

            let namespace = project_namespace_from_qoder_path(path);
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
                        id: "import-qoder".to_string(),
                        agent_type: "imported-from-qoder".to_string(),
                        session_id: None,
                    },
                );
                mem.namespace = namespace.clone();
                mem.tags = vec!["qoder-rule".to_string()];
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

/// Walk a rule file's ancestors for a `.qoder` component and use its parent
/// directory's name as the project id; files outside a `.qoder/` tree (e.g.
/// an unrelated `--path` override) fall back to `global`.
fn project_namespace_from_qoder_path(path: &Path) -> String {
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some(".qoder")
            && let Some(project_dir) = ancestor.parent()
            && let Some(name) = project_dir.file_name().and_then(|n| n.to_str())
        {
            return format!("project:{name}");
        }
    }
    "global".to_string()
}

fn collect_md_files_recursive(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if max_depth == 0 {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_md_files_recursive(&path, max_depth - 1));
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    out
}

/// A user-supplied override may be a single file or a directory (scanned
/// recursively, mirroring how Qoder groups rules into subdirectories).
fn probe_override(p: &Path) -> Option<DetectedSource> {
    if p.is_file() {
        return Some(DetectedSource {
            agent_key: "qoder",
            paths: vec![p.to_path_buf()],
        });
    }
    if p.is_dir() {
        let mut md_files = collect_md_files_recursive(p, 4);
        md_files.sort();
        if md_files.is_empty() {
            return None;
        }
        return Some(DetectedSource {
            agent_key: "qoder",
            paths: md_files,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_agent_import_qoder_{name}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_finds_top_level_rules() {
        let home = tmp_dir("home");
        let cwd = tmp_dir("cwd");
        let rules_dir = cwd.join(".qoder").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("project-overview.md"),
            "# Overview\nSpring Boot 3.x\n",
        )
        .unwrap();

        let source = QoderSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_nested_subdirectory_rules() {
        let home = tmp_dir("home2");
        let cwd = tmp_dir("cwd2");
        let backend_dir = cwd.join(".qoder").join("rules").join("backend");
        std::fs::create_dir_all(&backend_dir).unwrap();
        std::fs::write(backend_dir.join("spring.md"), "# Spring\nno raw SQL\n").unwrap();

        let source = QoderSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_returns_none_when_nothing_found() {
        let home = tmp_dir("home3");
        let cwd = tmp_dir("cwd3");
        let source = QoderSource;
        assert!(source.detect(&home, &cwd, None).is_none());
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_derives_project_namespace_from_qoder_ancestor() {
        let home = tmp_dir("home4");
        let cwd = tmp_dir("cwd4");
        let rules_dir = cwd.join(".qoder").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("known-bugs.md"),
            "# Known Bugs\nTimezone off by 8h\n",
        )
        .unwrap();

        let source = QoderSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);

        let expected_project = format!("project:{}", cwd.file_name().unwrap().to_str().unwrap());
        assert_eq!(outcome.candidates[0].memory.namespace, expected_project);
        assert_eq!(outcome.candidates[0].memory.memory_type, MemoryType::Fact);
        assert_eq!(outcome.candidates[0].memory.priority, Priority::Reference);
        assert!(
            outcome.candidates[0]
                .memory
                .tags
                .contains(&"qoder-rule".to_string())
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_empty_rule_file_is_skipped() {
        let home = tmp_dir("home5");
        let cwd = tmp_dir("cwd5");
        let rules_dir = cwd.join(".qoder").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("empty.md"), "   \n").unwrap();

        let source = QoderSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.files_skipped.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_override_path_outside_qoder_tree_falls_back_to_global() {
        let dir = tmp_dir("override");
        std::fs::write(dir.join("random.md"), "# X\nsome note\n").unwrap();

        let source = QoderSource;
        let detected = source
            .detect(
                &PathBuf::from("/nonexistent"),
                &PathBuf::from("/nonexistent"),
                Some(&dir),
            )
            .unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].memory.namespace, "global");

        std::fs::remove_dir_all(dir).ok();
    }
}
