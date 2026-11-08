//! M7.2c: can Landlock and seccomp be combined for Mindspace without breaking
//! normal work? An ISOLATED experiment: the filter is installed only in a
//! short-lived child (this test binary, re-executed). Nothing here touches the
//! product: `mind mcp` installs no seccomp filter, and these tests do not change
//! that. Only temporary directories, self-made sockets and local test servers
//! are used; no real agent, SSH, database or system socket is contacted.
//!
//! The child applies, depending on the mode, nothing / a product-class Landlock
//! ruleset (filesystem rights of ABI 3, as `src/sandbox` handles them) / a
//! seccomp filter that fails `socket()` and `io_uring_setup()` / both, then
//! reports one `PROBE <name> <result>` line per probe. The parent asserts the
//! whole matrix. Syscall numbers come from the `libc` crate for the build
//! architecture; on an architecture without a verified audit constant the tests
//! skip (they never guess).
//!
//! What this shows: with the filter, no NEW socket of any family can be created
//! (so no new connection to a pathname/abstract/TCP/UDP peer), while sockets that
//! were already open or connected before the filter keep working. It does not
//! show that this is safe to ship (see docs/sandbox-capabilities.md).

#![cfg(target_os = "linux")]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use mindspace::doctor::capabilities::{probe_kernel, LandlockState, Tri};

const PRE_UNIX_FD: i32 = 100;
const PRE_TCP_FD: i32 = 101;
const EACCES: i32 = libc::EACCES;

// ------------------------------------------------------------------ child

fn errno_of(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => format!("E{n}"),
        None => "error".to_string(),
    }
}

fn report(name: &str, r: std::io::Result<()>) {
    match r {
        Ok(()) => println!("PROBE {name} ok"),
        Err(e) => println!("PROBE {name} {}", errno_of(&e)),
    }
}

fn read_banner(mut s: impl Read) -> std::io::Result<()> {
    let mut b = [0u8; 5];
    s.read_exact(&mut b)?;
    if &b == b"HELLO" {
        Ok(())
    } else {
        Err(std::io::Error::other("unexpected banner"))
    }
}

fn apply_landlock(allowed: &Path) -> Result<(), String> {
    use landlock::{
        Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };
    let all = AccessFs::from_all(ABI::V3);
    let read = AccessFs::from_read(ABI::V3);
    let mut rs = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(|e| e.to_string())?
        .create()
        .map_err(|e| e.to_string())?;
    for dir in ["/usr", "/lib", "/lib64", "/bin"] {
        if Path::new(dir).exists() {
            let fd = PathFd::new(dir).map_err(|e| e.to_string())?;
            rs = rs
                .add_rule(PathBeneath::new(fd, read))
                .map_err(|e| e.to_string())?;
        }
    }
    if Path::new("/etc/ld.so.cache").exists() {
        let fd = PathFd::new("/etc/ld.so.cache").map_err(|e| e.to_string())?;
        rs = rs
            .add_rule(PathBeneath::new(fd, AccessFs::ReadFile | AccessFs::Execute))
            .map_err(|e| e.to_string())?;
    }
    let fd = PathFd::new(allowed).map_err(|e| e.to_string())?;
    rs = rs
        .add_rule(PathBeneath::new(fd, all))
        .map_err(|e| e.to_string())?;
    let st = rs.restrict_self().map_err(|e| e.to_string())?;
    if st.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!("only {:?}", st.ruleset));
    }
    Ok(())
}

