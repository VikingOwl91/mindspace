//! Process evidence for the schema-16/schema-17 compatibility boundary.
//! Reuses the existing fence_plan_compatibility MCP and migration-gate patterns.
//! PASS on a known-unsafe characterization means reproduced, never remediated.
#![cfg(target_os = "linux")]

#[path = "support/adversarial_common.rs"]
mod common;

use common::{current, driver, old, run, success, Case, ChildGuard};
use mindspace::ingest::document::{ingest_document_file, DocumentIngestOptions};
use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(15);
const MEMORY: &str = "s3e3bactivelegacycanary";
const DOCUMENT: &str = "s3e3bdocumentcanonicalcanary";
const MODEL: &str = "S3E3B_SYNTHETIC_MODEL_SETTING_NOT_A_SECRET_λ";
const PASSIVE_TABLES: [&str; 3] = [
    "asset_fence_plans",
    "asset_fence_plan_bindings",
    "asset_fence_plan_receipts",
];

fn cli(c: &Case, binary: &Path, args: &[&str], label: &str) -> std::process::Output {
    let mut command = c.mind(binary);
    command.args(args);
    run(command, label, c)
}

fn seed(c: &Case) {
    success(cli(c, &old(), &["init"], "old-init"));
    success(cli(c, &old(), &["space", "create", "notes"], "old-space"));
    let note = c.work.join("legacy.md");
    std::fs::write(&note, format!("# Synthetic fixture\n\n{MEMORY}.\n")).unwrap();
    let mut command = c.mind(&old());
    command.arg("ingest").arg(note).args(["--space", "notes"]);
    success(run(command, "old-ingest", c));
    assert_eq!(schema(c), 16);
}

