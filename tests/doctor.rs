//! `mind doctor` and the shared path resolution (M7.1).
//!
//! Everything runs the real binary with a CLEARED environment (only the
//! variables a test names), so the developer's own HOME, XDG_DATA_HOME,
//! MINDSPACE_* and PATH can neither help nor hurt. Nothing here touches the
//! network or a real database.
//!
//! "Landlock unavailable" is emulated the way tests/sandbox_failure_modes.rs
//! does it: a seccomp filter makes the three Landlock system calls fail with
//! `ENOSYS` / `EOPNOTSUPP`; the filter is inherited by the child server that
//! the doctor starts for its self-test. (The filter builder below is a copy of
//! that file's; the two should be merged into a shared helper one day.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

mod common;
use sha2::{Digest, Sha256};

const MIND: &str = env!("CARGO_BIN_EXE_mind");
/// Enough PATH for `git`; nothing from the developer's environment.
const SAFE_PATH: &str = "/usr/bin:/bin";

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A command with a cleared environment.
fn mind(args: &[&str]) -> Command {
    let mut c = Command::new(MIND);
    c.env_clear().env("PATH", SAFE_PATH).args(args);
    c
}

fn run(mut c: Command) -> Output {
    c.stdin(Stdio::null()).output().unwrap()
}

fn doctor_json(c: Command) -> (i32, Value, String) {
    let o = run(c);
    let stdout = String::from_utf8(o.stdout).unwrap();
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not exactly one JSON document ({e}):\n{stdout}"));
    (
        o.status.code().unwrap(),
        v,
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no check `{id}` in {report}"))
}

fn initialized(xdg: &Path) -> PathBuf {
    let o = run({
        let mut c = mind(&["init"]);
        c.env("XDG_DATA_HOME", xdg);
        c
    });
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    xdg.join("mindspace/mind.db")
}

fn doctor_with_xdg(xdg: &Path, extra: &[&str]) -> (i32, Value, String) {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(extra);
    let mut c = mind(&args);
    c.env("XDG_DATA_HOME", xdg);
    doctor_json(c)
}

/// name -> sha256 for every file in `dir` (non-recursive; no directories).
fn files(dir: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                m.insert(
                    e.file_name().to_string_lossy().into_owned(),
                    Sha256::digest(std::fs::read(&p).unwrap())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>(),
                );
            }
        }
    }
    m
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

// ------------------------------------------------------------------ paths

#[test]
fn the_database_path_priority_is_flag_then_environment_then_default() {
    let t = tmp();
    let (flag, env, xdg) = (
        t.path().join("flag.db"),
        t.path().join("env.db"),
        t.path().join("xdg"),
    );

    // Flag and environment both given: the flag wins.
    let mut c = mind(&["--db", flag.to_str().unwrap(), "doctor", "--json"]);
    c.env("MINDSPACE_DB", &env).env("XDG_DATA_HOME", &xdg);
    let (_, r, _) = doctor_json(c);
    let db = &check(&r, "database")["details"];
    assert_eq!(db["path"], flag.to_str().unwrap());
    assert_eq!(db["source"], "--db");

    // The flag may follow the subcommand (it is global).
    let mut c = mind(&["doctor", "--json", "--db", flag.to_str().unwrap()]);
    c.env("MINDSPACE_DB", &env);
    let (_, r, _) = doctor_json(c);
    assert_eq!(check(&r, "database")["details"]["source"], "--db");

    // Environment only.
    let mut c = mind(&["doctor", "--json"]);
    c.env("MINDSPACE_DB", &env).env("XDG_DATA_HOME", &xdg);
    let (_, r, _) = doctor_json(c);
    let db = &check(&r, "database")["details"];
    assert_eq!(db["path"], env.to_str().unwrap());
    assert_eq!(db["source"], "environment variable MINDSPACE_DB");
    assert_eq!(db["default"], false);

    // Neither: the default under XDG_DATA_HOME.
    let (_, r, _) = doctor_with_xdg(&xdg, &[]);
    let db = &check(&r, "database")["details"];
    assert_eq!(db["path"], xdg.join("mindspace/mind.db").to_str().unwrap());
    assert_eq!(db["source"], "environment variable XDG_DATA_HOME");
    assert_eq!(db["default"], true);
}

#[test]
fn the_model_directory_does_not_follow_the_database_path() {
    let t = tmp();
    let (xdg, elsewhere) = (t.path().join("xdg"), t.path().join("elsewhere.db"));
    let mut c = mind(&["--db", elsewhere.to_str().unwrap(), "doctor", "--json"]);
    c.env("XDG_DATA_HOME", &xdg);
    let (_, r, _) = doctor_json(c);
    let cfg = &check(&r, "config")["details"];
    assert_eq!(cfg["database"]["path"], elsewhere.to_str().unwrap());
    assert_eq!(
        cfg["data_dir"]["models"],
        xdg.join("mindspace/models").to_str().unwrap()
    );
    assert_eq!(cfg["model_directory_follows_database"], false);
}

