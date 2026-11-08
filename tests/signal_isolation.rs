//! M7.3b F2: can a sandboxed process signal processes outside its sandbox?
//!
//! Isolated children only: a re-executed copy of this test binary applies a
//! restriction to itself and then tries every way to signal a victim that the
//! parent started (the parent itself, a `sleep` in another process group, and a
//! process group of two `sleep`s). The parent counts the signals that were
//! DELIVERED, because `kill(pid, 0)` only asks for permission.
//!
//! Modes (env `M73_SIG_MODE`):
//!  * `fs`      Landlock filesystem rules only (what the product applies at ABI 3),
//!  * `scope`   plus `Scope::Signal` (Landlock ABI 6, Linux 6.12),
//!  * `seccomp` plus a filter that denies the signal system calls.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// The counters are process-wide: one scenario at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static USR1: AtomicU32 = AtomicU32::new(0);
static USR2: AtomicU32 = AtomicU32::new(0);
static IO: AtomicU32 = AtomicU32::new(0);
static RT: AtomicU32 = AtomicU32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    match sig {
        libc::SIGUSR1 => USR1.fetch_add(1, Ordering::SeqCst),
        libc::SIGUSR2 => USR2.fetch_add(1, Ordering::SeqCst),
        libc::SIGIO => IO.fetch_add(1, Ordering::SeqCst),
        _ => RT.fetch_add(1, Ordering::SeqCst),
    };
}

fn install_handlers() {
    for sig in [
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGIO,
        libc::SIGRTMIN() + 1,
    ] {
        // SAFETY: a plain async-signal-safe handler that only increments atomics.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

fn errname() -> String {
    format!(
        "errno {}",
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    )
}

fn sys_ret(r: libc::c_long) -> String {
    if r >= 0 {
        "returned 0 (permitted)".into()
    } else {
        errname()
    }
}

// ------------------------------------------------------------------ child

#[test]
fn signal_child() {
    let Some(mode) = std::env::var("M73_SIG_MODE").ok() else {
        return;
    };
    println!();
    let get = |k: &str| -> i32 { std::env::var(k).unwrap().parse().unwrap() };
    let (victim, sleeper, group) = (get("M73_VICTIM"), get("M73_SLEEPER"), get("M73_GROUP"));
    // restriction
    {
        use landlock::{
            Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, Scope, ABI,
        };
        let fs = AccessFs::from_all(ABI::V3);
        let mut rs = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(fs)
            .expect("fs");
        if mode == "scope" {
            rs = rs
                .scope(Scope::Signal)
                .expect("scope signal (needs Landlock ABI 6)");
        }
        let created = rs.create().expect("create");
        let st = created.restrict_self().expect("restrict");
        assert_eq!(st.ruleset, landlock::RulesetStatus::FullyEnforced);
    }
    if mode == "seccomp" {
        let denied = [
            libc::SYS_kill,
            libc::SYS_tkill,
            libc::SYS_tgkill,
            libc::SYS_rt_sigqueueinfo,
            libc::SYS_rt_tgsigqueueinfo,
            libc::SYS_pidfd_send_signal,
        ];
        let rules: Vec<_> = denied.iter().map(|n| (*n, None)).collect();
        let prog = deny_program(&rules).expect("x86_64/aarch64 only");
        install(&prog);
    }
    let p = |name: &str, v: String| println!("PROBE {name} => {v}");
    // SAFETY (all calls below): plain system calls with constant arguments aimed at
    // processes the parent test started for exactly this purpose.
    unsafe {
        p(
            "kill(victim, 0) [permission only]",
            sys_ret(libc::kill(victim, 0) as libc::c_long),
        );
        p(
            "kill(victim, SIGUSR1)",
            sys_ret(libc::kill(victim, libc::SIGUSR1) as libc::c_long),
        );
        p(
            "kill(sleeper, SIGTERM)",
            sys_ret(libc::kill(sleeper, libc::SIGTERM) as libc::c_long),
        );
        p(
            "kill(-group, SIGTERM) [process group]",
            sys_ret(libc::kill(-group, libc::SIGTERM) as libc::c_long),
        );
        p(
            "kill(-1, 0) [anyone at all]",
            sys_ret(libc::kill(-1, 0) as libc::c_long),
        );
        p(
            "tgkill(victim, victim, SIGUSR2)",
            sys_ret(libc::syscall(
                libc::SYS_tgkill,
                victim,
                victim,
                libc::SIGUSR2,
            )),
        );
        p(
            "tkill(victim, SIGUSR2)",
            sys_ret(libc::syscall(libc::SYS_tkill, victim, libc::SIGUSR2)),
        );
        let mut info = [0u8; 128];
        info[0] = libc::SIGRTMIN() as u8 + 1; // si_signo
        info[8] = 0xff; // si_code = SI_QUEUE-ish (negative, user)
        info[9] = 0xff;
        info[10] = 0xff;
        info[11] = 0xff;
        p(
            "rt_sigqueueinfo(victim, SIGRTMIN+1)",
            sys_ret(libc::syscall(
                libc::SYS_rt_sigqueueinfo,
                victim,
                libc::SIGRTMIN() + 1,
                info.as_ptr(),
            )),
        );
        let pidfd = libc::syscall(libc::SYS_pidfd_open, victim, 0);
        if pidfd >= 0 {
            p(
                "pidfd_send_signal(victim, SIGUSR2)",
                sys_ret(libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd as i32,
                    libc::SIGUSR2,
                    0usize,
                    0,
                )),
            );
        } else {
            p(
                "pidfd_send_signal(victim, SIGUSR2)",
                format!("pidfd_open failed: {}", errname()),
            );
        }
        // alternative route: F_SETOWN + O_ASYNC makes the kernel send SIGIO to the owner
        let mut fds = [0i32; 2];
        libc::pipe(fds.as_mut_ptr());
        let owner = libc::fcntl(fds[0], libc::F_SETOWN, victim);
        let own_err = errname();
        libc::fcntl(fds[0], libc::F_SETFL, libc::O_ASYNC | libc::O_NONBLOCK);
        libc::write(fds[1], b"x".as_ptr() as *const _, 1);
        p(
            "F_SETOWN(victim) + O_ASYNC [SIGIO]",
            if owner == 0 {
                "accepted".into()
            } else {
                own_err
            },
        );
        // what must keep working: signals to itself and to its own children
        p(
            "raise(0) / kill(self, 0)",
            sys_ret(libc::kill(libc::getpid(), 0) as libc::c_long),
        );
        // (a forked child that only waits: exec is not allowed under the empty fs ruleset)
        let id = libc::fork();
        if id == 0 {
            // ends by itself after 2 s if nobody can kill it (the seccomp mode)
            libc::alarm(2);
            loop {
                libc::pause();
            }
        }
        p(
            "kill(own child, SIGKILL)",
            sys_ret(libc::kill(id, libc::SIGKILL) as libc::c_long),
        );
        let mut status = 0;
        libc::waitpid(id, &mut status, 0);
    }
}

// ----------------------------------------------------------- BPF (test only)

fn deny_program(rules: &[(libc::c_long, Option<u32>)]) -> Option<Vec<libc::sock_filter>> {
    #[cfg(target_arch = "x86_64")]
    let arch = 0xC000_003Eu32;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xC000_00B7u32;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return None;
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let f = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
        let mut p = vec![
            f(0x20, 0, 0, 4),
            f(0x15, 1, 0, arch),
            f(0x06, 0, 0, 0x8000_0000),
            f(0x20, 0, 0, 0),
        ];
        let n = rules.len() as u8;
        for (i, (nr, _)) in rules.iter().enumerate() {
            p.push(f(0x15, n - i as u8, 0, *nr as u32));
        }
        p.push(f(0x06, 0, 0, 0x7FFF_0000));
        p.push(f(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32));
        Some(p)
    }
}

