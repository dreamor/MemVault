use rmcp::model::{Prompt, Resource, Tool};

pub struct MergeResult<T> {
    pub items: Vec<T>,
    pub conflicts: Vec<String>,
}

#[allow(dead_code)]
pub fn merge_tools(local_tools: Vec<Tool>, upstream_tools: Vec<Tool>) -> MergeResult<Tool> {
    let mut items = local_tools.clone();
    let mut conflicts = Vec::new();
    let local_names: std::collections::HashSet<String> =
        local_tools.iter().map(|t| t.name.to_string()).collect();

    for tool in upstream_tools {
        if local_names.contains(&tool.name.to_string()) {
            conflicts.push(format!(
                "tool '{}' exists in both local and upstream, using local",
                tool.name
            ));
        } else {
            items.push(tool);
        }
    }
    MergeResult { items, conflicts }
}

pub fn merge_resources(
    local_resources: Vec<Resource>,
    upstream_resources: Vec<Resource>,
) -> MergeResult<Resource> {
    let mut items = local_resources.clone();
    let mut conflicts = Vec::new();
    let local_uris: std::collections::HashSet<String> =
        local_resources.iter().map(|r| r.uri.to_string()).collect();

    for res in upstream_resources {
        if local_uris.contains(&res.uri.to_string()) {
            conflicts.push(format!(
                "resource '{}' exists in both local and upstream, using local",
                res.uri
            ));
        } else {
            items.push(res);
        }
    }
    MergeResult { items, conflicts }
}

pub fn merge_prompts(
    local_prompts: Vec<Prompt>,
    upstream_prompts: Vec<Prompt>,
) -> MergeResult<Prompt> {
    let mut items = local_prompts.clone();
    let mut conflicts = Vec::new();
    let local_names: std::collections::HashSet<String> =
        local_prompts.iter().map(|p| p.name.to_string()).collect();

    for prompt in upstream_prompts {
        if local_names.contains(&prompt.name.to_string()) {
            conflicts.push(format!(
                "prompt '{}' exists in both local and upstream, using local",
                prompt.name
            ));
        } else {
            items.push(prompt);
        }
    }
    MergeResult { items, conflicts }
}
