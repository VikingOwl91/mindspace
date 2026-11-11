//! Atomic legacy settings reads through the actual Store/M6 connection.
//! Fixtures are disposable; pair writers below are test arrangements using the
//! existing private M6 transaction, not a new production write API.

use super::*;
use crate::core::EmbeddingSettings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{mpsc, Arc};
use std::time::Instant;
use tempfile::TempDir;

const ENABLED: &str = "embedding.enabled";
const MODEL: &str = "embedding.model";
const WAIT: Duration = Duration::from_secs(15);

impl SqliteStore {
    /// Malformed-value fixture for storage and operational-caller unit tests.
    /// The normal M6 scope and persistent guards are retained.
    pub(crate) fn set_meta_blob_for_test(&self, key: &str, value: &[u8]) -> Result<()> {
        let c = self.connection()?;
        let tx = begin_write(&c, &self.revision_state)?;
        tx.execute(
            "INSERT INTO mind_meta(key,value) VALUES(?1,?2) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(dberr)?;
        tx.commit()
    }
}

fn settings(enabled: Option<&str>, model: Option<&str>) -> EmbeddingSettings {
    EmbeddingSettings {
        enabled: enabled.map(str::to_owned),
        model: model.map(str::to_owned),
    }
}

fn fixture() -> (TempDir, PathBuf, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    let store = SqliteStore::open(&path).unwrap();
    store
        .create_space("notes", "synthetic settings fixture")
        .unwrap();
    let note = dir.path().join("note.md");
    std::fs::write(&note, "# Synthetic\n\nsettingssnapshotcanary.\n").unwrap();
    crate::ingest::ingest_file(&store, &note, &["notes"]).unwrap();
    store
        .set_meta("fixture.unrelated", "SYNTHETIC_UNRELATED")
        .unwrap();
    (dir, path, store)
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

fn write_pair(store: &SqliteStore, pair: &EmbeddingSettings) {
    let c = store.connection().unwrap();
    let tx = begin_write(&c, &store.revision_state).unwrap();
    apply_pair(&tx, pair);
    tx.commit().unwrap();
}

fn apply_pair(c: &Connection, pair: &EmbeddingSettings) {
    for (key, value) in [(ENABLED, &pair.enabled), (MODEL, &pair.model)] {
        match value {
            Some(value) => {
                c.execute(
                    "INSERT INTO mind_meta(key,value) VALUES(?1,?2) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    rusqlite::params![key, value],
                )
                .unwrap();
            }
            None => {
                c.execute("DELETE FROM mind_meta WHERE key=?1", [key])
                    .unwrap();
            }
        }
    }
}

#[test]
fn atomic_settings_preserve_raw_optional_values_and_read_without_recording_revision() {
    let (_dir, _path, store) = fixture();
    for pair in [
        settings(None, None),
        settings(Some("true"), None),
        settings(None, Some("SYNTHETIC_MODEL")),
        settings(Some("false"), Some("")),
        settings(
            Some("arbitrary-enabled"),
            Some("SYNTHETIC_\0_\n_\u{202e}_漢字"),
        ),
    ] {
        write_pair(&store, &pair);
        let before = ledger(&store);
        assert_eq!(store.read_embedding_settings().unwrap(), pair);
        assert_eq!(store.read_embedding_settings().unwrap(), pair);
        assert_eq!(ledger(&store), before, "reads must allocate no R");
    }
    assert_eq!(
        store.get_meta("fixture.unrelated").unwrap().as_deref(),
        Some("SYNTHETIC_UNRELATED")
    );
    let c = store.connection().unwrap();
    assert_eq!(
        c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        17
    );
    validate_fence_inventory(&c).unwrap();
}

#[test]
fn atomic_settings_do_not_change_write_guards_accounting_or_unrelated_knowledge() {
    let (dir, _path, store) = fixture();
    let before = ledger(&store);
    store.set_meta(ENABLED, "true").unwrap();
    assert_eq!(ledger(&store), (before.0 + 1, before.1 + 1));
    store.set_meta(MODEL, "SYNTHETIC_MODEL").unwrap();
    let after_writes = ledger(&store);
    assert_eq!(after_writes, (before.0 + 2, before.1 + 2));
    assert_eq!(
        store.read_embedding_settings().unwrap(),
        settings(Some("true"), Some("SYNTHETIC_MODEL"))
    );
    assert_eq!(ledger(&store), after_writes);
    {
        let c = store.connection().unwrap();
        assert!(c
            .execute(
                "UPDATE mind_meta SET value='SYNTHETIC_UNAUTHORIZED' WHERE key='embedding.model'",
                []
            )
            .is_err());
        let tx = begin_write(&c, &store.revision_state).unwrap();
        apply_pair(&tx, &settings(Some("false"), Some("SYNTHETIC_ROLLBACK")));
        // Drop exercises the existing M6 rollback and active-context cleanup.
    }
    assert_eq!(ledger(&store), after_writes);
    assert_eq!(
        store.read_embedding_settings().unwrap(),
        settings(Some("true"), Some("SYNTHETIC_MODEL"))
    );
    let count: i64 = store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    let note = dir.path().join("another.md");
    std::fs::write(&note, "# More synthetic\n\nsettingsordinarywritecanary.\n").unwrap();
    crate::ingest::ingest_file(&store, &note, &["notes"]).unwrap();
    assert!(
        store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_, i64>(0))
            .unwrap()
            > count
    );
    store.verify_fts_index().unwrap();
}

#[test]
fn connection_scoped_settings_helper_keeps_the_callers_held_snapshot() {
    let (_dir, path, writer) = fixture();
    let old = settings(Some("false"), Some("SYNTHETIC_OLD"));
    let new = settings(Some("true"), Some("SYNTHETIC_NEW"));
    write_pair(&writer, &old);
    let reader = SqliteStore::open(&path).unwrap();
    let c = reader.connection().unwrap();
    let tx = Transaction::new_unchecked(&c, TransactionBehavior::Deferred).unwrap();
    assert_eq!(read_embedding_settings_on(&tx).unwrap(), old);
    write_pair(&writer, &new);
    assert_eq!(read_embedding_settings_on(&tx).unwrap(), old);
    tx.commit().unwrap();
    assert_eq!(read_embedding_settings_on(&c).unwrap(), new);
    drop(c);
    assert_eq!(reader.read_embedding_settings().unwrap(), new);
}

#[test]
fn reader_sees_old_pair_while_real_m6_writer_has_half_or_all_changes_uncommitted() {
    let (_dir, path, reader) = fixture();
    let old = settings(Some("false"), Some("SYNTHETIC_OLD"));
    let new = settings(Some("true"), Some("SYNTHETIC_NEW"));
    write_pair(&reader, &old);
    let writer = SqliteStore::open(&path).unwrap();
    let (event_tx, event_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let join = std::thread::spawn(move || {
        let c = writer.connection().unwrap();
        let tx = begin_write(&c, &writer.revision_state).unwrap();
        tx.execute(
            "UPDATE mind_meta SET value='true' WHERE key='embedding.enabled'",
            [],
        )
        .unwrap();
        event_tx.send("half").unwrap();
        release_rx.recv_timeout(WAIT).unwrap();
        tx.execute(
            "UPDATE mind_meta SET value='SYNTHETIC_NEW' WHERE key='embedding.model'",
            [],
        )
        .unwrap();
        event_tx.send("whole-uncommitted").unwrap();
        release_rx.recv_timeout(WAIT).unwrap();
        tx.commit().unwrap();
        event_tx.send("committed").unwrap();
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(event_rx.recv_timeout(WAIT).unwrap(), "half");
        assert_eq!(reader.read_embedding_settings().unwrap(), old);
        release_tx.send(()).unwrap();
        assert_eq!(event_rx.recv_timeout(WAIT).unwrap(), "whole-uncommitted");
        assert_eq!(reader.read_embedding_settings().unwrap(), old);
        release_tx.send(()).unwrap();
        assert_eq!(event_rx.recv_timeout(WAIT).unwrap(), "committed");
        assert_eq!(reader.read_embedding_settings().unwrap(), new);
    }));
    drop(release_tx);
    let child = join.join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    child.unwrap();
}

#[test]
fn concurrent_atomic_pair_commits_never_produce_a_split_observation() {
    let (_dir, path, reader) = fixture();
    let a = settings(Some("false"), Some("SYNTHETIC_A"));
    let b = settings(Some("true"), Some("SYNTHETIC_B"));
    write_pair(&reader, &a);
    let writer = SqliteStore::open(&path).unwrap();
    let (event_tx, event_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let completed = Arc::new(AtomicUsize::new(0));
    let progress = Arc::clone(&completed);
    let thread_a = a.clone();
    let thread_b = b.clone();
    let join = std::thread::spawn(move || {
        for pair in [&thread_b, &thread_a] {
            write_pair(&writer, pair);
            event_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
        }
        for i in 0..256 {
            write_pair(&writer, if i % 2 == 0 { &thread_b } else { &thread_a });
            progress.store(i + 1, AtomicOrdering::Release);
        }
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut saw_a = false;
        let mut saw_b = false;
        for expected in [&b, &a] {
            event_rx.recv_timeout(WAIT).unwrap();
            let value = reader.read_embedding_settings().unwrap();
            assert_eq!(&value, expected);
            saw_a |= value == a;
            saw_b |= value == b;
            release_tx.send(()).unwrap();
        }
        let deadline = Instant::now() + WAIT;
        let mut samples = 0;
        while completed.load(AtomicOrdering::Acquire) < 256 || samples < 256 {
            assert!(
                Instant::now() < deadline,
                "bounded concurrent settings observations"
            );
            let value = reader.read_embedding_settings().unwrap();
            assert!(value == a || value == b, "split committed pair: {value:?}");
            saw_a |= value == a;
            saw_b |= value == b;
            samples += 1;
        }
        assert!(
            saw_a && saw_b,
            "positive controls require both committed states"
        );
    }));
    drop(release_tx);
    let child = join.join();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    child.unwrap();
}

#[test]
fn coherent_read_does_not_make_separate_setting_writes_atomic() {
    let (_dir, _path, store) = fixture();
    write_pair(&store, &settings(Some("false"), Some("SYNTHETIC_OLD")));
    let first = store.get_meta(ENABLED).unwrap();
    store.set_meta(ENABLED, "true").unwrap();
    assert_eq!(
        store.read_embedding_settings().unwrap(),
        settings(Some("true"), Some("SYNTHETIC_OLD")),
        "this is a real committed intermediate state"
    );
    store.set_meta(MODEL, "SYNTHETIC_NEW").unwrap();
    let second = store.get_meta(MODEL).unwrap();
    assert_eq!(first.as_deref(), Some("false"));
    assert_eq!(second.as_deref(), Some("SYNTHETIC_NEW"));
    assert_eq!(
        store.read_embedding_settings().unwrap(),
        settings(Some("true"), Some("SYNTHETIC_NEW"))
    );
}

#[test]
fn malformed_settings_and_missing_storage_return_errors_not_absent_values() {
    for key in [ENABLED, MODEL] {
        let (_dir, _path, store) = fixture();
        write_pair(&store, &settings(Some("false"), Some("SYNTHETIC")));
        store
            .set_meta_blob_for_test(key, b"SYNTHETIC_BLOB")
            .unwrap();
        let before = ledger(&store);
        assert!(matches!(
            store.read_embedding_settings(),
            Err(Error::Storage(_))
        ));
        assert_eq!(ledger(&store), before);
    }
    let (_dir, _path, store) = fixture();
    let before = ledger(&store);
    store
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE mind_meta")
        .unwrap();
    assert!(matches!(
        store.read_embedding_settings(),
        Err(Error::Storage(_))
    ));
    assert_eq!(ledger(&store), before);
}

#[test]
fn busy_read_is_reported_without_a_revision_and_recovers_after_lock_release() {
    let (_dir, path, store) = fixture();
    let pair = settings(Some("true"), Some("SYNTHETIC_BUSY"));
    write_pair(&store, &pair);
    let before = ledger(&store);
    {
        let c = store.connection().unwrap();
        let mode: String = c
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        c.busy_timeout(Duration::from_millis(20)).unwrap();
    }
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let result = store.read_embedding_settings();
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(
        matches!(result, Err(Error::DatabaseBusy { .. })),
        "{result:?}"
    );
    assert_eq!(store.read_embedding_settings().unwrap(), pair);
    assert_eq!(ledger(&store), before);
}

// The fault is local to one SQLite file handle owned by one locked Store
// connection. It neither registers a global VFS nor changes production code.
static INJECTED_READ_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn fail_settings_file_read(
    _file: *mut rusqlite::ffi::sqlite3_file,
    _buffer: *mut std::ffi::c_void,
    _amount: std::ffi::c_int,
    _offset: rusqlite::ffi::sqlite3_int64,
) -> std::ffi::c_int {
    INJECTED_READ_CALLS.fetch_add(1, AtomicOrdering::Relaxed);
    rusqlite::ffi::SQLITE_IOERR_READ
}

struct FileReadFault<'connection> {
    file: *mut rusqlite::ffi::sqlite3_file,
    original: *const rusqlite::ffi::sqlite3_io_methods,
    _methods: Box<rusqlite::ffi::sqlite3_io_methods>,
    _connection: std::marker::PhantomData<&'connection Connection>,
}

