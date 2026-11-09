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

/// Atomically replace `target` with `contents`, keeping the file private
/// (`0600` on unix) **throughout** — unlike the best-effort helpers above,
/// this one is strict: a failure to write privately is a failure to write.
///
/// Why not `std::fs::write` + `restrict_file` after the rename: the payload
/// here is the env file, API keys included, and a plain write creates the
/// *temp* file at the default mode (`0644` under a typical `022` umask). The
/// secret sits world-readable for the whole write window, and a `chmod`
/// after the fact closes the barn with the horse already photographed. So:
///
/// 1. create the temp sibling with `O_EXCL` and mode `0600` — exclusive so a
///    concurrent writer cannot race us, and so a stale temp left by a crashed
///    run is *detected* rather than silently appended to (it is removed once,
///    then we insist on creating it fresh);
/// 2. write + fsync, so the rename below cannot expose a truncated file;
/// 3. rename over the target — atomic on both unix and Windows, and the
///    target inherits the temp file's `0600` inode mode (rename moves the
///    inode, it does not copy), which also fixes up an `init`-created
///    `.env` that a previous version left at `0644`. Deliberately no
///    `restrict_file` afterwards: the final mode must be *evidence* that
///    the temp was born private, not a chmod papering over a public write.
pub fn write_private_atomic(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = target.as_os_str().to_os_string();
    tmp_name.push(".tmp-private");
    let tmp = Path::new(&tmp_name);

    let write_once = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(tmp)?;
        std::io::Write::write_all(&mut file, contents)?;
        file.sync_all()
    };

    if let Err(e) = write_once() {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e);
        }
        // Stale temp from a crashed earlier run: drop it and insist on a
        // fresh exclusive create rather than trusting (or appending to)
        // whatever is already on disk.
        std::fs::remove_file(tmp)?;
        write_once()?;
    }

    match std::fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Never leave the secret-bearing temp behind on failure.
            std::fs::remove_file(tmp).ok();
            Err(e)
        }
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

    #[test]
    fn write_private_atomic_replaces_content_and_tightens_mode() {
        let target = temp_path("wpa");
        // Pre-existing world-readable file, like an init-created .env from
        // before this helper existed.
        std::fs::write(&target, b"OLD=1").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        write_private_atomic(&target, b"NEW=2").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"NEW=2");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "final file must be owner-only");
        }
        // The temp sibling must not survive the rename.
        let mut tmp = target.as_os_str().to_os_string();
        tmp.push(".tmp-private");
        assert!(!Path::new(&tmp).exists(), "temp file must be gone");
        std::fs::remove_file(&target).ok();
    }

    #[cfg(unix)]
    #[test]
    fn write_private_atomic_temp_file_is_never_world_readable() {
        // The audit's core complaint: the secret sits in the temp file, so
        // the temp itself must be born 0600 — not chmod'd after the write.
        // We cannot observe the temp mid-write from here, but we can assert
        // the observable contract: after a FAILED rename (target path is a
        // directory → rename fails), the temp is cleaned up, and on the
        // success path the mode check above covers the final file. Here we
        // additionally verify the failure path leaves no secret behind.
        use std::os::unix::fs::PermissionsExt;
        let target = temp_path("wpa_fail");
        std::fs::create_dir_all(&target).unwrap(); // rename onto a dir fails
        let err = write_private_atomic(&target, b"SECRET=1").unwrap_err();
        assert!(err.kind() != std::io::ErrorKind::AlreadyExists);
        let mut tmp = target.as_os_str().to_os_string();
        tmp.push(".tmp-private");
        assert!(
            !Path::new(&tmp).exists(),
            "failed write must not leave the secret-bearing temp on disk"
        );
        // Sanity: mode of a successfully created temp is checked via the
        // final-file test above (rename preserves the inode mode).
        let _ = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir_all(&target).ok();
    }

    #[test]
    fn write_private_atomic_recovers_from_stale_temp() {
        let target = temp_path("wpa_stale");
        let mut tmp = target.as_os_str().to_os_string();
        tmp.push(".tmp-private");
        // Crashed earlier run left a temp behind, possibly with partial
        // secrets in it. Must be discarded, not appended to or trusted.
        std::fs::write(&tmp, b"stale garbage").unwrap();

        write_private_atomic(&target, b"CLEAN=1").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"CLEAN=1");
        assert!(!Path::new(&tmp).exists());
        std::fs::remove_file(&target).ok();
    }
}
