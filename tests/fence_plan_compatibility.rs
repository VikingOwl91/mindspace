//! Actual schema-16 process compatibility with the passive schema-17 extension.
//! Opt-in cases require a separately preserved, authenticated old production binary.
//! They never activate a fence, rename an active relation or publish a document.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use mindspace::core::sha256_hex;
use mindspace::SqliteStore;
use rusqlite::Connection;
use serde_json::{json, Value};

const OLD_SHA256: &str = "a5960e6b98cd311f7f00d56bc165bc0ffd155e0b328051a961b86cae308ba31d";
const NEW_TABLES: &[&str] = &[
    "asset_fence_plans",
    "asset_fence_plan_bindings",
    "asset_fence_plan_receipts",
];
const WAIT: Duration = Duration::from_secs(15);

fn old_binary() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("MINDSPACE_SCHEMA16_BINARY")
            .expect("set MINDSPACE_SCHEMA16_BINARY to the preserved schema-16 production binary"),
    );
    let bytes = std::fs::read(&path).expect("read preserved old production binary");
    assert_eq!(
        sha256_hex(&bytes),
        OLD_SHA256,
        "the old-process fixture must use the reviewed production binary, not the current build"
    );
    path
}

fn command(binary: &Path, db: &Path) -> Command {
    let mut c = Command::new(binary);
    c.arg("--db").arg(db);
    for key in [
        "MINDSPACE_DB",
        "MINDSPACE_SPAWN_GATE",
        "MINDSPACE_MCP_ALLOW_ROOTS",
        "MINDSPACE_FENCE_PLAN_MIGRATION_GATE_PATH",
        "MINDSPACE_STORE_GATE_PAUSE_IN_COMMIT",
        "MINDSPACE_FENCE_PLAN_FAULT",
        "MINDSPACE_FENCE_PLAN_PAUSE",
        "MINDSPACE_FENCE_PLAN_GATE_PATH",
    ] {
        c.env_remove(key);
    }
    c
}

fn current_binary() -> PathBuf {
    std::env::var_os("MINDSPACE_S3E2_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_mind").into())
        .into()
}

fn succeeded(output: Output) {
    assert!(
        output.status.success(),
        "status {:?}; stdout {}; stderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    dir: tempfile::TempDir,
    db: PathBuf,
    root: PathBuf,
    note: PathBuf,
    old: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let old = old_binary();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data").join("mind.db");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let note = root.join("legacy.md");
        std::fs::write(
            &note,
            "# Compatibility\n\nquartzcanary original legacy content.\n",
        )
        .unwrap();
        succeeded(command(&old, &db).arg("init").output().unwrap());
        succeeded(
            command(&old, &db)
                .args(["space", "create", "notes"])
                .output()
                .unwrap(),
        );
        succeeded(
            command(&old, &db)
                .arg("ingest")
                .arg(&note)
                .args(["--space", "notes"])
                .output()
                .unwrap(),
        );
        assert_eq!(schema(&db), 16);
        Self {
            dir,
            db,
            root,
            note,
            old,
        }
    }
}

fn schema(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
}

struct Server {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
    next: u64,
}

