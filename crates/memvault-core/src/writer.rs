//! Delta-write on save —— Feature A（见 `docs/PAPER-INSPIRATIONS.md`）。
//!
//! 灵感来源：Qwen3.8-Flash-Next 技术报告 §2.1.1 Gated DeltaNet 的 delta rule：
//! "repeated or similar keys update an existing association instead of
//! accumulating unbounded outer products" —— 写入前先估计该 key 已关联的值，
//! 只写残差。纯追加式记忆库等价于论文否定的"无界外积累加性记忆"，会随使用
//! 无界膨胀，只能靠事后 `dedup` 清理。
//!
//! 本模块把去重/合并前移到写入路径：保存新记忆前先在同命名空间内查找相似项：
//!
//! - 相似度 > 0.95（Skip 档）：近乎重复，不写新条目，返回已有记忆；
//! - 相似度 > 阈值（默认 0.7，Merge 档）：把新内容中**与旧记忆不重叠的残差**
//!   追加进旧记忆，并刷新时间/强度，而不是 insert；
//! - 否则：正常插入。
//!
//! Delta 写入默认开启，`MEMVAULT_DELTA_WRITE=off` 可整体关闭；
//! 每个保存入口（CLI `--force` / MCP·REST `force_insert`）也提供显式旁路。

use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, warn};

use crate::dedup::{DedupAction, Deduplicator};
use crate::embedding::EmbeddingProvider;
use crate::error::{MemVaultError, Result};
use crate::models::{Memory, Priority, Visibility};
use crate::storage::MemoryStore;

/// 一次保存的结果。
#[derive(Debug, Clone)]
pub enum WriteOutcome {
    /// 新记忆已插入。
    Inserted(Memory),
    /// 内容被合并进已有记忆（delta 写入）：`memory` 是合并后的已有记忆，
    /// `residual_added` 表示是否有残差内容被追加（否则仅刷新了时间/强度）。
    Merged {
        memory: Memory,
        similarity: f32,
        residual_added: bool,
    },
    /// 近乎重复（相似度 > 0.95）：没有写入，返回已有记忆。
    Skipped { memory: Memory, similarity: f32 },
}

impl WriteOutcome {
    /// 无论哪种结果，最终承载这条记忆内容的记忆对象。
    pub fn memory(&self) -> &Memory {
        match self {
            WriteOutcome::Inserted(m) | WriteOutcome::Skipped { memory: m, .. } => m,
            WriteOutcome::Merged { memory, .. } => memory,
        }
    }
}

/// Delta 写入是否启用（读取 `MEMVAULT_DELTA_WRITE`，默认开启）。
pub fn delta_write_enabled() -> bool {
    !matches!(
        std::env::var("MEMVAULT_DELTA_WRITE")
            .as_deref()
            .map(str::to_lowercase),
        Ok(v) if v == "off" || v == "0" || v == "false" || v == "disabled"
    )
}

/// 带 delta 写入语义的保存器。
///
/// 与直接调用 `MemoryStore::save` 的区别：先用 `Deduplicator::check_duplicate`
/// 在同命名空间内查重，再决定插入 / 合并 / 跳过。内部写入仍走同一个
/// `MemoryStore`，因此历史快照、FTS 索引等存储层行为与直接保存完全一致。
pub struct MemoryWriter {
    store: Arc<dyn MemoryStore>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    dedup: Deduplicator,
    enabled: bool,
}

impl MemoryWriter {
    pub fn new(store: Arc<dyn MemoryStore>, embedder: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        let dedup = Deduplicator::new(store.clone(), embedder.clone());
        Self {
            store,
            embedder,
            dedup,
            enabled: delta_write_enabled(),
        }
    }

