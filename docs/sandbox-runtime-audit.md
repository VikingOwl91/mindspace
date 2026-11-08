# Sandbox runtime allowlist audit (M7.3b gate, after F1/F2)

Rules are built in `Plan::rules()` (`src/sandbox/mod.rs`). Labels: VERIFIED /
REASONED / UNKNOWN. No rule was widened by this audit.

| Rule | Rights | Why it exists | Exposure | Class |
|---|---|---|---|---|
| `/proc` (whole) | read | Own `/proc/self/{maps,fd,status,task}`; Git children are addressed via `/proc/<pid>/fd/N` (pinned roots); canary reads `/proc/<server>/stat`. | Every process of the same user: `cmdline`, `environ`, `maps`, `fd` listings readable if the kernel's ptrace check passes (same uid and dumpable). Correction (kernel-tested, see the update below): another process's `environ` and `maps` were already refused inside the sandbox by the kernel's Landlock ptrace scoping; `cmdline`, `fd` listings, `/proc/1/status` and the process list were readable. | **Overbroad.** Needed: `/proc/<own pid>` only. A rule beneath `/proc/<own pid>` is expressible at restrict time. Not tried; the canary and thread-self paths must be re-checked first. A non-dumpable process (see findings D1) closes the same-uid read of its own entries but not of others. |
| `/sys/fs/cgroup` | read | CPU-quota detection: Rust's `available_parallelism` reads `/proc/self/cgroup` and then `cpu.max` up the cgroup chain. VERIFIED by strace of a real sandboxed semantic search (see the cgroup section below). | Read-only cgroup metadata of the user slice. | **Necessary for honouring CPU quotas; kept.** The earlier "unknown, probably unnecessary" class was wrong. |
| `/usr`, `/lib`, `/lib64`, `/bin`, `/sbin` | read | Executables, libraries, Git, `/usr/local` (which is inside `/usr`). | Everything installed system-wide, including anything a package left world-readable. | **Necessary** for exec of Git and dynamic loading; broad by nature. `/usr/local` has no rule of its own: it is covered by `/usr` (VERIFIED by the layout, `ls /usr/local`), so it is not a separate exposure. |
| `extra_runtime_dirs()` | read | Directories of mapped shared libraries plus the Git binary's directory and `git --exec-path`. | After F1 it never takes `/etc`, `/proc`, `/sys`, `/dev`, `/run`, `/tmp`, `/var`, `/`, `/home`, `/root` (VERIFIED by tests). Under a standard layout all of it is inside `/usr`. **Git in the home directory** (e.g. `~/.local/bin/git`, a nix profile) puts that whole `bin` directory under a read rule, which can contain unrelated user programs and scripts. | **Necessary for non-standard layouts, overbroad when the layout puts Git in the home directory.** Not fixed (would break those users silently). Proposal: grant the Git binary itself as `FileExec` plus its exec-path, not the parent directory; verify with the Git probe. |
| `/etc/ld.so.cache`, `ld.so.preload`, `localtime` | file read | Loader and time zone. | Single files. | Necessary. |
| `/dev/urandom` (read), `/dev/null` (rw) | file | Randomness, discarding output. | Single files. | Necessary. |
| roots, db dir, models dir | per grant | The product's purpose. | By design. | Necessary. |

## Order of work (proposal, each step verified before the next)
1. Narrow `/proc` to `/proc/<own pid>`; check canary, Git pinned paths, `doctor`.
2. Narrow the Git directory rule to the binary plus exec-path.
3. `/sys/fs/cgroup` stays (see below).

## Update after the narrowing tasks (kernel-tested, uncommitted on the task branches)

* `/proc` (branch `feat/m7.3b-proc-narrowing`): the rule is now `/proc/<own pid>`. Corrects the table above:
  VERIFIED that the kernel already refuses another process's `environ` and `maps` to a process in a different
  Landlock domain (EACCES before our change); what the whole-`/proc` rule did expose was other processes'
  `cmdline`, `fd` listings, `/proc/1/status` and the process list. All of these are EACCES now
  (`sandbox::tests::the_kernel_hides_other_processes_from_a_sandboxed_process`, before and after values asserted).
  If the mounted `/proc` is not the one of this pid namespace, no `/proc` rule is made (fail closed, no fallback);
  VERIFIED by hand with `unshare -Upf` (the automated test does not cover it). No new ABI needed (fs rules, ABI 3).
* Scope of the rule: `/proc/<own pid>` names this process only. It does not cover the pid directories of children
  started later (a Git child has its own `/proc/<child pid>`); the child reaches the pinned roots through the
  parent's `/proc/<parent pid>/fd/N`, which the rule does cover. VERIFIED by the Git ingestion tests passing.

## Update: Git binary rule

* The binary is granted as one file (`FileExec`), not its directory. VERIFIED by kernel test (copy under a fake home,
  symlink to a second directory, replaced executable that fails closed). Before: a sibling file in the Git directory
  was readable. The exec-path directory is still granted as a directory (helpers live there); a Git whose own shared
  libraries sit outside `/usr` is not covered (UNKNOWN).

## Update: `/sys/fs/cgroup` (evidence, no code change)

Run: `mind mcp` sandboxed, the locally installed `nomic-embed-text-v1.5`, a temporary database with two indexed
memories, one `memory_search` with `mode: semantic`; strace of the server and its threads.

* `available_parallelism` needs cgroup CPU-quota data: the server opens `/proc/self/cgroup` and `cpu.max` at each level
  of its cgroup chain under `/sys/fs/cgroup`.
* Removing the rule changes the thread behaviour. Without a quota the results were identical (scores 0.7221 and
  0.4206, 38 threads both ways, the `cpu.max` reads failing with EACCES without the rule). Under
  `systemd-run --user --scope -p CPUQuota=100%` the server created 23 threads with the rule and 38 without it: the
  quota is ignored.
* The existing rule stays. Narrowing it to the `cpu.max` files of the ancestor chain is optional, fragile if the
  process changes cgroup, and not part of this changeset.