#[test]
fn without_home_and_xdg_nothing_is_written_to_the_working_directory() {
    let t = tmp();
    let cwd = t.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();

    // `init` refuses instead of creating ./mind.db.
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

    // The doctor reports it, with exit code 1, and still writes nothing there.
    let mut c = mind(&["doctor", "--json"]);
    c.current_dir(&cwd);
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 1);
    assert_eq!(check(&r, "database")["status"], "fail");
    assert_eq!(check(&r, "config")["status"], "fail");
    assert!(names(&cwd).is_empty(), "files appeared: {:?}", names(&cwd));

    // An explicit database path works without HOME; only the models cannot be located.
    let db = t.path().join("only.db");
    let o = run({
        let mut c = mind(&["--db", db.to_str().unwrap(), "init"]);
        c.current_dir(&cwd);
        c
    });
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut c = mind(&["--db", db.to_str().unwrap(), "doctor", "--json"]);
    c.current_dir(&cwd);
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 0, "{r}");
    assert_eq!(check(&r, "database")["status"], "pass");
    assert_eq!(check(&r, "config")["status"], "warn");
    assert!(check(&r, "config")["summary"]
        .as_str()
        .unwrap()
        .contains("models cannot be located"));
    assert!(names(&cwd).is_empty());
}

#[test]
fn a_relative_xdg_value_is_ignored_and_said_so() {
    let t = tmp();
    let home = t.path().join("home");
    let mut c = mind(&["doctor", "--json"]);
    c.env("XDG_DATA_HOME", "relative/dir").env("HOME", &home);
    let (_, r, _) = doctor_json(c);
    let db = &check(&r, "database")["details"];
    assert_eq!(
        db["path"],
        home.join(".local/share/mindspace/mind.db")
            .to_str()
            .unwrap()
    );
    assert_eq!(db["source"], "environment variable HOME");
    let cfg = check(&r, "config");
    assert_eq!(cfg["status"], "warn");
    assert!(cfg["summary"]
        .as_str()
        .unwrap()
        .contains("XDG_DATA_HOME=relative/dir"));
}

// -------------------------------------------------------------- database

#[test]
fn a_fresh_uninitialized_database_is_a_blocking_failure_and_stays_absent() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    let db = check(&r, "database");
    assert_eq!(db["status"], "fail");
    assert_eq!(db["remedy"], "run: mind init");
    assert!(
        !xdg.exists(),
        "the doctor must not create the data directory"
    );
    // Everything that does not depend on the database is still reported.
    assert_eq!(check(&r, "embeddings")["status"], "skip");
}

#[test]
fn a_valid_database_passes_and_the_report_is_pure_json() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    initialized(&xdg);
    let (code, r, stderr) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(stderr, "", "nothing but the JSON document may be printed");
    assert_eq!(r["format"], "mind-doctor/1");
    assert_eq!(r["summary"]["exit_code"], 0);
    let db = check(&r, "database");
    assert_eq!(db["status"], "pass");
    assert_eq!(
        db["details"]["schema_version"],
        db["details"]["supported_schema_version"]
    );
    let ids: Vec<&str> = r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "binary",
            "database",
            "config",
            "git",
            "embeddings",
            "sandbox"
        ]
    );
    for c in r["checks"].as_array().unwrap() {
        assert!(["pass", "warn", "fail", "skip"].contains(&c["status"].as_str().unwrap()));
    }
}

#[test]
fn the_text_report_names_every_check_and_the_exit_code() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    initialized(&xdg);
    let mut c = mind(&["doctor"]);
    c.env("XDG_DATA_HOME", &xdg);
    let o = run(c);
    let text = String::from_utf8(o.stdout).unwrap();
    for id in [
        "binary",
        "database",
        "config",
        "git",
        "embeddings",
        "sandbox",
    ] {
        assert!(text.contains(id), "{text}");
    }
    assert!(
        text.contains("Summary:") && text.contains("(exit code 0)"),
        "{text}"
    );
}

fn rusqlite_edit(db: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(sql).unwrap();
}

#[test]
fn an_incompatible_schema_is_reported_without_touching_the_file() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let db = initialized(&xdg);

    // Newer than this binary supports: blocking.
    rusqlite_edit(
        &db,
        "INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)",
    );
    let before = files(db.parent().unwrap());
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    let c = check(&r, "database");
    assert_eq!(c["status"], "fail");
    assert!(c["summary"]
        .as_str()
        .unwrap()
        .contains("newer than this binary supports"));
    assert_eq!(
        files(db.parent().unwrap()),
        before,
        "the doctor changed the database directory"
    );

    // A gap in the history: blocking.
    rusqlite_edit(&db, "DELETE FROM schema_migrations WHERE version = 99; DELETE FROM schema_migrations WHERE version = 5");
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    assert!(check(&r, "database")["summary"]
        .as_str()
        .unwrap()
        .contains("contiguous"));

    // Older than this binary: a warning; the doctor must NOT migrate it.
    // The tables of migrations 9, 10 and 11 are removed as well, so that a migrating open
    // WOULD change the file (re-creating them): the unchanged hashes below prove
    // the doctor did not migrate.
    rusqlite_edit(
        &db,
        "INSERT INTO schema_migrations (version, applied_at) VALUES (5, 0);
         DELETE FROM schema_migrations WHERE version IN (9, 10, 11, 12, 13);
         DROP TABLE doc_chunks; DROP TABLE doc_chunk_sets; DROP TABLE doc_nodes; DROP TABLE doc_extractions; DROP TABLE doc_blobs; DROP INDEX uq_logical_documents_location;
         DROP TABLE git_views; DROP TABLE repository_remotes; DROP TABLE scope_spaces; DROP TABLE scopes;",
    );
    let before = files(db.parent().unwrap());
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0);
    let c = check(&r, "database");
    assert_eq!(c["status"], "warn");
    assert_eq!(c["details"]["schema_version"], 8);
    assert!(c["summary"].as_str().unwrap().contains("will migrate"));
    assert_eq!(
        files(db.parent().unwrap()),
        before,
        "the doctor migrated the database"
    );
}

