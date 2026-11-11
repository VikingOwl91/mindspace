//! `mind setup` (M7.2): registering Mindspace with Claude Code, Codex CLI and
//! OpenCode.
//!
//! Three kinds of test:
//!  - FAKE clients (small Python programs that behave like the real `claude` /
//!    `codex` / `opencode` `mcp get|add` commands, including their quirks and a
//!    few failure modes). They make the error paths deterministic and record
//!    every call, so a test can prove that a client was NOT asked to add
//!    anything.
//!  - REAL clients, run in throw-away configuration directories (HOME,
//!    CLAUDE_CONFIG_DIR, CODEX_HOME). They check the assumptions the fakes
//!    encode. Skipped, with a message, when the client is not installed. The
//!    user's own configuration is never touched: every test sets its own HOME.
//!  - library-level tests of the guided session with scripted input.
//!
//! Nothing here touches the network.

#![cfg(unix)]

use std::io::{Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use mindspace::setup::{Client, Context, Request, Scope, Ui};

mod common;

const MIND: &str = env!("CARGO_BIN_EXE_mind");

/// One fake client program for all three names (it looks at its own name).
const FAKE: &str = r#"#!PYTHON
import json, os, sys, time
here = os.path.dirname(os.path.abspath(sys.argv[0]))
name = os.path.basename(sys.argv[0])
a = sys.argv[1:]
d = os.path.join(here, ".fake")
os.makedirs(d, exist_ok=True)
mode = open(os.path.join(d, "mode")).read().strip() if os.path.exists(os.path.join(d, "mode")) else ""
state = os.path.join(d, name + ".state.json")
with open(os.path.join(d, "calls.log"), "a") as f:
    f.write(json.dumps([name] + a) + "\n")
def load():
    return json.load(open(state)) if os.path.exists(state) else None
if mode == "hang":
    time.sleep(30)
if a[:1] == ["--version"]:
    print(name + " fake 1.0"); sys.exit(0)
if a[:1] != ["mcp"] or len(a) < 2:
    sys.exit(64)
if a[1] == "get":
    cur = load()
    if mode == "race" and cur is None:
        # another process registers the name right after the first lookup said "missing"
        counter = os.path.join(d, "gets")
        n = int(open(counter).read()) if os.path.exists(counter) else 0
        open(counter, "w").write(str(n + 1))
        if n == 0:
            json.dump({"scope": "user", "command": "/opt/other/mind", "args": ["--db", "/other.db", "mcp"]}, open(state, "w"))
    if cur is None:
        sys.stderr.write('No MCP server named "mindspace". Configured servers: \n' if name == "claude"
                         else "Error: No MCP server named 'mindspace' found.\n")
        sys.exit(1)
    if mode == "get_broken":
        sys.stderr.write("something else went wrong\n"); sys.exit(2)
    if name == "claude":
        status = "✘ Failed to connect" if mode == "bad_status" else "✔ Connected"
        scope = ("User config (available in all your projects)" if cur["scope"] == "user"
                 else "Local config (private to you in this project)")
        print("mindspace:"); print("  Scope: " + scope); print("  Status: " + status); print("  Type: stdio")
        print("  Command: " + cur["command"]); print("  Args: " + " ".join(cur["args"])); sys.exit(0)
    if name == "codex":
        if mode == "garbage_json":
            print("not json at all"); sys.exit(0)
        print(json.dumps({"name": "mindspace", "enabled": True,
                          "transport": {"type": "stdio", "command": cur["command"], "args": cur["args"], "env": cur.get("env")}}))
        sys.exit(0)
    sys.exit(64)
if a[1] == "add":
    rest = a[2:]; scope = "local"
    if name == "claude" and rest[:1] == ["-s"]:
        scope = rest[1]; rest = rest[2:]
    if name == "opencode" and rest[:1] == ["--global"]:
        rest = rest[1:]
    srv = rest[0]; assert rest[1] == "--"; cmd = rest[2:]
    cur = load()
    if cur is not None and name == "claude":
        sys.stderr.write("MCP server %s already exists in %s config\n" % (srv, scope)); sys.exit(1)
    if mode == "fail_add":
        sys.stderr.write("boom\n"); sys.exit(1)
    args = cmd[1:]
    if mode == "mismatch":
        args = args + ["--tampered"]
    json.dump({"scope": scope, "command": cmd[0], "args": args}, open(state, "w"))
    if mode == "partial":
        sys.stderr.write("crashed after writing\n"); sys.exit(1)
    print("Added"); sys.exit(0)
sys.exit(64)
"#;

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A throw-away world: HOME, fake client programs, an initialized database, folders to share.
struct World {
    _t: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    /// Holds `git` only. The world's PATH is `bin:tools`, so the REAL clients installed on
    /// this machine stay invisible to the tests that use fake ones.
    tools: PathBuf,
    db: PathBuf,
    projects: PathBuf,
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// First executable called `name` on the test process's own PATH.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| {
            p.is_file()
                && std::fs::metadata(p)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
        })
}

