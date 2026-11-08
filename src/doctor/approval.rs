//! Approval assurance diagnostics of `mind doctor` (M7.3b, ADR-030 section 5a).
//!
//! The question this answers: what can Mindspace honestly say about WHO may
//! create a repository grant on this machine? The answer is computed
//! conservatively and every claim carries its evidence:
//!
//!  * `supported`  - this build and host have the machinery,
//!  * `configured` - it is set up / available for use,
//!  * `verified`   - Mindspace has actually checked the security assumption,
//!  * `unknown`    - not determinable (never rounded up to `yes`).
//!
//! Rules (tested):
//!  * The reported level is the **verified** one. A0 means "no trustworthy approval
//!    authority has been proven", which is the default.
//!  * A1 needs a verified isolation of the agent from the grant command. Nothing in
//!    Mindspace verifies that today, so it stays `unknown` and the level A0. A
//!    terminal prompt, a client setting, or the Mindspace server's own sandbox
//!    are not evidence of it.
//!  * A2 and A3 need an implemented, configured and verified authority (polkit,
//!    hardware token). Mindspace implements neither, so they are never reported.
//!  * Host input facts only ever LOWER or annotate. A missing interface is
//!    `absent`, never "safe": the passive probes cannot prove that no injection
//!    path exists.
//!
//! The probes are passive: `stat` and `access(2)` on paths, environment variables,
//! one read of `/proc/sys/kernel/yama/ptrace_scope` and a read-only look at the
//! grant store. No device is opened, no input is read or written, no dialog is
//! shown, nothing is created.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::capabilities::{Evidence, Proven, SeccompEvidence};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Level {
    A0,
    A1,
    A2,
    A3,
}

/// What a passive look at a path found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathAccess {
    /// It exists and this process may use it as asked (`access(2)`).
    Accessible,
    /// It exists but this process may not.
    Inaccessible,
    /// It does not exist. NOT the same as safe.
    Absent,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capability {
    pub supported: Tri,
    pub configured: Tri,
    pub verified: Tri,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationEvidence {
    /// A check established that the agent cannot reach the grant command, the
    /// store and the approval channel. Mindspace has no such check yet.
    Verified,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoreState {
    /// No store file: no grant was ever made.
    Absent,
    Valid {
        grants: usize,
        active: usize,
    },
    /// It does not verify; no grant from it is used.
    Invalid {
        why: String,
    },
    Unknown {
        why: String,
    },
}

/// How same-user processes may trace each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PtraceFact {
    /// Yama `ptrace_scope` >= 1: only descendants.
    Restricted,
    /// Yama scope 0, or no Yama: same-user processes may trace each other.
    SameUserPossible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evdev {
    pub devices: usize,
    pub readable: usize,
    pub access: PathAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostInput {
    /// `/dev/uinput` (`access(W_OK)`): creating virtual keyboards and mice.
    pub uinput: PathAccess,
    /// `/dev/input/event*` (`access(R_OK)`): observing keystrokes.
    pub evdev: Evdev,
    /// An X11 socket the user may write to (synthetic input through XTEST).
    pub x11: PathAccess,
    /// The Wayland socket of the session (exists for any session process).
    pub wayland_socket: PathAccess,
    /// The session D-Bus socket.
    pub session_bus: PathAccess,
    pub ptrace: PtraceFact,
}

/// Everything the assessment looks at.
#[derive(Debug, Clone)]
pub struct Facts {
    pub grant_core_supported: bool,
    pub store: StoreState,
    /// A host polkit component is on `PATH` (Mindspace does not use it yet).
    pub polkit_on_host: Tri,
    pub input: HostInput,
    pub agent_isolation: IsolationEvidence,
    /// What the client or its settings claim about confining the shell. It is
    /// never read by Mindspace and never counts as evidence.
    pub client_declared_isolation: Tri,
    pub server_sandbox: Proven,
}

// ------------------------------------------------------------------ probing

/// Where the probes look. Injectable so tests can use a directory they own.
#[derive(Debug, Clone)]
pub struct HostEnv {
    pub dev_dir: PathBuf,
    pub x11_dir: PathBuf,
    pub runtime_dir: Option<PathBuf>,
    pub wayland_display: Option<OsString>,
    pub display: Option<OsString>,
    pub yama_file: PathBuf,
}

impl HostEnv {
    pub fn from_process() -> Self {
        Self {
            dev_dir: PathBuf::from("/dev"),
            x11_dir: PathBuf::from("/tmp/.X11-unix"),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            display: std::env::var_os("DISPLAY"),
            yama_file: PathBuf::from("/proc/sys/kernel/yama/ptrace_scope"),
        }
    }
}

/// `stat` plus `access(2)`. Opens nothing.
pub fn path_access(path: &Path, mode: libc::c_int) -> PathAccess {
    use std::os::unix::ffi::OsStrExt;
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PathAccess::Absent,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return PathAccess::Inaccessible
        }
        Err(_) => return PathAccess::Unknown,
    }
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return PathAccess::Unknown;
    };
    // SAFETY: access(2) with a valid NUL-terminated path; it only checks permissions.
    if unsafe { libc::access(c.as_ptr(), mode) } == 0 {
        PathAccess::Accessible
    } else {
        PathAccess::Inaccessible
    }
}

