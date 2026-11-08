# Socket-deny seccomp filter in the sandboxed `mind mcp` (M7.2e)

Linux x86_64 only. Labels: VERIFIED (executed test), REASONED (from code/kernel semantics), UNKNOWN. This page states what the
filter does and does not do; it makes no claim of network or IPC isolation. Evidence: `tests/seccomp_compat.rs`,
`tests/seccomp_isolation.rs`, `tests/doctor.rs`, unit tests in `src/sandbox/seccomp.rs`; spikes M7.2b-1/-2 (commits `49c027a`,
`b38ef3b`) and M7.2d (`c8d30b0`) are experiments, not approvals.

## Threat model

| Question | Answer |
| --- | --- |
| Which processes are restricted? | A `mind mcp` that runs the sandbox (roots configured, no `--insecure-no-sandbox`): stage 1, stage 2 (re-exec) and every child (Git). NOT restricted: every other command (`mind model install` needs sockets), `mind mcp` without roots, `--insecure-no-sandbox`, `mind doctor` itself. VERIFIED (`other_commands_and_unsandboxed_servers_are_not_filtered`). |
| Which threads? | The filter is per thread and survives `execve`. It is installed on the thread that later calls `exec`, so stage 2 has it whatever other threads exist (REASONED). Defence in depth: installation is refused unless the process has exactly one thread. Every thread of the running server shows `Seccomp: 2` (VERIFIED). |
| Which descriptors exist at installation? | `close_inherited_fds` runs first in both stages; installation is refused if anything above stderr is open. Only 0, 1, 2 remain. VERIFIED: a dormant TCP socket, a connected unix socket and a file inherited at fds 100..102 never reach the server. |
| Where is the filter installed? | First thing in `sandbox::enter` (stage 1), before layout checks, before the Git child that learns Git's exec path (found by test: that child ran unfiltered when the filter came later), before `restrict_self`. |
| What does seccomp do? | Denies with `EACCES`: `socket`, `io_uring_setup` (a ring can create sockets itself), `pidfd_getfd`, `ptrace`, `process_vm_readv/writev` (taking descriptors from, or acting inside, another process of the same user; Yama blocked this on the development host but is not a given). Kills the process on a foreign architecture and on x32-numbered calls. |
| What does Landlock do? | Filesystem access, ABI 3 rules, unchanged. It does not restrict sockets at that ABI. |
| What stays open? | Descriptors open at start, notably stdio: a client may hand over a socket as transport and it stays usable (VERIFIED `stdio_sockets_remain_usable_...`); `SCM_RIGHTS` received over such a socket (REASONED); `socketpair` and pipes (anonymous, no external peer; VERIFIED allowed); signals; reading `/proc`; filesystem sockets that are inherited. |

## x86_64 ABI special cases (found while testing)

