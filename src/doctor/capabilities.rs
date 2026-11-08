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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Evidence {
    /// The self-test ran and the server reported + verified an active sandbox.
    Active,
    /// The self-test ran and the server could not establish the sandbox.
    Unavailable,
    /// No usable evidence (not run, failed, inconsistent).
    #[default]
    None,
}

/// What the self-test child reported about the socket-deny seccomp policy,
/// parsed from the typed `seccomp` field (never from free text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeccompEvidence {
    /// `state: enforced` WITH the negative probes the server ran.
    Enforced,
    /// `state: not_applied`: an unsupported platform, said explicitly.
    NotApplied,
    /// `state: failed`.
    Failed,
    /// Absent, unknown state or incomplete: nothing may be claimed.
    #[default]
    Unknown,
}

/// Whether the self-test child showed the Landlock signal scope in effect: it
/// asked a process outside the sandbox and got `EPERM` (typed `signal_scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalEvidence {
    Enforced,
    /// The kernel has no signal scope (Landlock below ABI 6): nothing to enforce.
    NotAvailable,
    #[default]
    Unknown,
}

pub fn parse_signal_evidence(v: Option<&Value>) -> SignalEvidence {
    match v.and_then(|v| v["signal_scope"].as_str()) {
        Some("enforced") => SignalEvidence::Enforced,
        Some("not_available") => SignalEvidence::NotAvailable,
        _ => SignalEvidence::Unknown,
    }
}

/// Everything the self-test proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Proven {
    pub sandbox: Evidence,
    pub seccomp: SeccompEvidence,
    pub signals: SignalEvidence,
}

pub fn parse_seccomp_evidence(v: Option<&Value>) -> SeccompEvidence {
    let Some(v) = v else {
        return SeccompEvidence::Unknown;
    };
    match v["state"].as_str() {
        // A bare "enforced" is not enough: the negative probes must be present.
        Some("enforced")
            if v["probes"]["socket(AF_INET)"] == "EACCES"
                && v["probes"]["socket(AF_UNIX)"] == "EACCES" =>
        {
            SeccompEvidence::Enforced
        }
        Some("not_applied") => SeccompEvidence::NotApplied,
        Some("failed") => SeccompEvidence::Failed,
        _ => SeccompEvidence::Unknown,
    }
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
    // `No` only if the kernel itself lacks seccomp; a policy that blocks the
    // query is `Unknown`, never "unsupported" (see `sandbox::seccomp`).
    match crate::sandbox::seccomp::kernel_support() {
        crate::sandbox::seccomp::Support::Yes => Tri::Yes,
        crate::sandbox::seccomp::Support::No => Tri::No,
        crate::sandbox::seccomp::Support::Unknown => Tri::Unknown,
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
    /// Declared through Landlock rights / through the seccomp filter.
    declared_landlock: bool,
    declared_seccomp: bool,
    /// Landlock enforcement of this channel needs its own runtime evidence (the
    /// signal scope exists only on newer kernels).
    needs_signal_evidence: bool,
    basis: &'static str,
}

fn level_for(c: &Channel, seccomp_ok: Tri, proven: Proven) -> (Level, Vec<&'static str>) {
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
    let enforced_by_landlock = c.declared_landlock
        && proven.sandbox == Evidence::Active
        && (!c.needs_signal_evidence || proven.signals == SignalEvidence::Enforced);
    // The filter counts only with typed evidence from the active sandbox.
    let enforced_by_seccomp = c.declared_seccomp
        && proven.sandbox == Evidence::Active
        && proven.seccomp == SeccompEvidence::Enforced;
    let level = if enforced_by_landlock || enforced_by_seccomp {
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
pub fn assess(facts: &KernelFacts, declared: &Declared, proven: Proven) -> Value {
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
            declared_landlock: declared.filesystem,
            declared_seccomp: false,
            needs_signal_evidence: false,
            basis: "Landlock filesystem rights (ABI 3 or newer) restrict path access; only the self-test of `mind mcp` can show it active, and only while `mind mcp` runs with an allowed root",
        },
        Channel {
            name: "tcp",
            landlock: facts.rights.tcp,
            seccomp: Tri::Yes,
            declared_landlock: declared.tcp,
            declared_seccomp: declared.seccomp,
            needs_signal_evidence: false,
            basis: "Landlock TCP bind/connect rights (ABI 4+) exist but Mindspace does not use them; the seccomp socket-deny filter stops creating NEW sockets, not the use of inherited ones",
        },
        Channel {
            name: "udp",
            landlock: Tri::No,
            seccomp: Tri::Yes,
            declared_landlock: declared.udp,
            declared_seccomp: declared.seccomp,
            needs_signal_evidence: false,
            basis: "no Landlock right covers UDP (measured on ABI 10 in spike M7.2b-2); only the seccomp socket-deny filter on socket creation does; inherited sockets stay usable",
        },
        Channel {
            name: "unix_path_sockets",
            landlock: facts.rights.pathname_unix,
            seccomp: Tri::Yes,
            declared_landlock: declared.unix_path_sockets,
            declared_seccomp: declared.seccomp,
            needs_signal_evidence: false,
            basis: "Landlock pathname-socket right (a late ABI) exists but is not used; at ABI 3 any reachable socket path could be connected, which the seccomp filter prevents for NEW sockets; inherited sockets stay usable",
        },
        Channel {
            name: "unix_abstract_sockets",
            landlock: facts.rights.abstract_unix_scope,
            seccomp: Tri::Yes,
            declared_landlock: declared.unix_abstract_sockets,
            declared_seccomp: declared.seccomp,
            needs_signal_evidence: false,
            basis: "Landlock abstract-unix-socket scope (ABI 6+) exists but is not used; the seccomp filter prevents NEW sockets; inherited sockets stay usable",
        },
        Channel {
            name: "signals",
            landlock: facts.rights.signal_scope,
            seccomp: Tri::No,
            declared_landlock: declared.signals,
            declared_seccomp: false,
            needs_signal_evidence: true,
            basis: "Landlock signal scope (ABI 6+, Linux 6.12): used where the kernel has it and shown by asking a process outside the sandbox; below ABI 6 nothing restricts signals",
        },
    ];
    let mut ch = serde_json::Map::new();
    for c in &channels {
        let (level, via) = level_for(c, facts.seccomp_filter, proven);
        let mut entry = json!({
            "status": level,
            "applies_to": "a `mind mcp` started with an allowed root, as shown by the self-test child; not the doctor process",
            "available_via": via,
            "basis": c.basis,
        });
        if level == Level::Enforced && c.declared_seccomp && !c.declared_landlock {
            entry["enforced_by"] =
                json!("seccomp socket-deny filter (typed evidence from the self-test child)");
            entry["limits"] = json!("covers creation of NEW sockets only; descriptors open at start (stdio), socketpair, SCM_RIGHTS and signals are not covered; this is not network or IPC isolation");
        }
        ch.insert(c.name.into(), entry);
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
            "seccomp_policy": "socket-deny filter on x86_64 only (other architectures: explicitly not applied); denied: socket, io_uring_setup, pidfd_getfd, ptrace, process_vm_readv, process_vm_writev; foreign architectures and x32 numbers are killed",
        },
        "seccomp_evidence": {
            "state": match proven.seccomp {
                SeccompEvidence::Enforced => "enforced",
                SeccompEvidence::NotApplied => "not_applied",
                SeccompEvidence::Failed => "failed",
                SeccompEvidence::Unknown => "unknown",
            },
            "basis": "typed `seccomp` field of the self-test child's repo_outline result, with its negative probes; the filter mode `Seccomp: 2` alone is not accepted as proof of the policy",
        },
        "channels": Value::Object(ch),
        "limits": "`enforced` needs a declaration in the product code AND an active sandbox shown by the self-test (for the socket channels also typed seccomp evidence). Kernel support is not product isolation. Neither Landlock nor the socket filter covers already open file descriptors or already connected sockets, and neither is process, network or IPC isolation. Other kernels and platforms: not examined (unknown).",
    })
}

