use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;
use uuid::Uuid;

use crate::error::MemVaultError;
use crate::models::Priority;

type Result<T> = std::result::Result<T, MemVaultError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceStatus {
    Pending,
    Followed,
    Violated,
    Unknown,
}

impl std::fmt::Display for ComplianceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Followed => write!(f, "followed"),
            Self::Violated => write!(f, "violated"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for ComplianceStatus {
    type Err = MemVaultError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "followed" => Ok(Self::Followed),
            "violated" => Ok(Self::Violated),
            "unknown" => Ok(Self::Unknown),
            _ => Err(MemVaultError::Storage(format!(
                "invalid compliance status: {}",
                s
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceEvent {
    pub id: String,
    pub inject_session_id: String,
    /// Host-native session id (Codex/Claude Code `session_id`) used by the
    /// SessionEnd finalizer. `None` for MCP/REST callers that report through
    /// the returned `inject_session_id` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_session_id: Option<String>,
    pub memory_id: String,
    pub priority: Priority,
    pub status: ComplianceStatus,
    pub agent_id: String,
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
    pub reported_at: Option<DateTime<Utc>>,
    /// Structured reason behind the status — most important for `Unknown`:
    /// "agent never mentioned it" and "couldn't determine" are different
    /// facts and must not collapse into one null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceReport {
    pub inject_session_id: String,
    pub agent_id: String,
    pub total_injected: usize,
    pub must_followed: usize,
    pub must_violated: usize,
    pub ref_followed: usize,
    pub ref_violated: usize,
    pub pending: usize,
    /// `must_followed / (must_followed + must_violated)`.
    /// `None` when no MUST compliance verdict exists yet — an all-pending
    /// session must not be reported as 100% compliant.
    pub compliance_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateSummary {
    pub total_sessions: usize,
    /// Average of session rates that have actual MUST verdicts. `None` when
    /// every session is still pending/unknown.
    pub overall_rate: Option<f64>,
    /// Aggregate `followed / (followed + violated)` across all sessions,
    /// `None` when no MUST verdict exists.
    pub must_rate: Option<f64>,
    pub recent_sessions: Vec<ComplianceReport>,
}

/// Automatic judgment of whether an injected memory actually helped the task
/// it was injected for — a different axis from [`ComplianceStatus`], which is
/// a human report of "did the agent follow it". This is inferred later, from
/// a paired [`crate::episode::OutcomeInput`], independent of manual reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectivenessVerdict {
    Useful,
    Neutral,
    Harmful,
    InsufficientContext,
}

impl std::fmt::Display for EffectivenessVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Useful => write!(f, "useful"),
            Self::Neutral => write!(f, "neutral"),
            Self::Harmful => write!(f, "harmful"),
            Self::InsufficientContext => write!(f, "insufficient_context"),
        }
    }
}

impl std::str::FromStr for EffectivenessVerdict {
    type Err = MemVaultError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "useful" => Ok(Self::Useful),
            "neutral" => Ok(Self::Neutral),
            "harmful" => Ok(Self::Harmful),
            "insufficient_context" => Ok(Self::InsufficientContext),
            _ => Err(MemVaultError::Storage(format!(
                "invalid effectiveness verdict: {}",
                s
            ))),
        }
    }
}

/// A compliance event not yet judged for effectiveness — the candidate set
/// [`ComplianceStore::pending_since`] hands to a judge.
#[derive(Debug, Clone)]
pub struct PendingInjection {
    pub id: String,
    pub memory_id: String,
    pub priority: Priority,
}

/// A still-pending compliance row tied to a host-native session. The
/// SessionEnd finalizer needs both ids: `inject_session_id` selects the row
/// to report, `host_session_id` selects the batch.
#[derive(Debug, Clone)]
pub struct PendingComplianceEvent {
    pub id: String,
    pub inject_session_id: String,
    pub memory_id: String,
    pub priority: Priority,
}

/// Lifecycle of a durable SessionEnd compliance job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeJobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

impl std::fmt::Display for FinalizeJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl std::str::FromStr for FinalizeJobStatus {
    type Err = MemVaultError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(MemVaultError::Storage(format!(
                "invalid compliance finalize job status: {other}"
            ))),
        }
    }
}

/// One persisted SessionEnd compliance job. The hook only enqueues metadata;
/// transcript contents are read by the background worker so the hook never
/// performs I/O proportional to a multi-megabyte rollout.
#[derive(Debug, Clone)]
pub struct ComplianceFinalizeJob {
    pub id: i64,
    pub host_session_id: String,
    pub agent_id: String,
    pub transcript_path: Option<String>,
    pub transcript_tail: Option<String>,
    pub status: FinalizeJobStatus,
    pub attempts: u32,
    pub available_at: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Opaque token issued by the claim query. Heartbeat/complete/fail must
    /// present it so a worker whose lease was taken over cannot mutate the
    /// replacement worker's job.
    pub lease_token: Option<String>,
    /// Highest monotonic compliance-event sequence that existed when this
    /// worker claimed the job. Unlike SQLite's implicit `rowid`, this number
    /// is never reused after a delete, so a later event cannot slip into an
    /// older batch watermark. The worker only judges rows up to this point;
    /// a batch that arrives mid-flight is left for the next claim.
    pub batch_max_seq: i64,
}

/// Result of a verdict write fenced by a job lease. `LeaseLost` means the
/// job is no longer `running` under this token (typically reclaimed by a
/// newer worker), so the write did not happen and must not be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseWrite {
    Applied(usize),
    LeaseLost,
}

/// Identity of the worker that owns a finalize job. Passing one value keeps
/// the lease check attached to every write without a parallel token argument.
#[derive(Debug, Clone, Copy)]
pub struct JobLease<'a> {
    pub job_id: i64,
    pub lease_token: &'a str,
}