On the development kernel (7.2.14-zen) an **x32-numbered `socket` call creates a socket** and **32-bit `int 0x80` is served**
(VERIFIED, `x32_and_32_bit_entry_points_are_killed_not_filtered_around`). A filter that checks only the architecture value and the
number of `socket` (the M7.2d experiment's filter) is bypassed by the x32 number (VERIFIED, negative control). The product program
therefore kills (`SECCOMP_RET_KILL_PROCESS`) every call whose architecture is not x86_64 and every number with bit 30 set;
the child tests show both are killed by `SIGSYS`. The BPF program is checked by a small interpreter in the unit tests.

## Error policy

| Situation | Behaviour |
| --- | --- |
| x86_64, kernel supports seccomp, install and verification succeed | `enforced`; the typed `seccomp` field in `repo_outline`/`repo_ingest` results and `mind doctor` say so. |
| x86_64, install fails (EPERM, ENOSYS, EINVAL, ENOMEM, container policy) | **fail closed**: stage 1 returns `Unavailable("seccomp: ...")`, the repository tools stay OFF, memory tools work, nothing was restricted or ingested. VERIFIED (`a_filter_that_cannot_be_installed_keeps_the_repository_tools_off`, doctor test). |
| x86_64, more than one thread / open descriptors at installation | refused, same as above. Unit-tested. |
| Other architecture (no verified audit constant) | explicitly **not applied**: stderr line from stage 2, `not_applied` in the report; Landlock stays. The socket channels are not shown as enforced. |
| Kernel built without seccomp (`PR_GET_SECCOMP` gives `EINVAL`) | same "not applied" (kernel). |
| Cannot tell (the query is blocked by a policy, e.g. `seccomp()` answers `ENOSYS`) | treated as **required**, not as unsupported: if installation then works (via `prctl`) the filter is active (VERIFIED), otherwise fail closed. A policy must not look like an old kernel. |
| Pre-existing filter (container) | filters stack; ours is added. VERIFIED with an unrelated pre-existing filter. A policy that forbids installing takes the fail-closed row. |
| Stage 2: filter required but not demonstrably active | the start is refused (exit 78). The expectation is derived again from architecture and kernel in stage 2, not from an environment variable. |
| An opt-out | none. `--insecure-no-sandbox` still drops the whole kernel sandbox as before. |

## What "active" means

`Seccomp: 2` only says that some filter exists, and it is read from the calling thread (`/proc/thread-self`). The server
therefore also runs negative probes that must fail with `EACCES` (`socket` for UNIX/INET/INET6/NETLINK, `io_uring_setup`,
`pidfd_getfd`, `ptrace`, `process_vm_*`) and positive controls that must work (`socketpair`, `pipe`). A filter that denies only
`socket` (mode 2) is rejected (VERIFIED, `the_verification_checks_the_policy_not_just_the_filter_mode`). x32/`int 0x80` cannot be
probed at runtime without killing the process; that evidence is test-time only.

## `mind doctor`

`checks[id=sandbox].details.capabilities.seccomp_evidence` is parsed from the self-test child's typed `seccomp` object (state
plus its probes); free text is not interpreted. The socket channels (`tcp`, `udp`, `unix_path_sockets`, `unix_abstract_sockets`)
are `enforced` only with that evidence AND an active sandbox, and then carry `limits` ("NEW sockets only ... not network or IPC
isolation"). Kernel support stays in `kernel.*`; `signals` and the filesystem are unchanged. A blocked `seccomp` query is reported
as `unknown`, not `no`.

## Adversarial review (before the M7.2e commit)

| Check | Result |
| --- | --- |
| BPF correctness, second path | `scripts/check_seccomp_bpf.py` re-simulates the dumped program (own implementation, separate from the Rust interpreter) for 7 architecture values x 1,109 numbers incl. x32 and boundary values against a plain-language specification: 0 mismatches. The syscall numbers it uses were compared with `/usr/include/asm/unistd_64.h` (41, 101, 310, 311, 425, 438). Jump offsets re-derived: every jump lands inside the program, forward only. VERIFIED. |
| BPF in the kernel | The kernel accepted the program; real calls behave as specified: denied calls `EACCES`, x32 `socket` and x32 `getpid` and `int 0x80` end the process with `SIGSYS`; ordinary calls (files, threads, spawn, Git) work. VERIFIED on 7.2.14 only. |
| Unknown architecture | `program()` is `None`; the expectation is "not applied", reported. Unit-tested; no non-x86_64 hardware: UNKNOWN. |
| `socketpair` | allowed, and a control probe requires it. A profile that forbids it fails the verification and the tools stay off. VERIFIED. |
| `SCM_RIGHTS` | **a socket received over a channel that already exists is fully usable** (VERIFIED boundary; all modes incl. filtered). The filter cannot tell it from an ordinary read. |
| stdio sockets | usable, by design (the transport). VERIFIED. |
| `pidfd_getfd`, `ptrace`, `process_vm_readv` on a same-user victim | `EACCES` by the filter; unfiltered they are not `EACCES` (Yama decides). VERIFIED. |
| `io_uring_setup` | `EACCES`; a ring inherited at fd 103 never reaches the server (closed with the other descriptors). VERIFIED. |
| Descriptors across `exec` | dormant socket, connected socket, file and ring fds inherited into `mind mcp` are closed before the filter; none is open in the server. VERIFIED. The filter itself is inherited across `exec` (stage 2, Git; all 16 invocations). |
| Fail closed | install refused (EPERM / ENOSYS / `seccomp()` and `prctl` blocked): tools off. Verification failure (profile denying `socketpair`): tools off. Unknown kernel support: required, installs via `prctl` when possible. Landlock blocked after the filter: tools off, filter kept. Several threads / open descriptors at install: refused (unit-tested with injected observations; not reachable from outside without a product change). VERIFIED unless noted. |
| Finding fixed in this review | "kernel without seccomp" now needs `EINVAL` from `prctl` AND no `Seccomp:` line in `/proc/thread-self/status`. Before, a policy answering `EINVAL` to one call could have been read as an unsupported kernel, i.e. a silent downgrade. |

Not tested (UNKNOWN or REASONED only): the exit-78 path of stage 2 end to end (the check itself is tested in a child; a
test would need a Landlock-confined process without the filter and a product seam); `/proc/<pid>/mem` of a same-user process
(Landlock grants no write right on `/proc`, REASONED); behaviour of a 32-bit ELF started by the server (it would be killed at its
first syscall, REASONED); effects of `NO_NEW_PRIVS` on set-uid helpers (none are used, REASONED). `SIGSYS` kills the whole
server; the client sees the end of its pipe.

## Remaining risks

* Existing descriptors incl. stdio sockets, `SCM_RIGHTS`; signals; `/proc` reads. No network or IPC isolation is claimed.
* Deny list, not allowlist: a future socket-creating syscall would pass. Mitigated only by review when kernels add one.
* Verified on one kernel and one machine (x86_64, Linux 7.2.14). Other kernels, containers with strict profiles, real MCP clients
  (Claude Code, Codex, OpenCode) under the filter: UNKNOWN. aarch64 and every other architecture: not applied (needs its own
  verified constants and an arm64 CI job).
* x32 and `int 0x80` handling depends on the kernel configuration of the host; the filter does not.
* Embedding model use inside the filtered server: VERIFIED once with `M72D_MODEL_SRC` (opt-in, about 75 s in a debug build).
