//! Sandbox capability report of `mind doctor` (M7.2c).
//!
//! Read-only. Nothing is installed into the doctor process: Landlock is queried
//! by *creating a ruleset and closing it again* (never `restrict_self`), seccomp
//! by `seccomp(SECCOMP_GET_ACTION_AVAIL)` and `prctl(PR_GET_SECCOMP)`. No socket
//! is opened, no connection made, no other process contacted.
//!
//! Three things are kept apart on purpose:
//!  1. what the **kernel supports** (probed here),
//!  2. what **Mindspace's sandbox handles** (`sandbox::DECLARED`, from code),
//!  3. what is **proven active** (only the existing self-test of `mind mcp`).
//!
//! A channel is `enforced` only if (2) declares it and (3) shows the sandbox
//! active. A kernel ABI number alone never produces `enforced`.

use serde::Serialize;
use serde_json::{json, Value};

use crate::sandbox::{Declared, DECLARED, MIN_ABI};

/// Status vocabulary of the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Declared by the product code and shown active by the self-test.
    Enforced,
    /// The kernel offers a mechanism for it, but Mindspace does not use it.
    SupportedNotEnforced,
    /// No mechanism available here can cover it.
    NotSupported,
    /// Could not be determined reliably.
    Unknown,
}

/// A yes/no answer that may be unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandlockState {
    /// Highest ABI the kernel reports.
    Available(u32),
    /// No Landlock (`ENOSYS`: not built; `EOPNOTSUPP`: disabled at boot).
    Unavailable(String),
    Unknown(String),
}

/// Which optional Landlock rights the kernel accepts (probed one by one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandlockRights {
    pub tcp: Tri,
    pub abstract_unix_scope: Tri,
    pub signal_scope: Tri,
    pub pathname_unix: Tri,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelFacts {
    pub release: Option<String>,
    pub lsm: Option<String>,
    pub landlock: LandlockState,
    pub rights: LandlockRights,
    /// Can this kernel run a seccomp filter?
    pub seccomp_filter: Tri,
    /// `PR_GET_SECCOMP` of the doctor process itself (inherited, not ours).
    pub seccomp_mode_this_process: Option<u32>,
}

/// What the existing self-test established about the product sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The self-test ran and the server reported + verified an active sandbox.
    Active,
    /// The self-test ran and the server could not establish the sandbox.
    Unavailable,
    /// No usable evidence (not run, failed, inconsistent).
    None,
}

// ------------------------------------------------------------------ probing

/// Maps the result of a capability-probing syscall. `Ok(())` for success,
/// `EINVAL`/`E2BIG` mean "this kernel does not know the field", anything else
/// is not conclusive.
pub fn classify_right_probe(result: Result<(), i32>) -> Tri {
    match result {
        Ok(()) => Tri::Yes,
        Err(e) if e == libc::EINVAL || e == libc::E2BIG => Tri::No,
        Err(_) => Tri::Unknown,
    }
}

/// `ENOSYS`/`EOPNOTSUPP` from the version query: Landlock is absent or disabled.
pub fn classify_landlock_version(result: Result<u32, i32>) -> LandlockState {
    match result {
        Ok(v) if v >= 1 => LandlockState::Available(v),
        Ok(v) => LandlockState::Unknown(format!("the kernel reported ABI {v}")),
        Err(e) if e == libc::ENOSYS => LandlockState::Unavailable(
            "landlock_create_ruleset fails with ENOSYS: the kernel was built without Landlock"
                .into(),
        ),
        Err(e) if e == libc::EOPNOTSUPP => LandlockState::Unavailable(
            "landlock_create_ruleset fails with EOPNOTSUPP: Landlock is disabled at boot".into(),
        ),
        Err(e) => LandlockState::Unknown(format!(
            "landlock_create_ruleset failed with errno {e}: {}",
            std::io::Error::from_raw_os_error(e)
        )),
    }
}

