use std::path::Path;
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::agent_adapt;
use crate::config::default_agent_registry;
use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::hybrid::HybridMerger;
use crate::intent::{self, Intent};
use crate::models::*;
use crate::storage::MemoryStore;

pub struct MemoryRouter {
    store: Arc<dyn MemoryStore>,
    registry: Vec<AgentProfile>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
}

impl MemoryRouter {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            registry: default_agent_registry(),
            embedder: None,
        }
    }

    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = Some(embedder);
        self
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
        Self { store, registry: r, embedder: None }
    }

    pub fn load_registry_from_yaml(store: Arc<dyn MemoryStore>, path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| crate::error::MemVaultError::Storage(format!("Failed to read agent registry: {}", e)))?;

        let config: AgentRegistryConfig = serde_yaml::from_str(&content)
            .map_err(|e| crate::error::MemVaultError::InvalidInput(format!("Invalid agent registry YAML: {}", e)))?;

        info!("Loaded {} agent profiles from {}", config.agents.len(), path.display());
        Ok(Self::with_registry(store, config.agents))
    }

    pub fn get_agent_profile(&self, agent_id: &str) -> AgentProfile {
        // 1. Exact match in registry
        if let Some(p) = self.registry.iter().find(|a| a.id == agent_id) {
            return p.clone();
        }

        // 2. Partial type match
        let agent_type = agent_id.split('-').next().unwrap_or("");
        if let Some(p) = self.registry.iter().find(|a| a.agent_type.contains(agent_type)) {
            return p.clone();
        }

        // 3. Auto-detect from fingerprints
        if let Some(p) = agent_adapt::identify_agent(agent_id, None) {
            return p;
        }

        // 4. Default
        self.registry.iter().find(|a| a.id == "default").cloned()
            .unwrap_or_else(|| AgentProfile {
                id: "default".to_string(),
                agent_type: "general-assistant".to_string(),
                description: "Default".to_string(),
                inject_rules: InjectRules::default(),
            })
    }

    /// Identify agent with optional client_info (from MCP handshake).
    pub fn get_agent_profile_with_client_info(&self, agent_id: &str, client_info: Option<&str>) -> AgentProfile {
        // Try registry first
        if let Some(p) = self.registry.iter().find(|a| a.id == agent_id) {
            return p.clone();
        }

        // Try fingerprint with client_info
        if let Some(p) = agent_adapt::identify_agent(agent_id, client_info) {
            return p;
        }

        self.get_agent_profile(agent_id)
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
            query: String::new(),
            agent_id: Some(agent_id.to_string()),
            namespace: namespace.clone(),
            top_k: profile.inject_rules.max_memories * 2,
            ..SearchQuery::new(String::new())
        };

        let mut results = self.store.search(query).await?;

        // If embedder is available and there's a context hint, do hybrid search
        if let (Some(embedder), Some(hint)) = (&self.embedder, context_hint) {
            if !hint.is_empty() {
                match embedder.embed(&[hint.to_string()]).await {
                    Ok(embeddings) if !embeddings.is_empty() => {
                        let vector_results = self.store
                            .vector_search(&embeddings[0], profile.inject_rules.max_memories * 2, namespace.as_deref())
                            .await?;

                        debug!(keyword = results.len(), vector = vector_results.len(), "merging hybrid results");

                        results = HybridMerger::merge(
                            results,
                            vector_results,
                            profile.inject_rules.max_memories * 2,
                            0.4,
                            0.6,
                        );
                    }
                    Err(e) => {
                        warn!("Embedding failed, falling back to keyword search: {}", e);
                    }
                    _ => {}
                }
            }
        }

        // filter by agent's exclude_types (checks both memory_type and tags)
        // MUST memories are never excluded; non-MUST get score penalty instead of hard exclude
        if !profile.inject_rules.exclude_types.is_empty() {
            for r in results.iter_mut() {
                if r.memory.priority == Priority::Must {
                    continue;
                }
                let type_str = serde_json::to_string(&r.memory.memory_type).unwrap_or_default();
                let type_str = type_str.trim_matches('"');
                let mut penalty = false;
                if profile.inject_rules.exclude_types.iter().any(|et| et.eq_ignore_ascii_case(type_str)) {
                    penalty = true;
                }
                if !penalty {
                    for tag in &r.memory.tags {
                        if profile.inject_rules.exclude_types.iter().any(|et| et.eq_ignore_ascii_case(tag)) {
                            penalty = true;
                            break;
                        }
                    }
                }
                if penalty {
                    r.score *= 0.3; // soft penalty instead of hard exclude
                }
            }
        }

        // filter by intent (soft penalty instead of hard exclude)
        if intent.primary != Intent::General {
            for r in results.iter_mut() {
                if r.memory.priority == Priority::Must {
                    continue;
                }
                if intent::should_exclude_for_intent(
                    &intent.primary,
                    &r.memory.tags,
                    &profile.inject_rules.exclude_types,
                ) {
                    r.score *= 0.4;
                }
            }
        }

        // re-sort after score adjustments
        results.sort_by(|a, b| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        // remove extremely low scoring results
        results.retain(|r| r.memory.priority == Priority::Must || r.score > 0.05);

        // Cross-namespace fallback: if project namespace has few results, supplement from global
        if namespace.as_deref().is_some_and(|ns| ns != "global" && ns != "project:*") {
            let non_must_count = results.iter().filter(|r| r.memory.priority != Priority::Must).count();
            if non_must_count < profile.inject_rules.max_memories / 2 {
                let global_query = SearchQuery {
                    query: String::new(),
                    agent_id: Some(agent_id.to_string()),
                    namespace: Some("global".to_string()),
                    top_k: profile.inject_rules.max_memories,
                    ..SearchQuery::new(String::new())
                };
                let global_results = self.store.search(global_query).await?;
                let existing_ids: std::collections::HashSet<String> = results.iter().map(|r| r.memory.id.clone()).collect();
                for gr in global_results {
                    if !existing_ids.contains(&gr.memory.id) {
                        results.push(gr);
                    }
                }
                debug!(supplemented = results.len(), "cross-namespace fallback applied");
            }
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
