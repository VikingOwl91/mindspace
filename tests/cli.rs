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
        self.run_with_stdin(args, "")
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> (String, String, bool) {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new(env!("CARGO_BIN_EXE_mind"))
            .env("MINDSPACE_DB", self.db.path().join("mind.db"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run mind");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait for mind");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        )
    }
}

fn seed_search_fixture(mind: &Mind) {
    let (_, stderr, ok) = mind.run(&["init"]);
    assert!(ok, "init failed: {stderr}");
    for space in ["coding", "personal"] {
        let (_, stderr, ok) = mind.run(&["space", "create", space]);
        assert!(ok, "space create failed: {stderr}");
    }
    for (name, space, content) in [
        (
            "coding.md",
            "coding",
            "# Coding note\n\nMode contract marker for retrieval.\n",
        ),
        (
            "personal.md",
            "personal",
            "# Personal note\n\nMode contract marker for private retrieval.\n",
        ),
    ] {
        let path = mind.db.path().join(name);
        std::fs::write(&path, content).unwrap();
        let path = path.display().to_string();
        let (_, stderr, ok) = mind.run(&["ingest", &path, "--space", space]);
        assert!(ok, "ingest failed: {stderr}");
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

    let (recall_out, stderr, ok) =
        mind.run(&["recall", "no such retrieval marker", "--mode", "auto"]);
    assert!(ok, "auto recall failed: {stderr}");
    assert!(
        recall_out.contains("retrieval: requested auto, effective lexical"),
        "{recall_out}"
    );
    assert!(recall_out.contains("fallback:"), "{recall_out}");
    assert!(recall_out.contains("no results"), "{recall_out}");
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

#[test]
fn explicit_retrieval_modes_preserve_legacy_cli_output_and_filters() {
    let mind = Mind::new();
    seed_search_fixture(&mind);

    let (legacy_out, stderr, ok) = mind.run(&[
        "search",
        "mode contract marker",
        "--space",
        "coding",
        "--limit",
        "1",
        "--json",
    ]);
    assert!(ok, "legacy search failed: {stderr}");
    let legacy: serde_json::Value = serde_json::from_str(&legacy_out).unwrap();
    assert!(legacy.is_array());
    assert_eq!(legacy.as_array().unwrap().len(), 1);

    let (mode_out, stderr, ok) = mind.run(&[
        "search",
        "mode contract marker",
        "--space",
        "coding",
        "--limit",
        "1",
        "--mode",
        "lexical",
        "--json",
    ]);
    assert!(ok, "explicit lexical search failed: {stderr}");
    let mode: serde_json::Value = serde_json::from_str(&mode_out).unwrap();
    assert_eq!(mode["requested"], "lexical");
    assert_eq!(mode["effective"], "lexical");
    assert_eq!(mode["fallback"], serde_json::Value::Null);
    assert_eq!(mode["hits"].as_array().unwrap().len(), 1);
    assert_eq!(mode["hits"][0]["hit"]["memory_id"], legacy[0]["memory_id"]);
    assert_eq!(mode["hits"][0]["hit"]["score"], legacy[0]["score"]);
    assert_eq!(mode["hits"][0]["score_kind"], "lexical");
    assert_eq!(mode["hits"][0]["provenance"]["lexical_rank"], 1);
    assert_eq!(
        mode["hits"][0]["provenance"]["semantic_rank"],
        serde_json::Value::Null
    );
    assert_eq!(mode["lexical_candidates"], 1);
    assert_eq!(mode["semantic_candidates"], 0);

    let (recall_out, stderr, ok) = mind.run(&[
        "recall",
        "mode contract marker",
        "--space",
        "coding",
        "--limit",
        "1",
        "--mode",
        "lexical",
    ]);
    assert!(ok, "explicit lexical recall failed: {stderr}");
    assert!(recall_out.contains("retrieval: requested lexical, effective lexical"));
    assert!(recall_out.contains("answer: Coding note"));
    assert!(recall_out.contains("score: [lexical "));

    let (recall_json, stderr, ok) = mind.run(&[
        "recall",
        "mode contract marker",
        "--space",
        "coding",
        "--limit",
        "1",
        "--mode",
        "lexical",
        "--json",
    ]);
    assert!(ok, "explicit lexical recall JSON failed: {stderr}");
    let recall: serde_json::Value = serde_json::from_str(&recall_json).unwrap();
    assert_eq!(recall["requested"], "lexical");
    assert_eq!(recall["hits"][0]["hit"]["subject"], "Coding note");
}

#[test]
fn explicit_mode_fallback_errors_zero_limit_and_validation_are_visible() {
    let mind = Mind::new();
    seed_search_fixture(&mind);

    let (out, stderr, ok) = mind.run(&[
        "search",
        "no such retrieval marker",
        "--mode",
        "auto",
        "--json",
    ]);
    assert!(ok, "auto search failed: {stderr}");
    let auto: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(auto["requested"], "auto");
    assert_eq!(auto["effective"], "lexical");
    assert_eq!(auto["hits"], serde_json::json!([]));
    assert_eq!(auto["fallback"]["kind"], expected_unavailable_reason());

    let (out, stderr, ok) = mind.run(&["search", "no such retrieval marker", "--mode", "auto"]);
    assert!(ok, "auto human search failed: {stderr}");
    assert!(out.contains("retrieval: requested auto, effective lexical"));
    assert!(out.contains("fallback:"), "{out}");
    assert!(out.contains("no results"), "{out}");

    let (out, stderr, ok) = mind.run(&[
        "search", "anything", "--mode", "lexical", "--limit", "0", "--json",
    ]);
    assert!(ok, "zero-limit explicit lexical failed: {stderr}");
    let not_run: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(not_run["effective"], "not_run");
    assert_eq!(not_run["lexical_candidates"], 0);
    assert_eq!(not_run["hits"], serde_json::json!([]));

    let (out, stderr, ok) = mind.run(&[
        "search", "anything", "--mode", "auto", "--limit", "0", "--json",
    ]);
    assert!(ok, "zero-limit auto failed: {stderr}");
    let not_run: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(not_run["effective"], "not_run");
    assert_eq!(not_run["fallback"], serde_json::Value::Null);
    assert_eq!(not_run["hits"], serde_json::json!([]));

    for mode in ["semantic", "hybrid"] {
        let (out, stderr, ok) = mind.run(&["search", "anything", "--mode", mode, "--limit", "0"]);
        assert!(!ok, "zero-limit {mode} unexpectedly succeeded: {out}");
        assert!(out.is_empty());
        assert!(
            stderr.contains("requested retrieval mode unavailable"),
            "{mode}: {stderr}"
        );
    }

    let (out, stderr, ok) = mind.run(&["search", "", "--mode", "auto"]);
    assert!(!ok);
    assert!(out.is_empty());
    assert!(stderr.contains("empty query"), "{stderr}");

    for mode in ["semantic", "hybrid"] {
        let (out, stderr, ok) = mind.run(&["search", "anything", "--mode", mode]);
        assert!(!ok, "{mode} unexpectedly succeeded: {out}");
        assert!(out.is_empty(), "{mode} printed a partial result: {out}");
        assert!(
            stderr.contains("requested retrieval mode unavailable"),
            "{mode}: {stderr}"
        );
        assert!(
            stderr.contains("mind embeddings enable") || stderr.contains("--features embeddings"),
            "{mode}: {stderr}"
        );
    }

    let (out, stderr, ok) = mind.run(&["search", "anything", "--mode", "semanticish"]);
    assert!(!ok);
    assert!(out.is_empty());
    assert!(stderr.contains("possible values"), "{stderr}");
    assert!(stderr.contains("semantic"), "{stderr}");

    let (out, stderr, ok) = mind.run(&["recall", "anything", "--mode", "LEXICAL"]);
    assert!(!ok);
    assert!(out.is_empty());
    assert!(stderr.contains("possible values"), "{stderr}");

    #[cfg(feature = "embeddings")]
    {
        let store = mindspace::SqliteStore::open(&mind.db.path().join("mind.db")).unwrap();
        mindspace::embeddings::manager::set_enabled(&store, true).unwrap();
        let (out, stderr, ok) = mind.run(&["search", "anything", "--mode", "semantic"]);
        assert!(!ok);
        assert!(out.is_empty());
        assert!(
            stderr.contains("no embedding model is selected"),
            "{stderr}"
        );
        assert!(stderr.contains("mind model install <id>"), "{stderr}");
        assert!(stderr.contains("mind model use <id>"), "{stderr}");
    }
}

fn expected_unavailable_reason() -> &'static str {
    #[cfg(feature = "embeddings")]
    {
        "embeddings_disabled"
    }
    #[cfg(not(feature = "embeddings"))]
    {
        "feature_disabled"
    }
}

fn index_run_with_isolated_home(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mind"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("TMPDIR", root)
        .arg("--db")
        .arg(root.join("mind.db"))
        .args(["index", "run"])
        .output()
        .expect("run isolated index command")
}

#[test]
fn index_run_preserves_legitimate_settings_outcomes() {
    use mindspace::storage::Store;

    for (enabled, model, expected) in [
        (None, None, "embeddings disabled"),
        (
            Some("TRUE"),
            Some("synthetic-unknown"),
            "embeddings disabled",
        ),
        (
            Some("false"),
            Some("synthetic-unknown"),
            "embeddings disabled",
        ),
        (Some("true"), None, "no model selected"),
        (Some("true"), Some(""), "unknown model:"),
        (
            Some("true"),
            Some("synthetic-unknown"),
            "unknown model: synthetic-unknown",
        ),
        (
            Some("true"),
            Some(mindspace::embeddings::manifest::NOMIC_V1_5.id),
            "not installed",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = mindspace::SqliteStore::open(&dir.path().join("mind.db")).unwrap();
        if let Some(value) = enabled {
            store.set_meta("embedding.enabled", value).unwrap();
        }
        if let Some(value) = model {
            store.set_meta("embedding.model", value).unwrap();
        }
        drop(store);
        let output = index_run_with_isolated_home(dir.path());
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "expected {expected:?}, got {stderr}"
        );
    }
}

#[test]
fn index_run_propagates_settings_storage_errors_instead_of_disabled() {
    use mindspace::storage::Store;

    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("mind.db");
    let store = mindspace::SqliteStore::open(&database).unwrap();
    store.set_meta("embedding.enabled", "false").unwrap();
    store
        .set_meta("embedding.model", "SYNTHETIC_MODEL")
        .unwrap();
    drop(store);
    // Deliberately malformed disposable schema: constructor inventory still
    // exists, while decoding the required legacy settings query must fail.
    let raw = rusqlite::Connection::open(&database).unwrap();
    raw.execute_batch("ALTER TABLE mind_meta RENAME COLUMN value TO synthetic_unavailable")
        .unwrap();
    drop(raw);

    let output = index_run_with_isolated_home(dir.path());
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no such column"), "{stderr}");
    assert!(stderr.contains("value"), "{stderr}");
    assert!(!stderr.contains("embeddings disabled"), "{stderr}");
    assert!(!stderr.contains("no model selected"), "{stderr}");
}

#[test]
fn untrusted_text_is_terminal_safe_and_json_is_lossless() {
    let mind = Mind::new();
    assert!(mind.run(&["init"]).2);
    let controls = "\u{1b}]0;title\u{7}\u{1b}[2J\u{1b}[31m\u{9b}31m\r\u{8}\u{7f}\u{202e}\u{2066}";
    let heading_controls = controls.replace('\r', "");
    let subject = format!("EscapeToken Grüße 日本語 🦉 {heading_controls}");
    let space = format!("safe{controls}");
    assert!(mind.run(&["space", "create", &space]).2);
    let path = mind.db.path().join(format!("note{controls}.md"));
    std::fs::write(
        &path,
        format!("# {subject}\n\nEscapeToken body {controls}\nSecond line.\n"),
    )
    .unwrap();
    let path = path.to_str().unwrap();
    let (out, err, ok) = mind.run(&["ingest", path, "--space", &space]);
    assert!(ok, "{err}");
    assert_terminal_safe(&out);
    assert!(out.contains("Grüße 日本語 🦉"));
    for command in ["search", "recall"] {
        for mode in [None, Some("lexical"), Some("auto")] {
            let mut args = vec![command, "EscapeToken", "--space", &space];
            if let Some(mode) = mode {
                args.extend(["--mode", mode]);
            }
            let (out, err, ok) = mind.run(&args);
            assert!(ok, "{err}");
            assert_terminal_safe(&out);
            assert!(out.contains("Grüße 日本語 🦉"));
            args.push("--json");
            let (out, err, ok) = mind.run(&args);
            assert!(ok, "{err}");
            let json: serde_json::Value = serde_json::from_str(&out).unwrap();
            let hit = if mode.is_some() {
                &json["hits"][0]["hit"]
            } else {
                &json[0]
            };
            assert_eq!(hit["subject"], subject);
            assert!(hit["content"].as_str().unwrap().contains('\u{1b}'));
            assert!(hit["source_location"].as_str().unwrap().contains(controls));
            assert_eq!(hit["spaces"][0]["name"], space);
        }
    }
    // JSON ingest reports retain raw data too.
    let (out, err, ok) = mind.run(&["ingest", path, "--space", &space, "--json"]);
    assert!(ok, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(report["root"].as_str().unwrap().contains(controls));
}

fn assert_terminal_safe(output: &str) {
    assert!(
        !output
            .chars()
            .any(|c| (c.is_control() && c != '\n' && c != '\t')
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)),
        "unsafe output: {output:?}"
    );
}

#[cfg(unix)]
#[test]
fn public_help_hides_internal_ownership_but_explicit_admin_help_works() {
    let mind = Mind::new();
    for args in [vec!["--help"], vec!["help"], vec!["help", "help"]] {
        let (out, err, ok) = mind.run(&args);
        assert!(ok, "{err}");
        assert!(!out.contains("ownership"), "{out}");
        assert!(!out.contains("stage-document"), "{out}");
    }
    let (out, err, ok) = mind.run(&["ownership", "--help"]);
    assert!(ok, "{err}");
    assert!(out.contains("stage-document-lexical"));
}

#[test]
fn git_ingest_output_sanitizes_untrusted_paths_and_subjects() {
    let mind = Mind::new();
    assert!(mind.run(&["init"]).2);
    assert!(mind.run(&["space", "create", "coding"]).2);
    let repo = mind.db.path().join("repo\u{1b}]0;git\u{7}");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-q"]);
    let name = "GitToken\u{1b}[2J.md";
    std::fs::write(
        repo.join(name),
        "# GitToken Grüße 🦉 \u{1b}]0;title\u{7}\n\nGitToken \u{9b}31m body.\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "fixture",
    ]);
    git(&["branch", "branch\u{202e}"]);
    let (out, err, ok) = mind.run(&["ingest", repo.to_str().unwrap(), "--space", "coding"]);
    assert!(ok, "{err}");
    assert_terminal_safe(&out);
    assert!(out.contains("Grüße 🦉"));
    for command in ["search", "recall"] {
        let (out, err, ok) = mind.run(&[command, "GitToken", "--space", "coding"]);
        assert!(ok, "{err}");
        assert_terminal_safe(&out);
        assert!(out.contains("Grüße 🦉"));
    }
}

