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
    pub async fn connect_all(defs: &[UpstreamDef]) -> Result<Arc<Self>> {
        let mut connections = Vec::new();
        let mut tool_index = HashMap::new();
        let mut resource_index = HashMap::new();
        let mut prompt_index = HashMap::new();

        for (idx, def) in defs.iter().enumerate() {
            match Self::connect_one(def).await {
                Ok(conn) => {
                    for tool in &conn.tools {
                        tool_index.insert(tool.name.to_string(), idx);
                    }
                    for res in &conn.resources {
                        resource_index.insert(res.uri.to_string(), idx);
                    }
                    for prompt in &conn.prompts {
                        prompt_index.insert(prompt.name.to_string(), idx);
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
