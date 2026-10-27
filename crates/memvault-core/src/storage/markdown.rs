//! Markdown-as-source-of-truth storage backend (Phase 4, inspired by
//! EverOS's "Markdown is the source of truth" architecture).
//!
//! Opposite data-flow from the default `SqliteStore` + `memvault-cli sync`
//! (database → generated instruction files, one-way): here a directory of
//! human/git/Obsidian-editable `{id}.md` files is canonical, and an internal
//! SQLite file (`.memvault-cache.db`) is a disposable, rebuildable index —
//! FTS5, embeddings, episodes, skill stats, relations, trace watermarks all
//! live there, since none of that bookkeeping has a sensible one-file home.
//!
//! Only `save`/`update`/`delete`/`save_with_embedding`/`supersede` touch the
//! filesystem; every other `MemoryStore` method is a pure pass-through to
//! the cache. The cache is treated as eventually-consistent: `reindex()`
//! diffs every file's content against `Exporter::memory_to_markdown` of its
//! cached row (if any) and resyncs on any mismatch — a crash between a file
//! write and the matching cache write, or a hand-edit made outside MemVault
//! entirely (Obsidian, a text editor, `git checkout`), both self-heal the
//! same way, on the next `reindex`.
//!
//! Out of scope for v1 (see docs/DESIGN.md §18.6): git auto-commit,
//! running both backends at once, multi-machine merge of concurrent edits,
//! namespace subdirectories, NFS-safe locking.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tracing::{info, warn};

use super::MemoryStore;
use super::sqlite::SqliteStore;
use crate::error::{MemVaultError, Result};
use crate::io::{Exporter, Importer};
use crate::models::*;

