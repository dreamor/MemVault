use rmcp::model::{Prompt, Resource, Tool};
use std::collections::HashSet;
use std::fmt::Display;
use std::hash::Hash;

pub struct MergeResult<T> {
    pub items: Vec<T>,
    pub conflicts: Vec<String>,
}

/// Merge upstream items behind the local ones, first-wins by key.
///
/// The `seen` set is seeded with the local keys AND grows with every
/// accepted upstream item, so a key provided by two upstreams is listed
/// exactly once — matching the forwarding index in
/// [`crate::upstream::UpstreamManager`] (`register_index`), which is also
/// first-wins. Listing a duplicate the router can never address separately
/// would make `*/list` disagree with `*/read|get|call` routing.
fn merge_by_key<T, K>(
    local: Vec<T>,
    upstream: Vec<T>,
    key: impl Fn(&T) -> K,
    kind: &str,
) -> MergeResult<T>
where
    K: Hash + Eq + Clone + Display,
{
    let local_keys: HashSet<K> = local.iter().map(&key).collect();
    let mut seen = local_keys.clone();
    let mut items = local;
    let mut conflicts = Vec::new();

    for item in upstream {
        let k = key(&item);
        if seen.insert(k.clone()) {
            items.push(item);
        } else if local_keys.contains(&k) {
            conflicts.push(format!(
                "{kind} '{k}' exists in both local and upstream, using local"
            ));
        } else {
            conflicts.push(format!(
                "{kind} '{k}' provided by multiple upstreams, using the first"
            ));
        }
    }
    MergeResult { items, conflicts }
}

pub fn merge_tools(local_tools: Vec<Tool>, upstream_tools: Vec<Tool>) -> MergeResult<Tool> {
    merge_by_key(local_tools, upstream_tools, |t| t.name.to_string(), "tool")
}

pub fn merge_resources(
    local_resources: Vec<Resource>,
    upstream_resources: Vec<Resource>,
) -> MergeResult<Resource> {
    merge_by_key(
        local_resources,
        upstream_resources,
        |r| r.uri.to_string(),
        "resource",
    )
}

pub fn merge_prompts(
    local_prompts: Vec<Prompt>,
    upstream_prompts: Vec<Prompt>,
) -> MergeResult<Prompt> {
    merge_by_key(
        local_prompts,
        upstream_prompts,
        |p| p.name.to_string(),
        "prompt",
    )
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

    // Regression (issue #6): `all_resources()`/`all_prompts()` flat-map every
    // connected upstream, so two upstreams serving the same key arrive as two
    // entries in the `upstream` vector. The old check only tested against the
    // LOCAL key set and never registered accepted upstream keys, so both
    // copies were listed while routing stayed first-wins — list and read
    // disagreed.

    #[test]
    fn test_merge_resources_dedups_across_upstreams() {
        let merged = merge_resources(
            vec![Resource::new("memory://local", "L")],
            vec![
                Resource::new("fixture://shared", "from upstream 1"),
                Resource::new("fixture://other", "O"),
                Resource::new("fixture://shared", "from upstream 2"),
            ],
        );
        assert_eq!(
            merged
                .items
                .iter()
                .filter(|r| r.uri == "fixture://shared")
                .count(),
            1,
            "second upstream's copy of the same URI must be dropped"
        );
        assert_eq!(merged.items.len(), 3);
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].contains("fixture://shared"));
        assert!(merged.conflicts[0].contains("multiple upstreams"));
        // first-wins: the surviving entry is the first upstream's
        assert!(merged.items.iter().any(
            |r| r.uri == "fixture://shared" && r.name == "from upstream 1"
        ));
    }

    #[test]
    fn test_merge_prompts_dedups_across_upstreams() {
        let merged = merge_prompts(
            vec![],
            vec![
                Prompt::new(
                    "fixture_prompt",
                    Some("first"),
                    None::<Vec<rmcp::model::PromptArgument>>,
                ),
                Prompt::new(
                    "fixture_prompt",
                    Some("second"),
                    None::<Vec<rmcp::model::PromptArgument>>,
                ),
            ],
        );
        assert_eq!(merged.items.len(), 1);
        assert_eq!(merged.items[0].description.as_deref(), Some("first"));
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].contains("fixture_prompt"));
    }

    #[test]
    fn test_merge_tools_dedups_across_upstreams() {
        let merged = merge_tools(
            vec![tool("local_t")],
            vec![tool("up_shared"), tool("up_only"), tool("up_shared")],
        );
        assert_eq!(
            merged.items.iter().filter(|t| t.name == "up_shared").count(),
            1
        );
        assert_eq!(merged.items.len(), 3);
        assert_eq!(merged.conflicts.len(), 1);
        assert!(merged.conflicts[0].contains("up_shared"));
    }
}
