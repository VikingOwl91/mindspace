//! Private SQLite storage through the public library/CLI boundary.
//!
//! Umasks and fault limits are changed only in freshly exec'd, single-test child
//! processes. The ordinary test runner's process-global umask is never changed.
//! `private_storage_child` is an explicitly invoked subprocess endpoint, not an
//! independently ignored acceptance case. Production creation-time proof also
//! requires the native preparation checkpoint/syscall evidence recorded by the
//! private-database release report; final stat alone cannot prove absence of a
//! creation window.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

use mindspace::storage::Store;
use mindspace::{initialize, open_existing, SqliteStore};
use rusqlite::{Connection, OpenFlags};

const MIND: &str = env!("CARGO_BIN_EXE_mind");
const CHILD_CASE: &str = "MINDSPACE_PRIVATE_STORAGE_CHILD";

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

fn chmod(path: &Path, bits: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn private_files(path: &Path) {
    assert_eq!(mode(path), 0o600, "MAIN: {}", path.display());
    for suffix in ["-wal", "-shm", "-journal"] {
        let aux = sidecar(path, suffix);
        if aux.exists() {
            assert_eq!(mode(&aux), 0o600, "{}", aux.display());
        }
    }
}

fn raw(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap()
}

fn integrity(path: &Path) {
    let conn = raw(path);
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(result, "ok");
    assert!(!conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
}

fn run_child(case: &str, mask: u32) {
    let before = fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|line| line.starts_with("Umask:"))
        .unwrap()
        .to_owned();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "private_storage_child",
        "--ignored",
        "--nocapture",
    ])
    .env(CHILD_CASE, case);
    // SAFETY: pre_exec changes the mask only in this forked child before exec;
    // no parent thread or process observes a changed umask.
    unsafe {
        cmd.pre_exec(move || {
            libc::umask(mask as libc::mode_t);
            Ok(())
        });
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{case}, umask {mask:03o}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let after = fs::read_to_string("/proc/self/status").unwrap();
    assert!(after.lines().any(|line| line == before));
}

#[test]
fn new_main_parents_and_auxiliary_lifecycle_are_private_under_each_umask() {
    for mask in [0o000, 0o022, 0o077] {
        run_child("lifecycle", mask);
    }
}

#[test]
fn cli_default_initialization_is_private_under_each_umask() {
    for mask in [0o000, 0o022, 0o077] {
        run_child("cli", mask);
    }
}

#[test]
fn existing_user_modes_and_empty_file_behavior_are_preserved() {
    run_child("compatibility", 0o000);
}

#[test]
fn real_existing_aliases_work_but_dangling_links_cannot_create_storage() {
    run_child("aliases", 0o022);
}

#[test]
fn unsafe_parents_and_preexisting_sidecars_are_refused_without_chmod() {
    run_child("unsafe", 0o000);
}

#[test]
fn existing_only_open_does_not_create_and_uri_routing_cannot_bypass_preparation() {
    run_child("names", 0o022);
}

#[test]
fn a_database_named_like_a_temporary_publication_file_is_not_removed() {
    run_child("collision", 0o022);
}

#[test]
fn concurrent_initializers_leave_one_private_usable_database() {
    run_child("concurrency", 0o000);
}

#[test]
fn failed_initialization_leaves_private_recoverable_storage() {
    run_child("failure", 0o000);
}

#[test]
fn permission_denied_creation_does_not_modify_existing_parents() {
    run_child("denied", 0o022);
}

#[test]
fn reopening_and_backup_restore_do_not_add_recording_revisions() {
    run_child("revisions", 0o022);
}

