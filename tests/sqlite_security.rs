//! Assert the linked native engine, rather than just the Rust package version.

#[test]
fn bundled_sqlite_meets_the_wal_memory_exposure_fix_floor() {
    // RUSTSEC-2026-0459: SQLite before 3.54.0 can expose uninitialized memory
    // when opening a crafted database/WAL pair. This is a version guard, not
    // an exploit reproduction or a substitute for database lifecycle tests.
    assert!(
        rusqlite::version_number() >= 3_054_000,
        "linked SQLite {} predates the WAL memory-exposure fix (3.54.0)",
        rusqlite::version()
    );
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let sql_version: String = db
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sql_version, rusqlite::version());
    eprintln!(
        "linked SQLite {sql_version} ({})",
        rusqlite::version_number()
    );
}
