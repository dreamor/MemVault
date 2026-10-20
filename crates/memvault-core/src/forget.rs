//! "Forget" feature: permanent, auditable memory erasure.
//!
//! Distinct from [`crate::storage::MemoryStore::delete`] (soft delete — a
//! full snapshot survives forever in `memory_history` and is restorable via
//! the sqlite backend's `restore_checkpoint`). This module is the GDPR-style
//! right-to-erasure path: [`MemoryStore::purge`] removes the live row AND
//! every prior history snapshot, [`crate::compliance::ComplianceStore::purge_memory`]
//! cleans the orphaned `compliance_events` rows `delete`/decay never touch,
//! and every erasure is recorded in `forgotten_log` as a content-hash-only
//! audit entry — proof an erasure happened, without retaining what was
//! erased.
//!
//! Three paths, one engine ([`ForgetManager`]):
//! - **Manual**: a human erases one memory by id (CLI/MCP/REST).
//! - **Bulk**: a human erases a criteria-matched set (namespace/agent/tag/
//!   type/age) — always preview-then-confirm.
//! - **Auto**: an unattended retention sweep over memories that have sat in
//!   `archived:*` longer than a configurable threshold — opt-in, off by
//!   default (see [`ForgetConfig::auto_forget_enabled`]), and normally
//!   invoked from the end of `decay::run_decay` via `DecayManager::with_forget`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tracing::warn;

use crate::compliance::ComplianceStore;
use crate::error::{MemVaultError, Result};
use crate::models::{ForgetScope, ForgottenLogEntry, Memory, MemoryType, Priority, Visibility};
use crate::storage::MemoryStore;

/// How many rows a criteria scan pulls in before filtering client-side —
/// mirrors `DecayManager::run_decay`'s existing `list(None, 100_000, 0)`.
/// Neither `MemoryStore::list` nor `search` can express "namespace LIKE
/// 'archived:%' AND archived_at < cutoff" server-side; this is a
/// pre-existing limitation of the trait, not introduced here.
pub const FORGET_SCAN_LIMIT: usize = 100_000;

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => default,
    }
}

