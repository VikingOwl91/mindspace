//! Socket-deny seccomp filter for the sandboxed `mind mcp` (M7.2e).
//!
//! # Threat model
//!
//! * **Who is restricted:** only the process tree of a sandboxed `mind mcp`
//!   (roots configured, no `--insecure-no-sandbox`): the stage-1 process, which
//!   re-executes itself into stage 2, and every child of stage 2 (Git). No other
//!   `mind` command installs this filter; `mind mcp` without roots does not.
//! * **Which threads:** a seccomp filter is attached to the calling thread and
//!   inherited by the threads it creates and by `execve`. It is installed on the
//!   thread that later performs the stage-1 `exec`, so stage 2 starts with it
//!   no matter what other threads did. As a second guard the installation is
//!   refused unless the process has exactly one thread.
//! * **Descriptors at installation:** `close_inherited_fds` has closed
//!   everything above stderr; installation is refused if anything above stderr
//!   is open. 0, 1 and 2 stay as they are: the MCP transport. If a client hands
//!   over a socket there, it remains usable (a documented limit).
//! * **Task of the filter:** no NEW socket can be created (`socket`), no
//!   `io_uring` ring (it can create sockets itself), and no descriptor can be
//!   taken from or injected into another process of the same user
//!   (`pidfd_getfd`, `ptrace`, `process_vm_*`). `socketpair` and pipes stay
//!   allowed: they are anonymous and have no external peer.
//! * **Task of Landlock:** filesystem access (`src/sandbox/mod.rs`). Landlock does
//!   not restrict sockets at the ABI the project requires.
//! * **Not provided:** network or IPC isolation. Descriptors that are already
//!   open (stdio), `SCM_RIGHTS` received over them, signals, reading `/proc`.
//!
//! # Policy facts
//!
//! The program is architecture-specific. Only x86_64 has a verified audit
//! architecture and is activated; every other architecture reports
//! "not applied" explicitly. On x86_64 the x32 ABI shares the architecture
//! value and marks its syscalls with bit 30 of the number; those are killed,
//! like every foreign architecture, so no other numbering can reach `socket`.
//!
//! `Seccomp: 2` in `/proc/self/status` only says that some filter is active.
//! [`verify_active`] therefore also performs negative probes that must fail with
//! `EACCES` and positive controls that must work.

use serde_json::{json, Value};

/// `AUDIT_ARCH_X86_64`; `None` where no constant has been verified.
pub fn audit_arch() -> Option<u32> {
    #[cfg(target_arch = "x86_64")]
    return Some(0xC000_003E);
    #[cfg(not(target_arch = "x86_64"))]
    return None;
}

/// x32 syscalls carry this bit in the number (x86_64 only).
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7FFF_0000;
const RET_ERRNO: u32 = 0x0005_0000;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;

/// The denied system calls, with the reason that justifies each one.
pub fn denied() -> Vec<(&'static str, libc::c_long)> {
    vec![
        ("socket", libc::SYS_socket),
        ("io_uring_setup", libc::SYS_io_uring_setup),
        ("pidfd_getfd", libc::SYS_pidfd_getfd),
        ("ptrace", libc::SYS_ptrace),
        ("process_vm_readv", libc::SYS_process_vm_readv),
        ("process_vm_writev", libc::SYS_process_vm_writev),
    ]
}

/// The BPF program: foreign architecture or x32 number -> kill the process;
/// a denied call -> `EACCES`; everything else -> allow. `None` on an
/// architecture without a verified audit constant.
pub fn program() -> Option<Vec<libc::sock_filter>> {
    let arch = audit_arch()?;
    let f = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    let nrs: Vec<u32> = denied().iter().map(|(_, n)| *n as u32).collect();
    let n = nrs.len() as u8;
    let mut p = vec![
        f(BPF_LD_W_ABS, 0, 0, 4), // seccomp_data.arch
        f(BPF_JEQ_K, 1, 0, arch),
        f(BPF_RET_K, 0, 0, RET_KILL_PROCESS),
        f(BPF_LD_W_ABS, 0, 0, 0), // seccomp_data.nr
        f(BPF_JGE_K, 0, 1, X32_SYSCALL_BIT),
        f(BPF_RET_K, 0, 0, RET_KILL_PROCESS),
    ];
    for (i, nr) in nrs.iter().enumerate() {
        // on a match jump over the remaining comparisons and the ALLOW to the DENY
        p.push(f(BPF_JEQ_K, n - i as u8, 0, *nr));
    }
    p.push(f(BPF_RET_K, 0, 0, RET_ALLOW));
    p.push(f(BPF_RET_K, 0, 0, RET_ERRNO | libc::EACCES as u32));
    Some(p)
}

