use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info};

use crate::error::Result;
use crate::models::*;
use crate::storage::MemoryStore;

pub struct PromoteConfig {
    /// Minimum L1 memories in a group before consolidating to L2
    pub min_l1_for_l2: usize,
    /// Minimum L2 memories in a group before promoting to L3
    pub min_l2_for_l3: usize,
    /// Maximum age (days) of L1 memories to consider for promotion
    pub max_age_days: i64,
}

impl Default for PromoteConfig {
    fn default() -> Self {
        Self {
            min_l1_for_l2: 3,
            min_l2_for_l3: 2,
            max_age_days: 30,
        }
    }
}

pub struct PromoteResult {
    pub promoted_to_l2: usize,
    pub promoted_to_l3: usize,
    pub source_ids_consumed: Vec<String>,
}

pub struct Promoter {
    store: Arc<dyn MemoryStore>,
    config: PromoteConfig,
}

impl Promoter {
    pub fn new(store: Arc<dyn MemoryStore>, config: PromoteConfig) -> Self {
        Self { store, config }
    }

    /// Run the full promote pipeline: L1→L2, then L2→L3.
    pub async fn run(&self) -> Result<PromoteResult> {
        let mut result = PromoteResult {
            promoted_to_l2: 0,
            promoted_to_l3: 0,
            source_ids_consumed: Vec::new(),
        };

        // Phase 1: promote L1 → L2
        let l2_promoted = self.promote_l1_to_l2().await?;
        result.promoted_to_l2 = l2_promoted.len();
        for (_, ids) in &l2_promoted {
            result.source_ids_consumed.extend(ids.clone());
        }

        // Phase 2: promote L2 → L3
        let l3_promoted = self.promote_l2_to_l3().await?;
        result.promoted_to_l3 = l3_promoted.len();
        for (_, ids) in &l3_promoted {
            result.source_ids_consumed.extend(ids.clone());
        }

        info!(
            l2 = result.promoted_to_l2,
            l3 = result.promoted_to_l3,
            consumed = result.source_ids_consumed.len(),
            "promote pipeline complete"
        );

        Ok(result)
    }

    /// Group L1 memories by primary tag, consolidate groups with enough members into L2.
    async fn promote_l1_to_l2(&self) -> Result<Vec<(String, Vec<String>)>> {
        let all = self.store.list(None, 500, 0).await?;
        let cutoff = Utc::now() - chrono::Duration::days(self.config.max_age_days);

        let l1_memories: Vec<&Memory> = all
            .iter()
            .filter(|m| m.layer == MemoryLayer::L1 && m.created_at >= cutoff)
            .collect();

        if l1_memories.is_empty() {
            return Ok(Vec::new());
        }

        // Group by primary tag (first tag, or "general")
        let mut groups: HashMap<String, Vec<&Memory>> = HashMap::new();
        for mem in &l1_memories {
            let key = mem.tags.first().cloned().unwrap_or_else(|| "general".to_string());
            groups.entry(key).or_default().push(mem);
        }

        let mut promoted = Vec::new();

        for (tag, members) in &groups {
            if members.len() < self.config.min_l1_for_l2 {
                continue;
            }

            // Consolidate: merge content into a scenario-level summary
            let consolidated_content = Self::consolidate_l1(members);
            let source_ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();

            let mut new_mem = Memory::new(
                MemoryType::Fact,
                consolidated_content,
                Priority::Reference,
                SourceAgent {
                    id: "promote-pipeline".to_string(),
                    agent_type: "system".to_string(),
                    session_id: None,
                },
            );
            new_mem.layer = MemoryLayer::L2;
            new_mem.tags = vec![tag.clone()];
            new_mem.ai_generated = true;
            new_mem.confidence = 0.7;

            self.store.save(new_mem).await?;

            // Mark source L1 memories as promoted (update layer to L0 = archived raw)
            for id in &source_ids {
                if let Ok(mut m) = self.store.get(id).await {
                    m.layer = MemoryLayer::L0;
                    m.updated_at = Utc::now();
                    let _ = self.store.update(m).await;
                }
            }

            debug!(tag, count = members.len(), "promoted L1 group to L2");
            promoted.push((tag.clone(), source_ids));
        }

        Ok(promoted)
    }