fn raw(c: &Case) -> Connection {
    let conn = Connection::open_with_flags(&c.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.busy_timeout(WAIT).unwrap();
    conn
}

fn schema(c: &Case) -> i64 {
    raw(c)
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
}

// Same complete-row serialization oracle as fence_plan_compatibility, restricted
// to the old schema's tables. It excludes revision/accounting rows; isolated
// migration, crash and refusal phases use the separate exact accounting oracle.
fn ordinary(c: &Case) -> BTreeMap<String, Vec<String>> {
    let conn = raw(c);
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
            !PASSIVE_TABLES.contains(&name.as_str())
                && !["schema_migrations", "m6_revisions", "m6_row_availability"]
                    .contains(&name.as_str())
        })
        .map(|table| {
            let escaped = table.replace('"', "\"\"");
            let columns: Vec<String> = tx
                .prepare(&format!("PRAGMA table_info(\"{escaped}\")"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let expression = columns
                .iter()
                .map(|col| format!("quote(\"{}\")", col.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join("||'|'||");
            let mut rows: Vec<String> = tx
                .prepare(&format!("SELECT {expression} FROM \"{escaped}\""))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct Accounting {
    revisions: Vec<(i64, i64, String)>,
    availability: Vec<(String, String, i64)>,
}

fn accounting(c: &Case) -> Accounting {
    let conn = raw(c);
    let tx = conn.unchecked_transaction().unwrap();
    let revisions = tx
        .prepare("SELECT revision_id, recorded_at, kind FROM m6_revisions ORDER BY revision_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let availability = tx
        .prepare("SELECT table_name, entity_key, revision_id FROM m6_row_availability ORDER BY table_name, entity_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    Accounting {
        revisions,
        availability,
    }
}

fn assert_migration_accounting(c: &Case, before: &Accounting, label: &str) {
    let after = accounting(c);
    assert_eq!(after.revisions.len(), before.revisions.len() + 1);
    assert_eq!(after.revisions[..before.revisions.len()], before.revisions);
    let new_revision = after.revisions.last().unwrap();
    assert_eq!(new_revision.0, before.revisions.last().unwrap().0 + 1);
    assert_eq!(new_revision.2, "write");
    let mut expected = before.availability.clone();
    expected.push((
        "fence_plan_schema".to_string(),
        "local-fence-plan@1".to_string(),
        new_revision.0,
    ));
    expected.sort();
    assert_eq!(after.availability, expected);
    c.save(label, &json!({"before":before,"after":after,"oracle":"exactly one write revision and the schema17 availability marker; every preexisting accounting row unchanged"}));
}

fn assert_passive_inventory(c: &Case) -> Value {
    let conn = raw(c);
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(versions, (1..=17).collect::<Vec<_>>());
    let mut counts = BTreeMap::new();
    for name in PASSIVE_TABLES {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {name}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 0,
            "a maintenance test must create no passive plan or fence"
        );
        counts.insert(name, n);
    }
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    json!({"versions":versions,"passive_table_counts":counts,"foreign_key_violations":violations})
}

struct Server {
    process: ChildGuard,
    next: u64,
}

impl Server {
    fn start(c: &Case, label: &str) -> Self {
        let mut command = c.mind(&old());
        command.arg("mcp").arg("--allow-root").arg(&c.work);
        let mut s = Self {
            process: ChildGuard::spawn(command, c.artifacts.join(format!("{label}.stderr"))),
            next: 0,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"s3e3b-process-evidence","version":"1"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        s.process
            .send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string());
        s
    }
    fn send(&mut self, method: &str, params: Value) {
        self.next += 1;
        self.process.send(
            &json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params}).to_string(),
        );
    }
    fn receive(&self) -> Value {
        let response: Value = serde_json::from_str(&self.process.line()).unwrap();
        assert_eq!(response["id"], self.next, "correlated MCP reply required");
        assert!(response.get("error").is_none(), "{response}");
        response
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.send(method, params);
        self.receive()
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.rpc("tools/call", json!({"name":name,"arguments":arguments}));
        assert_eq!(response["result"]["isError"], false, "{response}");
        response["result"]["structuredContent"].clone()
    }
    fn hit(&mut self, marker: &str) -> Value {
        let found = self.call("memory_search", json!({"query":marker,"mode":"lexical"}));
        assert!(
            !found["hits"].as_array().unwrap().is_empty(),
            "missing positive-control hit for {marker}"
        );
        found
    }
    fn create(&mut self, marker: &str) {
        self.call(
            "memory_create",
            json!({"subject":"Synthetic process fixture","content":marker,"spaces":["notes"]}),
        );
    }
}

fn migration(c: &Case, label: &str, prefix: &Path) -> ChildGuard {
    let mut command = c.mind(&driver());
    command
        .arg("init")
        .env("MINDSPACE_FENCE_PLAN_MIGRATION_GATE_PATH", prefix);
    let mut child = ChildGuard::spawn(command, c.artifacts.join(format!("{label}.stderr")));
    let deadline = Instant::now() + WAIT;
    while !prefix.with_extension("reached").is_file() {
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "migration exited before COMMIT gate"
        );
        assert!(Instant::now() < deadline, "migration gate deadline");
        thread::sleep(Duration::from_millis(2));
    }
    child
}

fn writer_is_locked(c: &Case) {
    let conn = Connection::open(&c.db).unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    let e = conn.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert_eq!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn old_idle_connection_survives_upgrade_while_fresh_store_rejects() {
    let c = Case::new("old_idle_connection_survives_upgrade_while_fresh_store_rejects");
    seed(&c);
    let mut old_server = Server::start(&c, "old-idle");
    let before_hits = old_server.hit(MEMORY);
    let before = ordinary(&c);
    let before_accounting = accounting(&c);
    c.save("before-migration-files", &c.fingerprint());
    success(cli(&c, &current(), &["init"], "current-migrate"));
    c.save("after-migration-files", &c.fingerprint());
    assert_migration_accounting(&c, &before_accounting, "migration-accounting");
    assert_eq!(ordinary(&c), before);
    // This explicit maintenance operation belongs to the disposable fixture.
    // Capture the open/checkpoint interval before another native connection can
    // conceal file-family effects; connection-close effects are recorded apart.
    c.save("before-idle-checkpoint-files", &c.fingerprint());
    let maintenance =
        Connection::open_with_flags(&c.db, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    maintenance.busy_timeout(WAIT).unwrap();
    let checkpoint: (i64, i64, i64) = maintenance
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    c.save("after-idle-checkpoint-files", &c.fingerprint());
    assert_eq!(
        checkpoint.0, 0,
        "idle old connection must not block this checkpoint"
    );
    drop(maintenance);
    c.save("after-checkpoint-connection-close-files", &c.fingerprint());
    assert!(old_server.process.child.try_wait().unwrap().is_none());
    assert_eq!(old_server.hit(MEMORY), before_hits);
    old_server.create("s3e3boldidlewrite");
    old_server.hit("s3e3boldidlewrite");
    let before_reject = ordinary(&c);
    let before_reject_accounting = accounting(&c);
    c.save("before-fresh-old-files", &c.fingerprint());
    let refused = cli(
        &c,
        &old(),
        &["search", MEMORY, "--mode", "lexical", "--json"],
        "fresh-old-refusal",
    );
    c.save("after-fresh-old-files", &c.fingerprint());
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("newer than supported version 16"));
    assert_eq!(ordinary(&c), before_reject);
    assert_eq!(accounting(&c), before_reject_accounting);
    assert!(old_server.process.child.try_wait().unwrap().is_none());
    let inventory = assert_passive_inventory(&c);
    drop(old_server);
    c.finish("known_unsafe_characterization", "old admitted handle reads and mutates after schema17; fresh old Store rejects without logical mutation", json!({"old_handle_survived":true,"old_write_observed":true,"fresh_store_rejected":true,"ordinary_rows_preserved_by_migration":true,"checkpoint":{"mode":"TRUNCATE","busy":checkpoint.0,"log_frames":checkpoint.1,"checkpointed_frames":checkpoint.2,"did_not_evict_old_process":true,"existing_content_and_next_write_usable":true},"inventory":inventory,"limitations":"Does not establish zero filesystem effects or complete API coverage. Checkpoint observation is for an idle old connection, not a held read transaction."}));
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn old_paused_process_resumes_after_schema_migration() {
    let c = Case::new("old_paused_process_resumes_after_schema_migration");
    seed(&c);
    let mut old_server = Server::start(&c, "old-paused");
    old_server.hit(MEMORY);
    let pid = old_server.process.pid() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let deadline = Instant::now() + WAIT;
    loop {
        let mut status = 0;
        let observed = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
        assert!(observed >= 0, "waitpid stop acknowledgement failed");
        if observed == pid {
            assert!(libc::WIFSTOPPED(status));
            assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old-process stop acknowledgement deadline"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let before_accounting = accounting(&c);
    c.save("paused-before-migration-files", &c.fingerprint());
    success(cli(&c, &current(), &["init"], "migrate-while-stopped"));
    c.save("paused-after-migration-files", &c.fingerprint());
    assert_migration_accounting(&c, &before_accounting, "paused-migration-accounting");
    assert_eq!(schema(&c), 17);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    old_server.hit(MEMORY);
    old_server.create("s3e3bpausedoldwrite");
    old_server.hit("s3e3bpausedoldwrite");
    drop(old_server);
    c.finish("known_unsafe_characterization", "acknowledged stopped schema16 process resumes ordinary reads and writes after schema17 migration", json!({"stop_acknowledged_with":"waitpid WUNTRACED","post_resume_read_and_write":true,"limitations":"An idle admitted process was stopped; this does not locate an instruction within a retrieval operation."}));
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn queued_old_writer_resumes_after_held_migration_commit() {
    let c = Case::new("queued_old_writer_resumes_after_held_migration_commit");
    seed(&c);
    let mut old_server = Server::start(&c, "old-queued");
    let prefix = c.work.join("migration-queued-gate");
    let mut migrator = migration(&c, "held-migration", &prefix);
    writer_is_locked(&c);
    assert_eq!(schema(&c), 16);
    c.save("held-migration-files", &c.fingerprint());
    old_server.send("tools/call", json!({"name":"memory_create","arguments":{"content":"s3e3bqueuedoldwrite","spaces":["notes"]}}));
    assert!(
        old_server
            .process
            .try_line(Duration::from_millis(150))
            .is_none(),
        "old write replied before held migration was released"
    );
    std::fs::write(prefix.with_extension("go"), b"release").unwrap();
    assert!(migrator.wait().success());
    let response = old_server.receive();
    assert_eq!(response["result"]["isError"], false, "{response}");
    c.save("released-migration-files", &c.fingerprint());
    old_server.hit("s3e3bqueuedoldwrite");
    let inventory = assert_passive_inventory(&c);
    drop(old_server);
    c.finish("known_unsafe_characterization", "old request spans an acknowledged held migration COMMIT and succeeds after release", json!({"migration_commit_gate_acknowledged":true,"native_writer_probe_busy":true,"no_reply_observation_ms":150,"old_write_observed":true,"inventory":inventory,"limitations":"Request submission/no reply bounds do not identify the old client's exact blocked instruction."}));
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn concurrent_migrators_preserve_ordinary_rows_and_single_schema17_inventory() {
    let c = Case::new("concurrent_migrators_preserve_ordinary_rows_and_single_schema17_inventory");
    seed(&c);
    let before = ordinary(&c);
    let before_accounting = accounting(&c);
    let prefix = c.work.join("concurrent-migration-gate");
    let mut first = migration(&c, "first-migrator", &prefix);
    writer_is_locked(&c);
    let mut command = c.mind(&current());
    command.arg("init");
    let mut second = ChildGuard::spawn(command, c.artifacts.join("second-migrator.stderr"));
    c.save("two-migrator-files", &c.fingerprint());
    assert!(second.child.try_wait().unwrap().is_none());
    std::fs::write(prefix.with_extension("go"), b"release").unwrap();
    assert!(first.wait().success());
    assert!(second.wait().success());
    c.save("after-concurrent-migration-files", &c.fingerprint());
    assert_migration_accounting(&c, &before_accounting, "concurrent-migration-accounting");
    assert_eq!(ordinary(&c), before);
    let inventory = assert_passive_inventory(&c);
    c.finish("regression_invariant", "two actual migrators converge on exactly versions1..17 and empty passive tables while preserving ordinary rows", json!({"inventory":inventory,"ordinary_rows_equal":true,"limitations":"The second process is launched while the first holds COMMIT; its precise SQLite wait instruction is not claimed."}));
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn controlled_migration_crash_rolls_back_then_restart_preserves_old_client() {
    let c = Case::new("controlled_migration_crash_rolls_back_then_restart_preserves_old_client");
    seed(&c);
    let mut old_server = Server::start(&c, "old-crash");
    old_server.hit(MEMORY);
    let before = ordinary(&c);
    let before_accounting = accounting(&c);
    let prefix = c.work.join("crash-migration-gate");
    let mut migrator = migration(&c, "crashing-migrator", &prefix);
    writer_is_locked(&c);
    c.save("before-controlled-kill-files", &c.fingerprint());
    migrator.kill();
    assert!(!migrator.wait().success());
    c.save("after-controlled-kill-files", &c.fingerprint());
    assert_eq!(schema(&c), 16);
    assert_eq!(ordinary(&c), before);
    assert_eq!(accounting(&c), before_accounting);
    let absent: i64 = raw(&c)
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'asset_fence_plan%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(absent, 0);
    old_server.hit(MEMORY);
    old_server.create("s3e3boldaftercrash");
    let restart_accounting = accounting(&c);
    success(cli(&c, &current(), &["init"], "restart-maintenance"));
    c.save("after-restart-files", &c.fingerprint());
    assert_migration_accounting(&c, &restart_accounting, "restart-migration-accounting");
    old_server.hit("s3e3boldaftercrash");
    old_server.create("s3e3boldafterrestart");
    old_server.hit("s3e3boldafterrestart");
    let inventory = assert_passive_inventory(&c);
    drop(old_server);
    c.finish("regression_invariant", "killing only the fixture migrator at its acknowledged pre-COMMIT gate leaves schema16 ordinary state intact; restart reaches17", json!({"crash_point":"inside SQLite commit hook before release","schema_after_kill":16,"partial_passive_schema_objects":absent,"old_client_usable_after_restart":true,"inventory":inventory,"limitations":"This controlled known-precommit kill does not classify lost acknowledgement or arbitrary crash points as rollback."}));
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

#[test]
#[ignore = "run scripts/m8-s3e3b-harness.py"]
fn old_doctor_discloses_model_setting_after_incompatibility_without_observed_content_canaries() {
    let c = Case::new("old_doctor_discloses_model_setting_after_incompatibility_without_observed_content_canaries");
    seed(&c);
    let document = c.work.join("document.txt");
    std::fs::write(&document, format!("{DOCUMENT}\n")).unwrap();
    {
        let store = SqliteStore::open(&c.db).unwrap();
        ingest_document_file(&store, &document, &DocumentIngestOptions::default()).unwrap();
        store.set_meta("embedding.model", MODEL).unwrap();
    }
    let conn = raw(&c);
    let documents: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sources WHERE kind='document' AND instr(content,?1)>0",
            [DOCUMENT],
            |r| r.get(0),
        )
        .unwrap();
    let active: i64 = conn.query_row("SELECT COUNT(*) FROM memories m JOIN assertions a ON a.id=m.assertion_id WHERE m.state='active' AND instr(a.content,?1)>0", [MEMORY], |r| r.get(0)).unwrap();
    assert!(
        documents > 0 && active > 0,
        "content canaries must actually be stored"
    );
    drop(conn);
    let mut observations = Vec::new();
    for enabled in ["false", "true"] {
        {
            let store = SqliteStore::open(&c.db).unwrap();
            store.set_meta("embedding.enabled", enabled).unwrap();
            assert_eq!(
                store.get_meta("embedding.model").unwrap().as_deref(),
                Some(MODEL)
            );
        }
        let ordinary_before = ordinary(&c);
        for json_output in [true, false] {
            let label = format!(
                "doctor-enabled-{enabled}-{}",
                if json_output { "json" } else { "text" }
            );
            c.save(&format!("{label}-before-files"), &c.fingerprint());
            let args = if json_output {
                vec!["doctor", "--json"]
            } else {
                vec!["doctor"]
            };
            let out = cli(&c, &old(), &args, &label);
            c.save(&format!("{label}-after-files"), &c.fingerprint());
            let stdout = String::from_utf8(out.stdout).unwrap();
            let stderr = String::from_utf8(out.stderr).unwrap();
            assert!(
                !out.status.success(),
                "incompatible doctor cannot report overall success"
            );
            assert!(
                stdout.contains(MODEL),
                "known unsafe setting disclosure not reproduced"
            );
            assert!(!stdout.contains(MEMORY) && !stderr.contains(MEMORY));
            assert!(!stdout.contains(DOCUMENT) && !stderr.contains(DOCUMENT));
            if json_output {
                let report: Value = serde_json::from_str(&stdout).unwrap();
                assert_eq!(check(&report, "database")["status"], "fail");
                assert!(check(&report, "database")["summary"]
                    .as_str()
                    .unwrap()
                    .contains("newer than this binary supports"));
                assert_eq!(check(&report, "database")["details"]["schema_version"], 17);
                assert_eq!(
                    check(&report, "database")["details"]["supported_schema_version"],
                    16
                );
                assert_eq!(
                    check(&report, "embeddings")["details"]["selected_model"],
                    MODEL
                );
                assert_eq!(
                    check(&report, "embeddings")["details"]["enabled"],
                    enabled == "true"
                );
            } else {
                assert!(
                    stdout.contains("schema version 17 is newer than this binary supports (16)")
                );
                assert!(stdout.contains("selected_model"));
            }
            assert_eq!(ordinary(&c), ordinary_before);
            observations.push(json!({"configured_enabled":enabled,"format":if json_output {"json"} else {"text"},"exit_code":out.status.code(),"schema_incompatibility_detected":true,"subsequent_model_setting_disclosed":true,"synthetic_potentially_sensitive_metadata_disclosed":true,"active_memory_canary_observed":false,"document_canary_observed":false,"ordinary_rows_unchanged":true}));
        }
    }
    c.finish("known_unsafe_characterization", "old doctor detects incompatible schema17 yet emits arbitrary synthetic model-setting text in JSON/text with embeddings configured enabled and disabled", json!({"observations":observations,"canary_positive_controls":{"active_memory_rows":active,"document_source_rows":documents},"limitations":"No observed active-content canary in these four outputs is not universal non-disclosure. No diagnostic exception is accepted. File-family changes are recorded separately; ordinary row equality is not a zero-filesystem-effect claim."}));
}