#[test]
fn manual_controls_related_hits_and_inherited_spaces_are_safe_but_json_is_exact() {
    use mindspace::storage::Store;
    let mind = Mind::new();
    assert!(mind.run(&["init"]).2);
    let store = mindspace::SqliteStore::open(&mind.db.path().join("mind.db")).unwrap();
    let controls = "\u{1b}]52;c;AAAA\u{7}\u{1b}[2J\u{9b}\u{8}\r\t\u{7f}\u{202e}\u{2069}";
    let source = store
        .create_space(&format!("source{controls}"), "")
        .unwrap();
    let via = store.create_space(&format!("via{controls}"), "").unwrap();
    store.link_spaces(via.id, source.id).unwrap();
    let subject = format!("ManualToken e\u{301} 日本語 🦉 {controls}");
    for suffix in ["one", "two"] {
        store
            .record_manual_write(mindspace::core::ManualMemoryDraft {
                subject: Some(subject.clone()),
                content: format!("ManualToken body {controls}\n{suffix}"),
                spaces: vec![source.id],
                supersedes: None,
            })
            .unwrap();
    }
    drop(store);
    for command in ["search", "recall"] {
        for mode in [None, Some("lexical"), Some("auto")] {
            let mut args = vec![command, "ManualToken", "--space", &via.name];
            if let Some(mode) = mode {
                args.extend(["--mode", mode]);
            }
            let (out, err, ok) = mind.run(&args);
            assert!(ok, "{err}");
            assert_terminal_safe(&out);
            assert!(out.contains("e\u{301} 日本語 🦉"));
            assert!(out.contains("(via source"));
            if command == "recall" {
                assert!(out.contains("related:"));
            }
            args.push("--json");
            let (out, err, ok) = mind.run(&args);
            assert!(ok, "{err}");
            let result: serde_json::Value = serde_json::from_str(&out).unwrap();
            let hits = if mode.is_some() {
                &result["hits"]
            } else {
                &result
            };
            assert_eq!(hits.as_array().unwrap().len(), 2);
            for value in hits.as_array().unwrap() {
                let hit = if mode.is_some() { &value["hit"] } else { value };
                assert_eq!(hit["subject"], subject);
                assert!(hit["content"].as_str().unwrap().contains(controls));
                assert_eq!(hit["spaces"][0]["via"], source.name);
                assert_eq!(hit["spaces"][0]["name"], source.name);
            }
        }
    }
}

