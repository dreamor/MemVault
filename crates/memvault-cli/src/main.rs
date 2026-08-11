use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use memvault_core::decay::{DecayConfig, DecayManager};
use memvault_core::dedup::Deduplicator;
use memvault_core::extractor::Extractor;
use memvault_core::io::{Exporter, Importer};
use memvault_core::models::*;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::sync::SyncEngine;

#[derive(Parser)]
#[command(
    name = "memvault",
    version,
    about = "MemVault — AI Agent Memory Router CLI"
)]
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
        #[arg(
            long,
            help = "Memory layer: L0, L1, L2, L3 (auto-assigned from priority if omitted)"
        )]
        layer: Option<String>,
        #[arg(long, help = "Skill trigger pattern (for type=skill)")]
        skill_trigger: Option<String>,
        #[arg(long, value_delimiter = ',', help = "Skill steps (for type=skill)")]
        skill_steps: Option<Vec<String>>,
        #[arg(long, help = "Skill verification criteria (for type=skill)")]
        skill_verification: Option<String>,
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
    Delete { id: String },
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
    /// Extract memories from text
    Extract {
        #[arg(long)]
        text: String,
        /// Auto-save extracted memories
        #[arg(long)]
        save: bool,
        #[arg(long, default_value = "cli")]
        agent_id: String,
    },
    /// Scan for duplicate memories
    Dedup {
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Run decay cycle on all memories
    Decay,
    /// Export memories
    Export {
        /// Output format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Output file or directory
        #[arg(long)]
        output: String,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Import memories
    Import {
        /// Input format: json or markdown
        #[arg(long, default_value = "json")]
        format: String,
        /// Input file or directory
        #[arg(long)]
        input: String,
    },
    /// Confirm memories as read (updates access_count and last_read_at)
    ConfirmRead {
        /// Memory IDs to confirm (comma-separated)
        #[arg(long, value_delimiter = ',')]
        ids: Vec<String>,
    },
    /// Sync memories to project instruction files (CLAUDE.md, AGENTS.md, etc.)
    /// Zero-invasive: agents read these files natively without any configuration.
    Sync {
        /// Project directory to sync to (default: current directory)
        #[arg(long, default_value = ".")]
        dir: String,
        /// Watch mode: poll for database changes and auto-regenerate files
        #[arg(long)]
        watch: bool,
    },
}

fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/")
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(&raw[2..]);
    }
    PathBuf::from(raw)
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

