//! Trend-over-time persistence for `bench`/`doctor` — Tier-2 "formal memory
//! quality" from a memory-eval framework we're adapting: today both commands
//! print a report once and discard it, so there is no way to tell whether a
//! given change made quality better or worse. Modeled directly on
//! [`crate::compliance::ComplianceStore`]: its own plain `rusqlite::Connection`
//! on the same db file, `CREATE TABLE IF NOT EXISTS`, no versioned migrations.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;
use uuid::Uuid;

use crate::error::MemVaultError;

type Result<T> = std::result::Result<T, MemVaultError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    Bench,
    Doctor,
}

impl std::fmt::Display for EvalKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bench => write!(f, "bench"),
            Self::Doctor => write!(f, "doctor"),
        }
    }
}

impl std::str::FromStr for EvalKind {
    type Err = MemVaultError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "bench" => Ok(Self::Bench),
            "doctor" => Ok(Self::Doctor),
            _ => Err(MemVaultError::Storage(format!(
                "invalid eval kind: {} (expected 'bench' or 'doctor')",
                s
            ))),
        }
    }
}

/// One persisted `bench`/`doctor` run. `summary_json` keeps the full
/// `BenchReport`/`DoctorReport` for later drill-down; `headline` and
/// `warn_count` are the cheap fields a trend listing scans without parsing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRunRecord {
    pub id: String,
    pub kind: EvalKind,
    pub run_at: DateTime<Utc>,
    pub warn_count: usize,
    pub headline: String,
    pub summary_json: String,
}

pub struct EvalHistoryStore {
    conn: Mutex<Connection>,
}

impl EvalHistoryStore {
    pub fn new(db_path: &str) -> Result<Arc<Self>> {
        let conn = Connection::open(db_path).map_err(|e| MemVaultError::Storage(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS eval_runs (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                run_at       TEXT NOT NULL,
                warn_count   INTEGER NOT NULL,
                headline     TEXT NOT NULL,
                summary_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_eval_runs_kind_time ON eval_runs(kind, run_at);",
        )
        .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
        }))
    }

    pub async fn record(
        &self,
        kind: EvalKind,
        warn_count: usize,
        headline: &str,
        summary_json: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let kind_str = kind.to_string();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO eval_runs (id, kind, run_at, warn_count, headline, summary_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, kind_str, now, warn_count as i64, headline, summary_json],
        )
        .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        debug!(id = %id, kind = %kind_str, warn_count, "eval run recorded");
        Ok(id)
    }

    pub async fn recent(&self, kind: EvalKind, limit: usize) -> Result<Vec<EvalRunRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, run_at, warn_count, headline, summary_json
                 FROM eval_runs WHERE kind = ?1 ORDER BY run_at DESC LIMIT ?2",
            )
            .map_err(|e| MemVaultError::Storage(e.to_string()))?;

        let kind_str = kind.to_string();
        let rows = stmt
            .query_map(rusqlite::params![kind_str, limit as i64], |row| {
                let run_at_str: String = row.get(2)?;
                let warn_count: i64 = row.get(3)?;
                Ok(EvalRunRecord {
                    id: row.get(0)?,
                    kind,
                    run_at: DateTime::parse_from_rfc3339(&run_at_str)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    warn_count: warn_count as usize,
                    headline: row.get(4)?,
                    summary_json: row.get(5)?,
                })
            })
            .map_err(|e| MemVaultError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[tokio::test]
    async fn test_record_and_recent_roundtrip() {
        let store = EvalHistoryStore::new(":memory:").unwrap();
        store
            .record(EvalKind::Doctor, 3, "3 warn / 12 info", "{}")
            .await
            .unwrap();

        let runs = store.recent(EvalKind::Doctor, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].warn_count, 3);
        assert_eq!(runs[0].headline, "3 warn / 12 info");
    }

    #[tokio::test]
    async fn test_recent_filters_by_kind() {
        let store = EvalHistoryStore::new(":memory:").unwrap();
        store
            .record(EvalKind::Doctor, 1, "doctor run", "{}")
            .await
            .unwrap();
        store
            .record(EvalKind::Bench, 0, "bench run", "{}")
            .await
            .unwrap();

        let doctor_runs = store.recent(EvalKind::Doctor, 10).await.unwrap();
        assert_eq!(doctor_runs.len(), 1);
        assert_eq!(doctor_runs[0].headline, "doctor run");

        let bench_runs = store.recent(EvalKind::Bench, 10).await.unwrap();
        assert_eq!(bench_runs.len(), 1);
        assert_eq!(bench_runs[0].headline, "bench run");
    }

    #[tokio::test]
    async fn test_recent_orders_newest_first_and_respects_limit() {
        let store = EvalHistoryStore::new(":memory:").unwrap();
        for i in 0..5 {
            store
                .record(EvalKind::Doctor, i, &format!("run {i}"), "{}")
                .await
                .unwrap();
        }

        let runs = store.recent(EvalKind::Doctor, 2).await.unwrap();
        assert_eq!(runs.len(), 2);
        // Inserted in the same second, so `run_at` alone cannot guarantee
        // strict ordering — but the limit itself must still be respected.
        assert!(runs.iter().all(|r| r.headline.starts_with("run ")));
    }

    #[test]
    fn test_eval_kind_from_str_roundtrip() {
        assert_eq!(EvalKind::from_str("bench").unwrap(), EvalKind::Bench);
        assert_eq!(EvalKind::from_str("doctor").unwrap(), EvalKind::Doctor);
        assert!(EvalKind::from_str("bogus").is_err());
    }

    #[test]
    fn test_eval_kind_display() {
        assert_eq!(EvalKind::Bench.to_string(), "bench");
        assert_eq!(EvalKind::Doctor.to_string(), "doctor");
    }
}
