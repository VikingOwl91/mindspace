//! Real M6 fixtures for classification before ordinary file-observation effects.
//! Hooks are per-thread, bounded, and absent from production builds.

use super::*;
use crate::core::{AssertionEmbedding, SessionDraft};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;
use tempfile::TempDir;

const ROOT: &str = "/synthetic/observation-root";
const WAIT: Duration = Duration::from_secs(15);
type Hook = Box<dyn FnMut(&str, &Connection) -> Result<()>>;
thread_local! { static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) }; }

pub(super) fn checkpoint(stage: &str, conn: &Connection) -> Result<()> {
    let hook = HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(mut hook) = hook {
        let result = hook(stage, conn);
        HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
        result
    } else {
        Ok(())
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
fn with_hook<T>(
    hook: impl FnMut(&str, &Connection) -> Result<()> + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    HOOK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _clear = ClearHook;
    operation()
}

struct World {
    _dir: TempDir,
    path: PathBuf,
    store: SqliteStore,
    notes: Uuid,
    other: Uuid,
}
impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mind.db");
        let store = SqliteStore::open(&path).unwrap();
        let notes = store.create_space("notes", "synthetic fixture").unwrap().id;
        let other = store.create_space("other", "synthetic fixture").unwrap().id;
        Self {
            _dir: dir,
            path,
            store,
            notes,
            other,
        }
    }
    fn writer(&self) -> SqliteStore {
        SqliteStore::open(&self.path).unwrap()
    }
    fn observation(
        &self,
        scope: IngestScope,
        inputs: &[(&str, &str)],
        spaces: &[Uuid],
        at: i64,
    ) -> IngestObservation {
        observation(scope, inputs, spaces, at)
    }
    fn record(
        &self,
        scope: IngestScope,
        inputs: &[(&str, &str)],
        spaces: &[Uuid],
        at: i64,
    ) -> ObservationReport {
        self.store
            .record_observation(self.observation(scope, inputs, spaces, at))
            .unwrap()
    }
}
fn observation(
    scope: IngestScope,
    inputs: &[(&str, &str)],
    spaces: &[Uuid],
    at: i64,
) -> IngestObservation {
    IngestObservation {
        session: SessionDraft {
            root: ROOT.into(),
            scope,
            observed_at: Ts(at),
        },
        files: inputs
            .iter()
            .map(|(name, text)| {
                let location = format!("{ROOT}/{name}");
                crate::ingest::observe_bytes(
                    Path::new(name),
                    format!("# Topic\n\n{text}\n").into_bytes(),
                    location,
                    spaces,
                )
                .unwrap()
            })
            .collect(),
        spaces: spaces.to_vec(),
    }
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
fn dump(conn: &Connection, include_m6: bool) -> Vec<String> {
    let tables = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let mut result = Vec::new();
    for table in tables {
        if !include_m6 && table.starts_with("m6_") {
            continue;
        }
        let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
        let mut stmt = conn.prepare(&sql).unwrap();
        let mut rows = stmt
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|i| row.get_ref(i).map(|v| format!("{v:?}")))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .map(|row| format!("{table}:{:?}", row.unwrap()))
            .collect::<Vec<_>>();
        rows.sort();
        result.extend(rows);
    }
    result
}
fn assert_integrity(store: &SqliteStore) {
    store.verify_fts_index().unwrap();
    let c = store.connection().unwrap();
    validate_fence_inventory(&c).unwrap();
    assert_eq!(
        c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}
fn clear_authorizer(c: &Connection) {
    c.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .unwrap();
}
struct AuthorizerReset<'a>(&'a Connection);
impl Drop for AuthorizerReset<'_> {
    fn drop(&mut self) {
        clear_authorizer(self.0);
    }
}

