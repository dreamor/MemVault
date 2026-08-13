use crate::auth::hash_key;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Preference,
    Fact,
    Episode,
    Entity,
    Skill,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Priority {
    Must,
    Reference,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAgent {
    pub id: String,
    pub agent_type: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub content: String,
    pub instruction: Option<String>,
    pub priority: Priority,
    pub source_agent: SourceAgent,
    pub namespace: String,
    pub confidence: f64,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ai_generated: bool,
    pub human_reviewed: bool,
    pub decay_score: f64,
    pub access_count: u32,
    pub last_read_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub layer: MemoryLayer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_meta: Option<SkillMeta>,
}

impl Memory {
    pub fn new(
        memory_type: MemoryType,
        content: String,
        priority: Priority,
        source_agent: SourceAgent,
    ) -> Self {
        let now = Utc::now();
        let layer = match priority {
            Priority::Must => MemoryLayer::L3,
            Priority::Reference => MemoryLayer::L2,
            Priority::Background => MemoryLayer::L1,
        };
        Self {
            id: format!("mem_{}", uuid::Uuid::new_v4().as_simple()),
            memory_type,
            content,
            instruction: None,
            priority,
            source_agent,
            namespace: "global".to_string(),
            confidence: 0.8,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
            ai_generated: true,
            human_reviewed: false,
            decay_score: 1.0,
            access_count: 0,
            last_read_at: None,
            layer,
            skill_meta: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub agent_type: String,
    pub description: String,
    pub inject_rules: InjectRules,
    /// Optional API key for agent authentication.
    /// When set, the client must provide matching credentials on tool calls.
    /// The key is hashed (SHA-256) on load and never stored in plaintext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectRules {
    pub max_memories: usize,
    pub token_budget: usize,
    pub priority_order: Vec<Priority>,
    pub namespace_filter: Vec<String>,
    pub exclude_types: Vec<String>,
}

impl Default for InjectRules {
    fn default() -> Self {
        Self {
            max_memories: 8,
            token_budget: 1500,
            priority_order: vec![Priority::Must, Priority::Reference],
            namespace_filter: vec!["global".to_string()],
            exclude_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub agent_id: Option<String>,
    pub type_filter: Option<MemoryType>,
    pub priority_filter: Option<Priority>,
    pub namespace: Option<String>,
    pub top_k: usize,
    pub token_budget: Option<usize>,
}

impl SearchQuery {
    pub fn new(query: String) -> Self {
        Self {
            query,
            agent_id: None,
            type_filter: None,
            priority_filter: None,
            namespace: None,
            top_k: 10,
            token_budget: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub memory: Memory,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
#[derive(Default)]
pub enum MemoryLayer {
    L0,
    #[default]
    L1,
    L2,
    L3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub trigger: Option<String>,
    pub steps: Vec<String>,
    pub verification: Option<String>,
    #[serde(default = "default_skill_version")]
    pub version: u32,
}

fn default_skill_version() -> u32 {
    1
}

impl Default for SkillMeta {
    fn default() -> Self {
        Self {
            trigger: None,
            steps: Vec::new(),
            verification: None,
            version: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartOutput {
    pub injected: Vec<SearchResult>,
    pub overflow_count: usize,
    pub overflow_summaries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistryConfig {
    pub agents: Vec<AgentProfile>,
}

impl AgentRegistryConfig {
    /// Hash all api_key values in the agent profiles for secure in-memory storage.
    /// Call this after deserializing from YAML to avoid keeping plaintext keys.
    pub fn hash_api_keys_in_place(&mut self) {
        for agent in &mut self.agents {
            if let Some(ref key) = agent.api_key.take()
                && !key.is_empty()
            {
                // Store the hex-encoded SHA-256 hash instead of the raw key
                agent.api_key = Some(hash_key(key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_memory_new_defaults() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "test content".to_string(),
            Priority::Reference,
            agent,
        );

        assert!(mem.id.starts_with("mem_"));
        assert_eq!(mem.memory_type, MemoryType::Fact);
        assert_eq!(mem.content, "test content");
        assert_eq!(mem.priority, Priority::Reference);
        assert_eq!(mem.namespace, "global");
        assert_eq!(mem.confidence, 0.8);
        assert!(mem.tags.is_empty());
        assert!(mem.instruction.is_none());
        assert!(mem.ai_generated);
        assert!(!mem.human_reviewed);
        assert_eq!(mem.decay_score, 1.0);
        assert_eq!(mem.access_count, 0);
        assert!(mem.last_read_at.is_none());
    }

    #[test]
    fn test_memory_new_must_priority() {
        let agent = SourceAgent {
            id: "claude".to_string(),
            agent_type: "assistant".to_string(),
            session_id: Some("sess_1".to_string()),
        };
        let mem = Memory::new(
            MemoryType::Preference,
            "must rule".to_string(),
            Priority::Must,
            agent,
        );

        assert_eq!(mem.priority, Priority::Must);
    }

    #[test]
    fn test_memory_new_timestamps() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "tester".to_string(),
            session_id: None,
        };
        let before = Utc::now();
        let mem = Memory::new(
            MemoryType::Episode,
            "event".to_string(),
            Priority::Background,
            agent,
        );
        let after = Utc::now();

        assert!(mem.created_at >= before);
        assert!(mem.created_at <= after);
        assert_eq!(mem.created_at, mem.updated_at);
    }

    #[test]
    fn test_search_query_new() {
        let q = SearchQuery::new("find me".to_string());

        assert_eq!(q.query, "find me");
        assert_eq!(q.top_k, 10);
        assert!(q.agent_id.is_none());
        assert!(q.type_filter.is_none());
        assert!(q.priority_filter.is_none());
        assert!(q.namespace.is_none());
        assert!(q.token_budget.is_none());
    }

    #[test]
    fn test_search_query_custom() {
        let q = SearchQuery {
            query: "search".to_string(),
            agent_id: Some("agent1".to_string()),
            type_filter: Some(MemoryType::Preference),
            priority_filter: Some(Priority::Must),
            namespace: Some("project-alpha".to_string()),
            top_k: 5,
            token_budget: Some(500),
        };

        assert_eq!(q.query, "search");
        assert_eq!(q.agent_id, Some("agent1".to_string()));
        assert_eq!(q.top_k, 5);
    }

    #[test]
    fn test_inject_rules_default() {
        let rules = InjectRules::default();

        assert_eq!(rules.max_memories, 8);
        assert_eq!(rules.token_budget, 1500);
        assert_eq!(
            rules.priority_order,
            vec![Priority::Must, Priority::Reference]
        );
        assert_eq!(rules.namespace_filter, vec!["global".to_string()]);
        assert!(rules.exclude_types.is_empty());
    }

    #[test]
    fn test_memory_type_serde() {
        let cases = vec![
            (MemoryType::Preference, "\"preference\""),
            (MemoryType::Fact, "\"fact\""),
            (MemoryType::Episode, "\"episode\""),
            (MemoryType::Entity, "\"entity\""),
            (MemoryType::Skill, "\"skill\""),
        ];

        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json, expected,
                "MemoryType::{:?} serializes to {}",
                variant, expected
            );
            let deserialized: MemoryType = serde_json::from_str(expected).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn test_priority_serde() {
        let cases = vec![
            (Priority::Must, "\"MUST\""),
            (Priority::Reference, "\"REFERENCE\""),
            (Priority::Background, "\"BACKGROUND\""),
        ];

        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json, expected,
                "Priority::{:?} serializes to {}",
                variant, expected
            );
            let deserialized: Priority = serde_json::from_str(expected).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn test_memory_roundtrip_serde() {
        let agent = SourceAgent {
            id: "serde-test".to_string(),
            agent_type: "tester".to_string(),
            session_id: Some("sess_s".to_string()),
        };
        let mem = Memory::new(
            MemoryType::Skill,
            "test the JSON roundtrip".to_string(),
            Priority::Background,
            agent,
        );

        let json = serde_json::to_string(&mem).unwrap();
        let deserialized: Memory = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, mem.id);
        assert_eq!(deserialized.memory_type, mem.memory_type);
        assert_eq!(deserialized.content, mem.content);
        assert_eq!(deserialized.priority, mem.priority);
        assert_eq!(deserialized.namespace, mem.namespace);
    }

    #[test]
    fn test_source_agent_serde() {
        let agent = SourceAgent {
            id: "agent-x".to_string(),
            agent_type: "writer".to_string(),
            session_id: Some("abc".to_string()),
        };

        let json = serde_json::to_string(&agent).unwrap();
        let deserialized: SourceAgent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "agent-x");
        assert_eq!(deserialized.agent_type, "writer");
        assert_eq!(deserialized.session_id, Some("abc".to_string()));
    }

    #[test]
    fn test_search_result_holds_data() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "tester".to_string(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "searchable memory".to_string(),
            Priority::Reference,
            agent,
        );
        let result = SearchResult {
            memory: mem.clone(),
            score: 0.85,
        };

        assert_eq!(result.memory.id, mem.id);
        assert_eq!(result.score, 0.85);
    }

    #[test]
    fn test_skill_meta_serde_roundtrip() {
        let meta = SkillMeta {
            trigger: Some("database timeout".to_string()),
            steps: vec!["check network".to_string(), "check pool".to_string()],
            verification: Some("connection < 100ms".to_string()),
            version: 2,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: SkillMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.trigger, Some("database timeout".to_string()));
        assert_eq!(deserialized.steps.len(), 2);
        assert_eq!(deserialized.version, 2);
    }

    #[test]
    fn test_memory_with_skill_meta_roundtrip() {
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Skill,
            "deploy process".to_string(),
            Priority::Must,
            agent,
        );
        mem.skill_meta = Some(SkillMeta {
            trigger: Some("deploy".to_string()),
            steps: vec!["build".to_string(), "test".to_string(), "push".to_string()],
            verification: Some("health check passes".to_string()),
            version: 1,
        });

        let json = serde_json::to_string(&mem).unwrap();
        let deserialized: Memory = serde_json::from_str(&json).unwrap();
        assert!(deserialized.skill_meta.is_some());
        let sm = deserialized.skill_meta.unwrap();
        assert_eq!(sm.steps.len(), 3);
        assert_eq!(sm.trigger, Some("deploy".to_string()));
    }

    #[test]
    fn test_memory_without_skill_meta_compat() {
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
        let json = serde_json::to_string(&mem).unwrap();
        assert!(!json.contains("skill_meta"));
        let deserialized: Memory = serde_json::from_str(&json).unwrap();
        assert!(deserialized.skill_meta.is_none());
    }

    #[test]
    fn test_hash_api_keys_in_place_hashes_present_keys() {
        let mut config = AgentRegistryConfig {
            agents: vec![
                AgentProfile {
                    id: "with-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: Some("the-secret".into()),
                },
                AgentProfile {
                    id: "no-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: None,
                },
                AgentProfile {
                    id: "empty-key".into(),
                    agent_type: "general".into(),
                    description: String::new(),
                    inject_rules: InjectRules::default(),
                    api_key: Some("".into()),
                },
            ],
        };

        config.hash_api_keys_in_place();

        let with_key = config.agents.iter().find(|a| a.id == "with-key").unwrap();
        let hashed = with_key
            .api_key
            .as_ref()
            .expect("present key must be retained as a hash");
        assert_ne!(hashed, "the-secret", "plaintext key must never be stored");
        assert!(!hashed.is_empty());

        assert!(
            config
                .agents
                .iter()
                .find(|a| a.id == "no-key")
                .unwrap()
                .api_key
                .is_none(),
            "missing key stays None"
        );
        assert!(
            config
                .agents
                .iter()
                .find(|a| a.id == "empty-key")
                .unwrap()
                .api_key
                .is_none(),
            "empty key must be dropped"
        );
    }
}
