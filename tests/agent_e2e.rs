//! End-to-end "agent" tests over the real `mind` binary.
//!
//! A scripted agent does what an LLM agent would: outline a repository,
//! ingest only the directories that matter, search for definitions, then open
//! the reported source file at the reported commit and check that the reported
//! lines really contain the definition. Everything goes through MCP stdio
//! (and the CLI for parity), never through library calls.
//!
//! - `record_manual_write_is_found_in_trait_and_impl` always runs on a
//!   fixture shaped like the original failure.
//! - `real_repo_agent_flow` runs against any repository when
//!   `MINDSPACE_E2E_REPO=<repo dir>` is set (optionally
//!   `MINDSPACE_E2E_REQUIRED="symbol:path1,path2;symbol2:path"`). The repo is
//!   never touched: only its `.git` directory is copied into a temp dir.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// MCP client
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start(db: &Path, root: &Path) -> Self {
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
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Self { child, stdin, out, next_id: 1 };
        s.rpc(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
                   "clientInfo":{"name":"agent-e2e","version":"0"}}),
        );
        writeln!(s.stdin, "{}", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap();
        s
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let req = json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params});
        writeln!(self.stdin, "{req}").unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    /// A tool call that must succeed; returns its structured content.
    fn ok(&mut self, name: &str, args: Value) -> Value {
        let r = self.call(name, args);
        assert_eq!(r["isError"], false, "{name} failed: {}", r["content"][0]["text"]);
        r["structuredContent"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mind(db: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_mind"))
        .env("MINDSPACE_DB", db)
        .args(args)
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// Opens a file the way an agent would: at the commit the hit names.
struct Opener<'a> {
    repo: &'a Path,
    cache: HashMap<(String, String), String>,
}

impl<'a> Opener<'a> {
    fn lines(&mut self, hit: &Value) -> String {
        let commit = hit["commit_oid"].as_str().expect("git hit has a commit").to_string();
        let path = hit["path"].as_str().unwrap().to_string();
        let text = self
            .cache
            .entry((commit.clone(), path.clone()))
            .or_insert_with(|| git(self.repo, &["show", &format!("{commit}:{path}")]));
        let (start, end) = (hit["start_line"].as_u64().unwrap() as usize, hit["end_line"].as_u64().unwrap() as usize);
        text.lines().skip(start - 1).take(end - start + 1).collect::<Vec<_>>().join("\n")
    }
}

fn covers(hit: &Value, path: &str, line: u64) -> bool {
    hit["path"] == path
        && hit["start_line"].as_u64().unwrap() <= line
        && line <= hit["end_line"].as_u64().unwrap()
}

fn hits_of(v: &Value) -> Vec<Value> {
    v["hits"].as_array().cloned().unwrap_or_default()
}

fn key_set(hits: &[Value]) -> Vec<(String, u64)> {
    let mut k: Vec<_> = hits
        .iter()
        .map(|h| (h["path"].as_str().unwrap().to_string(), h["start_line"].as_u64().unwrap()))
        .collect();
    k.sort();
    k
}

/// `git grep` ground truth: `(path, line, name)` of `fn` definitions at HEAD.
fn ground_truth(repo: &Path, dirs: &[String]) -> Vec<(String, u64, String)> {
    let mut args = vec![
        "grep".to_string(), "-n".into(), "-I".into(), "-E".into(),
        r"^\s*(pub(\([a-z]+\))?\s+)?((async|const|unsafe)\s+)*fn\s+[A-Za-z_][A-Za-z0-9_]*".into(),
        "HEAD".into(), "--".into(),
    ];
    args.extend(dirs.iter().map(|d| format!("{d}/**/*.rs")));
    args.extend(dirs.iter().map(|d| format!("{d}/*.rs")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = Command::new("git").arg("-C").arg(repo).args(&refs).output().unwrap();
    let mut v = Vec::new();
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(rest) = l.strip_prefix("HEAD:") else { continue };
        let mut it = rest.splitn(3, ':');
        let (Some(path), Some(line), Some(content)) = (it.next(), it.next(), it.next()) else { continue };
        let Some(after) = content.split("fn ").nth(1) else { continue };
        let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if let Ok(n) = line.parse::<u64>() {
            v.push((path.to_string(), n, name));
        }
    }
    v.sort();
    v.dedup();
    v
}

struct Outcome {
    outline_nodes: usize,
    ingested_dirs: Vec<String>,
    ground_truth: usize,
    sampled: usize,
    found: usize,
    hits_opened: usize,
}

/// The scripted agent. `only` restricts which outline directories it picks.
fn agent_flow(
    source_repo: &Path,
    only: Option<&[&str]>,
    required: &[(&str, Vec<&str>)],
    sample: usize,
) -> Outcome {
    // Work on a copy of `.git` so the original repository stays untouched.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let cp = Command::new("cp").arg("-a").arg(source_repo.join(".git")).arg(repo.join(".git")).status().unwrap();
    assert!(cp.success(), "copying .git failed");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let db = root.join("mind.db");
    assert!(mind(&db, &["init"]).1);
    assert!(mind(&db, &["space", "create", "project"]).1);
    let mut s = Server::start(&db, &root);

    // 1. The agent discovers its tools.
    let tools = s.rpc("tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for t in ["repo_outline", "repo_ingest", "code_search"] {
        assert!(names.contains(&t), "tool {t} missing");
    }

    // 2. Outline, then choose directories that contain Rust code.
    let repo_s = repo.display().to_string();
    let outline = s.ok("repo_outline", json!({"path": repo_s, "depth": 2}));
    assert_eq!(outline["outline"]["source"], "git");
    assert_eq!(outline["outline"]["commit"], head, "outline reads HEAD");
    let nodes = outline["outline"]["nodes"].as_array().unwrap();
    let mut dirs: Vec<String> = nodes
        .iter()
        .filter(|n| n["depth"] == 1 && n["kinds"]["rust"].as_u64().unwrap_or(0) > 0)
        .map(|n| n["path"].as_str().unwrap().to_string())
        .filter(|p| only.is_none_or(|o| o.contains(&p.as_str())))
        .collect();
    if dirs.is_empty() {
        dirs = nodes
            .iter()
            .filter(|n| n["depth"] == 2 && n["kinds"]["rust"].as_u64().unwrap_or(0) > 0)
            .map(|n| n["path"].as_str().unwrap().to_string())
            .collect();
    }
    assert!(!dirs.is_empty(), "outline showed no Rust directories");

    // 3. Targeted ingest of just those directories.
    let ing = s.ok("repo_ingest", json!({"path": repo_s, "spaces": ["project"], "paths": dirs}));
    let report = &ing["ingested"][0];
    assert_eq!(report["kind"], "git");
    assert!(report["derivations_new"].as_u64().unwrap() > 0);
    assert_eq!(report["stale_retired"], 0);

    // 4. Required symbols: each must be found, and opening the reported
    //    lines at the reported commit must show the definition.
    let mut opener = Opener { repo: &repo, cache: HashMap::new() };
    let mut hits_opened = 0;
    for (symbol, files) in required {
        let hits = hits_of(&s.ok("code_search", json!({"query": symbol, "kinds": ["function","method"], "limit": 100})));
        for file in files {
            let h = hits
                .iter()
                .find(|h| h["path"] == *file && h["name"] == *symbol)
                .unwrap_or_else(|| panic!("{symbol} not found in {file}; got {:?}", key_set(&hits)));
            assert_eq!(h["commit_oid"], head);
            assert_eq!(h["current"], true);
            let text = opener.lines(h);
            assert!(text.contains(&format!("fn {symbol}")), "{file}: lines do not show {symbol}:\n{text}");
            hits_opened += 1;
        }
    }

    // 5. Recall and precision against `git grep` ground truth.
    let truth: Vec<_> = ground_truth(&repo, &dirs).into_iter().filter(|(_, _, n)| n.len() >= 6).collect();
    let step = (truth.len() / sample.max(1)).max(1);
    let picked: Vec<_> = truth.iter().step_by(step).take(sample).cloned().collect();
    let mut found = 0;
    for (i, (path, line, name)) in picked.iter().enumerate() {
        let hits = hits_of(&s.ok("code_search", json!({"query": name, "kinds": ["function","method"], "limit": 100})));
        if hits.iter().any(|h| covers(h, path, *line)) {
            found += 1;
        } else {
            eprintln!("MISS: {name} at {path}:{line}");
        }
        // Precision: whatever comes back must really be there.
        for h in hits.iter().take(5) {
            let text = opener.lines(h);
            let hn = h["name"].as_str().unwrap();
            assert!(text.contains(&format!("fn {hn}")), "{}:{} does not show fn {hn}", h["path"], h["start_line"]);
            hits_opened += 1;
        }
        // CLI parity on a few samples.
        if i < 3 {
            let (out, ok) = mind(&db, &["symbols", name, "--kind", "fn", "--kind", "method", "--limit", "100", "--json"]);
            assert!(ok);
            let cli: Vec<Value> = serde_json::from_str(&out).unwrap();
            assert_eq!(key_set(&cli), key_set(&hits), "CLI and MCP disagree for {name}");
        }
    }

    Outcome {
        outline_nodes: nodes.len(),
        ingested_dirs: dirs,
        ground_truth: truth.len(),
        sampled: picked.len(),
        found,
        hits_opened,
    }
}

// ---------------------------------------------------------------------------
// Fixture shaped like the original failure
// ---------------------------------------------------------------------------

fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    write(d.path(), "src/storage/mod.rs",
        "pub trait Store {\n    /// Atomically creates or reuses an explicitly written memory.\n    fn record_manual_write(&self, draft: ManualMemoryDraft) -> Result<ManualMemoryReport>;\n    fn get_space(&self, name: &str) -> Result<Space>;\n}\n");
    write(d.path(), "src/storage/sqlite.rs",
        "pub struct SqliteStore;\n\nimpl Store for SqliteStore {\n    fn record_manual_write(&self, draft: ManualMemoryDraft) -> Result<ManualMemoryReport> {\n        todo!()\n    }\n\n    fn get_space(&self, name: &str) -> Result<Space> {\n        todo!()\n    }\n}\n");
    write(d.path(), "src/mcp/tools.rs", "pub fn call_tool(name: &str) {}\n\npub async fn dispatch_request() {}\n");
    write(d.path(), "bench/bench.rs", "pub fn bench_only_function() {}\n");
    write(d.path(), "docs/notes.md", "# Notes\n\nStorage notes live here.\n");
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "fixture"]);
    d
}