#[test]
fn planner_accepts_no_mutation_sql_after_the_existing_tentative_m6_revision() {
    let w = World::new();
    let first = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A"), ("gone.md", "SYNTHETIC_GONE")],
        &[w.notes],
        100,
    );
    let before = ledger(&w.store);
    let c = w.store.connection().unwrap();
    let tx = begin_write(&c, &w.store.revision_state).unwrap();
    let changes = c.total_changes();
    let rows = dump(&c, false);
    let writes = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&writes);
    c.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. } => {
            observed.fetch_add(1, AtomicOrdering::SeqCst);
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }))
    .unwrap();
    let reset = AuthorizerReset(&c);
    let plan = plan_file_observation(
        &tx,
        w.observation(
            IngestScope::Directory,
            &[("new.md", "SYNTHETIC_NEW"), ("a.md", "SYNTHETIC_A")],
            &[w.other],
            200,
        ),
    )
    .unwrap();
    assert_eq!(plan.files[0].location, format!("{ROOT}/a.md"));
    assert!(
        matches!(&plan.classifications[0],Plan::Unchanged{logical,..} if *logical==first.files[0].logical_id)
    );
    assert_eq!(plan.residual_disappearances.len(), 1);
    assert_eq!(writes.load(AtomicOrdering::SeqCst), 0);
    assert_eq!(c.total_changes(), changes);
    assert_eq!(dump(&c, false), rows);
    drop(plan);
    drop(reset);
    drop(tx);
    drop(c);
    assert_eq!(ledger(&w.store), before);
    assert_integrity(&w.store);
}

#[test]
fn both_public_receivers_finish_planning_before_any_ordinary_effect() {
    for combined in [false, true] {
        let w = World::new();
        w.record(
            IngestScope::Directory,
            &[("a.md", "SYNTHETIC_A")],
            &[w.notes],
            100,
        );
        let before = ledger(&w.store);
        let stages = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::clone(&stages);
        let saved = Arc::new(Mutex::new(None));
        let state = Arc::clone(&saved);
        let obs = w.observation(
            IngestScope::Directory,
            &[("a.md", "SYNTHETIC_B")],
            &[w.other],
            200,
        );
        let report = with_hook(
            move |stage, c| {
                events.lock().unwrap().push(stage.to_owned());
                if stage == "before_plan" {
                    *state.lock().unwrap() =
                        Some((c.total_changes(), dump(c, false), unsafe { c.handle() }
                            as usize));
                } else if stage == "after_plan" {
                    let saved = state.lock().unwrap();
                    let (changes, rows, handle) = saved.as_ref().unwrap();
                    assert_eq!(c.total_changes(), *changes);
                    assert_eq!(dump(c, false), *rows);
                    assert_eq!(unsafe { c.handle() } as usize, *handle);
                    assert!(!c.is_autocommit());
                }
                Ok(())
            },
            || {
                if combined {
                    w.store.record_files_ingest(obs, None, &|| Ok(()))
                } else {
                    w.store.record_observation(obs)
                }
            },
        )
        .unwrap();
        assert_eq!(*stages.lock().unwrap(), ["before_plan", "after_plan"]);
        assert!(matches!(report.files[0].outcome, FileOutcome::Changed));
        assert_eq!(ledger(&w.store), (before.0 + 1, before.1 + 1));
        assert_integrity(&w.store);
    }
}

#[test]
fn a_preapply_refusal_rolls_back_without_session_scope_or_durable_revision() {
    for combined in [false, true] {
        let w = World::new();
        let before = ledger(&w.store);
        let rows = dump(&w.store.connection().unwrap(), true);
        let reached = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&reached);
        let obs = w.observation(
            IngestScope::Directory,
            &[("a.md", "SYNTHETIC_A")],
            &[w.notes],
            100,
        );
        let result = with_hook(
            move |stage, _| {
                if stage == "after_plan" {
                    count.fetch_add(1, AtomicOrdering::SeqCst);
                    return Err(Error::Storage("synthetic refusal before apply".into()));
                }
                Ok(())
            },
            || {
                if combined {
                    w.store.record_files_ingest(obs, None, &|| Ok(()))
                } else {
                    w.store.record_observation(obs)
                }
            },
        );
        assert!(
            matches!(result,Err(Error::Storage(ref text)) if text=="synthetic refusal before apply")
        );
        assert_eq!(reached.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(ledger(&w.store), before);
        assert_eq!(dump(&w.store.connection().unwrap(), true), rows);
        assert!(w.store.connection().unwrap().is_autocommit());
        w.store
            .set_meta("fixture.unrelated", "SYNTHETIC_OK")
            .unwrap();
    }
}

