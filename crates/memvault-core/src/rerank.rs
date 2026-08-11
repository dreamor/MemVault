use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::models::{Memory, Priority, SearchResult};

/// Configuration for the multi-signal reranker.
///
/// Each weight controls how much that signal contributes to the final score.
/// Weights are normalized internally, so they don't need to sum to 1.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// Whether reranking is enabled. When disabled, results pass through unchanged.
    pub enabled: bool,
    /// Weight for the existing hybrid/RRF score (default: 0.35)
    pub hybrid_weight: f64,
    /// Weight for query term overlap in content/instruction/tags (default: 0.25)
    pub overlap_weight: f64,
    /// Weight for memory recency (default: 0.15)
    pub recency_weight: f64,
    /// Weight for priority level (MUST > Reference > Background) (default: 0.15)
    pub priority_weight: f64,
    /// Weight for access frequency + recency (default: 0.10)
    pub access_weight: f64,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hybrid_weight: 0.35,
            overlap_weight: 0.25,
            recency_weight: 0.15,
            priority_weight: 0.15,
            access_weight: 0.10,
        }
    }
}

/// Multi-signal reranker.
///
/// Takes the initial hybrid search results and re-scores them using multiple
/// signals: query overlap, recency, priority, and access patterns.
/// This improves top-k precision without requiring external model dependencies.
#[derive(Debug, Clone)]
pub struct MultiSignalReranker {
    config: RerankConfig,
}

impl MultiSignalReranker {
    pub fn new(config: RerankConfig) -> Self {
        Self { config }
    }

    /// Rerank search results using multiple signals.
    ///
    /// - `query`: The original search query string.
    /// - `results`: The initial candidate results (from hybrid/keyword/vector search).
    /// - `now`: Current timestamp for recency calculations.
    ///
    /// Returns results sorted by the composite rerank score (descending),
    /// with MUST priorities always placed first.
    pub fn rerank(
        &self,
        query: &str,
        results: Vec<SearchResult>,
        now: &DateTime<Utc>,
    ) -> Vec<SearchResult> {
        if !self.config.enabled || results.is_empty() {
            return results;
        }

        let query_terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.len() >= 2)
            .collect();

        let total_weight = self.config.hybrid_weight
            + self.config.overlap_weight
            + self.config.recency_weight
            + self.config.priority_weight
            + self.config.access_weight;

        if total_weight == 0.0 {
            return results;
        }

        // Find max/min values for normalization
        let max_hybrid = results.iter().map(|r| r.score).fold(0.0_f64, f64::max);

        let max_access_count = results
            .iter()
            .map(|r| r.memory.access_count)
            .max()
            .unwrap_or(1)
            .max(1);

        let scored: Vec<SearchResult> = results
            .into_iter()
            .map(|r| {
                let mut new_score = 0.0;

                // 1. Normalized hybrid score (0..1)
                let hybrid_norm = if max_hybrid > 0.0 {
                    r.score / max_hybrid
                } else {
                    0.0
                };
                new_score += self.config.hybrid_weight * hybrid_norm;

                // 2. Query overlap score (0..1)
                let overlap = self.compute_overlap(&query_terms, &r.memory);
                new_score += self.config.overlap_weight * overlap;

                // 3. Recency score (0..1)
                let recency = self.compute_recency(&r.memory, now);
                new_score += self.config.recency_weight * recency;

                // 4. Priority boost (0..1)
                let priority_score = match r.memory.priority {
                    Priority::Must => 1.0,
                    Priority::Reference => 0.7,
                    Priority::Background => 0.4,
                };
                new_score += self.config.priority_weight * priority_score;

                // 5. Access score (0..1)
                let access_score = self.compute_access_score(&r.memory, max_access_count, now);
                new_score += self.config.access_weight * access_score;

                // Normalize final score
                let final_score = if total_weight > 0.0 {
                    new_score / total_weight
                } else {
                    0.0
                };

                SearchResult {
                    score: final_score,
                    memory: r.memory,
                }
            })
            .collect();

