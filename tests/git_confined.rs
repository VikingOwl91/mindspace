//! M7.3b.5b: the confined Git reader, against the real kernel.
//!
//! The probe (`mind git-probe`, test harness only) confines itself like the extraction worker --
//! non-dumpable, no-new-privs, socket-deny seccomp, Landlock read-only on the root descriptor --
//! with execute rights for exactly the git binary and its ELF interpreter, then drives the
//! confined reader. These tests observe what the kernel did, from outside. The repository is
//! compared byte for byte (and by mtime) before and after.
//!
//! Prerequisites (tests that need them say so and return): Linux with Landlock ABI 3 or newer, a
//! system `git` at /usr/bin/git, `/bin/sh` for the descriptor-inspection scripts.

#![cfg(target_os = "linux")]
#![allow(clippy::field_reassign_with_default)]

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use mindspace::ingest::confined_git::{ConfinedGitReader, GitExecutable};
use mindspace::ingest::deadline::Deadline;
use serde_json::Value;

const GIT: &str = "/usr/bin/git";
const MARKER: &str = "0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d";

fn kernel_ok() -> bool {
    if !Path::new(GIT).exists() {
        eprintln!("SKIP: {GIT} not found");
        return false;
    }
    if mindspace::sandbox::landlock_abi() < 3 {
        eprintln!("SKIP: Landlock ABI 3 or newer is required");
        return false;
    }
    true
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(GIT)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env_clear()
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A repository with two commits, a rust module pair, a document, an identity marker; and a
/// directory OUTSIDE it that holds a secret and the canary the hostile fixtures point at.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    secret: PathBuf,
    canary: PathBuf,
    canary_log: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    write(&root, "docs/guide.md", "# Guide\n\nalpha text\n");
    write(
        &root,
        "src/lib.rs",
        "pub mod util;\npub fn run() { util::helper(); }\n",
    );
    write(&root, "src/util.rs", "pub fn helper() {}\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "one"]);
    write(
        &root,
        "src/util.rs",
        "pub fn helper() {}\npub fn extra() {}\n",
    );
    git(&root, &["commit", "-q", "-am", "two"]);
    std::fs::write(root.join(".git/mindspace-repository-id"), MARKER).unwrap();
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, "TOP SECRET\n").unwrap();
    let canary_log = outside.join("canary.log");
    let canary = outside.join("canary.sh");
    std::fs::write(
        &canary,
        format!(
            "#!/bin/sh\necho \"ran $0 $@\" >> {}\n",
            canary_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&canary, std::fs::Permissions::from_mode(0o755)).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        outside,
        secret,
        canary,
        canary_log,
    }
}