#[test]
fn a_failed_hash_read_aborts_instead_of_becoming_missing_evidence() {
    let w = World::new();
    w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
    let before = ledger(&w.store);
    let rows = dump(&w.store.connection().unwrap(), true);
    let hit = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hit);
    let result = with_hook(
        move |stage, c| {
            if stage == "before_plan" {
                let count = Arc::clone(&count);
                c.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Read {
                        table_name: "sources",
                        column_name: "content_hash",
                    } => {
                        count.fetch_add(1, AtomicOrdering::SeqCst);
                        Authorization::Deny
                    }
                    _ => Authorization::Allow,
                }))
                .map_err(dberr)?;
            }
            Ok(())
        },
        || {
            w.store.record_observation(w.observation(
                IngestScope::Directory,
                &[("renamed.md", "SYNTHETIC_A")],
                &[w.other],
                200,
            ))
        },
    );
    {
        let c = w.store.connection().unwrap();
        clear_authorizer(&c);
        assert!(c.is_autocommit());
    }
    assert!(matches!(result, Err(Error::Storage(_))), "{result:?}");
    assert!(hit.load(AtomicOrdering::SeqCst) > 0);
    assert_eq!(ledger(&w.store), before);
    assert_eq!(dump(&w.store.connection().unwrap(), true), rows);
}

#[test]
fn move_classification_reuses_the_first_fallible_hash_observation() {
    let w = World::new();
    let first = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
    let c = w.store.connection().unwrap();
    let tx = begin_write(&c, &w.store.revision_state).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&reads);
    c.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Read {
            table_name: "sources",
            column_name: "content_hash",
        } => {
            if count.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Allow,
    }))
    .unwrap();
    let reset = AuthorizerReset(&c);
    let plan = plan_file_observation(
        &tx,
        w.observation(
            IngestScope::Directory,
            &[("renamed.md", "SYNTHETIC_A")],
            &[w.notes],
            200,
        ),
    )
    .unwrap();
    assert!(
        matches!(&plan.classifications[0],Plan::Continue{logical,from:Some(from),..} if *logical==first.files[0].logical_id && from==&format!("{ROOT}/a.md"))
    );
    assert!(plan.residual_disappearances.is_empty());
    assert_eq!(reads.load(AtomicOrdering::SeqCst), 1);
    drop(reset);
    let result = apply_file_observation(plan).unwrap();
    tx.commit().unwrap();
    drop(c);
    assert!(matches!(
        result.files[0].outcome,
        FileOutcome::Continued { .. }
    ));
    assert_integrity(&w.store);
}

#[test]
fn missing_version_hash_is_optional_but_corrupt_lineage_is_an_error() {
    let w = World::new();
    let logical = Uuid::now_v7();
    {
        let c = w.store.connection().unwrap();
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        tx.execute("INSERT INTO logical_sources(id,kind,state,current_location,created_at) VALUES(?1,'file','present',?2,1)",rusqlite::params![logical.to_string(),format!("{ROOT}/a.md")]).unwrap();
        tx.commit().unwrap();
    }
    {
        let c = w.store.connection().unwrap();
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        let plan = plan_file_observation(
            &tx,
            w.observation(
                IngestScope::File,
                &[("a.md", "SYNTHETIC_A")],
                &[w.notes],
                100,
            ),
        )
        .unwrap();
        assert!(matches!(&plan.classifications[0],Plan::Changed{logical:id,..}if *id==logical));
        drop(plan);
        drop(tx);
    }
    {
        let c = w.store.connection().unwrap();
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        tx.execute("INSERT INTO logical_sources(id,kind,state,current_location,created_at) VALUES('SYNTHETIC_CORRUPT_ID','file','present',?1,1)",[format!("{ROOT}/bad.md")]).unwrap();
        tx.commit().unwrap();
    }
    let before = ledger(&w.store);
    let result =
        w.store
            .record_observation(w.observation(IngestScope::Directory, &[], &[w.notes], 200));
    assert!(matches!(result,Err(Error::Storage(ref detail))if detail.contains("corrupt id")));
    assert_eq!(ledger(&w.store), before);
}

