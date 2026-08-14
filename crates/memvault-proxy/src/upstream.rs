use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, Prompt,
    ReadResourceRequestParams, ReadResourceResult, Resource, Tool,
};
use rmcp::service::Peer;
use rmcp::{RoleClient, ServiceExt};
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::config::UpstreamDef;

pub struct UpstreamConnection {
    pub name: String,
    pub peer: Peer<RoleClient>,
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub prompts: Vec<Prompt>,
}

pub struct UpstreamManager {
    connections: RwLock<Vec<UpstreamConnection>>,
    #[allow(dead_code)]
    tool_index: RwLock<HashMap<String, usize>>,
    resource_index: RwLock<HashMap<String, usize>>,
    prompt_index: RwLock<HashMap<String, usize>>,
}

impl UpstreamManager {
    /// Register an index in a first-wins manner: if the key is already claimed
    /// by an earlier connection, keep the earlier index and log a warning
    /// instead of silently routing the tool/resource to the wrong upstream.
    fn register_index(
        index: &mut HashMap<String, usize>,
        key: &str,
        idx: usize,
        kind: &str,
        conn_name: &str,
    ) {
        match index.entry(key.to_string()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(idx);
            }
            std::collections::hash_map::Entry::Occupied(e) => {
                tracing::warn!(
                    kind,
                    name = key,
                    held_by_conn = e.get(),
                    second_conn = idx,
                    conn = conn_name,
                    "duplicate name across upstreams — first connection wins"
                );
            }
        }
    }

    pub async fn connect_all(defs: &[UpstreamDef]) -> Result<Arc<Self>> {
        let mut connections = Vec::new();
        let mut tool_index = HashMap::new();
        let mut resource_index = HashMap::new();
        let mut prompt_index = HashMap::new();

        for (idx, def) in defs.iter().enumerate() {
            match Self::connect_one(def).await {
                Ok(conn) => {
                    for tool in &conn.tools {
                        Self::register_index(&mut tool_index, &tool.name, idx, "tool", &conn.name);
                    }
                    for res in &conn.resources {
                        Self::register_index(
                            &mut resource_index,
                            &res.uri,
                            idx,
                            "resource",
                            &conn.name,
                        );
                    }
                    for prompt in &conn.prompts {
                        Self::register_index(
                            &mut prompt_index,
                            &prompt.name,
                            idx,
                            "prompt",
                            &conn.name,
                        );
                    }
                    info!(name = %conn.name, tools = conn.tools.len(), resources = conn.resources.len(), "upstream connected");
                    connections.push(conn);
                }
                Err(e) => {
                    error!(name = %def.name, error = %e, "failed to connect upstream, skipping");
                }
            }
        }

        Ok(Arc::new(Self {
            connections: RwLock::new(connections),
            tool_index: RwLock::new(tool_index),
            resource_index: RwLock::new(resource_index),
            prompt_index: RwLock::new(prompt_index),
        }))
    }

    async fn connect_one(def: &UpstreamDef) -> Result<UpstreamConnection> {
        let peer = if def.is_stdio() {
            let cmd = def.command.as_ref().unwrap();
            let args = def.args.as_deref().unwrap_or(&[]);

            let mut command = tokio::process::Command::new(cmd);
            command.args(args);
            for (k, v) in &def.env {
                command.env(k, v);
            }

            let transport = rmcp::transport::TokioChildProcess::new(command)
                .context(format!("failed to spawn upstream '{}'", def.name))?;
            let service = ().serve(transport).await.context(format!(
                "failed to connect to upstream '{}' via stdio",
                def.name
            ))?;
            service.peer().clone()
        } else if def.is_http() {
            let url = def.url.as_ref().unwrap();
            let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.as_str());
            let service = ().serve(transport).await.context(format!(
                "failed to connect to upstream '{}' via HTTP",
                def.name
            ))?;
            service.peer().clone()
        } else {
            anyhow::bail!("upstream '{}' has neither command nor url", def.name);
        };

        let tools = peer.list_all_tools().await.unwrap_or_default();
        let resources = peer.list_all_resources().await.unwrap_or_default();
        let prompts = peer.list_all_prompts().await.unwrap_or_default();

        Ok(UpstreamConnection {
            name: def.name.clone(),
            peer,
            tools,
            resources,
            prompts,
        })
    }

    #[allow(dead_code)]
    pub async fn all_tools(&self) -> Vec<Tool> {
        let conns = self.connections.read().await;
        conns.iter().flat_map(|c| c.tools.clone()).collect()
    }

    pub async fn all_resources(&self) -> Vec<Resource> {
        let conns = self.connections.read().await;
        conns.iter().flat_map(|c| c.resources.clone()).collect()
    }

    pub async fn all_prompts(&self) -> Vec<Prompt> {
        let conns = self.connections.read().await;
        conns.iter().flat_map(|c| c.prompts.clone()).collect()
    }

    #[allow(dead_code)]
    pub async fn forward_tool_call(
        &self,
        tool_name: &str,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult> {
        let idx = {
            let index = self.tool_index.read().await;
            *index
                .get(tool_name)
                .context(format!("tool '{}' not found in any upstream", tool_name))?
        };
        let conns = self.connections.read().await;
        let conn = &conns[idx];
        let result = conn
            .peer
            .call_tool(params)
            .await
            .map_err(|e| anyhow::anyhow!("upstream tool call failed: {:?}", e))?;
        Ok(result)
    }

    pub async fn forward_read_resource(
        &self,
        uri: &str,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult> {
        let idx = {
            let index = self.resource_index.read().await;
            *index
                .get(uri)
                .context(format!("resource '{}' not found in any upstream", uri))?
        };
        let conns = self.connections.read().await;
        let conn = &conns[idx];
        let result = conn
            .peer
            .read_resource(params)
            .await
            .map_err(|e| anyhow::anyhow!("upstream resource read failed: {:?}", e))?;
        Ok(result)
    }

    pub async fn forward_get_prompt(
        &self,
        prompt_name: &str,
        params: GetPromptRequestParams,
    ) -> Result<GetPromptResult> {
        let idx = {
            let index = self.prompt_index.read().await;
            *index.get(prompt_name).context(format!(
                "prompt '{}' not found in any upstream",
                prompt_name
            ))?
        };
        let conns = self.connections.read().await;
        let conn = &conns[idx];
        let result = conn
            .peer
            .get_prompt(params)
            .await
            .map_err(|e| anyhow::anyhow!("upstream prompt get failed: {:?}", e))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def_neither(name: &str) -> UpstreamDef {
        UpstreamDef {
            name: name.to_string(),
            command: None,
            args: None,
            env: HashMap::new(),
            url: None,
        }
    }

    /// Regression: two upstreams declaring the same tool name used to silently
    /// overwrite each other (last connection won), routing a call to the wrong
    /// upstream. Registration must be first-wins.
    #[test]
    fn test_register_index_is_first_wins_on_duplicate() {
        let mut index = HashMap::new();

        UpstreamManager::register_index(&mut index, "read_file", 0, "tool", "connA");
        UpstreamManager::register_index(&mut index, "read_file", 1, "tool", "connB");
        UpstreamManager::register_index(&mut index, "search", 1, "tool", "connB");

        assert_eq!(
            index.get("read_file"),
            Some(&0),
            "first connection must win"
        );
        assert_eq!(index.get("search"), Some(&1));
    }

    #[tokio::test]
    async fn test_connect_all_empty() {
        let manager = UpstreamManager::connect_all(&[]).await.unwrap();
        assert!(manager.all_tools().await.is_empty());
        assert!(manager.all_resources().await.is_empty());
        assert!(manager.all_prompts().await.is_empty());
    }

    #[tokio::test]
    async fn test_connect_def_without_scheme_is_skipped() {
        let manager = UpstreamManager::connect_all(&[def_neither("broken")])
            .await
            .expect("failed connections are skipped, never fatal");
        assert!(
            manager.all_resources().await.is_empty(),
            "no connection should survive"
        );
    }

    #[tokio::test]
    async fn test_connect_http_to_dead_port_is_skipped() {
        let def = UpstreamDef {
            name: "dead-http".to_string(),
            command: None,
            args: None,
            env: HashMap::new(),
            url: Some("http://127.0.0.1:9/mcp".to_string()),
        };
        let manager = UpstreamManager::connect_all(&[def])
            .await
            .expect("unreachable upstream is skipped, never fatal");
        assert!(manager.all_tools().await.is_empty());
    }

    #[tokio::test]
    async fn test_forward_tool_missing_index_errors() {
        let manager = UpstreamManager::connect_all(&[]).await.unwrap();
        let err = manager
            .forward_tool_call("nope", CallToolRequestParams::new("nope"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found in any upstream"));
    }

    #[tokio::test]
    async fn test_forward_resource_missing_index_errors() {
        let manager = UpstreamManager::connect_all(&[]).await.unwrap();
        let err = manager
            .forward_read_resource(
                "memory://nope",
                ReadResourceRequestParams::new("memory://nope"),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found in any upstream"));
    }

    #[tokio::test]
    async fn test_forward_prompt_missing_index_errors() {
        let manager = UpstreamManager::connect_all(&[]).await.unwrap();
        let err = manager
            .forward_get_prompt("nope", GetPromptRequestParams::new("nope"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found in any upstream"));
    }
}
