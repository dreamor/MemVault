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

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Tool;

    fn tool(name: &str) -> Tool {
        Tool::new(
            name.to_string(),
            format!("{name} desc"),
            serde_json::Map::new(),
        )
    }

    #[test]
    fn test_merge_tools_append_upstream_and_report_conflicts() {
        let local = vec![tool("local_a"), tool("shared")];
        let upstream = vec![tool("upstream_b"), tool("shared")];

        let result = merge_tools(local, upstream);
        assert_eq!(
            result.items.len(),
            3,
            "local wins, upstream non-conflict appended"
        );
        assert_eq!(result.conflicts.len(), 1);
        assert!(result.conflicts[0].contains("shared"));
        // local version survives; only one "shared" total
        assert!(result.items.iter().any(|t| t.name == "local_a"));
        assert!(result.items.iter().any(|t| t.name == "upstream_b"));
        assert_eq!(
            result.items.iter().filter(|t| t.name == "shared").count(),
            1,
            "upstream duplicate dropped, local keeps its copy"
        );
    }

    #[test]
    fn test_merge_resources_conflict_detection() {
        let local = vec![Resource::new("memory://a", "A")];
        let upstream = vec![
            Resource::new("memory://b", "B"),
            Resource::new("memory://a", "A again"),
        ];

        let merged = merge_resources(local, upstream);
        assert_eq!(merged.items.len(), 2, "upstream duplicate a is dropped");
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].contains("memory://a"));
        assert!(merged.items.iter().any(|r| r.uri == "memory://b"));
    }

    #[test]
    fn test_merge_prompts_conflict_detection() {
        let local = vec![Prompt::new::<_, &str>(
            "prompt-a",
            Some("local"),
            None::<Vec<rmcp::model::PromptArgument>>,
        )];
        let upstream = vec![
            Prompt::new(
                "prompt-b",
                Some("b"),
                None::<Vec<rmcp::model::PromptArgument>>,
            ),
            Prompt::new(
                "prompt-a",
                Some("a"),
                None::<Vec<rmcp::model::PromptArgument>>,
            ),
        ];

        let merged = merge_prompts(local, upstream);
        assert_eq!(merged.items.len(), 2);
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].contains("prompt-a"));
    }
}
