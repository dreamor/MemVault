use std::sync::Arc;
use tracing::debug;

use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::storage::MemoryStore;

pub struct Deduplicator {
    store: Arc<dyn MemoryStore>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    similarity_threshold: f32,
    /// Stricter threshold for the VECTOR path. Word-overlap is a good proxy
    /// for "same assertion", but cosine similarity conflates "same topic"
    /// with "same fact" — "api at /v1" vs "api at /v2" score ~0.87 while
    /// asserting different things. Merging on topical similarity would
    /// destroy genuinely new facts, so the vector path demands near-identity.
    vector_similarity_threshold: f32,
}

#[derive(Debug, Clone)]
pub struct DedupResult {
    pub duplicates: Vec<DuplicatePair>,
    pub unique_count: usize,
}

#[derive(Debug, Clone)]
pub struct DuplicatePair {
    pub existing_id: String,
    pub existing_content: String,
    pub new_content: String,
    pub similarity: f32,
    pub action: DedupAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DedupAction {
    Skip,
    Merge,
    Conflict,
}

impl Deduplicator {
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        Self {
            store,
            embedder,
            similarity_threshold: 0.7,
            vector_similarity_threshold: 0.9,
        }
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.similarity_threshold = threshold;
        self
    }

    /// Current similarity threshold (above this, a candidate counts as duplicate).
    pub fn threshold(&self) -> f32 {
        self.similarity_threshold
    }

    /// Check if a memory is a duplicate of an existing one.
    /// Uses keyword overlap for text similarity, vector cosine if embedder available.
    pub async fn check_duplicate(
        &self,
        content: &str,
        namespace: Option<&str>,
    ) -> Result<Option<DuplicatePair>> {
        let existing = self.store.list(namespace, 500, 0).await?;

        if existing.is_empty() {
            return Ok(None);
        }

        // keyword-based similarity (Jaccard on words)
        let new_words = Self::tokenize(content);

        let mut best_match: Option<(String, String, f32)> = None;

        for mem in &existing {
            let existing_words = Self::tokenize(&mem.content);
            let jaccard = Self::jaccard_similarity(&new_words, &existing_words);

            if jaccard > self.similarity_threshold {
                let current_best = best_match.as_ref().map(|b| b.2).unwrap_or(0.0);
                if jaccard > current_best {
                    best_match = Some((mem.id.clone(), mem.content.clone(), jaccard));
                }
            }
        }

        // If we have an embedder, also check vector similarity
        if best_match.is_none()
            && let Some(ref embedder) = self.embedder
            && let Ok(query_emb) = embedder.embed(&[content.to_string()]).await
            && let Some(q_emb) = query_emb.first()
        {
            let vec_results = self.store.vector_search(q_emb, 5, namespace).await?;
            for r in vec_results {
                if r.score as f32 > self.vector_similarity_threshold {
                    let current_best = best_match.as_ref().map(|b| b.2).unwrap_or(0.0);
                    if r.score as f32 > current_best {
                        best_match = Some((
                            r.memory.id.clone(),
                            r.memory.content.clone(),
                            r.score as f32,
                        ));
                    }
                }
            }
        }

        if let Some((id, existing_content, sim)) = best_match {
            let action = if sim > 0.95 {
                DedupAction::Skip
            } else {
                DedupAction::Merge
            };

            debug!(similarity = sim, action = ?action, "duplicate detected");

            Ok(Some(DuplicatePair {
                existing_id: id,
                existing_content,
                new_content: content.to_string(),
                similarity: sim,
                action,
            }))
        } else {
            Ok(None)
        }
    }

    /// Scan all memories for duplicates.
    pub async fn scan(&self, namespace: Option<&str>) -> Result<DedupResult> {
        let memories = self.store.list(namespace, 10000, 0).await?;
        let mut duplicates = Vec::new();
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();

        for mem in &memories {
            let words = Self::tokenize(&mem.content);

            let mut is_dup = false;
            for (seen_id, seen_words) in &seen {
                let sim = Self::jaccard_similarity(&words, seen_words);
                if sim > self.similarity_threshold {
                    duplicates.push(DuplicatePair {
                        existing_id: seen_id.clone(),
                        existing_content: "".to_string(),
                        new_content: mem.content.clone(),
                        similarity: sim,
                        action: if sim > 0.95 {
                            DedupAction::Skip
                        } else {
                            DedupAction::Merge
                        },
                    });
                    is_dup = true;
                    break;
                }
            }

            if !is_dup {
                seen.push((mem.id.clone(), words));
            }
        }

        let unique_count = seen.len();
        debug!(
            total = memories.len(),
            unique = unique_count,
            duplicates = duplicates.len(),
            "dedup scan complete"
        );

        Ok(DedupResult {
            duplicates,
            unique_count,
        })
    }

