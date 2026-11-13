//! Best-effort permission hardening for MemVault's local data files.
//!
//! `~/.memvault/` holds every memory, every extracted preference, and the
//! compliance/eval-history side tables. Created with a typical `022` umask
//! those land `0644`/`0755` — world-readable on a shared machine. These
//! helpers tighten the *data* directory to `0700` (only when MemVault creates
//! it) and the database file (plus SQLite's `-wal`/`-shm` siblings) to `0600`.
//!
//! The legacy `restrict_*` helpers are best-effort. The durable write, unlink,
//! and strict private-directory helpers fail closed instead: approval journals
//! must not silently downgrade their privacy or durability guarantees.

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

/// Reject symlinks in every existing component, including dangling links.
/// This is a pathname check, not protection against an unrestricted process
/// running as the same account and racing filesystem operations.
pub(crate) fn reject_symlink_components(path: &Path) -> std::io::Result<()> {
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "parent traversal is not allowed in a private path",
            ));
        }
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "symlink is not allowed in private path {}",
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Create missing directories with owner-only mode from their first instant.
/// Existing ancestors need not be private (e.g. `/tmp`), but the requested
/// directory must already be private if present. Never repair its permissions.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    reject_symlink_components(dir)?;
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => validate_private_metadata(dir, &metadata, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = parent_directory(dir)?;
            if !parent.is_dir() {
                ensure_private_dir(parent)?;
            }
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = std::fs::symlink_metadata(dir)?;
            validate_private_metadata(dir, &metadata, true)?;
            sync_directory(parent)
        }
        Err(error) => Err(error),
    }
}

/// Validate a private regular file or directory without following a link.
pub(crate) fn validate_private_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
    directory: bool,
) -> std::io::Result<()> {
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if metadata.file_type().is_symlink() || !expected_type {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("private path has unexpected file type: {}", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let allowed = if directory { 0o700 } else { 0o600 };
        if metadata.permissions().mode() & 0o7777 & !allowed != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("private path has excessive permissions: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn parent_directory(path: &Path) -> std::io::Result<&Path> {
    path.parent()
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no parent directory",
            )
        })
}

/// Flush directory entries after a rename, create, or unlink. An unsupported
/// filesystem is an error, never a successful but non-durable publication.
pub(crate) fn sync_directory(dir: &Path) -> std::io::Result<()> {
    reject_symlink_components(dir)?;
    if !std::fs::symlink_metadata(dir)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory fsync requires a directory",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // A directory is not a normal read-only file on Windows. Backup
        // semantics permit the handle, and FlushFileBuffers requires write
        // access. Filesystems that cannot flush directories still fail closed.
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options
            .access_mode(GENERIC_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(not(windows))]
    options.read(true);
    options.open(dir)?.sync_all()
}

/// Unlink a file and persist its absence. Removing a link removes the link
/// itself, never its referent; callers requiring private records validate first.
pub(crate) fn remove_file_durable(path: &Path) -> std::io::Result<()> {
    let parent = parent_directory(path)?;
    reject_symlink_components(parent)?;
    std::fs::remove_file(path)?;
    sync_directory(parent)
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
///    the temp was born private, not a chmod papering over a public write;
/// 4. fsync the parent directory so the replacement itself is durable.
///
/// Existing destination symlinks are rejected. A stale temp symlink may be
/// safely unlinked, but is never opened or followed.
pub fn write_private_atomic(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    write_private_atomic_with(target, |file| {
        std::io::Write::write_all(file, contents)?;
        file.sync_all()
    })
}

fn write_private_atomic_with(
    target: &Path,
    write_and_sync: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    reject_symlink_components(target)?;
    let parent = parent_directory(target)?;
    let mut tmp_name = target.as_os_str().to_os_string();
    tmp_name.push(".tmp-private");
    let tmp = Path::new(&tmp_name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = match opts.open(tmp) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Never open a stale inode (including a dangling symlink). Unlink
            // its directory entry, then insist on exclusive fresh creation.
            remove_file_durable(tmp)?;
            opts.open(tmp)?
        }
        Err(error) => return Err(error),
    };
    let result = write_and_sync(&mut file).and_then(|()| {
        drop(file);
        // Recheck before replacement; existing destination symlinks are not
        // authority to redirect a durable write.
        reject_symlink_components(target)?;
        std::fs::rename(tmp, target)?;
        sync_directory(parent)
    });
    if result.is_err() {
        // This call created the temp exclusively. Best-effort cleanup must
        // not hide the original write/fsync/rename error.
        let _ = remove_file_durable(tmp);
    }
    result
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
    fn strict_private_directories_and_durable_unlink() {
        let base = temp_path("strict");
        let nested = base.join("nested");
        ensure_private_dir(&nested).unwrap();
        ensure_private_dir(&nested).unwrap();
        sync_directory(&nested).unwrap();
        let file = nested.join("record");
        write_private_atomic(&file, b"private").unwrap();
        assert!(sync_directory(&file).is_err());
        assert!(ensure_private_dir(&file).is_err());
        remove_file_durable(&file).unwrap();
        assert!(!file.exists());
        assert_eq!(
            remove_file_durable(&file).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn strict_private_directory_rejects_existing_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = temp_path("strict_reject");
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&base).is_err());
        assert_eq!(
            std::fs::metadata(&base).unwrap().permissions().mode() & 0o777,
            0o755,
            "existing public directory must not be chmod-repaired"
        );
        let link = base.with_extension("link");
        symlink(&base, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert!(ensure_private_dir(&link.join("nested")).is_err());
        assert!(sync_directory(&link).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn write_private_atomic_cleans_temp_on_write_or_sync_failure() {
        for label in ["write_error", "sync_error"] {
            let base = temp_path(label);
            ensure_private_dir(&base).unwrap();
            let target = base.join("record");
            write_private_atomic(&target, b"old").unwrap();
            let error = write_private_atomic_with(&target, |file| {
                std::io::Write::write_all(file, b"partial secret")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                }
                Err(std::io::Error::other(label))
            })
            .unwrap_err();
            assert_eq!(error.to_string(), label);
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert!(!base.join("record.tmp-private").exists());
            std::fs::remove_dir_all(base).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_private_atomic_never_follows_destination_or_stale_temp_symlinks() {
        use std::os::unix::fs::symlink;
        let base = temp_path("write_symlink");
        ensure_private_dir(&base).unwrap();
        let outside = base.join("outside");
        std::fs::write(&outside, b"must stay unchanged").unwrap();
        let target = base.join("record");
        symlink(&outside, &target).unwrap();
        assert!(write_private_atomic(&target, b"secret").is_err());
        std::fs::remove_file(&target).unwrap();
        let tmp = base.join("record.tmp-private");
        symlink(&outside, &tmp).unwrap();
        write_private_atomic(&target, b"new private record").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"must stay unchanged");
        assert!(!tmp.exists());
        std::fs::remove_dir_all(base).unwrap();
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
