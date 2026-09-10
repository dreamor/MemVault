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
    /// Minimum similar fact memories before consolidating into one semantic
    /// fact (episodic → semantic distillation, C3)
    pub min_facts_for_consolidation: usize,
    /// Jaccard similarity above which two FACT memories count as the same
    /// repeated fact
    pub fact_similarity: f32,
    /// Jaccard similarity above which two ENTITY memories are merged
    /// (entity normalization, C3). Higher bar than facts: entities are names,
    /// and merging distinct entities corrupts the relation graph.
    pub entity_merge_similarity: f32,
}

impl Default for PromoteConfig {
    fn default() -> Self {
        Self {
            min_l1_for_l2: 3,
            min_l2_for_l3: 2,
            max_age_days: 30,
            min_facts_for_consolidation: 2,
            fact_similarity: 0.7,
            entity_merge_similarity: 0.85,
        }
    }
}

pub struct PromoteResult {
    pub promoted_to_l2: usize,
    pub promoted_to_l3: usize,
    pub source_ids_consumed: Vec<String>,
    /// Repeated facts consolidated into semantic facts (C3).
    pub consolidated_facts: usize,
    /// Duplicate entities merged into a canonical one (C3).
    pub merged_entities: usize,
}

pub struct Promoter {
    store: Arc<dyn MemoryStore>,
    config: PromoteConfig,
}

impl Promoter {
    pub fn new(store: Arc<dyn MemoryStore>, config: PromoteConfig) -> Self {
        Self { store, config }
    }

    /// Run the full promote pipeline. The semantic stages (C3) run FIRST —
    /// they are dedup/normalization passes (consolidate repeated facts, merge
    /// duplicate entities) that must reduce redundancy before the tag-based
    /// L1→L2→L3 grouping promotions run on what remains.
    pub async fn run(&self) -> Result<PromoteResult> {
        let mut result = PromoteResult {
            promoted_to_l2: 0,
            promoted_to_l3: 0,
            source_ids_consumed: Vec::new(),
            consolidated_facts: 0,
            merged_entities: 0,
        };

        // Phase 0a (C3): consolidate repeated facts into semantic facts.
        result.consolidated_facts = self.consolidate_repeated_facts().await?;

        // Phase 0b (C3): merge duplicate entities.
        result.merged_entities = self.merge_duplicate_entities().await?;

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
            consolidated_facts = result.consolidated_facts,
            merged_entities = result.merged_entities,
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
            let key = mem
                .tags
                .first()
                .cloned()
                .unwrap_or_else(|| "general".to_string());
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
            new_mem.source_trace_ids = Self::union_source_trace_ids(members.iter().copied());

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

        let l2_memories: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L2).collect();

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
            new_mem.source_trace_ids = Self::union_source_trace_ids(stable.iter().map(|m| **m));

            self.store.save(new_mem).await?;

            // Demote consumed L2 sources to L0, mirroring promote_l1_to_l2's
            // archival step — otherwise the same L2 group gets re-promoted
            // into a duplicate L3 memory on every subsequent run.
            for id in &source_ids {
                if let Ok(mut m) = self.store.get(id).await {
                    m.layer = MemoryLayer::L0;
                    m.updated_at = Utc::now();
                    let _ = self.store.update(m).await;
                }
            }

