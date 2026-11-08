//! M7.3b, tasks 1 and 2: extraction is separate from persistence.
//!
//! Evidence for the split (design note `docs/design-m7.3b-dynamic-workers.md`):
//!  * extraction reads the database only through a read-only context and leaves
//!    every table as it was;
//!  * extraction followed by persistence gives the same result as the in-process
//!    ingest, for directories, files and Git (head, selections, history);
//!  * a failed or timed-out extraction persists nothing, and persistence still
//!    honours the deadline;
//!  * results are serializable and can be validated as untrusted input.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use mindspace::core::{
    CodeLanguage, CodeParserIdentity, Error, GitRefObservation, IngestOutcome, Result, Uuid,
};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{
    persist, Extracted, ExtractionContext, ExtractionLimits, StoreContext,
};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::{self, GitScope};
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

// ------------------------------------------------------------------ helpers

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    let store = open_existing(&path).unwrap();
    store.create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

/// A byte-identical copy of an (untouched) database, so both hold the same space ids.
fn db_clone(src: &Db) -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    std::fs::copy(&src.path, &path).unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }

    /// Row count of every table: a coarse but total snapshot of the contents.
    fn rows(&self) -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n: i64 = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
}

/// A plain directory with documents and Rust code.
fn directory() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "docs/guide.md",
        "# Guide\n\nalpha marker\n\n## Part\n\nbeta\n",
    );
    write(d.path(), "notes.txt", "plain note\n");
    write(
        d.path(),
        "src/lib.rs",
        "pub fn alpha_symbol() {}\npub struct Beta;\n",
    );
    write(
        d.path(),
        "src/more.rs",
        "use crate::alpha_symbol;\nfn gamma() { alpha_symbol(); }\n",
    );
    d
}

/// A Git repository with history: two commits, a branch, an annotated tag.
fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main", "."]);
    write(d.path(), "README.md", "# Repo\n\nfirst\n");
    write(d.path(), "src/lib.rs", "pub fn one() {}\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "first"]);
    git(d.path(), &["tag", "-a", "v1", "-m", "release one"]);
    git(d.path(), &["checkout", "-q", "-b", "feature"]);
    write(d.path(), "src/extra.rs", "pub fn two() { one(); }\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "feature work"]);
    git(d.path(), &["checkout", "-q", "main"]);
    write(d.path(), "docs/more.md", "# More\n\nsecond\n");
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "second"]);
    d
}

