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
}

impl Memory {
    pub fn new(
        memory_type: MemoryType,
        content: String,
        priority: Priority,
        source_agent: SourceAgent,
    ) -> Self {
        let now = Utc::now();
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub agent_type: String,
    pub description: String,
    pub inject_rules: InjectRules,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistryConfig {
    pub agents: Vec<AgentProfile>,
}
