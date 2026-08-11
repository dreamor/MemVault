use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::error::{MemVaultError, Result};
use crate::models::AgentProfile;

/// Credentials provided by an MCP client to authenticate as an agent.
///
/// - `agent_id`: The agent's unique identifier (e.g. "claude-desktop").
/// - `api_key`: The secret key configured for this agent in `agents.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCredentials {
    pub agent_id: String,
    pub api_key: String,
}

impl AgentCredentials {
    pub fn new(agent_id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            api_key: api_key.into(),
        }
    }
}

/// Agent authentication store.
///
/// Holds a map of agent_id → hashed API key. Agents that have a registered
/// key must provide matching credentials on every tool call. Agents without
/// a registered key are unauthenticated and receive the "default" profile.
///
/// This is a **local-first** auth model — it prevents MCP client
/// impersonation over stdio/SSE/REST, but does not protect against
/// direct database access on the same machine.
#[derive(Debug, Clone)]
pub struct AgentAuth {
    /// (agent_id, hex-encoded SHA-256 of api_key)
    keys: Vec<(String, String)>,
}

impl AgentAuth {
    /// Create an empty auth store — all agents are unauthenticated.
    pub fn empty() -> Self {
        Self { keys: Vec::new() }
    }

    /// Register an agent's API key.
    ///
    /// The key is hashed with SHA-256 and stored as a hex string.
    pub fn register(&mut self, agent_id: &str, api_key: &str) {
        let hash = hash_key(api_key);
        info!(agent_id, "agent registered with API key");
        // Remove any previous entry for this agent, then add the new one
        self.keys.retain(|(id, _)| id != agent_id);
        self.keys.push((agent_id.to_string(), hash));
    }

    /// Build from an iterator of (agent_id, optional_api_key) pairs.
    ///
    /// Agents whose api_key is `None` are skipped (unauthenticated).
    pub fn from_profiles<'a>(profiles: impl IntoIterator<Item = &'a AgentProfile>) -> Self {
        let mut auth = Self::empty();
        for p in profiles {
            if let Some(ref key) = p.api_key {
                auth.register(&p.id, key);
            }
        }
        auth
    }

    /// Authenticate an agent.
    ///
    /// Returns `Ok(())` if:
    /// - The agent has no registered key (unauthenticated mode allowed), OR
    /// - The agent has a registered key and the credentials match.
    ///
    /// Returns `Err` if:
    /// - The agent has a registered key but no credentials were provided, OR
    /// - The agent has a registered key and the credentials don't match.
    pub fn authenticate(&self, agent_id: &str, creds: Option<&AgentCredentials>) -> Result<()> {
        // Check if this agent has a registered key
        let stored_hash = match self.keys.iter().find(|(id, _)| id == agent_id) {
            Some((_, hash)) => hash,
            None => {
                // No key registered for this agent — unauthenticated mode
                return Ok(());
            }
        };

        // Agent requires authentication
        let provided = creds.ok_or_else(|| {
            MemVaultError::Auth(format!(
                "agent '{}' requires api_key but none was provided",
                agent_id
            ))
        })?;

        if provided.agent_id != agent_id {
            return Err(MemVaultError::Auth(format!(
                "agent_id mismatch: credentials are for '{}' but request is for '{}'",
                provided.agent_id, agent_id
            )));
        }

        let provided_hash = hash_key(&provided.api_key);

        // Constant-time comparison via SHA-256 (same length hash ensures basic timing resistance)
        if *stored_hash == provided_hash {
            Ok(())
        } else {
            Err(MemVaultError::Auth(format!(
                "invalid api_key for agent '{}'",
                agent_id
            )))
        }
    }

    /// Check if an agent has a registered key (requires authentication).
    pub fn requires_auth(&self, agent_id: &str) -> bool {
        self.keys.iter().any(|(id, _)| id == agent_id)
    }

    /// Number of registered agents.
    pub fn registered_count(&self) -> usize {
        self.keys.len()
    }
}

/// Hash an API key with SHA-256, returning a hex-encoded string.
pub fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_key_deterministic() {
        let h1 = hash_key("my-secret-key");
        let h2 = hash_key("my-secret-key");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_hash_key_different() {
        let h1 = hash_key("key-a");
        let h2 = hash_key("key-b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_authenticate_no_key_registered() {
        let auth = AgentAuth::empty();
        // No key registered → any agent_id passes
        assert!(auth.authenticate("unknown-agent", None).is_ok());
    }

    #[test]
    fn test_authenticate_with_valid_key() {
        let mut auth = AgentAuth::empty();
        auth.register("claude-desktop", "sk-secret");

        let creds = AgentCredentials::new("claude-desktop", "sk-secret");
        assert!(auth.authenticate("claude-desktop", Some(&creds)).is_ok());
    }

    #[test]
    fn test_authenticate_with_wrong_key() {
        let mut auth = AgentAuth::empty();
        auth.register("claude-desktop", "sk-secret");

        let creds = AgentCredentials::new("claude-desktop", "sk-wrong");
        let result = auth.authenticate("claude-desktop", Some(&creds));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid api_key"));
    }

    #[test]
    fn test_authenticate_missing_key_when_required() {
        let mut auth = AgentAuth::empty();
        auth.register("claude-desktop", "sk-secret");

        let result = auth.authenticate("claude-desktop", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires api_key"));
    }

    #[test]
    fn test_authenticate_agent_id_mismatch() {
        let mut auth = AgentAuth::empty();
        auth.register("claude-desktop", "sk-secret");

        let creds = AgentCredentials::new("claude-code", "sk-secret");
        let result = auth.authenticate("claude-desktop", Some(&creds));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("agent_id mismatch")
        );
    }

    #[test]
    fn test_from_profiles() {
        use crate::models::InjectRules;

        let profiles = vec![
            AgentProfile {
                id: "agent-a".into(),
                agent_type: "coding-assistant".into(),
                description: "".into(),
                inject_rules: InjectRules::default(),
                api_key: Some("key-a".into()),
            },
            AgentProfile {
                id: "agent-b".into(),
                agent_type: "assistant".into(),
                description: "".into(),
                inject_rules: InjectRules::default(),
                api_key: None, // no key → unauthenticated
            },
        ];

        let auth = AgentAuth::from_profiles(&profiles);
        assert_eq!(auth.registered_count(), 1);
        assert!(auth.requires_auth("agent-a"));
        assert!(!auth.requires_auth("agent-b"));
    }

    #[test]
    fn test_register_overwrites() {
        let mut auth = AgentAuth::empty();
        auth.register("agent", "key-v1");
        auth.register("agent", "key-v2");

        assert!(
            auth.authenticate("agent", Some(&AgentCredentials::new("agent", "key-v2")))
                .is_ok()
        );
        assert!(
            auth.authenticate("agent", Some(&AgentCredentials::new("agent", "key-v1")))
                .is_err()
        );
    }

    #[test]
    fn test_requires_auth() {
        let mut auth = AgentAuth::empty();
        assert!(!auth.requires_auth("anyone"));

        auth.register("secured", "key");
        assert!(auth.requires_auth("secured"));
        assert!(!auth.requires_auth("other"));
    }
}