fn install_fake(bin: &Path, name: &str) {
    std::fs::create_dir_all(bin).unwrap();
    let p = bin.join(name);
    let python = which("python3").expect("python3");
    std::fs::write(
        &p,
        FAKE.replace("#!PYTHON", &format!("#!{}", python.display())),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl World {
    /// `clients`: which fake programs to install.
    fn with(clients: &[&str]) -> World {
        let t = tempfile::tempdir().unwrap();
        let root = canon(t.path());
        let (home, bin, tools, data, projects) = (
            root.join("home"),
            root.join("bin"),
            root.join("tools"),
            root.join("data"),
            root.join("projects"),
        );
        for d in [&home, &bin, &tools, &data, &projects] {
            std::fs::create_dir_all(d).unwrap();
        }
        if let Some(git) = which("git") {
            std::os::unix::fs::symlink(git, tools.join("git")).unwrap();
        }
        for c in clients {
            install_fake(&bin, c);
        }
        let db = data.join("mind.db");
        mindspace::initialize(&db).unwrap();
        World {
            _t: t,
            root,
            home,
            bin,
            tools,
            db,
            projects,
        }
    }
    fn new() -> World {
        World::with(&["claude", "codex", "opencode"])
    }
    fn path(&self) -> String {
        format!("{}:{}", self.bin.display(), self.tools.display())
    }
    fn mode(&self, m: &str) {
        let d = self.bin.join(".fake");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mode"), m).unwrap();
    }
    fn folder(&self, name: &str) -> PathBuf {
        let p = self.projects.join(name);
        std::fs::create_dir_all(&p).unwrap();
        canon(&p)
    }
    /// `mind setup ...` with a cleared environment.
    fn setup(&self, args: &[&str]) -> Command {
        let mut c = Command::new(MIND);
        c.env_clear()
            .env("PATH", self.path())
            .env("HOME", &self.home)
            .env("MINDSPACE_DB", &self.db)
            .current_dir(&self.root)
            .arg("setup")
            .args(args);
        c
    }
    /// Every call the fake clients received, as argument vectors (name first).
    fn calls(&self) -> Vec<Vec<String>> {
        let log = self.bin.join(".fake/calls.log");
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn adds(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.get(2).map(String::as_str) == Some("add"))
            .collect()
    }
    fn state(&self, client: &str) -> Option<serde_json::Value> {
        let p = self.bin.join(format!(".fake/{client}.state.json"));
        std::fs::read_to_string(p)
            .ok()
            .map(|s| serde_json::from_str(&s).unwrap())
    }
    fn ctx(&self) -> Context {
        Context {
            db: self.db.clone(),
            exe: PathBuf::from(MIND),
            path_env: Some(self.path().into()),
            home: Some(self.home.clone()),
            codex_home: Some(self.home.join("codex").into()),
            interactive: false,
            selftest: false,
            client_timeout: Duration::from_secs(20),
        }
    }
}

fn run(mut c: Command) -> Output {
    c.stdin(Stdio::null()).output().unwrap()
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Hash of every file and directory name below `dir` (mtimes excluded).
fn tree(dir: &Path) -> Vec<(String, String)> {
    use sha2::{Digest, Sha256};
    let mut v = Vec::new();
    fn walk(base: &Path, dir: &Path, v: &mut Vec<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                v.push((rel.clone(), "dir".into()));
                walk(base, &p, v);
            } else if md.is_file() {
                let h = Sha256::digest(std::fs::read(&p).unwrap());
                v.push((rel, h.iter().map(|b| format!("{b:02x}")).collect()));
            } else {
                v.push((rel, "other".into()));
            }
        }
    }
    walk(dir, dir, &mut v);
    v.sort();
    v
}

fn no_opt_out(w: &World) {
    for call in w.calls() {
        for a in &call {
            assert!(
                !a.contains("insecure-no-sandbox"),
                "the sandbox opt-out was passed: {call:?}"
            );
        }
    }
}

/// Splits a displayed command line the way a POSIX shell would, WITHOUT executing it.
/// The shell is only used to unquote: each argument is printed NUL-terminated.
fn shell_split(line: &str) -> Vec<String> {
    let o = Command::new("sh")
        .arg("-c")
        .arg(format!("for a in {line}; do printf '%s\\0' \"$a\"; done"))
        .output()
        .unwrap();
    String::from_utf8(o.stdout)
        .unwrap()
        .split('\0')
        .filter(|s| !s.is_empty() || false)
        .map(String::from)
        .collect()
}

macro_rules! need_python {
    () => {
        if !python3_available() {
            eprintln!("skipped: python3 is not available (the fake clients need it)");
            return;
        }
    };
}

// ---------------------------------------------------------------- detection

#[test]
fn clients_are_detected_absent_present_and_ambiguous_without_running_them() {
    need_python!();
    // present: all three
    let w = World::new();
    let o = run(w.setup(&[]));
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    for c in ["claude-code", "codex", "opencode"] {
        assert!(
            text.contains(c)
                && !text
                    .lines()
                    .any(|l| l.contains(c) && l.contains("not found")),
            "{text}"
        );
    }
    assert!(
        w.calls().is_empty(),
        "detection must not run the clients: {:?}",
        w.calls()
    );
    assert!(w.adds().is_empty());

    // absent: only codex installed
    let w2 = World::with(&["codex"]);
    let text = out(&run(w2.setup(&[])));
    assert!(
        text.lines()
            .any(|l| l.contains("claude-code") && l.contains("not found")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("opencode") && l.contains("not found")),
        "{text}"
    );

    // ambiguous: two different `claude` programs on PATH
    let w3 = World::with(&["claude"]);
    let other = w3.root.join("bin2");
    install_fake(&other, "claude");
    let mut c = w3.setup(&[]);
    c.env(
        "PATH",
        format!(
            "{}:{}:{}",
            w3.bin.display(),
            other.display(),
            w3.tools.display()
        ),
    );
    let text = out(&run(c));
    assert!(text.contains("AMBIGUOUS"), "{text}");
    // ... a preview warns, an apply refuses and changes nothing.
    let mut c = w3.setup(&["claude-code", "--apply", "--yes"]);
    c.env(
        "PATH",
        format!(
            "{}:{}:{}",
            w3.bin.display(),
            other.display(),
            w3.tools.display()
        ),
    );
    let o = run(c);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("several different `claude`"),
        "{}",
        err(&o)
    );
    assert!(w3.adds().is_empty() && w3.state("claude").is_none());
}

#[test]
fn a_symlink_to_the_same_program_is_not_ambiguous() {
    need_python!();
    let w = World::with(&["claude"]);
    let alias = w.root.join("alias");
    std::fs::create_dir(&alias).unwrap();
    std::os::unix::fs::symlink(w.bin.join("claude"), alias.join("claude")).unwrap();
    let mut c = w.setup(&[]);
    c.env(
        "PATH",
        format!(
            "{}:{}:{}",
            w.bin.display(),
            alias.display(),
            w.tools.display()
        ),
    );
    assert!(!out(&run(c)).contains("AMBIGUOUS"));
}

// ------------------------------------------------------------------ preview

#[test]
fn a_preview_changes_nothing_and_never_runs_the_client() {
    need_python!();
    let w = World::new();
    let repos = w.folder("repos");
    let before = (tree(&w.root), std::fs::metadata(&w.db).unwrap().len());
    for client in ["claude-code", "codex", "opencode"] {
        let o = run(w.setup(&[client, "--ro", repos.to_str().unwrap()]));
        assert!(o.status.success(), "{client}: {}", err(&o));
        let text = out(&o);
        assert!(
            text.contains("This is a preview. Nothing is changed."),
            "{text}"
        );
        assert!(
            text.contains(&format!("READ-ONLY   {}", repos.display())),
            "{text}"
        );
        assert!(text.contains("To undo:"), "{text}");
    }
    assert!(
        w.calls().is_empty(),
        "a preview must not run any client: {:?}",
        w.calls()
    );
    assert_eq!(
        tree(&w.root),
        before.0,
        "the file system changed during a preview"
    );
}

#[test]
fn the_preview_shows_the_exact_command_and_it_survives_shell_quoting() {
    need_python!();
    let w = World::new();
    let repos = w.folder("repos");
    let o = run(w.setup(&[
        "claude-code",
        "--ro",
        repos.to_str().unwrap(),
        "--scope",
        "local",
    ]));
    let text = out(&o);
    let line = text
        .lines()
        .skip_while(|l| !l.starts_with("The registration is this command"))
        .nth(1)
        .unwrap()
        .trim();
    let argv = shell_split(line);
    assert!(argv[0].ends_with("claude"));
    assert_eq!(&argv[1..6], ["mcp", "add", "-s", "local", "mindspace"]);
    assert_eq!(argv[6], "--");
    assert_eq!(argv[7], MIND);
    assert_eq!(
        &argv[8..],
        [
            "--db",
            w.db.to_str().unwrap(),
            "mcp",
            "--allow-root",
            &format!("{}:ro", repos.display())
        ]
    );
}

#[test]
fn nothing_is_shared_by_default_and_the_text_says_so() {
    need_python!();
    let w = World::new();
    let o = run(w.setup(&["codex"]));
    let text = out(&o);
    assert!(text.contains("(none)  zero-permission setup"), "{text}");
    assert!(text.contains("By default NO folder is shared"), "{text}");
    assert!(text.contains("not a confidentiality guarantee"), "{text}");
    assert!(text.contains("do not limit what your agent"), "{text}");
    assert!(text.contains("fail-closed"), "{text}");
}

// ------------------------------------------------------------ registration

