//! Private creation for the bundled Linux/Unix SQLite VFS.
//!
//! SQLite copies MAIN's mode when creating WAL, SHM and rollback journals. Publish
//! an empty 0600 inode before SQLite sees it, then omit CREATE from SQLite's open.
//! A closed sibling is published with link(2): opening/closing a separate MAIN fd
//! could otherwise release another connection's process-associated POSIX locks.
//!
//! Canonical, owner/root-controlled parents exclude replacement by other UIDs.
//! Root and same-UID processes are trusted; this is not hostile same-UID isolation
//! or a guarantee for filesystems that do not enforce local Unix permissions.

use std::path::Path;

use rusqlite::Connection;

use crate::core::{Error, Result};

pub(super) fn open(path: &Path, create: bool) -> Result<Connection> {
    #[cfg(target_os = "linux")]
    {
        linux::open(path, create)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, create);
        Err(Error::Storage(
            "private SQLite file opening requires the qualified Linux Unix VFS".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::ffi::OsStr;
    use std::fs::{self, DirBuilder, Metadata, OpenOptions};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rusqlite::OpenFlags;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn refused(reason: impl std::fmt::Display) -> Error {
        Error::Storage(format!("private database opening refused: {reason}"))
    }

    fn io_error(path: &Path, source: io::Error) -> Error {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }

    fn metadata(path: &Path) -> Result<Option<Metadata>> {
        match fs::symlink_metadata(path) {
            Ok(m) => Ok(Some(m)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(path, e)),
        }
    }

    fn controlled_parent(parent: &Path, immediate: bool) -> Result<()> {
        // SAFETY: geteuid has no arguments or side effects.
        let uid = unsafe { libc::geteuid() };
        for (index, ancestor) in parent.ancestors().enumerate() {
            let m = fs::symlink_metadata(ancestor).map_err(|e| io_error(ancestor, e))?;
            if !m.is_dir() || (m.uid() != 0 && m.uid() != uid) {
                return Err(refused(format!(
                    "database parent {} is not a directory controlled by root or the operating UID",
                    ancestor.display()
                )));
            }
            let sticky_root = m.uid() == 0 && m.mode() & libc::S_ISVTX != 0;
            if m.mode() & 0o022 != 0 && (!sticky_root || (immediate && index == 0)) {
                return Err(refused(format!(
                    "database parent {} is writable by another user; use a protected database directory",
                    ancestor.display()
                )));
            }
        }
        Ok(())
    }

    fn private_parent(path: &Path) -> Result<PathBuf> {
        let absolute = std::path::absolute(path).map_err(|e| io_error(path, e))?;
        let mut anchor = absolute.as_path();
        let mut missing = Vec::new();
        while metadata(anchor)?.is_none() {
            let name = anchor
                .file_name()
                .ok_or_else(|| refused("missing parent components must be named directories"))?;
            missing.push(name.to_owned());
            anchor = anchor
                .parent()
                .ok_or_else(|| refused("database has no existing parent"))?;
        }
        let mut parent = fs::canonicalize(anchor).map_err(|e| io_error(anchor, e))?;
        controlled_parent(&parent, missing.is_empty())?;
        for name in missing.iter().rev() {
            parent.push(name);
            match DirBuilder::new().mode(0o700).create(&parent) {
                Ok(()) => {
                    let m = fs::symlink_metadata(&parent).map_err(|e| io_error(&parent, e))?;
                    if !m.is_dir() || m.mode() & 0o777 != 0o700 {
                        return Err(refused(
                            "new database directory was not created with mode 0700",
                        ));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_error(&parent, e)),
            }
            controlled_parent(&parent, true)?;
        }
        Ok(parent)
    }

    fn sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    }

    fn private_sidecars(path: &Path) -> Result<()> {
        // SAFETY: geteuid has no arguments or side effects.
        let uid = unsafe { libc::geteuid() };
        for suffix in ["-wal", "-shm", "-journal"] {
            let name = sidecar(path, suffix);
            if let Some(m) = metadata(&name)? {
                if !m.is_file() || m.uid() != uid || m.nlink() != 1 || m.mode() & 0o077 != 0 {
                    return Err(refused(format!(
                        "unsafe existing SQLite auxiliary file {}; recover it separately without changing user permissions implicitly",
                        name.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Remove only our unpublished sibling; MAIN and parent dirs survive failures.
    struct Pending(PathBuf);

    impl Drop for Pending {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn publish(path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| refused("database has no parent"))?;
        // Unrelated existing temporary names are never removed or overwritten.
        for _ in 0..32 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temp = parent.join(format!(
                ".mindspace-create-{}-{sequence}",
                std::process::id()
            ));
            if temp == path {
                continue;
            }
            let file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temp)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error(&temp, e)),
            };
            let pending = Pending(temp);
            let m = file.metadata().map_err(|e| io_error(&pending.0, e))?;
            if !m.is_file() || m.mode() & 0o777 != 0o600 {
                return Err(refused("new database inode was not created with mode 0600"));
            }
            // MUST precede publication: closing this fd later could drop another
            // same-process SQLite connection's POSIX locks on the published inode.
            drop(file);
            match fs::hard_link(&pending.0, path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_error(path, e)),
            }
            fs::remove_file(&pending.0).map_err(|e| io_error(&pending.0, e))?;
            return Ok(());
        }
        Err(refused("cannot reserve a unique private database inode"))
    }

    fn existing(path: &Path) -> Result<Connection> {
        let real = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
        let m = fs::symlink_metadata(&real).map_err(|e| io_error(&real, e))?;
        if !m.is_file() {
            return Err(refused("database must be a regular file"));
        }
        if m.mode() & 0o777 == 0o600 {
            // SAFETY: geteuid has no arguments or side effects.
            if m.uid() != unsafe { libc::geteuid() } {
                return Err(refused(
                    "private database is not owned by the operating UID",
                ));
            }
            controlled_parent(
                real.parent()
                    .ok_or_else(|| refused("database has no parent"))?,
                true,
            )?;
            private_sidecars(&real)?;
        }
        sqlite_open(&real)
    }

    fn sqlite_open(real: &Path) -> Result<Connection> {
        // No CREATE and no URI: a removal cannot recreate MAIN with SQLite's
        // default 0644, and modeof/vfs/URI aliases cannot bypass preparation.
        Connection::open_with_flags(
            real,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| Error::Storage(format!("cannot open database {}: {e}", real.display())))
    }

    pub(super) fn open(path: &Path, create: bool) -> Result<Connection> {
        if path.as_os_str().as_bytes().starts_with(b"file:") {
            return Err(refused(
                "SQLite URI filenames are not supported; use a filesystem path",
            ));
        }
        if create && (path.as_os_str().is_empty() || path == Path::new(":memory:")) {
            // SQLite's anonymous temporary files use DELETEONCLOSE/0600. Keep
            // the established library behavior without creating a literal path.
            return Connection::open(path).map_err(|e| Error::Storage(e.to_string()));
        }
        match metadata(path)? {
            Some(m) => {
                if m.file_type().is_symlink() && !path.exists() {
                    return Err(refused("dangling database symlink"));
                }
                return existing(path);
            }
            None if !create => {
                return Err(Error::NotInitialized {
                    path: path.display().to_string(),
                });
            }
            None => {}
        }
        let name = path
            .file_name()
            .filter(|n| *n != OsStr::new(".."))
            .ok_or_else(|| refused("database must have a filename"))?;
        let parent = private_parent(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let real = parent.join(name);
        if metadata(&real)?.is_some() {
            return existing(&real);
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            if metadata(&sidecar(&real, suffix))?.is_some() {
                // Another initializer may have published MAIN since our check.
                if metadata(&real)?.is_some() {
                    return existing(&real);
                }
                return Err(refused(
                    "orphan SQLite auxiliary file exists; recover it separately",
                ));
            }
        }
        publish(&real)?;
        existing(&real)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn published_inode_and_directories_are_private_before_sqlite_open() {
            let root = tempfile::tempdir().unwrap();
            let parent = private_parent(&root.path().join("new/private")).unwrap();
            let db = parent.join("mind.db");
            publish(&db).unwrap();
            let m = fs::symlink_metadata(&db).unwrap();
            assert_eq!(m.mode() & 0o777, 0o600);
            assert_eq!(m.len(), 0, "SQLite has not opened or initialized MAIN");
            assert_eq!(m.nlink(), 1, "owned temporary name was removed");
            assert_eq!(fs::symlink_metadata(&parent).unwrap().mode() & 0o777, 0o700);
            assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
            for suffix in ["-wal", "-shm", "-journal"] {
                assert!(!sidecar(&db, suffix).exists());
            }
        }

        #[test]
        fn sqlite_cannot_recreate_a_main_removed_after_preparation() {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join("mind.db");
            publish(&db).unwrap();
            fs::remove_file(&db).unwrap();
            assert!(sqlite_open(&db).is_err());
            assert!(!db.exists(), "SQLite open must not contain CREATE");
        }
    }
}