#[test]
fn files_that_are_not_mindspace_databases_are_rejected_with_the_right_advice() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let dir = xdg.join("mindspace");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("mind.db");

    // Empty file: initialization is the fix.
    std::fs::write(&db, b"").unwrap();
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    assert_eq!(check(&r, "database")["remedy"], "run: mind init");

    // Some other SQLite database: `mind init` would write into it, so it is not advised.
    std::fs::remove_file(&db).unwrap();
    rusqlite_edit(&db, "CREATE TABLE other (x)");
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    let c = check(&r, "database");
    assert!(c["summary"]
        .as_str()
        .unwrap()
        .contains("not a Mindspace database"));
    assert!(!c["remedy"].as_str().unwrap().starts_with("run: mind init"));

    // Not SQLite at all.
    std::fs::write(
        &db,
        b"this is not a database, just text that is long enough",
    )
    .unwrap();
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 1);
    assert_eq!(check(&r, "database")["status"], "fail");
}

#[test]
fn the_doctor_does_not_write_to_the_database_directory_idle_or_with_a_live_wal() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let db = initialized(&xdg);
    let dir = db.parent().unwrap().to_path_buf();

    // Idle: no -wal/-shm file may appear (a plain read-only open would leave both).
    let before = names(&dir);
    let before_hashes = files(&dir);
    let (code, _, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0);
    assert_eq!(names(&dir), before, "files appeared or vanished");
    assert_eq!(files(&dir), before_hashes, "file contents changed");

    // With a writer holding the WAL open: the database file and the log are untouched.
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer
        .execute_batch("CREATE TABLE zz_probe (x); INSERT INTO zz_probe VALUES (1);")
        .unwrap();
    let before = files(&dir);
    let names_before = names(&dir);
    assert!(
        names_before.iter().any(|n| n.ends_with("-wal")),
        "{names_before:?}"
    );
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        check(&r, "database")["details"]["write_ahead_log_present"],
        true
    );
    let after = files(&dir);
    for key in ["mind.db", "mind.db-wal"] {
        assert_eq!(before.get(key), after.get(key), "{key} changed");
    }
    assert_eq!(names(&dir), names_before);
    drop(writer);
}

// ------------------------------------------------------------ embeddings

#[test]
fn database_paths_with_special_characters_relative_paths_and_directories() {
    let t = tmp();
    // URI-special characters in the path must not change which file is opened.
    let odd = t.path().join("sp ace?q#h%25x");
    std::fs::create_dir_all(&odd).unwrap();
    let db = odd.join("mind.db");
    assert!(run(mind(&["--db", db.to_str().unwrap(), "init"]))
        .status
        .success());
    let (code, r, _) = doctor_json(mind(&["--db", db.to_str().unwrap(), "doctor", "--json"]));
    assert_eq!(code, 0, "{r}");
    assert_eq!(check(&r, "database")["status"], "pass");

    // A relative path is allowed (explicit) and called out.
    let mut c = mind(&["--db", "rel.db", "doctor", "--json"]);
    c.current_dir(t.path());
    let (_, r, _) = doctor_json(c);
    assert_eq!(
        check(&r, "database")["status"],
        "fail",
        "not initialized yet"
    );
    assert_eq!(check(&r, "config")["status"], "warn");
    assert!(check(&r, "config")["summary"]
        .as_str()
        .unwrap()
        .contains("is relative"));

    // A directory is not a database.
    let dir = t.path().join("a_directory");
    std::fs::create_dir(&dir).unwrap();
    let (code, r, _) = doctor_json(mind(&["--db", dir.to_str().unwrap(), "doctor", "--json"]));
    assert_eq!(code, 1);
    assert!(check(&r, "database")["summary"]
        .as_str()
        .unwrap()
        .contains("not a regular file"));
}

#[test]
fn a_database_path_that_is_not_valid_utf8_is_inspected_as_is() {
    use std::os::unix::ffi::OsStrExt;
    let t = tmp();
    let odd = t.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9-dir"));
    std::fs::create_dir_all(&odd).unwrap();
    let db = odd.join("mind.db");
    let mut c = mind(&["init"]);
    c.env("MINDSPACE_DB", &db);
    let o = run(c);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut c = mind(&["doctor", "--json"]);
    c.env("MINDSPACE_DB", &db);
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 0, "{r}");
    assert_eq!(check(&r, "database")["status"], "pass", "{r}");
}

#[test]
fn a_write_ahead_log_left_without_a_process_is_read_not_blamed() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let db = initialized(&xdg);
    let dir = db.parent().unwrap();
    // Snapshot the files while a writer holds the WAL open: that is what a crash leaves behind.
    let leftover = t.path().join("leftover");
    std::fs::create_dir_all(&leftover).unwrap();
    {
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer.pragma_update(None, "journal_mode", "wal").unwrap();
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch("CREATE TABLE zz_probe (x); INSERT INTO zz_probe VALUES (1);")
            .unwrap();
        for f in ["mind.db", "mind.db-wal", "mind.db-shm"] {
            std::fs::copy(dir.join(f), leftover.join(f)).unwrap();
        }
    }
    let before = files(&leftover);
    let names_before = names(&leftover);
    let mut c = mind(&[
        "--db",
        leftover.join("mind.db").to_str().unwrap(),
        "doctor",
        "--json",
    ]);
    c.env("XDG_DATA_HOME", &xdg);
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 0, "{r}");
    let d = check(&r, "database");
    assert!(
        d["status"] == "pass" || d["status"] == "warn",
        "a leftover log must not be reported as a broken database: {d}"
    );
    assert_eq!(names(&leftover), names_before);
    for key in ["mind.db", "mind.db-wal"] {
        assert_eq!(files(&leftover).get(key), before.get(key), "{key} changed");
    }
}