#[test]
fn claude_code_is_registered_verified_and_the_ro_mode_stays_ro() {
    need_python!();
    let w = World::new();
    let repos = w.folder("repos");
    let o = run(w.setup(&[
        "claude-code",
        "--ro",
        repos.to_str().unwrap(),
        "--apply",
        "--yes",
    ]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Registered and verified"), "{}", out(&o));
    let s = w.state("claude").unwrap();
    assert_eq!(s["scope"], "user");
    assert_eq!(s["command"], MIND);
    assert_eq!(
        s["args"],
        serde_json::json!([
            "--db",
            w.db.to_str().unwrap(),
            "mcp",
            "--allow-root",
            format!("{}:ro", repos.display())
        ])
    );
    // The calls: get (existence), add, get (verification). Nothing else, nothing for other clients.
    let kinds: Vec<String> = w
        .calls()
        .iter()
        .map(|c| format!("{} {}", c[0], c[2]))
        .collect();
    assert_eq!(kinds, ["claude get", "claude add", "claude get"]);
    no_opt_out(&w);
}

#[test]
fn codex_is_registered_and_read_back() {
    need_python!();
    let w = World::new();
    let repos = w.folder("repos");
    let o = run(w.setup(&["codex", "--ro", repos.to_str().unwrap(), "--apply", "--yes"]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Registered and read back"), "{}", out(&o));
    assert_eq!(
        w.state("codex").unwrap()["args"][4],
        format!("{}:ro", repos.display())
    );
    no_opt_out(&w);
}

#[test]
fn opencode_is_preview_only_and_an_apply_changes_nothing() {
    need_python!();
    let w = World::new();
    let o = run(w.setup(&["opencode", "--apply", "--yes"]));
    assert!(!o.status.success());
    assert!(
        err(&o).contains("cannot be applied by setup"),
        "{}",
        err(&o)
    );
    let text = out(&o);
    assert!(
        text.contains("opencode mcp add --global mindspace --"),
        "{text}"
    );
    assert!(
        err(&o).contains("preview only"),
        "the refusal must say why: {}",
        err(&o)
    );
    assert!(
        w.calls().is_empty(),
        "OpenCode must never be run by setup: {:?}",
        w.calls()
    );
    // the plain preview explains why
    let text = out(&run(w.setup(&["opencode"])));
    assert!(
        text.contains("overwrites an existing entry without asking"),
        "{text}"
    );
}

#[test]
fn no_roots_means_no_allow_root_arguments_at_all() {
    need_python!();
    let w = World::new();
    let o = run(w.setup(&["claude-code", "--apply", "--yes"]));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        w.state("claude").unwrap()["args"],
        serde_json::json!(["--db", w.db.to_str().unwrap(), "mcp"])
    );
}

#[test]
fn read_write_is_only_registered_when_asked_for_and_never_upgrades_a_ro_folder() {
    need_python!();
    let w = World::new();
    let (a, b) = (w.folder("a_ro"), w.folder("b_rw"));
    let o = run(w.setup(&[
        "codex",
        "--ro",
        a.to_str().unwrap(),
        "--rw",
        b.to_str().unwrap(),
        "--apply",
        "--yes",
    ]));
    assert!(o.status.success(), "{}", err(&o));
    let args = w.state("codex").unwrap()["args"].clone();
    assert_eq!(
        args,
        serde_json::json!([
            "--db",
            w.db.to_str().unwrap(),
            "mcp",
            "--allow-root",
            format!("{}:ro", a.display()),
            "--allow-root",
            format!("{}:rw", b.display())
        ])
    );
    assert!(
        out(&o).contains(&format!("READ-WRITE  {}", b.display())),
        "{}",
        out(&o)
    );
    // the same folder as ro and rw is ambiguous: refused, not resolved by guessing
    let w2 = World::new();
    let f = w2.folder("same");
    let o = run(w2.setup(&[
        "codex",
        "--ro",
        f.to_str().unwrap(),
        "--rw",
        f.to_str().unwrap(),
    ]));
    assert!(!o.status.success());
    assert!(err(&o).contains("both --ro and --rw"), "{}", err(&o));
}

#[test]
fn special_characters_and_spaces_in_paths_reach_the_client_unchanged_and_nothing_is_executed() {
    need_python!();
    let w = World::new();
    let names = [
        "sp ace",
        "it's",
        "q\"uote",
        "$HOME",
        "`touch PWNED`",
        "a;b&c|d",
        "$(touch PWNED)",
        "ünï ✓",
        "-dash",
        "star*",
    ];
    let mut spec = Vec::new();
    let mut want = vec![
        "--db".to_string(),
        w.db.display().to_string(),
        "mcp".to_string(),
    ];
    for n in names {
        let p = w.folder(n);
        spec.push(p.display().to_string());
        want.push("--allow-root".into());
        want.push(format!("{}:ro", p.display()));
    }
    let mut args: Vec<&str> = vec!["claude-code", "--apply", "--yes"];
    for s in &spec {
        args.push("--ro");
        args.push(s);
    }
    let o = run(w.setup(&args));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        w.state("claude").unwrap()["args"],
        serde_json::json!(want),
        "the client received different arguments"
    );
    // The displayed command, split like a shell would, gives the same vector.
    let text = out(&o);
    let line = text
        .lines()
        .skip_while(|l| !l.starts_with("The registration is this command"))
        .nth(1)
        .unwrap()
        .trim();
    let argv = shell_split(line);
    assert_eq!(
        &argv[8..],
        &want[..],
        "the displayed command is not a faithful, safe quoting"
    );
    for d in [&w.root, &w.projects, &w.home] {
        assert!(
            !d.join("PWNED").exists(),
            "a path was executed as shell code"
        );
    }
}

// ------------------------------------------------- refusals (nothing changes)

#[test]
fn bad_roots_are_refused_in_preview_and_apply_and_nothing_changes() {
    need_python!();
    let w = World::new();
    let file = w.projects.join("afile");
    std::fs::write(&file, "x").unwrap();
    let inside_db = w.db.parent().unwrap().to_path_buf();
    let colon = w.projects.join("a:b");
    std::fs::create_dir(&colon).unwrap();
    let before = tree(&w.root); // after the fixtures: only setup itself may change anything now
    let cases: Vec<(&str, String, &str)> = vec![
        ("--ro", "relative/dir".into(), "absolute path"),
        ("--ro", "/".into(), "root of the file system"),
        ("--rw", "/etc".into(), "system folder"),
        ("--ro", w.home.display().to_string(), "home directory"),
        ("--ro", w.root.display().to_string(), "home directory"), // contains HOME
        (
            "--ro",
            w.projects.join("missing").display().to_string(),
            "does not exist",
        ),
        ("--ro", file.display().to_string(), "not a directory"),
        (
            "--ro",
            inside_db.display().to_string(),
            "database directory",
        ),
        ("--ro", colon.display().to_string(), "contains ':'"),
        ("--rw", "~/projects".into(), "absolute path"),
    ];
    for (flag, path, why) in cases {
        for apply in [false, true] {
            let mut args = vec!["claude-code", flag, path.as_str()];
            if apply {
                args.extend(["--apply", "--yes"]);
            }
            let o = run(w.setup(&args));
            assert!(
                !o.status.success(),
                "{flag} {path} apply={apply} was accepted: {}",
                out(&o)
            );
            assert!(err(&o).contains(why), "{flag} {path}: {}", err(&o));
        }
    }
    assert!(w.calls().is_empty(), "{:?}", w.calls());
    assert_eq!(tree(&w.root), before);
}

#[test]
fn symlinked_roots_are_registered_by_their_resolved_path() {
    need_python!();
    let w = World::new();
    let real = w.folder("real");
    let link = w.root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let o = run(w.setup(&["codex", "--ro", link.to_str().unwrap(), "--apply", "--yes"]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("resolves to"), "{}", out(&o));
    assert_eq!(
        w.state("codex").unwrap()["args"][4],
        format!("{}:ro", real.display())
    );
    // A link that leads into the database directory is caught by its target.
    let sneaky = w.root.join("sneaky");
    std::os::unix::fs::symlink(w.db.parent().unwrap(), &sneaky).unwrap();
    let o = run(w.setup(&["codex", "--ro", sneaky.to_str().unwrap()]));
    assert!(!o.status.success());
    assert!(err(&o).contains("database directory"), "{}", err(&o));
}

#[test]
fn a_missing_or_foreign_database_is_refused() {
    need_python!();
    let w = World::new();
    let o = run({
        let mut c = w.setup(&["claude-code"]);
        c.env("MINDSPACE_DB", w.root.join("nodb/mind.db"));
        c
    });
    assert!(!o.status.success());
    assert!(err(&o).contains("run: mind init"), "{}", err(&o));
    assert!(
        !w.root.join("nodb").exists(),
        "setup must never create the database"
    );
    let other = w.root.join("other.db");
    rusqlite::Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t (x)")
        .unwrap();
    let o = run({
        let mut c = w.setup(&["claude-code"]);
        c.env("MINDSPACE_DB", &other);
        c
    });
    assert!(!o.status.success());
    assert!(
        err(&o).contains("not an initialized Mindspace database"),
        "{}",
        err(&o)
    );
}

// ---------------------------------------------------- existing entries

#[test]
fn an_existing_mindspace_entry_is_never_overwritten() {
    need_python!();
    for client in ["claude-code", "codex"] {
        let w = World::new();
        let program = if client == "claude-code" {
            "claude"
        } else {
            "codex"
        };
        // an entry somebody else registered (and which the fake keeps in its state)
        std::fs::create_dir_all(w.bin.join(".fake")).unwrap();
        let theirs = serde_json::json!({"scope": "user", "command": "/opt/their/mind", "args": ["--db", "/their.db", "mcp"]});
        std::fs::write(
            w.bin.join(format!(".fake/{program}.state.json")),
            theirs.to_string(),
        )
        .unwrap();
        let o = run(w.setup(&[client, "--apply", "--yes"]));
        assert!(!o.status.success(), "{client}");
        let e = err(&o);
        assert!(
            e.contains("already has a `mindspace` entry") && e.contains("remove"),
            "{client}: {e}"
        );
        assert_eq!(
            w.state(program).unwrap(),
            theirs,
            "{client}: the existing entry was changed"
        );
        assert!(
            w.adds().is_empty(),
            "{client}: `add` must not even be attempted: {:?}",
            w.adds()
        );
    }
}

#[test]
fn an_existing_entry_is_reported_in_the_apply_preview_too() {
    need_python!();
    let w = World::new();
    std::fs::create_dir_all(w.bin.join(".fake")).unwrap();
    std::fs::write(
        w.bin.join(".fake/codex.state.json"),
        r#"{"scope":"user","command":"/x","args":[]}"#,
    )
    .unwrap();
    let o = run(w.setup(&["codex", "--apply"])); // no --yes, not a terminal
    let text = out(&o);
    assert!(text.contains("ALREADY registered"), "{text}");
    assert!(!o.status.success());
}

#[test]
fn an_unreadable_existence_check_is_an_error_not_a_no() {
    need_python!();
    let w = World::new();
    // `mcp get` fails for a reason other than "no such server": setup must not assume "absent".
    std::fs::create_dir_all(w.bin.join(".fake")).unwrap();
    std::fs::write(
        w.bin.join(".fake/claude.state.json"),
        r#"{"scope":"user","command":"/x","args":[]}"#,
    )
    .unwrap();
    w.mode("get_broken");
    let o = run(w.setup(&["claude-code", "--apply", "--yes"]));
    assert!(!o.status.success());
    assert!(err(&o).contains("could not tell whether"), "{}", err(&o));
    assert!(w.adds().is_empty());
}

// ------------------------------------------------------ non-interactive

#[test]
fn without_a_terminal_nothing_is_changed_unless_yes_is_given() {
    need_python!();
    let w = World::new();
    let o = run(w.setup(&["claude-code", "--apply"]));
    assert!(!o.status.success());
    assert!(err(&o).contains("not a terminal"), "{}", err(&o));
    assert!(err(&o).contains("--yes"));
    assert!(w.adds().is_empty() && w.state("claude").is_none());
    // `mind setup` alone only lists; flags without a client are refused
    let o = run(w.setup(&["--apply"]));
    assert!(!o.status.success());
    assert!(err(&o).contains("name the client"), "{}", err(&o));
    let o = run(w.setup(&["--ro", "/tmp"]));
    assert!(!o.status.success());
    assert!(w.adds().is_empty());
}

// ------------------------------------------------- failures and partial states

#[test]
fn a_failed_add_is_reported_and_leaves_no_entry() {
    need_python!();
    let w = World::new();
    w.mode("fail_add");
    let o = run(w.setup(&["claude-code", "--apply", "--yes"]));
    assert!(!o.status.success());
    let e = err(&o);
    assert!(
        e.contains("`claude mcp add` failed") && e.contains("boom"),
        "{e}"
    );
    assert!(e.contains("No `mindspace` entry exists now"), "{e}");
    assert!(
        !out(&o).contains("Registered"),
        "success must not be reported"
    );
    assert!(w.state("claude").is_none());
}

#[test]
fn a_partial_state_after_a_failed_add_is_found_and_the_undo_command_is_given() {
    need_python!();
    let w = World::new();
    w.mode("partial");
    let o = run(w.setup(&["claude-code", "--scope", "local", "--apply", "--yes"]));
    assert!(!o.status.success());
    let e = err(&o);
    assert!(
        e.contains("EXISTS now") && e.contains("claude mcp remove mindspace -s local"),
        "{e}"
    );
    assert!(!out(&o).contains("Registered"));
}

#[test]
fn a_registration_the_client_cannot_start_is_not_reported_as_success() {
    need_python!();
    let w = World::new();
    w.mode("bad_status");
    let o = run(w.setup(&["claude-code", "--apply", "--yes"]));
    assert!(!o.status.success());
    let e = err(&o);
    assert!(
        e.contains("could NOT start the server")
            && e.contains("mind doctor")
            && e.contains("To undo: claude mcp remove mindspace -s user"),
        "{e}"
    );
    assert!(!out(&o).contains("Registered and"), "{}", out(&o));
}

#[test]
fn a_registration_that_reads_back_differently_is_not_verified() {
    need_python!();
    let w = World::new();
    w.mode("mismatch");
    for client in ["claude-code", "codex"] {
        let o = run(w.setup(&[client, "--apply", "--yes"]));
        assert!(!o.status.success(), "{client}");
        let e = err(&o);
        assert!(
            e.contains("NOT verified") && e.contains("to undo: "),
            "{client}: {e}"
        );
    }
}

#[test]
fn an_unreadable_answer_after_adding_is_not_verified() {
    need_python!();
    let w = World::new();
    w.mode("garbage_json");
    let o = run(w.setup(&["codex", "--apply", "--yes"]));
    assert!(!o.status.success());
    assert!(
        err(&o).contains("could not be read back") && err(&o).contains("NOT verified"),
        "{}",
        err(&o)
    );
}

#[test]
fn a_client_that_hangs_is_stopped_and_nothing_is_registered() {
    need_python!();
    let w = World::new();
    w.mode("hang");
    let mut ctx = w.ctx();
    ctx.client_timeout = Duration::from_secs(1);
    let req = Request {
        client: Client::ClaudeCode,
        scope: Scope::User,
        ro: vec![],
        rw: vec![],
        apply: true,
        yes: true,
    };
    let (mut input, mut buf) = (Cursor::new(Vec::<u8>::new()), Vec::new());
    let started = std::time::Instant::now();
    let r = mindspace::setup::run(
        &req,
        &ctx,
        &mut Ui {
            input: &mut input,
            out: &mut buf,
        },
    );
    let e = r.unwrap_err().to_string();
    assert!(
        e.contains("did not finish in time") && e.contains("nothing was changed"),
        "{e}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(w.state("claude").is_none());
}

// ------------------------------------------------------------------ sandbox

#[cfg(target_os = "linux")]
mod sandbox {
    use super::*;

    #[test]
    fn with_an_effective_sandbox_the_preview_says_so() {
        need_python!();
        let w = World::new();
        let repos = w.folder("repos");
        let text = out(&run(w.setup(&["codex", "--ro", repos.to_str().unwrap()])));
        assert!(
            text.contains("the kernel sandbox is enforced on this machine"),
            "{text}"
        );
    }

    #[test]
    fn without_a_sandbox_the_tools_stay_off_and_the_text_says_so_but_the_memory_tools_can_still_be_registered(
    ) {
        need_python!();
        for errno in [38, 95] {
            let w = World::new();
            let repos = w.folder("repos");
            let mut c = w.setup(&["codex", "--ro", repos.to_str().unwrap()]);
            common::block_landlock(&mut c, errno);
            let text = out(&run(c));
            assert!(
                text.contains("NOT available here") && text.contains("stay switched off"),
                "errno {errno}: {text}"
            );

            let mut c = w.setup(&["codex", "--ro", repos.to_str().unwrap(), "--apply", "--yes"]);
            common::block_landlock(&mut c, errno);
            let o = run(c);
            assert!(o.status.success(), "{}", err(&o));
            assert!(out(&o).contains("NOT available here"), "{}", out(&o));
            // Registering is safe (the server fails closed); the opt-out is never added.
            no_opt_out(&w);
            assert!(!w.state("codex").unwrap()["args"]
                .to_string()
                .contains("insecure"));
        }
    }
}

// ----------------------------------------------------------- codex backup

#[test]
fn the_codex_config_is_backed_up_before_codex_rewrites_it() {
    need_python!();
    let w = World::new();
    let cfg_dir = w.home.join("codex");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let cfg = cfg_dir.join("config.toml");
    let original =
        "# my notes\nmodel = \"x\"\n\n[mcp_servers.other]\ncommand = \"npx\"   # keep me\n";
    std::fs::write(&cfg, original).unwrap();
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    let o = run({
        let mut c = w.setup(&["codex", "--apply", "--yes"]);
        c.env("CODEX_HOME", &cfg_dir);
        c
    });
    assert!(o.status.success(), "{}", err(&o));
    let backups: Vec<PathBuf> = std::fs::read_dir(&cfg_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("mindspace-backup")
        })
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        std::fs::read_to_string(&backups[0]).unwrap(),
        original,
        "the backup must be the untouched original"
    );
    assert_eq!(
        std::fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777,
        0o600,
        "the backup keeps the permissions"
    );
    assert!(
        out(&o).contains("Backup of your Codex configuration"),
        "{}",
        out(&o)
    );
}

#[test]
fn a_symlinked_codex_config_is_backed_up_by_its_content() {
    need_python!();
    let w = World::new();
    let cfg_dir = w.home.join("codex");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let managed = w.root.join("dotfiles-config.toml");
    std::fs::write(&managed, "model = \"x\"\n").unwrap();
    std::os::unix::fs::symlink(&managed, cfg_dir.join("config.toml")).unwrap();
    let o = run({
        let mut c = w.setup(&["codex", "--apply", "--yes"]);
        c.env("CODEX_HOME", &cfg_dir);
        c
    });
    assert!(o.status.success(), "{}", err(&o));
    let backup = std::fs::read_dir(&cfg_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains("mindspace-backup"))
        .expect("a backup");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), "model = \"x\"\n");
}

