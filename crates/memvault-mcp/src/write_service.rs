//! Shared REST/MCP write orchestration; authentication remains in the transports
//! (docs/DESIGN.md §9.1 and §13.7).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use memvault_core::embedding::{EMBED_WRITE_WAIT, EmbeddingProvider, LazyEmbedder};
use memvault_core::error::{MemVaultError, Result};
use memvault_core::extractor::ExtractedMemory;
use memvault_core::facts::{
    FactAssertion, FactCardinality, FactWriteContext, FactWriteMode, FactWriteOutcome,
    FactWriteResult,
};
use memvault_core::llm_extractor::LazyLlmExtractor;
use memvault_core::models::{Memory, MemoryType, SourceAgent};
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::writer::{MemoryWriter, WriteOutcome};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::warn;

/// Local wire DTOs keep schema-generation dependencies out of the core.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FactAssertionInput {
    /// Stable caller-supplied subject/property/qualifier identity; never inferred.
    pub key: String,
    pub value: String,
    pub source_namespace: String,
    /// Comparable ordinal within this producer/source stream (not a timestamp).
    pub source_order: u64,
    pub cardinality: FactCardinalityInput,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactCardinalityInput {
    Single,
    Multi,
    Event,
    Unknown,
}

impl TryFrom<FactAssertionInput> for FactAssertion {
    type Error = MemVaultError;

