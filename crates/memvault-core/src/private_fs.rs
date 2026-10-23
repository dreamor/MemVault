//! Best-effort permission hardening for MemVault's local data files.
//!
//! `~/.memvault/` holds every memory, every extracted preference, and the
//! compliance/eval-history side tables. Created with a typical `022` umask
//! those land `0644`/`0755` — world-readable on a shared machine. These
//! helpers tighten the *data* directory to `0700` (only when MemVault creates
//! it) and the database file (plus SQLite's `-wal`/`-shm` siblings) to `0600`.
//!
//! Everything here is best-effort: an unsupported filesystem, a file we do not
//! own, or a sandbox that rejects `chmod` must never stop MemVault from opening
//! its own store, so failures are logged at debug and swallowed.

use std::path::Path;

/// Create `dir` (and parents) like `create_dir_all`, then tighten the leaf
/// directory to `0700` — but only when this call actually created it. An
/// existing directory is left untouched: we must never clobber a deliberately
/// chosen mode, and more importantly must never `chmod` a pre-existing parent
/// (e.g. a `--db /var/lib/memvault/data.db` whose `/var/lib` already exists).
pub fn create_dir_all_private(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir);
    Ok(())
}

/// Tighten `path` to `0600` (owner read/write) when it exists. No-op off unix
/// or when the file is absent.
pub fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists()
            && let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::debug!(path = %path.display(), error = %e, "could not restrict file permissions");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Tighten `dir` to `0700` when it is an existing directory. No-op off unix.
pub fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if dir.is_dir()
            && let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        {
            tracing::debug!(path = %dir.display(), error = %e, "could not restrict directory permissions");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Tighten a SQLite database file plus its `-wal`/`-shm` siblings.
///
/// SQLite copies the main database file's permissions when it creates the
/// journal files, so calling this after the first connection has opened (and
/// switched to WAL) is what keeps those siblings private too.
pub fn restrict_sqlite_files(path: &Path) {
    restrict_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut sibling = path.as_os_str().to_os_string();
        sibling.push(suffix);
        restrict_file(Path::new(&sibling));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "memvault_private_fs_{label}_{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn create_dir_all_private_creates_and_tightens_only_new_dirs() {
        let base = temp_path("dir");
        let nested = base.join("nested");
        create_dir_all_private(&nested).unwrap();
        assert!(nested.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "newly created data dir must be 0700");
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn create_dir_all_private_leaves_existing_dir_mode_alone() {
        let base = temp_path("keep");
        std::fs::create_dir_all(&base).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o751)).unwrap();
            create_dir_all_private(&base).unwrap();
            let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o751, "pre-existing dir mode must not be clobbered");
        }
        #[cfg(not(unix))]
        {
            create_dir_all_private(&base).unwrap();
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn restrict_file_hardens_db_and_wal_siblings() {
        use std::os::unix::fs::PermissionsExt;
        let base = temp_path("file");
        std::fs::create_dir_all(&base).unwrap();
        let db = base.join("data.db");
        std::fs::write(&db, b"x").unwrap();
        std::fs::write(base.join("data.db-wal"), b"x").unwrap();
        std::fs::write(base.join("data.db-shm"), b"x").unwrap();

        restrict_sqlite_files(&db);

        for p in [&db, &base.join("data.db-wal"), &base.join("data.db-shm")] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} must be 0600", p.display());
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn restrict_helpers_tolerate_missing_paths() {
        let missing = temp_path("missing");
        restrict_file(&missing);
        restrict_dir(&missing);
        restrict_sqlite_files(&missing);
    }
}
