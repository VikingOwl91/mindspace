//! Integration tests for the domain invariants. Tests run against temporary
//! SQLite databases and reopen them between phases to exercise persistence
//! across process boundaries, exactly as a user would experience.

use std::path::PathBuf;

use mindspace::core::{
    AssertionDraft, AssertionKind, EvidenceDraft, EvidenceKind, FileOutcome, IngestObservation,
    IngestScope, MemoryActionKind, MemorySpec, MemoryState, ObservedFile, SearchHit, SearchQuery,
    SessionDraft, SourceKind, Ts,
};
use mindspace::ingest;
use mindspace::retrieval;
use mindspace::storage::Store;
use mindspace::SqliteStore;
use mindspace::{initialize, open_existing, resolve_memory_id};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        open_existing(&self.db).expect("open existing db")
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self._dir.path().join(name);
        std::fs::write(&path, content).expect("write file");
        path
    }
}

fn search(store: &dyn Store, text: &str, space: Option<&str>) -> Vec<SearchHit> {
    let space_ids = space.map(|name| vec![store.get_space(name).expect("space").id]);
    retrieval::search(
        store,
        SearchQuery {
            text: text.to_string(),
            space_ids,
            limit: 20,
            at: None,
            include_superseded: false,
            include_retired: false,
        },
    )
    .expect("search")
}

fn one_hit(hits: &[SearchHit]) -> SearchHit {
    assert_eq!(hits.len(), 1, "expected exactly one hit: {hits:?}");
    hits[0].clone()
}

/// A small markdown file with two sections.
const TWO_SECTIONS_V1: &str = "\
# Notes

Intro line.

## Storage

Everything lives in SQLite.

## Parsing

Parser is deterministic.
";

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

#[test]
fn provenance_chain_from_memory_to_source() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    let hit = one_hit(&search(&store, "SQLite", Some("coding")));
    let detail = store.get_memory_detail(hit.memory_id).unwrap().unwrap();

    // memory -> assertion -> evidence -> source, all linked.
    assert_eq!(detail.assertion.subject, "Storage");
    assert_eq!(detail.evidence.id, detail.assertion.evidence_id);
    assert_eq!(detail.source.id, detail.evidence.source_id);
    assert_eq!(
        detail.evidence.heading_path.as_deref(),
        Some("Notes > Storage")
    );
    assert_eq!(detail.evidence.content, "Everything lives in SQLite.");
    assert_eq!(detail.source.kind, SourceKind::File);
    assert!(detail.source.location.ends_with("notes.md"));
    assert_eq!(detail.source.content_hash.len(), 64);
    // The evidence content hash identifies exactly the asserted content.
    assert_eq!(
        detail.evidence.content_hash,
        mindspace::core::sha256_hex(detail.evidence.content.as_bytes())
    );
    assert_eq!(detail.spaces.len(), 1);
    assert_eq!(detail.spaces[0].name, "coding");
}

// ---------------------------------------------------------------------------
// Idempotent ingest
// ---------------------------------------------------------------------------

#[test]
fn reingest_same_content_changes_nothing() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);

    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    assert!(matches!(second.files[0].outcome, FileOutcome::Unchanged));
    assert_eq!(first.files[0].logical_id, second.files[0].logical_id);
    for action in &second.files[0].actions {
        assert_eq!(action.action, MemoryActionKind::Unchanged, "{action:?}");
    }
    // No new memories: same count of subjects as the first ingest.
    for subject in ["storage", "parsing", "notes"] {
        assert_eq!(
            store.list_memories_by_subject(subject).unwrap().len(),
            1,
            "subject {subject} duplicated"
        );
    }
}

#[test]
fn reingest_completes_requested_spaces() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    store.create_space("linux", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);

    ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let second = ingest::ingest_file(&store, &path, &["coding", "linux"]).unwrap();

    assert!(second.files[0].actions.iter().all(
        |a| matches!(&a.action, MemoryActionKind::Reused { spaces_added }
            if spaces_added == &vec!["linux".to_string()])
    ));
    for subject in ["storage", "parsing", "notes"] {
        let memories = store.list_memories_by_subject(subject).unwrap();
        assert_eq!(memories.len(), 1);
        let mut spaces = memories[0].spaces.clone();
        spaces.sort();
        assert_eq!(spaces, vec!["coding", "linux"]);
    }
}

