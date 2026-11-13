//! Deterministic legacy-response snapshot tests through real Store/M6 machinery.
//! Test hooks are thread-local and never exist in a production build.

use super::*;
use crate::core::{ManualMemoryDraft, ManualMemoryReport};
use crate::retrieval;
use crate::storage::MemoryRead;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc;
use tempfile::TempDir;

struct Hook {
    stage: &'static str,
    action: Box<dyn FnOnce()>,
}

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Called only at cfg(test) boundaries inside connection-scoped SQL helpers.
/// Take the hook before calling it so writer-side reads cannot borrow it again.
pub(super) fn checkpoint(stage: &str) {
    let action = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|hook| hook.stage == stage) {
            slot.take().map(|hook| hook.action)
        } else {
            None
        }
    });
    if let Some(action) = action {
        action();
    }
}

struct ClearHook;
impl Drop for ClearHook {
    fn drop(&mut self) {
        HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn at<T>(stage: &'static str, action: impl FnOnce() + 'static, run: impl FnOnce() -> T) -> T {
    HOOK.with(|slot| {
        assert!(slot.borrow().is_none(), "nested test checkpoint");
        *slot.borrow_mut() = Some(Hook {
            stage,
            action: Box::new(action),
        });
    });
    let _clear = ClearHook;
    let value = run();
    HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "checkpoint {stage} was not reached"
        )
    });
    value
}

struct World {
    _dir: TempDir,
    path: PathBuf,
    store: SqliteStore,
    root: Uuid,
    red: Uuid,
    blue: Uuid,
    first: ManualMemoryReport,
    second: ManualMemoryReport,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mind.db");
        let store = SqliteStore::open(&path).unwrap();
        let root = store.create_space("root", "synthetic root").unwrap().id;
        let red = store.create_space("red", "synthetic red").unwrap().id;
        let blue = store.create_space("blue", "synthetic blue").unwrap().id;
        store.link_spaces(root, red).unwrap();
        let first = manual(&store, "sessioncanary first", red);
        let second = manual(&store, "sessioncanary second", blue);
        manual(&store, "synthetic conflict-only", red);
        Self {
            _dir: dir,
            path,
            store,
            root,
            red,
            blue,
            first,
            second,
        }
    }

    fn writer(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }

    fn query(&self) -> SearchQuery {
        SearchQuery {
            text: "sessioncanary".into(),
            space_ids: Some(vec![self.root]),
            limit: 20,
            at: None,
            include_superseded: false,
            include_retired: false,
        }
    }
}

fn manual(store: &SqliteStore, content: &str, space: Uuid) -> ManualMemoryReport {
    store
        .record_manual_write(ManualMemoryDraft {
            subject: Some("Shared".into()),
            content: content.into(),
            spaces: vec![space],
            supersedes: None,
        })
        .unwrap()
}

