use std::collections::HashMap;
use tracing::debug;

use crate::models::{HitSource, SearchResult};

pub struct HybridMerger;

/// Per-id accumulator while fusing the ranked lists.
struct FusedEntry {
    score: f64,
    sources: Vec<HitSource>,
    /// Agreement and RRF are per input channel, not per provenance label.
    recalled: [bool; 2],
    /// The first result object seen for this id carries the memory payload.
    result: Option<SearchResult>,
}

impl HybridMerger {
    /// Merge keyword and vector search results using Reciprocal Rank Fusion (RRF).
    /// RRF score = sum(1 / (k + rank_in_list)) for each list the item appears in.
    /// k=60 is the standard constant.
    ///
    /// Every merged result records WHICH list(s) recalled it and at what rank
    /// (`hit_sources`) — fusion throws that information away by default, and
    /// it is exactly what "why is this ranked first?" and injection auditing
    /// need.
    pub fn merge(
        keyword_results: Vec<SearchResult>,
        vector_results: Vec<SearchResult>,
        top_k: usize,
        keyword_weight: f64,
        vector_weight: f64,
    ) -> Vec<SearchResult> {
        const K: f64 = 60.0;

        let mut fused: HashMap<String, FusedEntry> = HashMap::new();

        for (channel, (results, weight)) in [
            (keyword_results, keyword_weight),
            (vector_results, vector_weight),
        ]
        .into_iter()
        .enumerate()
        {
            for (idx, result) in results.into_iter().enumerate() {
                let rank = idx + 1;
                let entry = fused.entry(result.memory.id.clone()).or_insert(FusedEntry {
                    score: 0.0,
                    sources: Vec::new(),
                    recalled: [false; 2],
                    result: None,
                });
                if !entry.recalled[channel] {
                    entry.score += weight / (K + rank as f64);
                    entry.recalled[channel] = true;
                }

                // Keep path-local ranks, particularly the historical keyword
                // rank, rather than relabeling that hit as current keyword.
                for source in &result.hit_sources {
                    if !entry.sources.contains(source) {
                        entry.sources.push(*source);
                    }
                }
                let has_channel_source = result.hit_sources.iter().any(|source| {
                    if channel == 0 {
                        matches!(
                            source,
                            HitSource::Keyword { .. } | HitSource::HistoryKeyword { .. }
                        )
                    } else {
                        matches!(source, HitSource::Vector { .. })
                    }
                });
                if !has_channel_source {
                    let source = if channel == 0 {
                        HitSource::Keyword { rank }
                    } else {
                        HitSource::Vector { rank }
                    };
                    if !entry.sources.contains(&source) {
                        entry.sources.push(source);
                    }
                }
                if entry.result.is_none() {
                    entry.result = Some(result);
                }
            }
        }

        let mut merged: Vec<(SearchResult, usize)> = fused
            .into_values()
            .filter_map(|entry| {
                entry.result.map(|mut r| {
                    r.score = entry.score;
                    r.hit_sources = entry.sources;
                    (
                        r,
                        entry.recalled.iter().filter(|&&recalled| recalled).count(),
                    )
                })
            })
            .collect();

        // Sort: MUST always first, then RRF score descending. Ties are broken
        // by number of input channels (multiple labels in one channel are
        // not independent agreement) and finally by id, so the order is
        // DETERMINISTIC — hash-map iteration order must never leak into rankings.
        merged.sort_by(|(a, a_channels), (b, b_channels)| {
            let a_must = a.memory.priority == crate::models::Priority::Must;
            let b_must = b.memory.priority == crate::models::Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b_channels.cmp(a_channels))
                    .then_with(|| a.memory.id.cmp(&b.memory.id)),
            }
        });

        merged.truncate(top_k);

        debug!(total = merged.len(), "hybrid merge complete");
        merged.into_iter().map(|(result, _)| result).collect()
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
                superseded_by: None,
                visibility: Visibility::Scoped,
                identity_verified: false,
                corroborating_agents: Vec::new(),
                occurred_at: None,
                source_trace_ids: Vec::new(),
                friction_evidence: None,
                archived_at: None,
                expires_at: None,
                proposes_supersede_of: None,
                fact_assertion: None,
                fact_management: None,
            },
            score,
            hit_sources: Vec::new(),
            as_of_source: None,
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
    fn test_merge_top_k_zero_returns_empty() {
        // top_k=0 means "return nothing" — even a MUST-priority memory must
        // not survive the merge, or hybrid mode would leak rows the caller
        // explicitly capped at zero.
        let kw = vec![
            make_result("a", "must see", Priority::Must, 0.95),
            make_result("b", "keyword match", Priority::Reference, 0.9),
        ];
        let vec_r = vec![make_result("a", "must see", Priority::Must, 0.85)];

        let merged = HybridMerger::merge(kw, vec_r, 0, 0.4, 0.6);
        assert!(
            merged.is_empty(),
            "merge with top_k=0 must return zero rows, got {}",
            merged.len()
        );
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
    fn test_merge_records_hit_sources() {
        let kw = vec![
            make_result("a", "both match", Priority::Reference, 0.9),
            make_result("b", "keyword only", Priority::Reference, 0.7),
        ];
        let vec_r = vec![
            make_result("a", "both match", Priority::Reference, 0.85),
            make_result("c", "vector only", Priority::Reference, 0.6),
        ];

        let merged = HybridMerger::merge(kw, vec_r, 10, 0.5, 0.5);

        let a = merged.iter().find(|r| r.memory.id == "a").unwrap();
        assert_eq!(a.hit_sources.len(), 2, "overlap must keep both paths");
        assert!(a.hit_sources.contains(&HitSource::Keyword { rank: 1 }));
        assert!(a.hit_sources.contains(&HitSource::Vector { rank: 1 }));

        let b = merged.iter().find(|r| r.memory.id == "b").unwrap();
        assert_eq!(b.hit_sources, vec![HitSource::Keyword { rank: 2 }]);

        let c = merged.iter().find(|r| r.memory.id == "c").unwrap();
        assert_eq!(c.hit_sources, vec![HitSource::Vector { rank: 2 }]);
    }

    /// Equal RRF scores must not fall back to hash-map iteration order:
    /// more recalling paths wins first, then id — deterministic every run.
    #[test]
    fn test_merge_tiebreak_is_deterministic() {
        // b only from keyword, c only from vector, identical contributions.
        let kw = vec![make_result("b", "x", Priority::Reference, 0.5)];
        let vec_r = vec![make_result("c", "y", Priority::Reference, 0.5)];

        let merged = HybridMerger::merge(kw.clone(), vec_r.clone(), 10, 1.0, 1.0);
        let first = merged[0].memory.id.clone();

        // Re-run (hash seeding differs per process) and swap input order:
        // same-score/single-source items tie on source count, so id decides.
        let merged2 = HybridMerger::merge(kw, vec_r, 10, 1.0, 1.0);
        assert_eq!(merged[1].memory.id, merged2[1].memory.id);
        assert_eq!(first, "b", "id tie-break must be ascending");
    }

    #[test]
    fn test_merge_preserves_history_keyword_only_provenance() {
        let mut historical = make_result("history", "old text", Priority::Reference, 0.8);
        historical.hit_sources = vec![HitSource::HistoryKeyword { rank: 7 }];

        let merged = HybridMerger::merge(vec![historical], Vec::new(), 10, 1.0, 1.0);
        assert_eq!(
            merged[0].hit_sources,
            vec![HitSource::HistoryKeyword { rank: 7 }]
        );
        assert_eq!(merged[0].score, 1.0 / 61.0);
    }

    #[test]
    fn test_merge_preserves_history_and_vector_path_local_ranks() {
        let mut historical = make_result("both", "old text", Priority::Reference, 0.8);
        historical.hit_sources = vec![HitSource::HistoryKeyword { rank: 9 }];
        let mut vector = historical.clone();
        vector.hit_sources = vec![HitSource::Vector { rank: 4 }, HitSource::ExplicitMatch];

        let merged = HybridMerger::merge(vec![historical], vec![vector], 10, 0.4, 0.6);
        assert_eq!(
            merged[0].hit_sources,
            vec![
                HitSource::HistoryKeyword { rank: 9 },
                HitSource::Vector { rank: 4 },
                HitSource::ExplicitMatch,
            ]
        );
        assert_eq!(merged[0].score, 0.4 / 61.0 + 0.6 / 61.0);
    }

    #[test]
    fn test_merge_provenance_labels_do_not_add_score_or_agreement() {
        let plain = make_result("a", "plain", Priority::Reference, 0.8);
        let mut labeled = make_result("z", "history", Priority::Reference, 0.8);
        labeled.hit_sources = vec![
            HitSource::HistoryKeyword { rank: 3 },
            HitSource::Keyword { rank: 5 },
            HitSource::ExplicitMatch,
        ];
        let merged = HybridMerger::merge(vec![labeled], vec![plain], 10, 1.0, 1.0);
        assert_eq!(
            merged[0].memory.id, "a",
            "equal single-channel scores tie by id"
        );
        assert_eq!(merged[0].score, merged[1].score);
        assert_eq!(merged[1].hit_sources.len(), 3);
    }

    #[test]
    fn test_merge_duplicate_rows_contribute_once_per_input_channel() {
        let result = make_result("a", "same", Priority::Reference, 0.8);
        let merged = HybridMerger::merge(
            vec![result.clone(), result.clone()],
            vec![result.clone(), result],
            10,
            0.4,
            0.6,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].score, 0.4 / 61.0 + 0.6 / 61.0);
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
