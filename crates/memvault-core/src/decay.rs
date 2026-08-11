use chrono::{DateTime, Utc};
use std::sync::Arc;
use tracing::{debug, info};

use crate::error::Result;
use crate::models::*;
use crate::storage::MemoryStore;

pub struct DecayManager {
    store: Arc<dyn MemoryStore>,
    config: DecayConfig,
}

#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub daily_decay_rate: f64,
    pub access_boost: f64,
    pub archive_threshold: f64,
    pub must_exempt: bool,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            daily_decay_rate: 0.02,
            access_boost: 0.1,
            archive_threshold: 0.2,
            must_exempt: true,
        }
    }
}

impl DecayManager {
    pub fn new(store: Arc<dyn MemoryStore>, config: DecayConfig) -> Self {
        Self { store, config }
    }

    /// Calculate decay score based on time elapsed since last update.
    pub fn calculate_decay(
        &self,
        current_score: f64,
        last_updated: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> f64 {
        let days = (now - last_updated).num_hours() as f64 / 24.0;
        if days <= 0.0 {
            return current_score;
        }

        let decayed = current_score * (1.0 - self.config.daily_decay_rate).powf(days);
        decayed.max(0.0)
    }

    /// Boost decay score when a memory is accessed.
    pub fn boost_on_access(&self, current_score: f64) -> f64 {
        (current_score + self.config.access_boost).min(1.0)
    }

    /// Run decay on all memories, update scores, return archived count.
    pub async fn run_decay(&self) -> Result<DecayReport> {
        let now = Utc::now();
        let memories = self.store.list(None, 100000, 0).await?;

        let mut updated = 0;
        let mut archived = 0;

        for mut mem in memories {
            if self.config.must_exempt && mem.priority == Priority::Must {
                continue;
            }

            let new_score = self.calculate_decay(mem.decay_score, mem.updated_at, now);

            if (new_score - mem.decay_score).abs() < 0.001 {
                continue;
            }

            if new_score < self.config.archive_threshold {
                mem.decay_score = new_score;
                mem.namespace = format!("archived:{}", mem.namespace);
                self.store.update(mem).await?;
                archived += 1;
            } else {
                mem.decay_score = new_score;
                self.store.update(mem).await?;
                updated += 1;
            }
        }

        let report = DecayReport { updated, archived };
        info!(
            updated = report.updated,
            archived = report.archived,
            "decay cycle complete"
        );
        Ok(report)
    }

    /// Record an access to a memory, boosting its decay score.
    pub async fn record_access(&self, id: &str) -> Result<()> {
        let mut mem = self.store.get(id).await?;
        mem.decay_score = self.boost_on_access(mem.decay_score);
        mem.access_count += 1;
        mem.updated_at = Utc::now();
        self.store.update(mem).await?;
        debug!(id, "memory access recorded");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DecayReport {
    pub updated: usize,
    pub archived: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;
    use chrono::Duration;

    fn make_config() -> DecayConfig {
        DecayConfig {
            daily_decay_rate: 0.05,
            access_boost: 0.15,
            archive_threshold: 0.3,
            must_exempt: true,
        }
    }

    #[test]
    fn test_decay_calculation() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let dm = DecayManager::new(store, make_config());

        let now = Utc::now();
        let ten_days_ago = now - Duration::days(10);

        let score = dm.calculate_decay(1.0, ten_days_ago, now);
        assert!(score < 1.0);
        assert!(score > 0.5); // 5% daily decay over 10 days ≈ 0.60
    }

    #[test]
    fn test_no_decay_for_recent() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let dm = DecayManager::new(store, make_config());

        let now = Utc::now();
        let score = dm.calculate_decay(1.0, now, now);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_boost_caps_at_one() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let dm = DecayManager::new(store, make_config());

        let boosted = dm.boost_on_access(0.95);
        assert!((boosted - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_run_decay() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        // Must memory should not decay
        store
            .save(Memory::new(
                MemoryType::Preference,
                "must rule".to_string(),
                Priority::Must,
                agent.clone(),
            ))
            .await
            .unwrap();

        // Old memory with low score
        let mut old = Memory::new(
            MemoryType::Fact,
            "old fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        old.decay_score = 0.25;
        old.updated_at = Utc::now() - Duration::days(30);
        store.save(old).await.unwrap();

        // Recent memory
        let recent = Memory::new(
            MemoryType::Fact,
            "recent fact".to_string(),
            Priority::Reference,
            agent,
        );
        store.save(recent).await.unwrap();

        let dm = DecayManager::new(store, make_config());
        let report = dm.run_decay().await.unwrap();

        // old memory should be archived (score 0.25 with 30 days decay → well below 0.3)
        assert!(report.archived >= 1);
    }

    #[tokio::test]
    async fn test_record_access() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut mem = Memory::new(
            MemoryType::Fact,
            "test".to_string(),
            Priority::Reference,
            agent,
        );
        mem.decay_score = 0.5;
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let dm = DecayManager::new(store.clone(), make_config());
        dm.record_access(&id).await.unwrap();

        let updated = store.get(&id).await.unwrap();
        assert!(updated.decay_score > 0.5);
        assert_eq!(updated.access_count, 1);
    }
}
