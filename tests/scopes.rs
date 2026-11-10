//! Scopes: code is attached to spaces through its repository or ingest root,
//! remotes are stored credential-free, and the 0009 migration sits on top of
//! the M6 baseline without re-running it.

#[path = "support/schema_fixture.rs"]
mod schema_fixture;

use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

struct Mind {
    dir: tempfile::TempDir,
}

impl Mind {
    fn new(spaces: &[&str]) -> Self {
        let m = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        assert!(m.run(&["init"]).2);
        for s in spaces {
            assert!(m.run(&["space", "create", s]).2);
        }
        m
    }

    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("mind.db")
    }

    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let out = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", self.db())
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }

    fn ingest(&self, path: &Path, spaces: &[&str]) {
        let p = path.display().to_string();
        let mut args = vec!["ingest", p.as_str()];
        for s in spaces {
            args.extend(["--space", s]);
        }
        let (_, err, ok) = self.run(&args);
        assert!(ok, "ingest failed: {err}");
    }

    fn symbols(&self, args: &[&str]) -> Vec<serde_json::Value> {
        let mut a = vec!["symbols"];
        a.extend_from_slice(args);
        a.push("--json");
        let (out, err, ok) = self.run(&a);
        assert!(ok, "symbols failed: {err}");
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }
}

fn names(hits: &[serde_json::Value]) -> Vec<String> {
    let mut n: Vec<String> = hits
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo(function: &str, origin: Option<&str>) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(
        d.path().join("lib.rs"),
        format!("pub fn {function}() {{}}\n"),
    )
    .unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    if let Some(url) = origin {
        git(d.path(), &["remote", "add", "origin", url]);
    }
    d
}

#[test]
fn space_filter_scopes_code_follows_links_and_reports_where_it_lives() {
    let m = Mind::new(&["alpha", "beta", "umbrella"]);
    let a = repo("alpha_only_fn", Some("git@github.com:Acme/Alpha.git"));
    let b = repo("beta_only_fn", Some("https://github.com/Acme/Beta.git"));
    m.ingest(a.path(), &["alpha"]);
    m.ingest(b.path(), &["beta"]);

    // Unfiltered sees both; a space sees only its own.
    assert_eq!(
        names(&m.symbols(&["_only_fn"])),
        vec!["alpha_only_fn", "beta_only_fn"]
    );
    assert_eq!(
        names(&m.symbols(&["_only_fn", "--space", "alpha"])),
        vec!["alpha_only_fn"]
    );
    assert_eq!(
        names(&m.symbols(&["_only_fn", "--space", "beta"])),
        vec!["beta_only_fn"]
    );
    assert_eq!(
        names(&m.symbols(&["_only_fn", "--space", "alpha", "--space", "beta"])),
        vec!["alpha_only_fn", "beta_only_fn"]
    );

    // Directed links: umbrella imports alpha, so it sees alpha's code only.
    assert!(m.symbols(&["_only_fn", "--space", "umbrella"]).is_empty());
    assert!(m.run(&["space", "link", "umbrella", "alpha"]).2);
    assert_eq!(
        names(&m.symbols(&["_only_fn", "--space", "umbrella"])),
        vec!["alpha_only_fn"]
    );
    assert!(m
        .symbols(&["_only_fn", "--space", "alpha"])
        .iter()
        .all(|h| h["spaces"] == serde_json::json!(["alpha"])));

    // Hits say where they live.
    let hit = &m.symbols(&["alpha_only_fn"])[0];
    assert_eq!(hit["repository_slug"], "Acme/Alpha");
    assert_eq!(hit["spaces"], serde_json::json!(["alpha"]));

    // Unknown spaces are an error, not an empty result.
    let (_, err, ok) = m.run(&["symbols", "x", "--space", "nope"]);
    assert!(!ok && err.contains("does not exist"), "{err}");
}

#[test]
fn repo_filter_accepts_owner_name_slugs_case_insensitively() {
    let m = Mind::new(&["alpha"]);
    let a = repo("sluggy_fn", Some("git@github.com:Acme/Alpha.git"));
    m.ingest(a.path(), &["alpha"]);
    for q in ["acme/alpha", "Acme/Alpha", "github.com/acme/alpha"] {
        assert_eq!(m.symbols(&["sluggy_fn", "--repo", q]).len(), 1, "{q}");
    }
    assert!(m.symbols(&["sluggy_fn", "--repo", "acme/other"]).is_empty());
}

#[test]
fn attaching_a_second_space_adds_and_never_replaces() {
    let m = Mind::new(&["alpha", "beta"]);
    let a = repo("shared_fn", None);
    m.ingest(a.path(), &["alpha"]);
    m.ingest(a.path(), &["beta"]);
    assert_eq!(m.symbols(&["shared_fn", "--space", "alpha"]).len(), 1);
    assert_eq!(m.symbols(&["shared_fn", "--space", "beta"]).len(), 1);
    assert_eq!(
        m.symbols(&["shared_fn"])[0]["spaces"],
        serde_json::json!(["alpha", "beta"])
    );
}

