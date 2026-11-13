//! Private, durable authority for one explicitly approved Markdown replacement.
//!
//! The vault's editable relationships are not approval receipts. Only records
//! written here by the review path authorize replay of their frozen payloads.
//! This protects against lower-trust Markdown writers, not unrestricted
//! filesystem processes running as the same account.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::markdown::{LockGuard, MarkdownStore};
use crate::error::{MemVaultError, Result};
use crate::io::{Exporter, Importer};
use crate::models::{Memory, MemoryLayer};
use crate::private_fs::{
    ensure_private_dir, reject_symlink_components, remove_private_file_durable, sync_directory,
    validate_private_metadata, write_private_atomic,
};

const VERSION: u32 = 1;
const LOCK_NAME: &str = ".memvault.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalPair {
    pub(super) target: Memory,
    pub(super) proposal: Memory,
    /// Exact Markdown bytes, not a filename. Before-images preserve raw
    /// formatting; approved after-images are canonical frozen renders.
    pub(super) target_file: String,
    pub(super) proposal_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalPhase {
    Intent,
    Aborting,
    /// Both files are durable. Cache publication may be retried, but this
    /// irrevocable decision can never transition back into compensation.
    Committing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalIntent {
    pub(super) version: u32,
    pub(super) operation_id: String,
    pub(super) vault_id: String,
    pub(super) phase: ApprovalPhase,
    pub(super) before: ApprovalPair,
    pub(super) after: ApprovalPair,
    // Includes immutable identity, both complete Memory snapshots, and their
    // canonical file bytes. The phase is the only mutable journal field.
    frozen_hash: String,
}

impl ApprovalIntent {
    pub(super) fn new(vault_id: &str, before: ApprovalPair, after: ApprovalPair) -> Result<Self> {
        let mut intent = Self {
            version: VERSION,
            operation_id: uuid::Uuid::new_v4().to_string(),
            vault_id: vault_id.to_string(),
            phase: ApprovalPhase::Intent,
            before,
            after,
            frozen_hash: String::new(),
        };
        intent.frozen_hash = intent.compute_frozen_hash()?;
        intent.validate(vault_id)?;
        Ok(intent)
    }

    fn compute_frozen_hash(&self) -> Result<String> {
        Ok(content_hash(&serde_json::to_vec(&(
            self.version,
            &self.operation_id,
            &self.vault_id,
            &self.before,
            &self.after,
        ))?))
    }

    pub(super) fn validate(&self, vault_id: &str) -> Result<()> {
        if self.version != VERSION {
            return Err(journal_error("unsupported approval journal version"));
        }
        validate_operation_id(&self.operation_id)?;
        if !valid_hash(&self.vault_id) || self.vault_id != vault_id {
            return Err(journal_error(
                "approval journal belongs to a different vault",
            ));
        }
        if !valid_hash(&self.frozen_hash) || self.frozen_hash != self.compute_frozen_hash()? {
            return Err(journal_error(
                "approval journal frozen payload hash mismatch",
            ));
        }
        validate_pair(&self.before, false)?;
        validate_pair(&self.after, true)?;
        let before = &self.before;
        let after = &self.after;
        if before.target.id == before.proposal.id
            || before.target.id != after.target.id
            || before.proposal.id != after.proposal.id
            || before.target.created_at != after.target.created_at
            || before.proposal.created_at != after.proposal.created_at
            || before.proposal.source_agent.id != after.proposal.source_agent.id
            || before.proposal.source_agent.agent_type != after.proposal.source_agent.agent_type
            || before.proposal.source_agent.session_id != after.proposal.source_agent.session_id
        {
            return Err(journal_error("approval journal paired identity mismatch"));
        }
        // An explicit, caller-authenticated reapproval may issue a NEW intent
        // for a legacy half-landed same-proposal claim. This constructor is
        // never called by enumeration merely because Markdown has that link.
        if before
            .target
            .superseded_by
            .as_deref()
            .is_some_and(|claim| claim != before.proposal.id)
            || before.proposal.human_reviewed
            || before.proposal.superseded_by.is_some()
            || before.proposal.proposes_supersede_of.as_deref() != Some(before.target.id.as_str())
            || !after.proposal.human_reviewed
            || after.proposal.proposes_supersede_of.is_some()
            || after.proposal.superseded_by.is_some()
            || after.target.superseded_by.as_deref() != Some(after.proposal.id.as_str())
            || after.target.layer != MemoryLayer::L0
        {
            return Err(journal_error("invalid frozen approval transition"));
        }
        // Retirement permits only its deterministic fact normalization, not
        // arbitrary assertion/management edits or changes to the old payload.
        // Restoring flags on AFTER would wrongly reject a cleared assertion or
        // suspended guard that the human retirement legitimately produced.
        let mut expected_retired = before.target.clone();
        expected_retired.layer = after.target.layer.clone();
        expected_retired.superseded_by = after.target.superseded_by.clone();
        crate::facts::sanitize_update(&before.target, &mut expected_retired);
        if revision_fingerprint(&expected_retired)? != revision_fingerprint(&after.target)? {
            return Err(journal_error(
                "approval retirement changes unrelated target fields",
            ));
        }
        // Reviewed edits can change the proposal payload, but cannot transfer,
        // remove, or reactivate its persisted management binding.
        let mut expected_proposal = after.proposal.clone();
        crate::facts::sanitize_update(&before.proposal, &mut expected_proposal);
        if revision_fingerprint(&expected_proposal)? != revision_fingerprint(&after.proposal)? {
            return Err(journal_error(
                "approval proposal has unnormalized fact metadata",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApprovalDisposition {
    Complete,
    Aborted,
    Cancelled,
}

/// Content-free terminal authority. Denying unknown fields also prevents a
/// supposedly terminal record from quietly retaining a before/after payload.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalReceipt {
    version: u32,
    operation_id: String,
    vault_id: String,
    disposition: ApprovalDisposition,
}

impl ApprovalReceipt {
    fn validate(&self, vault_id: &str, operation_id: &str) -> Result<()> {
        validate_operation_id(&self.operation_id)?;
        if self.version != VERSION || self.vault_id != vault_id || self.operation_id != operation_id
        {
            return Err(journal_error(
                "invalid or unsupported terminal approval receipt",
            ));
        }
        Ok(())
    }
}

pub(super) struct ApprovalJournal {
    // Hold the same single-writer discipline as the Markdown vault for the
    // entire journal lifetime. Never infer stale lock liveness from its PID.
    _lock: LockGuard,
    root: PathBuf,
    vault_dir: PathBuf,
    dir: PathBuf,
    receipts_dir: PathBuf,
    vault_id: String,
}

impl ApprovalJournal {
    pub(super) fn default_root() -> Result<PathBuf> {
        let base = if let Some(home) = std::env::var_os("MEMVAULT_HOME") {
            if home.is_empty() {
                return Err(journal_error(
                    "MEMVAULT_HOME is empty; no private journal fallback",
                ));
            }
            let home = home.into_string().map_err(|_| {
                journal_error("MEMVAULT_HOME is not valid Unicode for home expansion")
            })?;
            if home.starts_with("~/") && std::env::var_os("HOME").is_none() {
                return Err(journal_error("HOME is required to expand MEMVAULT_HOME"));
            }
            PathBuf::from(crate::env_file::shellexpand_home(&home))
        } else {
            let home = std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .ok_or_else(|| journal_error("HOME is unavailable for private approval journal"))?;
            PathBuf::from(home).join(".memvault")
        };
        Ok(base.join("markdown-approvals"))
    }

    pub(super) fn open(vault: &Path, private_root: &Path) -> Result<Self> {
        reject_symlink_components(vault).map_err(storage_io)?;
        let vault = std::fs::canonicalize(vault).map_err(storage_io)?;
        if !vault.is_dir() {
            return Err(journal_error("approval vault must be a directory"));
        }
        let root = if private_root.is_absolute() {
            private_root.to_path_buf()
        } else {
            std::fs::canonicalize(std::env::current_dir().map_err(storage_io)?)
                .map_err(storage_io)?
                .join(private_root)
        };
        reject_symlink_components(&root).map_err(storage_io)?;
        // With symlink/parent-traversal components rejected, this check also
        // works for a not-yet-created root. Never create journal files in-vault.
        if root.starts_with(&vault) {
            return Err(journal_error(
                "private approval journal must be outside the Markdown vault",
            ));
        }
        ensure_private_dir(&root).map_err(storage_io)?;
        let root = std::fs::canonicalize(&root).map_err(storage_io)?;
        if root.starts_with(&vault) {
            return Err(journal_error(
                "private approval journal resolves inside the Markdown vault",
            ));
        }
        let vault_id = content_hash(vault.as_os_str().as_encoded_bytes());
        let vault_dir = root.join(&vault_id);
        ensure_private_dir(&vault_dir).map_err(storage_io)?;
        let dir = vault_dir.join("operations");
        let receipts_dir = vault_dir.join("receipts");
        ensure_private_dir(&dir).map_err(storage_io)?;
        ensure_private_dir(&receipts_dir).map_err(storage_io)?;
        let lock_path = vault_dir.join(LOCK_NAME);
        match std::fs::symlink_metadata(&lock_path) {
            Ok(metadata) => {
                validate_private_metadata(&lock_path, &metadata, false).map_err(storage_io)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_io(error)),
        }
        let lock = LockGuard::acquire(&lock_path)?;
        validate_private_file(&lock_path)?;
        sync_directory(&vault_dir).map_err(storage_io)?;
        let journal = Self {
            _lock: lock,
            root,
            vault_dir,
            dir,
            receipts_dir,
            vault_id,
        };
        journal.validate_directory()?;
        Ok(journal)
    }

    pub(super) fn vault_id(&self) -> &str {
        &self.vault_id
    }

    /// Active operations only; terminal history lives in a sibling directory
    /// and is consulted by exact operation ID, never scanned during recovery.
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn write_intent(&self, intent: &ApprovalIntent) -> Result<()> {
        self.validate_directory()?;
        intent.validate(&self.vault_id)?;
        if intent.phase != ApprovalPhase::Intent {
            return Err(journal_error("write_intent requires the intent phase"));
        }
        if self.read_receipt(&intent.operation_id)?.is_some() {
            return Err(journal_error(
                "terminal approval operation cannot be reused",
            ));
        }
        if let Some(existing) = self.read_intent(&intent.operation_id)?
            && (existing.frozen_hash != intent.frozen_hash || existing.phase != intent.phase)
        {
            return Err(journal_error(
                "approval operation would overwrite different authority",
            ));
        }
        self.write_record(&self.intent_path(&intent.operation_id), intent)
    }

    /// Persist the irrevocable commit decision after both files are durable
    /// and before cache publication. A stale caller cannot revive an aborting
    /// operation, and repeated commit marking preserves the frozen authority.
    pub(super) fn mark_committing(&self, intent: &mut ApprovalIntent) -> Result<()> {
        self.validate_directory()?;
        intent.validate(&self.vault_id)?;
        if !matches!(
            intent.phase,
            ApprovalPhase::Intent | ApprovalPhase::Committing
        ) {
            return Err(journal_error("aborting approval cannot commit"));
        }
        if self.read_receipt(&intent.operation_id)?.is_some() {
            return Err(journal_error("terminal approval cannot begin committing"));
        }
        let existing = self
            .read_intent(&intent.operation_id)?
            .ok_or_else(|| journal_error("approval intent is missing before commit"))?;
        if existing.frozen_hash != intent.frozen_hash {
            return Err(journal_error("approval authority changed before commit"));
        }
        if !matches!(
            existing.phase,
            ApprovalPhase::Intent | ApprovalPhase::Committing
        ) {
            return Err(journal_error("durably aborting approval cannot commit"));
        }
        let mut committing = intent.clone();
        committing.phase = ApprovalPhase::Committing;
        self.write_record(&self.intent_path(&intent.operation_id), &committing)?;
        *intent = committing;
        Ok(())
    }

    /// Persist the no-roll-forward decision before callers perform any
    /// compensation. On an error the caller must not infer the phase from
    /// partially observed public Markdown files. Committing is irrevocable,
    /// including when the caller still holds a stale in-memory Intent.
    pub(super) fn mark_aborting(&self, intent: &mut ApprovalIntent) -> Result<()> {
        self.validate_directory()?;
        intent.validate(&self.vault_id)?;
        if intent.phase == ApprovalPhase::Committing {
            return Err(journal_error(
                "committing approval cannot begin compensation",
            ));
        }
        if self.read_receipt(&intent.operation_id)?.is_some() {
            return Err(journal_error("terminal approval cannot begin compensation"));
        }
        let existing = self
            .read_intent(&intent.operation_id)?
            .ok_or_else(|| journal_error("approval intent is missing before compensation"))?;
        if existing.frozen_hash != intent.frozen_hash {
            return Err(journal_error(
                "approval authority changed before compensation",
            ));
        }
        if existing.phase == ApprovalPhase::Committing {
            return Err(journal_error(
                "durably committing approval cannot begin compensation",
            ));
        }
        let mut aborting = intent.clone();
        aborting.phase = ApprovalPhase::Aborting;
        self.write_record(&self.intent_path(&intent.operation_id), &aborting)?;
        *intent = aborting;
        Ok(())
    }

    /// Enumerate actual durable operations, not the inbox or any bounded
    /// cache query. Validate every entry before performing any cleanup.
    pub(super) fn intents(&self) -> Result<Vec<ApprovalIntent>> {
        self.validate_directory()?;
        let mut records = BTreeMap::new();
        let mut operation_ids = BTreeSet::new();
        let mut cleanup = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(storage_io)? {
            let entry = entry.map_err(storage_io)?;
            let path = entry.path();
            validate_private_file(&path)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| journal_error("non-ASCII approval journal filename"))?;
            if let Some(stem) = name.strip_suffix(".tmp-private") {
                let operation_id = operation_from_record_name(stem)?;
                validate_operation_id(operation_id)?;
                operation_ids.insert(operation_id.to_string());
                cleanup.push(path);
            } else if let Some(operation_id) = name.strip_suffix(".json") {
                validate_operation_id(operation_id)?;
                operation_ids.insert(operation_id.to_string());
                records.insert(operation_id.to_string(), path);
            } else {
                return Err(journal_error("unexpected file in active approval journal"));
            }
        }
        let mut intents = Vec::new();
        for operation_id in operation_ids {
            // Targeted lookups make recovery scale with active operations,
            // not the ever-growing history of content-free terminal receipts.
            let receipt = self.read_receipt(&operation_id)?;
            let receipt_temp = temporary_path(&self.receipt_path(&operation_id));
            if private_file_present(&receipt_temp)? {
                // A temporary is not terminal authority, even if its bytes
                // happen to parse. Validate metadata only, then discard it.
                cleanup.push(receipt_temp);
            }
            if let Some(path) = records.remove(&operation_id) {
                if receipt.is_some() {
                    // The durable terminal receipt precedes payload removal.
                    // Never replay this record after a crash during unlink.
                    cleanup.push(path);
                } else {
                    let intent: ApprovalIntent = self.read_record(&path)?;
                    intent.validate(&self.vault_id)?;
                    if intent.operation_id != operation_id {
                        return Err(journal_error(
                            "approval filename/operation identity mismatch",
                        ));
                    }
                    intents.push(intent);
                }
            }
        }
        for path in cleanup {
            self.remove_record_if_present(&path)?;
        }
        Ok(intents)
    }

    /// Write content-free terminal authority first, then durably unlink every
    /// payload-bearing private record/temporary. A cleanup error is returned;
    /// the already-durable receipt still prevents replay on the next open.
    pub(super) fn finish(
        &self,
        intent: &ApprovalIntent,
        disposition: ApprovalDisposition,
    ) -> Result<()> {
        self.validate_directory()?;
        intent.validate(&self.vault_id)?;
        if (disposition == ApprovalDisposition::Complete
            && intent.phase != ApprovalPhase::Committing)
            || (disposition == ApprovalDisposition::Aborted
                && intent.phase != ApprovalPhase::Aborting)
        {
            return Err(journal_error(
                "approval disposition does not match durable phase",
            ));
        }
        if let Some(receipt) = self.read_receipt(&intent.operation_id)? {
            if receipt.disposition != disposition {
                return Err(journal_error("terminal approval disposition cannot change"));
            }
        } else {
            let existing = self
                .read_intent(&intent.operation_id)?
                .ok_or_else(|| journal_error("cannot finish an absent approval operation"))?;
            if existing.frozen_hash != intent.frozen_hash || existing.phase != intent.phase {
                return Err(journal_error(
                    "approval authority or phase changed before finish",
                ));
            }
            let receipt = ApprovalReceipt {
                version: VERSION,
                operation_id: intent.operation_id.clone(),
                vault_id: self.vault_id.clone(),
                disposition,
            };
            self.write_record(&self.receipt_path(&intent.operation_id), &receipt)?;
        }
        let operation_path = self.intent_path(&intent.operation_id);
        let receipt_path = self.receipt_path(&intent.operation_id);
        for path in [
            operation_path.clone(),
            temporary_path(&operation_path),
            temporary_path(&receipt_path),
        ] {
            self.remove_record_if_present(&path)?;
        }
        Ok(())
    }

    fn validate_directory(&self) -> Result<()> {
        for directory in [&self.root, &self.vault_dir, &self.dir, &self.receipts_dir] {
            reject_symlink_components(directory).map_err(storage_io)?;
            validate_private_metadata(
                directory,
                &std::fs::symlink_metadata(directory).map_err(storage_io)?,
                true,
            )
            .map_err(storage_io)?;
        }
        let mut entries = 0;
        for entry in std::fs::read_dir(&self.vault_dir).map_err(storage_io)? {
            let entry = entry.map_err(storage_io)?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| journal_error("unexpected filename in per-vault journal root"))?;
            let directory = match name.as_str() {
                LOCK_NAME => false,
                "operations" | "receipts" => true,
                _ => return Err(journal_error("unexpected entry in per-vault journal root")),
            };
            validate_private_metadata(
                &path,
                &std::fs::symlink_metadata(&path).map_err(storage_io)?,
                directory,
            )
            .map_err(storage_io)?;
            entries += 1;
        }
        if entries != 3 {
            return Err(journal_error("incomplete per-vault journal root"));
        }
        Ok(())
    }

    fn intent_path(&self, operation_id: &str) -> PathBuf {
        self.dir.join(format!("{operation_id}.json"))
    }

    fn receipt_path(&self, operation_id: &str) -> PathBuf {
        self.receipts_dir
            .join(format!("{operation_id}.receipt.json"))
    }

    fn read_intent(&self, operation_id: &str) -> Result<Option<ApprovalIntent>> {
        let path = self.intent_path(operation_id);
        if !private_file_present(&path)? {
            return Ok(None);
        }
        let intent: ApprovalIntent = self.read_record(&path)?;
        intent.validate(&self.vault_id)?;
        if intent.operation_id != operation_id {
            return Err(journal_error(
                "approval filename/operation identity mismatch",
            ));
        }
        Ok(Some(intent))
    }

    fn read_receipt(&self, operation_id: &str) -> Result<Option<ApprovalReceipt>> {
        let path = self.receipt_path(operation_id);
        if !private_file_present(&path)? {
            return Ok(None);
        }
        let receipt: ApprovalReceipt = self.read_record(&path)?;
        receipt.validate(&self.vault_id, operation_id)?;
        Ok(Some(receipt))
    }

    fn read_record<T: for<'de> Deserialize<'de> + Serialize>(&self, path: &Path) -> Result<T> {
        validate_private_file(path)?;
        let bytes = std::fs::read(path).map_err(storage_io)?;
        let record: T = serde_json::from_slice(&bytes)
            .map_err(|_| journal_error("malformed private approval journal record"))?;
        let original: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| journal_error("malformed private approval journal record"))?;
        // Memory's general-purpose import deserializer tolerates unknown or
        // missing defaulted fields. Private replay must not silently discard
        // fields from an unsupported snapshot schema or invent missing ones.
        if original != serde_json::to_value(&record)? {
            return Err(journal_error(
                "unsupported or incomplete approval snapshot schema",
            ));
        }
        Ok(record)
    }

    fn write_record(&self, path: &Path, record: &impl Serialize) -> Result<()> {
        private_file_present(path)?;
        private_file_present(&temporary_path(path))?;
        write_private_atomic(path, &serde_json::to_vec(record)?).map_err(storage_io)
    }

    fn remove_record_if_present(&self, path: &Path) -> Result<()> {
        if private_file_present(path)? {
            remove_private_file_durable(path).map_err(storage_io)
        } else {
            // Absence may follow a previous unlink whose directory fsync
            // failed. A successful retry must make that absence durable too.
            let parent = path
                .parent()
                .ok_or_else(|| journal_error("approval record has no parent directory"))?;
            sync_directory(parent).map_err(storage_io)
        }
    }
}

/// Content-normalized semantic/trust/lifecycle revision. Only read/access/
/// decay bookkeeping is ignored; `created_at` remains immutable identity.
/// Serialize the entire model and remove the explicit bookkeeping whitelist,
/// so future model fields are significant by default rather than forgotten.
pub(super) fn revision_fingerprint(memory: &Memory) -> Result<String> {
    if !memory.confidence.is_finite() {
        return Err(journal_error("non-finite confidence in approval revision"));
    }
    let mut normalized = memory.clone();
    normalized.content = normalized.content.trim().to_string();
    normalized.updated_at = normalized.created_at;
    normalized.access_count = 0;
    normalized.last_read_at = None;
    normalized.decay_score = 1.0;
    normalized.expires_at = normalized
        .expires_at
        .map(|expiry| {
            chrono::DateTime::from_timestamp_micros(expiry.timestamp_micros()).ok_or_else(|| {
                journal_error("approval expiry cannot be stored at microsecond precision")
            })
        })
        .transpose()?;
    let mut value = serde_json::to_value(&normalized)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| journal_error("approval revision is not an object"))?;
    for key in ["updated_at", "access_count", "last_read_at", "decay_score"] {
        object.remove(key);
    }
    Ok(content_hash(&serde_json::to_vec(&value)?))
}

pub(super) fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_pair(pair: &ApprovalPair, require_canonical: bool) -> Result<()> {
    for (memory, file) in [
        (&pair.target, &pair.target_file),
        (&pair.proposal, &pair.proposal_file),
    ] {
        MarkdownStore::validate_id(&memory.id)?;
        for id in [
            memory.superseded_by.as_deref(),
            memory.proposes_supersede_of.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            MarkdownStore::validate_id(id)?;
        }
        memory.validate_content()?;
        memory.validate_fact_metadata()?;
        if !memory.confidence.is_finite() || !memory.decay_score.is_finite() {
            return Err(journal_error("non-finite value in frozen approval payload"));
        }
        // Before-images preserve the user's exact formatting for compensation.
        // The approved after-images must be the canonical frozen render.
        if require_canonical && Exporter::memory_to_markdown(memory) != *file {
            return Err(journal_error(
                "frozen approval file bytes disagree with snapshot",
            ));
        }
        let parsed = if require_canonical {
            Importer::parse_markdown(file)
        } else {
            // Reindex treats filename identity and the frozen creation time
            // as context for minimal frontmatter. Never let a fresh parser
            // UUID or current clock replace either during before validation.
            Importer::parse_markdown_with_context(file, &memory.id, memory.created_at)
        }
        .map_err(|_| journal_error("frozen approval Markdown is not parseable"))?;
        if revision_fingerprint(&parsed)? != revision_fingerprint(memory)? {
            return Err(journal_error(
                "frozen approval Markdown loses semantic payload",
            ));
        }
    }
    Ok(())
}

fn validate_operation_id(operation_id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(operation_id)
        .map_err(|_| journal_error("invalid approval operation UUID"))?;
    if uuid.get_version_num() != 4 || uuid.to_string() != operation_id {
        return Err(journal_error(
            "approval operation must be a canonical UUIDv4 filename",
        ));
    }
    Ok(())
}

fn operation_from_record_name(name: &str) -> Result<&str> {
    name.strip_suffix(".json")
        .ok_or_else(|| journal_error("unexpected approval temporary filename"))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp-private");
    PathBuf::from(name)
}

fn private_file_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_private_metadata(path, &metadata, false).map_err(storage_io)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage_io(error)),
    }
}

