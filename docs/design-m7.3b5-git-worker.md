# M7.3b.5 design: Git ingestion through the broker (design only, nothing implemented)

Status: design. Basis: branch `feat/m7.3b4-mcp-broker-ingest` (the Directory path through the broker). Labels: **VERIFIED**
(observed here: code read, a test, or the experiment in section 3), **REASONED**, **UNKNOWN**. The Directory path is the reference
architecture; this note lists only what Git changes. The assurance limit is unchanged: a same-user shell agent that can run
`mind grant` is outside every guarantee, and broker authorization is the grant store's state, not human consent.

## 1. Invariants (the eight required) and how each is met

| # | Invariant | Mechanism |
|---|---|---|
| 1 | Git extraction only inside a confined worker | a Git mode of the existing worker; the broker, not the request, selects it (argv), so the worker cannot choose its sandbox |
| 2 | The frontend cannot expand root authority | unchanged: the frontend names a path, the broker matches it exactly against its own authorities and pins it |
| 3 | Git subprocesses cannot escape the worker | they inherit the worker's Landlock domain (cannot be left), seccomp, no-new-privs and cgroup; execute rights only on `git` and the ELF interpreter (section 3, VERIFIED) |
| 4 | Extraction and persistence are separate trust boundaries | the frontend re-verifies object identity from raw bytes (section 4.4) instead of trusting parsed fields; persistence happens only after the broker's `done` and the commit lease |
| 5 | Persistence atomic across its logical writes | one transaction for the git observation, the code observation and the relationship resolutions (section 4.6) |
| 6 | Directory ingestion unchanged | a new tool, a new `OpKind`, a new worker mode; the Directory tool, schema and protocol messages are not edited |
| 7 | Experimental and opt-in | its own hidden flag (not the Directory one), no default exposure, same experimental labelling |
| 8 | No silent fallback to in-process Git | every failure is an explicit error kind; the tool has no code path that calls the in-process extractor |

## 2. What exists today (VERIFIED by reading the code)

* **Extraction:** `ingest::git::GitReader` runs the system `git` through one function, `git_command`: `Command::new("git")` (a `PATH`
  lookup), all inherited `GIT_*` removed, config pinned (`core.fsmonitor=false`, `core.hooksPath=/dev/null`, `protocol.allow=never`,
  `GIT_NO_REPLACE_OBJECTS=1`, no system/global config), a deadline that kills the child. Plumbing only: `rev-parse`, `for-each-ref`,
  `rev-list --objects`, `cat-file --batch(-check)`, `ls-tree -z`, `merge-base --is-ancestor`, `worktree list`, `config`.
* **Pinning (MCP path):** `confined::open_git_reader` refuses a symlinked `.git`, handles a linked worktree's gitdir pointer and
  `commondir`, rejects links inside the git directory, checks alternates, and pins `git_dir`/`common_dir` as
  `/proc/<own pid>/fd/<n>` paths that git receives in `GIT_DIR`/`GIT_COMMON_DIR`.
* **Streaming:** `stream_confined_git`/`stream_git_with_reader` and `consume(.., Kind::Git, ..)` exist and are tested in-process
  (`tests/stream_gate.rs`, `record_stream.rs`). The worker only implements `Kind::Files`.
* **Context:** git derivation reuse is keyed by `(repository_id, blob_oid)`, the ref movement classification needs
  `git_refs_current(repository_id)`, and `stream_git_with_reader` resolves space NAMES through the context (a worker context cannot).
  The Directory manifest/known set is keyed by content hash only.
* **Persistence:** `persist_git` is three steps: `record_git_observation`, then `record_code_observation`, then
  `resolve_git_code_relationships` once per view commit. Each is its own transaction and recording revision. VERIFIED: the first is
  already split into a transaction helper (`record_git_observation_tx`); the third inlines its body in the method and reads the
  rows the second wrote (inside one transaction it sees its own uncommitted rows).
* **Identity marker:** `GitReader::repository_id` reads `<common_dir>/mindspace-repository-id` or CREATES it. The in-process
  `repo_ingest` therefore needs a read-write root. A read-only worker cannot create it.
