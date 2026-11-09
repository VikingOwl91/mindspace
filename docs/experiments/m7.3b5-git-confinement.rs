// M7.3b.5 evidence (NOT part of the build): can a Landlocked, non-dumpable process run the git
// plumbing commands the extractor uses against a READ-ONLY repository, and which way of naming
// the pinned descriptors works for a git CHILD? See docs/design-m7.3b5-git-worker.md, section 3.
//
// Reproduce (needs git, Linux with Landlock ABI 3+, and the `landlock` crate in the cargo cache):
//   cargo new --bin gitexp && cd gitexp && printf '[dependencies]\nlandlock = "0.4.7"\nlibc = "0.2"\n[workspace]\n' >> Cargo.toml
//   cp <this file> src/main.rs && cargo build
//   git init -q -b main /tmp/r && ... (make a few commits, a tag and a linked worktree)
//   ./target/debug/gitexp /tmp/r <none|gitonly|gitonly+ld|usr> <nodump|dumpable>
// Beware of shells that do not word-split: pass the mode and the flag as separate arguments.
//
// Findings (git 2.x, Arch, kernel 7.2): see the table in the design document.


use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
};

fn open_dir(p: &str) -> OwnedFd {
    let c = std::ffi::CString::new(p).unwrap();
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0, "open {p}");
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn sh(cmd: &mut Command) -> (String, String) {
    match cmd.output() {
        Ok(o) => (
            format!(
                "exit {} out {}B",
                o.status.code().map(|c| c.to_string()).unwrap_or("signal".into()),
                o.stdout.len()
            ),
            String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("").to_string(),
        ),
        Err(e) => (format!("SPAWN ERROR {e}"), String::new()),
    }
}

fn git_base(gitdir: &str, common: &str, cwd: &str) -> Command {
    let mut c = Command::new("/usr/bin/git");
    c.env_clear();
    for (k, v) in [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
    ] {
        c.env(k, v);
    }
    for cfg in [
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "core.alternateRefsCommand=",
        "protocol.allow=never",
        "gc.auto=0",
        "maintenance.auto=false",
    ] {
        c.arg("-c").arg(cfg);
    }
    c.env("GIT_DIR", gitdir).env("GIT_COMMON_DIR", common).current_dir(cwd);
    c.stdin(Stdio::null());
    c
}