/// Sets embedding state through the library, NOT through `mind embeddings enable`:
/// that command downloads the default model (about 550 MB) when it is missing,
/// and these tests must never touch the network.
#[cfg(feature = "embeddings")]
fn set_embedding_state(db: &Path, enabled: bool, model: Option<&str>) {
    use mindspace::storage::Store;
    let store = mindspace::storage::sqlite::SqliteStore::open(db).unwrap();
    mindspace::embeddings::manager::set_enabled(&store, enabled).unwrap();
    if let Some(m) = model {
        store.set_meta("embedding.model", m).unwrap();
    }
}

#[cfg(feature = "embeddings")]
#[test]
fn embeddings_disabled_is_a_skip_and_a_missing_model_is_a_warning() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    let db = initialized(&xdg);
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0);
    assert_eq!(check(&r, "embeddings")["status"], "skip");

    // Enabled, nothing selected.
    set_embedding_state(&db, true, None);
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0, "missing optional components are not blocking");
    let e = check(&r, "embeddings");
    assert_eq!(e["status"], "warn");
    assert_eq!(e["details"]["enabled"], true);
    assert!(e["summary"]
        .as_str()
        .unwrap()
        .contains("no model is selected"));

    // Selected, but its files are not there.
    set_embedding_state(&db, true, Some("nomic-embed-text-v1.5"));
    let (code, r, _) = doctor_with_xdg(&xdg, &[]);
    assert_eq!(code, 0);
    let e = check(&r, "embeddings");
    assert_eq!(e["status"], "warn");
    assert!(
        e["summary"].as_str().unwrap().contains("is not installed"),
        "{e}"
    );
    assert!(e["remedy"]
        .as_str()
        .unwrap()
        .contains("mind model install nomic-embed-text-v1.5"));
    assert!(
        !xdg.join("mindspace/models").exists(),
        "the doctor must not download or create models"
    );
}

// ---------------------------------------------------------------- config

#[test]
fn configuration_from_the_environment_is_validated_and_its_origin_shown() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    initialized(&xdg);
    let root = t.path().join("repos");
    std::fs::create_dir_all(&root).unwrap();

    let mut c = mind(&["doctor", "--json"]);
    c.env("XDG_DATA_HOME", &xdg)
        .env(
            "MINDSPACE_MCP_ALLOW_ROOTS",
            format!("{}:ro", root.display()),
        )
        .env("MINDSPACE_MCP_INGEST_TIMEOUT", "12");
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 0, "{r}");
    let cfg = check(&r, "config");
    assert_eq!(cfg["status"], "pass");
    assert_eq!(cfg["details"]["allowed_roots"]["roots"][0]["mode"], "ro");
    assert_eq!(cfg["details"]["ingest_timeout"]["value"], "12 s");

    // Invalid values are blocking and the sandbox self-test is not affected by them.
    let mut c = mind(&["doctor", "--json"]);
    c.env("XDG_DATA_HOME", &xdg)
        .env("MINDSPACE_MCP_ALLOW_ROOTS", "/does/not/exist:rw")
        .env("MINDSPACE_MCP_INGEST_TIMEOUT", "garbage");
    let (code, r, _) = doctor_json(c);
    assert_eq!(code, 1);
    assert_eq!(check(&r, "config")["status"], "fail");
    #[cfg(target_os = "linux")]
    assert_eq!(
        check(&r, "sandbox")["status"],
        "pass",
        "the child server must not inherit the user's MINDSPACE_* settings"
    );

    // A database directory that contains an allowed root: `mind mcp` would turn the tools off.
    let mut c = mind(&["doctor", "--json"]);
    c.env("XDG_DATA_HOME", &xdg).env(
        "MINDSPACE_MCP_ALLOW_ROOTS",
        format!("{}:rw", xdg.join("mindspace").display()),
    );
    let (_, r, _) = doctor_json(c);
    #[cfg(target_os = "linux")]
    {
        let cfg = check(&r, "config");
        assert_eq!(cfg["status"], "warn");
        assert!(
            cfg["summary"]
                .as_str()
                .unwrap()
                .contains("repository tools off"),
            "{cfg}"
        );
    }
    let _ = r;
}

#[test]
fn the_binary_check_flags_an_older_mind_that_comes_first_on_path() {
    let t = tmp();
    let xdg = t.path().join("xdg");
    initialized(&xdg);
    let bin = t.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("mind");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut c = mind(&["doctor", "--json"]);
    c.env("XDG_DATA_HOME", &xdg)
        .env("PATH", format!("{}:{SAFE_PATH}", bin.display()));
    let (code, r, _) = doctor_json(c);
    assert_eq!(
        code, 0,
        "a stale binary on PATH is a warning, not a failure"
    );
    let b = check(&r, "binary");
    assert_eq!(b["status"], "warn");
    assert_eq!(b["details"]["on_path"], fake.to_str().unwrap());
}

// ------------------------------------------------------------ exit codes