fn ledger(store: &SqliteStore) -> (i64, i64) {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT MAX(revision_id),COUNT(*) FROM m6_revisions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

/// One actual M6 transaction changes reachability AND both membership sets.
/// A root query returns `first` in either committed state. A split scope/candidate
/// read instead returns `second`, which is the negative control below.
fn swap_scope(writer: &SqliteStore, root: Uuid, red: Uuid, blue: Uuid, first: Uuid, second: Uuid) {
    let c = writer.connection().unwrap();
    let tx = begin_write(&c, &writer.revision_state).unwrap();
    tx.execute(
        "DELETE FROM space_links WHERE from_space=?1",
        [root.to_string()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO space_links(from_space,to_space,created_at) VALUES(?1,?2,?3)",
        rusqlite::params![root.to_string(), blue.to_string(), Ts::now().0],
    )
    .unwrap();
    for (memory, space) in [(first, blue), (second, red)] {
        tx.execute(
            "DELETE FROM memory_spaces WHERE memory_id=?1",
            [memory.to_string()],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO memory_spaces(memory_id,space_id,created_at) VALUES(?1,?2,?3)",
            rusqlite::params![memory.to_string(), space.to_string(), Ts::now().0],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn rename_and_retire(writer: &SqliteStore, red: Uuid, memory: Uuid) {
    let c = writer.connection().unwrap();
    let tx = begin_write(&c, &writer.revision_state).unwrap();
    tx.execute(
        "UPDATE spaces SET name='red-new',description='SYNTHETIC_NEW' WHERE id=?1",
        [red.to_string()],
    )
    .unwrap();
    tx.execute(
        "UPDATE memories SET state='retired',retired_at=?1,retire_reason='SYNTHETIC_NEW' WHERE id=?2",
        rusqlite::params![Ts::now().0, memory.to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
}

#[test]
fn independent_statement_negative_control_can_combine_scope_and_candidates() {
    let w = World::new();
    let links = w.store.list_space_links().unwrap();
    assert_eq!(links[0].to_space, w.red);
    swap_scope(
        &w.writer(),
        w.root,
        w.red,
        w.blue,
        w.first.memory_id,
        w.second.memory_id,
    );
    let mut stale_scope = w.query();
    stale_scope.space_ids = Some(vec![w.red]);
    let split = w.store.search(&stale_scope).unwrap();
    assert_eq!(split.len(), 1);
    assert_eq!(split[0].memory_id, w.second.memory_id);
    let coherent = retrieval::search(&w.store, w.query()).unwrap();
    assert_eq!(coherent.len(), 1);
    assert_eq!(coherent[0].memory_id, w.first.memory_id);
}

#[test]
fn operational_scope_and_candidates_keep_one_committed_state() {
    let w = World::new();
    let before = ledger(&w.store);
    let old = retrieval::search(&w.store, w.query()).unwrap();
    let writer = w.writer();
    let (root, red, blue, first, second) =
        (w.root, w.red, w.blue, w.first.memory_id, w.second.memory_id);
    let observed = at(
        "before_search",
        move || swap_scope(&writer, root, red, blue, first, second),
        || retrieval::search(&w.store, w.query()).unwrap(),
    );
    assert_eq!(observed, old);
    let new = retrieval::search(&w.store, w.query()).unwrap();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].memory_id, w.first.memory_id);
    assert_ne!(
        new, old,
        "membership attribution changes in the fresh snapshot"
    );
    assert_eq!(ledger(&w.store), (before.0 + 1, before.1 + 1));
}

#[test]
fn operational_candidate_rows_and_space_enrichment_keep_one_snapshot() {
    let w = World::new();
    let mut query = w.query();
    query.space_ids = None;
    let old = retrieval::search(&w.store, query.clone()).unwrap();
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "search_enrichment",
        move || rename_and_retire(&writer, red, first),
        || retrieval::search(&w.store, query.clone()).unwrap(),
    );
    assert_eq!(
        observed, old,
        "old row state must not acquire new membership names"
    );
    let new = retrieval::search(&w.store, query).unwrap();
    assert_eq!(new.len(), old.len() - 1);
}

#[test]
fn operational_fts_ranking_stays_pinned_when_corpus_changes_after_scope() {
    let w = World::new();
    let old = retrieval::search(&w.store, w.query()).unwrap();
    let writer = w.writer();
    let blue = w.blue;
    let observed = at(
        "before_search",
        move || {
            manual(
                &writer,
                "sessioncanary sessioncanary longer new synthetic corpus input",
                blue,
            );
        },
        || retrieval::search(&w.store, w.query()).unwrap(),
    );
    assert_eq!(
        observed, old,
        "BM25 corpus and candidates must use the scope snapshot"
    );
    let new = retrieval::search(&w.store, w.query()).unwrap();
    assert_eq!(new[0].memory_id, old[0].memory_id);
    assert_ne!(
        new[0].score, old[0].score,
        "positive corpus/ranking control"
    );
}

#[test]
fn detail_response_keeps_state_and_all_enrichment_in_one_snapshot() {
    let w = World::new();
    let old = w
        .store
        .get_memory_detail(w.first.memory_id)
        .unwrap()
        .unwrap();
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "detail_memory",
        move || rename_and_retire(&writer, red, first),
        || w.store.get_memory_detail(first).unwrap().unwrap(),
    );
    assert_eq!(observed, old);
    let new = w.store.get_memory_detail(first).unwrap().unwrap();
    assert_eq!(new.memory.state, MemoryState::Retired);
    assert_eq!(new.spaces[0].name, "red-new");
    assert_ne!(new, old);
}

#[test]
fn history_rows_and_memberships_keep_one_snapshot() {
    let w = World::new();
    let old = w.store.list_memories_by_subject("shared").unwrap();
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "summary_enrichment",
        move || rename_and_retire(&writer, red, first),
        || w.store.list_memories_by_subject("shared").unwrap(),
    );
    assert_eq!(observed, old);
    assert_ne!(w.store.list_memories_by_subject("shared").unwrap(), old);
}

#[test]
fn conflict_groups_members_and_space_attribution_keep_one_snapshot() {
    let w = World::new();
    let old = w.store.list_conflicts().unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].members.len(), 2);
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "conflict_members",
        move || rename_and_retire(&writer, red, first),
        || w.store.list_conflicts().unwrap(),
    );
    assert_eq!(observed, old);
    assert!(w.store.list_conflicts().unwrap().is_empty());
}

