use std::collections::{HashMap, HashSet};
use tracing::debug;

use crate::models::{HitSource, Priority, SearchResult};

/// Experimental hybrid-retrieval knobs used by the MAB anchor experiment.
///
/// Both fields default to the historical behavior: merge only `top_k`
/// candidates and do not reserve keyword anchors. The environment-backed
/// constructor keeps production call sites unchanged unless an operator
/// explicitly enables the experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorPolicy {
    /// Candidate count fetched from each channel and passed into the merge
    /// before reranking. Always at least `top_k`.
    pub candidate_k: usize,
    /// Number of top keyword-ranked candidates to reserve before reranking,
    /// even if their weighted RRF score would have pushed them below the
    /// candidate cutoff. A reserved id keeps every fused recall path; it is
    /// not required to be keyword-channel-only.
    pub keyword_anchors: usize,
}

impl AnchorPolicy {
    pub fn new(top_k: usize, candidate_k: Option<usize>, keyword_anchors: Option<usize>) -> Self {
        let floor = top_k.max(1);
        let candidate_k = candidate_k
            .filter(|value| *value > 0)
            .map(|value| value.clamp(floor, crate::models::MAX_TOP_K))
            .unwrap_or(top_k);
        let keyword_anchors = keyword_anchors.unwrap_or(0).min(crate::models::MAX_TOP_K);
        Self {
            candidate_k,
            keyword_anchors,
        }
    }

    /// Read the experiment knobs from the server environment.
    pub fn from_env(top_k: usize) -> Self {
        fn positive(name: &str) -> Option<usize> {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<usize>().ok())
        }
        Self::new(
            top_k,
            positive("MEMVAULT_HYBRID_CANDIDATE_K"),
            positive("MEMVAULT_HYBRID_KEYWORD_ANCHORS"),
        )
    }

    /// True when this policy asks for more than the historical merge/rerank
    /// path. Used for diagnostics, not for branching correctness.
    pub fn is_active(&self, top_k: usize) -> bool {
        self.candidate_k > top_k || self.keyword_anchors > 0
    }
}

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

/// Candidate set plus the keyword anchors that must survive reranking.
#[derive(Debug, Clone)]
pub struct AnchoredCandidates {
    pub candidates: Vec<SearchResult>,
    pub anchor_ids: Vec<String>,
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
        let mut merged = Self::fuse(
            keyword_results,
            vector_results,
            keyword_weight,
            vector_weight,
        );
        merged.truncate(top_k);

