//! M7.3a grant core (ADR-030, level A1): the CLI, the request tool, and what
//! `mind mcp --grants` does with the store. A request is never a grant.
//!
//! These tests prove the mechanics. They do NOT show protection against an agent
//! that can run `mind grant` as the same user (ADR-030, tier T2): that is the
//! documented limit of this level.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct World {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    state: PathBuf,
    project: PathBuf,
}

fn world() -> World {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let db = root.join("xdg/mindspace/mind.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(project.join("docs/guide.md"), "# Guide\n\ngrantmarker\n").unwrap();
    let w = World {
        state: root.join("state"),
        _t: t,
        db,
        project,
        root,
    };
    for args in [vec!["init"], vec!["space", "create", "p"]] {
        assert!(mind(&w, &args).output().unwrap().status.success());
    }
    w
}

/// HOME is the world's root, so the "not your home directory" rule applies to it.
fn mind(w: &World, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    c.args(args)
        .env("MINDSPACE_DB", &w.db)
        .env("XDG_DATA_HOME", w.root.join("xdg"))
        .env("XDG_STATE_HOME", &w.state)
        .env("HOME", &w.root)
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .env_remove("MINDSPACE_SANDBOX_STAGE2")
        .env_remove("MINDSPACE_SANDBOX_CANARY")
        .stdin(Stdio::null());
    c
}

fn run(w: &World, args: &[&str]) -> Output {
    mind(w, args).output().unwrap()
}

fn ok(w: &World, args: &[&str]) -> String {
    let o = run(w, args);
    assert!(
        o.status.success(),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn fail(w: &World, args: &[&str]) -> String {
    let o = run(w, args);
    assert!(!o.status.success(), "{args:?} should fail");
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn grants_json(w: &World, all: bool) -> Vec<Value> {
    let mut a = vec!["grant", "list", "--json"];
    if all {
        a.push("--all");
    }
    serde_json::from_str::<Value>(&ok(w, &a))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn requests_json(w: &World) -> Vec<Value> {
    serde_json::from_str::<Value>(&ok(w, &["grant", "requests", "--json"]))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn first_id(w: &World, kind: &str) -> String {
    let v = if kind == "grant" {
        grants_json(w, true)
    } else {
        requests_json(w)
    };
    v[0]["id"].as_str().unwrap().to_string()
}

// ------------------------------------------------------------------- server

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    id: u64,
    stderr: PathBuf,
}

impl Server {
    fn start(w: &World, args: &[&str]) -> Self {
        let stderr = w.root.join(format!("server-{}.err", rand_suffix()));
        let mut c = mind(w, &[&["mcp"], args].concat());
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self {
            child,
            stdin,
            out,
            id: 0,
            stderr,
        };
        s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"t","version":"0"}}),
        );
        writeln!(
            s.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| json!({"error":"no answer"}))
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn outline(&mut self, path: &Path) -> Value {
        self.call("repo_outline", json!({"path": path.display().to_string()}))
    }

    fn ingest(&mut self, path: &Path) -> Value {
        self.call(
            "repo_ingest",
            json!({"path": path.display().to_string(), "spaces": ["p"], "paths": ["docs"]}),
        )
    }

    fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn stop(mut self) -> String {
        drop(self.stdin);
        let end = Instant::now() + Duration::from_secs(15);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < end, "server did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

fn rand_suffix() -> String {
    format!(
        "{:x}",
        std::process::id() as u64 * 1_000_003 + Instant::now().elapsed().as_nanos() as u64
    ) + &format!("{:?}", std::thread::current().id()).replace(['(', ')'], "")
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

// -------------------------------------------------------------------- tests

#[test]
fn add_list_revoke_with_ro_and_rw() {
    let w = world();
    let p = w.project.display().to_string();
    assert!(grants_json(&w, true).is_empty());
    ok(&w, &["grant", "add", &p, "--yes"]);
    ok(&w, &["grant", "add", &p, "--rw", "--ttl", "2h", "--yes"]);
    let all = grants_json(&w, false);
    assert_eq!(all.len(), 2);
    let ro = all.iter().find(|g| g["access"] == "ro").unwrap();
    let rw = all.iter().find(|g| g["access"] == "rw").unwrap();
    assert_eq!(ro["path"], p.as_str());
    assert!(ro["expires_at"].is_null());
    assert!(!rw["expires_at"].is_null());
    assert_ne!(ro["id"], rw["id"], "unique ids");
    assert_eq!(ro["status"], "active");
    // revoke
    let id = ro["id"].as_str().unwrap();
    let out = ok(&w, &["grant", "revoke", id, "--reason", "done"]);
    assert!(out.contains("Servers that are running keep their access"));
    assert_eq!(grants_json(&w, false).len(), 1);
    let everything = grants_json(&w, true);
    assert_eq!(everything.len(), 2);
    assert!(everything
        .iter()
        .any(|g| g["id"] == id && g["status"] == "revoked"));
    assert!(fail(&w, &["grant", "revoke", id]).contains("already revoked"));
    assert!(fail(&w, &["grant", "revoke", "g-000000000000"]).contains("no grant"));
    assert!(fail(&w, &["grant", "revoke", "nonsense"]).contains("not a grant id"));
    // the store is private
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(w.state.join("mindspace/grants.jsonl"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn without_a_terminal_and_without_yes_nothing_is_granted() {
    let w = world();
    let p = w.project.display().to_string();
    let err = fail(&w, &["grant", "add", &p]);
    assert!(err.contains("no terminal"), "{err}");
    assert!(!w.state.join("mindspace/grants.jsonl").exists());
    assert!(grants_json(&w, true).is_empty());
}

#[test]
fn unsafe_or_invalid_paths_and_ttls_are_refused() {
    let w = world();
    let root = w.root.display().to_string();
    let store = w.state.join("mindspace");
    std::fs::create_dir_all(&store).unwrap();
    let file = w.project.join("docs/guide.md");
    for (arg, expected) in [
        (root.as_str(), "overlaps the grant store"),
        (store.to_str().unwrap(), "overlaps the grant store"),
        (
            w.db.parent().unwrap().to_str().unwrap(),
            "database directory",
        ),
        ("/", "filesystem root"),
        (file.to_str().unwrap(), "not a directory"),
        ("/definitely/not/there", "No such file"),
    ] {
        let err = fail(&w, &["grant", "add", arg, "--yes"]);
        assert!(err.contains(expected), "{arg}: {err}");
    }
    let p = w.project.display().to_string();
    for ttl in ["0s", "10", "1y", "366d", "-1h"] {
        let _ = fail(&w, &["grant", "add", &p, "--ttl", ttl, "--yes"]);
    }
    assert!(grants_json(&w, true).is_empty(), "nothing was recorded");
}

#[test]
fn a_request_is_recorded_but_never_creates_or_changes_a_grant() {
    let w = world();
    let p = w.project.clone();
    let mut s = Server::start(&w, &["--grants"]);
    let r = s.call(
        "repo_request_access",
        json!({"path": p.display().to_string(), "access": "rw", "reason": "please"}),
    );
    assert_eq!(r["isError"], false, "{}", text(&r));
    let sc = &r["structuredContent"];
    assert_eq!(sc["status"], "pending");
    assert_eq!(sc["granted"], false);
    let rid = sc["request_id"].as_str().unwrap().to_string();
    assert!(sc["message"]
        .as_str()
        .unwrap()
        .contains("REQUEST, not a permission"));
    // the repetition is the same request
    let again = s.call(
        "repo_request_access",
        json!({"path": p.display().to_string(), "access": "rw"}),
    );
    assert_eq!(again["structuredContent"]["request_id"], rid.as_str());
    assert_eq!(again["structuredContent"]["already_pending"], true);
    // no access, no grant, no store
    assert_eq!(s.outline(&p)["isError"], true);
    assert_eq!(s.ingest(&p)["isError"], true);
    let _ = s.stop();
    assert!(grants_json(&w, true).is_empty());
    assert!(!w.state.join("mindspace/grants.jsonl").exists());
    let pending = requests_json(&w);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], rid.as_str());
    assert_eq!(pending[0]["untrusted"], true);
    // a restart with --grants still has no access
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&p)["isError"], true);
    let _ = s.stop();
}

#[test]
fn the_request_tool_takes_no_authority_from_its_arguments_or_its_log() {
    let w = world();
    let p = w.project.display().to_string();
    let mut s = Server::start(&w, &["--grants"]);
    for bad in [
        json!({"path": p, "approved": true}),
        json!({"path": p, "granted": true, "id": "g-0123456789ab"}),
        json!({"path": p, "access": "admin"}),
        json!({"path": "relative"}),
        json!({"path": ""}),
        json!({"access": "ro"}),
    ] {
        let r = s.call("repo_request_access", bad.clone());
        assert_eq!(r["isError"], true, "{bad}");
    }
    let _ = s.stop();
    assert!(requests_json(&w).is_empty());
    // lines in the request log that look like approvals are ignored data
    let log = w.db.parent().unwrap().join("grant-requests.jsonl");
    std::fs::write(
        &log,
        format!(
            "{{\"id\":\"r-0123456789ab\",\"path\":\"{p}\",\"access\":\"rw\",\"ts\":\"t\",\"granted\":true}}\n\
             {{\"kind\":\"granted\",\"id\":\"g-0123456789ab\",\"path\":\"{p}\",\"access\":\"rw\"}}\n"
        ),
    )
    .unwrap();
    assert!(
        requests_json(&w).is_empty(),
        "malformed lines are not requests"
    );
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&w.project)["isError"], true);
    let _ = s.stop();
    assert!(grants_json(&w, true).is_empty());
}

#[test]
fn approving_a_request_gives_ro_access_at_the_next_start_under_the_full_sandbox() {
    let w = world();
    let p = w.project.clone();
    let mut s = Server::start(&w, &["--grants"]);
    s.call(
        "repo_request_access",
        json!({"path": p.display().to_string(), "access": "rw"}),
    );
    let _ = s.stop();
    let rid = first_id(&w, "request");
    // the human looks at it and downgrades to read-only
    let out = ok(&w, &["grant", "approve", &rid, "--ro", "--yes"]);
    assert!(
        out.contains("asked for:") && out.contains("READ-ONLY"),
        "{out}"
    );
    assert!(
        requests_json(&w).is_empty(),
        "answered requests leave the pending list"
    );
    assert!(fail(&w, &["grant", "approve", &rid, "--yes"]).contains("already answered"));
    assert!(fail(&w, &["grant", "deny", &rid]).contains("already answered"));
    // without --grants the store is not read
    let mut s = Server::start(&w, &[]);
    assert_eq!(s.outline(&p)["isError"], true);
    let _ = s.stop();
    // with --grants: outline works, ingest is refused (read-only), the sandbox is intact
    let mut s = Server::start(&w, &["--grants"]);
    let o = s.outline(&p);
    assert_eq!(o["isError"], false, "{}", text(&o));
    let sc = &o["structuredContent"];
    assert!(
        sc["sandbox"].as_str().unwrap().starts_with("enforced"),
        "{sc}"
    );
    #[cfg(target_arch = "x86_64")]
    assert_eq!(sc["seccomp"]["state"], "enforced", "{sc}");
    let i = s.ingest(&p);
    assert_eq!(i["isError"], true, "a read-only grant cannot ingest");
    assert!(
        s.stderr_text().contains("grants: using"),
        "{}",
        s.stderr_text()
    );
    let _ = s.stop();
}

#[test]
fn a_rw_grant_ingests_and_a_denied_request_can_never_be_approved() {
    let w = world();
    let p = w.project.clone();
    ok(&w, &["grant", "add", p.to_str().unwrap(), "--rw", "--yes"]);
    let mut s = Server::start(&w, &["--grants"]);
    let i = s.ingest(&p);
    assert_eq!(i["isError"], false, "{}", text(&i));
    let hits = s.call("memory_search", json!({"query": "grantmarker"}));
    assert_eq!(
        hits["structuredContent"]["hits"].as_array().unwrap().len(),
        1
    );
    // a path nobody granted is still refused
    let other = w.root.join("other");
    std::fs::create_dir(&other).unwrap();
    assert_eq!(s.outline(&other)["isError"], true);
    let _ = s.stop();
    // deny
    let mut s = Server::start(&w, &["--grants"]);
    s.call(
        "repo_request_access",
        json!({"path": other.display().to_string()}),
    );
    let _ = s.stop();
    let rid = first_id(&w, "request");
    ok(&w, &["grant", "deny", &rid]);
    assert!(fail(&w, &["grant", "approve", &rid, "--yes"]).contains("already answered"));
}

#[test]
fn a_restart_loads_only_valid_grants_expired_revoked_and_tampered_ones_are_not_used() {
    let w = world();
    let keep = w.project.clone();
    let expiring = w.root.join("expiring");
    let revoked = w.root.join("revoked");
    for d in [&expiring, &revoked] {
        std::fs::create_dir_all(d.join("docs")).unwrap();
    }
    ok(&w, &["grant", "add", keep.to_str().unwrap(), "--yes"]);
    ok(
        &w,
        &[
            "grant",
            "add",
            expiring.to_str().unwrap(),
            "--ttl",
            "1s",
            "--yes",
        ],
    );
    ok(&w, &["grant", "add", revoked.to_str().unwrap(), "--yes"]);
    let rid = grants_json(&w, true)
        .iter()
        .find(|g| g["path"] == revoked.to_str().unwrap())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(&w, &["grant", "revoke", &rid]);
    std::thread::sleep(Duration::from_millis(2500));
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&keep)["isError"], false);
    assert_eq!(s.outline(&expiring)["isError"], true, "expired");
    assert_eq!(s.outline(&revoked)["isError"], true, "revoked");
    let _ = s.stop();
    // tamper with the store: a flipped access word is detected, NOTHING is used
    let store = w.state.join("mindspace/grants.jsonl");
    let text0 = std::fs::read_to_string(&store).unwrap();
    std::fs::write(&store, text0.replacen("\"ro\"", "\"rw\"", 1)).unwrap();
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(
        s.outline(&keep)["isError"],
        true,
        "a broken store grants nothing"
    );
    let err = s.stop();
    assert!(err.contains("does not verify"), "{err}");
    assert!(fail(&w, &["grant", "list"]).contains("does not verify"));
    assert!(
        fail(&w, &["grant", "add", keep.to_str().unwrap(), "--yes"]).contains("does not verify")
    );
    // restoring the original text restores the store
    std::fs::write(&store, &text0).unwrap();
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&keep)["isError"], false);
    let _ = s.stop();
    // a group-writable store is not used either
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o664)).unwrap();
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&keep)["isError"], true);
    let _ = s.stop();
}