/// Protocol endpoint; every scenario is invoked by a named ordinary test above.
#[test]
#[ignore = "subprocess endpoint invoked by the private-storage tests"]
fn private_storage_child() {
    let case = std::env::var(CHILD_CASE).expect("explicit subprocess scenario");
    let root = tempfile::tempdir().unwrap();
    // This existing test-owned ancestor must be protected even when its parent
    // process deliberately supplied umask 000. No database exists yet.
    chmod(root.path(), 0o700);
    match case.as_str() {
        "lifecycle" => lifecycle(root.path()),
        "cli" => cli(root.path()),
        "compatibility" => compatibility(root.path()),
        "aliases" => aliases(root.path()),
        "unsafe" => unsafe_paths(root.path()),
        "names" => names(root.path()),
        "collision" => {
            let db = root
                .path()
                .join(format!(".mindspace-create-{}-0", std::process::id()));
            initialize(&db).unwrap();
            private_files(&db);
            assert!(open_existing(&db).is_ok());
        }
        "concurrency" => concurrency(root.path()),
        "failure" => failed_initialization(root.path()),
        "denied" => denied(root.path()),
        "revisions" => revisions(root.path()),
        other => panic!("unknown private-storage scenario {other}"),
    }
}

fn lifecycle(root: &Path) {
    let db = root.join("new/parents/mind.db");
    let first = SqliteStore::open(&db).unwrap();
    assert_eq!(mode(&root.join("new")), 0o700);
    assert_eq!(mode(db.parent().unwrap()), 0o700);
    first.create_space("one", "").unwrap();
    private_files(&db);
    assert!(sidecar(&db, "-wal").exists());
    assert!(sidecar(&db, "-shm").exists());

    let second = open_existing(&db).unwrap();
    second.create_space("two", "").unwrap();
    assert!(first.get_space("two").is_ok());
    private_files(&db);
    {
        let conn = raw(&db);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        private_files(&db);
    }
    drop(second);
    drop(first);
    let reopened = SqliteStore::open(&db).unwrap();
    assert!(reopened.get_space("one").is_ok());
    assert!(reopened.get_space("two").is_ok());
    private_files(&db);
    drop(reopened);

    // Exercise an actual live rollback journal too, with fixture-only DDL that
    // is rolled back before Mindspace is reopened.
    let conn = raw(&db);
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; BEGIN IMMEDIATE; CREATE TABLE rollback_probe(value TEXT)",
    )
    .unwrap();
    assert_eq!(mode(&sidecar(&db, "-journal")), 0o600);
    conn.execute_batch("ROLLBACK").unwrap();
    drop(conn);
    let reopened = SqliteStore::open(&db).unwrap();
    private_files(&db);
    assert!(reopened.get_space("two").is_ok());
    drop(reopened);
    integrity(&db);
}

fn cli(root: &Path) {
    let data = root.join("data");
    let run = |args: &[&str]| {
        let out = Command::new(MIND)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_DATA_HOME", &data)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init"]);
    let db = data.join("mindspace/mind.db");
    private_files(&db);
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(db.parent().unwrap()), 0o700);
    run(&["space", "create", "cli"]);
    run(&["space", "list"]);
    integrity(&db);
}

fn compatibility(root: &Path) {
    let parent = root.join("existing");
    fs::create_dir(&parent).unwrap();
    chmod(&parent, 0o755);
    let db = parent.join("legacy.db");
    initialize(&db).unwrap();
    chmod(&db, 0o644);
    let store = SqliteStore::open(&db).unwrap();
    store.create_space("legacy", "").unwrap();
    assert_eq!(mode(&db), 0o644);
    assert_eq!(mode(&parent), 0o755);
    drop(store);
    assert!(open_existing(&db).unwrap().get_space("legacy").is_ok());
    assert_eq!(mode(&db), 0o644);

    let empty = parent.join("empty.db");
    fs::write(&empty, b"").unwrap();
    chmod(&empty, 0o640);
    let store = SqliteStore::open(&empty).unwrap();
    store.create_space("empty", "").unwrap();
    assert_eq!(mode(&empty), 0o640);
    drop(store);
    integrity(&empty);
}

