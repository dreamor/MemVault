use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

use crate::error::{MemVaultError, Result};
use crate::models::*;
use crate::storage::MemoryStore;

/// JSON-escape a string for use as a YAML double-quoted scalar. JSON string
/// escaping (`\"`, `\\`, `\n`, ...) is a valid subset of YAML double-quote
/// escaping, so this is safe for arbitrary free text (ids, evidence notes)
/// without pulling in a YAML-specific escaper.
fn json_scalar(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Wrap a string as a YAML single-quoted scalar, doubling embedded single
/// quotes per YAML's escaping rule. Used to embed a compact JSON blob (e.g.
/// `skill_meta`) without JSON's own `"`/`\` escaping colliding with YAML's.
fn yaml_single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub struct Exporter {
    store: Arc<dyn MemoryStore>,
}

impl Exporter {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    /// Export all memories as JSON.
    pub async fn export_json(&self, namespace: Option<&str>) -> Result<String> {
        let memories = self.store.list(namespace, 100000, 0).await?;
        let json = serde_json::to_string_pretty(&memories)?;
        info!(count = memories.len(), "exported as JSON");
        Ok(json)
    }

    /// Export all memories as Markdown files content.
    /// Returns Vec<(filename, content)>.
    pub async fn export_markdown(&self, namespace: Option<&str>) -> Result<Vec<(String, String)>> {
        let memories = self.store.list(namespace, 100000, 0).await?;
        let mut files = Vec::new();

        for mem in &memories {
            let filename = format!("{}.md", mem.id);
            let content = Self::memory_to_markdown(mem);
            files.push((filename, content));
        }

        info!(count = files.len(), "exported as Markdown");
        Ok(files)
    }

    /// Export to a directory, creating one .md file per memory.
    pub async fn export_to_dir(&self, dir: &Path, namespace: Option<&str>) -> Result<usize> {
        std::fs::create_dir_all(dir)
            .map_err(|e| MemVaultError::Storage(format!("Failed to create export dir: {}", e)))?;

        let files = self.export_markdown(namespace).await?;
        for (filename, content) in &files {
            let path = dir.join(filename);
            std::fs::write(&path, content).map_err(|e| {
                MemVaultError::Storage(format!("Failed to write {}: {}", path.display(), e))
            })?;
        }

        Ok(files.len())
    }

    /// Render a memory to Markdown with YAML frontmatter, losslessly
    /// covering every `Memory` field (not just the ones a human would
    /// want to eyeball) so this is a safe round-trip format for backends
    /// that treat the file as canonical, not just a human-readable export.
    ///
    /// `pub(crate)` so [`crate::storage::markdown::MarkdownStore`] can reuse
    /// the exact same render for its canonical per-memory files and for
    /// diffing a file against its cached row to detect external edits.
    pub(crate) fn memory_to_markdown(mem: &Memory) -> String {
        let priority_str = serde_json::to_string(&mem.priority).unwrap_or_default();
        let type_str = serde_json::to_string(&mem.memory_type).unwrap_or_default();
        let layer_str = serde_json::to_string(&mem.layer).unwrap_or_default();

        let mut lines = vec![
            format!("id: {}", mem.id),
            format!("type: {}", type_str.trim_matches('"')),
            format!("priority: {}", priority_str.trim_matches('"')),
            format!("namespace: {}", mem.namespace),
            format!("tags: [{}]", mem.tags.join(", ")),
            format!("confidence: {}", mem.confidence),
            format!("source_agent: {}", mem.source_agent.id),
            format!("source_agent_type: {}", mem.source_agent.agent_type),
            format!("human_reviewed: {}", mem.human_reviewed),
            format!("ai_generated: {}", mem.ai_generated),
            format!("identity_verified: {}", mem.identity_verified),
            format!("decay_score: {}", mem.decay_score),
            format!("access_count: {}", mem.access_count),
            format!("layer: {}", layer_str.trim_matches('"')),
            format!("visibility: {}", mem.visibility.as_str()),
            format!("created: {}", mem.created_at.to_rfc3339()),
            format!("updated: {}", mem.updated_at.to_rfc3339()),
        ];

        if let Some(occurred) = mem.occurred_at {
            lines.push(format!("occurred: {}", occurred.to_rfc3339()));
        }
        if let Some(last_read) = mem.last_read_at {
            lines.push(format!("last_read: {}", last_read.to_rfc3339()));
        }
        if let Some(session_id) = &mem.source_agent.session_id {
            lines.push(format!("source_session_id: {}", json_scalar(session_id)));
        }
        if let Some(superseded) = &mem.superseded_by {
            lines.push(format!("superseded_by: {}", json_scalar(superseded)));
        }
        if !mem.corroborating_agents.is_empty() {
            lines.push(format!(
                "corroborating_agents: [{}]",
                mem.corroborating_agents.join(", ")
            ));
        }
        if !mem.source_trace_ids.is_empty() {
            lines.push(format!(
                "source_trace_ids: [{}]",
                mem.source_trace_ids.join(", ")
            ));
        }
        if let Some(evidence) = &mem.friction_evidence {
            lines.push(format!("friction_evidence: {}", json_scalar(evidence)));
        }
        if let Some(meta) = &mem.skill_meta {
            let json = serde_json::to_string(meta).unwrap_or_default();
            lines.push(format!("skill_meta: {}", yaml_single_quoted(&json)));
        }

        let instruction_line = mem
            .instruction
            .as_deref()
            .map(|i| format!("\n> Instruction: {}\n", i))
            .unwrap_or_default();

        format!(
            "---\n{}\n---\n\n{}\n{}\n",
            lines.join("\n"),
            mem.content,
            instruction_line
        )
    }
}

/// Result of an import run: how many memories were written, and which
/// ids were skipped because they already exist in the store (idempotent
/// import — re-importing an export into the same database is a no-op for
/// the overlapping ids instead of failing on the UNIQUE constraint).
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub imported: usize,
    /// Memory ids already present in the store that were not re-inserted.
    pub skipped: Vec<String>,
}