fn default_archived_retention_days() -> i64 {
    std::env::var("MEMVAULT_FORGET_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90)
}

fn default_auto_forget_enabled() -> bool {
    // Default OFF: unlike decay's archiving (reversible — archived
    // memories just sit there), auto-forget is permanent. Flipping every
    // existing install's "archived forever" into "eventually purged" by
    // default on upgrade would be a surprising, irreversible behavior
    // change — this must be an explicit opt-in.
    std::env::var("MEMVAULT_FORGET_AUTO_ENABLED")
        .ok()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Env-var-driven configuration (same free-function pattern as
/// `embedding.rs`/`friction.rs`/`outcome_inference.rs` — no serde/YAML
/// struct for this one).
#[derive(Debug, Clone)]
pub struct ForgetConfig {
    /// Memories archived longer than this (by `Memory.archived_at`) are
    /// eligible for the auto-forget sweep. `MEMVAULT_FORGET_RETENTION_DAYS`,
    /// default 90.
    pub archived_retention_days: i64,
    /// Whether `ForgetManager::auto_forget` does anything at all.
    /// `MEMVAULT_FORGET_AUTO_ENABLED`, default `false`.
    pub auto_forget_enabled: bool,
    /// Whether `Priority::Must` memories are protected from forget unless
    /// the caller explicitly overrides (`ForgetOptions::allow_must`).
    /// `MEMVAULT_FORGET_MUST_EXEMPT`, default `true`.
    pub must_exempt: bool,
    /// Whether `Visibility::Shared` memories are excluded from the
    /// **auto** sweep specifically (manual/bulk forget can still touch them
    /// — a human explicitly choosing to is sufficient confirmation).
    /// `MEMVAULT_FORGET_SHARED_AUTO_EXEMPT`, default `true`.
    pub shared_exempt_from_auto: bool,
}

impl Default for ForgetConfig {
    fn default() -> Self {
        Self {
            archived_retention_days: default_archived_retention_days(),
            auto_forget_enabled: default_auto_forget_enabled(),
            must_exempt: env_flag("MEMVAULT_FORGET_MUST_EXEMPT", true),
            shared_exempt_from_auto: env_flag("MEMVAULT_FORGET_SHARED_AUTO_EXEMPT", true),
        }
    }
}

/// Criteria selecting which memories a bulk/auto forget touches. All
/// fields are AND-combined. `ids` (when non-empty) bypasses criteria
/// matching entirely — a single manual forget must not pay the cost of a
/// full-table scan.
#[derive(Debug, Clone, Default)]
pub struct ForgetCriteria {
    pub ids: Vec<String>,
    pub namespace: Option<String>,
    pub namespace_prefix: Option<String>,
    pub source_agent_id: Option<String>,
    pub tag: Option<String>,
    pub memory_type: Option<MemoryType>,
    /// Matches on `Memory.updated_at` — consistent with decay's own "time
    /// since last touched" semantics. Not `created_at` (ingestion time) or
    /// `occurred_at` (often absent, caller-supplied event time).
    pub updated_before: Option<DateTime<Utc>>,
    /// Matches on `Memory.archived_at`; memories that were never archived
    /// never match this filter.
    pub archived_before: Option<DateTime<Utc>>,
}

/// Per-call options controlling a forget operation.
#[derive(Debug, Clone)]
pub struct ForgetOptions {
    pub scope: ForgetScope,
    /// Explicit override to include `Priority::Must` memories, which are
    /// otherwise excluded whenever `ForgetConfig::must_exempt` is true.
    pub allow_must: bool,
    /// Explicit override to include `Visibility::Shared` memories in the
    /// `Auto` scope (ignored for `Manual`/`Bulk`, which never apply this
    /// exclusion in the first place).
    pub allow_shared: bool,
    /// `execute()` refuses to run unless this is `true` — callers must
    /// preview first and pass back an explicit confirmation.
    pub confirmed: bool,
    pub reason: Option<String>,
    pub requested_by: String,
}

/// Why a candidate was excluded from an otherwise-matching forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgetExclusionReason {
    MustProtected,
    SharedNeedsConfirmation,
}

impl ForgetExclusionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ForgetExclusionReason::MustProtected => "must-protected",
            ForgetExclusionReason::SharedNeedsConfirmation => "shared-needs-confirmation",
        }
    }
}

impl std::fmt::Display for ForgetExclusionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// One memory that matched (or was excluded from) a forget criteria —
/// enough detail for a dry-run/preview UI without retaining the full
/// content beyond this call's response.
#[derive(Debug, Clone)]
pub struct ForgetCandidate {
    pub id: String,
    pub namespace: String,
    pub priority: Priority,
    pub visibility: Visibility,
    /// Truncated to 80 chars — preview/dry-run display only, never logged
    /// (the audit trail stores a hash, never content; see
    /// `content_fingerprint`).
    pub content_preview: String,
    pub excluded_reason: Option<ForgetExclusionReason>,
}

#[derive(Debug, Clone, Default)]
pub struct ForgetPreview {
    pub matched: Vec<ForgetCandidate>,
    pub excluded: Vec<ForgetCandidate>,
}

#[derive(Debug, Clone, Default)]
pub struct ForgetReport {
    pub purged: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub compliance_rows_purged: usize,
    /// Candidates that matched the criteria but were skipped (id, reason) —
    /// e.g. Must-protected. Surfaced so a caller forgetting a single
    /// protected memory sees *why* nothing happened instead of a silent
    /// "0 purged".
    pub excluded: Vec<(String, String)>,
}

/// SHA-256 hex digest of `content`, for the audit log — deliberate reuse of
/// `crate::auth::hash_key`'s hasher (a generic string-hashing helper, not
/// an API-key-specific operation).
pub fn content_fingerprint(content: &str) -> String {
    crate::auth::hash_key(content)
}

/// Orchestrates manual/bulk/auto erasure across the memory store and
/// (optionally) the compliance store's orphaned rows.
pub struct ForgetManager {
    store: Arc<dyn MemoryStore>,
    config: ForgetConfig,
    compliance: Option<Arc<ComplianceStore>>,
}

impl ForgetManager {
    pub fn new(store: Arc<dyn MemoryStore>, config: ForgetConfig) -> Self {
        Self {
            store,
            config,
            compliance: None,
        }
    }

