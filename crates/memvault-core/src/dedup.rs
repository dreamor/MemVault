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
        // `list()` (unlike `search()`) does not filter `superseded_by` —
        // a memory that was explicitly superseded must not be able to
        // silently absorb new content and disappear from search again.
        let existing: Vec<_> = self
            .store
            .list(namespace, 500, 0)
            .await?
            .into_iter()
            .filter(|m| m.superseded_by.is_none())
            .collect();

        if existing.is_empty() {
            return Ok(None);
        }

        // keyword-based similarity (Jaccard on words)
        let new_words = Self::tokenize(content);

        let mut best_match: Option<(String, String, f32)> = None;

        for mem in &existing {
            let existing_words = Self::tokenize(&mem.content);
            let similarity = Self::ordered_similarity(&new_words, &existing_words);

            if similarity > self.similarity_threshold {
                let current_best = best_match.as_ref().map(|b| b.2).unwrap_or(0.0);
                if similarity > current_best {
                    best_match = Some((mem.id.clone(), mem.content.clone(), similarity));
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
                    // Embedding cosine can't see argument order: short
                    // role-swapped sentences ("Alice owes Bob $100" / "Bob
                    // owes Alice $100", and CJK equivalents the whitespace
                    // tokenizer can't split at all — E2E measured 0.99 for
                    // 张三欠李四/李四欠张三) embed nearly identically. The
                    // keyword path guards this via `ordered_similarity`;
                    // apply the character-bag equivalent here so the vector
                    // path can't swallow facts that assert opposite
                    // relations.
                    if Self::same_characters_reordered(&r.memory.content, content) {
                        debug!(
                            existing_id = %r.memory.id,
                            score = r.score,
                            "vector near-duplicate rejected: same characters, reordered (distinct fact)"
                        );
                        continue;
                    }
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
        let memories: Vec<_> = self
            .store
            .list(namespace, 10000, 0)
            .await?
            .into_iter()
            .filter(|m| m.superseded_by.is_none())
            .collect();
        let mut duplicates = Vec::new();
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();

        for mem in &memories {
            let words = Self::tokenize(&mem.content);

            let mut is_dup = false;
            for (seen_id, seen_words) in &seen {
                let sim = Self::ordered_similarity(&words, seen_words);
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
        if a.is_empty() || b.is_empty() {
            return 0.0;
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

    /// Order-sensitive similarity used for delta-write decisions.
    ///
    /// Plain token-set Jaccard treats "Alice owes Bob $100" and "Bob owes
    /// Alice $100" as identical (same words, same set) even though they
    /// assert opposite facts. Requiring adjacent-token agreement as well
    /// keeps exact/near-exact duplicates at 1.0 but drops reordered
    /// argument structures below the merge threshold.
    pub(crate) fn ordered_similarity(a: &[String], b: &[String]) -> f32 {
        let unigram = Self::jaccard_similarity(a, b);
        if unigram == 0.0 {
            return 0.0;
        }
        // Only guard the case the bag-of-words metric gets exactly wrong:
        // identical word multisets with materially different order
        // ("Alice owes Bob" vs "Bob owes Alice"). Near-duplicates that differ
        // by even one token keep the original Jaccard score, so ordinary
        // paraphrases/prefixes still merge as before.
        if unigram >= 1.0 && a.len() >= 2 && b.len() >= 2 {
            // The bigram signal needs at least two tokens per side: an
            // unsegmented CJK sentence tokenizes to ONE token, where
            // unigram 1.0 means the texts are identical — the reorder
            // guard must not fire (windows(2) is empty there and the old
            // code scored exact duplicates 0.5, below the merge threshold).
            let bigram = Self::bigram_jaccard(a, b);
            if bigram < 0.5 {
                // Same words, shuffled enough to assert something different —
                // push below the 0.7 merge threshold instead of reporting 1.0.
                return 0.5;
            }
        }
        unigram
    }

    /// Character-bag guard for the VECTOR dedup path — the counterpart of
    /// [`Self::ordered_similarity`] for a metric that has no token order at
    /// all. When two texts carry the same character multiset (whitespace
    /// ignored) but are not identical, word/role order is the only thing
    /// distinguishing them, and reordered arguments usually assert a
    /// *different* fact ("张三欠李四100元" vs "李四欠张三100元"; "dog bites
    /// man" vs "man bites dog"). Character bags work for CJK, where the
    /// whitespace tokenizer produces a single token per sentence and the
    /// token-level guard never fires. Genuine paraphrases use different
    /// words, so their character bags differ and dedup proceeds normally.
    fn same_characters_reordered(a: &str, b: &str) -> bool {
        if a == b {
            return false;
        }
        // Whitespace-insensitive identity first: "deploy to prod" vs
        // "deploy  to   prod" is the same text in the same order — not a
        // reorder — even though the raw strings differ.
        let fa: String = a.chars().filter(|c| !c.is_whitespace()).collect();
        let fb: String = b.chars().filter(|c| !c.is_whitespace()).collect();
        if fa == fb {
            return false;
        }
        let mut ca: Vec<char> = fa.chars().collect();
        let mut cb: Vec<char> = fb.chars().collect();
        if ca.len() != cb.len() {
            return false;
        }
        ca.sort();
        cb.sort();
        ca == cb
    }

    fn bigram_jaccard(a: &[String], b: &[String]) -> f32 {
        let ab: Vec<(String, String)> =
            a.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
        let bb: Vec<(String, String)> =
            b.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
        if ab.is_empty() || bb.is_empty() {
            return 0.0;
        }
        let sa: std::collections::HashSet<&(String, String)> = ab.iter().collect();
        let sb: std::collections::HashSet<&(String, String)> = bb.iter().collect();
        let intersection = sa.intersection(&sb).count();
        let union = sa.union(&sb).count();
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
        // Empty text does not produce tokens; it must not match everything
        // (and `Memory::validate_content` rejects truly blank writes anyway).
        assert_eq!(Deduplicator::jaccard_similarity(&[], &[]), 0.0);
        // No overlap
        let a = vec!["alpha".to_string(), "beta".to_string()];
        let b = vec!["omega".to_string(), "gamma".to_string()];
        assert_eq!(Deduplicator::jaccard_similarity(&a, &b), 0.0);
        // Exact same set
        let x = vec!["one".to_string(), "two".to_string()];
        assert_eq!(Deduplicator::jaccard_similarity(&x, &x), 1.0);
    }

    #[test]
    fn test_ordered_similarity_rejects_reordered_arguments() {
        let a = Deduplicator::tokenize("Alice owes Bob USD100");
        let b = Deduplicator::tokenize("Bob owes Alice USD100");
        let sim = Deduplicator::ordered_similarity(&a, &b);
        assert!(sim < 0.7, "reordered facts must not be deduplicated: {sim}");
    }

    #[test]
    fn test_ordered_similarity_keeps_exact_duplicate() {
        let a = Deduplicator::tokenize("user prefers dark mode in all editors");
        let b = Deduplicator::tokenize("user prefers dark mode in all editors");
        assert_eq!(Deduplicator::ordered_similarity(&a, &b), 1.0);
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

    #[test]
    fn test_same_characters_reordered_guard() {
        // Role-swapped short sentences: same character bag, different fact.
        assert!(Deduplicator::same_characters_reordered(
            "张三欠李四100元",
            "李四欠张三100元"
        ));
        assert!(Deduplicator::same_characters_reordered(
            "Alice owes Bob $100",
            "Bob owes Alice $100"
        ));
        // Identical text is a true duplicate, not a reorder.
        assert!(!Deduplicator::same_characters_reordered(
            "same text",
            "same text"
        ));
        // A genuine paraphrase uses different words — different character
        // bag — so dedup must proceed normally.
        assert!(!Deduplicator::same_characters_reordered(
            "the deployment succeeded on monday",
            "monday's deployment was a success!"
        ));
        // Whitespace-only differences are not a reorder.
        assert!(!Deduplicator::same_characters_reordered(
            "deploy to prod",
            "deploy  to   prod"
        ));
    }

    /// E2E 回归:"李四欠张三100元" 与已存 "张三欠李四100元" 在真实嵌入
    /// 下余弦 ~0.99。中文整句在空白分词器下是单 token,关键词路径
    /// Jaccard=0 直接落到向量路径 —— 对调守卫必须在向量路径生效,
    /// 方向相反的事实不得互吞。
    #[tokio::test]
    async fn test_vector_path_rejects_reordered_cjk_facts() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        // StaticEmbedder maps every text to the same vector, so the stored
        // row matches the query with cosine ~1.0 — the exact condition the
        // real native embedder produces for these two sentences.
        store
            .save_with_embedding(
                Memory::new(
                    MemoryType::Fact,
                    "张三欠李四100元".into(),
                    Priority::Reference,
                    agent,
                ),
                vec![1.0, 0.0],
            )
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, Some(Arc::new(StaticEmbedder)));
        let result = dedup
            .check_duplicate("李四欠张三100元", None)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "role-swapped CJK facts assert opposite relations; the vector path must not dedup them, got {result:?}"
        );

        // Control: an exact re-save is still a duplicate.
        let same = dedup
            .check_duplicate("张三欠李四100元", None)
            .await
            .unwrap();
        assert!(
            same.is_some(),
            "identical content must still be flagged as duplicate"
        );
    }

    /// 单 token 文本(未分词的中文整句)完全相同时,关键词路径必须判重
    /// —— bigram 重排守卫需要每侧至少 2 个 token 才有信号,旧实现对
    /// 空窗口返回 0.0,把 exact duplicate 打到 0.5(低于 0.7 阈值),
    /// 无 embedder 时重复内容会无限累积。
    #[tokio::test]
    async fn test_identical_single_token_cjk_duplicates_without_embedder() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        store
            .save(Memory::new(
                MemoryType::Fact,
                "张三欠李四100元".into(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, None);
        let result = dedup
            .check_duplicate("张三欠李四100元", None)
            .await
            .unwrap();
        let pair = result.expect("identical single-token content must dedup on the keyword path");
        assert_eq!(pair.action, DedupAction::Skip);
        assert!(pair.similarity >= 0.95, "got {}", pair.similarity);
    }
}
