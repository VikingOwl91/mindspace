//! Validation of the folders a user wants to share through `mind setup`.
//!
//! The path syntax and the basic rules (existing directory, not `/`, canonical
//! form, most restrictive mode for duplicates) are those of `mind mcp
//! --allow-root` (`McpConfig::from_specs`); this module only adds the checks
//! that make sense when a person types the folders: absolute paths, no home
//! directory, no overlap with the database directory, characters that would
//! break the `DIR:mode` syntax, and a clear message for each refusal. Nothing
//! is ever created.

use std::path::{Path, PathBuf};

use crate::core::{Error, Result};
use crate::mcp::{Access, McpConfig};

/// One validated folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// As typed.
    pub requested: PathBuf,
    /// The resolved absolute path; this is what gets registered.
    pub canonical: PathBuf,
    pub access: Access,
    /// Things the person should know (symlink, writable by others, duplicate).
    pub notes: Vec<String>,
}

impl Root {
    /// The `--allow-root` value for this folder.
    pub fn spec(&self) -> String {
        let mode = match self.access {
            Access::ReadOnly => "ro",
            Access::ReadWrite => "rw",
        };
        format!("{}:{mode}", self.canonical.display())
    }
}

/// Folders that hold the system or everybody's data. Sharing one of these itself is
/// almost certainly a slip; a folder INSIDE one (`/usr/src/app`, `/var/www/site`) is fine.
/// Deliberately a list and not "everything directly under `/`": `/workspace`, `/srv`,
/// `/opt`, `/mnt` and `/data` are common and legitimate places for projects.
const SYSTEM_DIRS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/libx32", "/proc",
    "/root", "/run", "/sbin", "/sys", "/tmp", "/usr", "/var",
];

/// Why `canonical` is too broad to share, if it is.
pub fn broad_reason(canonical: &Path) -> Option<&'static str> {
    if canonical.parent().is_none() {
        return Some("is the root of the file system; name a specific project folder");
    }
    if SYSTEM_DIRS.iter().any(|d| canonical == Path::new(d)) {
        return Some("is a system folder; name a specific project folder inside it");
    }
    None
}

fn refuse(flag: &str, path: &Path, why: &str) -> Error {
    Error::Setup(format!("{flag} {}: {why}", path.display()))
}