    /// Opt into cleaning up `compliance_events` orphans on purge. Without
    /// this, those rows are left behind (same gap `delete()`/decay already
    /// have today).
    pub fn with_compliance(mut self, compliance: Arc<ComplianceStore>) -> Self {
        self.compliance = Some(compliance);
        self
    }

    /// Preview what `execute` would do, without mutating anything.
    pub async fn preview(
        &self,
        criteria: &ForgetCriteria,
        opts: &ForgetOptions,
    ) -> Result<ForgetPreview> {
        self.match_candidates(criteria, opts).await
    }

    /// Shared id-resolution + exclusion-tagging used by both `preview` and
    /// `execute`.
    async fn match_candidates(
        &self,
        criteria: &ForgetCriteria,
        opts: &ForgetOptions,
    ) -> Result<ForgetPreview> {
        let candidates: Vec<Memory> = if !criteria.ids.is_empty() {
            let mut found = Vec::with_capacity(criteria.ids.len());
            for id in &criteria.ids {
                if let Ok(m) = self.store.get(id).await {
                    found.push(m);
                }
            }
            found
        } else {
            let all = self.store.list(None, FORGET_SCAN_LIMIT, 0).await?;
            all.into_iter()
                .filter(|m| {
                    criteria
                        .namespace
                        .as_deref()
                        .is_none_or(|ns| m.namespace == ns)
                        && criteria
                            .namespace_prefix
                            .as_deref()
                            .is_none_or(|p| m.namespace.starts_with(p))
                        && criteria
                            .source_agent_id
                            .as_deref()
                            .is_none_or(|a| m.source_agent.id == a)
                        && criteria
                            .tag
                            .as_deref()
                            .is_none_or(|t| m.tags.iter().any(|x| x == t))
                        && criteria
                            .memory_type
                            .as_ref()
                            .is_none_or(|t| &m.memory_type == t)
                        && criteria.updated_before.is_none_or(|c| m.updated_at < c)
                        && criteria
                            .archived_before
                            .is_none_or(|c| m.archived_at.is_some_and(|a| a < c))
                })
                .collect()
        };

        let mut preview = ForgetPreview::default();
        for m in candidates {
            let excluded_reason =
                if m.priority == Priority::Must && self.config.must_exempt && !opts.allow_must {
                    Some(ForgetExclusionReason::MustProtected)
                } else if opts.scope == ForgetScope::Auto
                    && m.visibility == Visibility::Shared
                    && self.config.shared_exempt_from_auto
                    && !opts.allow_shared
                {
                    Some(ForgetExclusionReason::SharedNeedsConfirmation)
                } else {
                    None
                };
            let candidate = ForgetCandidate {
                id: m.id,
                namespace: m.namespace,
                priority: m.priority,
                visibility: m.visibility,
                content_preview: m.content.chars().take(80).collect(),
                excluded_reason,
            };
            if candidate.excluded_reason.is_some() {
                preview.excluded.push(candidate);
            } else {
                preview.matched.push(candidate);
            }
        }
        Ok(preview)
    }