/// Advisory single-writer lock (`{dir}/.memvault.lock`), created with
/// `O_EXCL` semantics via `create_new`. Deliberately simple: no liveness
/// check of the PID inside it. If a prior process crashed and left the lock
/// behind, the user must remove the file manually — documented, not
/// automated, to avoid the much harder problem of safely detecting a stale
/// lock across platforms.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn acquire(path: &Path) -> Result<Self> {
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(Self {
                    path: path.to_path_buf(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(MemVaultError::Storage(format!(
                    "lock file already exists: {}. Another MemVault process may be using \
                     this markdown store; if you're sure none is running (e.g. after a \
                     crash), delete this file and retry.",
                    path.display()
                )))
            }
            Err(e) => Err(MemVaultError::Storage(e.to_string())),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Outcome of one [`MarkdownStore::reindex`] pass.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReindexReport {
    /// Files present on disk with no matching cache row (new/hand-created).
    pub added: usize,
    /// Files whose content disagreed with the cached row (external edit, or
    /// repair of a crash between a file write and its cache write).
    pub resynced: usize,
    /// Cache rows whose file disappeared (hand-deleted outside MemVault).
    pub removed: usize,
    /// Files that failed to parse as Markdown-with-frontmatter; left alone.
    pub skipped_unparseable: usize,
}

impl ReindexReport {
    fn changed(&self) -> usize {
        self.added + self.resynced + self.removed
    }
}

/// Markdown-as-source-of-truth [`MemoryStore`] backend. See the module docs.
pub struct MarkdownStore {
    dir: PathBuf,
    cache: SqliteStore,
    _lock: LockGuard,
}

impl MarkdownStore {
    /// Open (creating if needed) a markdown-backed store rooted at `dir`.
    /// Runs an initial [`Self::reindex`] so a cache deleted or stale between
    /// runs — or a directory someone has been hand-editing — self-heals on
    /// open rather than silently serving a stale index.
    pub async fn open(dir: &Path) -> Result<Self> {
        let (store, _report) = Self::open_with_report(dir).await?;
        Ok(store)
    }

    /// Like [`Self::open`], but hands back the [`ReindexReport`] from the
    /// initial self-heal pass. Callers that would otherwise run their own
    /// `reindex()` right after opening (e.g. the CLI's `reindex` command)
    /// should use this and report *these* numbers — a second explicit pass
    /// always comes back 0/0/0 because the open already applied everything,
    /// which reads as "nothing to do" even when open just repaired the cache.
    pub async fn open_with_report(dir: &Path) -> Result<(Self, ReindexReport)> {
        crate::private_fs::create_dir_all_private(dir).map_err(|e| {
            MemVaultError::Storage(format!("failed to create {}: {}", dir.display(), e))
        })?;
        let lock = LockGuard::acquire(&dir.join(".memvault.lock"))?;
        let cache = SqliteStore::new(&dir.join(".memvault-cache.db"))?;
        let store = Self {
            dir: dir.to_path_buf(),
            cache,
            _lock: lock,
        };
        let report = store.reindex().await?;
        if report.changed() > 0 || report.skipped_unparseable > 0 {
            info!(
                added = report.added,
                resynced = report.resynced,
                removed = report.removed,
                skipped_unparseable = report.skipped_unparseable,
                "markdown store: initial reindex"
            );
        }
        Ok((store, report))
    }

    /// The internal SQLite cache file path — exposed so callers that bypass
    /// `MemoryStore` for SQLite-specific features (compliance tracking,
    /// eval-history) have somewhere consistent to put their own tables,
    /// mirroring how those side-stores share the main `data.db` file in the
    /// sqlite backend.
    pub fn cache_db_path(&self) -> PathBuf {
        self.dir.join(".memvault-cache.db")
    }

    /// Ids become file names (`{id}.md`) and temp-file stems, so they must be
    /// a single, safe path component. Without this, an imported or
    /// round-tripped memory whose id carries `../` (or a path separator) would
    /// make `save`/`update`/`delete`/`purge` read, overwrite or delete files
    /// outside the vault directory.
    fn validate_id(id: &str) -> Result<()> {
        let valid = !id.is_empty()
            && id.len() <= 200
            && !id.starts_with('.')
            && !id.contains("..")
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if valid {
            Ok(())
        } else {
            Err(MemVaultError::InvalidInput(format!(
                "invalid memory id {id:?}: ids must be 1-200 chars of [A-Za-z0-9._-], \
                 must not start with '.', and must not contain '..'"
            )))
        }
    }

    fn file_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    /// Write `content` to `{id}.md` atomically (temp file + rename), so a
    /// crash mid-write never leaves a half-written file in place.
    fn write_file_atomic(&self, id: &str, content: &str) -> Result<()> {
        let tmp_path = self.dir.join(format!(".tmp-{id}-{}", std::process::id()));
        std::fs::write(&tmp_path, content).map_err(|e| {
            MemVaultError::Storage(format!("failed to write {}: {}", tmp_path.display(), e))
        })?;
        std::fs::rename(&tmp_path, self.file_path(id)).map_err(|e| {
            MemVaultError::Storage(format!(
                "failed to move {} into place: {}",
                tmp_path.display(),
                e
            ))
        })
    }

    /// Normalize fields that are not part of the canonical Markdown file
    /// or whose SQLite value can legitimately differ between two in-memory
    /// snapshots, so an already-loaded cache row and the freshly loaded one
    /// compare structurally without forcing the file to be rewritten.
    fn quiesect_memory_for_reindex(buffer: &Memory) -> Memory {
        let mut normalized = buffer.clone();
        normalized.content = normalized.content.clone().trim().to_string();
        normalized.created_at = Default::default();
        normalized.updated_at = Default::default();
        normalized.last_read_at = None;
        normalized.access_count = 0;
        normalized.decay_score = 1.0;
        normalized.confidence = 0.0;
        normalized.source_agent.id = buffer.source_agent.id.trim().to_string();
        normalized.source_agent.agent_type = buffer.source_agent.agent_type.trim().to_string();
        normalized
    }

    /// Reconcile the markdown directory against the cache: new files are
    /// parsed and inserted, files whose content no longer matches their
    /// cached row are reparsed and resynced (clearing the stale embedding,
    /// since it was computed from the old text), and cache rows whose file
    /// vanished are dropped. Idempotent and safe to call repeatedly — a
    /// no-op pass only reads files and compares strings, no writes.
    pub async fn reindex(&self) -> Result<ReindexReport> {
        let mut report = ReindexReport::default();
        let mut seen_ids = HashSet::new();

        let entries = std::fs::read_dir(&self.dir).map_err(|e| {
            MemVaultError::Storage(format!("failed to read {}: {}", self.dir.display(), e))
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "reindex: failed to read a directory entry, skipped");
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            // Leftover crash artifact from `write_file_atomic` before its
            // rename landed — never a real memory id.
            if stem.starts_with(".tmp-") {
                continue;
            }
            seen_ids.insert(stem.clone());

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "reindex: failed to read file, skipped");
                    report.skipped_unparseable += 1;
                    continue;
                }
            };

            let cached = self.cache.get(&stem).await.ok();
            let mut mem = match Importer::parse_markdown(&content) {
                Ok(m) => m,
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "reindex: unparseable markdown file, skipped");
                    report.skipped_unparseable += 1;
                    continue;
                }
            };
            // The filename is authoritative for id, not whatever the
            // frontmatter claims (guards against a renamed/copy-pasted file).
            mem.id = stem.clone();
            // One-time migration for files written before namespace
            // canonicalization: rewrite the frontmatter `n:` to the canonical
            // spelling. Without the rewrite the file would diff against its
            // canonical cache row on every pass and resync forever.
            let canonical_ns =
                crate::models::canonical_project_namespace(&mem.namespace).into_owned();
            if canonical_ns != mem.namespace {
                mem.namespace = canonical_ns;
                if let Err(e) = self.write_file_atomic(&stem, &Exporter::memory_to_markdown(&mem)) {
                    warn!(file = %path.display(), error = %e, "reindex: failed to canonicalize frontmatter namespace, skipped");
                    report.skipped_unparseable += 1;
                    continue;
                }
            }

            // Normalize the memory snapshot that we compare against
            // before checking whether the row is truly unchanged.
            let canonical_file = Self::quiesect_memory_for_reindex(&mem);
            let unchanged = cached.is_some()
                && cached
                    .as_ref()
                    .map(Self::quiesect_memory_for_reindex)
                    .as_ref()
                    .map(|existing| {
                        existing.clone().id == canonical_file.id
                            && existing.content == canonical_file.content
                            && existing.priority == canonical_file.priority
                            && existing.source_agent.id == canonical_file.source_agent.id
                            && existing.source_agent.agent_type
                                == canonical_file.source_agent.agent_type
                    })
                    .unwrap_or(false);
            if unchanged {
                continue;
            }

            let had_cache_row = cached.is_some();

            if had_cache_row && let Err(e) = self.cache.delete(&stem).await {
                warn!(id = %stem, error = %e, "reindex: failed to clear stale cache row, skipped");
                continue;
            }
            match self.cache.save(mem).await {
                Ok(_) => {
                    if had_cache_row {
                        report.resynced += 1;
                    } else {
                        report.added += 1;
                    }
                }
                Err(e) => warn!(id = %stem, error = %e, "reindex: failed to (re)save into cache"),
            }
        }

        let cached_all = self.cache.list(None, 1_000_000, 0).await?;
        for mem in cached_all {
            if !seen_ids.contains(&mem.id) {
                match self.cache.delete(&mem.id).await {
                    Ok(()) => report.removed += 1,
                    Err(e) => {
                        warn!(id = %mem.id, error = %e, "reindex: failed to drop orphaned cache row")
                    }
                }
            }
        }

        Ok(report)
    }

    /// Poll [`Self::reindex`] on an interval until `stop_signal` is set.
    /// Blocks indefinitely; suitable for a daemon/watch mode. Mirrors
    /// `SyncEngine::sync_with_watch`'s polling idiom — no file-watcher
    /// dependency, just a cheap periodic diff.
    pub async fn reindex_watch(
        &self,
        interval_secs: u64,
        stop_signal: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        while !stop_signal.load(Ordering::Relaxed) {
            interval.tick().await;
            match self.reindex().await {
                Ok(report) if report.changed() > 0 => {
                    info!(
                        added = report.added,
                        resynced = report.resynced,
                        removed = report.removed,
                        "reindex: picked up external changes"
                    );
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "reindex failed"),
            }
        }
        info!("markdown reindex watch loop stopped");
        Ok(())
    }
}