        // Sort by score descending, MUST always first
        let mut sorted = scored;
        sorted.sort_by(|a, b| {
            let a_must = a.memory.priority == Priority::Must;
            let b_must = b.memory.priority == Priority::Must;
            match (a_must, b_must) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        debug!(
            input = sorted.len(),
            top_score = sorted.first().map(|r| r.score),
            "rerank complete"
        );

        sorted
    }

    /// Compute query term overlap with content, instruction, and tags.
    ///
    /// Returns a score in [0.0, 1.0] where 1.0 means all query terms were found.
    fn compute_overlap(&self, query_terms: &[String], memory: &Memory) -> f64 {
        if query_terms.is_empty() {
            return 0.5; // neutral score for empty queries
        }

        let mut overlap_count = 0;

        // Check content
        let content_lower = memory.content.to_lowercase();
        // Check instruction
        let instruction_lower = memory.instruction.as_deref().unwrap_or("").to_lowercase();
        // Check tags
        let tags_lower: Vec<String> = memory.tags.iter().map(|t| t.to_lowercase()).collect();

        for term in query_terms {
            let in_content = content_lower.contains(term);
            let in_instruction = instruction_lower.contains(term);
            let in_tags = tags_lower.iter().any(|t| t.contains(term));

            if in_content || in_instruction || in_tags {
                overlap_count += 1;
            }
        }

        // Bonus for tag matches (exact tag match = very relevant)
        let exact_tag_match = query_terms.iter().any(|t| tags_lower.contains(t));

        let base = overlap_count as f64 / query_terms.len() as f64;
        if exact_tag_match {
            (base + 0.2).min(1.0)
        } else {
            base
        }
    }

    /// Compute recency score based on updated_at.
    ///
    /// Returns a score in [0.0, 1.0] where 1.0 = updated right now.
    /// Decay curve: score = 1 / (1 + days_since_update / 7)
    /// Meaning: memories updated within a week get > 0.5, within a month get > 0.2
    fn compute_recency(&self, memory: &Memory, now: &DateTime<Utc>) -> f64 {
        let age = (now.timestamp_millis() - memory.updated_at.timestamp_millis()).max(0);
        let days = age as f64 / (24.0 * 3600.0 * 1000.0);
        1.0 / (1.0 + days / 7.0)
    }

    /// Compute access score based on access_count and last_read_at.
    ///
    /// Returns a score in [0.0, 1.0].
    /// Combines relative access frequency and recency of last access.
    fn compute_access_score(
        &self,
        memory: &Memory,
        max_access_count: u32,
        now: &DateTime<Utc>,
    ) -> f64 {
        // Frequency component
        let freq = if max_access_count > 0 {
            memory.access_count as f64 / max_access_count as f64
        } else {
            0.0
        };

        // Recency of last access
        let last_read_recency = match memory.last_read_at {
            Some(last) => {
                let age = (now.timestamp_millis() - last.timestamp_millis()).max(0);
                let days = age as f64 / (24.0 * 3600.0 * 1000.0);
                1.0 / (1.0 + days / 14.0) // longer decay window for access
            }
            None => 0.0,
        };

        // 60% frequency, 40% recency of last access
        0.6 * freq + 0.4 * last_read_recency
    }
}

/// Convenience function: create a default reranker, rerank, and return sorted results.
pub fn rerank_results(query: &str, results: Vec<SearchResult>) -> Vec<SearchResult> {
    let reranker = MultiSignalReranker::new(RerankConfig::default());
    let now = Utc::now();
    reranker.rerank(query, results, &now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_memory(
        id: &str,
        content: &str,
        priority: Priority,
        tags: Vec<&str>,
        instruction: Option<&str>,
        created_days_ago: i64,
        access_count: u32,
    ) -> Memory {
        let now = Utc::now();
        let created = now - chrono::Duration::days(created_days_ago);
        Memory {
            id: id.to_string(),
            memory_type: MemoryType::Fact,
            content: content.to_string(),
            instruction: instruction.map(|s| s.to_string()),
            priority,
            source_agent: SourceAgent {
                id: "test".to_string(),
                agent_type: "general".to_string(),
                session_id: None,
            },
            namespace: "global".to_string(),
            confidence: 0.8,
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
            created_at: created,
            updated_at: created,
            ai_generated: false,
            human_reviewed: false,
            decay_score: 1.0,
            access_count,
            last_read_at: None,
            layer: MemoryLayer::L1,
            skill_meta: None,
        }
    }

    #[test]
    fn test_empty_results() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let results = reranker.rerank("test", vec![], &Utc::now());
        assert!(results.is_empty());
    }

    #[test]
    fn test_disabled_reranker_passthrough() {
        let config = RerankConfig {
            enabled: false,
            ..RerankConfig::default()
        };
        let reranker = MultiSignalReranker::new(config);
        let mem = make_memory("a", "hello", Priority::Reference, vec![], None, 0, 0);
        let results = reranker.rerank(
            "hello",
            vec![SearchResult {
                score: 0.5,
                memory: mem,
            }],
            &Utc::now(),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 0.5);
    }

    #[test]
    fn test_must_always_first() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let m1 = make_memory(
            "a",
            "low priority",
            Priority::Background,
            vec![],
            None,
            0,
            0,
        );
        let m2 = make_memory("b", "must rule", Priority::Must, vec![], None, 100, 0);

        let results = reranker.rerank(
            "test",
            vec![
                SearchResult {
                    score: 0.9,
                    memory: m1,
                },
                SearchResult {
                    score: 0.1,
                    memory: m2,
                },
            ],
            &Utc::now(),
        );
        assert_eq!(results[0].memory.id, "b");
        assert_eq!(results[0].memory.priority, Priority::Must);
    }

