//! OpenClaw personal-agent-runtime memory adapter — **experimental**.
//!
//! OpenClaw's memory system was substantially rewritten in its 2.0 release
//! with no first-party documentation of the on-disk schema at the time this
//! adapter was written. The Markdown surface below was narrowed against a
//! real installation rather than left as an unbounded sweep: OpenClaw
//! follows the same dual-file convention Hermes uses (`USER.md`/`MEMORY.md`,
//! observed directly under the home dir on some installs and under a
//! `workspace/` subdirectory on others) plus a `memory/` directory of dated
//! journal entries. An earlier, unbounded "any `*.md` anywhere under the
//! home dir" version of this adapter was tried against a real install and
//! pulled in hundreds of false positives from `extensions/*/README.md`,
//! `skills/*/SKILL.md`, and `wiki/` pages — none of which are personal
//! memory — so this adapter deliberately does **not** walk the rest of the
//! OpenClaw home directory:
//!
//! - `$OPENCLAW_HOME`, `~/.openclaw/`, `~/.config/openclaw/` are checked
//!   (and each of their `workspace/` subdirectories) for `USER.md`,
//!   `MEMORY.md`, and any `.md` file directly inside a `memory/` folder.
//! - The same roots are also searched (bounded depth) for
//!   `memory*.json`/`memory*.jsonl` (filename-gated — session/log files are
//!   not memory-prefixed and are left alone), in case a different install
//!   uses a JSON-backed store instead.
//! - JSON/JSONL entries are scanned for the first of a handful of common
//!   free-text keys (`content`, `text`, `summary`, `fact`, `value`,
//!   `memory`, `note`); entries with none of these are skipped individually,
//!   not treated as a file-level failure.
//! - Markdown files are section-split like the other adapters.
//!
//! Every candidate here is tagged `experimental` in addition to the usual
//! `imported-openclaw` tag, and confidence is always [`ParseConfidence::Heuristic`]
//! — never `Structured`: even though the shape above is now confirmed
//! against one real install, it is not a documented contract and should be
//! revisited as OpenClaw's format continues to evolve. `--path` still
//! accepts an arbitrary file or directory (scanned unfiltered) for installs
//! that don't match this layout.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    AgentMemorySource, DetectedSource, ImportCandidate, ImportParseOutcome, ParseConfidence,
    split_by_heading,
};
use crate::models::{Memory, MemoryType, Priority, SourceAgent};

const CANDIDATE_TEXT_KEYS: &[&str] = &[
    "content", "text", "summary", "fact", "value", "memory", "note",
];

pub struct OpenClawSource;

impl AgentMemorySource for OpenClawSource {
    fn agent_key(&self) -> &'static str {
        "openclaw"
    }

    fn display_name(&self) -> &'static str {
        "OpenClaw (experimental — memory format not confirmed)"
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

        let mut roots = Vec::new();
        if let Some(env_home) = std::env::var_os("OPENCLAW_HOME") {
            roots.push(PathBuf::from(env_home));
        }
        roots.push(home.join(".openclaw"));
        roots.push(home.join(".config").join("openclaw"));

        let mut paths = Vec::new();
        for root in &roots {
            if !root.is_dir() {
                continue;
            }
            paths.extend(collect_known_memory_files(root));
            paths.extend(collect_memory_prefixed_json(root, 2));
        }
        paths.sort();
        paths.dedup();

        if paths.is_empty() {
            None
        } else {
            Some(DetectedSource {
                agent_key: "openclaw",
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

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            match ext {
                "md" => parse_markdown(&content, &mut outcome),
                "json" => parse_json_values(&content, path, &mut outcome),
                "jsonl" => parse_jsonl(&content, path, &mut outcome),
                _ => outcome
                    .files_skipped
                    .push((path.clone(), "unrecognized file type".to_string())),
            }
        }

        outcome
    }
}

fn push_candidate(text: String, outcome: &mut ImportParseOutcome) {
    let mut mem = Memory::new(
        MemoryType::Fact,
        text.clone(),
        Priority::Reference,
        SourceAgent {
            id: "import-openclaw".to_string(),
            agent_type: "imported-from-openclaw".to_string(),
            session_id: None,
        },
    );
    mem.namespace = "global".to_string();
    mem.tags = vec!["imported-openclaw".to_string(), "experimental".to_string()];
    mem.confidence = 0.5;
    mem.ai_generated = false;
    outcome.candidates.push(ImportCandidate {
        memory: mem,
        raw_excerpt: text,
        confidence: ParseConfidence::Heuristic,
    });
}

fn parse_markdown(content: &str, outcome: &mut ImportParseOutcome) {
    let sections = split_by_heading(content);
    if sections.is_empty() {
        return;
    }
    for (title, body) in sections {
        let text = match &title {
            Some(t) => format!("{t}\n\n{body}"),
            None => body,
        };
        push_candidate(text, outcome);
    }
}