#[async_trait]
impl MemoryStore for MarkdownStore {
    async fn save(&self, memory: Memory) -> Result<Memory> {
        memory.validate_content()?;
        Self::validate_id(&memory.id)?;
        // The file's frontmatter is the source of truth, so canonicalize the
        // namespace BEFORE writing it — the cache write below would
        // canonicalize anyway, and a file left non-canonical would make every
        // subsequent `reindex` diff against the canonical cache row forever.
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();
        let id = memory.id.clone();
        let content = Exporter::memory_to_markdown(&memory);
        self.write_file_atomic(&id, &content)?;
        self.cache.save(memory).await.inspect_err(|_| {
            let _ = std::fs::remove_file(self.file_path(&id));
        })
    }

    async fn save_with_embedding(&self, memory: Memory, embedding: Vec<f32>) -> Result<Memory> {
        memory.validate_content()?;
        Self::validate_id(&memory.id)?;
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();
        let id = memory.id.clone();
        let content = Exporter::memory_to_markdown(&memory);
        self.write_file_atomic(&id, &content)?;
        self.cache
            .save_with_embedding(memory, embedding)
            .await
            .inspect_err(|_| {
                let _ = std::fs::remove_file(self.file_path(&id));
            })
    }

    async fn get(&self, id: &str) -> Result<Memory> {
        self.cache.get(id).await
    }

