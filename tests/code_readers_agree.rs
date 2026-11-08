//! M7.3b gate: the readers of code relationships, symbols and `structure` agree on the effective
//! derivation of an input, also when two derivations share a timestamp, and a newer policy skip
//! hides the older relationships. The tie needs rows the store API cannot write (it stamps each
//! derivation with the clock), so this test inserts them through a second SQLite connection with a
//! stub for the application's revision function (test-only access; production SQL is unchanged).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use mindspace::core::{CodeSearchQuery, IngestOutcome, Uuid};
use mindspace::ingest::confined::Anchor;
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist, StoreContext};
use mindspace::ingest::outline::PathSelection;
use mindspace::ingest::stream::{
    consume, stream_confined_git, FrameWriter, Kind, ProducerLimits, StreamLimits,
};
use mindspace::ingest::GitScope;
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};

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

struct World {
    _repo: tempfile::TempDir,
    _dbdir: tempfile::TempDir,
    db: PathBuf,
    repository_id: Uuid,
    commit: String,
}

fn world() -> World {
    let repo = tempfile::tempdir().unwrap();
    let r = repo.path();
    git(r, &["init", "-q", "-b", "main", "."]);
    write(r, "src/lib.rs", "mod a;\nmod b;\n");
    write(
        r,
        "src/a.rs",
        "pub fn caller() { crate::b::target_fn(); }\n",
    );
    write(r, "src/b.rs", "pub fn target_fn() {}\n");
    git(r, &["add", "."]);
    git(r, &["commit", "-q", "-m", "one"]);
    let dbdir = tempfile::tempdir().unwrap();
    let db = dbdir.path().join("mind.db");
    initialize(&db).unwrap();
    let store = open_existing(&db).unwrap();
    store.create_space("p", "").unwrap();
    let root = std::fs::canonicalize(r).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    let anchor = Anchor::open(&root, Some(&root), &cfg, true).unwrap();
    let op = Uuid::now_v7();
    let mut out = Vec::new();
    {
        let mut w = FrameWriter::new(&mut out, op, None);
        stream_confined_git(
            &StoreContext(&store),
            &anchor,
            &cfg,
            &PathSelection::all(),
            GitScope::Head,
            &["p"],
            &Deadline::none(),
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.finish().unwrap();
    }
    let (ex, _) = consume(
        &out[..],
        op,
        Kind::Git,
        None,
        &StreamLimits::default(),
        None,
    )
    .unwrap();
    let IngestOutcome::Git(report) = persist(&store, ex, &Deadline::none()).unwrap() else {
        panic!()
    };
    let commit: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT commit_oid FROM code_contexts WHERE path='src/a.rs' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    World {
        _repo: repo,
        _dbdir: dbdir,
        db,
        repository_id: report.repository_id,
        commit,
    }
}

/// A writable connection that stands in for the application's revision function.
fn raw(db: &Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(db).unwrap();
    let rev: i64 = c
        .query_row(
            "SELECT COALESCE(MAX(revision_id), 1) FROM m6_row_availability",
            [],
            |r| r.get(0),
        )
        .unwrap();
    c.create_scalar_function(
        "m6_write_revision",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_| Ok(rev),
    )
    .unwrap();
    c
}

struct Row<'a> {
    id: &'a str,
    version: &'a str,
    status: &'a str,
    at: i64,
    entity: Option<&'a str>,
    call: Option<&'a str>,
}

fn add(c: &rusqlite::Connection, input: &str, r: &Row) {
    c.execute(
        "INSERT INTO code_derivations(id,input_id,language,extractor,extractor_version,grammar,grammar_version,status,error_count,created_at) VALUES(?1,?2,'rust','mind-code',?3,'tree-sitter-rust','0.24.2',?4,0,?5)",
        rusqlite::params![r.id, input, r.version, r.status, r.at],
    )
    .unwrap();
    if let Some(name) = r.entity {
        c.execute(
            "INSERT INTO code_entities(id,derivation_id,kind,name,qualified_name,syntax_path,parent_syntax_path,start_byte,end_byte,start_line,start_column,end_line,end_column,signature,visibility,is_async) VALUES(?1,?2,'function',?3,?3,?4,NULL,0,5,1,1,1,6,NULL,NULL,0)",
            rusqlite::params![format!("1{}", &r.id[1..]), r.id, name, format!("function:{name}@0")],
        )
        .unwrap();
    }
    if let Some(target) = r.call {
        c.execute(
            "INSERT INTO code_relationship_observations(id,derivation_id,kind,source_syntax_path,raw_target,start_byte,end_byte) VALUES(?1,?2,'calls',NULL,?3,0,5)",
            rusqlite::params![format!("o-{}", r.id), r.id, target],
        )
        .unwrap();
    }
}

fn symbols(store: &SqliteStore, text: &str) -> Vec<String> {
    store
        .search_code(&CodeSearchQuery {
            text: text.into(),
            kinds: vec![],
            repository: None,
            path_prefix: None,
            space_ids: None,
            limit: 100,
            all_versions: false,
        })
        .unwrap()
        .into_iter()
        .filter_map(|h| h.name)
        .collect()
}