        debug!(total = merged.len(), "hybrid merge complete");
        merged
    }

    /// Fuse both channels without truncating, preserving each id's combined
    /// RRF score and every recall path. `merge` truncates this list; the
    /// anchored path needs the full list so a reserved id can be recovered
    /// with its fused score and provenance instead of a raw keyword clone.
    fn fuse(
        keyword_results: Vec<SearchResult>,
        vector_results: Vec<SearchResult>,
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

        merged.into_iter().map(|(result, _)| result).collect()
    }

    /// Merge a wider candidate set while reserving the top keyword hits.
    ///
    /// The caller still owns reranking and the final `top_k` truncation. The
    /// returned `anchor_ids` are the keyword-ranked IDs that must survive
    /// reranking via [`Self::reserve_anchors`]. Anchors are the top keyword
    /// results whether or not the vector channel also recalled them: overlap
    /// is useful evidence, but it must not disqualify the keyword anchor.
    pub fn merge_anchored(
        keyword_results: Vec<SearchResult>,
        vector_results: Vec<SearchResult>,
        top_k: usize,
        keyword_weight: f64,
        vector_weight: f64,
        policy: AnchorPolicy,
    ) -> AnchoredCandidates {
        let mut anchor_ids = Vec::new();
        if policy.keyword_anchors > 0 {
            let mut seen = HashSet::new();
            for result in &keyword_results {
                if !seen.insert(result.memory.id.as_str()) {
                    continue;
                }
                anchor_ids.push(result.memory.id.clone());
                if anchor_ids.len() == policy.keyword_anchors {
                    break;
                }
            }
        }

        let fused = Self::fuse(
            keyword_results,
            vector_results,
            keyword_weight,
            vector_weight,
        );
        let mut candidates: Vec<SearchResult> = fused
            .iter()
            .take(policy.candidate_k.max(top_k))
            .cloned()
            .collect();
        let mut present: HashSet<String> = candidates
            .iter()
            .map(|result| result.memory.id.clone())
            .collect();
        // Recover a reserved id from the fused entry, not from the raw keyword
        // result. The candidate cutoff may have dropped it, but fusion still
        // knows its combined RRF score and every channel that recalled it.
        for anchor_id in &anchor_ids {
            if present.contains(anchor_id) {
                continue;
            }
            if let Some(anchor) = fused.iter().find(|result| result.memory.id == *anchor_id) {
                candidates.push(anchor.clone());
                present.insert(anchor.memory.id.clone());
            }
        }
        AnchoredCandidates {
            candidates,
            anchor_ids,
        }
    }

    /// Guarantee that reserved keyword anchors survive the final truncation
    /// while keeping MUST-priority memories non-negotiable.
    ///
    /// With no anchors this is exactly the historical `take(top_k)` pass-through,
    /// so the default retrieval path is unchanged. When anchors are active, the
    /// priority policy is: every MUST slot outranks anchor reservation. If there
    /// are more MUSTs than `top_k`, only MUSTs are returned. Otherwise anchors
    /// are selected next (in rerank order), and any remaining slots are filled
    /// from the non-anchor rerank order. Selection never changes output order:
    /// the returned list is resorted MUST-first, then by the original rerank
    /// position, so a reserved REFERENCE can never precede a MUST.
    pub fn reserve_anchors(
        ranked: &[SearchResult],
        anchor_ids: &[String],
        top_k: usize,
    ) -> Vec<SearchResult> {
        if top_k == 0 {
            return Vec::new();
        }
        if anchor_ids.is_empty() {
            return ranked.iter().take(top_k).cloned().collect();
        }
        let anchor_set: HashSet<&str> = anchor_ids.iter().map(String::as_str).collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut selected: Vec<(usize, SearchResult)> = Vec::with_capacity(top_k);

        // MUST is the hard invariant, even when the reranker violates it or an
        // anchor would otherwise replace the last non-anchor slot.
        for (index, result) in ranked.iter().enumerate() {
            if selected.len() == top_k {
                break;
            }
            if result.memory.priority == Priority::Must && seen.insert(result.memory.id.as_str()) {
                selected.push((index, result.clone()));
            }
        }

        // Reserved anchors take the next slots, in their original rerank order.
        for (index, result) in ranked.iter().enumerate() {
            if selected.len() == top_k {
                break;
            }
            if result.memory.priority != Priority::Must
                && anchor_set.contains(result.memory.id.as_str())
                && seen.insert(result.memory.id.as_str())
            {
                selected.push((index, result.clone()));
            }
        }

        // Fill the rest from the unmodified rerank order.
        for (index, result) in ranked.iter().enumerate() {
            if selected.len() == top_k {
                break;
            }
            if result.memory.priority != Priority::Must
                && !anchor_set.contains(result.memory.id.as_str())
                && seen.insert(result.memory.id.as_str())
            {
                selected.push((index, result.clone()));
            }
        }

        // Slot selection is not output order. Restore MUST-first, then rerank
        // order, so the final list is deterministic and priority-consistent.
        selected.sort_by(|(a_index, a), (b_index, b)| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            b_must.cmp(&a_must).then_with(|| a_index.cmp(b_index))
        });
        selected.into_iter().map(|(_, result)| result).collect()
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
    fn test_anchor_policy_defaults_to_historical_behavior() {
        let policy = AnchorPolicy::new(20, None, None);
        assert_eq!(policy.candidate_k, 20);
        assert_eq!(policy.keyword_anchors, 0);
        assert!(!policy.is_active(20));
    }

    #[test]
    fn test_anchor_policy_clamps_candidate_k() {
        let policy = AnchorPolicy::new(20, Some(2), Some(3));
        assert_eq!(
            policy.candidate_k, 20,
            "candidate_k must not shrink below top_k"
        );
        assert_eq!(policy.keyword_anchors, 3);
        let policy = AnchorPolicy::new(20, Some(usize::MAX), None);
        assert_eq!(policy.candidate_k, crate::models::MAX_TOP_K);
    }

    #[test]
    fn test_keyword_anchor_survives_vector_dominated_candidate_cutoff() {
        let kw = vec![make_result(
            "anchor",
            "rare keyword",
            Priority::Reference,
            0.9,
        )];
        let vec_r: Vec<SearchResult> = (0..20)
            .map(|i| make_result(&format!("vec_{i}"), "vector", Priority::Reference, 0.9))
            .collect();

        // With a five-candidate cutoff the rank-1 keyword result loses to
        // twenty vector-only results: 0.4/61 < 0.6/(60+20).
        let without = HybridMerger::merge_anchored(
            kw.clone(),
            vec_r.clone(),
            5,
            0.4,
            0.6,
            AnchorPolicy::new(5, Some(5), None),
        );
        assert!(
            !without
                .candidates
                .iter()
                .any(|result| result.memory.id == "anchor")
        );
        assert!(without.anchor_ids.is_empty());

        let with = HybridMerger::merge_anchored(
            kw.clone(),
            vec_r,
            5,
            0.4,
            0.6,
            AnchorPolicy::new(5, Some(5), Some(1)),
        );
        let anchor = with
            .candidates
            .iter()
            .find(|result| result.memory.id == "anchor")
            .unwrap();
        assert_eq!(anchor.hit_sources, vec![HitSource::Keyword { rank: 1 }]);
        assert_eq!(with.anchor_ids, vec!["anchor".to_string()]);

        // Even when the reranker would drop the anchor, reservation restores it.
        let ranked = vec![
            make_result("vec_0", "vector", Priority::Reference, 0.9),
            make_result("vec_1", "vector", Priority::Reference, 0.8),
            make_result("anchor", "rare keyword", Priority::Reference, 0.1),
        ];
        let final_results = HybridMerger::reserve_anchors(&ranked, &with.anchor_ids, 2);
        assert_eq!(final_results.len(), 2);
        assert!(
            final_results
                .iter()
                .any(|result| result.memory.id == "anchor")
        );
    }

    #[test]
    fn test_merge_anchored_recovers_fused_score_and_provenance() {
        // The anchor is keyword rank 1 but vector rank 50. With a tiny keyword
        // weight and a five-candidate cutoff, several vector-only hits beat it
        // and fusion truncates it. Recovery must preserve the fused RRF score
        // and the vector path, not append a raw keyword clone with score 0.
        let kw: Vec<SearchResult> = (0..50)
            .map(|i| {
                let id = if i == 0 {
                    "anchor".to_string()
                } else {
                    format!("kw_{i}")
                };
                make_result(&id, "keyword", Priority::Reference, 0.9)
            })
            .collect();
        let vec_r: Vec<SearchResult> = (0..50)
            .map(|i| {
                let id = if i == 49 {
                    "anchor".to_string()
                } else {
                    format!("vec_{i}")
                };
                make_result(&id, "vector", Priority::Reference, 0.9)
            })
            .collect();

        let without = HybridMerger::merge_anchored(
            kw.clone(),
            vec_r.clone(),
            5,
            0.01,
            1.0,
            AnchorPolicy::new(5, Some(5), None),
        );
        assert!(
            !without
                .candidates
                .iter()
                .any(|result| result.memory.id == "anchor"),
            "precondition: the cutoff must drop the fused anchor"
        );

        let with = HybridMerger::merge_anchored(
            kw,
            vec_r,
            5,
            0.01,
            1.0,
            AnchorPolicy::new(5, Some(5), Some(1)),
        );
        let anchor = with
            .candidates
            .iter()
            .find(|result| result.memory.id == "anchor")
            .expect("reserved anchor must be recovered");
        assert_eq!(
            anchor.hit_sources,
            vec![
                HitSource::Keyword { rank: 1 },
                HitSource::Vector { rank: 50 }
            ],
            "recovery must keep both fused recall paths"
        );
        assert!(
            anchor.score > 0.0,
            "recovery must keep the fused RRF score, got {}",
            anchor.score
        );
    }

    #[test]
    fn test_reserve_anchors_never_evicts_must() {
        // Both anchor ids are missing from the top-2 snapshot. The old
        // last-non-anchor replacement evicted the MUST and returned only
        // REFERENCE rows; the policy must keep the MUST and fill the remaining
        // slot with the highest-ranked reserved anchor.
        let ranked = vec![
            make_result("ref", "reference", Priority::Reference, 0.9),
            make_result("must", "must", Priority::Must, 0.8),
            make_result("anchor_a", "anchor a", Priority::Reference, 0.2),
            make_result("anchor_b", "anchor b", Priority::Reference, 0.1),
        ];
        let anchors = vec!["anchor_a".to_string(), "anchor_b".to_string()];
        let results = HybridMerger::reserve_anchors(&ranked, &anchors, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].memory.id, "must");
        assert_eq!(results[1].memory.id, "anchor_a");
        assert!(results.iter().all(
            |result| result.memory.priority == Priority::Must || result.memory.id == "anchor_a"
        ));
    }

    #[test]
    fn test_reserve_anchors_must_capacity_wins() {
        // top_k cannot hold both the MUST and the anchor. MUST priority wins;
        // the anchor is not allowed to displace it.
        let ranked = vec![
            make_result("must", "must", Priority::Must, 0.9),
            make_result("anchor", "anchor", Priority::Reference, 0.1),
        ];
        let results = HybridMerger::reserve_anchors(&ranked, &["anchor".to_string()], 1);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.id, "must");
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[test]
    fn test_reserve_anchors_without_anchors_is_unchanged_truncation() {
        // The default path must not gain a new re-sort. With no anchors this
        // stays exactly take(top_k), even if the caller's order is not
        // MUST-first (the upstream merge/rerank already guarantees that).
        let ranked = vec![
            make_result("ref_0", "reference", Priority::Reference, 0.9),
            make_result("must_1", "must", Priority::Must, 0.8),
            make_result("ref_2", "reference", Priority::Reference, 0.7),
        ];
        let results = HybridMerger::reserve_anchors(&ranked, &[], 2);
        assert_eq!(
            results
                .iter()
                .map(|result| result.memory.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ref_0", "must_1"]
        );
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
