//! Data path resolution and the small CLI/UX corrections of M7.1.
//!
//! The binary runs with a CLEARED environment (only what a test names), so the
//! developer's own HOME, XDG_DATA_HOME and MINDSPACE_* can neither help nor
//! hurt. The rules themselves are unit-tested in `src/paths.rs`; these tests
//! check what a user sees. Nothing touches the network.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const MIND: &str = env!("CARGO_BIN_EXE_mind");
const SAFE_PATH: &str = "/usr/bin:/bin";

fn mind(args: &[&str]) -> Command {
    let mut c = Command::new(MIND);
    c.env_clear().env("PATH", SAFE_PATH).args(args);
    c
}

fn run(mut c: Command) -> Output {
    c.stdin(Stdio::null()).output().unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The path `mind init` reports ("initialized mindspace at <path>").
fn init_path(mut c: Command) -> String {
    let o = run({
        c.arg("init");
        c
    });
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .strip_prefix("initialized mindspace at ")
        .unwrap()
        .to_string()
}

#[test]
fn the_database_path_priority_is_flag_then_environment_then_default() {
    let t = tempfile::tempdir().unwrap();
    let (flag, env, xdg) = (
        t.path().join("flag.db"),
        t.path().join("env.db"),
        t.path().join("xdg"),
    );

    let mut c = mind(&["--db", flag.to_str().unwrap()]);
    c.env("MINDSPACE_DB", &env).env("XDG_DATA_HOME", &xdg);
    assert_eq!(init_path(c), flag.to_str().unwrap(), "the flag wins");

    let mut c = mind(&[]);
    c.env("MINDSPACE_DB", &env).env("XDG_DATA_HOME", &xdg);
    assert_eq!(init_path(c), env.to_str().unwrap(), "then the environment");

    let mut c = mind(&[]);
    c.env("XDG_DATA_HOME", &xdg);
    assert_eq!(
        init_path(c),
        xdg.join("mindspace/mind.db").to_str().unwrap(),
        "then the default"
    );

    // HOME is the fallback when XDG_DATA_HOME is not usable (relative).
    let home = t.path().join("home");
    let mut c = mind(&[]);
    c.env("XDG_DATA_HOME", "relative/dir").env("HOME", &home);
    assert_eq!(
        init_path(c),
        home.join(".local/share/mindspace/mind.db")
            .to_str()
            .unwrap()
    );
}

#[test]
fn without_home_and_xdg_init_refuses_and_writes_nothing_to_the_working_directory() {
    let t = tempfile::tempdir().unwrap();
    let cwd = t.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    let mut c = mind(&["init"]);
    c.current_dir(&cwd);
    let o = run(c);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("MINDSPACE_DB") && err.contains("neither XDG_DATA_HOME nor HOME"),
        "{err}"
    );
    assert!(names(&cwd).is_empty(), "files appeared: {:?}", names(&cwd));
}

#[test]
fn without_home_and_xdg_models_are_not_found_and_search_falls_back_to_lexical() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("m.db");
    assert!(run(mind(&["--db", db.to_str().unwrap(), "init"]))
        .status
        .success());

    // No guessed model directory: the status says why it cannot answer.
    let o = run(mind(&[
        "--db",
        db.to_str().unwrap(),
        "embeddings",
        "status",
    ]));
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot determine where to keep data"));

    // Auto falls back to lexical and says why; an explicit semantic request fails clearly.
    let o = run(mind(&[
        "--db",
        db.to_str().unwrap(),
        "search",
        "hello",
        "--mode",
        "auto",
        "--json",
    ]));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["effective"], "lexical");
    // Without the embeddings feature the reason is the missing feature, not the path.
    let expected = if cfg!(feature = "embeddings") {
        "runtime_unavailable"
    } else {
        "feature_disabled"
    };
    assert_eq!(v["fallback"]["kind"], expected);
    let o = run(mind(&[
        "--db",
        db.to_str().unwrap(),
        "search",
        "hello",
        "--mode",
        "semantic",
    ]));
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    if cfg!(feature = "embeddings") {
        assert!(err.contains("cannot determine where to keep data"), "{err}");
    } else {
        assert!(err.contains("unavailable"), "{err}");
    }
    assert_eq!(names(t.path()), ["m.db"], "nothing else was created");
}

#[test]
fn an_uninitialized_database_gives_one_clear_error_for_mcp_before_any_sandbox_work() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repos");
    std::fs::create_dir(&root).unwrap();
    let db = t.path().join("db/mind.db");
    let mut c = mind(&["mcp", "--allow-root", &format!("{}:rw", root.display())]);
    c.env("MINDSPACE_DB", &db);
    let o = run(c);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(
        err.trim().lines().count(),
        1,
        "exactly one message expected:\n{err}"
    );
    assert!(err.contains("is not initialized; run: mind init"), "{err}");
    assert!(!err.contains("repository tools are OFF"), "{err}");
    assert!(
        !db.parent().unwrap().exists(),
        "no directory may be created"
    );
}

#[test]
fn init_names_the_next_step() {
    let t = tempfile::tempdir().unwrap();
    let o = run({
        let mut c = mind(&["init"]);
        c.env("XDG_DATA_HOME", t.path());
        c
    });
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("initialized mindspace at"), "{out}");
    assert!(out.contains("next: mind space create <name>"), "{out}");
}

#[test]
fn model_install_describes_its_arguments() {
    let o = run(mind(&["model", "install", "--help"]));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("Model id, as listed by `mind model list`"),
        "{out}"
    );
    assert!(out.contains("without asking for confirmation"), "{out}");
}