#[test]
fn a_directory_swapped_or_repointed_after_approval_is_not_used() {
    let w = world();
    let p = w.project.clone();
    ok(&w, &["grant", "add", p.to_str().unwrap(), "--yes"]);
    // 1. replaced by another directory at the same path
    std::fs::rename(&p, w.root.join("moved")).unwrap();
    std::fs::create_dir_all(p.join("docs")).unwrap();
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&p)["isError"], true);
    let err = s.stop();
    assert!(
        err.contains("NOT using") && err.contains("device/inode"),
        "{err}"
    );
    assert!(
        grants_json(&w, false)[0]["last_start_refusal"].is_string(),
        "the refusal is recorded in the audit log"
    );
    // 2. the path became a symlink to somewhere else
    std::fs::remove_dir_all(&p).unwrap();
    std::os::unix::fs::symlink(w.root.join("moved"), &p).unwrap();
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&p)["isError"], true);
    let err = s.stop();
    assert!(err.contains("NOT using"), "{err}");
}

#[test]
fn a_static_root_over_the_grant_store_is_refused_with_grants() {
    let w = world();
    // the state directory lives inside this root
    let o = mind(
        &w,
        &["mcp", "--grants", "--allow-root", w.root.to_str().unwrap()],
    )
    .output()
    .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("overlaps the grant store"), "{err}");
    // without --grants nothing reads the store, so the existing behavior stays
    let mut s = Server::start(&w, &["--allow-root", w.project.to_str().unwrap()]);
    assert_eq!(s.outline(&w.project)["isError"], false);
    let _ = s.stop();
}

