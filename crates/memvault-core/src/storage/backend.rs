//! Storage backend selection (Phase 4): `sqlite` (default) or `markdown`.
//!
//! Two side-stores bypass the `MemoryStore` trait entirely and open their
//! own SQLite connection straight at a file path — `ComplianceStore` and
//! `EvalHistoryStore` (`CREATE TABLE IF NOT EXISTS` their own tables, sharing
//! whatever file the main store already uses). [`OpenedStore::side_db_path`]
//! is that shared file: the main `data.db` for the sqlite backend, or the
//! markdown backend's internal cache file. Callers that need it should use
//! `side_db_path`, not re-derive a path themselves, so compliance/eval-history
//! stay colocated with whichever store is actually active.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::MemoryStore;
use super::markdown::MarkdownStore;
use super::sqlite::SqliteStore;
use crate::error::{MemVaultError, Result};

/// Which `MemoryStore` implementation to construct. `Sqlite` is the
/// long-standing default; `Markdown` is Phase 4, EverOS-inspired (see docs/DESIGN.md §18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Sqlite,
    Markdown,
}

impl StorageBackend {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "sqlite" => Some(Self::Sqlite),
            "markdown" | "md" => Some(Self::Markdown),
            _ => None,
        }
    }

    /// Read `MEMVAULT_STORAGE_BACKEND`; unset or unrecognized defaults to
    /// `Sqlite` (backward compatible — existing installs never set this).
    pub fn from_env() -> Self {
        std::env::var("MEMVAULT_STORAGE_BACKEND")
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or(Self::Sqlite)
    }
}

/// A constructed store plus the bits callers need that the `MemoryStore`
/// trait doesn't carry.
pub struct OpenedStore {
    pub store: Arc<dyn MemoryStore>,
    /// `Some` only when the sqlite backend is active — lets the handful of
    /// CLI/REST features that use SQLite-only operations (`backup_to`,
    /// `list_checkpoints`, `restore_checkpoint`, `schema_fingerprint`) reach
    /// the concrete type. These are out of scope for the markdown backend in
    /// v1 (backup/checkpoint semantics are SQLite-specific); callers should
    /// error clearly when this is `None` rather than silently no-op.
    pub sqlite_store: Option<Arc<SqliteStore>>,
    /// `Some` only when the markdown backend is active — lets callers reach
    /// markdown-specific operations with no `MemoryStore` equivalent
    /// (`reindex`, `reindex_watch`).
    pub markdown_store: Option<Arc<MarkdownStore>>,
    /// The [`ReindexReport`] from the markdown store's on-open self-heal
    /// pass (`Some` only when the markdown backend is active). `open()`
    /// already applies every pending add/resync/remove before the caller
    /// gets the store, so a CLI that runs an explicit `reindex` afterwards
    /// would always print zeros; surfacing this report lets it show the
    /// work the open actually did.
    pub markdown_reindex: Option<super::markdown::ReindexReport>,
    /// Shared file for `ComplianceStore`/`EvalHistoryStore` (see module docs).
    pub side_db_path: PathBuf,
    pub backend: StorageBackend,
}

/// Open the store selected by `MEMVAULT_STORAGE_BACKEND`. Thin wrapper over
/// [`open_with_backend`] for the common case; callers that need a
/// deterministic choice (tests, or a future `--storage-backend` CLI flag)
/// should call `open_with_backend` directly instead of mutating the process
/// environment to influence this function.
pub async fn open(db_path: &Path, markdown_dir: Option<&Path>) -> Result<OpenedStore> {
    open_with_backend(StorageBackend::from_env(), db_path, markdown_dir).await
}