impl Fixture {
    fn canary_runs(&self) -> usize {
        std::fs::read_to_string(&self.canary_log)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
}

/// Everything a read-only operation must not change: every path, its size, mtime and contents.
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    fn walk(base: &Path, rel: &Path, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(base.join(rel)).unwrap().flatten() {
            let path = rel.join(e.file_name());
            let meta = std::fs::symlink_metadata(e.path()).unwrap();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            if meta.is_file() {
                std::fs::read(e.path()).unwrap().hash(&mut h);
            }
            out.insert(
                path.display().to_string(),
                format!(
                    "{:o} len={} mtime={}.{} content={:x}",
                    meta.mode(),
                    meta.len(),
                    meta.mtime(),
                    meta.mtime_nsec(),
                    h.finish()
                ),
            );
            if meta.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, Path::new(""), &mut out);
    out
}

/// Starts `mind git-probe` with `root` as descriptor 4 (unless `place_root` is false).
fn probe_cmd(scenario: &str, root: &Path, env: &[(&str, &str)], place_root: bool) -> Command {
    let rootfile = std::fs::File::open(root).unwrap();
    let rootfd = rootfile.as_raw_fd();
    let mut c = Command::new(env!("CARGO_BIN_EXE_mind"));
    c.args(["git-probe", scenario, GIT]).env_clear();
    for (k, v) in env {
        c.env(k, v);
    }
    if place_root {
        // SAFETY: only dup2/fcntl between fork and exec.
        unsafe {
            c.pre_exec(move || {
                if rootfd == 4 {
                    libc::fcntl(4, libc::F_SETFD, 0);
                } else if libc::dup2(rootfd, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // `rootfile` must outlive the spawn; the caller spawns before it is dropped
    std::mem::forget(rootfile);
    c
}

fn probe(scenario: &str, root: &Path, env: &[(&str, &str)]) -> (Output, Value) {
    let out = probe_cmd(scenario, root, env, true).output().unwrap();
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
    let json = serde_json::from_str(&line).unwrap_or_else(|_| {
        panic!(
            "no JSON from the probe (exit {:?}): stdout={:?} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out, json)
}

fn exe() -> GitExecutable {
    GitExecutable::resolve(Path::new(GIT)).unwrap()
}

/// The reader, in THIS process (no kernel confinement): what the pins and the descriptor style do
/// on their own.
fn open_here(root: &Path) -> Result<ConfinedGitReader, mindspace::core::Error> {
    let f = std::fs::File::open(root).unwrap();
    use std::os::fd::AsFd;
    ConfinedGitReader::open(f.as_fd(), &exe(), Deadline::after(Duration::from_secs(20)))
}

// ------------------------------------------------------------------------------ the executable

#[test]
fn the_git_executable_is_resolved_with_its_interpreter_and_nothing_else_is_granted() {
    if !kernel_ok() {
        return;
    }
    let g = exe();
    assert!(g.exe().is_absolute());
    let grants = g.exec_grants();
    assert!(grants.len() <= 2, "{grants:?}");
    assert_eq!(grants[0], std::fs::canonicalize(GIT).unwrap());
    if let Some(i) = g.interpreter() {
        assert!(i.is_absolute() && i.exists(), "{i:?}");
    }
    // the interpreter equals what readelf reports, canonicalized
    let readelf = Command::new("readelf").args(["-l", GIT]).output();
    if let Ok(o) = readelf {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = text.lines().find(|l| l.contains("program interpreter")) {
            let want = line
                .split("interpreter: ")
                .nth(1)
                .unwrap()
                .trim_end_matches(']');
            assert_eq!(
                g.interpreter().unwrap(),
                std::fs::canonicalize(want).unwrap()
            );
        }
    }
}

#[test]
fn an_untrusted_executable_is_refused() {
    let d = tempfile::tempdir().unwrap();
    // relative path
    assert!(GitExecutable::resolve(Path::new("git")).is_err());
    // a script: its interpreter would be a third executable
    let script = d.path().join("git");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = GitExecutable::resolve(&script).unwrap_err().to_string();
    assert!(e.contains("ELF"), "{e}");
    // writable by others
    let copy = d.path().join("git2");
    std::fs::copy(GIT, &copy).ok();
    if copy.exists() {
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o757)).unwrap();
        let e = GitExecutable::resolve(&copy).unwrap_err().to_string();
        assert!(e.contains("writable"), "{e}");
    }
    // not executable / not a file
    assert!(GitExecutable::resolve(d.path()).is_err());
}

// ------------------------------------------------------------------------- success in the kernel

#[test]
fn head_is_read_by_a_non_dumpable_landlocked_worker_and_matches_git() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    let (out, v) = probe("head", &fx.root, &[]);
    assert!(out.status.success(), "{v}");
    // the confinement is real, observed in the probe
    assert_eq!(v["dumpable"], 0, "the worker must not be dumpable");
    assert_eq!(v["sandbox_seccomp"], "enforced");
    // the reader's answer equals what git says
    let head = git(&fx.root, &["rev-parse", "HEAD"]);
    assert_eq!(v["commit"], head.as_str());
    let tree = git(&fx.root, &["ls-tree", "-r", "HEAD"]);
    let want: Vec<(String, String, String, String)> = tree
        .lines()
        .map(|l| {
            let (meta, path) = l.split_once('\t').unwrap();
            let m: Vec<&str> = meta.split(' ').collect();
            (m[0].into(), m[1].into(), m[2].into(), path.into())
        })
        .collect();
    let got: Vec<(String, String, String, String)> =
        serde_json::from_value(v["files"].clone()).unwrap();
    assert_eq!(got, want);
    assert_eq!(got.len(), 3);
    // blob contents
    let blobs: Vec<(String, String)> = serde_json::from_value(v["blob_contents"].clone()).unwrap();
    let util = want.iter().find(|f| f.3 == "src/util.rs").unwrap();
    let content = &blobs.iter().find(|b| b.0 == util.2).unwrap().1;
    assert_eq!(content, "pub fn helper() {}\npub fn extra() {}\n");
    assert_eq!(v["commits"][0], head.as_str());
    // the identity is the marker that was there; nothing was created
    assert_eq!(v["repository_id"]["Ok"], MARKER);
}

#[test]
fn the_same_reader_works_without_the_kernel_confinement_too() {
    // the descriptor style and the pins do not depend on Landlock
    if !Path::new(GIT).exists() {
        return;
    }
    let fx = fixture();
    let r = open_here(&fx.root).unwrap();
    let snap = r.head().unwrap();
    assert_eq!(snap.commit, git(&fx.root, &["rev-parse", "HEAD"]));
    assert_eq!(snap.files.len(), 3);
    assert_eq!(r.repository_id().unwrap().to_string(), MARKER);
}

// ------------------------------------------------------------------------------- read-only

#[test]
fn reading_head_changes_nothing_in_the_repository_confined_or_not() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    let before = snapshot(&fx.root);
    let (out, _) = probe("head", &fx.root, &[]);
    assert!(out.status.success());
    assert_eq!(
        before,
        snapshot(&fx.root),
        "the confined worker changed the repository"
    );
    // and git itself, with the same pins and no kernel to stop it, writes nothing either
    let r = open_here(&fx.root).unwrap();
    let snap = r.head().unwrap();
    let blobs: Vec<String> = snap.files.iter().map(|f| f.2.clone()).collect();
    r.blob_contents(&blobs).unwrap();
    r.objects(&blobs).unwrap();
    assert_eq!(
        before,
        snapshot(&fx.root),
        "an unconfined confined-reader run changed the repository"
    );
}

#[test]
fn the_identity_marker_is_never_created() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    std::fs::remove_file(fx.root.join(".git/mindspace-repository-id")).unwrap();
    let before = snapshot(&fx.root);
    let (_, v) = probe("head", &fx.root, &[]);
    let msg = v["repository_id"]["Err"].as_str().unwrap();
    assert!(msg.contains("identity_missing"), "{msg}");
    assert!(!fx.root.join(".git/mindspace-repository-id").exists());
    assert_eq!(before, snapshot(&fx.root));
    // the same without the kernel
    let r = open_here(&fx.root).unwrap();
    assert!(r
        .repository_id()
        .unwrap_err()
        .to_string()
        .contains("identity_missing"));
    assert!(!fx.root.join(".git/mindspace-repository-id").exists());
}

// ------------------------------------------------------------------------------ adversarial

#[test]
fn every_adversarial_attempt_is_refused_by_the_kernel_and_nothing_changes() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    // positive control: without the sandbox the unpinned fsmonitor helper DOES start
    let c = fx.canary.to_str().unwrap();
    let ctl = open_here(&fx.root).unwrap();
    ctl.run_raw(
        &[
            "-c",
            &format!("core.fsmonitor={c}"),
            "status",
            "--porcelain",
        ],
        false,
    )
    .ok();
    assert!(fx.canary_runs() > 0, "the control did not start the helper");
    std::fs::remove_file(&fx.canary_log).unwrap();
    let before_repo = snapshot(&fx.root);
    let before_outside = snapshot(&fx.outside);
    let (out, v) = probe(
        "adversarial",
        &fx.root,
        &[
            ("PROBE_CANARY", fx.canary.to_str().unwrap()),
            ("PROBE_OUTSIDE", fx.secret.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{v}");
    let attempts = v["attempts"].as_array().unwrap();
    assert!(attempts.len() >= 12, "{attempts:#?}");
    for a in attempts {
        eprintln!(
            "  {:<80} refused={} {}",
            a["attempt"].as_str().unwrap(),
            a["refused"],
            a["detail"].as_str().unwrap().lines().next().unwrap_or("")
        );
    }
    let allowed: Vec<&Value> = attempts.iter().filter(|a| a["refused"] != true).collect();
    assert!(allowed.is_empty(), "NOT REFUSED: {allowed:#?}");
    assert_eq!(before_repo, snapshot(&fx.root), "the repository changed");
    assert_eq!(
        before_outside,
        snapshot(&fx.outside),
        "something outside changed"
    );
    assert_eq!(fx.canary_runs(), 0, "the canary program ran");
    assert!(!fx.outside.join("secret.txt.probe").exists());
    // the legacy pin style is among the refused attempts, with the reason on record
    let legacy = attempts
        .iter()
        .find(|a| a["attempt"].as_str().unwrap().contains("legacy pin style"))
        .expect("the negative control ran");
    eprintln!("legacy pin style, git said: {}", legacy["detail"]);
}

#[test]
fn a_hostile_repository_configuration_runs_nothing_with_or_without_the_kernel() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    let c = fx.canary.to_str().unwrap();
    for (k, v) in [
        ("core.fsmonitor", c),
        ("core.sshCommand", c),
        ("core.pager", c),
        ("core.gitProxy", c),
        ("core.editor", c),
        ("core.hooksPath", fx.outside.to_str().unwrap()),
        ("diff.external", c),
        ("diff.x.textconv", c),
        ("filter.x.clean", c),
        ("filter.x.smudge", c),
        ("filter.x.process", c),
        ("uploadpack.packObjectsHook", c),
        ("credential.helper", c),
        ("gpg.program", c),
        ("core.alternateRefsCommand", c),
        ("alias.ls-tree", &format!("!{c}")),
    ] {
        git(&fx.root, &["config", k, v]);
    }
    write(&fx.root, ".gitattributes", "* filter=x diff=x\n");
    // a hook the repository ships
    write(
        &fx.root,
        ".git/hooks/post-checkout",
        &format!("#!/bin/sh\n{c}\n"),
    );
    std::fs::set_permissions(
        fx.root.join(".git/hooks/post-checkout"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // positive control: this configuration DOES start the helper when git is allowed to (status
    // consults fsmonitor); otherwise "nothing ran" would prove nothing
    let st = Command::new(GIT)
        .arg("-C")
        .arg(&fx.root)
        .arg("status")
        .env_clear()
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(st.status.success() || fx.canary_runs() > 0);
    assert!(
        fx.canary_runs() > 0,
        "the control did not start the helper: the fixture is wrong"
    );
    std::fs::remove_file(&fx.canary_log).unwrap();
    // the reader, in this process: the pins alone
    let before = snapshot(&fx.root);
    let r = open_here(&fx.root).unwrap();
    let snap = r.head().unwrap();
    let blobs: Vec<String> = snap.files.iter().map(|f| f.2.clone()).collect();
    r.blob_contents(&blobs).unwrap();
    r.objects(&blobs).unwrap();
    assert_eq!(
        fx.canary_runs(),
        0,
        "a configured helper ran for the unconfined reader"
    );
    assert_eq!(before, snapshot(&fx.root));
    // the reader in the sandbox: the pins and the kernel
    let (out, v) = probe("head", &fx.root, &[]);
    assert!(out.status.success(), "{v}");
    assert_eq!(v["commit"], snap.commit.as_str());
    assert_eq!(
        fx.canary_runs(),
        0,
        "a configured helper ran in the sandbox"
    );
    assert_eq!(before, snapshot(&fx.root));
}

#[test]
fn replace_refs_and_a_hostile_object_environment_do_not_change_what_is_read() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    let c1 = git(&fx.root, &["rev-parse", "HEAD~1"]);
    let c2 = git(&fx.root, &["rev-parse", "HEAD"]);
    // a replacement ref that would make HEAD's commit read as the older one
    git(&fx.root, &["replace", &c2, &c1]);
    let r = open_here(&fx.root).unwrap();
    let (commits, _, _) = r.objects(std::slice::from_ref(&c2)).unwrap();
    assert_eq!(commits[0].oid, c2);
    assert_eq!(
        commits[0].message.trim(),
        "two",
        "the replacement was honoured"
    );
    // the process environment must not reach git (cleared, not filtered)
    std::env::set_var("GIT_ALTERNATE_OBJECT_DIRECTORIES", "/nonexistent");
    std::env::set_var("GIT_OBJECT_DIRECTORY", "/nonexistent");
    std::env::set_var("GIT_DIR", "/nonexistent");
    std::env::set_var("GIT_WORK_TREE", "/nonexistent");
    std::env::set_var("GIT_CONFIG_COUNT", "1");
    std::env::set_var("GIT_CONFIG_KEY_0", "core.fsmonitor");
    std::env::set_var("GIT_CONFIG_VALUE_0", fx.canary.to_str().unwrap());
    std::env::set_var("PATH", "/nonexistent");
    let r = open_here(&fx.root).unwrap();
    let snap = r.head().unwrap();
    assert_eq!(snap.commit, c2);
    assert_eq!(fx.canary_runs(), 0);
    for k in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        std::env::remove_var(k);
    }
}

// ---------------------------------------------------------------------------------- refusals

fn refusal(root: &Path) -> String {
    match open_here(root) {
        Ok(_) => "OPENED".into(),
        Err(e) => e.to_string(),
    }
}

#[test]
fn linked_worktrees_external_alternates_grafts_and_links_are_refused() {
    if !Path::new(GIT).exists() {
        return;
    }
    // a linked worktree as the root: `.git` is a file
    let fx = fixture();
    let wt = fx.outside.join("linked");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            "HEAD~1",
        ],
    );
    assert!(refusal(&wt).contains("linked worktree"), "{}", refusal(&wt));
    // the main repository, now that it HAS a linked worktree
    assert!(
        refusal(&fx.root).contains("linked worktrees"),
        "{}",
        refusal(&fx.root)
    );
    // ... and again once the worktree is gone
    git(
        &fx.root,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    assert!(open_here(&fx.root).is_ok(), "{}", refusal(&fx.root));

    // external object alternates
    let fx = fixture();
    write(
        &fx.root,
        ".git/objects/info/alternates",
        &format!("{}\n", fx.outside.display()),
    );
    assert!(
        refusal(&fx.root).contains("alternates"),
        "{}",
        refusal(&fx.root)
    );
    // grafts
    let fx = fixture();
    write(&fx.root, ".git/info/grafts", "");
    assert!(
        refusal(&fx.root).contains("grafts"),
        "{}",
        refusal(&fx.root)
    );
    // a git directory of a linked worktree (commondir)
    let fx = fixture();
    write(&fx.root, ".git/commondir", "..\n");
    assert!(
        refusal(&fx.root).contains("commondir"),
        "{}",
        refusal(&fx.root)
    );
    // `.git` as a symbolic link
    let fx = fixture();
    let real = fx.outside.join("moved.git");
    std::fs::rename(fx.root.join(".git"), &real).unwrap();
    std::os::unix::fs::symlink(&real, fx.root.join(".git")).unwrap();
    assert!(
        refusal(&fx.root).contains("symbolic link"),
        "{}",
        refusal(&fx.root)
    );
    // a link among the entries git follows
    let fx = fixture();
    std::fs::rename(fx.root.join(".git/config"), fx.outside.join("config")).unwrap();
    std::os::unix::fs::symlink(fx.outside.join("config"), fx.root.join(".git/config")).unwrap();
    assert!(
        refusal(&fx.root).contains("symbolic link"),
        "{}",
        refusal(&fx.root)
    );
    // `objects/info` is a link, so the alternates check cannot be trusted
    let fx = fixture();
    std::fs::create_dir_all(fx.outside.join("info")).unwrap();
    std::fs::remove_dir_all(fx.root.join(".git/objects/info")).ok();
    std::os::unix::fs::symlink(fx.outside.join("info"), fx.root.join(".git/objects/info")).unwrap();
    assert_ne!(refusal(&fx.root), "OPENED");
    // a bare repository, and a directory that is no repository
    let fx = fixture();
    let bare = fx.outside.join("bare.git");
    git(
        &fx.root,
        &["clone", "-q", "--bare", ".", bare.to_str().unwrap()],
    );
    assert!(
        refusal(&bare).contains("not a git repository"),
        "{}",
        refusal(&bare)
    );
    assert!(refusal(&fx.outside).contains("not a git repository"));
}

#[test]
fn a_sha256_repository_is_refused_instead_of_misread() {
    // The tree parser reads 20-byte entries: on a SHA-256 repository it returns 40-digit ids that
    // name no object. The reader says so up front.
    if !Path::new(GIT).exists() {
        return;
    }
    let fx = fixture();
    let root = fx.outside.join("sha256-repo");
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new(GIT)
        .arg("-C")
        .arg(&root)
        .args(["init", "-q", "--object-format=sha256", "-b", "main"])
        .env_clear()
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!("SKIP: this git cannot create a SHA-256 repository");
        return;
    }
    write(&root, "a.txt", "a\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "one"]);
    let e = refusal(&root);
    assert!(e.contains("object format `sha256`"), "{e}");
    if kernel_ok() {
        let (out, v) = probe("head", &root, &[]);
        assert_eq!(out.status.code(), Some(4));
        assert!(v["open_error"].as_str().unwrap().contains("sha256"), "{v}");
    }
    // ids that are not 40 lowercase hex digits never reach git
    let r = open_here(&fx.root).unwrap();
    for bad in [
        "--output=/tmp/x",
        "HEAD",
        &"a".repeat(64),
        &"A".repeat(40),
        "",
    ] {
        let e = r.blob_contents(&[bad.to_string()]).unwrap_err().to_string();
        assert!(e.contains("only 40-digit SHA-1"), "{bad}: {e}");
    }
}

#[test]
fn the_sandboxed_worker_refuses_a_linked_worktree_too() {
    if !kernel_ok() {
        return;
    }
    let fx = fixture();
    let wt = fx.outside.join("linked");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            "HEAD~1",
        ],
    );
    let (out, v) = probe("head", &wt, &[]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        v["open_error"]
            .as_str()
            .unwrap()
            .contains("linked worktree"),
        "{v}"
    );
    // a repository with alternates pointing outside the authorized root
    let fx = fixture();
    write(
        &fx.root,
        ".git/objects/info/alternates",
        &format!("{}\n", fx.outside.display()),
    );
    let (out, v) = probe("head", &fx.root, &[]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        v["open_error"].as_str().unwrap().contains("alternates"),
        "{v}"
    );
}

// ---------------------------------------------------------------------- inherited descriptors

#[test]
fn absent_descriptors_fail_closed() {
    if !kernel_ok() {
        return;
    }
    // no root descriptor at all
    let fx = fixture();
    let out = probe_cmd("head", &fx.root, &[], false).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("root descriptor"));
    // the git directory's descriptor number is not open in the child: git must fail, and must not
    // discover the repository from its working directory (which is the repository)
    let (out, v) = probe("missing-fd", &fx.root, &[]);
    assert!(out.status.success());
    // a number that is not open in the worker: the spawn fails (nothing is started)
    let absent = v["absent"].as_str().unwrap();
    assert!(absent.starts_with("refused"), "{absent}");
    // a descriptor that is open but names no repository: git fails; it does not look at its
    // working directory, which here IS the repository
    let foreign = v["foreign"].as_str().unwrap();
    assert!(foreign.starts_with("refused"), "{foreign}");
    assert!(foreign.contains("not a git repository"), "{foreign}");
    // the same outside the kernel
    let fx = fixture();
    let r = open_here(&fx.root).unwrap();
    assert!(r.run_without_descriptor().is_err());
    let e = r.run_with_foreign_descriptor().unwrap_err().to_string();
    assert!(e.contains("not a git repository"), "{e}");
    // a root descriptor that is not a directory
    let f = std::fs::File::open(fx.root.join("docs/guide.md"));
    if let Ok(f) = f {
        use std::os::fd::AsFd;
        let e = ConfinedGitReader::open(f.as_fd(), &exe(), Deadline::none());
        assert!(e.is_err());
    }
}

/// Writes an executable script into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn a_git_child_inherits_one_descriptor_a_cleared_environment_and_the_root_as_its_directory() {
    if !Path::new(GIT).exists() {
        return;
    }
    let fx = fixture();
    let r = open_here(&fx.root).unwrap();
    // a descriptor that was left inheritable by mistake, on a file with a recognizable name: the
    // child must still not get it (matched by what it points at, not by its number, which the
    // child's shell may reuse for its own bookkeeping)
    let leak_file = fx.outside.join("leaked-descriptor-target");
    std::fs::write(&leak_file, "x").unwrap();
    let c = std::ffi::CString::new(leak_file.to_str().unwrap()).unwrap();
    // SAFETY: a plain open without O_CLOEXEC, closed below.
    let leaky = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
    assert!(leaky > 2);
    let gfd = r.git_dir_fd();
    let spy = script(
        &fx.outside,
        "spy.sh",
        r#"echo "argv0=$0"
echo "args=$*"
env | sort | sed 's/^/env /'
echo "cwd=$(readlink /proc/self/cwd)"
for f in /proc/self/fd/*; do t=$(readlink "$f"); echo "fd ${f##*/} -> $t"; done"#,
    );
    let out = r.run_with_executable(&spy, &["rev-parse", "HEAD"]).unwrap();
    // SAFETY: closing the descriptor opened above.
    unsafe { libc::close(leaky) };
    let text = String::from_utf8_lossy(&out);
    // an absolute executable, the arguments as given, no shell wrapper
    assert!(text.contains(&format!("argv0={}", spy.display())), "{text}");
    assert!(text.contains("args=rev-parse HEAD"), "{text}");
    // the environment is exactly the pinned one: no PATH, no HOME, nothing from the parent
    let env: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("env "))
        .collect();
    let keys: Vec<&str> = env.iter().map(|l| l.split('=').next().unwrap()).collect();
    for forbidden in [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "{forbidden} reached git: {env:?}"
        );
    }
    assert!(
        env.contains(&format!("GIT_DIR=/proc/self/fd/{gfd}").as_str()),
        "{env:?}"
    );
    assert!(env.contains(&"GIT_NO_REPLACE_OBJECTS=1"), "{env:?}");
    assert!(env.contains(&"GIT_CONFIG_NOSYSTEM=1"), "{env:?}");
    assert!(env.contains(&"GIT_CONFIG_GLOBAL=/dev/null"), "{env:?}");
    assert!(env.contains(&"GIT_NO_LAZY_FETCH=1"), "{env:?}");
    assert!(
        keys.iter().all(|k| k.starts_with("GIT_") || ["LC_ALL", "PWD", "SHLVL", "_", "OLDPWD"].contains(k)),
        "{keys:?}"
    );
    // working directory: the root
    assert!(
        text.contains(&format!("cwd={}", fx.root.display())),
        "{text}"
    );
    // descriptors: stdio, the git directory, and (transiently) the shell's own directory listing
    let fds: Vec<(i32, String)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("fd "))
        .map(|l| {
            let (n, t) = l.split_once(" -> ").unwrap();
            (n.parse().unwrap(), t.to_string())
        })
        .collect();
    let inherited: Vec<&(i32, String)> = fds
        .iter()
        // (the shell's own bookkeeping descriptors read back as empty: its script and the
        // directory listing of the glob, both already closed when the link is read)
        .filter(|(n, t)| {
            *n > 2 && !t.is_empty() && !t.starts_with("/proc/") && !t.starts_with("pipe:")
        })
        .collect();
    assert_eq!(inherited.len(), 1, "unexpected descriptors: {fds:?}");
    assert_eq!(inherited[0].0, gfd);
    assert!(inherited[0].1.ends_with("/.git"), "{:?}", inherited[0]);
    assert!(
        !fds.iter()
            .any(|(_, t)| t.ends_with("leaked-descriptor-target")),
        "the leaky descriptor was inherited: {fds:?}"
    );
}