#[test]
fn if_the_backup_cannot_be_made_nothing_is_registered() {
    need_python!();
    let w = World::new();
    let cfg_dir = w.home.join("codex");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    // config.toml is a directory: not a regular file, so it cannot be copied safely.
    std::fs::create_dir(cfg_dir.join("config.toml")).unwrap();
    let o = run({
        let mut c = w.setup(&["codex", "--apply", "--yes"]);
        c.env("CODEX_HOME", &cfg_dir);
        c
    });
    assert!(!o.status.success());
    assert!(
        err(&o).contains("not a regular file") && err(&o).contains("nothing was changed"),
        "{}",
        err(&o)
    );
    assert!(w.adds().is_empty());
}

// --------------------------------------------------------- guided session

fn session(w: &World, answers: &str, interactive: bool) -> (Result<(), mindspace::Error>, String) {
    let mut ctx = w.ctx();
    ctx.interactive = interactive;
    let mut input = Cursor::new(answers.as_bytes().to_vec());
    let mut buf = Vec::new();
    let r = mindspace::setup::run_wizard(
        &ctx,
        &mut Ui {
            input: &mut input,
            out: &mut buf,
        },
    );
    (r, String::from_utf8_lossy(&buf).into_owned())
}

#[test]
fn the_guided_session_explains_then_registers_after_the_confirmations() {
    need_python!();
    let w = World::with(&["claude"]);
    let repos = w.folder("repos");
    // set up Claude Code: yes | folder | (end ro) | rw: no | scope: default | register: y
    let answers = format!("y\ny\n{}\n\nn\n\ny\n", repos.display());
    let (r, text) = session(&w, &answers, true);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    for needle in [
        "MCP (Model Context Protocol)",
        "one local database file",
        "read-only (--ro)",
        "are only examples",
        "accepted:",
        "Registered and verified",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
    assert_eq!(
        w.state("claude").unwrap()["args"][4],
        format!("{}:ro", repos.display())
    );
    assert_eq!(
        w.state("claude").unwrap()["scope"],
        "user",
        "the recommended scope is the default"
    );
}

#[test]
fn declining_the_final_question_changes_nothing() {
    need_python!();
    let w = World::with(&["claude"]);
    let (r, text) = session(&w, "y\nn\n\nn\n", true);
    assert!(
        r.unwrap_err()
            .to_string()
            .contains("cancelled; nothing was changed"),
        "{text}"
    );
    assert!(w.adds().is_empty() && w.state("claude").is_none());
}

#[test]
fn read_write_needs_the_word_yes_and_a_second_confirmation() {
    need_python!();
    let w = World::with(&["claude"]);
    let rw = w.folder("rw");
    // rw folder, then a half-hearted answer to the explicit question
    let (r, text) = session(&w, &format!("y\ny\n\ny\n{}\n\n\ny\n", rw.display()), true);
    assert!(r.is_err(), "{text}");
    assert!(
        text.contains("READ-WRITE access") && text.contains("Type 'yes'"),
        "{text}"
    );
    assert!(w.adds().is_empty(), "'y' is not enough for write access");
    // the full word, then the usual confirmation
    let w2 = World::with(&["claude"]);
    let rw2 = w2.folder("rw");
    let (r, text) = session(
        &w2,
        &format!("y\ny\n\ny\n{}\n\n\nyes\ny\n", rw2.display()),
        true,
    );
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(
        w2.state("claude").unwrap()["args"][4],
        format!("{}:rw", rw2.display())
    );
}

#[test]
fn a_bad_folder_is_explained_at_once_and_asked_again() {
    need_python!();
    let w = World::with(&["claude"]);
    let repos = w.folder("repos");
    let answers = format!("y\ny\nrelative/dir\n/\n{}\n\nn\n\ny\n", repos.display());
    let (r, text) = session(&w, &answers, true);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(text.matches("not accepted:").count() == 2, "{text}");
    assert_eq!(
        w.state("claude").unwrap()["args"].as_array().unwrap().len(),
        5
    );
}

#[test]
fn closing_the_input_cancels_without_changes() {
    need_python!();
    let w = World::with(&["claude"]);
    let (r, _) = session(&w, "y\n", true);
    assert!(r.unwrap_err().to_string().contains("cancelled"));
    assert!(w.adds().is_empty());
}

#[test]
fn with_two_clients_the_session_asks_which_and_a_wrong_answer_cancels() {
    need_python!();
    let w = World::with(&["claude", "codex"]);
    let (r, text) = session(&w, "7\n", true);
    assert!(r.is_err(), "{text}");
    assert!(
        text.contains("1) Claude Code") && text.contains("2) Codex CLI"),
        "{text}"
    );
    assert!(w.adds().is_empty());
}

#[test]
fn no_client_on_path_ends_the_session_politely() {
    need_python!();
    let w = World::with(&[]);
    let (r, text) = session(&w, "", true);
    r.unwrap();
    assert!(text.contains("No supported client was found"), "{text}");
}

#[test]
fn the_guided_session_leads_with_zero_permission_and_treats_folders_as_optional() {
    need_python!();
    let w = World::with(&["claude"]);
    // Set up: yes | share folders: just Enter (the default is no) | scope: default | register: y
    let (r, text) = session(&w, "y\n\n\ny\n", true);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    let zero = text
        .find("Zero-permission setup (the default)")
        .expect("explained up front");
    let optional = text
        .find("Optional: let Mindspace read repository folders")
        .expect("folders are called optional");
    assert!(
        zero < optional,
        "the zero-permission statement comes before the optional step"
    );
    assert!(
        text.contains("Setup without any folder is complete"),
        "{text}"
    );
    assert!(
        !text.contains("Folder to share"),
        "no folder was asked for: {text}"
    );
    assert!(
        !text[..optional].contains("read-only (--ro)"),
        "the permission modes are explained at the optional step, not before it: {text}"
    );
    assert!(
        text.contains("zero-permission setup") && text.contains("No folder is shared"),
        "{text}"
    );
    assert_eq!(
        w.state("claude").unwrap()["args"],
        serde_json::json!(["--db", w.db.to_str().unwrap(), "mcp"])
    );
}

#[test]
fn a_typed_tilde_is_expanded_in_the_guided_session_only_for_the_home_directory() {
    need_python!();
    let w = World::with(&["claude"]);
    let home_repo = w.home.join("repos");
    std::fs::create_dir_all(&home_repo).unwrap();
    let (r, text) = session(&w, "y\ny\n~/repos\n\nn\n\ny\n", true);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(
        w.state("claude").unwrap()["args"][4],
        format!("{}:ro", canon(&home_repo).display())
    );
}

#[test]
fn a_zero_permission_registration_is_a_complete_working_server() {
    need_python!();
    let w = World::new();
    let o = run(w.setup(&["claude-code", "--apply", "--yes"]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("No folder is shared (zero-permission setup)"),
        "{}",
        out(&o)
    );
    // Start exactly what was registered and talk to it.
    let st = w.state("claude").unwrap();
    let args: Vec<String> = st["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert!(!args.iter().any(|a| a == "--allow-root"));
    let mut child = Command::new(st["command"].as_str().unwrap())
        .args(&args)
        .env_clear()
        .env("PATH", w.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut rpc = |id: u64, method: &str, params: serde_json::Value| -> serde_json::Value {
        use std::io::BufRead;
        if id == 0 {
            // a notification: no answer comes back
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":method})
            )
            .unwrap();
            return serde_json::Value::Null;
        }

        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    };
    rpc(
        1,
        "initialize",
        serde_json::json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}),
    );
    rpc(0, "notifications/initialized", serde_json::json!({}));
    let tools = rpc(2, "tools/list", serde_json::json!({}));
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for t in [
        "memory_create",
        "memory_search",
        "space_create",
        "space_list",
        "code_search",
    ] {
        assert!(names.contains(&t.to_string()), "{t} missing from {names:?}");
    }
    let made = rpc(
        3,
        "tools/call",
        serde_json::json!({"name":"space_create","arguments":{"name":"zero"}}),
    );
    assert_eq!(made["result"]["isError"], false, "{made}");
    let note = rpc(
        4,
        "tools/call",
        serde_json::json!({"name":"memory_create","arguments":{"content":"zero-permission note","spaces":["zero"]}}),
    );
    assert_eq!(note["result"]["isError"], false, "{note}");
    // ... and the repository tools are off, with a reason, because no folder was shared.
    let outline = rpc(
        5,
        "tools/call",
        serde_json::json!({"name":"repo_outline","arguments":{"path": w.projects.display().to_string()}}),
    );
    assert_eq!(outline["result"]["isError"], true);
    let _ = child.kill();
    let _ = child.wait();
}

// ----------------------------------------------- a real terminal (pty)

#[cfg(target_os = "linux")]
#[test]
fn in_a_real_terminal_mind_setup_runs_the_guided_session() {
    use std::os::fd::FromRawFd;
    need_python!();
    let w = World::with(&["claude"]);
    let (mut master, mut slave) = (0, 0);
    // SAFETY: plain libc calls with valid out-pointers.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0);
    let (master, slave) = unsafe {
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    let mut cmd = w.setup(&[]);
    cmd.stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    drop(slave);
    let mut writer = master.try_clone().unwrap();
    let mut reader = master;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        let _ = tx.send(all);
    });
    // Set up Claude Code | share folders: no | default scope | register
    for line in ["y", "n", "", "y"] {
        std::thread::sleep(Duration::from_millis(150));
        writeln!(writer, "{line}").unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "mind setup did not finish"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let text =
        String::from_utf8_lossy(&rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default())
            .into_owned();
    assert!(status.success(), "{text}");
    assert!(
        text.contains("MCP (Model Context Protocol)") && text.contains("Registered and verified"),
        "{text}"
    );
    assert!(w.state("claude").is_some());
}

// ---------------------------------------------- review findings (M7.2a)

fn backups_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("mindspace-backup")
        })
        .collect();
    v.sort();
    v
}

