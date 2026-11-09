//! M7.3b.5b stabilization: the extractor version is part of a derivation's identity.
//!
//! `mind-code@3` -> `@4` changes what the extractor outputs for malformed input (a recovered item
//! inside an impl is now classified like its parent path). A derivation made by `@3` must not be
//! reused as if `@4` had made it: reuse is keyed on the whole identity (extractor, version,
//! grammar, grammar version) in the store query, in the insert, and in the worker's context
//! exchange. These tests relabel stored derivations as `@3` (exactly what an older binary
//! wrote), ingest again with the current code and compare the CONTENTS of the code tables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{CodeLanguage, CodeParserIdentity};
use mindspace::ingest::code;
use mindspace::ingest::confined::{extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, persist_git, Extracted, ExtractedGit, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::{extract_git_repository_selected_with_deadline, GitScope};
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

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

const CODE_TABLES: &[&str] = &[
    "code_inputs",
    "code_contexts",
    "code_derivations",
    "code_entities",
    "code_relationship_observations",
    "code_resolution_derivations",
    "code_relationship_resolutions",
    "code_resolution_candidates",
];

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
    fn counts(&self) -> BTreeMap<&'static str, i64> {
        CODE_TABLES.iter().map(|t| (*t, self.count(t))).collect()
    }
    fn versions(&self) -> BTreeMap<String, i64> {
        let c = self.raw();
        let mut s = c
            .prepare("SELECT extractor_version, COUNT(*) FROM code_derivations GROUP BY 1")
            .unwrap();
        s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .flatten()
            .collect()
    }
    /// What an older binary left behind: derivations made by `@3`, resolutions made by resolver
    /// version `1` (the version `@3` wrote).
    fn relabel_as_at3(&self) {
        let c = self.raw();
        // The store makes these tables immutable and versions their changes with SQL functions only
        // its own connection has; a legacy row was written by an older binary, not updated. For
        // this throwaway database only, the UPDATE guards on them are lifted for the statements
        // and restored (the store verifies its trigger inventory when it opens).
        let triggers: Vec<(String, String)> = c
            .prepare(
                "SELECT name, sql FROM sqlite_master WHERE type='trigger' \
                 AND tbl_name IN ('code_derivations','code_resolution_derivations') \
                 AND upper(sql) LIKE '% UPDATE %'",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        assert!(triggers.len() >= 2);
        for (name, _) in &triggers {
            c.execute_batch(&format!("DROP TRIGGER \"{name}\""))
                .unwrap();
        }
        let n = c
            .execute("UPDATE code_derivations SET extractor_version='3'", [])
            .unwrap();
        assert!(n > 0);
        c.execute(
            "UPDATE code_resolution_derivations SET resolver_version='1'",
            [],
        )
        .unwrap();
        for (_, sql) in &triggers {
            c.execute_batch(sql).unwrap();
        }
    }
    fn input_hashes(&self) -> Vec<(String, Option<String>, Option<String>)> {
        let c = self.raw();
        let mut s = c
            .prepare("SELECT content_hash, repository_id, blob_oid FROM code_inputs ORDER BY 1")
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .flatten()
            .collect()
    }
    /// Observations of the EFFECTIVE (newest) derivation of each input, and how many of those
    /// have a resolution.
    fn effective_observations(&self) -> (i64, i64) {
        let eff = "d.id = (SELECT d2.id FROM code_derivations d2 WHERE d2.input_id = d.input_id \
                   ORDER BY d2.created_at DESC, d2.id DESC LIMIT 1)";
        let q = |extra: &str| -> i64 {
            self.raw()
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM code_relationship_observations o \
                         JOIN code_derivations d ON d.id=o.derivation_id WHERE {eff} {extra}"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        (
            q(""),
            q("AND EXISTS (SELECT 1 FROM code_relationship_resolutions r WHERE r.observation_id=o.id)"),
        )
    }
}

fn identity_with(version: &str) -> CodeParserIdentity {
    let mut id = code::identity();
    id.extractor_version = version.into();
    id
}

// ---------------------------------------------------------------------------------------- identity

#[test]
fn the_extractor_is_version_4_and_every_skip_identity_follows() {
    let id = code::identity();
    assert_eq!(
        (id.extractor.as_str(), id.extractor_version.as_str()),
        ("mind-code", "4")
    );
    let policy = mindspace::ingest::code::ParsePolicy::unlimited();
    let ids = mindspace::worker::context::identities_for(&policy);
    assert_eq!(
        ids[0], id,
        "the context exchange asks about the current identity first"
    );
    for skip in &ids[1..] {
        assert!(skip.extractor_version.starts_with("4+skip("), "{skip:?}");
    }
}

// --------------------------------------------------------------------------------------- directory

fn tree() -> (tempfile::TempDir, PathBuf, McpConfig) {
    let d = tempfile::tempdir().unwrap();
    for (n, c) in [
        ("docs/guide.md", "# Guide\n\nalpha\n"),
        ("src/lib.rs", "pub fn alpha() {}\npub struct Beta;\n"),
        (
            "src/more.rs",
            "use crate::alpha;\nfn gamma() { alpha(); }\n",
        ),
    ] {
        let p = d.path().join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    }
    let root = std::fs::canonicalize(d.path()).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    (d, root, cfg)
}

