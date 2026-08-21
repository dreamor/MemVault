use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::models::{AgentProfile, InjectRules, Priority, SearchResult};

/// Known agent fingerprints for auto-detection.
/// When an agent connects without explicit agent_id, we try to identify it
/// from client_info, user-agent, or behavioral patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFingerprint {
    pub id_patterns: Vec<String>,
    pub client_info_patterns: Vec<String>,
    pub profile: AgentProfile,
}

pub fn builtin_fingerprints() -> Vec<AgentFingerprint> {
    vec![
        AgentFingerprint {
            id_patterns: vec!["claude-desktop".into(), "claude_desktop".into()],
            client_info_patterns: vec!["Claude".into(), "Anthropic".into()],
            profile: AgentProfile {
                id: "claude-desktop".into(),
                agent_type: "coding-assistant".into(),
                description: "Claude Desktop".into(),
                inject_rules: InjectRules {
                    max_memories: 8,
                    token_budget: 1500,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into(), "project:*".into()],
                    exclude_types: vec!["writing".into(), "design".into()],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            // bare "claude" is the common agent_id for the Claude Code CLI
            id_patterns: vec![
                "claude".into(),
                "claude-code".into(),
                "claude_code".into(),
                "ccli".into(),
            ],
            client_info_patterns: vec!["claude-code".into(), "Claude Code".into()],
            profile: AgentProfile {
                id: "claude-code".into(),
                agent_type: "coding-assistant".into(),
                description: "Claude Code CLI".into(),
                inject_rules: InjectRules {
                    max_memories: 10,
                    token_budget: 2000,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into(), "project:*".into()],
                    exclude_types: vec!["writing".into()],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["cursor".into(), "cursor-ide".into()],
            client_info_patterns: vec!["Cursor".into(), "cursor".into()],
            profile: AgentProfile {
                id: "cursor".into(),
                agent_type: "code-ide".into(),
                description: "Cursor IDE".into(),
                inject_rules: InjectRules {
                    max_memories: 6,
                    token_budget: 1200,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into(), "project:*".into()],
                    exclude_types: vec![],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["cline".into(), "roo".into(), "roo-code".into()],
            client_info_patterns: vec!["Cline".into(), "Roo".into()],
            profile: AgentProfile {
                id: "cline".into(),
                agent_type: "coding-assistant".into(),
                description: "Cline / Roo Code".into(),
                inject_rules: InjectRules {
                    max_memories: 8,
                    token_budget: 1500,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into(), "project:*".into()],
                    exclude_types: vec!["writing".into()],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["copilot".into(), "github-copilot".into()],
            client_info_patterns: vec!["Copilot".into(), "GitHub".into()],
            profile: AgentProfile {
                id: "copilot".into(),
                agent_type: "code-completion".into(),
                description: "GitHub Copilot".into(),
                inject_rules: InjectRules {
                    max_memories: 4,
                    token_budget: 600,
                    priority_order: vec![Priority::Must],
                    namespace_filter: vec!["global".into()],
                    exclude_types: vec!["writing".into(), "design".into(), "project".into()],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["windsurf".into(), "codeium".into()],
            client_info_patterns: vec!["Windsurf".into(), "Codeium".into()],
            profile: AgentProfile {
                id: "windsurf".into(),
                agent_type: "code-ide".into(),
                description: "Windsurf / Codeium".into(),
                inject_rules: InjectRules {
                    max_memories: 6,
                    token_budget: 1200,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into(), "project:*".into()],
                    exclude_types: vec![],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["chatgpt".into(), "openai".into()],
            client_info_patterns: vec!["ChatGPT".into(), "OpenAI".into()],
            profile: AgentProfile {
                id: "chatgpt".into(),
                agent_type: "general-assistant".into(),
                description: "ChatGPT".into(),
                inject_rules: InjectRules {
                    max_memories: 8,
                    token_budget: 1500,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into()],
                    exclude_types: vec![],
                },
                api_key: None,
            },
        },
        AgentFingerprint {
            id_patterns: vec!["gemini".into(), "google".into()],
            client_info_patterns: vec!["Gemini".into(), "Google".into()],
            profile: AgentProfile {
                id: "gemini".into(),
                agent_type: "general-assistant".into(),
                description: "Google Gemini".into(),
                inject_rules: InjectRules {
                    max_memories: 8,
                    token_budget: 1500,
                    priority_order: vec![Priority::Must, Priority::Reference],
                    namespace_filter: vec!["global".into()],
                    exclude_types: vec![],
                },
                api_key: None,
            },
        },
    ]
}

/// Try to identify an agent from its self-reported ID or client_info string.
pub fn identify_agent(agent_id: &str, client_info: Option<&str>) -> Option<AgentProfile> {
    let fingerprints = builtin_fingerprints();
    let id_lower = agent_id.to_lowercase();

    // Match by agent_id patterns
    for fp in &fingerprints {
        if fp
            .id_patterns
            .iter()
            .any(|p| id_lower.contains(&p.to_lowercase()))
        {
            debug!(detected = %fp.profile.id, from = "agent_id", "agent identified");
            return Some(fp.profile.clone());
        }
    }

    // Match by client_info patterns
    if let Some(info) = client_info {
        let info_lower = info.to_lowercase();
        for fp in &fingerprints {
            if fp
                .client_info_patterns
                .iter()
                .any(|p| info_lower.contains(&p.to_lowercase()))
            {
                debug!(detected = %fp.profile.id, from = "client_info", "agent identified");
                return Some(fp.profile.clone());
            }
        }
    }

    None
}

/// Adapt memory injection format based on agent capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InjectFormat {
    /// [MUST] / [REF] tag format (default, works for most agents)
    MustRef,
    /// XML-style tags (better for some models)
    Xml,
    /// System prompt style (imperative sentences)
    SystemPrompt,
    /// Markdown headers
    Markdown,
}

pub fn format_memories(results: &[SearchResult], format: InjectFormat) -> String {
    if results.is_empty() {
        return String::new();
    }

    match format {
        InjectFormat::MustRef => format_must_ref(results),
        InjectFormat::Xml => format_xml(results),
        InjectFormat::SystemPrompt => format_system_prompt(results),
        InjectFormat::Markdown => format_markdown(results),
    }
}

fn format_must_ref(results: &[SearchResult]) -> String {
    let mut output = String::from("[MEMORY CONTEXT - 必须遵循]:\n");
    for r in results {
        let tag = match r.memory.priority {
            Priority::Must => "[MUST]",
            Priority::Reference => "[REF]",
            Priority::Background => "[BG]",
        };
        let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
        output.push_str(&format!("{} {}\n", tag, text));
    }
    output
}

fn format_xml(results: &[SearchResult]) -> String {
    let mut output = String::from("<memory_context>\n");
    for r in results {
        let priority = match r.memory.priority {
            Priority::Must => "must",
            Priority::Reference => "reference",
            Priority::Background => "background",
        };
        let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
        // XML-escape user content: raw `<`, `>` or `&` in a memory would
        // otherwise break the enclosing tags or inject markup.
        let text = escape_xml(text);
        output.push_str(&format!(
            "  <memory priority=\"{}\">{}</memory>\n",
            priority, text
        ));
    }
    output.push_str("</memory_context>\n");
    output
}

/// Escape XML special characters (`&` first so `&lt;` isn't re-escaped).
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn format_system_prompt(results: &[SearchResult]) -> String {
    let mut output = String::from("Important user context you MUST follow:\n\n");
    for r in results {
        let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
        match r.memory.priority {
            Priority::Must => output.push_str(&format!("- ALWAYS: {}\n", text)),
            Priority::Reference => output.push_str(&format!("- Note: {}\n", text)),
            Priority::Background => output.push_str(&format!("- FYI: {}\n", text)),
        }
    }
    output
}

/// Escape Markdown special characters so memory content can't reopen/close
/// emphasis, code spans, or link syntax when interpolated into a bullet.
fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn format_markdown(results: &[SearchResult]) -> String {
    let mut output = String::from("## User Memory Context\n\n");

    let musts: Vec<_> = results
        .iter()
        .filter(|r| r.memory.priority == Priority::Must)
        .collect();
    let refs: Vec<_> = results
        .iter()
        .filter(|r| r.memory.priority == Priority::Reference)
        .collect();
    let bgs: Vec<_> = results
        .iter()
        .filter(|r| r.memory.priority == Priority::Background)
        .collect();

    if !musts.is_empty() {
        output.push_str("### Rules (MUST follow)\n\n");
        for r in musts {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            output.push_str(&format!("- {}\n", escape_markdown(text)));
        }
        output.push('\n');
    }

    if !refs.is_empty() {
        output.push_str("### Context\n\n");
        for r in refs {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            output.push_str(&format!("- {}\n", escape_markdown(text)));
        }
        output.push('\n');
    }

    // Background used to be silently dropped — surface it as low-priority notes.
    if !bgs.is_empty() {
        output.push_str("### Notes\n\n");
        for r in bgs {
            let text = r.memory.instruction.as_deref().unwrap_or(&r.memory.content);
            output.push_str(&format!("- {}\n", escape_markdown(text)));
        }
    }

    output
}

/// Determine the best injection format for an agent type.
pub fn best_format_for_agent(agent_type: &str) -> InjectFormat {
    match agent_type {
        "coding-assistant" | "code-ide" => InjectFormat::MustRef,
        "code-completion" => InjectFormat::SystemPrompt,
        "general-assistant" => InjectFormat::Xml,
        _ => InjectFormat::MustRef,
    }
}

/// Estimate appropriate token budget based on agent's typical context window.
pub fn estimate_token_budget(agent_type: &str) -> usize {
    match agent_type {
        "code-completion" => 400,    // very limited context (inline suggestions)
        "code-ide" => 1200,          // medium context
        "coding-assistant" => 1500,  // standard
        "general-assistant" => 2000, // large context models
        _ => 1500,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    #[test]
    fn test_identify_claude_desktop() {
        let profile = identify_agent("claude-desktop", None);
        assert!(profile.is_some());
        assert_eq!(profile.unwrap().id, "claude-desktop");
    }

    #[test]
    fn test_identify_from_client_info() {
        let profile = identify_agent("unknown-agent", Some("Cursor Editor v1.2"));
        assert!(profile.is_some());
        assert_eq!(profile.unwrap().id, "cursor");
    }

    #[test]
    fn test_identify_cline() {
        let profile = identify_agent("cline-vscode", None);
        assert!(profile.is_some());
        assert_eq!(profile.unwrap().id, "cline");
    }

    #[test]
    fn test_identify_unknown() {
        let profile = identify_agent("my-custom-bot", None);
        assert!(profile.is_none());
    }

    #[test]
    fn test_format_xml() {
        let results = vec![SearchResult {
            memory: Memory::new(
                MemoryType::Preference,
                "test".into(),
                Priority::Must,
                SourceAgent {
                    id: "t".into(),
                    agent_type: "t".into(),
                    session_id: None,
                },
            ),
            score: 1.0,
            hit_sources: Vec::new(),
        }];
        let xml = format_memories(&results, InjectFormat::Xml);
        assert!(xml.contains("<memory priority=\"must\">"));
        assert!(xml.contains("</memory_context>"));
    }

    #[test]
    fn test_format_system_prompt() {
        let results = vec![SearchResult {
            memory: Memory::new(
                MemoryType::Preference,
                "use Python".into(),
                Priority::Must,
                SourceAgent {
                    id: "t".into(),
                    agent_type: "t".into(),
                    session_id: None,
                },
            ),
            score: 1.0,
            hit_sources: Vec::new(),
        }];
        let sp = format_memories(&results, InjectFormat::SystemPrompt);
        assert!(sp.contains("ALWAYS: use Python"));
    }

    #[test]
    fn test_format_markdown() {
        let m = Memory::new(
            MemoryType::Fact,
            "uses FastAPI".into(),
            Priority::Reference,
            SourceAgent {
                id: "t".into(),
                agent_type: "t".into(),
                session_id: None,
            },
        );
        let results = vec![SearchResult {
            memory: m,
            score: 0.5,
            hit_sources: Vec::new(),
        }];
        let md = format_memories(&results, InjectFormat::Markdown);
        assert!(md.contains("### Context"));
        assert!(md.contains("- uses FastAPI"));
    }

    #[test]
    fn test_best_format() {
        assert_eq!(
            best_format_for_agent("coding-assistant"),
            InjectFormat::MustRef
        );
        assert_eq!(
            best_format_for_agent("general-assistant"),
            InjectFormat::Xml
        );
        assert_eq!(
            best_format_for_agent("code-completion"),
            InjectFormat::SystemPrompt
        );
    }

    /// Bare "claude" as an agent_id used to fall through with no match.
    #[test]
    fn test_identify_bare_claude() {
        let profile = identify_agent("claude", None);
        assert!(profile.is_some(), "bare 'claude' should resolve");
        assert_eq!(profile.unwrap().id, "claude-code");
    }

    /// Memory content with XML special chars must not break the generated XML.
    #[test]
    fn test_format_xml_escapes_content() {
        let results = vec![SearchResult {
            memory: Memory::new(
                MemoryType::Preference,
                "use x < y & prefer 'quotes'".into(),
                Priority::Must,
                SourceAgent {
                    id: "t".into(),
                    agent_type: "t".into(),
                    session_id: None,
                },
            ),
            score: 1.0,
            hit_sources: Vec::new(),
        }];
        let xml = format_memories(&results, InjectFormat::Xml);
        assert!(
            xml.contains("&lt;") && !xml.contains("< y"),
            "must escape <"
        );
        assert!(
            xml.contains("&amp;") && !xml.contains(" & "),
            "must escape &"
        );
        assert!(xml.contains("</memory>"));
    }

    /// Background priorities used to be silently dropped from Markdown output.
    #[test]
    fn test_format_markdown_includes_background() {
        let results = vec![SearchResult {
            memory: Memory::new(
                MemoryType::Fact,
                "old team convention".into(),
                Priority::Background,
                SourceAgent {
                    id: "t".into(),
                    agent_type: "t".into(),
                    session_id: None,
                },
            ),
            score: 0.3,
            hit_sources: Vec::new(),
        }];
        let md = format_memories(&results, InjectFormat::Markdown);
        assert!(
            md.contains("### Notes"),
            "background should render a Notes section"
        );
        assert!(md.contains("- old team convention"));
    }

    #[test]
    fn test_format_markdown_escapes_special_chars() {
        let m = Memory::new(
            MemoryType::Fact,
            "use *bold* or `code` and [links](evil)".into(),
            Priority::Reference,
            SourceAgent {
                id: "t".into(),
                agent_type: "t".into(),
                session_id: None,
            },
        );
        let results = vec![SearchResult {
            memory: m,
            score: 0.5,
            hit_sources: Vec::new(),
        }];
        let md = format_memories(&results, InjectFormat::Markdown);
        assert!(!md.contains("*bold*"), "unescaped emphasis must not survive");
        assert!(md.contains("\\*bold\\*"));
        assert!(md.contains("\\`code\\`"));
        assert!(md.contains("\\[links\\]"));
    }
}