#[test]
fn memory_lives_in_multiple_spaces_without_duplication() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    store.create_space("rust", "").unwrap();
    let path = e.write(
        "notes.md",
        "# Ownership\n\nBorrowing rules are checked at compile time.\n",
    );
    ingest::ingest_file(&store, &path, &["coding", "rust"]).unwrap();

    let memories = store.list_memories_by_subject("ownership").unwrap();
    assert_eq!(memories.len(), 1, "multi-space memory must not duplicate");
    assert_eq!(memories[0].spaces.len(), 2);

    assert_eq!(search(&store, "Borrowing", Some("coding")).len(), 1);
    assert_eq!(search(&store, "Borrowing", Some("rust")).len(), 1);
}

// ---------------------------------------------------------------------------
// Directed space links
// ---------------------------------------------------------------------------

fn linked_env() -> (Env, SqliteStore) {
    let e = env();
    let store = e.open();
    for name in ["project:owlry", "coding", "linux"] {
        store.create_space(name, "").unwrap();
    }
    let owlry = store.get_space("project:owlry").unwrap().id;
    let coding = store.get_space("coding").unwrap().id;
    let linux = store.get_space("linux").unwrap().id;
    store.link_spaces(owlry, coding).unwrap();
    store.link_spaces(coding, linux).unwrap();
    (e, store)
}

#[test]
fn search_reaches_imported_spaces_via_directed_links() {
    let e = linked_env().0;
    let store = e.open();
    let path = e.write("kernel.md", "# Kernel\n\nThe scheduler is preemptible.\n");
    ingest::ingest_file(&store, &path, &["linux"]).unwrap();

    // From the project, through coding, into linux.
    let hits = search(&store, "preemptible", Some("project:owlry"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].spaces[0].name, "linux");
    assert_eq!(hits[0].spaces[0].via.as_deref(), Some("coding -> linux"));

    // One hop up the chain sees it too.
    let hits = search(&store, "preemptible", Some("coding"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].spaces[0].via.as_deref(), Some("linux"));
}

#[test]
fn directed_links_have_no_reverse_visibility() {
    let (e, store) = linked_env();
    let path = e.write("owlry.md", "# Owlry\n\nOwlry is written in Rust.\n");
    ingest::ingest_file(&store, &path, &["project:owlry"]).unwrap();

    assert_eq!(search(&store, "Rust", Some("project:owlry")).len(), 1);
    // linux does not import owlry; it must not see owlry's memories.
    assert_eq!(search(&store, "Rust", Some("linux")).len(), 0);
    assert_eq!(search(&store, "Rust", Some("coding")).len(), 0);
}

#[test]
fn cyclic_space_links_terminate() {
    let (e, store) = linked_env();
    store.create_space("a", "").unwrap();
    store.create_space("b", "").unwrap();
    let a = store.get_space("a").unwrap().id;
    let b = store.get_space("b").unwrap().id;
    store.link_spaces(a, b).unwrap();
    store.link_spaces(b, a).unwrap();

    let path = e.write("note.md", "# Note\n\nCycles must not hang the search.\n");
    ingest::ingest_file(&store, &path, &["b"]).unwrap();

    let hits = search(&store, "hang", Some("a"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].spaces[0].via.as_deref(), Some("b"));
}

// ---------------------------------------------------------------------------
// Supersession and history
// ---------------------------------------------------------------------------