#[test]
fn held_response_session_covers_prefix_detail_counts_and_allows_queued_wal_writer() {
    const WAIT: Duration = Duration::from_secs(15);
    let w = World::new();
    let before = ledger(&w.store);
    let writer = w.writer();
    let old_detail = w
        .store
        .get_memory_detail(w.first.memory_id)
        .unwrap()
        .unwrap();
    let old_counts = w.store.list_spaces_with_counts().unwrap();
    let read = w.store.read_session().unwrap();
    assert_eq!(
        read.memory_id_prefix_matches(&w.first.memory_id.to_string())
            .unwrap(),
        vec![w.first.memory_id]
    );
    let (go_tx, go_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let (red, first) = (w.red, w.first.memory_id);
    let join = std::thread::spawn(move || {
        go_rx.recv_timeout(WAIT).unwrap();
        rename_and_retire(&writer, red, first);
        done_tx.send(()).unwrap();
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        go_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(read.get_memory_detail(first).unwrap().unwrap(), old_detail);
        assert_eq!(read.list_spaces_with_counts().unwrap(), old_counts);
        assert_eq!(read.list_memories_by_subject("shared").unwrap().len(), 3);
        assert_eq!(read.list_conflicts().unwrap()[0].members.len(), 2);
    }));
    drop(go_tx);
    drop(read);
    let child = join.join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    child.unwrap();
    assert_eq!(
        w.store
            .get_memory_detail(first)
            .unwrap()
            .unwrap()
            .memory
            .state,
        MemoryState::Retired
    );
    assert_eq!(ledger(&w.store), (before.0 + 1, before.1 + 1));
    let c = w.store.connection().unwrap();
    assert!(c.is_autocommit());
    validate_fence_inventory(&c).unwrap();
    assert_eq!(
        c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
}

#[test]
fn temporal_predicates_and_enrichment_use_the_same_pinned_response() {
    let w = World::new();
    let detail = w
        .store
        .get_memory_detail(w.first.memory_id)
        .unwrap()
        .unwrap();
    let at_time = detail.memory.created_at;
    let mut query = w.query();
    query.at = Some(at_time);
    let old = retrieval::search(&w.store, query.clone()).unwrap();
    assert!(old.iter().any(|h| h.memory_id == w.first.memory_id));
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "search_enrichment",
        move || rename_and_retire(&writer, red, first),
        || retrieval::search(&w.store, query.clone()).unwrap(),
    );
    assert_eq!(observed, old);
    let new = retrieval::search(&w.store, query).unwrap();
    assert!(
        new.iter().any(|h| h.memory_id == first),
        "at(T) retains prior episode eligibility"
    );
    assert_ne!(
        new, old,
        "current membership names remain snapshot-relative, not historical membership promises"
    );
}

