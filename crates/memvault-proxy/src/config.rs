use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    pub proxy: ProxySettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxySettings {
    #[serde(default = "default_transport")]
    pub transport: TransportMode,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_db")]
    pub db: String,

    #[serde(default)]
    pub upstreams: Vec<UpstreamDef>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportMode {
    Stdio,
    Sse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamDef {
    pub name: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
}

impl UpstreamDef {
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }
}

fn default_transport() -> TransportMode {
    TransportMode::Stdio
}

fn default_port() -> u16 {
    3778
}

fn default_db() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{}/.memvault/data.db", home)
}

pub fn load_config(path: &str) -> anyhow::Result<ProxyConfig> {
    let expanded = shellexpand::tilde(path).to_string();
    let p = PathBuf::from(&expanded);
    if !p.exists() {
        return Ok(ProxyConfig {
            proxy: ProxySettings {
                transport: default_transport(),
                port: default_port(),
                db: default_db(),
                upstreams: Vec::new(),
            },
        });
    }
    let content = std::fs::read_to_string(p)?;
    let config: ProxyConfig = serde_yaml::from_str(&content)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let yaml = r#"
proxy:
  transport: sse
  port: 4000
  db: ~/.memvault/test.db
  upstreams:
    - name: filesystem
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    - name: remote
      url: "http://localhost:5000/mcp"
"#;
        let config: ProxyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.proxy.transport, TransportMode::Sse);
        assert_eq!(config.proxy.port, 4000);
        assert_eq!(config.proxy.upstreams.len(), 2);
        assert!(config.proxy.upstreams[0].is_stdio());
        assert!(config.proxy.upstreams[1].is_http());
    }

    #[test]
    fn test_default_config() {
        let yaml = "proxy: {}";
        let config: ProxyConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.proxy.transport, TransportMode::Stdio);
        assert_eq!(config.proxy.port, 3778);
        assert!(config.proxy.upstreams.is_empty());
    }

    #[test]
    fn test_load_config_missing_file_uses_defaults() {
        let missing = format!(
            "/tmp/memvault_no_such_{}.yaml",
            uuid::Uuid::new_v4().simple()
        );
        let config = load_config(&missing).expect("missing config falls back to defaults");
        assert_eq!(config.proxy.transport, TransportMode::Stdio);
        assert_eq!(config.proxy.port, 3778);
        assert!(config.proxy.upstreams.is_empty());
    }

    #[test]
    fn test_load_config_parses_file() {
        let path = std::env::temp_dir().join(format!(
            "memvault_proxy_cfg_{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(
            &path,
            "proxy:\n  transport: sse\n  port: 4444\n  upstreams:\n    - name: a\n      url: http://127.0.0.1:9999/mcp\n",
        )
        .unwrap();
        let config = load_config(&path.to_string_lossy()).unwrap();
        assert_eq!(config.proxy.transport, TransportMode::Sse);
        assert_eq!(config.proxy.port, 4444);
        assert_eq!(config.proxy.upstreams.len(), 1);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_load_config_invalid_yaml_errors() {
        let path = std::env::temp_dir().join(format!(
            "memvault_proxy_bad_{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "proxy: [unclosed").unwrap();
        assert!(load_config(&path.to_string_lossy()).is_err());
        std::fs::remove_file(path).ok();
    }
}
