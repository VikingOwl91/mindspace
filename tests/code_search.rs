//! Symbol search over the deterministic code-structure layer (`mind symbols`,
//! MCP `code_search`): definitions with locations, current vs. stale.

use std::path::Path;
use std::process::Command;

struct Mind {
    dir: tempfile::TempDir,
}

impl Mind {
    fn new() -> Self {
        let m = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        assert!(m.run(&["init"]).2);
        assert!(m.run(&["space", "create", "p"]).2);
        m
    }

    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let out = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", self.dir.path().join("mind.db"))
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }

    fn ingest(&self, path: &Path, extra: &[&str]) {
        let p = path.display().to_string();
        let mut args = vec!["ingest", p.as_str(), "--space", "p"];
        args.extend_from_slice(extra);
        let (_, err, ok) = self.run(&args);
        assert!(ok, "ingest failed: {err}");
    }

    fn symbols(&self, args: &[&str]) -> Vec<serde_json::Value> {
        let mut a = vec!["symbols"];
        a.extend_from_slice(args);
        a.push("--json");
        let (out, err, ok) = self.run(&a);
        assert!(ok, "symbols failed: {err}");
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }
}

fn names(hits: &[serde_json::Value]) -> Vec<String> {
    hits.iter()
        .map(|h| {
            h["qualified_name"]
                .as_str()
                .or(h["name"].as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
}

const LIB: &str = "pub struct Store;\n\nimpl Store {\n    pub fn record_manual_write(&self) {}\n}\n\npub fn helper() {}\n";

#[test]
fn finds_definitions_ranked_with_locations() {
    let m = Mind::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(
        dir.path().join("src/other.rs"),
        "pub fn record_manual_write_all() {}\n",
    )
    .unwrap();
    m.ingest(dir.path(), &[]);

    let hits = m.symbols(&["record_manual_write"]);
    assert_eq!(
        hits[0]["name"], "record_manual_write",
        "exact name ranks first"
    );
    assert_eq!(hits[0]["kind"], "method");
    assert_eq!(hits[0]["match_rank"], 0);
    assert_eq!(hits[0]["start_line"], 4);
    assert!(hits[0]["path"].as_str().unwrap().ends_with("src/lib.rs"));
    assert_eq!(hits[0]["current"], true);
    assert!(names(&hits).iter().any(|n| n == "record_manual_write_all"));

    // Every term must match; kind and path filters narrow.
    assert_eq!(m.symbols(&["manual", "write", "--kind", "fn"]).len(), 1);
    assert_eq!(m.symbols(&["record", "--kind", "method"]).len(), 1);
    let only_other = m.symbols(&[
        "record",
        "--path",
        &format!(
            "{}/src/other.rs",
            dir.path().canonicalize().unwrap().display()
        ),
    ]);
    assert_eq!(names(&only_other), vec!["record_manual_write_all"]);
    assert!(m.symbols(&["no_such_symbol"]).is_empty());

    let (_, err, ok) = m.run(&["symbols", "x", "--kind", "banana"]);
    assert!(!ok && err.contains("unknown code kind"), "{err}");
}

#[test]
fn language_specific_definitions_keep_their_path_and_blob_across_ingests() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let common = "export function Shared() { return 1; }\n";
    let inputs = [
        ("shared.js", common, "Shared", "javascript", 1),
        ("shared.ts", common, "Shared", "typescript", 1),
        (
            "reader.go",
            "package reader\nfunc Read() {}\n",
            "Read",
            "go",
            2,
        ),
        (
            "view.tsx",
            "export const View = () => <div/>;\n",
            "View",
            "tsx",
            1,
        ),
        (
            "page.svelte",
            "<p>Page</p>\n<script lang='ts'>\nfunction Render() {}\n</script>",
            "Render",
            "svelte",
            3,
        ),
    ];
    for (path, content, ..) in inputs {
        std::fs::write(root.join(path), content).unwrap();
    }
    std::fs::write(root.join("other.py"), "def omitted(): pass\n").unwrap();
    let mut shared_blobs = Vec::new();
    for git_repo in [false, true] {
        let m = Mind::new();
        if git_repo {
            git(root, &["init", "-q", "-b", "main"]);
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", "neutral declarations"]);
        }
        let db = rusqlite::Connection::open(m.dir.path().join("mind.db")).unwrap();
        for _ in 0..2 {
            m.ingest(root, &[]);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM code_derivations", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                inputs.len() as i64
            );
            for (path, content, symbol, lang, line) in inputs {
                let hits = m.symbols(&[symbol, "--language", lang]);
                assert_eq!(hits.len(), 1, "{hits:?}");
                let hit = &hits[0];
                assert_eq!(hit["language"], lang);
                assert_eq!(hit["start_line"], line);
                assert_eq!(
                    hit["content_hash"],
                    mindspace::core::sha256_hex(content.as_bytes())
                );
                if git_repo {
                    assert_eq!(hit["path"], path);
                    let oid = Command::new("git")
                        .arg("-C")
                        .arg(root)
                        .args(["rev-parse", &format!("HEAD:{path}")])
                        .output()
                        .unwrap();
                    assert_eq!(
                        hit["blob_oid"],
                        String::from_utf8(oid.stdout).unwrap().trim()
                    );
                    if symbol == "Shared" {
                        shared_blobs.push(hit["blob_oid"].clone());
                    }
                } else {
                    assert_eq!(
                        hit["path"],
                        root.canonicalize().unwrap().join(path).to_str().unwrap()
                    );
                    assert!(hit["blob_oid"].is_null());
                }
            }
            assert!(m.symbols(&["omitted"]).is_empty());
        }
        let (outline, err, ok) = m.run(&["scan", root.to_str().unwrap(), "--json"]);
        assert!(ok, "{err}");
        let outline: serde_json::Value = serde_json::from_str(&outline).unwrap();
        assert_eq!(outline["nodes"][0]["unsupported_extensions"]["py"], 1);
        let (_, err, ok) = m.run(&["symbols", "omitted", "--language", "python"]);
        assert!(!ok && err.contains("unsupported symbol language"), "{err}");
    }
    assert_eq!(shared_blobs.len(), 4);
    assert!(shared_blobs.iter().all(|oid| oid == &shared_blobs[0]));
}

#[test]
fn selected_unsupported_only_and_mixed_inputs_report_observed_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("unknown")).unwrap();
    for (p, text) in [
        ("unknown/first.py", "def first(): pass"),
        ("unknown/second.PY", "def second(): pass"),
        ("unknown/record.private-suffix", "opaque"),
        ("outside.rb", "def outside; end"),
        ("known.go", "package pkg\nfunc Known() {}"),
    ] {
        std::fs::write(root.join(p), text).unwrap();
    }
    for git_repo in [false, true] {
        let m = Mind::new();
        if git_repo {
            git(root, &["init", "-q", "-b", "main"]);
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", "file types"]);
        }
        let args = ["ingest", root.to_str().unwrap(), "--space", "p"];
        let read = |extra: &[&str]| {
            let mut a = args.to_vec();
            a.extend_from_slice(extra);
            let (out, err, ok) = m.run(&a);
            assert!(ok, "{err}");
            out
        };
        let selected: serde_json::Value =
            serde_json::from_str(&read(&["--path", "unknown", "--json"])).unwrap();
        assert_eq!(
            selected["code"]["unsupported_extensions"],
            serde_json::json!({"py":2,"other":1})
        );
        assert_eq!(selected["code"]["supported_inputs"], 0);
        let human = read(&["--path", "unknown"]);
        assert!(
            human.contains("unsupported file extensions: other=1 py=2"),
            "{human}"
        );
        let mixed: serde_json::Value = serde_json::from_str(&read(&["--json"])).unwrap();
        assert_eq!(
            mixed["code"]["unsupported_extensions"],
            serde_json::json!({"py":2,"other":1,"rb":1})
        );
        assert_eq!(
            mixed["code"]["inputs_by_language"],
            serde_json::json!({"go":1})
        );
    }
}

