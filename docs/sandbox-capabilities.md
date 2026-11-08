# Sandbox capabilities: what Mindspace isolates, and what it does not

Applies to Linux. Labels: **VERIFIED** (observed by an executed test or command), **REASONED** (follows from code or
documented kernel semantics), **UNKNOWN** (not examined). This page is deliberately narrow: it states what the code
enforces today, not what the kernel could do.

## What is enforced today

`mind mcp` with at least one allowed root restricts itself (ADR-029, `src/sandbox/`) with **Landlock filesystem rights of
ABI 3** and, since M7.2e, on x86_64 with a **socket-deny seccomp filter** ([sandbox-seccomp.md](sandbox-seccomp.md)); both are
verified with probes that must be denied. That is the only isolation Mindspace applies.

| Channel | Status in `mind doctor` | Basis |
| --- | --- | --- |
| Filesystem | `enforced` (when the self-test shows the sandbox active) | Landlock ABI 3 rules; canary probes at every start. VERIFIED by the self-test and `tests/doctor.rs`. |
| TCP, UDP, unix pathname sockets, unix abstract sockets | `enforced` on x86_64 with typed evidence from the self-test child; otherwise `supported_not_enforced` / `not_supported` / `unknown` | the seccomp filter denies creating NEW sockets (`src/sandbox/seccomp.rs`); already open descriptors (stdio) stay usable. Not network or IPC isolation. |
| Signals | `supported_not_enforced` / `not_supported` / `unknown` | no Landlock scope and no filter for signals. |

Landlock at the project's minimum rule set alone would let a confined process connect to any unix socket path or abstract socket
it can name, connect to TCP and send UDP (VERIFIED for an equivalent rule set by `tests/seccomp_isolation.rs`,
`landlock_alone_fences_files_but_not_sockets`, and spike M7.2b-2, commit `b38ef3b`). The seccomp filter closes the creation of
new sockets; it can still signal same-user processes and use descriptors it already has.
Without allowed roots no sandbox is applied (there is nothing to protect; the repository tools are off).

`mind doctor` itself is **not** confined: it applies no Landlock rule and no seccomp filter to its own process. The
`capabilities` block describes what a `mind mcp` started with `--allow-root` applies, and says so
(`doctor_process_confined_by_mindspace: false`). `enforced` is reported only if the product code declares the channel AND the
self-test shows the sandbox active. A kernel ABI number alone never yields `enforced`. Each channel entry carries `applies_to`
to make this explicit: `enforced` describes the self-test child `mind mcp`, a snapshot of this machine, not the doctor.
(VERIFIED with `strace -f`: `landlock_restrict_self` occurs only in the child; the doctor process issues only
`landlock_create_ruleset` probes and a `seccomp(GET_ACTION_AVAIL)` query.)

## What the kernel could add (`supported_not_enforced`)

Probed read-only by creating a Landlock ruleset and closing it (never `restrict_self`) and by `seccomp(GET_ACTION_AVAIL)`.
Landlock network rules (TCP), scopes (abstract unix sockets, signals) and a pathname-socket right need newer ABIs; the
exact kernel release for each is REASONED from documentation, so the doctor probes the right itself instead of mapping an
ABI number. UDP has no Landlock right at all (VERIFIED on ABI 10, spike M7.2b-2); only seccomp can stop creating UDP
sockets.

## Channels that stay open

* Everything in the `supported_not_enforced` / `not_supported` rows above.
* Descriptors that are already open or connected when a sandbox starts: Landlock (VERIFIED) and a socket-creation seccomp filter
  (VERIFIED, `tests/seccomp_isolation.rs`) do not close them. A pre-connected socket can also receive further descriptors
  over `SCM_RIGHTS` (REASONED, not tested).
* Same-user attackers: a process with the user's rights can edit settings, hooks and approval files (spike M7.2b-1/-2:
  a Claude Code `Elicitation` hook answered `accept` without a human; VERIFIED once, one client).

## Why Landlock is not process isolation

Landlock restricts access rights of one process tree to kernel objects it mediates. It does not hide other processes,
does not isolate the network namespace, does not cover UDP, and, below the newest ABIs, does not cover unix sockets or
signals. It is a filesystem fence plus whatever further rights the rule set handles.

## Why seccomp is not a full isolation

The filter denies new sockets, `io_uring_setup`, `pidfd_getfd`, `ptrace` and `process_vm_*`, and kills foreign-architecture and x32
calls. It is not a syscall allowlist, does not cover inherited descriptors, signals or `/proc` reads, and is x86_64 only
(other architectures: explicitly not applied and reported). Details, error policy and evidence: [sandbox-seccomp.md](sandbox-seccomp.md).

## Not examined (UNKNOWN)

Kernels older than 7.2 and Landlock ABIs 3 to 9 on real kernels (profiles in the spikes are rule sets run on an ABI 10
kernel, not an emulation of older kernels); non-Linux platforms; separate-user isolation; a confined broker.

References: spike #1 (commit `49c027a`, `docs/spikes/m7.2b-jit-permissions.md`) and spike #2 (commit `b38ef3b`,
`docs/spikes/m7.2b-elicitation-sandbox.md`) on the spike branches. They are experiments on one machine, not a security approval.