#[test]
fn directory_and_file_ingests_are_scoped_by_their_root() {
    let m = Mind::new(&["alpha", "beta"]);
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    std::fs::write(da.path().join("a.rs"), "pub fn dir_alpha_fn() {}\n").unwrap();
    std::fs::write(db.path().join("b.rs"), "pub fn dir_beta_fn() {}\n").unwrap();
    m.ingest(da.path(), &["alpha"]);
    m.ingest(db.path(), &["beta"]);
    assert_eq!(
        names(&m.symbols(&["dir_", "--space", "alpha"])),
        vec!["dir_alpha_fn"]
    );
    assert_eq!(
        names(&m.symbols(&["dir_", "--space", "beta"])),
        vec!["dir_beta_fn"]
    );

    // A single file is its own root.
    let single = tempfile::tempdir().unwrap();
    std::fs::write(single.path().join("one.rs"), "pub fn single_file_fn() {}\n").unwrap();
    m.ingest(&single.path().join("one.rs"), &["beta"]);
    assert_eq!(
        names(&m.symbols(&["single_file_fn", "--space", "beta"])),
        vec!["single_file_fn"]
    );
    assert!(m
        .symbols(&["single_file_fn", "--space", "alpha"])
        .is_empty());
}

#[test]
fn remotes_are_stored_without_credentials_and_follow_the_latest_observation() {
    let m = Mind::new(&["alpha"]);
    let a = repo(
        "remote_fn",
        Some("https://ci:SECRETTOKEN123@github.com/Acme/Old.git"),
    );
    m.ingest(a.path(), &["alpha"]);

    let conn = Connection::open(m.db()).unwrap();
    let all: Vec<String> = conn
        .prepare("SELECT name || ' ' || url || ' ' || coalesce(slug_key,'') || ' ' || coalesce(slug,'') FROM repository_remotes")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(all.len(), 1);
    assert!(
        !all[0].contains("SECRETTOKEN123") && !all[0].contains("ci:"),
        "{}",
        all[0]
    );
    assert!(all[0].contains("github.com/acme/old"));

    // The remote moves: the new slug is current, the old row keeps its history.
    git(
        a.path(),
        &["remote", "set-url", "origin", "git@github.com:Acme/New.git"],
    );
    m.ingest(a.path(), &["alpha"]);
    assert_eq!(m.symbols(&["remote_fn"])[0]["repository_slug"], "Acme/New");
    let (current, total): (i64, i64) = conn
        .query_row(
            "SELECT SUM(is_current), COUNT(*) FROM repository_remotes",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((current, total), (1, 2));
}

#[test]
fn current_views_live_in_a_table_and_are_replaced_on_each_ingest() {
    let m = Mind::new(&["alpha"]);
    let a = repo("view_fn", None);
    m.ingest(a.path(), &["alpha"]);
    std::fs::write(a.path().join("lib.rs"), "pub fn view_fn_v2() {}\n").unwrap();
    git(a.path(), &["add", "-A"]);
    git(a.path(), &["commit", "-q", "-m", "two"]);
    m.ingest(a.path(), &["alpha"]);
    let conn = Connection::open(m.db()).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM git_views", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "only the latest ingest's views are kept");
    let mind_meta_views: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mind_meta WHERE key LIKE 'git_views:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mind_meta_views, 0, "mind_meta is no longer used for views");
    assert!(
        m.symbols(&["view_fn_v2"]).len() == 1
            && m.symbols(&["view_fn", "--kind", "fn"])
                .iter()
                .all(|h| h["name"] == "view_fn_v2")
    );
}

#[test]
fn migration_nine_upgrades_a_v8_database_without_repeating_the_m6_baseline() {
    let m = Mind::new(&["alpha"]);
    let before: (i64, i64) = {
        let conn = Connection::open(m.db()).unwrap();
        schema_fixture::remove_empty_ownership_metadata(&conn);
        // Simulate a database created by the M6 commit: no 0009 objects yet.
        for t in ["scope_spaces", "scopes", "repository_remotes", "git_views"] {
            conn.execute_batch(&format!("DROP TABLE {t};")).unwrap();
        }
        // the document tables of migration 10 go with it: the database predates both
        for t in [
            "doc_chunks",
            "doc_chunk_sets",
            "doc_nodes",
            "doc_extractions",
            "doc_blobs",
        ] {
            conn.execute_batch(&format!("DROP TABLE {t};")).unwrap();
        }
        conn.execute_batch("DROP INDEX uq_logical_documents_location;")
            .unwrap();
        conn.execute(
            "DELETE FROM schema_migrations WHERE version IN (9, 10, 11, 12)",
            [],
        )
        .unwrap();
        (
            conn.query_row("SELECT COUNT(*) FROM m6_revisions", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM m6_history_coverage", [], |r| r.get(0))
                .unwrap(),
        )
    };

    // Opening applies 9 through 17, including five metadata R; M6 baseline is not repeated.
    let (_, err, ok) = m.run(&["space", "list"]);
    assert!(ok, "{err}");
    let conn = Connection::open(m.db()).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, 17);
    for t in ["scopes", "scope_spaces", "repository_remotes", "git_views"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{t} missing");
    }
    // Original baseline and public query coverage remain unchanged. Schemas 13/14/15/16/17 record
    // one metadata availability revision each, without a knowledge mutation.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM m6_revisions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, before.0 + 5);
    let after: (i64, i64) = (
        conn.query_row(
            "SELECT COUNT(*) FROM m6_revisions WHERE kind = 'baseline'",
            [],
            |r| r.get(0),
        )
        .unwrap(),
        conn.query_row("SELECT COUNT(*) FROM m6_history_coverage", [], |r| r.get(0))
            .unwrap(),
    );
    assert_eq!(after.0, 1, "exactly one baseline revision");
    assert_eq!(after.1, before.1);
    assert!(before.0 >= 1);
}