#[test]
fn a_planted_symlink_or_file_at_the_backup_name_is_never_followed_or_overwritten() {
    need_python!();
    let w = World::new();
    let cfg_dir = w.home.join("codex");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let cfg = cfg_dir.join("config.toml");
    std::fs::write(&cfg, "model = \"x\"\n").unwrap();
    let victim = w.root.join("victim");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Every second the backup name could be derived from, three kinds of obstacles.
    let mut planted = Vec::new();
    for (i, stamp) in (now - 2..now + 6).enumerate() {
        let name = cfg_dir.join(format!("config.toml.mindspace-backup-{stamp}"));
        match i % 3 {
            0 => std::os::unix::fs::symlink(&victim, &name).unwrap(), // dangling link
            1 => std::fs::write(&name, "PRECIOUS").unwrap(),          // an older backup
            _ => {
                std::fs::write(w.root.join(format!("target{i}")), "TARGET").unwrap();
                std::os::unix::fs::symlink(w.root.join(format!("target{i}")), &name).unwrap();
                // link to a file
            }
        }
        planted.push(name);
    }
    let o = run({
        let mut c = w.setup(&["codex", "--apply", "--yes"]);
        c.env("CODEX_HOME", &cfg_dir);
        c
    });
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        !victim.exists(),
        "the backup was written through a planted symlink"
    );
    for (i, name) in planted.iter().enumerate() {
        match i % 3 {
            0 => assert!(std::fs::symlink_metadata(name)
                .unwrap()
                .file_type()
                .is_symlink()),
            1 => assert_eq!(
                std::fs::read_to_string(name).unwrap(),
                "PRECIOUS",
                "an existing backup was overwritten"
            ),
            _ => assert_eq!(
                std::fs::read_to_string(w.root.join(format!("target{i}"))).unwrap(),
                "TARGET"
            ),
        }
    }
    let new: Vec<_> = backups_in(&cfg_dir)
        .into_iter()
        .filter(|p| !planted.contains(p))
        .collect();
    assert_eq!(new.len(), 1, "exactly one new backup expected: {new:?}");
    assert_eq!(std::fs::read_to_string(&new[0]).unwrap(), "model = \"x\"\n");
}

