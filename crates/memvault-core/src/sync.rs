use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

use crate::error::{MemVaultError, Result};
use crate::models::*;
use crate::storage::MemoryStore;

// SyncState is implemented as a u64 hash via MemoryStore::sync_state_hash.

/// Detected project context for memory filtering.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub name: String,
    pub tech_stack: Vec<String>,
    pub root: PathBuf,
}

/// Configuration for sync output.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub generate_claude_md: bool,
    pub generate_agents_md: bool,
    pub generate_copilot: bool,
    pub generate_cursorrules: bool,
    pub generate_clinerules: bool,
    pub max_chars_compact: usize,
    pub max_chars_full: usize,
    /// Polling interval in seconds for --watch mode.
    pub watch_interval_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            generate_claude_md: true,
            generate_agents_md: true,
            generate_copilot: true,
            generate_cursorrules: true,
            generate_clinerules: true,
            max_chars_compact: 2000,
            max_chars_full: 6000,
            watch_interval_secs: 5,
        }
    }
}

pub struct SyncEngine {
    store: Arc<dyn MemoryStore>,
    config: SyncConfig,
}

impl SyncEngine {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            config: SyncConfig::default(),
        }
    }

    pub fn with_config(mut self, config: SyncConfig) -> Self {
        self.config = config;
        self
    }

    pub fn config(&self) -> &SyncConfig {
        &self.config
    }

    /// Detect project context from the given directory.
    pub fn detect_project(dir: &Path) -> ProjectContext {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());

        let mut tech_stack = Vec::new();

        if dir.join("Cargo.toml").exists() {
            tech_stack.push("rust".to_string());
        }
        if dir.join("package.json").exists() {
            tech_stack.push("javascript".to_string());
            tech_stack.push("nodejs".to_string());
        }
        if dir.join("tsconfig.json").exists() {
            tech_stack.push("typescript".to_string());
        }
        if dir.join("pyproject.toml").exists()
            || dir.join("setup.py").exists()
            || dir.join("requirements.txt").exists()
        {
            tech_stack.push("python".to_string());
        }
        if dir.join("go.mod").exists() {
            tech_stack.push("go".to_string());
        }
        if dir.join("pom.xml").exists() || dir.join("build.gradle").exists() {
            tech_stack.push("java".to_string());
        }
        if dir.join("Gemfile").exists() {
            tech_stack.push("ruby".to_string());
        }
        if dir.join("docker-compose.yml").exists() || dir.join("Dockerfile").exists() {
            tech_stack.push("docker".to_string());
        }

        // Try to read package.json name
        if let Ok(content) = std::fs::read_to_string(dir.join("package.json"))
            && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(n) = pkg.get("name").and_then(|v| v.as_str())
        {
            return ProjectContext {
                name: n.to_string(),
                tech_stack,
                root: dir.to_path_buf(),
            };
        }

        // Try Cargo.toml name
        if let Ok(content) = std::fs::read_to_string(dir.join("Cargo.toml")) {
            for line in content.lines() {
                if line.starts_with("name")
                    && let Some(n) = line.split('"').nth(1)
                {
                    return ProjectContext {
                        name: n.to_string(),
                        tech_stack,
                        root: dir.to_path_buf(),
                    };
                }
            }
        }

        ProjectContext {
            name,
            tech_stack,
            root: dir.to_path_buf(),
        }
    }

    /// Select memories relevant to this project context.
    pub async fn select_memories(&self, ctx: &ProjectContext) -> Result<Vec<SearchResult>> {
        // Bound the working set pulled into memory for relevance scoring. Sync
        // is meant to surface a curated subset of memories, not a full data
        // dump — this cap keeps `list()` from becoming an unbounded table scan
        // as a vault's memory count grows.
        const MAX_SYNC_CANDIDATES: usize = 2000;
        let all = self.store.list(None, MAX_SYNC_CANDIDATES, 0).await?;
        if all.len() >= MAX_SYNC_CANDIDATES {
            warn!(
                cap = MAX_SYNC_CANDIDATES,
                "select_memories hit the candidate cap; some older memories may not be \
                 considered for sync. Consider archiving or running decay."
            );
        }

        let mut selected: Vec<SearchResult> = Vec::new();

        for mem in all {
            let relevance = self.compute_project_relevance(&mem, ctx);
            if relevance > 0.1 {
                selected.push(SearchResult {
                    score: relevance,
                    memory: mem,
                    hit_sources: Vec::new(),
                });
            }
        }

        // Sort: MUST first, then by relevance
        selected.sort_by(|a, b| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        debug!(total = selected.len(), "memories selected for sync");
        Ok(selected)
    }

    fn compute_project_relevance(&self, mem: &Memory, ctx: &ProjectContext) -> f64 {
        // MUST always included
        if mem.priority == Priority::Must {
            return 1.0;
        }

        // Archived/decayed memories excluded
        if mem.decay_score < 0.3 || mem.namespace.starts_with("archived:") {
            return 0.0;
        }

        let mut score: f64 = 0.3; // base relevance

        // Project namespace match
        let project_ns = format!("project:{}", ctx.name);
        if mem.namespace == project_ns {
            score += 0.4;
        }

        // Tech stack tag match
        for tag in &mem.tags {
            let tag_lower = tag.to_lowercase();
            if ctx
                .tech_stack
                .iter()
                .any(|t| tag_lower.contains(t) || t.contains(&tag_lower))
            {
                score += 0.2;
                break;
            }
        }

        // Coding/style tags are broadly useful
        if mem.tags.iter().any(|t| t == "coding" || t == "style") {
            score += 0.1;
        }

        score.min(1.0)
    }

    /// Generate all instruction files in the project directory.
    pub async fn sync(&self, project_dir: &Path) -> Result<SyncReport> {
        let ctx = Self::detect_project(project_dir);
        info!(project = %ctx.name, tech_stack = ?ctx.tech_stack, "syncing memories to project files");

        let memories = self.select_memories(&ctx).await?;

        let mut report = SyncReport {
            files_written: Vec::new(),
            memories_synced: memories.len(),
            files_skipped: Vec::new(),
        };

        // Every known target is accounted for: written, or skipped WITH a
        // reason — a silent "just not generated" is what this prevents.
        let skip_if_disabled = |enabled: bool, target: &str, report: &mut SyncReport| {
            if !enabled {
                report.files_skipped.push(SyncSkip {
                    target: target.to_string(),
                    reason: "disabled in sync config".to_string(),
                });
            }
        };

        if self.config.generate_claude_md {
            let content = self.generate_claude_md(&memories);
            let path = project_dir.join("CLAUDE.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }
        skip_if_disabled(self.config.generate_claude_md, "CLAUDE.md", &mut report);

        if self.config.generate_agents_md {
            let content = self.generate_agents_md(&memories);
            let path = project_dir.join("AGENTS.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }
        skip_if_disabled(self.config.generate_agents_md, "AGENTS.md", &mut report);

        if self.config.generate_copilot {
            let content = self.generate_copilot_md(&memories);
            let dir = project_dir.join(".github");
            std::fs::create_dir_all(&dir).ok();
            let path = dir.join("copilot-instructions.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }
        skip_if_disabled(
            self.config.generate_copilot,
            ".github/copilot-instructions.md",
            &mut report,
        );

        if self.config.generate_cursorrules {
            let content = self.generate_cursorrules(&memories);
            let path = project_dir.join(".cursorrules");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }
        skip_if_disabled(
            self.config.generate_cursorrules,
            ".cursorrules",
            &mut report,
        );

        if self.config.generate_clinerules {
            let content = self.generate_clinerules(&memories);
            let path = project_dir.join(".clinerules");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }
        skip_if_disabled(self.config.generate_clinerules, ".clinerules", &mut report);

        info!(
            files = report.files_written.len(),
            memories = report.memories_synced,
            "sync complete"
        );
        Ok(report)
    }

    fn write_if_changed(&self, path: &Path, content: &str) -> Result<()> {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        if existing == content {
            return Ok(());
        }
        std::fs::write(path, content).map_err(|e| {
            MemVaultError::Storage(format!("Failed to write {}: {}", path.display(), e))
        })?;
        debug!(path = %path.display(), "file updated");
        Ok(())
    }

    // --- Format generators ---

    fn generate_claude_md(&self, memories: &[SearchResult]) -> String {
        let mut out = String::from("# Memory Context (Auto-generated by MemVault)\n\n");
        out.push_str("> Do not edit this file manually. Run `memvault sync` to regenerate.\n\n");

        let musts: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .collect();
        let refs: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority != Priority::Must)
            .take(15)
            .collect();

        if !musts.is_empty() {
            out.push_str("## Rules (MUST follow)\n\n");
            for r in &musts {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
            out.push('\n');
        }

        if !refs.is_empty() {
            out.push_str("## Context\n\n");
            for r in &refs {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
            out.push('\n');
        }

        self.truncate_to(&out, self.config.max_chars_full)
    }

    fn generate_agents_md(&self, memories: &[SearchResult]) -> String {
        let mut out = String::from("# Project Instructions\n\n");
        out.push_str("<!-- Auto-generated by MemVault. Run `memvault sync` to update. -->\n\n");

        let musts: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .collect();
        let refs: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority != Priority::Must)
            .take(12)
            .collect();

        if !musts.is_empty() {
            out.push_str("## Mandatory Rules\n\n");
            for r in &musts {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
            out.push('\n');
        }

        if !refs.is_empty() {
            out.push_str("## Project Context\n\n");
            for r in &refs {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
        }

        self.truncate_to(&out, self.config.max_chars_full)
    }

    fn generate_copilot_md(&self, memories: &[SearchResult]) -> String {
        // Copilot has ~8000 char limit, keep it concise
        let mut out = String::from("# Copilot Instructions\n\n");

        for r in memories
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
        {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            out.push_str(&format!("- ALWAYS: {}\n", text));
        }

        let refs: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority != Priority::Must)
            .take(8)
            .collect();

        if !refs.is_empty() {
            out.push_str("\n## Context\n\n");
            for r in &refs {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
        }

        self.truncate_to(&out, 7500)
    }

    fn generate_cursorrules(&self, memories: &[SearchResult]) -> String {
        let mut out = String::new();

        for r in memories
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
        {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            out.push_str(&format!("- {}\n", text));
        }

        let refs: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority != Priority::Must)
            .take(10)
            .collect();

        if !refs.is_empty() {
            out.push_str("\nContext:\n");
            for r in &refs {
                let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
                out.push_str(&format!("- {}\n", text));
            }
        }

        self.truncate_to(&out, self.config.max_chars_full)
    }

    fn generate_clinerules(&self, memories: &[SearchResult]) -> String {
        // Cline sends every turn — keep very compact
        let mut out = String::new();

        for r in memories
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .take(5)
        {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            out.push_str(&format!("ALWAYS: {}\n", text));
        }

        let refs: Vec<_> = memories
            .iter()
            .filter(|r| r.memory.priority != Priority::Must)
            .take(5)
            .collect();

        for r in &refs {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            out.push_str(&format!("NOTE: {}\n", text));
        }

        self.truncate_to(&out, self.config.max_chars_compact)
    }

    /// Run sync in a polling watch loop. Blocks indefinitely; suitable for daemon mode.
    /// `stop_signal` — set to true to gracefully stop the loop.
    pub async fn sync_with_watch(
        &self,
        project_dir: &Path,
        stop_signal: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut last_hash = self.store.sync_state_hash().await?;

        // Run initial sync immediately
        let report = self.sync(project_dir).await?;
        info!(
            files = report.files_written.len(),
            memories = report.memories_synced,
            "initial sync complete"
        );

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.watch_interval_secs,
        ));

        while !stop_signal.load(Ordering::Relaxed) {
            interval.tick().await;

            match self.store.sync_state_hash().await {
                Ok(hash) => {
                    if hash != last_hash {
                        debug!("change detected, re-syncing");
                        if let Err(e) = self.sync(project_dir).await {
                            warn!(error = %e, "sync failed on change");
                        }
                        last_hash = hash;
                    }
                }
                Err(e) => warn!(error = %e, "failed to check sync state"),
            }
        }

        info!("sync watch loop stopped");
        Ok(())
    }

    fn truncate_to(&self, content: &str, max_chars: usize) -> String {
        if content.len() <= max_chars {
            content.to_string()
        } else {
            let truncated: String = content.chars().take(max_chars - 30).collect();
            format!("{}\n\n<!-- truncated by MemVault -->", truncated)
        }
    }
}

/// A sync target that was NOT written, with the reason. Coverage accounting:
/// "which targets did this sync cover" must be answerable without reading
/// the config, and deliberate omissions must be distinguishable from gaps.
#[derive(Debug, Clone)]
pub struct SyncSkip {
    pub target: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct SyncReport {
    pub files_written: Vec<PathBuf>,
    pub memories_synced: usize,
    pub files_skipped: Vec<SyncSkip>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    async fn setup() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".into(),
            agent_type: "general".into(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Preference,
            "use Python".into(),
            Priority::Must,
            agent.clone(),
        );
        m1.instruction = Some("Always use Python, never Java".into());
        m1.tags = vec!["coding".into(), "python".into()];

        let mut m2 = Memory::new(
            MemoryType::Fact,
            "project uses FastAPI".into(),
            Priority::Reference,
            agent.clone(),
        );
        m2.tags = vec!["coding".into(), "python".into()];

        let m3 = Memory::new(
            MemoryType::Preference,
            "writing style concise".into(),
            Priority::Reference,
            agent,
        );

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();
        store.save(m3).await.unwrap();
        store
    }

    #[test]
    fn test_detect_project_with_cargo() {
        let dir = std::env::temp_dir().join("memvault_test_detect");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"my-app\"\n").unwrap();

        let ctx = SyncEngine::detect_project(&dir);
        assert_eq!(ctx.name, "my-app");
        assert!(ctx.tech_stack.contains(&"rust".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn test_select_memories() {
        let store = setup().await;
        let engine = SyncEngine::new(store);

        let ctx = ProjectContext {
            name: "test-project".into(),
            tech_stack: vec!["python".into()],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        assert!(!memories.is_empty());
        // MUST should be first
        assert_eq!(memories[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_select_memories_excludes_archived_namespace() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".into(),
            agent_type: "general".into(),
            session_id: None,
        };

        let mut archived = Memory::new(
            MemoryType::Fact,
            "old archived fact".into(),
            Priority::Reference,
            agent,
        );
        archived.namespace = "archived:old-project".into();
        store.save(archived).await.unwrap();

        let engine = SyncEngine::new(store);
        let ctx = ProjectContext {
            name: "test-project".into(),
            tech_stack: vec![],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        assert!(
            memories
                .iter()
                .all(|m| !m.memory.namespace.starts_with("archived:")),
            "archived-namespace memories must never be selected for sync"
        );
    }

    #[tokio::test]
    async fn test_select_memories_excludes_low_decay_score() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".into(),
            agent_type: "general".into(),
            session_id: None,
        };

        let mut decayed = Memory::new(
            MemoryType::Fact,
            "faded memory".into(),
            Priority::Reference,
            agent,
        );
        decayed.decay_score = 0.1; // below the 0.3 relevance threshold
        store.save(decayed).await.unwrap();

        let engine = SyncEngine::new(store);
        let ctx = ProjectContext {
            name: "test-project".into(),
            tech_stack: vec![],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        assert!(
            memories.iter().all(|m| m.memory.content != "faded memory"),
            "heavily decayed memories must be excluded from sync selection"
        );
    }

    #[tokio::test]
    async fn test_select_memories_boosts_project_namespace_match() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".into(),
            agent_type: "general".into(),
            session_id: None,
        };

        let mut project_mem = Memory::new(
            MemoryType::Fact,
            "project-specific detail".into(),
            Priority::Reference,
            agent.clone(),
        );
        project_mem.namespace = "project:my-app".into();

        let global_mem = Memory::new(
            MemoryType::Fact,
            "unrelated global detail".into(),
            Priority::Reference,
            agent,
        );

        store.save(project_mem).await.unwrap();
        store.save(global_mem).await.unwrap();

        let engine = SyncEngine::new(store);
        let ctx = ProjectContext {
            name: "my-app".into(),
            tech_stack: vec![],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        let project_result = memories
            .iter()
            .find(|m| m.memory.content == "project-specific detail")
            .expect("project-namespace memory should be selected");
        let global_result = memories
            .iter()
            .find(|m| m.memory.content == "unrelated global detail")
            .expect("global memory should still be selected at base relevance");

        assert!(
            project_result.score > global_result.score,
            "project-namespace match should score higher than a global-namespace memory"
        );
    }

    #[tokio::test]
    async fn test_generate_claude_md() {
        let store = setup().await;
        let engine = SyncEngine::new(store);

        let ctx = ProjectContext {
            name: "test".into(),
            tech_stack: vec!["python".into()],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        let content = engine.generate_claude_md(&memories);

        assert!(content.contains("# Memory Context"));
        assert!(content.contains("Always use Python"));
        assert!(content.contains("## Rules (MUST follow)"));
    }

    #[tokio::test]
    async fn test_generate_agents_md() {
        let store = setup().await;
        let engine = SyncEngine::new(store);

        let ctx = ProjectContext {
            name: "test".into(),
            tech_stack: vec!["python".into()],
            root: PathBuf::from("/tmp"),
        };

        let memories = engine.select_memories(&ctx).await.unwrap();
        let content = engine.generate_agents_md(&memories);

        assert!(content.contains("# Project Instructions"));
        assert!(content.contains("## Mandatory Rules"));
    }

    #[tokio::test]
    async fn test_sync_writes_files() {
        let store = setup().await;
        let engine = SyncEngine::new(store);

        let dir = std::env::temp_dir().join("memvault_test_sync");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"test-proj\"\n").unwrap();

        let report = engine.sync(&dir).await.unwrap();

        assert!(report.files_written.len() >= 4);
        assert!(dir.join("CLAUDE.md").exists());
        assert!(dir.join("AGENTS.md").exists());
        assert!(dir.join(".clinerules").exists());

        let claude_content = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(claude_content.contains("Always use Python"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- detect_project across stacks ---

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memvault_detect_{}_{}",
            name,
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_detect_project_package_json_takes_name_priority() {
        let dir = temp_dir("pkgjson");
        std::fs::write(
            dir.join("package.json"),
            r#"{"name": "my-web-app", "version": "1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("tsconfig.json"), "{}").unwrap();

        let ctx = SyncEngine::detect_project(&dir);
        assert_eq!(ctx.name, "my-web-app", "package.json name should win");
        assert!(ctx.tech_stack.contains(&"javascript".to_string()));
        assert!(ctx.tech_stack.contains(&"typescript".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_project_multi_file_stack() {
        let dir = temp_dir("multistack");
        std::fs::write(dir.join("go.mod"), "module x").unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[project]").unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM scratch").unwrap();
        std::fs::write(dir.join("Gemfile"), "source :rubygems").unwrap();
        std::fs::write(dir.join("pom.xml"), "<project/>").unwrap();

        let ctx = SyncEngine::detect_project(&dir);
        assert!(ctx.tech_stack.contains(&"go".to_string()));
        assert!(ctx.tech_stack.contains(&"python".to_string()));
        assert!(ctx.tech_stack.contains(&"docker".to_string()));
        assert!(ctx.tech_stack.contains(&"ruby".to_string()));
        assert!(ctx.tech_stack.contains(&"java".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_project_cargo_toml_name() {
        let dir = temp_dir("cargoname");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"some-crate\"\n").unwrap();
        let ctx = SyncEngine::detect_project(&dir);
        assert_eq!(ctx.name, "some-crate");
        assert!(ctx.tech_stack.contains(&"rust".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_project_empty_dir_uses_folder_name() {
        let dir = temp_dir("bare");
        let ctx = SyncEngine::detect_project(&dir);
        let folder = dir.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(ctx.name, folder);
        assert!(ctx.tech_stack.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- config accessors ---

    #[test]
    fn test_with_config_and_accessor() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let cfg = SyncConfig {
            generate_copilot: false,
            generate_cursorrules: false,
            generate_clinerules: false,
            max_chars_compact: 100,
            max_chars_full: 200,
            ..SyncConfig::default()
        };
        let engine = SyncEngine::new(store).with_config(cfg.clone());
        assert_eq!(engine.config().max_chars_full, 200);
        assert!(!engine.config().generate_copilot);
    }

    // --- format generators ---

    fn mock_result(content: &str, priority: Priority, agent: &SourceAgent) -> SearchResult {
        SearchResult {
            memory: Memory::new(MemoryType::Fact, content.into(), priority, agent.clone()),
            score: 0.9,
            hit_sources: Vec::new(),
        }
    }

    #[test]
    fn test_generate_copilot_md_empty() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let content = engine.generate_copilot_md(&[]);
        assert!(content.contains("# Copilot Instructions"));
    }

    #[test]
    fn test_generate_cursorrules_must_only() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let agent = SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        let results = vec![mock_result("always do X", Priority::Must, &agent)];
        let content = engine.generate_cursorrules(&results);
        assert!(content.contains("- always"));
    }

    #[test]
    fn test_generate_clinerules_formats() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let agent = SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        let must = mock_result("do this", Priority::Must, &agent);
        let mut note = mock_result("remember that", Priority::Reference, &agent);
        note.memory.tags = vec!["coding".into()];

        let content = engine.generate_clinerules(&[must, note]);
        assert!(content.contains("ALWAYS: do this"));
        assert!(content.contains("NOTE: remember that"));
    }

    #[test]
    fn test_truncate_to_keeps_short() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let out = engine.truncate_to("short", 100);
        assert_eq!(out, "short");
    }

    #[test]
    fn test_truncate_to_truncates_long() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let long = "a".repeat(500);
        let out = engine.truncate_to(&long, 50);
        assert!(out.ends_with("<!-- truncated by MemVault -->"));
        assert!(out.len() < long.len());
    }

    #[tokio::test]
    async fn test_sync_respects_generate_flags() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        store
            .save(Memory::new(
                MemoryType::Preference,
                "use Go for services".into(),
                Priority::Must,
                agent,
            ))
            .await
            .unwrap();

        let cfg = SyncConfig {
            generate_claude_md: true,
            generate_agents_md: false,
            generate_copilot: false,
            generate_cursorrules: false,
            generate_clinerules: false,
            ..SyncConfig::default()
        };
        let engine = SyncEngine::new(store).with_config(cfg);

        let dir = temp_dir("flags");
        let report = engine.sync(&dir).await.unwrap();
        assert!(dir.join("CLAUDE.md").exists());
        assert!(!dir.join("AGENTS.md").exists());
        assert!(
            !report
                .files_written
                .iter()
                .any(|p| p.ends_with("AGENTS.md"))
        );
        assert_eq!(report.memories_synced, 1);

        // Coverage accounting: every disabled target is reported as skipped
        // with a reason — written + skipped must cover all known targets.
        let skipped_targets: Vec<&str> = report
            .files_skipped
            .iter()
            .map(|s| s.target.as_str())
            .collect();
        assert_eq!(skipped_targets.len(), 4);
        assert!(skipped_targets.contains(&"AGENTS.md"));
        assert!(skipped_targets.contains(&".cursorrules"));
        assert!(skipped_targets.contains(&".clinerules"));
        assert!(skipped_targets.contains(&".github/copilot-instructions.md"));
        for skip in &report.files_skipped {
            assert!(!skip.reason.is_empty(), "skips must carry a reason");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// All targets enabled → nothing skipped.
    #[tokio::test]
    async fn test_sync_full_coverage_no_skips() {
        let store = Arc::new(crate::storage::sqlite::SqliteStore::in_memory().unwrap());
        let engine = SyncEngine::new(store);
        let dir = temp_dir("full_coverage");
        let report = engine.sync(&dir).await.unwrap();
        assert!(report.files_skipped.is_empty());
        assert_eq!(report.files_written.len(), 5);
        std::fs::remove_dir_all(&dir).ok();
    }
}