    #[test]
    fn test_query_overlap_boosts() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let m1 = make_memory(
            "a",
            "Python API development",
            Priority::Reference,
            vec![],
            None,
            0,
            0,
        );
        let m2 = make_memory(
            "b",
            "writing style guide",
            Priority::Reference,
            vec![],
            None,
            0,
            0,
        );

        let results = reranker.rerank(
            "Python API",
            vec![
                SearchResult {
                    score: 0.25,
                    memory: m1,
                }, // lower hybrid score
                SearchResult {
                    score: 0.30,
                    memory: m2,
                }, // slightly higher but irrelevant
            ],
            &Utc::now(),
        );
        // "a" matches both "Python" AND "API" → overlap boost should bring it above "b"
        assert_eq!(results[0].memory.id, "a");
    }

    #[test]
    fn test_recency_prefers_newer() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let now = Utc::now();
        let m1 = make_memory(
            "old",
            "same content",
            Priority::Reference,
            vec![],
            None,
            30,
            0,
        );
        let m2 = make_memory(
            "new",
            "same content",
            Priority::Reference,
            vec![],
            None,
            0,
            0,
        );

        let results = reranker.rerank(
            "content",
            vec![
                SearchResult {
                    score: 0.5,
                    memory: m1,
                },
                SearchResult {
                    score: 0.5,
                    memory: m2,
                },
            ],
            &now,
        );
        // Same hybrid score, same content → newer should win
        assert_eq!(results[0].memory.id, "new");
    }

    #[test]
    fn test_tag_match_bonus() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let m1 = make_memory(
            "tagged",
            "some text",
            Priority::Reference,
            vec!["python", "api"],
            None,
            0,
            0,
        );
        let m2 = make_memory(
            "plain",
            "some text",
            Priority::Reference,
            vec![],
            None,
            0,
            0,
        );

        let results = reranker.rerank(
            "python",
            vec![
                SearchResult {
                    score: 0.5,
                    memory: m1,
                },
                SearchResult {
                    score: 0.5,
                    memory: m2,
                },
            ],
            &Utc::now(),
        );
        // Tag match should boost "tagged" above "plain"
        assert_eq!(results[0].memory.id, "tagged");
    }

    #[test]
    fn test_access_frequency_boost() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let m1 = make_memory("frequent", "same", Priority::Reference, vec![], None, 0, 10);
        let m2 = make_memory("rare", "same", Priority::Reference, vec![], None, 0, 1);

        let results = reranker.rerank(
            "same",
            vec![
                SearchResult {
                    score: 0.5,
                    memory: m1,
                },
                SearchResult {
                    score: 0.5,
                    memory: m2,
                },
            ],
            &Utc::now(),
        );
        assert_eq!(results[0].memory.id, "frequent");
    }

    #[test]
    fn test_rerank_results_convenience() {
        let mem = make_memory("a", "test data", Priority::Reference, vec![], None, 0, 0);
        let results = rerank_results(
            "test",
            vec![SearchResult {
                score: 0.5,
                memory: mem,
            }],
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_overlap_empty_query() {
        let reranker = MultiSignalReranker::new(RerankConfig::default());
        let mem = make_memory("a", "anything", Priority::Reference, vec![], None, 0, 0);
        // compute_overlap is private, test through rerank
        let results = reranker.rerank(
            "",
            vec![SearchResult {
                score: 0.5,
                memory: mem,
            }],
            &Utc::now(),
        );
        assert_eq!(results.len(), 1);
        // Empty query should still produce valid score
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_zero_weight_no_crash() {
        let config = RerankConfig {
            enabled: true,
            hybrid_weight: 0.0,
            overlap_weight: 0.0,
            recency_weight: 0.0,
            priority_weight: 0.0,
            access_weight: 0.0,
        };
        let reranker = MultiSignalReranker::new(config);
        let mem = make_memory("a", "test", Priority::Reference, vec![], None, 0, 0);
        let results = reranker.rerank(
            "test",
            vec![SearchResult {
                score: 0.5,
                memory: mem,
            }],
            &Utc::now(),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 0.5);
    }
}