impl<'connection> FileReadFault<'connection> {
    fn install(connection: &'connection Connection) -> Self {
        let mut file: *mut rusqlite::ffi::sqlite3_file = std::ptr::null_mut();
        // SAFETY: the caller retains the Store mutex guard for this connection,
        // with no active statements. SQLITE_FCNTL_FILE_POINTER writes one file
        // pointer to the correctly sized out-parameter. The connection outlives
        // this guard, so SQLite cannot close the file before restoration.
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_FILE_POINTER,
                std::ptr::addr_of_mut!(file).cast(),
            )
        };
        assert_eq!(result, rusqlite::ffi::SQLITE_OK);
        assert!(!file.is_null());
        // SAFETY: successful FILE_POINTER provides the live connection's file.
        let original = unsafe { (*file).pMethods };
        assert!(!original.is_null());
        // Version 3 guarantees all fields of the current io-method struct exist.
        // Failing this precondition is an unsupported test environment, not PASS.
        assert!(unsafe { (*original).iVersion } >= 3);
        // SAFETY: version/nonnull checks above cover the complete struct copy.
        // The Box retains its stable address until Drop restores the old table.
        let mut methods = Box::new(unsafe { *original });
        methods.xRead = Some(fail_settings_file_read);
        // SAFETY: exclusive connection ownership prevents any concurrent method
        // invocation during the pointer replacement. Only this file is affected.
        unsafe { (*file).pMethods = std::ptr::from_ref(methods.as_ref()) };
        Self {
            file,
            original,
            _methods: methods,
            _connection: std::marker::PhantomData,
        }
    }
}

