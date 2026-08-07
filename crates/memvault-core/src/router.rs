use std::path::Path;
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::config::default_agent_registry;
use crate::error::Result;
use crate::intent::{self, Intent};
use crate::models::*;
use crate::storage::MemoryStore;

pub struct MemoryRouter {
    store: Arc<dyn MemoryStore>,
    registry: Vec<AgentProfile>,
}

impl MemoryRouter {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            registry: default_agent_registry(),
        }
    }

    pub fn with_registry(store: Arc<dyn MemoryStore>, registry: Vec<AgentProfile>) -> Self {
        let mut r = registry;
        if !r.iter().any(|a| a.id == "default") {
            r.push(AgentProfile {
                id: "default".to_string(),
                agent_type: "general-assistant".to_string(),
                description: "Default agent profile".to_string(),
                inject_rules: InjectRules::default(),
            });
        }
        Self { store, registry: r }
    }

    pub fn load_registry_from_yaml(store: Arc<dyn MemoryStore>, path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::error::MemVaultError::Storage(format!("Failed to read agent registry: {}", e)))?;

        let config: AgentRegistryConfig = serde_yaml::from_str(&content)
            .map_err(|e| crate::error::MemVaultError::InvalidInput(format!("Invalid agent registry YAML: {}", e)))?;

        info!("Loaded {} agent profiles from {}", config.agents.len(), path.display());
        Ok(Self::with_registry(store, config.agents))
    }

    pub fn get_agent_profile(&self, agent_id: &str) -> &AgentProfile {
        self.registry
            .iter()
            .find(|a| a.id == agent_id)
            .or_else(|| {
                let agent_type = agent_id.split('-').next().unwrap_or("");
                self.registry.iter().find(|a| a.agent_type.contains(agent_type))
            })
            .unwrap_or_else(|| self.registry.iter().find(|a| a.id == "default").unwrap())
    }

    pub async fn session_start(
        &self,
        agent_id: &str,
        context_hint: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let profile = self.get_agent_profile(agent_id);
        debug!(agent_id, agent_type = %profile.agent_type, "session_start");

        let intent = context_hint
            .map(intent::analyze_intent)
            .unwrap_or_else(|| intent::IntentResult {
                primary: Intent::General,
                domains: vec!["general".to_string()],
                confidence: 0.5,
            });
        debug!(intent = ?intent.primary, confidence = intent.confidence, "intent analyzed");

        let namespace = project
            .map(|p| format!("project:{}", p))
            .or_else(|| {
                profile.inject_rules.namespace_filter
                    .first()
                    .filter(|ns| *ns != "project:*")
                    .cloned()
            });

        let query = SearchQuery {
            query: String::new(), // session_start loads all memories, uses intent for filtering
            agent_id: Some(agent_id.to_string()),
            namespace,
            top_k: profile.inject_rules.max_memories * 2,
            ..SearchQuery::new(String::new())
        };

        let mut results = self.store.search(query).await?;

        // filter by agent's exclude_types (checks both memory_type and tags)
        // MUST memories are never excluded
        if !profile.inject_rules.exclude_types.is_empty() {
            results.retain(|r| {
                if r.memory.priority == Priority::Must {
                    return true;
                }
                let type_str = serde_json::to_string(&r.memory.memory_type).unwrap_or_default();
                let type_str = type_str.trim_matches('"');
                if profile.inject_rules.exclude_types.iter().any(|et| et.eq_ignore_ascii_case(type_str)) {
                    return false;
                }
                for tag in &r.memory.tags {
                    if profile.inject_rules.exclude_types.iter().any(|et| et.eq_ignore_ascii_case(tag)) {
                        return false;
                    }
                }
                true
            });
        }

        // filter by intent (skip MUST — they always pass)
        if intent.primary != Intent::General {
            results.retain(|r| {
                if r.memory.priority == Priority::Must {
                    return true;
                }
                !intent::should_exclude_for_intent(
                    &intent.primary,
                    &r.memory.tags,
                    &profile.inject_rules.exclude_types,
                )
            });
        }

        // trim to token budget
        let before_trim = results.len();
        Self::trim_to_budget(&mut results, profile.inject_rules.token_budget);

        // final cap on count
        results.truncate(profile.inject_rules.max_memories);

        let must_count = results.iter().filter(|r| r.memory.priority == Priority::Must).count();
        let ref_count = results.len() - must_count;
        debug!(
            agent_id,
            before_filter = before_trim,
            after = results.len(),
            must = must_count,
            r#ref = ref_count,
            token_budget = profile.inject_rules.token_budget,
            "session_start complete"
        );

        Ok(results)
    }

    pub fn format_as_instructions(&self, results: &[SearchResult]) -> String {
        if results.is_empty() {
            return String::new();
        }

        let mut must_lines = Vec::new();
        let mut ref_lines = Vec::new();
        let mut bg_lines = Vec::new();

        for r in results {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            match r.memory.priority {
                Priority::Must => must_lines.push(format!("[MUST] {}", text)),
                Priority::Reference => ref_lines.push(format!("[REF] {}", text)),
                Priority::Background => bg_lines.push(format!("[BG] {}", text)),
            }
        }

        let mut output = String::from("[MEMORY CONTEXT - 必须遵循]:\n");
        for line in must_lines.iter().chain(ref_lines.iter()).chain(bg_lines.iter()) {
            output.push_str(line);
            output.push('\n');
        }

        output
    }

    pub fn estimate_tokens(text: &str) -> usize {
        let ascii_count = text.chars().filter(|c| c.is_ascii()).count();
        let non_ascii_count = text.chars().count() - ascii_count;
        // ~4 chars/token for English, ~1.5 chars/token for CJK
        (ascii_count / 4) + (non_ascii_count * 2 / 3) + 1
    }

    pub fn trim_to_budget(results: &mut Vec<SearchResult>, token_budget: usize) {
        let mut total = 0;
        let mut keep = 0;
        for r in results.iter() {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            let tokens = Self::estimate_tokens(text) + 15; // overhead for [MUST]/[REF] tag + newline
            if total + tokens > token_budget && r.memory.priority != Priority::Must {
                break;
            }
            total += tokens;
            keep += 1;
        }
        results.truncate(keep);
    }

    pub async fn get_mcp_resource_content(&self, uri: &str) -> Result<String> {
        debug!(uri, "reading MCP resource");
        match uri {
            "memory://user-profile" => {
                let query = SearchQuery {
                    query: String::new(),
                    priority_filter: Some(Priority::Must),
                    top_k: 20,
                    ..SearchQuery::new(String::new())
                };
                let mut results = self.store.search(query).await?;
                Self::trim_to_budget(&mut results, 800);
                Ok(self.format_as_instructions(&results))
            }
            "memory://project-context" => {
                let query = SearchQuery {
                    query: String::new(),
                    priority_filter: Some(Priority::Reference),
                    top_k: 10,
                    ..SearchQuery::new(String::new())
                };
                let mut results = self.store.search(query).await?;
                Self::trim_to_budget(&mut results, 700);
                Ok(self.format_as_instructions(&results))
            }
            _ => Ok(String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    async fn setup() -> MemoryRouter {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        let agent = SourceAgent {
            id: "claude-desktop".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(MemoryType::Preference, "user prefers Python".to_string(), Priority::Must, agent.clone());
        m1.instruction = Some("代码使用 Python，不用 Java".to_string());

        let mut m2 = Memory::new(MemoryType::Fact, "project uses FastAPI".to_string(), Priority::Reference, agent.clone());
        m2.tags = vec!["coding".to_string(), "project".to_string()];

        let mut m3 = Memory::new(MemoryType::Preference, "writing style: concise".to_string(), Priority::Reference, agent);
        m3.tags = vec!["writing".to_string(), "style".to_string()];

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();
        store.save(m3).await.unwrap();

        MemoryRouter::new(store)
    }

    #[tokio::test]
    async fn test_session_start() {
        let router = setup().await;
        let results = router.session_start("claude-desktop", None, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_format_instructions() {
        let router = setup().await;
        let results = router.session_start("claude-desktop", None, None).await.unwrap();
        let formatted = router.format_as_instructions(&results);
        assert!(formatted.contains("[MUST]"));
        assert!(formatted.contains("[MEMORY CONTEXT"));
    }

    #[tokio::test]
    async fn test_mcp_resource() {
        let router = setup().await;
        let content = router.get_mcp_resource_content("memory://user-profile").await.unwrap();
        assert!(content.contains("[MUST]"));
    }

    #[tokio::test]
    async fn test_token_budget_trims() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent { id: "test".to_string(), agent_type: "general".to_string(), session_id: None };

        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!("This is memory number {} with some content to consume tokens", i),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let router = MemoryRouter::new(store);
        let results = router.session_start("default", None, None).await.unwrap();
        assert!(results.len() <= 8);
    }

    #[tokio::test]
    async fn test_must_always_included() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent { id: "test".to_string(), agent_type: "general".to_string(), session_id: None };

        let m1 = Memory::new(MemoryType::Preference, "critical rule".to_string(), Priority::Must, agent.clone());
        for i in 0..20 {
            let m = Memory::new(
                MemoryType::Fact,
                format!("Filler memory {} with lots of content to fill the token budget up quickly", i),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }
        store.save(m1).await.unwrap();

        let router = MemoryRouter::new(store);
        let results = router.session_start("default", None, None).await.unwrap();
        assert!(results.iter().any(|r| r.memory.priority == Priority::Must));
    }

    #[test]
    fn test_estimate_tokens() {
        assert!(MemoryRouter::estimate_tokens("hello world") < 10);
        assert!(MemoryRouter::estimate_tokens("代码使用 Python，不用 Java") > 5);
    }

    #[test]
    fn test_yaml_registry_parse() {
        let yaml = r#"
agents:
  - id: my-agent
    agent_type: coding-assistant
    description: "Custom coding agent"
    inject_rules:
      max_memories: 5
      token_budget: 1000
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: ["writing"]
"#;
        let config: AgentRegistryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].id, "my-agent");
        assert_eq!(config.agents[0].inject_rules.token_budget, 1000);
    }
}
