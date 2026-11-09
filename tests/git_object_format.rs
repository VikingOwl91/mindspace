//! M7.3b.5c step 0: a repository whose object format is not SHA-1 is refused before anything is
//! read or written.
//!
//! The tree parser (`parse_tree`) reads fixed 20-byte entries. On a SHA-256 repository it did not
//! fail: it returned 40-digit entry ids that name no object and left hash bytes in the next entry's
//! mode, and the ingestion stored them. These tests use REAL SHA-256 repositories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::ingest;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing};

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git available");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A repository with a nested directory (so there are trees below the root) and Rust code.
fn repo(format: &str) -> Option<(tempfile::TempDir, PathBuf)> {
    let d = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(d.path()).unwrap().join("repo");
    std::fs::create_dir_all(path.join("src/inner")).unwrap();
    git(
        &path,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    )?;
    std::fs::write(path.join("README.md"), "# Readme\n\nalpha\n").unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn alpha() {}\n").unwrap();
    std::fs::write(path.join("src/inner/deep.rs"), "pub fn deep() {}\n").unwrap();
    git(&path, &["add", "-A"])?;
    git(&path, &["commit", "-q", "-m", "one"])?;
    Some((d, path))
}

fn db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    open_existing(&path).unwrap().create_space("p", "").unwrap();
    (dir, path)
}

/// Every row of every user table, as sorted text.
fn dump(path: &Path) -> BTreeMap<String, Vec<String>> {
    let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    let mut out = BTreeMap::new();
    for t in tables {
        let cols: Vec<String> = c
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        if cols.is_empty() {
            continue;
        }
        let expr = cols
            .iter()
            .map(|n| format!("quote(\"{n}\")"))
            .collect::<Vec<_>>()
            .join("||'|'||");
        let mut rows: Vec<String> = c
            .prepare(&format!("SELECT {expr} FROM \"{t}\""))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        rows.sort();
        out.insert(t, rows);
    }
    out
}

#[test]
fn a_sha256_repository_is_refused_with_a_clear_error_and_nothing_is_stored() {
    let Some((_d, repo)) = repo("sha256") else {
        eprintln!("SKIP: this git cannot create a SHA-256 repository");
        return;
    };
    let (_dir, dbp) = db();
    let before = dump(&dbp);
    let store = open_existing(&dbp).unwrap();
    for scope in ["head", "history"] {
        let r = match scope {
            "head" => ingest::ingest_path(&store, &repo, &["p"]).map(|_| ()),
            _ => ingest::ingest_git_repository_selected(
                &store,
                &repo,
                &["p"],
                &ingest::outline::PathSelection::all(),
                ingest::GitScope::History,
            )
            .map(|_| ()),
        };
        let e = r.unwrap_err().to_string();
        assert!(
            e.contains("object format") && e.contains("sha256") && e.contains("not supported"),
            "{scope}: {e}"
        );
    }
    drop(store);
    assert_eq!(before, dump(&dbp), "a refused repository left rows behind");
    assert!(
        !repo.join(".git/mindspace-repository-id").exists(),
        "a refused repository got an identity marker written into it"
    );
}

#[test]
fn a_sha1_repository_still_ingests_and_its_tree_entries_are_real_objects() {
    let (_d, repo) = repo("sha1").unwrap();
    let (_dir, dbp) = db();
    let store = open_existing(&dbp).unwrap();
    ingest::ingest_path(&store, &repo, &["p"]).unwrap();
    drop(store);
    let c = rusqlite::Connection::open(&dbp).unwrap();
    let ids: Vec<String> = c
        .prepare("SELECT DISTINCT target_oid FROM git_tree_entries")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    assert!(ids.len() >= 4, "{ids:?}");
    for id in ids {
        assert_eq!(id.len(), 40, "{id}");
        // every stored entry names an object the repository really has
        assert!(
            git(&repo, &["cat-file", "-e", &id]).is_some(),
            "{id} does not exist"
        );
    }
}