fn aliases(root: &Path) {
    let real = root.join("real");
    fs::create_dir(&real).unwrap();
    chmod(&real, 0o700);
    let parent_alias = root.join("parent-alias");
    symlink(&real, &parent_alias).unwrap();
    let db = parent_alias.join("mind.db");
    initialize(&db).unwrap();
    private_files(&real.join("mind.db"));
    let alias = root.join("db-alias");
    symlink(real.join("mind.db"), &alias).unwrap();
    open_existing(&alias)
        .unwrap()
        .create_space("alias", "")
        .unwrap();
    assert!(open_existing(&db).unwrap().get_space("alias").is_ok());

    let missing = root.join("must-not-be-created.db");
    let dangling = root.join("dangling");
    symlink(&missing, &dangling).unwrap();
    assert!(SqliteStore::open(&dangling).is_err());
    assert!(!missing.exists());
    assert!(fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    let missing_parent = root.join("missing-real-parent");
    let dangling_parent = root.join("dangling-parent");
    symlink(&missing_parent, &dangling_parent).unwrap();
    assert!(SqliteStore::open(&dangling_parent.join("mind.db")).is_err());
    assert!(!missing_parent.exists());
}

fn unsafe_paths(root: &Path) {
    let shared = root.join("shared");
    fs::create_dir(&shared).unwrap();
    chmod(&shared, 0o777);
    assert!(SqliteStore::open(&shared.join("mind.db")).is_err());
    assert_eq!(mode(&shared), 0o777);
    assert!(!shared.join("mind.db").exists());

    let existing_parent = root.join("existing-private");
    fs::create_dir(&existing_parent).unwrap();
    chmod(&existing_parent, 0o700);
    let existing = existing_parent.join("mind.db");
    initialize(&existing).unwrap();
    chmod(&existing_parent, 0o777);
    assert!(SqliteStore::open(&existing).is_err());
    assert_eq!(mode(&existing_parent), 0o777);
    assert_eq!(mode(&existing), 0o600);
    chmod(&existing_parent, 0o700);
    drop(SqliteStore::open(&existing).unwrap());

    for (i, suffix) in ["-wal", "-shm", "-journal"].iter().enumerate() {
        let dir = root.join(format!("orphan-{i}"));
        fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o700);
        let db = dir.join("mind.db");
        let aux = sidecar(&db, suffix);
        fs::write(&aux, b"unrelated existing content").unwrap();
        chmod(&aux, 0o644);
        assert!(SqliteStore::open(&db).is_err());
        assert!(!db.exists());
        assert_eq!(mode(&aux), 0o644);
        assert_eq!(fs::read(&aux).unwrap(), b"unrelated existing content");
    }

    for hazard in ["mode", "symlink", "hardlink", "directory"] {
        let dir = root.join(hazard);
        fs::create_dir(&dir).unwrap();
        chmod(&dir, 0o700);
        let db = dir.join("mind.db");
        initialize(&db).unwrap();
        let aux = sidecar(&db, "-wal");
        let target = dir.join("target");
        fs::write(&target, b"fixture target").unwrap();
        chmod(&target, 0o644);
        match hazard {
            "mode" => {
                fs::write(&aux, b"unsafe existing WAL").unwrap();
                chmod(&aux, 0o644);
            }
            "symlink" => symlink(&target, &aux).unwrap(),
            "hardlink" => fs::hard_link(&target, &aux).unwrap(),
            "directory" => fs::create_dir(&aux).unwrap(),
            _ => unreachable!(),
        }
        assert!(SqliteStore::open(&db).is_err(), "accepted {hazard}");
        assert_eq!(mode(&db), 0o600);
        assert_eq!(fs::read(&target).unwrap(), b"fixture target");
        assert_eq!(mode(&target), 0o644);
    }
}

fn names(root: &Path) {
    let absent = root.join("missing/never-created.db");
    assert!(open_existing(&absent).is_err());
    assert!(!absent.parent().unwrap().exists());
    let uri = format!("file:{}?mode=rwc", root.join("uri.db").display());
    assert!(SqliteStore::open(Path::new(&uri)).is_err());
    assert!(!root.join("uri.db").exists());
    for special in [":memory:", ""] {
        let store = SqliteStore::open(Path::new(special)).unwrap();
        store.create_space("anonymous", "").unwrap();
        assert!(store.get_space("anonymous").is_ok());
    }
    let literal = root.join("literal?#%.db");
    initialize(&literal).unwrap();
    private_files(&literal);
}

