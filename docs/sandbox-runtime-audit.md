# Sandbox runtime allowlist audit (M7.3b gate, after F1/F2)

Rules are built in `Plan::rules()` (`src/sandbox/mod.rs`). Labels: VERIFIED /
REASONED / UNKNOWN. No rule was widened by this audit.

| Rule | Rights | Why it exists | Exposure | Class |
|---|---|---|---|---|
| `/proc` (whole) | read | Own `/proc/self/{maps,fd,status,task}`; Git children are addressed via `/proc/<pid>/fd/N` (pinned roots); canary reads `/proc/<server>/stat`. | Every process of the same user: `cmdline`, `environ`, `maps`, `fd` listings readable if the kernel's ptrace check passes (same uid and dumpable). VERIFIED that the same-uid `environ` of a running shell is readable outside the sandbox; REASONED that the rule keeps it readable inside (Landlock does not filter by pid). | **Overbroad.** Needed: `/proc/<own pid>` only. A rule beneath `/proc/<own pid>` is expressible at restrict time. Not tried; the canary and thread-self paths must be re-checked first. A non-dumpable process (see findings D1) closes the same-uid read of its own entries but not of others. |
| `/sys/fs/cgroup` | read | No justification in the code or docs; introduced with the first sandbox (707c481). Likely CPU-count discovery (`available_parallelism`), REASONED. | Cgroup limits and process lists of the user slice. | **Unknown, probably unnecessary.** VERIFIED: removing it keeps all 659 tests green, but no test exercises embedding/thread-pool sizing. Remove only after a real embedding run in the sandbox. |
| `/usr`, `/lib`, `/lib64`, `/bin`, `/sbin` | read | Executables, libraries, Git, `/usr/local` (which is inside `/usr`). | Everything installed system-wide, including anything a package left world-readable. | **Necessary** for exec of Git and dynamic loading; broad by nature. `/usr/local` has no rule of its own: it is covered by `/usr` (VERIFIED by the layout, `ls /usr/local`), so it is not a separate exposure. |
| `extra_runtime_dirs()` | read | Directories of mapped shared libraries plus the Git binary's directory and `git --exec-path`. | After F1 it never takes `/etc`, `/proc`, `/sys`, `/dev`, `/run`, `/tmp`, `/var`, `/`, `/home`, `/root` (VERIFIED by tests). Under a standard layout all of it is inside `/usr`. **Git in the home directory** (e.g. `~/.local/bin/git`, a nix profile) puts that whole `bin` directory under a read rule, which can contain unrelated user programs and scripts. | **Necessary for non-standard layouts, overbroad when the layout puts Git in the home directory.** Not fixed (would break those users silently). Proposal: grant the Git binary itself as `FileExec` plus its exec-path, not the parent directory; verify with the Git probe. |
| `/etc/ld.so.cache`, `ld.so.preload`, `localtime` | file read | Loader and time zone. | Single files. | Necessary. |
| `/dev/urandom` (read), `/dev/null` (rw) | file | Randomness, discarding output. | Single files. | Necessary. |
| roots, db dir, models dir | per grant | The product's purpose. | By design. | Necessary. |

## Order of work (proposal, each step verified before the next)
1. Narrow `/proc` to `/proc/<own pid>`; check canary, Git pinned paths, `doctor`.
2. Narrow the Git directory rule to the binary plus exec-path.
3. Remove `/sys/fs/cgroup` after an embedding run in the sandbox.
