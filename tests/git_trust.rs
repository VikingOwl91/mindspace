//! M7.3b.5e: the trust gate for the administrator-provided git executable.
//!
//! Attacker model (what the policy defends against): any process that is NOT an accepted owner
//! (default: root) and cannot write the directories or files the executable is reached through, so
//! it can neither replace nor modify them. That includes other users, a same-user shell agent when
//! the chain is root-owned, and the sandboxed MCP frontend. Root is not defended against, nor
//! is the administrator who chose the path.
//!
//! The tests produce each failure mode with real files: the test user's uid is added to the
//! accepted owners so that user-owned fixtures can be made trusted or untrusted by their modes.

#![allow(clippy::field_reassign_with_default)]

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use mindspace::ingest::confined_git::{FilePin, GitExecutable, TrustPolicy};

const GIT: &str = "/usr/bin/git";

fn me() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

fn lenient() -> TrustPolicy {
    TrustPolicy {
        owners: vec![0, me()],
    }
}

/// A private directory below the repository's `target` (every ancestor is owned by root or by the
/// test user and writable by nobody else), not below `/tmp` (world-writable).
fn workdir() -> (tempfile::TempDir, PathBuf) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/trust-tests");
    std::fs::create_dir_all(&base).unwrap();
    let d = tempfile::Builder::new().tempdir_in(&base).unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = std::fs::canonicalize(d.path()).unwrap();
    (d, p)
}

