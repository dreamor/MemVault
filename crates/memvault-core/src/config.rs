use crate::models::{AgentProfile, InjectRules, Priority};
use serde::{Deserialize, Serialize};

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
            api_key: None,
            inject_channel: None,
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
            api_key: None,
            inject_channel: None,
        },
        AgentProfile {
            id: "default".to_string(),
            agent_type: "general-assistant".to_string(),
            description: "Default agent profile".to_string(),
            inject_rules: InjectRules::default(),
            api_key: None,
            inject_channel: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = MemVaultConfig::default();
        assert_eq!(config.data_dir, "~/.memvault");
        assert_eq!(config.default_token_budget, 1500);
        assert_eq!(config.default_max_memories, 8);
    }

    #[test]
    fn test_default_agent_registry_has_expected_agents() {
        let registry = default_agent_registry();
        assert_eq!(registry.len(), 3);

        let ids: Vec<&str> = registry.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"claude-desktop"));
        assert!(ids.contains(&"claude-code"));
        assert!(ids.contains(&"default"));
    }

    #[test]
    fn test_claude_desktop_excludes_writing_and_design() {
        let registry = default_agent_registry();
        let desktop = registry.iter().find(|a| a.id == "claude-desktop").unwrap();
        assert_eq!(desktop.agent_type, "coding-assistant");
        assert!(
            desktop
                .inject_rules
                .exclude_types
                .contains(&"writing".to_string())
        );
        assert!(
            desktop
                .inject_rules
                .exclude_types
                .contains(&"design".to_string())
        );
    }

    #[test]
    fn test_claude_code_excludes_only_writing() {
        let registry = default_agent_registry();
        let code = registry.iter().find(|a| a.id == "claude-code").unwrap();
        assert!(
            code.inject_rules
                .exclude_types
                .contains(&"writing".to_string())
        );
        assert!(
            !code
                .inject_rules
                .exclude_types
                .contains(&"design".to_string())
        );
    }

    #[test]
    fn test_default_agent_uses_default_inject_rules() {
        let registry = default_agent_registry();
        let default = registry.iter().find(|a| a.id == "default").unwrap();
        assert_eq!(
            default.inject_rules.max_memories,
            InjectRules::default().max_memories
        );
        assert_eq!(
            default.inject_rules.token_budget,
            InjectRules::default().token_budget
        );
        assert!(default.inject_rules.exclude_types.is_empty());
    }

    #[test]
    fn test_claude_desktop_has_project_namespace_filter() {
        let registry = default_agent_registry();
        let desktop = registry.iter().find(|a| a.id == "claude-desktop").unwrap();
        assert!(
            desktop
                .inject_rules
                .namespace_filter
                .contains(&"project:*".to_string())
        );
    }
}