* **Limits:** no per-kind count for refs, commits, trees, tags or blobs. Bounded only by 100000 frames, 512 MiB of stream, 512 MiB of
  content, a 32 MiB blob and the deadline (VERIFIED in `stream.rs`: the assembler pushes each record without a count check).
* **Parent-side checks that do NOT exist for Git:** object ids are not recomputed. The parsed commit record is lossy
  (`parse_commit` drops continuation headers such as `gpgsig`, trims the message, folds time zones into a timestamp), so the parent
  cannot re-derive a commit's id from what the stream carries. For files the content hash is recomputed; for Git nothing equivalent exists.

## 3. Experiment: git inside the worker's confinement (VERIFIED)

`docs/experiments/m7.3b5-git-confinement.rs` confines a process like the worker (non-dumpable, no-new-privs, Landlock with a
READ-ONLY repository descriptor, system paths read-only) and runs the eight plumbing commands against a repository with two
commits, an annotated tag and a linked worktree.

| Question | Result |
|---|---|
| Can git run against a read-only repository at all? | **Yes.** All eight commands succeed; `.git` writes are refused (`EACCES`) |
| Does the current pin style work for a git CHILD of a non-dumpable process? (`GIT_DIR=/proc/<parent pid>/fd/N`, cwd the same) | **No.** Every spawn fails with `EACCES`; with the process dumpable (control) it works. A child cannot open a non-dumpable parent's descriptor links. `GitReader::pinned` cannot be reused as is |
| Do descriptors the child INHERITS, named through the child's own `/proc/self/fd/N`, work? | **Yes**, all eight commands, non-dumpable, read-only |
| Smallest execute grant | **Execute on the `git` binary and on the ELF interpreter, nothing else.** No exec-path directory, no broad `/usr` execute. Without the interpreter grant the spawn fails |
| Needs the worker to be dumpable? | No (and it must not be) |

Method notes: three false starts were fixed along the way (descriptor numbers colliding, `chdir` running before `pre_exec`, a shell
that does not word-split); only results from the corrected program are listed. Not tested: very large repositories, packs with
deltas across alternates, a repository whose root is a linked worktree (the common directory is then outside the root; see 4.3).

## 4. Design

### 4.1 Protocol and authority (the Directory mechanics, reused)
* `OpKind::GitExtract` next to `DirectoryExtract`. `Begin{op, kind, root, spaces}` is otherwise unchanged and still closed: no
  scope, history flag, path selection, limit or grant field. **v1 scope is `GitScope::Head` only** (HEAD, live worktree HEADs, tags as
  releases). Branches, remotes, tag views and full history need their own count limits and are a later step.
* The broker maps `OpKind` to the worker argv (`extraction-worker --git`); the worker cannot select a mode. Authority, root pinning,
  replay refusal, revocation rechecks (100 ms), the commit lease, the cgroup and the watchdog are untouched.
* The root is the directory that contains `.git` (or a bare repository). The broker does not run git; it only matches the path.

### 4.2 Worker sandbox: `enter_git_worker`
Same as `enter_extraction_worker` (non-dumpable, no-new-privs, socket-deny seccomp, system paths read, one read-only root
descriptor, own `/proc/<pid>` read) plus **exactly two** more execute rights: the `git` binary and its ELF interpreter. The broker
(unconfined) resolves both and passes them as extra descriptors (the worker never searches `PATH`); the worker runs git by that
absolute path. The git reader is changed to the inherited-descriptor pin style (section 3): the worker opens `.git` (and a distinct
common dir if it is inside the root) beneath the root descriptor, keeps those descriptors, and each git child inherits only them
(plus stdio) and uses `GIT_DIR=/proc/self/fd/N`. The channel and control descriptors are close-on-exec for git children.
Every git child therefore: is in the worker's Landlock domain (cannot widen it), cannot write, cannot open sockets, cannot become
privileged, dies with the cgroup, and can execute nothing but git. The deadline kills it as before.