/// The report for this machine.
pub fn report(proven: Proven) -> Value {
    assess(&probe_kernel(), &DECLARED, proven)
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

    fn proven(sandbox: Evidence, seccomp: SeccompEvidence) -> Proven {
        Proven {
            sandbox,
            seccomp,
            ..Default::default()
        }
    }

    const ALL_SOCKET_CHANNELS: [&str; 4] =
        ["tcp", "udp", "unix_path_sockets", "unix_abstract_sockets"];

    #[test]
    fn a_new_kernel_is_not_a_sandboxed_product() {
        // ABI 10, every right accepted, Landlock sandbox proven, but NO seccomp
        // evidence: the socket channels are only supported, never enforced.
        let v = assess(
            &facts(LandlockState::Available(10), Tri::Yes, Tri::Yes),
            &DECLARED,
            proven(Evidence::Active, SeccompEvidence::Unknown),
        );
        assert_eq!(status(&v, "filesystem"), "enforced");
        for ch in ALL_SOCKET_CHANNELS.iter().chain(&["signals"]) {
            assert_eq!(status(&v, ch), "supported_not_enforced", "{ch}");
        }
        assert_eq!(v["channels"]["udp"]["available_via"], json!(["seccomp"]));
        assert_eq!(v["doctor_process_confined_by_mindspace"], false);
        assert_eq!(v["seccomp_evidence"]["state"], "unknown");
    }

    #[test]
    fn socket_channels_are_enforced_only_with_typed_seccomp_evidence() {
        let f = facts(LandlockState::Available(10), Tri::Yes, Tri::Yes);
        let v = assess(
            &f,
            &DECLARED,
            proven(Evidence::Active, SeccompEvidence::Enforced),
        );
        for ch in ALL_SOCKET_CHANNELS {
            assert_eq!(status(&v, ch), "enforced", "{ch}");
            assert!(v["channels"][ch]["limits"]
                .as_str()
                .unwrap()
                .contains("NEW sockets only"));
        }
        // signals and the filesystem are not seccomp's job
        assert_eq!(status(&v, "signals"), "supported_not_enforced");
        assert_eq!(v["product"]["uses_seccomp"], true);
        // every other combination keeps them off `enforced`
        for (sb, sc) in [
            (Evidence::Active, SeccompEvidence::NotApplied),
            (Evidence::Active, SeccompEvidence::Failed),
            (Evidence::Active, SeccompEvidence::Unknown),
            (Evidence::Unavailable, SeccompEvidence::Enforced),
            (Evidence::None, SeccompEvidence::Enforced),
        ] {
            let v = assess(&f, &DECLARED, proven(sb, sc));
            for ch in ALL_SOCKET_CHANNELS {
                assert_ne!(status(&v, ch), "enforced", "{ch} with {sb:?}/{sc:?}");
            }
        }
    }

    #[test]
    fn a_declaration_without_evidence_is_not_enforcement() {
        let f = facts(LandlockState::Available(10), Tri::Yes, Tri::Yes);
        for ev in [Evidence::None, Evidence::Unavailable] {
            let v = assess(&f, &DECLARED, proven(ev, SeccompEvidence::Enforced));
            assert_eq!(status(&v, "filesystem"), "supported_not_enforced");
        }
        // a hypothetical Landlock TCP declaration needs the sandbox evidence too
        let mut d = DECLARED;
        d.tcp = true;
        d.seccomp = false;
        let none = assess(&f, &d, proven(Evidence::None, SeccompEvidence::Unknown));
        assert_eq!(status(&none, "tcp"), "supported_not_enforced");
        let on = assess(&f, &d, proven(Evidence::Active, SeccompEvidence::Unknown));
        assert_eq!(status(&on, "tcp"), "enforced");
        // and without a seccomp declaration the filter evidence changes nothing
        assert_eq!(status(&on, "udp"), "supported_not_enforced");
    }

    #[test]
    fn the_seccomp_evidence_is_parsed_strictly() {
        let good = json!({"state":"enforced","probes":{"socket(AF_INET)":"EACCES","socket(AF_UNIX)":"EACCES"}});
        assert_eq!(
            parse_seccomp_evidence(Some(&good)),
            SeccompEvidence::Enforced
        );
        // "enforced" without the negative probes proves nothing
        let bare = json!({"state":"enforced"});
        assert_eq!(
            parse_seccomp_evidence(Some(&bare)),
            SeccompEvidence::Unknown
        );
        let wrong = json!({"state":"enforced","probes":{"socket(AF_INET)":"allowed","socket(AF_UNIX)":"EACCES"}});
        assert_eq!(
            parse_seccomp_evidence(Some(&wrong)),
            SeccompEvidence::Unknown
        );
        assert_eq!(
            parse_seccomp_evidence(Some(&json!({"state":"not_applied"}))),
            SeccompEvidence::NotApplied
        );
        assert_eq!(
            parse_seccomp_evidence(Some(&json!({"state":"failed"}))),
            SeccompEvidence::Failed
        );
        assert_eq!(
            parse_seccomp_evidence(Some(&json!({"state":"whatever"}))),
            SeccompEvidence::Unknown
        );
        assert_eq!(
            parse_seccomp_evidence(Some(&json!("enforced"))),
            SeccompEvidence::Unknown
        );
        assert_eq!(parse_seccomp_evidence(None), SeccompEvidence::Unknown);
    }

    #[test]
    fn abi_3_supports_only_the_filesystem_through_landlock() {
        let v = assess(
            &facts(LandlockState::Available(3), Tri::No, Tri::Yes),
            &DECLARED,
            proven(Evidence::Active, SeccompEvidence::NotApplied),
        );
        assert_eq!(status(&v, "filesystem"), "enforced");
        assert_eq!(status(&v, "tcp"), "supported_not_enforced"); // kernel could (seccomp), product did not
        assert_eq!(v["channels"]["tcp"]["available_via"], json!(["seccomp"]));
        assert_eq!(status(&v, "signals"), "not_supported");
    }

    #[test]
    fn below_the_project_minimum_the_filesystem_is_not_supported() {
        let v = assess(
            &facts(LandlockState::Available(2), Tri::No, Tri::No),
            &DECLARED,
            Proven::default(),
        );
        assert_eq!(status(&v, "filesystem"), "not_supported");
    }

    #[test]
    fn missing_landlock_and_missing_seccomp_leave_nothing_supported() {
        let v = assess(
            &facts(LandlockState::Unavailable("x".into()), Tri::No, Tri::No),
            &DECLARED,
            proven(Evidence::Unavailable, SeccompEvidence::Unknown),
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
            Proven::default(),
        );
        for ch in [
            "filesystem",
            "tcp",
            "udp",
            "unix_path_sockets",
            "unix_abstract_sockets",
            "signals",
        ] {
            assert_eq!(status(&v, ch), "unknown", "{ch}");
        }
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
        let v = assess(&f, &DECLARED, Proven::default());
        assert!(v["channels"]["filesystem"]["status"].is_string());
        if let LandlockState::Available(abi) = f.landlock {
            if abi >= 3 {
                assert_ne!(status(&v, "filesystem"), "not_supported");
            }
        }
    }
}