// ----------------------------------------------------------------- kernel

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Yes,
    /// The kernel was built without seccomp (`PR_GET_SECCOMP` -> `EINVAL`).
    No,
    /// Cannot tell, for example a policy blocks the query. NOT treated as "no".
    Unknown,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Pure classification of the three read-only observations.
///
/// * `prctl_get`: result of `prctl(PR_GET_SECCOMP)` (`Ok` or the errno).
/// * `status_has_seccomp_line`: whether `/proc/thread-self/status` has a
///   `Seccomp:` line (it exists exactly when the kernel has `CONFIG_SECCOMP`);
///   `None` if the file could not be read.
/// * `action_avail`: `seccomp(SECCOMP_GET_ACTION_AVAIL, ERRNO)` succeeded.
///
/// `No` needs BOTH `EINVAL` from `prctl` AND the missing status line: a policy
/// that happens to answer `EINVAL`/`ENOSYS` for one call must not look like an
/// unsupported kernel, because "unsupported" means "do not install", which
/// would be a silent downgrade.
pub fn classify_support(
    prctl_get: Result<(), i32>,
    status_has_seccomp_line: Option<bool>,
    action_avail: bool,
) -> Support {
    match (prctl_get, status_has_seccomp_line) {
        (Err(e), Some(false)) if e == libc::EINVAL => Support::No,
        (Ok(()), _) if action_avail => Support::Yes,
        _ => Support::Unknown,
    }
}

/// Read-only observations, classified by [`classify_support`].
pub fn kernel_support() -> Support {
    // SAFETY: PR_GET_SECCOMP takes no pointer and changes nothing.
    let r = unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    let prctl_get = if r < 0 { Err(errno()) } else { Ok(()) };
    let status = std::fs::read_to_string("/proc/thread-self/status")
        .ok()
        .map(|s| s.lines().any(|l| l.starts_with("Seccomp:")));
    let action: u32 = RET_ERRNO;
    // SAFETY: `action` outlives the call; SECCOMP_GET_ACTION_AVAIL only reads it.
    let q = unsafe { libc::syscall(libc::SYS_seccomp, 2u32, 0u32, &action as *const u32) };
    classify_support(prctl_get, status, q == 0)
}

/// Installs the program on the calling thread (`NO_NEW_PRIVS` first). The
/// error is the raw errno.
pub fn install(prog: &[libc::sock_filter]) -> Result<(), i32> {
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut _,
    };
    // SAFETY: valid program that outlives the calls.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(errno());
        }
        if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog) != 0 {
            return Err(errno());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- policy

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotApplied {
    UnsupportedArchitecture(&'static str),
    KernelWithoutSeccomp,
}

impl NotApplied {
    pub fn reason(&self) -> String {
        match self {
            NotApplied::UnsupportedArchitecture(a) => {
                format!("architecture {a} is not supported by the filter (x86_64 only)")
            }
            NotApplied::KernelWithoutSeccomp => "the kernel was built without seccomp".into(),
        }
    }
}

/// What this process MUST have, derived locally from architecture and kernel,
/// never from an environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    Required,
    NotApplied(NotApplied),
}

pub fn expectation_for(arch_supported: bool, kernel: Support) -> Expectation {
    if !arch_supported {
        Expectation::NotApplied(NotApplied::UnsupportedArchitecture(std::env::consts::ARCH))
    } else if kernel == Support::No {
        Expectation::NotApplied(NotApplied::KernelWithoutSeccomp)
    } else {
        // Yes and Unknown: the filter is required; Unknown must not downgrade.
        Expectation::Required
    }
}

pub fn expectation() -> Expectation {
    expectation_for(audit_arch().is_some(), kernel_support())
}

/// What was proven about the filter in a running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub arch: &'static str,
    /// Result of each negative probe (all `EACCES`) and positive control.
    pub probes: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Enforced(Evidence),
    NotApplied(NotApplied),
    /// Installation or verification failed (the repository tools are off).
    Failed(String),
    /// Not a sandboxed start; nothing was evaluated.
    NotEvaluated,
}