/// First recognized free-text field on a JSON object, or the value itself
/// when it is already a plain string.
fn extract_candidate_text(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => CANDIDATE_TEXT_KEYS.iter().find_map(|key| {
            map.get(*key).and_then(|v| v.as_str()).and_then(|s| {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            })
        }),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        _ => None,
    }
}

fn parse_json_values(content: &str, path: &Path, outcome: &mut ImportParseOutcome) {
    let values: Vec<Value> = if let Ok(arr) = serde_json::from_str::<Vec<Value>>(content) {
        arr
    } else if let Ok(single) = serde_json::from_str::<Value>(content) {
        vec![single]
    } else {
        outcome
            .files_skipped
            .push((path.to_path_buf(), "invalid JSON".to_string()));
        return;
    };

    let mut found_any = false;
    for value in &values {
        if let Some(text) = extract_candidate_text(value) {
            push_candidate(text, outcome);
            found_any = true;
        }
    }
    if !found_any {
        outcome.files_skipped.push((
            path.to_path_buf(),
            "no recognized memory-like fields in JSON entries".to_string(),
        ));
    }
}

fn parse_jsonl(content: &str, path: &Path, outcome: &mut ImportParseOutcome) {
    let mut found_any = false;
    let mut any_line = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        any_line = true;
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = extract_candidate_text(&value) {
            push_candidate(text, outcome);
            found_any = true;
        }
    }
    if any_line && !found_any {
        outcome.files_skipped.push((
            path.to_path_buf(),
            "no recognized memory-like fields in JSONL entries".to_string(),
        ));
    }
}

/// The confirmed-in-the-wild Markdown memory surface: `USER.md`/`MEMORY.md`
/// (checked at `root` directly and under `root/workspace/`, since some
/// installs nest state there) plus any `.md` file directly inside a
/// `memory/` folder at either of those locations. Deliberately does not walk
/// `extensions/`, `skills/`, `wiki/`, or any other subdirectory — see the
/// module doc comment for why an earlier unbounded version of this over-collected.
fn collect_known_memory_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in [root.to_path_buf(), root.join("workspace")] {
        for name in ["USER.md", "MEMORY.md"] {
            let p = base.join(name);
            if p.is_file() {
                out.push(p);
            }
        }
        let memory_dir = base.join("memory");
        if let Ok(entries) = std::fs::read_dir(&memory_dir) {
            let mut daily: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
                .collect();
            daily.sort();
            out.extend(daily);
        }
    }
    out
}

/// `memory*.json` / `memory*.jsonl` (filename-gated, so session/log files
/// with unrelated names are left alone), for installs that use a JSON-backed
/// store instead of the Markdown convention above. Bounded depth keeps this
/// from wandering into large unrelated subdirectories (caches, extensions).
fn collect_memory_prefixed_json(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
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
            out.extend(collect_memory_prefixed_json(&path, max_depth - 1));
            continue;
        }
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let lower = filename.to_lowercase();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if (ext == "json" || ext == "jsonl") && lower.starts_with("memory") {
            out.push(path);
        }
    }
    out
}

/// An explicit `--path` override is scanned unfiltered (any `.md`/`.json`/
/// `.jsonl` file, recursively) since the user pointed directly at it and
/// presumably knows it holds memory content.
fn probe_override(p: &Path) -> Option<DetectedSource> {
    if p.is_file() {
        return Some(DetectedSource {
            agent_key: "openclaw",
            paths: vec![p.to_path_buf()],
        });
    }
    if p.is_dir() {
        let mut files = collect_any_supported_file(p, 3);
        files.sort();
        if files.is_empty() {
            return None;
        }
        return Some(DetectedSource {
            agent_key: "openclaw",
            paths: files,
        });
    }
    None
}