fn concurrency(root: &Path) {
    let db = Arc::new(root.join("nested/mind.db"));
    let barrier = Arc::new(Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let (db, barrier) = (Arc::clone(&db), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                let store = SqliteStore::open(&db)?;
                store.create_space(&format!("creator-{i}"), "")?;
                Ok::<_, mindspace::Error>(())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().any(Result::is_ok));
    for error in results.iter().filter_map(|r| r.as_ref().err()) {
        let text = error.to_string().to_lowercase();
        assert!(
            text.contains("busy") || text.contains("locked"),
            "unexpected concurrent failure: {error}"
        );
    }
    let store = SqliteStore::open(&db).unwrap();
    private_files(&db);
    assert!(!store.list_spaces().unwrap().is_empty());
    drop(store);
    integrity(&db);
    let allowed = ["mind.db", "mind.db-wal", "mind.db-shm", "mind.db-journal"];
    for entry in fs::read_dir(db.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        assert!(
            allowed.iter().any(|n| entry.file_name() == *n),
            "temporary creation artifact survived: {:?}",
            entry.path()
        );
    }
}

fn failed_initialization(root: &Path) {
    let db = root.join("failed/mind.db");
    // No process other than this freshly exec'd test child sees these resource
    // limits or signal disposition changes. The one-byte file ceiling forces a
    // real SQLite initialization write failure after private MAIN publication.
    let (old_limit, old_signal) = unsafe {
        let mut limit = std::mem::zeroed::<libc::rlimit>();
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit), 0);
        let signal = libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        assert_ne!(signal, libc::SIG_ERR);
        let low = libc::rlimit {
            rlim_cur: 1,
            rlim_max: limit.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &low), 0);
        (limit, signal)
    };
    let result = SqliteStore::open(&db);
    unsafe {
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &old_limit), 0);
        assert_ne!(libc::signal(libc::SIGXFSZ, old_signal), libc::SIG_ERR);
    }
    assert!(
        result.is_err(),
        "resource fault did not fail initialization"
    );
    assert!(
        db.exists(),
        "published private MAIN was unexpectedly deleted"
    );
    private_files(&db);
    assert_eq!(mode(db.parent().unwrap()), 0o700);
    let store = SqliteStore::open(&db).unwrap();
    store.create_space("recovered", "").unwrap();
    drop(store);
    assert!(open_existing(&db).unwrap().get_space("recovered").is_ok());
    integrity(&db);
}

fn denied(root: &Path) {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: permission-denial fixture requires an unprivileged test account");
        return;
    }
    let parent = root.join("denied");
    fs::create_dir(&parent).unwrap();
    chmod(&parent, 0o500);
    let db = parent.join("mind.db");
    let result = initialize(&db);
    assert_eq!(mode(&parent), 0o500);
    assert!(!db.exists());
    chmod(&parent, 0o700);
    assert!(result.is_err());
    initialize(&db).unwrap();
    private_files(&db);
}

fn revisions(root: &Path) {
    let db = root.join("mind.db");
    let store = SqliteStore::open(&db).unwrap();
    store.create_space("preserved", "").unwrap();
    drop(store);
    let count = || {
        raw(&db)
            .query_row("SELECT COUNT(*) FROM m6_revisions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    let before = count();
    let backup = fs::read(&db).unwrap();
    initialize(&db).unwrap();
    drop(open_existing(&db).unwrap());
    assert_eq!(count(), before);
    let restored = root.join("restored.db");
    fs::write(&restored, backup).unwrap();
    chmod(&restored, 0o600);
    let store = open_existing(&restored).unwrap();
    assert!(store.get_space("preserved").is_ok());
    let restored_count: i64 = raw(&restored)
        .query_row("SELECT COUNT(*) FROM m6_revisions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(restored_count, before);
    private_files(&restored);
    drop(store);
    integrity(&restored);
}
