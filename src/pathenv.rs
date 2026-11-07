//! Finding programs on `PATH` (shared by `mind doctor` and `mind setup`).
//!
//! Empty and relative `PATH` entries are skipped: they mean "the current
//! directory" to a shell, and a program found there would not be the one
//! started later by name.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The executables called `name` in the absolute directories of `path_env`, in
/// `PATH` order. Two entries that resolve to the same file count once.
pub fn executables(name: &str, path_env: Option<&OsStr>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let Some(path_env) = path_env else {
        return found;
    };
    let mut resolved: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(path_env).filter(|d| d.is_absolute()) {
        let cand = dir.join(name);
        let is_exec = std::fs::metadata(&cand)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if !is_exec {
            continue;
        }
        let real = std::fs::canonicalize(&cand).unwrap_or_else(|_| cand.clone());
        if !resolved.contains(&real) {
            resolved.push(real);
            found.push(cand);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn finds_in_path_order_and_counts_a_symlink_once() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, c) = (t.path().join("a"), t.path().join("b"), t.path().join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir(d).unwrap();
        }
        let first = exe(&a, "tool");
        std::os::unix::fs::symlink(&first, b.join("tool")).unwrap();
        let other = exe(&c, "tool");
        let path = std::env::join_paths([&a, &b, &c]).unwrap();
        assert_eq!(executables("tool", Some(&path)), vec![first, other]);
    }

    #[test]
    fn empty_and_relative_entries_are_ignored() {
        let t = tempfile::tempdir().unwrap();
        exe(t.path(), "tool");
        let rel = format!(":relative:{}", "x");
        assert!(executables("tool", Some(OsStr::new(&rel))).is_empty());
        assert!(executables("tool", Some(OsStr::new(":."))).is_empty());
    }

    #[test]
    fn a_missing_or_non_executable_file_is_not_found() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("tool");
        std::fs::write(&p, "x").unwrap();
        let path = std::env::join_paths([t.path()]).unwrap();
        assert!(executables("tool", Some(&path)).is_empty());
        assert!(executables("tool", None).is_empty());
    }
}