/// Observation ids of the derivations of `input` that a resolution run resolved.
fn resolved_observations(c: &rusqlite::Connection, resolver: &str, input: &str) -> Vec<String> {
    c.prepare("SELECT o.id FROM code_relationship_resolutions r JOIN code_resolution_derivations rd ON rd.id=r.resolution_derivation_id JOIN code_relationship_observations o ON o.id=r.observation_id JOIN code_derivations d ON d.id=o.derivation_id WHERE rd.resolver=?1 AND d.input_id=?2 ORDER BY o.id")
        .unwrap()
        .query_map(rusqlite::params![resolver, input], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect()
}

fn effective_ids(store: &SqliteStore) -> Vec<String> {
    store
        .code_structure("src/a.rs")
        .unwrap()
        .into_iter()
        .filter(|d| d.effective)
        .map(|d| d.id.to_string())
        .collect()
}

#[test]
fn a_tie_has_one_effective_derivation_and_a_newer_skip_hides_older_relationships() {
    let w = world();
    let c = raw(&w.db);
    let input: String = c.query_row("SELECT i.id FROM code_inputs i JOIN code_contexts c ON c.input_id=i.id WHERE c.path='src/a.rs'", [], |r| r.get(0)).unwrap();
    let (d0, t0): (String, i64) = c
        .query_row(
            "SELECT id, created_at FROM code_derivations WHERE input_id=?1",
            [&input],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let store = open_existing(&w.db).unwrap();

    // baseline: the normal derivation is effective, its call was resolved by the ingest
    assert_eq!(effective_ids(&store), vec![d0.clone()]);
    assert!(
        !resolved_observations(&c, "mind-rust-resolver", &input).is_empty(),
        "baseline resolution has a.rs's call"
    );
    assert_eq!(symbols(&store, "caller"), vec!["caller".to_string()]);

    // 1. two derivations with the same timestamp; the greater id is a policy skip
    let at = t0 + 1_000;
    let (x, y) = (
        "00000000-0000-7000-8000-000000000001",
        "00000000-0000-7000-8000-000000000002",
    );
    add(
        &c,
        &input,
        &Row {
            id: x,
            version: "9",
            status: "complete",
            at,
            entity: Some("tie_loser"),
            call: Some("crate::b::tie_loser_target"),
        },
    );
    add(
        &c,
        &input,
        &Row {
            id: y,
            version: "3+skip(p1:source_bytes>1)",
            status: "failed",
            at,
            entity: None,
            call: None,
        },
    );
    // 2. exactly one effective derivation: the greater id
    assert_eq!(effective_ids(&store), vec![y.to_string()]);
    let all = store.code_structure("src/a.rs").unwrap();
    assert_eq!(all.iter().filter(|d| d.effective).count(), 1);
    assert_eq!(all.len(), 3, "history stays visible");
    let skip = all.iter().find(|d| d.effective).unwrap();
    assert_eq!(skip.skip_reason.as_deref(), Some("p1:source_bytes>1"));
    // 3 and 4. every reader ignores what the skip hides
    assert!(
        symbols(&store, "tie_loser").is_empty(),
        "entity of the tie loser"
    );
    assert!(
        symbols(&store, "caller").is_empty(),
        "entity of the older derivation"
    );
    assert_eq!(
        symbols(&store, "target_fn"),
        vec!["target_fn".to_string()],
        "other files are unaffected"
    );
    store
        .resolve_git_code_relationships(w.repository_id, &w.commit, "gate", "1")
        .unwrap();
    assert!(
        resolved_observations(&c, "gate", &input).is_empty(),
        "relationships of hidden derivations are not observed"
    );

    // 5. a later tie whose greater id carries relationships: that derivation is effective everywhere
    let at2 = t0 + 2_000;
    let (z, v) = (
        "00000000-0000-7000-8000-000000000003",
        "00000000-0000-7000-8000-000000000004",
    );
    add(
        &c,
        &input,
        &Row {
            id: z,
            version: "3+skip(p1:source_bytes>2)",
            status: "failed",
            at: at2,
            entity: None,
            call: None,
        },
    );
    add(
        &c,
        &input,
        &Row {
            id: v,
            version: "10",
            status: "complete",
            at: at2,
            entity: Some("tie_winner"),
            call: Some("crate::b::target_fn"),
        },
    );
    assert_eq!(effective_ids(&store), vec![v.to_string()]);
    assert_eq!(
        symbols(&store, "tie_winner"),
        vec!["tie_winner".to_string()]
    );
    assert!(symbols(&store, "tie_loser").is_empty());
    store
        .resolve_git_code_relationships(w.repository_id, &w.commit, "gate", "2")
        .unwrap();
    let seen = resolved_observations(&c, "gate", &input);
    assert_eq!(
        seen,
        vec![format!("o-{v}")],
        "only the effective derivation's relationship is resolved"
    );
    // the structure reader and the symbol reader name the same derivation
    let eff = store
        .code_structure("src/a.rs")
        .unwrap()
        .into_iter()
        .find(|d| d.effective)
        .unwrap();
    assert_eq!(eff.id.to_string(), v);
    assert_eq!(
        eff.entities
            .iter()
            .filter_map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec!["tie_winner".to_string()]
    );
}