#[test]
fn ingest_error_from_an_untrusted_filename_is_terminal_safe() {
    let mind = Mind::new();
    assert!(mind.run(&["init"]).2);
    assert!(mind.run(&["space", "create", "coding"]).2);
    let directory = mind.db.path().join("untrusted");
    std::fs::create_dir(&directory).unwrap();
    let filename = "Grüße 日本語 🦉 \u{1b}]0;error\u{7}\u{202e}.txt";
    std::fs::write(directory.join(filename), [0xff, 0xfe]).unwrap();
    for json in [false, true] {
        let mut args = vec!["ingest", directory.to_str().unwrap(), "--space", "coding"];
        if json {
            args.push("--json");
        }
        let (out, err, ok) = mind.run(&args);
        assert!(!ok);
        assert!(out.is_empty());
        assert_terminal_safe(&err);
        assert!(err.contains("Grüße 日本語 🦉"));
        assert!(err.contains("not valid UTF-8"));
    }
}

fn memory_fixture() -> Mind {
    let mind = Mind::new();
    for args in [
        &["init"][..],
        &["space", "create", "coding"][..],
        &["space", "create", "personal"][..],
    ] {
        let (_, stderr, ok) = mind.run(args);
        assert!(ok, "{args:?} failed: {stderr}");
    }
    mind
}