fn ingest_dir(db: &Db, root: &Path, cfg: &McpConfig) {
    let store = db.store();
    let space = store.get_space("p").unwrap().id;
    let anchor = Anchor::open(root, Some(root), cfg, false).unwrap();
    let ex = extract_directory(
        &StoreContext(&store),
        &anchor,
        cfg,
        &[space],
        &Deadline::none(),
    )
    .unwrap();
    persist(&store, Extracted::Files(ex), &Deadline::none()).unwrap();
}

fn alpha_hits(db: &Db) -> usize {
    mindspace::retrieval::search_code(
        &db.store(),
        mindspace::core::CodeSearchQuery {
            text: "alpha".into(),
            kinds: vec![],
            repository: None,
            path_prefix: None,
            space_ids: None,
            limit: 100,
            all_versions: false,
        },
    )
    .unwrap()
    .len()
}

#[test]
fn an_at3_derivation_is_not_reused_as_at4_and_the_upgrade_is_idempotent() {
    let (_d, root, cfg) = tree();
    let db = db();
    ingest_dir(&db, &root, &cfg);
    assert_eq!(db.versions(), BTreeMap::from([("4".to_string(), 2)]));
    let hits = alpha_hits(&db);
    assert!(hits > 0);

    // an unchanged ingestion with the same version reuses everything and adds nothing
    let before_same = db.counts();
    ingest_dir(&db, &root, &cfg);
    assert_eq!(before_same, db.counts());

    // the store's answer, by identity
    let (hash, repo, blob): (String, Option<String>, Option<String>) = db.input_hashes()[0].clone();
    assert!(repo.is_none() && blob.is_none());
    let has = |v: &str| {
        db.store()
            .has_code_derivation(CodeLanguage::Rust, &hash, None, None, &identity_with(v))
            .unwrap()
    };
    assert!(has("4") && !has("3"));

    // an older binary's derivations: the same files, unchanged
    db.relabel_as_at3();
    assert!(has("3") && !has("4"), "@3 must not answer for @4");
    let before_upgrade = db.counts();

    // the upgrade: every unchanged file is parsed again, beside the old derivation
    ingest_dir(&db, &root, &cfg);
    assert_eq!(
        db.versions(),
        BTreeMap::from([("3".to_string(), 2), ("4".to_string(), 2)])
    );
    let after_upgrade = db.counts();
    assert_eq!(after_upgrade["code_derivations"], 4);
    assert_eq!(
        after_upgrade["code_inputs"], before_upgrade["code_inputs"],
        "inputs are content-addressed"
    );
    assert_eq!(
        after_upgrade["code_contexts"], before_upgrade["code_contexts"],
        "no duplicate context"
    );
    assert!(has("4"));
    // readers see the newest derivation only: no duplicate hits
    assert_eq!(alpha_hits(&db), hits, "the upgrade duplicated search hits");

    // repeating the ingestion at @4 reuses and adds nothing, in any code table
    ingest_dir(&db, &root, &cfg);
    assert_eq!(after_upgrade, db.counts());
    assert_eq!(alpha_hits(&db), hits);
}

// -------------------------------------------------------------------------------------------- git

fn extract_git(db: &Db, r: &Repo) -> ExtractedGit {
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

fn ingest_git(db: &Db, r: &Repo) {
    let ex = extract_git(db, r);
    persist_git(&db.store(), ex, &Deadline::none()).unwrap();
}

#[test]
fn a_git_ingestion_re_derives_and_re_resolves_after_an_extractor_change() {
    let r = repo();
    let db = db();
    ingest_git(&db, &r);
    let (obs, resolved) = db.effective_observations();
    assert!(
        obs > 0 && resolved > 0,
        "the control: {obs} observations, {resolved} resolved"
    );
    assert_eq!(db.versions().keys().collect::<Vec<_>>(), ["4"]);

    // unchanged repository, same version: nothing is parsed again, nothing added
    let steady = db.counts();
    ingest_git(&db, &r);
    assert_eq!(steady, db.counts());

    // an older binary's derivations
    db.relabel_as_at3();
    let before = db.counts();
    ingest_git(&db, &r);
    assert_eq!(db.count("code_derivations"), 2 * before["code_derivations"]);
    assert_eq!(db.count("code_inputs"), before["code_inputs"]);
    assert_eq!(db.count("code_contexts"), before["code_contexts"]);

    // what a reader sees: the observations of the newest derivations, each with its resolution.
    // (A resolution computed from the @3 derivations belongs to @3 observations that no reader
    // looks at any more.)
    let (obs2, resolved2) = db.effective_observations();
    assert_eq!(obs2, obs);
    assert_eq!(
        resolved2, resolved,
        "the relationships of the new derivations were not resolved"
    );

    // and it is stable: a further ingestion changes nothing
    let upgraded = db.counts();
    ingest_git(&db, &r);
    assert_eq!(upgraded, db.counts());
}
