use std::collections::HashMap;
use tracing::debug;

use crate::models::SearchResult;

pub struct HybridMerger;

impl HybridMerger {
    /// Merge keyword and vector search results using Reciprocal Rank Fusion (RRF).
    /// RRF score = sum(1 / (k + rank_in_list)) for each list the item appears in.
    /// k=60 is the standard constant.
    pub fn merge(
        keyword_results: Vec<SearchResult>,
        vector_results: Vec<SearchResult>,
        top_k: usize,
        keyword_weight: f64,
        vector_weight: f64,
    ) -> Vec<SearchResult> {
        const K: f64 = 60.0;

        let mut rrf_scores: HashMap<String, (f64, Option<SearchResult>)> = HashMap::new();

        // Score keyword results
        for (rank, result) in keyword_results.into_iter().enumerate() {
            let rrf = keyword_weight / (K + rank as f64 + 1.0);
            let entry = rrf_scores
                .entry(result.memory.id.clone())
                .or_insert((0.0, None));
            entry.0 += rrf;
            if entry.1.is_none() {
                entry.1 = Some(result);
            }
        }

        // Score vector results
        for (rank, result) in vector_results.into_iter().enumerate() {
            let rrf = vector_weight / (K + rank as f64 + 1.0);
            let entry = rrf_scores
                .entry(result.memory.id.clone())
                .or_insert((0.0, None));
            entry.0 += rrf;
            if entry.1.is_none() {
                entry.1 = Some(result);
            }
        }

        let mut merged: Vec<SearchResult> = rrf_scores
            .into_values()
            .filter_map(|(score, result)| {
                result.map(|mut r| {
                    r.score = score;
                    r
                })
            })
            .collect();

        // Sort by RRF score descending, MUST always first
        merged.sort_by(|a, b| {
            let a_must = a.memory.priority == crate::models::Priority::Must;
            let b_must = b.memory.priority == crate::models::Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        merged.truncate(top_k);

        debug!(total = merged.len(), "hybrid merge complete");
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_result(id: &str, content: &str, priority: Priority, score: f64) -> SearchResult {
        SearchResult {
            memory: Memory {
                id: id.to_string(),
                memory_type: MemoryType::Fact,
                content: content.to_string(),
                instruction: None,
                priority,
                source_agent: SourceAgent {
                    id: "test".to_string(),
                    agent_type: "general".to_string(),
                    session_id: None,
                },
                namespace: "global".to_string(),
                confidence: 0.8,
                tags: Vec::new(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                ai_generated: false,
                human_reviewed: false,
                decay_score: 1.0,
                access_count: 0,
                last_read_at: None,
                layer: MemoryLayer::L1,
                skill_meta: None,
            },
            score,
        }
    }

    #[test]
    fn test_merge_disjoint() {
        let kw = vec![make_result("a", "keyword match", Priority::Reference, 0.9)];
        let vec_r = vec![make_result("b", "vector match", Priority::Reference, 0.8)];

        let merged = HybridMerger::merge(kw, vec_r, 10, 0.4, 0.6);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_overlap_boosts() {
        let kw = vec![
            make_result("a", "both match", Priority::Reference, 0.9),
            make_result("b", "keyword only", Priority::Reference, 0.7),
        ];
        let vec_r = vec![
            make_result("a", "both match", Priority::Reference, 0.85),
            make_result("c", "vector only", Priority::Reference, 0.6),
        ];

        let merged = HybridMerger::merge(kw, vec_r, 10, 0.4, 0.6);
        // "a" appears in both lists, should have highest RRF score
        assert_eq!(merged[0].memory.id, "a");
    }

    #[test]
    fn test_must_always_first() {
        let kw = vec![make_result("a", "high keyword", Priority::Reference, 0.99)];
        let vec_r = vec![make_result("b", "must rule", Priority::Must, 0.1)];

        let merged = HybridMerger::merge(kw, vec_r, 10, 0.5, 0.5);
        assert_eq!(merged[0].memory.priority, Priority::Must);
    }

    #[test]
    fn test_top_k_limit() {
        let kw: Vec<SearchResult> = (0..20)
            .map(|i| make_result(&format!("kw_{}", i), "kw", Priority::Reference, 0.5))
            .collect();
        let vec_r = Vec::new();

        let merged = HybridMerger::merge(kw, vec_r, 5, 1.0, 0.0);
        assert_eq!(merged.len(), 5);
    }
}