#[test]
fn changed_file_supersedes_and_keeps_history() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);
    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    // Actions are ordered by subject, not by position in the file.
    let old_id = first.files[0]
        .actions
        .iter()
        .find(|a| a.subject == "Storage")
        .unwrap()
        .memory_id;

    std::fs::write(
        &path,
        TWO_SECTIONS_V1.replace("Everything lives in SQLite.", "Everything lives in DuckDB."),
    )
    .unwrap();
    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let storage_action = second.files[0]
        .actions
        .iter()
        .find(|a| a.subject == "Storage")
        .unwrap();
    let new_id = storage_action.memory_id;
    assert!(matches!(
        &storage_action.action,
        MemoryActionKind::Superseded { .. }
    ));

    // The superseded memory stays fully inspectable.
    let old_detail = store.get_memory_detail(old_id).unwrap().unwrap();
    assert_eq!(old_detail.memory.state, MemoryState::Superseded);
    assert_eq!(old_detail.memory.superseded_by, Some(new_id));
    assert_eq!(old_detail.assertion.content, "Everything lives in SQLite.");

    // Default search shows only current truth.
    assert_eq!(search(&store, "SQLite", Some("coding")).len(), 0);
    assert_eq!(search(&store, "DuckDB", Some("coding")).len(), 1);
    // Superseded knowledge is one flag away.
    let query = SearchQuery {
        text: "SQLite".to_string(),
        space_ids: Some(vec![store.get_space("coding").unwrap().id]),
        limit: 20,
        at: None,
        include_superseded: true,
        include_retired: false,
    };
    let hits = retrieval::search(&store, query.clone()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].state, MemoryState::Superseded);

    // History shows the full lifecycle, oldest first.
    let history = store.list_memories_by_subject("storage").unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].memory_id, old_id);
    assert_eq!(history[1].memory_id, new_id);
}

#[test]
fn removed_section_retires_with_reason() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);
    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let parsing_id = first.files[0]
        .actions
        .iter()
        .find(|a| a.subject == "Parsing")
        .unwrap()
        .memory_id;

    std::fs::write(
        &path,
        "# Notes\n\nIntro line.\n\n## Storage\n\nEverything lives in SQLite.\n",
    )
    .unwrap();
    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    assert!(second.files[0]
        .actions
        .iter()
        .any(|a| { a.memory_id == parsing_id && a.action == MemoryActionKind::Retired }));
    let detail = store.get_memory_detail(parsing_id).unwrap().unwrap();
    assert_eq!(detail.memory.state, MemoryState::Retired);
    assert_eq!(
        detail.memory.retire_reason.as_deref(),
        Some("subject no longer present in source at re-ingest")
    );
    assert!(detail.memory.retired_at.is_some());
}

#[test]
fn reingest_after_manual_retire_recreates() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Solitary\n\nOne memory only.\n");
    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let id = first.files[0].actions[0].memory_id;
    store
        .retire_memory(id, Some("manual".into()), Ts::now())
        .unwrap();

    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    assert_eq!(second.files[0].actions[0].action, MemoryActionKind::Created);
    assert_ne!(second.files[0].actions[0].memory_id, id);
    assert_eq!(search(&store, "Solitary", Some("coding")).len(), 1);
}

// ---------------------------------------------------------------------------
// Conflicts
// ---------------------------------------------------------------------------

#[test]
fn contradictory_claims_coexist_and_conflict() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let p1 = e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let p2 = e.write("b.md", "# Kernel\n\nThe scheduler is RT.\n");
    ingest::ingest_file(&store, &p1, &["coding"]).unwrap();
    ingest::ingest_file(&store, &p2, &["coding"]).unwrap();

    // Both stay active; the engine never collapses them.
    assert_eq!(store.list_memories_by_subject("kernel").unwrap().len(), 2);
    assert_eq!(search(&store, "CFS", Some("coding")).len(), 1);
    assert_eq!(search(&store, "RT", Some("coding")).len(), 1);

    let conflicts = store.list_conflicts().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].space_name, "coding");
    assert_eq!(conflicts[0].subject, "kernel");
    assert_eq!(conflicts[0].members.len(), 2);
}

#[test]
fn same_subject_in_different_spaces_is_no_conflict() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    store.create_space("linux", "").unwrap();
    let p1 = e.write("a.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let p2 = e.write("b.md", "# Kernel\n\nThe scheduler is RT.\n");
    ingest::ingest_file(&store, &p1, &["coding"]).unwrap();
    ingest::ingest_file(&store, &p2, &["linux"]).unwrap();
    assert!(store.list_conflicts().unwrap().is_empty());
}