#[test]
fn early_error_and_unwind_release_read_transaction_without_allocating_revision() {
    let w = World::new();
    let before = ledger(&w.store);
    {
        let read = w.store.read_session().unwrap();
        assert!(matches!(
            read.get_space("SYNTHETIC_ABSENT"),
            Err(Error::SpaceNotFound { .. })
        ));
    }
    assert!(w.store.connection().unwrap().is_autocommit());
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let read = w.store.read_session().unwrap();
        read.list_spaces().unwrap();
        panic!("synthetic cancellation while session owns the connection");
    }));
    assert!(unwound.is_err());
    // std::sync::Mutex is poisoned by an unwind through its held guard; the
    // existing Store contract must reject reuse rather than silently reconnect.
    assert!(matches!(w.store.list_spaces(), Err(Error::Storage(_))));
    let reopened = w.writer();
    assert_eq!(ledger(&reopened), before);
    reopened
        .set_meta("fixture.after_unwind", "SYNTHETIC_OK")
        .unwrap();
}

#[test]
fn busy_response_read_is_explicit_and_cleanup_allows_reuse() {
    let w = World::new();
    let before = ledger(&w.store);
    {
        let c = w.store.connection().unwrap();
        assert_eq!(
            c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        c.busy_timeout(Duration::from_millis(20)).unwrap();
    }
    let blocker = Connection::open(&w.path).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    {
        let read = w.store.read_session().unwrap();
        assert!(matches!(
            read.list_spaces_with_counts(),
            Err(Error::DatabaseBusy { .. })
        ));
    }
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(w.store.connection().unwrap().is_autocommit());
    assert_eq!(w.store.list_spaces_with_counts().unwrap().len(), 3);
    assert_eq!(ledger(&w.store), before);
}

#[test]
fn response_session_propagates_actual_main_read_io_error() {
    let w = World::new();
    let before = ledger(&w.store);
    {
        let c = w.store.connection().unwrap();
        assert_eq!(
            c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        c.execute_batch("PRAGMA mmap_size=0; PRAGMA shrink_memory")
            .unwrap();
        // SAFETY: this Store's connection guard is exclusively retained, and
        // no statements are active; the call releases cached clean pages.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_db_release_memory(c.handle()) },
            rusqlite::ffi::SQLITE_OK
        );
    }
    let read = read_session::SqliteMemoryRead::begin(&w.store).unwrap();
    {
        let _fault = settings_tests::FileReadFault::install(read.connection_for_test());
        let error = read.list_spaces().unwrap_err();
        assert!(matches!(error, Error::Storage(_)), "{error:?}");
        // SAFETY: the still-live connection is exclusively retained by read.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_extended_errcode(read.connection_for_test().handle()) },
            rusqlite::ffi::SQLITE_IOERR_READ
        );
    }
    // Restore the copied file-method table before session cleanup can close it.
    drop(read);
    let reopened = w.writer();
    assert_eq!(ledger(&reopened), before);
    assert_eq!(reopened.list_spaces_with_counts().unwrap().len(), 3);
}

struct SyntheticRuntime;
impl crate::embeddings::EmbeddingRuntime for SyntheticRuntime {
    fn embed_document(&self, _: &str) -> Result<Vec<f32>> {
        panic!("read responses never index");
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
        Ok(vec![1.0, 0.0])
    }
    fn model_id(&self) -> &str {
        SYNTHETIC_MODEL.id
    }
    fn dimension(&self) -> usize {
        2
    }
}

static SYNTHETIC_MODEL: crate::embeddings::manifest::ModelManifestEntry =
    crate::embeddings::manifest::ModelManifestEntry {
        id: "session-synthetic",
        dimension: 2,
        artifacts: &[
            crate::embeddings::manifest::Artifact {
                filename: "model.safetensors",
                url: "https://example.invalid/model",
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                size: 3,
            },
            crate::embeddings::manifest::Artifact {
                filename: "tokenizer.json",
                url: "https://example.invalid/tokenizer",
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                size: 3,
            },
        ],
        ..crate::embeddings::manifest::NOMIC_V1_5
    };

fn synthetic_lookup(id: &str) -> Option<&'static crate::embeddings::manifest::ModelManifestEntry> {
    (id == SYNTHETIC_MODEL.id).then_some(&SYNTHETIC_MODEL)
}