fn install(prog: &[libc::sock_filter]) {
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut _,
    };
    // SAFETY: valid program; NO_NEW_PRIVS first.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog),
            0
        );
    }
}

// ----------------------------------------------------------------- parent

struct Run {
    probes: BTreeMap<String, String>,
    victim_signals: (u32, u32, u32, u32),
    sleeper_alive: bool,
    group_alive: bool,
}

fn alive(pid: i32) -> bool {
    // a killed child stays a zombie until reaped: check the state
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn run(mode: &str) -> Option<Run> {
    let _one_at_a_time = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    install_handlers();
    for c in [&USR1, &USR2, &IO, &RT] {
        c.store(0, Ordering::SeqCst);
    }
    let spawn = |n: usize| -> (Vec<Child>, i32) {
        let mut v = Vec::new();
        let mut pgid = 0;
        for _ in 0..n {
            let mut c = Command::new("sleep");
            c.arg("60").stdin(Stdio::null()).process_group(pgid);
            let ch = c.spawn().unwrap();
            if pgid == 0 {
                pgid = ch.id() as i32;
            }
            v.push(ch);
        }
        (v, pgid)
    };
    let (mut sleeper, sleeper_pid) = spawn(1);
    let (mut group, group_pgid) = spawn(2);
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "signal_child", "--nocapture", "--test-threads=1"])
        .env("M73_SIG_MODE", mode)
        .env("M73_VICTIM", std::process::id().to_string())
        .env("M73_SLEEPER", sleeper_pid.to_string())
        .env("M73_GROUP", group_pgid.to_string())
        .process_group(0);
    let out = c.output().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let result = if out.status.success() && text.contains("PROBE") {
        let probes = text
            .lines()
            .filter_map(|l| l.strip_prefix("PROBE "))
            .filter_map(|l| l.split_once(" => "))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(Run {
            probes,
            victim_signals: (
                USR1.load(Ordering::SeqCst),
                USR2.load(Ordering::SeqCst),
                IO.load(Ordering::SeqCst),
                RT.load(Ordering::SeqCst),
            ),
            sleeper_alive: alive(sleeper_pid),
            group_alive: group.iter().any(|g| alive(g.id() as i32)),
        })
    } else if err.contains("Landlock")
        || text.contains("needs Landlock ABI 6")
        || err.contains("needs Landlock ABI 6")
    {
        eprintln!("skipped {mode}: {err}");
        None
    } else {
        panic!("child failed ({mode}): {text}\n{err}");
    };
    for c in sleeper.iter_mut().chain(group.iter_mut()) {
        let _ = c.kill();
        let _ = c.wait();
    }
    result
}

