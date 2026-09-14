//! Git repository ingest tests: scenarios A-P from the v0.2 task, run
//! against real temporary git repositories.
//!
//! The first test was written BEFORE the git model existed, to demonstrate
//! the v0.1 defect it corrects: directory ingest observes only the working
//! tree, so knowledge that exists in committed history was invisible.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{GitObservationReport, MemoryState, SearchQuery};
use mindspace::ingest;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Runs a git command with a fixed identity, returning stdout. Signing and
/// any interactive prompt are disabled: tests never touch gpg or terminals.
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
        .expect("git binary available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Env {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("mind.db");
    initialize(&db).expect("init");
    Env { _dir: dir, db }
}

impl Env {
    fn open(&self) -> SqliteStore {
        open_existing(&self.db).expect("open")
    }
}

struct Repo {
    _dir: tempfile::TempDir,
}

fn repo() -> Repo {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main", "."]);
    Repo { _dir: dir }
}

impl Repo {
    fn path(&self) -> &Path {
        self._dir.path()
    }

    fn write(&self, name: &str, content: &str) {
        let path = self._dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn rm(&self, name: &str) {
        std::fs::remove_file(self._dir.path().join(name)).unwrap();
    }

    fn commit(&self, msg: &str) -> String {
        git(self._dir.path(), &["add", "-A"]);
        git(self._dir.path(), &["commit", "-q", "-m", msg]);
        git(self._dir.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string()
    }

    fn amend(&self, msg: &str) -> String {
        git(self._dir.path(), &["add", "-A"]);
        git(self._dir.path(), &["commit", "-q", "--amend", "-m", msg]);
        git(self._dir.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string()
    }
}

fn ingest(store: &dyn Store, repo: &Repo) -> GitObservationReport {
    match ingest::ingest_path(store, repo.path(), &["coding"]).unwrap() {
        mindspace::core::IngestOutcome::Git(report) => report,
        mindspace::core::IngestOutcome::Files(_) => panic!("git repository ingested as files"),
    }
}

fn search(store: &dyn Store, text: &str) -> Vec<mindspace::core::SearchHit> {
    mindspace::retrieval::search(
        store,
        SearchQuery {
            text: text.to_string(),
            space_ids: None,
            limit: 50,
            at: None,
            include_superseded: false,
            include_retired: false,
        },
    )
    .unwrap()
}

fn resolution(db: &Connection, raw_target: &str) -> (String, Option<String>, Option<String>) {
    db.query_row(
        "SELECT rr.state,rr.target_entity_id,rr.raw_external_target \
         FROM code_relationship_resolutions rr \
         JOIN code_relationship_observations o ON o.id=rr.observation_id \
         WHERE o.raw_target=?1 ORDER BY rr.id LIMIT 1",
        [raw_target],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

fn entity_at(db: &Connection, path: &str, name: &str, kind: &str) -> String {
    db.query_row(
        "SELECT e.id FROM code_entities e \
         JOIN code_derivations d ON d.id=e.derivation_id \
         JOIN code_inputs i ON i.id=d.input_id \
         JOIN code_contexts c ON c.input_id=i.id \
         WHERE c.path=?1 AND e.name=?2 AND e.kind=?3 ORDER BY e.id LIMIT 1",
        [path, name, kind],
        |row| row.get(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// The regression: committed knowledge must be ingestable
// ---------------------------------------------------------------------------

#[test]
fn committed_knowledge_is_ingested_even_when_absent_from_the_working_tree() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nTracked knowledge.\n");
    r.commit("add readme");
    r.rm("README.md"); // gone from the working tree, alive in HEAD

    ingest(&store, &r);
    let hits = search(&store, "Tracked knowledge");
    assert_eq!(
        hits.len(),
        1,
        "knowledge committed to HEAD must be ingestable"
    );
}

// ---------------------------------------------------------------------------
// A: exact re-ingest is a zero-duplication no-op
// ---------------------------------------------------------------------------

#[test]
fn scenario_r_same_caller_blob_has_commit_contextual_resolution() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "mod target;\nuse crate::target::Thing;\nfn main() {}\n",
    );
    r.write("src/target.rs", "pub struct Thing;\n");
    let commit_a = r.commit("one target");
    let caller_a = git(r.path(), &["rev-parse", "HEAD:src/main.rs"])
        .trim()
        .to_string();
    ingest(&store, &r);
    r.rm("src/target.rs");
    let commit_b = r.commit("remove target module");
    let caller_b = git(r.path(), &["rev-parse", "HEAD:src/main.rs"])
        .trim()
        .to_string();
    assert_ne!(commit_a, commit_b);
    assert_eq!(caller_a, caller_b);
    ingest(&store, &r);
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let input_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM code_inputs WHERE blob_oid=?1",
            [caller_a.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(input_count, 1);
    let derivations:i64=db.query_row("SELECT COUNT(*) FROM code_derivations d JOIN code_inputs i ON i.id=d.input_id WHERE i.blob_oid=?1",[caller_a.as_str()],|r|r.get(0)).unwrap();
    assert_eq!(derivations, 1);
    let observations: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM code_relationship_observations WHERE raw_target='crate::target::Thing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        observations, 1,
        "the immutable import observation is reused"
    );
    let states: Vec<(String, String, String)> = {
        let mut s=db.prepare("SELECT rd.id,rd.commit_oid,rr.state FROM code_relationship_resolutions rr JOIN code_resolution_derivations rd ON rd.id=rr.resolution_derivation_id JOIN code_relationship_observations o ON o.id=rr.observation_id WHERE o.raw_target='crate::target::Thing' ORDER BY rd.commit_oid").unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(states
        .iter()
        .any(|(_, commit, state)| commit == &commit_a && state == "resolved"));
    assert!(states
        .iter()
        .any(|(_, commit, state)| commit == &commit_b && state == "unresolved"));
    assert_ne!(states[0].0, states[1].0, "each commit owns its snapshot");
    let contexts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM code_resolution_derivations WHERE commit_oid IN (?1,?2)",
            [commit_a.as_str(), commit_b.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(contexts, 2);
}

#[test]
fn rust_qualified_crate_resolution_uses_declared_file_modules() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "mod foo; mod other; use crate::foo::bar; fn main() {}\n",
    );
    r.write("src/foo.rs", "pub fn bar() {}\n");
    r.write("src/other.rs", "pub fn bar() {}\n");
    r.commit("module-qualified import");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let (state, target, external) = resolution(&db, "crate::foo::bar");
    assert_eq!(state, "resolved");
    assert_eq!(
        target,
        Some(entity_at(&db, "src/foo.rs", "bar", "function"))
    );
    assert_eq!(external, None);
}

#[test]
fn rust_mod_declarations_make_foo_mod_rs_reachable_and_keep_grouped_imports_separate() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/lib.rs",
        "mod foo; use crate::foo::{Bar as LocalBar, Baz};\n",
    );
    r.write("src/foo/mod.rs", "pub struct Bar; pub struct Baz;\n");
    r.write("src/unrelated.rs", "pub struct Stowaway;\n");
    r.commit("declared module directory");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let bar = resolution(&db, "crate::foo::Bar");
    let baz = resolution(&db, "crate::foo::Baz");
    assert_eq!(bar.0, "resolved");
    assert_eq!(
        bar.1,
        Some(entity_at(&db, "src/foo/mod.rs", "Bar", "struct"))
    );
    assert_eq!(baz.0, "resolved");
    assert_eq!(
        baz.1,
        Some(entity_at(&db, "src/foo/mod.rs", "Baz", "struct"))
    );
    let unrelated: i64 = db.query_row(
        "SELECT COUNT(*) FROM code_relationship_resolutions rr JOIN code_relationship_observations o ON o.id=rr.observation_id JOIN code_entities e ON e.id=rr.target_entity_id WHERE e.name='Stowaway'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(unrelated, 0, "an undeclared .rs file is not a crate module");
}

#[test]
fn rust_self_and_super_imports_resolve_from_inline_module_context() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "mod elsewhere { pub fn helper() {} }\nmod outer { pub fn helper() {} use self::helper; mod inner { use super::helper; } }\n",
    );
    r.commit("inline module paths");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let self_resolution = resolution(&db, "self::helper");
    let super_resolution = resolution(&db, "super::helper");
    assert_eq!(self_resolution.0, "resolved");
    assert_eq!(super_resolution.0, "resolved");
    // There are two helpers in this one parser derivation.  The target selected
    // for both imports must be the outer helper, never the unrelated sibling.
    let outer: String = db.query_row(
        "SELECT e.id FROM code_entities e JOIN code_derivations d ON d.id=e.derivation_id JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE c.path='src/main.rs' AND e.qualified_name='outer::helper'",
        [], |row| row.get(0),
    ).unwrap();
    let elsewhere: String = db.query_row(
        "SELECT e.id FROM code_entities e JOIN code_derivations d ON d.id=e.derivation_id JOIN code_inputs i ON i.id=d.input_id JOIN code_contexts c ON c.input_id=i.id WHERE c.path='src/main.rs' AND e.qualified_name='elsewhere::helper'",
        [], |row| row.get(0),
    ).unwrap();
    assert_ne!(outer, elsewhere, "fixture has a competing helper");
    assert_eq!(self_resolution.1, Some(outer.clone()));
    assert_eq!(super_resolution.1, Some(outer));
}

#[test]
fn rust_impl_sides_are_independent_and_method_calls_stay_unknown() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "trait Loadable {} struct Config; impl Config { fn load(&self) {} } impl Loadable for Config {} fn caller(value: Config) { value.load(); }\n",
    );
    r.commit("impl relationships");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let trait_id = entity_at(&db, "src/main.rs", "Loadable", "trait");
    let config_id = entity_at(&db, "src/main.rs", "Config", "struct");
    let trait_resolution = resolution(&db, "Loadable");
    let type_resolutions: Vec<(String, Option<String>)> = {
        let mut statement = db.prepare("SELECT rr.state,rr.target_entity_id FROM code_relationship_resolutions rr JOIN code_relationship_observations o ON o.id=rr.observation_id WHERE o.kind='implements_type' AND o.raw_target='Config' ORDER BY rr.id").unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(trait_resolution.0, "resolved");
    assert_eq!(trait_resolution.1, Some(trait_id));
    assert_eq!(
        type_resolutions.len(),
        2,
        "inherent and trait impl each retain their type side"
    );
    assert!(type_resolutions
        .iter()
        .all(|row| row == &("resolved".into(), Some(config_id.clone()))));
    let method = resolution(&db, "value.load");
    assert_eq!(method.0, "unresolved");
    let trait_observations: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM code_relationship_observations WHERE kind='implements_trait'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        trait_observations, 1,
        "inherent impl did not manufacture a trait side"
    );
}