#[test]
fn record_manual_write_is_found_in_trait_and_impl() {
    let repo = fixture();
    let out = agent_flow(
        repo.path(),
        Some(&["src"]),
        &[("record_manual_write", vec!["src/storage/mod.rs", "src/storage/sqlite.rs"])],
        20,
    );
    assert_eq!(out.ingested_dirs, vec!["src"], "targeted: only the chosen directory");
    assert_eq!(out.found, out.sampled, "recall against git grep");
    assert!(out.sampled >= 3);
}

#[test]
fn fixture_details_qualified_names_scoping_and_memory_boundary() {
    let repo = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("repo")).unwrap();
    assert!(Command::new("cp").arg("-a").arg(repo.path().join(".git")).arg(root.join("repo/.git")).status().unwrap().success());
    let db = root.join("mind.db");
    assert!(mind(&db, &["init"]).1 && mind(&db, &["space", "create", "project"]).1);
    let mut s = Server::start(&db, &root);
    let p = root.join("repo").display().to_string();
    s.ok("repo_ingest", json!({"path": p, "spaces": ["project"], "paths": ["src", "docs"]}));

    // Trait declaration vs. implementation: named after the right owners.
    let hits = hits_of(&s.ok("code_search", json!({"query": "record_manual_write"})));
    let q: Vec<&str> = hits.iter().map(|h| h["qualified_name"].as_str().unwrap()).collect();
    assert!(q.contains(&"Store::record_manual_write"), "{q:?}");
    assert!(q.contains(&"SqliteStore::record_manual_write"), "{q:?}");
    // A directory that was not selected is not searchable.
    assert!(hits_of(&s.ok("code_search", json!({"query": "bench_only_function"}))).is_empty());
    // Code is not a memory: document search does not return definitions,
    // while the document that was ingested is found.
    assert!(hits_of(&s.ok("memory_search", json!({"query": "record_manual_write"}))).is_empty());
    assert_eq!(hits_of(&s.ok("memory_search", json!({"query": "Storage notes"}))).len(), 1);
}

