// DO NOT APPLY: rejected candidate block is for disposable archive replay only.
// Test-only template: runner selects one marked block.
// Actual public M6 Store receiver; RAII fault injection changes no production decisions.
// PASS reproduces the rejected candidate incompatibility, not isolation.

// S3E4D_BASELINE_BEGIN
// Exact public Store receiver with a per-thread, test-only native xRead fault.
thread_local! {
    static REVIEW_FATAL_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REVIEW_FATAL_OBSERVED: RefCell<Option<(bool,i32)>> = const { RefCell::new(None) };
}
pub(super) struct ReviewFatalGuard<'conn> {
    _fault: settings_tests::FileReadFault<'conn>,
    conn: &'conn Connection,
}
impl Drop for ReviewFatalGuard<'_> {
    fn drop(&mut self) {
        let value = (self.conn.is_autocommit(), unsafe {
            rusqlite::ffi::sqlite3_extended_errcode(self.conn.handle())
        });
        REVIEW_FATAL_OBSERVED.with(|s| *s.borrow_mut() = Some(value));
    }
}
pub(super) fn s3e4d_review_fatal_guard(conn: &Connection) -> Option<ReviewFatalGuard<'_>> {
    if !REVIEW_FATAL_ARMED.with(|s| s.replace(false)) {
        return None;
    }
    assert!(
        !conn.is_autocommit(),
        "fault starts within actual M6 transaction"
    );
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_db_release_memory(conn.handle()) },
        rusqlite::ffi::SQLITE_OK
    );
    Some(ReviewFatalGuard {
        _fault: settings_tests::FileReadFault::install(conn),
        conn,
    })
}
#[test]
fn s3e4d_review_fatal_summary_read() {
    let seconds = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let nanos = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let fixed_ms = seconds * 1000 + nanos / 1_000_000;
    let slice = |id: Uuid| (id.as_u128() >> 36) as u64 & ((1 << 26) - 1);
    let adjacent_before = Uuid::now_v7();
    let adjacent_after = Uuid::now_v7();
    assert_eq!((adjacent_before.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!((adjacent_after.as_u128() >> 80) as u64, fixed_ms);
    let adjacent_gap = slice(adjacent_after).wrapping_sub(slice(adjacent_before)) & ((1 << 26) - 1);
    assert_eq!(adjacent_gap, 1);
    let w = World::new();
    w.record(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_OLD")],
        &[w.notes],
        100,
    );
    let ledger_before = ledger(&w.store);
    let rows_before = dump(&w.store.connection().unwrap(), true);
    {
        let c = w.store.connection().unwrap();
        assert_eq!(
            c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        c.execute_batch("PRAGMA mmap_size=0; PRAGMA shrink_memory")
            .unwrap();
    }
    let input = w.observation(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_CHANGED")],
        &[w.notes],
        200,
    );
    let before = Uuid::now_v7();
    REVIEW_FATAL_ARMED.with(|s| s.set(true));
    let result = w.store.record_observation(input);
    let after = Uuid::now_v7();
    assert!(
        !REVIEW_FATAL_ARMED.with(|s| s.get()),
        "exact target boundary must be hit"
    );
    let (auto_after_fault, extended) = REVIEW_FATAL_OBSERVED
        .with(|s| s.borrow_mut().take())
        .expect("fault guard completed");
    let error = format!("{result:?}");
    let failed_gap = slice(after).wrapping_sub(slice(before)) & ((1 << 26) - 1);
    let autocommit_after_cleanup = w.store.connection().unwrap().is_autocommit();
    let reopened = w.writer();
    let rows_after = dump(&reopened.connection().unwrap(), true);
    let ledger_after = ledger(&reopened);
    let next = reopened
        .record_observation(observation(
            IngestScope::File,
            &[("next.md", "SYNTHETIC_NEXT")],
            &[w.notes],
            300,
        ))
        .unwrap();
    let next_gap = slice(next.session_id).wrapping_sub(slice(after)) & ((1 << 26) - 1);
    assert_integrity(&reopened);
    let record = serde_json::json!({"baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857","mode":"baseline","boundary":"actual public Store::record_observation; test-only fault guard at summary entry","actual_error":error,"automatic_rollback_after_read_fault":auto_after_fault,"extended_read_code":extended,"failed_uuid_gap":failed_gap,"subsequent_session_gap":next_gap,"all_rows_rolled_back":rows_before==rows_after,"ledger_rolled_back":ledger_before==ledger_after,"autocommit_after_cleanup":autocommit_after_cleanup,"clock_validated":true,"fixed_unix_millis":fixed_ms,"adjacent_gap":adjacent_gap,"adjacent_control_gap":adjacent_gap,"before_uuid":before.to_string(),"after_uuid":after.to_string()});
    println!("PUBLIC_FATAL_READ_PROBE {}", record);
    let out = PathBuf::from(std::env::var_os("MINDSPACE_S3E4D_PROBE_OUT").unwrap())
        .join("fatal_summary_read.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(out, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    assert!(auto_after_fault);
    assert_eq!(extended, rusqlite::ffi::SQLITE_IOERR_READ);
    assert!(autocommit_after_cleanup);
    assert_eq!(rows_before, rows_after);
    assert_eq!(ledger_before, ledger_after);
    assert_eq!(next_gap, 1);
    assert_eq!((before.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!((after.as_u128() >> 80) as u64, fixed_ms);
    assert!(error.contains("disk I/O error"), "{error}");
    assert_eq!(failed_gap, 4);
}
// S3E4D_BASELINE_END

// S3E4D_CANDIDATE_BEGIN
// Exact public Store receiver with a per-thread, test-only native xRead fault.
thread_local! {
    static REVIEW_FATAL_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REVIEW_FATAL_OBSERVED: RefCell<Option<(bool,i32)>> = const { RefCell::new(None) };
}
pub(super) struct ReviewFatalGuard<'conn> {
    _fault: settings_tests::FileReadFault<'conn>,
    conn: &'conn Connection,
}
impl Drop for ReviewFatalGuard<'_> {
    fn drop(&mut self) {
        let value = (self.conn.is_autocommit(), unsafe {
            rusqlite::ffi::sqlite3_extended_errcode(self.conn.handle())
        });
        REVIEW_FATAL_OBSERVED.with(|s| *s.borrow_mut() = Some(value));
    }
}
pub(super) fn s3e4d_review_fatal_guard(conn: &Connection) -> Option<ReviewFatalGuard<'_>> {
    if !REVIEW_FATAL_ARMED.with(|s| s.replace(false)) {
        return None;
    }
    assert!(
        !conn.is_autocommit(),
        "fault starts within actual M6 transaction"
    );
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_db_release_memory(conn.handle()) },
        rusqlite::ffi::SQLITE_OK
    );
    Some(ReviewFatalGuard {
        _fault: settings_tests::FileReadFault::install(conn),
        conn,
    })
}
#[test]
fn s3e4d_review_fatal_summary_read() {
    let seconds = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let nanos = std::env::var("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let fixed_ms = seconds * 1000 + nanos / 1_000_000;
    let slice = |id: Uuid| (id.as_u128() >> 36) as u64 & ((1 << 26) - 1);
    let adjacent_before = Uuid::now_v7();
    let adjacent_after = Uuid::now_v7();
    assert_eq!((adjacent_before.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!((adjacent_after.as_u128() >> 80) as u64, fixed_ms);
    let adjacent_gap = slice(adjacent_after).wrapping_sub(slice(adjacent_before)) & ((1 << 26) - 1);
    assert_eq!(adjacent_gap, 1);
    let w = World::new();
    w.record(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_OLD")],
        &[w.notes],
        100,
    );
    let ledger_before = ledger(&w.store);
    let rows_before = dump(&w.store.connection().unwrap(), true);
    {
        let c = w.store.connection().unwrap();
        assert_eq!(
            c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        c.execute_batch("PRAGMA mmap_size=0; PRAGMA shrink_memory")
            .unwrap();
    }
    let input = w.observation(
        IngestScope::File,
        &[("a.md", "SYNTHETIC_CHANGED")],
        &[w.notes],
        200,
    );
    let before = Uuid::now_v7();
    REVIEW_FATAL_ARMED.with(|s| s.set(true));
    let result = w.store.record_observation(input);
    let after = Uuid::now_v7();
    assert!(
        !REVIEW_FATAL_ARMED.with(|s| s.get()),
        "exact target boundary must be hit"
    );
    let (auto_after_fault, extended) = REVIEW_FATAL_OBSERVED
        .with(|s| s.borrow_mut().take())
        .expect("fault guard completed");
    let error = format!("{result:?}");
    let failed_gap = slice(after).wrapping_sub(slice(before)) & ((1 << 26) - 1);
    let autocommit_after_cleanup = w.store.connection().unwrap().is_autocommit();
    let reopened = w.writer();
    let rows_after = dump(&reopened.connection().unwrap(), true);
    let ledger_after = ledger(&reopened);
    let next = reopened
        .record_observation(observation(
            IngestScope::File,
            &[("next.md", "SYNTHETIC_NEXT")],
            &[w.notes],
            300,
        ))
        .unwrap();
    let next_gap = slice(next.session_id).wrapping_sub(slice(after)) & ((1 << 26) - 1);
    assert_integrity(&reopened);
    let record = serde_json::json!({"baseline":"3f7fd3422047b40f9dc6fa787d4b523d7d90d857","mode":"candidate","boundary":"actual public Store::record_observation; test-only fault guard at summary entry","actual_error":error,"automatic_rollback_after_read_fault":auto_after_fault,"extended_read_code":extended,"failed_uuid_gap":failed_gap,"subsequent_session_gap":next_gap,"all_rows_rolled_back":rows_before==rows_after,"ledger_rolled_back":ledger_before==ledger_after,"autocommit_after_cleanup":autocommit_after_cleanup,"clock_validated":true,"fixed_unix_millis":fixed_ms,"adjacent_gap":adjacent_gap,"adjacent_control_gap":adjacent_gap,"before_uuid":before.to_string(),"after_uuid":after.to_string()});
    println!("PUBLIC_FATAL_READ_PROBE {}", record);
    let out = PathBuf::from(std::env::var_os("MINDSPACE_S3E4D_PROBE_OUT").unwrap())
        .join("fatal_summary_read.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(out, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    assert!(auto_after_fault);
    assert_eq!(extended, rusqlite::ffi::SQLITE_IOERR_READ);
    assert!(autocommit_after_cleanup);
    assert_eq!(rows_before, rows_after);
    assert_eq!(ledger_before, ledger_after);
    assert_eq!(next_gap, 1);
    assert_eq!((before.as_u128() >> 80) as u64, fixed_ms);
    assert_eq!((after.as_u128() >> 80) as u64, fixed_ms);
    assert!(error.contains("revisioned write required"), "{error}");
    assert_eq!(failed_gap, 2);
}
// S3E4D_CANDIDATE_END
