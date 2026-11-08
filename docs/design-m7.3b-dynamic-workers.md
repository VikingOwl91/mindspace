# Design note: dynamic repository workers (M7.3b)

Status: **design note, not implemented**, 2026-11-08. Basis: `main` at `f9a1206` (M7.3a grant core, M7.3a.1 approval diagnostics),
ADR-029 (sandbox), ADR-030 (grants), the seccomp work (M7.2e) and spike #1 (`49c027a`, `docs/spikes/m7.2b-jit-permissions.md` on that branch).
Labels: **VERIFIED** (executed, here or in a cited spike), **REASONED**, **UNKNOWN**.

> **Assurance limit (unchanged).** The grant store is written by `mind grant`, which any program in the user's account can run. M7.3b
> makes grants usable **without restarting the server**; it does not make it harder for an agent with a shell to create one. It must not claim
> more than M7.3a.1 reports: **A0 verified**, A1 supported and configured but not verified. Nothing below assumes otherwise.

## 1. What problem it solves

Today a grant is used only when `mind mcp --grants` **starts**, because the whole server is one Landlock domain whose roots are fixed
(a domain can only shrink; VERIFIED in spike #1, T7a: "a rule added by a child cannot widen the parent's domain"). After a human runs
`mind grant approve`, the agent still has to wait for a restart. M7.3b lets the running server use a newly granted directory.

**Consequence of the kernel rule.** A child of the confined server can never be given more rights than the server has. The process that
starts a worker for a new directory must therefore be **outside** the server's Landlock domain: a small, separate **broker**.

## 2. Findings in the existing code (REASONED from reading)

* `repo_ingest` runs in the sandboxed server process, which also holds the database. Extraction and persistence are mixed in one call, but the
  seam is narrow:
  * Directory: `ingest::confined::ingest_directory` reads files through an `Anchor` (`observe_bytes` -> `ObservedFile`), then
    `ingest::record_observed_files` parses source files (tree-sitter, in-process) and calls `store.record_observation` and
    `store.record_code_observation`.
  * Git: `ingest_git_with_reader` reads objects through a `GitReader` (Git child processes), builds a `GitObservation`, then calls
    `store.record_git_observation` and `store.resolve_git_code_relationships`.
  * The only database **reads** during extraction are `resolve_spaces`/`get_space` (names -> ids) and `has_code_derivation` (an optimization
    that skips parsing a known blob). Everything else before the `record_*` calls is pure.
* The transfer types (`ObservedFile`, `IngestObservation`, `GitObservation`, `CodeObservation` and what they contain) derive `Debug`/`Clone`
  but not `Serialize`/`Deserialize`; a wire format needs serde derives on that tree.
* `sandbox::enter` (stage 1) restricts itself and **replaces** itself by `exec`; there is no surviving parent. `verify_stage2` rejects any
  descriptor above stderr. Both must change for a broker (section 5).
* Reusable unchanged: seccomp program, installation and verification; `open_verified_dir` (`openat2` without symlinks + device/inode check);
  `build_ruleset` rules made from a verified descriptor; the canary and Git probes; `grants::{Store, validate_root, resolve_for_start}`;
  the confined `Anchor` ingest code.

## 3. Architecture

```
                         MCP client (+ agent)
                               │ stdio
                               ▼
 ┌───────────────────────────────────────────────────────────────────────────────────────────┐
 │ `mind mcp --grants`   (one process tree per MCP session; no shared daemon)                │
 │                                                                                           │
 │  BROKER (stage 1 stays alive)             FRONTEND (today's stage 2, unchanged domain)    │
 │  · NOT Landlock-confined, seccomp'd       · Landlock: static roots + db dir + system      │
 │  · reads the grant store (fresh, per op)  · holds the DATABASE (only writer)              │
 │  · no stdio, no db, no parsing            · seccomp socket-deny (as today)                │
 │  · opens roots (openat2, no symlinks,     · parses MCP; asks the broker for dynamic paths │
 │    dev/ino from the grant)                · PERSISTS what a worker extracted              │
 │  · starts / kills workers                 · cannot read or write the grant store          │
 │            ▲   fd 3: unix socketpair, one inherited descriptor, no path   ▲               │
 │            └────────────────── fixed request vocabulary ────────────────────┘             │
 │       │ fork+exec per operation: root dir fd (O_PATH) at fd 4, request on a pipe/socketpair│
 │       ▼                                                                                   │
 │  WORKER (`mind worker`, one per operation)                                                │
 │  · Landlock: system read paths + THE ONE root fd (ro or rw); no db, no store, no models   │
 │  · seccomp socket-deny (inherited), own process group, killed at TTL / revoke / deadline  │
 │  · runs the existing confined extraction; emits an observation, never touches the db      │
 └───────────────────────────────────────────────────────────────────────────────────────────┘

 Data flow (dynamic path):  agent ─tool─▶ frontend ─{op,path,selection}▶ broker ─validates against the store─▶ worker
                            worker ─observation (framed, size-capped)▶ broker ─▶ frontend ─▶ SQLite (frontend only)
```

## 4. Answers

1. **Where extraction and persistence separate.** Introduce `extract_*` (reads and parses, returns a plain `Extracted` value) and `persist(store,
   Extracted)` next to `record_observed_files` / `ingest_git_with_reader`; the in-process path (static roots) is `persist(extract(..))`
   and keeps today's behaviour and tests. Space names are resolved to ids by the frontend and passed in; the `has_code_derivation` shortcut is
   dropped in the worker (it parses every code blob; the store already ignores a known derivation), a cost to measure.
2. **Who owns database write rights.** The frontend only (as today). The worker's Landlock domain has no rule for the database directory.
   The M6 write fence is process-local and stays that way.
3. **Starting and ending workers.** One worker **per operation** (no pool): the broker forks and execs `mind worker` with a cleared environment,
   its own process group, `PR_SET_PDEATHSIG`, and exactly two descriptors above stderr: the root directory (`O_PATH`) and the request/response
   channel. The worker applies Landlock from the root descriptor, runs the canary, then reads one request. It ends when done, at the deadline,
   at TTL expiry, on revocation, or when the broker dies; the broker kills the whole process group (Git children included) and reaps it.
4. **Binding grant, path, rights, TTL to the worker.** The frontend sends only data (`op`, a path, a selection, a deadline). The broker
   re-reads and verifies the store, finds an **active** grant whose canonical root contains the path, re-opens that root with `openat2`
   (no symlinks) and compares device/inode with the grant, derives `ro`/`rw` and the TTL **from the grant**, and passes the descriptor. The worker
   gets the access mode from its Landlock rule (the kernel enforces it), not from a flag. The grant id travels only as an audit label.
   The worker resolves the request path beneath the root descriptor (`RESOLVE_BENEATH`, no magic links).
5. **Preventing an agent from activating a grant itself.** The MCP surface has no way to name, create or confirm a grant: the broker accepts
   a fixed vocabulary (`outline`, `ingest_extract`, `ping`), takes **no** grant id, and decides from the store. The frontend cannot write the
   store (outside its Landlock domain, VERIFIED in M7.3a tests) and the broker never writes it. **This does not stop a shell agent that runs
   `mind grant` as the user** (assurance A0, reported by `mind doctor`); M7.3b adds no protection and no claim there.
6. **Revocation.** *New* operations: the broker re-reads the store before every operation; a revoked or expired grant is refused. *Running*
   operations: while a worker runs the broker re-checks the store about every second and at TTL, and kills the worker group on revocation or
   expiry (spike #1, T9: a running call ended after 0.6 s). The frontend then gets an error and persists nothing for that operation. Data a
   worker had already delivered before the revocation may be persisted; memories created from a grant are **not** deleted by revoking it.
7. **Crashes and restarts.** *Worker crash or timeout:* the broker reports an error, the frontend persists nothing (it persists only a
   complete, validated result inside one transaction); no zombie (reaped, group killed). *Broker death:* the frontend loses its channel; dynamic
   operations fail closed with a clear error, static roots and memory tools keep working; workers die with the broker (PDEATHSIG, VERIFIED T10b).
   *Frontend death or client disconnect:* the broker sees the exit, kills its workers and exits with the frontend's status (T10c). Nothing is
   persisted by the broker, so a restart starts clean; grants live only in the store.
8. **Parallel sessions.** Each `mind mcp` session has its own broker, frontend and workers (separate processes, separate channels); a worker's
   descriptors cannot name another session's. They share the user's grant store, as today (no per-session binding in M7.3a; a binding is a later,
   separate decision). Two operations on the same root run in two workers (T12: 120 concurrent writes, no corruption); SQLite serialises persistence.
9. **Path TOCTOU and inherited descriptors.** The broker never passes a path to the worker, only a verified descriptor. The check and the use are
   the same descriptor (`open_verified_dir`, M7.3a). Landlock rules come from that descriptor (already implemented for static roots). Before
   confinement the worker closes everything except its two descriptors and verifies this (like `verify_stage2`, with an allowlist of two numbers
   instead of none). The frontend gets its channel as exactly one more descriptor (fd 3), checked to be a unix stream socket; any other
   descriptor stays a start error. Descriptors that already exist remain a documented boundary (M7.2e).
10. **Reuse.** See section 2. New: the broker loop, the worker mode, the framing, the serde types, the `extract`/`persist` split, `verify_*` with a
    descriptor allowlist, and fork-and-stay in place of `exec` in stage 1.

## 4a. Domains and trust (explicit)

| Process | Landlock domain | seccomp (socket-deny) | Database | Grant store | Trusts |
| --- | --- | --- | --- | --- | --- |
| Broker (stage 1 stays alive) | **none** in M7.3b (self-confinement is an open PoC, section 9) | yes, installed before the fork | never | read-only use, fresh per operation | the kernel and the store; **nothing the frontend or a worker says** |
| Frontend (today's stage 2) | **its own**: static roots, database directory, system paths (unchanged) | yes (as today) | **sole writer** | no access (outside its domain) | nothing from workers (their output is validated); asks the broker, cannot instruct it |
| Worker (per operation) | **a third, narrower one**: system read paths + the one root descriptor; no database, no store, no models | yes (inherited) | no access | no access | the root descriptor and one request |

The three domains are different on purpose: the broker must be able to open new roots (so it cannot sit inside the frontend's domain), and the
frontend must not be able to (so it cannot be the broker).

**What the broker does not trust.** From the frontend it accepts a fixed vocabulary of requests carrying only data (an operation name, a path,
a selection, a deadline). A grant id, access mode, TTL or "this path is approved" in a request is **not** a field of the protocol and would be
rejected as malformed; the broker derives all of them from the store. It also does not trust a worker's exit status or output for anything but
reporting.

**Worker output is untrusted.** A worker parses hostile repository content (Git objects, tree-sitter); a parser bug could make it send arbitrary
bytes. The frontend therefore validates everything before the single persisting transaction: strict schema (unknown fields rejected), sizes and
counts against the limits below, UTF-8, path locations beneath the requested root, content hashes **recomputed** from the received content,
object ids and counts consistent with the content, and no identifiers chosen by the worker (the frontend allocates ids, resolves space names
itself). A worker can at worst lie about the content of the repository it was allowed to read; it cannot name another repository, a space, a row
or a file outside its root.

## 4b. IPC protocol (planned; nothing is implemented)

One protocol, used on both hops (frontend <-> broker over the inherited socketpair, broker <-> worker over the worker's channel).

* **Framing.** `type: u8`, `length: u32` big-endian, then `length` bytes. Types: `REQUEST`, `CHUNK` (a batch of extracted records), `DONE`
  (counts and a SHA-256 over all chunk payloads), `ERROR` (code and a message of at most 1 KiB), `CANCEL`. A frame with an unknown type, a length above the
  limit, a payload that is not strict UTF-8 JSON for its type, or out of order (`CHUNK` before `REQUEST`, anything after `DONE`) is a protocol
  violation.
* **Limits (initial, to be tuned by measurement):** frame payload 1 MiB; request 16 KiB; at most 100,000 frames and 512 MiB of chunk payload per
  operation; per-file content as today's ingest limits; `DONE` must match the counts and the hash. Larger repositories fail with an actionable
  error ("use `paths`"), like the unscoped-ingest limit today. Only the limit values are provisional.
* **Streaming and memory.** The worker sends records in chunks while it reads; the broker relays each chunk through a bounded buffer (one frame) and
  never accumulates the result; the frontend accumulates up to the byte limit in memory (records hold `Arc<str>`/`Bytes`-style shared content so a
  large file is held once), validates after `DONE`, then persists in one transaction. Nothing is written before validation succeeds.
* **Errors.** Any violation, timeout, hash/count mismatch, worker exit without `DONE`, or `ERROR` frame ends the operation: the frontend discards what
  it received and returns one error to the tool caller; nothing is persisted; no retry is attempted automatically.
* **Worker abort.** The broker kills the worker's **process group** (Git children included) and reaps it on: a protocol violation; a size or frame-count
  overrun; no progress for N seconds (per-frame deadline) or the operation deadline; TTL expiry; revocation seen by the periodic store check; a
  `CANCEL` from the frontend; frontend disconnect; broker shutdown. The worker also exits on EOF of its channel and carries `PR_SET_PDEATHSIG`.
  Killing is the only abort mechanism (no cooperative flag the worker could ignore).

## 5. Security invariants (to be tested)

* I1. The worker has no database, grant store, model or home access; its writable set is the one granted root (rw) or nothing (ro).
* I2. The root is fixed by a descriptor checked against the granted device/inode; a swapped path never widens it.
* I3. Rights and TTL come from the store at the time of the operation, never from the frontend or the worker.
* I4. The frontend has no channel that can create, change or confirm a grant, and no filesystem path to the store.
* I5. A worker (and its Git children) cannot create sockets (seccomp, inherited) and holds no descriptor except its two.
* I6. Revocation or expiry ends a running worker within a bounded time; nothing from it is persisted afterwards.
* I7. Any failure (worker, broker, protocol, verification) ends in an error for that operation and never in broader access or a partial write.
* I8. The broker parses only fixed-size, length-checked frames and never touches MCP traffic, the database or repository content.
* I9. The reported approval assurance does not change; `mind doctor` keeps A0 unless agent isolation is verified. This design claims nothing above A0.
* I10. The broker takes no grant id, access mode, TTL or approval claim from the frontend or a worker; they exist only in the store.
* I11. The broker runs under the existing socket-deny seccomp policy and in a Landlock domain different from the frontend's and the worker's.
* I12. The frontend is the only process that writes the database; it persists only a fully received, validated result, in one transaction.
* I13. Worker output is untrusted data: schema, sizes, locations and hashes are checked, and the worker chooses no identifier.
* I14. A revocation or expiry is checked **before** every new operation and **during** every running one; the two cases are tested separately.

## 6. Alternatives and trade-offs

| Alternative | Why not (for now) |
| --- | --- |
| Child process of the confined server | Impossible: a child cannot receive rights the parent lacks (VERIFIED, spike #1). |
| Restart the server after a grant | Client reconnect behaviour is UNKNOWN for all three clients (spike #2); session state would be lost. |
| Server unconfined, workers only for repository content | Gives up today's guarantee that the process holding the database and parsing MCP is confined. |
| Shared persistent broker daemon (systemd user unit) | Larger and longer-lived TCB, cross-session confusion, install burden; no gain over per-session brokers for the MVP. |
| Worker writes the database | Widens the worker's rights; the M6 fence is process-local. |
| Long-lived per-grant workers / a pool | More state to revoke and leak; per-operation workers make revocation and cleanup trivial (cost: one process start per operation, to be measured). |
| Self-confining the broker with Landlock now | Whether `O_PATH` opens work under a confined broker is UNKNOWN (spike #1 §4); schedule a PoC, keep the broker small and seccomp'd meanwhile. |

Trade-off accepted: the broker is a new **unconfined-by-Landlock** process. Its size is the main risk, so it stays tiny, single-threaded, parses
nothing but its own frames, runs under the socket-deny filter, and is covered by the invariants above.

## 7. Test strategy

* Unit: frame encoding/limits (oversize, truncated, hostile), grant -> worker-parameter derivation (ro/rw, TTL, containment of the path), serde round trips of the transfer
  types, `extract`/`persist` equivalence with the current in-process ingest (same database contents for directory, git, selections, re-ingest idempotence).
* Worker alone (no broker): under Landlock+seccomp it cannot read outside the root, open the database, create a socket, signal, or keep a descriptor; it exits on EOF.
* Broker with a fake worker: kill on TTL, on revoke, on deadline; process-group cleanup; zombie-free; broker death kills workers; worker crash leaves no persistence.
* Adversarial frontend: forged ops, grant ids, paths outside every grant, `..`, symlink swaps (stress, as in M7.3a), oversize frames, flooding.
* Descriptor and TOCTOU: swapped root between validation and spawn (refused); inherited-fd audit of worker and frontend.
* End to end through a real client: request -> `mind grant approve` -> the running server ingests the directory without restart; revoke -> next op refused; mid-op revoke ends it.
* Regression: all current Landlock/seccomp/doctor/MCP suites unchanged for static roots; `mind doctor` assurance stays A0.

## 8. Smallest implementable tasks (each shippable alone)

1. **Split only.** `extract`/`persist` for directory ingest, in-process; serde derives for the transfer types; round-trip and equivalence tests. No behaviour change.
2. Same split for Git ingest (`GitObservation`).
3. **`mind worker` mode** (hidden), driven by a test harness: sandbox from a root descriptor, request in, framed result out; the worker-alone tests above.
4. **Broker core** as a library: spawn, descriptor passing, deadlines, kill-by-group, store re-check loop; tests with a fake worker.
5. **Fork-and-stay in stage 1** for `--grants` only, with the channel descriptor and the allowlist change in `verify_stage2`; static-only starts keep today's exec path.
6. **Frontend integration:** `repo_outline` / `repo_ingest` use the broker for paths outside the static roots; error mapping; real-client test.
7. **Hardening/PoC:** Landlock-confine the broker (needs the `O_PATH` check), resource limits (memory, output size, time), measure worker start cost, doctor evidence for "dynamic workers supported".

## 9. Open questions (UNKNOWN)

* Does the broker work under its own Landlock domain (`O_PATH` opens, `exec` of the worker)? PoC needed.
* Cost of one worker per operation on a large repository (start, parsing without the derivation shortcut, transfer size). Measure after task 3.
* Memory behaviour of tree-sitter on hostile input inside a worker; limits (`RLIMIT_AS`, cgroup) not yet chosen.
* Whether to bind grants to a session/client (needs a trustworthy session identity; a client-supplied id must not be used).
* Behaviour on kernels without `openat2` (older than 5.6: below the project minimum) and on non-Linux platforms (no sandbox: dynamic workers stay off).

## 10. Verdict

* Architecture (fork-and-stay broker, per-operation Landlock'd workers, frontend as sole database writer): **GO** as a design; mechanics are
  VERIFIED in spike #1 and the building blocks exist in `main`.
* Implementation of task 1 and 2 (pure refactor): **GO**.
* Broker self-confinement, performance on large repositories, resource limits: **NEEDS MORE EVIDENCE**.
* Any claim that dynamic workers make self-approval harder: **NO-GO** (assurance stays A0).