#[test]
fn two_backups_in_the_same_second_get_different_names() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("config.toml");
    std::fs::write(&cfg, "a = 1\n").unwrap();
    let a = mindspace::setup::client::backup_codex_config(&cfg)
        .unwrap()
        .unwrap();
    let b = mindspace::setup::client::backup_codex_config(&cfg)
        .unwrap()
        .unwrap();
    assert_ne!(a, b);
    for p in [a, b] {
        assert_eq!(std::fs::read_to_string(p).unwrap(), "a = 1\n");
    }
}

#[test]
fn backups_keep_the_permissions_of_the_config_and_are_never_wider_while_being_written() {
    for mode in [0o600u32, 0o640, 0o644] {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("config.toml");
        std::fs::write(&cfg, "secret = \"x\"\n").unwrap();
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(mode)).unwrap();
        let b = mindspace::setup::client::backup_codex_config(&cfg)
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::metadata(&b).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}

#[test]
fn a_codex_add_that_fails_leaves_the_config_unchanged_and_the_backup_identical() {
    need_python!();
    for mode in ["fail_add", "partial"] {
        let w = World::new();
        let cfg_dir = w.home.join("codex");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let original = "# notes\nmodel = \"x\"\n[mcp_servers.other]\ncommand = \"npx\"  # keep\n";
        std::fs::write(cfg_dir.join("config.toml"), original).unwrap();
        w.mode(mode);
        let o = run({
            let mut c = w.setup(&["codex", "--apply", "--yes"]);
            c.env("CODEX_HOME", &cfg_dir);
            c
        });
        assert!(!o.status.success(), "{mode}");
        assert_eq!(
            std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap(),
            original,
            "{mode}: setup must not touch the config itself"
        );
        let b = backups_in(&cfg_dir);
        assert_eq!(b.len(), 1, "{mode}");
        assert_eq!(std::fs::read_to_string(&b[0]).unwrap(), original, "{mode}");
        assert!(
            err(&o).contains(&b[0].display().to_string()),
            "{mode}: the error must name the backup: {}",
            err(&o)
        );
    }
}