### 4.3 Identity (the marker) and what v1 refuses
* **v1 requires an existing `mindspace-repository-id` marker.** The worker reads it (read-only works); the **broker reads it too**
  through the pinned root and reports the expected id to the frontend, which refuses a stream whose header names another
  repository. A compromised worker therefore cannot write into another repository's rows. A repository without a marker fails with
  `identity_missing` and says to ingest it once with `mind ingest` (the human's in-process CLI, not a fallback of this tool).
* The marker is repository content: a hostile directory can ship a marker equal to a victim repository's id. This exists in-process
  too; it is a residual risk (decision D3).
* Refused in v1, fail closed with a clear error: a symlinked `.git` (already refused), a linked worktree or a `commondir` outside
  the authorized root, alternates outside the root (the worker's Landlock denies the read; the existing `check_alternates` message
  is reused), links inside the git directory (already refused), shallow/grafts are honoured as written.

### 4.4 Verified object transport (the Git analogue of the content hash)
The frontend must not trust parsed fields. Records for commits and tags carry their **raw object bytes** with the id; trees and
blobs are re-hashable from what is carried. The frontend recomputes `hash("<type> <len>\0" + bytes)` and compares it with the id
(SHA-1 or SHA-256 by id length), re-parses commits, trees and tags with the existing pure parsers, and rejects a mismatch. Then the
worker can only choose WHICH objects to report, never what an object contains (an object id binds its content). Non-text blobs are
sent as `(oid, size)` without content and cannot be re-hashed: residual, documented. Cost: a protocol revision for the Git records and
a SHA-1 implementation (no `sha1` dependency today; decision D2).

### 4.5 Context exchange and placeholders
* Request additionally carries space name/id pairs so the worker resolves names only from the parent's list (the Directory worker
  already refuses to resolve anything itself).
* The worker lists the distinct `(repository_id, blob_oid)` of the ingestable files at the views (manifest). The parent answers with
  the known derivations and the previous ref state `git_refs_current(repository_id)` (bounded). Placeholder provenance is the
  Directory rule keyed by `(repo, blob_oid, identity)`: the assembler currently refuses `reused` for Git and must accept it there.
* `check_result` additions: spaces, identities, repository id equals the broker-reported one, every reuse announced, object count
  limits.

### 4.6 Persistence: one transaction
`Store::record_git_ingest(observation, code, resolve: &[commit], before_commit)` runs `record_git_observation_tx`,
`record_code_observation_tx` and one `resolve_git_code_relationships_tx` per view commit (extracted from the method body) in ONE
transaction and one revision, then `before_commit` (the commit lease, unchanged), then COMMIT. The commit-state model
(COMMITTED / ROLLED_BACK / UNKNOWN) and the lease invariant of the Directory path apply unchanged. This is a storage change that
the in-process `persist_git` benefits from as well, and it can land first (decision sequence 5a). No schema change.

### 4.7 Limits (new, explicit)
Producer side (worker, early exit) and consumer side (assembler, hostile worker): `max_refs`, `max_commits`, `max_trees`,
`max_tags`, `max_files` (2000, as `repo_ingest` for unscoped trees) and the existing stream/content/blob/deadline bounds. Proposed
starting values for review (D4): refs 10000, commits 20000, trees 200000, tags 10000. A repository above them is refused with
`too_large` before most of the work is done. There is no per-operation memory limit (as for directories); the unit's limits apply.

### 4.8 Tool contract
A **separate tool** `repo_ingest_git_isolated` (path, spaces) behind its **own hidden flag** (`--isolated-git-ingest`, implying the
broker), so that enabling one tool never enables the other and the Directory tool's schema stays byte-identical. Result and failure
shapes mirror the Directory tool (`commit_state`, `persisted`, error kinds) plus `identity_missing`, `git_unavailable`,
`not_a_git_repository`, `unsupported_repository` (linked worktree, outside alternates), `too_large`, `object_verification_failed`.

## 5. Threat model additions
* **Hostile `.git`** (config, hooks, `core.fsmonitor`, pager, alternates, replace refs, symlinks, odd packs): config is pinned and
  the known execution vectors disabled; whatever remains runs inside Landlock (read-only, exec only git), seccomp (no sockets) and
  no-new-privs, so a git exploit gains only the worker's authority, and the worker holds nothing but one read-only repository.