    /// 显式开关（覆盖环境变量判定），主要供测试使用。
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 覆盖相似度阈值（透传给内部 `Deduplicator`）。
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.dedup = self.dedup.with_threshold(threshold);
        self
    }

    /// 保存一条记忆。
    ///
    /// `embedding` 是调用方为新内容预计算的向量（可选）；插入路径直接使用它，
    /// 合并路径会在合并完成后用合并后的文本重新嵌入（尽力而为，失败仅告警）。
    /// `force` 旁路 delta 写入，等价于直接保存。
    pub async fn save(
        &self,
        memory: Memory,
        embedding: Option<Vec<f32>>,
        force: bool,
    ) -> Result<WriteOutcome> {
        // Defense in depth: memories built directly (not through
        // `Extractor::extract_guarded`) never pass through the content
        // guard otherwise — refuse outright rather than launder or redact.
        if crate::sensitive::is_sensitive(&memory.content)
            || memory
                .instruction
                .as_deref()
                .is_some_and(crate::sensitive::is_sensitive)
        {
            return Err(MemVaultError::InvalidInput(
                "content appears to contain sensitive credentials; refusing to save".to_string(),
            ));
        }

        if force || !self.enabled {
            return self
                .plain_save(memory, embedding)
                .await
                .map(WriteOutcome::Inserted);
        }

        match self
            .dedup
            .check_duplicate(&memory.content, Some(&memory.namespace))
            .await?
        {
            Some(pair) if pair.action == DedupAction::Skip => {
                let existing = self.store.get(&pair.existing_id).await?;
                debug!(
                    id = %existing.id,
                    similarity = pair.similarity,
                    "delta-write: near-duplicate, skipping insert"
                );
                Ok(WriteOutcome::Skipped {
                    memory: existing,
                    similarity: pair.similarity,
                })
            }
            Some(pair) => {
                let existing = self.store.get(&pair.existing_id).await?;
                let (merged, residual_added) =
                    merge_memory(existing, &memory, self.dedup.threshold());
                let id = merged.id.clone();
                let updated = self.store.update(merged).await?;
                self.refresh_embedding(&updated).await;
                debug!(
                    id = %id,
                    similarity = pair.similarity,
                    residual_added,
                    "delta-write: merged into existing memory"
                );
                Ok(WriteOutcome::Merged {
                    memory: updated,
                    similarity: pair.similarity,
                    residual_added,
                })
            }
            None => self
                .plain_save(memory, embedding)
                .await
                .map(WriteOutcome::Inserted),
        }
    }

    async fn plain_save(&self, memory: Memory, embedding: Option<Vec<f32>>) -> Result<Memory> {
        match embedding {
            Some(emb) => self.store.save_with_embedding(memory, emb).await,
            None => self.store.save(memory).await,
        }
    }

    /// 合并后用合并文本重新嵌入，让残差也能被语义检索召回。
    /// 尽力而为：嵌入/写入失败只告警，不让保存失败。
    async fn refresh_embedding(&self, memory: &Memory) {
        let Some(embedder) = &self.embedder else {
            return;
        };
        let text = memory
            .instruction
            .clone()
            .unwrap_or_else(|| memory.content.clone());
        match embedder.embed(&[text]).await {
            Ok(embs) if !embs.is_empty() => {
                if let Some(emb) = embs.into_iter().next()
                    && let Err(e) = self.store.set_embedding(&memory.id, emb).await
                {
                    warn!("delta-write: failed to refresh embedding: {}", e);
                }
            }
            Err(e) => warn!("delta-write: re-embedding failed: {}", e),
            _ => {}
        }
    }
}

/// 把新记忆的残差合并进旧记忆（delta 写入核心，纯函数）。
///
/// 规则：
/// - `content`：新内容按句子/行切分成片段，与旧内容重叠度高（jaccard ≥ 阈值）
///   的片段丢弃，其余作为残差追加到旧内容末尾；
/// - `instruction`：旧为空时采用新的，否则保留旧的（不静默改写指令）；
/// - `tags`：并集；
/// - `priority`：单调升级（Background < Reference < Must），不降级；
/// - `confidence`：取较大者；
/// - `visibility`：任一方为 Shared 即升为 Shared；
/// - `skill_meta`：旧为空时采用新的；
/// - `human_reviewed`：任一方已审核即为已审核；
/// - `decay_score`：重置为 1.0（被再次确认的记忆等同新生）；
/// - `updated_at`：置为当前时间。
///
/// 返回合并后的记忆与"是否有残差被追加"。
pub fn merge_memory(existing: Memory, incoming: &Memory, threshold: f32) -> (Memory, bool) {
    let mut merged = existing;
    merged.updated_at = Utc::now();
    merged.decay_score = 1.0;

    let residual = extract_residual(&merged.content, &incoming.content, threshold);
    let residual_added = !residual.is_empty();
    if residual_added {
        merged.content = format!("{}\n{}", merged.content.trim_end(), residual);
    }

    if merged.instruction.is_none() {
        merged.instruction = incoming.instruction.clone();
    }
    for tag in &incoming.tags {
        if !merged.tags.iter().any(|t| t == tag) {
            merged.tags.push(tag.clone());
        }
    }
    if priority_rank(&incoming.priority) > priority_rank(&merged.priority) {
        merged.priority = incoming.priority.clone();
    }
    if incoming.confidence > merged.confidence {
        merged.confidence = incoming.confidence;
    }
    if incoming.visibility == Visibility::Shared {
        merged.visibility = Visibility::Shared;
    }
    if merged.skill_meta.is_none() {
        merged.skill_meta = incoming.skill_meta.clone();
    }
    if incoming.human_reviewed {
        merged.human_reviewed = true;
    }

    // Corroboration signal for the feature-flagged MUST trust gate (see
    // `router::format::is_trusted`): accumulate distinct identity-verified
    // `source_agent.id`s across merges. The existing memory's own author
    // only counts once it's known to be identity-verified — an unverified
    // memory never retroactively self-corroborates just by being merged
    // into.
    if merged.identity_verified
        && !merged
            .corroborating_agents
            .contains(&merged.source_agent.id)
    {
        merged
            .corroborating_agents
            .push(merged.source_agent.id.clone());
    }
    if incoming.identity_verified
        && !merged
            .corroborating_agents
            .contains(&incoming.source_agent.id)
    {
        merged
            .corroborating_agents
            .push(incoming.source_agent.id.clone());
    }
    merged.identity_verified = merged.identity_verified || incoming.identity_verified;

    (merged, residual_added)
}