#[test]
fn a_hung_git_child_is_killed_at_the_deadline_and_leaves_no_process() {
    if !Path::new(GIT).exists() {
        return;
    }
    let fx = fixture();
    let f = std::fs::File::open(&fx.root).unwrap();
    use std::os::fd::AsFd;
    let r = ConfinedGitReader::open(
        f.as_fd(),
        &exe(),
        Deadline::after(Duration::from_millis(400)),
    )
    .unwrap();
    let pidfile = fx.outside.join("pid");
    let hang = script(
        &fx.outside,
        "hang.sh",
        &format!("echo $$ > {}\nexec sleep 30", pidfile.display()),
    );
    let t = Instant::now();
    let e = r
        .run_with_executable(&hang, &["rev-parse", "HEAD"])
        .unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert!(
        e.to_string().to_lowercase().contains("limit") || e.to_string().contains("time"),
        "{e}"
    );
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(gone(pid), "the hung child {pid} is still running");
}

/// True once the process no longer exists (or is only a zombie awaiting its reaper).
fn gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(s)
                if s.rsplit(')')
                    .next()
                    .unwrap_or("")
                    .trim_start()
                    .starts_with('Z') =>
            {
                return true
            }
            Ok(_) => {}
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_git_child_ends_when_its_worker_is_killed() {
    if !kernel_ok() {
        return;
    }
    use std::io::{BufRead, BufReader};
    let fx = fixture();
    let mut cmd = probe_cmd("hold", &fx.root, &[], true);
    cmd.stdout(std::process::Stdio::piped());
    let mut worker = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(worker.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let v: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("probe said {line:?}"));
    let child = v["git_child"].as_i64().unwrap() as i32;
    assert!(
        std::path::Path::new(&format!("/proc/{child}")).exists(),
        "the git child is not running"
    );
    // while it runs it is in the worker's confinement: a Landlock domain and no new privileges
    let status = std::fs::read_to_string(format!("/proc/{child}/status")).unwrap();
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("NoNewPrivs:") && l.trim().ends_with('1')),
        "{status}"
    );
    worker.kill().unwrap();
    worker.wait().unwrap();
    assert!(gone(child), "the git child {child} outlived its worker");
}