    async fn update(&self, memory: Memory) -> Result<Memory> {
        memory.validate_content()?;
        Self::validate_id(&memory.id)?;
        // Markdown is canonical: the file is rewritten first. If the
        // matching cache write then fails, the file and cache disagree —
        // the next `reindex` repairs it (treats the file as the newer
        // truth and resyncs), same as any other drift.
        let mut memory = memory;
        memory.namespace =
            crate::models::canonical_project_namespace(&memory.namespace).into_owned();
        let content = Exporter::memory_to_markdown(&memory);
        self.write_file_atomic(&memory.id, &content)?;
        self.cache.update(memory).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        Self::validate_id(id)?;
        // Validate existence (NotFound contract) before touching the file.
        self.cache.get(id).await?;
        let path = self.file_path(id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                MemVaultError::Storage(format!("failed to delete {}: {}", path.display(), e))
            })?;
        }
        self.cache.delete(id).await
    }

    async fn purge(&self, id: &str) -> Result<()> {
        Self::validate_id(id)?;
        // Validate existence (NotFound contract) before touching the file.
        self.cache.get(id).await?;
        let path = self.file_path(id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                MemVaultError::Storage(format!("failed to purge {}: {}", path.display(), e))
            })?;
        }
        // cache.purge (not cache.delete) also erases the cache's own
        // memory_history rows — delete() leaves a full-content snapshot in
        // the internal .memvault-cache.db forever, which would make
        // "permanently forgotten" on this backend a lie.
        self.cache.purge(id).await
    }

    async fn record_forgotten(&self, entry: &ForgottenLogEntry) -> Result<()> {
        self.cache.record_forgotten(entry).await
    }

    async fn list_forgotten_log(
        &self,
        memory_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ForgottenLogEntry>> {
        self.cache.list_forgotten_log(memory_id, limit).await
    }

    async fn search(&self, query: SearchQuery) -> Result<SearchOutcome> {
        self.cache.search(query).await
    }

    async fn vector_search(
        &self,
        embedding: &[f32],
        top_k: usize,
        namespace: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        self.cache.vector_search(embedding, top_k, namespace).await
    }

    async fn list(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        self.cache.list(namespace, limit, offset).await
    }

    async fn get_embedding(&self, id: &str) -> Result<Option<Vec<f32>>> {
        self.cache.get_embedding(id).await
    }

    async fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        self.cache.set_embedding(id, embedding).await
    }

    async fn clear_embedding(&self, id: &str) -> Result<()> {
        self.cache.clear_embedding(id).await
    }

    async fn record_access(&self, ids: &[String]) -> Result<usize> {
        self.cache.record_access(ids).await
    }

    async fn list_without_embedding(&self, limit: usize) -> Result<Vec<Memory>> {
        self.cache.list_without_embedding(limit).await
    }

    async fn list_pending(
        &self,
        namespace: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        self.cache.list_pending(namespace, limit, offset).await
    }

    async fn sync_state_hash(&self) -> Result<u64> {
        self.cache.sync_state_hash().await
    }

    async fn record_episode(&self, episode: EpisodeRecord) -> Result<()> {
        self.cache.record_episode(episode).await
    }

    async fn get_episode(&self, memory_id: &str) -> Result<EpisodeRecord> {
        self.cache.get_episode(memory_id).await
    }

    async fn list_episodes(&self, filter: EpisodeFilter) -> Result<Vec<EpisodeRecord>> {
        self.cache.list_episodes(filter).await
    }

    async fn update_episode_lesson(
        &self,
        memory_id: &str,
        lesson: &str,
        lesson_memory_id: Option<&str>,
    ) -> Result<()> {
        self.cache
            .update_episode_lesson(memory_id, lesson, lesson_memory_id)
            .await
    }

    async fn record_skill_injection(&self, skill_memory_id: &str) -> Result<()> {
        self.cache.record_skill_injection(skill_memory_id).await
    }

    async fn record_skill_outcome(&self, skill_memory_id: &str, success: bool) -> Result<()> {
        self.cache
            .record_skill_outcome(skill_memory_id, success)
            .await
    }

    async fn get_skill_stats(&self, skill_memory_id: &str) -> Result<Option<SkillStats>> {
        self.cache.get_skill_stats(skill_memory_id).await
    }

    async fn supersede(&self, old_id: &str, new_id: &str) -> Result<()> {
        // Same shape as `SqliteStore::supersede` (get, flip fields, update)
        // but routed through *our* `update` so the markdown file is
        // rewritten too, not just the cache row.
        if old_id == new_id {
            return Err(MemVaultError::InvalidInput(
                "a memory cannot supersede itself".to_string(),
            ));
        }
        let old = MemoryStore::get(self, old_id).await?;
        MemoryStore::get(self, new_id).await?; // validates existence
        let mut updated = old;
        updated.superseded_by = Some(new_id.to_string());
        updated.layer = MemoryLayer::L0;
        updated.updated_at = chrono::Utc::now();
        MemoryStore::update(self, updated).await?;
        Ok(())
    }

    async fn list_shared(&self, limit: usize) -> Result<Vec<Memory>> {
        self.cache.list_shared(limit).await
    }

    async fn add_relation(&self, relation: MemoryRelation) -> Result<i64> {
        self.cache.add_relation(relation).await
    }

    async fn relations_of_subject(&self, memory_id: &str) -> Result<Vec<MemoryRelation>> {
        self.cache.relations_of_subject(memory_id).await
    }

    async fn relations_of_object(&self, memory_id: &str) -> Result<Vec<MemoryRelation>> {
        self.cache.relations_of_object(memory_id).await
    }

    async fn delete_relation(&self, relation_id: i64) -> Result<()> {
        self.cache.delete_relation(relation_id).await
    }

    async fn get_trace_watermark(
        &self,
        agent_key: &str,
        session_id: &str,
    ) -> Result<Option<TraceWatermark>> {
        self.cache.get_trace_watermark(agent_key, session_id).await
    }

    async fn set_trace_watermark(&self, watermark: &TraceWatermark) -> Result<()> {
        self.cache.set_trace_watermark(watermark).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "memvault_md_store_{label}_{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[tokio::test]
    async fn test_save_writes_file_and_cache() {
        let dir = temp_dir("save");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "hello markdown".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        assert!(dir.join(format!("{id}.md")).exists());
        let fetched = store.get(&id).await.unwrap();
        assert_eq!(fetched.content, "hello markdown");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_update_rewrites_file() {
        let dir = temp_dir("update");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "v1".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem.clone()).await.unwrap();

        mem.content = "v2".to_string();
        store.update(mem).await.unwrap();

        let on_disk = std::fs::read_to_string(dir.join(format!("{id}.md"))).unwrap();
        assert!(on_disk.contains("v2"));
        assert!(!on_disk.contains("v1\n"));

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_delete_removes_file_and_cache_row() {
        let dir = temp_dir("delete");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "to delete".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        store.delete(&id).await.unwrap();

        assert!(!dir.join(format!("{id}.md")).exists());
        assert!(store.get(&id).await.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    /// Regression: an id carrying `../` (or a path separator) must never reach
    /// the filesystem — `file_path` derives `{id}.md` from it, so an
    /// unvalidated import could write to, or delete, files outside the vault.
    #[tokio::test]
    async fn test_traversal_ids_are_rejected_before_touching_the_filesystem() {
        let dir = temp_dir("traversal");
        let store = MarkdownStore::open(&dir).await.unwrap();

        // `../escape` would resolve to this path if it were not rejected.
        let escape = dir.parent().unwrap().join("memvault_traversal_escape.md");
        std::fs::remove_file(&escape).ok();

        for evil in [
            "../memvault_traversal_escape",
            "../../etc/passwd",
            "a/b",
            "a\\b",
            ".hidden",
            "..",
            "",
        ] {
            let mut mem = Memory::new(
                MemoryType::Fact,
                "malicious payload".to_string(),
                Priority::Reference,
                agent(),
            );
            mem.id = evil.to_string();

            let err = store
                .save(mem)
                .await
                .expect_err("a traversal-capable id must be rejected");
            assert!(
                matches!(err, MemVaultError::InvalidInput(_)),
                "expected InvalidInput for {evil:?}, got {err:?}"
            );
            assert!(
                store.delete(evil).await.is_err(),
                "delete must reject {evil:?}"
            );
        }

        assert!(
            !escape.exists(),
            "no file may be created outside the markdown directory"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_validate_id_accepts_canonical_ids_and_rejects_unsafe_ones() {
        let long_ok = "x".repeat(200);
        for ok in ["mem_0123abcd", "note-1", "a.b_c-D9", long_ok.as_str()] {
            assert!(
                MarkdownStore::validate_id(ok).is_ok(),
                "{ok:?} should be a valid id"
            );
        }
        let long_bad = "x".repeat(201);
        for bad in [
            "",
            "..",
            "a..b",
            "../up",
            "/abs",
            "a/b",
            "a\\b",
            ".hidden",
            long_bad.as_str(),
        ] {
            assert!(
                MarkdownStore::validate_id(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn test_purge_removes_file_and_cache_history_row() {
        // delete() leaves a full-content snapshot in the internal cache's
        // memory_history forever (restorable via the cache's own
        // SqliteStore::restore_checkpoint); purge() must not.
        let dir = temp_dir("purge");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "to purge".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        store.purge(&id).await.unwrap();

        assert!(!dir.join(format!("{id}.md")).exists());
        assert!(store.get(&id).await.is_err());
        assert!(
            store
                .cache
                .list_checkpoints(Some(&id), 10)
                .await
                .unwrap()
                .is_empty(),
            "purge must also clear the cache's memory_history for this id"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_record_and_list_forgotten_log_via_cache() {
        let dir = temp_dir("forgotten_log");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let entry = ForgottenLogEntry {
            log_id: None,
            memory_id: "mem_md_erased".to_string(),
            content_hash: "deadbeef".to_string(),
            namespace: "archived:global".to_string(),
            priority: Priority::Reference,
            visibility: Visibility::Scoped,
            scope: ForgetScope::Manual,
            reason: None,
            requested_by: "cli".to_string(),
            must_override: false,
            erased_at: chrono::Utc::now(),
        };
        store.record_forgotten(&entry).await.unwrap();

        let rows = store.list_forgotten_log(None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_id, "mem_md_erased");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_picks_up_hand_created_file() {
        let dir = temp_dir("hand_created");
        let store = MarkdownStore::open(&dir).await.unwrap();
        std::fs::write(
            dir.join("mem_handwritten.md"),
            "---\nid: mem_handwritten\ntype: fact\npriority: REFERENCE\n---\n\nwritten by hand\n",
        )
        .unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.resynced, 0);

        let fetched = store.get("mem_handwritten").await.unwrap();
        assert_eq!(fetched.content, "written by hand");

        std::fs::remove_dir_all(dir).ok();
    }

    /// `open_with_report` must surface the open-time self-heal numbers —
    /// the work an explicit follow-up `reindex()` can no longer report (it
    /// always finds an already-healed cache and returns zeros).
    #[tokio::test]
    async fn test_open_with_report_surfaces_initial_reindex() {
        let dir = temp_dir("open_with_report");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mem_pre.md"),
            "---\nid: mem_pre\ntype: fact\npriority: REFERENCE\n---\n\npre-existing\n",
        )
        .unwrap();

        let (store, report) = MarkdownStore::open_with_report(&dir).await.unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.resynced, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(store.get("mem_pre").await.unwrap().content, "pre-existing");

        // The explicit second pass is a no-op — exactly the misleading
        // zeros the open-time report exists to disambiguate.
        let again = store.reindex().await.unwrap();
        assert_eq!(again.added, 0);
        assert_eq!(again.resynced, 0);
        assert_eq!(again.removed, 0);

        std::fs::remove_dir_all(dir).ok();
    }

    /// 回归(复查发现):手编文件触发 resync 时,缓存行必须保留 frontmatter
    /// 里的原始创建时间 —— `created:`/`updated:` 曾只写不读,resync 后
    /// created_at 变成编辑时刻,记忆的"年龄"被静默改写。
    #[tokio::test]
    async fn test_resync_preserves_created_at_from_frontmatter() {
        let dir = temp_dir("resync_created_at");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "original".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();
        let original = store.get(&id).await.unwrap();

        // Externally edit the content; the file still carries its `created:`.
        let path = dir.join(format!("{id}.md"));
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("original", "edited externally");
        std::fs::write(&path, edited).unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.resynced, 1);

        let resynced = store.get(&id).await.unwrap();
        assert_eq!(resynced.content, "edited externally");
        assert_eq!(
            resynced.created_at, original.created_at,
            "resync must preserve the frontmatter's creation time, not reset it to the edit time"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_resyncs_externally_edited_file() {
        let dir = temp_dir("external_edit");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "original".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // Simulate an external editor changing the file directly, bypassing
        // the store entirely.
        let path = dir.join(format!("{id}.md"));
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("original", "edited externally");
        std::fs::write(&path, edited).unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.resynced, 1);
        assert_eq!(report.added, 0);

        let fetched = store.get(&id).await.unwrap();
        assert_eq!(fetched.content, "edited externally");

        std::fs::remove_dir_all(dir).ok();
    }

    /// Reindex must be idempotent across repeated calls, not just within a
    /// single pass: once an external edit has been resynced, a second
    /// reindex with no further changes must report an all-zero report
    /// instead of resyncing the same row again (which would also spuriously
    /// rewrite its cache timestamp).
    #[tokio::test]
    async fn test_reindex_second_pass_after_resync_is_noop() {
        let dir = temp_dir("external_edit_twice");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "original".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let path = dir.join(format!("{id}.md"));
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("original", "edited externally");
        std::fs::write(&path, edited).unwrap();

        let first = store.reindex().await.unwrap();
        assert_eq!(first.resynced, 1);

        let second = store.reindex().await.unwrap();
        assert_eq!(
            second.changed(),
            0,
            "a second reindex with no further file changes must be a no-op, got {:?}",
            second
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// One reindex pass covering an add, an edit and a delete together must
    /// attribute each file to the right bucket instead of mis-sorting them.
    #[tokio::test]
    async fn test_reindex_combined_add_edit_delete_in_one_pass() {
        let dir = temp_dir("combined_add_edit_delete");
        let store = MarkdownStore::open(&dir).await.unwrap();

        let to_edit = Memory::new(
            MemoryType::Fact,
            "will be edited".to_string(),
            Priority::Reference,
            agent(),
        );
        let to_delete = Memory::new(
            MemoryType::Fact,
            "will be deleted".to_string(),
            Priority::Reference,
            agent(),
        );
        let edit_id = to_edit.id.clone();
        let delete_id = to_delete.id.clone();
        store.save(to_edit).await.unwrap();
        store.save(to_delete).await.unwrap();

        // Edit one existing file directly.
        let edit_path = dir.join(format!("{edit_id}.md"));
        let edited = std::fs::read_to_string(&edit_path)
            .unwrap()
            .replace("will be edited", "edited by hand");
        std::fs::write(&edit_path, edited).unwrap();

        // Delete the other existing file directly.
        std::fs::remove_file(dir.join(format!("{delete_id}.md"))).unwrap();

        // Hand-create a brand new file.
        std::fs::write(
            dir.join("mem_added_by_hand.md"),
            "---\nid: mem_added_by_hand\ntype: fact\npriority: REFERENCE\n---\n\nadded by hand\n",
        )
        .unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.added, 1, "the hand-created file: {:?}", report);
        assert_eq!(report.resynced, 1, "the hand-edited file: {:?}", report);
        assert_eq!(report.removed, 1, "the hand-deleted file: {:?}", report);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_drops_cache_row_for_deleted_file() {
        let dir = temp_dir("hand_deleted");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "will be hand-deleted".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        std::fs::remove_file(dir.join(format!("{id}.md"))).unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.removed, 1);
        assert!(store.get(&id).await.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_is_noop_when_nothing_changed() {
        let dir = temp_dir("noop");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "steady state".to_string(),
            Priority::Reference,
            agent(),
        );
        store.save(mem).await.unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.changed(), 0);
        assert_eq!(report.skipped_unparseable, 0);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_skips_unparseable_file_without_failing() {
        let dir = temp_dir("unparseable");
        let store = MarkdownStore::open(&dir).await.unwrap();
        std::fs::write(dir.join("mem_bad.md"), "not valid frontmatter at all").unwrap();

        let report = store.reindex().await.unwrap();
        assert_eq!(report.skipped_unparseable, 1);
        assert!(store.get("mem_bad").await.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_open_twice_without_closing_fails_lock() {
        let dir = temp_dir("lock");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let second = MarkdownStore::open(&dir).await;
        assert!(second.is_err(), "a second open must fail on the lock file");
        drop(store);

        // Dropping the first store releases the lock; a third open succeeds.
        let third = MarkdownStore::open(&dir).await;
        assert!(
            third.is_ok(),
            "lock must be released when the store is dropped"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_supersede_rewrites_markdown_file() {
        let dir = temp_dir("supersede");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let old = Memory::new(
            MemoryType::Fact,
            "old fact".to_string(),
            Priority::Reference,
            agent(),
        );
        let new = Memory::new(
            MemoryType::Fact,
            "new fact".to_string(),
            Priority::Reference,
            agent(),
        );
        let old_id = old.id.clone();
        let new_id = new.id.clone();
        store.save(old).await.unwrap();
        store.save(new).await.unwrap();

        store.supersede(&old_id, &new_id).await.unwrap();

        let on_disk = std::fs::read_to_string(dir.join(format!("{old_id}.md"))).unwrap();
        assert!(on_disk.contains(&format!("superseded_by: \"{new_id}\"")));
        let fetched = store.get(&old_id).await.unwrap();
        assert_eq!(fetched.superseded_by, Some(new_id));

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_save_rejects_sensitive_content_and_cleans_up_the_file() {
        let dir = temp_dir("sensitive_rollback");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "prod key is AKIAABCDEFGHIJKLMNOP, keep it safe".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();

        let result = store.save(mem).await;
        assert!(
            result.is_err(),
            "sensitive content must be rejected by the cache's backstop check"
        );
        assert!(
            !dir.join(format!("{id}.md")).exists(),
            "the file written before the cache rejection must be rolled back, \
             not left as an orphan"
        );
        assert!(store.get(&id).await.is_err());

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_save_with_embedding_rejects_sensitive_content_and_cleans_up_the_file() {
        let dir = temp_dir("sensitive_rollback_embed");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "token: eyJhbGciOi.abcdef".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();

        let result = store.save_with_embedding(mem, vec![0.1, 0.2, 0.3]).await;
        assert!(result.is_err());
        assert!(
            !dir.join(format!("{id}.md")).exists(),
            "the file must be rolled back when save_with_embedding is rejected"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_watch_picks_up_external_edit_then_stops() {
        let dir = temp_dir("watch");
        let store = Arc::new(MarkdownStore::open(&dir).await.unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "original watched content".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let watch_store = store.clone();
        let watch_stop = stop.clone();
        let handle = tokio::spawn(async move {
            watch_store.reindex_watch(1, watch_stop).await.unwrap();
        });

        // Simulate an external editor changing the file directly while the
        // watch loop is running, bypassing the store entirely.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let path = dir.join(format!("{id}.md"));
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("original watched content", "edited while watching");
        std::fs::write(&path, edited).unwrap();

        // Give the 1s-interval loop time to tick at least once and reindex.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        stop.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("watch loop must stop promptly after the stop signal")
            .unwrap();

        let fetched = store.get(&id).await.unwrap();
        assert_eq!(fetched.content, "edited while watching");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_pass_through_methods_reach_the_cache() {
        let dir = temp_dir("passthrough");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mem = Memory::new(
            MemoryType::Fact,
            "searchable".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let outcome = store
            .search(SearchQuery::new("searchable".to_string()))
            .await
            .unwrap();
        assert!(outcome.results.iter().any(|r| r.memory.id == id));

        let listed = store.list(None, 10, 0).await.unwrap();
        assert_eq!(listed.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    /// The markdown backend implements the whole `MemoryStore` trait by
    /// forwarding every non-file operation to its internal SQLite cache. Each
    /// forward is a one-liner, but it is still a real trait impl: exercise the
    /// embedding / episode / skill / relation / watermark / access delegations
    /// so the wiring (right cache method, right argument order) is covered, not
    /// just the file-backed `save`/`get`/`search`/`list` paths.
    #[tokio::test]
    async fn test_pass_through_embedding_episode_skill_relation_watermark() {
        let dir = temp_dir("passthrough_full");
        let store = MarkdownStore::open(&dir).await.unwrap();

        let mem = Memory::new(
            MemoryType::Fact,
            "delegated fact".to_string(),
            Priority::Reference,
            agent(),
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // --- embeddings: set → get → vector_search → clear ---
        assert!(store.get_embedding(&id).await.unwrap().is_none());
        store.set_embedding(&id, vec![1.0, 2.0, 3.0]).await.unwrap();
        // The cache normalizes on write, so assert the round-trip preserves
        // the dimension rather than the raw magnitudes.
        let got = store
            .get_embedding(&id)
            .await
            .unwrap()
            .expect("embedding set");
        assert_eq!(got.len(), 3, "embedding dimension preserved");
        // Reaches the cache; the just-set vector is the only candidate.
        let _hits = store
            .vector_search(&[1.0, 2.0, 3.0], 5, None)
            .await
            .unwrap();
        store.clear_embedding(&id).await.unwrap();
        assert!(store.get_embedding(&id).await.unwrap().is_none());
        // Without an embedding the memory shows up in the backfill list.
        let missing = store.list_without_embedding(10).await.unwrap();
        assert!(missing.iter().any(|m| m.id == id));

        // --- access counting / listing / hashing delegations ---
        assert_eq!(
            store
                .record_access(std::slice::from_ref(&id))
                .await
                .unwrap(),
            1
        );
        let _pending = store.list_pending(None, 10, 0).await.unwrap();
        let _shared = store.list_shared(10).await.unwrap();
        let _hash = store.sync_state_hash().await.unwrap();

        // --- episodes: record → get → list → update lesson ---
        store
            .record_episode(EpisodeRecord {
                memory_id: id.clone(),
                task: "deploy the dashboard".into(),
                task_type: Some("deploy".into()),
                status: OutcomeStatus::Failure,
                cause: Some("missing env var".into()),
                lesson: None,
                lesson_memory_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let ep = store.get_episode(&id).await.unwrap();
        assert_eq!(ep.task, "deploy the dashboard");
        assert_eq!(ep.status, OutcomeStatus::Failure);
        let eps = store.list_episodes(EpisodeFilter::default()).await.unwrap();
        assert!(eps.iter().any(|e| e.memory_id == id));
        store
            .update_episode_lesson(&id, "set the env var", None)
            .await
            .unwrap();
        assert_eq!(
            store.get_episode(&id).await.unwrap().lesson.as_deref(),
            Some("set the env var")
        );

        // --- skill stats: injection + outcomes accumulate via the cache ---
        let skill = Memory::new(
            MemoryType::Skill,
            "deploy runbook".to_string(),
            Priority::Reference,
            agent(),
        );
        let skill_id = skill.id.clone();
        store.save(skill).await.unwrap();
        store.record_skill_injection(&skill_id).await.unwrap();
        store.record_skill_outcome(&skill_id, true).await.unwrap();
        let stats = store
            .get_skill_stats(&skill_id)
            .await
            .unwrap()
            .expect("tracked");
        assert_eq!(stats.injected_count, 1);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.executions(), 1);

        // --- relations: add → query → delete ---
        let rid = store
            .add_relation(MemoryRelation {
                relation_id: None,
                subject_id: id.clone(),
                predicate: "located_in".into(),
                object_id: None,
                object_text: Some("prod-cluster".into()),
                confidence: 0.9,
                source_memory_id: None,
                created_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        assert!(rid >= 1);
        let out = store.relations_of_subject(&id).await.unwrap();
        assert!(out.iter().any(|r| r.predicate == "located_in"));
        let _inbound = store.relations_of_object(&id).await.unwrap();
        store.delete_relation(rid).await.unwrap();
        assert!(store.relations_of_subject(&id).await.unwrap().is_empty());

        // --- trace watermark: none → set → round-trip ---
        assert!(
            store
                .get_trace_watermark("claude-code", "sess-1")
                .await
                .unwrap()
                .is_none()
        );
        store
            .set_trace_watermark(&TraceWatermark {
                agent_key: "claude-code".into(),
                session_id: "sess-1".into(),
                last_seq: 7,
                updated_at: chrono::Utc::now(),
            })
            .await
            .unwrap();
        let wm = store
            .get_trace_watermark("claude-code", "sess-1")
            .await
            .unwrap()
            .expect("watermark round-trips");
        assert_eq!(wm.last_seq, 7);

        std::fs::remove_dir_all(dir).ok();
    }
    #[tokio::test]
    async fn test_save_canonicalizes_project_namespace_in_file_and_cache() {
        let dir = temp_dir("ns_canon");
        let store = MarkdownStore::open(&dir).await.unwrap();
        let mut mem = Memory::new(
            MemoryType::Fact,
            "canonical file".to_string(),
            Priority::Reference,
            agent(),
        );
        mem.namespace = "project:MemVault/".to_string();
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        // The file frontmatter stores the canonical spelling...
        let file_content = std::fs::read_to_string(dir.join(format!("{id}.md"))).unwrap();
        assert!(
            file_content.contains("namespace: project:memvault"),
            "file frontmatter must be canonical"
        );
        assert!(
            !file_content.contains("project:MemVault"),
            "no non-canonical spelling may persist"
        );

        // ...and reads match whatever spelling the caller uses.
        let listed = store.list(Some("project:MemVault"), 10, 0).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].namespace, "project:memvault");

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_reindex_rewrites_legacy_noncanonical_namespace() {
        let dir = temp_dir("ns_legacy");
        std::fs::create_dir_all(&dir).unwrap();
        // A file written before namespace canonicalization still carries the
        // old spelling in its frontmatter.
        let legacy = "---
id: legacy_ns
type: fact
priority: REFERENCE
namespace: project:MemVault
confidence: 0.9
source_agent: test
---
legacy canonicalization test
";
        std::fs::write(dir.join("legacy_ns.md"), legacy).unwrap();

        let store = MarkdownStore::open(&dir).await.unwrap();

        let listed = store.list(Some("project:memvault"), 10, 0).await.unwrap();
        assert_eq!(
            listed.len(),
            1,
            "legacy row must be reachable by the canonical spelling"
        );
        assert_eq!(listed[0].namespace, "project:memvault");

        // The file itself was rewritten to the canonical spelling.
        let file_content = std::fs::read_to_string(dir.join("legacy_ns.md")).unwrap();
        assert!(
            file_content.contains("namespace: project:memvault"),
            "frontmatter must be rewritten once"
        );
        assert!(!file_content.contains("project:MemVault"));

        // Converged: a second reindex reports no changes (no resync loop).
        let report = store.reindex().await.unwrap();
        assert_eq!(
            report.changed(),
            0,
            "reindex must settle after canonicalization"
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