fn validate_private_file(path: &Path) -> Result<()> {
    reject_symlink_components(path).map_err(storage_io)?;
    validate_private_metadata(
        path,
        &std::fs::symlink_metadata(path).map_err(storage_io)?,
        false,
    )
    .map_err(storage_io)
}

fn journal_error(message: &str) -> MemVaultError {
    MemVaultError::Storage(message.to_string())
}

fn storage_io(error: std::io::Error) -> MemVaultError {
    MemVaultError::Storage(format!("private approval journal I/O failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, Priority, SkillMeta, SourceAgent, Visibility};

    struct TempRoots {
        vault: PathBuf,
        private: PathBuf,
    }

    impl TempRoots {
        fn new() -> Self {
            let label = uuid::Uuid::new_v4();
            let vault = std::env::temp_dir().join(format!("memvault_approval_vault_{label}"));
            let private = std::env::temp_dir().join(format!("memvault_approval_private_{label}"));
            std::fs::create_dir(&vault).unwrap();
            Self { vault, private }
        }

        fn open(&self) -> ApprovalJournal {
            ApprovalJournal::open(&self.vault, &self.private).unwrap()
        }
    }

    impl Drop for TempRoots {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.vault);
            let _ = std::fs::remove_dir_all(&self.private);
        }
    }

    fn memory(content: &str) -> Memory {
        Memory::new(
            MemoryType::Fact,
            content.to_string(),
            Priority::Reference,
            SourceAgent {
                id: "synthetic-review-test".to_string(),
                agent_type: "test".to_string(),
                session_id: Some("synthetic-session".to_string()),
            },
        )
    }

    fn pair(target: Memory, proposal: Memory) -> ApprovalPair {
        ApprovalPair {
            target_file: Exporter::memory_to_markdown(&target),
            proposal_file: Exporter::memory_to_markdown(&proposal),
            target,
            proposal,
        }
    }

    fn intent(journal: &ApprovalJournal) -> ApprovalIntent {
        let mut target = memory("synthetic BEFORE secret marker");
        target.human_reviewed = true;
        let mut proposal = memory("synthetic AFTER secret marker");
        proposal.proposes_supersede_of = Some(target.id.clone());
        let before = pair(target.clone(), proposal.clone());
        target.layer = MemoryLayer::L0;
        target.superseded_by = Some(proposal.id.clone());
        proposal.human_reviewed = true;
        proposal.proposes_supersede_of = None;
        ApprovalIntent::new(journal.vault_id(), before, pair(target, proposal)).unwrap()
    }

    fn fact_intent(journal: &ApprovalJournal, suspended_before: bool) -> ApprovalIntent {
        let mut target = memory("synthetic managed fact BEFORE");
        let mut proposal = memory("synthetic managed proposal BEFORE");
        proposal.proposes_supersede_of = Some(target.id.clone());
        for (memory, value, order) in [(&mut target, "Tuesday", 7), (&mut proposal, "Wednesday", 8)]
        {
            memory.namespace = "project:synthetic-facts".to_string();
            memory.fact_assertion = Some(crate::facts::FactAssertion {
                key: "release-day".to_string(),
                value: value.to_string(),
                source_namespace: "synthetic-release-stream".to_string(),
                source_order: order,
                cardinality: crate::facts::FactCardinality::Single,
            });
            memory.fact_management = Some(
                crate::facts::FactManagement::for_memory(memory, &memory.source_agent.id).unwrap(),
            );
            if suspended_before {
                memory.fact_assertion = None;
                memory.fact_management.as_mut().unwrap().suspend();
            }
        }
        let before = pair(target.clone(), proposal.clone());
        target.layer = MemoryLayer::L0;
        target.superseded_by = Some(proposal.id.clone());
        proposal.content = "synthetic human-reviewed fact AFTER".to_string();
        proposal.human_reviewed = true;
        proposal.proposes_supersede_of = None;
        proposal.fact_management = None;
        crate::facts::sanitize_update(&before.target, &mut target);
        crate::facts::sanitize_update(&before.proposal, &mut proposal);
        ApprovalIntent::new(journal.vault_id(), before, pair(target, proposal)).unwrap()
    }

    #[test]
    fn fact_retirement_accepts_only_expected_clear_and_suspended_binding() {
        for suspended_before in [false, true] {
            let roots = TempRoots::new();
            let journal = roots.open();
            let original = fact_intent(&journal, suspended_before);
            for after in [&original.after.target, &original.after.proposal] {
                assert!(after.fact_assertion.is_none());
                assert_eq!(
                    after.fact_management.as_ref().unwrap().state(),
                    crate::facts::FactManagementState::Suspended
                );
            }
            journal.write_intent(&original).unwrap();
            let loaded = journal.intents().unwrap().remove(0);
            assert_eq!(
                serde_json::to_value(&loaded).unwrap(),
                serde_json::to_value(&original).unwrap()
            );

            for edit in [0, 1, 2, 3] {
                let mut changed = original.clone();
                match edit {
                    0 => changed.after.target.content.push_str(" unrelated edit"),
                    1 => changed.after.target.fact_management = None,
                    2 => {
                        let mut different = changed.before.target.clone();
                        different.fact_assertion = original.before.proposal.fact_assertion.clone();
                        if different.fact_assertion.is_none() {
                            different.fact_assertion = Some(crate::facts::FactAssertion {
                                key: "different-key".to_string(),
                                value: "different-value".to_string(),
                                source_namespace: "different-stream".to_string(),
                                source_order: 99,
                                cardinality: crate::facts::FactCardinality::Single,
                            });
                        }
                        let mut management = crate::facts::FactManagement::for_memory(
                            &different,
                            &different.source_agent.id,
                        )
                        .unwrap();
                        management.suspend();
                        changed.after.target.fact_management = Some(management);
                    }
                    _ => {
                        changed.after.target.fact_assertion = Some(crate::facts::FactAssertion {
                            key: "release-day".to_string(),
                            value: "Tuesday".to_string(),
                            source_namespace: "synthetic-release-stream".to_string(),
                            source_order: 7,
                            cardinality: crate::facts::FactCardinality::Single,
                        });
                    }
                }
                changed.after.target_file = Exporter::memory_to_markdown(&changed.after.target);
                rewrite_hash(&mut changed);
                assert!(changed.validate(journal.vault_id()).is_err());
            }
        }
    }

    #[test]
    fn fact_proposal_guard_and_immutable_source_cannot_be_substituted() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let original = fact_intent(&journal, false);
        for edit in [0, 1, 2, 3, 4, 5] {
            let mut changed = original.clone();
            match edit {
                0 => changed.after.proposal.fact_management = None,
                1 => {
                    changed.after.proposal.fact_management =
                        changed.before.proposal.fact_management.clone();
                }
                2 => changed.after.proposal.source_agent.id = "different-author".to_string(),
                3 => changed.after.proposal.source_agent.agent_type = "different-type".to_string(),
                4 => changed.after.proposal.source_agent.session_id = None,
                _ => changed.after.proposal.created_at += chrono::Duration::seconds(1),
            }
            changed.after.proposal_file = Exporter::memory_to_markdown(&changed.after.proposal);
            rewrite_hash(&mut changed);
            assert!(changed.validate(journal.vault_id()).is_err());
        }
        let mut malformed = original;
        malformed
            .before
            .proposal
            .fact_assertion
            .as_mut()
            .unwrap()
            .key
            .clear();
        malformed.before.proposal_file = Exporter::memory_to_markdown(&malformed.before.proposal);
        rewrite_hash(&mut malformed);
        assert!(malformed.validate(journal.vault_id()).is_err());
    }

    #[test]
    fn revision_fingerprint_covers_fact_assertion_and_management_identity() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let original = fact_intent(&journal, false);
        let before = &original.before.target;
        let mut changed = before.clone();
        changed.fact_assertion.as_mut().unwrap().value = "Thursday".to_string();
        assert_ne!(
            revision_fingerprint(before).unwrap(),
            revision_fingerprint(&changed).unwrap()
        );
        changed = before.clone();
        changed.fact_management.as_mut().unwrap().suspend();
        assert_ne!(
            revision_fingerprint(before).unwrap(),
            revision_fingerprint(&changed).unwrap()
        );
        changed = before.clone();
        changed.fact_management = None;
        assert_ne!(
            revision_fingerprint(before).unwrap(),
            revision_fingerprint(&changed).unwrap()
        );
    }

    fn rewrite_hash(intent: &mut ApprovalIntent) {
        intent.frozen_hash = intent.compute_frozen_hash().unwrap();
    }

    #[test]
    fn durable_intent_and_aborting_phase_survive_reopen() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        assert!(journal.dir().starts_with(&roots.private));
        assert!(!journal.dir().starts_with(&roots.vault));
        journal.write_intent(&operation).unwrap();
        journal.write_intent(&operation).unwrap();
        let expected = serde_json::to_value(&operation).unwrap();
        drop(journal);
        let journal = roots.open();
        let pending = journal.intents().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(serde_json::to_value(&pending[0]).unwrap(), expected);
        journal.mark_aborting(&mut operation).unwrap();
        assert_eq!(operation.phase, ApprovalPhase::Aborting);
        drop(journal);
        let journal = roots.open();
        assert_eq!(journal.intents().unwrap()[0].phase, ApprovalPhase::Aborting);
        journal.mark_aborting(&mut operation).unwrap();
        journal
            .finish(&operation, ApprovalDisposition::Aborted)
            .unwrap();
        assert!(journal.intents().unwrap().is_empty());
    }

    #[test]
    fn committing_is_durable_irrevocable_and_preserves_frozen_authority() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        let mut stale = operation.clone();
        let frozen_hash = operation.frozen_hash.clone();
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Complete)
                .is_err()
        );
        journal.mark_committing(&mut operation).unwrap();
        assert_eq!(operation.phase, ApprovalPhase::Committing);
        assert_eq!(operation.frozen_hash, frozen_hash);
        assert!(journal.mark_aborting(&mut operation).is_err());
        assert!(journal.mark_aborting(&mut stale).is_err());
        assert_eq!(stale.phase, ApprovalPhase::Intent);
        assert!(
            journal
                .finish(&stale, ApprovalDisposition::Complete)
                .is_err()
        );
        assert_eq!(
            journal.intents().unwrap()[0].phase,
            ApprovalPhase::Committing
        );
        drop(journal);
        let journal = roots.open();
        operation = journal.intents().unwrap().remove(0);
        assert_eq!(operation.phase, ApprovalPhase::Committing);
        assert_eq!(operation.frozen_hash, frozen_hash);
        assert!(journal.mark_aborting(&mut stale).is_err());
        journal.mark_committing(&mut operation).unwrap();
        journal.mark_committing(&mut stale).unwrap();
        assert_eq!(stale.phase, ApprovalPhase::Committing);
        assert_eq!(stale.frozen_hash, frozen_hash);
        journal
            .finish(&operation, ApprovalDisposition::Complete)
            .unwrap();
        assert!(journal.intents().unwrap().is_empty());
    }

    #[test]
    fn cancelled_committing_scrubs_authority_but_never_allows_rollback() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        let mut stale = operation.clone();
        journal.mark_committing(&mut operation).unwrap();
        journal
            .finish(&operation, ApprovalDisposition::Cancelled)
            .unwrap();
        assert!(journal.intents().unwrap().is_empty());
        assert!(journal.mark_aborting(&mut operation).is_err());
        assert!(journal.mark_aborting(&mut stale).is_err());
        assert!(journal.mark_committing(&mut operation).is_err());
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Aborted)
                .is_err()
        );
        assert!(journal.write_intent(&stale).is_err());
    }

    #[test]
    fn committing_cannot_revive_aborting_or_changed_authority() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        assert!(journal.mark_committing(&mut operation).is_err());
        journal.write_intent(&operation).unwrap();
        let mut changed = operation.clone();
        changed.after.proposal.content.push_str(" changed");
        changed.after.proposal_file = Exporter::memory_to_markdown(&changed.after.proposal);
        rewrite_hash(&mut changed);
        assert!(journal.mark_committing(&mut changed).is_err());
        let mut stale = operation.clone();
        journal.mark_aborting(&mut operation).unwrap();
        assert!(journal.mark_committing(&mut operation).is_err());
        assert!(journal.mark_committing(&mut stale).is_err());
        assert_eq!(journal.intents().unwrap()[0].phase, ApprovalPhase::Aborting);
        journal
            .finish(&operation, ApprovalDisposition::Aborted)
            .unwrap();
    }

    #[test]
    fn minimal_before_frontmatter_uses_frozen_context_and_preserves_raw_bytes() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let created_at = chrono::DateTime::from_timestamp(1_700_000_000, 123_456_000).unwrap();
        let target_file =
            "---\n# preserve this formatting\ntype: fact\n---\n\n  synthetic minimal target \n"
                .to_string();
        let proposal_file = "---\ntype: fact\nproposes_supersede_of: minimal-target\n---\n\n synthetic minimal proposal\n".to_string();
        let mut target =
            Importer::parse_markdown_with_context(&target_file, "minimal-target", created_at)
                .unwrap();
        let mut proposal =
            Importer::parse_markdown_with_context(&proposal_file, "minimal-proposal", created_at)
                .unwrap();
        assert_eq!(target.id, "minimal-target");
        assert_eq!(proposal.id, "minimal-proposal");
        assert_eq!(target.created_at, created_at);
        assert_eq!(proposal.created_at, created_at);
        let before = ApprovalPair {
            target: target.clone(),
            proposal: proposal.clone(),
            target_file: target_file.clone(),
            proposal_file: proposal_file.clone(),
        };
        target.layer = MemoryLayer::L0;
        target.superseded_by = Some(proposal.id.clone());
        proposal.human_reviewed = true;
        proposal.proposes_supersede_of = None;
        let after = pair(target, proposal);
        let operation =
            ApprovalIntent::new(journal.vault_id(), before.clone(), after.clone()).unwrap();
        operation.validate(journal.vault_id()).unwrap();
        journal.write_intent(&operation).unwrap();
        let pending = journal.intents().unwrap();
        assert_eq!(pending[0].before.target_file, target_file);
        assert_eq!(pending[0].before.proposal_file, proposal_file);
        assert_eq!(pending[0].before.target.created_at, created_at);
        let mut explicit_change = before.clone();
        explicit_change.target_file = target_file.replace("type: fact", "type: preference");
        assert!(ApprovalIntent::new(journal.vault_id(), explicit_change, after.clone()).is_err());
        let mut explicit_created = before;
        explicit_created.target_file =
            target_file.replace("type: fact", "type: fact\ncreated: 2020-01-01T00:00:00Z");
        assert!(ApprovalIntent::new(journal.vault_id(), explicit_created, after.clone()).is_err());
        let mut noncanonical_after = after;
        noncanonical_after.target_file = target_file;
        assert!(
            ApprovalIntent::new(journal.vault_id(), operation.before, noncanonical_after).is_err()
        );
    }

    #[test]
    fn open_binds_canonical_vault_and_holds_private_lifetime_lock() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let expected = content_hash(
            std::fs::canonicalize(&roots.vault)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes(),
        );
        assert_eq!(journal.vault_id(), expected);
        assert!(ApprovalJournal::open(&roots.vault, &roots.private).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [
                &roots.private,
                &journal.vault_dir,
                &journal.dir,
                &journal.receipts_dir,
            ] {
                assert_eq!(
                    std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            assert_eq!(
                std::fs::metadata(journal.vault_dir.join(LOCK_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(journal);
        assert!(roots.open().intents().unwrap().is_empty());
    }

    #[test]
    fn root_inside_vault_is_rejected_without_creation() {
        let roots = TempRoots::new();
        let inside = roots.vault.join("private-journal");
        assert!(ApprovalJournal::open(&roots.vault, &inside).is_err());
        assert!(!inside.exists());
        assert!(ApprovalJournal::open(&roots.vault, &roots.vault).is_err());
    }

    #[test]
    fn terminal_receipts_have_no_payload_and_prevent_reuse() {
        for disposition in [
            ApprovalDisposition::Complete,
            ApprovalDisposition::Aborted,
            ApprovalDisposition::Cancelled,
        ] {
            let roots = TempRoots::new();
            let journal = roots.open();
            let mut operation = intent(&journal);
            journal.write_intent(&operation).unwrap();
            if disposition == ApprovalDisposition::Aborted {
                journal.mark_aborting(&mut operation).unwrap();
            } else if disposition == ApprovalDisposition::Complete {
                journal.mark_committing(&mut operation).unwrap();
            }
            journal.finish(&operation, disposition).unwrap();
            journal.finish(&operation, disposition).unwrap();
            let receipt_path = journal.receipt_path(&operation.operation_id);
            let bytes = std::fs::read(&receipt_path).unwrap();
            let receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 4);
            let text = String::from_utf8(bytes).unwrap();
            for forbidden in [
                "synthetic",
                "content",
                "before",
                "after",
                "target",
                "proposal",
            ] {
                assert!(!text.contains(forbidden), "receipt retained {forbidden}");
            }
            assert!(!journal.intent_path(&operation.operation_id).exists());
            assert!(journal.intents().unwrap().is_empty());
            assert!(journal.write_intent(&operation).is_err());
            assert!(journal.mark_aborting(&mut operation).is_err());
            drop(journal);
            assert!(roots.open().intents().unwrap().is_empty());
        }
    }

    #[test]
    fn terminal_receipt_crash_cleanup_never_replays_stale_payload() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        journal.mark_committing(&mut operation).unwrap();
        let receipt = ApprovalReceipt {
            version: VERSION,
            operation_id: operation.operation_id.clone(),
            vault_id: journal.vault_id().to_string(),
            disposition: ApprovalDisposition::Complete,
        };
        journal
            .write_record(&journal.receipt_path(&operation.operation_id), &receipt)
            .unwrap();
        let temp = temporary_path(&journal.intent_path(&operation.operation_id));
        write_private_atomic(&temp, b"partial synthetic payload").unwrap();
        let receipt_temp = temporary_path(&journal.receipt_path(&operation.operation_id));
        write_private_atomic(&receipt_temp, b"partial receipt").unwrap();
        drop(journal);
        let journal = roots.open();
        assert!(journal.intents().unwrap().is_empty());
        assert!(!journal.intent_path(&operation.operation_id).exists());
        assert!(!temp.exists());
        assert!(!receipt_temp.exists());
        assert!(journal.receipt_path(&operation.operation_id).exists());
    }

    #[test]
    fn stale_private_temporaries_are_removed_not_promoted() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let operation = intent(&journal);
        let orphan = temporary_path(&journal.intent_path(&operation.operation_id));
        write_private_atomic(&orphan, b"incomplete synthetic secret").unwrap();
        assert!(journal.intents().unwrap().is_empty());
        assert!(!orphan.exists());
        journal.write_intent(&operation).unwrap();
        write_private_atomic(&orphan, b"partial phase transition").unwrap();
        assert_eq!(journal.intents().unwrap()[0].phase, ApprovalPhase::Intent);
        assert!(!orphan.exists());
    }

    #[test]
    fn absent_or_changed_authority_cannot_abort_or_finish() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        assert!(journal.mark_aborting(&mut operation).is_err());
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Complete)
                .is_err()
        );
        assert_eq!(operation.phase, ApprovalPhase::Intent);
        journal.write_intent(&operation).unwrap();
        let mut changed = operation.clone();
        changed.after.proposal.content.push_str(" changed");
        changed.after.proposal_file = Exporter::memory_to_markdown(&changed.after.proposal);
        rewrite_hash(&mut changed);
        assert!(journal.write_intent(&changed).is_err());
        assert!(journal.mark_aborting(&mut changed).is_err());
        assert!(
            journal
                .finish(&changed, ApprovalDisposition::Complete)
                .is_err()
        );
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Aborted)
                .is_err()
        );
        journal.mark_aborting(&mut operation).unwrap();
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Complete)
                .is_err()
        );
        assert!(journal.write_intent(&operation).is_err());
        journal
            .finish(&operation, ApprovalDisposition::Cancelled)
            .unwrap();
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Aborted)
                .is_err()
        );
    }

    #[test]
    fn malformed_unsupported_wrong_vault_and_payload_hash_fail_closed() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let operation = intent(&journal);
        let path = journal.intent_path(&operation.operation_id);
        write_private_atomic(&path, b"{ broken").unwrap();
        assert!(journal.intents().is_err());
        let mut corrupted = operation.clone();
        corrupted.version += 1;
        rewrite_hash(&mut corrupted);
        write_private_atomic(&path, &serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(journal.intents().is_err());
        corrupted = operation.clone();
        corrupted.after.proposal.content.push_str(" substituted");
        write_private_atomic(&path, &serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(journal.intents().is_err());
        let other_roots = TempRoots::new();
        let other_journal = other_roots.open();
        let wrong_vault_path = other_journal.intent_path(&operation.operation_id);
        write_private_atomic(&wrong_vault_path, &serde_json::to_vec(&operation).unwrap()).unwrap();
        assert!(other_journal.intents().is_err());
        let mut unsupported = serde_json::to_value(&operation).unwrap();
        unsupported["before"]["target"]["unsupported_authority"] = serde_json::json!(true);
        write_private_atomic(&path, &serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert!(journal.intents().is_err());
        write_private_atomic(&path, &serde_json::to_vec(&operation).unwrap()).unwrap();
        assert_eq!(journal.intents().unwrap().len(), 1);
    }

    #[test]
    fn explicit_new_intent_allows_legacy_same_proposal_claim_but_never_infers_authority() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let original = intent(&journal);
        let mut before = original.before.clone();
        before.target.superseded_by = Some(before.proposal.id.clone());
        before.target.layer = MemoryLayer::L0;
        before.target_file = Exporter::memory_to_markdown(&before.target);
        std::fs::write(
            roots.vault.join(format!("{}.md", before.target.id)),
            &before.target_file,
        )
        .unwrap();
        std::fs::write(
            roots.vault.join(format!("{}.md", before.proposal.id)),
            &before.proposal_file,
        )
        .unwrap();
        assert!(
            journal.intents().unwrap().is_empty(),
            "Markdown links are not authority"
        );
        let explicit =
            ApprovalIntent::new(journal.vault_id(), before.clone(), original.after.clone())
                .unwrap();
        assert_ne!(explicit.operation_id, original.operation_id);
        journal.write_intent(&explicit).unwrap();
        assert_eq!(journal.intents().unwrap().len(), 1);
        before.target.superseded_by = Some("another-proposal".to_string());
        before.target_file = Exporter::memory_to_markdown(&before.target);
        assert!(ApprovalIntent::new(journal.vault_id(), before, original.after).is_err());
    }

    #[test]
    fn frozen_identity_links_file_bytes_and_approved_transition_are_validated() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let original = intent(&journal);
        let mut changed = original.clone();
        changed.operation_id = "../invalid".to_string();
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.after.proposal_file.push_str("changed bytes");
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.after.proposal.human_reviewed = false;
        changed.after.proposal_file = Exporter::memory_to_markdown(&changed.after.proposal);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.after.target.layer = MemoryLayer::L3;
        changed.after.target_file = Exporter::memory_to_markdown(&changed.after.target);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.after.target.created_at += chrono::Duration::seconds(1);
        changed.after.target_file = Exporter::memory_to_markdown(&changed.after.target);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed
            .after
            .target
            .content
            .push_str(" unrelated target edit");
        changed.after.target_file = Exporter::memory_to_markdown(&changed.after.target);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.before.proposal.proposes_supersede_of = Some("../invalid".to_string());
        changed.before.proposal_file = Exporter::memory_to_markdown(&changed.before.proposal);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
        changed = original.clone();
        changed.before.target.id = changed.before.proposal.id.clone();
        changed.before.target_file = Exporter::memory_to_markdown(&changed.before.target);
        rewrite_hash(&mut changed);
        assert!(changed.validate(journal.vault_id()).is_err());
    }

    #[test]
    fn filename_identity_unknown_receipt_fields_and_unknown_files_fail_closed() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let operation = intent(&journal);
        let wrong_name = journal.intent_path(&uuid::Uuid::new_v4().to_string());
        write_private_atomic(&wrong_name, &serde_json::to_vec(&operation).unwrap()).unwrap();
        assert!(journal.intents().is_err());
        remove_private_file_durable(&wrong_name).unwrap();
        journal.write_intent(&operation).unwrap();
        let receipt_path = journal.receipt_path(&operation.operation_id);
        let receipt = serde_json::json!({
            "version": VERSION,
            "operation_id": operation.operation_id,
            "vault_id": journal.vault_id(),
            "disposition": "complete",
            "before": "synthetic retained body"
        });
        write_private_atomic(&receipt_path, &serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(journal.intents().is_err());
        remove_private_file_durable(&receipt_path).unwrap();
        let unknown = journal.dir().join("unexpected.json");
        write_private_atomic(&unknown, b"{}").unwrap();
        assert!(journal.intents().is_err());
    }

    #[test]
    fn finish_failure_keeps_intent_and_never_creates_a_false_receipt() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        journal.mark_committing(&mut operation).unwrap();
        let receipt_path = journal.receipt_path(&operation.operation_id);
        ensure_private_dir(&receipt_path).unwrap();
        assert!(
            journal
                .finish(&operation, ApprovalDisposition::Complete)
                .is_err()
        );
        assert!(journal.intent_path(&operation.operation_id).is_file());
        assert!(receipt_path.is_dir());
        assert!(!temporary_path(&receipt_path).exists());
        std::fs::remove_dir(&receipt_path).unwrap();
        assert_eq!(
            journal.intents().unwrap()[0].phase,
            ApprovalPhase::Committing
        );
    }

    #[cfg(unix)]
    #[test]
    fn unprivate_roots_records_and_symlinks_fail_closed_without_repair() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let roots = TempRoots::new();
        std::fs::create_dir(&roots.private).unwrap();
        std::fs::set_permissions(&roots.private, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ApprovalJournal::open(&roots.vault, &roots.private).is_err());
        assert_eq!(
            std::fs::metadata(&roots.private)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        std::fs::set_permissions(&roots.private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = roots.open();
        let operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        let path = journal.intent_path(&operation.operation_id);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(journal.intents().is_err());
        assert!(journal.write_intent(&operation).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(roots.vault.join("missing-outside-target"), &path).unwrap();
        assert!(journal.intents().is_err());
        assert!(journal.write_intent(&operation).is_err());
        std::fs::remove_file(&path).unwrap();
        let temp = temporary_path(&path);
        symlink(roots.vault.join("missing-outside-target"), &temp).unwrap();
        assert!(journal.intents().is_err());
        assert!(journal.write_intent(&operation).is_err());
        std::fs::remove_file(&temp).unwrap();
        std::fs::set_permissions(&roots.private, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(journal.intents().is_err());
        std::fs::set_permissions(&roots.private, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(journal);
        let link = roots.vault.join("private-link");
        symlink(&roots.private, &link).unwrap();
        assert!(ApprovalJournal::open(&roots.vault, &link).is_err());
        let vault_link = roots.private.join("vault-link");
        symlink(&roots.vault, &vault_link).unwrap();
        assert!(ApprovalJournal::open(&vault_link, &roots.private).is_err());
    }

    #[test]
    fn revision_ignores_only_bookkeeping_and_normalizes_canonical_precision() {
        let original = memory("synthetic payload");
        let expected = revision_fingerprint(&original).unwrap();
        let mut bookkeeping = original.clone();
        bookkeeping.updated_at += chrono::Duration::hours(1);
        bookkeeping.access_count = 123;
        bookkeeping.last_read_at = Some(bookkeeping.updated_at);
        bookkeeping.decay_score = 0.125;
        bookkeeping.content = " \nsynthetic payload\n\t".to_string();
        assert_eq!(revision_fingerprint(&bookkeeping).unwrap(), expected);
        let mut precise = original.clone();
        precise.expires_at =
            Some(chrono::DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap());
        let mut stored = precise.clone();
        stored.expires_at =
            Some(chrono::DateTime::from_timestamp(1_700_000_000, 123_456_000).unwrap());
        assert_eq!(
            revision_fingerprint(&precise).unwrap(),
            revision_fingerprint(&stored).unwrap()
        );
        stored.expires_at = stored
            .expires_at
            .map(|date| date + chrono::Duration::microseconds(1));
        assert_ne!(
            revision_fingerprint(&precise).unwrap(),
            revision_fingerprint(&stored).unwrap()
        );
    }

    #[test]
    fn revision_preserves_semantic_trust_scope_lifecycle_and_created_identity() {
        let original = memory("synthetic payload");
        let expected = revision_fingerprint(&original).unwrap();
        let changes: Vec<(&str, serde_json::Value)> = vec![
            ("id", serde_json::json!("other-memory")),
            ("type", serde_json::json!("preference")),
            ("content", serde_json::json!("changed payload")),
            ("instruction", serde_json::json!("changed instruction")),
            ("priority", serde_json::json!("MUST")),
            (
                "source_agent",
                serde_json::json!({"id":"other","agent_type":"test","session_id":null}),
            ),
            ("namespace", serde_json::json!("project:other")),
            ("confidence", serde_json::json!(0.3)),
            ("tags", serde_json::json!(["changed"])),
            (
                "created_at",
                serde_json::json!(
                    (original.created_at + chrono::Duration::seconds(1)).to_rfc3339()
                ),
            ),
            ("ai_generated", serde_json::json!(false)),
            ("human_reviewed", serde_json::json!(true)),
            (
                "occurred_at",
                serde_json::json!(original.created_at.to_rfc3339()),
            ),
            ("layer", serde_json::json!("L0")),
            (
                "skill_meta",
                serde_json::to_value(SkillMeta {
                    trigger: Some("test".to_string()),
                    steps: vec!["test step".to_string()],
                    verification: None,
                    version: 1,
                })
                .unwrap(),
            ),
            (
                "visibility",
                serde_json::to_value(Visibility::Shared).unwrap(),
            ),
            ("superseded_by", serde_json::json!("successor")),
            ("identity_verified", serde_json::json!(true)),
            ("corroborating_agents", serde_json::json!(["verified-peer"])),
            ("source_trace_ids", serde_json::json!(["trace-id"])),
            ("friction_evidence", serde_json::json!("test evidence")),
            (
                "archived_at",
                serde_json::json!(original.created_at.to_rfc3339()),
            ),
            (
                "expires_at",
                serde_json::json!(original.created_at.to_rfc3339()),
            ),
            ("proposes_supersede_of", serde_json::json!("target")),
        ];
        for (field, value) in changes {
            let mut changed = serde_json::to_value(&original).unwrap();
            changed[field] = value;
            let changed: Memory = serde_json::from_value(changed).unwrap();
            assert_ne!(
                revision_fingerprint(&changed).unwrap(),
                expected,
                "ignored {field}"
            );
        }
    }

    #[test]
    fn large_ordinary_inbox_does_not_hide_one_durable_intent() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let operation = intent(&journal);
        for index in 0..10_001 {
            std::fs::write(
                roots.vault.join(format!("ordinary-pending-{index}.md")),
                &operation.before.proposal_file,
            )
            .unwrap();
        }
        assert!(journal.intents().unwrap().is_empty());
        journal.write_intent(&operation).unwrap();
        let pending = journal.intents().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, operation.operation_id);
    }

    #[test]
    fn terminal_history_is_not_scanned_and_related_receipts_still_control_recovery() {
        use std::io::Write;
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        journal.write_intent(&operation).unwrap();
        let stale = operation.clone();
        // Populate synthetic terminal history without making the test a
        // durability-throughput benchmark. Only the active ID may be read.
        for _ in 0..10_001 {
            let receipt = ApprovalReceipt {
                version: VERSION,
                operation_id: uuid::Uuid::new_v4().to_string(),
                vault_id: journal.vault_id().to_string(),
                disposition: ApprovalDisposition::Complete,
            };
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(journal.receipt_path(&receipt.operation_id))
                .unwrap()
                .write_all(&serde_json::to_vec(&receipt).unwrap())
                .unwrap();
        }
        let corrupt_id = uuid::Uuid::new_v4().to_string();
        let unrelated = journal.receipt_path(&corrupt_id);
        write_private_atomic(&unrelated, b"{ unrelated malformed receipt").unwrap();
        let pending = journal.intents().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, operation.operation_id);
        let mut related_attempt = stale.clone();
        related_attempt.operation_id = corrupt_id;
        rewrite_hash(&mut related_attempt);
        assert!(journal.write_intent(&related_attempt).is_err());
        let related = journal.receipt_path(&operation.operation_id);
        write_private_atomic(&related, b"{ related malformed receipt").unwrap();
        assert!(journal.intents().is_err());
        assert!(journal.intent_path(&operation.operation_id).exists());
        remove_private_file_durable(&related).unwrap();
        journal.mark_committing(&mut operation).unwrap();
        journal
            .finish(&operation, ApprovalDisposition::Complete)
            .unwrap();
        assert!(journal.write_intent(&stale).is_err());
        let path = journal.intent_path(&operation.operation_id);
        write_private_atomic(&path, &serde_json::to_vec(&operation).unwrap()).unwrap();
        let operation_temp = temporary_path(&path);
        let receipt_temp = temporary_path(&related);
        write_private_atomic(&operation_temp, b"partial synthetic payload").unwrap();
        write_private_atomic(&receipt_temp, b"malformed temporary is not a receipt").unwrap();
        assert!(journal.intents().unwrap().is_empty());
        assert!(!path.exists());
        assert!(!operation_temp.exists());
        assert!(!receipt_temp.exists());
        assert!(related.exists());
        assert!(unrelated.exists());
        drop(journal);
        assert!(roots.open().intents().unwrap().is_empty());
    }

    #[test]
    fn per_vault_root_rejects_unknown_entries_without_scanning_receipts() {
        let roots = TempRoots::new();
        let journal = roots.open();
        let unexpected = journal.vault_dir.join("unexpected.json");
        write_private_atomic(&unexpected, b"{}").unwrap();
        assert!(journal.intents().is_err());
        let operation = intent(&journal);
        assert!(journal.write_intent(&operation).is_err());
        remove_private_file_durable(&unexpected).unwrap();
        assert!(journal.intents().unwrap().is_empty());
        let active_receipt = journal
            .dir()
            .join(format!("{}.receipt.json", operation.operation_id));
        write_private_atomic(&active_receipt, b"{}").unwrap();
        assert!(journal.intents().is_err());
        remove_private_file_durable(&active_receipt).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            for directory in [&journal.vault_dir, &journal.dir, &journal.receipts_dir] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
                assert!(journal.intents().is_err());
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            std::fs::remove_dir(&journal.receipts_dir).unwrap();
            symlink(&roots.vault, &journal.receipts_dir).unwrap();
            assert!(journal.intents().is_err());
            std::fs::remove_file(&journal.receipts_dir).unwrap();
            ensure_private_dir(&journal.receipts_dir).unwrap();
        }
        assert!(journal.intents().unwrap().is_empty());
    }

    #[test]
    fn operation_enumeration_is_not_limited_by_inbox_or_ten_thousand_records() {
        use std::io::Write;
        let roots = TempRoots::new();
        let journal = roots.open();
        let mut operation = intent(&journal);
        // Synthetic records only. Direct private creation avoids making this
        // enumeration regression test a durability-throughput benchmark.
        for _ in 0..10_001 {
            operation.operation_id = uuid::Uuid::new_v4().to_string();
            rewrite_hash(&mut operation);
            let path = journal.intent_path(&operation.operation_id);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(path)
                .unwrap()
                .write_all(&serde_json::to_vec(&operation).unwrap())
                .unwrap();
        }
        std::fs::write(
            roots.vault.join("ordinary-pending.md"),
            b"not journal authority",
        )
        .unwrap();
        assert_eq!(journal.intents().unwrap().len(), 10_001);
    }
}
