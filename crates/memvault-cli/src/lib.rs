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
    /// Record the outcome of an executed task (episodic memory).
    /// Failures are later reflected into lessons for similar future tasks.
    Outcome {
        #[arg(long, help = "What task was executed")]
        task: String,
        #[arg(long, help = "success | failure | partial")]
        status: String,
        #[arg(long, help = "Attribution of the outcome, when known")]
        cause: Option<String>,
        #[arg(long, help = "Coarse task category (deploy/debug/refactor/...)")]
        task_type: Option<String>,
        #[arg(
            long,
            help = "Skill memory followed during the task (attributes the outcome to its stats)"
        )]
        skill_id: Option<String>,
        #[arg(long, default_value = "global")]
        namespace: String,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long, default_value = "cli")]
        agent_id: String,
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
    /// Review the pending-review queue (list, or approve/reject a single memory).
    /// Backend equivalent of the dashboard's Review tab.
    Review {
        #[arg(long, help = "Approve a pending memory by id (marks human-reviewed)")]
        approve: Option<String>,
        #[arg(long, help = "Reject (delete) a pending memory by id")]
        reject: Option<String>,
        #[arg(long, default_value = "20")]
        limit: usize,
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
    /// List history entries (checkpoints) for a memory, or recent changes across all memories
    Checkpoints {
        #[arg(long)]
        memory_id: Option<String>,
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Restore a memory to the state captured by a checkpoint from `checkpoints`
    Restore {
        #[arg(long)]
        history_id: i64,
    },
    /// Show embedding provider status and which features are degraded without it
    Status,
}

pub fn resolve_path(raw: &str) -> PathBuf {
    if raw.starts_with("~/")
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(&raw[2..]);
    }
    PathBuf::from(raw)
}

/// Parse a priority string, rejecting unknown values instead of silently
/// downgrading a MUST memory to Reference.
pub fn parse_priority(s: &str) -> Result<Priority, String> {
    match s.to_uppercase().as_str() {
        "MUST" => Ok(Priority::Must),
        "REFERENCE" => Ok(Priority::Reference),
        "BACKGROUND" => Ok(Priority::Background),
        _ => Err(format!("invalid priority: {s}")),
    }
}

/// Parse a memory-type string, rejecting unknown values instead of silently
/// turning a typed memory into a Fact.
pub fn parse_memory_type(s: &str) -> Result<MemoryType, String> {
    match s.to_lowercase().as_str() {
        "preference" => Ok(MemoryType::Preference),
        "episode" => Ok(MemoryType::Episode),
        "entity" => Ok(MemoryType::Entity),
        "skill" => Ok(MemoryType::Skill),
        "fact" => Ok(MemoryType::Fact),
        _ => Err(format!("invalid memory type: {s}")),
    }
}

