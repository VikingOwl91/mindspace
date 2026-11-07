//! `mind scan` and `mind ingest --path`: outline first, then ingest only
//! what was selected. End-to-end through the compiled binary.

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
        let (_, err, ok) = m.run(&["init"]);
        assert!(ok, "{err}");
        let (_, err, ok) = m.run(&["space", "create", "p"]);
        assert!(ok, "{err}");
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

    fn search_count(&self, q: &str) -> usize {
        let (out, err, ok) = self.run(&["search", q, "--json"]);
        assert!(ok, "{err}");
        serde_json::from_str::<serde_json::Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .len()
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
}

fn tree(root: &Path) {
    write(root, "README.md", "# Root\n\nrootmarker\n");
    write(root, "docs/guide.md", "# Guide\n\ndocsmarker\n");
    write(root, "notes/todo.md", "# Todo\n\nnotesmarker\n");
    write(root, "docs/deep/er/x.md", "# Deep\n\ndeepmarker\n");
}

#[test]
fn scan_reports_directories_to_depth_and_honours_gitignore() {
    let m = Mind::new();
    let root = tempfile::tempdir().unwrap();
    tree(root.path());
    write(root.path(), ".gitignore", "build/\n");
    write(root.path(), "build/out.md", "# Out\n\nignoredmarker\n");

    let (out, err, ok) = m.run(&["scan", &root.path().display().to_string(), "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["source"], "directory");
    let nodes = v["nodes"].as_array().unwrap();
    let paths: Vec<&str> = nodes.iter().map(|n| n["path"].as_str().unwrap()).collect();
    assert_eq!(paths[0], ".");
    assert!(paths.contains(&"docs"));
    assert!(paths.contains(&"docs/deep"));
    assert!(!paths.contains(&"docs/deep/er"), "depth 2 is the limit");
    assert!(!paths.contains(&"build"), "ignored directory is not listed");
    // README, 3 docs/notes md, .gitignore => 5 files
    assert_eq!(nodes[0]["files"], 5);
}

#[test]
fn directory_ingest_selection_is_scoped_and_ignore_aware() {
    let m = Mind::new();
    let root = tempfile::tempdir().unwrap();
    tree(root.path());
    write(root.path(), ".gitignore", "notes/\n");
    let r = root.path().display().to_string();

    let (_, err, ok) = m.run(&["ingest", &r, "--space", "p", "--path", "docs"]);
    assert!(ok, "{err}");
    assert_eq!(m.search_count("docsmarker"), 1);
    assert_eq!(m.search_count("deepmarker"), 1);
    assert_eq!(m.search_count("rootmarker"), 0, "outside the selection");

    // A later selective ingest of another path must not mark `docs` absent.
    let (_, err, ok) = m.run(&["ingest", &r, "--space", "p", "--path", "README.md"]);
    assert!(ok, "{err}");
    assert_eq!(m.search_count("docsmarker"), 1);

    // Ignore rules apply to a full directory ingest.
    let (_, err, ok) = m.run(&["ingest", &r, "--space", "p"]);
    assert!(ok, "{err}");
    assert_eq!(m.search_count("notesmarker"), 0, ".gitignore excludes notes/");
}

#[test]
fn selection_rejects_paths_escaping_the_root() {
    let m = Mind::new();
    let root = tempfile::tempdir().unwrap();
    tree(root.path());
    let r = root.path().display().to_string();
    for bad in ["../x", "/etc"] {
        let (_, err, ok) = m.run(&["ingest", &r, "--space", "p", "--path", bad]);
        assert!(!ok, "{bad} must be rejected");
        assert!(err.contains("invalid path"), "{err}");
    }
}

#[test]
fn git_scan_and_selection_use_the_committed_tree() {
    let m = Mind::new();
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    tree(repo.path());
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    let r = repo.path().display().to_string();

    let (out, err, ok) = m.run(&["scan", &r, "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["source"], "git");
    assert!(v["commit"].as_str().unwrap().len() >= 40);
    assert_eq!(v["nodes"][0]["files"], 4);

    let (_, err, ok) = m.run(&["ingest", &r, "--space", "p", "--path", "docs"]);
    assert!(ok, "{err}");
    assert_eq!(m.search_count("docsmarker"), 1);
    assert_eq!(m.search_count("notesmarker"), 0, "outside the selection");
}
