use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

use crate::error::{MemVaultError, Result};
use crate::models::*;
use crate::storage::MemoryStore;

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
            std::fs::write(&path, content)
                .map_err(|e| MemVaultError::Storage(format!("Failed to write {}: {}", path.display(), e)))?;
        }

        Ok(files.len())
    }

    fn memory_to_markdown(mem: &Memory) -> String {
        let priority_str = serde_json::to_string(&mem.priority).unwrap_or_default();
        let type_str = serde_json::to_string(&mem.memory_type).unwrap_or_default();

        format!(
            "---\n\
            id: {}\n\
            type: {}\n\
            priority: {}\n\
            namespace: {}\n\
            tags: [{}]\n\
            confidence: {}\n\
            source_agent: {}\n\
            human_reviewed: {}\n\
            created: {}\n\
            updated: {}\n\
            ---\n\n\
            {}\n\
            {}\n",
            mem.id,
            type_str.trim_matches('"'),
            priority_str.trim_matches('"'),
            mem.namespace,
            mem.tags.join(", "),
            mem.confidence,
            mem.source_agent.id,
            mem.human_reviewed,
            mem.created_at.to_rfc3339(),
            mem.updated_at.to_rfc3339(),
            mem.content,
            mem.instruction.as_deref().map(|i| format!("\n> Instruction: {}\n", i)).unwrap_or_default(),
        )
    }
}

pub struct Importer {
    store: Arc<dyn MemoryStore>,
}

impl Importer {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    /// Import memories from JSON string.
    pub async fn import_json(&self, json: &str) -> Result<usize> {
        let memories: Vec<Memory> = serde_json::from_str(json)
            .map_err(|e| MemVaultError::InvalidInput(format!("Invalid JSON: {}", e)))?;

        let count = memories.len();
        for mem in memories {
            self.store.save(mem).await?;
        }

        info!(count, "imported from JSON");
        Ok(count)
    }

    /// Import from a Markdown file with YAML frontmatter.
    pub fn parse_markdown(content: &str) -> Result<Memory> {
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Err(MemVaultError::InvalidInput("Missing YAML frontmatter".to_string()));
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
        }

        let fm: FrontMatter = serde_yaml::from_str(frontmatter)
            .map_err(|e| MemVaultError::InvalidInput(format!("Invalid frontmatter: {}", e)))?;

        let memory_type = fm.memory_type
            .and_then(|t| serde_json::from_str(&format!("\"{}\"", t)).ok())
            .unwrap_or(MemoryType::Fact);

        let priority = fm.priority
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

        if let Some(id) = fm.id {
            mem.id = id;
        }
        mem.namespace = fm.namespace.unwrap_or_else(|| "global".to_string());
        mem.tags = fm.tags.unwrap_or_default();
        mem.confidence = fm.confidence.unwrap_or(0.8);
        mem.instruction = instruction;
        mem.ai_generated = false;

        Ok(mem)
    }

    /// Import all .md files from a directory.
    pub async fn import_from_dir(&self, dir: &Path) -> Result<usize> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| MemVaultError::Storage(format!("Failed to read dir: {}", e)))?;

        let mut count = 0;
        for entry in entries {
            let entry = entry.map_err(|e| MemVaultError::Storage(e.to_string()))?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "md") {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| MemVaultError::Storage(format!("Failed to read {}: {}", path.display(), e)))?;

                match Self::parse_markdown(&content) {
                    Ok(mem) => {
                        self.store.save(mem).await?;
                        count += 1;
                    }
                    Err(e) => {
                        debug!(file = %path.display(), error = %e, "skipping invalid file");
                    }
                }
            }
        }

        info!(count, "imported from directory");
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    async fn setup() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent { id: "test".to_string(), agent_type: "general".to_string(), session_id: None };

        let mut m1 = Memory::new(MemoryType::Preference, "prefers Python".to_string(), Priority::Must, agent.clone());
        m1.instruction = Some("Use Python, not Java".to_string());
        m1.tags = vec!["coding".to_string()];

        store.save(m1).await.unwrap();
        store.save(Memory::new(MemoryType::Fact, "uses FastAPI".to_string(), Priority::Reference, agent)).await.unwrap();

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

        let count = importer.import_json(json).await.unwrap();
        assert_eq!(count, 1);

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
        let count = importer.import_json(&json).await.unwrap();
        assert_eq!(count, 2);

        let all = store2.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 2);
    }
}