#[test]
fn actual_read_io_failure_during_planning_has_no_durable_effects() {
    let w = World::new();
    w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
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
        let tx = begin_write(&c, &w.store.revision_state).unwrap();
        // SAFETY: exclusive Store guard, no active statements; release only clean cached pages.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_db_release_memory(c.handle()) },
            rusqlite::ffi::SQLITE_OK
        );
        {
            let _fault = settings_tests::FileReadFault::install(&c);
            let result = plan_file_observation(
                &tx,
                w.observation(
                    IngestScope::Directory,
                    &[("renamed.md", "SYNTHETIC_A")],
                    &[w.notes],
                    200,
                ),
            );
            assert!(matches!(result, Err(Error::Storage(_))));
            // SAFETY: still-live exclusively owned SQLite handle, before another SQL operation.
            assert_eq!(
                unsafe { rusqlite::ffi::sqlite3_extended_errcode(c.handle()) },
                rusqlite::ffi::SQLITE_IOERR_READ
            );
        }
        drop(tx);
    }
    let reopened = w.writer();
    assert_eq!(ledger(&reopened), before);
    assert_integrity(&reopened);
}

#[test]
fn full_and_partial_plans_preserve_ambiguity_replacement_and_disappearance_rules() {
    let w = World::new();
    let first = w.record(
        IngestScope::Directory,
        &[
            ("a.md", "SYNTHETIC_SAME"),
            ("b.md", "SYNTHETIC_SAME"),
            ("gone.md", "SYNTHETIC_GONE"),
        ],
        &[w.notes],
        100,
    );
    let c = w.store.connection().unwrap();
    let tx = begin_write(&c, &w.store.revision_state).unwrap();
    let plan = plan_file_observation(
        &tx,
        w.observation(
            IngestScope::Directory,
            &[("new.md", "SYNTHETIC_SAME")],
            &[w.other],
            200,
        ),
    )
    .unwrap();
    assert!(matches!(plan.classifications[0], Plan::Created { .. }));
    assert_eq!(plan.residual_disappearances.len(), 3);
    let report = apply_file_observation(plan).unwrap();
    tx.commit().unwrap();
    drop(c);
    assert!(matches!(report.files[0].outcome, FileOutcome::Created));
    assert_eq!(report.events.len(), 3);
    assert!(!first
        .files
        .iter()
        .any(|f| f.logical_id == report.files[0].logical_id));
    let partial = w.record(
        IngestScope::File,
        &[("gone.md", "SYNTHETIC_DIFFERENT")],
        &[w.notes],
        300,
    );
    assert!(matches!(partial.files[0].outcome, FileOutcome::Created));
    assert!(partial.events.is_empty());
    let old = first
        .files
        .iter()
        .find(|f| f.location.ends_with("/gone.md"))
        .unwrap();
    assert_ne!(partial.files[0].logical_id, old.logical_id);
    assert_integrity(&w.store);
}

#[test]
fn partial_observation_never_infers_a_move_or_marks_other_paths_absent() {
    let w = World::new();
    let seed = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A"), ("keep.md", "SYNTHETIC_KEEP")],
        &[w.notes],
        100,
    );
    let moved = w.record(
        IngestScope::File,
        &[("new.md", "SYNTHETIC_A")],
        &[w.notes],
        200,
    );
    assert!(matches!(moved.files[0].outcome, FileOutcome::Created));
    assert!(moved.events.is_empty());
    for old in seed.files {
        assert_eq!(
            w.store
                .get_source_lifecycle(old.logical_id)
                .unwrap()
                .unwrap()
                .logical
                .state,
            SourcePresence::Present
        );
    }
    assert_integrity(&w.store);
}