    /// Group L2 memories by namespace, promote stable patterns into L3 persona.
    async fn promote_l2_to_l3(&self) -> Result<Vec<(String, Vec<String>)>> {
        let all = self.store.list(None, 500, 0).await?;

        let l2_memories: Vec<&Memory> = all
            .iter()
            .filter(|m| m.layer == MemoryLayer::L2)
            .collect();

        if l2_memories.is_empty() {
            return Ok(Vec::new());
        }

        // Group by namespace
        let mut groups: HashMap<String, Vec<&Memory>> = HashMap::new();
        for mem in &l2_memories {
            groups.entry(mem.namespace.clone()).or_default().push(mem);
        }

        let mut promoted = Vec::new();

        for (ns, members) in &groups {
            if members.len() < self.config.min_l2_for_l3 {
                continue;
            }

            // Only promote preference-type or high-access memories to L3
            let stable: Vec<&&Memory> = members
                .iter()
                .filter(|m| m.access_count >= 2 || m.memory_type == MemoryType::Preference)
                .collect();

            if stable.len() < self.config.min_l2_for_l3 {
                continue;
            }

            let persona_content = Self::consolidate_l2(&stable);
            let source_ids: Vec<String> = stable.iter().map(|m| m.id.clone()).collect();

            let mut new_mem = Memory::new(
                MemoryType::Preference,
                persona_content.clone(),
                Priority::Must,
                SourceAgent {
                    id: "promote-pipeline".to_string(),
                    agent_type: "system".to_string(),
                    session_id: None,
                },
            );
            new_mem.layer = MemoryLayer::L3;
            new_mem.namespace = ns.clone();
            new_mem.instruction = Some(persona_content);
            new_mem.ai_generated = true;
            new_mem.confidence = 0.8;

            self.store.save(new_mem).await?;

            debug!(namespace = ns, count = stable.len(), "promoted L2 group to L3");
            promoted.push((ns.clone(), source_ids));
        }

        Ok(promoted)
    }

    /// Merge multiple L1 atomic facts into a consolidated L2 description.
    fn consolidate_l1(memories: &[&Memory]) -> String {
        let mut parts: Vec<&str> = memories
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        parts.sort();
        parts.dedup();

        if parts.len() == 1 {
            return parts[0].to_string();
        }

        let joined = parts.join("; ");
        if joined.len() <= 200 {
            joined
        } else {
            format!(
                "{} (consolidated from {} atomic facts)",
                &joined[..197],
                memories.len()
            )
        }
    }