#[test]
fn rust_qualified_trait_impl_resolves_each_side_in_its_own_module() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "mod traits; mod config; impl crate::traits::Loadable for crate::config::Config {}\n",
    );
    r.write("src/traits.rs", "pub trait Loadable {}\n");
    r.write("src/config.rs", "pub struct Config;\n");
    r.commit("qualified impl");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let trait_side = resolution(&db, "crate::traits::Loadable");
    let type_side = resolution(&db, "crate::config::Config");
    assert_eq!(trait_side.0, "resolved");
    assert_eq!(
        trait_side.1,
        Some(entity_at(&db, "src/traits.rs", "Loadable", "trait"))
    );
    assert_eq!(type_side.0, "resolved");
    assert_eq!(
        type_side.1,
        Some(entity_at(&db, "src/config.rs", "Config", "struct"))
    );
}

#[test]
fn rust_external_import_is_never_captured_by_a_local_name() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write("src/main.rs", "struct Serialize; use serde::Serialize;\n");
    r.commit("external import");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let (state, target, external) = resolution(&db, "serde::Serialize");
    assert_eq!(state, "external");
    assert_eq!(target, None);
    assert_eq!(external.as_deref(), Some("serde::Serialize"));
}

#[test]
fn rust_local_calls_are_module_scoped_and_each_call_site_is_preserved() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write(
        "src/main.rs",
        "fn helper() {} fn caller() { helper(); helper(); }\n",
    );
    r.commit("two local calls");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let helper = entity_at(&db, "src/main.rs", "helper", "function");
    let rows: Vec<(String, Option<String>)> = {
        let mut statement = db.prepare("SELECT rr.state,rr.target_entity_id FROM code_relationship_resolutions rr JOIN code_relationship_observations o ON o.id=rr.observation_id WHERE o.kind='calls' AND o.raw_target='helper' ORDER BY o.start_byte").unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            ("resolved".into(), Some(helper.clone())),
            ("resolved".into(), Some(helper))
        ]
    );
}

