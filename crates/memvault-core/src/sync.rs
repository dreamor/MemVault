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
        };

        if self.config.generate_claude_md {
            let content = self.generate_claude_md(&memories);
            let path = project_dir.join("CLAUDE.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }

        if self.config.generate_agents_md {
            let content = self.generate_agents_md(&memories);
            let path = project_dir.join("AGENTS.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }

        if self.config.generate_copilot {
            let content = self.generate_copilot_md(&memories);
            let dir = project_dir.join(".github");
            std::fs::create_dir_all(&dir).ok();
            let path = dir.join("copilot-instructions.md");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }

        if self.config.generate_cursorrules {
            let content = self.generate_cursorrules(&memories);
            let path = project_dir.join(".cursorrules");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }

        if self.config.generate_clinerules {
            let content = self.generate_clinerules(&memories);
            let path = project_dir.join(".clinerules");
            self.write_if_changed(&path, &content)?;
            report.files_written.push(path);
        }

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

#[derive(Debug)]
pub struct SyncReport {
    pub files_written: Vec<PathBuf>,
    pub memories_synced: usize,
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
            memories.iter().all(|m| !m.memory.namespace.starts_with("archived:")),
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
}