/// Parse a layer string, rejecting unknown values instead of silently
/// defaulting to L1.
pub fn parse_layer(s: &str) -> Result<MemoryLayer, String> {
    match s.to_uppercase().as_str() {
        "L0" => Ok(MemoryLayer::L0),
        "L1" => Ok(MemoryLayer::L1),
        "L2" => Ok(MemoryLayer::L2),
        "L3" => Ok(MemoryLayer::L3),
        _ => Err(format!("invalid layer: {s}")),
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
                parse_memory_type(&r#type).map_err(anyhow::Error::msg)?,
                content,
                parse_priority(&priority).map_err(anyhow::Error::msg)?,
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
                mem.layer = parse_layer(&l).map_err(anyhow::Error::msg)?;
            }
            if skill_trigger.is_some() || skill_steps.is_some() || skill_verification.is_some() {
                mem.skill_meta = Some(SkillMeta {
                    trigger: skill_trigger,
                    steps: skill_steps.unwrap_or_default(),
                    verification: skill_verification,
                    version: 1,
                });
            }
            // 保存时优先生成向量(与 MCP/proxy 一致):embedder 可用则写入 int8,
            // 否则降级无向量保存并告警;MEMVAULT_EMBEDDING_PROVIDER=off 可整体关闭。
            let embed_text = mem
                .instruction
                .clone()
                .unwrap_or_else(|| mem.content.clone())
                .to_string();

            match memvault_core::embedding::build_embedder_from_env().await {
                Some(embedder) => match embedder.embed(&[embed_text]).await {
                    Ok(embeddings) if !embeddings.is_empty() => {
                        let saved = store
                            .save_with_embedding(mem, embeddings.into_iter().next().unwrap())
                            .await?;
                        println!("Saved: {} (embedded int8)", saved.id);
                    }
                    Err(e) => {
                        eprintln!("warning: embedding failed ({}), saving without vector", e);
                        let saved = store.save(mem).await?;
                        println!("Saved: {}", saved.id);
                    }
                    _ => {
                        let saved = store.save(mem).await?;
                        println!("Saved: {}", saved.id);
                    }
                },
                None => {
                    let saved = store.save(mem).await?;
                    println!("Saved: {}", saved.id);
                }
            }
        }

        Commands::Outcome {
            task,
            status,
            cause,
            task_type,
            skill_id,
            namespace,
            tags,
            agent_id,
        } => {
            let status = memvault_core::models::OutcomeStatus::parse(&status).ok_or_else(|| {
                anyhow::anyhow!("invalid status '{status}' — expected success, failure, or partial")
            })?;
            let input = memvault_core::episode::OutcomeInput {
                task,
                status,
                cause,
                task_type,
                skill_id,
                tags: tags.unwrap_or_default(),
                namespace,
                source_agent: SourceAgent {
                    id: agent_id,
                    agent_type: "cli".to_string(),
                    session_id: None,
                },
            };
            let embedder = memvault_core::embedding::build_embedder_from_env().await;
            let llm = memvault_core::llm_extractor::build_llm_extractor_from_env().await;
            let recorded = memvault_core::episode::record_outcome(
                store.as_ref(),
                input.clone(),
                embedder.as_deref(),
            )
            .await?;
            println!(
                "Recorded: {} — {}",
                recorded.memory.id, recorded.memory.content
            );
            if recorded.embedded {
                println!("(embedded int8)");
            }
            if !recorded.flagged_skills.is_empty() {
                println!(
                    "Flagged for revision: {}",
                    recorded.flagged_skills.join(", ")
                );
            }
            if let Some(ref draft_id) = recorded.skill_draft_id {
                println!("Skill draft created (needs review): {}", draft_id);
            }

            match memvault_core::reflection::reflect_and_store(
                store.as_ref(),
                &recorded.memory.id,
                input.into(),
                llm.as_deref(),
                embedder.as_deref(),
            )
            .await
            {
                Ok(Some(record)) => {
                    println!(
                        "Lesson ({:?}): {} [{}]",
                        record.source, record.lesson, record.lesson_memory.id
                    );
                    if let Some(hint) = record.escalation_hint {
                        println!("Hint: {}", hint);
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("warning: lesson reflection failed ({}); outcome kept", e),
            }
        }

        Commands::Search {
            query,
            top_k,
            namespace,
        } => {
            let outcome = store
                .search(SearchQuery {
                    query,
                    top_k,
                    namespace,
                    ..SearchQuery::new(String::new())
                })
                .await?;
            let results = outcome.results;
            // Relaxed keyword matches must be disclosed: silently presenting
            // them as exact matches misleads the user about recall quality.
            match outcome.keyword_tier {
                memvault_core::models::KeywordTier::RelaxedUnigram => {
                    println!(
                        "(note: relaxed match — fell back to single characters; results may be less precise)"
                    );
                }
                memvault_core::models::KeywordTier::SynonymFallback => {
                    println!(
                        "(note: relaxed match — fell back to synonym/any-token matching; results may be less precise)"
                    );
                }
                _ => {}
            }
            if results.is_empty() {
                println!("No memories found.");
            } else {
                for r in &results {
                    println!("---");
                    let sources = r
                        .hit_sources
                        .iter()
                        .map(|h| h.tag())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if sources.is_empty() {
                        println!(
                            "[{:?}] {} (score: {:.2})",
                            r.memory.priority, r.memory.id, r.score
                        );
                    } else {
                        println!(
                            "[{:?}] {} (score: {:.2}) [{}]",
                            r.memory.priority, r.memory.id, r.score, sources
                        );
                    }
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
        Commands::Review {
            approve,
            reject,
            limit,
        } => {
            if let Some(id) = approve {
                let mut mem = store.get(&id).await?;
                if mem.human_reviewed {
                    println!("Already reviewed: {id}");
                } else {
                    mem.human_reviewed = true;
                    store.update(mem).await?;
                    println!("Approved: {id}");
                }
            } else if let Some(id) = reject {
                store.delete(&id).await?;
                println!("Rejected (deleted): {id}");
            } else {
                let pending = store.list_pending(None, limit, 0).await?;
                if pending.is_empty() {
                    println!("No memories pending review.");
                } else {
                    for m in &pending {
                        println!(
                            "[{:?}|{:?}] {} — {}",
                            m.priority,
                            m.layer,
                            m.id,
                            truncate(&m.content, 60)
                        );
                    }
                    println!("Pending: {}", pending.len());
                }
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
            let injection = router
                .session_start(&agent_id, context.as_deref(), project.as_deref())
                .await?;
            let formatted = router.format_as_instructions(&injection.results);
            if formatted.is_empty() {
                println!("No memories to inject for agent '{}'.", agent_id);
            } else {
                println!("{}", formatted);
            }
            // Skip reasons make injection decisions auditable: "I saved it, why
            // didn't the agent get it?" must have an answer.
            if !injection.skipped.is_empty() {
                println!("--- {} candidate(s) not injected:", injection.skipped.len());
                for s in &injection.skipped {
                    println!("  • {} — {}", s.id, s.reason);
                }
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
            let outcome = Extractor::extract_with_coverage(&text);
            let extracted = outcome.memories;
            // Coverage first: users must see how much of the input was
            // actually covered, not only what was found.
            let cov = &outcome.coverage;
            println!(
                "Coverage: {} line(s) in — {} extracted, {} no signal, {} empty",
                cov.input_lines, cov.extracted_lines, cov.no_signal_lines, cov.empty_lines
            );
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
            // Wire up the configured embedding provider (if any) so CLI
            // dedup gets the same vector-assisted matching as memvault-mcp
            // and memvault-proxy, instead of always running keyword-only.
            let embedder = memvault_core::embedding::build_embedder_from_env().await;
            let dedup = Deduplicator::new(store, embedder);
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
                if !report.files_skipped.is_empty() {
                    println!("Skipped {} target(s):", report.files_skipped.len());
                    for s in &report.files_skipped {
                        println!("  {} — {}", s.target, s.reason);
                    }
                }
            }
        }

        Commands::Checkpoints { memory_id, limit } => {
            let entries = store.list_checkpoints(memory_id.as_deref(), limit).await?;
            if entries.is_empty() {
                println!("No history entries found.");
            } else {
                for e in &entries {
                    println!(
                        "[{}] {} memory={} at={}",
                        e.history_id, e.operation, e.memory_id, e.changed_at
                    );
                }
            }
        }

        Commands::Restore { history_id } => {
            let restored = store.restore_checkpoint(history_id).await?;
            println!(
                "Restored: {} (\"{}\")",
                restored.id,
                truncate(&restored.content, 60)
            );
        }

        Commands::Status => {
            let embedder = memvault_core::embedding::build_embedder_from_env().await;
            match &embedder {
                Some(_) => println!("Embedding provider: configured and reachable"),
                None => println!("Embedding provider: none configured -> keyword-only mode"),
            }
            for cap in memvault_core::capabilities::capability_report(&embedder) {
                let mark = if cap.available { "✓" } else { "✗" };
                println!("  [{mark}] {} — {}", cap.name, cap.note);
            }
            match store.schema_fingerprint() {
                Ok((version, checksum)) => {
                    println!("Schema: v{} (fingerprint {})", version, checksum);
                }
                Err(e) => println!("Schema: fingerprint unavailable ({e})"),
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

    fn outcome_cmd(task: &str, status: &str) -> Commands {
        Commands::Outcome {
            task: task.to_string(),
            status: status.to_string(),
            cause: None,
            task_type: None,
            skill_id: None,
            namespace: "global".to_string(),
            tags: None,
            agent_id: "cli".to_string(),
        }
    }

    #[tokio::test]
    async fn test_outcome_records_episode() {
        let db = temp_db();
        run(cli(db.clone(), outcome_cmd("deploy the cli", "failure")))
            .await
            .unwrap();

        let store = SqliteStore::new(std::path::Path::new(&db)).unwrap();
        let memories = store.list(None, 100, 0).await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::Episode);
        assert!(memories[0].content.contains("[failure] deploy the cli"));

        let episodes = store.list_episodes(EpisodeFilter::default()).await.unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].status, OutcomeStatus::Failure);
    }

    #[tokio::test]
    async fn test_outcome_rejects_invalid_status() {
        let db = temp_db();
        let err = run(cli(db, outcome_cmd("deploy", "maybe")))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid status"));
    }

    #[test]
    fn test_parse_priority_variants() {
        assert_eq!(parse_priority("MUST"), Ok(Priority::Must));
        assert_eq!(parse_priority("must"), Ok(Priority::Must));
        assert_eq!(parse_priority("REFERENCE"), Ok(Priority::Reference));
        assert_eq!(parse_priority("BACKGROUND"), Ok(Priority::Background));
    }

    /// Regression: a typo'd --priority used to silently downgrade a
    /// would-be MUST memory to Reference with no warning at all — the same
    /// bug class REST's parse_priority already rejects with a 400.
    #[test]
    fn test_parse_priority_rejects_unknown() {
        assert!(parse_priority("anything-else").is_err());
        assert!(parse_priority("MSUT").is_err());
    }

    #[test]
    fn test_parse_memory_type_variants() {
        assert_eq!(parse_memory_type("preference"), Ok(MemoryType::Preference));
        assert_eq!(parse_memory_type("episode"), Ok(MemoryType::Episode));
        assert_eq!(parse_memory_type("entity"), Ok(MemoryType::Entity));
        assert_eq!(parse_memory_type("skill"), Ok(MemoryType::Skill));
        assert_eq!(parse_memory_type("fact"), Ok(MemoryType::Fact));
    }

    #[test]
    fn test_parse_memory_type_rejects_unknown() {
        assert!(parse_memory_type("unknown").is_err());
    }

    #[test]
    fn test_parse_layer_variants() {
        assert_eq!(parse_layer("L0"), Ok(MemoryLayer::L0));
        assert_eq!(parse_layer("l2"), Ok(MemoryLayer::L2));
        assert_eq!(parse_layer("L3"), Ok(MemoryLayer::L3));
    }

    #[test]
    fn test_parse_layer_rejects_unknown() {
        assert!(parse_layer("bogus").is_err());
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
    async fn test_review_list_approve_reject() {
        let db = temp_db();
        for i in 0..3 {
            run(cli(db.clone(), save_cmd(&format!("pending memory {i}"))))
                .await
                .unwrap();
        }
        // No-arg review lists the pending queue.
        run(cli(
            db.clone(),
            Commands::Review {
                approve: None,
                reject: None,
                limit: 10,
            },
        ))
        .await
        .unwrap();

        let all = list_all(&db).await;
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|m| !m.human_reviewed));

        // Approve one => drops out of the pending queue, becomes reviewed.
        let first = all[0].id.clone();
        run(cli(
            db.clone(),
            Commands::Review {
                approve: Some(first.clone()),
                reject: None,
                limit: 10,
            },
        ))
        .await
        .unwrap();
        let all = list_all(&db).await;
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter()
                .filter(|m| m.id == first && m.human_reviewed)
                .count(),
            1,
            "approved memory must be marked human-reviewed"
        );
        assert_eq!(
            all.iter().filter(|m| !m.human_reviewed).count(),
            2,
            "exactly two should remain pending"
        );

        // Reject = deletion.
        let second = all.iter().find(|m| !m.human_reviewed).unwrap().id.clone();
        run(cli(
            db.clone(),
            Commands::Review {
                approve: None,
                reject: Some(second.clone()),
                limit: 10,
            },
        ))
        .await
        .unwrap();
        let all = list_all(&db).await;
        assert_eq!(all.len(), 2);
        assert!(
            !all.iter().any(|m| m.id == second),
            "rejected memory deleted"
        );
    }

    /// Regression: `memvault save --priority MSUT` (typo) used to silently
    /// save as REFERENCE with no error — the CLI never got the same 400-on-
    /// bad-input treatment REST's parse_priority already has.
    #[tokio::test]
    async fn test_save_rejects_invalid_priority() {
        let db = temp_db();
        let result = run(cli(
            db.clone(),
            Commands::Save {
                content: "typo'd priority".to_string(),
                priority: "MSUT".to_string(),
                r#type: "fact".to_string(),
                namespace: "global".to_string(),
                agent_id: "cli".to_string(),
                instruction: None,
                tags: None,
                layer: None,
                skill_trigger: None,
                skill_steps: None,
                skill_verification: None,
            },
        ))
        .await;
        assert!(result.is_err());
        assert!(list_all(&db).await.is_empty(), "nothing should be saved");
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
    async fn test_checkpoints_lists_history() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Save {
                content: "to be deleted".to_string(),
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
            },
        ))
        .await
        .unwrap();

        let id = list_all(&db).await[0].id.clone();
        run(cli(db.clone(), Commands::Delete { id: id.clone() }))
            .await
            .unwrap();

        // Checkpoints only prints, so assert through the store directly.
        let store = SqliteStore::new(std::path::Path::new(&db)).unwrap();
        let history = store.list_checkpoints(Some(&id), 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].operation, "delete");

        // The CLI command itself should also run without error.
        run(cli(
            db.clone(),
            Commands::Checkpoints {
                memory_id: Some(id),
                limit: 10,
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_restore_reverts_to_prior_state() {
        let db = temp_db();
        run(cli(
            db.clone(),
            Commands::Save {
                content: "original content".to_string(),
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
            },
        ))
        .await
        .unwrap();

        let id = list_all(&db).await[0].id.clone();

        // No CLI subcommand performs a raw update, so go through the store
        // directly to set up the "someone edited this" precondition.
        let store = SqliteStore::new(std::path::Path::new(&db)).unwrap();
        let mut edited = store.get(&id).await.unwrap();
        edited.content = "overwritten by mistake".to_string();
        store.update(edited).await.unwrap();
        assert_eq!(
            store.get(&id).await.unwrap().content,
            "overwritten by mistake"
        );

        let history_id = store.list_checkpoints(Some(&id), 10).await.unwrap()[0].history_id;

        run(cli(db.clone(), Commands::Restore { history_id }))
            .await
            .unwrap();

        assert_eq!(store.get(&id).await.unwrap().content, "original content");
    }

    #[tokio::test]
    async fn test_status_runs_without_embedder_configured() {
        // Correctness of the degraded/available split itself is covered by
        // memvault_core::capabilities unit tests; this just checks the CLI
        // wiring (build_embedder_from_env + capability_report + print) runs
        // end to end without an embedding provider configured.
        let db = temp_db();
        run(cli(db, Commands::Status)).await.unwrap();
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