            debug!(
                namespace = ns,
                count = stable.len(),
                "promoted L2 group to L3"
            );
            promoted.push((ns.clone(), source_ids));
        }

        Ok(promoted)
    }

    /// Tokenize for fact consolidation. Unlike [`Deduplicator::tokenize`]
    /// this keeps single-character tokens, so trailing digits ("#0" vs "#1")
    /// remain distinguishing — otherwise distinct numbered facts would look
    /// identical and get merged.
    fn consolidation_words(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect()
    }

    /// C3 fact consolidation: the same fact stated across several memories
    /// (typically distilled from different episodes) is consolidated into ONE
    /// L2 semantic fact with raised confidence; the sources are archived to L0
    /// and linked back via `consolidated_from` relations (provenance).
    async fn consolidate_repeated_facts(&self) -> Result<usize> {
        use crate::dedup::Deduplicator;

        let all = self.store.list(None, 500, 0).await?;
        let facts: Vec<&Memory> = all
            .iter()
            .filter(|m| {
                m.memory_type == MemoryType::Fact
                    && matches!(m.layer, MemoryLayer::L1 | MemoryLayer::L2)
                    && m.superseded_by.is_none()
            })
            .collect();
        if facts.len() < self.config.min_facts_for_consolidation {
            return Ok(0);
        }

        // Greedy clustering by Jaccard similarity.
        let words: Vec<Vec<String>> = facts
            .iter()
            .map(|m| Self::consolidation_words(&m.content))
            .collect();
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for (i, w) in words.iter().enumerate() {
            let mut placed = false;
            for cluster in clusters.iter_mut() {
                let rep = cluster[0];
                if Deduplicator::jaccard_similarity(w, &words[rep]) >= self.config.fact_similarity {
                    cluster.push(i);
                    placed = true;
                    break;
                }
            }
            if !placed {
                clusters.push(vec![i]);
            }
        }

        let mut consolidated = 0usize;
        for cluster in clusters {
            if cluster.len() < self.config.min_facts_for_consolidation {
                continue;
            }
            let members: Vec<&Memory> = cluster.iter().map(|&i| facts[i]).collect();

            // Representative content: the longest member (most specific).
            let representative = members
                .iter()
                .max_by_key(|m| m.content.chars().count())
                .expect("cluster is non-empty");

            let confidence = members.iter().map(|m| m.confidence).fold(0.0f64, f64::max) + 0.05;

            let mut tags: Vec<String> = Vec::new();
            for m in &members {
                for t in &m.tags {
                    if !tags.contains(t) {
                        tags.push(t.clone());
                    }
                }
            }

            let mut new_mem = Memory::new(
                MemoryType::Fact,
                representative.content.clone(),
                Priority::Reference,
                SourceAgent {
                    id: "promote-pipeline".to_string(),
                    agent_type: "system".to_string(),
                    session_id: None,
                },
            );
            new_mem.layer = MemoryLayer::L2;
            new_mem.namespace = representative.namespace.clone();
            new_mem.tags = tags;
            new_mem.ai_generated = true;
            new_mem.human_reviewed = false;
            new_mem.confidence = confidence.min(0.95);
            new_mem.source_trace_ids = Self::union_source_trace_ids(members.iter().copied());

            let saved = self.store.save(new_mem).await?;

            // Provenance: link every source fact to the consolidated fact.
            for m in &members {
                let rel = MemoryRelation {
                    relation_id: None,
                    subject_id: saved.id.clone(),
                    predicate: "consolidated_from".to_string(),
                    object_id: Some(m.id.clone()),
                    object_text: None,
                    confidence: 1.0,
                    source_memory_id: None,
                    created_at: Utc::now(),
                };
                if let Err(e) = self.store.add_relation(rel).await {
                    debug!(error = %e, "failed to record consolidation provenance");
                }
            }

            // Archive the sources.
            for m in &members {
                if let Ok(mut src) = self.store.get(&m.id).await {
                    src.layer = MemoryLayer::L0;
                    src.updated_at = Utc::now();
                    let _ = self.store.update(src).await;
                }
            }

            debug!(
                count = members.len(),
                consolidated_id = %saved.id,
                "consolidated repeated facts into semantic fact"
            );
            consolidated += 1;
        }
        Ok(consolidated)
    }

    /// C3 entity normalization: near-identical ENTITY memories are merged
    /// into one canonical entity — relations of the duplicates are re-pointed
    /// to the canonical, then the duplicates are marked superseded (archived,
    /// never deleted, so history stays restorable).
    async fn merge_duplicate_entities(&self) -> Result<usize> {
        use crate::dedup::Deduplicator;

        let all = self.store.list(None, 500, 0).await?;
        let entities: Vec<&Memory> = all
            .iter()
            .filter(|m| {
                m.memory_type == MemoryType::Entity
                    && m.layer != MemoryLayer::L0
                    && m.superseded_by.is_none()
            })
            .collect();
        if entities.len() < 2 {
            return Ok(0);
        }

        let words: Vec<Vec<String>> = entities
            .iter()
            .map(|m| Deduplicator::tokenize(&m.content))
            .collect();

        let mut merged = 0usize;
        let mut absorbed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for i in 0..entities.len() {
            if absorbed.contains(&i) {
                continue;
            }
            for j in (i + 1)..entities.len() {
                if absorbed.contains(&j) {
                    continue;
                }
                let sim = Deduplicator::jaccard_similarity(&words[i], &words[j]);
                if sim < self.config.entity_merge_similarity {
                    continue;
                }

                // Canonical = the more-connected entity (it has more graph
                // context to preserve); ties keep the earlier one.
                let rels_i = self.relation_degree(&entities[i].id).await;
                let rels_j = self.relation_degree(&entities[j].id).await;
                let (canon_idx, dup_idx) = if rels_j > rels_i { (j, i) } else { (i, j) };
                let canonical = entities[canon_idx];
                let duplicate = entities[dup_idx];

                // Re-point the duplicate's relations to the canonical entity,
                // skipping triples the canonical already has.
                let existing = self
                    .store
                    .relations_of_subject(&canonical.id)
                    .await
                    .unwrap_or_default();
                for rel in self
                    .store
                    .relations_of_subject(&duplicate.id)
                    .await
                    .unwrap_or_default()
                {
                    let dup = existing.iter().any(|e| {
                        e.predicate == rel.predicate
                            && e.object_id == rel.object_id
                            && e.object_text == rel.object_text
                    });
                    if dup {
                        if let Some(rid) = rel.relation_id {
                            let _ = self.store.delete_relation(rid).await;
                        }
                        continue;
                    }
                    let moved = MemoryRelation {
                        relation_id: None,
                        subject_id: canonical.id.clone(),
                        predicate: rel.predicate,
                        object_id: rel.object_id,
                        object_text: rel.object_text,
                        confidence: rel.confidence,
                        source_memory_id: rel.source_memory_id,
                        created_at: rel.created_at,
                    };
                    if let Some(rid) = rel.relation_id {
                        let _ = self.store.delete_relation(rid).await;
                    }
                    let _ = self.store.add_relation(moved).await;
                }
                // Same for relations where the duplicate is the object.
                for rel in self
                    .store
                    .relations_of_object(&duplicate.id)
                    .await
                    .unwrap_or_default()
                {
                    let moved = MemoryRelation {
                        relation_id: None,
                        subject_id: rel.subject_id,
                        predicate: rel.predicate,
                        object_id: Some(canonical.id.clone()),
                        object_text: None,
                        confidence: rel.confidence,
                        source_memory_id: rel.source_memory_id,
                        created_at: rel.created_at,
                    };
                    if let Some(rid) = rel.relation_id {
                        let _ = self.store.delete_relation(rid).await;
                    }
                    let _ = self.store.add_relation(moved).await;
                }

                // Carry the duplicate's raw-evidence provenance onto the
                // canonical survivor so merging entities does not drop the
                // trace chain. Only touch the canonical when it actually
                // gains ids, to keep its `updated_at` stable otherwise.
                if let Ok(mut canon_mem) = self.store.get(&canonical.id).await {
                    let mut gained = false;
                    for id in &duplicate.source_trace_ids {
                        if !canon_mem.source_trace_ids.contains(id) {
                            canon_mem.source_trace_ids.push(id.clone());
                            gained = true;
                        }
                    }
                    if gained {
                        canon_mem.updated_at = Utc::now();
                        let _ = self.store.update(canon_mem).await;
                    }
                }

                // Mark the duplicate superseded by the canonical + archive.
                if let Ok(mut dup_mem) = self.store.get(&duplicate.id).await {
                    dup_mem.superseded_by = Some(canonical.id.clone());
                    dup_mem.layer = MemoryLayer::L0;
                    dup_mem.updated_at = Utc::now();
                    let _ = self.store.update(dup_mem).await;
                }

                absorbed.insert(dup_idx);
                merged += 1;
                debug!(
                    canonical = %canonical.id,
                    duplicate = %duplicate.id,
                    similarity = sim,
                    "merged duplicate entities"
                );
            }
        }
        Ok(merged)
    }

    /// Number of relations touching a memory (outgoing + incoming), used to
    /// pick the canonical entity when merging duplicates.
    async fn relation_degree(&self, memory_id: &str) -> usize {
        let out = self
            .store
            .relations_of_subject(memory_id)
            .await
            .map(|r| r.len())
            .unwrap_or(0);
        let inbound = self
            .store
            .relations_of_object(memory_id)
            .await
            .map(|r| r.len())
            .unwrap_or(0);
        out + inbound
    }

    /// Order-stable union of the members' `source_trace_ids`: iterate the
    /// members in order and append each id the first time it is seen. The
    /// dedup is deliberate (a shared raw-evidence row must not appear twice)
    /// and the first-seen ordering keeps promoted provenance deterministic
    /// for tests.
    fn union_source_trace_ids<'a>(members: impl IntoIterator<Item = &'a Memory>) -> Vec<String> {
        let mut merged: Vec<String> = Vec::new();
        for m in members {
            for id in &m.source_trace_ids {
                if !merged.contains(id) {
                    merged.push(id.clone());
                }
            }
        }
        merged
    }

    /// Merge multiple L1 atomic facts into a consolidated L2 description.
    fn consolidate_l1(memories: &[&Memory]) -> String {
        let mut parts: Vec<&str> = memories.iter().map(|m| m.content.as_str()).collect();
        parts.sort();
        parts.dedup();

        if parts.len() == 1 {
            return parts[0].to_string();
        }

        let joined = parts.join("; ");
        if joined.len() <= 200 {
            joined
        } else {
            // Truncate on a UTF-8 char boundary: byte-slicing at a fixed offset
            // panics when the cut lands inside a multi-byte char (CJK content).
            let mut end = 197;
            while !joined.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{} (consolidated from {} atomic facts)",
                &joined[..end],
                memories.len()
            )
        }
    }

    /// Merge multiple L2 scenario memories into a L3 persona statement.
    fn consolidate_l2(memories: &[&&Memory]) -> String {
        let parts: Vec<&str> = memories
            .iter()
            .map(|m| m.instruction.as_deref().unwrap_or(m.content.as_str()))
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

    /// Regression: `consolidate_l1` used to slice `&joined[..197]` on a
    /// byte boundary. With CJK content (>200 bytes) that offset can land in
    /// the middle of a multi-byte char and panic.
    #[test]
    fn test_consolidate_l1_truncates_on_char_boundary_with_cjk() {
        // 66 CJK chars = 198 bytes; 80 CJK chars = 240 bytes. Joined length
        // exceeds 200 and byte 197 lands inside the first (all-CJK) member,
        // NOT on a character boundary.
        let m1 = Memory::new(
            MemoryType::Fact,
            "中".repeat(66),
            Priority::Reference,
            make_agent(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "编".repeat(80),
            Priority::Reference,
            make_agent(),
        );
        let refs: Vec<&Memory> = vec![&m1, &m2];

        let out = Promoter::consolidate_l1(&refs);

        assert!(out.ends_with("(consolidated from 2 atomic facts)"));
        // The truncated prefix must be valid UTF-8 (would have panicked before the fix).
        let prefix = out.trim_end_matches(" (consolidated from 2 atomic facts)");
        assert!(!prefix.is_empty());
        assert!(
            prefix.chars().count() < 67,
            "prefix should be truncated to <200 bytes"
        );
    }

    /// Boundary: content exactly at the 200-byte limit is returned as-is.
    #[test]
    fn test_consolidate_l1_keeps_content_at_byte_limit() {
        let m1 = Memory::new(
            MemoryType::Fact,
            "中".repeat(66),
            Priority::Reference,
            make_agent(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "ab".to_string(),
            Priority::Reference,
            make_agent(),
        );
        let refs: Vec<&Memory> = vec![&m1, &m2];

        // joined = 198 + 2 ("; ") + 2 = 202 bytes → truncation path; assert no panic.

        let joined = Promoter::consolidate_l1(&refs);
        // Sorting is byte-lexicographic: "ab…" < "中…", so "ab" sorts first.
        // Regardless of ordering, result must be valid UTF-8.
        assert!(!joined.is_empty());
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
    async fn test_promote_l2_to_l3_is_idempotent() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

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
        let first = promoter.run().await.unwrap();
        assert_eq!(first.promoted_to_l3, 1);

        // Running the pipeline again on the same store must not re-consume
        // the (now-demoted) L2 sources into a second, duplicate L3 memory.
        let second = promoter.run().await.unwrap();
        assert_eq!(
            second.promoted_to_l3, 0,
            "sources demoted to L0 must not be re-promoted"
        );

        let all = store.list(None, 100, 0).await.unwrap();
        let l3: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L3).collect();
        assert_eq!(
            l3.len(),
            1,
            "only one L3 memory should exist after re-running promote"
        );
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

        assert_eq!(
            result.promoted_to_l2, 0,
            "L0 memories must not be promoted to L2"
        );

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

        assert_eq!(
            result.promoted_to_l2, 0,
            "memories older than the cutoff must be excluded"
        );
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

        assert_eq!(
            result.promoted_to_l3, 2,
            "each namespace should produce its own L3 memory"
        );

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

    #[tokio::test]
    async fn test_consolidate_repeated_facts() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Two near-identical facts (differ only in trailing noise) → one
        // consolidated semantic fact.
        for content in [
            "the checkout service uses PostgreSQL 15",
            "the checkout service uses PostgreSQL 15.",
        ] {
            let mut m = Memory::new(
                MemoryType::Fact,
                content.to_string(),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            store.save(m).await.unwrap();
        }
        // An unrelated fact must survive untouched.
        let mut other = Memory::new(
            MemoryType::Fact,
            "the team ships on tuesdays".to_string(),
            Priority::Reference,
            make_agent(),
        );
        other.layer = MemoryLayer::L1;
        store.save(other.clone()).await.unwrap();

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();
        assert_eq!(result.consolidated_facts, 1);

        let all = store.list(None, 100, 0).await.unwrap();
        // The consolidated fact is L2 and carries provenance relations.
        let consolidated: Vec<&Memory> = all
            .iter()
            .filter(|m| {
                m.memory_type == MemoryType::Fact
                    && m.layer == MemoryLayer::L2
                    && m.content.contains("PostgreSQL")
            })
            .collect();
        assert_eq!(consolidated.len(), 1);
        let provenance = store
            .relations_of_subject(&consolidated[0].id)
            .await
            .unwrap();
        assert_eq!(provenance.len(), 2, "both sources must be linked");
        assert!(
            provenance
                .iter()
                .all(|r| r.predicate == "consolidated_from")
        );

        // Sources archived to L0.
        let archived = all
            .iter()
            .filter(|m| m.layer == MemoryLayer::L0 && m.content.contains("PostgreSQL"))
            .count();
        assert_eq!(archived, 2);

        // The unrelated fact is untouched (still L1).
        assert!(
            all.iter()
                .any(|m| m.id == other.id && m.layer == MemoryLayer::L1)
        );
    }

    #[tokio::test]
    async fn test_merge_duplicate_entities() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        // Two near-identical entities. The one WITH a relation is the more
        // established node and must become the canonical survivor.
        let mut bare = Memory::new(
            MemoryType::Entity,
            "payment service".to_string(),
            Priority::Background,
            make_agent(),
        );
        bare.layer = MemoryLayer::L2;
        let bare_id = bare.id.clone();
        store.save(bare).await.unwrap();

        let mut connected = Memory::new(
            MemoryType::Entity,
            "payment service.".to_string(),
            Priority::Background,
            make_agent(),
        );
        connected.layer = MemoryLayer::L2;
        let connected_id = connected.id.clone();
        store.save(connected).await.unwrap();

        store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: connected_id.clone(),
                predicate: "depends_on".to_string(),
                object_id: None,
                object_text: Some("stripe api".to_string()),
                confidence: 0.8,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap();

        let promoter = Promoter::new(store.clone(), PromoteConfig::default());
        let result = promoter.run().await.unwrap();
        assert_eq!(result.merged_entities, 1);

        // The relation-bearing entity survives as canonical; the bare one is
        // superseded by it and archived.
        let bare_mem = store.get(&bare_id).await.unwrap();
        assert_eq!(
            bare_mem.superseded_by.as_deref(),
            Some(connected_id.as_str())
        );
        assert_eq!(bare_mem.layer, MemoryLayer::L0);
        let connected_mem = store.get(&connected_id).await.unwrap();
        assert!(connected_mem.superseded_by.is_none());

        // The relation stays on the canonical entity.
        let rels = store.relations_of_subject(&connected_id).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].predicate, "depends_on");
        assert_eq!(rels[0].object_text.as_deref(), Some("stripe api"));
        assert!(
            store
                .relations_of_subject(&bare_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Raw-evidence provenance must survive L1→L2 consolidation: the surviving
    /// L2 memory inherits the order-stable deduplicated union of every group
    /// member's `source_trace_ids`.
    #[tokio::test]
    async fn test_l1_to_l2_unions_source_trace_ids() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        for (i, trace) in ["trace_a", "trace_b"].iter().enumerate() {
            // Deliberately dissimilar content so the C3 fact-consolidation
            // pass (phase 0a) does not pre-consume the pair and leave the
            // L1→L2 stage empty.
            let content = if i == 0 {
                "the checkout service uses postgres"
            } else {
                "the team ships releases on tuesdays"
            };
            let mut m = Memory::new(
                MemoryType::Fact,
                content.to_string(),
                Priority::Reference,
                make_agent(),
            );
            m.layer = MemoryLayer::L1;
            m.tags = vec!["rust".to_string()];
            m.source_trace_ids = vec![trace.to_string()];
            store.save(m).await.unwrap();
        }

        // Capture the order the promoter will observe, then derive the
        // expected union from it — makes the assertion about order-stable
        // dedup, not about SQLite's row ordering.
        let before = store.list(None, 100, 0).await.unwrap();
        let expected: Vec<String> = before
            .iter()
            .filter(|m| m.layer == MemoryLayer::L1)
            .flat_map(|m| m.source_trace_ids.clone())
            .collect();
        assert_eq!(expected.len(), 2, "both members carry provenance");

        let config = PromoteConfig {
            min_l1_for_l2: 2,
            ..PromoteConfig::default()
        };
        let promoter = Promoter::new(store.clone(), config);
        let result = promoter.run().await.unwrap();
        assert_eq!(result.promoted_to_l2, 1);

        let all = store.list(None, 100, 0).await.unwrap();
        let l2: Vec<&Memory> = all.iter().filter(|m| m.layer == MemoryLayer::L2).collect();
        assert_eq!(l2.len(), 1);
        assert_eq!(
            l2[0].source_trace_ids, expected,
            "L2 must inherit the order-stable union of its members' provenance"
        );
        assert!(l2[0].source_trace_ids.contains(&"trace_a".to_string()));
        assert!(l2[0].source_trace_ids.contains(&"trace_b".to_string()));
    }

    /// Dedup is order-stable and drops ids shared by multiple members.
    #[test]
    fn test_union_source_trace_ids_is_order_stable_and_deduped() {
        let mut a = Memory::new(
            MemoryType::Fact,
            "a".to_string(),
            Priority::Reference,
            make_agent(),
        );
        a.source_trace_ids = vec!["t1".to_string(), "shared".to_string()];
        let mut b = Memory::new(
            MemoryType::Fact,
            "b".to_string(),
            Priority::Reference,
            make_agent(),
        );
        b.source_trace_ids = vec!["shared".to_string(), "t2".to_string()];
        let mut c = Memory::new(
            MemoryType::Fact,
            "c".to_string(),
            Priority::Reference,
            make_agent(),
        );
        c.source_trace_ids = vec!["t3".to_string()];

        let merged = Promoter::union_source_trace_ids([&a, &b, &c]);
        assert_eq!(
            merged,
            vec![
                "t1".to_string(),
                "shared".to_string(),
                "t2".to_string(),
                "t3".to_string()
            ]
        );
    }
}
