//! Hermes Agent (Nous Research) memory adapter.
//!
//! Hermes keeps a "dual-track" memory: `~/.hermes/USER.md` (user profile /
//! preferences) and `~/.hermes/MEMORY.md` (project/environment notes), both
//! free-form Markdown, split into sections the same way as Codex's
//! `AGENTS.md`. Self-distilled procedures under `~/.hermes/skills/*.md` are
//! structurally identical to the SOP format MemVault already imports via
//! `memvault import-skills`, so they reuse [`crate::sop::parse_sops`]
//! directly instead of a bespoke parser.
//!
//! Hermes has no per-project memory directory (unlike Claude/Codex), so
//! every candidate here lands in the `global` namespace; `--namespace` on
//! the CLI still overrides it per import.

use std::path::{Path, PathBuf};

use super::{
    AgentMemorySource, DetectedSource, ImportCandidate, ImportParseOutcome, ParseConfidence,
    split_by_heading,
};
use crate::models::{Memory, MemoryType, Priority, SkillMeta, SourceAgent};
use crate::sop::parse_sops;

pub struct HermesSource;

impl AgentMemorySource for HermesSource {
    fn agent_key(&self) -> &'static str {
        "hermes"
    }

    fn display_name(&self) -> &'static str {
        "Hermes Agent"
    }

    fn detect(
        &self,
        home: &Path,
        _cwd: &Path,
        override_path: Option<&Path>,
    ) -> Option<DetectedSource> {
        if let Some(p) = override_path {
            return probe_override(p);
        }

        let mut paths = Vec::new();
        let hermes_dir = home.join(".hermes");

        let user_md = hermes_dir.join("USER.md");
        if user_md.is_file() {
            paths.push(user_md);
        }
        let memory_md = hermes_dir.join("MEMORY.md");
        if memory_md.is_file() {
            paths.push(memory_md);
        }

        let skills_dir = hermes_dir.join("skills");
        paths.extend(collect_skill_files(&skills_dir));

        if paths.is_empty() {
            None
        } else {
            Some(DetectedSource {
                agent_key: "hermes",
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
            // Skill files sit either directly in `skills/` or one level
            // down in `skills/<name>/`, matching `collect_skill_files`.
            let in_skills_dir = path
                .ancestors()
                .skip(1)
                .take(2)
                .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("skills"));

            if in_skills_dir {
                parse_skill_file(path, &content, &mut outcome);
                continue;
            }

            match filename {
                "USER.md" => push_sections(
                    &content,
                    MemoryType::Preference,
                    "imported-hermes-user",
                    &mut outcome,
                ),
                "MEMORY.md" => push_sections(
                    &content,
                    MemoryType::Fact,
                    "imported-hermes-memory",
                    &mut outcome,
                ),
                _ => outcome
                    .files_skipped
                    .push((path.clone(), "unrecognized Hermes file".to_string())),
            }
        }

        outcome
    }
}

fn push_sections(
    content: &str,
    memory_type: MemoryType,
    tag: &str,
    outcome: &mut ImportParseOutcome,
) {
    for (title, body) in split_by_heading(content) {
        let text = match &title {
            Some(t) => format!("{t}\n\n{body}"),
            None => body.clone(),
        };
        let mut mem = Memory::new(
            memory_type.clone(),
            text,
            Priority::Reference,
            SourceAgent {
                id: "import-hermes".to_string(),
                agent_type: "imported-from-hermes".to_string(),
                session_id: None,
            },
        );
        mem.namespace = "global".to_string();
        mem.tags = vec![tag.to_string()];
        mem.confidence = 0.7;
        mem.ai_generated = false;
        outcome.candidates.push(ImportCandidate {
            memory: mem,
            raw_excerpt: body,
            confidence: ParseConfidence::SectionSplit,
        });
    }
}

fn parse_skill_file(path: &Path, content: &str, outcome: &mut ImportParseOutcome) {
    let fallback = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported-hermes-skill".to_string());
    let parsed = parse_sops(content, &fallback);

    if parsed.skills.is_empty() {
        outcome.files_skipped.push((
            path.to_path_buf(),
            "no steps found in skill file".to_string(),
        ));
        return;
    }

    for skill in parsed.skills {
        let mut mem = Memory::new(
            MemoryType::Skill,
            skill.title.clone(),
            Priority::Reference,
            SourceAgent {
                id: "import-hermes".to_string(),
                agent_type: "imported-from-hermes".to_string(),
                session_id: None,
            },
        );
        mem.namespace = "global".to_string();
        mem.tags = vec!["imported-hermes-skill".to_string()];
        mem.confidence = 0.8;
        mem.ai_generated = false;
        mem.skill_meta = Some(SkillMeta {
            trigger: skill.trigger,
            steps: skill.steps,
            verification: skill.verification,
            version: 1,
        });
        outcome.candidates.push(ImportCandidate {
            memory: mem,
            raw_excerpt: content.to_string(),
            confidence: ParseConfidence::Structured,
        });
    }
}