    fn try_from(input: FactAssertionInput) -> Result<Self> {
        let assertion = Self {
            key: input.key,
            value: input.value,
            source_namespace: input.source_namespace,
            source_order: input.source_order,
            cardinality: match input.cardinality {
                FactCardinalityInput::Single => FactCardinality::Single,
                FactCardinalityInput::Multi => FactCardinality::Multi,
                FactCardinalityInput::Event => FactCardinality::Event,
                FactCardinalityInput::Unknown => FactCardinality::Unknown,
            },
        };
        assertion.validate()?;
        Ok(assertion)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactWriteInput {
    #[default]
    Preserve,
    Resolve,
}

impl From<FactWriteInput> for FactWriteMode {
    fn from(input: FactWriteInput) -> Self {
        match input {
            FactWriteInput::Preserve => Self::Preserve,
            FactWriteInput::Resolve => Self::Resolve,
        }
    }
}

/// Transport-produced proof, never deserialized from a request. An admin gate
/// alone is not proof that a caller-declared source produced the extracted text.
pub(crate) struct WriteActor {
    id: String,
    key_authenticated: bool,
    record_identity: bool,
}

impl WriteActor {
    /// `key_authenticated` comes only from successful router authentication,
    /// never from registry membership, body source ids, or client proof flags.
    pub(crate) fn authenticated(id: &str, key_authenticated: bool) -> Self {
        Self {
            id: id.to_string(),
            key_authenticated,
            record_identity: memvault_core::router::identity_verification_enabled(),
        }
    }

    fn bind(&self, memory: &mut Memory) {
        memory.sanitize_public_intake();
        memory.identity_verified =
            self.key_authenticated && self.record_identity && memory.source_agent.id == self.id;
    }

    fn fact_context(&self) -> Result<FactWriteContext> {
        if self.key_authenticated {
            FactWriteContext::verified_with_identity_recording(
                self.id.clone(),
                self.record_identity,
            )
        } else {
            Ok(FactWriteContext::default())
        }
    }
}

enum WriteDecision {
    Legacy(WriteOutcome),
    Fact(FactWriteResult),
}

pub(crate) struct WriteReport {
    decision: WriteDecision,
    pub(crate) embedded: Option<bool>,
    pub(crate) vector_pending: Option<bool>,
    pub(crate) embedding_diagnostic: Option<&'static str>,
}

impl WriteReport {
    pub(crate) fn memory(&self) -> Option<&Memory> {
        match &self.decision {
            WriteDecision::Legacy(outcome) => Some(outcome.memory()),
            WriteDecision::Fact(result) => result.memory.as_ref(),
        }
    }

    pub(crate) fn is_written(&self) -> bool {
        match &self.decision {
            WriteDecision::Legacy(outcome) => !matches!(outcome, WriteOutcome::Skipped { .. }),
            WriteDecision::Fact(result) => result.is_written(),
        }
    }

    pub(crate) fn refused(&self) -> bool {
        matches!(&self.decision, WriteDecision::Fact(result) if matches!(result.outcome,
            FactWriteOutcome::Ambiguity | FactWriteOutcome::Conflict |
            FactWriteOutcome::ReviewRequired | FactWriteOutcome::Unsupported))
    }

    pub(crate) fn unsupported(&self) -> bool {
        matches!(&self.decision, WriteDecision::Fact(result) if result.outcome == FactWriteOutcome::Unsupported)
    }

    pub(crate) fn status(&self) -> &'static str {
        match &self.decision {
            WriteDecision::Legacy(WriteOutcome::Inserted(_)) => "saved",
            WriteDecision::Legacy(WriteOutcome::Merged { .. }) => "merged",
            WriteDecision::Legacy(WriteOutcome::Skipped { .. }) => "skipped",
            WriteDecision::Fact(result) => match result.outcome {
                FactWriteOutcome::Inserted | FactWriteOutcome::Superseded => "saved",
                FactWriteOutcome::Merged => "merged",
                FactWriteOutcome::Skipped
                | FactWriteOutcome::Idempotent
                | FactWriteOutcome::Stale => "skipped",
                FactWriteOutcome::Ambiguity => "ambiguity",
                FactWriteOutcome::Conflict => "conflict",
                FactWriteOutcome::ReviewRequired => "review_required",
                FactWriteOutcome::Unsupported => "unsupported",
            },
        }
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        match &self.decision {
            WriteDecision::Fact(result) => result.reason.as_deref(),
            WriteDecision::Legacy(_) => None,
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let memory = self.memory();
        let mut result = serde_json::json!({
            "id": memory.map(|m| &m.id),
            "status": self.status(),
            "identity_verified": memory.map(|m| m.identity_verified),
            "embedded": self.embedded,
            "vector_pending": self.vector_pending,
        });
        match &self.decision {
            WriteDecision::Legacy(WriteOutcome::Merged {
                similarity,
                residual_added,
                ..
            }) => {
                result["similarity"] = serde_json::json!(similarity);
                result["residual_added"] = serde_json::json!(residual_added);
            }
            WriteDecision::Legacy(WriteOutcome::Skipped { similarity, .. }) => {
                result["similarity"] = serde_json::json!(similarity);
            }
            WriteDecision::Legacy(WriteOutcome::Inserted(_)) => {}
            WriteDecision::Fact(fact) => {
                result["fact_outcome"] = serde_json::json!(fact.outcome);
                result["written"] = serde_json::json!(fact.is_written());
                result["superseded_id"] = serde_json::json!(fact.superseded_id);
                result["reason"] = serde_json::json!(fact.reason);
                result["fact_assertion"] =
                    serde_json::json!(memory.and_then(|m| m.fact_assertion.as_ref()));
                result["fact_management"] =
                    serde_json::json!(memory.and_then(|m| m.fact_management.as_ref()));
                // The persisted current row is the evidence. A refusal never
                // serializes a new request as if it had been stored.
                result["memory"] = serde_json::json!(memory.map(crate::rest_api::memory_to_json));
            }
        }
        if let Some(diagnostic) = self.embedding_diagnostic {
            result["embedding_diagnostic"] = serde_json::json!(diagnostic);
        }
        result
    }
}

#[derive(Clone)]
pub(crate) struct WriteService {
    store: Arc<dyn MemoryStore>,
    embedder: LazyEmbedder,
    backfill_guard: Arc<AtomicBool>,
}

impl WriteService {
    pub(crate) fn new(
        store: Arc<dyn MemoryStore>,
        embedder: LazyEmbedder,
        backfill_guard: Arc<AtomicBool>,
    ) -> Self {
        Self {
            store,
            embedder,
            backfill_guard,
        }
    }

    pub(crate) async fn save(
        &self,
        mut memory: Memory,
        actor: &WriteActor,
        force: bool,
        mode: FactWriteMode,
    ) -> Result<WriteReport> {
        actor.bind(&mut memory);
        validate(&memory)?;
        if !force && mode == FactWriteMode::Resolve {
            if memory.fact_assertion.is_none() {
                return Err(MemVaultError::InvalidInput(
                    "fact_write=resolve requires fact_assertion".into(),
                ));
            }
            // Resolution is a provider-free atomic decision. No embedding is
            // precomputed, and the resolver is invoked exactly once.
            let writer = MemoryWriter::new(self.store.clone(), None);
            let result = writer
                .save_fact(memory, None, false, mode, &actor.fact_context()?)
                .await?;
            if result.is_written()
                && let Some(memory) = result.memory.as_ref()
            {
                metrics::counter!("memvault_memories_saved_total").increment(1);
                self.embed_committed(memory).await;
            }
            let mut report = WriteReport {
                decision: WriteDecision::Fact(result),
                embedded: None,
                vector_pending: None,
                embedding_diagnostic: None,
            };
            self.inspect_embedding(&mut report, false).await;
            return Ok(report);
        }

        let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        let writer = MemoryWriter::new(self.store.clone(), provider.clone());
        self.save_prepared(memory, actor, force, provider.as_deref(), &writer)
            .await
    }

    pub(crate) async fn save_batch(
        &self,
        memories: Vec<Memory>,
        actor: &WriteActor,
        mode: FactWriteMode,
    ) -> Result<Vec<WriteReport>> {
        // Validate every candidate before warming a provider or persisting the
        // first one. This is preflight, not a cross-memory transaction.
        for memory in &memories {
            validate(memory)?;
            if mode == FactWriteMode::Resolve && memory.fact_assertion.is_none() {
                return Err(MemVaultError::InvalidInput(
                    "fact_write=resolve requires explicit fact_assertions for every extracted item"
                        .into(),
                ));
            }
        }
        if mode == FactWriteMode::Resolve {
            let mut reports = Vec::with_capacity(memories.len());
            for memory in memories {
                reports.push(self.save(memory, actor, false, mode).await?);
            }
            return Ok(reports);
        }
        if memories.is_empty() {
            return Ok(Vec::new());
        }
        let provider = self.embedder.get_timeout(EMBED_WRITE_WAIT).await;
        let writer = MemoryWriter::new(self.store.clone(), provider.clone());
        let mut reports = Vec::with_capacity(memories.len());
        for memory in memories {
            reports.push(
                self.save_prepared(memory, actor, false, provider.as_deref(), &writer)
                    .await?,
            );
        }
        Ok(reports)
    }

    async fn save_prepared(
        &self,
        mut memory: Memory,
        actor: &WriteActor,
        force: bool,
        provider: Option<&dyn EmbeddingProvider>,
        writer: &MemoryWriter,
    ) -> Result<WriteReport> {
        actor.bind(&mut memory);
        let text = memory.instruction.as_ref().unwrap_or(&memory.content);
        let embedding = match provider {
            Some(provider) => match provider.embed(std::slice::from_ref(text)).await {
                Ok(vectors) => vectors.into_iter().next(),
                Err(e) => {
                    warn!(error = %e, "auto-embedding failed; saving without a new vector");
                    None
                }
            },
            None => None,
        };
        let structured = memory.fact_assertion.is_some();
        let force = force || memory.memory_type == MemoryType::Skill || memory.skill_meta.is_some();
        let decision = if structured {
            WriteDecision::Fact(
                writer
                    .save_fact(
                        memory,
                        embedding,
                        force,
                        FactWriteMode::Preserve,
                        &actor.fact_context()?,
                    )
                    .await?,
            )
        } else {
            WriteDecision::Legacy(writer.save(memory, embedding, force).await?)
        };
        let mut report = WriteReport {
            decision,
            embedded: None,
            vector_pending: None,
            embedding_diagnostic: None,
        };
        match report.status() {
            "saved" => metrics::counter!("memvault_memories_saved_total").increment(1),
            "merged" => metrics::counter!("memvault_memories_merged_total").increment(1),
            "skipped" => metrics::counter!("memvault_memories_skipped_total").increment(1),
            _ => {}
        }
        self.inspect_embedding(&mut report, true).await;
        Ok(report)
    }

    async fn embed_committed(&self, memory: &Memory) {
        let Some(provider) = self.embedder.get_timeout(EMBED_WRITE_WAIT).await else {
            return;
        };
        let text = memory.instruction.as_ref().unwrap_or(&memory.content);
        match provider.embed(std::slice::from_ref(text)).await {
            Ok(vectors) => {
                if let Some(vector) = vectors.into_iter().next()
                    && let Err(e) = self.store.set_embedding(&memory.id, vector).await
                {
                    warn!(error = %e, "post-commit fact embedding persistence failed");
                }
            }
            Err(e) => warn!(error = %e, "post-commit fact embedding failed; fact remains stored"),
        }
    }

    async fn inspect_embedding(&self, report: &mut WriteReport, legacy_backfill: bool) {
        let Some(memory) = report.memory() else {
            return;
        };
        // The attempted vector is not the persisted state: skip may retain an
        // old vector, merge re-embeds different text, and backfill may fail.
        let (embedded, diagnostic) = match self.store.get_embedding(&memory.id).await {
            Ok(vector) => (Some(vector.is_some()), None),
            Err(e) => {
                warn!(error = %e, "could not inspect persisted outcome embedding");
                (None, Some("persisted_embedding_state_unavailable"))
            }
        };
        report.embedded = embedded;
        report.embedding_diagnostic = diagnostic;
        report.vector_pending = match embedded {
            Some(true) => Some(false),
            Some(false) => Some(self.embedder.expects_embedding().await),
            None => None,
        };
        // Stale, idempotent and refused resolves never schedule network work or
        // mutate embeddings. Written resolves may kick ordinary best-effort
        // backfill; this does not promise globally atomic/CAS vector updates.
        if report.vector_pending == Some(true) && (legacy_backfill || report.is_written()) {
            self.spawn_backfill();
        }
    }

    pub(crate) fn spawn_backfill(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            if service.embedder.expects_embedding().await
                && let Some(provider) = service.embedder.get().await
            {
                MemoryRouter::spawn_embedding_backfill(
                    service.store,
                    provider,
                    service.backfill_guard,
                );
            }
        });
    }

