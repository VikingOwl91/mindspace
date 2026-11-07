//! `mind repo info` and the repository block of MCP `repo_outline`: slug from
//! remotes, credentials never shown, worktrees listed, nothing written.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn repo_with_remotes(remotes: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("a.md"), "# A\n\nbody\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    for (name, url) in remotes {
        git(d.path(), &["remote", "add", name, url]);
    }
    d
}

fn mind(db: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_mind"))
        .env("MINDSPACE_DB", db)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn info(repo: &Path) -> Value {
    let (out, err, ok) = mind(Path::new("/nonexistent/unused.db"), &["repo", "info", &repo.display().to_string(), "--json"]);
    assert!(ok, "{err}");
    serde_json::from_str(&out).unwrap()
}

#[test]
fn origin_decides_the_slug_and_mirrors_do_not_confuse_it() {
    let r = repo_with_remotes(&[
        ("origin", "git@github.com:VikingOwl91/mindspace.git"),
        ("mirror", "https://git.example.com/vikingowl91/mindspace.git"),
    ]);
    let v = info(r.path());
    assert_eq!(v["slug"], "VikingOwl91/mindspace");
    assert_eq!(v["slug_key"], "github.com/vikingowl91/mindspace");
    assert_eq!(v["slug_source"], "remote 'origin'");
    assert_eq!(v["remotes"].as_array().unwrap().len(), 2);
    assert_eq!(v["head"]["branch"], "main");
    assert!(v["identity"].is_null(), "never ingested, so no identity yet");
}

#[test]
fn a_single_slug_without_origin_is_used_and_disagreement_stays_unnamed() {
    let only = repo_with_remotes(&[("gitlab", "https://gitlab.com/g/s/proj.git")]);
    let v = info(only.path());
    assert_eq!(v["slug_key"], "gitlab.com/g/s/proj");
    assert_eq!(v["slug_source"], "remote 'gitlab'");

    let split = repo_with_remotes(&[
        ("a", "https://github.com/x/one.git"),
        ("b", "https://gitlab.com/y/two.git"),
    ]);
    let v = info(split.path());
    assert!(v["slug"].is_null());
    assert_eq!(v["slug_candidates"], json!(["github.com/x/one", "gitlab.com/y/two"]));

    let none = repo_with_remotes(&[]);
    let v = info(none.path());
    assert!(v["slug"].is_null() && v["remotes"].as_array().unwrap().is_empty());
}

#[test]
fn credentials_in_remote_urls_never_appear_in_any_output() {
    let r = repo_with_remotes(&[("origin", "https://ci-bot:SECRETTOKEN123@github.com/o/r.git")]);
    let db = r.path().join("x.db");
    let path = r.path().display().to_string();
    let (human, _, ok1) = mind(&db, &["repo", "info", &path]);
    let (json_out, _, ok2) = mind(&db, &["repo", "info", &path, "--json"]);
    assert!(ok1 && ok2);
    for out in [&human, &json_out] {
        assert!(!out.contains("SECRETTOKEN123") && !out.contains("ci-bot"), "{out}");
    }
    assert!(human.contains("github.com/o/r.git"));
}

#[test]
fn info_and_scan_write_nothing_into_the_repository() {
    let r = repo_with_remotes(&[("origin", "git@github.com:o/r.git")]);
    let db = r.path().join("never.db");
    let path = r.path().display().to_string();
    assert!(mind(&db, &["repo", "info", &path]).2);
    assert!(mind(&db, &["scan", &path]).2);
    assert!(!r.path().join(".git/mindspace-repository-id").exists());
    assert!(!db.exists(), "neither command opens a database");
}

#[test]
fn identity_appears_after_the_first_ingest() {
    let r = repo_with_remotes(&[("origin", "git@github.com:o/r.git")]);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mind.db");
    assert!(mind(&db, &["init"]).2 && mind(&db, &["space", "create", "p"]).2);
    let path = r.path().display().to_string();
    assert!(mind(&db, &["ingest", &path, "--space", "p"]).2);
    let v = info(r.path());
    assert!(v["identity"].is_string(), "{v}");
}

#[test]
fn worktrees_are_listed_with_prunable_ones_marked() {
    let r = repo_with_remotes(&[]);
    let parent = tempfile::tempdir().unwrap();
    let live = parent.path().join("live");
    let gone = parent.path().join("gone");
    git(r.path(), &["worktree", "add", "-q", "-b", "live-branch", live.to_str().unwrap()]);
    git(r.path(), &["worktree", "add", "-q", "--detach", gone.to_str().unwrap()]);
    std::fs::remove_dir_all(&gone).unwrap();

    let v = info(r.path());
    let wts = v["worktrees"].as_array().unwrap();
    assert_eq!(wts.len(), 3);
    assert_eq!(wts[0]["is_main"], true);
    let by = |suffix: &str| wts.iter().find(|w| w["path"].as_str().unwrap().ends_with(suffix)).unwrap();
    assert_eq!(by("live")["branch"], "live-branch");
    assert_eq!(by("live")["prunable"], false);
    assert_eq!(by("gone")["prunable"], true);
    assert_eq!(by("gone")["detached"], true);
}

#[test]
fn non_git_directory_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let (_, err, ok) = mind(Path::new("/nonexistent/unused.db"), &["repo", "info", &d.path().display().to_string()]);
    assert!(!ok);
    assert!(err.contains("not a git repository"), "{err}");
}