impl Status {
    /// The additive, typed report field (`seccomp` in `repo_outline`).
    pub fn to_json(&self) -> Value {
        let scope = "new sockets and the listed alternative paths only; descriptors open at start (stdio), socketpair and signals are not covered";
        match self {
            Status::Enforced(e) => json!({
                "state": "enforced",
                "scope": scope,
                "arch": e.arch,
                "denied_syscalls": denied().iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                "probes": e.probes.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
            }),
            Status::NotApplied(r) => json!({"state": "not_applied", "reason": r.reason()}),
            Status::Failed(r) => json!({"state": "failed", "reason": r}),
            Status::NotEvaluated => json!({"state": "not_evaluated"}),
        }
    }
}

/// Number of threads of this process.
fn thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(|d| d.count())
}

fn seccomp_mode() -> Option<u32> {
    // The calling thread: a filter is per thread, and `/proc/self` would show
    // the main thread of the process.
    std::fs::read_to_string("/proc/thread-self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Seccomp:"))
        .and_then(|v| v.trim().parse().ok())
}

fn close_if_open(fd: libc::c_long) {
    if fd >= 0 {
        // SAFETY: closing a descriptor a probe just unexpectedly obtained.
        unsafe { libc::close(fd as i32) };
    }
}

/// Behavioural evidence that the policy is active: the filter mode, negative
/// probes that must fail with `EACCES` (an unfiltered kernel answers
/// differently for each) and positive controls that must work. Mode `2` alone
/// is NOT accepted as proof of the policy.
pub fn verify_active() -> Result<Evidence, String> {
    match seccomp_mode() {
        Some(2) => {}
        other => return Err(format!("the seccomp filter mode is {other:?}, expected 2")),
    }
    let mut probes: Vec<(String, String)> = Vec::new();
    let mut check = |name: &str, r: libc::c_long| -> Result<(), String> {
        if r >= 0 {
            close_if_open(r);
            return Err(format!("`{name}` was not denied (returned {r})"));
        }
        let e = errno();
        if e != libc::EACCES {
            return Err(format!("`{name}` failed with errno {e}, expected EACCES"));
        }
        probes.push((name.to_string(), "EACCES".to_string()));
        Ok(())
    };
    // SAFETY (all probes): constant arguments; the calls either fail or the
    // result is closed. None of them can affect another process: the pid
    // arguments are 0/-1 and the lengths are 0.
    unsafe {
        for (label, domain) in [
            ("socket(AF_UNIX)", libc::AF_UNIX),
            ("socket(AF_INET)", libc::AF_INET),
            ("socket(AF_INET6)", libc::AF_INET6),
            ("socket(AF_NETLINK)", libc::AF_NETLINK),
        ] {
            check(
                label,
                libc::socket(domain, libc::SOCK_STREAM, 0) as libc::c_long,
            )?;
        }
        let null = std::ptr::null::<u8>();
        check(
            "io_uring_setup",
            libc::syscall(libc::SYS_io_uring_setup, 0u32, null),
        )?;
        check(
            "pidfd_getfd",
            libc::syscall(libc::SYS_pidfd_getfd, -1i32, -1i32, 0u32),
        )?;
        check(
            "ptrace",
            libc::syscall(
                libc::SYS_ptrace,
                1 as libc::c_long,
                0 as libc::c_long,
                null,
                null,
            ),
        )?;
        check(
            "process_vm_readv",
            libc::syscall(libc::SYS_process_vm_readv, 0, null, 0, null, 0, 0),
        )?;
        check(
            "process_vm_writev",
            libc::syscall(libc::SYS_process_vm_writev, 0, null, 0, null, 0, 0),
        )?;
        // controls: the filter must not be broader than intended
        let mut fds = [0i32; 2];
        if libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) != 0 {
            return Err(format!("control socketpair failed with errno {}", errno()));
        }
        libc::close(fds[0]);
        libc::close(fds[1]);
        probes.push(("socketpair(AF_UNIX) [control]".into(), "allowed".into()));
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(format!("control pipe failed with errno {}", errno()));
        }
        libc::close(fds[0]);
        libc::close(fds[1]);
        probes.push(("pipe [control]".into(), "allowed".into()));
    }
    Ok(Evidence {
        arch: std::env::consts::ARCH,
        probes,
    })
}