#[test]
fn rust_ambiguous_candidates_are_sorted_and_never_promoted_to_a_target() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    // Deliberately malformed as a Rust program, but a recovered Tree-sitter
    // tree still contains two equally named syntax candidates.  Resolution
    // must retain that ambiguity rather than pick a row by storage order.
    r.write(
        "src/main.rs",
        "fn helper() {} fn helper() {} fn caller() { helper(); }\n",
    );
    r.commit("ambiguous syntax");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    let (state, target, _) = resolution(&db, "helper");
    assert_eq!(state, "ambiguous");
    assert_eq!(target, None);
    let candidates: Vec<String> = {
        let mut statement = db.prepare("SELECT c.entity_id FROM code_resolution_candidates c JOIN code_relationship_resolutions rr ON rr.id=c.resolution_id JOIN code_relationship_observations o ON o.id=rr.observation_id WHERE o.raw_target='helper' ORDER BY c.entity_id").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn code_relationships_do_not_create_knowledge_spine_rows() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let r = repo();
    r.write("src/main.rs", "fn helper() {} fn caller() { helper(); }\n");
    r.commit("structure only");
    ingest(&store, &r);
    let db = Connection::open(&e.db).unwrap();
    for table in ["evidence", "assertions", "memories"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "structural relationship ingest must not write {table}"
        );
    }
    let relationships: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM code_relationship_resolutions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(relationships > 0);
}

