use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
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
    pub compliance_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateSummary {
    pub total_sessions: usize,
    pub overall_rate: f64,
    pub must_rate: f64,
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

pub struct ComplianceStore {
    conn: Mutex<Connection>,
}

impl ComplianceStore {
    pub fn new(db_path: &str) -> Result<Arc<Self>> {
        let conn = Connection::open(db_path).map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS compliance_events (
                id                TEXT PRIMARY KEY,
                inject_session_id TEXT NOT NULL,
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
            CREATE INDEX IF NOT EXISTS idx_compliance_agent ON compliance_events(agent_id);"
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
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let priority_str = serde_json::to_string(priority)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO compliance_events (id, inject_session_id, memory_id, priority, status, agent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6)",
            rusqlite::params![id, inject_session_id, memory_id, priority_str, agent_id, now],
        ).map_err(|e| MemVaultError::Storage(e.to_string()))?;

        debug!(id = %id, session = %inject_session_id, memory = %memory_id, "compliance event recorded");
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
                compliance_rate: 1.0,
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
            must_followed as f64 / must_total as f64
        } else {
            1.0
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

        for r in &reports {
            total_must_f += r.must_followed;
            total_must_v += r.must_violated;
            rate_sum += r.compliance_rate;
        }

        let must_total = total_must_f + total_must_v;
        let must_rate = if must_total > 0 {
            total_must_f as f64 / must_total as f64
        } else {
            1.0
        };
        let overall_rate = if total_sessions > 0 {
            rate_sum / total_sessions as f64
        } else {
            1.0
        };

        Ok(AggregateSummary {
            total_sessions,
            overall_rate,
            must_rate,
            recent_sessions: reports,
        })
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
        assert_eq!(report.compliance_rate, 1.0);
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
        assert_eq!(summary.must_rate, 0.5);
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
        assert_eq!(report.compliance_rate, 1.0);
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
        assert_eq!(report.compliance_rate, 0.5);
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
        // No MUST memories injected -> compliance_rate defaults to 1.0
        assert_eq!(report.compliance_rate, 1.0);
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
        assert_eq!(summary.overall_rate, 1.0);
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
}