    /// Optional relation work only runs after a successful, clean LLM auto-save.
    /// Its source link is the writer's actual outcome, including legacy skip/merge.
    pub(crate) async fn process_relations(
        &self,
        llm: &LazyLlmExtractor,
        context: &str,
        source: &SourceAgent,
        target: &WriteReport,
    ) -> Option<serde_json::Value> {
        if matches!(&target.decision, WriteDecision::Fact(_)) && !target.is_written() {
            return None;
        }
        let memory = target.memory()?;
        if !std::env::var("MEMVAULT_RELATIONS")
            .ok()
            .is_some_and(|v| memvault_core::env_file::parse_bool(&v) == Some(true))
        {
            return None;
        }
        let llm = llm.get().await?;
        match llm.extract_relations(context).await {
            Ok(triples) if !triples.is_empty() => {
                match memvault_core::relations::store_relation_triples(
                    self.store.as_ref(),
                    &triples,
                    &memory.namespace,
                    source,
                    Some(&memory.id),
                )
                .await
                {
                    Ok(stored) => {
                        if stored > 0 {
                            self.spawn_backfill();
                        }
                        Some(serde_json::json!({ "extracted": triples.len(), "stored": stored }))
                    }
                    Err(e) => {
                        warn!(error = %e, "relation persistence failed; memories kept");
                        None
                    }
                }
            }
            Ok(_) => None,
            Err(e) => {
                warn!(error = %e, "relation extraction failed; memories kept");
                None
            }
        }
    }
}