#[test]
fn memory_create_stores_an_explicit_memory_that_search_finds() {
    let mind = memory_fixture();
    let (out, err, ok) = mind.run(&[
        "memory",
        "create",
        "Use a bounded cache for the parser.",
        "--space",
        "coding",
    ]);
    assert!(ok, "memory create failed: {err}");
    assert!(out.contains("created"), "{out}");
    let (out, _, ok) = mind.run(&["search", "bounded", "--space", "coding"]);
    assert!(ok);
    assert!(out.contains("bounded cache"), "{out}");
    let (out, _, _) = mind.run(&["search", "bounded", "--space", "personal"]);
    assert!(!out.contains("bounded cache"), "wrong space leaked: {out}");
}

#[test]
fn memory_create_derives_the_subject_from_the_first_line_unless_given() {
    let mind = memory_fixture();
    let (out, err, ok) = mind.run(&[
        "memory",
        "create",
        "Cache policy\nBounded.",
        "--space",
        "coding",
        "--json",
    ]);
    assert!(ok, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).expect("json report");
    assert_eq!(report["subject"], "Cache policy");
    assert!(report["memory_id"].is_string());
    let (out, err, ok) = mind.run(&[
        "memory",
        "create",
        "Other body",
        "--space",
        "coding",
        "--subject",
        "Explicit",
        "--json",
    ]);
    assert!(ok, "{err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["subject"], "Explicit");
}

#[test]
fn memory_create_is_idempotent_and_adds_new_spaces() {
    let mind = memory_fixture();
    let args = |space| ["memory", "create", "Same note", "--space", space, "--json"];
    let (first, err, ok) = mind.run(&args("coding"));
    assert!(ok, "{err}");
    let (second, err, ok) = mind.run(&args("coding"));
    assert!(ok, "{err}");
    let (third, err, ok) = mind.run(&args("personal"));
    assert!(ok, "{err}");
    let id = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap()["memory_id"].clone();
    assert_eq!(id(&first), id(&second), "repeat created a second memory");
    assert_eq!(id(&first), id(&third), "new space created a second memory");
    let spaces = serde_json::from_str::<serde_json::Value>(&third).unwrap()["spaces"].clone();
    assert!(spaces.to_string().contains("personal"), "{spaces}");
}

#[test]
fn memory_create_reads_the_content_from_stdin_with_a_dash() {
    let mind = memory_fixture();
    let (_, err, ok) = mind.run_with_stdin(
        &["memory", "create", "-", "--space", "coding"],
        "Piped decision\nWe pipe notes in.\n",
    );
    assert!(ok, "{err}");
    let (out, _, _) = mind.run(&["search", "pipe", "--space", "coding"]);
    assert!(out.contains("Piped decision"), "{out}");
}

#[test]
fn memory_create_refuses_unknown_spaces_empty_content_and_a_missing_space() {
    let mind = memory_fixture();
    let (_, err, ok) = mind.run(&["memory", "create", "x", "--space", "nope"]);
    assert!(!ok && !err.is_empty(), "unknown space accepted");
    let (_, err, ok) = mind.run(&["memory", "create", "   ", "--space", "coding"]);
    assert!(!ok && !err.is_empty(), "blank content accepted");
    let (_, err, ok) = mind.run(&["memory", "create", "x"]);
    assert!(!ok && err.contains("--space"), "{err}");
}