* **A compromised worker** can lie by choosing which objects to report; with 4.4 it cannot fabricate content or history, and with 4.3
  it cannot write into another repository. It can still read everything in its authorized repository.
* **Resource exhaustion:** deep history, huge packs, `rev-list` over millions of objects: producer/consumer count limits, the
  wall clock, the stream ceilings; descendants die with the cgroup.
* **Not addressed:** marker poisoning (D3), SHA-1 collisions (as in git itself), non-text blob contents.

## 6. Test plan (sketch)
Parity: the same repository ingested in-process and through the worker gives identical tables (the Directory differential method).
Hostile fixtures: hooks and fsmonitor commands that would write outside (must not run), config injection, alternates outside the
root, symlinked objects, replace refs, a gitlink submodule, a shallow clone, a pack with many tiny objects, a marker pre-seeded with
another repository's id, a commit whose raw bytes do not hash to its id (verification), an oversized history (limits). Persistence:
fault injection between the three writes (database contents compared), commit-lease and revocation boundaries re-run for Git,
crash/timeout/cancel as for directories, no-fallback assertion (git missing, not a repository, no marker).

## 7. Open technical decisions
* **D1 Marker creation.** v1 requires a pre-existing marker. Alternative: a broker-side `GitIdentity` step that writes the marker
  through the pinned root with `O_EXCL`/`O_NOFOLLOW`, only for read-write authority, with the UUID chosen by the broker. It gives
  the broker its first write into repository content. Recommendation: not in v1.
* **D2 Verified transport.** Recommended (4.4). Alternatives: accept worker-asserted structure (as the Directory path accepts a
  fresh parse), or verify trees and blobs only. Needs a SHA-1 crate (collision-detecting is better but heavier).
* **D3 Marker trust.** Whether to additionally bind the repository id to the authorized path in the database (a repository that
  moves would then need re-binding). Recommendation: document, decide later.
* **D4 Limit values** in 4.7, and whether `max_files` stays 2000 for Git.
* **D5 Linked worktrees and external alternates.** Refused in v1. Supporting them needs a second authorized root (the common dir)
  and an authority model for "the repository is two directories".
* **D6 Rust `sha1` vs shelling `git hash-object`** for verification: the second needs another git child in the FRONTEND and is
  rejected.
* **D7 Scope growth:** the order in which branches/remotes/tags/history are admitted, each with its own limits.

## 8. Recommended implementation sequence (each step independently committable and testable)
1. **5a, Git persistence atomicity** (storage only, in-process path): `record_git_ingest`, `resolve_..._tx`, `persist_git_gated`;
   fault injection between the three writes; parity with today's reports. No worker, no protocol change.
2. **5b, confined git reader**: the inherited-descriptor pin style and an absolute git path in `GitReader`; tests in a Landlocked
   non-dumpable child (the experiment turned into tests, plus hostile-config fixtures). Still no broker.
3. **5c, verified transport and limits**: raw bytes for commits/tags, the assembler's hash verification, per-kind caps, `check_result`
   for Git and `reused` placeholders keyed by repository/blob; in-process producer/consumer tests including hostile streams.
4. **5d, Git worker mode**: `enter_git_worker`, broker-provided git/interpreter descriptors, `OpKind::GitExtract`, worker Git run
   with the Git context exchange and identity reporting; driven by the existing real-MCP-style harness, no MCP tool yet.
5. **5e, MCP tool**: the separate tool and flag, the real-MCP scenarios (static and dynamic roots, unauthorized, revocation at every
   boundary, crash, malformed stream, lease/commit faults, reuse) and documentation.
Exit criterion for each step: fmt, clippy, the full suite, the release and systemd runs, repeated runs of the concurrency-sensitive
suites, and a mutation check for each new guarantee.

## 9. Out of scope
New grant mechanisms, schema migrations, history/branch/remote scopes in v1, marker creation by the worker, any in-process fallback,
changes to the Directory tool.