#[test]
fn file_edits_make_old_definitions_not_current() {
    let m = Mind::new();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn old_name() {}\n").unwrap();
    m.ingest(dir.path(), &[]);
    assert_eq!(m.symbols(&["old_name"]).len(), 1);

    std::fs::write(&file, "pub fn new_name() {}\n").unwrap();
    m.ingest(dir.path(), &[]);
    assert!(
        m.symbols(&["old_name"]).is_empty(),
        "stale hidden by default"
    );
    assert_eq!(m.symbols(&["new_name"]).len(), 1);

    let all = m.symbols(&["old_name", "--all-versions"]);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["current"], false);
}

#[test]
fn git_head_moves_make_old_definitions_not_current() {
    let m = Mind::new();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.path().join("crates/a")).unwrap();
    std::fs::write(
        repo.path().join("crates/a/lib.rs"),
        "pub fn first_impl() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "one"]);
    m.ingest(repo.path(), &[]);

    let hits = m.symbols(&["first_impl"]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["path"], "crates/a/lib.rs");
    assert!(hits[0]["commit_oid"].as_str().unwrap().len() >= 40);
    assert!(hits[0]["repository_path"].is_string());

    std::fs::write(
        repo.path().join("crates/a/lib.rs"),
        "pub fn second_impl() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "two"]);
    m.ingest(repo.path(), &[]);

    assert!(m.symbols(&["first_impl"]).is_empty());
    assert_eq!(m.symbols(&["second_impl"]).len(), 1);
    let old = m.symbols(&["first_impl", "--all-versions"]);
    assert_eq!(old[0]["current"], false);

    // Repository and path filters.
    assert_eq!(m.symbols(&["second_impl", "--path", "crates/a"]).len(), 1);
    assert!(m.symbols(&["second_impl", "--path", "crates/b"]).is_empty());
    let dir_name = repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(m.symbols(&["second_impl", "--repo", &dir_name]).len(), 1);
    assert!(m
        .symbols(&["second_impl", "--repo", "definitely-not-a-repo"])
        .is_empty());
}

#[test]
fn linked_worktrees_are_current_for_code_too() {
    let m = Mind::new();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("lib.rs"), "pub fn on_main() {}\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "one"]);
    git(repo.path(), &["branch", "feature"]);
    let parent = tempfile::tempdir().unwrap();
    let wt = parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"],
    );
    std::fs::write(wt.join("feat.rs"), "pub fn on_feature() {}\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "feat"]);

    m.ingest(repo.path(), &[]);
    // Both worktree HEADs are current views, so both commits' definitions are.
    let on_main = m.symbols(&["on_main"]);
    assert!(!on_main.is_empty() && on_main.iter().all(|h| h["current"] == true));
    let on_feature = m.symbols(&["on_feature"]);
    assert_eq!(on_feature.len(), 1);
    assert_eq!(on_feature[0]["current"], true);
}

#[test]
fn mcp_code_search_tool_returns_hits_and_rejects_bad_input() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let m = Mind::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), LIB).unwrap();
    m.ingest(dir.path(), &[]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_mind"))
        .env("MINDSPACE_DB", m.dir.path().join("mind.db"))
        .arg("mcp")
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
        req: serde_json::Value,
    ) -> serde_json::Value {
        writeln!(stdin, "{req}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    let init = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}});
    rpc(&mut stdin, &mut out, init);
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let mut call = |id: u64, args: serde_json::Value| {
        rpc(
            &mut stdin,
            &mut out,
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"code_search","arguments":args}}),
        )
    };

    let ok = call(
        2,
        serde_json::json!({"query":"record_manual_write","kinds":["method"]}),
    );
    assert_eq!(ok["result"]["isError"], false);
    let hits = ok["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["name"], "record_manual_write");

    let bad_kind = call(3, serde_json::json!({"query":"x","kinds":["banana"]}));
    assert_eq!(bad_kind["result"]["isError"], true);
    let extra = call(4, serde_json::json!({"query":"x","bogus":1}));
    assert_eq!(extra["result"]["isError"], true);
    let unsupported = call(5, serde_json::json!({"query":"x", "language":"python"}));
    assert_eq!(unsupported["result"]["isError"], true);
    assert!(unsupported
        .to_string()
        .contains("unsupported symbol language"));
    let absent = call(6, serde_json::json!({"query":"absent", "language":"go"}));
    assert_eq!(absent["result"]["isError"], false);
    assert_eq!(
        absent["result"]["structuredContent"]["hits"],
        serde_json::json!([])
    );
    assert!(absent["result"]["structuredContent"]["supported_languages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l == "go"));

    let _ = child.kill();
    let _ = child.wait();
}
