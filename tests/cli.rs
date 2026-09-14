//! End-to-end tests: run the compiled `mind` binary exactly as a user
//! would, with an isolated `MINDSPACE_DB`.

use std::process::Command;

struct Mind {
    db: tempfile::TempDir,
}

impl Mind {
    fn new() -> Self {
        Self {
            db: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn run(&self, args: &[&str]) -> (String, String, bool) {
        let out = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", self.db.path().join("mind.db"))
            .args(args)
            .output()
            .expect("run mind");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }
}

fn example_path() -> String {
    // CARGO_MANIFEST_DIR is the crate root; the example lives in examples/.
    format!("{}/examples/example.md", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn acceptance_flow_from_clean_clone() {
    let mind = Mind::new();

    let (out, _, ok) = mind.run(&["init"]);
    assert!(ok, "init failed");
    assert!(out.contains("initialized"));

    // init is idempotent.
    let (_, _, ok) = mind.run(&["init"]);
    assert!(ok);

    for space in ["linux", "coding", "project:owlry"] {
        let (_, err, ok) = mind.run(&["space", "create", space]);
        assert!(ok, "space create {space} failed: {err}");
    }
    // Duplicate space fails with an actionable error.
    let (_, err, ok) = mind.run(&["space", "create", "linux"]);
    assert!(!ok);
    assert!(err.contains("already exists"), "{err}");

    let (_, err, ok) = mind.run(&["space", "link", "project:owlry", "coding"]);
    assert!(ok, "link failed: {err}");
    let (_, err, ok) = mind.run(&["space", "link", "coding", "linux"]);
    assert!(ok, "link failed: {err}");
    // Self link and duplicate link fail.
    let (_, _, ok) = mind.run(&["space", "link", "coding", "coding"]);
    assert!(!ok);
    let (_, _, ok) = mind.run(&["space", "link", "project:owlry", "coding"]);
    assert!(!ok);

    let example = example_path();
    let (_, err, ok) = mind.run(&["ingest", &example, "--space", "project:owlry"]);
    assert!(ok, "ingest failed: {err}");
    // Idempotent re-ingest.
    let (out, _, ok) = mind.run(&["ingest", &example, "--space", "project:owlry"]);
    assert!(ok);
    assert!(out.contains("unchanged"), "{out}");
    assert!(out.contains("logical"), "{out}");

    // Search finds a section directly and explains the space.
    let (out, _, ok) = mind.run(&["search", "SQLite", "--space", "project:owlry", "--json"]);
    assert!(ok, "search failed");
    assert!(out.contains("\"Storage\""), "{out}");
    assert!(out.contains("project:owlry"), "{out}");

    // Recall presents the highest-ranked current memory as an answer while
    // preserving the same provenance users would see in search.
    let (out, _, ok) = mind.run(&["recall", "SQLite", "--space", "project:owlry"]);
    assert!(ok, "recall failed");
    assert!(out.contains("answer: Storage"), "{out}");
    assert!(out.contains("single SQLite database file"), "{out}");
    assert!(out.contains("source:"), "{out}");
    assert!(out.contains("memory:"), "{out}");
    let (out, _, ok) = mind.run(&["recall", "SQLite", "--space", "project:owlry", "--json"]);
    assert!(ok, "recall json failed");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(json.is_array());
    assert_eq!(json[0]["subject"], "Storage");

    // Traversal: a memory ingested into linux is visible from project:owlry
    // through the directed links, with the path recorded.
    let kernel = mind.db.path().join("kernel.md");
    std::fs::write(&kernel, "# Kernel\n\nThe scheduler is preemptible.\n").unwrap();
    let kernel = kernel.display().to_string();
    let (_, err, ok) = mind.run(&["ingest", &kernel, "--space", "linux"]);
    assert!(ok, "ingest failed: {err}");
    let (out, _, ok) = mind.run(&["search", "preemptible", "--space", "project:owlry"]);
    assert!(ok);
    assert!(out.contains("via coding -> linux"), "{out}");
    let (out, _, ok) = mind.run(&["recall", "preemptible", "--space", "project:owlry"]);
    assert!(ok, "recall via imported space failed");
    assert!(out.contains("via coding -> linux"), "{out}");

    // Recall defaults to a compact answer plus related evidence, while an
    // explicit limit can restrict it to the answer alone.
    for name in ["recall-one.md", "recall-two.md"] {
        let path = mind.db.path().join(name);
        std::fs::write(&path, "# Recall\n\nDurable recall marker.\n").unwrap();
        let path = path.display().to_string();
        let (_, err, ok) = mind.run(&["ingest", &path, "--space", "linux"]);
        assert!(ok, "ingest failed: {err}");
    }
    let (out, _, ok) = mind.run(&[
        "recall",
        "durable recall marker",
        "--space",
        "project:owlry",
    ]);
    assert!(ok, "recall related results failed");
    assert!(out.contains("related:"), "{out}");
    assert!(out.contains("via coding -> linux"), "{out}");
    let (out, _, ok) = mind.run(&[
        "recall",
        "durable recall marker",
        "--space",
        "project:owlry",
        "--limit",
        "1",
    ]);
    assert!(ok, "limited recall failed");
    assert!(!out.contains("related:"), "{out}");

    // Trace the provenance chain of the first search hit.
    let (out, _, ok) = mind.run(&["search", "SQLite", "--space", "project:owlry", "--json"]);
    assert!(ok);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    let memory_id = json[0]["memory_id"].as_str().unwrap().to_string();
    let (out, _, ok) = mind.run(&["trace", &memory_id]);
    assert!(ok, "trace failed");
    assert!(out.contains("memory "), "{out}");
    assert!(out.contains("assertion "), "{out}");
    assert!(out.contains("evidence "), "{out}");
    assert!(out.contains("source "), "{out}");
    assert!(out.contains("sha256:"), "{out}");

    // Inspect shows the supersession hop after the file changes.
    std::fs::write(
        &kernel,
        "# Kernel\n\nThe scheduler now uses EEVDF for fair scheduling.\n",
    )
    .unwrap();
    let (_, _, ok) = mind.run(&["ingest", &kernel, "--space", "linux"]);
    assert!(ok);
    let (out, _, ok) = mind.run(&["search", "preemptible", "--include-superseded", "--json"]);
    assert!(ok);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    let superseded_id = json[0]["memory_id"].as_str().unwrap().to_string();
    let (out, _, ok) = mind.run(&["inspect", &superseded_id]);
    assert!(ok);
    assert!(out.contains("superseded"), "{out}");

    // History lists both generations of the subject.
    let (out, _, ok) = mind.run(&["history", "kernel"]);
    assert!(ok);
    assert!(out.contains("2 memories"), "{out}");

    // Conflicts: same subject, same space, two sources.
    let other = mind.db.path().join("other.md");
    std::fs::write(&other, "# Kernel\n\nThe scheduler is O(1) based.\n").unwrap();
    let other = other.display().to_string();
    let (_, _, ok) = mind.run(&["ingest", &other, "--space", "linux"]);
    assert!(ok);
    let (out, _, ok) = mind.run(&["conflicts"]);
    assert!(ok);
    assert!(out.contains("linux / \"kernel\""), "{out}");

    // Retire, then verify it disappears from default search but stays
    // inspectable.
    let (out, _, ok) = mind.run(&["search", "O(1)", "--json"]);
    assert!(ok);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    let retire_id = json[0]["memory_id"].as_str().unwrap().to_string();
    let (out, _, ok) = mind.run(&["retire", &retire_id, "--reason", "wrong claim"]);
    assert!(ok, "retire failed: {out}");
    assert!(out.contains("retired"), "{out}");
    let (out, _, ok) = mind.run(&["search", "O(1)"]);
    assert!(ok);
    assert!(out.contains("no results"), "{out}");
    let (out, _, ok) = mind.run(&["recall", "absent-term"]);
    assert!(ok);
    assert_eq!(out.trim(), "no results");
    let (out, _, ok) = mind.run(&["inspect", &retire_id]);
    assert!(ok);
    assert!(out.contains("wrong claim"), "{out}");
}

#[test]
fn commands_fail_actionably() {
    let mind = Mind::new();

    // Any command but init on an uninitialized database fails with a hint.
    let (_, err, ok) = mind.run(&["space", "list"]);
    assert!(!ok);
    assert!(err.contains("mind init"), "{err}");

    let (_, _, ok) = mind.run(&["init"]);
    assert!(ok);

    // Ingest of an unsupported format fails.
    let bad = mind.db.path().join("bad.bin");
    std::fs::write(&bad, "not source").unwrap();
    let bad = bad.display().to_string();
    let (_, err, ok) = mind.run(&["ingest", &bad, "--space", "coding"]);
    assert!(!ok);
    assert!(err.contains("unsupported file format"), "{err}");
    assert!(err.contains(".md"), "{err}");

    // Ingest into a missing space names the space and the fix.
    let (_, err, ok) = mind.run(&["ingest", &example_path(), "--space", "nope"]);
    assert!(!ok);
    assert!(err.contains("'nope'"), "{err}");
    assert!(err.contains("mind space create nope"), "{err}");

    // Search for a missing space fails the same way.
    let (_, err, ok) = mind.run(&["search", "x", "--space", "nope"]);
    assert!(!ok);
    assert!(err.contains("mind space create nope"), "{err}");

    let (_, err, ok) = mind.run(&["recall", "x", "--space", "nope"]);
    assert!(!ok);
    assert!(err.contains("mind space create nope"), "{err}");

    let (_, err, ok) = mind.run(&["recall", ""]);
    assert!(!ok);
    assert!(err.contains("empty query"), "{err}");

    // Unknown memory id prefix fails with the id hint.
    let (_, err, ok) = mind.run(&["inspect", "zzzz"]);
    assert!(!ok);
    assert!(err.contains("no memory matches"), "{err}");

    // Empty space name fails.
    let (_, err, ok) = mind.run(&["space", "create", "   "]);
    assert!(!ok);
    assert!(err.contains("empty"), "{err}");
}