#[test]
fn continued_files_do_not_reconcile_but_unchanged_files_add_membership_and_recreate_retired_memory()
{
    let w = World::new();
    let seed = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
    let initial = w.store.list_memories_by_subject("topic").unwrap().remove(0);
    let detail = w
        .store
        .get_memory_detail(initial.memory_id)
        .unwrap()
        .unwrap();
    w.store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id: detail.assertion.id,
            derivation_key: "synthetic-observation@1".into(),
            model: "synthetic".into(),
            model_version: "1".into(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let moved = w.record(
        IngestScope::Directory,
        &[("b.md", "SYNTHETIC_A")],
        &[w.other],
        200,
    );
    assert_eq!(moved.files[0].logical_id, seed.files[0].logical_id);
    assert!(
        matches!(&moved.files[0].outcome,FileOutcome::Continued{from_location:Some(from)}if from==&format!("{ROOT}/a.md"))
    );
    assert!(moved.files[0].actions.is_empty());
    assert_eq!(
        w.store
            .get_memory_detail(initial.memory_id)
            .unwrap()
            .unwrap()
            .spaces
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["notes"]
    );
    let unchanged = w.record(
        IngestScope::File,
        &[("b.md", "SYNTHETIC_A")],
        &[w.other],
        300,
    );
    assert!(matches!(unchanged.files[0].outcome, FileOutcome::Unchanged));
    assert_eq!(
        w.store
            .get_memory_detail(initial.memory_id)
            .unwrap()
            .unwrap()
            .spaces
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["notes", "other"]
    );
    w.store
        .retire_memory(
            initial.memory_id,
            Some("synthetic manual retirement".into()),
            Ts(350),
        )
        .unwrap();
    let recreated = w.record(
        IngestScope::File,
        &[("b.md", "SYNTHETIC_A")],
        &[w.notes],
        400,
    );
    assert!(matches!(recreated.files[0].outcome, FileOutcome::Unchanged));
    let history = w.store.list_memories_by_subject("topic").unwrap();
    assert_eq!(history.len(), 2);
    let active = history
        .iter()
        .find(|m| m.state == MemoryState::Active)
        .unwrap();
    assert_ne!(active.memory_id, initial.memory_id);
    assert_eq!(active.assertion_id, initial.assertion_id);
    assert_eq!(
        w.store
            .embedding_index_status("synthetic-observation@1")
            .unwrap()
            .embedded,
        1
    );
    assert_integrity(&w.store);
}

#[test]
fn duplicate_input_locations_keep_the_existing_preapply_classification_policy() {
    let w = World::new();
    let before = ledger(&w.store);
    let rows = dump(&w.store.connection().unwrap(), true);
    let c = w.store.connection().unwrap();
    let tx = begin_write(&c, &w.store.revision_state).unwrap();
    let plan = plan_file_observation(
        &tx,
        w.observation(
            IngestScope::File,
            &[("same.md", "SYNTHETIC_A"), ("same.md", "SYNTHETIC_B")],
            &[w.notes],
            100,
        ),
    )
    .unwrap();
    assert_eq!(plan.classifications.len(), 2);
    assert!(plan
        .classifications
        .iter()
        .all(|p| matches!(p, Plan::Created { .. })));
    assert!(plan.files[0].content.contains("SYNTHETIC_A"));
    assert!(plan.files[1].content.contains("SYNTHETIC_B"));
    // Existing schema rejects two source observations at one location in a
    // session. Planning must neither silently deduplicate nor widen that policy.
    let result = apply_file_observation(plan);
    assert!(
        matches!(result, Err(Error::Storage(ref detail)) if detail.contains("UNIQUE constraint failed: source_observations.session_id, source_observations.location")),
        "{result:?}"
    );
    drop(tx);
    assert!(c.is_autocommit());
    drop(c);
    assert_eq!(ledger(&w.store), before);
    assert_eq!(dump(&w.store.connection().unwrap(), true), rows);
    assert_integrity(&w.store);
}

#[test]
fn a_queued_receiver_classifies_after_the_prior_writer_commits() {
    let w = World::new();
    let seed = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
    let before = ledger(&w.store);
    let queued = w.writer();
    let (entered_tx, entered_rx) = mpsc::channel();
    {
        let c = queued.connection().unwrap();
        let mut signalled = false;
        c.authorizer(Some(move |ctx: AuthContext<'_>| {
            if matches!(
                ctx.action,
                AuthAction::Transaction {
                    operation: TransactionOperation::Begin
                }
            ) && !signalled
            {
                signalled = true;
                entered_tx.send(()).unwrap();
            }
            Authorization::Allow
        }))
        .unwrap();
    }
    let queued_input = w.observation(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_B")],
        &[w.notes],
        300,
    );
    let first_input = w.observation(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_B")],
        &[w.notes],
        200,
    );
    let children = Arc::new(Mutex::new(Vec::new()));
    let spawned = Arc::clone(&children);
    let mut queued = Some(queued);
    let mut queued_input = Some(queued_input);
    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_hook(
            move |stage, _| {
                if stage == "after_plan" {
                    let queued = queued.take().unwrap();
                    let input = queued_input.take().unwrap();
                    spawned
                        .lock()
                        .unwrap()
                        .push(std::thread::spawn(move || queued.record_observation(input)));
                    entered_rx.recv_timeout(WAIT).unwrap();
                }
                Ok(())
            },
            || w.store.record_observation(first_input),
        )
    }));
    let results = children
        .lock()
        .unwrap()
        .drain(..)
        .map(|child| child.join())
        .collect::<Vec<_>>();
    let first = match applied {
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(results.len(), 1);
    let second = results.into_iter().next().unwrap().unwrap().unwrap();
    assert!(matches!(first.files[0].outcome, FileOutcome::Changed));
    assert!(matches!(second.files[0].outcome, FileOutcome::Unchanged));
    assert_eq!(second.files[0].logical_id, seed.files[0].logical_id);
    assert_eq!(ledger(&w.store), (before.0 + 2, before.1 + 2));
    assert_integrity(&w.store);
}