fn copy_git(dir: &Path) -> PathBuf {
    let p = dir.join("git");
    std::fs::copy(GIT, &p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn skip() -> bool {
    if !Path::new(GIT).exists() {
        eprintln!("SKIP: {GIT} not found");
        return true;
    }
    false
}

fn refused(r: Result<GitExecutable, mindspace::core::Error>) -> String {
    match r {
        Ok(g) => panic!("accepted: {g:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_system_git_passes_the_default_root_only_policy_and_is_pinned() {
    if skip() {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(GIT).unwrap();
    if md.uid() != 0 {
        eprintln!("SKIP: {GIT} is not root-owned here");
        return;
    }
    let g = GitExecutable::resolve_trusted(Path::new(GIT), &TrustPolicy::default()).unwrap();
    let (exe, interp) = g.pins().unwrap();
    assert_eq!(exe.uid, 0);
    assert_eq!(exe.mode & 0o022, 0);
    assert_eq!(exe.sha256.len(), 64);
    if g.interpreter().is_some() {
        assert_eq!(interp.unwrap().uid, 0, "the interpreter is pinned too");
    }
    // the pinned files verify, and the sandbox gets exactly those descriptors
    assert_eq!(
        g.verify_pins().unwrap().len(),
        1 + usize::from(g.interpreter().is_some())
    );
}

#[test]
fn a_user_owned_executable_is_refused_by_default_whatever_its_modes() {
    if skip() {
        return;
    }
    let (_d, dir) = workdir();
    let exe = copy_git(&dir);
    let e = refused(GitExecutable::resolve_trusted(
        &exe,
        &TrustPolicy::default(),
    ));
    assert!(e.contains("not trusted"), "{e}");
}

#[test]
fn a_trusted_chain_is_accepted_and_pinned_with_an_explicit_owner_policy() {
    if skip() {
        return;
    }
    let (_d, dir) = workdir();
    let exe = copy_git(&dir);
    let g = GitExecutable::resolve_trusted(&exe, &lenient()).unwrap();
    assert_eq!(g.exe(), exe.as_path());
    assert!(g.pins().is_some());
    g.verify_pins().unwrap();
}

#[test]
fn a_file_or_a_directory_writable_by_others_is_refused() {
    if skip() {
        return;
    }
    let (_d, dir) = workdir();
    let exe = copy_git(&dir);
    for mode in [0o775, 0o757, 0o777] {
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(mode)).unwrap();
        let e = refused(GitExecutable::resolve_trusted(&exe, &lenient()));
        assert!(
            e.contains("the file") && e.contains("not trusted"),
            "{mode:o}: {e}"
        );
    }
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    GitExecutable::resolve_trusted(&exe, &lenient()).unwrap();
    // the directory the file lives in, and one above it
    let inner = dir.join("a/b");
    std::fs::create_dir_all(&inner).unwrap();
    for d in [dir.join("a"), inner.clone()] {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let deep = inner.join("git");
    std::fs::copy(GIT, &deep).unwrap();
    std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o755)).unwrap();
    GitExecutable::resolve_trusted(&deep, &lenient()).unwrap();
    for (writable, mode) in [(&inner, 0o775), (&dir.join("a"), 0o757), (&inner, 0o777)] {
        std::fs::set_permissions(writable, std::fs::Permissions::from_mode(mode)).unwrap();
        let e = refused(GitExecutable::resolve_trusted(&deep, &lenient()));
        assert!(
            e.contains("the directory") && e.contains("not trusted"),
            "{mode:o}: {e}"
        );
        std::fs::set_permissions(writable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // a sticky world-writable directory (like /tmp) does not make a chain trusted either
    let sticky = dir.join("a");
    std::fs::set_permissions(&sticky, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let e = refused(GitExecutable::resolve_trusted(&deep, &lenient()));
    assert!(e.contains("the directory"), "{e}");
    std::fs::set_permissions(&sticky, std::fs::Permissions::from_mode(0o755)).unwrap();
    // and the real /tmp, as a path, is refused too
    let e = refused(GitExecutable::resolve_trusted(
        &std::env::temp_dir().join("git"),
        &lenient(),
    ));
    assert!(!e.is_empty());
}

#[test]
fn symbolic_links_are_followed_only_through_a_trusted_chain() {
    if skip() {
        return;
    }
    let (_d, dir) = workdir();
    let exe = copy_git(&dir);
    // a link owned by an accepted owner, in a trusted directory
    let link = dir.join("git-link");
    symlink(&exe, &link).unwrap();
    let g = GitExecutable::resolve_trusted(&link, &lenient()).unwrap();
    assert_eq!(
        g.exe(),
        exe.as_path(),
        "the real path is what is pinned and executed"
    );
    // `..` is resolved against the real tree
    let dotted = dir.join("x/../git");
    std::fs::create_dir_all(dir.join("x")).unwrap();
    std::fs::set_permissions(dir.join("x"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        GitExecutable::resolve_trusted(&dotted, &lenient())
            .unwrap()
            .exe(),
        exe.as_path()
    );
    // a link under a directory others can write is not trusted: they could repoint it
    let loose = dir.join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o777)).unwrap();
    symlink(&exe, loose.join("git")).unwrap();
    let e = refused(GitExecutable::resolve_trusted(
        &loose.join("git"),
        &lenient(),
    ));
    assert!(e.contains("the directory"), "{e}");
    // a loop, a dangling link, a relative path, a directory
    symlink(dir.join("loop-b"), dir.join("loop-a")).unwrap();
    symlink(dir.join("loop-a"), dir.join("loop-b")).unwrap();
    assert!(refused(GitExecutable::resolve_trusted(
        &dir.join("loop-a"),
        &lenient()
    ))
    .contains("too many"));
    symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
    assert!(GitExecutable::resolve_trusted(&dir.join("dangling"), &lenient()).is_err());
    assert!(GitExecutable::resolve_trusted(Path::new("git"), &lenient()).is_err());
    assert!(GitExecutable::resolve_trusted(&dir, &lenient()).is_err());
    // a link owned by someone who is not accepted: only the default policy can say so here
    assert!(GitExecutable::resolve_trusted(&link, &TrustPolicy::default()).is_err());
}

#[test]
fn a_script_is_refused_because_its_interpreter_would_be_a_third_executable() {
    let (_d, dir) = workdir();
    let script = dir.join("git");
    std::fs::write(&script, "#!/bin/sh\nexec /usr/bin/git \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = refused(GitExecutable::resolve_trusted(&script, &lenient()));
    assert!(e.contains("ELF"), "{e}");
}

#[test]
fn the_pins_detect_a_replaced_a_modified_and_a_re_moded_file() {
    if skip() {
        return;
    }
    let (_d, dir) = workdir();
    let exe = copy_git(&dir);
    let g = GitExecutable::resolve_trusted(&exe, &lenient()).unwrap();
    g.verify_pins().unwrap();
    // touching the file changes nothing that is pinned
    let f = std::fs::OpenOptions::new().write(true).open(&exe).unwrap();
    f.set_modified(std::time::SystemTime::now()).unwrap();
    drop(f);
    g.verify_pins().unwrap();
    // a mode change
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o711)).unwrap();
    assert!(g
        .verify_pins()
        .unwrap_err()
        .to_string()
        .contains("not the file that was approved"));
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    g.verify_pins().unwrap();
    // modified in place: same inode, other content (one byte at the end)
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&exe).unwrap();
        f.write_all(b"\0").unwrap();
    }
    assert!(g
        .verify_pins()
        .unwrap_err()
        .to_string()
        .contains("not the file that was approved"));
    // same size, other bytes (overwrite one byte in the middle)
    std::fs::copy(GIT, &exe).unwrap();
    g.verify_pins().unwrap();
    {
        use std::os::unix::fs::FileExt;
        let f = std::fs::OpenOptions::new().write(true).open(&exe).unwrap();
        let mut b = [0u8; 1];
        f.read_exact_at(&mut b, 100_000).ok();
        f.write_all_at(&[b[0] ^ 0xff], 100_000).unwrap();
    }
    assert!(g
        .verify_pins()
        .unwrap_err()
        .to_string()
        .contains("not the file that was approved"));
    // replaced: another inode with identical content is ALSO refused (identity is pinned)
    std::fs::copy(GIT, dir.join("git.new")).unwrap();
    std::fs::set_permissions(dir.join("git.new"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(dir.join("git.new"), &exe).unwrap();
    assert!(g
        .verify_pins()
        .unwrap_err()
        .to_string()
        .contains("not the file that was approved"));
    // and a link in the place of the file is not followed by the verification
    std::fs::remove_file(&exe).unwrap();
    symlink(GIT, &exe).unwrap();
    assert!(g.verify_pins().is_err());
}

#[test]
fn a_pin_round_trips_through_the_command_line_and_malformed_pins_are_refused() {
    let p = FilePin {
        dev: 1,
        ino: 2,
        size: 3,
        uid: 0,
        mode: 0o100755,
        sha256: "ab".repeat(32),
    };
    assert_eq!(FilePin::parse(&p.to_arg()).unwrap(), p);
    for bad in [
        "",
        "1:2:3:0:4",
        "1:2:3:0:4:xyz",
        &format!("1:2:3:0:4:{}", "A".repeat(64)),
        &format!("x:2:3:0:4:{}", "a".repeat(64)),
        &format!("1:2:3:0:4:{}:5", "a".repeat(64)),
    ] {
        assert!(FilePin::parse(bad).is_err(), "{bad}");
    }
}
