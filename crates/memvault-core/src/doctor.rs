//! Memory hygiene inspection ("doctor"): deterministic, read-only health
//! checks over the store.
//!
//! Inspired by claude-obsidian's lint engine (original analysis doc
//! archived; provenance: docs/DESIGN.md §15): no network, no LLM, no
//! writes, and a
//! stable JSON-serializable report that can be compared across runs and
//! machines. The CLI surfaces it as `memvault-cli doctor`; the report shape is
//! also what a future Dashboard health page would consume.
//!
//! Checks:
//! - `dangling_superseded_by` (warn): `superseded_by` points at a memory
//!   that no longer exists
//! - `dangling_lesson_memory` (warn): an episode's `lesson_memory_id`
//!   points at a deleted lesson memory (the orphan failure mode that
//!   `episode.rs`/`reflection.rs` roll back on write)
//! - `orphaned_proposal` (warn): a pending edit proposal whose target was
//!   deleted or superseded while it waited in the inbox
//! - `stale_unarchived` (info): decay score already below the archive
//!   threshold but the namespace was never archived (decay cycle not run)
//! - `expired_unarchived` (info): memories past their `expires_at` that
//!   were never archived (decay cycle not run)
//! - `live_looking_archived` (info): human-reviewed MUST/REFERENCE rows on
//!   the archive layer (L0) carrying no archival marker — invisible to
//!   every injection path while looking perfectly live in `list`
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
use crate::models::{EpisodeFilter, MemoryLayer, NEEDS_REVISION_TAG, Priority};
use crate::storage::MemoryStore;
use crate::trace::{TRACE_NAMESPACE_PREFIX, TRACE_SOURCE_TAG};

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
        findings.push(self.check_orphaned_proposal(&memories).await);
        findings.push(self.check_dangling_lesson_memory().await);
        findings.push(check_stale_unarchived(&memories));
        findings.push(Self::check_expired_unarchived(&memories));
        findings.push(Self::check_live_looking_archived(&memories));
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

    /// Proposals whose target no longer exists.
    ///
    /// A proposal is a pending memory carrying `proposes_supersede_of`. If the
    /// target is deleted or forgotten while the proposal waits, approving it
    /// cannot do what the reviewer was shown: `supersede` would fail on a missing
    /// row, or — depending on ordering — silently leave the approved text as a
    /// standalone memory that replaced nothing. The review UI already renders this
    /// (`target_still_live: false`), but a proposal can sit in the inbox for
    /// weeks, and the target can disappear after it was listed. This is the check
    /// that finds them without anyone opening each one.
    async fn check_orphaned_proposal(&self, memories: &[crate::models::Memory]) -> Finding {
        let mut items = Vec::new();
        for (id, target) in memories.iter().filter_map(|m| {
            m.proposes_supersede_of
                .as_deref()
                .map(|target| (m.id.clone(), target.to_string()))
        }) {
            match self.store.get(&target).await {
                Err(_) => items.push(FindingItem {
                    id,
                    detail: format!(
                        "proposes an edit to {target}, which no longer exists — approving it \
                         would supersede nothing; reject it or re-propose against the \
                         replacement"
                    ),
                }),
                // A target that has itself been superseded is not missing, but the
                // proposal was written against text that is no longer current.
                // Info rather than Warn: approving still works, it just may not be
                // what the proposer meant.
                Ok(t) if t.superseded_by.is_some() => items.push(FindingItem {
                    id,
                    detail: format!(
                        "proposes an edit to {target}, which has since been superseded by {:?} \
                         — re-check the proposal against the current text",
                        t.superseded_by
                    ),
                }),
                Ok(_) => {}
            }
        }
        finding("orphaned_proposal", Severity::Warn, items)
    }

    /// Memories past their `expires_at` that were never archived.
    ///
    /// Expiry removes a memory from retrieval and injection immediately, so this
    /// is not a correctness bug — but the archive step is what connects expiry to
    /// the auto-forget sweep, which measures its retention window against
    /// `archived_at` (§9.5). An expired memory that is never archived therefore
    /// lingers in the live table forever, invisible to retrieval and unreachable
    /// by the purge. `decay` archives on expiry; this flags the ones where decay
    /// has not run.
    fn check_expired_unarchived(memories: &[crate::models::Memory]) -> Finding {
        let now = chrono::Utc::now();
        let items = memories
            .iter()
            .filter(|m| m.is_expired_at(now) && !m.namespace.starts_with("archived:"))
            .map(|m| FindingItem {
                id: m.id.clone(),
                detail: format!(
                    "expired {} but not archived — out of retrieval already, and the \
                     auto-forget retention sweep cannot reach it until `memvault-cli decay` \
                     archives it",
                    m.expires_at
                        .map(|e| e.to_rfc3339())
                        .unwrap_or_else(|| "(unparseable)".to_string())
                ),
            })
            .collect();
        finding("expired_unarchived", Severity::Info, items)
    }

    /// Human-reviewed MUST/REFERENCE rows sitting on the archive layer (L0)
    /// with no visible reason.
    ///
    /// Since 0.6.8 a single `is_archived` predicate (`router.rs`) excludes L0
    /// rows from every automatic exposure path — injection body, deterministic
    /// fast path, overflow, and the MCP resources — regardless of priority or
    /// review state. That is deliberate (L0 is archive material), but it is
    /// silent: a reviewed preference that ended up on L0 simply stops being
    /// injected, and `list` still shows it looking perfectly live. Every
    /// *deliberate* archival leaves a marker this check can see — supersede and
    /// dedup set `superseded_by`, decay archives on expiry, trace ingestion
    /// tags its raw evidence (`source:trace`, `trace:*` namespace), and
    /// distilled sources are cited by a derived memory's `source_trace_ids` —
    /// so a reviewed MUST/REFERENCE row carrying none of those markers is
    /// either legacy data demoted before the markers existed or a stray
    /// demotion, and either way a human should decide.
    ///
    /// Info, not Warn: promotion legitimately demotes consumed sources to L0
    /// without citing every member id, so a hit can still be a row whose
    /// content lives on inside a consolidated memory. The restore is one
    /// command — `update --priority` re-derives the layer (MUST→L3,
    /// REFERENCE→L2) — and the detail says so.
    fn check_live_looking_archived(memories: &[crate::models::Memory]) -> Finding {
        let now = chrono::Utc::now();
        // Ids cited as distillation sources by some derived memory: consumed
        // raw evidence, archived by design.
        let cited_as_source: std::collections::HashSet<&str> = memories
            .iter()
            .flat_map(|m| m.source_trace_ids.iter().map(String::as_str))
            .collect();
        let items = memories
            .iter()
            .filter(|m| {
                m.layer == MemoryLayer::L0
                    && m.human_reviewed
                    && matches!(m.priority, Priority::Must | Priority::Reference)
                    && m.superseded_by.is_none()
                    && !m.is_expired_at(now)
                    && !m.namespace.starts_with("archived:")
                    && !m.namespace.starts_with(TRACE_NAMESPACE_PREFIX)
                    && !m.tags.iter().any(|t| t == TRACE_SOURCE_TAG)
                    && !cited_as_source.contains(m.id.as_str())
            })
            .map(|m| FindingItem {
                id: m.id.clone(),
                detail: format!(
                    "human-reviewed {} on the archive layer (L0) — excluded from every \
                     injection path, yet not expired, not superseded, not trace evidence, \
                     and not cited as a distillation source. If no consolidated memory \
                     carries its content (promotion also demotes the sources it consumes), \
                     restore it with `memvault-cli update {} --priority {}`, which \
                     re-derives the layer",
                    format!("{:?}", m.priority).to_lowercase(),
                    m.id,
                    format!("{:?}", m.priority).to_uppercase()
                ),
            })
            .collect();
        finding("live_looking_archived", Severity::Info, items)
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
    async fn test_detects_orphaned_proposal() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let target = fact(&store, "the oncall rotation is alice").await;

        let proposal = crate::proposal::propose_text_edit(
            store.as_ref(),
            &target.id,
            "alice",
            "bob",
            crate::proposal::EditField::Content,
            SourceAgent {
                id: "claude-code".into(),
                agent_type: "coding-assistant".into(),
                session_id: None,
            },
        )
        .await
        .unwrap();

        // Healthy while the target exists.
        let report = Doctor::new(store.clone()).run().await.unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "orphaned_proposal"),
            "a proposal with a live target is not a finding"
        );

        // Forget the target — the proposal is now waiting to supersede nothing.
        store.delete(&target.id).await.unwrap();
        let report = Doctor::new(store).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "orphaned_proposal")
            .expect("an orphaned proposal must be reported");
        assert_eq!(finding.severity, Severity::Warn);
        assert_eq!(finding.count, 1);
        assert_eq!(finding.items[0].id, proposal.proposal_id);
        assert!(
            finding.items[0].detail.contains(&target.id),
            "the detail must name the missing target: {}",
            finding.items[0].detail
        );
        assert!(!report.is_healthy());
    }

    #[tokio::test]
    async fn test_flags_a_proposal_whose_target_was_itself_superseded() {
        // Not missing, so approving still works — but the proposal was written
        // against text that is no longer current, which is worth surfacing.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let target = fact(&store, "the retry budget is three").await;
        let proposal = crate::proposal::propose_text_edit(
            store.as_ref(),
            &target.id,
            "three",
            "five",
            crate::proposal::EditField::Content,
            SourceAgent {
                id: "claude-code".into(),
                agent_type: "coding-assistant".into(),
                session_id: None,
            },
        )
        .await
        .unwrap();

        let replacement = fact(&store, "the retry budget is nine").await;
        store.supersede(&target.id, &replacement.id).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "orphaned_proposal")
            .expect("a proposal against superseded text must be flagged");
        assert_eq!(finding.count, 1);
        assert_eq!(finding.items[0].id, proposal.proposal_id);
        assert!(
            finding.items[0].detail.contains("superseded"),
            "{}",
            finding.items[0].detail
        );
    }

    #[tokio::test]
    async fn test_detects_expired_but_unarchived() {
        // Not a correctness bug — expiry already removes the memory from
        // retrieval — but the archive step is what connects expiry to the
        // auto-forget sweep, which measures retention against `archived_at`. An
        // expired memory that is never archived lingers forever: invisible to
        // retrieval and unreachable by the purge.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut mem = fact(&store, "the freeze ran until friday").await;
        mem.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        store.update(mem.clone()).await.unwrap();

        let report = Doctor::new(store.clone()).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "expired_unarchived")
            .expect("an expired, unarchived memory must be reported");
        assert_eq!(
            finding.severity,
            Severity::Info,
            "actionable but not alarming"
        );
        assert_eq!(finding.count, 1);
        assert!(
            finding.items[0].detail.contains("decay"),
            "the detail must say how to fix it: {}",
            finding.items[0].detail
        );

        // Archiving clears it: the sweep can now reach the memory.
        let mut archived = store.get(&mem.id).await.unwrap();
        archived.namespace = format!("archived:{}", archived.namespace);
        archived.archived_at = Some(chrono::Utc::now());
        store.update(archived).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "expired_unarchived"),
            "an archived expiry is exactly where the retention sweep expects it"
        );
    }

    #[tokio::test]
    async fn test_detects_live_looking_archived() {
        // The upgrade surprise this check exists for: a reviewed preference
        // demoted to L0 stops being injected anywhere (0.6.8's `is_archived`
        // gate) while `list` still shows it looking perfectly live.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut mem = fact(&store, "use markdown tables for comparisons").await;
        mem.human_reviewed = true;
        mem.layer = MemoryLayer::L0;
        store.update(mem.clone()).await.unwrap();

        let report = Doctor::new(store.clone()).run().await.unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.check == "live_looking_archived")
            .expect("a reviewed REFERENCE row on L0 with no archival marker must be reported");
        assert_eq!(
            finding.severity,
            Severity::Info,
            "a human has to decide whether the archive was deliberate"
        );
        assert_eq!(finding.count, 1);
        assert_eq!(finding.items[0].id, mem.id);
        assert!(
            finding.items[0].detail.contains("--priority REFERENCE"),
            "the detail must carry the one-command restore: {}",
            finding.items[0].detail
        );

        // The documented restore — re-deriving the layer from the priority —
        // clears the finding.
        let mut restored = store.get(&mem.id).await.unwrap();
        restored.layer = MemoryLayer::L2;
        store.update(restored).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "live_looking_archived"),
            "a row back on L2 is injectable again — nothing to report"
        );
    }

    #[tokio::test]
    async fn test_live_looking_archived_skips_rows_with_an_explanation() {
        // Every deliberate archival leaves a marker; a row carrying any of
        // them is archived by design and must not be flagged.
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // 1. Superseded (supersede/dedup both set the pointer).
        let mut superseded = fact(&store, "the old deploy sop").await;
        superseded.human_reviewed = true;
        superseded.layer = MemoryLayer::L0;
        superseded.superseded_by = Some("mem_replacement".to_string());
        store.update(superseded).await.unwrap();

        // 2. Expired (decay archives on expiry).
        let mut expired = fact(&store, "the freeze ran until friday").await;
        expired.human_reviewed = true;
        expired.layer = MemoryLayer::L0;
        expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        store.update(expired).await.unwrap();

        // 3. Raw trace evidence (tagged, `trace:*` namespace).
        let mut evidence = fact(&store, "raw transcript turn").await;
        evidence.human_reviewed = true;
        evidence.layer = MemoryLayer::L0;
        evidence.namespace = format!("{TRACE_NAMESPACE_PREFIX}claude-code");
        evidence.tags = vec![TRACE_SOURCE_TAG.to_string()];
        store.update(evidence.clone()).await.unwrap();

        // 4. Distillation source cited by a derived memory.
        let mut source = fact(&store, "a preference observed once").await;
        source.human_reviewed = true;
        source.layer = MemoryLayer::L0;
        store.update(source.clone()).await.unwrap();
        let mut derived = fact(&store, "consolidated preference").await;
        derived.source_trace_ids = vec![evidence.id.clone(), source.id.clone()];
        store.update(derived).await.unwrap();

        // 5. Unreviewed, and 6. Background priority — no "should be live"
        // signal; promotion fodder looks like this.
        let mut unreviewed = fact(&store, "episode residue").await;
        unreviewed.layer = MemoryLayer::L0;
        store.update(unreviewed).await.unwrap();
        let mut background = Memory::new(
            MemoryType::Fact,
            "background noise".to_string(),
            Priority::Background,
            agent(),
        );
        background.human_reviewed = true;
        background.layer = MemoryLayer::L0;
        store.save(background).await.unwrap();

        let report = Doctor::new(store).run().await.unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "live_looking_archived"),
            "every L0 row here has an archival marker or no live signal: {:?}",
            report.findings
        );
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