#[test]
fn an_existing_writer_lock_returns_busy_without_planning_or_durable_effects() {
    let w = World::new();
    let before = ledger(&w.store);
    {
        let c = w.store.connection().unwrap();
        c.busy_timeout(Duration::from_millis(20)).unwrap();
    }
    let blocker = w.writer();
    let c = blocker.connection().unwrap();
    let tx = begin_write(&c, &blocker.revision_state).unwrap();
    let reached = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&reached);
    let result = with_hook(
        move |_, _| {
            count.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        },
        || {
            w.store.record_observation(w.observation(
                IngestScope::Directory,
                &[("a.md", "SYNTHETIC_A")],
                &[w.notes],
                100,
            ))
        },
    );
    assert!(
        matches!(result, Err(Error::DatabaseBusy { .. })),
        "{result:?}"
    );
    assert_eq!(reached.load(AtomicOrdering::SeqCst), 0);
    drop(tx);
    drop(c);
    assert_eq!(ledger(&w.store), before);
}

#[test]
fn a_connection_local_commit_veto_rolls_back_the_planned_receiver() {
    let w = World::new();
    let before = ledger(&w.store);
    let rows = dump(&w.store.connection().unwrap(), true);
    {
        let c = w.store.connection().unwrap();
        c.commit_hook(Some(|| true)).unwrap();
    }
    let result = w.store.record_observation(w.observation(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    ));
    {
        let c = w.store.connection().unwrap();
        register_revision_context(&c, &w.store.revision_state).unwrap();
        assert!(c.is_autocommit());
    }
    assert!(
        result.is_err() && !matches!(result, Err(Error::CommitOutcomeUnknown { .. })),
        "{result:?}"
    );
    assert_eq!(ledger(&w.store), before);
    assert_eq!(dump(&w.store.connection().unwrap(), true), rows);
    w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    );
    assert_eq!(ledger(&w.store), (before.0 + 1, before.1 + 1));
}