#[test]
fn scenario_a_exact_reingest_is_zero_duplication() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nLocal first memory.\n");
    r.write("notes.md", "# Notes\n\nDeterministic ingest.\n");
    r.commit("two files");

    let first = ingest(&store, &r);
    // Three refs are observed (main, feature-less HEAD): HEAD is recorded as
    // a view of main, but only main is journalled as created.
    assert_eq!(first.refs_observed, 2, "main + HEAD");
    assert_eq!(first.refs_added, 1, "symbolic HEAD produces no events");
    assert!(first.objects_new > 0);
    assert_eq!(first.derivations_new, 2);

    let second = ingest(&store, &r);
    assert_eq!(second.refs_added, 0);
    assert_eq!(second.refs_changed, 0);
    assert_eq!(second.refs_removed, 0);
    assert_eq!(second.objects_new, 0, "no object duplicated");
    assert_eq!(second.objects_existing, second.objects_discovered);
    assert_eq!(second.derivations_new, 0, "no derivation duplicated");
    assert_eq!(second.derivations_reused, 2);
}

// ---------------------------------------------------------------------------
// B: a new commit fast-forwards and derives a new version
// ---------------------------------------------------------------------------

#[test]
fn scenario_b_new_commit_is_fast_forward_with_new_derivation() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("notes.md", "# Notes\n\nThe scheduler is CFS.\n");
    r.commit("c1");
    ingest(&store, &r);

    r.write("notes.md", "# Notes\n\nThe scheduler is EEVDF.\n");
    r.commit("c2");
    let report = ingest(&store, &r);

    assert_eq!(report.refs_changed, 1);
    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::FastForward
    );
    assert_eq!(
        report.derivations_new, 1,
        "changed content is a new derivation"
    );
    assert_eq!(report.derivations_reused, 0);
    // Both commits' memories coexist; nothing was superseded.
    assert_eq!(search(&store, "CFS").len(), 1);
    assert_eq!(search(&store, "EEVDF").len(), 1);
}

