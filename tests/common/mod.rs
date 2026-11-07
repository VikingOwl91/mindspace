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
