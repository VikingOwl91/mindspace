//! M7.3b.5a: one logical Git ingestion commits entirely or not at all.
//!
//! `persist_git` used to be three kinds of write, each in its own transaction and recording
//! revision: the git observation, then the code observation, then one relationship resolution
//! per view commit (which reads the rows the code observation just committed). A failure or a
//! crash between any two left an observed repository without its code structure, or code
//! structure without its resolutions. These tests inject REAL SQLite failures (`RAISE(ABORT)`
//! triggers installed through a second connection, a commit-hook veto) at every boundary and
//! compare the CONTENTS of every table and the recording revisions, not only the returned error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist_git, persist_git_gated, ExtractedGit, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::{extract_git_repository_selected_with_deadline, GitScope};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

/// The commit-fault hooks are process-global one-shot switches; every test of this file holds this
/// lock so that an armed fault can only be consumed by the test that armed it.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with Rust modules that reference each other (so relationships resolve), a document,
/// an annotated tag and a linked worktree on the older commit: TWO view commits, hence two
/// relationship resolutions per ingestion.
struct Repo {
    _d: tempfile::TempDir,
    path: PathBuf,
}

fn repo() -> Repo {
    let d = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(d.path()).unwrap().join("repo");
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::create_dir_all(path.join("docs")).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    let w = |rel: &str, text: &str| std::fs::write(path.join(rel), text).unwrap();
    w("docs/guide.md", "# Guide\n\nalpha text\n");
    w(
        "src/lib.rs",
        "pub mod util;\nuse crate::util::helper;\npub fn run() { helper(); }\n",
    );
    w("src/util.rs", "pub fn helper() {}\n");
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "one"]);
    w("src/util.rs", "pub fn helper() {}\npub fn extra() {}\n");
    git(&path, &["commit", "-q", "-am", "two"]);
    git(&path, &["tag", "-a", "v1", "-m", "release one"]);
    let wt = path.parent().unwrap().join("wt");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            "HEAD~1",
        ],
    );
    Repo { _d: d, path }
}

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    open_existing(&path).unwrap().create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.raw()
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn integrity(&self) -> String {
        self.raw()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap()
    }
    /// Every row of every user table, as sorted text: what the database CONTAINS.
    fn dump(&self) -> BTreeMap<String, Vec<String>> {
        let c = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
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
    /// Row counts of every user table (ids and times differ between runs; counts do not).
    fn counts(&self) -> BTreeMap<String, usize> {
        self.dump().into_iter().map(|(t, r)| (t, r.len())).collect()
    }
    /// Content that must be the same for equal ingestions, with ids and times left out.
    fn derived(&self) -> Vec<(String, Vec<String>)> {
        let c = self.raw();
        let q = |sql: &str| -> Vec<String> {
            let mut v: Vec<String> = c
                .prepare(sql)
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect();
            v.sort();
            v
        };
        vec![
            ("entities".into(), q("SELECT kind||'|'||name||'|'||syntax_path FROM code_entities")),
            ("resolution states".into(), q("SELECT state FROM code_relationship_resolutions")),
            ("resolution commits".into(), q("SELECT commit_oid||'|'||resolver||'|'||resolver_version FROM code_resolution_derivations")),
            ("contexts".into(), q("SELECT path||'|'||IFNULL(commit_oid,'') FROM code_contexts")),
            ("commits".into(), q("SELECT oid FROM git_commits")),
            ("refs".into(), q("SELECT name||'|'||target_oid FROM git_refs")),
        ]
    }
}

fn extract(db: &Db, r: &Repo) -> ExtractedGit {
    let store = db.store();
    extract_git_repository_selected_with_deadline(
        &StoreContext(&store),
        &r.path,
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &Deadline::none(),
    )
    .unwrap()
}

fn inject(db: &Db, table: &str, when: &str) {
    db.raw()
        .execute_batch(&format!(
            "CREATE TRIGGER fault BEFORE INSERT ON {table} {when} \
             BEGIN SELECT RAISE(ABORT, 'injected fault at {table}'); END;"
        ))
        .unwrap();
}

fn clear(db: &Db) {
    db.raw().execute_batch("DROP TRIGGER fault").unwrap();
}

/// A clean ingestion into a fresh database: the reference everything else must equal.
fn reference(r: &Repo) -> Db {
    let d = db();
    let ex = extract(&d, r);
    persist_git(&d.store(), ex, &Deadline::none()).unwrap();
    d
}

// ------------------------------------------------------------------ the fixture is what it says

