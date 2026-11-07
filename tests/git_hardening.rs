//! Adversarial regression tests for Git hardening (MIND-2).
//!
//! Threat model: a repository directory (for example an unpacked archive that
//! includes `.git`) and the calling environment are untrusted input. Ingest
//! must (a) never execute anything the repository or the environment
//! configures, (b) read exactly the repository it was pointed at, (c) record
//! object content that really hashes to the recorded object id, and (d) leave
//! the target repository untouched apart from its identity marker.
//!
//! Evidence status of each test against the code *before* the hardening fix
//! is stated on the test (observed by running it, not assumed):
//!  * `inherited_git_dir_does_not_redirect_ingest`   RED before the fix
//!  * `inherited_git_namespace_does_not_hide_refs`   GUARD (my hypothesis that it
//!    was red was wrong: observed green before the fix)
//!  * `replace_refs_do_not_change_recorded_content`  RED before the fix
//!  * all others are GUARDS: they hold before the fix and pin the property.
//!
//! Test preparation never executes poisoned configuration: repositories are
//! created and committed first (with user/system Git config disabled), and the
//! hostile config is appended afterwards as plain text. Every "payload" is a
//! script that only creates an empty marker file inside a temp directory.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Git for *test preparation only*: user and system config are disabled and a
/// fixed identity is used. This is the safe setup path; it never runs against
/// a repository whose config has been poisoned.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        // `git commit` spawns a detached `git maintenance run --auto`, which creates and removes
        // objects/maintenance.lock a few milliseconds later. Fixture setup must not leave that behind,
        // or a fingerprint taken right after it races with it (seen on a loaded CI runner).
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn new_repo(path: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    for (name, body) in files {
        let p = path.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    git(path, &["add", "-A"]);
    git(path, &["commit", "-q", "-m", "init"]);
}

struct Db {
    path: PathBuf,
}

impl Db {
    fn new(dir: &Path) -> Self {
        let db = Db {
            path: dir.join("mind.db"),
        };
        assert!(db.mind(&["init"], &[]).status.success());
        assert!(db.mind(&["space", "create", "s"], &[]).status.success());
        db
    }

    /// Runs the real `mind` binary. The caller's environment is inherited on
    /// purpose, because inheriting it is part of what is being tested.
    fn mind(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mind"));
        cmd.arg("--db").arg(&self.path).args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run mind")
    }

    fn ingest(&self, repo: &Path, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let mut args = vec!["ingest", repo.to_str().unwrap(), "--space", "s"];
        args.extend_from_slice(extra);
        self.mind(&args, env)
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Fingerprint of every file under `dir`: relative path -> (len, sha256).
/// mtimes are deliberately excluded (reading may update atime only); content
/// and presence are what must not change.
fn fingerprint(dir: &Path, skip: &[&str]) -> BTreeMap<String, (u64, String)> {
    fn walk(base: &Path, dir: &Path, skip: &[&str], out: &mut BTreeMap<String, (u64, String)>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            if skip.iter().any(|s| rel == *s) {
                continue;
            }
            let md = fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                walk(base, &p, skip, out);
            } else if md.is_file() {
                let bytes = fs::read(&p).unwrap();
                let hex: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.insert(rel, (md.len(), hex));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, skip, &mut out);
    out
}

fn markers(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("FIRED"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// A helper that records that it ran by creating `<markers>/FIRED-<name>`.
fn payload(markers: &Path, name: &str) -> String {
    let dir = markers.join("bin");
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}.sh"));
    fs::write(
        &p,
        format!(
            "#!/bin/sh\ntouch '{}/FIRED-{name}'\nexit 0\n",
            markers.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p.display().to_string()
}

/// Config text whose every exec-capable key points at a marker payload.
fn hostile_config(markers: &Path, repo_for_include_if: Option<&Path>) -> String {
    let mut c = String::new();
    let hooks = markers.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    for h in [
        "post-checkout",
        "post-merge",
        "post-index-change",
        "reference-transaction",
        "pre-auto-gc",
        "fsmonitor-watchman",
    ] {
        let p = hooks.join(h);
        fs::write(
            &p,
            format!("#!/bin/sh\ntouch '{}/FIRED-hook-{h}'\n", markers.display()),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    c.push_str(&format!("[core]\n\thooksPath = {}\n", hooks.display()));
    for (sect, key, name) in [
        ("core", "fsmonitor", "fsmonitor"),
        ("core", "alternateRefsCommand", "alternateRefsCommand"),
        ("core", "sshCommand", "sshCommand"),
        ("core", "gitProxy", "gitProxy"),
        ("core", "askPass", "askPass"),
        ("core", "editor", "editor"),
        ("core", "pager", "pager"),
        ("uploadpack", "packObjectsHook", "packObjectsHook"),
        ("diff", "external", "diffexternal"),
        ("gpg", "program", "gpgprogram"),
        ("credential", "helper", "credentialhelper"),
    ] {
        c.push_str(&format!("[{sect}]\n\t{key} = {}\n", payload(markers, name)));
    }
    for k in ["smudge", "clean", "process"] {
        c.push_str(&format!(
            "[filter \"evil\"]\n\t{k} = {}\n",
            payload(markers, &format!("filter-{k}"))
        ));
    }
    c.push_str(&format!(
        "[diff \"evil\"]\n\ttextconv = {}\n",
        payload(markers, "textconv")
    ));
    c.push_str(&format!(
        "[remote \"ext\"]\n\turl = ext::{}\n",
        payload(markers, "ext-remote")
    ));
    let inc = markers.join("included.gitconfig");
    fs::write(
        &inc,
        format!(
            "[core]\n\tfsmonitor = {}\n",
            payload(markers, "include-fsmonitor")
        ),
    )
    .unwrap();
    c.push_str(&format!("[include]\n\tpath = {}\n", inc.display()));
    if let Some(r) = repo_for_include_if {
        c.push_str(&format!(
            "[includeIf \"gitdir:{}/\"]\n\tpath = {}\n",
            r.display(),
            inc.display()
        ));
    }
    c
}

/// Every ingest-side command a user or agent can run against a repository.
fn all_modes(db: &Db, repo: &Path, env: &[(&str, &str)]) -> Vec<(String, Output)> {
    let r = repo.to_str().unwrap();
    let mut v = vec![
        ("scan".to_string(), db.mind(&["scan", r], env)),
        ("repo info".to_string(), db.mind(&["repo", "info", r], env)),
        ("ingest".to_string(), db.ingest(repo, &[], env)),
        (
            "ingest --history".to_string(),
            db.ingest(repo, &["--history"], env),
        ),
        (
            "ingest --branches --remotes --tags".to_string(),
            db.ingest(repo, &["--branches", "--remotes", "--tags"], env),
        ),
        ("symbols".to_string(), db.mind(&["symbols", "hello"], env)),
    ];
    v.retain(|_| true);
    v
}

// ---------------------------------------------------------------------------
// Environment attacks (RED before the fix)
// ---------------------------------------------------------------------------

/// An inherited `GIT_DIR` (as set inside git hooks, `git rebase -x`, some CI
/// runners) overrides `current_dir`, so Git reads a different repository than
/// the one ingested and Mindspace records its code under the wrong identity.
#[test]
fn inherited_git_dir_does_not_redirect_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
    new_repo(&one, &[("src/a.rs", "pub fn from_repo_one() {}\n")]);
    new_repo(&two, &[("src/a.rs", "pub fn from_repo_two() {}\n")]);
    let db = Db::new(tmp.path());

    let gd = two.join(".git");
    let o = db.ingest(&one, &[], &[("GIT_DIR", gd.to_str().unwrap())]);
    assert!(o.status.success(), "{}", text(&o));

    let one_hits = text(&db.mind(&["symbols", "from_repo_one"], &[]));
    let two_hits = text(&db.mind(&["symbols", "from_repo_two"], &[]));
    assert!(
        one_hits.contains("from_repo_one"),
        "repo one not ingested: {one_hits}"
    );
    assert!(
        !two_hits.contains("from_repo_two"),
        "code of the repo named by GIT_DIR was recorded: {two_hits}"
    );
}

/// `GIT_NAMESPACE` restricts `for-each-ref` to a sub-namespace, so an
/// inherited value could make real refs disappear (and retire memories).
#[test]
fn inherited_git_namespace_does_not_hide_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    new_repo(&repo, &[("src/a.rs", "pub fn visible() {}\n")]);
    let db = Db::new(tmp.path());

    let o = db.ingest(&repo, &[], &[("GIT_NAMESPACE", "elsewhere")]);
    assert!(o.status.success(), "{}", text(&o));
    let hits = text(&db.mind(&["symbols", "visible"], &[]));
    assert!(
        hits.contains("visible"),
        "refs were hidden by GIT_NAMESPACE: {hits}"
    );
}

// ---------------------------------------------------------------------------
// Evidence integrity (RED before the fix)
// ---------------------------------------------------------------------------

/// `refs/replace/*` makes `git cat-file --batch` return different bytes for an
/// object id than the bytes that hash to it. Recorded evidence would then
/// claim "blob <oid> contains X" where X does not hash to <oid>.
#[test]
fn replace_refs_do_not_change_recorded_content() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    new_repo(&repo, &[("src/a.rs", "pub fn real_function() {}\n")]);
    let real = git(&repo, &["rev-parse", "HEAD:src/a.rs"]);
    let fake_file = tmp.path().join("fake.rs");
    fs::write(&fake_file, "pub fn replaced_by_attacker() {}\n").unwrap();
    let fake = git(&repo, &["hash-object", "-w", fake_file.to_str().unwrap()]);
    git(&repo, &["replace", &real, &fake]);
    let db = Db::new(tmp.path());

    let o = db.ingest(&repo, &[], &[]);
    assert!(o.status.success(), "{}", text(&o));

    let real_hits = text(&db.mind(&["symbols", "real_function"], &[]));
    let fake_hits = text(&db.mind(&["symbols", "replaced_by_attacker"], &[]));
    assert!(
        real_hits.contains("real_function"),
        "real content missing: {real_hits}"
    );
    assert!(
        !fake_hits.contains("replaced_by_attacker"),
        "substituted content was recorded for blob {real}: {fake_hits}"
    );
}

// ---------------------------------------------------------------------------
// Guards (hold before the fix; pin the property)
// ---------------------------------------------------------------------------

/// Hostile repository config, hooks and includes never execute in any mode.
#[test]
fn hostile_repo_config_and_hooks_never_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("evil");
    let mk = tmp.path().join("markers");
    fs::create_dir_all(&mk).unwrap();
    new_repo(
        &repo,
        &[
            ("src/lib.rs", "pub fn hello() -> u32 { 1 }\n"),
            ("README.md", "# t\n"),
            (
                ".gitattributes",
                "*.rs filter=evil diff=evil\n*.md filter=evil\n",
            ),
        ],
    );
    git(&repo, &["tag", "-a", "-m", "rel", "v1.0.0"]);
    // Poison only now, as plain text: nothing above ran poisoned config.
    let cfg = repo.join(".git/config");
    let mut c = fs::read_to_string(&cfg).unwrap();
    c.push_str(&hostile_config(&mk, Some(&repo)));
    fs::write(&cfg, c).unwrap();
    for h in [
        "post-checkout",
        "post-merge",
        "reference-transaction",
        "pre-auto-gc",
    ] {
        let p = repo.join(".git/hooks").join(h);
        fs::write(
            &p,
            format!("#!/bin/sh\ntouch '{}/FIRED-repohook-{h}'\n", mk.display()),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let db = Db::new(tmp.path());
    for (name, o) in all_modes(&db, &repo, &[]) {
        assert!(
            o.status.success() || name == "symbols",
            "{name} failed: {}",
            text(&o)
        );
    }
    assert_eq!(markers(&mk), Vec::<String>::new(), "payload executed");
}

/// Hostile *user-level* configuration and config injected through the
/// environment must not execute or influence ingest either.
#[test]
fn hostile_global_config_and_env_config_never_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    let mk = tmp.path().join("markers");
    fs::create_dir_all(&mk).unwrap();
    new_repo(&repo, &[("src/lib.rs", "pub fn hello() -> u32 { 1 }\n")]);
    let global = tmp.path().join("hostile-global.gitconfig");
    fs::write(&global, hostile_config(&mk, None)).unwrap();

    let db = Db::new(tmp.path());
    let fsm = payload(&mk, "env-fsmonitor");
    let params = format!("'core.fsmonitor={fsm}'");
    let env = [
        ("GIT_CONFIG_GLOBAL", global.to_str().unwrap()),
        ("GIT_CONFIG_SYSTEM", global.to_str().unwrap()),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.fsmonitor"),
        ("GIT_CONFIG_VALUE_0", fsm.as_str()),
        ("GIT_CONFIG_PARAMETERS", params.as_str()),
        ("GIT_EXTERNAL_DIFF", fsm.as_str()),
        ("GIT_SSH_COMMAND", fsm.as_str()),
        ("GIT_PAGER", fsm.as_str()),
        ("GIT_ASKPASS", fsm.as_str()),
    ];
    for (name, o) in all_modes(&db, &repo, &env) {
        assert!(
            o.status.success() || name == "symbols",
            "{name} failed: {}",
            text(&o)
        );
    }
    assert_eq!(markers(&mk), Vec::<String>::new(), "payload executed");
}

/// Ingest changes nothing in the target repository except its identity marker.
#[test]
fn ingest_leaves_target_repo_untouched_except_identity_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    new_repo(
        &repo,
        &[
            ("src/lib.rs", "pub fn hello() -> u32 { 1 }\n"),
            ("a.md", "# t\n\nx\n"),
        ],
    );
    git(&repo, &["tag", "-a", "-m", "rel", "v1"]);
    git(&repo, &["branch", "side"]);
    let before = fingerprint(&repo, &[]);
    let db = Db::new(tmp.path());
    for (name, o) in all_modes(&db, &repo, &[]) {
        assert!(
            o.status.success() || name == "symbols",
            "{name} failed: {}",
            text(&o)
        );
    }
    let mut after = fingerprint(&repo, &[]);
    let marker = after.remove(".git/mindspace-repository-id");
    assert!(
        marker.is_some(),
        "identity marker expected (ADR-014 identity)"
    );
    assert_eq!(before, after, "target repository changed during ingest");
}

/// Hostile ref names (leading dashes, option-like text) stay inert data.
#[test]
fn hostile_ref_names_are_inert() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    let mk = tmp.path().join("markers");
    fs::create_dir_all(&mk).unwrap();
    new_repo(&repo, &[("a.rs", "pub fn f() {}\n")]);
    let c = git(&repo, &["rev-parse", "HEAD"]);
    for (dir, name) in [
        ("refs/heads", "--upload-pack=touch-FIRED-ref"),
        ("refs/heads", "-x"),
        ("refs/tags", "--output=ref-injected"),
    ] {
        fs::write(repo.join(".git").join(dir).join(name), format!("{c}\n")).unwrap();
    }
    let db = Db::new(tmp.path());
    let o = db.ingest(&repo, &["--branches", "--tags"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(markers(&mk), Vec::<String>::new());
    // Option injection would create files in the repo dir (cwd of git).
    for name in ["ref-injected", "FIRED-ref"] {
        assert!(!repo.join(name).exists(), "{name} was created");
    }
}

/// A credential in a remote URL must never be written to the database.
#[test]
fn remote_url_credentials_are_not_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    new_repo(&repo, &[("a.rs", "pub fn f() {}\n")]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://user:s3cret-token@example.invalid/owner/name.git",
        ],
    );
    let db = Db::new(tmp.path());
    let o = db.ingest(&repo, &["--remotes"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let info = db.mind(&["repo", "info", repo.to_str().unwrap()], &[]);
    assert!(
        !text(&info).contains("s3cret-token"),
        "credential in repo info: {}",
        text(&info)
    );
    for ext in ["", "-wal"] {
        let p = PathBuf::from(format!("{}{ext}", db.path.display()));
        if let Ok(bytes) = fs::read(&p) {
            assert!(
                !bytes.windows(12).any(|w| w == b"s3cret-token"),
                "credential found in {}",
                p.display()
            );
        }
    }
}

/// Directory ingest does not follow symlinks out of the root, and `--path`
/// rejects traversal and absolute paths.
#[test]
fn symlinks_and_path_escapes_do_not_leave_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, outside) = (tmp.path().join("root"), tmp.path().join("outside"));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.rs"), "pub fn outside_secret_fn() {}\n").unwrap();
    fs::write(root.join("src/a.rs"), "pub fn inside_fn() {}\n").unwrap();
    symlink("../../outside/secret.rs", root.join("src/link.rs")).unwrap();
    symlink("../outside", root.join("linkdir")).unwrap();
    let db = Db::new(tmp.path());

    let o = db.ingest(&root, &[], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&db.mind(&["symbols", "outside_secret_fn"], &[])).contains("outside_secret_fn"));

    for bad in ["../outside", "/etc", "linkdir/../x"] {
        let o = db.ingest(&root, &["--path", bad], &[]);
        assert!(
            !o.status.success(),
            "--path {bad} was accepted: {}",
            text(&o)
        );
    }
}
