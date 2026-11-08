# ADR-030: Just-in-time repository authorization (M7.3)

Status: **proposed**, 2026-11-08. Not implemented. No product change. Basis: `main` at `ea5e651`; spike #1 `49c027a`
(`docs/spikes/m7.2b-jit-permissions.md` on that branch), spike #2 `b38ef3b` (`docs/spikes/m7.2b-elicitation-sandbox.md`), seccomp
compatibility spike `c8d30b0`, the real-client run (Claude Code 2.1.319, headless), and the probes of this ADR
(`scripts/spike/m73_identity_probe.py`).
Labels: **VERIFIED** (executed, result known), **REASONED** (follows from verified facts or kernel/protocol semantics),
**UNKNOWN**. A confirmation dialog is never treated as proof that a human consented.

## 1. Question

Can a running MCP agent ask for access to one more repository, a human approve it, and Mindspace enforce exactly that, **without
an agent that has unrestricted shell access under the human's own identity being able to grant itself the permission**?

Guiding principle: consent must come from an authority the agent cannot control.

## 2. Answer in one paragraph

Not with anything that runs under the human's uid and is driven by keyboard, pointer or a protocol answer. Measured on this
development host (Linux 7.2.14, Wayland): a same-user process can read all 24 input devices and create a virtual keyboard through
`/dev/uinput` (the user is in group `input`; input-injection tools are installed); a Claude Code `Elicitation` hook answers `accept` without a human;
the same-uid approver channel of spike #1 accepts the agent's own `accept`; and a rootless "separate identity" (user namespace
with the caller's subuid range) is **not** a separate trust domain, because the namespace's creator may `kill`, `ptrace` and
`pidfd_getfd` its processes. What does hold: a **confined** agent shell (no input devices, no session sockets, no ptrace) cannot
reach a user-level approver at all, and a root-owned authority (polkit) answers `challenge` to an unauthenticated caller. So the
architecture has to be stated per **threat tier**, with the approval authority as a replaceable component whose *assurance level*
is measured and reported, not assumed.

## 3. Security model