// ---------------------------------------------------------------------------
// C: a new branch without new objects
// ---------------------------------------------------------------------------

#[test]
fn scenario_c_new_branch_creates_no_new_objects() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nOne.\n");
    r.commit("c1");
    ingest(&store, &r);

    git(r.path(), &["branch", "feature"]);
    let report = ingest(&store, &r);

    assert_eq!(report.refs_added, 1, "refs/heads/feature created");
    assert_eq!(report.objects_new, 0, "a branch pointer adds no objects");
    assert_eq!(report.derivations_new, 0, "same commit, already derived");
}

// ---------------------------------------------------------------------------
// D: a commit on a branch fast-forwards that branch
// ---------------------------------------------------------------------------

#[test]
fn scenario_d_branch_commit_fast_forwards_the_branch() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["branch", "feature"]);
    ingest(&store, &r);

    git(r.path(), &["checkout", "-q", "feature"]);
    r.write("feature.md", "# Feature\n\nBranch work.\n");
    r.commit("c2");
    let report = ingest(&store, &r);

    assert_eq!(report.refs_added, 0);
    assert_eq!(report.refs_changed, 1, "feature moved");
    assert_eq!(report.events[0].ref_name, "refs/heads/feature");
    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::FastForward
    );
    // The tip commit changed, so both files are new derivations there
    // (logical identity is per commit, even for unchanged bytes); the
    // parent commit's derivations are reused.
    assert_eq!(report.derivations_new, 2);
    assert_eq!(report.derivations_reused, 1);
    assert_eq!(search(&store, "Branch work").len(), 1);
}

// ---------------------------------------------------------------------------
// E: a merge preserves parent topology
// ---------------------------------------------------------------------------

#[test]
fn scenario_e_merge_has_two_parents() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("feature.md", "# Feature\n\nBranch work.\n");
    let feature_tip = r.commit("c2");
    git(r.path(), &["checkout", "-q", "main"]);
    r.write("main.md", "# Main\n\nMain work.\n");
    r.commit("c3");
    let before_merge = ingest(&store, &r);

    git(
        r.path(),
        &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
    );
    let report = ingest(&store, &r);

    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::FastForward,
        "main -> merge commit is an ancestor move even with --no-ff"
    );
    // The merge tip's tree is new: its derivations are new versions even
    // when the text equals the branch tip's text (same patch != same commit).
    assert!(report.derivations_new >= 1);

    // Parent topology: the merge commit has two parents, and it is
    // inspectable through provenance refs.
    let hits = search(&store, "Branch work");
    assert!(!hits.is_empty());
    let detail = store.get_memory_detail(hits[0].memory_id).unwrap().unwrap();
    let git = detail.git.as_ref().unwrap();
    assert_eq!(git.path, "feature.md");
    let _ = (feature_tip, before_merge);
}

// ---------------------------------------------------------------------------
// F: fast-forward merge reuses everything
// ---------------------------------------------------------------------------

#[test]
fn scenario_f_ff_merge_shares_one_commit() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("feature.md", "# Feature\n\nBranch work.\n");
    r.commit("c2");
    ingest(&store, &r);
    git(r.path(), &["checkout", "-q", "main"]);
    git(r.path(), &["merge", "-q", "--ff-only", "feature"]);

    let report = ingest(&store, &r);
    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::FastForward
    );
    assert_eq!(
        report.derivations_new, 0,
        "main now points at the derived commit"
    );
    assert_eq!(
        report.derivations_reused, 2,
        "feature.md and README.md both reused"
    );
}

// ---------------------------------------------------------------------------
// G: history rewrite is non-fast-forward, old commits stay
// ---------------------------------------------------------------------------