    /// Purge every matched candidate. Requires `opts.confirmed == true` —
    /// callers must call `preview` first and pass back explicit
    /// confirmation. Best-effort per id: one failure is recorded in
    /// `ForgetReport.failed` and does not block the rest of the batch (an
    /// all-or-nothing transaction would let one locked/already-gone row
    /// block an otherwise-compliant erasure of everything else).
    pub async fn execute(
        &self,
        criteria: &ForgetCriteria,
        opts: &ForgetOptions,
    ) -> Result<ForgetReport> {
        if !opts.confirmed {
            return Err(MemVaultError::InvalidInput(
                "forget execute requires options.confirmed = true; call preview() first"
                    .to_string(),
            ));
        }

        let preview = self.match_candidates(criteria, opts).await?;
        let excluded = preview
            .excluded
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    c.excluded_reason.map(|r| r.to_string()).unwrap_or_default(),
                )
            })
            .collect();
        let mut report = ForgetReport {
            excluded,
            ..Default::default()
        };

        for candidate in preview.matched {
            let mem = match self.store.get(&candidate.id).await {
                Ok(m) => m,
                Err(e) => {
                    report.failed.push((candidate.id, e.to_string()));
                    continue;
                }
            };

            let entry = ForgottenLogEntry {
                log_id: None,
                memory_id: mem.id.clone(),
                content_hash: content_fingerprint(&mem.content),
                namespace: mem.namespace.clone(),
                priority: mem.priority.clone(),
                visibility: mem.visibility,
                scope: opts.scope,
                reason: opts.reason.clone(),
                requested_by: opts.requested_by.clone(),
                must_override: mem.priority == Priority::Must,
                erased_at: Utc::now(),
            };
            if let Err(e) = self.store.record_forgotten(&entry).await {
                report.failed.push((mem.id, e.to_string()));
                continue;
            }
            if let Err(e) = self.store.purge(&mem.id).await {
                report.failed.push((mem.id, e.to_string()));
                continue;
            }
            if let Some(compliance) = &self.compliance {
                match compliance.purge_memory(&mem.id).await {
                    Ok(n) => report.compliance_rows_purged += n,
                    // Non-fatal: the memory itself is already erased; a
                    // stray compliance_events row is a lesser leak, not a
                    // reason to report the whole erasure as failed.
                    Err(e) => {
                        warn!(id = %mem.id, error = %e, "compliance orphan cleanup failed after purge")
                    }
                }
            }
            report.purged.push(mem.id);
        }

        Ok(report)
    }

    fn auto_forget_criteria_and_options(&self) -> (ForgetCriteria, ForgetOptions) {
        let cutoff = Utc::now() - Duration::days(self.config.archived_retention_days);
        (
            ForgetCriteria {
                namespace_prefix: Some("archived:".to_string()),
                archived_before: Some(cutoff),
                ..Default::default()
            },
            ForgetOptions {
                scope: ForgetScope::Auto,
                allow_must: false,
                allow_shared: false,
                confirmed: true,
                reason: Some("auto-forget: archived retention exceeded".to_string()),
                requested_by: "system:auto-forget".to_string(),
            },
        )
    }

    /// Run the retention sweep. No-op (returns an empty report) when
    /// `config.auto_forget_enabled` is false — the default. Callable
    /// standalone (CLI `forget --auto`, a cron) or from
    /// `DecayManager::run_decay` via `with_forget`.
    pub async fn auto_forget(&self) -> Result<ForgetReport> {
        if !self.config.auto_forget_enabled {
            return Ok(ForgetReport::default());
        }
        let (criteria, opts) = self.auto_forget_criteria_and_options();
        self.execute(&criteria, &opts).await
    }

    /// Preview what `auto_forget` would purge, without mutating anything —
    /// usable even when `auto_forget_enabled` is false (so an operator can
    /// see what turning it on would do before opting in).
    pub async fn preview_auto_forget(&self) -> Result<ForgetPreview> {
        let (criteria, opts) = self.auto_forget_criteria_and_options();
        self.preview(&criteria, &opts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceAgent;
    use crate::storage::sqlite::SqliteStore;

    fn test_agent() -> SourceAgent {
        SourceAgent {
            id: "test-agent".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    fn test_config() -> ForgetConfig {
        ForgetConfig {
            archived_retention_days: 90,
            auto_forget_enabled: false,
            must_exempt: true,
            shared_exempt_from_auto: true,
        }
    }

    fn manual_options(allow_must: bool) -> ForgetOptions {
        ForgetOptions {
            scope: ForgetScope::Manual,
            allow_must,
            allow_shared: false,
            confirmed: true,
            reason: Some("test".to_string()),
            requested_by: "tester".to_string(),
        }
    }

    #[tokio::test]
    async fn test_preview_matches_by_namespace_prefix() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut archived = Memory::new(
            MemoryType::Fact,
            "old archived fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        archived.namespace = "archived:global".to_string();
        let archived_id = archived.id.clone();
        store.save(archived).await.unwrap();

        let live = Memory::new(
            MemoryType::Fact,
            "still live".to_string(),
            Priority::Reference,
            test_agent(),
        );
        store.save(live).await.unwrap();

        let manager = ForgetManager::new(store, test_config());
        let criteria = ForgetCriteria {
            namespace_prefix: Some("archived:".to_string()),
            ..Default::default()
        };
        let preview = manager
            .preview(&criteria, &manual_options(false))
            .await
            .unwrap();

        assert_eq!(preview.matched.len(), 1);
        assert_eq!(preview.matched[0].id, archived_id);
        assert!(preview.excluded.is_empty());
    }

    #[tokio::test]
    async fn test_preview_excludes_must_without_allow_must() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let must_mem = Memory::new(
            MemoryType::Preference,
            "must rule".to_string(),
            Priority::Must,
            test_agent(),
        );
        let id = must_mem.id.clone();
        store.save(must_mem).await.unwrap();

        let manager = ForgetManager::new(store, test_config());
        let criteria = ForgetCriteria {
            ids: vec![id.clone()],
            ..Default::default()
        };

        let preview = manager
            .preview(&criteria, &manual_options(false))
            .await
            .unwrap();
        assert!(preview.matched.is_empty());
        assert_eq!(preview.excluded.len(), 1);
        assert_eq!(
            preview.excluded[0].excluded_reason,
            Some(ForgetExclusionReason::MustProtected)
        );

        let preview_override = manager
            .preview(&criteria, &manual_options(true))
            .await
            .unwrap();
        assert_eq!(preview_override.matched.len(), 1);
        assert!(preview_override.excluded.is_empty());
    }

    #[tokio::test]
    async fn test_preview_excludes_shared_for_auto_scope_only() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut shared = Memory::new(
            MemoryType::Fact,
            "team pool fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        shared.visibility = Visibility::Shared;
        let id = shared.id.clone();
        store.save(shared).await.unwrap();

        let manager = ForgetManager::new(store, test_config());
        let criteria = ForgetCriteria {
            ids: vec![id.clone()],
            ..Default::default()
        };

        let auto_opts = ForgetOptions {
            scope: ForgetScope::Auto,
            allow_must: false,
            allow_shared: false,
            confirmed: true,
            reason: None,
            requested_by: "system:auto-forget".to_string(),
        };
        let auto_preview = manager.preview(&criteria, &auto_opts).await.unwrap();
        assert!(auto_preview.matched.is_empty());
        assert_eq!(
            auto_preview.excluded[0].excluded_reason,
            Some(ForgetExclusionReason::SharedNeedsConfirmation)
        );

        // Manual/Bulk scopes never apply the shared exclusion — a human
        // explicitly choosing to forget it is sufficient confirmation.
        let manual_preview = manager
            .preview(&criteria, &manual_options(false))
            .await
            .unwrap();
        assert_eq!(manual_preview.matched.len(), 1);
    }

    #[tokio::test]
    async fn test_execute_requires_confirmed() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let manager = ForgetManager::new(store, test_config());
        let mut opts = manual_options(false);
        opts.confirmed = false;
        let err = manager.execute(&ForgetCriteria::default(), &opts).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_execute_purges_and_logs_audit_entry_with_hash_not_content() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "sensitive content to erase".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        let manager = ForgetManager::new(store.clone(), test_config());
        let criteria = ForgetCriteria {
            ids: vec![id.clone()],
            ..Default::default()
        };
        let report = manager
            .execute(&criteria, &manual_options(false))
            .await
            .unwrap();

        assert_eq!(report.purged, vec![id.clone()]);
        assert!(report.failed.is_empty());
        assert!(store.get(&id).await.is_err());

        let log = store.list_forgotten_log(Some(&id), 10).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].content_hash, content_fingerprint(&mem.content));
        assert_ne!(log[0].content_hash, mem.content);
        assert_eq!(log[0].scope, ForgetScope::Manual);
    }

    #[tokio::test]
    async fn test_execute_cleans_compliance_orphans_when_attached() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "to erase with compliance rows".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let compliance = ComplianceStore::new(":memory:").unwrap();
        compliance
            .record_injection("inj_1", &id, &Priority::Reference, "claude")
            .await
            .unwrap();

        let manager = ForgetManager::new(store.clone(), test_config()).with_compliance(compliance);
        let criteria = ForgetCriteria {
            ids: vec![id.clone()],
            ..Default::default()
        };
        let report = manager
            .execute(&criteria, &manual_options(false))
            .await
            .unwrap();

        assert_eq!(report.purged, vec![id]);
        assert_eq!(report.compliance_rows_purged, 1);
    }

    #[tokio::test]
    async fn test_execute_is_best_effort_across_ids() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "real memory".to_string(),
            Priority::Reference,
            test_agent(),
        );
        let good_id = mem.id.clone();
        store.save(mem).await.unwrap();

        let manager = ForgetManager::new(store.clone(), test_config());
        let criteria = ForgetCriteria {
            ids: vec!["mem_does_not_exist".to_string(), good_id.clone()],
            ..Default::default()
        };
        let report = manager
            .execute(&criteria, &manual_options(false))
            .await
            .unwrap();

        // The bad id never becomes a "matched" candidate (match_candidates
        // skips ids that don't resolve via store.get), so it's not reported
        // as failed either — only the real, erasable candidate is acted on.
        assert_eq!(report.purged, vec![good_id.clone()]);
        assert!(store.get(&good_id).await.is_err());
    }

    /// Regression: forgetting a Must-protected memory by id without
    /// `allow_must` must report *why* nothing happened (excluded, not a
    /// silent "0 purged") — surfaced to CLI/MCP/REST callers.
    #[tokio::test]
    async fn test_execute_reports_excluded_candidates_not_silently() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let must_mem = Memory::new(
            MemoryType::Preference,
            "must rule".to_string(),
            Priority::Must,
            test_agent(),
        );
        let id = must_mem.id.clone();
        store.save(must_mem).await.unwrap();

        let manager = ForgetManager::new(store.clone(), test_config());
        let criteria = ForgetCriteria {
            ids: vec![id.clone()],
            ..Default::default()
        };
        let report = manager
            .execute(&criteria, &manual_options(false))
            .await
            .unwrap();

        assert!(report.purged.is_empty());
        assert_eq!(
            report.excluded,
            vec![(id.clone(), "must-protected".to_string())]
        );
        assert!(
            store.get(&id).await.is_ok(),
            "excluded candidate must survive"
        );
    }

    #[tokio::test]
    async fn test_auto_forget_noop_when_disabled() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut old_archived = Memory::new(
            MemoryType::Fact,
            "ancient archived fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        old_archived.namespace = "archived:global".to_string();
        old_archived.archived_at = Some(Utc::now() - Duration::days(365));
        let id = old_archived.id.clone();
        store.save(old_archived).await.unwrap();

        let manager = ForgetManager::new(store.clone(), test_config()); // auto_forget_enabled: false
        let report = manager.auto_forget().await.unwrap();

        assert!(report.purged.is_empty());
        assert!(
            store.get(&id).await.is_ok(),
            "disabled auto-forget must not purge anything"
        );
    }

    #[tokio::test]
    async fn test_auto_forget_respects_retention_days() {
        let store: Arc<dyn MemoryStore> = Arc::new(SqliteStore::in_memory().unwrap());

        let mut old_archived = Memory::new(
            MemoryType::Fact,
            "ancient archived fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        old_archived.namespace = "archived:global".to_string();
        old_archived.archived_at = Some(Utc::now() - Duration::days(200));
        let old_id = old_archived.id.clone();
        store.save(old_archived).await.unwrap();

        let mut recent_archived = Memory::new(
            MemoryType::Fact,
            "recently archived fact".to_string(),
            Priority::Reference,
            test_agent(),
        );
        recent_archived.namespace = "archived:global".to_string();
        recent_archived.archived_at = Some(Utc::now() - Duration::days(5));
        let recent_id = recent_archived.id.clone();
        store.save(recent_archived).await.unwrap();

        let mut config = test_config();
        config.auto_forget_enabled = true;
        config.archived_retention_days = 90;
        let manager = ForgetManager::new(store.clone(), config);

        let report = manager.auto_forget().await.unwrap();

        assert_eq!(report.purged, vec![old_id.clone()]);
        assert!(store.get(&old_id).await.is_err());
        assert!(store.get(&recent_id).await.is_ok());
    }
}