/// The experimental filter: every new socket (any family) and io_uring (which
/// could create sockets behind the filter's back) fail with EACCES.
fn socket_filter() -> Option<Vec<libc::sock_filter>> {
    common::seccomp_denier(
        &[(libc::SYS_socket, None), (libc::SYS_io_uring_setup, None)],
        EACCES,
    )
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Entry point of the child. A normal run of this test file executes it as a
/// no-op (the variable is unset); the parent tests re-execute the binary with it set.
#[test]
fn child_entry() {
    let mode = env("M72C_MODE");
    if mode.is_empty() {
        return;
    }
    // the harness prints "test child_entry ... " without a newline
    println!();
    let allowed = std::path::PathBuf::from(env("M72C_ALLOWED"));
    let outside_file = env("M72C_OUTSIDE_FILE");
    let sock_path = env("M72C_UNIX_PATH");
    let abs_name = env("M72C_ABSTRACT");
    let tcp4: u16 = env("M72C_TCP4").parse().unwrap_or(0);
    let tcp6: u16 = env("M72C_TCP6").parse().unwrap_or(0);
    let udp: u16 = env("M72C_UDP").parse().unwrap_or(0);

    let want_landlock = mode == "landlock" || mode == "both";
    let want_seccomp = mode == "seccomp" || mode == "both";

    if want_landlock {
        if let Err(e) = apply_landlock(&allowed) {
            println!("ABORT landlock {e}");
            std::process::exit(78);
        }
    }
    // Failure modes of the filter installation: a controlled abort BEFORE any probe.
    if mode == "seccomp_install_blocked" || mode == "seccomp_bad_program" {
        if mode == "seccomp_install_blocked" {
            // emulate a kernel/policy that forbids installing filters: a first
            // filter makes prctl(PR_SET_SECCOMP) fail with ENOSYS
            let first = common::seccomp_denier(
                &[(libc::SYS_prctl, Some(libc::PR_SET_SECCOMP as u32))],
                libc::ENOSYS,
            )
            .unwrap();
            // PR_SET_NO_NEW_PRIVS (38) is a different argument value, so this works
            common::install_seccomp(&first).expect("the first filter installs");
        }
        let prog = if mode == "seccomp_bad_program" {
            Vec::new() // an empty program is invalid (EINVAL)
        } else {
            socket_filter().unwrap()
        };
        match common::install_seccomp(&prog) {
            Ok(()) => {
                println!("ABORT-EXPECTED install unexpectedly succeeded");
                std::process::exit(1);
            }
            Err(e) => {
                println!("ABORT seccomp {}", errno_of(&e));
                std::process::exit(78);
            }
        }
    }
    if want_seccomp {
        let prog = socket_filter().unwrap();
        if let Err(e) = common::install_seccomp(&prog) {
            println!("ABORT seccomp {}", errno_of(&e));
            std::process::exit(78);
        }
    }

    let t = Duration::from_secs(2);
    // normal file access
    report(
        "file_inside_rw",
        (|| {
            let p = allowed.join("work.txt");
            std::fs::write(&p, b"x")?;
            std::fs::read(&p).map(|_| ())
        })(),
    );
    report(
        "file_outside_read",
        std::fs::read(&outside_file).map(|_| ()),
    );
    // new sockets: every family and kind
    report(
        "unix_path_connect",
        UnixStream::connect(&sock_path).and_then(|mut s| {
            s.set_read_timeout(Some(t))?;
            read_banner(&mut s)
        }),
    );
    report(
        "unix_abstract_connect",
        (|| {
            let addr = SocketAddr::from_abstract_name(abs_name.as_bytes())?;
            let mut s = UnixStream::connect_addr(&addr)?;
            s.set_read_timeout(Some(t))?;
            read_banner(&mut s)
        })(),
    );
    report(
        "tcp4_connect",
        TcpStream::connect(("127.0.0.1", tcp4)).and_then(|mut s| {
            s.set_read_timeout(Some(t))?;
            read_banner(&mut s)
        }),
    );
    if tcp6 != 0 {
        report(
            "tcp6_connect",
            TcpStream::connect(("::1", tcp6)).and_then(|mut s| {
                s.set_read_timeout(Some(t))?;
                read_banner(&mut s)
            }),
        );
    }
    report(
        "udp_send",
        UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.send_to(b"x", ("127.0.0.1", udp)).map(|_| ())),
    );
    // SAFETY: plain syscalls with constant arguments, results checked.
    report("netlink_socket", unsafe {
        let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, 0);
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            libc::close(fd);
            Ok(())
        }
    });
    report("io_uring_setup", unsafe {
        // Invalid arguments on purpose: ENOSYS/EPERM/EFAULT mean "not blocked by
        // the filter"; only the filter answers EACCES.
        let r = libc::syscall(libc::SYS_io_uring_setup, 0u32, std::ptr::null_mut::<u8>());
        if r < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            libc::close(r as i32);
            Ok(())
        }
    });
    // sockets opened before the filter, connected before the filter
    report(
        "preconnected_unix_read",
        read_banner(unsafe {
            use std::os::unix::io::FromRawFd;
            UnixStream::from_raw_fd(PRE_UNIX_FD)
        }),
    );
    report(
        "preconnected_tcp_read",
        read_banner(unsafe {
            use std::os::unix::io::FromRawFd;
            TcpStream::from_raw_fd(PRE_TCP_FD)
        }),
    );
    // IPC a normal run needs
    report("pipe", unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            libc::close(fds[0]);
            libc::close(fds[1]);
            Ok(())
        }
    });
    report("socketpair_anonymous", unsafe {
        let mut fds = [0i32; 2];
        if libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            libc::close(fds[0]);
            libc::close(fds[1]);
            Ok(())
        }
    });
    report(
        "thread_spawn",
        std::thread::spawn(|| ())
            .join()
            .map_err(|_| std::io::Error::other("panic")),
    );
    if Path::new("/usr/bin/true").exists() {
        report(
            "spawn_process",
            Command::new("/usr/bin/true").status().and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other("nonzero"))
                }
            }),
        );
    }
    std::process::exit(0);
}

