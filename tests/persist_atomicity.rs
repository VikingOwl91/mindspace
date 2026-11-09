//! M7.3b.4 integrity gate: one logical ingestion commits entirely or not at all.
//!
//! `persist_files` used to be two transactions: the observation (files, sources, sessions...) and,
//! after it had COMMITTED, the code derivations. A failure or a crash between them left the
//! observation without its derivations. These tests inject a real SQLite failure (a `RAISE(ABORT)`
//! trigger installed through a second connection) at that boundary and compare the CONTENTS of
//! every table, not only the returned error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mindspace::core::Uuid;
use mindspace::ingest::confined::{extract_directory, Anchor};
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, Extracted, StoreContext};
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

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
    fn space(&self) -> Uuid {
        self.store().get_space("p").unwrap().id
    }
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    /// The full content of every user table, as sorted text rows: what the database CONTAINS.
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
            let cols = c
                .prepare(&format!("PRAGMA table_info(\"{t}\")"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .flatten()
                .collect::<Vec<_>>();
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
    fn count(&self, table: &str) -> i64 {
        self.raw()
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
                r.get(0)
            })
            .unwrap()
    }
}

fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, McpConfig) {
    let d = tempfile::tempdir().unwrap();
    for (n, c) in files {
        let p = d.path().join(n);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, c).unwrap();
    }
    let root = std::fs::canonicalize(d.path()).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    (d, root, cfg)
}

fn extracted(db: &Db, root: &Path, cfg: &McpConfig) -> Extracted {
    let store = db.store();
    let anchor = Anchor::open(root, Some(root), cfg, false).unwrap();
    Extracted::Files(
        extract_directory(
            &StoreContext(&store),
            &anchor,
            cfg,
            &[db.space()],
            &Deadline::none(),
        )
        .unwrap(),
    )
}

fn sample() -> (tempfile::TempDir, PathBuf, McpConfig) {
    tree(&[
        ("docs/guide.md", "# Guide\n\nalpha\n"),
        ("src/lib.rs", "pub fn alpha() {}\npub struct Beta;\n"),
        (
            "src/more.rs",
            "use crate::alpha;\nfn gamma() { alpha(); }\n",
        ),
    ])
}

fn inject(db: &Db, table: &str) {
    db.raw()
        .execute_batch(&format!(
            "CREATE TRIGGER fault_injection BEFORE INSERT ON {table} \
             BEGIN SELECT RAISE(ABORT, 'injected fault at {table}'); END;"
        ))
        .unwrap();
}

fn clear(db: &Db) {
    db.raw()
        .execute_batch("DROP TRIGGER fault_injection")
        .unwrap();
}

// -----------------------------------------------------------------------------------------

#[test]
fn a_failure_right_after_the_observation_write_leaves_the_database_exactly_as_it_was() {
    let (_d, root, cfg) = sample();
    let db = db();
    let ex = extracted(&db, &root, &cfg);
    let before = db.dump();
    // the first row the SECOND write inserts: this is the boundary between the two writes
    inject(&db, "code_inputs");
    let err = persist(&db.store(), ex, &Deadline::none()).unwrap_err();
    assert!(
        err.to_string().contains("injected fault at code_inputs"),
        "{err}"
    );
    let after = db.dump();
    // contents, table by table (a diff names the leaked table)
    for (table, rows) in &before {
        assert_eq!(
            after.get(table).unwrap(),
            rows,
            "table `{table}` changed although the ingestion failed: the observation was committed without its derivations"
        );
    }
    assert_eq!(before, after);
}

#[test]
fn a_late_failure_inside_the_derivation_write_leaves_the_database_exactly_as_it_was() {
    let (_d, root, cfg) = sample();
    let db = db();
    let ex = extracted(&db, &root, &cfg);
    let before = db.dump();
    // after inputs, derivations and the first entities were written
    inject(&db, "code_relationship_observations");
    let err = persist(&db.store(), ex, &Deadline::none()).unwrap_err();
    assert!(err.to_string().contains("injected fault"), "{err}");
    assert_eq!(before, db.dump(), "partial state after a late failure");
    // and every other table individually, for a readable failure
    for t in [
        "sources",
        "code_inputs",
        "code_derivations",
        "code_entities",
    ] {
        assert_eq!(db.count(t), 0, "{t}");
    }
}

#[test]
fn a_retry_after_a_failure_equals_one_clean_ingestion_and_costs_one_revision() {
    let (_d, root, cfg) = sample();
    // reference: one clean ingestion
    let clean = db();
    let ex = extracted(&clean, &root, &cfg);
    let r0 = clean.count("m6_revisions");
    persist(&clean.store(), ex, &Deadline::none()).unwrap();
    let revisions_for_one_ingestion = clean.count("m6_revisions") - r0;
    // a failed attempt followed by a successful one
    let db = db();
    let r1 = db.count("m6_revisions");
    inject(&db, "code_derivations");
    assert!(persist(&db.store(), extracted(&db, &root, &cfg), &Deadline::none()).is_err());
    assert_eq!(
        db.count("m6_revisions"),
        r1,
        "a failed ingestion consumed a revision"
    );
    clear(&db);
    persist(&db.store(), extracted(&db, &root, &cfg), &Deadline::none()).unwrap();
    for t in [
        "sources",
        "code_inputs",
        "code_derivations",
        "code_entities",
        "code_contexts",
    ] {
        assert_eq!(db.count(t), clean.count(t), "{t}");
    }
    assert_eq!(db.count("m6_revisions") - r1, revisions_for_one_ingestion);
    // the logical operation is ONE recording revision
    assert_eq!(
        revisions_for_one_ingestion, 1,
        "one logical ingestion, one revision"
    );
}

#[test]
fn a_document_only_ingestion_still_commits_without_code() {
    let (_d, root, cfg) = tree(&[("a.md", "# A\n\nonly text\n")]);
    let db = db();
    let ex = extracted(&db, &root, &cfg);
    persist(&db.store(), ex, &Deadline::none()).unwrap();
    assert!(db.count("sources") > 0);
    assert_eq!(db.count("code_derivations"), 0);
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
fn ingesting_the_same_directory_again_adds_no_structure_and_no_duplicate_search_hits() {
    // A retry after an uncertain commit outcome is a re-ingestion. It used to add one identical
    // `code_contexts` row per file each time (a NULL commit id defeats INSERT OR IGNORE), and
    // `code_search` then returned every hit once per ingestion.
    let (_d, root, cfg) = sample();
    let db = db();
    persist(&db.store(), extracted(&db, &root, &cfg), &Deadline::none()).unwrap();
    let structure = [
        "code_inputs",
        "code_derivations",
        "code_entities",
        "code_contexts",
        "sources",
    ]
    .map(|t| db.count(t));
    let hits = alpha_hits(&db);
    assert!(hits > 0);
    for _ in 0..3 {
        persist(&db.store(), extracted(&db, &root, &cfg), &Deadline::none()).unwrap();
    }
    let again = [
        "code_inputs",
        "code_derivations",
        "code_entities",
        "code_contexts",
        "sources",
    ]
    .map(|t| db.count(t));
    assert_eq!(structure, again, "re-ingestion grew the derived structure");
    assert_eq!(hits, alpha_hits(&db), "re-ingestion duplicated search hits");
}