#[test]
fn duplicate_sections_in_one_file_conflict() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write(
        "dup.md",
        "# Kernel\n\nClaim one.\n\n# Kernel\n\nClaim two.\n",
    );
    let record = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    assert_eq!(record.files[0].actions.len(), 2);
    assert!(record.files[0]
        .actions
        .iter()
        .all(|a| a.action == MemoryActionKind::Created));
    assert_eq!(store.list_conflicts().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Retirement and deletion semantics
// ---------------------------------------------------------------------------

#[test]
fn retire_is_non_destructive_and_one_way() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Stale\n\nThis claim went stale.\n");
    let record = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let id = record.files[0].actions[0].memory_id;

    let at = Ts::now();
    let retired = store
        .retire_memory(id, Some("no longer true".into()), at)
        .unwrap();
    assert_eq!(retired.state, MemoryState::Retired);
    assert_eq!(retired.retire_reason.as_deref(), Some("no longer true"));
    assert_eq!(retired.retired_at, Some(at));

    assert_eq!(search(&store, "stale", Some("coding")).len(), 0);
    let query = SearchQuery {
        text: "stale".to_string(),
        space_ids: None,
        limit: 20,
        at: None,
        include_superseded: false,
        include_retired: true,
    };
    assert_eq!(retrieval::search(&store, query.clone()).unwrap().len(), 1);

    // Retiring a non-active memory fails.
    assert!(matches!(
        store.retire_memory(id, None, at),
        Err(mindspace::core::Error::NotRetirable { .. })
    ));

    // The evidence chain survives retirement untouched.
    let detail = store.get_memory_detail(id).unwrap().unwrap();
    assert_eq!(detail.assertion.content, "This claim went stale.");
    assert_eq!(detail.source.kind, SourceKind::File);
}

#[test]
fn deleting_the_file_changes_nothing() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("doomed.md", "# Doomed\n\nThe file will be deleted.\n");
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    std::fs::remove_file(&path).unwrap();

    assert_eq!(search(&store, "deleted", Some("coding")).len(), 1);
    assert_eq!(store.list_memories_by_subject("doomed").unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Temporal filtering
// ---------------------------------------------------------------------------

#[test]
fn search_at_time_sees_the_past() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();

    // Hand-built observations with controlled timestamps: t1 < t2.
    let t1 = Ts::now();
    let t2 = Ts(t1.0 + 1_000_000); // one second later
    let coding = store.get_space("coding").unwrap().id;
    let observation = |ts: Ts, fact: &str| IngestObservation {
        session: SessionDraft {
            root: "/tmp/facts.md".into(),
            scope: IngestScope::File,
            observed_at: ts,
        },
        spaces: vec![],
        files: vec![ObservedFile {
            location: "/tmp/facts.md".into(),
            content: fact.to_string(),
            content_hash: mindspace::core::sha256_hex(fact.as_bytes()),
            title: "Facts".into(),
            media_type: "text/markdown".into(),
            size_bytes: fact.len() as u64,
            evidences: vec![EvidenceDraft {
                kind: EvidenceKind::Document,
                heading_path: None,
                start_line: Some(1),
                end_line: Some(1),
                ordinal: 0,
                content: fact.to_string(),
                content_hash: mindspace::core::sha256_hex(fact.as_bytes()),
            }],
            assertions: vec![AssertionDraft {
                evidence_idx: 0,
                kind: AssertionKind::Claim,
                subject: "Speed".into(),
                subject_normalized: "speed".into(),
                predicate: None,
                content: fact.to_string(),
                extraction: "markdown:preamble".into(),
            }],
            memories: vec![MemorySpec {
                assertion_idx: 0,
                spaces: vec![coding],
            }],
        }],
    };

    store
        .record_observation(observation(t1, "The tool is fast."))
        .unwrap();
    store
        .record_observation(observation(t2, "The tool is faster."))
        .unwrap();

    let coding = store.get_space("coding").unwrap().id;
    let at = |t: Ts| SearchQuery {
        text: "tool".to_string(),
        space_ids: Some(vec![coding]),
        limit: 20,
        at: Some(t),
        include_superseded: false,
        include_retired: false,
    };

    // At t1: the first fact is current truth even though it is superseded now.
    let hits = retrieval::search(&store, at(t1)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content, "The tool is fast.");

    // At t2: the second fact is current.
    let hits = retrieval::search(&store, at(t2)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content, "The tool is faster.");

    // Default (now): only the current fact.
    assert_eq!(search(&store, "tool", Some("coding")).len(), 1);
    assert_eq!(
        search(&store, "tool", Some("coding"))[0].content,
        "The tool is faster."
    );
}

// ---------------------------------------------------------------------------
// Id resolution
// ---------------------------------------------------------------------------

