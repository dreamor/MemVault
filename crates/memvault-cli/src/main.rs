use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use memvault_core::models::*;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::router::MemoryRouter;

#[derive(Parser)]
#[command(name = "memvault", version, about = "MemVault — AI Agent Memory Router CLI")]
struct Cli {
    #[arg(long, default_value = "~/.memvault/data.db")]
    db: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a new memory
    Save {
        #[arg(long)]
        content: String,

        #[arg(long, default_value = "REFERENCE")]
        priority: String,

        #[arg(long, default_value = "fact")]
        r#type: String,

        #[arg(long, default_value = "global")]
        namespace: String,

        #[arg(long, default_value = "cli")]
        agent_id: String,

        #[arg(long)]
        instruction: Option<String>,

        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },

    /// Search memories
    Search {
        #[arg(long)]
        query: String,

        #[arg(long, default_value = "10")]
        top_k: usize,

        #[arg(long)]
        namespace: Option<String>,
    },

    /// List all memories
    List {
        #[arg(long, default_value = "20")]
        limit: usize,

        #[arg(long)]
        namespace: Option<String>,
    },

    /// Delete a memory by ID
    Delete {
        id: String,
    },

    /// Simulate session_start for an agent
    SessionStart {
        #[arg(long, default_value = "claude-desktop")]
        agent_id: String,

        #[arg(long)]
        context: Option<String>,

        #[arg(long)]
        project: Option<String>,
    },

    /// Show MCP Resource content
    Resource {
        #[arg(default_value = "memory://user-profile")]
        uri: String,
    },
}

fn resolve_db_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/") {
        if let Some(home) = dirs_next() {
            return home.join(&raw[2..]);
        }
    }
    PathBuf::from(raw)
}

fn dirs_next() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn parse_priority(s: &str) -> Priority {
    match s.to_uppercase().as_str() {
        "MUST" => Priority::Must,
        "BACKGROUND" => Priority::Background,
        _ => Priority::Reference,
    }
}

fn parse_memory_type(s: &str) -> MemoryType {
    match s.to_lowercase().as_str() {
        "preference" => MemoryType::Preference,
        "episode" => MemoryType::Episode,
        "entity" => MemoryType::Entity,
        "skill" => MemoryType::Skill,
        _ => MemoryType::Fact,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let db_path = resolve_db_path(&cli.db);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path.parent()
        .map(|p| p.join("agents.yaml"))
        .unwrap_or_else(|| PathBuf::from("agents.yaml"));

    let router = if registry_path.exists() {
        MemoryRouter::load_registry_from_yaml(store.clone(), &registry_path)?
    } else {
        MemoryRouter::new(store.clone())
    };

    match cli.command {
        Commands::Save {
            content,
            priority,
            r#type,
            namespace,
            agent_id,
            instruction,
            tags,
        } => {
            let mut mem = Memory::new(
                parse_memory_type(&r#type),
                content,
                parse_priority(&priority),
                SourceAgent {
                    id: agent_id,
                    agent_type: "cli".to_string(),
                    session_id: None,
                },
            );
            mem.namespace = namespace;
            mem.instruction = instruction;
            mem.tags = tags.unwrap_or_default();

            let saved = store.save(mem).await?;
            println!("Saved memory: {}", saved.id);
        }

        Commands::Search { query, top_k, namespace } => {
            let sq = SearchQuery {
                query,
                top_k,
                namespace,
                ..SearchQuery::new(String::new())
            };
            let results = store.search(sq).await?;

            if results.is_empty() {
                println!("No memories found.");
            } else {
                for r in &results {
                    println!("---");
                    println!("ID: {}", r.memory.id);
                    println!("Priority: {:?}", r.memory.priority);
                    println!("Content: {}", r.memory.content);
                    if let Some(ref inst) = r.memory.instruction {
                        println!("Instruction: {}", inst);
                    }
                    println!("Score: {:.2}", r.score);
                }
                println!("---");
                println!("Found {} memories.", results.len());
            }
        }

        Commands::List { limit, namespace } => {
            let memories = store.list(namespace.as_deref(), limit, 0).await?;

            if memories.is_empty() {
                println!("No memories stored.");
            } else {
                for m in &memories {
                    println!("[{:?}] {} — {}", m.priority, m.id, truncate(&m.content, 60));
                }
                println!("Total: {}", memories.len());
            }
        }

        Commands::Delete { id } => {
            store.delete(&id).await?;
            println!("Deleted: {}", id);
        }

        Commands::SessionStart { agent_id, context, project } => {
            let results = router
                .session_start(&agent_id, context.as_deref(), project.as_deref())
                .await?;

            let formatted = router.format_as_instructions(&results);
            if formatted.is_empty() {
                println!("No memories to inject for agent '{}'.", agent_id);
            } else {
                println!("{}", formatted);
            }
        }

        Commands::Resource { uri } => {
            let content = router.get_mcp_resource_content(&uri).await?;
            if content.is_empty() {
                println!("No content for resource: {}", uri);
            } else {
                println!("{}", content);
            }
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
