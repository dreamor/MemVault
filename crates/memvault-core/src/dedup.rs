use std::sync::Arc;
use tracing::debug;

use crate::embedding::EmbeddingProvider;
use crate::error::Result;
use crate::models::Memory;
use crate::storage::MemoryStore;

/// Whether a stored memory may absorb new content as a dedup candidate
/// (audit #15). Two disqualifiers, both because the candidate is not current
/// truth:
///
/// - `superseded_by` — it was replaced; merging into it would make the new
///   content vanish from search along with it.
/// - `proposes_supersede_of` — it is a PENDING proposal, a draft awaiting a
///   human decision. An ordinary save that dedup-matched it was skipped or
///   merged into the draft: invisible to normal retrieval while pending, and
///   DELETED outright if the proposal was later rejected — the caller's save
///   silently died with someone else's review decision.
///
/// The vector path already excluded both in SQL (`vector_search`:
/// `superseded_by IS NULL AND proposes_supersede_of IS NULL`); this predicate
/// keeps the keyword/list paths on the identical rule, defined once so the
/// two cannot drift again. Approved proposals clear their link on approval
/// (`proposal::approve_memory`), so a live replacement stays a candidate.
fn is_dedup_candidate(m: &Memory) -> bool {
    m.superseded_by.is_none() && m.proposes_supersede_of.is_none()
}

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
        // `list()` (unlike `search()`) applies no currency filters of its
        // own — superseded rows and pending proposals must not be able to
        // silently absorb new content, so the eligibility rule lives in
        // `is_dedup_candidate` and is shared with `scan()` and (in SQL) the
        // vector path.
        let existing: Vec<_> = self
            .store
            .list(namespace, 500, 0)
            .await?
            .into_iter()
            .filter(is_dedup_candidate)
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
            // >0.95 is a pure duplicate only when nothing is new: a single
            // digit change in a long body still scores above it, and Skip
            // would silently lose the value update (C4 follow-up). Route
            // those to Merge so the delta-write residual filter appends the
            // changed sentence.
            let value_update = sim > 0.95
                && Self::carries_value_update(
                    &Self::tokenize(content),
                    &Self::tokenize(&existing_content),
                );
            let action = if sim > 0.95 && !value_update {
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
            // Same eligibility rule as check_duplicate: superseded rows and
            // pending proposals are not current truth and must not absorb
            // or be reported as duplicates of live content.
            .filter(is_dedup_candidate)
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
                        // Same rule as check_duplicate: a digit change is an
                        // update, not a pure duplicate (C4 follow-up).
                        action: if sim > 0.95 && !Self::carries_value_update(&words, seen_words) {
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

    /// Split text into comparable tokens.
    ///
    /// Single-character tokens are KEPT: filtering them out made
    /// "timeout 5" and "timeout 9" tokenize to the identical `["timeout"]`,
    /// scoring 1.0 and silently skipping the numeric update — a value change
    /// is new information, never a duplicate.
    ///
    /// A `+`/`-` that STARTS a token and is directly followed by a digit
    /// stays attached to it (C4 completion): the plain splitter dropped the
    /// sign, so "balance -5" tokenized identically to "balance 5", scored
    /// 1.00 and was Skipped as a pure duplicate — a sign flip is a value
    /// update, never a duplicate. Hyphens inside or after a word keep their
    /// old separator behavior ("well-known" → well|known, "model-3" →
    /// model|3), so only whitespace-delimited signed numbers change.
    pub(crate) fn tokenize(text: &str) -> Vec<String> {
        let text = text.to_lowercase();
        let chars: Vec<char> = text.chars().collect();
        let mut tokens: Vec<String> = Vec::new();
        let mut current = String::new();
        for (i, c) in chars.iter().enumerate() {
            let starts_signed_number = (*c == '-' || *c == '+')
                && current.is_empty()
                && chars.get(i + 1).is_some_and(|n| n.is_numeric());
            if c.is_alphanumeric() || *c == '_' || starts_signed_number {
                current.push(*c);
            } else if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        tokens
    }

    /// True when `new_words` carries a digit-bearing token that
    /// `existing_words` has never seen — the signature of a value/version
    /// update ("timeout 5" → "timeout 9", "v1.2.3" → "v1.2.4", "100元" →
    /// "200元"). A near-duplicate carrying one is an UPDATE, not a pure
    /// duplicate: it must route to Merge (and its changed sentence must
    /// survive residual extraction), never be skipped/dropped as covered
    /// text (C4 follow-up — the silent data loss behind an honestly
    /// reported merge). Word-only swaps ("in all editors" → "in the
    /// editors") deliberately do NOT qualify: those are paraphrases the
    /// similarity threshold treats as covered.
    pub(crate) fn carries_value_update(new_words: &[String], existing_words: &[String]) -> bool {
        new_words
            .iter()
            .any(|t| !existing_words.contains(t) && t.chars().any(|c| c.is_numeric()))
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

    /// Audit #15: a PENDING proposal is a draft awaiting a human decision,
    /// not current truth — but the keyword/list dedup path used to offer it
    /// as a candidate (the vector path always excluded it in SQL, so the two
    /// paths disagreed). A normal save of near-identical text was skipped or
    /// merged into the draft: invisible to normal retrieval while pending,
    /// and deleted outright if the proposal was later rejected.
    #[tokio::test]
    async fn test_pending_proposal_is_not_a_dedup_candidate() {
        let store = setup().await;
        let ag = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let target = store
            .save(Memory::new(
                MemoryType::Fact,
                "the release checklist lives in the wiki".to_string(),
                Priority::Reference,
                ag.clone(),
            ))
            .await
            .unwrap();
        let mut proposal = Memory::new(
            MemoryType::Fact,
            "the unicorn migration playbook lives in the wiki".to_string(),
            Priority::Reference,
            ag.clone(),
        );
        proposal.proposes_supersede_of = Some(target.id.clone());
        let proposal = store.save(proposal).await.unwrap();

        let dedup = Deduplicator::new(store.clone(), None);
        let pair = dedup
            .check_duplicate("the unicorn migration playbook lives in the wiki", None)
            .await
            .unwrap();
        assert!(
            pair.is_none(),
            "a pending proposal must not absorb a normal save: {pair:?}"
        );

        let scan = dedup.scan(None).await.unwrap();
        assert!(
            !scan.duplicates.iter().any(|d| d.existing_id == proposal.id),
            "scan must not report the pending proposal as a duplicate target"
        );

        // An APPROVED proposal — link consumed on approval, now a live
        // memory — stays a normal candidate. The exclusion is for pending
        // drafts only.
        let mut approved = store.get(&proposal.id).await.unwrap();
        approved.proposes_supersede_of = None;
        approved.human_reviewed = true;
        store.update(approved).await.unwrap();
        let pair = dedup
            .check_duplicate("the unicorn migration playbook lives in the wiki", None)
            .await
            .unwrap();
        assert_eq!(
            pair.as_ref().map(|p| p.existing_id.as_str()),
            Some(proposal.id.as_str()),
            "an approved (live) memory is an ordinary dedup candidate"
        );
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
    fn test_tokenize_keeps_single_chars() {
        let tokens = Deduplicator::tokenize("a bc xy z");
        assert!(tokens.contains(&"bc".to_string()));
        assert!(tokens.contains(&"xy".to_string()));
        // Single-char tokens are semantic (digits, grades, version letters):
        // filtering them made "timeout 5" and "timeout 9" tokenize to the
        // identical ["timeout"] and the update was swallowed as a duplicate.
        assert!(tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"z".to_string()));
    }

    /// C4 completion: a sign directly attached to a number is part of the
    /// value — "balance -5" must not tokenize identically to "balance 5".
    /// Word-internal hyphens keep their separator behavior.
    #[test]
    fn test_tokenize_keeps_number_signs() {
        let tokens = Deduplicator::tokenize("balance -5 and +7");
        assert!(tokens.contains(&"-5".to_string()));
        assert!(tokens.contains(&"+7".to_string()));
        // Hyphens inside/after a word are still separators.
        assert_eq!(
            Deduplicator::tokenize("well-known model-3"),
            vec!["well", "known", "model", "3"]
        );
        assert_eq!(Deduplicator::tokenize("5-3"), vec!["5", "3"]);
    }

    /// C4 completion regression (v0.6.12 audit, P2): the sign flip
    /// "balance 5" → "balance -5" scored 1.00 once the tokenizer dropped
    /// '-', and the second save was Skipped as a pure duplicate — the
    /// negative value was lost.
    #[tokio::test]
    async fn test_signed_value_change_is_not_deduped() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        store
            .save(Memory::new(
                MemoryType::Fact,
                "balance 5".into(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, None);
        let result = dedup.check_duplicate("balance -5", None).await.unwrap();
        assert!(
            result.is_none(),
            "a sign flip is a value update, not a duplicate, got {result:?}"
        );

        // Control: unchanged content is still a duplicate.
        let same = dedup.check_duplicate("balance 5", None).await.unwrap();
        assert!(
            same.is_some(),
            "identical content must still be flagged as duplicate"
        );
    }

    /// C4 regression: a value change is new information — it must not be
    /// skipped as a near-exact duplicate just because the differing token is
    /// a single character.
    #[tokio::test]
    async fn test_numeric_value_change_is_not_deduped() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        store
            .save(Memory::new(
                MemoryType::Preference,
                "timeout 5".into(),
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, None);
        let result = dedup.check_duplicate("timeout 9", None).await.unwrap();
        assert!(
            result.is_none(),
            "a changed numeric value must not be swallowed as a duplicate, got {result:?}"
        );

        // Control: an unchanged value is still a duplicate.
        let same = dedup.check_duplicate("timeout 5", None).await.unwrap();
        assert!(
            same.is_some(),
            "identical content must still be flagged as duplicate"
        );
    }

    /// C4 follow-up: a single digit change inside a LONG body still scores
    /// > 0.95 — the near-exact band. It must route to Merge (so the
    /// residual filter appends the changed sentence), not Skip, which would
    /// report "pure duplicate" and silently lose the value update.
    #[tokio::test]
    async fn test_near_exact_numeric_change_merges_instead_of_skipping() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = crate::models::SourceAgent {
            id: "t".into(),
            agent_type: "g".into(),
            session_id: None,
        };
        // 42 tokens; changing the final digit gives 41/43 ≈ 0.953 > 0.95.
        let filler = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi \
                      omicron pi rho sigma tau upsilon phi chi psi omega one two three four five \
                      six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
        let existing_content = format!("{filler} timeout 5");
        store
            .save(Memory::new(
                MemoryType::Preference,
                existing_content,
                Priority::Reference,
                agent,
            ))
            .await
            .unwrap();

        let dedup = Deduplicator::new(store, None);
        let result = dedup
            .check_duplicate(&format!("{filler} timeout 9"), None)
            .await
            .unwrap();
        let pair = result.expect("must still be detected as a near-duplicate");
        assert!(pair.similarity > 0.95, "got {}", pair.similarity);
        assert_eq!(
            pair.action,
            DedupAction::Merge,
            "a value update must not be skipped as a pure duplicate"
        );

        // Control: content identical to the stored row (similarity 1.0,
        // nothing novel) is still a pure-duplicate Skip.
        let identical = dedup
            .check_duplicate(&format!("{filler} timeout 5"), None)
            .await
            .unwrap();
        let identical = identical.expect("identical content must be flagged");
        assert!(identical.similarity > 0.999);
        assert_eq!(identical.action, DedupAction::Skip);
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