fn show(mode: &str, r: &Run) {
    eprintln!("---- mode `{mode}` ----");
    for (k, v) in &r.probes {
        eprintln!("{k:<44} {v}");
    }
    eprintln!(
        "delivered to the victim: SIGUSR1={} SIGUSR2={} SIGIO={} SIGRT={}; sleeper alive={}; group alive={}",
        r.victim_signals.0, r.victim_signals.1, r.victim_signals.2, r.victim_signals.3, r.sleeper_alive, r.group_alive
    );
}

#[test]
fn at_the_current_abi_a_sandboxed_process_can_really_signal_outsiders() {
    let r = run("fs").expect("fs mode runs everywhere");
    show("fs (what the product applies today)", &r);
    // Not just permission: the signals ARRIVED, a process died, a process group died.
    assert!(
        r.victim_signals.0 >= 1 && r.victim_signals.1 >= 1,
        "SIGUSR1/2 delivered"
    );
    assert!(
        !r.sleeper_alive,
        "kill(pid, SIGTERM) of an outside process worked"
    );
    assert!(
        !r.group_alive,
        "kill(-pgid, SIGTERM) of an outside group worked"
    );
    assert!(r.victim_signals.2 >= 1, "SIGIO via F_SETOWN arrived");
    assert!(r.probes["kill(-1, 0) [anyone at all]"].contains("permitted"));
}

#[test]
fn landlock_signal_scope_stops_every_route_and_keeps_own_children_killable() {
    let Some(r) = run("scope") else {
        eprintln!("skipped: Landlock ABI 6 (Linux 6.12) is not available here");
        return;
    };
    show("scope (Landlock ABI 6 Scope::Signal)", &r);
    assert_eq!(r.victim_signals, (0, 0, 0, 0), "nothing was delivered");
    assert!(r.sleeper_alive && r.group_alive, "no outsider was harmed");
    for k in [
        "kill(victim, SIGUSR1)",
        "kill(sleeper, SIGTERM)",
        "kill(-group, SIGTERM) [process group]",
        "tgkill(victim, victim, SIGUSR2)",
        "tkill(victim, SIGUSR2)",
        "rt_sigqueueinfo(victim, SIGRTMIN+1)",
        "pidfd_send_signal(victim, SIGUSR2)",
    ] {
        assert!(r.probes[k].starts_with("errno 1"), "{k}: {}", r.probes[k]); // EPERM
    }
    // what a server needs must keep working
    assert!(
        r.probes["kill(own child, SIGKILL)"].contains("permitted"),
        "{}",
        r.probes["kill(own child, SIGKILL)"]
    );
    assert!(r.probes["raise(0) / kill(self, 0)"].contains("permitted"));
}

#[test]
fn a_seccomp_filter_on_the_signal_calls_cannot_keep_own_children_killable() {
    let Some(r) = run("seccomp") else {
        eprintln!("skipped: seccomp program needs x86_64 or aarch64");
        return;
    };
    show("seccomp (deny the signal system calls)", &r);
    assert_eq!(
        r.victim_signals.0 + r.victim_signals.1 + r.victim_signals.3,
        0
    );
    // ... but it also stops the server from killing its own timed-out Git: incompatible
    assert!(
        r.probes["kill(own child, SIGKILL)"].starts_with("errno 1"),
        "{}",
        r.probes["kill(own child, SIGKILL)"]
    );
    // and F_SETOWN/O_ASYNC is a route that no syscall filter on the signal calls closes
    assert!(
        r.victim_signals.2 >= 1,
        "SIGIO still arrived: {:?}",
        r.victim_signals
    );
}