### 3.1 Identities and tiers
* **Human**: owns the uid and the session; the only party that may widen access.
* **Agent**: an LLM-driven client acting through MCP and, depending on the client, a shell.
* Tier **T0**: agent has MCP tools only (no shell), may be prompt-injected. Tier **T1**: agent has a *confined* shell (client sandbox: no
  `/dev/input`, no session bus/Wayland/X11 sockets, no ptrace, no access to the approver's files). Tier **T2**: agent has an
  *unrestricted* shell under the human's uid (the worst case named in the task).
* Mindspace cannot observe which tier it runs in (the client decides). It can measure the host (section 6.4).

### 3.2 Trusted computing base (TCB)
Kernel (Landlock, seccomp, uid separation), the grant authority, the broker, and for T2 additionally the OS component that
authenticates the human (polkit/PAM, or a hardware token). **Not** in the TCB: the MCP client, the MCP frontend (it parses
untrusted input), workers, the agent, any agent-writable file (settings, hooks, units, the grant request file).

### 3.3 Components and trust boundaries
```
 ┌────────────┐  stdio/MCP   ┌───────────────────┐ socketpair ┌───────────────┐  fd(root) ┌──────────────┐
 │ MCP client │─────────────▶│ MCP frontend      │───────────▶│ BROKER        │──────────▶│ WORKER / grant│
 │ (+ agent)  │◀─────────────│ Landlock+seccomp  │  requests  │ holds grants, │  one dir  │ Landlock: 1   │
 └────────────┘              │ no repo rights,   │            │ spawns workers│           │ inode, ro/rw  │
      ▲  agent shell (T1/T2) │ cannot approve    │            └──────▲────────┘           │ no db, no net │
      │                      └───────────────────┘                   │ signed decision    └──────────────┘
      │                                                  ┌───────────┴────────────┐
      │ ✗ must not reach                                 │ GRANT AUTHORITY        │◀── human (terminal / polkit
      └─────────────────────────────────────────────────▶│ shows canonical path,  │    / hardware key)
                                                         │ mode, TTL, session     │
                                                         └────────────────────────┘
```
Boundaries: (1) client ⇄ frontend: untrusted. (2) frontend ⇄ broker: one inherited socketpair, no filesystem path; the frontend is
Landlock-confined without rights on repositories, the approver channel or the broker (spike #1 T7a). (3) broker ⇄ worker: one
directory fd. (4) authority ⇄ broker: the only channel that creates grants; its strength defines the assurance level (section 5).

### 3.4 Permission objects
A **grant** = {canonical path, device+inode of the directory (checked at approval and at worker start), mode ro|rw, session id,
expiry (TTL, default session only), issuer assurance level, random id}. Grants live in broker memory; nothing persists by default.
A persistent grant (`mind grant --persist`) is re-validated (inode, expiry) at every use and is a separate, later decision.

### 3.5 Path and descriptor handling
* The broker canonicalizes, opens the directory `O_PATH` **once**, and the approver shows that canonical path; if the entry changes
  while waiting, the grant is refused (VERIFIED, spike #1 T4a). After the grant the worker is bound to the inode (T4b).
* Landlock cannot widen a domain and cannot revoke open descriptors, so each grant is a **separate worker process** with its own
  Landlock domain (spike #1), receives exactly one directory fd, and revocation means killing the worker (T9). Inherited
  descriptors are closed before confinement (`close_inherited_fds`, M7.2e VERIFIED).
* Workers never get the database (M6 write fence is process-local); extraction happens in the worker, persistence in the frontend.

### 3.6 Lifecycle
Broker restart or crash: all grants are gone and workers die with it (`PR_SET_PDEATHSIG`, VERIFIED T10); calls fail closed.
Client crash/disconnect: the frontend's socketpair closes, the broker kills its workers and exits (T10c). Expiry and explicit revoke
kill the worker, also during a running call (T9, T9c). Two agents = two frontend sessions = two brokers or one broker with
session-scoped grants (T12 VERIFIED for separate brokers); a grant id from another session is rejected.

### 3.7 What a T2 agent can do under the human's uid (evidence)
| Action | Evidence |
| --- | --- |
| run `mind grant` / write any same-uid approval file or FIFO | VERIFIED (spike #1 T7c) |
| answer an elicitation through a settings hook | VERIFIED (spike #2, Claude Code) |
| create a virtual keyboard/mouse and click or type into the human's dialog | **VERIFIED capability**: `/dev/uinput` opened `O_WRONLY`, `UI_GET_VERSION` = 5 (no event was injected) |
| read keystrokes incl. a typed password | **VERIFIED capability**: 24 `/dev/input/event*` opened `O_RDONLY` (group `input`) |
| kill / ptrace / take descriptors from a "separate" rootless process it created | **VERIFIED** (`m73_identity_probe.py`: 3 of 7 attacks succeed) |
| start its own copy of the approver, edit user units/config, launch lookalike dialogs | REASONED (same uid) |
| ptrace a non-descendant | blocked here only by Yama `ptrace_scope=1` (VERIFIED value) |

## 4. Options compared

Security properties (✓ holds, ✗ does not hold, ◐ conditional, ? UNKNOWN):

| # | Option | T2: shell agent, same uid | Authentic human consent | Forged grants | Compromised MCP frontend | Revocation / TTL | Session binding | Parallel agents |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `mind grant` by the human in a terminal | ✗ (agent runs the same command) | ✗ none provable | ✗ store/CLI same uid | ✓ if the store is outside the frontend's Landlock | ✓ | ◐ stored in the grant | ✓ |
| 2 | MCP elicitation (client dialog) | ✗ hook VERIFIED | ✗ client-defined; interactive UNKNOWN | ✗ (hook, headless) | ✗ the answer arrives at the process that owns the connection | ◐ | ◐ | ✓ |
| 3 | broker under own identity, rootless (subuid) | ✗ **refuted**: creator can kill/ptrace it | ✗ does not address consent | n/a | ✓ | ✓ | ✓ | ✓ |
| 3b | same, system uid (root-installed service) | ◐ protects broker state only | ✗ consent still needs a channel | ✓ | ✓ | ✓ | ✓ | ✓ |
| 4 | broker + trusted desktop confirmation process | ✗ on this host (uinput/evdev); in general the broker cannot tell the genuine approver from an agent-run copy | ◐ only if input is isolated and the dialog is unspoofed | ✗ | ✓ | ✓ | ✓ | ✓ |
| 5 | broker + polkit action (root-owned authority) | ◐ agent cannot answer without authenticating (`challenge`, VERIFIED); password leaks via evdev on this host | ✓ with an authentication the agent cannot observe | ✓ | ✓ | ✓ | ✓ | ✓ |
| 6 | confined agent shell + user-level approver (terminal/GUI) | ✓ for T1 (channels vanish, VERIFIED with bubblewrap); ✗ for T2 | ◐ depends on the client's confinement | ◐ | ✓ | ✓ | ✓ | ✓ |
| 7 | hardware key (FIDO2/touch) as the authority's proof | ◐ cannot be spoofed by input injection; blind-touch / confused-deputy risk | ◐ no trusted display on the token | ✓ | ✓ | ✓ | ✓ | ✓ |

Practical properties:

| # | Linux/Wayland | Other OS | Install / maintenance |
| --- | --- | --- | --- |
| 1 | terminal only, compositor-independent | portable | none |
| 2 | client-dependent (dialog); headless = cancel | protocol-level, client-dependent | none; hooks can alter |
| 3/3b | rootless: works (VERIFIED) but useless; system: systemd unit | Windows service / macOS launchd daemon | 3b needs root, packaging |
| 4 | Wayland protocols differ per compositor (UNKNOWN per compositor); X11 is spoofable | separate per OS | desktop component, high |
| 5 | polkit + a running polkit agent; policy file in `/usr/share/polkit-1/actions` | macOS Authorization Services/LocalAuthentication, Windows UAC (REASONED, UNKNOWN) | root install, action file, agent per desktop |
| 6 | needs client sandboxing (Claude Code/Codex offer some; per-client UNKNOWN) | partly | low for Mindspace, high for users |
| 7 | hidraw/udev permissions, libfido2 | libfido2 is cross-platform (REASONED) | token needed |

## 5. Decision

1. **Authority is an interface with a measured assurance level**, not a feature. The broker only accepts a *decision object*
   (hash of canonical path, mode, TTL, session id, nonce) from the authority and starts a worker for it.
   * **A0** static roots (today). **A1** human approves in a terminal the agent does not control (user-level approver). **A2** root-owned
     authority (polkit action; `auth_self`/fingerprint) with an authentication the agent cannot observe. **A3** A2 or A1 plus a hardware
     presence proof.
2. **MVP = grant core + A1**, in two steps, each independently shippable:
   * **M7.3a** `mind grant add|list|revoke` and a grant store read at server start (no mid-session widening yet), plus an MCP tool
     `repo_request_access` that only *records a pending request* and tells the agent to ask the human. Smallest step; no worker refactor.
   * **M7.3b** mid-session grants: broker + per-grant workers (spike #1) and the extract/persist split of ingest. This is the large part.
3. **`mind doctor` reports the approval assurance this host can actually give** and says when a shell agent could approve itself
   (section 6.4). The documentation claims protection for **T0 and T1 only**.
4. **No claim for T2** until A2 or A3 exists *and* its prerequisites hold on the host. That is a stated limit, not a gap to hide.
5. Elicitation is allowed as a *convenience channel for A1-level requests* only if a client demonstrably shows a dialog and cannot
   answer it by hook; it is never sufficient for T2.

### Rejected
* Elicitation as the authority: forgeable (VERIFIED).
* A confirmation dialog in the user session as the authority for T2: input injection and lookalike approvers (VERIFIED capability).
* Rootless subuid broker as a trust boundary: refuted by the probe.
* A single long-lived privileged broker as MVP: needs root, and does not solve consent by itself.
* Persistent grants as default: widen the blast radius and survive the session.

## 5a. Sharpened requirements (added with M7.3a)

### Host capabilities (what makes a user-level approval forgeable)
A user-level approval (A1) can be answered by a same-uid process whenever any of these hold. `mind doctor` is to report each fact it
can read without side effects, and never to infer protection from a missing fact it cannot check:
| Fact | How it is read | Consequence if present |
| --- | --- | --- |
| input devices readable or `/dev/uinput` writable by the user | `open` check, nothing read or written | keystrokes observable, clicks and typing injectable: no dialog or typed secret is trustworthy |
| `kernel.yama.ptrace_scope = 0` | sysctl | same-uid processes can read and write each other's memory |
| X11 reachable (`DISPLAY`, XTEST) | socket path exists | synthetic input to X11 and XWayland clients |
| session or system D-Bus reachable | socket path exists | spoofed notifications, portal and unit manipulation |
| `systemctl --user` available | binary and bus | agent can create or replace user units, including an approver |
| polkit authentication agent registered | bus query (A2 only) | determines whether A2 is usable at all |
"Unknown" is reported as unknown. Mindspace never reports a stronger level than it measured.

### Agent isolation (the premise of every tier below T2)
T1 holds only if the client confines the agent's shell so that it has **no** access to: the input devices above; the Wayland, X11 and
D-Bus sockets of the session; ptrace and `/proc/<pid>/mem` of the human's processes; the approver's binary, configuration and
sockets; and the grant store. Mindspace cannot enforce or observe this. The documentation therefore says: "protects agents without a shell
and agents whose shell the client confines"; it does not say "protects against agents".

### Approval assurance levels (what each level may claim)
| Level | Authority | May claim | May not claim |
| --- | --- | --- | --- |
| A0 | static roots chosen by the human at registration | nothing is granted at run time | n/a |
| A1 | human in a terminal or user-level dialog; the grant store written by the human's CLI | a request alone never creates a grant; grants are validated, expire and can be revoked; protects T0 and T1 | protection against T2; authenticity of the human |
| A2 | root-owned authority (polkit) with an authentication the agent cannot observe | T2 protection on a host whose facts above are clean | T2 protection when input devices are exposed (a typed password can be read) |
| A3 | A2 or A1 plus a hardware presence proof | T2 protection even with exposed input devices, limited by blind-touch | what the token's missing display cannot show |

### Revocation
* A grant can be revoked at any time in the store; **new starts** never use it. A running server keeps the Landlock domain it was
  started with, which can neither shrink nor lose open descriptors. In M7.3a revocation therefore takes effect when the server is
  restarted; this is a stated limit. Mid-session revocation needs the per-grant workers of M7.3b (kill the worker, VERIFIED in spike #1).
* The grant store is outside every directory the confined server may write (the server's database directory is read-write for it).
  A root, static or granted, that contains the store, the database directory or the user's home is refused.
* Store integrity: events are chained by hash. This detects accidental damage and naive edits; it is **not** protection against a
  same-uid attacker, who can rewrite the whole chain. Any chain failure means no grant is used (fail closed).

## 6. Verdicts, assumptions, acceptance

### 6.1 Verdict
| Question | Verdict |
| --- | --- |
| Grant core (broker, per-grant workers, TTL, revoke, session binding) | **GO** (mechanics VERIFIED in spike #1) |
| Authorization for T0/T1 with A1 | **GO**, documented limits |
| Authorization that holds against T2 (unrestricted same-uid shell) on a desktop like this host | **NO-GO** today |
| A2 (polkit) as the T2 authority | **NEEDS MORE EVIDENCE**: needs a root-installed action; only the `challenge` behaviour was observed |
| A3 (hardware presence) | **NEEDS MORE EVIDENCE**: no token tested |

### 6.2 Open assumptions (UNKNOWN unless noted)
1. A polkit action with `$(path)`-style message details reaches the human through the desktop's agent and cannot be answered by the
   agent. Needs root to install the action.
2. The client sandboxes of Claude Code, Codex and OpenCode remove the channels of section 3.7 when enabled (bubblewrap does, VERIFIED;
   per client UNKNOWN). In this environment headless Claude Code ran a not-allowlisted MCP tool without a prompt, so client permission
   settings are **policy, not an authority**.
3. Another compositor/seat configuration without `input` group access makes evdev unreadable (REASONED).
4. FIDO2 user-presence prompts cannot be triggered and answered by software (REASONED).
5. Behaviour on macOS/Windows (Authorization Services, UAC secure desktop): REASONED only.

### 6.3 Acceptance criteria for the first implementation (M7.3a/b)
* The frontend and workers cannot create, modify or approve a grant: negative tests as in spike #1 T7a/T7b run in CI.
* A grant needs the authority's decision for the exact (canonical path, inode, mode, session, nonce); changed symlink or inode while
  waiting refuses it; `/`, the data directory and its parent are never grantable.
* No decision, decline, cancel, garbage or timeout creates no grant and no worker; all failures are closed.
* TTL, explicit revoke, client disconnect and broker death end the worker; a running call is interrupted; no process is left.
* Grants are session-bound; a grant id from another session is rejected; two agents do not share grants.
* Workers have one directory fd, no database, Landlock (hard requirement) and the socket-deny seccomp filter.
* `mind doctor` reports the approval assurance level and the host facts of 6.4; it never reports a level it has not measured.
* Documentation states T0/T1 only and lists the T2 limit with the evidence of 3.7.

### 6.4 Host facts `mind doctor` should read (read-only)
User in group `input` or `/dev/uinput`/`/dev/input/event*` accessible; `kernel.yama.ptrace_scope`; X11 reachable (`DISPLAY`, XTEST);
session/system bus reachable; `systemctl --user` available; whether a polkit agent is registered. Any exposed channel lowers the
reported assurance for T2 to "none".

## 7. Evidence of this ADR (development host: Linux 7.2.14, Wayland, user in `input`, Yama `ptrace_scope=1`, a subuid range configured)
| Probe | Result |
| --- | --- |
| `/dev/uinput` open `O_WRONLY` + `UI_GET_VERSION` | opened, version 5: virtual input devices can be created (nothing injected) |
| `/dev/input/event*` open `O_RDONLY` | 24 devices readable (nothing read) |
| rootless subuid process vs. its creator (`m73_identity_probe.py`) | read environ/mem and the 0600 files: denied; `kill`, `ptrace(SEIZE)`, `pidfd_getfd`: **allowed**; `SO_PEERCRED` on its socket gives the subuid, an agent-made socket gives the caller's own uid (a client that checks the peer uid is not fooled) |
| `pkcheck --process $$` for a polkit `auth_self` action, no interaction | exit 2 `challenge`: cannot be authorized without authentication |
| bubblewrap shell (`--dev /dev --unshare-all`) | `/dev/uinput`, `/dev/input/*`, Wayland, session bus, X11 sockets all unreachable |
| Claude Code headless, MCP tool not in `--allowedTools` | tool executed without prompt: permission policy is user/agent-changeable |
| Spike #1 (reproduced earlier in M7.2b): same-uid approver accepts the agent's own `accept` | T7c PASS = grant given |
| Spike #2: hook-answered `accept` | VERIFIED once, Claude Code |