/// Skill files live either directly in `skills/*.md` or one level down in
/// `skills/<name>/*.md` (observed in the wild as `skills/board-cli/SKILL.md`,
/// `skills/apple/DESCRIPTION.md`) — deeper nesting (e.g. a skill's own
/// `references/` subfolder) is intentionally not followed, so unrelated
/// supporting docs aren't mistaken for top-level skills.
fn collect_skill_files(skills_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return out;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            let Ok(nested) = std::fs::read_dir(&path) else {
                continue;
            };
            let mut nested_md: Vec<PathBuf> = nested
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
                .collect();
            nested_md.sort();
            out.extend(nested_md);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// A user-supplied override may be a single file or a directory of `.md`
/// files (non-recursive).
fn probe_override(p: &Path) -> Option<DetectedSource> {
    if p.is_file() {
        return Some(DetectedSource {
            agent_key: "hermes",
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
            agent_key: "hermes",
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
            "memvault_agent_import_hermes_{name}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_finds_user_and_memory_md() {
        let home = tmp_dir("home");
        let cwd = tmp_dir("cwd");
        std::fs::create_dir_all(home.join(".hermes")).unwrap();
        std::fs::write(
            home.join(".hermes").join("USER.md"),
            "# Profile\nSenior backend engineer\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".hermes").join("MEMORY.md"),
            "# Environment\nDeploys via GitHub Actions\n",
        )
        .unwrap();

        let source = HermesSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_skills() {
        let home = tmp_dir("home2");
        let cwd = tmp_dir("cwd2");
        let skills_dir = home.join(".hermes").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("deploy.md"),
            "# Deploy\ntrigger: deploy\n1. build\n2. push\n",
        )
        .unwrap();

        let source = HermesSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 1);

        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].memory.memory_type, MemoryType::Skill);
        assert!(outcome.candidates[0].memory.skill_meta.is_some());
        assert_eq!(
            outcome.candidates[0].confidence,
            ParseConfidence::Structured
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    /// Real-world layout observed against an actual Hermes install: each
    /// skill lives in its own subdirectory (`skills/board-cli/SKILL.md`,
    /// `skills/apple/DESCRIPTION.md`), not as a flat file directly under
    /// `skills/`. An earlier version of `detect()` only did a non-recursive
    /// `read_dir(skills/)` and silently found nothing on installs shaped
    /// this way.
    #[test]
    fn test_detect_finds_nested_skill_directories() {
        let home = tmp_dir("home2b");
        let cwd = tmp_dir("cwd2b");
        let skills_dir = home.join(".hermes").join("skills");
        std::fs::create_dir_all(skills_dir.join("board-cli")).unwrap();
        std::fs::create_dir_all(skills_dir.join("apple")).unwrap();
        // A skill dir's own reference material must not be picked up.
        std::fs::create_dir_all(skills_dir.join("board-cli").join("references")).unwrap();
        std::fs::write(
            skills_dir.join("board-cli").join("SKILL.md"),
            "# Board CLI\ntrigger: board\n1. call the REST API\n2. render results\n",
        )
        .unwrap();
        std::fs::write(
            skills_dir.join("apple").join("DESCRIPTION.md"),
            "# Apple\njust a category description, no steps\n",
        )
        .unwrap();
        std::fs::write(
            skills_dir
                .join("board-cli")
                .join("references")
                .join("api.md"),
            "1. this must not be treated as a top-level skill\n",
        )
        .unwrap();

        let source = HermesSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(
            detected.paths.len(),
            2,
            "should find SKILL.md and DESCRIPTION.md, not references/api.md"
        );

        let outcome = source.parse(&detected);
        assert_eq!(
            outcome.candidates.len(),
            1,
            "only board-cli/SKILL.md has steps"
        );
        assert_eq!(outcome.candidates[0].memory.content, "Board CLI");
        assert_eq!(
            outcome.files_skipped.len(),
            1,
            "apple/DESCRIPTION.md has no steps"
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_returns_none_when_nothing_found() {
        let home = tmp_dir("home3");
        let cwd = tmp_dir("cwd3");
        let source = HermesSource;
        assert!(source.detect(&home, &cwd, None).is_none());
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_user_and_memory_get_expected_types() {
        let home = tmp_dir("home4");
        let cwd = tmp_dir("cwd4");
        std::fs::create_dir_all(home.join(".hermes")).unwrap();
        std::fs::write(
            home.join(".hermes").join("USER.md"),
            "# Profile\nprefers dark mode\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".hermes").join("MEMORY.md"),
            "# Env\nproject uses postgres\n",
        )
        .unwrap();

        let source = HermesSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 2);

        let user_cand = outcome
            .candidates
            .iter()
            .find(|c| c.memory.tags.contains(&"imported-hermes-user".to_string()))
            .unwrap();
        assert_eq!(user_cand.memory.memory_type, MemoryType::Preference);

        let mem_cand = outcome
            .candidates
            .iter()
            .find(|c| {
                c.memory
                    .tags
                    .contains(&"imported-hermes-memory".to_string())
            })
            .unwrap();
        assert_eq!(mem_cand.memory.memory_type, MemoryType::Fact);
        assert_eq!(mem_cand.memory.namespace, "global");

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_skill_file_without_steps_is_skipped() {
        let home = tmp_dir("home5");
        let cwd = tmp_dir("cwd5");
        let skills_dir = home.join(".hermes").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("empty.md"), "# No Steps\njust prose\n").unwrap();

        let source = HermesSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.files_skipped.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_override_path_directory() {
        let dir = tmp_dir("override");
        std::fs::write(dir.join("USER.md"), "# Profile\nlikes rust\n").unwrap();

        let source = HermesSource;
        let detected = source
            .detect(
                &PathBuf::from("/nonexistent"),
                &PathBuf::from("/nonexistent"),
                Some(&dir),
            )
            .unwrap();
        assert_eq!(detected.paths.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }
}
