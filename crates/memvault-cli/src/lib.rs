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
        #[arg(long, help = "Sharing scope: scoped (default) or shared (team pool)")]
        visibility: Option<String>,
        #[arg(
            long,
            help = "Force insert, skipping delta-write dedup/merge (docs/PAPER-INSPIRATIONS.md Feature A)"
        )]
        force: bool,
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
        #[arg(
            long,
            value_name = "PATH",
            help = "Host hook stdin JSON (Claude Code SessionStart payload); '-' reads stdin. \
                    Takes precedence over --context/--project"
        )]
        hook_input: Option<String>,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "plain",
            help = "Output: plain (human) or hook-json (Claude Code SessionStart envelope)"
        )]
        format: String,
    },
    /// Show MCP Resource content
    Resource {
        #[arg(default_value = "memory://user-profile")]
        uri: String,
    },
    /// Extract memories from text or a host transcript
    Extract {
        #[arg(long, help = "Text to extract from")]
        text: Option<String>,
        /// Auto-save extracted memories
        #[arg(long)]
        save: bool,
        #[arg(long, default_value = "cli")]
        agent_id: String,
        #[arg(
            long,
            value_name = "PATH",
            help = "Host transcript file to extract from; '-' reads stdin"
        )]
        transcript: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Host hook stdin JSON (e.g. Claude Code Stop payload); '-' reads stdin. \
                    Its transcript_path is used when --transcript is absent"
        )]
        hook_input: Option<String>,
        #[arg(
            long,
            help = "Mark saved memories as human-reviewed (skip the review inbox)"
        )]
        approve: bool,
        #[arg(
            long,
            value_name = "KIND",
            default_value = "auto",
            help = "Transcript input kind: auto (sniff Claude Code JSONL, fall back to text) or text"
        )]
        source: String,
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
    /// Import skills from Markdown SOP files (Phase D). Each #/## heading
    /// becomes a skill; `trigger:`/`verification:` lines and list items
    /// become the skill metadata. Imported skills enter the review inbox
    /// unless --approve is given.
    ImportSkills {
        #[arg(long, help = "SOP markdown file")]
        file: Option<PathBuf>,
        #[arg(long, help = "Directory of .md SOP files")]
        dir: Option<PathBuf>,
        #[arg(long, default_value = "global")]
        namespace: String,
        #[arg(long, help = "Mark imported skills as human-reviewed (skip inbox)")]
        approve: bool,
    },
    /// Cold-start import: read another agent's native memory files (Claude
    /// Code's CLAUDE.md/auto-memory, Codex CLI's AGENTS.md, Hermes Agent's
    /// USER.md/MEMORY.md/skills, Qoder's .qoder/rules, OpenClaw's memory
    /// store — experimental) and save them as candidate memories. Imported
    /// memories enter the review inbox unless --approve is given, and are
    /// always saved at REFERENCE priority regardless of the source format.
    /// For any other agent, pass --paste with the copied memory text as a
    /// generic fallback.
    ImportAgent {
        #[arg(
            long,
            help = "Agent to import from (claude, codex, hermes, qoder, openclaw); omit or pass 'all' to try every known agent"
        )]
        agent: Option<String>,
        #[arg(long, help = "Only detect and report; read nothing, write nothing")]
        scan: bool,
        #[arg(
            long,
            help = "Override the auto-detected source file/dir (requires exactly one --agent)"
        )]
        path: Option<PathBuf>,
        #[arg(
            long,
            help = "Override the namespace inferred for every imported candidate"
        )]
        namespace: Option<String>,
        #[arg(
            long,
            help = "Parse and report what would be imported without writing to the store"
        )]
        dry_run: bool,
        #[arg(long, help = "Mark imported memories as human-reviewed (skip inbox)")]
        approve: bool,
        #[arg(
            long,
            help = "Generic fallback for any agent without a dedicated adapter: pass the copied memory text directly ('-' reads stdin), extracted the same way as `memvault extract`. --agent becomes a free-form label instead of an adapter key, and --scan/--path do not apply."
        )]
        paste: Option<String>,
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
    /// Mark one memory as superseded by another (human-confirmed knowledge
    /// replacement). The old memory is archived to L0 with a pointer — never
    /// deleted — and retrieval serves the replacement from then on.
    Supersede {
        #[arg(long, help = "ID of the outdated memory")]
        old: String,
        #[arg(long, help = "ID of the replacement memory")]
        new: String,
    },
    /// Show embedding provider status and which features are degraded without it
    Status,
    /// Task-level memory benchmark (docs/PAPER-INSPIRATIONS.md Feature B).
    /// Samples your own outcome history (episodes that distilled a lesson) and
    /// measures: does the lesson get retrieved? does it get injected? With
    /// `--judge`, an LLM additionally scores "plan without memory" vs "plan
    /// with injected memory" against the known failure cause — the delta is
    /// the task-level value of your memory (not just retrieval recall).
    Bench {
        #[arg(long, default_value = "default")]
        agent_id: String,
        #[arg(long, default_value = "20", help = "Max episodes to sample")]
        limit: usize,
        #[arg(
            long,
            help = "Enable LLM judge scoring (needs an LLM provider; off by default)"
        )]
        judge: bool,
        #[arg(long, help = "Emit the report as JSON")]
        json: bool,
    },
    /// Trend-over-time view of past `bench`/`doctor` runs — each run
    /// persists itself automatically, this just lists what accumulated.
    EvalHistory {
        #[arg(long, help = "'bench' or 'doctor'")]
        kind: String,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long, help = "Emit each run's full summary as JSON")]
        json: bool,
    },
    /// Memory hygiene inspection: dangling supersede/lesson pointers, stale
    /// unarchived memories, live contradictions, near-duplicates, review
    /// backlog, and skills flagged for revision. Read-only and deterministic
    /// (no network/LLM). `--json` emits the machine-readable report.
    Doctor {
        #[arg(long, help = "Emit the report as JSON")]
        json: bool,
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

/// Read a hook payload or transcript from a path, or stdin when the path is `-`.
fn read_input_source(path: &str) -> Result<String> {
    if path == "-" {
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

/// Whether hook-adjacent diagnostics may be emitted (MEMVAULT_HOOK_DEBUG=1).
fn debug_hook_log() -> bool {
    std::env::var("MEMVAULT_HOOK_DEBUG").is_ok_and(|v| v == "1")
}

/// Resolve the text for `extract`: explicit `--text`, else a transcript file
/// given directly or via the hook payload's `transcript_path`.
///
/// Returns `Ok(None)` when the caller is a hook and there is nothing to do —
/// a hook exits cleanly without surfacing an error to the host. Manual
/// callers get a hard error instead.
fn resolve_extract_text(
    text: Option<String>,
    transcript_path: Option<String>,
    via_hook: bool,
    source: &str,
) -> Result<Option<String>> {
    if let Some(text) = text {
        return Ok(Some(text));
    }
    let Some(path) = transcript_path else {
        if via_hook {
            eprintln!("memvault: hook payload has no transcript_path; nothing to extract");
            return Ok(None);
        }
        eprintln!(
            "error: nothing to extract — pass --text <string> or --transcript <path|-> \
             (hook callers use --hook-input)"
        );
        std::process::exit(2);
    };
    let raw = match read_input_source(&path) {
        Ok(raw) => raw,
        Err(err) => {
            if via_hook {
                eprintln!("memvault: transcript unreadable ({err}); skipping extract");
                return Ok(None);
            }
            return Err(err);
        }
    };
    // `auto` sniffs the Claude Code JSONL shape and falls back to raw text on
    // anything else; `text` forces the pass-through.
    Ok(Some(if source == "text" {
        raw
    } else {
        memvault_core::transcript::transcript_to_text(&raw)
    }))
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
            visibility,
            force,
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
            if let Some(v) = visibility {
                mem.visibility = memvault_core::models::Visibility::parse(&v);
            }
            // 保存时优先生成向量(与 MCP/proxy 一致):embedder 可用则写入 int8,
            // 否则降级无向量保存并告警;MEMVAULT_EMBEDDING_PROVIDER=off 可整体关闭。
            let embed_text = mem
                .instruction
                .clone()
                .unwrap_or_else(|| mem.content.clone())
                .to_string();
            let embedder = memvault_core::embedding::build_embedder_from_env().await;
            let (embedding, emb_note) = match &embedder {
                Some(e) => match e.embed(&[embed_text]).await {
                    Ok(embeddings) if !embeddings.is_empty() => {
                        (embeddings.into_iter().next(), " (embedded int8)")
                    }
                    Err(e2) => {
                        eprintln!("warning: embedding failed ({}), saving without vector", e2);
                        (None, "")
                    }
                    _ => (None, ""),
                },
                None => (None, ""),
            };

            // Delta 写入(docs/PAPER-INSPIRATIONS.md Feature A):同命名空间内先查重,
            // 近重复跳过、相似项合并残差、--force 直插。技能(SOP)是过程性知识,
            // 与事实/偏好合并语义上不成立,一律按 force 插入。
            let force = force || mem.skill_meta.is_some();
            let writer = memvault_core::writer::MemoryWriter::new(store.clone(), embedder);
            match writer.save(mem, embedding, force).await? {
                memvault_core::writer::WriteOutcome::Inserted(m) => {
                    println!("Saved: {}{}", m.id, emb_note);
                }
                memvault_core::writer::WriteOutcome::Merged {
                    memory,
                    similarity,
                    residual_added,
                } => {
                    let note = if residual_added {
                        "residual appended"
                    } else {
                        "content unchanged, strength refreshed"
                    };
                    println!(
                        "Merged into existing memory {} (similarity {:.2}, {})",
                        memory.id, similarity, note
                    );
                }
                memvault_core::writer::WriteOutcome::Skipped { memory, similarity } => {
                    println!(
                        "Skipped: near-duplicate of existing memory {} (similarity {:.2})",
                        memory.id, similarity
                    );
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
                input.clone().into(),
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

            // Effectiveness: pair this outcome against recent pending
            // injections for the same agent. Best-effort, same as reflection.
            if let Ok(compliance) =
                memvault_core::compliance::ComplianceStore::new(&db_path.to_string_lossy())
            {
                match memvault_core::effectiveness::judge_recent_injections(
                    &compliance,
                    store.as_ref(),
                    llm.as_deref(),
                    &input,
                )
                .await
                {
                    Ok(n) if n > 0 => println!("Effectiveness judged: {n} injection(s)"),
                    Ok(_) => {}
                    Err(e) => eprintln!(
                        "warning: effectiveness judging failed ({}); outcome kept",
                        e
                    ),
                }
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
            hook_input,
            format,
        } => {
            if !matches!(format.as_str(), "plain" | "hook-json") {
                anyhow::bail!(
                    "invalid --format '{}': expected 'plain' or 'hook-json'",
                    format
                );
            }
            // The host hook payload is the authoritative input; explicit flags
            // only fill fields the host did not provide (cwd → project scope).
            let hook = match hook_input.as_deref() {
                Some(path) => Some(memvault_core::hook_envelope::HookInput::parse(
                    &read_input_source(path)?,
                )),
                None => None,
            };
            let project = hook.as_ref().and_then(|h| h.cwd.clone()).or(project);
            let context = hook.as_ref().and_then(|h| h.prompt.clone()).or(context);
            // Feature D: a multi-line --context is a turn sequence — weight it
            // by recency so retrieval is conditioned on the recent context, not
            // just a flat string. Single-line input passes through unchanged.
            let context_key = context
                .as_deref()
                .map(memvault_core::query_expand::weight_turns_by_recency);
            let injection = router
                .session_start(&agent_id, context_key.as_deref(), project.as_deref())
                .await?;
            let formatted = router.format_as_instructions(&injection.results);
            match format.as_str() {
                // Claude Code SessionStart: stdout must be exactly one valid
                // JSON envelope; skip reasons go to stderr (debug-only) so the
                // injected context stream stays clean.
                "hook-json" => {
                    println!(
                        "{}",
                        memvault_core::hook_envelope::session_start_hook_json(&formatted)
                    );
                    if debug_hook_log() {
                        for s in &injection.skipped {
                            eprintln!("memvault: not injected {} — {}", s.id, s.reason);
                        }
                    }
                }
                _ => {
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
            transcript,
            hook_input,
            approve,
            source,
        } => {
            let hook = match hook_input.as_deref() {
                Some(path) => Some(memvault_core::hook_envelope::HookInput::parse(
                    &read_input_source(path)?,
                )),
                None => None,
            };
            let via_hook = hook.is_some();
            // Explicit --transcript wins over the payload's transcript_path.
            let transcript_path =
                transcript.or_else(|| hook.as_ref().and_then(|h| h.transcript_path.clone()));
            let Some(text) = resolve_extract_text(text, transcript_path, via_hook, &source)? else {
                return Ok(());
            };
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
                                session_id: hook.as_ref().and_then(|h| h.session_id.clone()),
                            },
                        );
                        mem.instruction = e.instruction;
                        mem.tags = e.tags;
                        mem.confidence = e.confidence;
                        mem.human_reviewed = approve;
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

        Commands::ImportSkills {
            file,
            dir,
            namespace,
            approve,
        } => {
            // Collect the SOP files to import.
            let mut files: Vec<PathBuf> = Vec::new();
            if let Some(f) = file {
                files.push(resolve_path(&f.to_string_lossy()));
            }
            if let Some(d) = dir {
                let dir_path = resolve_path(&d.to_string_lossy());
                if !dir_path.is_dir() {
                    anyhow::bail!("not a directory: {}", dir_path.display());
                }
                let mut md_files: Vec<PathBuf> = std::fs::read_dir(&dir_path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "md"))
                    .collect();
                md_files.sort();
                files.extend(md_files);
            }
            if files.is_empty() {
                anyhow::bail!("nothing to import — pass --file or --dir");
            }

            let mut imported = 0usize;
            let mut skipped = 0usize;
            for path in &files {
                let content = std::fs::read_to_string(path)?;
                let fallback = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "imported-sop".to_string());
                let parsed = memvault_core::sop::parse_sops(&content, &fallback);
                skipped += parsed.skipped_no_steps;
                for skill in &parsed.skills {
                    let mut mem = Memory::new(
                        MemoryType::Skill,
                        skill.title.clone(),
                        Priority::Reference,
                        SourceAgent {
                            id: "sop-import".to_string(),
                            agent_type: "importer".to_string(),
                            session_id: None,
                        },
                    );
                    mem.namespace = namespace.clone();
                    mem.tags = vec!["imported-sop".to_string()];
                    mem.human_reviewed = approve;
                    mem.skill_meta = Some(SkillMeta {
                        trigger: skill.trigger.clone(),
                        steps: skill.steps.clone(),
                        verification: skill.verification.clone(),
                        version: 1,
                    });
                    let saved = store.save(mem).await?;
                    println!("  imported: {} ({})", skill.title, saved.id);
                    imported += 1;
                }
            }
            println!(
                "Imported {} skill(s) from {} file(s){}; {} section(s) skipped (no steps).",
                imported,
                files.len(),
                if approve { ", marked reviewed" } else { "" },
                skipped
            );
        }

        Commands::ImportAgent {
            agent,
            scan,
            path,
            namespace,
            dry_run,
            approve,
            paste,
        } => {
            if let Some(paste_arg) = paste {
                if scan {
                    anyhow::bail!(
                        "--scan has no effect with --paste (nothing to detect for pasted text)"
                    );
                }
                if path.is_some() {
                    anyhow::bail!("--path is not compatible with --paste");
                }

                let text = if paste_arg == "-" {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                } else {
                    paste_arg
                };
                if text.trim().is_empty() {
                    anyhow::bail!("--paste text is empty");
                }

                let label = agent.unwrap_or_else(|| "manual".to_string());
                let extraction = Extractor::extract_with_coverage(&text);
                println!(
                    "manual paste ({label}): {} line(s), {} extracted, {} no-signal",
                    extraction.coverage.input_lines,
                    extraction.coverage.extracted_lines,
                    extraction.coverage.no_signal_lines
                );

                let embedder = memvault_core::embedding::build_embedder_from_env().await;
                let dedup = Deduplicator::new(store.clone(), embedder);

                let mut saved_count = 0usize;
                let mut duplicate_count = 0usize;
                for extracted in extraction.memories {
                    let mut mem = Memory::new(
                        extracted.memory_type,
                        extracted.content,
                        // Trust boundary: every import-agent path (files or
                        // pasted text) lands at REFERENCE, never MUST —
                        // consistent regardless of what the extractor itself
                        // would have assigned.
                        Priority::Reference,
                        SourceAgent {
                            id: format!("import-manual-{label}"),
                            agent_type: format!("imported-manual:{label}"),
                            session_id: None,
                        },
                    );
                    mem.instruction = extracted.instruction;
                    mem.tags = extracted.tags;
                    mem.confidence = extracted.confidence;
                    mem.ai_generated = false;
                    if let Some(ns) = &namespace {
                        mem.namespace = ns.clone();
                    }

                    if let Some(dup) = dedup
                        .check_duplicate(&mem.content, Some(&mem.namespace))
                        .await?
                    {
                        duplicate_count += 1;
                        println!(
                            "  duplicate of {}: \"{}\" — skipped",
                            dup.existing_id,
                            truncate(&mem.content, 50)
                        );
                        continue;
                    }

                    mem.human_reviewed = approve;

                    if dry_run {
                        println!("  would import: \"{}\"", truncate(&mem.content, 60));
                        saved_count += 1;
                        continue;
                    }

                    let saved = store.save(mem).await?;
                    println!(
                        "  imported: {} ({})",
                        truncate(&saved.content, 50),
                        saved.id
                    );
                    saved_count += 1;
                }

                println!(
                    "manual paste ({label}): {} imported{}{}, {} duplicate(s) skipped",
                    saved_count,
                    if dry_run { " (dry-run)" } else { "" },
                    if approve { ", marked reviewed" } else { "" },
                    duplicate_count
                );

                return Ok(());
            }

            if path.is_some()
                && agent
                    .as_deref()
                    .is_none_or(|a| a.eq_ignore_ascii_case("all"))
            {
                anyhow::bail!("--path requires exactly one --agent");
            }

            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            let adapters = memvault_core::agent_import::all_adapters();
            let selected: Vec<_> = adapters
                .into_iter()
                .filter(|a| match agent.as_deref() {
                    None => true,
                    Some(k) if k.eq_ignore_ascii_case("all") => true,
                    Some(k) => k.eq_ignore_ascii_case(a.agent_key()),
                })
                .collect();

            if selected.is_empty() {
                anyhow::bail!(
                    "unknown agent: {}",
                    agent.unwrap_or_else(|| "<none>".to_string())
                );
            }

            let embedder = memvault_core::embedding::build_embedder_from_env().await;
            let dedup = Deduplicator::new(store.clone(), embedder);

            for adapter in &selected {
                let Some(detected) = adapter.detect(&home, &cwd, path.as_deref()) else {
                    println!(
                        "{}: not detected (use --path to point at a memory file/dir manually)",
                        adapter.display_name()
                    );
                    continue;
                };

                let outcome = adapter.parse(&detected);
                println!(
                    "{}: {} file(s) scanned, {} candidate(s) parsed",
                    adapter.display_name(),
                    outcome.files_scanned,
                    outcome.candidates.len()
                );
                for (skipped_path, reason) in &outcome.files_skipped {
                    println!("  skipped {}: {}", skipped_path.display(), reason);
                }

                if scan {
                    continue;
                }

                let mut saved_count = 0usize;
                let mut duplicate_count = 0usize;
                for candidate in outcome.candidates {
                    let mut mem = candidate.memory;
                    if let Some(ns) = &namespace {
                        mem.namespace = ns.clone();
                    }

                    if let Some(dup) = dedup
                        .check_duplicate(&mem.content, Some(&mem.namespace))
                        .await?
                    {
                        duplicate_count += 1;
                        println!(
                            "  duplicate of {}: \"{}\" — skipped",
                            dup.existing_id,
                            truncate(&mem.content, 50)
                        );
                        continue;
                    }

                    mem.human_reviewed = approve;

                    if dry_run {
                        println!("  would import: \"{}\"", truncate(&mem.content, 60));
                        saved_count += 1;
                        continue;
                    }

                    let saved = store.save(mem).await?;
                    println!(
                        "  imported: {} ({})",
                        truncate(&saved.content, 50),
                        saved.id
                    );
                    saved_count += 1;
                }

                println!(
                    "{}: {} imported{}{}, {} duplicate(s) skipped",
                    adapter.display_name(),
                    saved_count,
                    if dry_run { " (dry-run)" } else { "" },
                    if approve { ", marked reviewed" } else { "" },
                    duplicate_count
                );
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

        Commands::Supersede { old, new } => {
            store.supersede(&old, &new).await?;
            println!("Superseded: {} -> {}", old, new);
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

        Commands::Bench {
            agent_id,
            limit,
            judge,
            json,
        } => {
            let samples = memvault_core::bench::collect_samples(store.as_ref(), limit).await?;
            if samples.is_empty() {
                println!(
                    "No episodes with lessons found. Record task outcomes first \
                     (memvault outcome --task ... --status failure --cause ...)."
                );
                return Ok(());
            }
            let judge_provider = if judge {
                memvault_core::llm_extractor::build_llm_extractor_from_env().await
            } else {
                None
            };
            if judge && judge_provider.is_none() {
                eprintln!(
                    "warning: --judge requested but no LLM provider is available; \
                     running retrieval/injection layers only"
                );
            }
            let config = memvault_core::bench::BenchConfig {
                agent_id,
                ..Default::default()
            };
            let report = memvault_core::bench::run_bench(
                &router,
                store.as_ref(),
                &samples,
                &config,
                judge_provider.as_ref(),
            )
            .await?;

            let bench_headline = if report.judged > 0 {
                format!(
                    "{} judged: pass_without {}/{} -> pass_with {}/{}",
                    report.judged,
                    report.pass_without,
                    report.judged,
                    report.pass_with,
                    report.judged
                )
            } else {
                format!(
                    "{} samples, {} retrieved, {} injected",
                    report.total, report.retrieved, report.injected
                )
            };
            // `warn_count`'s generic meaning here: judged samples that still
            // failed to avoid the known pitfall even with the memory injected
            // — the bench-specific analogue of doctor's "warn" findings.
            let bench_concerning = report.judged.saturating_sub(report.pass_with);
            if let Ok(history) =
                memvault_core::eval_history::EvalHistoryStore::new(&db_path.to_string_lossy())
            {
                let _ = history
                    .record(
                        memvault_core::eval_history::EvalKind::Bench,
                        bench_concerning,
                        &bench_headline,
                        &serde_json::to_string(&report)?,
                    )
                    .await;
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Task-level memory benchmark — {} samples ({} with live lesson memory)",
                    report.total, report.with_lesson_memory
                );
                if report.with_lesson_memory > 0 {
                    println!(
                        "  Retrieval: {}/{} lessons found in top-k search",
                        report.retrieved, report.with_lesson_memory
                    );
                    println!(
                        "  Injection: {}/{} lessons actually injected at session start",
                        report.injected, report.with_lesson_memory
                    );
                }
                println!(
                    "  Avg injected size: {:.0} chars (~{:.0} tokens)",
                    report.avg_injected_chars,
                    report.avg_injected_chars / 4.0
                );
                if report.judged > 0 {
                    println!(
                        "  Judge ({} samples): pass without memory {}/{} -> with memory {}/{}",
                        report.judged,
                        report.pass_without,
                        report.judged,
                        report.pass_with,
                        report.judged
                    );
                } else if judge {
                    println!("  Judge: LLM calls produced no verdicts");
                } else {
                    println!(
                        "  Judge: skipped (rerun with --judge to measure task-level success delta)"
                    );
                }
                println!();
                for row in &report.rows {
                    let flags = format!(
                        "{}{}",
                        if row.retrieved { "R" } else { "-" },
                        if row.injected { "I" } else { "-" }
                    );
                    let verdict = match (row.pass_without, row.pass_with) {
                        (Some(a), Some(b)) => format!(" judge:{a}->{b}"),
                        _ => String::new(),
                    };
                    println!(
                        "  [{}] {} — lesson: {}{}",
                        flags,
                        truncate(&row.task, 60),
                        row.lesson
                            .as_deref()
                            .map(|l| truncate(l, 60))
                            .unwrap_or_else(|| "(none)".to_string()),
                        verdict
                    );
                }
                println!();
                println!("Legend: R=retrieved in top-k, I=injected at session start");
            }
        }

        Commands::Doctor { json } => {
            let doctor = memvault_core::doctor::Doctor::new(store);
            let report = doctor.run().await?;

            let warn_items: usize = report
                .findings
                .iter()
                .filter(|f| f.severity == memvault_core::doctor::Severity::Warn)
                .map(|f| f.count)
                .sum();
            let info_items: usize = report
                .findings
                .iter()
                .filter(|f| f.severity == memvault_core::doctor::Severity::Info)
                .map(|f| f.count)
                .sum();
            let doctor_headline = format!(
                "{} warn / {} info across {} memories",
                warn_items, info_items, report.total_memories
            );
            if let Ok(history) =
                memvault_core::eval_history::EvalHistoryStore::new(&db_path.to_string_lossy())
            {
                let _ = history
                    .record(
                        memvault_core::eval_history::EvalKind::Doctor,
                        report.warn_count(),
                        &doctor_headline,
                        &serde_json::to_string(&report)?,
                    )
                    .await;
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "MemVault Doctor — scanned {} memories",
                    report.total_memories
                );
                if report.findings.is_empty() {
                    println!("No findings. Vault is healthy ✓");
                } else {
                    for finding in &report.findings {
                        let level = match finding.severity {
                            memvault_core::doctor::Severity::Warn => "WARN",
                            memvault_core::doctor::Severity::Info => "INFO",
                        };
                        println!("\n[{level}] {} ({})", finding.check, finding.count);
                        for item in &finding.items {
                            println!("  - {}: {}", item.id, item.detail);
                        }
                        if finding.count > finding.items.len() {
                            println!("  … and {} more", finding.count - finding.items.len());
                        }
                    }
                    let warns = report.warn_count();
                    if warns > 0 {
                        println!(
                            "\nResult: {warns} check(s) reported problems — see WARN findings above"
                        );
                    } else {
                        println!("\nResult: no structural problems (INFO findings are advisory)");
                    }
                }
            }
        }

        Commands::EvalHistory { kind, limit, json } => {
            let kind: memvault_core::eval_history::EvalKind = kind.parse()?;
            let history =
                memvault_core::eval_history::EvalHistoryStore::new(&db_path.to_string_lossy())?;
            let runs = history.recent(kind, limit).await?;

            if runs.is_empty() {
                println!("No {kind} runs recorded yet — run `memvault {kind}` at least once.");
                return Ok(());
            }

            for run in &runs {
                if json {
                    println!("{}", run.summary_json);
                } else {
                    println!(
                        "{}  warn_count={}  {}",
                        run.run_at.to_rfc3339(),
                        run.warn_count,
                        run.headline
                    );
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
            visibility: None,
            force: false,
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
                visibility: None,
                force: false,
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
            // The review-flow test needs three distinct rows. After tokenize
            // (single digits dropped) these contents are identical, so
            // delta-write would (correctly) treat them as duplicates — force
            // them in.
            run(cli(
                db.clone(),
                Commands::Save {
                    content: format!("pending memory {i}"),
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
                    visibility: None,
                    force: true,
                },
            ))
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
                visibility: None,
                force: false,
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
                hook_input: None,
                format: "plain".to_string(),
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
                text: Some("The weather is fine.".to_string()),
                save: false,
                agent_id: "cli".to_string(),
                transcript: None,
                hook_input: None,
                approve: false,
                source: "auto".to_string(),
            },
        ))
        .await
        .unwrap();
        assert!(list_all(&db).await.is_empty());

        run(cli(
            db.clone(),
            Commands::Extract {
                text: Some("I always prefer dark mode".to_string()),
                save: true,
                agent_id: "cli".to_string(),
                transcript: None,
                hook_input: None,
                approve: true,
                source: "auto".to_string(),
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
                visibility: None,
                force: false,
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
                visibility: None,
                force: false,
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
    async fn test_doctor_runs_and_flags_stale_memory() {
        // Check logic is covered by memvault_core::doctor unit tests; this
        // exercises the CLI wiring (both output modes) end to end.
        let db = temp_db();
        run(cli(db.clone(), save_cmd("a normal fact")))
            .await
            .unwrap();

        // Force one memory below the archive threshold without running a
        // decay cycle -> the stale_unarchived info finding must surface.
        let store = SqliteStore::new(std::path::Path::new(&db)).unwrap();
        let mem = store.list(None, 1, 0).await.unwrap().pop().unwrap();
        let mut stale = store.get(&mem.id).await.unwrap();
        stale.decay_score = 0.05;
        store.update(stale).await.unwrap();
        drop(store);

        run(cli(db.clone(), Commands::Doctor { json: false }))
            .await
            .unwrap();
        run(cli(db, Commands::Doctor { json: true })).await.unwrap();
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
                visibility: None,
                force: false,
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
                hook_input: None,
                format: "plain".to_string(),
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_extract_from_transcript_respects_inbox_and_approve() {
        let db = temp_db();
        let path = std::env::temp_dir().join(format!(
            "memvault-transcript-test-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","message":{"content":"I always prefer dark mode"}}"#,
                "\n",
                r#"{"type":"assistant","isSidechain":true,"message":{"content":"noise"}}"#,
                "\n",
            ),
        )
        .unwrap();

        let cli_path = path.to_string_lossy().into_owned();
        run(cli(
            db.clone(),
            Commands::Extract {
                text: None,
                save: true,
                agent_id: "claude-code".to_string(),
                transcript: Some(cli_path),
                hook_input: None,
                approve: false,
                source: "auto".to_string(),
            },
        ))
        .await
        .unwrap();
        let all = list_all(&db).await;
        assert_eq!(all.len(), 1, "transcript preference should be extracted");
        assert!(!all[0].human_reviewed, "hook drafts default to the inbox");
        assert_eq!(all[0].source_agent.id, "claude-code");

        run(cli(
            db.clone(),
            Commands::Extract {
                text: Some("I always write tests".to_string()),
                save: true,
                agent_id: "cli".to_string(),
                transcript: None,
                hook_input: None,
                approve: true,
                source: "auto".to_string(),
            },
        ))
        .await
        .unwrap();
        let all = list_all(&db).await;
        assert_eq!(all.len(), 2);
        let approved = all
            .iter()
            .find(|m| m.content.contains("tests"))
            .expect("approved memory present");
        assert!(approved.human_reviewed, "--approve skips the inbox");
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
                visibility: None,
                force: false,
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

    #[tokio::test]
    async fn test_import_skills_from_file_and_dir() {
        let db = temp_db();
        let dir =
            std::env::temp_dir().join(format!("memvault_sop_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();

        let sop1 = "# Deploy Runbook\ntrigger: deploy\n1. build\n2. push\n";
        let sop2 = "## Backup DB\n- dump\n- verify\n";
        std::fs::write(dir.join("deploy.md"), sop1).unwrap();
        std::fs::write(dir.join("backup.md"), sop2).unwrap();

        run(cli(
            db.clone(),
            Commands::ImportSkills {
                file: None,
                dir: Some(dir.clone()),
                namespace: "global".to_string(),
                approve: false,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        let skills: Vec<_> = memories
            .iter()
            .filter(|m| m.memory_type == MemoryType::Skill)
            .collect();
        assert_eq!(skills.len(), 2);
        let titles: Vec<&str> = skills.iter().map(|m| m.content.as_str()).collect();
        assert!(titles.contains(&"Deploy Runbook"));
        assert!(titles.contains(&"Backup DB"));

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_dry_run_does_not_write() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_dry_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: false,
                path: Some(dir.clone()),
                namespace: None,
                dry_run: true,
                approve: false,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert!(memories.is_empty(), "dry-run must not write to the store");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_scan_does_not_write() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_scan_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: true,
                path: Some(dir.clone()),
                namespace: None,
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert!(memories.is_empty(), "--scan must not write to the store");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_saves_to_inbox_unless_approved() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_inbox_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: false,
                path: Some(dir.clone()),
                namespace: None,
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert!(
            !memories[0].human_reviewed,
            "default import must land in the review inbox"
        );
        assert_eq!(memories[0].priority, Priority::Reference);
        assert!(memories[0].tags.contains(&"imported-agents-md".to_string()));

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_approve_skips_inbox() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_approve_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: false,
                path: Some(dir.clone()),
                namespace: None,
                dry_run: false,
                approve: true,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert!(
            memories[0].human_reviewed,
            "--approve must skip the review inbox"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_namespace_override() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_ns_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Style\nUse 4-space indent\n").unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: false,
                path: Some(dir.clone()),
                namespace: Some("project:custom".to_string()),
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].namespace, "project:custom");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_unknown_agent_errors() {
        let db = temp_db();
        let result = run(cli(
            db,
            Commands::ImportAgent {
                agent: Some("no-such-agent".to_string()),
                scan: true,
                path: None,
                namespace: None,
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_import_agent_path_requires_single_agent() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_pathall_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let result = run(cli(
            db,
            Commands::ImportAgent {
                agent: Some("all".to_string()),
                scan: true,
                path: Some(dir.clone()),
                namespace: None,
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await;
        assert!(result.is_err(), "--path with --agent all must be rejected");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_deduplicates_against_existing_memory() {
        let db = temp_db();
        let dir = std::env::temp_dir().join(format!(
            "memvault_import_agent_dedup_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("AGENTS.md"),
            "# Style\nAlways use four space indentation everywhere in this codebase\n",
        )
        .unwrap();

        // Pre-seed a near-identical memory in the same namespace the import will use.
        run(cli(
            db.clone(),
            Commands::Save {
                content: "Always use four space indentation everywhere in this codebase"
                    .to_string(),
                priority: "REFERENCE".to_string(),
                r#type: "fact".to_string(),
                namespace: "project:custom".to_string(),
                agent_id: "seed".to_string(),
                instruction: None,
                tags: None,
                layer: None,
                skill_trigger: None,
                skill_steps: None,
                skill_verification: None,
                visibility: None,
                force: false,
            },
        ))
        .await
        .unwrap();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("codex".to_string()),
                scan: false,
                path: Some(dir.clone()),
                namespace: Some("project:custom".to_string()),
                dry_run: false,
                approve: false,
                paste: None,
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1, "near-duplicate import must be skipped");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_import_agent_paste_extracts_and_saves_with_default_label() {
        let db = temp_db();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: None,
                namespace: None,
                dry_run: false,
                approve: false,
                paste: Some("I prefer dark mode for all editors".to_string()),
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].priority, Priority::Reference);
        assert!(!memories[0].human_reviewed);
        assert_eq!(
            memories[0].source_agent.agent_type,
            "imported-manual:manual"
        );
    }

    #[tokio::test]
    async fn test_import_agent_paste_uses_agent_as_free_form_label() {
        let db = temp_db();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: Some("gemini-cli".to_string()),
                scan: false,
                path: None,
                namespace: None,
                dry_run: false,
                approve: false,
                paste: Some("我们的项目使用 Kubernetes 部署".to_string()),
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].source_agent.agent_type,
            "imported-manual:gemini-cli"
        );
    }

    #[tokio::test]
    async fn test_import_agent_paste_dry_run_does_not_write() {
        let db = temp_db();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: None,
                namespace: None,
                dry_run: true,
                approve: false,
                paste: Some("I always want concise commit messages".to_string()),
            },
        ))
        .await
        .unwrap();

        assert!(list_all(&db).await.is_empty());
    }

    #[tokio::test]
    async fn test_import_agent_paste_approve_skips_inbox() {
        let db = temp_db();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: None,
                namespace: None,
                dry_run: false,
                approve: true,
                paste: Some("I never want emojis in commit messages".to_string()),
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert!(memories[0].human_reviewed);
    }

    #[tokio::test]
    async fn test_import_agent_paste_rejects_scan() {
        let db = temp_db();
        let result = run(cli(
            db,
            Commands::ImportAgent {
                agent: None,
                scan: true,
                path: None,
                namespace: None,
                dry_run: false,
                approve: false,
                paste: Some("some text".to_string()),
            },
        ))
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_import_agent_paste_rejects_path() {
        let db = temp_db();
        let result = run(cli(
            db,
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: Some(PathBuf::from("/tmp")),
                namespace: None,
                dry_run: false,
                approve: false,
                paste: Some("some text".to_string()),
            },
        ))
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_import_agent_paste_rejects_empty_text() {
        let db = temp_db();
        let result = run(cli(
            db,
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: None,
                namespace: None,
                dry_run: false,
                approve: false,
                paste: Some("   \n  ".to_string()),
            },
        ))
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_import_agent_paste_namespace_override() {
        let db = temp_db();

        run(cli(
            db.clone(),
            Commands::ImportAgent {
                agent: None,
                scan: false,
                path: None,
                namespace: Some("project:manual".to_string()),
                dry_run: false,
                approve: false,
                paste: Some("I prefer tabs over spaces".to_string()),
            },
        ))
        .await
        .unwrap();

        let memories = list_all(&db).await;
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].namespace, "project:manual");
    }

    #[tokio::test]
    async fn test_doctor_runs_persist_to_eval_history() {
        let db = temp_db();
        run(cli(db.clone(), save_cmd("a normal fact")))
            .await
            .unwrap();

        run(cli(db.clone(), Commands::Doctor { json: false }))
            .await
            .unwrap();
        run(cli(db.clone(), Commands::Doctor { json: false }))
            .await
            .unwrap();

        let history = memvault_core::eval_history::EvalHistoryStore::new(&db).unwrap();
        let runs = history
            .recent(memvault_core::eval_history::EvalKind::Doctor, 10)
            .await
            .unwrap();
        assert_eq!(runs.len(), 2, "each doctor run must persist its own row");

        // `eval-history` itself must run end to end without error.
        run(cli(
            db.clone(),
            Commands::EvalHistory {
                kind: "doctor".to_string(),
                limit: 10,
                json: false,
            },
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_eval_history_rejects_unknown_kind() {
        let db = temp_db();
        let result = run(cli(
            db,
            Commands::EvalHistory {
                kind: "bogus".to_string(),
                limit: 10,
                json: false,
            },
        ))
        .await;
        assert!(result.is_err());
    }
}