#[test]
fn an_entry_that_appears_between_the_check_and_the_add_is_not_overwritten() {
    need_python!();
    let w = World::new();
    w.mode("race");
    let o = run(w.setup(&["codex", "--apply", "--yes"]));
    assert!(!o.status.success(), "{}", out(&o));
    assert!(
        err(&o).contains("appeared") || err(&o).contains("already has"),
        "{}",
        err(&o)
    );
    assert!(
        w.adds().is_empty(),
        "`add` must not run once the entry exists: {:?}",
        w.adds()
    );
    assert_eq!(
        w.state("codex").unwrap()["command"],
        "/opt/other/mind",
        "the other process's entry was overwritten"
    );
}

#[test]
fn the_report_of_an_existing_entry_never_contains_its_environment_values() {
    need_python!();
    let w = World::new();
    std::fs::create_dir_all(w.bin.join(".fake")).unwrap();
    std::fs::write(
        w.bin.join(".fake/codex.state.json"),
        r#"{"scope":"user","command":"/x/mind","args":["--db","/x.db","mcp"],"env":{"API_TOKEN":"s3cr3t-value"}}"#,
    )
    .unwrap();
    let o = run(w.setup(&["codex", "--apply", "--yes"]));
    assert!(!o.status.success());
    let all = format!("{}{}", out(&o), err(&o));
    assert!(all.contains("already has a `mindspace` entry"), "{all}");
    assert!(
        !all.contains("s3cr3t-value") && !all.contains("API_TOKEN"),
        "secret leaked: {all}"
    );
}

#[test]
fn paths_that_are_not_valid_utf8_are_refused_because_they_could_not_be_registered_faithfully() {
    need_python!();
    use std::os::unix::ffi::OsStrExt;
    let w = World::new();
    let odd = w.projects.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir(&odd).unwrap();
    let o = run({
        let mut c = w.setup(&["claude-code", "--apply", "--yes"]);
        c.arg("--ro").arg(&odd);
        c
    });
    assert!(!o.status.success());
    assert!(err(&o).contains("not valid UTF-8"), "{}", err(&o));
    assert!(w.adds().is_empty());
}

#[test]
fn a_link_whose_target_has_a_control_character_in_its_name_is_refused() {
    need_python!();
    let w = World::new();
    let nasty = w.projects.join("evil\x1b[2Jname");
    std::fs::create_dir(&nasty).unwrap();
    let link = w.root.join("friendly");
    std::os::unix::fs::symlink(&nasty, &link).unwrap();
    let o = run(w.setup(&["claude-code", "--ro", link.to_str().unwrap()]));
    assert!(!o.status.success());
    assert!(err(&o).contains("control character"), "{}", err(&o));
    assert!(
        !out(&o).contains('\x1b') && !err(&o).contains('\x1b'),
        "terminal escape sequence reached the output"
    );
}

