//! Claude Code / Claude Desktop memory adapter.
//!
//! Two source shapes are supported:
//! - `CLAUDE.md` (global `~/.claude/CLAUDE.md` or project-root `./CLAUDE.md`):
//!   free-form human-authored instructions, split into sections by heading.
//! - Auto-memory files (`~/.claude/projects/*/memory/*.md`): YAML frontmatter
//!   (`name`, `description`, `metadata.type`) plus a Markdown body, mapped
//!   directly to memory fields. The per-project `MEMORY.md` index file (a
//!   list of links, no unique content of its own) is skipped.

use std::path::{Path, PathBuf};

use super::{
    AgentMemorySource, DetectedSource, ImportCandidate, ImportParseOutcome, ParseConfidence,
    infer_namespace_from_parent, split_by_heading,
};
use crate::models::{Memory, MemoryType, Priority, SourceAgent};

pub struct ClaudeSource;

impl AgentMemorySource for ClaudeSource {
    fn agent_key(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code / Claude Desktop"
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

        let global_claude_md = home.join(".claude").join("CLAUDE.md");
        if global_claude_md.is_file() {
            paths.push(global_claude_md);
        }

        let project_claude_md = cwd.join("CLAUDE.md");
        if project_claude_md.is_file() {
            paths.push(project_claude_md);
        }

        let projects_dir = home.join(".claude").join("projects");
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let memory_dir = entry.path().join("memory");
                let Ok(mem_entries) = std::fs::read_dir(&memory_dir) else {
                    continue;
                };
                let mut md_files: Vec<PathBuf> = mem_entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
                    .collect();
                md_files.sort();
                paths.extend(md_files);
            }
        }

        if paths.is_empty() {
            None
        } else {
            Some(DetectedSource {
                agent_key: "claude",
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

            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if filename == "MEMORY.md" {
                outcome
                    .files_skipped
                    .push((path.clone(), "index file (no unique content)".to_string()));
                continue;
            }

            if filename == "CLAUDE.md" {
                let namespace = infer_namespace_from_parent(path, ".claude");
                for (title, body) in split_by_heading(&content) {
                    let text = match &title {
                        Some(t) => format!("{t}\n\n{body}"),
                        None => body.clone(),
                    };
                    let mut mem = Memory::new(
                        MemoryType::Fact,
                        text,
                        Priority::Reference,
                        SourceAgent {
                            id: "import-claude".to_string(),
                            agent_type: "imported-from-claude".to_string(),
                            session_id: None,
                        },
                    );
                    mem.namespace = namespace.clone();
                    mem.tags = vec!["imported-claude-md".to_string()];
                    mem.confidence = 0.7;
                    mem.ai_generated = false;
                    outcome.candidates.push(ImportCandidate {
                        memory: mem,
                        raw_excerpt: body,
                        confidence: ParseConfidence::SectionSplit,
                    });
                }
                continue;
            }

            match parse_auto_memory(&content) {
                Some((mem, raw)) => outcome.candidates.push(ImportCandidate {
                    memory: mem,
                    raw_excerpt: raw,
                    confidence: ParseConfidence::Structured,
                }),
                None => outcome.files_skipped.push((
                    path.clone(),
                    "empty or unparseable auto-memory file".to_string(),
                )),
            }
        }

        outcome
    }
}

/// A user-supplied override path may be a single file or a directory of
/// `.md` files (non-recursive — good enough for a manually pointed-at
/// export directory).
fn probe_override(p: &Path) -> Option<DetectedSource> {
    if p.is_file() {
        return Some(DetectedSource {
            agent_key: "claude",
            paths: vec![p.to_path_buf()],
        });
    }
    if p.is_dir() {
        let mut md_files: Vec<PathBuf> = std::fs::read_dir(p)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
            .collect();
        md_files.sort();
        if md_files.is_empty() {
            return None;
        }
        return Some(DetectedSource {
            agent_key: "claude",
            paths: md_files,
        });
    }
    None
}

#[derive(serde::Deserialize)]
struct AutoMemoryFrontMatter {
    #[allow(dead_code)]
    name: Option<String>,
    description: Option<String>,
    metadata: Option<AutoMemoryMetadata>,
}

#[derive(serde::Deserialize)]
struct AutoMemoryMetadata {
    #[serde(rename = "type")]
    mem_type: Option<String>,
}