#[test]
fn the_fixture_has_two_view_commits_code_and_resolutions() {
    let _serial = serial();
    let r = repo();
    let d = db();
    let ex = extract(&d, &r);
    assert_eq!(
        ex.observation.view_commits.len(),
        2,
        "HEAD and the linked worktree"
    );
    assert!(!ex.code_inputs.is_empty());
    let refd = reference(&r);
    assert_eq!(
        refd.count("code_resolution_derivations"),
        2,
        "one resolution per view commit"
    );
    assert!(refd.count("code_relationship_resolutions") > 0);
    assert!(refd.count("git_commits") >= 2);
}

// ------------------------------------------------------------------ one ingestion, one revision

#[test]
fn one_git_ingestion_consumes_exactly_one_recording_revision() {
    let _serial = serial();
    let r = repo();
    let d = db();
    let before = d.count("m6_revisions");
    let ex = extract(&d, &r);
    persist_git(&d.store(), ex, &Deadline::none()).unwrap();
    let used = d.count("m6_revisions") - before;
    eprintln!("revisions consumed by one git ingestion: {used}");
    assert_eq!(
        used, 1,
        "git observation + code + 2 resolutions used {used} revisions"
    );
}

// ------------------------------------------------------------------ every write boundary

/// Each case: (what is faulted, the table, an optional WHEN clause). The ones marked BOUNDARY fire
/// exactly where one of the old transactions ended and the next began.
const FAULTS: &[(&str, &str, &str)] = &[
    ("first git write", "git_commits", ""),
    ("git refs", "git_refs", ""),
    ("late git write (views)", "git_views", ""),
    ("BOUNDARY git -> code: first code input", "code_inputs", ""),
    ("code derivations", "code_derivations", ""),
    (
        "late code write (relationships)",
        "code_relationship_observations",
        "",
    ),
    (
        "BOUNDARY code -> resolution: first resolution",
        "code_resolution_derivations",
        "",
    ),
    (
        "BOUNDARY resolution 1 -> 2: the second view commit",
        "code_resolution_derivations",
        "WHEN (SELECT COUNT(*) FROM code_resolution_derivations) >= 1",
    ),
    ("inside a resolution", "code_relationship_resolutions", ""),
];

#[test]
fn a_failure_at_any_write_boundary_leaves_every_table_and_every_revision_unchanged() {
    let _serial = serial();
    let r = repo();
    let mut report = Vec::new();
    for (what, table, when) in FAULTS {
        let d = db();
        let ex = extract(&d, &r);
        let before = d.dump();
        inject(&d, table, when);
        let err = persist_git(&d.store(), ex, &Deadline::none()).unwrap_err();
        assert!(
            err.to_string().contains("injected fault"),
            "{what}: unexpected error {err}"
        );
        let after = d.dump();
        let leaked: Vec<String> = before
            .iter()
            .filter(|(t, rows)| after.get(*t) != Some(rows))
            .map(|(t, rows)| format!("{t} ({} -> {} rows)", rows.len(), after[t].len()))
            .collect();
        let revisions = d.count("m6_revisions") - before["m6_revisions"].len() as i64;
        eprintln!(
            "{what:<52} leaked tables: {:>2}, revisions consumed: {revisions}",
            leaked.len()
        );
        if !leaked.is_empty() || revisions != 0 {
            report.push(format!(
                "{what}: {} tables changed, {revisions} revisions: {}",
                leaked.len(),
                leaked.join(", ")
            ));
        }
        assert_eq!(d.integrity(), "ok", "{what}");
    }
    assert!(
        report.is_empty(),
        "partial persistence at {} of {} boundaries:\n{}",
        report.len(),
        FAULTS.len(),
        report.join("\n")
    );
}

#[test]
fn a_retry_after_a_failure_equals_one_clean_ingestion_at_every_boundary() {
    let _serial = serial();
    let r = repo();
    let clean = reference(&r);
    let mut report = Vec::new();
    for (what, table, when) in FAULTS {
        let d = db();
        let ex = extract(&d, &r);
        inject(&d, table, when);
        assert!(
            persist_git(&d.store(), ex, &Deadline::none()).is_err(),
            "{what}"
        );
        clear(&d);
        let ex = extract(&d, &r);
        persist_git(&d.store(), ex, &Deadline::none()).unwrap();
        if d.counts() != clean.counts() || d.derived() != clean.derived() {
            let diff: Vec<String> = clean
                .counts()
                .into_iter()
                .filter(|(t, n)| d.counts().get(t) != Some(n))
                .map(|(t, n)| format!("{t}: {} vs {n}", d.counts()[&t]))
                .collect();
            report.push(format!(
                "{what}: differs from a clean ingestion ({})",
                diff.join(", ")
            ));
        }
    }
    assert!(report.is_empty(), "{}", report.join("\n"));
}

// ------------------------------------------------------------------ a repeated ingestion is stable