#[derive(Debug, Clone)]
pub struct FinalizeEnqueue {
    pub job_id: i64,
    /// True only when this call inserted a new row. Duplicate SessionEnd
    /// deliveries are idempotent and return the existing job instead.
    pub inserted: bool,
    pub status: FinalizeJobStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivenessSummary {
    pub useful: usize,
    pub neutral: usize,
    pub harmful: usize,
    pub insufficient: usize,
    pub unjudged: usize,
    /// `useful / (useful + neutral + harmful)` — `insufficient`/`unjudged`
    /// excluded: neither one is evidence the memory helped or hurt.
    pub usefulness_rate: f64,
    pub harmful_rate: f64,
    /// `(useful + neutral + harmful + insufficient) / total` — how much of
    /// the queried window has been judged at all, regardless of verdict.
    pub coverage: f64,
}

fn finalize_job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ComplianceFinalizeJob> {
    let status_str: String = row.get(5)?;
    let status = status_str.parse().map_err(|_| {
        rusqlite::Error::InvalidColumnType(5, "status".to_string(), rusqlite::types::Type::Text)
    })?;
    let attempts: i64 = row.get(6)?;
    Ok(ComplianceFinalizeJob {
        id: row.get(0)?,
        host_session_id: row.get(1)?,
        agent_id: row.get(2)?,
        transcript_path: row.get(3)?,
        transcript_tail: row.get(4)?,
        status,
        attempts: attempts.max(0) as u32,
        available_at: row.get(7)?,
        last_error: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        lease_token: row.get(11)?,
        batch_max_seq: row.get(12)?,
    })
}

pub struct ComplianceStore {
    conn: Mutex<Connection>,
}

impl ComplianceStore {
    pub fn new(db_path: &str) -> Result<Arc<Self>> {
        let mut conn =
            Connection::open(db_path).map_err(|e| MemVaultError::Storage(e.to_string()))?;
        // The short-lived SessionEnd hook and the long-lived HTTP worker open
        // separate connections to the same database. Wait for the writer lock
        // instead of failing the enqueue under normal WAL contention.
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS compliance_events (
                id                TEXT PRIMARY KEY,
                batch_seq         INTEGER NOT NULL DEFAULT 0,
                inject_session_id TEXT NOT NULL,
                host_session_id   TEXT,
                memory_id         TEXT NOT NULL,
                priority          TEXT NOT NULL,
                status            TEXT NOT NULL DEFAULT 'pending',
                agent_id          TEXT NOT NULL,
                evidence          TEXT,
                created_at        TEXT NOT NULL,
                reported_at       TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_compliance_session ON compliance_events(inject_session_id);
            CREATE INDEX IF NOT EXISTS idx_compliance_memory ON compliance_events(memory_id);
            CREATE INDEX IF NOT EXISTS idx_compliance_status ON compliance_events(status);
            CREATE INDEX IF NOT EXISTS idx_compliance_agent ON compliance_events(agent_id);

            CREATE TABLE IF NOT EXISTS compliance_finalize_jobs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                host_session_id TEXT NOT NULL UNIQUE,
                agent_id        TEXT NOT NULL,
                transcript_path TEXT,
                transcript_tail TEXT,
                status          TEXT NOT NULL DEFAULT 'queued',
                attempts        INTEGER NOT NULL DEFAULT 0,
                available_at    TEXT NOT NULL,
                last_error      TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                lease_token     TEXT,
                batch_max_seq   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_compliance_jobs_status
                ON compliance_finalize_jobs(status, available_at);

            CREATE TABLE IF NOT EXISTS compliance_event_seq (
                id       INTEGER PRIMARY KEY CHECK (id = 1),
                next_seq INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS compliance_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );"
        ).map_err(|e| MemVaultError::Storage(e.to_string()))?;

        // Databases created before the reason column existed need it added;
        // CREATE TABLE IF NOT EXISTS never alters an existing table.
        let has_reason: bool = conn
            .prepare("SELECT name FROM pragma_table_info('compliance_events')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "reason");
        if !has_reason {
            conn.execute("ALTER TABLE compliance_events ADD COLUMN reason TEXT", [])
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }

        // Same check-then-ALTER pattern for the effectiveness columns —
        // added after `reason`, so a database predating them needs the ALTER.
        let has_effectiveness: bool = conn
            .prepare("SELECT name FROM pragma_table_info('compliance_events')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "effectiveness");
        if !has_effectiveness {
            conn.execute_batch(
                "ALTER TABLE compliance_events ADD COLUMN effectiveness TEXT;
                 ALTER TABLE compliance_events ADD COLUMN effectiveness_reason TEXT;
                 ALTER TABLE compliance_events ADD COLUMN effectiveness_judged_at TEXT;",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }

        // Host-native session id lets the SessionEnd hook close the exact
        // injection batch it belongs to. Added after the effectiveness
        // columns; old databases need the ALTER + index backfill.
        let has_host_session_id: bool = conn
            .prepare("SELECT name FROM pragma_table_info('compliance_events')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "host_session_id");
        if !has_host_session_id {
            conn.execute_batch("ALTER TABLE compliance_events ADD COLUMN host_session_id TEXT;")
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }

        // Lease owner token: added for the worker-generation race where an
        // old worker resumes after its job was reclaimed by a newer one.
        let has_lease_token: bool = conn
            .prepare("SELECT name FROM pragma_table_info('compliance_finalize_jobs')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "lease_token");
        if !has_lease_token {
            conn.execute(
                "ALTER TABLE compliance_finalize_jobs ADD COLUMN lease_token TEXT",
                [],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }

        // Batch watermark migration is one immediate transaction. A crash
        // between adding `batch_max_seq` and copying the legacy value used to
        // leave the new column permanently zero because the next start only
        // checked whether the column existed. Keeping ALTER, backfill, and
        // sequence initialization atomic makes the upgrade restartable.
        let migration = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let has_batch_seq: bool = migration
            .prepare("SELECT name FROM pragma_table_info('compliance_events')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "batch_seq");
        if !has_batch_seq {
            migration
                .execute(
                    "ALTER TABLE compliance_events
                 ADD COLUMN batch_seq INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }
        // Backfill pre-sequence rows once. Their existing rowids are unique
        // among rows that still exist, and the sequence counter is then
        // raised above the maximum so future values can never repeat them.
        migration
            .execute(
                "UPDATE compliance_events SET batch_seq = rowid WHERE batch_seq = 0",
                [],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        migration
            .execute(
                "INSERT OR IGNORE INTO compliance_event_seq (id, next_seq) VALUES (1, 1)",
                [],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let has_batch_max_seq: bool = migration
            .prepare("SELECT name FROM pragma_table_info('compliance_finalize_jobs')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "batch_max_seq");
        if !has_batch_max_seq {
            migration
                .execute(
                    "ALTER TABLE compliance_finalize_jobs
                 ADD COLUMN batch_max_seq INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }
        let has_legacy_watermark: bool = migration
            .prepare("SELECT name FROM pragma_table_info('compliance_finalize_jobs')")
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .any(|name| name == "batch_max_rowid");
        // Backfill once, tracked by an explicit marker. "batch_max_seq = 0"
        // is also a legitimate watermark for a claimed batch with no pending
        // rows, so gating the recovery on that value alone re-inflated every
        // such job from the dead legacy column on each restart. The marker is
        // only written after the copy, so an interrupted old migration (column
        // added, copy not yet applied) still recovers on the next open.
        let legacy_watermark_migrated: bool = migration
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM compliance_meta WHERE key = 'batch_max_rowid_v1'
                 )",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        if has_legacy_watermark && !legacy_watermark_migrated {
            migration
                .execute(
                    "UPDATE compliance_finalize_jobs
                     SET batch_max_seq = batch_max_rowid
                     WHERE batch_max_seq = 0 AND batch_max_rowid > 0",
                    [],
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            migration
                .execute(
                    "INSERT OR REPLACE INTO compliance_meta (key, value)
                     VALUES ('batch_max_rowid_v1', '1')",
                    [],
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        }
        // A legacy job's `batch_max_rowid` can be higher than every surviving
        // event (its newest rows were deleted). New sequences must clear both
        // the surviving events and every migrated watermark, or the first
        // post-upgrade event could reuse the deleted rowid and fall into an
        // old claim.
        migration
            .execute(
                "UPDATE compliance_event_seq
             SET next_seq = MAX(
                 COALESCE((SELECT MAX(batch_seq) FROM compliance_events), 0),
                 COALESCE((SELECT MAX(batch_max_seq) FROM compliance_finalize_jobs), 0)
             ) + 1
             WHERE id = 1
               AND next_seq <= MAX(
                 COALESCE((SELECT MAX(batch_seq) FROM compliance_events), 0),
                 COALESCE((SELECT MAX(batch_max_seq) FROM compliance_finalize_jobs), 0)
               )",
                [],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        // Created after the ALTER above: an old table has no column at the
        // time the CREATE TABLE batch runs, so indexing it there would fail
        // before the migration ever got a chance to add it.
        migration
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_compliance_host_session
                 ON compliance_events(host_session_id);
             CREATE INDEX IF NOT EXISTS idx_compliance_batch
                 ON compliance_events(host_session_id, status, batch_seq);",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        migration
            .commit()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        crate::private_fs::restrict_file(std::path::Path::new(db_path));

        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
        }))
    }

    pub async fn record_injection(
        &self,
        inject_session_id: &str,
        memory_id: &str,
        priority: &Priority,
        agent_id: &str,
    ) -> Result<String> {
        self.record_injection_with_host_session(
            inject_session_id,
            memory_id,
            priority,
            agent_id,
            None,
        )
        .await
    }

    /// Like [`record_injection`], but also stores the host-native session id
    /// so a SessionEnd hook can finalize exactly this injection batch.
    pub async fn record_injection_with_host_session(
        &self,
        inject_session_id: &str,
        memory_id: &str,
        priority: &Priority,
        agent_id: &str,
        host_session_id: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let priority_str = serde_json::to_string(priority)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        let conn = self.conn.lock().await;
        // Allocate the batch sequence and insert the event in one SQLite
        // transaction. The counter only moves forward, so deleting an event
        // (including the newest one) can never make a later event reuse an
        // older batch watermark.
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let batch_seq: i64 = tx
            .query_row(
                "UPDATE compliance_event_seq
                 SET next_seq = next_seq + 1
                 WHERE id = 1
                 RETURNING next_seq - 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        tx.execute(
            "INSERT INTO compliance_events (id, batch_seq, inject_session_id, host_session_id, memory_id, priority, status, agent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
            rusqlite::params![
                id,
                batch_seq,
                inject_session_id,
                host_session_id,
                memory_id,
                priority_str,
                agent_id,
                now
            ],
        ).map_err(|e| MemVaultError::Storage(e.to_string()))?;
        tx.commit()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        debug!(
            id = %id,
            batch_seq,
            session = %inject_session_id,
            host_session = ?host_session_id,
            memory = %memory_id,
            "compliance event recorded"
        );
        Ok(id)
    }

    pub async fn report(
        &self,
        inject_session_id: &str,
        memory_id: &str,
        status: ComplianceStatus,
        evidence: Option<&str>,
    ) -> Result<()> {
        self.report_with_reason(inject_session_id, memory_id, status, evidence, None)
            .await
    }

    /// Like [`report`], with a structured reason attached. Required reading
    /// for `Unknown` outcomes: without it, "not mentioned" and "couldn't
    /// tell" are indistinguishable in hindsight.
    pub async fn report_with_reason(
        &self,
        inject_session_id: &str,
        memory_id: &str,
        status: ComplianceStatus,
        evidence: Option<&str>,
        reason: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let status_str = status.to_string();

        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_events SET status = ?1, evidence = ?2, reported_at = ?3, reason = ?4
             WHERE inject_session_id = ?5 AND memory_id = ?6",
                rusqlite::params![status_str, evidence, now, reason, inject_session_id, memory_id],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        if affected == 0 {
            return Err(MemVaultError::Storage(format!(
                "no compliance event found for session={}, memory={}",
                inject_session_id, memory_id
            )));
        }
        Ok(())
    }

    pub async fn get_report(&self, inject_session_id: &str) -> Result<ComplianceReport> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT priority, status, agent_id FROM compliance_events WHERE inject_session_id = ?1"
        ).map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let rows: Vec<(String, String, String)> = stmt
            .query_map([inject_session_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        if rows.is_empty() {
            return Ok(ComplianceReport {
                inject_session_id: inject_session_id.to_string(),
                agent_id: String::new(),
                total_injected: 0,
                must_followed: 0,
                must_violated: 0,
                ref_followed: 0,
                ref_violated: 0,
                pending: 0,
                compliance_rate: None,
            });
        }

        let agent_id = rows[0].2.clone();
        let mut must_followed = 0usize;
        let mut must_violated = 0usize;
        let mut ref_followed = 0usize;
        let mut ref_violated = 0usize;
        let mut pending = 0usize;

        for (priority, status, _) in &rows {
            let is_must = priority == "MUST";
            match status.as_str() {
                "followed" => {
                    if is_must {
                        must_followed += 1;
                    } else {
                        ref_followed += 1;
                    }
                }
                "violated" => {
                    if is_must {
                        must_violated += 1;
                    } else {
                        ref_violated += 1;
                    }
                }
                "pending" => pending += 1,
                _ => {}
            }
        }

        let must_total = must_followed + must_violated;
        let compliance_rate = if must_total > 0 {
            Some(must_followed as f64 / must_total as f64)
        } else {
            None
        };

        Ok(ComplianceReport {
            inject_session_id: inject_session_id.to_string(),
            agent_id,
            total_injected: rows.len(),
            must_followed,
            must_violated,
            ref_followed,
            ref_violated,
            pending,
            compliance_rate,
        })
    }

    pub async fn get_summary(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<AggregateSummary> {
        let conn = self.conn.lock().await;

        let query = if agent_id.is_some() {
            "SELECT DISTINCT inject_session_id FROM compliance_events WHERE agent_id = ?1 ORDER BY created_at DESC LIMIT ?2"
        } else {
            "SELECT DISTINCT inject_session_id FROM compliance_events ORDER BY created_at DESC LIMIT ?2"
        };

        let session_ids: Vec<String> = if let Some(aid) = agent_id {
            let mut stmt = conn
                .prepare(query)
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            stmt.query_map(rusqlite::params![aid, limit as i64], |row| row.get(0))
                .map_err(|e| MemVaultError::Storage(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            let mut stmt = conn.prepare("SELECT DISTINCT inject_session_id FROM compliance_events ORDER BY created_at DESC LIMIT ?1")
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            stmt.query_map([limit as i64], |row| row.get(0))
                .map_err(|e| MemVaultError::Storage(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect()
        };

        drop(conn);

        let mut reports = Vec::new();
        for sid in &session_ids {
            reports.push(self.get_report(sid).await?);
        }

        let total_sessions = reports.len();
        let (mut total_must_f, mut total_must_v) = (0usize, 0usize);
        let mut rate_sum = 0.0f64;
        let mut rated_sessions = 0usize;

        for r in &reports {
            total_must_f += r.must_followed;
            total_must_v += r.must_violated;
            if let Some(rate) = r.compliance_rate {
                rate_sum += rate;
                rated_sessions += 1;
            }
        }

        let must_total = total_must_f + total_must_v;
        let must_rate = if must_total > 0 {
            Some(total_must_f as f64 / must_total as f64)
        } else {
            None
        };
        let overall_rate = if rated_sessions > 0 {
            Some(rate_sum / rated_sessions as f64)
        } else {
            None
        };

        Ok(AggregateSummary {
            total_sessions,
            overall_rate,
            must_rate,
            recent_sessions: reports,
        })
    }

    /// Pending compliance rows for one host-native session, oldest first.
    /// The SessionEnd finalizer uses this to judge exactly the memories that
    /// were injected at this session's start.
    pub async fn pending_compliance_for_host_session(
        &self,
        host_session_id: &str,
    ) -> Result<Vec<PendingComplianceEvent>> {
        self.pending_compliance_for_host_session_upto(host_session_id, i64::MAX)
            .await
    }

    /// Like [`pending_compliance_for_host_session`], but only rows that
    /// existed at or before `max_seq`. A claimed job snapshots this
    /// watermark so a batch arriving mid-flight is not judged against the
    /// previous batch's transcript, and a deleted/newer rowid cannot be
    /// mistaken for part of the claimed batch.
    pub async fn pending_compliance_for_host_session_upto(
        &self,
        host_session_id: &str,
        max_seq: i64,
    ) -> Result<Vec<PendingComplianceEvent>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, inject_session_id, memory_id, priority
                 FROM compliance_events
                 WHERE host_session_id = ?1 AND status = 'pending' AND batch_seq <= ?2
                 ORDER BY batch_seq ASC",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![host_session_id, max_seq], |row| {
                let priority_str: String = row.get(3)?;
                let priority = serde_json::from_str::<Priority>(&format!("\"{priority_str}\""))
                    .unwrap_or(Priority::Reference);
                Ok(PendingComplianceEvent {
                    id: row.get(0)?,
                    inject_session_id: row.get(1)?,
                    memory_id: row.get(2)?,
                    priority,
                })
            })
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Mark every still-pending row for a host session with one verdict.
    /// Used when no compliance judge exists: recording `unknown` is honest
    /// and stops the backlog from growing forever.
    pub async fn finalize_pending_for_host_session(
        &self,
        host_session_id: &str,
        status: ComplianceStatus,
        evidence: Option<&str>,
        reason: Option<&str>,
    ) -> Result<usize> {
        self.finalize_pending_for_host_session_upto(
            host_session_id,
            i64::MAX,
            status,
            evidence,
            reason,
        )
        .await
    }

    /// Like [`finalize_pending_for_host_session`], but leaves rows newer than
    /// `max_seq` pending. Used by a worker whose fallback/verdict applies
    /// only to the batch it actually claimed.
    pub async fn finalize_pending_for_host_session_upto(
        &self,
        host_session_id: &str,
        max_seq: i64,
        status: ComplianceStatus,
        evidence: Option<&str>,
        reason: Option<&str>,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let status_str = status.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE compliance_events
             SET status = ?1, evidence = ?2, reported_at = ?3, reason = ?4
             WHERE host_session_id = ?5 AND status = 'pending' AND batch_seq <= ?6",
            rusqlite::params![status_str, evidence, now, reason, host_session_id, max_seq],
        )
        .map_err(|e| MemVaultError::Storage(e.to_string()))
    }

    /// Whether this worker still owns `job_id` under `lease_token`.
    pub async fn lease_is_active(&self, job_id: i64, lease_token: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        Self::lease_is_active_locked(&conn, job_id, lease_token)
    }

    fn lease_is_active_locked(conn: &Connection, job_id: i64, lease_token: &str) -> Result<bool> {
        let active: bool = conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM compliance_finalize_jobs
                     WHERE id = ?1 AND status = 'running' AND lease_token = ?2
                 )",
                rusqlite::params![job_id, lease_token],
                |row| row.get(0),
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Ok(active)
    }

    /// Record a verdict only while the caller still owns the job lease.
    /// The lease check and the update are one SQL statement, so a worker
    /// that resumes after its job was reclaimed cannot overwrite the new
    /// owner's pending rows.
    pub async fn report_with_reason_leased(
        &self,
        lease: JobLease<'_>,
        inject_session_id: &str,
        memory_id: &str,
        status: ComplianceStatus,
        evidence: Option<&str>,
        reason: Option<&str>,
    ) -> Result<LeaseWrite> {
        let now = Utc::now().to_rfc3339();
        let status_str = status.to_string();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_events
                 SET status = ?1, evidence = ?2, reported_at = ?3, reason = ?4
                 WHERE inject_session_id = ?5 AND memory_id = ?6
                   AND EXISTS(
                       SELECT 1 FROM compliance_finalize_jobs
                       WHERE id = ?7 AND status = 'running' AND lease_token = ?8
                   )",
                rusqlite::params![
                    status_str,
                    evidence,
                    now,
                    reason,
                    inject_session_id,
                    memory_id,
                    lease.job_id,
                    lease.lease_token
                ],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        if affected > 0 {
            return Ok(LeaseWrite::Applied(affected));
        }
        if !Self::lease_is_active_locked(&conn, lease.job_id, lease.lease_token)? {
            return Ok(LeaseWrite::LeaseLost);
        }
        Err(MemVaultError::Storage(format!(
            "no compliance event found for session={}, memory={}",
            inject_session_id, memory_id
        )))
    }

    /// Lease-fenced [`finalize_pending_for_host_session_upto`]. Returns
    /// `LeaseLost` without touching any row when the job was taken over.
    pub async fn finalize_pending_for_host_session_upto_leased(
        &self,
        lease: JobLease<'_>,
        host_session_id: &str,
        max_seq: i64,
        status: ComplianceStatus,
        evidence: Option<&str>,
        reason: Option<&str>,
    ) -> Result<LeaseWrite> {
        let now = Utc::now().to_rfc3339();
        let status_str = status.to_string();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_events
                 SET status = ?1, evidence = ?2, reported_at = ?3, reason = ?4
                 WHERE host_session_id = ?5 AND status = 'pending' AND batch_seq <= ?6
                   AND EXISTS(
                       SELECT 1 FROM compliance_finalize_jobs
                       WHERE id = ?7 AND status = 'running' AND lease_token = ?8
                   )",
                rusqlite::params![
                    status_str,
                    evidence,
                    now,
                    reason,
                    host_session_id,
                    max_seq,
                    lease.job_id,
                    lease.lease_token
                ],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        if affected > 0 {
            return Ok(LeaseWrite::Applied(affected));
        }
        if Self::lease_is_active_locked(&conn, lease.job_id, lease.lease_token)? {
            Ok(LeaseWrite::Applied(0))
        } else {
            Ok(LeaseWrite::LeaseLost)
        }
    }

    /// Finalize pending rows older than `before` across every host session.
    /// Historical rows predate `host_session_id`, so this is the one-time
    /// migration path to close an existing backlog.
    pub async fn finalize_stale_pending(
        &self,
        before: DateTime<Utc>,
        status: ComplianceStatus,
        reason: Option<&str>,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let status_str = status.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE compliance_events
             SET status = ?1, reported_at = ?2, reason = ?3
             WHERE status = 'pending' AND created_at < ?4",
            rusqlite::params![status_str, now, reason, before.to_rfc3339()],
        )
        .map_err(|e| MemVaultError::Storage(e.to_string()))
    }

    /// Persist a SessionEnd job without doing any LLM work. Duplicate
    /// deliveries for the same host session are idempotent; a previously
    /// failed job is put back on the queue, queued jobs only refresh their
    /// transcript, and running jobs are left alone. A `done` job is reopened
    /// when that host session has accumulated NEW pending compliance rows
    /// (the host resumed and produced another injection batch) — dedup is on
    /// the outstanding work, not on the host session id forever. With no new
    /// pending rows a duplicate SessionEnd stays `done`, preserving
    /// idempotency.
    pub async fn enqueue_finalize_job(
        &self,
        host_session_id: &str,
        agent_id: &str,
        transcript_path: Option<&str>,
        transcript_tail: Option<&str>,
    ) -> Result<FinalizeEnqueue> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO compliance_finalize_jobs
                    (host_session_id, agent_id, transcript_path, transcript_tail,
                     status, attempts, available_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?5, ?5, ?5)",
                rusqlite::params![
                    host_session_id,
                    agent_id,
                    transcript_path,
                    transcript_tail,
                    now
                ],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            > 0;

        let (job_id, status_str): (i64, String) = conn
            .query_row(
                "SELECT id, status FROM compliance_finalize_jobs WHERE host_session_id = ?1",
                [host_session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        let mut status: FinalizeJobStatus = status_str.parse()?;

        if !inserted && status == FinalizeJobStatus::Failed {
            conn.execute(
                "UPDATE compliance_finalize_jobs
                 SET status = 'queued', attempts = 0, available_at = ?2,
                     last_error = NULL, lease_token = NULL, updated_at = ?2
                 WHERE id = ?1 AND status = 'failed'",
                rusqlite::params![job_id, now],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            status = FinalizeJobStatus::Queued;
        } else if !inserted && status == FinalizeJobStatus::Queued {
            // A retry from the host can carry a newer transcript path. Do not
            // overwrite a non-null value with null.
            conn.execute(
                "UPDATE compliance_finalize_jobs
                 SET agent_id = ?2,
                     transcript_path = COALESCE(?3, transcript_path),
                     transcript_tail = COALESCE(?4, transcript_tail),
                     lease_token = NULL, updated_at = ?5
                 WHERE id = ?1 AND status = 'queued'",
                rusqlite::params![job_id, agent_id, transcript_path, transcript_tail, now],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        } else if !inserted && status == FinalizeJobStatus::Running {
            // A batch can arrive while the previous batch is still being
            // judged. The running worker cannot see this job-row update, but
            // the next claim must use the newer transcript. Completion below
            // will atomically turn the job back into `queued` if pending
            // rows still exist, so this update cannot be lost.
            conn.execute(
                "UPDATE compliance_finalize_jobs
                 SET agent_id = ?2,
                     transcript_path = COALESCE(?3, transcript_path),
                     transcript_tail = COALESCE(?4, transcript_tail),
                     updated_at = ?5
                 WHERE id = ?1 AND status = 'running'
                   AND EXISTS (
                       SELECT 1 FROM compliance_events
                       WHERE host_session_id = ?6 AND status = 'pending'
                   )",
                rusqlite::params![
                    job_id,
                    agent_id,
                    transcript_path,
                    transcript_tail,
                    now,
                    host_session_id
                ],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        } else if !inserted && status == FinalizeJobStatus::Done {
            // A resumed host session can inject a second batch after the
            // first SessionEnd job completed. Reopen the job only when such
            // pending work actually exists; duplicate SessionEnd deliveries
            // with nothing new pending stay done (idempotent retry).
            let has_pending: bool = conn
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM compliance_events
                         WHERE host_session_id = ?1 AND status = 'pending'
                     )",
                    [host_session_id],
                    |row| row.get(0),
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            if has_pending {
                conn.execute(
                    "UPDATE compliance_finalize_jobs
                     SET status = 'queued', attempts = 0, available_at = ?2,
                         last_error = NULL, lease_token = NULL, agent_id = ?3,
                         transcript_path = COALESCE(?4, transcript_path),
                         transcript_tail = COALESCE(?5, transcript_tail),
                         updated_at = ?2
                     WHERE id = ?1 AND status = 'done'",
                    rusqlite::params![job_id, now, agent_id, transcript_path, transcript_tail],
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
                status = FinalizeJobStatus::Queued;
            }
        }

        Ok(FinalizeEnqueue {
            job_id,
            inserted,
            status,
        })
    }

    /// Atomically claim the oldest due job. `running` jobs are excluded, so
    /// two server processes (or a restart race) cannot judge the same job.
    pub async fn claim_next_finalize_job(&self) -> Result<Option<ComplianceFinalizeJob>> {
        let now = Utc::now().to_rfc3339();
        let lease_token = Uuid::new_v4().to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "UPDATE compliance_finalize_jobs
                 SET status = 'running', attempts = attempts + 1,
                     lease_token = ?2,
                     batch_max_seq = COALESCE((
                         SELECT MAX(batch_seq) FROM compliance_events
                         WHERE compliance_events.host_session_id =
                               compliance_finalize_jobs.host_session_id
                           AND compliance_events.status = 'pending'
                     ), 0),
                     updated_at = ?1
                 WHERE id = (
                     SELECT id FROM compliance_finalize_jobs
                     WHERE status = 'queued' AND available_at <= ?1
                     ORDER BY created_at ASC, id ASC
                     LIMIT 1
                 )
                 RETURNING id, host_session_id, agent_id, transcript_path, transcript_tail,
                           status, attempts, available_at, last_error, created_at, updated_at,
                           lease_token, batch_max_seq",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        stmt.query_row(rusqlite::params![now, lease_token], finalize_job_from_row)
            .optional()
            .map_err(|e| MemVaultError::Storage(e.to_string()))
    }

    /// Extend the lease on a job this worker is still actively processing.
    /// The stale-running reaper uses `updated_at` as the lease timestamp, so
    /// without a heartbeat a legitimate job slower than the stale threshold
    /// would be requeued mid-flight and judged twice. Returns `false` when
    /// the job is no longer `running` (for example, it was reclaimed by a
    /// different worker after this worker paused for too long).
    pub async fn heartbeat_finalize_job(&self, job_id: i64, lease_token: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_finalize_jobs
                 SET updated_at = ?2
                 WHERE id = ?1 AND status = 'running' AND lease_token = ?3",
                rusqlite::params![job_id, now, lease_token],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Mark a job complete. `note` is diagnostic only (for example, the job
    /// timed out and its remaining rows were finalized as unknown).
    ///
    /// This is also the final race barrier: if a new pending batch arrived
    /// while the worker was judging, the job becomes `queued` instead of
    /// `done`. Returns the resulting status, or `None` when the lease token
    /// no longer owns the job.
    pub async fn complete_finalize_job(
        &self,
        job_id: i64,
        lease_token: &str,
        note: Option<&str>,
    ) -> Result<Option<FinalizeJobStatus>> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let status: Option<String> = conn
            .query_row(
                "UPDATE compliance_finalize_jobs
                 SET status = CASE WHEN EXISTS(
                         SELECT 1 FROM compliance_events
                         WHERE compliance_events.host_session_id =
                               compliance_finalize_jobs.host_session_id
                           AND compliance_events.status = 'pending'
                     ) THEN 'queued' ELSE 'done' END,
                     attempts = CASE WHEN EXISTS(
                         SELECT 1 FROM compliance_events
                         WHERE compliance_events.host_session_id =
                               compliance_finalize_jobs.host_session_id
                           AND compliance_events.status = 'pending'
                     ) THEN 0 ELSE attempts END,
                     available_at = CASE WHEN EXISTS(
                         SELECT 1 FROM compliance_events
                         WHERE compliance_events.host_session_id =
                               compliance_finalize_jobs.host_session_id
                           AND compliance_events.status = 'pending'
                     ) THEN ?3 ELSE available_at END,
                     lease_token = NULL,
                     last_error = ?2,
                     updated_at = ?3
                 WHERE id = ?1 AND status = 'running' AND lease_token = ?4
                 RETURNING status",
                rusqlite::params![job_id, note, now, lease_token],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        match status {
            Some(status) => Ok(Some(status.parse()?)),
            None => Ok(None),
        }
    }

    /// Mark a job failed after it could not be finalized even via the
    /// conservative unknown fallback. Keeping the row (instead of deleting
    /// it) preserves the audit trail for doctor/backlog inspection.
    pub async fn fail_finalize_job(
        &self,
        job_id: i64,
        lease_token: &str,
        error: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_finalize_jobs
                 SET status = 'failed', last_error = ?2, lease_token = NULL,
                     updated_at = ?3
                 WHERE id = ?1 AND status = 'running' AND lease_token = ?4",
                rusqlite::params![job_id, error, now, lease_token],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Return jobs left in `running` by a crashed worker to the queue.
    pub async fn reap_stale_finalize_jobs(&self, older_than: chrono::Duration) -> Result<usize> {
        let now = Utc::now();
        let cutoff = (now - older_than).to_rfc3339();
        let now = now.to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE compliance_finalize_jobs
             SET status = 'queued', available_at = ?1, updated_at = ?1,
                 last_error = 'worker stopped while job was running; requeued',
                 lease_token = NULL
             WHERE status = 'running' AND updated_at < ?2",
            rusqlite::params![now, cutoff],
        )
        .map_err(|e| MemVaultError::Storage(e.to_string()))
    }

    /// Safety net for rows that no `SessionEnd` ever reported: close pending
    /// rows older than `older_than` as `unknown`.
    ///
    /// `SessionEnd` covers hook-driven sessions, but injections recorded by
    /// MCP/REST callers (or hosts without a session-end event) can still be
    /// left pending forever — the exact shape that used to make the aggregate
    /// report look vacuously perfect. Called from `SessionStart`, so the
    /// backlog can never grow without bound.
    pub async fn sweep_stale_pending(&self, older_than: chrono::Duration) -> Result<usize> {
        self.finalize_stale_pending(
            Utc::now() - older_than,
            ComplianceStatus::Unknown,
            Some("stale pending sweep at session start; no compliance report arrived"),
        )
        .await
    }

    /// Injections for `agent_id` not yet judged for effectiveness, created at
    /// or after `since`, most recent first. This — not `inject_session_id` —
    /// is the join key for automatic judging: the caller (`record_outcome`)
    /// knows which agent it is, not which session injected what.
    pub async fn pending_since(
        &self,
        agent_id: &str,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PendingInjection>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, memory_id, priority FROM compliance_events
                 WHERE agent_id = ?1 AND effectiveness IS NULL AND created_at >= ?2
                 ORDER BY created_at DESC LIMIT ?3",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let rows = stmt
            .query_map(
                rusqlite::params![agent_id, since.to_rfc3339(), limit as i64],
                |row| {
                    let priority_str: String = row.get(2)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        priority_str,
                    ))
                },
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, memory_id, priority_str)| {
                serde_json::from_str::<Priority>(&format!("\"{priority_str}\""))
                    .ok()
                    .map(|priority| PendingInjection {
                        id,
                        memory_id,
                        priority,
                    })
            })
            .collect();

        Ok(rows)
    }

    /// How many times `memory_id` has been judged "harmful" since `since`.
    /// Feeds `crate::decay::DecayManager`'s harmful-verdict acceleration —
    /// repeated, LLM-judged real-world harm should compound with
    /// contradiction evidence instead of only ever being logged.
    pub async fn harmful_count_for_memory(
        &self,
        memory_id: &str,
        since: DateTime<Utc>,
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let verdict = EffectivenessVerdict::Harmful.to_string();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM compliance_events
                 WHERE memory_id = ?1 AND effectiveness = ?2 AND effectiveness_judged_at >= ?3",
                rusqlite::params![memory_id, verdict, since.to_rfc3339()],
                |row| row.get(0),
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Ok(count as usize)
    }

    /// Permanently delete every `compliance_events` row for `memory_id` —
    /// part of GDPR-style erasure (`forget::ForgetManager::execute`).
    /// `compliance_events` has no foreign key to `memories(id)`, so a
    /// normal `delete()`/decay archive never cleans these up; this is the
    /// only path that does. Returns the number of rows removed.
    pub async fn purge_memory(&self, memory_id: &str) -> Result<usize> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "DELETE FROM compliance_events WHERE memory_id = ?1",
                rusqlite::params![memory_id],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;
        Ok(affected)
    }

    /// Record an automatic effectiveness judgment by primary key `id`
    /// (`PendingInjection::id`, not `inject_session_id`+`memory_id` — a
    /// single row is targeted here, not "every memory in a session").
    pub async fn record_effectiveness(
        &self,
        id: &str,
        verdict: EffectivenessVerdict,
        reason: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let verdict_str = verdict.to_string();

        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE compliance_events
                 SET effectiveness = ?1, effectiveness_reason = ?2, effectiveness_judged_at = ?3
                 WHERE id = ?4",
                rusqlite::params![verdict_str, reason, now, id],
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        if affected == 0 {
            return Err(MemVaultError::Storage(format!(
                "no compliance event found with id={}",
                id
            )));
        }
        Ok(())
    }

    /// Effectiveness rates over the most recent `limit` compliance events
    /// (optionally scoped to one agent) — formulas mirror the memory-eval
    /// framework this closes the loop for: usefulness/harmful rate exclude
    /// `insufficient`/`unjudged`, `coverage` measures how much of the window
    /// has been judged at all regardless of verdict.
    pub async fn get_effectiveness_summary(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<EffectivenessSummary> {
        let conn = self.conn.lock().await;

        let rows: Vec<Option<String>> = if let Some(aid) = agent_id {
            let mut stmt = conn
                .prepare(
                    "SELECT effectiveness FROM compliance_events WHERE agent_id = ?1
                     ORDER BY created_at DESC LIMIT ?2",
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            stmt.query_map(rusqlite::params![aid, limit as i64], |row| row.get(0))
                .map_err(|e| MemVaultError::Storage(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT effectiveness FROM compliance_events
                     ORDER BY created_at DESC LIMIT ?1",
                )
                .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            stmt.query_map([limit as i64], |row| row.get(0))
                .map_err(|e| MemVaultError::Storage(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let total = rows.len();
        let (mut useful, mut neutral, mut harmful, mut insufficient, mut unjudged) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for verdict in &rows {
            match verdict.as_deref() {
                Some("useful") => useful += 1,
                Some("neutral") => neutral += 1,
                Some("harmful") => harmful += 1,
                Some("insufficient_context") => insufficient += 1,
                _ => unjudged += 1,
            }
        }

        let decided = useful + neutral + harmful;
        let usefulness_rate = if decided > 0 {
            useful as f64 / decided as f64
        } else {
            0.0
        };
        let harmful_rate = if decided > 0 {
            harmful as f64 / decided as f64
        } else {
            0.0
        };
        let coverage = if total > 0 {
            (useful + neutral + harmful + insufficient) as f64 / total as f64
        } else {
            1.0
        };

        Ok(EffectivenessSummary {
            useful,
            neutral,
            harmful,
            insufficient,
            unjudged,
            usefulness_rate,
            harmful_rate,
            coverage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finalize_jobs_are_idempotent_and_claimed_once() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let first = store
            .enqueue_finalize_job("host-1", "codex", Some("/tmp/rollout.jsonl"), None)
            .await
            .unwrap();
        assert!(first.inserted);
        assert_eq!(first.status, FinalizeJobStatus::Queued);

        let duplicate = store
            .enqueue_finalize_job("host-1", "codex", Some("/tmp/newer.jsonl"), None)
            .await
            .unwrap();
        assert!(!duplicate.inserted);
        assert_eq!(duplicate.job_id, first.job_id);

        let claimed = store
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("job must be claimable");
        assert_eq!(claimed.host_session_id, "host-1");
        assert_eq!(claimed.transcript_path.as_deref(), Some("/tmp/newer.jsonl"));
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.status, FinalizeJobStatus::Running);
        assert!(
            store.claim_next_finalize_job().await.unwrap().is_none(),
            "a running job must not be claimed twice"
        );

        store
            .complete_finalize_job(claimed.id, claimed.lease_token.as_deref().unwrap(), None)
            .await
            .unwrap();
        let after_done = store
            .enqueue_finalize_job("host-1", "codex", None, None)
            .await
            .unwrap();
        assert!(!after_done.inserted);
        assert_eq!(after_done.status, FinalizeJobStatus::Done);
    }

    /// Regression: a resumed host session can produce a NEW injection batch
    /// after its first SessionEnd job is `done`. Dedup on the host session
    /// id alone used to leave those rows pending forever while reporting
    /// "queued". Reopen the job when pending work exists, but keep duplicate
    /// SessionEnd deliveries with nothing new idempotent.
    #[tokio::test]
    async fn done_finalize_job_is_reopened_by_a_new_pending_batch() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let first = store
            .enqueue_finalize_job("host-resume", "codex", None, None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        store
            .complete_finalize_job(claimed.id, claimed.lease_token.as_deref().unwrap(), None)
            .await
            .unwrap();

        // Nothing new pending: a duplicate SessionEnd stays done.
        let duplicate = store
            .enqueue_finalize_job("host-resume", "codex", None, None)
            .await
            .unwrap();
        assert_eq!(duplicate.job_id, first.job_id);
        assert!(!duplicate.inserted);
        assert_eq!(duplicate.status, FinalizeJobStatus::Done);

        // The host resumed and injected a second batch.
        store
            .record_injection_with_host_session(
                "inject-2",
                "mem-2",
                &Priority::Reference,
                "codex",
                Some("host-resume"),
            )
            .await
            .unwrap();
        let reopened = store
            .enqueue_finalize_job("host-resume", "codex", Some("/tmp/new-rollout.jsonl"), None)
            .await
            .unwrap();
        assert_eq!(reopened.job_id, first.job_id);
        assert!(!reopened.inserted);
        assert_eq!(
            reopened.status,
            FinalizeJobStatus::Queued,
            "a new pending batch must reopen the completed job"
        );

        let claimed_again = store.claim_next_finalize_job().await.unwrap().unwrap();
        assert_eq!(
            claimed_again.attempts, 1,
            "a new generation starts with a clean attempt count"
        );
        assert_eq!(
            claimed_again.transcript_path.as_deref(),
            Some("/tmp/new-rollout.jsonl"),
            "the reopened job must adopt the newer transcript"
        );

        store
            .finalize_pending_for_host_session(
                "host-resume",
                ComplianceStatus::Unknown,
                None,
                Some("test batch closed"),
            )
            .await
            .unwrap();
        store
            .complete_finalize_job(
                claimed_again.id,
                claimed_again.lease_token.as_deref().unwrap(),
                None,
            )
            .await
            .unwrap();
        let after_second = store
            .enqueue_finalize_job("host-resume", "codex", None, None)
            .await
            .unwrap();
        assert_eq!(after_second.status, FinalizeJobStatus::Done);
    }

    #[tokio::test]
    async fn failed_finalize_job_is_requeued_by_a_duplicate_session_end() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let queued = store
            .enqueue_finalize_job("host-retry", "codex", None, None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        store
            .fail_finalize_job(
                claimed.id,
                claimed.lease_token.as_deref().unwrap(),
                "temporary storage failure",
            )
            .await
            .unwrap();

        let retried = store
            .enqueue_finalize_job("host-retry", "codex", None, None)
            .await
            .unwrap();
        assert_eq!(retried.job_id, queued.job_id);
        assert!(!retried.inserted);
        assert_eq!(retried.status, FinalizeJobStatus::Queued);
        assert_eq!(
            store
                .claim_next_finalize_job()
                .await
                .unwrap()
                .unwrap()
                .attempts,
            1,
            "requeue should reset attempts"
        );
    }

    #[tokio::test]
    async fn finalize_job_survives_store_reopen() {
        let path = std::env::temp_dir().join(format!(
            "memvault-compliance-jobs-{}.db",
            uuid::Uuid::new_v4()
        ));
        {
            let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
            store
                .enqueue_finalize_job("host-persistent", "codex", Some("/tmp/r.jsonl"), None)
                .await
                .unwrap();
        }
        let reopened = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        let job = reopened
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("queued job must survive process restart");
        assert_eq!(job.host_session_id, "host-persistent");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn stale_running_finalize_job_is_reaped() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .enqueue_finalize_job("host-crash", "codex", None, None)
            .await
            .unwrap();
        assert!(store.claim_next_finalize_job().await.unwrap().is_some());
        tokio::time::sleep(Duration::from_millis(2)).await;

        assert_eq!(
            store
                .reap_stale_finalize_jobs(chrono::Duration::zero())
                .await
                .unwrap(),
            1
        );
        let reclaimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        assert_eq!(reclaimed.host_session_id, "host-crash");
        assert_eq!(reclaimed.attempts, 2);
    }

    /// A new batch can arrive while the previous batch is still running.
    /// Completion must not report `done` while any pending row remains;
    /// otherwise the final SessionEnd is lost and the job row says there is
    /// nothing left to do.
    #[tokio::test]
    async fn running_job_with_new_pending_batch_is_requeued_at_completion() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .enqueue_finalize_job("host-race", "codex", Some("/tmp/first.jsonl"), None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        let lease_token = claimed.lease_token.clone().unwrap();

        store
            .record_injection_with_host_session(
                "inject-race",
                "mem-race",
                &Priority::Must,
                "codex",
                Some("host-race"),
            )
            .await
            .unwrap();
        let enqueued = store
            .enqueue_finalize_job("host-race", "codex", Some("/tmp/second.jsonl"), None)
            .await
            .unwrap();
        assert_eq!(enqueued.status, FinalizeJobStatus::Running);

        assert_eq!(
            store
                .complete_finalize_job(claimed.id, &lease_token, Some("first batch done"))
                .await
                .unwrap(),
            Some(FinalizeJobStatus::Queued),
            "completion must requeue while the second batch is still pending"
        );

        let next = store
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("the requeued job must be claimable");
        assert_eq!(next.host_session_id, "host-race");
        assert_eq!(
            next.transcript_path.as_deref(),
            Some("/tmp/second.jsonl"),
            "the next claim must use the newer batch's transcript"
        );
        assert_eq!(next.attempts, 1, "a new batch starts a fresh attempt count");
        assert!(next.batch_max_seq > claimed.batch_max_seq);
    }

    /// The worker snapshots the highest event row id at claim time. Rows
    /// inserted afterwards belong to the next batch and must not be judged
    /// against the old transcript or closed by the old worker's fallback.
    #[tokio::test]
    async fn claim_watermark_excludes_a_later_batch() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection_with_host_session(
                "inject-batch-1",
                "mem-batch-1",
                &Priority::Must,
                "codex",
                Some("host-watermark"),
            )
            .await
            .unwrap();
        store
            .enqueue_finalize_job("host-watermark", "codex", Some("/tmp/batch-1.jsonl"), None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();

        store
            .record_injection_with_host_session(
                "inject-batch-2",
                "mem-batch-2",
                &Priority::Must,
                "codex",
                Some("host-watermark"),
            )
            .await
            .unwrap();

        let snapshot = store
            .pending_compliance_for_host_session_upto("host-watermark", claimed.batch_max_seq)
            .await
            .unwrap();
        assert_eq!(snapshot.len(), 1, "only the claimed batch is visible");
        assert_eq!(snapshot[0].inject_session_id, "inject-batch-1");
        assert_eq!(
            store
                .pending_compliance_for_host_session("host-watermark")
                .await
                .unwrap()
                .len(),
            2
        );

        let closed = store
            .finalize_pending_for_host_session_upto(
                "host-watermark",
                claimed.batch_max_seq,
                ComplianceStatus::Unknown,
                None,
                Some("old batch closed"),
            )
            .await
            .unwrap();
        assert_eq!(closed, 1);
        let remaining = store
            .pending_compliance_for_host_session("host-watermark")
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].inject_session_id, "inject-batch-2");
    }

    /// SQLite may reuse the highest `rowid` after that row is deleted. The
    /// batch watermark must use a monotonic sequence instead, or a brand-new
    /// event could be swept into a claim made before it existed.
    #[tokio::test]
    async fn deleted_event_sequence_is_not_reused_for_a_later_batch() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let first = store
            .record_injection_with_host_session(
                "inject-seq-1",
                "mem-seq-1",
                &Priority::Must,
                "codex",
                Some("host-seq"),
            )
            .await
            .unwrap();
        store
            .enqueue_finalize_job("host-seq", "codex", Some("/tmp/seq-1.jsonl"), None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        assert_eq!(claimed.batch_max_seq, 1);

        {
            let conn = store.conn.lock().await;
            conn.execute("DELETE FROM compliance_events WHERE id = ?1", [&first])
                .unwrap();
        }
        store
            .record_injection_with_host_session(
                "inject-seq-2",
                "mem-seq-2",
                &Priority::Must,
                "codex",
                Some("host-seq"),
            )
            .await
            .unwrap();

        let snapshot = store
            .pending_compliance_for_host_session_upto("host-seq", claimed.batch_max_seq)
            .await
            .unwrap();
        assert!(
            snapshot.is_empty(),
            "a new event must not reuse the deleted event's sequence"
        );
        let conn = store.conn.lock().await;
        let second_seq: i64 = conn
            .query_row(
                "SELECT batch_seq FROM compliance_events WHERE inject_session_id = 'inject-seq-2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(second_seq, 2);
    }

    /// Once a stale worker's lease is reclaimed, its heartbeat, completion,
    /// and failure paths must all be no-ops; only the replacement owner may
    /// mutate the job.
    #[tokio::test]
    async fn stale_worker_cannot_mutate_a_reclaimed_job() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .enqueue_finalize_job("host-takeover", "codex", None, None)
            .await
            .unwrap();
        let worker_a = store.claim_next_finalize_job().await.unwrap().unwrap();
        let token_a = worker_a.lease_token.clone().unwrap();

        tokio::time::sleep(Duration::from_millis(2)).await;
        assert_eq!(
            store
                .reap_stale_finalize_jobs(chrono::Duration::zero())
                .await
                .unwrap(),
            1
        );
        let worker_b = store.claim_next_finalize_job().await.unwrap().unwrap();
        let token_b = worker_b.lease_token.clone().unwrap();
        assert_eq!(worker_b.attempts, 2);
        assert_ne!(token_a, token_b, "each claim must issue a new lease token");

        assert!(
            !store
                .heartbeat_finalize_job(worker_a.id, &token_a)
                .await
                .unwrap(),
            "the old worker must not extend the new lease"
        );
        assert_eq!(
            store
                .complete_finalize_job(worker_a.id, &token_a, Some("stale completion"))
                .await
                .unwrap(),
            None,
            "the old worker must not complete the new owner's job"
        );
        assert!(
            !store
                .fail_finalize_job(worker_a.id, &token_a, "stale failure")
                .await
                .unwrap(),
            "the old worker must not fail the new owner's job"
        );

        assert_eq!(
            store
                .complete_finalize_job(worker_b.id, &token_b, None)
                .await
                .unwrap(),
            Some(FinalizeJobStatus::Done)
        );
    }

    /// Job-state fencing alone is not enough: the old worker must also be
    /// unable to write verdicts for rows now owned by the replacement.
    #[tokio::test]
    async fn stale_worker_cannot_write_compliance_verdicts() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection_with_host_session(
                "inject-takeover-verdict",
                "mem-takeover-verdict",
                &Priority::Must,
                "codex",
                Some("host-takeover-verdict"),
            )
            .await
            .unwrap();
        store
            .enqueue_finalize_job("host-takeover-verdict", "codex", None, None)
            .await
            .unwrap();
        let worker_a = store.claim_next_finalize_job().await.unwrap().unwrap();
        let token_a = worker_a.lease_token.clone().unwrap();

        tokio::time::sleep(Duration::from_millis(2)).await;
        store
            .reap_stale_finalize_jobs(chrono::Duration::zero())
            .await
            .unwrap();
        let worker_b = store.claim_next_finalize_job().await.unwrap().unwrap();
        let token_b = worker_b.lease_token.clone().unwrap();

        assert_eq!(
            store
                .report_with_reason_leased(
                    JobLease {
                        job_id: worker_a.id,
                        lease_token: &token_a,
                    },
                    "inject-takeover-verdict",
                    "mem-takeover-verdict",
                    ComplianceStatus::Violated,
                    Some("stale verdict"),
                    None,
                )
                .await
                .unwrap(),
            LeaseWrite::LeaseLost,
            "a stale worker must not write a verdict"
        );
        assert_eq!(
            store
                .finalize_pending_for_host_session_upto_leased(
                    JobLease {
                        job_id: worker_a.id,
                        lease_token: &token_a,
                    },
                    "host-takeover-verdict",
                    worker_a.batch_max_seq,
                    ComplianceStatus::Unknown,
                    None,
                    Some("stale fallback"),
                )
                .await
                .unwrap(),
            LeaseWrite::LeaseLost,
            "a stale worker must not close the replacement's pending rows"
        );
        assert_eq!(
            store
                .get_report("inject-takeover-verdict")
                .await
                .unwrap()
                .pending,
            1
        );

        assert_eq!(
            store
                .report_with_reason_leased(
                    JobLease {
                        job_id: worker_b.id,
                        lease_token: &token_b,
                    },
                    "inject-takeover-verdict",
                    "mem-takeover-verdict",
                    ComplianceStatus::Followed,
                    Some("new owner verdict"),
                    None,
                )
                .await
                .unwrap(),
            LeaseWrite::Applied(1)
        );
        assert_eq!(
            store
                .complete_finalize_job(worker_b.id, &token_b, None)
                .await
                .unwrap(),
            Some(FinalizeJobStatus::Done)
        );
    }

    #[tokio::test]
    async fn heartbeat_extends_running_lease_and_ignores_finished_jobs() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .enqueue_finalize_job("host-heartbeat", "codex", None, None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        let lease_token = claimed.lease_token.clone().unwrap();

        tokio::time::sleep(Duration::from_millis(2)).await;
        assert!(
            store
                .heartbeat_finalize_job(claimed.id, &lease_token)
                .await
                .unwrap()
        );

        let conn = store.conn.lock().await;
        let updated_after_heartbeat: String = conn
            .query_row(
                "SELECT updated_at FROM compliance_finalize_jobs WHERE id = ?1",
                [claimed.id],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(
            updated_after_heartbeat > claimed.updated_at,
            "heartbeat must advance the lease timestamp"
        );

        store
            .complete_finalize_job(claimed.id, &lease_token, None)
            .await
            .unwrap();
        assert!(
            !store
                .heartbeat_finalize_job(claimed.id, &lease_token)
                .await
                .unwrap(),
            "a finished job must not be resurrected as running"
        );
    }

    #[tokio::test]
    async fn test_compliance_flow() {
        let store = ComplianceStore::new(":memory:").unwrap();

        store
            .record_injection("inj_001", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_001", "mem_b", &Priority::Reference, "claude")
            .await
            .unwrap();

        store
            .report("inj_001", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();
        store
            .report(
                "inj_001",
                "mem_b",
                ComplianceStatus::Violated,
                Some("ignored preference"),
            )
            .await
            .unwrap();

        let report = store.get_report("inj_001").await.unwrap();
        assert_eq!(report.must_followed, 1);
        assert_eq!(report.must_violated, 0);
        assert_eq!(report.ref_violated, 1);
        assert_eq!(report.compliance_rate, Some(1.0));
    }

    #[tokio::test]
    async fn test_purge_memory_deletes_only_that_memorys_rows() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_001", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_002", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_001", "mem_b", &Priority::Reference, "claude")
            .await
            .unwrap();

        let affected = store.purge_memory("mem_a").await.unwrap();
        assert_eq!(affected, 2, "both mem_a rows (inj_001, inj_002) are purged");

        // mem_a's row under inj_001 is gone; mem_b's (different memory)
        // survives, so the session now has exactly one tracked injection.
        let report_after = store.get_report("inj_001").await.unwrap();
        assert_eq!(report_after.must_violated + report_after.must_followed, 0);
        assert_eq!(report_after.total_injected, 1);

        // Purging an id with no rows is a no-op, not an error.
        assert_eq!(store.purge_memory("mem_never_existed").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_compliance_summary() {
        let store = ComplianceStore::new(":memory:").unwrap();

        store
            .record_injection("inj_001", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_002", "mem_b", &Priority::Must, "claude")
            .await
            .unwrap();

        store
            .report("inj_001", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();
        store
            .report("inj_002", "mem_b", ComplianceStatus::Violated, None)
            .await
            .unwrap();

        let summary = store.get_summary(Some("claude"), 10).await.unwrap();
        assert_eq!(summary.total_sessions, 2);
        assert_eq!(summary.must_rate, Some(0.5));
    }

    #[tokio::test]
    async fn test_report_errors_when_no_matching_event() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let result = store
            .report(
                "nonexistent_session",
                "mem_x",
                ComplianceStatus::Followed,
                None,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_report_for_unknown_session_returns_empty_with_full_compliance() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let report = store.get_report("never_existed").await.unwrap();
        assert_eq!(report.total_injected, 0);
        assert_eq!(report.compliance_rate, None);
        assert_eq!(report.agent_id, "");
    }

    #[tokio::test]
    async fn test_get_report_pending_when_not_reported() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_pending", "mem_a", &Priority::Reference, "claude")
            .await
            .unwrap();
        // Never call report() for this memory
        let report = store.get_report("inj_pending").await.unwrap();
        assert_eq!(report.pending, 1);
        assert_eq!(report.total_injected, 1);
    }

    #[tokio::test]
    async fn test_get_report_must_violated_lowers_compliance_rate() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_v", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_v", "mem_b", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .report("inj_v", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();
        store
            .report("inj_v", "mem_b", ComplianceStatus::Violated, None)
            .await
            .unwrap();

        let report = store.get_report("inj_v").await.unwrap();
        assert_eq!(report.must_followed, 1);
        assert_eq!(report.must_violated, 1);
        assert_eq!(report.compliance_rate, Some(0.5));
    }

    #[tokio::test]
    async fn test_get_report_ref_followed_counted() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_ref", "mem_a", &Priority::Reference, "claude")
            .await
            .unwrap();
        store
            .report("inj_ref", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();

        let report = store.get_report("inj_ref").await.unwrap();
        assert_eq!(report.ref_followed, 1);
        // No MUST verdict exists -> N/A, not a vacuous 100%.
        assert_eq!(report.compliance_rate, None);
    }

    #[tokio::test]
    async fn test_get_summary_without_agent_filter_spans_all_agents() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_a", "mem_a", &Priority::Must, "agent-1")
            .await
            .unwrap();
        store
            .record_injection("inj_b", "mem_b", &Priority::Must, "agent-2")
            .await
            .unwrap();
        store
            .report("inj_a", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();
        store
            .report("inj_b", "mem_b", ComplianceStatus::Followed, None)
            .await
            .unwrap();

        let summary = store.get_summary(None, 10).await.unwrap();
        assert_eq!(summary.total_sessions, 2);
        assert_eq!(summary.overall_rate, Some(1.0));
        assert_eq!(summary.must_rate, Some(1.0));
    }

    #[tokio::test]
    async fn test_get_report_all_pending_must_is_none_not_full_compliance() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_pending_must", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        let report = store.get_report("inj_pending_must").await.unwrap();
        assert_eq!(report.pending, 1);
        assert_eq!(
            report.compliance_rate, None,
            "pending MUST rows must not be reported as 100% compliant"
        );

        let summary = store.get_summary(Some("claude"), 10).await.unwrap();
        assert_eq!(summary.overall_rate, None);
        assert_eq!(summary.must_rate, None);
    }

    #[tokio::test]
    async fn test_sweep_stale_pending_closes_old_rows() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_old", "mem_a", &Priority::Must, "codex")
            .await
            .unwrap();
        // created_at is stamped before the sweep runs, so a zero-length
        // window still catches it.
        let swept = store
            .sweep_stale_pending(chrono::Duration::zero())
            .await
            .unwrap();
        assert_eq!(swept, 1);
        let report = store.get_report("inj_old").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(
            report.compliance_rate, None,
            "a sweep must not invent a followed verdict"
        );
    }

    #[tokio::test]
    async fn test_migrates_legacy_table_without_host_session_id() {
        // Regression: the CREATE TABLE batch used to index host_session_id at
        // open time, so an old DB (table exists, column absent) failed before
        // the ALTER migration could add it.
        let path =
            std::env::temp_dir().join(format!("memvault-compliance-migrate-{}.db", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    inject_session_id TEXT NOT NULL,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .record_injection_with_host_session(
                "inj_legacy",
                "mem_x",
                &Priority::Must,
                "codex",
                Some("host-9"),
            )
            .await
            .unwrap();
        let pending = store
            .pending_compliance_for_host_session("host-9")
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// Old job tables predate both the lease token and the batch watermark.
    /// Opening one must add both columns and the first claim must populate
    /// them, otherwise upgraded installs cannot use lease fencing.
    #[tokio::test]
    async fn test_migrates_legacy_finalize_job_lease_columns() {
        let path =
            std::env::temp_dir().join(format!("memvault-compliance-migrate-{}.db", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    inject_session_id TEXT NOT NULL,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );
                 CREATE TABLE compliance_finalize_jobs (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    host_session_id TEXT NOT NULL UNIQUE,
                    agent_id        TEXT NOT NULL,
                    transcript_path TEXT,
                    transcript_tail TEXT,
                    status          TEXT NOT NULL DEFAULT 'queued',
                    attempts        INTEGER NOT NULL DEFAULT 0,
                    available_at    TEXT NOT NULL,
                    last_error      TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .record_injection_with_host_session(
                "inj_legacy_lease",
                "mem_legacy_lease",
                &Priority::Must,
                "codex",
                Some("host-legacy-lease"),
            )
            .await
            .unwrap();
        store
            .enqueue_finalize_job("host-legacy-lease", "codex", None, None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        assert!(
            claimed.lease_token.is_some(),
            "claim must issue a lease token after migration"
        );
        assert_eq!(
            claimed.batch_max_seq, 1,
            "claim must snapshot the existing pending batch after migration"
        );
        assert_eq!(
            store
                .complete_finalize_job(claimed.id, claimed.lease_token.as_deref().unwrap(), None,)
                .await
                .unwrap(),
            Some(FinalizeJobStatus::Queued),
            "the migrated pending row must keep the job open"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Existing events predate `batch_seq`; opening the store must backfill
    /// them, and the next allocated sequence must be strictly greater so a
    /// queued job's old watermark cannot capture post-upgrade events.
    #[tokio::test]
    async fn test_migrates_legacy_events_to_monotonic_batch_seq() {
        let path =
            std::env::temp_dir().join(format!("memvault-compliance-migrate-{}.db", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    inject_session_id TEXT NOT NULL,
                    host_session_id   TEXT,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );
                INSERT INTO compliance_events
                    (id, inject_session_id, host_session_id, memory_id, priority, status, agent_id, created_at)
                VALUES
                    ('legacy-event', 'inject-legacy-seq', 'host-legacy-seq', 'mem-legacy',
                     'MUST', 'pending', 'codex', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .record_injection_with_host_session(
                "inject-modern-seq",
                "mem-modern",
                &Priority::Must,
                "codex",
                Some("host-legacy-seq"),
            )
            .await
            .unwrap();
        store
            .enqueue_finalize_job("host-legacy-seq", "codex", None, None)
            .await
            .unwrap();
        let claimed = store.claim_next_finalize_job().await.unwrap().unwrap();
        assert_eq!(
            claimed.batch_max_seq, 2,
            "the migrated event owns seq 1 and the new event must get seq 2"
        );
        let snapshot = store
            .pending_compliance_for_host_session_upto("host-legacy-seq", 1)
            .await
            .unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, "legacy-event");
        let _ = std::fs::remove_file(&path);
    }

    /// A legacy job can watermark a row that no longer exists. Migration must
    /// move the allocator past that watermark too, otherwise the first new
    /// event reuses the deleted rowid and appears inside the old batch.
    #[tokio::test]
    async fn test_legacy_job_watermark_is_skipped_by_new_sequences() {
        let path =
            std::env::temp_dir().join(format!("memvault-compliance-migrate-{}.db", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    inject_session_id TEXT NOT NULL,
                    host_session_id   TEXT,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );
                INSERT INTO compliance_events
                    (id, inject_session_id, host_session_id, memory_id, priority, status, agent_id, created_at)
                VALUES
                    ('legacy-kept', 'inject-legacy-kept', 'host-legacy-gap', 'mem-legacy',
                     'MUST', 'pending', 'codex', '2026-01-01T00:00:00Z');
                 CREATE TABLE compliance_finalize_jobs (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    host_session_id TEXT NOT NULL UNIQUE,
                    agent_id        TEXT NOT NULL,
                    transcript_path TEXT,
                    transcript_tail TEXT,
                    status          TEXT NOT NULL DEFAULT 'queued',
                    attempts        INTEGER NOT NULL DEFAULT 0,
                    available_at    TEXT NOT NULL,
                    last_error      TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL,
                    batch_max_rowid INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO compliance_finalize_jobs
                    (host_session_id, agent_id, status, available_at, created_at, updated_at, batch_max_rowid)
                VALUES
                    ('host-legacy-gap', 'codex', 'queued', '2026-01-01T00:00:00Z',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 5);",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        store
            .record_injection_with_host_session(
                "inject-after-gap",
                "mem-after-gap",
                &Priority::Must,
                "codex",
                Some("host-legacy-gap"),
            )
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let new_seq: i64 = conn
            .query_row(
                "SELECT batch_seq FROM compliance_events WHERE inject_session_id = 'inject-after-gap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let migrated_watermark: i64 = conn
            .query_row(
                "SELECT batch_max_seq FROM compliance_finalize_jobs WHERE host_session_id = 'host-legacy-gap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(migrated_watermark, 5);
        assert_eq!(
            new_seq, 6,
            "the allocator must skip a deleted rowid still referenced by an old job"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Simulate a crash after the old migration added `batch_max_seq` but
    /// before it copied the legacy watermark. On the next open the backfill
    /// must still run, and the allocator must clear the recovered watermark.
    #[tokio::test]
    async fn test_interrupted_watermark_migration_is_recovered() {
        let path =
            std::env::temp_dir().join(format!("memvault-compliance-migrate-{}.db", Uuid::new_v4()));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    batch_seq         INTEGER NOT NULL DEFAULT 0,
                    inject_session_id TEXT NOT NULL,
                    host_session_id   TEXT,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );
                INSERT INTO compliance_events
                    (id, batch_seq, inject_session_id, host_session_id, memory_id, priority, status, agent_id, created_at)
                VALUES
                    ('interrupted-kept', 7, 'inject-interrupted-kept', 'host-interrupted',
                     'mem-interrupted', 'MUST', 'pending', 'codex', '2026-01-01T00:00:00Z');
                 CREATE TABLE compliance_finalize_jobs (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    host_session_id TEXT NOT NULL UNIQUE,
                    agent_id        TEXT NOT NULL,
                    transcript_path TEXT,
                    transcript_tail TEXT,
                    status          TEXT NOT NULL DEFAULT 'queued',
                    attempts        INTEGER NOT NULL DEFAULT 0,
                    available_at    TEXT NOT NULL,
                    last_error      TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL,
                    lease_token     TEXT,
                    batch_max_rowid INTEGER NOT NULL DEFAULT 0,
                    batch_max_seq   INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO compliance_finalize_jobs
                    (host_session_id, agent_id, status, available_at, created_at, updated_at,
                     batch_max_rowid, batch_max_seq)
                VALUES
                    ('host-interrupted', 'codex', 'queued', '2026-01-01T00:00:00Z',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 100, 0);
                 CREATE TABLE compliance_event_seq (
                    id       INTEGER PRIMARY KEY CHECK (id = 1),
                    next_seq INTEGER NOT NULL
                );
                INSERT INTO compliance_event_seq (id, next_seq) VALUES (1, 8);",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        {
            let conn = store.conn.lock().await;
            let watermark: i64 = conn
                .query_row(
                    "SELECT batch_max_seq FROM compliance_finalize_jobs WHERE host_session_id = 'host-interrupted'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(watermark, 100, "the legacy watermark must be recovered");
        }
        store
            .record_injection_with_host_session(
                "inject-after-interruption",
                "mem-after-interruption",
                &Priority::Must,
                "codex",
                Some("host-interrupted"),
            )
            .await
            .unwrap();
        let conn = store.conn.lock().await;
        let new_seq: i64 = conn
            .query_row(
                "SELECT batch_seq FROM compliance_events
                 WHERE inject_session_id = 'inject-after-interruption'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            new_seq, 101,
            "the allocator must skip the recovered legacy watermark"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A claimed batch with no pending rows legitimately has watermark 0.
    /// The one-shot legacy backfill must not treat that zero as "not
    /// migrated yet" and overwrite it from the dead `batch_max_rowid` column
    /// on the next open.
    #[tokio::test]
    async fn test_empty_batch_watermark_survives_reopen() {
        let path = std::env::temp_dir().join(format!(
            "memvault-compliance-empty-batch-{}.db",
            Uuid::new_v4()
        ));
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE compliance_events (
                    id                TEXT PRIMARY KEY,
                    batch_seq         INTEGER NOT NULL DEFAULT 0,
                    inject_session_id TEXT NOT NULL,
                    host_session_id   TEXT,
                    memory_id         TEXT NOT NULL,
                    priority          TEXT NOT NULL,
                    status            TEXT NOT NULL DEFAULT 'pending',
                    evidence          TEXT,
                    agent_id          TEXT NOT NULL,
                    created_at        TEXT NOT NULL,
                    reported_at       TEXT
                );
                 CREATE TABLE compliance_finalize_jobs (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    host_session_id TEXT NOT NULL UNIQUE,
                    agent_id        TEXT NOT NULL,
                    transcript_path TEXT,
                    transcript_tail TEXT,
                    status          TEXT NOT NULL DEFAULT 'queued',
                    attempts        INTEGER NOT NULL DEFAULT 0,
                    available_at    TEXT NOT NULL,
                    last_error      TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL,
                    lease_token     TEXT,
                    batch_max_rowid INTEGER NOT NULL DEFAULT 0,
                    batch_max_seq   INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO compliance_finalize_jobs
                    (host_session_id, agent_id, status, available_at, created_at,
                     updated_at, batch_max_rowid, batch_max_seq)
                VALUES
                    ('host-empty-batch', 'codex', 'queued', '2020-01-01T00:00:00Z',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 100, 0);
                 CREATE TABLE compliance_event_seq (
                    id       INTEGER PRIMARY KEY CHECK (id = 1),
                    next_seq INTEGER NOT NULL
                );
                INSERT INTO compliance_event_seq (id, next_seq) VALUES (1, 102);",
            )
            .unwrap();
        }

        let store = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        {
            let conn = store.conn.lock().await;
            let migrated: i64 = conn
                .query_row(
                    "SELECT batch_max_seq FROM compliance_finalize_jobs
                     WHERE host_session_id = 'host-empty-batch'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(migrated, 100, "the legacy watermark is copied once");
        }
        let claimed = store
            .claim_next_finalize_job()
            .await
            .unwrap()
            .expect("the queued job is claimable");
        assert_eq!(
            claimed.batch_max_seq, 0,
            "a batch with no pending rows has watermark 0"
        );
        drop(store);

        let reopened = ComplianceStore::new(path.to_str().unwrap()).unwrap();
        {
            let conn = reopened.conn.lock().await;
            let watermark: i64 = conn
                .query_row(
                    "SELECT batch_max_seq FROM compliance_finalize_jobs
                     WHERE host_session_id = 'host-empty-batch'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                watermark, 0,
                "a legitimate empty-batch watermark must not be re-inflated"
            );
        }
        reopened
            .record_injection_with_host_session(
                "inject-after-empty-batch",
                "mem-after-empty-batch",
                &Priority::Must,
                "codex",
                Some("host-empty-batch"),
            )
            .await
            .unwrap();
        {
            let conn = reopened.conn.lock().await;
            let new_seq: i64 = conn
                .query_row(
                    "SELECT batch_seq FROM compliance_events
                     WHERE inject_session_id = 'inject-after-empty-batch'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                new_seq, 102,
                "the allocator keeps counting above the migrated legacy watermark"
            );
        }
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_host_session_injection_and_finalize() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection_with_host_session(
                "inj_host",
                "mem_a",
                &Priority::Must,
                "codex",
                Some("host-session-1"),
            )
            .await
            .unwrap();
        store
            .record_injection_with_host_session(
                "inj_host",
                "mem_b",
                &Priority::Reference,
                "codex",
                Some("host-session-1"),
            )
            .await
            .unwrap();

        let pending = store
            .pending_compliance_for_host_session("host-session-1")
            .await
            .unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].inject_session_id, "inj_host");

        let finalized = store
            .finalize_pending_for_host_session(
                "host-session-1",
                ComplianceStatus::Unknown,
                None,
                Some("no judge configured"),
            )
            .await
            .unwrap();
        assert_eq!(finalized, 2);
        assert!(
            store
                .pending_compliance_for_host_session("host-session-1")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_finalize_stale_pending_closes_legacy_rows() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_old", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        let finalized = store
            .finalize_stale_pending(
                Utc::now() + chrono::Duration::seconds(1),
                ComplianceStatus::Unknown,
                Some("legacy backlog cleanup"),
            )
            .await
            .unwrap();
        assert_eq!(finalized, 1);
        let report = store.get_report("inj_old").await.unwrap();
        assert_eq!(report.pending, 0);
        assert_eq!(report.compliance_rate, None);
    }

    #[test]
    fn test_compliance_status_from_str_valid() {
        use std::str::FromStr;
        assert_eq!(
            ComplianceStatus::from_str("followed").unwrap(),
            ComplianceStatus::Followed
        );
        assert_eq!(
            ComplianceStatus::from_str("violated").unwrap(),
            ComplianceStatus::Violated
        );
        assert_eq!(
            ComplianceStatus::from_str("pending").unwrap(),
            ComplianceStatus::Pending
        );
        assert_eq!(
            ComplianceStatus::from_str("unknown").unwrap(),
            ComplianceStatus::Unknown
        );
    }

    #[test]
    fn test_compliance_status_from_str_invalid() {
        use std::str::FromStr;
        assert!(ComplianceStatus::from_str("bogus").is_err());
    }

    #[test]
    fn test_compliance_status_display() {
        assert_eq!(ComplianceStatus::Followed.to_string(), "followed");
        assert_eq!(ComplianceStatus::Violated.to_string(), "violated");
        assert_eq!(ComplianceStatus::Pending.to_string(), "pending");
        assert_eq!(ComplianceStatus::Unknown.to_string(), "unknown");
    }

    #[tokio::test]
    async fn test_pending_since_excludes_already_judged_and_out_of_window() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let id_a = store
            .record_injection("inj_a", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .record_injection("inj_a", "mem_b", &Priority::Reference, "claude")
            .await
            .unwrap();

        // Already judged — must not come back as pending.
        store
            .record_effectiveness(&id_a, EffectivenessVerdict::Useful, Some("helped"))
            .await
            .unwrap();

        let since = Utc::now() - chrono::Duration::hours(1);
        let pending = store.pending_since("claude", since, 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].memory_id, "mem_b");

        // Outside the window — must not come back either.
        let future_since = Utc::now() + chrono::Duration::hours(1);
        let pending_future = store
            .pending_since("claude", future_since, 10)
            .await
            .unwrap();
        assert!(pending_future.is_empty());
    }

    #[tokio::test]
    async fn test_record_effectiveness_errors_on_unknown_id() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let result = store
            .record_effectiveness("nonexistent", EffectivenessVerdict::Neutral, None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_effectiveness_summary_rates_and_coverage() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let id_a = store
            .record_injection("inj_a", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        let id_b = store
            .record_injection("inj_a", "mem_b", &Priority::Reference, "claude")
            .await
            .unwrap();
        let id_c = store
            .record_injection("inj_a", "mem_c", &Priority::Reference, "claude")
            .await
            .unwrap();
        // mem_d stays unjudged.
        store
            .record_injection("inj_a", "mem_d", &Priority::Reference, "claude")
            .await
            .unwrap();

        store
            .record_effectiveness(&id_a, EffectivenessVerdict::Useful, None)
            .await
            .unwrap();
        store
            .record_effectiveness(&id_b, EffectivenessVerdict::Harmful, None)
            .await
            .unwrap();
        store
            .record_effectiveness(&id_c, EffectivenessVerdict::InsufficientContext, None)
            .await
            .unwrap();

        let summary = store
            .get_effectiveness_summary(Some("claude"), 10)
            .await
            .unwrap();
        assert_eq!(summary.useful, 1);
        assert_eq!(summary.harmful, 1);
        assert_eq!(summary.insufficient, 1);
        assert_eq!(summary.unjudged, 1);
        assert_eq!(summary.usefulness_rate, 0.5); // useful / (useful+neutral+harmful) = 1/2
        assert_eq!(summary.harmful_rate, 0.5);
        assert_eq!(summary.coverage, 0.75); // 3 judged (incl. insufficient) / 4 total
    }

    #[test]
    fn test_effectiveness_verdict_from_str_roundtrip() {
        use std::str::FromStr;
        assert_eq!(
            EffectivenessVerdict::from_str("useful").unwrap(),
            EffectivenessVerdict::Useful
        );
        assert_eq!(
            EffectivenessVerdict::from_str("insufficient_context").unwrap(),
            EffectivenessVerdict::InsufficientContext
        );
        assert!(EffectivenessVerdict::from_str("bogus").is_err());
    }

    #[tokio::test]
    async fn test_unknown_status_not_counted_as_followed_or_violated() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_u", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();
        store
            .report("inj_u", "mem_a", ComplianceStatus::Unknown, None)
            .await
            .unwrap();

        let report = store.get_report("inj_u").await.unwrap();
        assert_eq!(report.must_followed, 0);
        assert_eq!(report.must_violated, 0);
        assert_eq!(report.pending, 0);
    }

    #[tokio::test]
    async fn test_report_with_reason_persists_reason() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_reason", "mem_a", &Priority::Reference, "claude")
            .await
            .unwrap();

        store
            .report_with_reason(
                "inj_reason",
                "mem_a",
                ComplianceStatus::Unknown,
                None,
                Some("not mentioned in transcript"),
            )
            .await
            .unwrap();

        let conn = store.conn.lock().await;
        let reason: Option<String> = conn
            .query_row(
                "SELECT reason FROM compliance_events WHERE inject_session_id = ?1 AND memory_id = ?2",
                rusqlite::params!["inj_reason", "mem_a"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason.as_deref(), Some("not mentioned in transcript"));
    }

    #[tokio::test]
    async fn test_report_with_reason_none_leaves_reason_null() {
        let store = ComplianceStore::new(":memory:").unwrap();
        store
            .record_injection("inj_noreason", "mem_a", &Priority::Must, "claude")
            .await
            .unwrap();

        store
            .report("inj_noreason", "mem_a", ComplianceStatus::Followed, None)
            .await
            .unwrap();

        let conn = store.conn.lock().await;
        let reason: Option<String> = conn
            .query_row(
                "SELECT reason FROM compliance_events WHERE inject_session_id = ?1 AND memory_id = ?2",
                rusqlite::params!["inj_noreason", "mem_a"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, None);
    }

    #[tokio::test]
    async fn test_harmful_count_for_memory_respects_since_boundary() {
        let store = ComplianceStore::new(":memory:").unwrap();
        let id_harmful = store
            .record_injection("inj_h", "mem_h", &Priority::Reference, "claude")
            .await
            .unwrap();
        let id_useful = store
            .record_injection("inj_h2", "mem_h", &Priority::Reference, "claude")
            .await
            .unwrap();

        store
            .record_effectiveness(
                &id_harmful,
                EffectivenessVerdict::Harmful,
                Some("caused confusion"),
            )
            .await
            .unwrap();
        store
            .record_effectiveness(&id_useful, EffectivenessVerdict::Useful, None)
            .await
            .unwrap();

        let past = Utc::now() - chrono::Duration::hours(1);
        let count = store.harmful_count_for_memory("mem_h", past).await.unwrap();
        assert_eq!(
            count, 1,
            "only the Harmful verdict should count, not Useful"
        );

        let future = Utc::now() + chrono::Duration::hours(1);
        let count_future = store
            .harmful_count_for_memory("mem_h", future)
            .await
            .unwrap();
        assert_eq!(
            count_future, 0,
            "a judged_at before `since` must be excluded"
        );
    }
}