/// Parse one `~/.claude/projects/*/memory/*.md` auto-memory file. Returns
/// `(Memory, raw_body)` or `None` when the file has no usable frontmatter or
/// content — never an error, the caller records it as a skip instead.
fn parse_auto_memory(content: &str) -> Option<(Memory, String)> {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    let (frontmatter, body) = if parts.len() >= 3 {
        (parts[1].trim(), parts[2].trim())
    } else {
        ("", content.trim())
    };

    let fm: AutoMemoryFrontMatter = if frontmatter.is_empty() {
        AutoMemoryFrontMatter {
            name: None,
            description: None,
            metadata: None,
        }
    } else {
        serde_yaml::from_str(frontmatter).ok()?
    };

    let mem_type_str = fm
        .metadata
        .and_then(|m| m.mem_type)
        .unwrap_or_else(|| "unknown".to_string());

    let text = if !body.is_empty() {
        body.to_string()
    } else {
        fm.description.clone().unwrap_or_default()
    };
    if text.is_empty() {
        return None;
    }

    let memory_type = match mem_type_str.as_str() {
        "user" => MemoryType::Preference,
        _ => MemoryType::Fact,
    };

    let mut mem = Memory::new(
        memory_type,
        text.clone(),
        Priority::Reference,
        SourceAgent {
            id: "import-claude".to_string(),
            agent_type: "imported-from-claude".to_string(),
            session_id: None,
        },
    );
    mem.namespace = "global".to_string();
    mem.tags = vec![
        "imported-claude-memory".to_string(),
        format!("claude-{mem_type_str}"),
    ];
    mem.confidence = 0.85;
    mem.ai_generated = false;

    Some((mem, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_agent_import_claude_{name}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_finds_global_and_project_claude_md() {
        let home = tmp_dir("home");
        let cwd = tmp_dir("cwd");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join("CLAUDE.md"),
            "# Rules\nuse rust\n",
        )
        .unwrap();
        std::fs::write(cwd.join("CLAUDE.md"), "# Project rules\nuse pytest\n").unwrap();

        let source = ClaudeSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_auto_memory_files() {
        let home = tmp_dir("home2");
        let cwd = tmp_dir("cwd2");
        let mem_dir = home
            .join(".claude")
            .join("projects")
            .join("abc123")
            .join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(
            mem_dir.join("user_role.md"),
            "---\nname: user-role\ndescription: test\nmetadata:\n  type: user\n---\n\nData scientist\n",
        )
        .unwrap();
        std::fs::write(
            mem_dir.join("MEMORY.md"),
            "- [User Role](user_role.md) — role\n",
        )
        .unwrap();

        let source = ClaudeSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.files_skipped.len(), 1);
        assert_eq!(
            outcome.candidates[0].memory.memory_type,
            MemoryType::Preference
        );
        assert_eq!(
            outcome.candidates[0].confidence,
            ParseConfidence::Structured
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_returns_none_when_nothing_found() {
        let home = tmp_dir("home3");
        let cwd = tmp_dir("cwd3");
        let source = ClaudeSource;
        assert!(source.detect(&home, &cwd, None).is_none());
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_claude_md_splits_sections_and_sets_namespace() {
        let home = tmp_dir("home4");
        let cwd = tmp_dir("cwd4");
        std::fs::write(
            cwd.join("CLAUDE.md"),
            "# Coding\nUse rustfmt\n\n# Testing\nUse cargo test\n",
        )
        .unwrap();

        let source = ClaudeSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 2);
        for c in &outcome.candidates {
            assert_eq!(c.memory.priority, Priority::Reference);
            assert!(c.memory.namespace.starts_with("project:"));
            assert_eq!(c.confidence, ParseConfidence::SectionSplit);
        }

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_override_path_directory_of_md_files() {
        let dir = tmp_dir("override");
        std::fs::write(
            dir.join("a.md"),
            "---\nmetadata:\n  type: project\n---\n\nuses FastAPI\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.txt"), "ignored, not markdown").unwrap();

        let source = ClaudeSource;
        let detected = source
            .detect(
                &PathBuf::from("/nonexistent"),
                &PathBuf::from("/nonexistent"),
                Some(&dir),
            )
            .unwrap();
        assert_eq!(detected.paths.len(), 1);
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].memory.memory_type, MemoryType::Fact);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_auto_memory_skips_empty_content() {
        let content = "---\nname: empty\n---\n\n";
        assert!(parse_auto_memory(content).is_none());
    }
}