/// Open the explicitly given `backend`.
///
/// - `sqlite`: opens `db_path` directly (existing behavior, unchanged).
/// - `markdown`: opens `markdown_dir` (explicit override, else
///   `MEMVAULT_MARKDOWN_DIR`); `db_path` is ignored.
pub async fn open_with_backend(
    backend: StorageBackend,
    db_path: &Path,
    markdown_dir: Option<&Path>,
) -> Result<OpenedStore> {
    match backend {
        StorageBackend::Sqlite => {
            if let Some(parent) = db_path.parent() {
                crate::private_fs::create_dir_all_private(parent)
                    .map_err(|e| MemVaultError::Storage(e.to_string()))?;
            }
            let sqlite = Arc::new(SqliteStore::new(db_path)?);
            Ok(OpenedStore {
                store: sqlite.clone(),
                sqlite_store: Some(sqlite),
                markdown_store: None,
                markdown_reindex: None,
                side_db_path: db_path.to_path_buf(),
                backend: StorageBackend::Sqlite,
            })
        }
        StorageBackend::Markdown => {
            let dir = markdown_dir
                .map(|d| d.to_path_buf())
                .or_else(|| {
                    std::env::var("MEMVAULT_MARKDOWN_DIR")
                        .ok()
                        .map(PathBuf::from)
                })
                .ok_or_else(|| {
                    MemVaultError::InvalidInput(
                        "MEMVAULT_STORAGE_BACKEND=markdown requires --markdown-dir or \
                         MEMVAULT_MARKDOWN_DIR to be set"
                            .to_string(),
                    )
                })?;
            let (store, reindex_report) = MarkdownStore::open_with_report(&dir).await?;
            let md = Arc::new(store);
            Ok(OpenedStore {
                side_db_path: md.cache_db_path(),
                store: md.clone(),
                sqlite_store: None,
                markdown_store: Some(md),
                markdown_reindex: Some(reindex_report),
                backend: StorageBackend::Markdown,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_accepts_known_values_case_insensitively() {
        assert_eq!(
            StorageBackend::parse("sqlite"),
            Some(StorageBackend::Sqlite)
        );
        assert_eq!(
            StorageBackend::parse("SQLite"),
            Some(StorageBackend::Sqlite)
        );
        assert_eq!(
            StorageBackend::parse("markdown"),
            Some(StorageBackend::Markdown)
        );
        assert_eq!(StorageBackend::parse("MD"), Some(StorageBackend::Markdown));
        assert_eq!(StorageBackend::parse("nonsense"), None);
    }

    // `from_env`/`open` (the env-var-reading entry points) are exercised by
    // callers (CLI, mcp, proxy bootstrap); these tests use
    // `open_with_backend` directly to avoid mutating the process-global
    // `MEMVAULT_STORAGE_BACKEND` env var, which `cargo test`'s parallel
    // threads would otherwise race on with every other test that calls
    // `open`/`from_env` indirectly through `run()`.

    #[tokio::test]
    async fn test_open_sqlite_default_backend() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_backend_sqlite_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let db_path = dir.join("data.db");
        let opened = open_with_backend(StorageBackend::Sqlite, &db_path, None)
            .await
            .unwrap();
        assert_eq!(opened.backend, StorageBackend::Sqlite);
        assert!(opened.sqlite_store.is_some());
        assert!(opened.markdown_store.is_none());
        assert!(opened.markdown_reindex.is_none());
        assert_eq!(opened.side_db_path, db_path);
        std::fs::remove_dir_all(dir).ok();
    }

    /// The markdown backend must hand back the open-time reindex report so
    /// the CLI's `reindex` command can show the work the open actually did
    /// (an explicit second pass always reports zeros).
    #[tokio::test]
    async fn test_markdown_backend_carries_initial_reindex_report() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_backend_reindex_report_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mem_pre.md"),
            "---\nid: mem_pre\ntype: fact\npriority: REFERENCE\n---\n\npre-existing\n",
        )
        .unwrap();

        let opened = open_with_backend(
            StorageBackend::Markdown,
            &PathBuf::from("/unused/data.db"),
            Some(&dir),
        )
        .await
        .unwrap();
        let report = opened
            .markdown_reindex
            .expect("markdown backend must carry the open-time reindex report");
        assert_eq!(report.added, 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_open_markdown_backend_requires_dir() {
        let result = open_with_backend(
            StorageBackend::Markdown,
            &PathBuf::from("/unused/data.db"),
            None,
        )
        .await;
        assert!(result.is_err(), "markdown backend without a dir must error");
    }

    #[tokio::test]
    async fn test_open_markdown_backend_with_explicit_dir() {
        let dir = std::env::temp_dir().join(format!(
            "memvault_backend_markdown_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let opened = open_with_backend(
            StorageBackend::Markdown,
            &PathBuf::from("/unused/data.db"),
            Some(&dir),
        )
        .await
        .unwrap();
        assert_eq!(opened.backend, StorageBackend::Markdown);
        assert!(opened.sqlite_store.is_none());
        assert!(opened.markdown_store.is_some());
        assert_eq!(opened.side_db_path, dir.join(".memvault-cache.db"));
        std::fs::remove_dir_all(dir).ok();
    }
}