    /// Merge multiple L2 scenario memories into a L3 persona statement.
    fn consolidate_l2(memories: &[&&Memory]) -> String {
        let parts: Vec<&str> = memories
            .iter()
            .map(|m| {
                m.instruction
                    .as_deref()
                    .unwrap_or(m.content.as_str())
            })
            .collect();

        if parts.len() == 1 {
            return parts[0].to_string();
        }

        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    fn make_agent() -> SourceAgent {
        SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn test_promote_l1_to_l2() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Create 4 L1 memories with same tag → should promote
        for i in 0..4 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("fact about coding #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            m.tags = vec!["coding".to_string()];
            store.save(m).await.unwrap();
        }

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l2, 1);
        assert_eq!(result.source_ids_consumed.len(), 4);

        // Verify: new L2 memory exists
        let all = store.list(None, 100, 0).await.unwrap();
        let l2: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L2).collect();
        assert_eq!(l2.len(), 1);
        assert!(l2[0].content.contains("coding"));
    }

    #[tokio::test]
    async fn test_promote_l2_to_l3() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Create 3 L2 preference memories with access_count >= 2
        for i in 0..3 {
            let mut m = Memory::new(
                MemoryType::Preference,
                format!("user prefers style #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L2;
            m.access_count = 3;
            m.tags = vec!["style".to_string()];
            store.save(m).await.unwrap();
        }

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l3, 1);

        let all = store.list(None, 100, 0).await.unwrap();
        let l3: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L3).collect();
        assert_eq!(l3.len(), 1);
        assert_eq!(l3[0].priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_no_promote_below_threshold() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Only 2 L1 memories (below default min_l1_for_l2 = 3)
        for i in 0..2 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("fact #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            m.tags = vec!["coding".to_string()];
            store.save(m).await.unwrap();
        }

        let promoter = Promoter::new(store, PromoteConfig::default());
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l2, 0);
        assert_eq!(result.promoted_to_l3, 0);
    }

    #[tokio::test]
    async fn test_source_memories_demoted_to_l0() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut ids = Vec::new();

        for i in 0..3 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("fact #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            m.tags = vec!["coding".to_string()];
            let saved = store.save(m).await.unwrap();
            ids.push(saved.id);
        }

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        promoter.run().await.unwrap();

        // Source memories should now be L0 (archived)
        for id in &ids {
            let m = store.get(id).await.unwrap();
            assert_eq!(m.layer, MemoryLayer::L0);
        }
    }

    #[tokio::test]
    async fn test_l0_memories_excluded_from_l1_to_l2_promotion() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Save memories already at L0 -- these should never be picked up
        // as L1→L2 promotion candidates, even if they'd otherwise satisfy
        // the tag/threshold grouping.
        for i in 0..5 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("archived fact #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L0;
            m.tags = vec!["coding".to_string()];
            store.save(m).await.unwrap();
        }

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l2, 0, "L0 memories must not be promoted to L2");

        let all = store.list(None, 100, 0).await.unwrap();
        assert!(all.iter().all(|m| m.layer == MemoryLayer::L0));
    }

    #[tokio::test]
    async fn test_l1_to_l2_respects_max_age_cutoff() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // These L1 memories are "fresh" by created_at (just saved), so with
        // a max_age_days of 0 the cutoff is "now" and they should all be
        // considered too old (created_at < cutoff means excluded; created_at
        // == now with cutoff == now is a boundary, so use a negative window
        // to unambiguously exclude everything).
        for i in 0..5 {
            let mut m = Memory::new(
                MemoryType::Fact,
                format!("fact #{}", i),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            m.tags = vec!["coding".to_string()];
            store.save(m).await.unwrap();
        }

        let config = PromoteConfig {
            max_age_days: -1, // cutoff is in the future -> nothing qualifies as "recent enough"
            ..PromoteConfig::default()
        };
        let promoter = Promoter::new(store.clone(), config);
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l2, 0, "memories older than the cutoff must be excluded");
    }

    #[tokio::test]
    async fn test_l2_to_l3_does_not_merge_across_namespaces() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Two namespaces, each with enough L2 preference memories to clear
        // min_l2_for_l3 on their own -- they must NOT be merged into a
        // single L3 memory.
        for ns in ["project:alpha", "project:beta"] {
            for i in 0..2 {
                let mut m = Memory::new(
                    MemoryType::Preference,
                    format!("{} preference #{}", ns, i),
                    Priority::Reference,
                    make_agent(),
                );
                m.layer = MemoryLayer::L2;
                m.namespace = ns.to_string();
                m.access_count = 3;
                store.save(m).await.unwrap();
            }
        }

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();

        assert_eq!(result.promoted_to_l3, 2, "each namespace should produce its own L3 memory");

        let all = store.list(None, 100, 0).await.unwrap();
        let l3: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L3).collect();
        assert_eq!(l3.len(), 2);
        let namespaces: std::collections::HashSet<&str> =
            l3.iter().map(|m| m.namespace.as_str()).collect();
        assert!(namespaces.contains("project:alpha"));
        assert!(namespaces.contains("project:beta"));
        // Content from one namespace must not leak into the other's L3 memory.
        for mem in &l3 {
            assert!(mem.content.contains(&mem.namespace));
        }
    }
}