impl Drop for FileReadFault<'_> {
    fn drop(&mut self) {
        // SAFETY: the borrowed connection still owns the file, and the Store
        // mutex is still held. Restore before the copied methods Box is freed,
        // including assertion unwinds; closing SQLite never sees a dangling table.
        unsafe { (*self.file).pMethods = self.original };
    }
}

#[test]
fn actual_sqlite_read_io_error_propagates_instead_of_missing_settings() {
    let (_dir, _path, store) = fixture();
    let expected = settings(Some("true"), Some("SYNTHETIC_IO"));
    write_pair(&store, &expected);
    let before = ledger(&store);
    let c = store.connection().unwrap();
    let mode: String = c
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    // Force actual MAIN-file reads instead of mmap or clean pager-cache hits.
    c.execute_batch("PRAGMA mmap_size=0; PRAGMA shrink_memory")
        .unwrap();
    // SAFETY: the connection is exclusively held, with no live statements.
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_db_release_memory(c.handle()) },
        rusqlite::ffi::SQLITE_OK
    );
    INJECTED_READ_CALLS.store(0, AtomicOrdering::Relaxed);
    {
        let _fault = FileReadFault::install(&c);
        let result = read_embedding_settings_on(&c);
        assert!(matches!(result, Err(Error::Storage(_))), "{result:?}");
        assert!(INJECTED_READ_CALLS.load(AtomicOrdering::Relaxed) > 0);
        // SAFETY: same exclusively owned, still-open SQLite handle. Inspect the
        // actual engine result before another SQL operation can overwrite it.
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_extended_errcode(c.handle()) },
            rusqlite::ffi::SQLITE_IOERR_READ
        );
    }
    // The original methods have been restored before reuse or connection close.
    assert_eq!(read_embedding_settings_on(&c).unwrap(), expected);
    drop(c);
    assert_eq!(ledger(&store), before);
}