// ------------------------------------------------------------------------------- performance

/// Not a gate: `cargo test --release --test git_confined performance -- --ignored --nocapture`
/// prints what the confined reader costs against the in-process reader on the same repository.
#[test]
#[ignore]
fn performance_observation() {
    if !Path::new(GIT).exists() {
        return;
    }
    let fx = fixture();
    for d in 0..40 {
        for f in 0..50 {
            write(
                &fx.root,
                &format!("src/m{d}/f{f}.rs"),
                &format!("pub fn f{d}_{f}() {{ /* {} */ }}\n", "x".repeat(200)),
            );
        }
    }
    git(&fx.root, &["add", "-A"]);
    git(&fx.root, &["commit", "-q", "-m", "many"]);
    let work = |files: &[(String, String, String, String)]| -> Vec<String> {
        files
            .iter()
            .filter(|f| f.1 == "blob")
            .map(|f| f.2.clone())
            .collect()
    };
    for round in 0..3 {
        let t = Instant::now();
        let plain = mindspace::ingest::git::GitReader::open(&fx.root).unwrap();
        let head = plain.head_commit().unwrap();
        let files = plain.commit_files(&head).unwrap();
        let blobs = plain.blob_contents(&work(&files)).unwrap();
        let in_process = t.elapsed();
        let t = Instant::now();
        let r = open_here(&fx.root).unwrap();
        let snap = r.head().unwrap();
        let confined_blobs = r.blob_contents(&work(&snap.files)).unwrap();
        let confined = t.elapsed();
        assert_eq!(blobs.len(), confined_blobs.len());
        let t = Instant::now();
        let (out, _) = probe("head", &fx.root, &[]);
        assert!(out.status.success());
        let in_sandbox = t.elapsed();
        eprintln!(
            "round {round}: {} files | in-process {:?} | confined reader (no kernel) {:?} | sandboxed probe incl. process start + Landlock {:?}",
            files.len(), in_process, confined, in_sandbox
        );
    }
}
