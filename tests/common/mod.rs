//! Shared by the integration tests that emulate "Landlock unavailable".
//!
//! A seccomp filter makes the three Landlock system calls fail the way a
//! kernel without (`ENOSYS`, 38) or with a disabled (`EOPNOTSUPP`, 95) Landlock
//! would. The filter is inherited by every process the command starts, so a
//! `mind` that starts a child `mind mcp` confines both.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

use std::os::unix::process::CommandExt;
use std::process::Command;

/// seccomp filter: the Landlock syscalls (444 to 446) fail with `errno`.
pub fn landlock_blocker(errno: i32) -> Vec<libc::sock_filter> {
    let (bpf_ld, bpf_jmp, bpf_ret) = (0x00u16, 0x05u16, 0x06u16);
    let (w, abs, jeq, jge, jgt, k) = (0x00u16, 0x20u16, 0x10u16, 0x30u16, 0x20u16, 0x00u16);
    #[cfg(target_arch = "x86_64")]
    let arch = 0xC000_003Eu32;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xC000_00B7u32;
    let f = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    vec![
        f(bpf_ld | w | abs, 0, 0, 4),
        f(bpf_jmp | jeq | k, 0, 4, arch),
        f(bpf_ld | w | abs, 0, 0, 0),
        f(bpf_jmp | jge | k, 0, 2, 444),
        f(bpf_jmp | jgt | k, 1, 0, 446),
        f(bpf_ret | k, 0, 0, 0x0005_0000 | errno as u32),
        f(bpf_ret | k, 0, 0, 0x7FFF_0000),
    ]
}

/// Starts `c` with the Landlock calls failing with `errno`.
pub fn block_landlock(c: &mut Command, errno: i32) {
    let prog = landlock_blocker(errno);
    // SAFETY: only prctl, on data prepared before the fork.
    unsafe {
        c.pre_exec(move || {
            let fprog = libc::sock_fprog {
                len: prog.len() as u16,
                filter: prog.as_ptr() as *mut _,
            };
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Audit architecture of the filter below; `None` where this harness has no
/// verified constant (tests then skip instead of guessing).
pub fn audit_arch() -> Option<u32> {
    #[cfg(target_arch = "x86_64")]
    return Some(0xC000_003E);
    #[cfg(target_arch = "aarch64")]
    return Some(0xC000_00B7);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return None;
}

/// seccomp filter: each `(syscall number, optional value of argument 0)` fails
/// with `errno`; everything else is allowed. A foreign architecture is killed.
/// Numbers come from the `libc` crate for the build architecture (never
/// hard-coded). Returns `None` on architectures without a verified audit constant.
pub fn seccomp_denier(
    rules: &[(libc::c_long, Option<u32>)],
    errno: i32,
) -> Option<Vec<libc::sock_filter>> {
    let arch = audit_arch()?;
    let f = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    const LD_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JEQ: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const RET: u16 = 0x06;
    let deny = 0x0005_0000 | errno as u32;
    let mut p = vec![
        f(LD_ABS, 0, 0, 4),
        f(JEQ, 1, 0, arch),
        f(RET, 0, 0, 0x8000_0000), // KILL_PROCESS
    ];
    for (nr, arg0) in rules {
        match arg0 {
            None => {
                p.push(f(LD_ABS, 0, 0, 0));
                p.push(f(JEQ, 0, 1, *nr as u32));
                p.push(f(RET, 0, 0, deny));
            }
            Some(a) => {
                p.push(f(LD_ABS, 0, 0, 0));
                p.push(f(JEQ, 0, 3, *nr as u32));
                p.push(f(LD_ABS, 0, 0, 16)); // args[0], low 32 bits
                p.push(f(JEQ, 0, 1, *a));
                p.push(f(RET, 0, 0, deny));
            }
        }
    }
    p.push(f(RET, 0, 0, 0x7FFF_0000));
    Some(p)
}

/// Installs `prog` on the calling thread (and its later children). Test-only.
pub fn install_seccomp(prog: &[libc::sock_filter]) -> std::io::Result<()> {
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut _,
    };
    // SAFETY: prctl with a valid program that outlives the call.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Starts `c` with `prog` installed between fork and exec.
pub fn seccomp_command(c: &mut Command, prog: Vec<libc::sock_filter>) {
    // SAFETY: only prctl, on data prepared before the fork.
    unsafe {
        c.pre_exec(move || install_seccomp(&prog));
    }
}

/// The PRODUCT's socket-deny program (`mindspace::sandbox::seccomp::program`),
/// so the tests exercise the real filter and not a copy. `None` on
/// architectures without a verified audit constant.
pub fn socket_deny_filter() -> Option<Vec<libc::sock_filter>> {
    mindspace::sandbox::seccomp::program()
}

/// Makes each `(source, target)` pair true in the child: the source descriptor
/// appears at the fixed number `target` and stays open across `exec`.
///
/// The sources are first copied to numbers far above any target, so a source
/// that happens to sit at another pair's target number (likely in a big parallel
/// test run) is not overwritten, and a source that already equals its target
/// still loses its close-on-exec flag. Call only between fork and exec.
pub fn place_fds(pairs: &[(i32, i32)]) -> std::io::Result<()> {
    let mut staged = Vec::with_capacity(pairs.len());
    for &(from, _) in pairs {
        // SAFETY: fcntl(F_DUPFD) on a descriptor of this process.
        let tmp = unsafe { libc::fcntl(from, libc::F_DUPFD, 500) };
        if tmp < 0 {
            return Err(std::io::Error::last_os_error());
        }
        staged.push(tmp);
    }
    for (tmp, &(_, to)) in staged.iter().zip(pairs) {
        // SAFETY: dup2 onto the target number; dup2 clears close-on-exec.
        if unsafe { libc::dup2(*tmp, to) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    for tmp in staged {
        // SAFETY: closing the staging copies made above.
        unsafe { libc::close(tmp) };
    }
    Ok(())
}
