//! Test support for the extraction worker (M7.3b.2a): enters the SAME confinement as
//! `mind extraction-worker` (`sandbox::enter_extraction_worker`, descriptors 3 and 4 as the parent
//! places them) and then tries the things a hostile worker would try, printing what the kernel
//! answered. usage: worker_probe <read|list|write>:<path> | socket:<inet|inet6|unix|netlink> |
//! fd:<n> | parent-environ:<pid> | exec:<program> | sh:<command> ... `exec` tries to START a program
//! (`<program> -c true`) and reports what the kernel answered; `sh` runs `/bin/sh -c <command>` and
//! reports its exit code. Prints one `RESULT <arg> <outcome>` line per argument.
use std::os::fd::{FromRawFd, OwnedFd};

fn outcome<T>(r: std::io::Result<T>) -> String {
    match r {
        Ok(_) => "ALLOWED".into(),
        Err(e) => match e.raw_os_error() {
            Some(libc::EACCES) => "EACCES".into(),
            Some(libc::EPERM) => "EPERM".into(),
            Some(libc::EBADF) => "EBADF".into(),
            Some(libc::ENOENT) => "ENOENT".into(),
            Some(n) => format!("errno {n}"),
            None => e.to_string(),
        },
    }
}

fn main() {
    // SAFETY: the test places the root directory at descriptor 4 (and a channel at 3).
    let root = unsafe { OwnedFd::from_raw_fd(4) };
    let report = match mindspace::sandbox::enter_extraction_worker(&root, &[3, 4]) {
        Ok(r) => r,
        Err(e) => {
            println!("SANDBOX-REFUSED {e}");
            std::process::exit(78);
        }
    };
    println!("SANDBOX {}", serde_json::to_string(&report).unwrap());
    for arg in std::env::args().skip(1) {
        let (kind, rest) = arg.split_once(':').unwrap_or((arg.as_str(), ""));
        let res = match kind {
            "read" => outcome(std::fs::read(rest)),
            "list" => outcome(std::fs::read_dir(rest).map(|_| ())),
            "write" => outcome(std::fs::write(rest, b"x")),
            "create" => outcome(std::fs::File::create(rest)),
            "parent-environ" => outcome(std::fs::read(format!("/proc/{rest}/environ"))),
            "exec" => match std::process::Command::new(rest)
                .args(["-c", "true"])
                .stdin(std::process::Stdio::null())
                .status()
            {
                Ok(st) if st.success() => "ALLOWED".into(),
                Ok(st) => format!("started, exit {:?}", st.code()),
                Err(e) => outcome::<()>(Err(e)),
            },
            "sh" => match std::process::Command::new("/bin/sh")
                .args(["-c", rest])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Ok(st) => format!("exit {:?}", st.code()),
                Err(e) => outcome::<()>(Err(e)),
            },
            "proc-list" => outcome(std::fs::read_dir("/proc").map(|_| ())),
            "socket" => {
                let domain = match rest {
                    "inet" => libc::AF_INET,
                    "inet6" => libc::AF_INET6,
                    "unix" => libc::AF_UNIX,
                    _ => libc::AF_NETLINK,
                };
                // SAFETY: a socket attempt; a descriptor, if any, is closed at once.
                let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                    "ALLOWED".into()
                } else {
                    outcome::<()>(Err(std::io::Error::last_os_error()))
                }
            }
            "fd" => {
                // an inherited descriptor: can it still be used?
                let n: i32 = rest.parse().unwrap_or(-1);
                let mut buf = [0u8; 8];
                // SAFETY: reading from a descriptor number; EBADF if it was closed.
                let r = unsafe { libc::read(n, buf.as_mut_ptr().cast(), buf.len()) };
                if r >= 0 {
                    "ALLOWED".into()
                } else {
                    outcome::<()>(Err(std::io::Error::last_os_error()))
                }
            }
            "fds" => {
                let mut v: Vec<String> = std::fs::read_dir("/proc/self/fd")
                    .map(|d| {
                        d.flatten()
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                v.sort();
                v.join(",")
            }
            other => format!("unknown probe {other}"),
        };
        println!("RESULT {arg} {res}");
    }
}