fn vector_fixture(w: &World) -> (TempDir, retrieval::SemanticCapability, String) {
    let dir = tempfile::tempdir().unwrap();
    let model_dir = crate::embeddings::manager::model_dir_in(dir.path(), SYNTHETIC_MODEL.id);
    std::fs::create_dir_all(&model_dir).unwrap();
    for artifact in SYNTHETIC_MODEL.artifacts {
        std::fs::write(model_dir.join(artifact.filename), b"abc").unwrap();
    }
    w.store.set_meta("embedding.enabled", "true").unwrap();
    w.store
        .set_meta("embedding.model", SYNTHETIC_MODEL.id)
        .unwrap();
    let key =
        crate::embeddings::manifest::derivation_from_manifest(&SYNTHETIC_MODEL).derivation_key();
    w.store
        .save_embeddings(&[
            AssertionEmbedding {
                assertion_id: w.first.assertion_id,
                derivation_key: key.clone(),
                model: SYNTHETIC_MODEL.id.into(),
                model_version: SYNTHETIC_MODEL.version.into(),
                dimension: 2,
                embedding: vec![1.0, 0.0],
            },
            AssertionEmbedding {
                assertion_id: w.second.assertion_id,
                derivation_key: key.clone(),
                model: SYNTHETIC_MODEL.id.into(),
                model_version: SYNTHETIC_MODEL.version.into(),
                dimension: 2,
                embedding: vec![0.0, 1.0],
            },
        ])
        .unwrap();
    let capability =
        retrieval::semantic::resolve_with(&w.store, dir.path(), synthetic_lookup, &|_, _| {
            Ok(Box::new(SyntheticRuntime))
        })
        .unwrap();
    assert!(matches!(
        capability,
        retrieval::SemanticCapability::Ready(_)
    ));
    (dir, capability, key)
}

fn flip_vectors(writer: &SqliteStore, key: &str, first: Uuid, second: Uuid) {
    let c = writer.connection().unwrap();
    let tx = begin_write(&c, &writer.revision_state).unwrap();
    for (id, vector) in [(first, vec![0.0, 1.0]), (second, vec![1.0, 0.0])] {
        assert_eq!(
            tx.execute(
                "UPDATE assertion_embeddings SET embedding=?1 WHERE assertion_id=?2 AND derivation_key=?3",
                rusqlite::params![encode_f32_le(&vector).unwrap(), id.to_string(), key],
            )
            .unwrap(),
            1
        );
    }
    tx.commit().unwrap();
}

#[test]
fn semantic_hybrid_and_auto_pin_vector_ranking_and_both_branches() {
    use crate::retrieval::{RetrievalOptions, SearchMode};
    for mode in [SearchMode::Semantic, SearchMode::Hybrid, SearchMode::Auto] {
        let w = World::new();
        let (_assets, capability, key) = vector_fixture(&w);
        let mut query = w.query();
        query.space_ids = None;
        let old = retrieval::search_with_mode(
            &w.store,
            query.clone(),
            mode,
            &capability,
            RetrievalOptions::default(),
        )
        .unwrap();
        assert_eq!(old.hits.len(), 2);
        let writer = w.writer();
        let (first, second) = (w.first.assertion_id, w.second.assertion_id);
        let observed = at(
            "before_vectors",
            move || flip_vectors(&writer, &key, first, second),
            || {
                retrieval::search_with_mode(
                    &w.store,
                    query.clone(),
                    mode,
                    &capability,
                    RetrievalOptions::default(),
                )
                .unwrap()
            },
        );
        assert_eq!(
            observed, old,
            "{mode:?}: vector scores/ranks must stay in the response snapshot"
        );
        let new = retrieval::search_with_mode(
            &w.store,
            query,
            mode,
            &capability,
            RetrievalOptions::default(),
        )
        .unwrap();
        assert_ne!(new, old, "{mode:?}: positive vector mutation control");
        if mode == SearchMode::Semantic {
            assert_eq!(new.hits[0].hit.memory_id, w.second.memory_id);
        }
    }
}

