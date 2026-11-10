//! Test-only removal of the later, empty S3b metadata from old-schema fixtures.
//! Existing source/history revisions stay intact; only the metadata birth R is removed.
pub fn remove_empty_ownership_metadata(conn: &rusqlite::Connection) {
    let owners: i64 = conn
        .query_row("SELECT COUNT(*) FROM asset_ownership", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        owners, 0,
        "downgrade fixture must never erase an enrollment"
    );
    let revision: i64 = conn.query_row("SELECT revision_id FROM m6_row_availability WHERE table_name='asset_ownership_schema' AND entity_key='local-enroll@1'", [], |r| r.get(0)).unwrap();
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    let triggers: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND name LIKE 'm6_s3b_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in triggers {
        conn.execute_batch(&format!("DROP TRIGGER \"{name}\";"))
            .unwrap();
    }
    conn.execute_batch("DROP TABLE asset_ownership_history; DROP TABLE asset_ownership; DROP TABLE asset_control_intents; DELETE FROM schema_migrations WHERE version=13;").unwrap();
    for (name, sql) in [
        (
            "m6_fence_m6_row_availability_d",
            "DELETE FROM m6_row_availability WHERE table_name LIKE 'asset_%'".to_string(),
        ),
        (
            "m6_fence_revisions_delete",
            format!("DELETE FROM m6_revisions WHERE revision_id={revision}"),
        ),
    ] {
        let definition: String = conn
            .query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute_batch(&format!("DROP TRIGGER {name}; {sql}; {definition};"))
            .unwrap();
    }
}