#[test]
fn grants_add_to_static_roots_and_do_not_weaken_the_sandbox() {
    let w = world();
    let granted = w.root.join("granted");
    std::fs::create_dir_all(granted.join("docs")).unwrap();
    ok(&w, &["grant", "add", granted.to_str().unwrap(), "--yes"]);
    let mut s = Server::start(
        &w,
        &[
            "--grants",
            "--allow-root",
            &format!("{}:ro", w.project.display()),
        ],
    );
    for dir in [&w.project, &granted] {
        let o = s.outline(dir);
        assert_eq!(o["isError"], false, "{}", text(&o));
        assert!(o["structuredContent"]["sandbox"]
            .as_str()
            .unwrap()
            .starts_with("enforced"));
    }
    // outside both: refused by the allowlist (and the kernel)
    assert_eq!(s.outline(&w.root)["isError"], true);
    let _ = s.stop();
}

// ------------------------------------------------- review: races and faults

fn project_dir(w: &World, name: &str) -> PathBuf {
    let d = w.root.join(name);
    std::fs::create_dir_all(d.join("docs")).unwrap();
    std::fs::write(d.join("docs/a.md"), format!("# {name}\n")).unwrap();
    d
}

#[test]
fn many_processes_writing_at_once_keep_one_valid_chain() {
    let w = world();
    let dirs: Vec<PathBuf> = (0..12).map(|i| project_dir(&w, &format!("c{i}"))).collect();
    let children: Vec<_> = dirs
        .iter()
        .map(|d| {
            let mut c = mind(&w, &["grant", "add", d.to_str().unwrap(), "--rw", "--yes"]);
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    assert_eq!(grants_json(&w, true).len(), 12);
    // approvals and revokes interleaved with more adds
    let ids: Vec<String> = grants_json(&w, true)
        .iter()
        .map(|g| g["id"].as_str().unwrap().to_string())
        .collect();
    let mut kids = Vec::new();
    for id in ids.iter().take(6) {
        let mut c = mind(&w, &["grant", "revoke", id]);
        kids.push(
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for i in 0..4 {
        let d = project_dir(&w, &format!("d{i}"));
        let mut c = mind(&w, &["grant", "add", d.to_str().unwrap(), "--yes"]);
        kids.push(
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    let all = grants_json(&w, true);
    assert_eq!(all.len(), 16);
    assert_eq!(all.iter().filter(|g| g["status"] == "revoked").count(), 6);
    let unique: std::collections::BTreeSet<_> =
        all.iter().map(|g| g["id"].as_str().unwrap()).collect();
    assert_eq!(unique.len(), 16);
}

#[test]
fn killing_writers_midway_never_leaves_an_unusable_store() {
    let w = world();
    let dirs: Vec<PathBuf> = (0..24).map(|i| project_dir(&w, &format!("k{i}"))).collect();
    let mut kids: Vec<Child> = dirs
        .iter()
        .map(|d| {
            let mut c = mind(&w, &["grant", "add", d.to_str().unwrap(), "--yes"]);
            c.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(Duration::from_millis(15));
    for (i, k) in kids.iter_mut().enumerate() {
        if i % 2 == 0 {
            let _ = k.kill();
        }
    }
    for mut k in kids {
        let _ = k.wait();
    }
    // whatever was completed is there and verifies; the store is still writable
    let survived = grants_json(&w, true).len();
    assert!(survived <= 24);
    let extra = project_dir(&w, "after");
    ok(&w, &["grant", "add", extra.to_str().unwrap(), "--yes"]);
    assert_eq!(grants_json(&w, true).len(), survived + 1);
}

#[test]
fn a_torn_last_line_does_not_disable_the_store_and_is_repaired_by_the_next_write() {
    let w = world();
    ok(&w, &["grant", "add", w.project.to_str().unwrap(), "--yes"]);
    let store = w.state.join("mindspace/grants.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&store)
        .unwrap();
    f.write_all(b"{\"seq\":2,\"ts\":\"2026").unwrap();
    drop(f);
    assert_eq!(
        grants_json(&w, false).len(),
        1,
        "the verified prefix is still used"
    );
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&w.project)["isError"], false);
    let _ = s.stop();
    let other = project_dir(&w, "second");
    ok(&w, &["grant", "add", other.to_str().unwrap(), "--yes"]);
    assert_eq!(grants_json(&w, false).len(), 2);
    let text = std::fs::read_to_string(&store).unwrap();
    assert!(text.ends_with('\n') && !text.contains("\"seq\":2,\"ts\":\"2026\n"));
}

#[test]
fn a_write_that_hits_a_file_size_limit_leaves_the_store_exactly_as_it_was() {
    use std::os::unix::process::CommandExt;
    let w = world();
    ok(&w, &["grant", "add", w.project.to_str().unwrap(), "--yes"]);
    let store = w.state.join("mindspace/grants.jsonl");
    let before = std::fs::read(&store).unwrap();
    let extra = project_dir(&w, "limited");
    let limit = before.len() as libc::rlim_t + 40; // a new event does not fit
    let mut c = mind(&w, &["grant", "add", extra.to_str().unwrap(), "--yes"]);
    // SAFETY: setrlimit and signal(SIG_IGN) only, between fork and exec.
    unsafe {
        c.pre_exec(move || {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let rl = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let o = c.output().unwrap();
    assert!(!o.status.success(), "the write must fail");
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot write"));
    assert_eq!(
        std::fs::read(&store).unwrap(),
        before,
        "no half line was left behind"
    );
    assert_eq!(grants_json(&w, true).len(), 1);
    // and without the limit it works
    ok(&w, &["grant", "add", extra.to_str().unwrap(), "--yes"]);
    assert_eq!(grants_json(&w, true).len(), 2);
}

#[test]
fn a_symlinked_or_hard_linked_store_is_refused_by_the_commands_and_the_server() {
    let w = world();
    ok(&w, &["grant", "add", w.project.to_str().unwrap(), "--yes"]);
    let store = w.state.join("mindspace/grants.jsonl");
    let copy = w.root.join("copy.jsonl");
    std::fs::copy(&store, &copy).unwrap();
    // hard link
    let link = w.root.join("hard.jsonl");
    std::fs::hard_link(&store, &link).unwrap();
    assert!(fail(&w, &["grant", "list"]).contains("hard link"));
    let mut s = Server::start(&w, &["--grants"]);
    assert_eq!(s.outline(&w.project)["isError"], true);
    assert!(s.stop().contains("hard link"));
    std::fs::remove_file(&link).unwrap();
    // symlink
    std::fs::remove_file(&store).unwrap();
    std::os::unix::fs::symlink(&copy, &store).unwrap();
    let before = std::fs::read(&copy).unwrap();
    let _ = fail(&w, &["grant", "list"]);
    let extra = project_dir(&w, "linked");
    let _ = fail(&w, &["grant", "add", extra.to_str().unwrap(), "--yes"]);
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        before,
        "nothing written through the link"
    );
}

#[test]
fn swapping_a_granted_directory_while_the_server_starts_never_widens_access() {
    let w = world();
    let granted = project_dir(&w, "granted");
    let decoy = project_dir(&w, "decoy");
    ok(&w, &["grant", "add", granted.to_str().unwrap(), "--yes"]);
    let backup = w.root.join("granted.bak");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapper = {
        let (granted, decoy, backup, stop) =
            (granted.clone(), decoy.clone(), backup.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // granted -> backup, granted becomes a symlink to the decoy, then restore
                if std::fs::rename(&granted, &backup).is_ok() {
                    let _ = std::os::unix::fs::symlink(&decoy, &granted);
                    std::thread::sleep(Duration::from_millis(3));
                    let _ = std::fs::remove_file(&granted);
                    let _ = std::fs::rename(&backup, &granted);
                }
                std::thread::sleep(Duration::from_millis(12));
            }
        })
    };
    let (mut refused, mut served) = (0, 0);
    let mut reasons: std::collections::BTreeMap<String, u32> = Default::default();
    for _ in 0..60 {
        let out = mind(&w, &["mcp", "--grants"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = out;
        let mut stdin = out.stdin.take().unwrap();
        let mut reader = BufReader::new(out.stdout.take().unwrap());
        let mut ask = |id: u64, m: &str, p: Value| -> Value {
            if id == 0 {
                // a notification: no id, no answer
                let _ = writeln!(stdin, "{}", json!({"jsonrpc":"2.0","method":m}));
                return Value::Null;
            }
            let _ = writeln!(
                stdin,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":m,"params":p})
            );
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            serde_json::from_str(&line).unwrap_or(Value::Null)
        };
        ask(
            1,
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}),
        );
        ask(0, "notifications/initialized", json!({}));
        let d = ask(
            2,
            "tools/call",
            json!({"name":"repo_outline","arguments":{"path":decoy.display().to_string()}}),
        );
        let g = ask(
            3,
            "tools/call",
            json!({"name":"repo_outline","arguments":{"path":granted.display().to_string()}}),
        );
        if d.is_null() {
            // the server refused to start ("changed while starting"): fail closed
            refused += 1;
            drop(stdin);
            let mut err = String::new();
            let _ = std::io::Read::read_to_string(&mut out.stderr.take().unwrap(), &mut err);
            let _ = out.wait();
            let why = [
                "changed while the server was starting",
                "not the directory that was approved",
                "cannot open without following symlinks",
            ]
            .into_iter()
            .find(|k| err.contains(k))
            .unwrap_or("other");
            *reasons.entry(why.to_string()).or_default() += 1;
            continue;
        }
        assert_eq!(
            d["result"]["isError"], true,
            "the never-granted decoy became accessible: {d}"
        );
        if g["result"]["isError"] == false {
            served += 1;
        } else {
            refused += 1;
        }
        drop(stdin);
        let _ = out.wait();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    swapper.join().unwrap();
    eprintln!(
        "swap race: {served} starts served the granted directory, {refused} refused: {reasons:?}"
    );
    assert_eq!(served + refused, 60);
}

// ------------------------------------------------ review: request-log faults

fn memory_and_static_root_still_work(s: &mut Server, root: &Path) {
    let created = s.call(
        "memory_create",
        json!({"content": "unaffected by the log", "spaces": ["p"]}),
    );
    assert_eq!(created["isError"], false, "{}", text(&created));
    let found = s.call("memory_search", json!({"query": "unaffected"}));
    assert_eq!(
        found["structuredContent"]["hits"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        s.outline(root)["isError"],
        false,
        "repository tools are unaffected"
    );
}

#[test]
fn a_broken_request_log_never_affects_memory_or_repository_tools() {
    use std::os::unix::fs::PermissionsExt;
    let w = world();
    let log = w.db.parent().unwrap().join("grant-requests.jsonl");
    let p = w.project.display().to_string();
    let root_arg = ["--allow-root", p.as_str()];
    // a directory in the log's place
    std::fs::create_dir(&log).unwrap();
    let mut s = Server::start(&w, &root_arg);
    let r = s.call("repo_request_access", json!({"path": &p}));
    assert_eq!(r["isError"], true);
    memory_and_static_root_still_work(&mut s, &w.project);
    let _ = s.stop();
    std::fs::remove_dir(&log).unwrap();
    // 300 KiB: over the limit
    std::fs::write(&log, vec![b'x'; 300 * 1024]).unwrap();
    let mut s = Server::start(&w, &root_arg);
    let r = s.call("repo_request_access", json!({"path": &p}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("too many"), "{}", text(&r));
    memory_and_static_root_still_work(&mut s, &w.project);
    let _ = s.stop();
    // binary garbage with no final newline, within the limit
    std::fs::write(&log, b"\xff\xfe\x00garbage without newline").unwrap();
    let mut s = Server::start(&w, &root_arg);
    let r = s.call("repo_request_access", json!({"path": &p}));
    assert_eq!(r["isError"], false, "{}", text(&r));
    memory_and_static_root_still_work(&mut s, &w.project);
    let _ = s.stop();
    assert_eq!(
        requests_json(&w).len(),
        1,
        "the new request is readable next to the damage"
    );
    // not writable
    std::fs::write(&log, "").unwrap();
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mut s = Server::start(&w, &root_arg);
    let r = s.call("repo_request_access", json!({"path": &p}));
    if std::fs::OpenOptions::new().append(true).open(&log).is_err() {
        assert_eq!(r["isError"], true);
    }
    memory_and_static_root_still_work(&mut s, &w.project);
    let _ = s.stop();
}

#[test]
fn fifty_pending_requests_is_the_limit_and_concurrent_servers_do_not_tear_the_log() {
    let w = world();
    let p = w.project.display().to_string();
    let mut s = Server::start(&w, &["--allow-root", &p]);
    let (mut ok_n, mut err_n) = (0, 0);
    for i in 0..53 {
        let r = s.call(
            "repo_request_access",
            json!({"path": format!("/nowhere/{i}")}),
        );
        if r["isError"] == true {
            err_n += 1;
        } else {
            ok_n += 1;
        }
    }
    assert_eq!((ok_n, err_n), (50, 3));
    memory_and_static_root_still_work(&mut s, &w.project);
    let _ = s.stop();
    assert_eq!(requests_json(&w).len(), 50);
    // two servers at once, fresh log
    std::fs::remove_file(w.db.parent().unwrap().join("grant-requests.jsonl")).unwrap();
    let mut a = Server::start(&w, &[]);
    let mut b = Server::start(&w, &[]);
    let (ta, tb) = std::thread::scope(|sc| {
        let ha = sc.spawn(|| {
            (0..15)
                .filter(|i| {
                    a.call("repo_request_access", json!({"path": format!("/a/{i}")}))["isError"]
                        == false
                })
                .count()
        });
        let hb = sc.spawn(|| {
            (0..15)
                .filter(|i| {
                    b.call("repo_request_access", json!({"path": format!("/b/{i}")}))["isError"]
                        == false
                })
                .count()
        });
        (ha.join().unwrap(), hb.join().unwrap())
    });
    let _ = a.stop();
    let _ = b.stop();
    let pending = requests_json(&w);
    assert_eq!(
        pending.len(),
        ta + tb,
        "every accepted request is a whole, readable line"
    );
    assert!(pending.len() >= 25);
}