#[test]
fn empty_and_relative_path_entries_are_not_used_to_find_a_client() {
    need_python!();
    let w = World::with(&[]);
    // a `claude` in the working directory, reachable only through "" or a relative PATH entry
    install_fake(&w.root, "claude");
    let mut c = w.setup(&[]);
    c.env(
        "PATH",
        format!(
            ":relative:{}:{}",
            w.root.display().to_string().replacen('/', "", 1),
            w.tools.display()
        ),
    );
    let text = out(&run(c));
    assert!(
        text.lines()
            .any(|l| l.contains("claude-code") && l.contains("not found")),
        "{text}"
    );
}

// ------------------------------------------------------------ real clients

fn find_real(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| {
            p.is_file()
                && std::fs::metadata(p)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
        })
}

struct Real {
    _t: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    db: PathBuf,
    path: String,
}

fn real_world(program: &Path) -> Real {
    let t = tempfile::tempdir().unwrap();
    let root = canon(t.path());
    let (home, data) = (root.join("home"), root.join("data"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let db = data.join("mind.db");
    mindspace::initialize(&db).unwrap();
    let path = format!("{}:/usr/bin:/bin", program.parent().unwrap().display());
    Real {
        _t: t,
        root,
        home,
        db,
        path,
    }
}

impl Real {
    /// An environment that confines the client to this world.
    fn env(&self, c: &mut Command) {
        c.env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"))
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .current_dir(&self.root);
    }
    fn setup(&self, args: &[&str]) -> Command {
        let mut c = Command::new(MIND);
        self.env(&mut c);
        c.env("MINDSPACE_DB", &self.db).arg("setup").args(args);
        c
    }
    fn client(&self, program: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(program);
        self.env(&mut c);
        c.args(args);
        run(c)
    }
}

#[test]
fn real_claude_code_registers_verifies_refuses_a_second_time_and_keeps_foreign_entries() {
    let Some(claude) = find_real("claude") else {
        eprintln!("skipped: `claude` is not installed");
        return;
    };
    let r = real_world(&claude);
    let repos = r.root.join("repos");
    std::fs::create_dir(&repos).unwrap();
    let repos = canon(&repos);

    // a foreign server first (this also creates the client's own config in the throw-away HOME)
    let o = r.client(
        &claude,
        &["mcp", "add", "-s", "user", "other", "--", "/bin/true"],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let cfg = r.home.join(".claude/.claude.json");
    let before_bytes = std::fs::read(&cfg).unwrap();

    // preview: the client's configuration does not change
    let o = run(r.setup(&["claude-code", "--ro", repos.to_str().unwrap()]));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        std::fs::read(&cfg).unwrap(),
        before_bytes,
        "the preview changed Claude's configuration"
    );

    // apply
    let o = run(r.setup(&[
        "claude-code",
        "--ro",
        repos.to_str().unwrap(),
        "--apply",
        "--yes",
    ]));
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("Registered and verified"), "{}", out(&o));
    let get = r.client(&claude, &["mcp", "get", "mindspace"]);
    let text = out(&get);
    assert!(
        text.contains("User config") && text.contains("Connected"),
        "{text}"
    );
    assert!(text.contains(&format!("{}:ro", repos.display())), "{text}");
    // the foreign server is still there, unchanged
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["other"]["command"], "/bin/true");

    // a second setup refuses and the file does not change
    let after_first = std::fs::read(&cfg).unwrap();
    let o = run(r.setup(&["claude-code", "--apply", "--yes"]));
    assert!(!o.status.success());
    assert!(
        err(&o).contains("already has a `mindspace` entry"),
        "{}",
        err(&o)
    );
    assert_eq!(
        std::fs::read(&cfg).unwrap(),
        after_first,
        "the existing entry was touched"
    );
}

#[test]
fn real_claude_code_local_scope_is_keyed_to_the_directory_setup_runs_in() {
    let Some(claude) = find_real("claude") else {
        eprintln!("skipped: `claude` is not installed");
        return;
    };
    let r = real_world(&claude);
    let o = run(r.setup(&["claude-code", "--scope", "local", "--apply", "--yes"]));
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let text = out(&r.client(&claude, &["mcp", "get", "mindspace"]));
    assert!(text.contains("Local config"), "{text}");
}

#[test]
fn real_codex_registers_backs_up_keeps_foreign_servers_and_never_overwrites() {
    let Some(codex) = find_real("codex") else {
        eprintln!("skipped: `codex` is not installed");
        return;
    };
    let mut r = real_world(&codex);
    // Exercise the selected real runtime without reintroducing other installed
    // Codex binaries through /usr/bin. Production ambiguity checks remain intact.
    r.path = codex.parent().unwrap().display().to_string();
    let detection = mindspace::setup::client::detect(
        mindspace::setup::Client::Codex,
        Some(std::ffi::OsStr::new(&r.path)),
    );
    assert_eq!(detection.paths.len(), 1);
    assert_eq!(canon(&detection.paths[0]), canon(&codex));
    let repos = r.root.join("repos");
    std::fs::create_dir(&repos).unwrap();
    let repos = canon(&repos);
    let cfg_dir = r.home.join(".codex");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let cfg = cfg_dir.join("config.toml");
    let original = "# my own notes\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"npx\"\nargs = [\"-y\", \"x\"]   # keep me\n";
    std::fs::write(&cfg, original).unwrap();

    // preview: nothing changes
    let o = run(r.setup(&["codex", "--ro", repos.to_str().unwrap()]));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), original);

    let o = run(r.setup(&["codex", "--ro", repos.to_str().unwrap(), "--apply", "--yes"]));
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("Registered and read back"), "{}", out(&o));
    // the backup is the untouched original
    let backup = std::fs::read_dir(&cfg_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains("mindspace-backup"))
        .expect("a backup");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    // the foreign server survives (Codex may reformat it; the content is the same)
    let other: serde_json::Value =
        serde_json::from_slice(&r.client(&codex, &["mcp", "get", "other", "--json"]).stdout)
            .unwrap();
    assert_eq!(other["transport"]["command"], "npx");
    assert_eq!(other["transport"]["args"], serde_json::json!(["-y", "x"]));
    let mine: serde_json::Value = serde_json::from_slice(
        &r.client(&codex, &["mcp", "get", "mindspace", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(
        mine["transport"]["args"][4],
        format!("{}:ro", repos.display())
    );

    // Codex itself would overwrite a second registration silently; setup must not let it.
    let after = std::fs::read(&cfg).unwrap();
    let o = run(r.setup(&["codex", "--apply", "--yes"]));
    assert!(!o.status.success());
    assert!(
        err(&o).contains("already has a `mindspace` entry"),
        "{}",
        err(&o)
    );
    assert_eq!(
        std::fs::read(&cfg).unwrap(),
        after,
        "the existing Codex entry was overwritten"
    );
}

#[test]
fn real_opencode_is_never_started_by_a_preview() {
    let Some(opencode) = find_real("opencode") else {
        eprintln!("skipped: `opencode` is not installed");
        return;
    };
    let r = real_world(&opencode);
    let o = run(r.setup(&["opencode"]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("opencode mcp add --global mindspace --"),
        "{}",
        out(&o)
    );
    assert!(
        !r.home.join(".local").exists() && !r.home.join(".config").exists(),
        "the preview made OpenCode create files: {:?}",
        tree(&r.home)
    );
}