// The Landlock system calls have the same numbers on every architecture
// (the unified range 444..=446). Ruleset attribute: three u64 fields.
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
// Access bits (UAPI): READ_FILE = bit 2; RESOLVE_UNIX = bit 16.
const FS_READ_FILE: u64 = 1 << 2;
const FS_RESOLVE_UNIX: u64 = 1 << 16;
const NET_BIND_CONNECT_TCP: u64 = 0b11;
const SCOPE_ABSTRACT_UNIX: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn landlock_version() -> Result<u32, i32> {
    // SAFETY: the version query takes no pointer and installs nothing.
    let r = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<u8>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as u32)
    }
}

/// Creates (and immediately closes) a ruleset that handles the given rights.
/// A kernel that does not know a field answers EINVAL/E2BIG. `restrict_self` is
/// never called, so the process is not confined.
fn ruleset_accepts(fs: u64, net: u64, scoped: u64) -> Result<(), i32> {
    let attr: [u64; 3] = [fs, net, scoped];
    // SAFETY: `attr` outlives the call and is `size_of_val` bytes long.
    let fd = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            attr.as_ptr(),
            std::mem::size_of_val(&attr),
            0 as libc::c_ulong,
        )
    };
    if fd < 0 {
        return Err(errno());
    }
    // SAFETY: fd was just returned by the kernel and is owned here.
    unsafe { libc::close(fd as i32) };
    Ok(())
}

fn seccomp_filter_supported() -> Tri {
    // SECCOMP_GET_ACTION_AVAIL (2) with SECCOMP_RET_ERRNO: a pure query.
    let action: u32 = 0x0005_0000;
    // SAFETY: `action` outlives the call; the operation only reads it.
    let r = unsafe { libc::syscall(libc::SYS_seccomp, 2u32, 0u32, &action as *const u32) };
    if r == 0 {
        return Tri::Yes;
    }
    match errno() {
        e if e == libc::ENOSYS || e == libc::EINVAL => Tri::No,
        _ => Tri::Unknown,
    }
}

