//! Read-only and read-write allowed roots (MIND-3, ADR-026 addendum).
//!
//! `--allow-root <DIR>[:ro|:rw]`. `repo_outline` needs any root; `repo_ingest`
//! needs a read-write root. The most specific (longest) root governing a path
//! decides; the same directory listed twice gets the more restrictive mode; an
//! ingest target must not contain a read-only root. A root without a suffix is
//! read-write, as before.
//!
//! These tests were written before the implementation. Against the previous
//! code the root specs `<dir>:ro` / `<dir>:rw` cannot even be parsed (the
//! `:` was a list delimiter), so a read-only root could not be expressed and
//! every allowed root granted ingest, including its write into the repository.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
}

fn mind() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mind"))
}

fn db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mind.db");
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        let out = mind().env("MINDSPACE_DB", &db).args(args).output().unwrap();
        assert!(out.status.success());
    }
    (dir, db)
}

impl Server {
    /// `specs` are raw `--allow-root` values, e.g. `"/data/a:ro"`.
    fn start(db: &Path, specs: &[String]) -> Self {
        Self::start_with_env(db, specs, None)
    }

    fn start_with_env(db: &Path, specs: &[String], env_roots: Option<&str>) -> Self {
        let mut cmd = mind();
        cmd.env("MINDSPACE_DB", db)
            .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
            .arg("mcp");
        if let Some(e) = env_roots {
            cmd.env("MINDSPACE_MCP_ALLOW_ROOTS", e);
        }
        for s in specs {
            cmd.arg("--allow-root").arg(s);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self {
            child,
            stdin,
            out,
            next_id: 1,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"t","version":"0"}}),
        );
        assert!(init.get("result").is_some(), "server did not start: {init}");
        writeln!(
            s.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn outline(&mut self, path: &Path) -> Value {
        self.call("repo_outline", json!({"path": path.display().to_string()}))
    }

    fn ingest(&mut self, path: &Path, paths: Option<&[&str]>) -> Value {
        let mut args = json!({"path": path.display().to_string(), "spaces": ["p"]});
        if let Some(p) = paths {
            args["paths"] = json!(p);
        }
        self.call("repo_ingest", args)
    }

    /// Calls `repo_outline` or `repo_ingest` with just the arguments that tool takes.
    fn tool(&mut self, tool: &str, path: &Path) -> Value {
        match tool {
            "repo_outline" => self.outline(path),
            _ => self.ingest(path, None),
        }
    }

    fn hit_count(&mut self, query: &str) -> usize {
        let r = self.call("memory_search", json!({"query": query}));
        r["structuredContent"]["hits"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(usize::MAX)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn ok(r: &Value) -> bool {
    r["isError"] == false
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A markdown project with a distinct searchable word per directory.
fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "docs/guide.md", "# Guide\n\nguidemarker\n");
    write(d.path(), "secret/plan.md", "# Plan\n\nsecretmarker\n");
    write(d.path(), "open/notes.md", "# Notes\n\nopenmarker\n");
    d
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap()
}

fn spec(p: &Path, mode: &str) -> String {
    format!("{}:{mode}", canon(p).display())
}

/// Start the server only to see whether the specs are accepted.
fn startup(db: &Path, specs: &[&str], env_roots: Option<&str>) -> Output {
    let mut cmd = mind();
    cmd.env("MINDSPACE_DB", db)
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .arg("mcp");
    if let Some(e) = env_roots {
        cmd.env("MINDSPACE_MCP_ALLOW_ROOTS", e);
    }
    for s in specs {
        cmd.arg("--allow-root").arg(s);
    }
    cmd.stdin(Stdio::null()).output().unwrap()
}

// ---------------------------------------------------------------------------
// The feature
// ---------------------------------------------------------------------------

#[test]
fn read_only_root_allows_outline_but_refuses_ingest_and_writes_nothing() {
    let (_g, db) = db();
    let proj = project();
    // A git repository, so the identity marker (a write into the repo) is observable.
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(proj.path())
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.invalid")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let mut s = Server::start(&db, &[spec(proj.path(), "ro")]);
    assert!(
        ok(&s.outline(proj.path())),
        "outline must work on a read-only root"
    );

    let r = s.ingest(proj.path(), None);
    assert_eq!(
        r["isError"], true,
        "ingest into a read-only root must be refused"
    );
    let msg = text(&r);
    assert!(
        msg.contains("read-only"),
        "message must name the missing right: {msg}"
    );
    assert!(
        msg.contains(canon(proj.path()).to_str().unwrap()),
        "message must name the root: {msg}"
    );
    assert_eq!(s.hit_count("guidemarker"), 0, "database must be unchanged");
    assert!(
        !proj.path().join(".git/mindspace-repository-id").exists(),
        "no identity marker may be written into a read-only repository"
    );
}

#[test]
fn suffixless_and_rw_roots_stay_writable() {
    for suffix in ["", ":rw"] {
        let (_g, db) = db();
        let proj = project();
        let s = format!("{}{suffix}", canon(proj.path()).display());
        let mut srv = Server::start(&db, &[s]);
        let r = srv.ingest(proj.path(), None);
        assert!(ok(&r), "suffix '{suffix}': {}", text(&r));
        assert!(srv.hit_count("guidemarker") > 0, "suffix '{suffix}'");
    }
}

#[test]
fn environment_variable_accepts_modes() {
    let (_g, db) = db();
    let (ro, rw) = (project(), project());
    let env = format!(
        "{}:ro:{}:rw",
        canon(ro.path()).display(),
        canon(rw.path()).display()
    );
    let mut s = Server::start_with_env(&db, &[], Some(&env));
    assert_eq!(s.ingest(ro.path(), None)["isError"], true);
    assert!(ok(&s.ingest(rw.path(), None)));
    assert!(ok(&s.outline(ro.path())));
}

// ---------------------------------------------------------------------------
// Overlap rules
// ---------------------------------------------------------------------------

#[test]
fn most_specific_root_wins_rw_parent_ro_child() {
    let (_g, db) = db();
    let proj = project();
    let specs = [
        spec(proj.path(), "rw"),
        spec(&proj.path().join("secret"), "ro"),
    ];
    let mut s = Server::start(&db, &specs);

    // The read-only child is refused as a target and as a selected path.
    assert_eq!(s.ingest(&proj.path().join("secret"), None)["isError"], true);
    let sel = s.ingest(proj.path(), Some(&["secret"]));
    assert_eq!(
        sel["isError"],
        true,
        "paths into a read-only root: {}",
        text(&sel)
    );
    // Ingesting the parent unscoped would include the read-only child: refused.
    let all = s.ingest(proj.path(), None);
    assert_eq!(
        all["isError"],
        true,
        "parent containing a ro root: {}",
        text(&all)
    );
    assert!(text(&all).contains("read-only"), "{}", text(&all));
    assert_eq!(s.hit_count("secretmarker"), 0);
    // Selecting only the writable parts works, and never leaks the ro part.
    let r = s.ingest(proj.path(), Some(&["docs", "open"]));
    assert!(ok(&r), "{}", text(&r));
    assert!(s.hit_count("guidemarker") > 0 && s.hit_count("openmarker") > 0);
    assert_eq!(
        s.hit_count("secretmarker"),
        0,
        "read-only content leaked into the database"
    );
    // Reading is still allowed everywhere.
    assert!(ok(&s.outline(&proj.path().join("secret"))));
}

#[test]
fn most_specific_root_wins_ro_parent_rw_child() {
    let (_g, db) = db();
    let proj = project();
    let specs = [
        spec(proj.path(), "ro"),
        spec(&proj.path().join("open"), "rw"),
    ];
    let mut s = Server::start(&db, &specs);
    assert_eq!(s.ingest(proj.path(), None)["isError"], true);
    assert_eq!(s.ingest(proj.path(), Some(&["docs"]))["isError"], true);
    let r = s.ingest(&proj.path().join("open"), None);
    assert!(ok(&r), "{}", text(&r));
    assert!(s.hit_count("openmarker") > 0);
    assert_eq!(s.hit_count("guidemarker"), 0);
}

#[test]
fn same_directory_listed_twice_gets_the_more_restrictive_mode() {
    for order in [["ro", "rw"], ["rw", "ro"]] {
        let (_g, db) = db();
        let proj = project();
        let specs = [spec(proj.path(), order[0]), spec(proj.path(), order[1])];
        let mut s = Server::start(&db, &specs);
        assert_eq!(
            s.ingest(proj.path(), None)["isError"],
            true,
            "order {order:?}"
        );
        assert!(ok(&s.outline(proj.path())), "order {order:?}");
    }
}

#[test]
fn a_symlink_cannot_move_a_write_out_of_the_governing_root() {
    let (_g, db) = db();
    let (rw, ro) = (project(), project());
    // A symlink inside the writable root that points at the read-only root.
    std::os::unix::fs::symlink(ro.path(), rw.path().join("alias")).unwrap();
    let specs = [spec(rw.path(), "rw"), spec(ro.path(), "ro")];
    let mut s = Server::start(&db, &specs);
    let r = s.ingest(&rw.path().join("alias"), None);
    assert_eq!(r["isError"], true, "{}", text(&r));
    assert!(text(&r).contains("read-only"), "{}", text(&r));
    assert_eq!(s.hit_count("guidemarker"), 0);
}

// ---------------------------------------------------------------------------
// Path-handling adversaries (guards for the unchanged gate)
// ---------------------------------------------------------------------------

#[test]
fn prefix_collision_traversal_and_missing_paths_stay_outside() {
    let (_g, db) = db();
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("repo");
    let evil = base.path().join("repo-evil");
    write(&root, "a.md", "# A\n\nrootmarker\n");
    write(&evil, "b.md", "# B\n\nevilmarker\n");
    let mut s = Server::start(&db, &[canon(&root).display().to_string()]);

    for p in [
        evil.clone(),
        root.join("../repo-evil"),
        base.path().join("repo/../repo-evil"),
        base.path().join("repo-nonexistent"),
        PathBuf::from(format!("{}x", canon(&root).display())),
    ] {
        for tool in ["repo_outline", "repo_ingest"] {
            let r = s.tool(tool, &p);
            assert_eq!(r["isError"], true, "{tool} {}", p.display());
            assert!(
                text(&r).contains("outside the allowed roots"),
                "{tool}: {}",
                text(&r)
            );
        }
    }
    assert_eq!(s.hit_count("evilmarker"), 0);
}

#[test]
fn trailing_slash_and_case_variants() {
    let (_g, db) = db();
    let proj = project();
    let with_slash = format!("{}/:ro", canon(proj.path()).display());
    let mut s = Server::start(&db, &[with_slash]);
    assert!(ok(&s.outline(proj.path())), "trailing slash root");
    assert_eq!(s.ingest(proj.path(), None)["isError"], true);

    // A differently-cased spelling must not be accepted as the root. On a
    // case-insensitive filesystem the spelling resolves to the same directory
    // (and is then correctly governed by the same root), so only assert the
    // distinction when the filesystem is case-sensitive.
    let upper = PathBuf::from(canon(proj.path()).display().to_string().to_uppercase());
    if !upper.exists() {
        for tool in ["repo_outline", "repo_ingest"] {
            let r = s.tool(tool, &upper);
            assert_eq!(r["isError"], true, "{tool}");
            assert!(
                text(&r).contains("outside the allowed roots"),
                "{}",
                text(&r)
            );
        }
    }
}

#[test]
fn without_roots_both_tools_stay_disabled() {
    let (_g, db) = db();
    let proj = project();
    let mut s = Server::start(&db, &[]);
    for tool in ["repo_outline", "repo_ingest"] {
        let r = s.tool(tool, proj.path());
        assert_eq!(r["isError"], true);
        assert!(text(&r).contains("disabled"), "{}", text(&r));
    }
}

// ---------------------------------------------------------------------------
// Malformed specs fail at startup, not at request time
// ---------------------------------------------------------------------------

#[test]
fn malformed_root_specs_stop_the_server() {
    let (_g, db) = db();
    let proj = project();
    let p = canon(proj.path()).display().to_string();
    for bad in [
        "ro".to_string(),     // mode without a path
        format!("{p}:rx"),    // unknown mode
        format!("{p}:ro:rw"), // two modes for one path
        ":ro".to_string(),    // empty path
        "".to_string(),       // empty spec
        format!("{p}:"),      // trailing delimiter
        format!("{p}:RO"),    // modes are lowercase only
        "/".to_string(),      // filesystem root (existing rule)
        "/definitely/not/there:rw".to_string(),
    ] {
        let o = startup(&db, &[&bad], None);
        assert!(!o.status.success(), "spec {bad:?} must be refused");
        let err = String::from_utf8_lossy(&o.stderr);
        // The failure must come from the spec, not from an unrelated problem
        // such as a missing database (which would make this test vacuous).
        assert!(
            !err.contains("not initialized"),
            "spec {bad:?} failed for the wrong reason: {err}"
        );
        assert!(!err.trim().is_empty(), "spec {bad:?} must explain itself");
    }
    // A separate `--allow-root ro` never attaches to the previous argument.
    let o = startup(&db, &[&p, "ro"], None);
    assert!(
        !o.status.success(),
        "a lone `ro` argument must not bind to the previous root"
    );
    // A directory literally named `ro` is spelled `./ro` or by absolute path.
    let ok_spec = format!("{p}:ro");
    assert!(startup(&db, &[&ok_spec], None).status.success());
}