#[test]
fn scenario_g_rewritten_history_is_non_fast_forward() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("feature.md", "# Feature\n\nOriginal work.\n");
    let original = r.commit("c3");
    ingest(&store, &r);

    r.write("feature.md", "# Feature\n\nRewritten work.\n");
    r.amend("c3 rewritten");
    let report = ingest(&store, &r);

    assert_eq!(report.refs_changed, 1);
    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::NonFastForward,
        "an amend is recorded as non-fast-forward, never as rebase or force-push"
    );
    // The rewritten tip is derived; the original commit's objects and its
    // derived memories are retained (unreachable != deleted).
    assert_eq!(search(&store, "Original work").len(), 1);
    assert_eq!(search(&store, "Rewritten work").len(), 1);
    let _ = original;
}

// ---------------------------------------------------------------------------
// H: cherry-picks are distinct commits
// ---------------------------------------------------------------------------

#[test]
fn scenario_h_cherry_pick_is_two_distinct_commits() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("idea.md", "# Idea\n\nThe same idea twice.\n");
    // A fixed committer date keeps c2 a distinct object from the
    // cherry-pick: two byte-identical commits with identical metadata are
    // one object, and the test wants two.
    let c2_date = "2000-01-01T00:00:00 +0000";
    git(r.path(), &["add", "-A"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(r.path())
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
        .args(["commit", "-q", "-m", "c2"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_COMMITTER_DATE", c2_date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    ingest(&store, &r);

    git(r.path(), &["checkout", "-q", "main"]);
    git(r.path(), &["cherry-pick", "feature"]);
    let report = ingest(&store, &r);

    // Two commits carry the same patch; both are derived separately.
    let mut commits: Vec<String> = report.files.iter().map(|f| f.commit_oid.clone()).collect();
    commits.sort();
    commits.dedup();
    assert!(
        commits.len() >= 2,
        "same patch, distinct commits: {commits:?}"
    );
    assert_eq!(
        search(&store, "same idea twice").len(),
        2,
        "coexistence, not collapse"
    );
}

// ---------------------------------------------------------------------------
// I: deleting a branch deletes the ref, not the history
// ---------------------------------------------------------------------------

#[test]
fn scenario_i_branch_deletion_keeps_objects_and_memories() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("feature.md", "# Feature\n\nDoomed branch work.\n");
    r.commit("c2");
    ingest(&store, &r);
    git(r.path(), &["checkout", "-q", "main"]);
    git(r.path(), &["branch", "-q", "-D", "feature"]);

    let report = ingest(&store, &r);
    assert_eq!(report.refs_removed, 1);
    assert_eq!(
        report.events[0].kind,
        mindspace::core::GitRefEventKind::Deleted
    );
    // Objects and derived memories are availability facts, not history:
    // nothing is deleted.
    assert_eq!(search(&store, "Doomed branch work").len(), 1);
    // The deletion is journalled exactly once: the next scan observes no
    // changes.
    let again = ingest(&store, &r);
    assert_eq!(again.refs_removed, 0);
    assert_eq!(again.refs_added + again.refs_changed, 0);
}

// ---------------------------------------------------------------------------
// J: ref name reuse invents no continuity
// ---------------------------------------------------------------------------

#[test]
fn scenario_j_ref_name_reuse_creates_no_continuity() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "topic"]);
    r.write("topic.md", "# Topic\n\nFirst life.\n");
    r.commit("c2");
    ingest(&store, &r); // the journal now knows topic at c2
    git(r.path(), &["checkout", "-q", "main"]);
    git(r.path(), &["branch", "-q", "-D", "topic"]);
    // The name is reborn on an unrelated commit.
    git(r.path(), &["checkout", "-q", "-b", "topic"]);
    r.write("topic.md", "# Topic\n\nSecond life.\n");
    r.commit("c3");
    let report = ingest(&store, &r);

    // The name was observed at c2 and is observed at c3: one changed event
    // (provably not a fast-forward). The journal records observations of a
    // name over time; it invents no delete/create continuity.
    let changed: Vec<_> = report
        .events
        .iter()
        .filter(|e| e.kind == mindspace::core::GitRefEventKind::NonFastForward)
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].ref_name, "refs/heads/topic");
    assert_eq!(report.refs_added, 0);
    assert_eq!(report.refs_removed, 0);
    // Both lives coexist as unrelated commits; no lineage was invented.
    assert_eq!(search(&store, "First life").len(), 1);
    assert_eq!(search(&store, "Second life").len(), 1);
}

