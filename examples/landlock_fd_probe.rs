//! M7.3b.2b gate: does an open directory descriptor override a Landlock domain that does not cover
//! it? Opens directory B and a file in it, restricts the process to directory A only, then tries to
//! use the descriptors. usage: landlock_fd_probe <A> <B>
use std::os::fd::AsRawFd;

use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
};

fn errno(r: std::io::Result<impl Sized>) -> String {
    match r {
        Ok(_) => "ALLOWED".into(),
        Err(e) => e
            .raw_os_error()
            .map(|n| {
                if n == libc::EACCES {
                    "EACCES".into()
                } else {
                    format!("errno {n}")
                }
            })
            .unwrap_or_else(|| e.to_string()),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dir_a, dir_b) = (&a[1], &a[2]);
    let b_dir = std::fs::File::open(dir_b).unwrap();
    let b_file = std::fs::File::open(format!("{dir_b}/secret.txt")).unwrap();
    let rs = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V3))
        .unwrap()
        .create()
        .unwrap()
        .add_rule(PathBeneath::new(
            PathFd::new(dir_a).unwrap(),
            AccessFs::from_read(ABI::V3),
        ))
        .unwrap();
    rs.restrict_self().unwrap();
    let via = |sub: &str| format!("/proc/self/fd/{}/{sub}", b_dir.as_raw_fd());
    println!(
        "RESULT open B by path: {}",
        errno(std::fs::read(format!("{dir_b}/secret.txt")))
    );
    println!(
        "RESULT open B/secret via the directory descriptor (/proc/self/fd/N/secret.txt): {}",
        errno(std::fs::read(via("secret.txt")))
    );
    println!(
        "RESULT list B via the directory descriptor: {}",
        errno(std::fs::read_dir(via(".")).map(|_| ()))
    );
    // SAFETY: openat relative to the inherited directory descriptor, the way fd-based code does it.
    let name = std::ffi::CString::new("secret.txt").unwrap();
    let fd = unsafe { libc::openat(b_dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY) };
    println!(
        "RESULT openat(dirfd of B, secret.txt): {}",
        if fd >= 0 {
            "ALLOWED".to_string()
        } else {
            errno(Err::<(), _>(std::io::Error::last_os_error()))
        }
    );
    let mut buf = Vec::new();
    use std::io::Read;
    let mut f = &b_file;
    println!(
        "RESULT read an already open file of B: {}",
        errno(f.read_to_end(&mut buf))
    );
    println!(
        "RESULT read in A: {}",
        errno(std::fs::read_dir(dir_a).map(|_| ()))
    );
}