#[test]
fn id_prefixes_resolve_uniquely_or_fail_loudly() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Alpha\n\nFirst.\n\n# Beta\n\nSecond.\n");
    let record = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let full_a = record.files[0].actions[0].memory_id.to_string();
    let full_b = record.files[0].actions[1].memory_id.to_string();

    // Full ids resolve.
    assert_eq!(
        resolve_memory_id(&store, &full_a).unwrap(),
        record.files[0].actions[0].memory_id
    );
    // The full id without hyphens is also accepted as a (unique) prefix.
    let stripped_a: String = full_a.chars().filter(|c| *c != '-').collect();
    assert_eq!(
        resolve_memory_id(&store, &stripped_a).unwrap(),
        record.files[0].actions[0].memory_id
    );
    // Too short or unknown fails.
    assert!(matches!(
        resolve_memory_id(&store, "ab"),
        Err(mindspace::core::Error::MemoryNotFound { .. })
    ));
    assert!(matches!(
        resolve_memory_id(&store, "zzzzzz"),
        Err(mindspace::core::Error::MemoryNotFound { .. })
    ));
    // A shared prefix is ambiguous. Both memories were created in the same
    // ingest, so their UUIDv7 timestamps share a long common prefix.
    let stripped_b: String = full_b.chars().filter(|c| *c != '-').collect();
    let shared = stripped_a
        .chars()
        .zip(stripped_b.chars())
        .take_while(|(a, b)| a == b)
        .count();
    assert!(shared >= 4, "expected a common UUIDv7 timestamp prefix");
    let prefix = &stripped_a[..shared];
    assert!(matches!(
        resolve_memory_id(&store, prefix),
        Err(mindspace::core::Error::AmbiguousPrefix { .. })
    ));
}

// ---------------------------------------------------------------------------
// Immutability and persistence
// ---------------------------------------------------------------------------

#[test]
fn immutability_triggers_reject_raw_updates() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Locked\n\nEvidence must never change.\n");
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    drop(store);

    let raw = rusqlite::Connection::open(&e.db).unwrap();
    for table in ["sources", "evidence", "assertions"] {
        let sql = format!("UPDATE {table} SET content = 'tampered'");
        let result = raw.execute(&sql, []);
        assert!(result.is_err(), "{table} accepted an UPDATE");
    }
}

#[test]
fn database_persists_across_restarts() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", TWO_SECTIONS_V1);
    let record = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let id = record.files[0].actions[0].memory_id;
    store
        .retire_memory(id, Some("gone".into()), Ts::now())
        .unwrap();
    drop(store);

    // Fresh process, same file.
    let store = e.open();
    assert_eq!(store.list_spaces().unwrap().len(), 1);
    assert_eq!(search(&store, "SQLite", Some("coding")).len(), 1);
    let detail = store.get_memory_detail(id).unwrap().unwrap();
    assert_eq!(detail.memory.state, MemoryState::Retired);
    assert_eq!(detail.memory.retire_reason.as_deref(), Some("gone"));
}

// ---------------------------------------------------------------------------
// Search behavior details
// ---------------------------------------------------------------------------

#[test]
fn search_scores_rank_and_snippet_and_explain_spaces() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write(
        "notes.md",
        "# SQLite\n\nSQLite stores data in a single file.\n",
    );
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    let hits = search(&store, "single file", Some("coding"));
    assert_eq!(hits.len(), 1);
    assert!(hits[0].score > 0.0, "user-facing score is positive bm25");
    assert!(
        hits[0].snippet.contains("single"),
        "snippet: {}",
        hits[0].snippet
    );
    assert_eq!(hits[0].extraction, "markdown:section-heading");
    assert_eq!(hits[0].spaces.len(), 1);
    assert_eq!(hits[0].spaces[0].via, None, "direct membership has no via");
}

#[test]
fn punctuation_queries_fall_back_to_phrase_search() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write(
        "notes.md",
        "# Bounds\n\nPanics on O(1) access violations.\n",
    );
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    assert_eq!(search(&store, "O(1)", Some("coding")).len(), 1);
    // A hyphen is parsed as FTS syntax first; malformed input must retry as
    // a literal phrase and produce an ordinary empty result instead of a SQL
    // error about a missing column.
    assert!(search(&store, "absent-term", Some("coding")).is_empty());
}