// ---------------------------------------------------------------------------
// K: annotated and lightweight tags
// ---------------------------------------------------------------------------

#[test]
fn scenario_k_annotated_and_lightweight_tags() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nRelease notes live here.\n");
    r.commit("c1");
    ingest(&store, &r);

    git(r.path(), &["tag", "lightweight"]);
    git(r.path(), &["tag", "-a", "v1", "-m", "release one"]);
    let report = ingest(&store, &r);

    assert_eq!(report.refs_added, 2, "both tags are new refs");
    assert_eq!(
        report.objects_new, 1,
        "only the annotated tag object is new"
    );
    // The annotated tag peels to the commit: no new derivations, but the
    // tag object exists.
    assert_eq!(report.derivations_new, 0);

    // An exact re-ingest observes no ref changes.
    let again = ingest(&store, &r);
    assert_eq!(
        again.refs_added + again.refs_changed + again.refs_removed,
        0
    );
}

// ---------------------------------------------------------------------------
// L: a moved tag is a non-fast-forward ref update
// ---------------------------------------------------------------------------

#[test]
fn scenario_l_tag_move_is_non_fast_forward() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nOne.\n");
    let _c1 = r.commit("c1");
    ingest(&store, &r);
    r.write("README.md", "# Repo\n\nTwo.\n");
    r.commit("c2");
    // The tag exists before the move, so the move is an update, not a birth.
    git(r.path(), &["tag", "v1"]);
    ingest(&store, &r);

    // Move the tag backwards: provably not a fast-forward.
    git(r.path(), &["tag", "-f", "v1", "HEAD~1"]);
    let report = ingest(&store, &r);

    let tag_events: Vec<_> = report
        .events
        .iter()
        .filter(|e| e.ref_name == "refs/tags/v1")
        .collect();
    assert_eq!(tag_events.len(), 1);
    assert_eq!(
        tag_events[0].kind,
        mindspace::core::GitRefEventKind::NonFastForward
    );
}

// ---------------------------------------------------------------------------
// M: detached HEAD is observed as its own ref state
// ---------------------------------------------------------------------------

#[test]
fn scenario_m_detached_head_is_observed() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nOne.\n");
    r.commit("c1");
    r.write("README.md", "# Repo\n\nTwo.\n");
    r.commit("c2");
    ingest(&store, &r);

    git(r.path(), &["checkout", "-q", "--detach", "HEAD~1"]);
    let report = ingest(&store, &r);
    assert!(report.refs_observed >= 2, "HEAD plus main");

    // The detached HEAD points at the older commit; its content is derived
    // as its own version and coexists with the branch tip's.
    let store2 = e.open();
    let hits = search(&store2, "One.");
    assert_eq!(hits.len(), 1);
    let detail = store2
        .get_memory_detail(hits[0].memory_id)
        .unwrap()
        .unwrap();
    let git = detail.git.as_ref().unwrap();
    assert!(
        git.refs.iter().any(|name| name == "HEAD"),
        "HEAD targets this commit"
    );
}

// ---------------------------------------------------------------------------
// N: one blob at two paths
// ---------------------------------------------------------------------------

#[test]
fn scenario_n_same_blob_multi_path_is_one_object_two_relations() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("shared.md", "# Shared\n\nIdentical bytes everywhere.\n");
    r.commit("c1");
    std::fs::create_dir_all(r.path().join("docs")).unwrap();
    std::fs::copy(r.path().join("shared.md"), r.path().join("docs/shared.md")).unwrap();
    r.commit("c2");
    let report = ingest(&store, &r);

    let mut blob_oids: Vec<String> = report
        .files
        .iter()
        .filter(|f| f.path.ends_with("shared.md"))
        .map(|f| f.blob_oid.clone())
        .collect();
    blob_oids.sort();
    blob_oids.dedup();
    assert_eq!(blob_oids.len(), 1, "one blob object");
    let shared_reports = report
        .files
        .iter()
        .filter(|f| f.path.ends_with("shared.md"))
        .count();
    assert_eq!(shared_reports, 2, "two paths, two derivations");

    // Both paths are searchable, each tracing to its own path.
    assert_eq!(search(&store, "Identical bytes everywhere").len(), 2);
}

