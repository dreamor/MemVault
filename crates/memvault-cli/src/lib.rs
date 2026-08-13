//! MemVault CLI library.
//!
//! The command-line dispatch lives here (not in [`main.rs`]) so every command can
//! be exercised by unit tests against a temporary database.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::{Parser, Subcommand};

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
pub struct Cli {
    #[arg(long, default_value = "~/.memvault/data.db")]
    pub db: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
    /// Run promote pipeline: consolidate L1→L2, promote L2→L3
    Promote {
        #[arg(
            long,
            default_value = "3",
            help = "Min L1 memories to consolidate into L2"
        )]
        min_l1: usize,
        #[arg(long, default_value = "2", help = "Min L2 memories to promote to L3")]
        min_l2: usize,
    },
    /// Run decay cycle on all memories
    Decay,
    /// Create a consistent point-in-time database backup
    Backup {
        #[arg(long, help = "Path to write the backup file")]
        output: PathBuf,
    },
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

pub fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/")
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(&raw[2..]);
    }
    PathBuf::from(raw)
}

pub fn parse_priority(s: &str) -> Priority {
    match s.to_uppercase().as_str() {
        "MUST" => Priority::Must,
        "BACKGROUND" => Priority::Background,
        _ => Priority::Reference,
    }
}

pub fn parse_memory_type(s: &str) -> MemoryType {
    match s.to_lowercase().as_str() {
        "preference" => MemoryType::Preference,
        "episode" => MemoryType::Episode,
        "entity" => MemoryType::Entity,
        "skill" => MemoryType::Skill,
        _ => MemoryType::Fact,
    }
}

pub fn parse_layer(s: &str) -> MemoryLayer {
    match s.to_uppercase().as_str() {
        "L0" => MemoryLayer::L0,
        "L2" => MemoryLayer::L2,
        "L3" => MemoryLayer::L3,
        _ => MemoryLayer::L1,
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max).collect::<String>())
    }
}