#[test]
fn an_invalid_invocation_exits_with_2_and_cannot_switch_the_sandbox_off() {
    for args in [
        &["doctor", "--bogus"][..],
        &["doctor", "--insecure-no-sandbox"],
    ] {
        let o = run(mind(args));
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(o.stdout.is_empty());
    }
}

// --------------------------------------------------------------- sandbox

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn the_sandbox_self_test_separates_kernel_evidence_from_application_validation() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let (code, r, _) = doctor_with_xdg(&xdg, &[]);
        assert_eq!(code, 0, "{r}");
        let s = check(&r, "sandbox");
        assert_eq!(s["status"], "pass", "{s}");
        let kernel = &s["details"]["kernel_evidence"];
        assert_eq!(kernel["state"], "enforced");
        assert!(kernel["server_report"]
            .as_str()
            .unwrap()
            .starts_with("enforced (Landlock"));
        assert!(kernel["basis"]
            .as_str()
            .unwrap()
            .contains("cannot observe the kernel state"));
        let app = &s["details"]["application_validation"];
        assert_eq!(app["inside_root_allowed"], true);
        assert_eq!(app["outside_root_refused"], true);
        assert_eq!(app["dotdot_escape_refused"], true);
        assert!(app["basis"]
            .as_str()
            .unwrap()
            .contains("does not by itself prove the kernel sandbox"));
    }

    fn unavailable_case(errno: i32) {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let mut c = mind(&["doctor", "--json"]);
        c.env("XDG_DATA_HOME", &xdg).env("TMPDIR", &scratch);
        common::block_landlock(&mut c, errno);
        let (code, r, _) = doctor_json(c);
        assert_eq!(
            code, 0,
            "an unavailable sandbox is safe (fails closed), so not blocking: {r}"
        );
        let s = check(&r, "sandbox");
        assert_eq!(s["status"], "warn", "{s}");
        assert_eq!(s["details"]["kernel_evidence"]["state"], "unavailable");
        assert!(!s["details"]["kernel_evidence"]["reason"]
            .as_str()
            .unwrap()
            .is_empty());
        // The tools are off, so the application checks were not (and cannot be) run.
        assert_eq!(
            s["details"]["application_validation"]["inside_root_allowed"],
            false
        );
        assert!(s["details"]["application_validation"]["outside_root_refused"].is_null());
        // Capabilities: no Landlock means nothing is supported, and the report says why.
        let caps = &s["details"]["capabilities"];
        assert_eq!(caps["kernel"]["landlock"]["state"], "unavailable", "{caps}");
        assert!(caps["kernel"]["seccomp_mode_of_doctor_process"]
            .as_str()
            .unwrap()
            .starts_with("filter"));
        for ch in [
            "filesystem",
            "tcp",
            "unix_path_sockets",
            "unix_abstract_sockets",
            "signals",
        ] {
            let st = &caps["channels"][ch]["status"];
            if ch == "filesystem" || ch == "signals" {
                assert_eq!(st, "not_supported", "{ch}: {caps}");
            } else {
                assert_ne!(st, "enforced", "{ch}: {caps}");
            }
        }
        let hint = s["remedy"].as_str().unwrap();
        assert!(
            !hint.contains("--insecure-no-sandbox"),
            "never recommend the opt-out: {hint}"
        );
        assert!(
            names(&scratch).is_empty(),
            "temporary files left behind: {:?}",
            names(&scratch)
        );
    }

    #[test]
    fn without_landlock_enosys_the_sandbox_check_warns_and_says_why() {
        unavailable_case(38);
    }

    #[test]
    fn with_landlock_disabled_eopnotsupp_the_sandbox_check_warns_and_says_why() {
        unavailable_case(95);
    }

    #[test]
    fn capabilities_are_added_without_changing_the_existing_fields() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let (code, r, _) = doctor_with_xdg(&xdg, &[]);
        assert_eq!(code, 0, "{r}");
        assert_eq!(r["format"], "mind-doctor/1");
        let s = check(&r, "sandbox");
        // every field that existed before is still there
        for k in ["elapsed_ms", "kernel_evidence", "application_validation"] {
            assert!(s["details"].get(k).is_some(), "{k} missing: {s}");
        }
        assert_eq!(s["details"]["kernel_evidence"]["state"], "enforced");
        let caps = &s["details"]["capabilities"];
        assert_eq!(caps["doctor_process_confined_by_mindspace"], false);
        assert_eq!(caps["product"]["uses_seccomp"], true);
        assert_eq!(caps["product"]["landlock_filesystem_abi"], 3);
        assert_eq!(caps["channels"]["filesystem"]["status"], "enforced");
        // Signals: enforced exactly when the kernel has Landlock's signal scope (ABI 6)
        // and the self-test child showed it (an outside process answered EPERM).
        let st = caps["channels"]["signals"]["status"].as_str().unwrap();
        if mindspace::sandbox::landlock_abi() >= 6 {
            assert_eq!(st, "enforced", "{caps}");
        } else {
            assert_ne!(st, "enforced", "no signal scope on this kernel: {caps}");
        }
        // The socket channels are enforced exactly when the self-test child
        // reported the typed seccomp evidence (x86_64); never by kernel support alone.
        let evidence = caps["seccomp_evidence"]["state"].as_str().unwrap();
        for ch in ["tcp", "udp", "unix_path_sockets", "unix_abstract_sockets"] {
            let st = caps["channels"][ch]["status"].as_str().unwrap();
            if evidence == "enforced" {
                assert_eq!(st, "enforced", "{ch}: {caps}");
                assert!(caps["channels"][ch]["limits"]
                    .as_str()
                    .unwrap()
                    .contains("NEW sockets only"));
            } else {
                assert_ne!(st, "enforced", "{ch} without evidence: {caps}");
            }
        }
        #[cfg(target_arch = "x86_64")]
        assert_eq!(evidence, "enforced", "{caps}");
        assert!(caps["limits"]
            .as_str()
            .unwrap()
            .contains("process, network or IPC isolation"));
    }

    #[test]
    fn the_text_report_shows_the_capability_status_of_each_channel() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let out = run({
            let mut c = mind(&["doctor"]);
            c.env("XDG_DATA_HOME", &xdg);
            c
        });
        let text = String::from_utf8_lossy(&out.stdout);
        for ch in [
            "filesystem",
            "tcp",
            "udp",
            "unix_path_sockets",
            "unix_abstract_sockets",
            "signals",
        ] {
            assert!(
                text.contains(&format!("capabilities.channels.{ch}.status: ")),
                "{ch} missing in:\n{text}"
            );
        }
        assert!(text.contains("capabilities.doctor_process_confined_by_mindspace: false"));
    }

    /// A policy that blocks the `seccomp` query looks like an unsupported
    /// kernel but is not: the doctor says "unknown", never "no".
    #[test]
    fn a_blocked_seccomp_query_is_unknown_not_unsupported() {
        let Some(prog) = common::seccomp_denier(&[(libc::SYS_seccomp, None)], libc::ENOSYS) else {
            eprintln!("skipped: no verified seccomp audit constant for this architecture");
            return;
        };
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let mut c = mind(&["doctor", "--json"]);
        c.env("XDG_DATA_HOME", &xdg);
        common::seccomp_command(&mut c, prog);
        let (code, r, _) = doctor_json(c);
        assert_eq!(code, 0, "{r}");
        let caps = &check(&r, "sandbox")["details"]["capabilities"];
        assert_eq!(
            caps["kernel"]["seccomp_filter_supported"], "unknown",
            "{caps}"
        );
    }

    /// An installation failure must never end in a running sandboxed server
    /// that merely lacks the filter: the repository tools stay off.
    #[test]
    fn a_filter_that_cannot_be_installed_makes_the_sandbox_unavailable() {
        let Some(prog) = common::seccomp_denier(
            &[(libc::SYS_prctl, Some(libc::PR_SET_SECCOMP as u32))],
            libc::EPERM,
        ) else {
            eprintln!("skipped: no verified seccomp audit constant for this architecture");
            return;
        };
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let mut c = mind(&["doctor", "--json"]);
        c.env("XDG_DATA_HOME", &xdg);
        common::seccomp_command(&mut c, prog);
        let (code, r, _) = doctor_json(c);
        assert_eq!(
            code, 0,
            "unavailable is safe (fail closed), not blocking: {r}"
        );
        let s = check(&r, "sandbox");
        assert_eq!(s["status"], "warn", "{s}");
        assert_eq!(s["details"]["kernel_evidence"]["state"], "unavailable");
        assert!(s["details"]["kernel_evidence"]["reason"]
            .as_str()
            .unwrap()
            .contains("seccomp"));
        let caps = &s["details"]["capabilities"];
        for ch in [
            "filesystem",
            "tcp",
            "udp",
            "unix_path_sockets",
            "unix_abstract_sockets",
        ] {
            assert_ne!(caps["channels"][ch]["status"], "enforced", "{ch}: {caps}");
        }
    }

    fn approval(r: &Value) -> Value {
        check(r, "sandbox")["details"]["approval"].clone()
    }

    #[test]
    fn approval_assurance_is_added_additively_and_defaults_to_a0() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let (code, r, _) = doctor_with_xdg(&xdg, &[]);
        assert_eq!(code, 0, "{r}");
        assert_eq!(r["format"], "mind-doctor/1");
        let s = check(&r, "sandbox");
        assert_eq!(s["status"], "pass");
        // everything that existed before is still there
        for k in [
            "elapsed_ms",
            "kernel_evidence",
            "application_validation",
            "capabilities",
        ] {
            assert!(s["details"].get(k).is_some(), "{k} missing: {s}");
        }
        let a = approval(&r);
        assert_eq!(a["assurance"]["effective"], "A0", "{a}");
        assert_eq!(a["assurance"]["verified"], "A0");
        assert_eq!(a["assurance"]["supported"], "A1");
        assert_eq!(a["assurance"]["configured"], "A1");
        // client isolation is unknown, not "no" and not "yes"
        assert_eq!(a["agent_isolation"]["state"], "unknown");
        assert_eq!(a["agent_isolation"]["verified"], "no");
        assert_eq!(a["assurance"]["unknown"]["agent_isolation"], true);
        // the terminal prompt is available but unverified; nothing else is implemented
        let term = &a["authorities"]["terminal_cli"]["capability"];
        assert_eq!(
            (
                term["supported"].as_str(),
                term["configured"].as_str(),
                term["verified"].as_str()
            ),
            (Some("yes"), Some("yes"), Some("no"))
        );
        for other in ["polkit", "hardware_token"] {
            let c = &a["authorities"][other]["capability"];
            assert_eq!(
                (c["supported"].as_str(), c["verified"].as_str()),
                (Some("no"), Some("no")),
                "{other}"
            );
        }
        assert!(a["open_limits"]
            .to_string()
            .contains("human consent is not established"));
        assert!(!a.to_string().contains("\"safe\""));
    }

    #[test]
    fn the_reported_host_input_facts_match_what_the_kernel_says_and_never_raise_the_level() {
        use std::os::unix::ffi::OsStrExt;
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let (_, r, _) = doctor_with_xdg(&xdg, &[]);
        let a = approval(&r);
        let access = |p: &str, mode: libc::c_int| -> &'static str {
            let c = std::ffi::CString::new(std::ffi::OsStr::new(p).as_bytes()).unwrap();
            if !Path::new(p).exists() {
                "absent"
            } else if unsafe { libc::access(c.as_ptr(), mode) } == 0 {
                "accessible"
            } else {
                "inaccessible"
            }
        };
        let facts = &a["host_input"]["facts"];
        assert_eq!(
            facts["uinput"],
            access("/dev/uinput", libc::W_OK),
            "{facts}"
        );
        // whatever the host exposes, the level stays where the evidence puts it
        assert_eq!(a["assurance"]["effective"], "A0");
        let path = a["host_input"]["injection_or_observation_path"]
            .as_str()
            .unwrap();
        assert!(["accessible", "none_observed", "unknown"].contains(&path));
        if path == "accessible" {
            assert!(a["open_limits"].to_string().contains("inject input"));
        }
        // with the environment cleared the session sockets cannot even be located
        assert_eq!(facts["session_bus"], "unknown");
    }

    #[test]
    fn the_doctor_changes_nothing_on_disk_for_the_approval_diagnosis() {
        use std::os::unix::fs::MetadataExt;
        let t = tmp();
        let root = t.path().canonicalize().unwrap();
        let xdg = root.join("xdg");
        initialized(&xdg);
        let state = root.join("state");
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let with_state = |args: &[&str]| {
            let mut c = mind(args);
            c.env("XDG_DATA_HOME", &xdg)
                .env("XDG_STATE_HOME", &state)
                .env("HOME", &root);
            c
        };
        // 1. no store: doctor must not create the state directory or the store
        let (code, r, _) = doctor_json(with_state(&["doctor", "--json"]));
        assert_eq!(code, 0);
        assert_eq!(approval(&r)["grant_core"]["store"]["state"], "absent");
        assert!(!state.exists(), "the doctor created {}", state.display());
        // 2. a valid store: untouched (content, inode, mtime), counted
        assert!(run(with_state(&[
            "grant",
            "add",
            project.to_str().unwrap(),
            "--yes"
        ]))
        .status
        .success());
        let file = state.join("mindspace/grants.jsonl");
        let snap = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (
                std::fs::read(p).unwrap(),
                m.ino(),
                m.mtime(),
                m.mtime_nsec(),
                m.len(),
            )
        };
        let before = snap(&file);
        let entries_before = std::fs::read_dir(state.join("mindspace")).unwrap().count();
        let (code, r, _) = doctor_json(with_state(&["doctor", "--json"]));
        assert_eq!(code, 0);
        let st = &approval(&r)["grant_core"]["store"];
        assert_eq!(
            (
                st["state"].as_str(),
                st["grants"].as_u64(),
                st["active"].as_u64()
            ),
            (Some("valid"), Some(1), Some(1))
        );
        assert_eq!(approval(&r)["grant_core"]["capability"]["verified"], "yes");
        assert_eq!(snap(&file), before, "the doctor changed the store");
        assert_eq!(
            std::fs::read_dir(state.join("mindspace")).unwrap().count(),
            entries_before
        );
        // a valid, active grant does not raise the assurance
        assert_eq!(approval(&r)["assurance"]["effective"], "A0");
        // 3. a tampered store: reported, exit code unchanged, store untouched
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, text.replacen("\"ro\"", "\"rw\"", 1)).unwrap();
        let tampered = snap(&file);
        let (code, r, _) = doctor_json(with_state(&["doctor", "--json"]));
        assert_eq!(
            code, 0,
            "an invalid store is a finding, not a failed diagnosis"
        );
        let a = approval(&r);
        assert_eq!(a["grant_core"]["store"]["state"], "invalid");
        assert_eq!(a["assurance"]["configured"], "A0");
        assert_eq!(a["assurance"]["effective"], "A0");
        assert_eq!(snap(&file), tampered);
    }

    #[test]
    fn the_text_report_shows_the_approval_assurance() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let out = run({
            let mut c = mind(&["doctor"]);
            c.env("XDG_DATA_HOME", &xdg);
            c
        });
        let text = String::from_utf8_lossy(&out.stdout);
        for needle in [
            "approval.assurance.effective: A0",
            "approval.assurance.verified: A0",
            "approval.agent_isolation.state: unknown",
            "approval.authorities.terminal_cli.capability.verified: no",
        ] {
            assert!(text.contains(needle), "{needle} missing in:\n{text}");
        }
    }

    #[test]
    fn temporary_files_are_removed_after_a_normal_run() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let mut c = mind(&["doctor", "--json"]);
        c.env("XDG_DATA_HOME", &xdg).env("TMPDIR", &scratch);
        let (code, _, _) = doctor_json(c);
        assert_eq!(code, 0);
        assert!(names(&scratch).is_empty(), "{:?}", names(&scratch));
    }

    #[test]
    fn a_diagnosis_that_cannot_run_exits_with_2_and_leaves_nothing_behind() {
        let t = tmp();
        let xdg = t.path().join("xdg");
        initialized(&xdg);
        let mut c = mind(&["doctor", "--json"]);
        c.env("XDG_DATA_HOME", &xdg)
            .env("TMPDIR", t.path().join("no/such/dir"));
        let (code, r, _) = doctor_json(c);
        assert_eq!(code, 2, "{r}");
        let s = check(&r, "sandbox");
        assert_eq!(s["status"], "fail");
        assert!(s["summary"]
            .as_str()
            .unwrap()
            .contains("could not prepare the self-test"));
        assert_eq!(
            s["diagnostic_error"], true,
            "machine readers must be able to tell"
        );
        assert!(check(&r, "database").get("diagnostic_error").is_none());
        // The other checks were still reported.
        assert_eq!(check(&r, "database")["status"], "pass");
    }

    // The library function with a fake server: timeouts, early exits, garbage.
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fake-mind");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn a_server_that_never_answers_is_stopped_at_the_deadline_and_cleaned_up() {
        use mindspace::doctor::selftest::{run_in, SelftestError};
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let fake = script(t.path(), "exec sleep 60");
        let started = std::time::Instant::now();
        let r = run_in(&scratch, &fake, std::time::Duration::from_millis(800));
        assert!(matches!(r, Err(SelftestError::Timeout(_))), "{r:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(names(&scratch).is_empty(), "{:?}", names(&scratch));
    }

    #[test]
    fn a_server_that_exits_at_once_is_reported_not_trusted() {
        use mindspace::doctor::selftest::{run_in, KernelEvidence};
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let fake = script(t.path(), "echo 'sandbox stage failed' >&2; exit 78");
        let r = run_in(&scratch, &fake, std::time::Duration::from_secs(5)).unwrap();
        match r.kernel {
            KernelEvidence::NotReported(why) => {
                assert!(why.contains("sandbox stage failed"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.inside_allowed, None);
        assert!(names(&scratch).is_empty());
    }

    #[test]
    fn a_server_that_claims_enforcement_without_refusing_outside_paths_is_inconsistent() {
        // The fake answers every request with an "enforced" outline, including for
        // the outside paths: the doctor must not accept that as a pass.
        use mindspace::doctor::selftest::{run_in, KernelEvidence};
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let fake = script(
            t.path(),
            r#"while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":false,"content":[{"type":"text","text":"{\\"sandbox\\":\\"enforced (fake)\\"}"}]}}\n' "$id";;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id";;
  esac
done"#,
        );
        let r = run_in(&scratch, &fake, std::time::Duration::from_secs(10)).unwrap();
        assert!(matches!(r.kernel, KernelEvidence::Enforced(_)));
        assert_eq!(r.inside_allowed, Some(true));
        assert_eq!(
            r.outside_refused,
            Some(false),
            "the fake did not refuse the outside path"
        );
        assert!(names(&scratch).is_empty());
    }

    #[test]
    fn a_grandchild_holding_the_pipes_cannot_stretch_the_deadline() {
        use mindspace::doctor::selftest::{run_in, SelftestError};
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        // The background `sleep` inherits the server's stdout/stderr and outlives it.
        let fake = script(t.path(), "sleep 8 &\nwait");
        let started = std::time::Instant::now();
        let r = run_in(&scratch, &fake, std::time::Duration::from_secs(1));
        assert!(matches!(r, Err(SelftestError::Timeout(_))), "{r:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "returned only after {:?}",
            started.elapsed()
        );
        assert!(names(&scratch).is_empty());
    }

    #[test]
    fn the_server_process_is_gone_after_a_timeout() {
        use mindspace::doctor::selftest::run_in;
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let pidfile = t.path().join("pid");
        let fake = script(
            t.path(),
            &format!("echo $$ > {}\nexec sleep 60", pidfile.display()),
        );
        let _ = run_in(&scratch, &fake, std::time::Duration::from_millis(800));
        let pid: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "server process {pid} still exists after the self-test returned"
        );
    }

    #[test]
    fn a_server_that_is_gone_before_the_first_request_is_a_finding_not_a_broken_test() {
        use mindspace::doctor::selftest::{run_in, KernelEvidence};
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        let Some(truebin) = ["/bin/true", "/usr/bin/true"]
            .iter()
            .map(Path::new)
            .find(|p| p.exists())
        else {
            return;
        };
        // `true` exits at once: depending on timing the first write may hit a closed pipe.
        for _ in 0..100 {
            let r = run_in(&scratch, truebin, std::time::Duration::from_secs(5))
                .expect("an exited server must be reported, not raised as a protocol error");
            assert!(matches!(r.kernel, KernelEvidence::NotReported(_)));
        }
        assert!(names(&scratch).is_empty());
    }

    #[test]
    fn an_error_that_is_not_the_allowlist_refusal_does_not_count_as_a_refusal() {
        use mindspace::doctor::selftest::run_in;
        let t = tmp();
        let scratch = t.path().join("tmpdir");
        std::fs::create_dir(&scratch).unwrap();
        // Inside the root: enforced. Anything mentioning `outside`: an unrelated failure.
        let fake = script(
            t.path(),
            r#"while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *outside*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"internal error: boom"}]}}\n' "$id";;
    *tools/call*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":false,"content":[{"type":"text","text":"{\\"sandbox\\":\\"enforced (fake)\\"}"}]}}\n' "$id";;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id";;
  esac
done"#,
        );
        let r = run_in(&scratch, &fake, std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(r.inside_allowed, Some(true));
        assert_eq!(r.outside_refused, Some(false), "{r:?}");
        assert_eq!(r.traversal_refused, Some(false), "{r:?}");
    }
}