/// Validates `ro` and `rw` folders. `db_dir` is the (canonical) directory of the
/// database, `home` the user's home directory if known.
pub fn validate(
    ro: &[PathBuf],
    rw: &[PathBuf],
    db_dir: &Path,
    home: Option<&Path>,
) -> Result<Vec<Root>> {
    let home = home.and_then(|h| std::fs::canonicalize(h).ok());
    let mut out: Vec<Root> = Vec::new();
    let all = ro
        .iter()
        .map(|p| ("--ro", p, Access::ReadOnly))
        .chain(rw.iter().map(|p| ("--rw", p, Access::ReadWrite)));
    for (flag, requested, access) in all {
        let Some(text) = requested.to_str() else {
            return Err(refuse(
                flag,
                requested,
                "is not valid UTF-8 and could not be registered faithfully",
            ));
        };
        if !requested.is_absolute() {
            return Err(refuse(
                flag,
                requested,
                "must be an absolute path. A leading ~ is expanded by your shell; if it was quoted, write the full path",
            ));
        }
        if text.chars().any(char::is_control) {
            return Err(refuse(
                flag,
                requested,
                "contains a control character (such as a newline)",
            ));
        }
        if text.contains(':') {
            return Err(refuse(
                flag,
                requested,
                "contains ':', which Mindspace uses to separate the folder from its mode (DIR:ro); rename the folder or point to it through a link without ':'",
            ));
        }
        let canonical = std::fs::canonicalize(requested).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => refuse(
                flag,
                requested,
                "does not exist (setup never creates folders)",
            ),
            std::io::ErrorKind::PermissionDenied => {
                refuse(flag, requested, "cannot be read (permission denied)")
            }
            _ => refuse(flag, requested, &format!("cannot be resolved: {e}")),
        })?;
        if !canonical.is_dir() {
            return Err(refuse(flag, requested, "is not a directory"));
        }
        let Some(canon_text) = canonical.to_str() else {
            return Err(refuse(
                flag,
                requested,
                "resolves to a path that is not valid UTF-8 and could not be registered faithfully",
            ));
        };
        if canon_text.chars().any(char::is_control) {
            return Err(refuse(
                flag,
                requested,
                "resolves to a path containing a control character",
            ));
        }
        if canon_text.contains(':') {
            return Err(refuse(
                flag,
                requested,
                &format!("resolves to {canon_text}, which contains ':'"),
            ));
        }
        if let Some(why) = broad_reason(&canonical) {
            return Err(refuse(
                flag,
                requested,
                &format!("resolves to {canon_text}, which {why}"),
            ));
        }
        if let Some(h) = &home {
            if canonical == *h || h.starts_with(&canonical) {
                return Err(refuse(
                    flag,
                    requested,
                    &format!("is (or contains) your home directory {}; that would share everything in it. Name a specific project folder", h.display()),
                ));
            }
        }
        if canonical.starts_with(db_dir) || db_dir.starts_with(&canonical) {
            return Err(refuse(
                flag,
                requested,
                &format!("overlaps the database directory {}; the sandbox opens the whole database directory, so shared folders must be separate from it", db_dir.display()),
            ));
        }
        // The same rules mind mcp applies when it starts (and its message if one is broken).
        let spec = format!(
            "{}:{}",
            canonical.display(),
            if access == Access::ReadOnly {
                "ro"
            } else {
                "rw"
            }
        );
        McpConfig::from_specs(&[spec]).map_err(|e| refuse(flag, requested, &e.to_string()))?;

        let mut notes = Vec::new();
        if canonical != *requested {
            notes.push(format!(
                "{} is a link or contains one; it resolves to {}",
                requested.display(),
                canonical.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(&canonical) {
                let mode = m.permissions().mode();
                if mode & 0o002 != 0 && mode & 0o1000 == 0 {
                    notes.push("other users can write in this folder; do not share folders that untrusted local processes can modify".to_string());
                }
            }
        }
        if let Some(prev) = out.iter_mut().find(|r| r.canonical == canonical) {
            if prev.access != access {
                return Err(Error::Setup(format!(
                    "{} is given as both --ro and --rw; choose one mode",
                    canonical.display()
                )));
            }
            prev.notes.push(format!(
                "{} names the same folder and was merged",
                requested.display()
            ));
            continue;
        }
        out.push(Root {
            requested: requested.clone(),
            canonical,
            access,
            notes,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("db");
        let repos = t.path().join("repos");
        std::fs::create_dir(&db).unwrap();
        std::fs::create_dir(&repos).unwrap();
        let (db, repos) = (
            std::fs::canonicalize(db).unwrap(),
            std::fs::canonicalize(repos).unwrap(),
        );
        (t, db, repos)
    }

    #[test]
    fn a_plain_folder_is_accepted_with_its_mode_in_the_spec() {
        let (_t, db, repos) = dirs();
        let r = validate(std::slice::from_ref(&repos), &[], &db, None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].access, Access::ReadOnly);
        assert_eq!(r[0].spec(), format!("{}:ro", repos.display()));
    }

    #[test]
    fn refusals_name_the_flag_and_the_reason() {
        let (t, db, repos) = dirs();
        let cases: Vec<(PathBuf, &str)> = vec![
            (PathBuf::from("relative/dir"), "absolute path"),
            (PathBuf::from("/"), "root of the file system"),
            (PathBuf::from("/etc"), "system folder"),
            (t.path().join("missing"), "does not exist"),
            (PathBuf::from("/tmp/a:b"), "contains ':'"),
            (PathBuf::from("/tmp/a\nb"), "control character"),
            (db.clone(), "database directory"),
            (db.join("inner"), "does not exist"),
        ];
        for (p, why) in cases {
            let e = validate(std::slice::from_ref(&p), &[], &db, None)
                .unwrap_err()
                .to_string();
            assert!(e.starts_with("--ro "), "{e}");
            assert!(e.contains(why), "{p:?}: {e}");
        }
        // A file is not a directory.
        let f = repos.join("f");
        std::fs::write(&f, "x").unwrap();
        assert!(validate(&[], &[f], &db, None)
            .unwrap_err()
            .to_string()
            .contains("not a directory"));
    }

    #[test]
    fn only_system_folders_are_too_broad_not_every_top_level_folder() {
        for broad in [
            "/", "/etc", "/usr", "/home", "/var", "/tmp", "/root", "/sys", "/proc",
        ] {
            assert!(broad_reason(Path::new(broad)).is_some(), "{broad}");
        }
        // Common, legitimate places for projects, and folders inside system folders.
        for ok in [
            "/workspace",
            "/srv",
            "/opt",
            "/mnt",
            "/data",
            "/app",
            "/code",
            "/media",
            "/usr/src/app",
            "/var/www/site",
            "/home/alice/src",
            "/mnt/data/projects",
        ] {
            assert!(
                broad_reason(Path::new(ok)).is_none(),
                "{ok} must be allowed"
            );
        }
    }

    #[test]
    fn the_home_directory_and_its_ancestors_are_refused() {
        let (t, db, repos) = dirs();
        let home = repos.join("me");
        std::fs::create_dir(&home).unwrap();
        let home = std::fs::canonicalize(home).unwrap();
        for p in [home.clone(), repos.clone(), t.path().to_path_buf()] {
            let e = validate(std::slice::from_ref(&p), &[], &db, Some(&home));
            assert!(
                e.is_err() && e.unwrap_err().to_string().contains("home directory"),
                "{p:?}"
            );
        }
        std::fs::create_dir(home.join("project")).unwrap();
        assert!(validate(&[home.join("project")], &[], &db, Some(&home)).is_ok());
    }

    #[test]
    fn a_database_directory_inside_a_root_is_an_overlap_too() {
        let (_t, db, repos) = dirs();
        let inner_db = repos.join("data");
        std::fs::create_dir(&inner_db).unwrap();
        let e = validate(
            &[repos],
            &[],
            &std::fs::canonicalize(inner_db).unwrap(),
            None,
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("overlaps the database directory"),
            "{e}"
        );
        let _ = db;
    }

    #[test]
    fn symlinks_resolve_and_are_noted_and_duplicates_merge_or_conflict() {
        let (t, db, repos) = dirs();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&repos, &link).unwrap();
        let r = validate(&[link.clone(), repos.clone()], &[], &db, None).unwrap();
        assert_eq!(r.len(), 1, "same folder through a link and directly");
        assert_eq!(r[0].canonical, repos);
        assert!(r[0].notes.iter().any(|n| n.contains("resolves to")));
        assert!(r[0].notes.iter().any(|n| n.contains("merged")));
        let e = validate(&[link], &[repos], &db, None).unwrap_err();
        assert!(e.to_string().contains("both --ro and --rw"), "{e}");
    }

    #[test]
    fn a_link_into_the_database_directory_is_caught_by_its_target() {
        let (t, db, _repos) = dirs();
        let link = t.path().join("sneaky");
        std::os::unix::fs::symlink(&db, &link).unwrap();
        let e = validate(&[link], &[], &db, None).unwrap_err();
        assert!(e.to_string().contains("database directory"), "{e}");
    }
}