    pub(crate) fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() > 1)
            .map(|w| w.to_string())
            .collect()
    }

    pub(crate) fn jaccard_similarity(a: &[String], b: &[String]) -> f32 {
        if a.is_empty() && b.is_empty() {
            return 1.0;
        }
        let set_a: std::collections::HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
        let set_b: std::collections::HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
        let intersection = set_a.intersection(&set_b).count();
        let union = set_a.union(&set_b).count();
        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;
    use crate::storage::sqlite::SqliteStore;

    async fn setup() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        store
            .save(Memory::new(
                MemoryType::Preference,
                "user prefers Python for coding".to_string(),
                Priority::Must,
                agent.clone(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "project uses FastAPI".to_string(),
                Priority::Reference,
                agent.clone(),
            ))
            .await
            .unwrap();

        store
    }

    #[tokio::test]
    async fn test_detect_duplicate() {
        let store = setup().await;
        let dedup = Deduplicator::new(store, None);

        let result = dedup
            .check_duplicate("user prefers Python for coding tasks", None)
            .await
            .unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_no_duplicate() {
        let store = setup().await;
        let dedup = Deduplicator::new(store, None);

        let result = dedup
            .check_duplicate("completely different unrelated content about weather", None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_scan() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        store
            .save(Memory::new(
                MemoryType::Fact,
                "user likes Python coding".to_string(),
                Priority::Reference,
                agent.clone(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "user likes Python for coding".to_string(),
                Priority::Reference,
                agent.clone(),
            ))
            .await
            .unwrap();
        store
            .save(Memory::new(
                MemoryType::Fact,
                "project uses Rust".to_string(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, None);
        let result = dedup.scan(None).await.unwrap();

        assert!(!result.duplicates.is_empty());
        assert_eq!(result.unique_count, 2);
    }

    #[test]
    fn test_jaccard() {
        let a = vec![
            "user".to_string(),
            "likes".to_string(),
            "python".to_string(),
        ];
        let b = vec![
            "user".to_string(),
            "likes".to_string(),
            "python".to_string(),
            "coding".to_string(),
        ];
        let sim = Deduplicator::jaccard_similarity(&a, &b);
        assert!(sim > 0.7);
    }

    #[test]
    fn test_jaccard_edge_cases() {
        // Both empty → treated as identical
        assert_eq!(Deduplicator::jaccard_similarity(&[], &[]), 1.0);
        // No overlap
        let a = vec!["alpha".to_string(), "beta".to_string()];
        let b = vec!["omega".to_string(), "gamma".to_string()];
        assert_eq!(Deduplicator::jaccard_similarity(&a, &b), 0.0);
        // Exact same set
        let x = vec!["one".to_string(), "two".to_string()];
        assert_eq!(Deduplicator::jaccard_similarity(&x, &x), 1.0);
    }

    #[test]
    fn test_tokenize_filters_single_chars() {
        let tokens = Deduplicator::tokenize("a bc xy z");
        assert!(tokens.contains(&"bc".to_string()));
        assert!(tokens.contains(&"xy".to_string()));
        assert!(tokens.iter().all(|t| t.len() > 1));
    }

    #[tokio::test]
    async fn test_check_duplicate_scoped_by_namespace() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        let mut mem = Memory::new(
            MemoryType::Fact,
            "identical sentence appears once".into(),
            Priority::Reference,
            agent,
        );
        mem.namespace = "project:alpha".to_string();
        store.save(mem).await.unwrap();

        let dedup = Deduplicator::new(store, None);
        // Same text in a different namespace must NOT match (scoped list).
        let in_other = dedup
            .check_duplicate("identical sentence appears once", Some("project:other"))
            .await
            .unwrap();
        assert!(in_other.is_none());

        // Same text in the matching namespace must match with Skip action.
        let in_same = dedup
            .check_duplicate("identical sentence appears once", Some("project:alpha"))
            .await
            .unwrap();
        let pair = in_same.expect("should detect duplicate in scoped namespace");
        assert_eq!(pair.action, DedupAction::Skip);
    }

    struct StaticEmbedder;
    #[async_trait::async_trait]
    impl crate::embedding::EmbeddingProvider for StaticEmbedder {
        async fn embed(&self, texts: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        fn dimension(&self) -> usize {
            2
        }
    }

    #[tokio::test]
    async fn test_check_duplicate_uses_vector_fallback() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        // No keyword overlap with the query, but a near-identical embedding.
        store
            .save_with_embedding(
                Memory::new(
                    MemoryType::Fact,
                    "completely unrelated words here".into(),
                    Priority::Reference,
                    agent,
                ),
                vec![0.99, 0.02],
            )
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, Some(Arc::new(StaticEmbedder)));
        let result = dedup
            .check_duplicate("totally different vocabulary", None)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "vector similarity should still flag a duplicate"
        );
    }
}