fn seccomp_mode_of_this_process() -> Option<u32> {
    // SAFETY: PR_GET_SECCOMP takes no pointer and changes nothing.
    let r = unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    (r >= 0).then_some(r as u32)
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Reads the facts from the running kernel. Read-only, see the module comment.
pub fn probe_kernel() -> KernelFacts {
    let landlock = classify_landlock_version(landlock_version());
    let have = matches!(landlock, LandlockState::Available(_));
    // Without Landlock no right can exist; with an unknown state nothing is known.
    let right = |fs: u64, net: u64, scoped: u64| {
        if have {
            classify_right_probe(ruleset_accepts(fs, net, scoped))
        } else if matches!(landlock, LandlockState::Unavailable(_)) {
            Tri::No
        } else {
            Tri::Unknown
        }
    };
    let rights = LandlockRights {
        tcp: right(FS_READ_FILE, NET_BIND_CONNECT_TCP, 0),
        abstract_unix_scope: right(FS_READ_FILE, 0, SCOPE_ABSTRACT_UNIX),
        signal_scope: right(FS_READ_FILE, 0, SCOPE_SIGNAL),
        pathname_unix: right(FS_READ_FILE | FS_RESOLVE_UNIX, 0, 0),
    };
    KernelFacts {
        release: read_trimmed("/proc/sys/kernel/osrelease"),
        lsm: read_trimmed("/sys/kernel/security/lsm"),
        landlock,
        rights,
        seccomp_filter: seccomp_filter_supported(),
        seccomp_mode_this_process: seccomp_mode_of_this_process(),
    }
}

// --------------------------------------------------------------- assessment

#[derive(Debug, Clone, Copy)]
struct Channel {
    name: &'static str,
    landlock: Tri,
    /// Could a seccomp filter fence it (new sockets only for the socket rows)?
    seccomp: Tri,
    declared: bool,
    basis: &'static str,
}

fn level_for(c: &Channel, seccomp_ok: Tri, evidence: Evidence) -> (Level, Vec<&'static str>) {
    let sec = match (c.seccomp, seccomp_ok) {
        (Tri::Yes, Tri::Yes) => Tri::Yes,
        (Tri::No, _) | (_, Tri::No) => Tri::No,
        _ => Tri::Unknown,
    };
    let mut via = Vec::new();
    if c.landlock == Tri::Yes {
        via.push("landlock");
    }
    if sec == Tri::Yes {
        via.push("seccomp");
    }
    let level = if c.declared && evidence == Evidence::Active {
        Level::Enforced
    } else if !via.is_empty() {
        Level::SupportedNotEnforced
    } else if c.landlock == Tri::No && sec == Tri::No {
        Level::NotSupported
    } else {
        Level::Unknown
    };
    (level, via)
}

/// Builds the report. Pure: all inputs are parameters.
pub fn assess(facts: &KernelFacts, declared: &Declared, evidence: Evidence) -> Value {
    // Filesystem: Landlock ABI >= MIN_ABI (project minimum) is required.
    let min_abi = MIN_ABI as u32;
    let fs_landlock = match &facts.landlock {
        LandlockState::Available(v) if *v >= min_abi => Tri::Yes,
        LandlockState::Available(_) | LandlockState::Unavailable(_) => Tri::No,
        LandlockState::Unknown(_) => Tri::Unknown,
    };
    let channels = [
        Channel {
            name: "filesystem",
            landlock: fs_landlock,
            seccomp: Tri::No,
            declared: declared.filesystem,
            basis: "Landlock filesystem rights (ABI 3 or newer) restrict path access; only the self-test of `mind mcp` can show it active, and only while `mind mcp` runs with an allowed root",
        },
        Channel {
            name: "tcp",
            landlock: facts.rights.tcp,
            seccomp: Tri::Yes,
            declared: declared.tcp,
            basis: "Landlock TCP bind/connect rights (ABI 4+); seccomp could stop creating new sockets, not use of inherited ones",
        },
        Channel {
            name: "udp",
            landlock: Tri::No,
            seccomp: Tri::Yes,
            declared: declared.udp,
            basis: "no Landlock right covers UDP (measured on ABI 10 in spike M7.2b-2); only a seccomp filter on socket creation could; inherited sockets stay usable",
        },
        Channel {
            name: "unix_path_sockets",
            landlock: facts.rights.pathname_unix,
            seccomp: Tri::Yes,
            declared: declared.unix_path_sockets,
            basis: "Landlock pathname-socket right (a late ABI); at ABI 3 any reachable socket path can be connected; inherited sockets stay usable",
        },
        Channel {
            name: "unix_abstract_sockets",
            landlock: facts.rights.abstract_unix_scope,
            seccomp: Tri::Yes,
            declared: declared.unix_abstract_sockets,
            basis: "Landlock abstract-unix-socket scope (ABI 6+); inherited sockets stay usable",
        },
        Channel {
            name: "signals",
            landlock: facts.rights.signal_scope,
            seccomp: Tri::No,
            declared: declared.signals,
            basis: "Landlock signal scope (ABI 6+); seccomp cannot express it usefully",
        },
    ];
    let mut ch = serde_json::Map::new();
    for c in &channels {
        let (level, via) = level_for(c, facts.seccomp_filter, evidence);
        ch.insert(
            c.name.into(),
            json!({
                "status": level,
                "applies_to": "a `mind mcp` started with an allowed root, as shown by the self-test child; not the doctor process",
                "available_via": via,
                "basis": c.basis,
            }),
        );
    }
    let landlock = match &facts.landlock {
        LandlockState::Available(v) => json!({"state": "available", "highest_abi": v}),
        LandlockState::Unavailable(r) => json!({"state": "unavailable", "reason": r}),
        LandlockState::Unknown(r) => json!({"state": "unknown", "reason": r}),
    };
    let tri = |t: Tri| match t {
        Tri::Yes => "yes",
        Tri::No => "no",
        Tri::Unknown => "unknown",
    };
    let mode = match facts.seccomp_mode_this_process {
        Some(0) => json!("none"),
        Some(1) => json!("strict"),
        Some(2) => json!("filter (inherited by this process, not installed by Mindspace)"),
        Some(n) => json!(format!("mode {n}")),
        None => json!("unknown"),
    };
    json!({
        "scope": "describes what a `mind mcp` started with --allow-root applies, not the doctor process: the doctor installs no Landlock rule and no seccomp filter on itself",
        "doctor_process_confined_by_mindspace": false,
        "kernel": {
            "release": facts.release,
            "active_lsm": facts.lsm,
            "landlock": landlock,
            "landlock_rights_accepted": {
                "tcp": tri(facts.rights.tcp),
                "abstract_unix_scope": tri(facts.rights.abstract_unix_scope),
                "signal_scope": tri(facts.rights.signal_scope),
                "pathname_unix": tri(facts.rights.pathname_unix),
            },
            "seccomp_filter_supported": tri(facts.seccomp_filter),
            "seccomp_mode_of_doctor_process": mode,
        },
        "product": {
            "landlock_filesystem_abi": min_abi,
            "uses_landlock_network_rules": declared.tcp,
            "uses_landlock_scopes": declared.unix_abstract_sockets || declared.signals,
            "uses_landlock_pathname_socket_right": declared.unix_path_sockets,
            "uses_seccomp": declared.seccomp,
        },
        "channels": Value::Object(ch),
        "limits": "`enforced` needs a declaration in the product code AND an active sandbox shown by the self-test. Kernel support is not product isolation. Landlock never covers already open file descriptors or already connected sockets, and is not process isolation. Other kernels and platforms: not examined (unknown).",
    })
}

/// The report for this machine.
pub fn report(evidence: Evidence) -> Value {
    assess(&probe_kernel(), &DECLARED, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(landlock: LandlockState, r: Tri, seccomp: Tri) -> KernelFacts {
        KernelFacts {
            release: Some("test".into()),
            lsm: None,
            landlock,
            rights: LandlockRights {
                tcp: r,
                abstract_unix_scope: r,
                signal_scope: r,
                pathname_unix: r,
            },
            seccomp_filter: seccomp,
            seccomp_mode_this_process: Some(0),
        }
    }

    fn status(v: &Value, ch: &str) -> String {
        v["channels"][ch]["status"].as_str().unwrap().to_string()
    }

    #[test]
    fn probe_results_map_to_yes_no_unknown() {
        assert_eq!(classify_right_probe(Ok(())), Tri::Yes);
        assert_eq!(classify_right_probe(Err(libc::EINVAL)), Tri::No);
        assert_eq!(classify_right_probe(Err(libc::E2BIG)), Tri::No);
        assert_eq!(classify_right_probe(Err(libc::EPERM)), Tri::Unknown);
        assert!(matches!(
            classify_landlock_version(Ok(10)),
            LandlockState::Available(10)
        ));
        assert!(matches!(
            classify_landlock_version(Err(libc::ENOSYS)),
            LandlockState::Unavailable(_)
        ));
        assert!(matches!(
            classify_landlock_version(Err(libc::EOPNOTSUPP)),
            LandlockState::Unavailable(_)
        ));
        assert!(matches!(
            classify_landlock_version(Err(libc::EPERM)),
            LandlockState::Unknown(_)
        ));
    }

    #[test]
    fn a_new_kernel_is_not_a_sandboxed_product() {
        // ABI 10 with every right accepted: the product handles none of them.
        let v = assess(
            &facts(LandlockState::Available(10), Tri::Yes, Tri::Yes),
            &DECLARED,
            Evidence::Active,
        );
        assert_eq!(status(&v, "filesystem"), "enforced");
        for ch in [
            "tcp",
            "unix_path_sockets",
            "unix_abstract_sockets",
            "signals",
        ] {
            assert_eq!(status(&v, ch), "supported_not_enforced", "{ch}");
        }
        // UDP: no Landlock right, only seccomp could.
        assert_eq!(status(&v, "udp"), "supported_not_enforced");
        assert_eq!(v["channels"]["udp"]["available_via"], json!(["seccomp"]));
        assert_eq!(v["doctor_process_confined_by_mindspace"], false);
        assert_eq!(v["product"]["uses_seccomp"], false);
    }

    #[test]
    fn enforced_needs_evidence_not_just_a_declaration_or_an_abi() {
        let f = facts(LandlockState::Available(10), Tri::Yes, Tri::Yes);
        for ev in [Evidence::None, Evidence::Unavailable] {
            let v = assess(&f, &DECLARED, ev);
            assert_eq!(status(&v, "filesystem"), "supported_not_enforced");
        }
        // Even a (hypothetical) declaration of TCP without evidence is not enforced.
        let mut d = DECLARED;
        d.tcp = true;
        assert_eq!(
            status(&assess(&f, &d, Evidence::None), "tcp"),
            "supported_not_enforced"
        );
        assert_eq!(status(&assess(&f, &d, Evidence::Active), "tcp"), "enforced");
    }

    #[test]
    fn abi_3_supports_only_the_filesystem() {
        let v = assess(
            &facts(LandlockState::Available(3), Tri::No, Tri::Yes),
            &DECLARED,
            Evidence::Active,
        );
        assert_eq!(status(&v, "filesystem"), "enforced");
        assert_eq!(status(&v, "tcp"), "supported_not_enforced"); // via seccomp only
        assert_eq!(v["channels"]["tcp"]["available_via"], json!(["seccomp"]));
        assert_eq!(status(&v, "signals"), "not_supported");
    }

    #[test]
    fn below_the_project_minimum_the_filesystem_is_not_supported() {
        let v = assess(
            &facts(LandlockState::Available(2), Tri::No, Tri::No),
            &DECLARED,
            Evidence::None,
        );
        assert_eq!(status(&v, "filesystem"), "not_supported");
    }

    #[test]
    fn missing_landlock_and_missing_seccomp_leave_nothing_supported() {
        let v = assess(
            &facts(LandlockState::Unavailable("x".into()), Tri::No, Tri::No),
            &DECLARED,
            Evidence::Unavailable,
        );
        for ch in [
            "filesystem",
            "tcp",
            "udp",
            "unix_path_sockets",
            "unix_abstract_sockets",
            "signals",
        ] {
            assert_eq!(status(&v, ch), "not_supported", "{ch}");
        }
        assert_eq!(v["kernel"]["landlock"]["state"], "unavailable");
    }

    #[test]
    fn what_cannot_be_determined_is_unknown_not_guessed() {
        let v = assess(
            &facts(
                LandlockState::Unknown("e".into()),
                Tri::Unknown,
                Tri::Unknown,
            ),
            &DECLARED,
            Evidence::None,
        );
        for ch in [
            "filesystem",
            "tcp",
            "udp",
            "unix_path_sockets",
            "unix_abstract_sockets",
        ] {
            assert_eq!(status(&v, ch), "unknown", "{ch}");
        }
        // signals: Landlock unknown, seccomp cannot cover -> still unknown
        assert_eq!(status(&v, "signals"), "unknown");
    }

    #[test]
    fn the_doctor_never_confines_itself() {
        // Guard: no doctor source installs a Landlock or seccomp restriction on
        // the doctor process. (Reasoned from code; this keeps it that way.)
        for (name, src) in [
            ("capabilities.rs", include_str!("capabilities.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("selftest.rs", include_str!("selftest.rs")),
        ] {
            // split so that this very test does not match itself
            for needle in [
                ["restrict", "_self"].concat(),
                ["PR_SET", "_SECCOMP"].concat(),
            ] {
                let hits = src.matches(needle.as_str()).count();
                let allowed = if name == "capabilities.rs" { 2 } else { 0 };
                assert!(hits <= allowed, "{name} mentions {needle} {hits} times");
            }
        }
    }

    #[test]
    fn a_real_probe_of_this_machine_returns_a_consistent_report() {
        let f = probe_kernel();
        // The probes install nothing: if they did, the filesystem of the test
        // process would be restricted and this read would fail.
        assert!(std::fs::read_to_string("/proc/self/status").is_ok());
        let v = assess(&f, &DECLARED, Evidence::None);
        assert!(v["channels"]["filesystem"]["status"].is_string());
        if let LandlockState::Available(abi) = f.landlock {
            if abi >= 3 {
                assert_ne!(status(&v, "filesystem"), "not_supported");
            }
        }
    }
}