fn validate(memory: &Memory) -> Result<()> {
    memory.validate_content()?;
    if let Some(assertion) = &memory.fact_assertion {
        assertion.validate()?;
    }
    if memvault_core::sensitive::is_sensitive(&memory.content)
        || memory
            .instruction
            .as_deref()
            .is_some_and(memvault_core::sensitive::is_sensitive)
    {
        return Err(MemVaultError::InvalidInput(
            "content appears to contain sensitive credentials; refusing to save".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn apply_timestamps(
    memory: &mut Memory,
    occurred_at: Option<&str>,
    expires_at: Option<&str>,
) -> Result<()> {
    memory.occurred_at = memvault_core::models::parse_optional_stamp(occurred_at, "occurred_at")
        .map_err(MemVaultError::InvalidInput)?;
    memory.expires_at = expires_at
        .map(|stamp| {
            chrono::DateTime::parse_from_rfc3339(stamp)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| {
                    MemVaultError::InvalidInput(format!(
                        "expires_at must be an RFC3339 timestamp: {e}"
                    ))
                })
        })
        .transpose()?;
    Ok(())
}

/// Convert only caller-supplied, positionally matched extraction metadata.
/// A null entry deliberately leaves that extracted item unasserted.
pub(crate) fn extraction_assertions(
    input: Option<Vec<Option<FactAssertionInput>>>,
    mode: FactWriteMode,
    auto_save: bool,
) -> Result<Option<Vec<Option<FactAssertion>>>> {
    let assertions = input
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.map(FactAssertion::try_from).transpose())
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    if auto_save
        && mode == FactWriteMode::Resolve
        && assertions
            .as_ref()
            .is_none_or(|items| items.is_empty() || items.iter().any(Option::is_none))
    {
        return Err(MemVaultError::InvalidInput(
            "fact_write=resolve with auto_save requires explicit fact_assertions for every extracted item".into(),
        ));
    }
    Ok(assertions)
}

pub(crate) fn extraction_memories(
    extracted: &[ExtractedMemory],
    source: &SourceAgent,
    method: &str,
    occurred_at: Option<&str>,
    assertions: Option<&[Option<FactAssertion>]>,
    mode: FactWriteMode,
) -> Result<Vec<Memory>> {
    if assertions.is_some_and(|items| items.len() != extracted.len()) {
        return Err(MemVaultError::InvalidInput(
            "fact_assertions must have exactly one entry per extracted item (null for no assertion)".into(),
        ));
    }
    extracted
        .iter()
        .enumerate()
        .map(|(i, candidate)| {
            let mut memory = Memory::new(
                candidate.memory_type.clone(),
                candidate.content.clone(),
                candidate.priority.clone(),
                source.clone(),
            );
            memory.instruction = candidate.instruction.clone();
            memory.tags = candidate.tags.clone();
            memory.tags.push(format!("method:{method}"));
            memory.confidence = candidate.confidence;
            memory.fact_assertion = assertions.and_then(|items| items[i].clone());
            // Resolved explicit extraction is the same producer operation across
            // REST/MCP. Normalize only its transport-specific type label; retain
            // declared source id/session and all genuine payload/authority fields.
            if mode == FactWriteMode::Resolve && memory.fact_assertion.is_some() {
                memory.source_agent.agent_type = "extractor".to_string();
            }
            apply_timestamps(&mut memory, occurred_at, None)?;
            validate(&memory)?;
            Ok(memory)
        })
        .collect()
}