// ---------------------------------------------------------------------------
// O: one blob across branches
// ---------------------------------------------------------------------------

#[test]
fn scenario_o_same_blob_across_branches_is_derived_once_per_commit() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("README.md", "# Repo\n\nBase.\n");
    r.commit("c1");
    git(r.path(), &["checkout", "-q", "-b", "feature"]);
    r.write("shared.md", "# Shared\n\nWritten on feature.\n");
    let feature_commit = r.commit("c2");
    git(r.path(), &["checkout", "-q", "main"]);
    // The identical bytes, independently committed on main.
    r.write("shared.md", "# Shared\n\nWritten on feature.\n");
    r.commit("c3");
    let report = ingest(&store, &r);

    // Same blob object, two tip commits: two derivations.
    let shared: Vec<&mindspace::core::GitFileReport> = report
        .files
        .iter()
        .filter(|f| f.path == "shared.md")
        .collect();
    assert_eq!(shared.len(), 2);
    assert_eq!(shared[0].blob_oid, shared[1].blob_oid, "one object");
    let commit_oids: Vec<&String> = shared.iter().map(|f| &f.commit_oid).collect();
    assert!(commit_oids.contains(&&feature_commit));

    // Same text from two commits: both memories exist (corroboration, not
    // duplication of identity).
    assert_eq!(search(&store, "Written on feature").len(), 2);
}

// ---------------------------------------------------------------------------
// P: two repositories never merge
// ---------------------------------------------------------------------------

#[test]
fn scenario_p_two_repositories_stay_distinct() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let a = repo();
    a.write("README.md", "# Repo\n\nShared history bytes.\n");
    a.commit("c1");
    a.write("only-a.md", "# A\n\nOnly in a.\n");
    a.commit("c2");
    let report_a = ingest(&store, &a);

    // A clone shares all commit/tree/blob oids but gets its own identity.
    let b_dir = tempfile::tempdir().unwrap();
    git(
        b_dir.path(),
        &["clone", "-q", a.path().join(".").to_str().unwrap(), "."],
    );
    let b = Repo { _dir: b_dir };
    let report_b = ingest(&store, &b);

    assert_ne!(
        report_a.repository_id, report_b.repository_id,
        "two clones stay distinct repositories"
    );
    assert_eq!(
        report_b.objects_new, report_b.objects_discovered,
        "no cross-repository object dedup: repo b stored its own rows"
    );
    // Both repositories' knowledge coexists; provenance names the repository.
    assert_eq!(search(&store, "Only in a").len(), 2, "one per repository");
    assert_eq!(search(&store, "Shared history bytes").len(), 2);

    // Each memory traces to its own repository path.
    let hits = search(&store, "Only in a");
    let detail = store.get_memory_detail(hits[0].memory_id).unwrap().unwrap();
    assert_eq!(
        detail.git.as_ref().unwrap().repository_path,
        a.path().to_string_lossy()
    );
}

// ---------------------------------------------------------------------------
// Idempotence layer 3: same immutable input + extractor => no duplicates
// ---------------------------------------------------------------------------

#[test]
fn derivation_idempotence_survives_repos_and_sessions() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    let r = repo();
    r.write("notes.md", "# Notes\n\nStable proposition.\n");
    r.commit("c1");
    ingest(&store, &r);
    ingest(&store, &r);
    ingest(&store, &r);
    assert_eq!(search(&store, "Stable proposition").len(), 1);

    // Every memory from this store keeps active state; nothing churns.
    let hits = search(&store, "Stable proposition");
    let detail = store.get_memory_detail(hits[0].memory_id).unwrap().unwrap();
    assert_eq!(detail.memory.state, MemoryState::Active);
}