#[test]
fn filtered_by_requested_space_only() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    store.create_space("linux", "").unwrap();
    let path = e.write("notes.md", "# Only\n\nPresent in one space.\n");
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    let coding = store.get_space("coding").unwrap().id;
    let linux = store.get_space("linux").unwrap().id;
    let at = |t: Option<Ts>, space: Option<Vec<mindspace::core::Uuid>>| SearchQuery {
        text: "present".into(),
        space_ids: space,
        limit: 20,
        at: t,
        include_superseded: false,
        include_retired: false,
    };
    assert_eq!(
        retrieval::search(&store, at(None, Some(vec![coding])))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        retrieval::search(&store, at(None, Some(vec![linux])))
            .unwrap()
            .len(),
        0
    );
    // Some(empty) means "no spaces requested": nothing matches.
    assert!(retrieval::search(&store, at(None, Some(vec![])))
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// Adversarial model tests (v0 architecture review)
// ---------------------------------------------------------------------------

#[test]
fn heading_rename_supersedes_and_preserves_lineage() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Storage\n\nEverything lives in SQLite.\n");
    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let old_id = first.files[0].actions[0].memory_id;

    // Only the heading changes; the proposition survives.
    std::fs::write(&path, "# Persistence\n\nEverything lives in SQLite.\n").unwrap();
    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    assert_eq!(
        second.files[0].actions.len(),
        1,
        "{:?}",
        second.files[0].actions
    );
    assert!(matches!(
        &second.files[0].actions[0].action,
        MemoryActionKind::Superseded { .. }
    ));
    let new_id = second.files[0].actions[0].memory_id;

    let old = store.get_memory_detail(old_id).unwrap().unwrap();
    assert_eq!(old.memory.state, MemoryState::Superseded);
    assert_eq!(old.memory.superseded_by, Some(new_id));
    assert_eq!(old.assertion.content, "Everything lives in SQLite.");

    let new = store.get_memory_detail(new_id).unwrap().unwrap();
    assert_eq!(new.memory.state, MemoryState::Active);
    assert_eq!(new.assertion.subject, "Persistence");
    assert_eq!(new.assertion.content, "Everything lives in SQLite.");
}

#[test]
fn recreate_after_retire_reuses_the_assertion() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Kernel\n\nThe scheduler is CFS.\n");
    let first = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let first_id = first.files[0].actions[0].memory_id;
    store
        .retire_memory(first_id, Some("manual".into()), Ts::now())
        .unwrap();

    let second = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let second_id = second.files[0].actions[0].memory_id;
    assert_ne!(second_id, first_id);

    // The interpretation did not change; only the belief episode did.
    // Two assertion rows identical in every observable column would be
    // manufactured identity.
    let a = store.get_memory_detail(first_id).unwrap().unwrap();
    let b = store.get_memory_detail(second_id).unwrap().unwrap();
    assert_eq!(
        a.assertion.id, b.assertion.id,
        "same evidence + rule + subject + content must be the same assertion"
    );
    assert_eq!(a.assertion.created_at, b.assertion.created_at);
}

#[test]
fn proposition_flip_flop_keeps_episodes_and_reuses_interpretations() {
    let e = env();
    let store = e.open();
    store.create_space("coding", "").unwrap();
    let path = e.write("notes.md", "# Toolkit\n\nThe project uses iced.\n");
    let h1 = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let iced1 = h1.files[0].actions[0].memory_id;

    std::fs::write(&path, "# Toolkit\n\nThe project uses slint.\n").unwrap();
    ingest::ingest_file(&store, &path, &["coding"]).unwrap();

    // v3 is byte-identical to v1: the file flips back.
    std::fs::write(&path, "# Toolkit\n\nThe project uses iced.\n").unwrap();
    let h3 = ingest::ingest_file(&store, &path, &["coding"]).unwrap();
    let iced2 = h3.files[0].actions[0].memory_id;

    let a = store.get_memory_detail(iced1).unwrap().unwrap();
    let b = store.get_memory_detail(iced2).unwrap().unwrap();
    assert_eq!(a.memory.state, MemoryState::Superseded);
    assert_eq!(b.memory.state, MemoryState::Active);
    assert_eq!(b.memory.superseded_by, None);
    // Byte-identical revert: same source, same evidence, same interpretation,
    // one new belief episode.
    assert_eq!(a.assertion.id, b.assertion.id);
}