pub fn probe_input(env: &HostEnv) -> HostInput {
    let uinput = path_access(&env.dev_dir.join("uinput"), libc::W_OK);
    let evdev = match std::fs::read_dir(env.dev_dir.join("input")) {
        Ok(rd) => {
            let nodes: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
                .map(|e| e.path())
                .collect();
            let readable = nodes
                .iter()
                .filter(|p| path_access(p, libc::R_OK) == PathAccess::Accessible)
                .count();
            Evdev {
                devices: nodes.len(),
                readable,
                access: if nodes.is_empty() {
                    PathAccess::Absent
                } else if readable > 0 {
                    PathAccess::Accessible
                } else {
                    PathAccess::Inaccessible
                },
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Evdev {
            devices: 0,
            readable: 0,
            access: PathAccess::Absent,
        },
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Evdev {
            devices: 0,
            readable: 0,
            access: PathAccess::Inaccessible,
        },
        Err(_) => Evdev {
            devices: 0,
            readable: 0,
            access: PathAccess::Unknown,
        },
    };
    let x11 = match std::fs::read_dir(&env.x11_dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('X'))
            .map(|e| path_access(&e.path(), libc::W_OK))
            .max_by_key(|a| matches!(a, PathAccess::Accessible))
            .unwrap_or(PathAccess::Absent),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PathAccess::Absent,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => PathAccess::Inaccessible,
        Err(_) => PathAccess::Unknown,
    };
    let wayland_socket = match (&env.runtime_dir, &env.wayland_display) {
        (Some(rt), Some(d)) if !d.is_empty() => {
            let p = Path::new(d);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                rt.join(p)
            };
            path_access(&full, libc::W_OK)
        }
        (_, None) => PathAccess::Absent,
        _ => PathAccess::Unknown,
    };
    let session_bus = match &env.runtime_dir {
        Some(rt) => path_access(&rt.join("bus"), libc::W_OK),
        None => PathAccess::Unknown,
    };
    let ptrace = match std::fs::read_to_string(&env.yama_file) {
        Ok(s) => match s.trim().parse::<u32>() {
            Ok(0) => PtraceFact::SameUserPossible,
            Ok(_) => PtraceFact::Restricted,
            Err(_) => PtraceFact::Unknown,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PtraceFact::SameUserPossible,
        Err(_) => PtraceFact::Unknown,
    };
    HostInput {
        uinput,
        evdev,
        x11,
        wayland_socket,
        session_bus,
        ptrace,
    }
}

pub fn probe_store() -> StoreState {
    use crate::grants::{Status, Store};
    let state = match crate::paths::state_dir() {
        Ok(s) => s,
        Err(e) => return StoreState::Unknown { why: e.to_string() },
    };
    let store = Store::in_dir(&state.path);
    if std::fs::symlink_metadata(store.file()).is_err() {
        return StoreState::Absent;
    }
    match store.load() {
        Ok(r) => {
            let now = time::OffsetDateTime::now_utc();
            StoreState::Valid {
                grants: r.records.len(),
                active: r
                    .records
                    .iter()
                    .filter(|g| g.status(now) == Status::Active)
                    .count(),
            }
        }
        Err(e) => StoreState::Invalid { why: e.to_string() },
    }
}

impl Facts {
    /// The real machine. Passive; see the module comment.
    pub fn from_process(proven: Proven) -> Self {
        let polkit = if crate::pathenv::executables("pkcheck", std::env::var_os("PATH").as_deref())
            .is_empty()
        {
            Tri::No
        } else {
            Tri::Yes
        };
        Self {
            grant_core_supported: true,
            store: probe_store(),
            polkit_on_host: polkit,
            input: probe_input(&HostEnv::from_process()),
            // No check of the agent's isolation exists. It is unknown, not "no".
            agent_isolation: IsolationEvidence::Unknown,
            // Client settings are not read and would not count.
            client_declared_isolation: Tri::Unknown,
            server_sandbox: proven,
        }
    }
}

// --------------------------------------------------------------- assessment

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionPath {
    /// At least one way for a same-user process to inject or observe input.
    Accessible,
    /// None of the inspected interfaces is usable. NOT proof that none exists.
    NoneObserved,
    Unknown,
}

pub fn injection_path(i: &HostInput) -> InjectionPath {
    let hit = i.uinput == PathAccess::Accessible
        || i.evdev.access == PathAccess::Accessible
        || i.x11 == PathAccess::Accessible;
    if hit {
        return InjectionPath::Accessible;
    }
    let unknown = [i.uinput, i.evdev.access, i.x11].contains(&PathAccess::Unknown);
    if unknown {
        InjectionPath::Unknown
    } else {
        InjectionPath::NoneObserved
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelDecision {
    pub supported: Level,
    pub configured: Level,
    pub verified: Level,
    pub blockers: Vec<String>,
}

/// The conservative computation. `verified` is the level to rely on.
pub fn decide(f: &Facts) -> LevelDecision {
    let mut blockers = Vec::new();
    // What the build implements: the terminal authority (A1) on top of the grant core.
    let supported = if f.grant_core_supported {
        Level::A1
    } else {
        Level::A0
    };
    // What is available for use. The terminal authority needs no setup, but
    // "available" is not "trustworthy"; polkit and tokens are not implemented.
    let store_ok = !matches!(f.store, StoreState::Invalid { .. });
    let configured = if f.grant_core_supported && store_ok {
        Level::A1
    } else {
        Level::A0
    };
    let mut verified = Level::A0;
    if matches!(f.store, StoreState::Invalid { .. }) {
        blockers.push("the grant store does not verify; no grant from it is used".to_string());
    }
    if f.agent_isolation != IsolationEvidence::Verified {
        blockers.push(
            "A1 needs verified isolation of the agent from `mind grant`, the grant store and the approval channel; \
             nothing verifies that (client settings and the Mindspace server's own sandbox are not evidence of it)"
                .to_string(),
        );
    } else if configured >= Level::A1 {
        verified = Level::A1;
    }
    blockers.push(
        "A2 needs an implemented, configured and verified root-owned authority (polkit); Mindspace implements none".to_string(),
    );
    blockers
        .push("A3 needs a verified hardware presence proof; Mindspace implements none".to_string());
    if injection_path(&f.input) != InjectionPath::NoneObserved {
        blockers.push(
            "this host exposes or may expose input injection or observation to same-user processes; a typed secret or a clicked dialog is not trustworthy here"
                .to_string(),
        );
    }
    LevelDecision {
        supported,
        configured,
        verified,
        blockers,
    }
}

fn tri(b: bool) -> Tri {
    if b {
        Tri::Yes
    } else {
        Tri::No
    }
}

/// The additive JSON object (`details.approval`).
pub fn assess(f: &Facts) -> Value {
    let d = decide(f);
    let isolation_verified = f.agent_isolation == IsolationEvidence::Verified;
    let injection = injection_path(&f.input);
    let sandbox = match (f.server_sandbox.sandbox, f.server_sandbox.seccomp) {
        (Evidence::Active, SeccompEvidence::Enforced) => "landlock_and_seccomp_enforced",
        (Evidence::Active, _) => "landlock_enforced_seccomp_not_proven",
        (Evidence::Unavailable, _) => "unavailable",
        (Evidence::None, _) => "unknown",
    };
    let mut limits: Vec<String> = vec![
        "A request through MCP never creates a grant (tested), but `mind grant` run under your account does; this tool cannot tell you from another program in your account".into(),
        "a terminal confirmation is not an authority and never raises the level".into(),
        "a grant applies at the next start of `mind mcp --grants`; a running server is not changed, and revoking acts at the next start".into(),
        "human consent is not established at any level reported here".into(),
        "the passive host probes cannot prove that no input-injection path exists; `none_observed` is not `safe`".into(),
    ];
    if injection == InjectionPath::Accessible {
        limits.push(
            "input devices or an X11 socket are accessible to your user: a program in your account can observe keystrokes or inject input".into(),
        );
    }
    if f.input.ptrace != PtraceFact::Restricted {
        limits.push(
            "same-user processes may be able to trace each other (Yama ptrace_scope is 0, absent or unreadable)".into(),
        );
    }
    let level_json = |l: Level| serde_json::to_value(l).unwrap_or(Value::Null);
    serde_json::json!({
        "scope": "who may create repository grants on this machine; passive checks only (no device opened, no input read, no dialog, nothing written)",
        "grant_core": {
            "capability": Capability {
                supported: tri(f.grant_core_supported),
                configured: match &f.store { StoreState::Invalid { .. } => Tri::No, _ => tri(f.grant_core_supported) },
                verified: match &f.store { StoreState::Valid { .. } | StoreState::Absent => Tri::Yes, StoreState::Invalid { .. } => Tri::No, StoreState::Unknown { .. } => Tri::Unknown },
            },
            "store": f.store,
            "meaning_of_verified": "the store file, if any, passes the chain and field checks; it says nothing about who wrote it",
        },
        "authorities": {
            "terminal_cli": {
                "capability": Capability {
                    supported: Tri::Yes,
                    configured: Tri::Yes,
                    verified: tri(isolation_verified),
                },
                "note": "`mind grant add|approve` in a terminal. It is verified only if the agent is verified to be unable to run it; a prompt proves nothing",
            },
            "polkit": {
                "capability": Capability { supported: Tri::No, configured: Tri::No, verified: Tri::No },
                "host_component_present": f.polkit_on_host,
                "note": "not implemented by Mindspace (ADR-030, A2)",
            },
            "hardware_token": {
                "capability": Capability { supported: Tri::No, configured: Tri::No, verified: Tri::No },
                "host_component_present": Tri::Unknown,
                "note": "not implemented by Mindspace (ADR-030, A3)",
            },
        },
        "host_input": {
            "facts": f.input,
            "injection_or_observation_path": injection,
            "meaning": "facts only: they can lower or annotate an assessment, never raise it, and an absent interface is not evidence of safety",
        },
        "agent_isolation": {
            "state": f.agent_isolation,
            "verified": tri(isolation_verified),
            "client_declared": f.client_declared_isolation,
            "evidence": [],
            "note": "client settings are not read and would not be evidence; the Mindspace server sandbox confines the server, not the agent's shell",
        },
        "server_sandbox": {
            "state": sandbox,
            "scope": "the `mind mcp` process (Landlock, seccomp), see the sandbox section; not the agent's shell",
        },
        "assurance": {
            "meaning": "A0 means only that no trustworthy approval authority has been proven. It is not a statement that this host is secure or insecure, and not a statement about anything except who may create repository grants",
            "supported": level_json(d.supported),
            "configured": level_json(d.configured),
            "verified": level_json(d.verified),
            "effective": level_json(d.verified),
            "unknown": {
                "agent_isolation": !isolation_verified,
                "host_injection_paths": injection != InjectionPath::NoneObserved,
            },
            "blockers": d.blockers,
        },
        "open_limits": limits,
    })
}

/// The report for this machine.
pub fn report(proven: Proven) -> Value {
    assess(&Facts::from_process(proven))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn input(uinput: PathAccess, evdev: PathAccess, x11: PathAccess) -> HostInput {
        HostInput {
            uinput,
            evdev: Evdev {
                devices: (evdev != PathAccess::Absent) as usize,
                readable: (evdev == PathAccess::Accessible) as usize,
                access: evdev,
            },
            x11,
            wayland_socket: PathAccess::Accessible,
            session_bus: PathAccess::Accessible,
            ptrace: PtraceFact::Restricted,
        }
    }

    fn facts(i: HostInput) -> Facts {
        Facts {
            grant_core_supported: true,
            store: StoreState::Absent,
            polkit_on_host: Tri::Yes,
            input: i,
            agent_isolation: IsolationEvidence::Unknown,
            client_declared_isolation: Tri::Unknown,
            server_sandbox: Proven {
                sandbox: Evidence::Active,
                seccomp: SeccompEvidence::Enforced,
                ..Default::default()
            },
        }
    }

    use PathAccess::{Absent, Accessible, Inaccessible};

    #[test]
    fn an_unprotected_same_user_setup_is_a0_even_though_a1_is_supported_and_configured() {
        let f = facts(input(Accessible, Accessible, Accessible));
        let d = decide(&f);
        assert_eq!(d.supported, Level::A1);
        assert_eq!(d.configured, Level::A1);
        assert_eq!(d.verified, Level::A0, "{:?}", d.blockers);
        let v = assess(&f);
        assert_eq!(v["assurance"]["effective"], "A0");
        assert_eq!(v["assurance"]["verified"], "A0");
        assert_eq!(
            v["authorities"]["terminal_cli"]["capability"]["verified"],
            "no"
        );
        assert_eq!(v["agent_isolation"]["state"], "unknown");
        assert_eq!(v["assurance"]["unknown"]["agent_isolation"], true);
    }

    #[test]
    fn client_claims_and_a_terminal_prompt_do_not_raise_the_level() {
        let mut f = facts(input(Absent, Absent, Absent));
        f.client_declared_isolation = Tri::Yes;
        assert_eq!(decide(&f).verified, Level::A0);
        assert_eq!(assess(&f)["agent_isolation"]["state"], "unknown");
        assert_eq!(assess(&f)["agent_isolation"]["client_declared"], "yes");
        // the server's own sandbox is about the server, not the agent
        f.server_sandbox = Proven {
            sandbox: Evidence::Active,
            seccomp: SeccompEvidence::Enforced,
            ..Default::default()
        };
        assert_eq!(decide(&f).verified, Level::A0);
    }

    #[test]
    fn a1_exists_only_with_verified_agent_isolation() {
        let mut f = facts(input(Accessible, Accessible, Accessible));
        f.agent_isolation = IsolationEvidence::Verified;
        let d = decide(&f);
        assert_eq!(d.verified, Level::A1);
        assert_eq!(
            assess(&f)["authorities"]["terminal_cli"]["capability"]["verified"],
            "yes"
        );
        // never above A1: no polkit or token is implemented, whatever the host has
        assert!(d.verified < Level::A2);
        // a broken store lowers it again
        f.store = StoreState::Invalid { why: "x".into() };
        assert_eq!(decide(&f).verified, Level::A0);
        assert_eq!(decide(&f).configured, Level::A0);
    }

    #[test]
    fn host_input_facts_never_change_the_level() {
        let variants = [
            input(Accessible, Accessible, Accessible),
            input(Inaccessible, Inaccessible, Inaccessible),
            input(Absent, Absent, Absent),
            input(
                PathAccess::Unknown,
                PathAccess::Unknown,
                PathAccess::Unknown,
            ),
            input(Accessible, Absent, Absent),
        ];
        for isolation in [IsolationEvidence::Unknown, IsolationEvidence::Verified] {
            let levels: Vec<Level> = variants
                .iter()
                .map(|i| {
                    let mut f = facts(i.clone());
                    f.agent_isolation = isolation;
                    decide(&f).verified
                })
                .collect();
            assert!(
                levels.windows(2).all(|w| w[0] == w[1]),
                "{isolation:?}: the level moved with the host input facts: {levels:?}"
            );
        }
    }

    #[test]
    fn missing_or_inaccessible_interfaces_are_not_reported_as_safe() {
        for i in [
            input(Absent, Absent, Absent),
            input(Inaccessible, Inaccessible, Inaccessible),
        ] {
            assert_eq!(injection_path(&i), InjectionPath::NoneObserved);
            let v = assess(&facts(i));
            assert_eq!(
                v["host_input"]["injection_or_observation_path"],
                "none_observed"
            );
            let text = v.to_string();
            assert!(!text.contains("\"safe\""), "no safety claim: {text}");
            assert!(text.contains("not `safe`") || text.contains("not evidence of safety"));
            assert_eq!(v["assurance"]["effective"], "A0");
            // still unknown, not "known to be fine"
            assert_eq!(v["assurance"]["unknown"]["agent_isolation"], true);
        }
        // unreadable facts are unknown, not "none"
        assert_eq!(
            injection_path(&input(PathAccess::Unknown, Absent, Absent)),
            InjectionPath::Unknown
        );
        // any single accessible path wins over absent ones
        assert_eq!(
            injection_path(&input(Absent, Accessible, Absent)),
            InjectionPath::Accessible
        );
    }

    #[test]
    fn exposed_input_is_listed_as_a_limit_and_a_blocker() {
        let v = assess(&facts(input(Accessible, Accessible, Absent)));
        let limits = v["open_limits"].to_string();
        assert!(limits.contains("observe keystrokes or inject input"));
        let blockers = v["assurance"]["blockers"].to_string();
        assert!(blockers.contains("input injection or observation"));
        assert!(blockers.contains("A2") && blockers.contains("A3"));
    }

    #[test]
    fn the_report_separates_supported_configured_verified_and_unknown() {
        let v = assess(&facts(input(Absent, Absent, Absent)));
        for name in ["terminal_cli", "polkit", "hardware_token"] {
            let c = &v["authorities"][name]["capability"];
            for k in ["supported", "configured", "verified"] {
                assert!(
                    ["yes", "no", "unknown"].contains(&c[k].as_str().unwrap()),
                    "{name}.{k}"
                );
            }
        }
        assert_eq!(v["authorities"]["polkit"]["capability"]["supported"], "no");
        assert_eq!(v["authorities"]["polkit"]["host_component_present"], "yes");
        assert_eq!(
            v["authorities"]["hardware_token"]["host_component_present"],
            "unknown"
        );
        for k in ["supported", "configured", "verified", "effective"] {
            assert!(["A0", "A1", "A2", "A3"].contains(&v["assurance"][k].as_str().unwrap()));
        }
        assert_eq!(v["assurance"]["supported"], "A1");
        assert_eq!(v["assurance"]["configured"], "A1");
        assert_eq!(v["assurance"]["verified"], "A0");
        assert_eq!(
            v["server_sandbox"]["state"],
            "landlock_and_seccomp_enforced"
        );
        assert_eq!(v["grant_core"]["store"]["state"], "absent");
    }

    #[test]
    fn ptrace_and_unreadable_facts_are_reported_as_what_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |content: Option<&str>| {
            let p = dir.path().join("yama");
            let _ = std::fs::remove_file(&p);
            if let Some(c) = content {
                std::fs::write(&p, c).unwrap();
            }
            HostEnv {
                dev_dir: dir.path().join("dev"),
                x11_dir: dir.path().join("x11"),
                runtime_dir: None,
                wayland_display: None,
                display: None,
                yama_file: p,
            }
        };
        assert_eq!(probe_input(&mk(Some("1\n"))).ptrace, PtraceFact::Restricted);
        assert_eq!(
            probe_input(&mk(Some("0\n"))).ptrace,
            PtraceFact::SameUserPossible
        );
        assert_eq!(
            probe_input(&mk(None)).ptrace,
            PtraceFact::SameUserPossible,
            "no Yama"
        );
        assert_eq!(probe_input(&mk(Some("junk"))).ptrace, PtraceFact::Unknown);
        let i = probe_input(&mk(None));
        assert_eq!(
            i.session_bus,
            PathAccess::Unknown,
            "no runtime dir is unknown, not absent"
        );
        assert_eq!(i.uinput, PathAccess::Absent);
    }

    #[test]
    fn probes_read_permissions_without_opening_or_changing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join("dev");
        std::fs::create_dir_all(dev.join("input")).unwrap();
        let uinput = dev.join("uinput");
        std::fs::write(&uinput, "").unwrap();
        std::fs::set_permissions(&uinput, std::fs::Permissions::from_mode(0o666)).unwrap();
        for n in ["event0", "event1", "mouse0"] {
            std::fs::write(dev.join("input").join(n), "").unwrap();
        }
        std::fs::set_permissions(
            dev.join("input/event0"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::set_permissions(
            dev.join("input/event1"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let x11 = dir.path().join("x11");
        std::fs::create_dir(&x11).unwrap();
        std::fs::write(x11.join("X0"), "").unwrap();
        let before = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.len(), m.permissions().mode(), m.modified().unwrap())
        };
        let snapshot: Vec<_> = [
            &uinput,
            &dev.join("input/event0"),
            &dev.join("input/event1"),
            &x11.join("X0"),
        ]
        .iter()
        .map(|p| before(p))
        .collect();
        let env = HostEnv {
            dev_dir: dev.clone(),
            x11_dir: x11.clone(),
            runtime_dir: Some(dir.path().to_path_buf()),
            wayland_display: Some("wl-0".into()),
            display: Some(":0".into()),
            yama_file: dir.path().join("none"),
        };
        let i = probe_input(&env);
        assert_eq!(i.uinput, Accessible);
        assert_eq!(i.evdev.devices, 2, "only event* nodes are counted");
        assert_eq!(i.evdev.access, Accessible);
        assert_eq!(i.wayland_socket, PathAccess::Absent);
        assert_eq!(i.session_bus, PathAccess::Absent);
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(i.evdev.readable, 1, "the 0000 node is not readable");
            std::fs::set_permissions(&uinput, std::fs::Permissions::from_mode(0o444)).unwrap();
            assert_eq!(
                probe_input(&env).uinput,
                Inaccessible,
                "read-only is not writable"
            );
        }
        assert_eq!(i.x11, Accessible);
        // nothing was created, removed or changed
        let after: Vec<_> = [
            &uinput,
            &dev.join("input/event0"),
            &dev.join("input/event1"),
            &x11.join("X0"),
        ]
        .iter()
        .map(|p| before(p))
        .collect();
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(snapshot[1..], after[1..]);
        }
        assert_eq!(std::fs::read_dir(&dev).unwrap().count(), 2);
    }

    #[test]
    fn grant_decisions_never_read_these_host_facts() {
        // Tripwire: the code that creates, validates or applies grants must not
        // depend on the diagnostics (they annotate, they never decide).
        let needles = [
            ["crate::doctor", "::approval"].concat(),
            ["approval", "::report"].concat(),
            ["Host", "Input"].concat(),
        ];
        for (name, src) in [
            ("grants.rs", include_str!("../grants.rs")),
            ("cli/grant.rs", include_str!("../cli/grant.rs")),
            ("sandbox/mod.rs", include_str!("../sandbox/mod.rs")),
            ("sandbox/seccomp.rs", include_str!("../sandbox/seccomp.rs")),
        ] {
            for n in &needles {
                assert!(!src.contains(n.as_str()), "{name} refers to {n}");
            }
        }
    }

    #[test]
    fn a0_is_explained_as_a_statement_about_approval_only() {
        let v = assess(&facts(input(Accessible, Accessible, Accessible)));
        let m = v["assurance"]["meaning"].as_str().unwrap();
        assert!(
            m.contains("not a statement that this host is secure or insecure"),
            "{m}"
        );
    }

    #[test]
    fn the_real_probe_runs_without_side_effects_and_without_a_safety_claim() {
        let v = report(Proven::default());
        assert_eq!(v["assurance"]["effective"], "A0");
        assert!(!v.to_string().contains("\"safe\""));
    }
}