pub struct Importer {
    store: Arc<dyn MemoryStore>,
}

impl Importer {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    /// Whether a memory id already exists in the store.
    async fn exists(&self, id: &str) -> Result<bool> {
        match self.store.get(id).await {
            Ok(_) => Ok(true),
            Err(MemVaultError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Import memories from JSON string. Ids already present are skipped
    /// and reported in `ImportReport::skipped` rather than failing.
    pub async fn import_json(&self, json: &str) -> Result<ImportReport> {
        let memories: Vec<Memory> = serde_json::from_str(json)
            .map_err(|e| MemVaultError::InvalidInput(format!("Invalid JSON: {}", e)))?;

        let mut report = ImportReport::default();
        for mem in memories {
            if self.exists(&mem.id).await? {
                debug!(id = %mem.id, "skipping already-existing memory on json import");
                report.skipped.push(mem.id);
                continue;
            }
            self.store.save(mem).await?;
            report.imported += 1;
        }

        info!(
            imported = report.imported,
            skipped = report.skipped.len(),
            "imported from JSON"
        );
        Ok(report)
    }

    /// Import from a Markdown file with YAML frontmatter.
    pub fn parse_markdown(content: &str) -> Result<Memory> {
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Err(MemVaultError::InvalidInput(
                "Missing YAML frontmatter".to_string(),
            ));
        }

        let frontmatter = parts[1].trim();
        let body = parts[2].trim();

        #[derive(serde::Deserialize)]
        struct FrontMatter {
            id: Option<String>,
            #[serde(rename = "type")]
            memory_type: Option<String>,
            priority: Option<String>,
            namespace: Option<String>,
            tags: Option<Vec<String>>,
            confidence: Option<f64>,
            source_agent: Option<String>,
            source_agent_type: Option<String>,
            source_session_id: Option<String>,
            occurred: Option<String>,
            last_read: Option<String>,
            human_reviewed: Option<bool>,
            ai_generated: Option<bool>,
            identity_verified: Option<bool>,
            decay_score: Option<f64>,
            access_count: Option<u32>,
            layer: Option<String>,
            visibility: Option<String>,
            superseded_by: Option<String>,
            corroborating_agents: Option<Vec<String>>,
            source_trace_ids: Option<Vec<String>>,
            friction_evidence: Option<String>,
            skill_meta: Option<String>,
        }

        let fm: FrontMatter = serde_yaml::from_str(frontmatter)
            .map_err(|e| MemVaultError::InvalidInput(format!("Invalid frontmatter: {}", e)))?;

        let memory_type = fm
            .memory_type
            .and_then(|t| serde_json::from_str(&format!("\"{}\"", t)).ok())
            .unwrap_or(MemoryType::Fact);

        let priority = fm
            .priority
            .and_then(|p| serde_json::from_str(&format!("\"{}\"", p)).ok())
            .unwrap_or(Priority::Reference);

        let (content_text, instruction) = if let Some(idx) = body.find("\n> Instruction: ") {
            let content = body[..idx].trim().to_string();
            let inst = body[idx + 16..].trim().to_string();
            (content, Some(inst))
        } else {
            (body.to_string(), None)
        };

        let agent_id = fm.source_agent.unwrap_or_else(|| "import".to_string());
        let mut mem = Memory::new(
            memory_type,
            content_text,
            priority,
            SourceAgent {
                id: agent_id,
                agent_type: "import".to_string(),
                session_id: None,
            },
        );

        // Canonical markdown files may still omit an explicit `id:`
        // line in extremely minimal frontmatter. Preserve the original
        // pre-insert identity for that case.
        if let Some(id) = fm.id {
            mem.id = id;
        }
        mem.namespace = fm.namespace.unwrap_or_else(|| "global".to_string());
        mem.tags = fm.tags.unwrap_or_default();
        mem.confidence = fm.confidence.unwrap_or(0.8);
        mem.instruction = instruction;
        // Default to "not ai-generated" for plain hand-authored imports;
        // a round-tripped export carries the real value in `ai_generated`
        // and overrides this below.
        mem.ai_generated = false;
        if let Some(agent_type) = fm.source_agent_type {
            mem.source_agent.agent_type = agent_type;
        }
        mem.source_agent.session_id = fm.source_session_id;
        if let Some(hr) = fm.human_reviewed {
            mem.human_reviewed = hr;
        }
        if let Some(ag) = fm.ai_generated {
            mem.ai_generated = ag;
        }
        if let Some(iv) = fm.identity_verified {
            mem.identity_verified = iv;
        }
        if let Some(ds) = fm.decay_score {
            mem.decay_score = ds;
        }
        if let Some(ac) = fm.access_count {
            mem.access_count = ac;
        }
        if let Some(layer) = fm
            .layer
            .and_then(|l| serde_json::from_str::<MemoryLayer>(&format!("\"{}\"", l)).ok())
        {
            mem.layer = layer;
        }
        if let Some(vis) = fm.visibility {
            mem.visibility = Visibility::parse(&vis);
        }
        mem.superseded_by = fm.superseded_by;
        mem.corroborating_agents = fm.corroborating_agents.unwrap_or_default();
        mem.source_trace_ids = fm.source_trace_ids.unwrap_or_default();
        mem.friction_evidence = fm.friction_evidence;
        mem.skill_meta = fm
            .skill_meta
            .and_then(|s| serde_json::from_str::<SkillMeta>(&s).ok());
        // Event-time provenance survives the markdown round-trip when the
        // export carried it; unparseable values are dropped (lossy format,
        // never fail an import over one optional field).
        mem.occurred_at = fm.occurred.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        });
        mem.last_read_at = fm.last_read.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        });

        Ok(mem)
    }

    /// Import all .md files from a directory.
    pub async fn import_from_dir(&self, dir: &Path) -> Result<ImportReport> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| MemVaultError::Storage(format!("Failed to read dir: {}", e)))?;

        let mut report = ImportReport::default();
        for entry in entries {
            let entry = entry.map_err(|e| MemVaultError::Storage(e.to_string()))?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "md") {
                let content = std::fs::read_to_string(&path).map_err(|e| {
                    MemVaultError::Storage(format!("Failed to read {}: {}", path.display(), e))
                })?;

                match Self::parse_markdown(&content) {
                    Ok(mem) => {
                        if self.exists(&mem.id).await? {
                            report.skipped.push(mem.id);
                            continue;
                        }
                        self.store.save(mem).await?;
                        report.imported += 1;
                    }
                    Err(e) => {
                        debug!(file = %path.display(), error = %e, "skipping invalid file");
                    }
                }
            }
        }

        info!(
            imported = report.imported,
            skipped = report.skipped.len(),
            "imported from directory"
        );
        Ok(report)
    }

    /// Import a single Markdown file with YAML frontmatter.
    /// Unlike `import_from_dir`, a malformed file is an error (loud) — the
    /// caller explicitly named this file, so a silent skip would be confusing.
    pub async fn import_markdown_file(&self, path: &Path) -> Result<ImportReport> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            MemVaultError::Storage(format!("Failed to read {}: {}", path.display(), e))
        })?;
        let mem = Self::parse_markdown(&content)?;
        let mut report = ImportReport::default();
        if self.exists(&mem.id).await? {
            debug!(id = %mem.id, "skipping already-existing memory on single-file import");
            report.skipped.push(mem.id);
        } else {
            self.store.save(mem).await?;
            report.imported = 1;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    async fn setup() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Preference,
            "prefers Python".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m1.instruction = Some("Use Python, not Java".to_string());
        m1.tags = vec!["coding".to_string()];

        store.save(m1).await.unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "uses FastAPI".to_string(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn test_export_json() {
        let store = setup().await;
        let exporter = Exporter::new(store);

        let json = exporter.export_json(None).await.unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[tokio::test]
    async fn test_export_markdown() {
        let store = setup().await;
        let exporter = Exporter::new(store);

        let files = exporter.export_markdown(None).await.unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].0.ends_with(".md"));
        assert!(files[0].1.contains("---"));
    }

    #[tokio::test]
    async fn test_import_json() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store.clone());

        let json = r#"[{
            "id": "mem_test1",
            "type": "fact",
            "content": "imported fact",
            "instruction": null,
            "priority": "REFERENCE",
            "source_agent": {"id": "import", "agent_type": "test", "session_id": null},
            "namespace": "global",
            "confidence": 0.8,
            "tags": [],
            "created_at": "2026-08-07T00:00:00Z",
            "updated_at": "2026-08-07T00:00:00Z",
            "ai_generated": false,
            "human_reviewed": false,
            "decay_score": 1.0,
            "access_count": 0
        }]"#;

        let report = importer.import_json(json).await.unwrap();
        assert_eq!(report.imported, 1);
        assert!(report.skipped.is_empty());

        let mem = store.get("mem_test1").await.unwrap();
        assert_eq!(mem.content, "imported fact");
    }

    #[test]
    fn test_parse_markdown() {
        let md = "---\nid: mem_md1\ntype: preference\npriority: MUST\ntags: [coding]\n---\n\nprefers Python\n\n> Instruction: Use Python";
        let mem = Importer::parse_markdown(md).unwrap();
        assert_eq!(mem.id, "mem_md1");
        assert_eq!(mem.memory_type, MemoryType::Preference);
        assert_eq!(mem.priority, Priority::Must);
        assert!(mem.instruction.is_some());
    }

    #[tokio::test]
    async fn test_roundtrip_json() {
        let store1 = setup().await;
        let exporter = Exporter::new(store1);
        let json = exporter.export_json(None).await.unwrap();

        let store2 = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store2.clone());
        let report = importer.import_json(&json).await.unwrap();
        assert_eq!(report.imported, 2);
        assert!(report.skipped.is_empty());

        let all = store2.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_export_empty_namespace() {
        let store = setup().await;
        let exporter = Exporter::new(store);

        let json = exporter.export_json(Some("nonexistent-ns")).await.unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_empty());
    }

    #[tokio::test]
    async fn test_import_from_dir() {
        let dir = std::env::temp_dir().join("memvault_test_import");
        std::fs::create_dir_all(&dir).unwrap();

        let md =
            "---\nid: mem_import_dir\ntype: fact\npriority: REFERENCE\n---\n\nimported from dir\n";
        std::fs::write(dir.join("test.md"), md).unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store.clone());
        let report = importer.import_from_dir(&dir).await.unwrap();
        assert_eq!(report.imported, 1);

        let mem = store.get("mem_import_dir").await.unwrap();
        assert_eq!(mem.content, "imported from dir");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_export_namespace_filtered() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Fact,
            "global memory".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m1.namespace = "global".to_string();

        let mut m2 = Memory::new(
            MemoryType::Fact,
            "project memory".to_string(),
            Priority::Reference,
            agent,
        );
        m2.namespace = "project:myapp".to_string();

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let exporter = Exporter::new(store);
        let json = exporter.export_json(Some("project:myapp")).await.unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["content"], "project memory");
    }

    #[test]
    fn test_parse_markdown_missing_fields() {
        // Minimal frontmatter — should use defaults
        let md = "---\nid: mem_minimal\n---\n\njust content";
        let mem = Importer::parse_markdown(md).unwrap();
        assert_eq!(mem.id, "mem_minimal");
        assert_eq!(mem.memory_type, MemoryType::Fact); // default
        assert_eq!(mem.priority, Priority::Reference); // default
    }

    #[test]
    fn test_parse_markdown_invalid_frontmatter() {
        // No frontmatter at all
        let md = "just plain text without frontmatter";
        let result = Importer::parse_markdown(md);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_export_to_dir_writes_files() {
        let store = setup().await;
        let exporter = Exporter::new(store);

        let dir = std::env::temp_dir().join(format!(
            "memvault_io_dir_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        let count = exporter.export_to_dir(&dir, None).await.unwrap();
        assert_eq!(count, 2);

        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 2);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_export_to_dir_empty_namespace() {
        let store = setup().await;
        let exporter = Exporter::new(store);

        let dir = std::env::temp_dir().join(format!(
            "memvault_io_empty_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        let count = exporter
            .export_to_dir(&dir, Some("no-such-ns"))
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert!(dir.is_dir(), "directory should still be created");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_from_dir_empty_returns_zero() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_io_empty_dir_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store);
        let report = importer.import_from_dir(&dir).await.unwrap();
        assert_eq!(report.imported, 0);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_from_dir_skips_invalid_and_non_md() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_io_mixed_{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Valid markdown
        std::fs::write(dir.join("good.md"), "---\nid: mem_good\n---\n\nfine").unwrap();
        // Invalid frontmatter
        std::fs::write(dir.join("bad.md"), "no frontmatter here").unwrap();
        // Non-markdown file
        std::fs::write(
            dir.join("notes.txt"),
            "---\nid: mem_ignored\ntype: fact\n---\n\nx",
        )
        .unwrap();

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store.clone());
        let report = importer.import_from_dir(&dir).await.unwrap();
        assert_eq!(report.imported, 1, "only the valid .md file should import");
        assert!(store.get("mem_good").await.is_ok());
        assert!(store.get("mem_ignored").await.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_json_skips_existing_ids() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let importer = Importer::new(store.clone());

        let json = r#"[{
            "id": "mem_dup",
            "type": "fact",
            "content": "existing fact",
            "instruction": null,
            "priority": "REFERENCE",
            "source_agent": {"id": "import", "agent_type": "test", "session_id": null},
            "namespace": "global",
            "confidence": 0.8,
            "tags": [],
            "created_at": "2026-08-07T00:00:00Z",
            "updated_at": "2026-08-07T00:00:00Z",
            "ai_generated": false,
            "human_reviewed": false,
            "decay_score": 1.0,
            "access_count": 0
        }]"#;

        let first = importer.import_json(json).await.unwrap();
        assert_eq!(first.imported, 1);
        assert!(first.skipped.is_empty());

        // Re-importing the same export must not fail on UNIQUE; the existing
        // id is skipped and reported.
        let second = importer.import_json(json).await.unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped, vec!["mem_dup".to_string()]);
        assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 1);
    }

    #[test]
    fn test_parse_markdown_full_frontmatter() {
        let md = "---\nid: mem_full\ntype: fact\npriority: MUST\nnamespace: project:z\nconfidence: 0.9\nsource_agent: bob\ntags: [a, b]\n---\n\nsome content";
        let mem = Importer::parse_markdown(md).unwrap();
        assert_eq!(mem.id, "mem_full");
        assert_eq!(mem.namespace, "project:z");
        assert_eq!(mem.confidence, 0.9);
        assert_eq!(mem.source_agent.id, "bob");
        assert_eq!(mem.memory_type, MemoryType::Fact);
    }

    #[test]
    fn test_parse_markdown_invalid_yaml() {
        let md = "---\nid: [unclosed\n---\n\nbody";
        let result = Importer::parse_markdown(md);
        assert!(result.is_err());
    }

    #[test]
    fn test_markdown_roundtrip_preserves_occurred_at() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Fact,
            "happened at a specific time".to_string(),
            Priority::Reference,
            agent,
        );
        let occurred =
            chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2023, 5, 7, 13, 56, 0).unwrap();
        mem.occurred_at = Some(occurred);

        let md = Exporter::memory_to_markdown(&mem);
        assert!(
            md.contains("occurred: 2023-05-07T13:56:00+00:00\n"),
            "exported frontmatter must carry occurred, got:\n{md}"
        );

        let parsed = Importer::parse_markdown(&md).unwrap();
        assert_eq!(parsed.occurred_at, Some(occurred));
    }

    #[test]
    fn test_parse_markdown_occurred_invalid_dropped_lossy() {
        // Lossy by design: an unparseable `occurred` never fails the import.
        let md = "---\nid: mem_occ_bad\noccurred: yesterday\n---\n\nbody";
        let mem = Importer::parse_markdown(md).unwrap();
        assert_eq!(mem.occurred_at, None);
    }

    #[test]
    fn test_markdown_roundtrip_lossless_full_fields() {
        let agent = SourceAgent {
            id: "claude-code".to_string(),
            agent_type: "ide".to_string(),
            session_id: Some("sess_42".to_string()),
        };
        let mut mem = Memory::new(
            MemoryType::Skill,
            "deploy process".to_string(),
            Priority::Must,
            agent,
        );
        mem.instruction = Some("run the deploy checklist".to_string());
        mem.tags = vec!["ops".to_string(), "deploy".to_string()];
        mem.ai_generated = true;
        mem.human_reviewed = true;
        mem.identity_verified = true;
        mem.decay_score = 0.42;
        mem.access_count = 7;
        mem.last_read_at =
            Some(chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2024, 1, 2, 3, 4, 5).unwrap());
        mem.layer = MemoryLayer::L3;
        mem.visibility = Visibility::Shared;
        mem.superseded_by = Some("mem_newer".to_string());
        mem.corroborating_agents = vec!["agent_a".to_string(), "agent_b".to_string()];
        mem.source_trace_ids = vec!["mem_trace1".to_string(), "mem_trace2".to_string()];
        mem.friction_evidence = Some("3 tool retries: nested, colons: here".to_string());
        mem.skill_meta = Some(SkillMeta {
            trigger: Some("deploy, prod: push".to_string()),
            steps: vec!["build".to_string(), "test, then push".to_string()],
            verification: Some("health check passes".to_string()),
            version: 3,
        });

        let md = Exporter::memory_to_markdown(&mem);
        let parsed = Importer::parse_markdown(&md).unwrap();

        assert_eq!(parsed.id, mem.id);
        assert_eq!(parsed.memory_type, mem.memory_type);
        assert_eq!(parsed.content, mem.content);
        assert_eq!(parsed.instruction, mem.instruction);
        assert_eq!(parsed.priority, mem.priority);
        assert_eq!(parsed.tags, mem.tags);
        assert_eq!(parsed.source_agent.id, mem.source_agent.id);
        assert_eq!(parsed.source_agent.agent_type, mem.source_agent.agent_type);
        assert_eq!(parsed.source_agent.session_id, mem.source_agent.session_id);
        assert_eq!(parsed.ai_generated, mem.ai_generated);
        assert_eq!(parsed.human_reviewed, mem.human_reviewed);
        assert_eq!(parsed.identity_verified, mem.identity_verified);
        assert_eq!(parsed.decay_score, mem.decay_score);
        assert_eq!(parsed.access_count, mem.access_count);
        assert_eq!(parsed.last_read_at, mem.last_read_at);
        assert_eq!(parsed.layer, mem.layer);
        assert_eq!(parsed.visibility, mem.visibility);
        assert_eq!(parsed.superseded_by, mem.superseded_by);
        assert_eq!(parsed.corroborating_agents, mem.corroborating_agents);
        assert_eq!(parsed.source_trace_ids, mem.source_trace_ids);
        assert_eq!(parsed.friction_evidence, mem.friction_evidence);
        assert_eq!(
            parsed.skill_meta.as_ref().map(|m| &m.trigger),
            mem.skill_meta.as_ref().map(|m| &m.trigger)
        );
        assert_eq!(
            parsed.skill_meta.as_ref().map(|m| &m.steps),
            mem.skill_meta.as_ref().map(|m| &m.steps)
        );
        assert_eq!(
            parsed.skill_meta.as_ref().map(|m| m.version),
            mem.skill_meta.as_ref().map(|m| m.version)
        );
    }

    #[test]
    fn test_markdown_roundtrip_defaults_without_optional_fields() {
        // A memory with no optional provenance set must still round-trip
        // its mandatory scalar fields and emit none of the optional lines.
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent,
        );
        let md = Exporter::memory_to_markdown(&mem);
        assert!(!md.contains("superseded_by:"));
        assert!(!md.contains("corroborating_agents:"));
        assert!(!md.contains("source_trace_ids:"));
        assert!(!md.contains("friction_evidence:"));
        assert!(!md.contains("skill_meta:"));
        assert!(!md.contains("last_read:"));
        assert!(!md.contains("source_session_id:"));

        let parsed = Importer::parse_markdown(&md).unwrap();
        assert_eq!(parsed.decay_score, mem.decay_score);
        assert_eq!(parsed.access_count, mem.access_count);
        assert_eq!(parsed.layer, mem.layer);
        assert_eq!(parsed.visibility, mem.visibility);
        assert!(parsed.superseded_by.is_none());
        assert!(parsed.skill_meta.is_none());
    }

    #[test]
    fn test_export_markdown_none_occurred_at_emits_no_line() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "no provenance".to_string(),
            Priority::Reference,
            agent,
        );
        let md = Exporter::memory_to_markdown(&mem);
        assert!(
            !md.contains("occurred:"),
            "None occurred_at must not emit a frontmatter line, got:\n{md}"
        );
    }
}
