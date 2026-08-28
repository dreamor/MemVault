//! Memory hygiene inspection ("doctor"): deterministic, read-only health
//! checks over the store.
//!
//! Inspired by claude-obsidian's lint engine (original analysis doc
//! archived; provenance: docs/DESIGN.md §16): no network, no LLM, no
//! writes, and a
//! stable JSON-serializable report that can be compared across runs and
//! machines. The CLI surfaces it as `memvault doctor`; the report shape is
//! also what a future Dashboard health page would consume.
//!
//! Checks:
//! - `dangling_superseded_by` (warn): `superseded_by` points at a memory
//!   that no longer exists
//! - `dangling_lesson_memory` (warn): an episode's `lesson_memory_id`
//!   points at a deleted lesson memory (the orphan failure mode that
//!   `episode.rs`/`reflection.rs` roll back on write)
//! - `stale_unarchived` (info): decay score already below the archive
//!   threshold but the namespace was never archived (decay cycle not run)
//! - `active_contradictions` (info): memories with live counter-evidence —
//!   candidates for review or supersede
//! - `duplicate_pairs` (info): near-duplicate content (keyword-only, so the
//!   check stays deterministic without an embedding provider)
//! - `pending_review` (info): AI-extracted memories awaiting human approval
//! - `needs_revision_skills` (info): skills flagged by failed executions

use std::sync::Arc;

use serde::Serialize;

use crate::decay::DecayConfig;
use crate::dedup::Deduplicator;
use crate::error::Result;
use crate::evidence::has_active_contradiction;
use crate::models::{EpisodeFilter, NEEDS_REVISION_TAG};
use crate::storage::MemoryStore;

/// Hard cap on per-finding item lists so the report stays bounded on huge
/// stores (same boundedness stance as the injection formatter).
const MAX_ITEMS_PER_FINDING: usize = 20;
/// Doctor scans at most this many memories (consistent with decay).
const SCAN_LIMIT: usize = 100000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingItem {
    pub id: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    pub count: usize,
    pub items: Vec<FindingItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub total_memories: usize,
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    /// Number of warn-level findings — `0` means the vault is "healthy".
    pub fn warn_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warn && f.count > 0)
            .count()
    }

    pub fn is_healthy(&self) -> bool {
        self.warn_count() == 0
    }
}

pub struct Doctor {
    store: Arc<dyn MemoryStore>,
}

impl Doctor {
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    pub async fn run(&self) -> Result<DoctorReport> {
        // list() excludes superseded memories by design; doctor needs the
        // full population to audit supersede pointers, so pull both groups.
        let mut memories = self.store.list(None, SCAN_LIMIT, 0).await?;
        let mut offset = memories.len();
        loop {
            let page = self.store.list(None, SCAN_LIMIT, offset).await?;
            if page.is_empty() {
                break;
            }
            offset += page.len();
            memories.extend(page);
            if offset >= SCAN_LIMIT {
                break;
            }
        }
        let total_memories = memories.len();

        let mut findings = Vec::new();
        findings.push(self.check_dangling_superseded_by(&memories).await);
        findings.push(self.check_dangling_lesson_memory().await);
        findings.push(check_stale_unarchived(&memories));
        findings.push(self.check_active_contradictions(&memories).await);
        findings.push(self.check_duplicate_pairs().await);
        findings.push(self.check_pending_review().await);
        findings.push(check_needs_revision_skills(&memories));
        findings.retain(|f| f.count > 0);

        Ok(DoctorReport {
            total_memories,
            findings,
        })
    }

    async fn check_dangling_superseded_by(&self, memories: &[crate::models::Memory]) -> Finding {
        let mut items = Vec::new();
        for (id, replacement) in memories.iter().filter_map(|m| {
            m.superseded_by
                .as_deref()
                .map(|replacement| (m.id.clone(), replacement.to_string()))
        }) {
            if self.store.get(&replacement).await.is_err() {
                items.push(FindingItem {
                    id,
                    detail: format!("superseded_by → missing {replacement}"),
                });
            }
        }
        finding("dangling_superseded_by", Severity::Warn, items)
    }