/// Stage 1. `Ok(Status)` when the process is as required (`Enforced`) or the
/// platform is explicitly unsupported (`NotApplied`); `Err` means the filter
/// was required and could not be established: the caller MUST NOT continue
/// with the repository tools on. `installer` is a test seam.
pub fn apply_with(
    expectation: Expectation,
    open_fds: &dyn Fn() -> Vec<i32>,
    threads: &dyn Fn() -> Option<usize>,
    installer: &dyn Fn(&[libc::sock_filter]) -> Result<(), i32>,
) -> Result<Status, String> {
    let arch_note = std::env::consts::ARCH;
    match expectation {
        Expectation::NotApplied(r) => Ok(Status::NotApplied(r)),
        Expectation::Required => {
            let prog = program().ok_or_else(|| {
                format!("no seccomp program for architecture {arch_note} (internal inconsistency)")
            })?;
            match threads() {
                Some(1) => {}
                other => {
                    return Err(format!(
                        "refusing to install the seccomp filter: expected exactly one thread, found {other:?}"
                    ))
                }
            }
            let fds = open_fds();
            if !fds.is_empty() {
                return Err(format!(
                    "refusing to install the seccomp filter: descriptors above stderr are open: {fds:?}"
                ));
            }
            installer(&prog).map_err(|e| {
                format!(
                    "the seccomp filter could not be installed (errno {e}: {})",
                    std::io::Error::from_raw_os_error(e)
                )
            })?;
            verify_active()
                .map(Status::Enforced)
                .map_err(|e| format!("the seccomp filter was installed but does not verify: {e}"))
        }
    }
}

/// Stage 1 with the real kernel interfaces.
pub fn apply() -> Result<Status, String> {
    apply_with(
        expectation(),
        &super::open_fds_above_stderr,
        &thread_count,
        &install,
    )
}