fn priority_rank(p: &Priority) -> u8 {
    match p {
        Priority::Background => 0,
        Priority::Reference => 1,
        Priority::Must => 2,
    }
}

/// 把新内容切分成片段，返回其中与已有内容不重叠的残差（换行连接）。
///
/// 片段与已有内容的 jaccard 相似度达到阈值即视为"已知"而丢弃——
/// 这就是论文里"只写残差"的文本版实现。
fn extract_residual(existing: &str, incoming: &str, threshold: f32) -> String {
    let existing_tokens = Deduplicator::tokenize(existing);
    let residual: Vec<String> = split_segments(incoming)
        .into_iter()
        .filter(|seg| {
            let seg_tokens = Deduplicator::tokenize(seg);
            Deduplicator::jaccard_similarity(&seg_tokens, &existing_tokens) < threshold
        })
        .collect();
    residual.join("\n")
}

/// 按句界切分文本。
///
/// 边界符：换行、`。`、`；`、`;`，以及**仅当后面是空白或文本结束**时的 `.`——
/// 这样 `10.20.30.40`（IP）、`v1.2.3`（版本）、`main.rs`（扩展名）不会被切碎，
/// 而英文句尾的 `. ` 仍能正常断句。
fn split_segments(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments = Vec::new();
    let mut current = String::new();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        let is_boundary = match c {
            '\n' | '。' | '；' | ';' => true,
            '.' => chars.get(i + 1).is_none_or(|next| next.is_whitespace()),
            _ => false,
        };
        if is_boundary {
            let seg = current.trim().to_string();
            if !seg.is_empty() {
                segments.push(seg);
            }
            current.clear();
        }
    }
    let seg = current.trim().to_string();
    if !seg.is_empty() {
        segments.push(seg);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MemoryType, SourceAgent};
    use crate::storage::sqlite::SqliteStore;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    fn mem(content: &str) -> Memory {
        Memory::new(
            MemoryType::Fact,
            content.to_string(),
            Priority::Reference,
            agent(),
        )
    }

    fn writer(store: Arc<SqliteStore>) -> MemoryWriter {
        MemoryWriter::new(store, None).with_enabled(true)
    }

    async fn store_with(content: &str) -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        store.save(mem(content)).await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_insert_when_no_match() {
        let store = store_with("project uses Rust for backend services").await;
        let w = writer(store.clone());

        let outcome = w
            .save(
                mem("totally unrelated weather conversation today"),
                None,
                false,
            )
            .await
            .unwrap();

        assert!(matches!(outcome, WriteOutcome::Inserted(_)));
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_skip_on_near_exact_duplicate() {
        let store = store_with("user prefers dark mode in all editors").await;
        let w = writer(store.clone());

        let outcome = w
            .save(mem("user prefers dark mode in all editors"), None, false)
            .await
            .unwrap();

        let WriteOutcome::Skipped { memory, similarity } = outcome else {
            panic!("expected Skipped, got {:?}", outcome);
        };
        assert!(similarity > 0.95);
        assert_eq!(memory.content, "user prefers dark mode in all editors");
        // Nothing new was written.
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_merge_appends_residual_and_refreshes() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut existing = mem("user prefers dark mode in all editors");
        existing.decay_score = 0.4; // aged memory
        existing.tags = vec!["theme".to_string()];
        store.save(existing).await.unwrap();
        // Overlap of this content pair is ~0.64 (word-level jaccard), below
        // the 0.7 default — lower the threshold to exercise the merge path.
        // In production the vector-similarity fallback covers such pairs.
        let w = writer(store.clone()).with_threshold(0.5);

        // Same topic, overlapping wording, but carries a new fact.
        let mut incoming = mem("user prefers dark mode in all editors. Tab width is 4 spaces");
        incoming.tags = vec!["theme".to_string(), "formatting".to_string()];
        let outcome = w.save(incoming, None, false).await.unwrap();

        let WriteOutcome::Merged {
            memory,
            similarity,
            residual_added,
        } = outcome
        else {
            panic!("expected Merged, got {:?}", outcome);
        };
        assert!(residual_added);
        assert!(similarity > 0.5);
        assert!(memory.content.contains("Tab width is 4 spaces"));
        assert!(!memory.content.contains("dark mode\nuser prefers")); // no duplication of the overlap
        assert_eq!(memory.decay_score, 1.0, "merge must refresh strength");
        assert!(memory.tags.contains(&"theme".to_string()));
        assert!(memory.tags.contains(&"formatting".to_string()));
        // Still exactly one memory — nothing accumulated.
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_merge_without_residual_only_refreshes() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut existing = mem("user prefers dark mode in all editors");
        existing.decay_score = 0.3;
        let before = existing.updated_at;
        store.save(existing).await.unwrap();
        let w = writer(store.clone());

        // Rephrased but fully-covered content: jaccard over threshold (Merge),
        // yet no residual segment survives.
        let outcome = w
            .save(mem("user prefers dark mode in the editors"), None, false)
            .await
            .unwrap();

        let WriteOutcome::Merged {
            memory,
            residual_added,
            ..
        } = outcome
        else {
            panic!("expected Merged, got {:?}", outcome);
        };
        assert!(!residual_added);
        assert_eq!(memory.content, "user prefers dark mode in all editors");
        assert_eq!(memory.decay_score, 1.0);
        assert!(memory.updated_at >= before);
    }

    #[tokio::test]
    async fn test_merge_escalates_priority_and_adopts_instruction() {
        let store = store_with("deploy window is Friday afternoon").await;
        let w = writer(store.clone());

        let mut incoming = mem("deploy window is Friday afternoon, no exceptions");
        incoming.priority = Priority::Must;
        incoming.instruction = Some("Never deploy outside Friday afternoon".to_string());
        let outcome = w.save(incoming, None, false).await.unwrap();

        let WriteOutcome::Merged { memory, .. } = outcome else {
            panic!("expected Merged, got {:?}", outcome);
        };
        assert_eq!(memory.priority, Priority::Must);
        assert_eq!(
            memory.instruction.as_deref(),
            Some("Never deploy outside Friday afternoon")
        );
    }

    #[tokio::test]
    async fn test_merge_never_downgrades_priority() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut existing = mem("always run cargo fmt before commit");
        existing.priority = Priority::Must;
        store.save(existing).await.unwrap();
        let w = writer(store.clone());

        let mut incoming = mem("always run cargo fmt before commit, every time");
        incoming.priority = Priority::Background;
        let outcome = w.save(incoming, None, false).await.unwrap();

        let WriteOutcome::Merged { memory, .. } = outcome else {
            panic!("expected Merged, got {:?}", outcome);
        };
        assert_eq!(memory.priority, Priority::Must);
    }

    #[tokio::test]
    async fn test_force_bypasses_delta_write() {
        let store = store_with("user prefers dark mode in all editors").await;
        let w = writer(store.clone());

        let outcome = w
            .save(mem("user prefers dark mode in all editors"), None, true)
            .await
            .unwrap();

        assert!(matches!(outcome, WriteOutcome::Inserted(_)));
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_disabled_writer_always_inserts() {
        let store = store_with("user prefers dark mode in all editors").await;
        let w = MemoryWriter::new(store.clone(), None).with_enabled(false);

        let outcome = w
            .save(mem("user prefers dark mode in all editors"), None, false)
            .await
            .unwrap();

        assert!(matches!(outcome, WriteOutcome::Inserted(_)));
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_duplicate_in_other_namespace_is_not_matched() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mut existing = mem("identical sentence appears once here");
        existing.namespace = "project:alpha".to_string();
        store.save(existing).await.unwrap();
        let w = writer(store.clone());

        let mut incoming = mem("identical sentence appears once here");
        incoming.namespace = "project:beta".to_string();
        let outcome = w.save(incoming, None, false).await.unwrap();

        assert!(
            matches!(outcome, WriteOutcome::Inserted(_)),
            "namespace-scoped dedup must not cross namespaces"
        );
        assert_eq!(store.list(None, 10, 0).await.unwrap().len(), 2);
    }

    #[test]
    fn test_extract_residual_drops_overlap() {
        let existing = "build server IP is 10.20.30.40";
        let incoming = "build server IP is 10.20.30.40. Timeout is 30 seconds";
        let residual = extract_residual(existing, incoming, 0.7);
        assert_eq!(residual, "Timeout is 30 seconds");
    }

    #[test]
    fn test_extract_residual_chinese_segments() {
        let existing = "构建服务器 IP 是 10.20.30.40";
        let incoming = "构建服务器 IP 是 10.20.30.40。超时时间设置为 30 秒";
        let residual = extract_residual(existing, incoming, 0.7);
        assert!(residual.contains("超时时间"));
        assert!(!residual.contains("10.20.30.40"));
    }

    #[test]
    fn test_extract_residual_empty_when_fully_covered() {
        let existing = "user prefers dark mode in all editors";
        let residual = extract_residual(existing, "user prefers dark mode in all editors", 0.7);
        assert!(residual.is_empty());
    }

    #[test]
    fn test_merge_memory_escalates_visibility() {
        let existing = mem("team standup at ten");
        assert_eq!(existing.visibility, Visibility::Scoped);
        let mut incoming = mem("team standup at ten sharp");
        incoming.visibility = Visibility::Shared;

        let (merged, _) = merge_memory(existing, &incoming, 0.7);
        assert_eq!(merged.visibility, Visibility::Shared);
    }

    #[test]
    fn test_merge_memory_adopts_skill_meta_and_review() {
        let existing = mem("restart service when stuck");
        let mut incoming = mem("restart service when stuck completely");
        incoming.skill_meta = Some(crate::models::SkillMeta {
            trigger: Some("service stuck".to_string()),
            steps: vec!["systemctl restart app".to_string()],
            verification: Some("curl health endpoint".to_string()),
            version: 1,
        });
        incoming.human_reviewed = true;

        let (merged, _) = merge_memory(existing, &incoming, 0.7);
        assert!(merged.skill_meta.is_some());
        assert!(merged.human_reviewed);
    }

    #[test]
    fn test_merge_memory_accumulates_distinct_verified_agents() {
        let mut existing = mem("server region is us-east-1");
        existing.identity_verified = true;
        existing.source_agent.id = "agent-a".to_string();

        let mut incoming = mem("server region is us-east-1, confirmed");
        incoming.identity_verified = true;
        incoming.source_agent.id = "agent-b".to_string();

        let (merged, _) = merge_memory(existing, &incoming, 0.5);
        assert!(merged.identity_verified);
        assert_eq!(
            merged.corroborating_agents,
            vec!["agent-a".to_string(), "agent-b".to_string()]
        );
    }

    #[test]
    fn test_merge_memory_does_not_double_count_same_agent() {
        let mut existing = mem("server region is us-east-1");
        existing.identity_verified = true;
        existing.source_agent.id = "agent-a".to_string();

        let mut incoming = mem("server region is us-east-1, confirmed");
        incoming.identity_verified = true;
        incoming.source_agent.id = "agent-a".to_string();

        let (merged, _) = merge_memory(existing, &incoming, 0.5);
        assert_eq!(merged.corroborating_agents, vec!["agent-a".to_string()]);
    }

    #[test]
    fn test_merge_memory_ignores_unverified_incoming() {
        let existing = mem("server region is us-east-1"); // not verified
        let mut incoming = mem("server region is us-east-1, confirmed");
        incoming.identity_verified = false;
        incoming.source_agent.id = "agent-b".to_string();

        let (merged, _) = merge_memory(existing, &incoming, 0.5);
        assert!(!merged.identity_verified);
        assert!(merged.corroborating_agents.is_empty());
    }

    #[test]
    fn test_delta_write_enabled_default() {
        // Env hygiene: this test only asserts the "absent → on" direction.
        // (Parallel tests may set the var; we don't mutate global env here.)
        if std::env::var("MEMVAULT_DELTA_WRITE").is_err() {
            assert!(delta_write_enabled());
        }
    }
}
