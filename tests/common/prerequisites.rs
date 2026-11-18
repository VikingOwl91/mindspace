//! Runtime prerequisites for integration tests. A missing capability may skip a
//! developer's test; strict CI requires every selected test to run.
#![allow(dead_code)]

use std::io::Write;

fn missing(reason: &str) {
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("unnamed test");
    if std::env::var_os("MINDSPACE_STRICT_TESTS").is_some() {
        panic!("{test}: missing test prerequisite: {reason}");
    }
    // Bypass libtest's capture so a developer sees the reason even on a passing run.
    writeln!(std::io::stderr().lock(), "SKIPPED {test}: {reason}").unwrap();
}

/// Record only the tests that actually checked prerequisites. The optional CI
/// log is shared by all harnesses, so append a complete record in one write.
fn observed(capability: &str) -> bool {
    if let Some(path) = std::env::var_os("MINDSPACE_TEST_PREREQUISITE_LOG") {
        let thread = std::thread::current();
        let record = format!(
            "{} {} {}\n",
            env!("CARGO_CRATE_NAME"),
            thread.name().unwrap_or("unnamed"),
            capability
        );
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open prerequisite log")
            .write_all(record.as_bytes())
            .expect("record test prerequisite");
    }
    true
}

pub fn require(present: bool, capability: &str) -> bool {
    if present {
        observed(capability)
    } else {
        missing(capability);
        false
    }
}

pub fn required_value<T>(value: Option<T>, capability: &str) -> Option<T> {
    require(value.is_some(), capability);
    value
}

pub fn system_git() -> bool {
    require(
        std::path::Path::new("/usr/bin/git").try_exists().unwrap(),
        "/usr/bin/git",
    )
}

#[cfg(target_os = "linux")]
pub fn landlock(minimum: u32) -> bool {
    use mindspace::doctor::capabilities::{probe_kernel, LandlockState};
    match probe_kernel().landlock {
        LandlockState::Available(v) if v >= minimum => {
            observed(&format!("Landlock ABI >= {minimum}"))
        }
        LandlockState::Unknown(_) => panic!("could not determine the kernel Landlock ABI"),
        observed => {
            missing(&format!(
                "requires Landlock ABI {minimum}, observed {observed:?}"
            ));
            false
        }
    }
}

#[cfg(target_os = "linux")]
pub enum Cgroup {
    Direct,
    DirectOrSibling,
    WithControllers,
}

#[cfg(target_os = "linux")]
pub fn cgroup_delegated(placement: Cgroup) -> bool {
    use std::{fs, io::ErrorKind, path::Path};
    // Probe the kernel capability independently of Broker::new. A broker
    // startup error after this succeeds is a test failure, even outside CI.
    // In-process Broker instances can temporarily move this test process to
    // their supervisor cgroup. Keep the original location from the first probe.
    static ORIGINAL: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(parent) = ORIGINAL.get_or_init(|| {
        fs::read_to_string("/proc/self/cgroup")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("0::/"))
            .map(|relative| Path::new("/sys/fs/cgroup").join(relative))
    }) else {
        missing("requires a unified cgroup v2 hierarchy");
        return false;
    };
    if !parent.join("cgroup.kill").try_exists().unwrap() {
        missing("requires cgroup.kill in the process cgroup");
        return false;
    }
    let mut candidates = vec![parent.as_path()];
    if !matches!(placement, Cgroup::Direct) {
        // Direct Broker::new and watchdogs configured with aggregate limits
        // may place the tree beside the current process cgroup.
        if let Some(sibling_parent) = parent
            .parent()
            .filter(|p| *p != Path::new("/sys/fs/cgroup"))
        {
            candidates.push(sibling_parent);
        }
    }
    for base in candidates {
        let dir = base.join(format!("mind-test-probe-{}", uuid::Uuid::now_v7()));
        match fs::create_dir(&dir) {
            Ok(()) => {
                let controllers = fs::read_to_string(dir.join("cgroup.controllers"))
                    .expect("read probe controllers");
                fs::remove_dir(dir).expect("remove the empty cgroup probe");
                if !matches!(placement, Cgroup::WithControllers)
                    || ["memory", "pids"]
                        .iter()
                        .all(|required| controllers.split_whitespace().any(|v| &v == required))
                {
                    return observed("delegated cgroup v2");
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
                ) => {}
            Err(e) => panic!("could not probe cgroup delegation: {e}"),
        }
    }
    missing(if matches!(placement, Cgroup::WithControllers) {
        "requires a delegated cgroup with the memory and pids controllers"
    } else {
        "requires a writable delegated cgroup v2 subtree"
    });
    false
}

pub fn git_object_format(program: &str) -> bool {
    let output = std::process::Command::new(program)
        .args(["init", "-h"])
        .env("LC_ALL", "C")
        .output()
        .expect("query git init options");
    // Git returns 129 for a usage page (some versions return success).
    assert!(
        output.status.success() || output.status.code() == Some(129),
        "git init -h failed: {}",
        output.status
    );
    let help = [output.stdout, output.stderr].concat();
    let help = String::from_utf8_lossy(&help);
    assert!(
        help.contains("--bare"),
        "git init did not return its option list"
    );
    require(
        help.contains("--object-format"),
        "git init with --object-format",
    )
}
