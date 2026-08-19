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
