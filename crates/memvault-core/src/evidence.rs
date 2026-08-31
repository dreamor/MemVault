//! Evidence relations: provenance and grounding for memories.
//!
//! Implements the corrected §2.2 reference point from the claude-obsidian
//! review (original analysis doc archived; provenance: docs/DESIGN.md §16):
//! instead of a separate source/claim ledger pair, evidence rides on the
//! existing `memory_relations` table
//! (migration 11) under three canonical predicates:
//!
//! - [`PREDICATE_SUPPORTS`] — `(S, supports, X)`: memory S is evidence FOR X
//! - [`PREDICATE_CONTRADICTS`] — `(S, contradicts, X)`: memory S is counter-evidence against X
//! - [`PREDICATE_SOURCED_FROM`] — `(X, sourced_from, text)`: X was derived
//!   from an external source (URL, document path, conversation reference —
//!   stored as free text via `object_text`)
//!
//! Direction convention: for supports/contradicts the SUBJECT is the
//! evidence and the OBJECT is the memory being supported/contradicted.
//! Decay uses inbound contradicts to accelerate forgetting (§P1 of the
//! review doc): forgetting becomes evidence-driven, not just time-driven.

use chrono::Utc;

use crate::error::{MemVaultError, Result};
use crate::models::{Memory, MemoryRelation};
use crate::storage::MemoryStore;

/// Evidence FOR the object memory.
pub const PREDICATE_SUPPORTS: &str = "supports";
/// Counter-evidence AGAINST the object memory.
pub const PREDICATE_CONTRADICTS: &str = "contradicts";
/// External provenance of the subject memory (free-text object).
pub const PREDICATE_SOURCED_FROM: &str = "sourced_from";

/// The three evidence predicates, for validation at API boundaries.
pub const EVIDENCE_PREDICATES: [&str; 3] = [
    PREDICATE_SUPPORTS,
    PREDICATE_CONTRADICTS,
    PREDICATE_SOURCED_FROM,
];

/// Whether a predicate string is one of the canonical evidence predicates.
pub fn is_evidence_predicate(predicate: &str) -> bool {
    EVIDENCE_PREDICATES.contains(&predicate)
}

/// Parsed form of [`EvidenceKind`] for API boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Supports,
    Contradicts,
    SourcedFrom,
}

impl EvidenceKind {
    pub fn predicate(self) -> &'static str {
        match self {
            EvidenceKind::Supports => PREDICATE_SUPPORTS,
            EvidenceKind::Contradicts => PREDICATE_CONTRADICTS,
            EvidenceKind::SourcedFrom => PREDICATE_SOURCED_FROM,
        }
    }

    /// Parse from the wire form; accepts only the canonical strings.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "supports" => Ok(Self::Supports),
            "contradicts" => Ok(Self::Contradicts),
            "sourced_from" | "sourced-from" => Ok(Self::SourcedFrom),
            other => Err(MemVaultError::InvalidInput(format!(
                "unknown evidence kind '{other}' (expected supports | contradicts | sourced_from)"
            ))),
        }
    }
}

/// Aggregated evidence view of one memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvidenceSummary {
    /// Count of ACTIVE (non-superseded, non-archived) memories supporting it.
    pub supports: usize,
    /// Count of ACTIVE memories contradicting it.
    pub contradicts: usize,
    /// External sources the memory claims provenance from (object texts).
    pub sources: Vec<String>,
}

/// A memory is "active" counter-/support-evidence while it has not been
/// superseded and not archived — a superseded rebuttal must stop biting.
fn is_active(memory: &Memory) -> bool {
    memory.superseded_by.is_none() && !memory.namespace.starts_with("archived:")
}