#[test]
fn lost_transaction_is_refused_instead_of_starting_another_snapshot() {
    let w = World::new();
    let before = ledger(&w.store);
    let read = read_session::SqliteMemoryRead::begin(&w.store).unwrap();
    read.list_spaces().unwrap();
    // This deliberately simulates SQLite ending a read transaction behind the
    // session after an exceptional engine outcome. It grants no write context.
    read.connection_for_test()
        .execute_batch("ROLLBACK")
        .unwrap();
    assert!(matches!(read.list_spaces(), Err(Error::Storage(_))));
    assert!(Box::new(read).finish().is_err());
    assert!(matches!(
        w.store.list_spaces(),
        Err(Error::ConnectionDiscarded)
    ));
    assert_eq!(ledger(&w.writer()), before);
}

#[test]
fn session_preserves_optional_settings_and_reports_malformed_values_without_revision() {
    let w = World::new();
    let before = ledger(&w.store);
    {
        let read = w.store.read_session().unwrap();
        assert_eq!(
            read.read_embedding_settings().unwrap(),
            EmbeddingSettings {
                enabled: None,
                model: None
            }
        );
        read.finish().unwrap();
    }
    assert_eq!(ledger(&w.store), before);
    w.store.set_meta("embedding.enabled", "false").unwrap();
    w.store
        .set_meta_blob_for_test("embedding.model", b"SYNTHETIC_BLOB")
        .unwrap();
    let before_read = ledger(&w.store);
    {
        let read = w.store.read_session().unwrap();
        assert!(matches!(
            read.read_embedding_settings(),
            Err(Error::Storage(_))
        ));
    }
    assert!(w.store.connection().unwrap().is_autocommit());
    assert_eq!(ledger(&w.store), before_read);
}

#[test]
fn endpoint_prefix_resolution_and_detail_collection_share_the_snapshot() {
    let w = World::new();
    let prefix = w.first.memory_id.to_string();
    let old = crate::memory_detail_for_prefix(&w.store, &prefix).unwrap();
    let writer = w.writer();
    let (red, first) = (w.red, w.first.memory_id);
    let observed = at(
        "prefix_matches",
        move || rename_and_retire(&writer, red, first),
        || crate::memory_detail_for_prefix(&w.store, &prefix).unwrap(),
    );
    assert_eq!(
        observed, old,
        "the prefix lookup has already established the response snapshot"
    );
    assert_ne!(
        crate::memory_detail_for_prefix(&w.store, &prefix).unwrap(),
        old
    );
}

#[test]
fn requested_space_name_and_entire_lexical_response_share_one_snapshot() {
    let w = World::new();
    let mut query = w.query();
    query.space_ids = None;
    let names = vec!["root".to_owned()];
    let old = retrieval::search_named(&w.store, query.clone(), Some(&names)).unwrap();
    assert_eq!(old.len(), 1);
    let writer = w.writer();
    let (root, red, first) = (w.root, w.red, w.first.memory_id);
    let observed = at(
        "space_name",
        move || {
            let c = writer.connection().unwrap();
            let tx = begin_write(&c, &writer.revision_state).unwrap();
            tx.execute(
                "UPDATE spaces SET name='root-new' WHERE id=?1",
                [root.to_string()],
            )
            .unwrap();
            tx.execute(
                "UPDATE spaces SET name='red-new' WHERE id=?1",
                [red.to_string()],
            )
            .unwrap();
            tx.execute("UPDATE memories SET state='retired',retired_at=?1,retire_reason='SYNTHETIC_NEW' WHERE id=?2", rusqlite::params![Ts::now().0, first.to_string()]).unwrap();
            tx.commit().unwrap();
        },
        || retrieval::search_named(&w.store, query.clone(), Some(&names)).unwrap(),
    );
    assert_eq!(
        observed, old,
        "name lookup already established the response snapshot"
    );
    assert!(matches!(
        retrieval::search_named(&w.store, query.clone(), Some(&names)),
        Err(Error::SpaceNotFound { .. })
    ));
    let new_names = vec!["root-new".to_owned()];
    assert!(retrieval::search_named(&w.store, query, Some(&new_names))
        .unwrap()
        .is_empty());
}