fn parse_layer(s: &str) -> MemoryLayer {
    match s.to_uppercase().as_str() {
        "L0" => MemoryLayer::L0,
        "L2" => MemoryLayer::L2,
        "L3" => MemoryLayer::L3,
        _ => MemoryLayer::L1,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let db_path = resolve_path(&cli.db);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let store = Arc::new(SqliteStore::new(&db_path)?);

    let registry_path = db_path
        .parent()
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
            layer,
            skill_trigger,
            skill_steps,
            skill_verification,
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
            if let Some(l) = layer {
                mem.layer = parse_layer(&l);
            }
            if skill_trigger.is_some() || skill_steps.is_some() || skill_verification.is_some() {
                mem.skill_meta = Some(SkillMeta {
                    trigger: skill_trigger,
                    steps: skill_steps.unwrap_or_default(),
                    verification: skill_verification,
                    version: 1,
                });
            }
            let saved = store.save(mem).await?;
            println!("Saved: {}", saved.id);
        }

        Commands::Search {
            query,
            top_k,
            namespace,
        } => {
            let results = store
                .search(SearchQuery {
                    query,
                    top_k,
                    namespace,
                    ..SearchQuery::new(String::new())
                })
                .await?;
            if results.is_empty() {
                println!("No memories found.");
            } else {
                for r in &results {
                    println!("---");
                    println!(
                        "[{:?}] {} (score: {:.2})",
                        r.memory.priority, r.memory.id, r.score
                    );
                    println!("  {}", r.memory.content);
                    if let Some(ref inst) = r.memory.instruction {
                        println!("  -> {}", inst);
                    }
                }
                println!("--- {} results", results.len());
            }
        }

        Commands::List { limit, namespace } => {
            let memories = store.list(namespace.as_deref(), limit, 0).await?;
            if memories.is_empty() {
                println!("No memories stored.");
            } else {
                for m in &memories {
                    let reviewed = if m.human_reviewed { " ✓" } else { "" };
                    println!(
                        "[{:?}|{:?}] {} — {}{}",
                        m.priority,
                        m.layer,
                        m.id,
                        truncate(&m.content, 50),
                        reviewed
                    );
                }
                println!("Total: {}", memories.len());
            }
        }

        Commands::Delete { id } => {
            store.delete(&id).await?;
            println!("Deleted: {}", id);
        }

        Commands::SessionStart {
            agent_id,
            context,
            project,
        } => {
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
                println!("No content for: {}", uri);
            } else {
                println!("{}", content);
            }
        }

        Commands::Extract {
            text,
            save,
            agent_id,
        } => {
            let extracted = Extractor::extract(&text);
            if extracted.is_empty() {
                println!("No memories extracted.");
            } else {
                for (i, e) in extracted.iter().enumerate() {
                    println!(
                        "{}. [{:?}] {:?} — {}",
                        i + 1,
                        e.priority,
                        e.memory_type,
                        e.content
                    );
                    if let Some(ref inst) = e.instruction {
                        println!("   -> {}", inst);
                    }
                    println!(
                        "   tags: {:?}, confidence: {:.0}%",
                        e.tags,
                        e.confidence * 100.0
                    );
                }
                println!("Extracted {} memories.", extracted.len());

                if save {
                    for e in extracted {
                        let mut mem = Memory::new(
                            e.memory_type,
                            e.content,
                            e.priority,
                            SourceAgent {
                                id: agent_id.clone(),
                                agent_type: "extractor".to_string(),
                                session_id: None,
                            },
                        );
                        mem.instruction = e.instruction;
                        mem.tags = e.tags;
                        mem.confidence = e.confidence;
                        let saved = store.save(mem).await?;
                        println!("  Saved: {}", saved.id);
                    }
                }
            }
        }

        Commands::Dedup { namespace } => {
            let dedup = Deduplicator::new(store, None);
            let result = dedup.scan(namespace.as_deref()).await?;
            println!("Unique: {}", result.unique_count);
            if result.duplicates.is_empty() {
                println!("No duplicates found.");
            } else {
                for d in &result.duplicates {
                    println!("---");
                    println!(
                        "Duplicate of {}: \"{}\" (similarity: {:.0}%)",
                        d.existing_id,
                        truncate(&d.new_content, 50),
                        d.similarity * 100.0
                    );
                    println!("  Action: {:?}", d.action);
                }
                println!("--- {} duplicates found", result.duplicates.len());
            }
        }

        Commands::Decay => {
            let dm = DecayManager::new(store, DecayConfig::default());
            let report = dm.run_decay().await?;
            println!(
                "Decay cycle: {} updated, {} archived",
                report.updated, report.archived
            );
        }

        Commands::Export {
            format,
            output,
            namespace,
        } => {
            let exporter = Exporter::new(store);
            match format.as_str() {
                "json" => {
                    let json = exporter.export_json(namespace.as_deref()).await?;
                    std::fs::write(&output, json)?;
                    println!("Exported to {}", output);
                }
                "markdown" | "md" => {
                    let path = resolve_path(&output);
                    let count = exporter.export_to_dir(&path, namespace.as_deref()).await?;
                    println!("Exported {} files to {}", count, path.display());
                }
                _ => println!("Unknown format: {}. Use 'json' or 'markdown'.", format),
            }
        }

        Commands::Import { format, input } => {
            let importer = Importer::new(store);
            match format.as_str() {
                "json" => {
                    let content = std::fs::read_to_string(&input)?;
                    let count = importer.import_json(&content).await?;
                    println!("Imported {} memories from {}", count, input);
                }
                "markdown" | "md" => {
                    let path = resolve_path(&input);
                    let count = importer.import_from_dir(&path).await?;
                    println!("Imported {} memories from {}", count, path.display());
                }
                _ => println!("Unknown format: {}. Use 'json' or 'markdown'.", format),
            }
        }

        Commands::ConfirmRead { ids } => {
            router.confirm_read(&ids).await?;
            println!("Confirmed {} memories as read.", ids.len());
        }

        Commands::Sync { dir, watch } => {
            let sync_dir = resolve_path(&dir);
            let engine = SyncEngine::new(store);

            if watch {
                println!(
                    "Watching for changes in database (interval: {}s)...",
                    engine.config().watch_interval_secs
                );
                println!("Generating instruction files in: {}", sync_dir.display());
                println!("Press Ctrl+C to stop.");

                let stop = Arc::new(AtomicBool::new(false));
                let s = stop.clone();
                tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    s.store(true, Ordering::Relaxed);
                    println!("\nShutting down...");
                });

                engine.sync_with_watch(&sync_dir, stop).await?;
            } else {
                let report = engine.sync(&sync_dir).await?;
                println!(
                    "Synced {} memories to {} files:",
                    report.memories_synced,
                    report.files_written.len()
                );
                for f in &report.files_written {
                    println!("  {}", f.display());
                }
            }
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}