// ---------------------------------------------------------------------------
// Real repository (opt-in)
// ---------------------------------------------------------------------------

#[test]
fn real_repo_agent_flow() {
    let Some(repo) = std::env::var_os("MINDSPACE_E2E_REPO").map(PathBuf::from) else {
        eprintln!("skipped: set MINDSPACE_E2E_REPO=<repo dir> to run against a real repository");
        return;
    };
    let spec = std::env::var("MINDSPACE_E2E_REQUIRED").unwrap_or_default();
    let required: Vec<(&str, Vec<&str>)> = spec
        .split(';')
        .filter(|g| !g.trim().is_empty())
        .map(|g| {
            let (sym, files) = g.split_once(':').expect("symbol:file1,file2");
            (sym, files.split(',').collect())
        })
        .collect();
    let min_recall: f64 = std::env::var("MINDSPACE_E2E_MIN_RECALL").ok().and_then(|v| v.parse().ok()).unwrap_or(0.9);
    let out = agent_flow(&repo, None, &required, 25);
    println!(
        "E2E {}: outline_nodes={} ingested={:?} ground_truth_fns={} sampled={} found={} hits_opened_and_verified={}",
        repo.display(), out.outline_nodes, out.ingested_dirs, out.ground_truth, out.sampled, out.found, out.hits_opened
    );
    assert!(out.sampled > 0, "no ground truth to sample");
    assert!(out.found as f64 / out.sampled as f64 >= min_recall, "recall {}/{} below {min_recall}", out.found, out.sampled);
}