// ----------------------------------------------------------------- parent

struct World {
    _dir: tempfile::TempDir,
    allowed: std::path::PathBuf,
    outside_file: std::path::PathBuf,
    sock_path: std::path::PathBuf,
    abs_name: String,
    tcp4: u16,
    tcp6: u16,
    udp: u16,
    _udp: UdpSocket,
    pre_unix: UnixStream,
    pre_tcp: TcpStream,
}

fn serve_hello<L, S>(listener: L, accept: fn(&L) -> std::io::Result<S>)
where
    L: Send + 'static,
    S: Write + Send + 'static,
{
    std::thread::spawn(move || {
        while let Ok(mut c) = accept(&listener) {
            let _ = c.write_all(b"HELLO");
            std::mem::forget(c); // keep it open: the peer may read later
        }
    });
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("allowed");
    let outside = dir.path().join("outside");
    std::fs::create_dir(&allowed).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let outside_file = outside.join("secret.txt");
    std::fs::write(&outside_file, "outside").unwrap();
    let sock_path = outside.join("fake-agent.sock");
    let ul = UnixListener::bind(&sock_path).unwrap();
    let abs_name = format!("m72c-{}", dir.path().file_name().unwrap().to_string_lossy());
    let al = UnixListener::bind_addr(&SocketAddr::from_abstract_name(abs_name.as_bytes()).unwrap())
        .unwrap();
    let t4 = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp4 = t4.local_addr().unwrap().port();
    let t6 = TcpListener::bind("[::1]:0").ok();
    let tcp6 = t6
        .as_ref()
        .map(|l| l.local_addr().unwrap().port())
        .unwrap_or(0);
    let u = UdpSocket::bind("127.0.0.1:0").unwrap();
    let udp = u.local_addr().unwrap().port();
    // pre-connected sockets (connected BEFORE the child is filtered)
    let pre_unix = UnixStream::connect(&sock_path).unwrap();
    let pre_tcp = TcpStream::connect(("127.0.0.1", tcp4)).unwrap();
    // every accepted connection is greeted with HELLO
    serve_hello(ul, |l| l.accept().map(|(s, _)| s));
    serve_hello(al, |l| l.accept().map(|(s, _)| s));
    serve_hello(t4, |l| l.accept().map(|(s, _)| s));
    if let Some(t6) = t6 {
        serve_hello(t6, |l| l.accept().map(|(s, _)| s));
    }
    World {
        _dir: dir,
        allowed,
        outside_file,
        sock_path,
        abs_name,
        tcp4,
        tcp6,
        udp,
        _udp: u,
        pre_unix,
        pre_tcp,
    }
}

