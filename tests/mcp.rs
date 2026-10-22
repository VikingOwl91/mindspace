use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use mindspace::storage::Store;
use mindspace::SqliteStore;
use rusqlite::Connection;
use serde_json::{json, Value};
use tempfile::TempDir;

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    finished: bool,
}

impl McpProcess {
    fn spawn(db: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mind"))
            .arg("--db")
            .arg(db)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut process = Self {
            child,
            stdin: Some(stdin),
            stdout,
            finished: false,
        };
        let initialized = process.request(json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mindspace-tests", "version": "1" }
            }
        }));
        assert_eq!(initialized["id"], "init");
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        process.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        process
    }

    fn send(&mut self, message: Value) {
        self.send_raw(&message.to_string());
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line).unwrap();
        assert_ne!(bytes, 0, "MCP process exited before replying");
        serde_json::from_str(line.trim_end()).unwrap()
    }

    fn request(&mut self, message: Value) -> Value {
        self.send(message);
        self.read()
    }

    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.request(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
    }

    fn shutdown(mut self) -> ExitStatus {
        self.stdin.take();
        let status = self.child.wait().unwrap();
        self.finished = true;
        status
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if !self.finished {
            self.stdin.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn database() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    let store = SqliteStore::open(&path).unwrap();
    store.create_space("coding", "").unwrap();
    drop(store);
    (dir, path)
}

fn success_content(response: &Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    let structured = response["result"]["structuredContent"].clone();
    let text: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(structured, text);
    structured
}

#[test]
fn real_binary_exposes_all_eleven_tools_and_runs_the_memory_lifecycle() {
    let (_dir, db) = database();
    let mut process = McpProcess::spawn(&db);

    let list = process.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 11);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool.get("outputSchema").is_none());
    }
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "memory_create",
        "memory_update",
        "memory_retire",
        "memory_get",
        "memory_search",
        "memory_history",
        "memory_conflicts",
        "memory_add_spaces",
        "space_create",
        "space_list",
        "space_link",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }

    let linux = success_content(&process.call(
        3,
        "space_create",
        json!({ "name": "linux", "description": "Linux notes" }),
    ));
    assert_eq!(linux["name"], "linux");

    let link =
        success_content(&process.call(4, "space_link", json!({ "from": "coding", "to": "linux" })));
    assert!(link["from_space"].is_string());
    assert!(link["to_space"].is_string());
    assert_ne!(link["from_space"], link["to_space"]);

    let created = success_content(&process.call(
        5,
        "memory_create",
        json!({
            "subject": "Cache policy",
            "content": "Cache policy\nUse the local cache.",
            "spaces": ["coding"]
        }),
    ));
    let memory_id = created["memory_id"].as_str().unwrap().to_string();
    assert_eq!(created["action"]["kind"], "created");

    let updated = success_content(&process.call(
        6,
        "memory_update",
        json!({
            "id": memory_id,
            "content": "Cache policy\nUse the bounded local cache.",
            "spaces": ["coding"]
        }),
    ));
    assert_eq!(updated["action"]["kind"], "superseded");
    let current_id = updated["memory_id"].as_str().unwrap().to_string();

    let added = success_content(&process.call(
        7,
        "memory_add_spaces",
        json!({ "id": current_id, "spaces": ["linux"] }),
    ));
    assert_eq!(added["spaces_added"][0], "linux");

    let detail = success_content(&process.call(8, "memory_get", json!({ "id": current_id })));
    assert_eq!(detail["assertion"]["extraction"], "manual:write");
    assert_eq!(detail["source"]["kind"], "manual");
    assert_eq!(detail["source"]["location"], "manual://writes");
    assert_eq!(detail["memory"]["superseded_by"], Value::Null);

    let search = success_content(&process.call(
        9,
        "memory_search",
        json!({ "query": "bounded local cache", "spaces": ["coding"], "limit": 5 }),
    ));
    assert_eq!(search["hits"][0]["memory_id"], current_id);
    let matches = search["hits"][0]["spaces"].as_array().unwrap();
    let linux_match = matches
        .iter()
        .find(|space| space["name"] == "linux")
        .unwrap();
    assert_eq!(linux_match["via"], "linux");

    let history =
        success_content(&process.call(10, "memory_history", json!({ "subject": "cache policy" })));
    assert_eq!(history["memories"].as_array().unwrap().len(), 2);

    let second = success_content(&process.call(
        11,
        "memory_create",
        json!({
            "subject": "Cache policy",
            "content": "Cache policy\nUse no cache.",
            "spaces": ["coding"]
        }),
    ));
    let conflicts = success_content(&process.call(12, "memory_conflicts", json!({})));
    assert_eq!(conflicts["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(
        conflicts["conflicts"][0]["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let retired = success_content(&process.call(
        13,
        "memory_retire",
        json!({ "id": second["memory_id"], "reason": "not current" }),
    ));
    assert_eq!(retired["state"], "retired");
    let spaces = success_content(&process.call(14, "space_list", json!({})));
    assert_eq!(spaces["spaces"].as_array().unwrap().len(), 2);

    assert!(process.shutdown().success());
}