impl Server {
    fn start(binary: &Path, db: &Path, root: &Path) -> Self {
        let mut child = command(binary, db)
            .arg("mcp")
            .arg("--allow-root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Self {
            child,
            input,
            output,
            next: 0,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"s3e2-compatibility-tests","version":"1"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        writeln!(
            s.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s.input.flush().unwrap();
        s
    }

    fn send(&mut self, method: &str, params: Value) {
        self.next += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&self) -> Value {
        let line = self
            .output
            .recv_timeout(WAIT)
            .expect("bounded MCP reply wait");
        serde_json::from_str(&line).unwrap()
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.send(method, params);
        self.receive()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":name,"arguments":arguments}))["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn success(result: Value) -> Value {
    assert_eq!(result["isError"], false, "{result}");
    result["structuredContent"].clone()
}

fn rows(conn: &Connection, table: &str) -> Vec<String> {
    let escaped = table.replace('"', "\"\"");
    let columns: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info(\"{escaped}\")"))
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let expression = columns
        .iter()
        .map(|c| format!("quote(\"{}\")", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join("||'|'||");
    let mut result: Vec<String> = conn
        .prepare(&format!("SELECT {expression} FROM \"{escaped}\""))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    result.sort();
    result
}

fn ordinary_snapshot(path: &Path) -> BTreeMap<String, Vec<String>> {
    let conn = Connection::open(path).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let tables: Vec<String> = tx
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .filter(|name| {
            !NEW_TABLES.contains(&name.as_str())
                && !["schema_migrations", "m6_revisions", "m6_row_availability"]
                    .contains(&name.as_str())
        })
        .map(|table| {
            let values = rows(&tx, &table);
            (table, values)
        })
        .collect()
}

fn capture_reads(server: &mut Server, memory: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for mode in ["lexical", "semantic", "hybrid", "auto"] {
        out.push(server.call(
            "memory_search",
            json!({"query":"quartzcanary","spaces":["notes"],"mode":mode,"limit":20}),
        ));
    }
    out.push(server.call(
        "memory_search",
        json!({"query":"quartzcanary","spaces":["notes"],"mode":"lexical",
            "at":"2099-01-01T00:00:00Z","limit":20}),
    ));
    out.push(server.call("memory_get", json!({"id":memory})));
    out.push(server.call("memory_history", json!({"subject":"compatibility"})));
    out.push(server.call("memory_conflicts", json!({})));
    out.push(server.call("space_list", json!({})));
    out
}

#[test]
fn passive_migration_attaches_no_trigger_to_existing_tables() {
    let sql = include_str!("../migrations/0017_passive_fence_plans.sql");
    let mut count = 0;
    for trigger in sql.split("CREATE TRIGGER ").skip(1) {
        let tokens: Vec<_> = trigger.split_whitespace().collect();
        let on = tokens.iter().position(|word| *word == "ON").unwrap();
        assert!(
            NEW_TABLES.contains(&tokens[on + 1]),
            "ordinary-table trigger: {}",
            tokens[0]
        );
        count += 1;
    }
    assert!(
        count >= 9,
        "each passive table needs insertion and immutability guards"
    );
    assert!(!sql.contains("ALTER TABLE"));
    assert!(!sql.contains("CREATE VIRTUAL TABLE"));
    assert!(!sql.contains("INSERT INTO m6_row_availability"));
}

#[test]
#[ignore = "requires the separately preserved schema-16 production binary via MINDSPACE_SCHEMA16_BINARY"]
fn actual_old_open_readers_and_writers_survive_passive_migration_but_fresh_opens_refuse() {
    let f = Fixture::new();
    let mut old = Server::start(&f.old, &f.db, &f.root);
    let definitions = old.rpc("tools/list", json!({}))["result"]["tools"].clone();
    let hits = success(old.call(
        "memory_search",
        json!({"query":"quartzcanary","spaces":["notes"],"mode":"lexical"}),
    ));
    let memory = hits["hits"][0]["hit"]["memory_id"]
        .as_str()
        .expect("the explicit lexical outcome has a ranked source hit")
        .to_string();
    let before_reads = capture_reads(&mut old, &memory);
    let before_rows = ordinary_snapshot(&f.db);
    let availability_before = {
        let c = Connection::open(&f.db).unwrap();
        rows(&c, "m6_row_availability")
    };
    let current = SqliteStore::open(&f.db).unwrap();
    assert_eq!(schema(&f.db), 17);
    assert_eq!(ordinary_snapshot(&f.db), before_rows);
    assert_eq!(capture_reads(&mut old, &memory), before_reads);
    let after = {
        let c = Connection::open(&f.db).unwrap();
        rows(&c, "m6_row_availability")
    };
    assert_eq!(after.len(), availability_before.len() + 1);
    assert!(availability_before.iter().all(|r| after.contains(r)));
    drop(current);

    // Current code has the same default MCP definitions; schema upgrade grants no endpoint.
    let mut new = Server::start(&current_binary(), &f.db, &f.root);
    assert_eq!(
        new.rpc("tools/list", json!({}))["result"]["tools"],
        definitions
    );
    assert_eq!(capture_reads(&mut new, &memory), before_reads);

    // The same old handle performs actual ordinary mutations which reach M6 guards.
    success(old.call(
        "space_create",
        json!({"name":"other","description":"unrelated"}),
    ));
    let created = success(old.call(
        "memory_create",
        json!({"subject":"Old writer","content":"oldwriter can still create","spaces":["notes"]}),
    ));
    let updated = success(old.call(
        "memory_update",
        json!({"id":created["memory_id"],"content":"oldwriter can still update","spaces":["notes"]}),
    ));
    success(old.call(
        "memory_add_spaces",
        json!({"id":updated["memory_id"],"spaces":["other"]}),
    ));
    success(old.call(
        "memory_retire",
        json!({"id":updated["memory_id"],"reason":"compatibility test"}),
    ));
    std::fs::write(
        &f.note,
        "# Compatibility\n\nnewoldwritermarker changed source.\n",
    )
    .unwrap();
    success(old.call("repo_ingest", json!({"path":f.root,"spaces":["notes"]})));
    let hits = success(old.call(
        "memory_search",
        json!({"query":"newoldwritermarker","mode":"lexical"}),
    ));
    assert!(!hits["hits"].as_array().unwrap().is_empty());
    success(new.call(
        "memory_create",
        json!({"content":"new writer remains ordinary","spaces":["notes"]}),
    ));

    let rows_before_refusal = ordinary_snapshot(&f.db);
    let version_before_refusal = schema(&f.db);
    let refusal = command(&f.old, &f.db)
        .args([
            "search",
            "newoldwritermarker",
            "--space",
            "notes",
            "--mode",
            "lexical",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!refusal.status.success());
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("newer than supported version 16"));
    assert_eq!(schema(&f.db), version_before_refusal);
    assert_eq!(ordinary_snapshot(&f.db), rows_before_refusal);
}

fn wait_for_file(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + WAIT;
    loop {
        if path.exists() {
            return;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "migration exited before announcing its held commit"
        );
        assert!(
            Instant::now() < deadline,
            "migration gate timed out: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

struct RunningChild(Child);

impl std::ops::Deref for RunningChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for RunningChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn migration_child(db: &Path, prefix: &Path) -> RunningChild {
    RunningChild(
        command(&current_binary(), db)
            .arg("init")
            .env("MINDSPACE_FENCE_PLAN_MIGRATION_GATE_PATH", prefix)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[test]
#[ignore = "requires schema-16 production binary and current spawn-gate driver"]
fn actual_old_writer_queued_inside_passive_migration_commit_remains_allowed() {
    let f = Fixture::new();
    let mut old = Server::start(&f.old, &f.db, &f.root);
    let prefix = f.dir.path().join("migration-held");
    let mut migration = migration_child(&f.db, &prefix);
    wait_for_file(&prefix.with_extension("reached"), &mut migration);
    assert_eq!(
        schema(&f.db),
        16,
        "uncommitted schema17 must not become visible"
    );
    old.send(
        "tools/call",
        json!({"name":"memory_create","arguments":{
        "content":"queued old writer remains allowed","spaces":["notes"]}}),
    );
    assert!(
        matches!(
            old.output.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "queued writer must wait for the held migration"
    );
    std::fs::write(prefix.with_extension("go"), b"release").unwrap();
    assert!(migration.wait().unwrap().success());
    success(old.receive()["result"].clone());
    assert_eq!(schema(&f.db), 17);
    let results = success(old.call(
        "memory_search",
        json!({"query":"queued old writer","mode":"lexical"}),
    ));
    assert!(!results["hits"].as_array().unwrap().is_empty());
    let c = Connection::open(&f.db).unwrap();
    for table in NEW_TABLES {
        let n: i64 = c
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 0,
            "no passive plan is fabricated by an ordinary queued writer"
        );
    }
}

#[test]
#[ignore = "requires schema-16 production binary and current spawn-gate driver"]
fn interrupted_actual_passive_migration_rolls_back_and_old_writer_stays_usable() {
    let f = Fixture::new();
    let mut old = Server::start(&f.old, &f.db, &f.root);
    let before = ordinary_snapshot(&f.db);
    let prefix = f.dir.path().join("migration-crash");
    let mut migration = migration_child(&f.db, &prefix);
    wait_for_file(&prefix.with_extension("reached"), &mut migration);
    migration.kill().unwrap();
    assert!(!migration.wait().unwrap().success());
    assert_eq!(schema(&f.db), 16);
    assert_eq!(ordinary_snapshot(&f.db), before);
    let c = Connection::open(&f.db).unwrap();
    let count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'asset_fence_plan%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "interrupted DDL and new-table triggers must roll back"
    );
    success(old.call(
        "memory_create",
        json!({"content":"old writer after migration crash","spaces":["notes"]}),
    ));
    succeeded(
        command(&current_binary(), &f.db)
            .arg("init")
            .output()
            .unwrap(),
    );
    assert_eq!(schema(&f.db), 17);
    success(old.call(
        "memory_create",
        json!({"content":"old writer after migration retry","spaces":["notes"]}),
    ));
}

fn main_schema_read_only(path: &Path) -> i64 {
    // This disposable connection reads MAIN only. It neither coordinates WAL/SHM
    // nor checkpoints pages, independently of the production admission helper.
    let mut uri = String::from("file:");
    for byte in path.to_str().unwrap().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                uri.push(byte as char);
            }
            byte => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.push_str("?immutable=1");
    let c = Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get(0)
    })
    .unwrap()
}

type FileFingerprint = Option<(u64, std::time::SystemTime, String)>;

fn file_fingerprints(db: &Path) -> BTreeMap<String, FileFingerprint> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_os_string();
            path.push(suffix);
            let path = PathBuf::from(path);
            let fingerprint = match std::fs::read(&path) {
                Ok(bytes) => {
                    let metadata = std::fs::metadata(&path).unwrap();
                    Some((
                        metadata.len(),
                        metadata.modified().unwrap(),
                        sha256_hex(&bytes),
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => panic!("read disposable fixture {}: {e}", path.display()),
            };
            (suffix.to_string(), fingerprint)
        })
        .collect()
}

fn passive_preview_command(db: &Path, revision: i64) -> Command {
    let mut c = command(&current_binary(), db);
    c.args([
        "ownership",
        "prepare-fence-plan",
        "--asset-id",
        "00000000-0000-4000-8000-000000000001",
        "--legacy-source-version",
        "00000000-0000-4000-8000-000000000002",
        "--document-id",
        "00000000-0000-4000-8000-000000000003",
        "--document-source-version",
        "00000000-0000-4000-8000-000000000004",
        "--extraction-id",
        "00000000-0000-4000-8000-000000000005",
        "--nonce",
        "00000000-0000-4000-8000-000000000006",
        "--expected-revision",
        &revision.to_string(),
        "--json",
    ]);
    c
}

fn passive_inspection_command(db: &Path) -> Command {
    let mut c = command(&current_binary(), db);
    c.args([
        "ownership",
        "inspect-fence-plan",
        "--operation-id",
        &format!(
            "local-fence-plan@1:00000000-0000-4000-8000-000000000006:{}",
            "0".repeat(64)
        ),
        "--json",
    ]);
    c
}

fn refusal(output: Output, expected: &str) {
    assert!(
        !output.status.success(),
        "read request unexpectedly succeeded"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(expected),
        "expected {expected:?}; got {error}"
    );
}

#[test]
#[ignore = "requires schema-16 production binary and current driver for read-only admission regression"]
fn actual_old_connection_requires_explicit_checkpoint_before_passive_read_admission() {
    let f = Fixture::new();
    assert_eq!(main_schema_read_only(&f.db), 16);

    // A rejected preview must not migrate; a rejected inspection must not leave
    // sidecars. Compare bytes/existence before any subsequent SQLite open.
    let before = file_fingerprints(&f.db);
    for mut c in [
        passive_preview_command(&f.db, 1),
        passive_inspection_command(&f.db),
    ] {
        refusal(c.output().unwrap(), "checkpointed initialized schema 17");
        assert_eq!(file_fingerprints(&f.db), before);
    }
    assert_eq!(main_schema_read_only(&f.db), 16);

    let mut old = Server::start(&f.old, &f.db, &f.root);
    let upgraded = SqliteStore::open(&f.db).unwrap();
    drop(upgraded);
    assert_eq!(
        schema(&f.db),
        17,
        "a normal reader sees the committed migration in WAL"
    );
    assert_eq!(
        main_schema_read_only(&f.db),
        16,
        "the old open connection keeps the uncheckpointed MAIN bootstrap"
    );
    success(old.call(
        "memory_create",
        json!({"content":"old client before explicit checkpoint","spaces":["notes"]}),
    ));
    let hits = success(old.call(
        "memory_search",
        json!({"query":"old client before explicit checkpoint","mode":"lexical"}),
    ));
    assert!(!hits["hits"].as_array().unwrap().is_empty());
    assert_eq!(main_schema_read_only(&f.db), 16);

    let before = file_fingerprints(&f.db);
    for mut c in [
        passive_preview_command(&f.db, 1),
        passive_inspection_command(&f.db),
    ] {
        refusal(c.output().unwrap(), "checkpointed initialized schema 17");
        assert_eq!(
            file_fingerprints(&f.db),
            before,
            "refused MAIN16/WAL17 read must not coordinate or checkpoint sidecars"
        );
    }

    // This is a separate, explicit trusted fixture maintenance operation. The
    // production preview/inspection commands never perform this checkpoint.
    let maintenance = Connection::open(&f.db).unwrap();
    let checkpoint: (i64, i64, i64) = maintenance
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(
        checkpoint.0, 0,
        "the old idle MCP connection is not a pinned read transaction"
    );
    assert_eq!(main_schema_read_only(&f.db), 17);
    let revision: i64 = maintenance
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    let main_before = file_fingerprints(&f.db)[""].clone();

    // Admitted requests now reach their domain validation: this legacy-only
    // fixture deliberately has no P or receipt. Native RO coordination on this
    // valid database may update SHM read marks; it must not write MAIN or an R.
    refusal(
        passive_preview_command(&f.db, revision).output().unwrap(),
        "matching existing document preparation required",
    );
    refusal(
        passive_inspection_command(&f.db).output().unwrap(),
        "passive operation not observed",
    );
    assert_eq!(file_fingerprints(&f.db)[""], main_before);
    let after_revision: i64 = maintenance
        .query_row("SELECT MAX(revision_id) FROM m6_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(after_revision, revision);
    assert_eq!(main_schema_read_only(&f.db), 17);

    success(old.call(
        "memory_create",
        json!({"content":"old client after explicit checkpoint","spaces":["notes"]}),
    ));
    let hits = success(old.call(
        "memory_search",
        json!({"query":"old client after explicit checkpoint","mode":"lexical"}),
    ));
    assert!(!hits["hits"].as_array().unwrap().is_empty());
}