/// Ids and timestamps differ between runs; everything else must not.
fn normalize(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let hex = |c: u8| c.is_ascii_hexdigit();
    while i < b.len() {
        // uuid: 8-4-4-4-12
        if i + 36 <= b.len()
            && [8, 13, 18, 23].iter().all(|&k| b[i + k] == b'-')
            && b[i..i + 36]
                .iter()
                .enumerate()
                .all(|(k, &c)| [8, 13, 18, 23].contains(&k) || hex(c))
        {
            out.push_str("<id>");
            i += 36;
            continue;
        }
        // RFC 3339 time: 2026-11-08T10:20:30...Z
        if i + 20 <= b.len()
            && b[i + 4] == b'-'
            && b[i + 10] == b'T'
            && b[i..i + 4].iter().all(u8::is_ascii_digit)
        {
            let mut j = i + 19;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            if j < b.len() && b[j] == b'Z' {
                out.push_str("<time>");
                i = j + 1;
                continue;
            }
        }
        // microsecond timestamps (`Ts(1794142654740130)`)
        if b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j - i >= 15 {
                out.push_str("<ts>");
                i = j;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn outcome_text(o: &[IngestOutcome]) -> String {
    normalize(&format!("{o:#?}"))
}

/// A read-only context that remembers what was asked.
struct Recording<'a> {
    inner: StoreContext<'a>,
    calls: RefCell<Vec<&'static str>>,
}

impl<'a> Recording<'a> {
    fn new(store: &'a dyn Store) -> Self {
        Self {
            inner: StoreContext(store),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl ExtractionContext for Recording<'_> {
    fn space_id(&self, name: &str) -> Result<Uuid> {
        self.calls.borrow_mut().push("space_id");
        self.inner.space_id(name)
    }
    fn git_refs_current(&self, id: Uuid) -> Result<Vec<GitRefObservation>> {
        self.calls.borrow_mut().push("git_refs_current");
        self.inner.git_refs_current(id)
    }
    fn has_code_derivation(
        &self,
        l: CodeLanguage,
        h: &str,
        r: Option<Uuid>,
        b: Option<&str>,
        i: &CodeParserIdentity,
    ) -> Result<bool> {
        self.calls.borrow_mut().push("has_code_derivation");
        self.inner.has_code_derivation(l, h, r, b, i)
    }
}

/// Every way `ingest_paths_with_deadline` can go, as (label, path, selection, scope).
fn scenarios(dir: &Path, repo: &Path) -> Vec<(&'static str, PathBuf, PathSelection, GitScope)> {
    vec![
        (
            "directory",
            dir.to_path_buf(),
            PathSelection::all(),
            GitScope::Head,
        ),
        (
            "single file",
            dir.join("docs/guide.md"),
            PathSelection::all(),
            GitScope::Head,
        ),
        (
            "git head",
            repo.to_path_buf(),
            PathSelection::all(),
            GitScope::Head,
        ),
        (
            "git selection",
            repo.to_path_buf(),
            PathSelection::new(&["src"]).unwrap(),
            GitScope::Head,
        ),
        (
            "git history",
            repo.to_path_buf(),
            PathSelection::all(),
            GitScope::History,
        ),
        (
            "git views",
            repo.to_path_buf(),
            PathSelection::all(),
            GitScope::Views(ingest::GitViews {
                branches: true,
                remotes: false,
                tags: true,
            }),
        ),
    ]
}

fn extract_one(
    ctx: &dyn ExtractionContext,
    path: &Path,
    sel: &PathSelection,
    scope: GitScope,
) -> Result<Extracted> {
    let canonical = std::fs::canonicalize(path).unwrap();
    if canonical.is_dir() && ingest::git::looks_like_git_repository(&canonical) {
        return ingest::extract_git_repository_selected_with_deadline(
            ctx,
            &canonical,
            &["p"],
            sel,
            scope,
            &Deadline::none(),
        )
        .map(Extracted::Git);
    }
    ingest::extract_path_with_deadline(ctx, path, &["p"], &Deadline::none())
}

// -------------------------------------------------------------------- tests

#[test]
fn extraction_reads_through_a_read_only_context_and_leaves_every_table_alone() {
    let (dir, repo) = (directory(), repo());
    for (label, path, sel, scope) in scenarios(dir.path(), repo.path()) {
        let db = db();
        let store = db.store();
        let before = db.rows();
        let ctx = Recording::new(&store);
        let extracted =
            extract_one(&ctx, &path, &sel, scope).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(
            db.rows(),
            before,
            "{label}: extraction changed the database"
        );
        assert!(
            ctx.calls.borrow().iter().all(|c| [
                "space_id",
                "git_refs_current",
                "has_code_derivation"
            ]
            .contains(c)),
            "{label}"
        );
        assert!(
            ctx.calls.borrow().contains(&"space_id"),
            "{label}: spaces are resolved through the context"
        );
        assert!(extracted.content_bytes() > 0, "{label}");
    }
}

#[test]
fn extract_then_persist_equals_the_in_process_ingest() {
    let (dir, repo) = (directory(), repo());
    for (label, path, sel, scope) in scenarios(dir.path(), repo.path()) {
        // A: the public in-process ingest
        let a = db();
        let sa = a.store();
        let direct = match label {
            "single file" => vec![ingest::ingest_path(&sa, &path, &["p"]).unwrap()],
            _ => ingest::ingest_paths(&sa, &path, &sel, scope, &["p"]).unwrap(),
        };
        // B: extraction, then persistence, through the new interfaces
        let b = db();
        let sb = b.store();
        let extracted = extract_one(&StoreContext(&sb), &path, &sel, scope).unwrap();
        let split = vec![persist(&sb, extracted, &Deadline::none()).unwrap()];
        // the directory and file scenarios are single outcomes in both
        if label == "git selection"
            || label.starts_with("git")
            || label == "single file"
            || label == "directory"
        {
            assert_eq!(
                outcome_text(&direct),
                outcome_text(&split),
                "{label}: outcomes differ"
            );
        }
        assert_eq!(a.rows(), b.rows(), "{label}: the database contents differ");
    }
}

#[test]
fn a_second_git_ingest_derives_its_ref_events_from_the_previous_state_in_the_database() {
    let repo = repo();
    let db = db();
    let store = db.store();
    ingest::ingest_git_repository(&store, repo.path(), &["p"]).unwrap();
    // main moves forward
    write(repo.path(), "docs/third.md", "# Third\n");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "third"]);
    let before = db.rows();
    let ctx = Recording::new(&store);
    let ex = ingest::extract_git_repository_selected_with_deadline(
        &ctx,
        repo.path(),
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &Deadline::none(),
    )
    .unwrap();
    assert_eq!(db.rows(), before, "extraction wrote");
    assert!(ctx.calls.borrow().contains(&"git_refs_current"));
    let kinds: Vec<String> = ex
        .observation
        .ref_events
        .iter()
        .map(|e| format!("{}:{:?}", e.name, e.kind))
        .collect();
    assert!(
        kinds.iter().any(|k| k.contains("FastForward")),
        "the moved ref is a fast-forward of the recorded state: {kinds:?}"
    );
}

#[test]
fn a_failed_extraction_persists_nothing_and_persistence_still_honours_the_deadline() {
    let (dir, repo) = (directory(), repo());
    let db = db();
    let store = db.store();
    let before = db.rows();

    // 1. an expired deadline stops extraction (directory and git)
    let expired = Deadline::after(Duration::from_nanos(1));
    std::thread::sleep(Duration::from_millis(5));
    let e = ingest::extract_path_with_deadline(&StoreContext(&store), dir.path(), &["p"], &expired);
    assert!(matches!(e, Err(Error::IngestTimeout { .. })), "{e:?}");
    let e = ingest::extract_git_repository_selected_with_deadline(
        &StoreContext(&store),
        repo.path(),
        &["p"],
        &PathSelection::all(),
        GitScope::Head,
        &expired,
    );
    assert!(
        matches!(e, Err(Error::IngestTimeout { .. })),
        "{:?}",
        e.err()
    );

    // 2. an unknown space, a missing path
    assert!(ingest::extract_path_with_deadline(
        &StoreContext(&store),
        dir.path(),
        &["nope"],
        &Deadline::none()
    )
    .is_err());
    assert!(ingest::extract_path_with_deadline(
        &StoreContext(&store),
        &dir.path().join("missing"),
        &["p"],
        &Deadline::none()
    )
    .is_err());

    // 3. a context that fails midway (the read the extraction depends on)
    struct Failing;
    impl ExtractionContext for Failing {
        fn space_id(&self, _: &str) -> Result<Uuid> {
            Ok(Uuid::nil())
        }
        fn git_refs_current(&self, _: Uuid) -> Result<Vec<GitRefObservation>> {
            Err(Error::Storage("the database went away".into()))
        }
        fn has_code_derivation(
            &self,
            _: CodeLanguage,
            _: &str,
            _: Option<Uuid>,
            _: Option<&str>,
            _: &CodeParserIdentity,
        ) -> Result<bool> {
            Err(Error::Storage("the database went away".into()))
        }
    }
    assert!(
        ingest::extract_path_with_deadline(&Failing, dir.path(), &["p"], &Deadline::none())
            .is_err(),
        "directory with Rust files"
    );
    assert!(
        extract_one(&Failing, repo.path(), &PathSelection::all(), GitScope::Head).is_err(),
        "git"
    );
    assert_eq!(db.rows(), before, "a failed extraction left rows behind");

    // 4. extraction succeeded, but the limit expired before the write: nothing is written
    for (_, path, sel, scope) in scenarios(dir.path(), repo.path()) {
        let ex = extract_one(&StoreContext(&store), &path, &sel, scope).unwrap();
        let late = Deadline::after(Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        let r = persist(&store, ex, &late);
        assert!(matches!(r, Err(Error::IngestTimeout { .. })), "{r:?}");
    }
    assert_eq!(db.rows(), before, "persist wrote after the deadline");
}

#[test]
fn a_real_result_validates_and_a_tampered_one_is_rejected() {
    let (dir, repo) = (directory(), repo());
    let db = db();
    let store = db.store();
    let ctx = StoreContext(&store);
    let limits = ExtractionLimits::default();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let files = extract_one(&ctx, dir.path(), &PathSelection::all(), GitScope::Head).unwrap();
    files.validate(Some(&root), &limits).unwrap();
    let git = extract_one(&ctx, repo.path(), &PathSelection::all(), GitScope::Head).unwrap();
    git.validate(None, &limits).unwrap();

    let reject = |x: &Extracted, root: Option<&Path>, l: &ExtractionLimits, what: &str| {
        let e = x.validate(root, l).expect_err(what);
        assert!(matches!(e, Error::InvalidExtraction(_)), "{what}: {e:?}");
    };
    // content no longer matches its hash
    let mut x = files.clone();
    if let Extracted::Files(f) = &mut x {
        f.observation.files[0].content.push_str(" tampered");
    }
    reject(&x, Some(&root), &limits, "content hash");
    // a location outside the root, a `..` location, a different session root
    let mut x = files.clone();
    if let Extracted::Files(f) = &mut x {
        f.observation.files[0].location = "/etc/passwd".into();
    }
    reject(&x, Some(&root), &limits, "location outside the root");
    let mut x = files.clone();
    if let Extracted::Files(f) = &mut x {
        f.observation.files[0].location = format!("{}/../outside", root.display());
    }
    reject(&x, Some(&root), &limits, "dot-dot location");
    reject(
        &files,
        Some(Path::new("/some/other/root")),
        &limits,
        "session root",
    );
    // size limits
    reject(
        &files,
        Some(&root),
        &ExtractionLimits {
            max_files: 1,
            ..limits
        },
        "file count",
    );
    reject(
        &files,
        Some(&root),
        &ExtractionLimits {
            max_content_bytes: 10,
            ..limits
        },
        "content bytes",
    );
    // code inputs
    let mut x = files.clone();
    if let Extracted::Files(f) = &mut x {
        f.code_inputs[0].content.push('x');
    }
    reject(&x, Some(&root), &limits, "code hash");
    // git: absolute or climbing paths, malformed ids, bad hash
    for tamper in 0..4 {
        let mut x = git.clone();
        if let Extracted::Git(g) = &mut x {
            let d = &mut g.observation.derivations[0];
            match tamper {
                0 => d.path = "/abs/path.md".into(),
                1 => d.path = "../escape.md".into(),
                2 => d.blob_oid = "not-an-oid".into(),
                _ => d.content.push('x'),
            }
        }
        reject(&x, None, &limits, &format!("git tamper {tamper}"));
    }
    // a validated value persists like any other
    persist(&store, files, &Deadline::none()).unwrap();
    persist(&store, git, &Deadline::none()).unwrap();
}

#[test]
fn extracted_values_round_trip_through_serde_and_persist_the_same() {
    let (dir, repo) = (directory(), repo());
    for (label, path, sel, scope) in scenarios(dir.path(), repo.path()) {
        let a = db();
        let b = db_clone(&a);
        let ex = extract_one(&StoreContext(&a.store()), &path, &sel, scope).unwrap();
        let json = serde_json::to_string(&ex).unwrap();
        let back: Extracted = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&ex).unwrap(),
            "{label}: the round trip changed the value"
        );
        let oa = persist(&a.store(), ex, &Deadline::none()).unwrap();
        let ob = persist(&b.store(), back, &Deadline::none()).unwrap();
        assert_eq!(
            normalize(&format!("{oa:?}")),
            normalize(&format!("{ob:?}")),
            "{label}"
        );
        assert_eq!(a.rows(), b.rows(), "{label}");
        // a hostile frame does not deserialize into something else
        assert!(
            serde_json::from_str::<Extracted>(&json.replacen("\"kind\"", "\"kinds\"", 1)).is_err(),
            "{label}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_anchored_confined_extraction_matches_the_confined_ingest() {
    use mindspace::ingest::confined::{
        extract_confined_git, extract_directory, ingest_confined, Anchor,
    };
    use mindspace::mcp::McpConfig;
    let (dir, repo) = (directory(), repo());
    for (label, root, is_git) in [("directory", dir.path(), false), ("git", repo.path(), true)] {
        let root = std::fs::canonicalize(root).unwrap();
        let cfg = McpConfig::new(&[&root]).unwrap();
        let anchor = Anchor::open(&root, Some(&root), &cfg, true).unwrap();
        let a = db();
        let sa = a.store();
        let direct = ingest_confined(
            &sa,
            &anchor,
            &cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
        )
        .unwrap();

        let b = db();
        let sb = b.store();
        let before = b.rows();
        let ex = if is_git {
            Extracted::Git(
                extract_confined_git(
                    &StoreContext(&sb),
                    &anchor,
                    &cfg,
                    &PathSelection::all(),
                    GitScope::Head,
                    &["p"],
                    &Deadline::none(),
                )
                .unwrap(),
            )
        } else {
            let ids = vec![sb.get_space("p").unwrap().id];
            Extracted::Files(
                extract_directory(&StoreContext(&sb), &anchor, &cfg, &ids, &Deadline::none())
                    .unwrap(),
            )
        };
        assert_eq!(b.rows(), before, "{label}: confined extraction wrote");
        ex.validate(Some(&root), &ExtractionLimits::default())
            .or_else(|e| if is_git { Ok(()) } else { Err(e) })
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        let split = persist(&sb, ex, &Deadline::none()).unwrap();
        assert_eq!(outcome_text(&direct), outcome_text(&[split]), "{label}");
        assert_eq!(a.rows(), b.rows(), "{label}");
    }
}
