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
    /// Memories with at least one ACTIVE contradiction against them decay
    /// this many times faster (evidence-driven forgetting, not just
    /// time-driven; claude-obsidian review provenance: docs/DESIGN.md §16). Set to 1.0 to
    /// disable the acceleration.
    pub contradiction_multiplier: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            daily_decay_rate: 0.02,
            access_boost: 0.1,
            archive_threshold: 0.2,
            must_exempt: true,
            contradiction_multiplier: 3.0,
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
        self.calculate_decay_at_rate(
            current_score,
            last_updated,
            now,
            self.config.daily_decay_rate,
        )
    }

    /// Decay at an explicit daily rate — lets callers accelerate forgetting
    /// for memories with active counter-evidence without touching config.
    pub fn calculate_decay_at_rate(
        &self,
        current_score: f64,
        last_updated: DateTime<Utc>,
        now: DateTime<Utc>,
        daily_rate: f64,
    ) -> f64 {
        let days = (now - last_updated).num_hours() as f64 / 24.0;
        if days <= 0.0 {
            return current_score;
        }

        let decayed = current_score * (1.0 - daily_rate.clamp(0.0, 1.0)).powf(days);
        decayed.max(0.0)
    }

    /// Boost decay score when a memory is accessed.
    pub fn boost_on_access(&self, current_score: f64) -> f64 {
        (current_score + self.config.access_boost).min(1.0)
    }

    /// Run decay on all memories, update scores, return archived count.
    ///
    /// Memories with an active contradiction decay
    /// `contradiction_multiplier` times faster: counter-evidence pushes a
    /// memory toward archival instead of waiting out the clock.
    pub async fn run_decay(&self) -> Result<DecayReport> {
        let now = Utc::now();
        let memories = self.store.list(None, 100000, 0).await?;

        let mut updated = 0;
        let mut archived = 0;
        let mut contradicted = 0;

        for mut mem in memories {
            if self.config.must_exempt && mem.priority == Priority::Must {
                continue;
            }

            let is_contradicted = self.config.contradiction_multiplier > 1.0
                && crate::evidence::has_active_contradiction(&*self.store, &mem.id).await;
            if is_contradicted {
                contradicted += 1;
            }
            let daily_rate = if is_contradicted {
                (self.config.daily_decay_rate * self.config.contradiction_multiplier).min(1.0)
            } else {
                self.config.daily_decay_rate
            };

            let new_score =
                self.calculate_decay_at_rate(mem.decay_score, mem.updated_at, now, daily_rate);

            if (new_score - mem.decay_score).abs() < 0.001 {
                continue;
            }

            if new_score < self.config.archive_threshold {
                mem.decay_score = new_score;
                if !mem.namespace.starts_with("archived:") {
                    mem.namespace = format!("archived:{}", mem.namespace);
                }
                self.store.update(mem).await?;
                archived += 1;
            } else {
                mem.decay_score = new_score;
                self.store.update(mem).await?;
                updated += 1;
            }
        }

        let report = DecayReport {
            updated,
            archived,
            contradicted,
        };
        info!(
            updated = report.updated,
            archived = report.archived,
            contradicted = report.contradicted,
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
    /// Memories that decayed under an active contradiction (accelerated
    /// rate). Informational — `updated`/`archived` still count them.
    pub contradicted: usize,
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
            contradiction_multiplier: 3.0,
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
    async fn test_archived_prefix_does_not_stack_across_decay_cycles() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut old = Memory::new(
            MemoryType::Fact,
            "old fact".to_string(),
            Priority::Reference,
            agent,
        );
        old.decay_score = 0.25;
        old.updated_at = Utc::now() - Duration::days(30);
        let saved = store.save(old).await.unwrap();

        let dm = DecayManager::new(store.clone(), make_config());
        dm.run_decay().await.unwrap();
        let after_first = store.get(&saved.id).await.unwrap();
        assert_eq!(after_first.namespace, "archived:global");

        // A second decay cycle on an already-archived memory must not stack
        // another "archived:" prefix onto the namespace.
        dm.run_decay().await.unwrap();
        let after_second = store.get(&saved.id).await.unwrap();
        assert_eq!(after_second.namespace, "archived:global");
    }

    #[test]
    fn test_decay_at_rate_higher_rate_decays_faster() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let dm = DecayManager::new(store, make_config());
        let now = Utc::now();
        let ten_days_ago = now - Duration::days(10);

        let normal = dm.calculate_decay_at_rate(1.0, ten_days_ago, now, 0.05);
        let accelerated = dm.calculate_decay_at_rate(1.0, ten_days_ago, now, 0.15);
        assert!(accelerated < normal);
        // Rate clamps: a runaway multiplier can never push the score negative.
        let clamped = dm.calculate_decay_at_rate(1.0, ten_days_ago, now, 5.0);
        assert!(clamped >= 0.0);
    }

    #[tokio::test]
    async fn test_contradicted_memory_decays_faster() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let five_days_ago = Utc::now() - Duration::days(5);

        let mut plain = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        plain.decay_score = 0.9;
        plain.updated_at = five_days_ago;
        let plain = store.save(plain).await.unwrap();

        let mut challenged = Memory::new(
            MemoryType::Fact,
            "challenged fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        challenged.decay_score = 0.9;
        challenged.updated_at = five_days_ago;
        let challenged = store.save(challenged).await.unwrap();

        // Active counter-evidence against `challenged` only.
        let rebuttal = Memory::new(
            MemoryType::Fact,
            "rebuttal".to_string(),
            Priority::Reference,
            agent,
        );
        let rebuttal = store.save(rebuttal).await.unwrap();
        crate::evidence::add_evidence(
            &*store,
            &rebuttal.id,
            crate::evidence::EvidenceKind::Contradicts,
            Some(&challenged.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let dm = DecayManager::new(store.clone(), make_config());
        let report = dm.run_decay().await.unwrap();
        assert_eq!(report.contradicted, 1);

        let plain_after = store.get(&plain.id).await.unwrap().decay_score;
        let challenged_after = store.get(&challenged.id).await.unwrap().decay_score;
        assert!(
            challenged_after < plain_after,
            "contradicted memory ({challenged_after}) must decay faster than plain ({plain_after})"
        );
    }

    #[tokio::test]
    async fn test_superseded_contradiction_does_not_accelerate() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mut challenged = Memory::new(
            MemoryType::Fact,
            "challenged fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        challenged.updated_at = Utc::now() - Duration::days(5);
        let challenged = store.save(challenged).await.unwrap();

        let mut plain = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        plain.updated_at = challenged.updated_at;
        let plain = store.save(plain).await.unwrap();

        let rebuttal = Memory::new(
            MemoryType::Fact,
            "rebuttal".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        let rebuttal = store.save(rebuttal).await.unwrap();
        let correction = Memory::new(
            MemoryType::Fact,
            "correction".to_string(),
            Priority::Reference,
            agent,
        );
        let correction = store.save(correction).await.unwrap();

        crate::evidence::add_evidence(
            &*store,
            &rebuttal.id,
            crate::evidence::EvidenceKind::Contradicts,
            Some(&challenged.id),
            None,
            0.9,
        )
        .await
        .unwrap();
        // Rebuttal itself is superseded → contradiction inactive.
        store.supersede(&rebuttal.id, &correction.id).await.unwrap();

        let dm = DecayManager::new(store.clone(), make_config());
        let report = dm.run_decay().await.unwrap();
        assert_eq!(report.contradicted, 0);

        let plain_after = store.get(&plain.id).await.unwrap().decay_score;
        let challenged_after = store.get(&challenged.id).await.unwrap().decay_score;
        assert!(
            (challenged_after - plain_after).abs() < 0.001,
            "a superseded rebuttal must not accelerate decay"
        );
    }

    #[tokio::test]
    async fn test_contradiction_multiplier_disabled_short_circuits() {
        // contradiction_multiplier = 1.0 disables evidence-driven forgetting:
        // run_decay must not count (or accelerate) contradicted memories.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };
        let five_days_ago = Utc::now() - Duration::days(5);

        let mut challenged = Memory::new(
            MemoryType::Fact,
            "challenged fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        challenged.decay_score = 0.9;
        challenged.updated_at = five_days_ago;
        let challenged = store.save(challenged).await.unwrap();

        let mut plain = Memory::new(
            MemoryType::Fact,
            "plain fact".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        plain.decay_score = 0.9;
        plain.updated_at = five_days_ago;
        let plain = store.save(plain).await.unwrap();

        let rebuttal = Memory::new(
            MemoryType::Fact,
            "rebuttal".to_string(),
            Priority::Reference,
            agent,
        );
        let rebuttal = store.save(rebuttal).await.unwrap();
        crate::evidence::add_evidence(
            &*store,
            &rebuttal.id,
            crate::evidence::EvidenceKind::Contradicts,
            Some(&challenged.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let mut config = make_config();
        config.contradiction_multiplier = 1.0;
        let dm = DecayManager::new(store.clone(), config);
        let report = dm.run_decay().await.unwrap();
        assert_eq!(
            report.contradicted, 0,
            "disabled multiplier must not count contradictions"
        );

        let plain_after = store.get(&plain.id).await.unwrap().decay_score;
        let challenged_after = store.get(&challenged.id).await.unwrap().decay_score;
        assert!(
            (challenged_after - plain_after).abs() < 0.001,
            "disabled acceleration must leave scores equal: {challenged_after} vs {plain_after}"
        );
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