#[test]
fn real_binary_reports_protocol_errors_and_tool_errors_separately() {
    let (_dir, db) = database();
    let mut process = McpProcess::spawn(&db);

    process.send_raw("malformed");
    assert_eq!(process.read()["error"]["code"], -32700);
    process.send(json!([]));
    assert_eq!(process.read()["error"]["code"], -32600);

    let unknown_method = process.request(json!({
        "jsonrpc": "2.0",
        "id": "method-id",
        "method": "not/a/method"
    }));
    assert_eq!(unknown_method["id"], "method-id");
    assert_eq!(unknown_method["error"]["code"], -32601);

    let unknown_tool = process.call(33, "not_a_tool", json!({}));
    assert_eq!(unknown_tool["id"], 33);
    assert_eq!(unknown_tool["error"]["code"], -32602);
    assert!(unknown_tool["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));

    let invalid_args = process.call(34, "memory_create", json!({ "content": "missing spaces" }));
    assert_eq!(invalid_args["result"]["isError"], true);
    assert!(process.shutdown().success());
}

#[test]
fn independent_writer_processes_serialize_and_duplicate_writes_are_idempotent() {
    let (_dir, db) = database();
    let mut first = McpProcess::spawn(&db);
    let mut second = McpProcess::spawn(&db);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "name": "memory_create",
            "arguments": {
                "subject": "Concurrent note",
                "content": "The same concurrent write.",
                "spaces": ["coding"]
            }
        }
    });
    first.send(request.clone());
    second.send(request);
    let first_result = success_content(&first.read());
    let second_result = success_content(&second.read());
    assert_eq!(first_result["memory_id"], second_result["memory_id"]);

    assert!(first.shutdown().success());
    assert!(second.shutdown().success());
    let conn = Connection::open(&db).unwrap();
    for table in ["sources", "evidence", "assertions", "memories"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "unexpected rows in {table}");
    }
    let manual_sources: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM logical_sources WHERE kind='manual'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(manual_sources, 1);
}

#[test]
fn wal_reader_runs_while_writer_holds_lock_and_busy_wait_is_bounded() {
    let (_dir, db) = database();
    let mut process = McpProcess::spawn(&db);
    let lock = Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = Instant::now();
    let read =
        success_content(&process.call(40, "memory_search", json!({ "query": "not present" })));
    assert!(read["hits"].as_array().unwrap().is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));

    let write_started = Instant::now();
    let blocked = process.call(
        41,
        "memory_create",
        json!({ "content": "Blocked write", "spaces": ["coding"] }),
    );
    let elapsed = write_started.elapsed();
    assert_eq!(blocked["result"]["isError"], true);
    assert!(blocked["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("database is busy"));
    assert!(elapsed >= Duration::from_secs(4), "waited only {elapsed:?}");
    assert!(elapsed < Duration::from_secs(8), "waited {elapsed:?}");

    lock.execute_batch("ROLLBACK").unwrap();
    let created = success_content(&process.call(
        42,
        "memory_create",
        json!({ "content": "Blocked write", "spaces": ["coding"] }),
    ));
    assert_eq!(created["action"]["kind"], "created");
    assert!(process.shutdown().success());

    let conn = Connection::open(&db).unwrap();
    let memories: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(memories, 1);
}

#[test]
fn uninitialized_database_fails_on_stderr_without_protocol_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("missing.db");
    let output = Command::new(env!("CARGO_BIN_EXE_mind"))
        .arg("--db")
        .arg(db)
        .arg("mcp")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mind init"));
}