#[cfg(feature = "spawn-gate")]
#[test]
fn committed_before_ack_loss_is_not_reported_as_rollback() {
    const CHILD: &str = "MINDSPACE_OBSERVATION_PLANNING_ACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let logs = tempfile::tempdir().unwrap();
        let stdout = std::fs::File::create(logs.path().join("stdout")).unwrap();
        let stderr = std::fs::File::create(logs.path().join("stderr")).unwrap();
        let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","storage::sqlite::observation_planning_tests::committed_before_ack_loss_is_not_reported_as_rollback","--nocapture","--test-threads=1"]).env_clear().env("PATH","/usr/bin:/bin").env("HOME",logs.path()).env("TMPDIR",logs.path()).env(CHILD,"1").stdout(stdout).stderr(stderr).spawn().unwrap();
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("bounded isolated lost-ack test timed out")
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.success(),
            "child stdout={} stderr={}",
            std::fs::read_to_string(logs.path().join("stdout")).unwrap(),
            std::fs::read_to_string(logs.path().join("stderr")).unwrap()
        );
        return;
    }
    let w = World::new();
    let before = ledger(&w.store);
    gate::LOSE_ACK.store(true, Ordering::SeqCst);
    let result = w.store.record_observation(w.observation(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_A")],
        &[w.notes],
        100,
    ));
    assert!(
        matches!(result, Err(Error::CommitOutcomeUnknown { .. })),
        "{result:?}"
    );
    assert!(matches!(
        w.store.set_meta("fixture.after_unknown", "SYNTHETIC"),
        Err(Error::ConnectionDiscarded)
    ));
    let reopened = w.writer();
    assert_eq!(ledger(&reopened), (before.0 + 1, before.1 + 1));
    let retry = reopened
        .record_observation(observation(
            IngestScope::Directory,
            &[("a.md", "SYNTHETIC_A")],
            &[w.notes],
            100,
        ))
        .unwrap();
    assert!(matches!(retry.files[0].outcome, FileOutcome::Unchanged));
    assert_eq!(ledger(&reopened), (before.0 + 2, before.1 + 2));
    assert_integrity(&reopened);
}

#[test]
fn modified_disappeared_and_reappeared_files_preserve_historical_vector_bytes_and_history() {
    let w = World::new();
    let original = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_VECTOR_OLD")],
        &[w.notes],
        100,
    );
    let old = w.store.list_memories_by_subject("topic").unwrap().remove(0);
    let key = "synthetic-observation-history@1";
    let expected = encode_f32_le(&[1.0, 0.0]).unwrap();
    w.store
        .save_embeddings(&[AssertionEmbedding {
            assertion_id: old.assertion_id,
            derivation_key: key.into(),
            model: "synthetic".into(),
            model_version: "1".into(),
            dimension: 2,
            embedding: vec![1.0, 0.0],
        }])
        .unwrap();
    let changed = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_VECTOR_NEW")],
        &[w.notes],
        200,
    );
    assert!(matches!(changed.files[0].outcome, FileOutcome::Changed));
    assert_eq!(changed.files[0].logical_id, original.files[0].logical_id);
    let absent = w.record(IngestScope::Directory, &[], &[w.notes], 300);
    assert_eq!(absent.events.len(), 1);
    assert!(matches!(
        absent.events[0].kind,
        SourceEventKind::Disappeared { .. }
    ));
    let returned = w.record(
        IngestScope::Directory,
        &[("a.md", "SYNTHETIC_VECTOR_NEW")],
        &[w.notes],
        400,
    );
    assert!(
        matches!(&returned.files[0].outcome, FileOutcome::Continued { from_location: Some(from) } if from == &format!("{ROOT}/a.md"))
    );
    assert_eq!(returned.files[0].logical_id, original.files[0].logical_id);
    assert!(returned.files[0].actions.is_empty());
    let history = w.store.list_memories_by_subject("topic").unwrap();
    assert_eq!(history.len(), 2);
    assert!(history
        .iter()
        .any(|m| m.memory_id == old.memory_id && m.state == MemoryState::Superseded));
    assert_eq!(
        history
            .iter()
            .filter(|m| m.state == MemoryState::Active)
            .count(),
        1
    );
    let status = w.store.embedding_index_status(key).unwrap();
    assert_eq!((status.total_assertions, status.embedded), (2, 1));
    let bytes: Vec<u8> = w.store.connection().unwrap().query_row(
        "SELECT embedding FROM assertion_embeddings WHERE assertion_id=?1 AND derivation_key=?2",
        rusqlite::params![old.assertion_id.to_string(), key],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(bytes, expected);
    let historic = w
        .store
        .search_vectors(
            &[1.0, 0.0],
            &SearchQuery {
                text: "SYNTHETIC_VECTOR_OLD".into(),
                space_ids: None,
                limit: 10,
                at: None,
                include_superseded: true,
                include_retired: true,
            },
            key,
        )
        .unwrap();
    assert_eq!(historic.len(), 1);
    assert_eq!(historic[0].memory_id, old.memory_id);
    assert_eq!(historic[0].score, 1.0);
    assert_integrity(&w.store);
}