/// Record one evidence relation.
///
/// Validation:
/// - `supports` / `contradicts` require `object_id` (memory-to-memory);
///   the object must exist and differ from the subject.
/// - `sourced_from` requires non-empty `source` text (external provenance).
/// - Exact-duplicate triples are rejected, same stance as
///   [`crate::relations::store_relation_triples`].
///
/// Returns the stored relation (with its new `relation_id`).
pub async fn add_evidence(
    store: &(impl MemoryStore + ?Sized),
    subject_id: &str,
    kind: EvidenceKind,
    object_id: Option<&str>,
    source: Option<&str>,
    confidence: f64,
) -> Result<MemoryRelation> {
    let subject = store.get(subject_id).await?;

    let (object_id, object_text) = match kind {
        EvidenceKind::Supports | EvidenceKind::Contradicts => {
            let Some(obj) = object_id else {
                return Err(MemVaultError::InvalidInput(format!(
                    "evidence kind '{}' requires object_id (the memory being supported/contradicted)",
                    kind.predicate()
                )));
            };
            if obj == subject.id {
                return Err(MemVaultError::InvalidInput(
                    "a memory cannot be evidence for itself".to_string(),
                ));
            }
            // Existence check: get errors with NotFound when missing.
            store.get(obj).await?;
            (Some(obj.to_string()), None)
        }
        EvidenceKind::SourcedFrom => {
            let Some(text) = source.map(str::trim).filter(|t| !t.is_empty()) else {
                return Err(MemVaultError::InvalidInput(
                    "evidence kind 'sourced_from' requires a non-empty source (URL, document, ...)"
                        .to_string(),
                ));
            };
            (None, Some(text.to_string()))
        }
    };

    // Duplicate guard: same (subject, predicate, object) already stored.
    let existing = store.relations_of_subject(&subject.id).await?;
    let duplicate = existing.iter().any(|r| {
        r.predicate == kind.predicate()
            && r.object_id.as_deref() == object_id.as_deref()
            && r.object_text.as_deref() == object_text.as_deref()
    });
    if duplicate {
        return Err(MemVaultError::InvalidInput(format!(
            "evidence already recorded: {} —{}→ {}",
            subject.id,
            kind.predicate(),
            object_id
                .as_deref()
                .unwrap_or(object_text.as_deref().unwrap_or("?"))
        )));
    }

    let relation = MemoryRelation {
        relation_id: None,
        subject_id: subject.id.clone(),
        predicate: kind.predicate().to_string(),
        object_id,
        object_text,
        confidence: confidence.clamp(0.0, 1.0),
        source_memory_id: None,
        created_at: Utc::now(),
    };
    let relation_id = store.add_relation(relation.clone()).await?;
    Ok(MemoryRelation {
        relation_id: Some(relation_id),
        ..relation
    })
}

/// Collect the evidence profile of one memory:
/// - INBOUND supports/contradicts (subject = the evidence, object = this
///   memory), filtered to active evidence memories;
/// - OUTBOUND sourced_from (object texts).
pub async fn evidence_summary(
    store: &(impl MemoryStore + ?Sized),
    memory_id: &str,
) -> Result<EvidenceSummary> {
    // Existence check first so unknown ids error instead of yielding an
    // innocent-looking empty summary.
    store.get(memory_id).await?;

    let mut summary = EvidenceSummary::default();

    let inbound = store
        .relations_of_object(memory_id)
        .await
        .unwrap_or_default();
    for rel in inbound {
        if rel.predicate != PREDICATE_SUPPORTS && rel.predicate != PREDICATE_CONTRADICTS {
            continue;
        }
        // The evidence memory itself is the SUBJECT of the relation.
        let Ok(evidence_memory) = store.get(rel.subject_id.as_str()).await else {
            continue; // deleted-but-not-cascaded remnants: ignore
        };
        if !is_active(&evidence_memory) {
            continue;
        }
        match rel.predicate.as_str() {
            PREDICATE_SUPPORTS => summary.supports += 1,
            PREDICATE_CONTRADICTS => summary.contradicts += 1,
            _ => {}
        }
    }

    let outbound = store
        .relations_of_subject(memory_id)
        .await
        .unwrap_or_default();
    for rel in outbound {
        if rel.predicate == PREDICATE_SOURCED_FROM
            && let Some(text) = rel.object_text.as_deref()
        {
            summary.sources.push(text.to_string());
        }
    }

    Ok(summary)
}

/// Whether a memory has at least one ACTIVE contradiction against it.
/// Cheap path used by the decay cycle to accelerate forgetting.
pub async fn has_active_contradiction(
    store: &(impl MemoryStore + ?Sized),
    memory_id: &str,
) -> bool {
    let inbound = match store.relations_of_object(memory_id).await {
        Ok(rels) => rels,
        Err(_) => return false,
    };
    for rel in inbound {
        if rel.predicate != PREDICATE_CONTRADICTS {
            continue;
        }
        if let Ok(evidence_memory) = store.get(rel.subject_id.as_str()).await
            && is_active(&evidence_memory)
        {
            return true;
        }
    }
    false
}

