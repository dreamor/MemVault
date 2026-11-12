//! Explicit, producer-ordered fact streams. Keys are supplied by the caller;
//! no text extraction, fuzzy matching, or valid-time inference happens here.
//!
//! Management is bound to a live record, not a separate head/tombstone table.
//! Public intake and literal writes discard management claims. Only the atomic
//! resolver can create active management from a verified server context; edits
//! retain a suspended, record-bound guard until that record is deleted/forgotten.

use serde::{Deserialize, Serialize};

use crate::error::{MemVaultError, Result};
use crate::models::{Memory, MemoryType, Priority, Visibility, canonical_project_namespace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactCardinality {
    Single,
    Multi,
    Event,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactAssertion {
    pub key: String,
    pub value: String,
    pub source_namespace: String,
    pub source_order: u64,
    pub cardinality: FactCardinality,
}

fn validate_field(name: &str, value: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(MemVaultError::InvalidInput(format!(
            "{name} must be nonblank, contain no control characters, and be at most {maximum} bytes"
        )));
    }
    if crate::sensitive::is_sensitive(value) {
        return Err(MemVaultError::InvalidInput(format!(
            "{name} appears to contain sensitive credentials; refusing to save"
        )));
    }
    Ok(())
}

impl FactAssertion {
    pub fn validate(&self) -> Result<()> {
        validate_field("fact_assertion.key", &self.key, 1024)?;
        validate_field("fact_assertion.value", &self.value, 8192)?;
        validate_field(
            "fact_assertion.source_namespace",
            &self.source_namespace,
            256,
        )?;
        // A credential assignment can be split across metadata fields.
        if crate::sensitive::is_sensitive(&format!("{}={}", self.key, self.value)) {
            return Err(MemVaultError::InvalidInput(
                "fact assertion appears to contain sensitive credentials; refusing to save".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactWriteMode {
    #[default]
    Preserve,
    Resolve,
}

/// Authentication evidence supplied by the server, never deserialized from a
/// request or inferred from `Memory::identity_verified`/review flags.
///
/// `verified` is for callers that have already checked the named producer's
/// registered key. This is an in-process trust boundary, not an authentication
/// mechanism: transports must not call it for body-declared or admin-source ids.
#[derive(Debug, Clone, Default)]
pub struct FactWriteContext {
    producer_id: Option<String>,
    record_identity: bool,
}

impl FactWriteContext {
    /// Existing in-process callers retain the identity-recording default.
    pub fn verified(producer_id: impl Into<String>) -> Result<Self> {
        Self::verified_with_identity_recording(producer_id, true)
    }

    /// Key-authenticated producer proof and a separate server recording policy.
    /// Disabling public identity recording does not remove private management
    /// authority; enabling it does not create proof for an unverified context.
    pub fn verified_with_identity_recording(
        producer_id: impl Into<String>,
        record_identity: bool,
    ) -> Result<Self> {
        let producer_id = producer_id.into();
        validate_field("verified producer_id", &producer_id, 256)?;
        Ok(Self {
            producer_id: Some(producer_id),
            record_identity,
        })
    }

    pub fn producer_id(&self) -> Option<&str> {
        self.producer_id.as_deref()
    }

    pub(crate) fn records_identity(&self) -> bool {
        self.producer_id.is_some() && self.record_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactManagementState {
    Active,
    Suspended,
}

/// Read-only report of a server-managed stream. Serde is deliberately retained
/// for trusted storage/history codecs; public import must sanitize it. Private
/// fields and record binding prevent callers from manufacturing it through the
/// normal constructors or transferring an existing mark to another record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactManagement {
    record_id: String,
    producer_id: String,
    namespace: String,
    source_namespace: String,
    key: String,
    source_order: u64,
    state: FactManagementState,
}

impl FactManagement {
    pub fn producer_id(&self) -> &str {
        &self.producer_id
    }
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn source_namespace(&self) -> &str {
        &self.source_namespace
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn source_order(&self) -> u64 {
        self.source_order
    }
    pub fn state(&self) -> FactManagementState {
        self.state
    }

    pub(crate) fn for_memory(memory: &Memory, producer_id: &str) -> Result<Self> {
        let assertion = memory
            .fact_assertion
            .as_ref()
            .ok_or_else(|| MemVaultError::InvalidInput("resolve requires fact_assertion".into()))?;
        Ok(Self {
            record_id: memory.id.clone(),
            producer_id: producer_id.into(),
            namespace: canonical_project_namespace(&memory.namespace).into_owned(),
            source_namespace: assertion.source_namespace.clone(),
            key: assertion.key.clone(),
            source_order: assertion.source_order,
            state: FactManagementState::Active,
        })
    }

    pub(crate) fn validate(&self, record_id: &str) -> Result<()> {
        if self.record_id != record_id {
            return Err(MemVaultError::InvalidInput(
                "fact management belongs to another record".into(),
            ));
        }
        validate_field("fact_management.producer_id", &self.producer_id, 256)?;
        validate_field("fact_management.namespace", &self.namespace, 1024)?;
        validate_field(
            "fact_management.source_namespace",
            &self.source_namespace,
            256,
        )?;
        validate_field("fact_management.key", &self.key, 1024)
    }

    pub(crate) fn suspend(&mut self) {
        self.state = FactManagementState::Suspended;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactWriteOutcome {
    Inserted,
    Merged,
    Skipped,
    Superseded,
    Idempotent,
    Stale,
    Ambiguity,
    Conflict,
    ReviewRequired,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct FactWriteResult {
    pub outcome: FactWriteOutcome,
    /// Persisted current/winning row when unambiguous, never an unsaved input.
    pub memory: Option<Memory>,
    pub superseded_id: Option<String>,
    pub reason: Option<String>,
}

impl FactWriteResult {
    pub fn is_written(&self) -> bool {
        matches!(
            self.outcome,
            FactWriteOutcome::Inserted | FactWriteOutcome::Merged | FactWriteOutcome::Superseded
        )
    }

    pub(crate) fn new(
        outcome: FactWriteOutcome,
        memory: Option<Memory>,
        reason: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            memory,
            superseded_id: None,
            reason: reason.map(str::to_owned),
        }
    }

    pub fn unsupported() -> Self {
        Self::new(
            FactWriteOutcome::Unsupported,
            None,
            Some("backend does not support atomic fact resolution"),
        )
    }
}

/// Both asserted records and suspended guards are isolated from fuzzy delta,
/// dedup scans, vector-fallback dedup, promotion, and residual text absorption.
pub(crate) fn is_structured(memory: &Memory) -> bool {
    memory.fact_assertion.is_some() || memory.fact_management.is_some()
}

pub(crate) fn resolve_eligibility(
    memory: &Memory,
    context: &FactWriteContext,
) -> Option<&'static str> {
    let Some(producer) = context.producer_id() else {
        return Some("a verified producer is required");
    };
    if memory.source_agent.id != producer {
        return Some("source author does not match verified producer");
    }
    if memory
        .fact_assertion
        .as_ref()
        .is_none_or(|a| a.cardinality != FactCardinality::Single)
    {
        return Some("resolve requires an explicit single-valued assertion");
    }
    if memory.memory_type != MemoryType::Fact
        || memory.priority != Priority::Reference
        || memory.human_reviewed
        || !memory.ai_generated
        || memory.visibility != Visibility::Scoped
        || memory.instruction.is_some()
        || memory.skill_meta.is_some()
        || memory.proposes_supersede_of.is_some()
        || memory.superseded_by.is_some()
        || !matches!(
            memory.layer,
            crate::models::MemoryLayer::L1 | crate::models::MemoryLayer::L2
        )
        || memory.namespace.starts_with("archived:")
        || memory.archived_at.is_some()
        || memory
            .tags
            .iter()
            .any(|tag| crate::rerank::AUTHORITY_TAGS.contains(&tag.to_lowercase().as_str()))
    {
        return Some("only scoped, unreviewed AI Fact/REFERENCE streams can be managed");
    }
    None
}

/// Ignore record ids, ingest timestamps and usage/decay counters. All supplied
/// semantic/authority metadata must agree for an equal-ordinal retry; the body
/// is part of the assertion, so an equal value with a different body conflicts.
pub(crate) fn same_semantic_payload(a: &Memory, b: &Memory) -> bool {
    a.content == b.content
        && a.instruction == b.instruction
        && a.memory_type == b.memory_type
        && a.priority == b.priority
        && a.namespace == b.namespace
        && a.tags == b.tags
        && a.source_agent.id == b.source_agent.id
        && a.source_agent.agent_type == b.source_agent.agent_type
        && a.source_agent.session_id == b.source_agent.session_id
        && a.confidence == b.confidence
        && a.ai_generated == b.ai_generated
        && a.human_reviewed == b.human_reviewed
        && a.visibility == b.visibility
        && a.layer == b.layer
        && a.occurred_at == b.occurred_at
        && a.expires_at == b.expires_at
        && a.fact_assertion == b.fact_assertion
        && a.skill_meta == b.skill_meta
        && a.source_trace_ids == b.source_trace_ids
        && a.friction_evidence == b.friction_evidence
        && a.proposes_supersede_of == b.proposes_supersede_of
}

/// Normalize an ordinary update using the *persisted* mark, never an incoming
/// claim. Semantic/namespace/authority edits clear stale assertions and suspend
/// management but retain original stream identity for future lookup/refusal.
/// Nonsemantic usage/decay changes may retain it; atomic retirement intentionally
/// keeps old assertion/mark for history and is performed through the trusted path.
pub(crate) fn sanitize_update(existing: &Memory, incoming: &mut Memory) {
    let changed = existing.content != incoming.content
        || existing.instruction != incoming.instruction
        || existing.namespace != incoming.namespace
        || existing.memory_type != incoming.memory_type
        || existing.priority != incoming.priority
        || existing.source_agent.id != incoming.source_agent.id
        || existing.source_agent.agent_type != incoming.source_agent.agent_type
        || existing.source_agent.session_id != incoming.source_agent.session_id
        || existing.confidence != incoming.confidence
        || existing.corroborating_agents != incoming.corroborating_agents
        || existing.source_trace_ids != incoming.source_trace_ids
        || existing.friction_evidence != incoming.friction_evidence
        || existing.human_reviewed != incoming.human_reviewed
        || existing.ai_generated != incoming.ai_generated
        || existing.identity_verified != incoming.identity_verified
        || existing.visibility != incoming.visibility
        || existing.proposes_supersede_of != incoming.proposes_supersede_of
        || existing.superseded_by != incoming.superseded_by
        || existing.layer != incoming.layer
        || existing.tags != incoming.tags
        || existing.occurred_at != incoming.occurred_at
        || existing.expires_at != incoming.expires_at
        || existing.fact_assertion != incoming.fact_assertion
        || existing.skill_meta != incoming.skill_meta;
    incoming.fact_management = existing.fact_management.clone();
    if changed && is_structured(existing) {
        incoming.fact_assertion = None;
        if let Some(management) = incoming.fact_management.as_mut() {
            management.suspend();
        }
    }
}

/// Index identity follows the mark's original namespace after suspension, so a
/// human namespace edit cannot accidentally make the old stream writable again.
pub(crate) fn group_fields(memory: &Memory) -> (Option<&str>, Option<&str>, Option<&str>) {
    if let Some(mark) = memory.fact_management.as_ref() {
        (
            Some(mark.namespace()),
            Some(mark.source_namespace()),
            Some(mark.key()),
        )
    } else if let Some(assertion) = memory.fact_assertion.as_ref() {
        (
            Some(&memory.namespace),
            Some(&assertion.source_namespace),
            Some(&assertion.key),
        )
    } else {
        (None, None, None)
    }
}
