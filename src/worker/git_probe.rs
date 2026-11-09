//! TEST HARNESS (feature `spawn-gate`; the release binary contains none of this): the process the
//! confinement tests drive. It confines itself exactly like the extraction worker (non-dumpable,
//! no-new-privs, socket-deny seccomp, Landlock read-only on the root descriptor 4) plus execute
//! rights for the git binary and its interpreter, then uses the confined Git reader and prints ONE
//! JSON line. The kernel's answers are observed here, in the confined process, not asserted by it.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::Path;

use serde_json::{json, Value};

use crate::ingest::confined_git::{ConfinedGitReader, GitExecutable};
use crate::ingest::deadline::Deadline;
use crate::worker::ROOT_FD;

fn errno_of<T>(r: std::io::Result<T>) -> String {
    match r {
        Ok(_) => "ALLOWED".into(),
        Err(e) => match e.raw_os_error() {
            Some(libc::EACCES) => "EACCES".into(),
            Some(libc::EPERM) => "EPERM".into(),
            Some(n) => format!("errno {n}"),
            None => e.to_string(),
        },
    }
}

fn dumpable() -> i32 {
    // SAFETY: a plain prctl query.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }
}

/// Entry point; the exit code is the process' (0: the scenario ran, 3: the root descriptor is
/// missing, 4: the sandbox or the reader could not be established, 5: unknown scenario).
pub fn main(scenario: &str, git: &Path) -> i32 {
    // SAFETY: F_GETFD only tests whether the number is open.
    if unsafe { libc::fcntl(ROOT_FD, libc::F_GETFD) } == -1 {
        println!(
            "{}",
            json!({"error": "the root descriptor (4) is not open"})
        );
        return 3;
    }
    // SAFETY: the parent placed this descriptor before exec; nothing else of ours owns it.
    let root = unsafe { OwnedFd::from_raw_fd(ROOT_FD) };
    // resolved BEFORE the confinement, as the broker would
    let exe = match GitExecutable::resolve(git) {
        Ok(e) => e,
        Err(e) => {
            println!("{}", json!({"error": format!("git executable: {e}")}));
            return 4;
        }
    };
    let sandbox = match crate::sandbox::enter_extraction_worker_with_exec(
        &root,
        &[ROOT_FD],
        &exe.exec_grants(),
    ) {
        Ok(s) => s,
        Err(e) => {
            println!("{}", json!({"error": format!("sandbox: {e}")}));
            return 4;
        }
    };
    let ms = std::env::var("PROBE_DEADLINE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30_000);
    let reader = match ConfinedGitReader::open(
        root.as_fd(),
        &exe,
        Deadline::after(std::time::Duration::from_millis(ms)),
    ) {
        Ok(r) => r,
        Err(e) => {
            println!(
                "{}",
                json!({"open_error": e.to_string(), "dumpable": dumpable()})
            );
            return 4;
        }
    };
    let out = match scenario {
        "head" => head(&reader),
        "adversarial" => adversarial(&reader, &exe),
        "missing-fd" => {
            let show = |r: crate::core::Result<Vec<u8>>| match r {
                Ok(o) => format!("UNEXPECTED SUCCESS: {}", String::from_utf8_lossy(&o)),
                Err(e) => format!("refused: {e}"),
            };
            json!({
                "absent": show(reader.run_without_descriptor()),
                "foreign": show(reader.run_with_foreign_descriptor()),
            })
        }
        "hold" => return hold(&reader),
        _ => {
            println!("{}", json!({"error": "unknown scenario"}));
            return 5;
        }
    };
    let mut line = out;
    line["dumpable"] = json!(dumpable());
    line["sandbox_seccomp"] = json!(sandbox.seccomp);
    line["landlock_abi"] = json!(sandbox.landlock_abi);
    println!("{line}");
    0
}

fn head(reader: &ConfinedGitReader) -> Value {
    let snap = match reader.head() {
        Ok(s) => s,
        Err(e) => return json!({"error": e.to_string()}),
    };
    let blobs: Vec<String> = snap
        .files
        .iter()
        .filter(|f| f.1 == "blob")
        .map(|f| f.2.clone())
        .collect();
    let contents = reader.blob_contents(&blobs).unwrap_or_default();
    let all: Vec<String> = std::iter::once(snap.commit.clone())
        .chain(snap.files.iter().map(|f| f.2.clone()))
        .collect();
    let (commits, _trees, _tags) = reader.objects(&all).unwrap_or_default();
    json!({
        "commit": snap.commit,
        "files": snap.files,
        "blob_contents": contents.iter().map(|b| json!([b.oid, b.content])).collect::<Vec<_>>(),
        "commits": commits.iter().map(|c| c.oid.clone()).collect::<Vec<_>>(),
        "repository_id": reader.repository_id().map(|u| u.to_string()).map_err(|e| e.to_string()),
    })
}

/// Each attempt must be REFUSED. `refused: true` means the kernel (or git itself, failing
/// closed) did not let it through.
fn adversarial(reader: &ConfinedGitReader, exe: &GitExecutable) -> Value {
    let mut results: Vec<Value> = Vec::new();
    let mut note = |name: &str, refused: bool, detail: String| {
        results.push(json!({"attempt": name, "refused": refused, "detail": detail}));
    };
    let canary = std::env::var("PROBE_CANARY").unwrap_or_default();
    let outside = std::env::var("PROBE_OUTSIDE").unwrap_or_else(|_| "/etc/passwd".into());

    let r = std::fs::read(&outside);
    note(
        "worker reads a file outside the root",
        r.is_err(),
        errno_of(r),
    );
    let r = std::fs::File::create(format!("{outside}.probe"));
    note("worker creates a file next to it", r.is_err(), errno_of(r));
    let own = format!("/proc/{}/fd/{}", std::process::id(), ROOT_FD);
    let r = std::fs::File::create(format!("{own}/probe-write"));
    note("worker writes inside the root", r.is_err(), errno_of(r));
    let r = std::fs::File::create(format!("{own}/.git/mindspace-repository-id"));
    note("worker writes the identity marker", r.is_err(), errno_of(r));

    // git children, through the hardened builder
    let mut git = |name: &str, args: &[&str], pins: bool| {
        let r = reader.run_raw(args, pins);
        let (refused, detail) = match &r {
            Ok(o) => (false, String::from_utf8_lossy(o).trim().to_string()),
            Err(e) => (true, e.to_string()),
        };
        note(name, refused, detail);
    };
    git(
        "git hashes a file outside the root",
        &["hash-object", &outside],
        true,
    );
    git(
        "git writes an object (hash-object -w)",
        &["hash-object", "-w", "--stdin"],
        true,
    );
    git(
        "git updates a ref",
        &["update-ref", "refs/heads/probe-evil", "HEAD"],
        true,
    );
    git(
        "git is pointed at another git directory",
        &["--git-dir=/etc", "rev-parse", "HEAD"],
        true,
    );
    git(
        "git reads a config file outside the root",
        &["config", "--file", &outside, "--list"],
        true,
    );
    let marker = format!(
        "/proc/self/fd/{}/mindspace-repository-id",
        reader.git_dir_fd()
    );
    git(
        "git writes the identity marker through its inherited descriptor",
        &["config", "--file", &marker, "x.y", "z"],
        true,
    );
    if !canary.is_empty() {
        // the kernel alone, without the -c pins: git would start the helper, the sandbox refuses
        let fsmonitor = format!("core.fsmonitor={canary}");
        // git itself succeeds (a failing fsmonitor is a warning); that the helper did NOT start
        // is checked from outside, by the canary file it would have written
        let r = reader.run_raw(&["-c", &fsmonitor, "status", "--porcelain"], false);
        note(
            "git is told to run an fsmonitor helper (pins OFF); the canary is checked outside",
            true,
            format!("git exited {}", if r.is_ok() { "0" } else { "non-zero" }),
        );
        let r = std::process::Command::new(&canary).output();
        note(
            "worker executes a program that is not git",
            r.is_err(),
            errno_of(r),
        );
    }
    let r = std::process::Command::new("/bin/sh")
        .args(["-c", "true"])
        .output();
    note("worker executes /bin/sh", r.is_err(), errno_of(r));

    // the design's negative control: the worker's own /proc path does not work for a child
    let legacy = format!("/proc/{}/fd/{}", std::process::id(), reader.git_dir_fd());
    let r = std::process::Command::new(exe.exe())
        .env_clear()
        .env("GIT_DIR", &legacy)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["rev-parse", "HEAD"])
        .output();
    let (refused, detail) = match r {
        Ok(o) if o.status.success() => (false, "worked".to_string()),
        Ok(o) => (true, String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => (true, e.to_string()),
    };
    note(
        "child uses the worker's /proc/<pid>/fd path (legacy pin style)",
        refused,
        detail,
    );
    json!({ "attempts": results })
}

/// Starts a git child that blocks on its stdin, reports both pids and waits: the test then kills
/// this process and observes whether the child ends.
fn hold(reader: &ConfinedGitReader) -> i32 {
    let child = match reader.spawn_raw(&["cat-file", "--batch"]) {
        Ok(c) => c,
        Err(e) => {
            println!("{}", json!({"error": format!("cannot start git: {e}")}));
            return 4;
        }
    };
    println!(
        "{}",
        json!({"worker": std::process::id(), "git_child": child.id()})
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // keep the child's stdin open (it is owned by `child`) until killed
    std::thread::sleep(std::time::Duration::from_secs(600));
    drop(child);
    0
}