    async fn check_dangling_lesson_memory(&self) -> Finding {
        let mut items = Vec::new();
        let episodes = self
            .store
            .list_episodes(EpisodeFilter {
                limit: SCAN_LIMIT,
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        for ep in episodes {
            let Some(lesson_id) = ep.lesson_memory_id.as_deref() else {
                continue;
            };
            if self.store.get(lesson_id).await.is_err() {
                items.push(FindingItem {
                    id: ep.memory_id.clone(),
                    detail: format!("lesson_memory_id → missing {lesson_id}"),
                });
            }
        }
        finding("dangling_lesson_memory", Severity::Warn, items)
    }

    async fn check_active_contradictions(&self, memories: &[crate::models::Memory]) -> Finding {
        let mut items = Vec::new();
        for mem in memories {
            if mem.namespace.starts_with("archived:") {
                continue;
            }
            if has_active_contradiction(&*self.store, &mem.id).await {
                items.push(FindingItem {
                    id: mem.id.clone(),
                    detail: format!("has live counter-evidence: {}", short(&mem.content)),
                });
            }
        }
        finding("active_contradictions", Severity::Info, items)
    }

    async fn check_duplicate_pairs(&self) -> Finding {
        // Keyword-only on purpose: deterministic without an embedding
        // provider (lint-engine stance — no external dependencies).
        let dedup = Deduplicator::new(self.store.clone(), None);
        let mut items = Vec::new();
        if let Ok(result) = dedup.scan(None).await {
            for pair in result.duplicates {
                items.push(FindingItem {
                    id: pair.existing_id,
                    detail: format!(
                        "≈ duplicate (similarity {:.0}%): {}",
                        pair.similarity * 100.0,
                        short(&pair.new_content)
                    ),
                });
            }
        }
        finding("duplicate_pairs", Severity::Info, items)
    }

    async fn check_pending_review(&self) -> Finding {
        let pending = self
            .store
            .list_pending(None, SCAN_LIMIT, 0)
            .await
            .unwrap_or_default();
        let items = pending
            .iter()
            .map(|m| FindingItem {
                id: m.id.clone(),
                detail: format!("awaiting review: {}", short(&m.content)),
            })
            .collect();
        finding("pending_review", Severity::Info, items)
    }
}

fn check_stale_unarchived(memories: &[crate::models::Memory]) -> Finding {
    let threshold = DecayConfig::default().archive_threshold;
    let items = memories
        .iter()
        .filter(|m| !m.namespace.starts_with("archived:") && m.decay_score < threshold)
        .map(|m| FindingItem {
            id: m.id.clone(),
            detail: format!("decay_score {:.2} < {threshold}", m.decay_score),
        })
        .collect();
    finding("stale_unarchived", Severity::Info, items)
}

fn check_needs_revision_skills(memories: &[crate::models::Memory]) -> Finding {
    let items = memories
        .iter()
        .filter(|m| m.tags.iter().any(|t| t == NEEDS_REVISION_TAG))
        .map(|m| FindingItem {
            id: m.id.clone(),
            detail: format!("skill flagged after failure: {}", short(&m.content)),
        })
        .collect();
    finding("needs_revision_skills", Severity::Info, items)
}

fn finding(check: &'static str, severity: Severity, mut items: Vec<FindingItem>) -> Finding {
    let count = items.len();
    items.truncate(MAX_ITEMS_PER_FINDING);
    Finding {
        check,
        severity,
        count,
        items,
    }
}

fn short(text: &str) -> String {
    let trimmed = text.chars().take(40).collect::<String>();
    if text.chars().count() > 40 {
        format!("{trimmed}...")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceKind, add_evidence};
    use crate::models::{Memory, MemoryType, Priority, SourceAgent};
    use crate::storage::sqlite::SqliteStore;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "doctor".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    async fn fact(store: &SqliteStore, content: &str) -> crate::models::Memory {
        store
            .save(Memory::new(
                MemoryType::Fact,
                content.to_string(),
                Priority::Reference,
                agent(),
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_healthy_vault_reports_no_findings() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // Human-authored + reviewed, fresh decay score → nothing to flag.
        let mut mem = Memory::new(
            MemoryType::Fact,
            "a perfectly fine fact".to_string(),
            Priority::Reference,
            agent(),
        );
        mem.ai_generated = false;
        mem.human_reviewed = true;
        store.save(mem).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            report.findings.is_empty(),
            "expected no findings, got {:?}",
            report.findings
        );
        assert!(report.is_healthy());
        assert_eq!(report.total_memories, 1);
    }

    #[tokio::test]
    async fn test_detects_dangling_superseded_by() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut mem = fact(&store, "old knowledge").await;
        mem.superseded_by = Some("mem_does_not_exist".to_string());
        store.update(mem).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "dangling_superseded_by")
            .expect("dangling superseded_by must be reported");
        assert_eq!(finding.severity, Severity::Warn);
        assert_eq!(finding.count, 1);
        assert!(!report.is_healthy());
    }

    #[tokio::test]
    async fn test_detects_stale_unarchived() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut mem = fact(&store, "forgotten fact").await;
        mem.decay_score = 0.05;
        store.update(mem).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "stale_unarchived" && f.count == 1)
        );
    }

    #[tokio::test]
    async fn test_detects_active_contradictions() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let claim = fact(&store, "the api is at /v1").await;
        let rebuttal = fact(&store, "the api is at /v2").await;
        add_evidence(
            &*store,
            &rebuttal.id,
            EvidenceKind::Contradicts,
            Some(&claim.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "active_contradictions")
            .expect("contradiction must be reported");
        assert_eq!(finding.count, 1);
        assert_eq!(finding.items[0].id, claim.id);
    }

    #[tokio::test]
    async fn test_detects_duplicate_pairs() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        fact(&store, "user likes Python coding").await;
        fact(&store, "user likes Python for coding").await;

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "duplicate_pairs" && f.count >= 1)
        );
    }

    #[tokio::test]
    async fn test_detects_pending_review_and_flagged_skills() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // Default Memory is ai_generated=true, human_reviewed=false → pending.
        fact(&store, "extracted but unreviewed").await;

        let mut skill = Memory::new(
            MemoryType::Skill,
            "flaky deploy skill".to_string(),
            Priority::Reference,
            agent(),
        );
        skill.tags = vec![NEEDS_REVISION_TAG.to_string()];
        skill.human_reviewed = true; // keep it out of pending_review
        store.save(skill).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "pending_review" && f.count == 1)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.check == "needs_revision_skills" && f.count == 1)
        );
    }

    #[tokio::test]
    async fn test_report_serializes_to_json() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        fact(&store, "anything").await;
        let report = Doctor::new(store).run().await.unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("total_memories"));
    }
}