fn main() {
    let repo = std::env::args().nth(1).expect("repo path");
    // exec mode: "none" | "gitonly" (only /usr/bin/git) | "usr" (all of /usr)
    let mode = std::env::args().nth(2).unwrap_or("none".into());
    let with_exec = mode == "usr";
    let dumpable = std::env::args().nth(3).as_deref() == Some("dumpable");
    // descriptors, as the worker holds them: root = 4, .git = 5
    let root = open_dir(&repo);
    let gitd = open_dir(&format!("{repo}/.git"));
    unsafe {
        // move both out of the way first: the originals may already sit at 4 or 5
        let r = libc::fcntl(root.as_raw_fd(), libc::F_DUPFD, 100);
        let g = libc::fcntl(gitd.as_raw_fd(), libc::F_DUPFD, 100);
        libc::dup2(r, 4);
        libc::dup2(g, 5);
        // copies for variant B, inherited by children (dup2 targets have no CLOEXEC)
        libc::dup2(4, 10);
        libc::dup2(5, 11);
    }
    let pid = std::process::id();

    // ---- confinement (the worker's shape)
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, if dumpable { 1 } else { 0 }, 0, 0, 0);
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }
    let abi = ABI::V3;
    let all = AccessFs::from_all(abi);
    let mut rs = Ruleset::default().handle_access(all).unwrap().create().unwrap();
    let read = AccessFs::ReadFile | AccessFs::ReadDir;
    for p in ["/usr", "/lib", "/lib64", "/etc/ld.so.cache"] {
        if let Ok(fd) = PathFd::new(p) {
            let acc = if with_exec { read | AccessFs::Execute } else { read };
            rs = rs.add_rule(PathBeneath::new(fd, acc)).unwrap();
        }
    }
    if mode == "gitonly" || mode == "gitonly+ld" {
        let fd = PathFd::new("/usr/bin/git").unwrap();
        rs = rs
            .add_rule(PathBeneath::new(fd, AccessFs::Execute | AccessFs::ReadFile))
            .unwrap();
    }
    if mode == "gitonly+ld" {
        // the ELF interpreter is executed by the kernel on the binary's behalf
        let ld = std::fs::canonicalize("/lib64/ld-linux-x86-64.so.2").unwrap();
        let fd = PathFd::new(&ld).unwrap();
        rs = rs
            .add_rule(PathBeneath::new(fd, AccessFs::Execute | AccessFs::ReadFile))
            .unwrap();
        println!("   (interpreter: {})", ld.display());
    }
    // the worker's own /proc entry, read
    if let Ok(fd) = PathFd::new(format!("/proc/{pid}")) {
        rs = rs.add_rule(PathBeneath::new(fd, read)).unwrap();
    }
    if let Ok(fd) = PathFd::new("/dev/null") {
        rs = rs
            .add_rule(PathBeneath::new(fd, AccessFs::ReadFile | AccessFs::WriteFile))
            .unwrap();
    }
    // the repository: READ ONLY, by descriptor
    rs = rs
        .add_rule(PathBeneath::new(&root, read))
        .unwrap();
    rs.restrict_self().unwrap();
    println!(
        "== confined: dumpable={dumpable} exec={mode} pid={pid} (Landlock V3, root read-only)"
    );

    // ---- diagnosis: the simplest spawns
    {
        let a_cwd = format!("/proc/{pid}/fd/4");
        let mut c = Command::new("/usr/bin/true");
        c.env_clear().stdin(Stdio::null());
        println!("   diag true (no cwd):             {:?}", sh(&mut c).0);
        let mut c = Command::new("/usr/bin/true");
        c.env_clear().stdin(Stdio::null()).current_dir("/");
        println!("   diag true cwd=/ :               {:?}", sh(&mut c).0);
        let mut c = Command::new("/usr/bin/true");
        c.env_clear().stdin(Stdio::null()).current_dir(&a_cwd);
        println!("   diag true cwd={a_cwd}:  {:?}", sh(&mut c).0);
        let mut c = Command::new("/usr/bin/true");
        c.env_clear().stdin(Stdio::null()).current_dir("/proc/self/fd/10");
        println!("   diag true cwd=/proc/self/fd/10: {:?}", sh(&mut c).0);
        println!("   self readlink fd/4  = {:?}", std::fs::read_link(&a_cwd));
        println!("   self readlink fd/10 = {:?}", std::fs::read_link("/proc/self/fd/10"));
    }
    // ---- the commands the extractor runs
    let cmds: Vec<(&str, Vec<&str>)> = vec![
        ("rev-parse --git-common-dir", vec!["rev-parse", "--git-common-dir"]),
        ("for-each-ref", vec!["for-each-ref", "--format=%(refname) %(objectname)"]),
        ("rev-list --objects --all", vec!["rev-list", "--objects", "--no-object-names", "--all"]),
        ("ls-tree -z -r HEAD", vec!["ls-tree", "-z", "-r", "HEAD"]),
        ("cat-file --batch-check", vec!["cat-file", "--batch-all-objects", "--batch-check"]),
        ("worktree list --porcelain", vec!["worktree", "list", "--porcelain"]),
        ("merge-base --is-ancestor", vec!["merge-base", "--is-ancestor", "HEAD", "HEAD"]),
        ("config --list", vec!["config", "--list"]),
    ];

    // Variant A: the paths as the current code builds them: through THIS process' pid
    let a_git = format!("/proc/{pid}/fd/5");
    let a_cwd = format!("/proc/{pid}/fd/4");
    // Variant B: descriptors inherited by the child, named through the CHILD'S own /proc/self
    for (label, mk) in [
        ("A parent-pid /proc paths", 0u8),
        ("B inherited fds via /proc/self", 1u8),
    ] {
        println!("-- variant {label}");
        for (name, args) in &cmds {
            let mut c = if mk == 0 {
                git_base(&a_git, &a_git, &a_cwd)
            } else {
                git_base("/proc/self/fd/11", "/proc/self/fd/11", "/proc/self/fd/10")
            };
            c.args(args);
            let (res, err) = sh(&mut c);
            println!("   {name:<28} {res:<22} {err}");
        }
    }
    println!("-- probes with plain tools (exec=usr only)");
    for (label, path) in [("B own-pid-of-child /proc/self/fd/11", "/proc/self/fd/11"), ("A parent pid /proc/<pid>/fd/5", a_git.as_str())] {
        for (tool, args) in [("/usr/bin/ls", vec!["-d", path]), ("/usr/bin/ls", vec![path]), ("/usr/bin/cat", vec![]), ] {
            if tool.ends_with("cat") {
                let target = format!("{path}/HEAD");
                let mut c = Command::new(tool);
                c.arg(&target).env_clear().stdin(Stdio::null());
                let (r, e) = sh(&mut c);
                println!("   {label}: cat {target}: {r} {e}");
            } else {
                let mut c = Command::new(tool);
                c.args(&args).env_clear().stdin(Stdio::null());
                let (r, e) = sh(&mut c);
                println!("   {label}: ls {}: {r} {e}", args.join(" "));
            }
        }
    }
    // the same, from the confined process itself (what the directory worker already does)
    println!("   self: read_dir({a_git}) = {:?}", std::fs::read_dir(&a_git).map(|d| d.count()).map_err(|e| e.to_string()));
    println!("   self: read({a_git}/HEAD) = {:?}", std::fs::read_to_string(format!("{a_git}/HEAD")).map_err(|e| e.to_string()));
    // writes must fail (the marker the in-process code would create)
    let w = std::fs::File::create(format!("/proc/{pid}/fd/5/mindspace-repository-id"));
    println!("-- write attempt into .git: {}", match w { Ok(_) => "ALLOWED (BAD)".into(), Err(e) => format!("refused: {e}") });
}
