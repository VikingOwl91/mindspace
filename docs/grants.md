# Repository grants (M7.3a)

Implements the first step of [ADR-030](adr-030-jit-authorization.md): approval assurance **A1**. Read the limit first.

> **Limit.** A grant is created by `mind grant ...`, run by you. A program that runs **as your user** can run the same command.
> What this step shows (tests): an MCP tool call, a request, or text in the request log can never create a grant, and the confined
> server cannot write the store. What it does **not** show: that a human decided. `mind grant` cannot tell you from another
> program in your account, and Mindspace has **not verified** that any client confines an agent's shell. So there is no claim
> that an agent *with* a shell is kept from granting itself access. If a client does confine the shell (no access to the store,
> to `mind`, to input devices, to the session sockets; ADR-030, tier T1), the same facts would hold; that is an assumption about
> the client, not something Mindspace checks. An unrestricted same-user shell agent (tier T2) is not protected at all.

## What the words mean
| Term | Here |
| --- | --- |
| Request | a line in `grant-requests.jsonl` (untrusted text from the agent). It changes nothing. |
| Grant | a verified `granted` event in the store, created by `mind grant add` or `approve`. |
| Human consent | **not established**. A grant means "someone ran the command as this user", nothing more. |
| Host assurance | **not measured** in this step (`mind doctor` does not report it yet; ADR-030, section 5a). |
| Agent isolation | an assumption about the client, **not verified**. |
| Running server vs. restart | a running server never gains or loses access; grants, expiry and revocation act when `mind mcp --grants` starts. |

## Use
```sh
mind mcp --grants ...                        # also use the directories you granted (the store is read only with this flag)
mind grant add ~/code/project [--rw] [--ttl 8h]   # shows the canonical directory, asks, records
mind grant list [--all] [--json]
mind grant revoke g-0123456789ab [--reason ...]
mind grant requests                          # requests agents recorded (their text is not trusted)
mind grant approve r-0123456789ab [--ro] [--ttl 8h]
mind grant deny r-0123456789ab
```
The agent side is the MCP tool `repo_request_access(path, access = "ro", reason?)`. It writes one line to a request log next to the
database and answers `status: pending, granted: false`. Nothing about the running server changes. Grants take effect when
`mind mcp --grants` **starts**; a running server never gains access (Landlock cannot be widened). Revoking and expiry also act at
the next start; mid-session revocation needs the per-grant workers of a later step.

## Data model
Event log `grants.jsonl` in the state directory (`$XDG_STATE_HOME/mindspace`, else `~/.local/state/mindspace`; directory 0700, file 0600),
one JSON object per line, chained by SHA-256:

| Event | Fields |
| --- | --- |
| `granted` | `id` (`g-` + 12 hex), canonical `path`, `dev`, `ino`, `access` (`ro`/`rw`), `created_at`, optional `expires_at`, optional `request_id`, `source` (`cli-add`/`cli-approve`) |
| `revoked` | `id`, optional `reason` |
| `request_denied` | `request_id`, optional `reason` |
| `skipped` | `id`, `reason` (a start refused an active grant; recorded once per distinct reason) |

Every line also has `seq`, `ts`, `prev` (hash of the previous line) and `hash`. Pending requests are a separate, untrusted log,
`grant-requests.jsonl` next to the database, which the sandboxed server may append to (at most 50 pending, 256 KiB, input validated).

## Rules
* **Creation:** only the CLI writes `granted`. The confined server can neither write the store (it lives outside the database directory the
  sandbox opens) nor read it after start. A line in the request log that looks like an approval is just malformed data.
* **Path:** canonicalized; must be an existing directory; not `/`; no `:` or NUL; must not equal or contain your home directory; must not
  overlap the grant store or the database directory (a root there would let the server forge grants or damage its state). The same
  store-overlap rule applies to `--allow-root` when `--grants` is given.
* **Confirmation:** `mind grant add|approve` shows directory, access and expiry and asks on a terminal; without a terminal it needs `--yes`.
* **The store file** is opened relative to a checked directory descriptor without following symlinks (`O_NOFOLLOW`), and the OPENED
  file is checked: regular, owned by you, a single hard link, not group/other-writable, at most 8 MiB. The directory must be owned by
  you and not group/other-writable and not a symlink. Writers hold an exclusive `flock` for the whole read-verify-append, readers a
  shared one, so concurrent `add`, `approve` and `revoke` produce one valid chain (tested with 12 and 16 concurrent writers and with
  writers killed mid-run).
* **Crashes and full disks:** an event is one `write` of one line. A last line without a newline (a writer killed or a disk that filled
  up mid-write) is not an event: readers use the verified part before it, and the next writer removes it. A failed write removes its own
  partial line (tested with a file size limit). A complete line that does not verify is an error, never repaired.
* **Validation before use** (every start): the chain and every field verify, otherwise **no grant is used**; the store is a regular file, not
  group/other-writable, at most 8 MiB; the grant is active (not expired, not revoked); the path still canonicalizes to the stored path and
  the directory has the stored device/inode (a replaced directory or a re-pointed symlink is refused); the path rules above hold.
* **From validation to the kernel rule:** the roots go through stage 1 -> argument list -> stage 2, but the Landlock rule is created in stage 1
  from a descriptor opened with `openat2(RESOLVE_NO_SYMLINKS)` and compared with the device and inode that were approved (granted roots) or
  seen when the plan was made (static roots). A directory swapped for another directory or a symlink in that window makes the server refuse to
  start the repository tools (before any restriction is applied). Before this review the rule was created from the path, which followed a
  symlink swapped in after validation (REASONED; the stress test below can only show that nothing widened, not that the old code was open).
* **Stage 2:** `mind mcp` re-executes itself under the sandbox. Stage 1 resolves the grants and passes them as `--allow-root` arguments; stage 2
  does not read the store. The Landlock and seccomp setup is unchanged.
* **Audit:** grant, revoke and deny events are in the log; `mind grant list --json` shows the last refusal per grant.

## What the hash chain is for
It detects accidental damage and naive edits (changing `ro` to `rw`, deleting or reordering lines). It is **not** a defence against a
program running as your user: that program can rewrite the whole chain or simply run `mind grant`.

## Further limits
* Expiry uses the wall clock. Setting the clock back makes an expired grant active again; so does an attacker who can change the clock.
* The check against a replaced directory uses device and inode, which can be reused after a directory is deleted and a new one created; the
  canonical path and the owner of the path must still match, but this is not a cryptographic binding.
* A grant applies to every `mind mcp --grants` started by the same user; there is no per-client or per-session binding in this step, and
  Mindspace does not trust an agent name or a client-supplied session id for anything.
* Dropping the LAST events of the log (for example a `revoked` line) leaves a chain that still verifies; a hash chain without an external anchor
  cannot detect that. Another reason it is not a defence against a same-user attacker.
* Revoking changes the store only. Running servers keep what they started with.

## Not in this step
Mid-session grants, per-grant workers, polkit, hardware tokens, a desktop dialog, client configuration, network approval services,
persistence of sessions, and `mind doctor` reporting of approval assurance (ADR-030, section 5a).