/// Execute the given CLI command against the database path in `cli`.
pub async fn run(cli: Cli) -> Result<()> {
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

        Commands::Backup { output } => {
            store.backup_to(&output).await?;
            println!("Backup written to {}", output.display());
        }

        Commands::Promote { min_l1, min_l2 } => {
            use memvault_core::promote::{PromoteConfig, Promoter};
            let config = PromoteConfig {
                min_l1_for_l2: min_l1,
                min_l2_for_l3: min_l2,
                ..PromoteConfig::default()
            };
            let promoter = Promoter::new(store, config);
            let result = promoter.run().await?;
            println!(
                "Promote: {} consolidated to L2, {} promoted to L3 ({} sources consumed)",
                result.promoted_to_l2,
                result.promoted_to_l3,
                result.source_ids_consumed.len()
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

#[cfg(test)]
mod tests {
    use super::*;
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;
    use uuid::Uuid;

    /// A fresh per-test database path inside a unique temp directory.
    fn temp_db() -> String {
        let dir =
            std::env::temp_dir().join(format!("memvault_cli_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("test.db").to_string_lossy().to_string()
    }

    fn cli(db: String, command: Commands) -> Cli {
        Cli { db, command }
    }

    async fn list_all(db: &str) -> Vec<Memory> {
        // Reopening the same SQLite file shares persisted rows.
        SqliteStore::new(std::path::Path::new(db))
            .unwrap()
            .list(None, 1000, 0)
            .await
            .expect("list should succeed")
    }

    fn save_cmd(content: &str) -> Commands {
        Commands::Save {
            content: content.to_string(),
            priority: "REFERENCE".to_string(),
            r#type: "fact".to_string(),
            namespace: "global".to_string(),
            agent_id: "cli".to_string(),
            instruction: None,
            tags: None,
            layer: None,
            skill_trigger: None,
            skill_steps: None,
            skill_verification: None,
        }
    }

    #[test]
    fn test_parse_priority_variants() {
        assert_eq!(parse_priority("MUST"), Priority::Must);
        assert_eq!(parse_priority("must"), Priority::Must);
        assert_eq!(parse_priority("BACKGROUND"), Priority::Background);
        assert_eq!(parse_priority("anything-else"), Priority::Reference);
    }

    #[test]
    fn test_parse_memory_type_variants() {
        assert_eq!(parse_memory_type("preference"), MemoryType::Preference);
        assert_eq!(parse_memory_type("episode"), MemoryType::Episode);
        assert_eq!(parse_memory_type("entity"), MemoryType::Entity);
        assert_eq!(parse_memory_type("skill"), MemoryType::Skill);
        assert_eq!(parse_memory_type("unknown"), MemoryType::Fact);
    }

    #[test]
    fn test_parse_layer_variants() {
        assert_eq!(parse_layer("L0"), MemoryLayer::L0);
        assert_eq!(parse_layer("l2"), MemoryLayer::L2);
        assert_eq!(parse_layer("L3"), MemoryLayer::L3);
        assert_eq!(parse_layer("bogus"), MemoryLayer::L1);
    }

    #[test]
    fn test_truncate_behavior() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn test_resolve_path_tilde() {
        let home = std::env::var("HOME").unwrap_or_default();
        let expanded = resolve_path("~/x/y.db");
        assert!(
            expanded.starts_with(&home),
            "expanded {:?} should live under HOME {:?}",
            expanded,
            home
        );
        assert!(expanded.to_string_lossy().ends_with("x/y.db"));
        assert_eq!(resolve_path("/abs/path.db"), PathBuf::from("/abs/path.db"));
    }

    #[tokio::test]
    async fn test_save_then_search_list_and_delete() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Save {
                content: "prefers Rust".to_string(),
                priority: "MUST".to_string(),
                r#type: "preference".to_string(),
                namespace: "global".to_string(),
                agent_id: "cli".to_string(),
                instruction: Some("always use Rust".to_string()),
                tags: Some(vec!["coding".to_string(), "lang".to_string()]),
                layer: Some("L3".to_string()),
                skill_trigger: None,
                skill_steps: None,
                skill_verification: None,
            },
        ))
        .await
        .unwrap();

        let all = list_all(&db).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "prefers Rust");
        assert_eq!(all[0].priority, Priority::Must);
        assert_eq!(all[0].memory_type, MemoryType::Preference);
        assert_eq!(all[0].layer, MemoryLayer::L3);

        // Search finds it.
        run(cli(
            db.clone(),
            Commands::Search {
                query: "Rust".to_string(),
                top_k: 10,
                namespace: None,
            },
        ))
        .await
        .unwrap();

        // List with a filtered namespace still works.
        run(cli(
            db.clone(),
            Commands::List {
                limit: 10,
                namespace: Some("global".to_string()),
            },
        ))
        .await
        .unwrap();

        let id = all[0].id.clone();
        run(cli(
            db.clone(),
            Commands::ConfirmRead {
                ids: vec![id.clone()],
            },
        ))
        .await
        .unwrap();

        run(cli(db.clone(), Commands::Delete { id })).await.unwrap();
        assert!(list_all(&db).await.is_empty());
    }

    #[tokio::test]
    async fn test_list_empty_nothing_matches() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Search {
                query: "zzz".to_string(),
                top_k: 5,
                namespace: None,
            },
        ))
        .await
        .unwrap();
        run(cli(
            db.clone(),
            Commands::List {
                limit: 10,
                namespace: None,
            },
        ))
        .await
        .unwrap();
        run(cli(
            db.clone(),
            Commands::Resource {
                uri: "memory://user-profile".to_string(),
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_session_start_and_resource_empty() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::SessionStart {
                agent_id: "claude-desktop".to_string(),
                context: Some("hi".to_string()),
                project: None,
            },
        ))
        .await
        .unwrap();
        run(cli(
            db.clone(),
            Commands::Resource {
                uri: "memory://project-context".to_string(),
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_extract_with_and_without_save() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Extract {
                text: "The weather is fine.".to_string(),
                save: false,
                agent_id: "cli".to_string(),
            },
        ))
        .await
        .unwrap();
        assert!(list_all(&db).await.is_empty());

        run(cli(
            db.clone(),
            Commands::Extract {
                text: "I always prefer dark mode".to_string(),
                save: true,
                agent_id: "cli".to_string(),
            },
        ))
        .await
        .unwrap();
        assert_eq!(list_all(&db).await.len(), 1);
    }

    #[tokio::test]
    async fn test_dedup_decay_promote_run() {
        let db = temp_db();
        run(cli(db.clone(), save_cmd("alpha"))).await.unwrap();
        run(cli(db.clone(), Commands::Dedup { namespace: None }))
            .await
            .unwrap();
        run(cli(db.clone(), Commands::Decay)).await.unwrap();
        run(cli(
            db.clone(),
            Commands::Promote {
                min_l1: 3,
                min_l2: 2,
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_export_and_import_roundtrip() {
        let db = temp_db();
        run(cli(db.clone(), save_cmd("roundtrip"))).await.unwrap();

        let out = std::env::temp_dir().join(format!(
            "memvault_cli_export_{}.json",
            Uuid::new_v4().simple()
        ));
        run(cli(
            db.clone(),
            Commands::Export {
                format: "json".to_string(),
                output: out.to_string_lossy().to_string(),
                namespace: None,
            },
        ))
        .await
        .unwrap();
        assert!(out.exists());

        // Import into a fresh database.
        let db2 = temp_db();
        run(cli(
            db2.clone(),
            Commands::Import {
                format: "json".to_string(),
                input: out.to_string_lossy().to_string(),
            },
        ))
        .await
        .unwrap();
        assert_eq!(list_all(&db2).await.len(), 1);

        // Unknown export format is tolerated without error.
        run(cli(
            db.clone(),
            Commands::Export {
                format: "xml".to_string(),
                output: out.to_string_lossy().to_string(),
                namespace: None,
            },
        ))
        .await
        .unwrap();

        std::fs::remove_file(&out).ok();
    }

    #[tokio::test]
    async fn test_backup_writes_file() {
        let db = temp_db();
        let backup =
            std::env::temp_dir().join(format!("memvault_cli_bak_{}.db", Uuid::new_v4().simple()));
        run(cli(
            db.clone(),
            Commands::Backup {
                output: backup.clone(),
            },
        ))
        .await
        .unwrap();
        assert!(backup.exists());
        std::fs::remove_file(backup).ok();
    }

    #[tokio::test]
    async fn test_save_skill_memory_with_meta() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Save {
                content: "release steps".to_string(),
                priority: "MUST".to_string(),
                r#type: "skill".to_string(),
                namespace: "project:ops".to_string(),
                agent_id: "cli".to_string(),
                instruction: Some("follow the runbook".to_string()),
                tags: Some(vec!["ops".to_string()]),
                layer: None,
                skill_trigger: Some("release".to_string()),
                skill_steps: Some(vec!["build".to_string(), "tag".to_string()]),
                skill_verification: Some("health check".to_string()),
            },
        ))
        .await
        .unwrap();

        let all = list_all(&db).await;
        assert_eq!(all.len(), 1);
        let meta = all[0].skill_meta.as_ref().expect("skill meta stored");
        assert_eq!(meta.trigger.as_deref(), Some("release"));
        assert_eq!(meta.steps.len(), 2);
    }

    #[tokio::test]
    async fn test_session_start_with_memory_present() {
        let db = temp_db();
        run(cli(db.clone(), save_cmd("remember this")))
            .await
            .unwrap();
        run(cli(
            db.clone(),
            Commands::SessionStart {
                agent_id: "claude-desktop".to_string(),
                context: Some("begin".to_string()),
                project: None,
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_resource_with_content() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Save {
                content: "user prefers terminal tools".to_string(),
                priority: "MUST".to_string(),
                r#type: "preference".to_string(),
                namespace: "global".to_string(),
                agent_id: "cli".to_string(),
                instruction: Some("always use terminal".to_string()),
                tags: None,
                layer: None,
                skill_trigger: None,
                skill_steps: None,
                skill_verification: None,
            },
        ))
        .await
        .unwrap();
        run(cli(
            db.clone(),
            Commands::Resource {
                uri: "memory://user-profile".to_string(),
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_export_markdown_branch() {
        let db = temp_db();
        run(cli(db.clone(), save_cmd("md export"))).await.unwrap();
        let out_dir =
            std::env::temp_dir().join(format!("memvault_cli_md_{}", Uuid::new_v4().simple()));
        run(cli(
            db.clone(),
            Commands::Export {
                format: "markdown".to_string(),
                output: out_dir.to_string_lossy().to_string(),
                namespace: None,
            },
        ))
        .await
        .unwrap();
        assert!(out_dir.is_dir());
        let count = std::fs::read_dir(&out_dir).unwrap().count();
        assert_eq!(count, 1);
        std::fs::remove_dir_all(out_dir).ok();
    }

    #[tokio::test]
    async fn test_import_markdown_and_unknown_formats() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_cli_md_import_{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("note.md"),
            "---\nid: mem_cli_import\ntype: fact\npriority: REFERENCE\n---\n\nimported note\n",
        )
        .unwrap();

        run(cli(
            db.clone(),
            Commands::Import {
                format: "markdown".to_string(),
                input: dir.to_string_lossy().to_string(),
            },
        ))
        .await
        .unwrap();
        let all = list_all(&db).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "imported note");

        // Unknown formats are tolerated.
        run(cli(
            db.clone(),
            Commands::Import {
                format: "xml".to_string(),
                input: dir.to_string_lossy().to_string(),
            },
        ))
        .await
        .unwrap();
        assert_eq!(list_all(&db).await.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_sync_non_watch_writes_report() {
        let db = temp_db();
        let out_dir =
            std::env::temp_dir().join(format!("memvault_cli_sync_{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&out_dir).unwrap();
        run(cli(
            db.clone(),
            Commands::Sync {
                dir: out_dir.to_string_lossy().to_string(),
                watch: false,
            },
        ))
        .await
        .unwrap();
        std::fs::remove_dir_all(out_dir).ok();
    }
}