struct Run {
    code: i32,
    lines: Vec<String>,
}

impl Run {
    fn get(&self, probe: &str) -> Option<String> {
        let prefix = format!("PROBE {probe} ");
        self.lines
            .iter()
            .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
    }
    fn abort(&self) -> Option<&String> {
        self.lines.iter().find(|l| l.starts_with("ABORT"))
    }
}

fn run_child(w: &World, mode: &str) -> Run {
    let exe = std::env::current_exe().unwrap();
    let mut c = Command::new(exe);
    c.args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env("M72C_MODE", mode)
        .env("M72C_ALLOWED", &w.allowed)
        .env("M72C_OUTSIDE_FILE", &w.outside_file)
        .env("M72C_UNIX_PATH", &w.sock_path)
        .env("M72C_ABSTRACT", &w.abs_name)
        .env("M72C_TCP4", w.tcp4.to_string())
        .env("M72C_TCP6", w.tcp6.to_string())
        .env("M72C_UDP", w.udp.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (u, t) = (w.pre_unix.as_raw_fd(), w.pre_tcp.as_raw_fd());
    // SAFETY: dup2 only, between fork and exec; clears CLOEXEC on the copies.
    unsafe {
        c.pre_exec(move || {
            if libc::dup2(u, PRE_UNIX_FD) < 0 || libc::dup2(t, PRE_TCP_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // The child must end by itself; a watchdog kills it so a hang can neither
    // block the suite nor leave a filtered process behind.
    let mut child = c.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the {mode} child did not finish within 30 s and was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = reader.join().unwrap();
    Run {
        code: status.code().unwrap_or(-1),
        lines: text
            .lines()
            .filter(|l| l.starts_with("PROBE ") || l.starts_with("ABORT"))
            .map(str::to_string)
            .collect(),
    }
}

/// `None`: the experiment cannot run here, with the reason printed.
fn prerequisites(need_landlock: bool, need_seccomp: bool) -> Option<World> {
    if need_seccomp {
        if common::audit_arch().is_none() {
            eprintln!(
                "skipped: no verified seccomp audit constant for this architecture (UNKNOWN)"
            );
            return None;
        }
        if probe_kernel().seccomp_filter != Tri::Yes {
            eprintln!("skipped: seccomp filters are not supported here");
            return None;
        }
    }
    if need_landlock {
        match probe_kernel().landlock {
            LandlockState::Available(v) if v >= 3 => {}
            other => {
                eprintln!("skipped: Landlock ABI 3+ not available ({other:?})");
                return None;
            }
        }
    }
    Some(world())
}

fn expect(r: &Run, probe: &str, want: &str) {
    let got = r
        .get(probe)
        .unwrap_or_else(|| format!("<missing; lines: {:?}>", r.lines));
    assert_eq!(got, want, "probe {probe}");
}

const DENIED: &str = "E13";

#[test]
fn the_unconfined_baseline_reaches_everything() {
    let Some(w) = prerequisites(false, false) else {
        return;
    };
    let r = run_child(&w, "none");
    assert_eq!(r.code, 0, "{:?}", r.lines);
    for p in [
        "file_inside_rw",
        "file_outside_read",
        "unix_path_connect",
        "unix_abstract_connect",
        "tcp4_connect",
        "udp_send",
        "preconnected_unix_read",
        "preconnected_tcp_read",
        "pipe",
        "socketpair_anonymous",
        "thread_spawn",
    ] {
        expect(&r, p, "ok");
    }
    // io_uring may be disabled by the host; only the filter answers EACCES
    assert_ne!(r.get("io_uring_setup").as_deref(), Some(DENIED));
}

#[test]
fn landlock_alone_fences_files_but_not_sockets() {
    let Some(w) = prerequisites(true, false) else {
        return;
    };
    let r = run_child(&w, "landlock");
    assert_eq!(r.code, 0, "{:?}", r.lines);
    expect(&r, "file_inside_rw", "ok");
    expect(&r, "file_outside_read", DENIED);
    // the documented open channels at the project's ABI-3-class rule set:
    for p in [
        "unix_path_connect",
        "unix_abstract_connect",
        "tcp4_connect",
        "udp_send",
    ] {
        expect(&r, p, "ok");
    }
    expect(&r, "preconnected_unix_read", "ok");
    expect(&r, "preconnected_tcp_read", "ok");
    if Path::new("/usr/bin/true").exists() {
        expect(&r, "spawn_process", "ok");
    }
}

#[test]
fn seccomp_alone_stops_new_sockets_but_not_existing_ones_or_files() {
    let Some(w) = prerequisites(false, true) else {
        return;
    };
    let r = run_child(&w, "seccomp");
    assert_eq!(r.code, 0, "{:?}", r.lines);
    for p in [
        "unix_path_connect",
        "unix_abstract_connect",
        "tcp4_connect",
        "udp_send",
        "netlink_socket",
        "io_uring_setup",
    ] {
        expect(&r, p, DENIED);
    }
    if w.tcp6 != 0 {
        expect(&r, "tcp6_connect", DENIED);
    }
    // remaining boundary: descriptors that already exist keep working
    expect(&r, "preconnected_unix_read", "ok");
    expect(&r, "preconnected_tcp_read", "ok");
    // seccomp is not a filesystem fence
    expect(&r, "file_outside_read", "ok");
    expect(&r, "file_inside_rw", "ok");
}

#[test]
fn landlock_plus_seccomp_closes_both_and_normal_work_continues() {
    let Some(w) = prerequisites(true, true) else {
        return;
    };
    let r = run_child(&w, "both");
    assert_eq!(r.code, 0, "{:?}", r.lines);
    expect(&r, "file_outside_read", DENIED);
    expect(&r, "file_inside_rw", "ok");
    for p in [
        "unix_path_connect",
        "unix_abstract_connect",
        "tcp4_connect",
        "udp_send",
        "netlink_socket",
        "io_uring_setup",
    ] {
        expect(&r, p, DENIED);
    }
    if w.tcp6 != 0 {
        expect(&r, "tcp6_connect", DENIED);
    }
    // what a normal run still needs
    for p in [
        "pipe",
        "socketpair_anonymous",
        "thread_spawn",
        "preconnected_unix_read",
        "preconnected_tcp_read",
    ] {
        expect(&r, p, "ok");
    }
    if Path::new("/usr/bin/true").exists() {
        expect(&r, "spawn_process", "ok");
    }
}

#[test]
fn a_filter_that_cannot_be_installed_aborts_the_child_before_any_probe() {
    for mode in ["seccomp_install_blocked", "seccomp_bad_program"] {
        let Some(w) = prerequisites(false, true) else {
            return;
        };
        let r = run_child(&w, mode);
        assert_eq!(r.code, 78, "{mode}: {:?}", r.lines);
        let abort = r
            .abort()
            .unwrap_or_else(|| panic!("{mode}: no abort line: {:?}", r.lines));
        assert!(abort.starts_with("ABORT seccomp E"), "{mode}: {abort}");
        assert!(
            !r.lines.iter().any(|l| l.starts_with("PROBE ")),
            "{mode}: work continued after the failed installation: {:?}",
            r.lines
        );
    }
}

#[test]
fn the_parent_is_not_affected_by_the_childs_filters() {
    // The filter lives in the child only: this process can still open a socket.
    let Some(_w) = prerequisites(true, true) else {
        return;
    };
    let r = run_child(&_w, "both");
    assert_eq!(r.code, 0, "{:?}", r.lines);
    assert!(UdpSocket::bind("127.0.0.1:0").is_ok());
    assert!(std::fs::read(&_w.outside_file).is_ok());
}
