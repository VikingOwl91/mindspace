//! Disposable S3e.3c protocol actor, compiled against archived schema-16 source.
//!
//! Copy only to `tests/s3e3c_old_store_actor.rs` in that disposable archive.
//! The parent supplies an owned fixture root and supervises process deadlines.
//! No Store implementation, SQL, migration or production entrypoint is changed.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use mindspace::storage::Store;
use mindspace::SqliteStore;
use serde_json::{json, Value};

const PREFIX: &str = "S3E3C_OLD ";

fn emit(value: &Value) {
    println!("\n{PREFIX}{value}");
    io::stdout().flush().unwrap();
}

fn fixture_database() -> PathBuf {
    let root = PathBuf::from(
        std::env::var_os("S3E3C_FIXTURE_ROOT").expect("explicit fixture root required"),
    );
    let path =
        PathBuf::from(std::env::var_os("S3E3C_ACTOR_DB").expect("explicit fixture DB required"));
    assert!(root.is_absolute() && path.is_absolute());
    let root = root
        .canonicalize()
        .expect("fixture root must already exist");
    assert!(root.is_dir());
    let parent = path
        .parent()
        .expect("fixture DB must have a parent")
        .canonicalize()
        .expect("fixture DB parent must already exist");
    assert!(parent.starts_with(&root), "DB parent escapes fixture root");
    let database = parent.join(path.file_name().expect("fixture DB filename required"));
    if database.symlink_metadata().is_ok() {
        assert!(
            database.canonicalize().unwrap().starts_with(&root),
            "existing DB target escapes fixture root"
        );
    }
    database
}

fn reply(request: &Value, mut value: Value) {
    value["event"] = json!("reply");
    value["op"] = request["op"].clone();
    if let Some(id) = request.get("id") {
        value["id"] = id.clone();
    }
    emit(&value);
}

#[test]
#[ignore = "subprocess protocol endpoint; compile/run only in a disposable historical archive"]
fn old_store_actor() {
    let database = fixture_database();
    let store = SqliteStore::open(&database).expect("historical Store admission failed");
    emit(&json!({"event":"ready", "protocol":"s3e3c-old-store-actor/1"}));

    for line in io::stdin().lock().lines() {
        let line = line.expect("actor stdin read");
        assert!(
            line.len() <= 65_536,
            "fixture command exceeds protocol limit"
        );
        let request: Value = serde_json::from_str(&line).expect("JSON fixture command");
        match request["op"].as_str().expect("command op required") {
            "get" => {
                let key = request["key"].as_str().expect("get key required");
                let result = match store.get_meta(key) {
                    Ok(value) => json!({"ok":true, "value":value}),
                    Err(error) => json!({"ok":false, "error":error.to_string()}),
                };
                reply(&request, result);
            }
            "set" => {
                let key = request["key"].as_str().expect("set key required");
                let value = request["value"].as_str().expect("set value required");
                let result = match store.set_meta(key, value) {
                    Ok(()) => json!({"ok":true}),
                    Err(error) => json!({"ok":false, "error":error.to_string()}),
                };
                reply(&request, result);
            }
            "pair" => {
                // These are two actual old Store calls, not one read snapshot.
                // In particular this actor does not retrofit atomic pair routing.
                let result = match (
                    store.get_meta("embedding.enabled"),
                    store.get_meta("embedding.model"),
                ) {
                    (Ok(enabled), Ok(model)) => {
                        json!({"ok":true, "enabled":enabled, "model":model})
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        json!({"ok":false, "error":error.to_string()})
                    }
                };
                reply(&request, result);
            }
            "exit" => {
                drop(store);
                let mut released = json!({"event":"released", "reason":"exit"});
                if let Some(id) = request.get("id") {
                    released["id"] = id.clone();
                }
                emit(&released);
                return;
            }
            other => panic!("unknown fixture operation: {other}"),
        }
    }
    drop(store);
    emit(&json!({"event":"released", "reason":"eof"}));
}