fn collect_any_supported_file(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
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
            out.extend(collect_any_supported_file(&path, max_depth - 1));
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if matches!(ext, "md" | "json" | "jsonl") {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_agent_import_openclaw_{name}_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_finds_memory_json_in_home_openclaw_dir() {
        let home = tmp_dir("home");
        let cwd = tmp_dir("cwd");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("memory.json"),
            r#"[{"content": "user prefers dark mode"}]"#,
        )
        .unwrap();
        // A same-directory session log must NOT be picked up.
        std::fs::write(
            home.join(".openclaw").join("session-2026-01-01.jsonl"),
            "{}",
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 1);
        assert_eq!(detected.paths[0].file_name().unwrap(), "memory.json");

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_returns_none_when_nothing_found() {
        let home = tmp_dir("home2");
        let cwd = tmp_dir("cwd2");
        let source = OpenClawSource;
        assert!(source.detect(&home, &cwd, None).is_none());
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_json_array_extracts_recognized_keys() {
        let home = tmp_dir("home3");
        let cwd = tmp_dir("cwd3");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("memory-store.json"),
            r#"[{"text": "likes Rust"}, {"summary": "deploys via Docker"}, {"unrelated": "ignored"}]"#,
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 2);
        for c in &outcome.candidates {
            assert_eq!(c.confidence, ParseConfidence::Heuristic);
            assert!(c.memory.tags.contains(&"experimental".to_string()));
        }

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_parse_jsonl_extracts_recognized_keys() {
        let home = tmp_dir("home4");
        let cwd = tmp_dir("cwd4");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("memory.jsonl"),
            "{\"fact\": \"team uses trunk-based development\"}\n{\"nothing_useful\": 1}\n",
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(
            outcome.candidates[0].memory.content,
            "team uses trunk-based development"
        );

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_json_with_no_recognized_fields_is_skipped() {
        let home = tmp_dir("home5");
        let cwd = tmp_dir("cwd5");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("memory.json"),
            r#"[{"id": 1}, {"id": 2}]"#,
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.files_skipped.len(), 1);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_invalid_json_is_skipped_not_fatal() {
        let home = tmp_dir("home6");
        let cwd = tmp_dir("cwd6");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("memory.json"),
            "{not valid json",
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        let outcome = source.parse(&detected);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.files_skipped.len(), 1);
        assert!(outcome.files_skipped[0].1.contains("invalid JSON"));

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_user_and_memory_md_at_root() {
        let home = tmp_dir("home7");
        let cwd = tmp_dir("cwd7");
        std::fs::create_dir_all(home.join(".openclaw")).unwrap();
        std::fs::write(
            home.join(".openclaw").join("USER.md"),
            "# User\nprefers vim\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".openclaw").join("MEMORY.md"),
            "# Notes\ndeploys via GitHub Actions\n",
        )
        .unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_user_and_memory_md_under_workspace() {
        let home = tmp_dir("home8");
        let cwd = tmp_dir("cwd8");
        let workspace = home.join(".openclaw").join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("USER.md"), "# User\nlikes rust\n").unwrap();
        std::fs::write(workspace.join("MEMORY.md"), "# Notes\nteam uses postgres\n").unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_detect_finds_daily_journal_entries_in_memory_dir() {
        let home = tmp_dir("home9");
        let cwd = tmp_dir("cwd9");
        let memory_dir = home.join(".openclaw").join("workspace").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("2026-05-07.md"), "# Journal\ndid X today\n").unwrap();
        std::fs::write(memory_dir.join("2026-05-08.md"), "# Journal\ndid Y today\n").unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 2);

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    /// Regression guard: an earlier unbounded version of this adapter walked
    /// the whole OpenClaw home directory for any `*.md` file and, against a
    /// real install, pulled in hundreds of unrelated docs from
    /// `extensions/*/README.md` and `skills/*/SKILL.md`. Neither must be
    /// picked up by the default (non-override) detection path.
    #[test]
    fn test_unrelated_markdown_outside_known_paths_is_ignored() {
        let home = tmp_dir("home10");
        let cwd = tmp_dir("cwd10");
        let openclaw_dir = home.join(".openclaw");
        std::fs::create_dir_all(openclaw_dir.join("extensions").join("some-plugin")).unwrap();
        std::fs::create_dir_all(openclaw_dir.join("skills").join("board-cli")).unwrap();
        std::fs::write(
            openclaw_dir
                .join("extensions")
                .join("some-plugin")
                .join("README.md"),
            "# Some Plugin\nInstall instructions here\n",
        )
        .unwrap();
        std::fs::write(
            openclaw_dir
                .join("skills")
                .join("board-cli")
                .join("SKILL.md"),
            "---\nname: board-cli\n---\nsome skill doc\n",
        )
        .unwrap();
        // A real memory file alongside the noise must still be found.
        std::fs::write(openclaw_dir.join("USER.md"), "# User\nreal memory\n").unwrap();

        let source = OpenClawSource;
        let detected = source.detect(&home, &cwd, None).unwrap();
        assert_eq!(detected.paths.len(), 1, "only USER.md should be detected");
        assert_eq!(detected.paths[0].file_name().unwrap(), "USER.md");

        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn test_markdown_notes_are_section_split() {
        let dir = tmp_dir("md_override");
        std::fs::write(dir.join("notes.md"), "# Preferences\nprefers vim\n").unwrap();

        let source = OpenClawSource;
        let detected = source
            .detect(
                &PathBuf::from("/nonexistent"),
                &PathBuf::from("/nonexistent"),
                Some(&dir),
            )
            .unwrap();
        let outcome = source.parse(&detected);
        assert_eq!(outcome.candidates.len(), 1);
        assert!(outcome.candidates[0].memory.content.contains("prefers vim"));

        std::fs::remove_dir_all(dir).ok();
    }
}