/// Stage 2: the expectation is derived locally again. A required filter that is
/// not demonstrably active is an error (the start is refused), whatever any
/// environment variable says.
pub fn verify_stage2() -> Result<Status, String> {
    match expectation() {
        Expectation::NotApplied(r) => Ok(Status::NotApplied(r)),
        Expectation::Required => verify_active()
            .map(Status::Enforced)
            .map_err(|e| format!("the required seccomp policy is not active: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny interpreter for exactly the instructions the program uses.
    fn run(prog: &[libc::sock_filter], nr: u32, arch: u32) -> u32 {
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = &prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => a = if i.k == 0 { nr } else { arch },
                BPF_JEQ_K => pc += if a == i.k { i.jt } else { i.jf } as usize,
                BPF_JGE_K => pc += if a >= i.k { i.jt } else { i.jf } as usize,
                BPF_RET_K => return i.k,
                c => panic!("unexpected opcode {c:#x}"),
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn the_program_decides_by_architecture_number_and_x32_bit() {
        let prog = program().unwrap();
        let arch = audit_arch().unwrap();
        let eacces = RET_ERRNO | libc::EACCES as u32;
        for (name, nr) in denied() {
            assert_eq!(run(&prog, nr as u32, arch), eacces, "{name}");
        }
        // ordinary calls pass
        for nr in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_socketpair,
            libc::SYS_pipe2,
            libc::SYS_connect,
            libc::SYS_execve,
            libc::SYS_clone,
        ] {
            assert_eq!(run(&prog, nr as u32, arch), RET_ALLOW, "{nr}");
        }
        // a foreign architecture is killed, even for allowed numbers, and
        // for the number of `socket` in that other numbering
        for other in [0x4000_0003u32, 0xC000_00B7, 0x4000_0028] {
            assert_eq!(run(&prog, libc::SYS_read as u32, other), RET_KILL_PROCESS);
            assert_eq!(run(&prog, libc::SYS_socket as u32, other), RET_KILL_PROCESS);
        }
        // x32: same architecture value, bit 30 set -> killed, including x32 `socket`
        for nr in [
            X32_SYSCALL_BIT | libc::SYS_socket as u32,
            X32_SYSCALL_BIT,
            X32_SYSCALL_BIT | libc::SYS_read as u32,
        ] {
            assert_eq!(run(&prog, nr, arch), RET_KILL_PROCESS, "{nr:#x}");
        }
        // the boundary: the highest non-x32 number still goes through the normal rules
        assert_eq!(run(&prog, X32_SYSCALL_BIT - 1, arch), RET_ALLOW);
    }

    #[test]
    #[cfg(not(target_arch = "x86_64"))]
    fn without_a_verified_architecture_there_is_no_program() {
        assert!(program().is_none());
        assert!(audit_arch().is_none());
    }

    #[test]
    fn only_a_missing_kernel_feature_counts_as_unsupported() {
        let e = |n| Err::<(), i32>(n);
        // CONFIG_SECCOMP off: EINVAL and no status line
        assert_eq!(
            classify_support(e(libc::EINVAL), Some(false), false),
            Support::No
        );
        // EINVAL from a policy while the kernel plainly has the status line
        assert_eq!(
            classify_support(e(libc::EINVAL), Some(true), false),
            Support::Unknown
        );
        // EINVAL but /proc unreadable: cannot tell
        assert_eq!(
            classify_support(e(libc::EINVAL), None, false),
            Support::Unknown
        );
        // other errors, ENOSYS from a policy
        assert_eq!(
            classify_support(e(libc::EPERM), Some(false), false),
            Support::Unknown
        );
        assert_eq!(
            classify_support(e(libc::ENOSYS), Some(false), false),
            Support::Unknown
        );
        // supported, with and without the action query
        assert_eq!(classify_support(Ok(()), Some(true), true), Support::Yes);
        assert_eq!(
            classify_support(Ok(()), Some(true), false),
            Support::Unknown
        );
    }

    #[test]
    fn the_decision_table() {
        use Expectation::*;
        assert_eq!(expectation_for(true, Support::Yes), Required);
        assert_eq!(expectation_for(true, Support::Unknown), Required); // no downgrade
        assert_eq!(
            expectation_for(true, Support::No),
            NotApplied(self::NotApplied::KernelWithoutSeccomp)
        );
        assert!(matches!(
            expectation_for(false, Support::Yes),
            NotApplied(self::NotApplied::UnsupportedArchitecture(_))
        ));
        assert!(matches!(
            expectation_for(false, Support::Unknown),
            NotApplied(self::NotApplied::UnsupportedArchitecture(_))
        ));
    }

    fn none() -> Vec<i32> {
        vec![]
    }
    fn one() -> Option<usize> {
        Some(1)
    }

    #[test]
    fn a_required_filter_that_cannot_be_installed_is_an_error_not_a_downgrade() {
        #[cfg(target_arch = "x86_64")]
        {
            for e in [libc::EPERM, libc::ENOSYS, libc::EINVAL, libc::ENOMEM] {
                let r = apply_with(Expectation::Required, &none, &one, &|_| Err(e));
                assert!(r.unwrap_err().contains("could not be installed"), "{e}");
            }
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn installation_is_refused_with_other_threads_or_open_descriptors() {
        let ok = |_: &[libc::sock_filter]| -> Result<(), i32> { panic!("must not be called") };
        let r = apply_with(Expectation::Required, &none, &|| Some(2), &ok);
        assert!(r.unwrap_err().contains("exactly one thread"));
        let r = apply_with(Expectation::Required, &none, &|| None, &ok);
        assert!(r.unwrap_err().contains("exactly one thread"));
        let r = apply_with(Expectation::Required, &|| vec![7], &one, &ok);
        assert!(r.unwrap_err().contains("descriptors above stderr"));
    }

    #[test]
    fn an_unsupported_platform_is_reported_not_hidden() {
        let r = apply_with(
            Expectation::NotApplied(NotApplied::UnsupportedArchitecture("riscv64")),
            &none,
            &one,
            &|_| panic!("nothing may be installed"),
        )
        .unwrap();
        let j = r.to_json();
        assert_eq!(j["state"], "not_applied");
        assert!(j["reason"].as_str().unwrap().contains("not supported"));
    }

    #[test]
    fn this_test_process_is_not_filtered() {
        // verify_active must reject a process without the filter (mode 0 or a
        // foreign filter that does not deny the probes): nothing here installs one.
        if seccomp_mode() != Some(2) {
            assert!(verify_active().unwrap_err().contains("filter mode"));
        }
    }

    #[test]
    fn json_report_shapes() {
        assert_eq!(Status::NotEvaluated.to_json()["state"], "not_evaluated");
        assert_eq!(Status::Failed("x".into()).to_json()["state"], "failed");
        let e = Status::Enforced(Evidence {
            arch: "x86_64",
            probes: vec![("socket(AF_UNIX)".into(), "EACCES".into())],
        })
        .to_json();
        assert_eq!(e["state"], "enforced");
        assert_eq!(e["probes"]["socket(AF_UNIX)"], "EACCES");
        assert!(e["scope"]
            .as_str()
            .unwrap()
            .contains("descriptors open at start"));
        assert!(e["denied_syscalls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "pidfd_getfd"));
    }
}
