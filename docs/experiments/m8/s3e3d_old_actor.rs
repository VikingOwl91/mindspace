//! Disposable actor compiled against the pinned historical library.
//! Store operations are actual old Store calls. Snapshot/prepared modes use a
//! separately labelled native connection linked to that historical dependency;
//! they are not instrumentation of Store's private statement cache.
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

fn emit(v: Value) {
    println!("\nS3E3D_OLD {v}");
    io::stdout().flush().unwrap();
}
fn request() -> Value {
    let mut line = String::new();
    assert!(io::stdin().lock().read_line(&mut line).unwrap() > 0);
    assert!(line.len() < 65_536);
    serde_json::from_str(&line).unwrap()
}
fn path() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("S3E3D_FIXTURE_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    let db = PathBuf::from(std::env::var_os("S3E3D_ACTOR_DB").unwrap());
    assert!(root.is_dir() && db.is_absolute());
    let parent = db.parent().unwrap().canonicalize().unwrap();
    assert!(parent.starts_with(&root));
    let db = parent.join(db.file_name().unwrap());
    if db.exists() {
        assert!(db.canonicalize().unwrap().starts_with(&root));
    }
    db
}
fn rows(conn: &Connection) -> Vec<(String, String)> {
    conn.prepare("SELECT key,value FROM mind_meta ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
#[test]
#[ignore = "historical subprocess endpoint, only disposable databases"]
fn old_layout_actor() {
    let db = path();
    let mode = std::env::var("S3E3D_MODE").unwrap_or_else(|_| "store".into());
    let store = SqliteStore::open(&db).unwrap();
    if mode == "store" {
        emit(json!({"event":"ready","access":"historical Store"}));
        loop {
            let r = request();
            let answer = match r["op"].as_str().unwrap() {
                "get" => match store.get_meta(r["key"].as_str().unwrap()) {
                    Ok(v) => json!({"ok":true,"value":v}),
                    Err(e) => json!({"ok":false,"error":e.to_string()}),
                },
                "set" => {
                    match store.set_meta(r["key"].as_str().unwrap(), r["value"].as_str().unwrap()) {
                        Ok(()) => json!({"ok":true}),
                        Err(e) => json!({"ok":false,"error":e.to_string()}),
                    }
                }
                "exit" => {
                    drop(store);
                    emit(json!({"event":"released"}));
                    return;
                }
                op => panic!("unsupported command {op}"),
            };
            emit(json!({"event":"reply","op":r["op"],"id":r["id"],"result":answer}));
        }
    }
    let conn = Connection::open(db).unwrap();
    match mode.as_str() {
        "snapshot" => {
            conn.execute_batch("BEGIN DEFERRED").unwrap();
            let before = rows(&conn);
            emit(
                json!({"event":"ready","access":"historical native SQL","mode":mode,"before":before}),
            );
            assert_eq!(request()["op"], "release");
            let after = rows(&conn);
            assert_eq!(before, after);
            conn.execute_batch("ROLLBACK").unwrap();
            emit(json!({"event":"observation","ok":true,"rows":after}));
        }
        "prepared" => {
            let mut stmt = conn
                .prepare("SELECT key,value FROM mind_meta ORDER BY key")
                .unwrap();
            emit(json!({"event":"ready","access":"historical native SQL","mode":mode}));
            assert_eq!(request()["op"], "release");
            let result = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>());
            emit(match result {
                Ok(v) => json!({"event":"observation","ok":true,"rows":v}),
                Err(e) => json!({"event":"observation","ok":false,"error":e.to_string()}),
            });
        }
        "stepped" => {
            let mut stmt = conn
                .prepare("SELECT key,value FROM mind_meta ORDER BY key")
                .unwrap();
            let mut cursor = stmt.query([]).unwrap();
            let row = cursor.next().unwrap().unwrap();
            let mut result = vec![(
                row.get::<_, String>(0).unwrap(),
                row.get::<_, String>(1).unwrap(),
            )];
            emit(json!({"event":"ready","access":"historical native SQL","mode":mode}));
            assert_eq!(request()["op"], "release");
            while let Some(row) = cursor.next().unwrap() {
                result.push((
                    row.get::<_, String>(0).unwrap(),
                    row.get::<_, String>(1).unwrap(),
                ));
            }
            emit(json!({"event":"observation","ok":true,"rows":result}));
        }
        m => panic!("unsupported mode {m}"),
    }
    let retry_error = conn
        .query_row(
            "SELECT value FROM mind_meta WHERE key='embedding.model'",
            [],
            |r| r.get::<_, String>(0),
        )
        .expect_err("old SQL execution must fail after its snapshot/statement is released")
        .to_string();
    drop(conn);
    drop(store);
    emit(json!({"event":"released","retry_error":retry_error}));
}