/// Find active contradiction pairs *within* a given set of memory ids.
///
/// Unlike [`has_active_contradiction`] (one memory vs. the whole store),
/// this restricts both sides of the relation to `ids` — used to flag
/// conflicts among memories that are actually about to be injected into a
/// session, not just to accelerate decay. Returns `(memory_id,
/// conflicting_with)` pairs, deduplicated regardless of relation direction.
pub async fn contradictions_among(
    store: &(impl MemoryStore + ?Sized),
    ids: &[String],
) -> Result<Vec<(String, String)>> {
    use std::collections::HashSet;

    let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut pairs = Vec::new();

    for id in ids {
        let inbound = store.relations_of_object(id).await.unwrap_or_default();
        for rel in inbound {
            if rel.predicate != PREDICATE_CONTRADICTS {
                continue;
            }
            if !id_set.contains(rel.subject_id.as_str()) {
                continue; // the evidence memory isn't part of this injection batch
            }
            let Ok(evidence_memory) = store.get(&rel.subject_id).await else {
                continue;
            };
            if !is_active(&evidence_memory) {
                continue;
            }
            let key = if id.as_str() < rel.subject_id.as_str() {
                (id.clone(), rel.subject_id.clone())
            } else {
                (rel.subject_id.clone(), id.clone())
            };
            if seen.insert(key) {
                pairs.push((id.clone(), rel.subject_id.clone()));
            }
        }
    }

    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, Priority, SourceAgent};
    use crate::storage::sqlite::SqliteStore;
    use std::sync::Arc;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    async fn memory(store: &SqliteStore, content: &str) -> Memory {
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

    #[test]
    fn test_kind_parse() {
        assert_eq!(
            EvidenceKind::parse("supports").unwrap(),
            EvidenceKind::Supports
        );
        assert_eq!(
            EvidenceKind::parse("CONTRADICTS").unwrap(),
            EvidenceKind::Contradicts
        );
        assert_eq!(
            EvidenceKind::parse("sourced_from").unwrap(),
            EvidenceKind::SourcedFrom
        );
        assert_eq!(
            EvidenceKind::parse("sourced-from").unwrap(),
            EvidenceKind::SourcedFrom
        );
        assert!(EvidenceKind::parse("refutes").is_err());
    }

    #[tokio::test]
    async fn test_add_supports_and_contradicts() {
        let store = SqliteStore::in_memory().unwrap();
        let claim = memory(&store, "deploy window is Friday").await;
        let support = memory(&store, "team agreed on Friday deploys").await;
        let rebuttal = memory(&store, "ops memo bans Friday deploys").await;

        add_evidence(
            &store,
            &support.id,
            EvidenceKind::Supports,
            Some(&claim.id),
            None,
            0.8,
        )
        .await
        .unwrap();
        add_evidence(
            &store,
            &rebuttal.id,
            EvidenceKind::Contradicts,
            Some(&claim.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let summary = evidence_summary(&store, &claim.id).await.unwrap();
        assert_eq!(summary.supports, 1);
        assert_eq!(summary.contradicts, 1);
        assert!(summary.sources.is_empty());
        assert!(has_active_contradiction(&store, &claim.id).await);
    }

    #[tokio::test]
    async fn test_add_sourced_from() {
        let store = SqliteStore::in_memory().unwrap();
        let claim = memory(&store, "postgres 16 in production").await;

        let rel = add_evidence(
            &store,
            &claim.id,
            EvidenceKind::SourcedFrom,
            None,
            Some("https://wiki.example/db-stack"),
            1.0,
        )
        .await
        .unwrap();
        assert_eq!(rel.predicate, PREDICATE_SOURCED_FROM);
        assert_eq!(
            rel.object_text.as_deref(),
            Some("https://wiki.example/db-stack")
        );

        let summary = evidence_summary(&store, &claim.id).await.unwrap();
        assert_eq!(summary.sources, vec!["https://wiki.example/db-stack"]);
    }

    #[tokio::test]
    async fn test_validations() {
        let store = SqliteStore::in_memory().unwrap();
        let a = memory(&store, "memory a").await;
        let b = memory(&store, "memory b").await;

        // supports without object_id
        assert!(
            add_evidence(&store, &a.id, EvidenceKind::Supports, None, None, 0.8)
                .await
                .is_err()
        );
        // contradicts without object_id (same memory-to-memory requirement)
        assert!(
            add_evidence(&store, &a.id, EvidenceKind::Contradicts, None, None, 0.8)
                .await
                .is_err()
        );
        // self-evidence
        assert!(
            add_evidence(
                &store,
                &a.id,
                EvidenceKind::Contradicts,
                Some(&a.id),
                None,
                0.8
            )
            .await
            .is_err()
        );
        // nonexistent object
        assert!(
            add_evidence(
                &store,
                &a.id,
                EvidenceKind::Supports,
                Some("mem_missing"),
                None,
                0.8
            )
            .await
            .is_err()
        );
        // sourced_from without source text
        assert!(
            add_evidence(
                &store,
                &a.id,
                EvidenceKind::SourcedFrom,
                None,
                Some("  "),
                0.8
            )
            .await
            .is_err()
        );
        // sourced_from with explicit None source
        assert!(
            add_evidence(&store, &a.id, EvidenceKind::SourcedFrom, None, None, 0.8)
                .await
                .is_err()
        );
        // unknown memory as subject
        assert!(
            add_evidence(
                &store,
                "mem_missing",
                EvidenceKind::Supports,
                Some(&b.id),
                None,
                0.8
            )
            .await
            .is_err()
        );
        // confidence clamps rather than errors
        let rel = add_evidence(
            &store,
            &a.id,
            EvidenceKind::Supports,
            Some(&b.id),
            None,
            1.7,
        )
        .await
        .unwrap();
        assert_eq!(rel.confidence, 1.0);
    }

    #[tokio::test]
    async fn test_duplicate_evidence_rejected() {
        let store = SqliteStore::in_memory().unwrap();
        let a = memory(&store, "memory a").await;
        let b = memory(&store, "memory b").await;

        add_evidence(
            &store,
            &a.id,
            EvidenceKind::Supports,
            Some(&b.id),
            None,
            0.8,
        )
        .await
        .unwrap();
        let dup = add_evidence(
            &store,
            &a.id,
            EvidenceKind::Supports,
            Some(&b.id),
            None,
            0.5,
        )
        .await;
        assert!(dup.is_err(), "identical triple must be rejected");

        // Different predicate for the same pair is fine.
        add_evidence(
            &store,
            &a.id,
            EvidenceKind::Contradicts,
            Some(&b.id),
            None,
            0.5,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_superseded_rebuttal_stops_biting() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let claim = memory(&store, "the api lives at /v1").await;
        let rebuttal = memory(&store, "the api lives at /v2 only").await;
        let newer = memory(&store, "correction: api moved to /v3").await;

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
        assert!(has_active_contradiction(&*store, &claim.id).await);

        // The rebuttal itself gets superseded → contradiction goes inactive.
        store.supersede(&rebuttal.id, &newer.id).await.unwrap();
        assert!(!has_active_contradiction(&*store, &claim.id).await);

        let summary = evidence_summary(&*store, &claim.id).await.unwrap();
        assert_eq!(summary.contradicts, 0);
    }

    #[tokio::test]
    async fn test_archived_evidence_ignored() {
        let store = SqliteStore::in_memory().unwrap();
        let claim = memory(&store, "claim").await;
        let mut rebuttal = memory(&store, "rebuttal").await;

        add_evidence(
            &store,
            &rebuttal.id,
            EvidenceKind::Contradicts,
            Some(&claim.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        rebuttal.namespace = format!("archived:{}", rebuttal.namespace);
        store.update(rebuttal).await.unwrap();
        assert!(!has_active_contradiction(&store, &claim.id).await);
    }

    #[tokio::test]
    async fn test_summary_unknown_memory_errors() {
        let store = SqliteStore::in_memory().unwrap();
        assert!(evidence_summary(&store, "mem_missing").await.is_err());
    }

    #[tokio::test]
    async fn test_contradictions_among_restricted_to_id_set() {
        let store = SqliteStore::in_memory().unwrap();
        let a = memory(&store, "deploy window is Friday").await;
        let b = memory(&store, "deploy window is Monday").await;
        let unrelated = memory(&store, "unrelated evidence memory").await;

        // b contradicts a, but b is NOT in the batch we ask about below.
        add_evidence(
            &store,
            &b.id,
            EvidenceKind::Contradicts,
            Some(&a.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let ids = vec![a.id.clone(), unrelated.id.clone()];
        let pairs = contradictions_among(&store, &ids).await.unwrap();
        assert!(
            pairs.is_empty(),
            "contradiction must be ignored when the evidence memory isn't in the batch"
        );

        let ids_with_b = vec![a.id.clone(), b.id.clone()];
        let pairs = contradictions_among(&store, &ids_with_b).await.unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(
            (pairs[0].0 == a.id && pairs[0].1 == b.id)
                || (pairs[0].0 == b.id && pairs[0].1 == a.id)
        );
    }

    #[tokio::test]
    async fn test_contradictions_among_ignores_superseded_evidence() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let a = memory(&store, "claim a").await;
        let b = memory(&store, "claim b contradicts a").await;
        let newer = memory(&store, "correction").await;

        add_evidence(
            &*store,
            &b.id,
            EvidenceKind::Contradicts,
            Some(&a.id),
            None,
            0.9,
        )
        .await
        .unwrap();
        store.supersede(&b.id, &newer.id).await.unwrap();

        let ids = vec![a.id.clone(), b.id.clone()];
        let pairs = contradictions_among(&*store, &ids).await.unwrap();
        assert!(
            pairs.is_empty(),
            "superseded evidence must not surface as a conflict"
        );
    }
}
