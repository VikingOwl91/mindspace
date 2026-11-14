//! `server_status` against real `mind mcp` processes: which database a live server opened,
//! how it was chosen, and whether a plain `mind` would choose the same one.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn xdg(&self) -> PathBuf {
        self.home.path().join("xdg")
    }

    fn default_db(&self) -> PathBuf {
        self.xdg().join("mindspace").join("mind.db")
    }

    fn mind(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
        c.env_remove("MINDSPACE_DB")
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.xdg());
        c
    }

    fn init(&self, db: &Path) {
        let out = self
            .mind()
            .arg("--db")
            .arg(db)
            .arg("init")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Start `mind mcp` with the given extra arguments and environment, ask for the status.
    fn status(&self, args: &[&std::ffi::OsStr], db_env: Option<&Path>, full: bool) -> Value {
        let mut cmd = self.mind();
        if let Some(p) = db_env {
            cmd.env("MINDSPACE_DB", p);
        }
        let mut child = cmd
            .args(args)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut send = |v: Value| {
            writeln!(stdin, "{v}").unwrap();
            stdin.flush().unwrap();
        };
        let mut recv = || -> Value {
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        };
        send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"status-test","version":"1"}}}),
        );
        recv();
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"server_status","arguments":{"full_path":full}}}),
        );
        let reply = recv();
        drop(stdin);
        let _ = child.wait();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        reply["result"]["structuredContent"].clone()
    }
}

fn os(s: &Path) -> &std::ffi::OsStr {
    s.as_os_str()
}

#[test]
fn an_explicit_db_is_reported_and_flagged_when_a_plain_mind_would_choose_another() {
    let e = Env::new();
    let db = e.home.path().join("quickstart").join("mind.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    e.init(&db);
    let s = e.status(&[std::ffi::OsStr::new("--db"), os(&db)], None, true);
    assert_eq!(s["database"]["selected_by"], "--db", "{s}");
    assert_eq!(s["database"]["path"], db.display().to_string(), "{s}");
    assert_eq!(s["database"]["is_default"], false);
    assert_eq!(s["cli_default"]["matches"], false, "{s}");
    let hint = s["cli_default"]["hint"].as_str().unwrap();
    assert!(hint.contains("mind doctor"), "{hint}");
    assert!(
        hint.contains(&e.default_db().display().to_string()),
        "{hint}"
    );
}

#[test]
fn mindspace_db_from_the_environment_is_reported_and_matches_a_plain_mind() {
    let e = Env::new();
    let db = e.home.path().join("env.db");
    e.init(&db);
    let s = e.status(&[], Some(&db), true);
    assert!(
        s["database"]["selected_by"]
            .as_str()
            .unwrap()
            .contains("MINDSPACE_DB"),
        "{s}"
    );
    assert_eq!(s["cli_default"]["matches"], true, "{s}");
}

#[test]
fn the_xdg_default_is_reported_as_the_default_location() {
    let e = Env::new();
    std::fs::create_dir_all(e.default_db().parent().unwrap()).unwrap();
    e.init(&e.default_db());
    let s = e.status(&[], None, false);
    assert_eq!(s["database"]["is_default"], true, "{s}");
    assert!(
        s["database"]["selected_by"]
            .as_str()
            .unwrap()
            .contains("default"),
        "{s}"
    );
    assert_eq!(s["cli_default"]["matches"], true, "{s}");
    // the home directory is hidden unless the full path is requested
    let shown = s["database"]["path"].as_str().unwrap();
    assert!(shown.starts_with("~/"), "{shown}");
    assert!(!shown.contains(e.home.path().to_str().unwrap()), "{shown}");
    assert_eq!(
        s["database"]["schema_version"],
        s["database"]["supported_schema_version"]
    );
}

#[test]
fn a_missing_database_is_refused_and_not_created() {
    let e = Env::new();
    let db = e.home.path().join("missing.db");
    let out = e
        .mind()
        .arg("--db")
        .arg(&db)
        .arg("mcp")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!db.exists(), "the server must not create the database");
    assert!(!out.stderr.is_empty());
}

#[test]
fn an_invalid_database_is_refused_and_left_untouched() {
    let e = Env::new();
    let db = e.home.path().join("garbage.db");
    std::fs::write(&db, b"this is not a sqlite database").unwrap();
    let before = std::fs::read(&db).unwrap();
    let out = e
        .mind()
        .arg("--db")
        .arg(&db)
        .arg("mcp")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!out.stderr.is_empty());
    assert_eq!(std::fs::read(&db).unwrap(), before, "the file was modified");
}