#[test]
fn ingesting_the_same_repository_again_adds_no_derived_structure() {
    let _serial = serial();
    let r = repo();
    let d = db();
    for _ in 0..2 {
        let ex = extract(&d, &r);
        persist_git(&d.store(), ex, &Deadline::none()).unwrap();
    }
    let once = reference(&r);
    for t in [
        "code_inputs",
        "code_derivations",
        "code_entities",
        "code_resolution_derivations",
        "code_relationship_resolutions",
        "git_commits",
        "git_trees",
    ] {
        assert_eq!(d.count(t), once.count(t), "{t} grew on re-ingestion");
    }
}

// ------------------------------------------------------------------ a repository without code

#[test]
fn a_repository_without_source_code_still_commits_in_one_revision() {
    let _serial = serial();
    let d0 = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(d0.path()).unwrap().join("docs-only");
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("a.md"), "# A\n\ntext\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "one"]);
    let r = Repo { _d: d0, path };
    let d = db();
    let before = d.count("m6_revisions");
    let ex = extract(&d, &r);
    assert!(ex.code_inputs.is_empty());
    persist_git(&d.store(), ex, &Deadline::none()).unwrap();
    assert_eq!(d.count("m6_revisions") - before, 1);
    assert_eq!(d.count("code_inputs"), 0);
    assert!(d.count("git_commits") > 0);
}

// ------------------------------------------------------------------ the pre-commit gate

#[test]
fn a_refusal_right_before_the_commit_rolls_everything_back() {
    let _serial = serial();
    let r = repo();
    let d = db();
    let ex = extract(&d, &r);
    let before = d.dump();
    // every statement succeeded; the gate (the later commit-time authorization) says no
    let err = persist_git_gated(&d.store(), ex, &Deadline::none(), &|| {
        Err(mindspace::Error::Storage("refused at the gate".into()))
    })
    .unwrap_err();
    assert!(err.to_string().contains("refused at the gate"), "{err}");
    assert_eq!(d.dump(), before, "a refused commit left a trace");
    assert_eq!(d.integrity(), "ok");
}

// ------------------------------------------------------------------ the COMMIT itself

#[test]
fn a_failed_sqlite_commit_is_a_clean_rollback_and_the_handle_stays_usable() {
    let _serial = serial();
    let r = repo();
    let d = db();
    let store = d.store(); // one handle for both attempts
    let ex = extract(&d, &r);
    let before = d.dump();
    // a real SQLite COMMIT failure (the commit hook vetoes): SQLite turns it into a rollback
    mindspace::storage::sqlite::gate::FAIL_COMMIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let err = persist_git(&store, ex, &Deadline::none()).unwrap_err();
    assert!(
        !matches!(err, mindspace::Error::CommitOutcomeUnknown { .. }),
        "a vetoed COMMIT is a definite rollback, not an unknown outcome: {err}"
    );
    assert_eq!(d.dump(), before, "the vetoed commit left a trace");
    assert_eq!(d.integrity(), "ok");
    // the same handle ingests correctly afterwards, and equals a clean ingestion
    let ex = extract(&d, &r);
    persist_git(&store, ex, &Deadline::none()).unwrap();
    let clean = reference(&r);
    assert_eq!(d.counts(), clean.counts());
    assert_eq!(d.derived(), clean.derived());
}

#[test]
fn an_unknown_commit_outcome_is_reported_as_unknown_and_a_retry_adds_no_derived_structure() {
    let _serial = serial();
    let r = repo();
    let d = db();
    let ex = extract(&d, &r);
    // the COMMIT really succeeds, but the storage layer must report that it cannot know
    mindspace::storage::sqlite::gate::LOSE_ACK.store(true, std::sync::atomic::Ordering::SeqCst);
    let err = persist_git(&d.store(), ex, &Deadline::none()).unwrap_err();
    assert!(
        matches!(err, mindspace::Error::CommitOutcomeUnknown { .. }),
        "an unknown outcome must not look like an ordinary failure or a success: {err}"
    );
    let clean = reference(&r);
    // in fact it is committed, completely: one revision, all derived structure present
    assert_eq!(d.derived(), clean.derived());
    // the documented remedy: ingest again (on a new handle). It adds no derived structure.
    let ex = extract(&d, &r);
    persist_git(&d.store(), ex, &Deadline::none()).unwrap();
    assert_eq!(
        d.derived(),
        clean.derived(),
        "a retry after an unknown outcome changed the derived structure"
    );
    for t in [
        "code_inputs",
        "code_derivations",
        "code_entities",
        "code_contexts",
        "code_resolution_derivations",
        "code_relationship_resolutions",
        "git_commits",
        "git_trees",
        "git_blobs",
    ] {
        assert_eq!(d.count(t), clean.count(t), "{t} differs after the retry");
    }
}