// ---- MCP ------------------------------------------------------------------

fn mcp_outline(db: &Path, root: &Path, path: &Path) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mind"))
        .env("MINDSPACE_DB", db)
        .env_remove("MINDSPACE_MCP_ALLOW_ROOTS")
        .args(["mcp", "--allow-root"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    fn rpc(
        stdin: &mut std::process::ChildStdin,
        out: &mut BufReader<std::process::ChildStdout>,
        req: Value,
    ) -> Value {
        writeln!(stdin, "{req}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    rpc(&mut stdin, &mut out, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}));
    writeln!(stdin, "{}", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap();
    let resp = rpc(&mut stdin, &mut out, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"repo_outline","arguments":{"path": path.display().to_string()}}}));
    let _ = child.kill();
    let _ = child.wait();
    resp["result"].clone()
}

#[test]
fn repo_outline_reports_the_slug_and_redacts_worktree_paths_outside_the_roots() {
    let allowed = tempfile::tempdir().unwrap();
    let root: PathBuf = allowed.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.md"), "# A\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    git(&repo, &["remote", "add", "origin", "https://user:SECRETTOKEN123@github.com/o/r.git"]);
    let inside = root.join("wt-inside");
    git(&repo, &["worktree", "add", "-q", "-b", "in", inside.to_str().unwrap()]);
    let outside_parent = tempfile::tempdir().unwrap();
    let outside = outside_parent.path().canonicalize().unwrap().join("wt-outside");
    git(&repo, &["worktree", "add", "-q", "-b", "out", outside.to_str().unwrap()]);

    let db = root.join("mind.db");
    assert!(mind(&db, &["init"]).2);
    let result = mcp_outline(&db, &root, &repo);
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("SECRETTOKEN123"), "credential leaked");
    assert!(!text.contains(outside.to_str().unwrap()), "outside path leaked");

    let repo_block = &result["structuredContent"]["repository"];
    assert_eq!(repo_block["slug"], "o/r");
    assert_eq!(repo_block["slug_key"], "github.com/o/r");
    let wts = repo_block["worktrees"].as_array().unwrap();
    let branch = |b: &str| wts.iter().find(|w| w["branch"] == b).unwrap();
    assert_eq!(branch("in")["path"], inside.to_str().unwrap());
    assert!(branch("out")["path"].is_null(), "outside worktree path is redacted");
    assert_eq!(branch("out")["branch"], "out", "other facts stay visible");
}
