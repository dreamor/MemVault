use serde::{Deserialize, Serialize};
use crate::models::{AgentProfile, InjectRules, Priority};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemVaultConfig {
    pub data_dir: String,
    pub default_token_budget: usize,
    pub default_max_memories: usize,
}

impl Default for MemVaultConfig {
    fn default() -> Self {
        Self {
            data_dir: dirs().to_string(),
            default_token_budget: 1500,
            default_max_memories: 8,
        }
    }
}

fn dirs() -> &'static str {
    "~/.memvault"
}

pub fn default_agent_registry() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            id: "claude-desktop".to_string(),
            agent_type: "coding-assistant".to_string(),
            description: "Claude Desktop coding assistant".to_string(),
            inject_rules: InjectRules {
                max_memories: 8,
                token_budget: 1500,
                priority_order: vec![Priority::Must, Priority::Reference],
                namespace_filter: vec!["global".to_string(), "project:*".to_string()],
                exclude_types: vec!["writing".to_string(), "design".to_string()],
            },
        },
        AgentProfile {
            id: "claude-code".to_string(),
            agent_type: "coding-assistant".to_string(),
            description: "Claude Code CLI assistant".to_string(),
            inject_rules: InjectRules {
                max_memories: 8,
                token_budget: 1500,
                priority_order: vec![Priority::Must, Priority::Reference],
                namespace_filter: vec!["global".to_string(), "project:*".to_string()],
                exclude_types: vec!["writing".to_string()],
            },
        },
        AgentProfile {
            id: "default".to_string(),
            agent_type: "general-assistant".to_string(),
            description: "Default agent profile".to_string(),
            inject_rules: InjectRules::default(),
        },
    ]
}
