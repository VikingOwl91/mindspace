//! `repo_outline` / `repo_ingest` over the real MCP binary: they fail closed
//! without allowed roots, and the allowlist cannot be escaped.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
}

fn db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mind.db");
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", &db)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    (dir, db)
}

impl Server {
    fn start(db: &Path, roots: &[&Path]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mind"));
        cmd.env("MINDSPACE_DB", db).env_remove("MINDSPACE_MCP_ALLOW_ROOTS").arg("mcp");
        for r in roots {
            cmd.arg("--allow-root").arg(r);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self { child, stdin, out, next_id: 1 };
        s.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"t","version":"0"}}));
        writeln!(s.stdin, "{}", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params})).unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "docs/guide.md", "# Guide\n\nguidemarker\n");
    write(d.path(), "src/lib.rs", "pub fn alpha_symbol() {}\n");
    write(d.path(), "other/notes.md", "# Notes\n\nnotesmarker\n");
    d
}

#[test]
fn tools_fail_closed_without_allowed_roots() {
    let (_g, db) = db();
    let proj = project();
    let mut s = Server::start(&db, &[]);
    let p = proj.path().display().to_string();
    for (tool, args) in [
        ("repo_outline", json!({"path": p})),
        ("repo_ingest", json!({"path": p, "spaces": ["p"]})),
    ] {
        let r = s.call(tool, args);
        assert_eq!(r["isError"], true, "{tool}");
        assert!(text(&r).contains("disabled"), "{}", text(&r));
    }
}

#[test]
fn allowlist_cannot_be_escaped() {
    let (_g, db) = db();
    let proj = project();
    let outside = project();
    let root = proj.path().canonicalize().unwrap();
    let mut s = Server::start(&db, &[&root]);

    let denied = |s: &mut Server, p: String| {
        let r = s.call("repo_outline", json!({"path": p}));
        assert_eq!(r["isError"], true);
        text(&r)
    };
    // Outside the root, existing vs. non-existing: indistinguishable.
    let a = denied(&mut s, outside.path().display().to_string());
    let b = denied(&mut s, "/definitely/not/there/at/all".to_string());
    assert_eq!(a, b, "refusals must not reveal what exists");
    assert!(a.contains("outside the allowed roots"));
    // Relative path, `..` traversal out of the root, symlink out of the root.
    assert!(denied(&mut s, "docs".into()).contains("absolute"));
    let traversal = format!("{}/../{}", root.display(), outside.path().file_name().unwrap().to_string_lossy());
    assert!(denied(&mut s, traversal).contains("outside the allowed roots"));
    std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
    assert!(denied(&mut s, root.join("escape").display().to_string()).contains("outside the allowed roots"));
    // `.git` internals and plain files.
    std::fs::create_dir_all(root.join("sub/.git")).unwrap();
    assert!(denied(&mut s, root.join("sub/.git").display().to_string()).contains(".git"));
    assert!(denied(&mut s, root.join("src/lib.rs").display().to_string()).contains("directory"));
    // Ingest obeys the same gate and writes nothing when denied.
    let r = s.call("repo_ingest", json!({"path": outside.path().display().to_string(), "spaces": ["p"]}));
    assert_eq!(r["isError"], true);
    let hits = s.call("memory_search", json!({"query": "notesmarker"}));
    assert_eq!(hits["structuredContent"]["hits"].as_array().unwrap().len(), 0);
}

#[test]
fn startup_rejects_filesystem_root_and_missing_roots() {
    let (_g, db) = db();
    for bad in ["/", "/definitely/not/a/dir"] {
        let out = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", &db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .args(["mcp", "--allow-root", bad])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad} must be refused");
    }
}

#[test]
fn outline_then_scoped_ingest_then_search() {
    let (_g, db) = db();
    let proj = project();
    let root = proj.path().canonicalize().unwrap();
    let mut s = Server::start(&db, &[&root]);
    let p = root.display().to_string();

    let o = s.call("repo_outline", json!({"path": p, "depth": 1}));
    assert_eq!(o["isError"], false, "{}", text(&o));
    let nodes = o["structuredContent"]["outline"]["nodes"].as_array().unwrap();
    let dirs: Vec<&str> = nodes.iter().map(|n| n["path"].as_str().unwrap()).collect();
    assert!(dirs.contains(&"docs") && dirs.contains(&"src") && dirs.contains(&"other"));

    // Ingest only docs and src (what the outline pointed at).
    let r = s.call("repo_ingest", json!({"path": p, "spaces": ["p"], "paths": ["docs", "src"]}));
    assert_eq!(r["isError"], false, "{}", text(&r));
    let found = s.call("memory_search", json!({"query": "guidemarker"}));
    assert_eq!(found["structuredContent"]["hits"].as_array().unwrap().len(), 1);
    let skipped = s.call("memory_search", json!({"query": "notesmarker"}));
    assert_eq!(skipped["structuredContent"]["hits"].as_array().unwrap().len(), 0);
    let code = s.call("code_search", json!({"query": "alpha_symbol"}));
    assert_eq!(code["structuredContent"]["hits"].as_array().unwrap().len(), 1);
    // The ingest attached the directory to space `p`; code_search can scope by it.
    let scoped = s.call("code_search", json!({"query": "alpha_symbol", "spaces": ["p"]}));
    let hits = scoped["structuredContent"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["spaces"], json!(["p"]));
    let other = s.call("code_search", json!({"query": "alpha_symbol", "spaces": ["nope"]}));
    assert_eq!(other["isError"], true, "unknown space is an error");

    // Unknown space and escaping selection are errors, not writes.
    let bad_space = s.call("repo_ingest", json!({"path": p, "spaces": ["nope"]}));
    assert_eq!(bad_space["isError"], true);
    let bad_sel = s.call("repo_ingest", json!({"path": p, "spaces": ["p"], "paths": ["../x"]}));
    assert_eq!(bad_sel["isError"], true);
}

#[test]
fn unscoped_ingest_of_a_huge_tree_is_refused() {
    let (_g, db) = db();
    let big = tempfile::tempdir().unwrap();
    for i in 0..2001 {
        write(big.path(), &format!("many/f{i}.txt"), "x\n");
    }
    write(big.path(), "small/a.md", "# A\n\nsmallmarker\n");
    let root = big.path().canonicalize().unwrap();
    let mut s = Server::start(&db, &[&root]);
    let p = root.display().to_string();

    let r = s.call("repo_ingest", json!({"path": p, "spaces": ["p"]}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("repo_outline"), "{}", text(&r));
    let ok = s.call("repo_ingest", json!({"path": p, "spaces": ["p"], "paths": ["small"]}));
    assert_eq!(ok["isError"], false, "{}", text(&ok));
}
