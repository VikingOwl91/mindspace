# M8 S3b: trusted local legacy enrollment contract

Status: **Model A selected; ACCEPT for bounded implementation planning**, subject to the
explicit trust boundary and implementation gates below. No enrollment is implemented.
Baseline: signed S3 `a60e7c118d90097e56972bc4ec1957a8e7b8d245`.
Companion: [S3a](m8-s3a-retrieval-ownership.md). P1a remains a deferred alternative.

## 1. Decision and threat-model comparison

Select **Model A: a direct trusted local administrative operation** for F-only legacy
enrollment. “Administrative” means deliberately exercising the database-owning OS account's
existing authority through a dedicated command, not root, a setuid helper or independent
human authentication. No remote/delegated enrollment, external grant or approval service.
The task's requested A/B evaluation makes this a deliberate change to the previous
principal-bound contract, not evidence that same-UID execution authenticates a human.

| Question | Model A: selected local administration | Model B: deferred principal-bound authority |
| --- | --- | --- |
| Who may initiate? | Trusted local process of the database-owning OS account, explicitly invoking the admin operation. Kernel UID is account attribution, not a distinct person. | Independently verified principal/delegate, admitted by a protected issuer/channel. |
| Untrusted inputs | MCP requests, source text, metadata, paths in documents, request logs and imported plans cannot authorize enrollment. | Same, plus untrusted local clients and potentially hostile same-UID processes. |
| Trusted components | Kernel/local filesystem/SQLite, executable, admin process, application handlers and database-owning account. | Those plus isolated issuer, principal authentication, protected decisions and controlled committer/fencing service. |
| Same-UID malicious shell / compromised frontend | Outside A. Such a process can invoke the CLI, impersonate an account-level action or rewrite its DB. A terminal prompt cannot stop it. | Requires a genuinely separate protection boundary; a same-UID daemon or hash chain alone is insufficient. |
| Database binding | Explicit existing physical DB target, canonical path/device/inode, source UUID and expected state/R. Stable local target required. | Protected instance registration, clone/rebind policy and authenticated request binding. |
| Authorization | Explicit direct administrative invocation and confirmation of the canonical intent, with private nontransferable in-process context. | Protected durable approval of the full tuple, validity/revocation and delegation policy. |
| Recovery fence | SQLite writer serialization, receipt-first replay checks, exact expected state and immutable one-way state; logically fences stale effects. | Also durable authority decisions/session generations and revocation/committer-death coordination where permissions outlive a local call. |
| Approval expiry/revoke | No external approval exists. Local cancellation is best effort before COMMIT; no asynchronous revoke/TTL guarantee. | Fresh decision validation under exclusion, ordered revoke/expiry and recovery of authority state. |
| Required now? | Sufficient for this expressly local, enroll-only feature under A's stated assumptions. | Required if independently attributable humans, hostile same-UID clients, remote/multi-user or delegated approvals enter scope. Not a prerequisite for A. |

A is unsuitable wherever the database-owning account or administrative process is untrusted.
Mindspace cannot infer the agent's confinement tier; installations with untrusted shell
access under that account must treat A as offering **no separation from that agent**.
No A1/A2/A3 assurance level is claimed, and existing grant/doctor assurances are unchanged.
Future document publication must select its own threat model; this decision does not
approve using A for cross-kind ownership or expanding filesystem access.

### Explicit changes from the preceding stronger contract

- Replace `VerifiedPrincipalContext`/protected issuer with a trusted local admin context;
  receipts record OS-account attribution, never a claimed human principal supplied by a client.
- Replace externally durable precommit approvals with a self-binding full operation identity.
  Successful receipts are durable; an abandoned preview/confirmation is not a durable approval.
- Replace external lease/session-death fencing with mandatory transactional **logical** fencing
  for the irreversible enroll-only transition. Absence never proves a prior process is dead.
- Remove external grant renewal, expiry and asynchronous revocation promises from A.
  Preserve these requirements under B; do not present local cancellation as equivalent.
- Bind the physical local target without claiming portable authority across clones/restores.

These are disclosed differences in guarantees and threat scope. The preceding PARTIAL
finding about missing Model-B primitives remains true for B; it does not block A.

## 2. Evidence from the actual S3 code

**Observed source, not tests of future enrollment:**

| Source | Consequence for the contract |
| --- | --- |
| [CLI parse](../src/cli/mod.rs#L1556), [paths](../src/paths.rs#L183) | Explicit `--db` origin is distinguishable from environment/default resolution. A must require the command-line origin, not silently inherit `MINDSPACE_DB`. |
| [grant CLI](../src/cli/grant.rs#L1), confirmation at 101 | CLI runs as the local user and explicitly cannot distinguish same-UID programs. Reuse display/confirmation mechanics, not human-authentication claims. |
| [MCP protocol](../src/mcp/protocol.rs#L251), [tool dispatch](../src/mcp/tools.rs#L475) | Closed tool vocabulary and unknown-tool refusal are existing boundaries. No enrollment route, forwarding flag or generic admin dispatcher is to be added. `clientInfo` is untrusted descriptive text. |
| [open_existing](../src/lib.rs#L49), [SqliteStore::open](../src/storage/sqlite.rs#L343) | Existence check precedes a create-capable open. Admin opening must be existing-only, validated and bound to the confirmed target; do not blindly reuse this path. |
| [begin_write](../src/storage/sqlite.rs#L701) | `BEGIN IMMEDIATE` serializes writers; recording authorization and R are connection-local. Nested writes are refused. Receipt lookup under this writer must precede expected-R rejection. |
| [commit classification](../src/storage/sqlite.rs#L133) | Busy/Locked/Constraint COMMIT failures are definitive noncommit; other COMMIT failures can be Unknown. Poisoned connections are rejected; disarm/discard uncertain admin handles. |
| [migration runner](../src/storage/sqlite.rs#L897) | Runner already owns raw writer transactions with inactive revision authorization; FK enforcement is temporarily disabled. New metadata coverage needs a private in-transaction R_meta scope and explicit FK validation. |
| [grants](../src/grants.rs#L141), [shared hold](../src/grants.rs#L617) | Grants bind filesystem paths/dev/inode/access/expiry, not ownership operations. Their locks order grant revocation but are not needed to authorize direct A administration. |
| [broker](../src/worker/broker.rs#L320), [commit lease](../src/worker/broker.rs#L1586), [client](../src/worker/client.rs#L1094) | Peer PID controls process lifetime, not human identity. A does not delegate commits to this broker. Its in-memory replay cache is not a receipt ledger. |
| [approval diagnostics](../src/doctor/approval.rs#L13), [ADR-030](adr-030-jit-authorization.md) | Default verified A0; independently trusted human approvals are unproven. Model A does not raise this level or claim to solve ADR-030's T2 same-UID problem. |

Model B would additionally need an actually protected issuer/decision ledger; verified
principal/delegation channel; protected DB registration/rebinding; immutable decision-to-
operation binding even before SQL commitment; ordered revoke/expiry exclusion; durable
attempt generations and controlled committer/broker-death recovery. Existing code provides
some transaction/lease patterns, not these semantic authority primitives. Implementing a
generic principal system is unjustified for A. If B becomes required, the smallest first
prerequisite is a separately reviewed, disabled-by-default exact-operation issuer/channel
for one DB/action, with protected decision/revocation storage and assurance tests; no
permissive mock or same-UID prompt may be its production implementation.

## 3. Minimal local administrative interface and target binding

Proposed direct CLI only, with a read/preview mode and explicit apply mode, for example:

```text
mind --db /absolute/existing/mind.db ownership enroll-legacy \
  --source-id F --expected-revision R --nonce UUID             # preview, no enrollment
mind --db /absolute/existing/mind.db ownership enroll-legacy \
  --source-id F --expected-revision R --operation-id FULL_ID \
  --apply --confirm-intent SHA256                             # exact explicit action
```

Names are an implementation contract, not an existing command. There is no bulk adopt,
background queue, MCP endpoint/opcode, public `Store` enrollment method or implicit call
from legacy/document ingestion, indexing, metadata mutation, setup or DB open.

1. Require an explicit absolute command-line `--db`, full source UUID, operation/action
   version and expected R. Refuse environment/default DB selection, SQLite URI/memory
   targets, partial source-ID resolution and source-supplied DB locators. Do not read the
   source file or infer ownership from its path.
2. Run without elevated/setuid identity; effective/real UID must agree and own the DB.
   Read actor UID from the OS, never `--principal`, `clientInfo`, request-file fields or
   display names. Record `local-os-account@1` attribution; no human/user-name authentication.
3. Use an existing-only read-write open without CREATE or URI interpretation. Canonicalize
   the explicit target and bind `(canonical path, st_dev, st_ino)` from an existing regular
   single-link file; reject final symlinks, hardlink aliases, unsafe permissions and untrusted
   writable parent paths. Validate stable target before/after open and before COMMIT on the
   same connection. DB and WAL/SHM directory must be controlled by the trusted account; refuse
   unsupported/nonlocal locking environments. This is under a stable-directory trust assumption,
   not a proof against arbitrary hostile concurrent namespace replacement.
4. Show exact DB identity, OS actor kind/UID, F, file-kind/source/base fingerprint, expected
   Unmanaged/epoch 0, R, and intended `LegacyEnrolled(legacy,1)`. Display-escape controls/bidi
   from source labels and keep full IDs/digest unambiguous. Labels are untrusted metadata.
5. Explicit apply must confirm the exact recomputed canonical intent: interactive confirmation
   after preview, or the noninteractive `--apply --confirm-intent <full digest>` above.
   Noninteractive mode deliberately trusts the invoking process equally; no generic `--yes`
   or inherited environment value substitutes for the full-intent confirmation. Imported
   plan bytes are input only and must match independently read DB/OS facts.
6. Only this direct handler may construct a private, nonclone, nonserializable local-admin
   context bound to that connection/intent/actor. A private synchronous storage composition
   consumes it for one attempt. No fork, child/broker committer, deferred job, callback
   retaining the context, environment switch or metadata row can construct/retain that scope.
   Ordinary M6 write authorization does not arm it. Clear it on error/panic/rollback/discard.

The command is “privileged” within application routing, not an OS privilege escalation.
An MCP client limited to MCP cannot invoke it because no tool/argument maps to that route.
A client with arbitrary same-account shell execution can invoke it: that is the explicitly
excluded stronger threat, not a hidden human-consent guarantee.

Rename, clone, backup restore, DB replacement or inode-changing maintenance during an
unresolved attempt is unsupported. Detect changed target identity and refuse execution;
resolve against the original stable DB or an explicitly reviewed recovery first. A copied
receipt remains historical evidence, not a transferable approval. Do not silently rewrite
its target or rebind an old operation. New physical target/UID requires a new confirmed
operation; portable instance identity/restore reconciliation belongs to a later design.

## 4. Immutable intent and minimal schema

The sole supported transition is `Unmanaged(F) → LegacyEnrolled(F, legacy, epoch=1)`.
There is no D, location reservation, active-state flag, document owner, selected manifest,
publication, deactivation/unenroll or mutable ownership epoch.

Use one strict, versioned canonical encoding with fixed field order, length-prefixed UTF-8
strings, canonical UUID bytes and fixed-width unsigned integers. Reject invalid/extra
fields; no lossy Unicode normalization or ambiguous JSON hashing. Canonical request contains:

- Domain/version `mindspace.s3b.local-enroll@1`, cryptographically random UUID nonce.
- Physical DB identity above; actor kind and kernel-derived real/effective UID.
- Exact F, source kind `file`, deterministic source/base fingerprint and expected recording R.
- Action `enroll_legacy@1`; expected Unmanaged/epoch 0; intended legacy owner/epoch 1.

For this version the base fingerprint is SHA-256 over the same canonical encoding of
F's `id`, `kind`, `state`, nullable `current_location` and `created_at`; require Git-only
identity columns to be NULL. Bind the global expected R as well, so any recorded source/
lifecycle/knowledge change invalidates a new attempt conservatively. Do not choose a
“latest source” by timestamp or read live source bytes. Present/absent F can be enrolled;
this records control metadata only, without asserting that the physical file exists.

Use big-endian u64 for lengths/UID/dev/ino/R/epoch, UUIDs as 16 bytes, ASCII fixed enum tags
and an explicit 0/1 discriminant for nullable fields. Encode `created_at` as fixed-width
big-endian two's-complement i64, preserving valid negative timestamps. Reject overflow, negative revisions,
invalid UUIDs/UTF-8, unsupported versions and canonical requests over 64 KiB; never truncate.
PID/start time/session IDs may be audit-only, not bound intent, so restart retry is identical.

Full operation ID is `local-enroll@1:<nonce>:<sha256(canonical_request)>`. Store recomputes
and checks it, not just a caller digest. **A bare UUID is not an operation identity.**
Changing any bound field with the same full ID fails even if no SQL commit occurred.
Changing the full ID is a new operation requiring a fresh explicit admin invocation and
confirmation. A nonce alone has no durable uniqueness/approval semantics before commitment.
No external pending-decision ledger is needed: the ID binds bytes, not authority. It is
neither a signature nor evidence that a human approved an abandoned attempt.

Candidate additive migration after schema 12, numbered after branch recheck; no SQL here:

| Record | Enforceable minimum |
| --- | --- |
| `asset_ownership` | Asset UUID PK; UNIQUE NOT NULL F FK with kind=file trigger; owner CHECK legacy; epoch CHECK 1; immutable enrollment-location snapshot; created/updated R equal; reference to its successful operation. No D/head/publication columns or mutable owner fields. |
| `asset_control_intents` | Append-only full-operation-ID PK and UNIQUE nonce for committed operations; canonical request/version/digest, physical DB binding, actor kind/UID, action, expected state/base/R, intended transition, result asset/epoch and committed R. No pending/approved state, bearer secrets or caller-asserted human principal. |
| Typed history/availability | Append-only enrollment birth/snapshot under that same R and new coverage from R_meta. No recorded-R query API or historical ranking promise. |

Successful operations with a reused nonce and changed digest conflict as well as full-ID
mismatches. Before any successful receipt, a changed digest is a different full operation,
not a replay; it still needs fresh exact authorization. This explicitly replaces the prior
stronger promise of a durable approval ledger binding a standalone UUID before commitment.

Require immutable UPDATE/DELETE rejection, CHECK/FK/unique, same-R/result consistency and
persistent enforcement inventory. Deferred FKs may join receipt and owner circularly;
precommit validation must prove they match. Inserts require both M6 R and a private
transaction-local admin/intent/source context. Ordinary `set_meta`, source lifecycle writes,
labels or a generic `begin_write` cannot insert/change receipts or ownership. Test raw
attempts even with M6 authorization. Full DB rewriting by its trusted owner is outside A;
SQLite triggers are application enforcement, not tamper-proof approval storage.

Normal supported-binary migration may install **empty** tables, never backfill enrollment.
Inside the runner's existing writer use a private RAII R_meta scope, DDL/coverage/fences/
schema registration in one transaction. Do not nest `begin_write`, bypass global fences or
rerun M6 baseline. Explicit FK/enforcement validation is required before COMMIT; restore
settings on every exit, classify unknown outcomes conservatively, discard uncertain handles.
Repeated migration/open consumes no extra R_meta. Older binaries reject the newer schema;
rollback after committed upgrade is not destructive DOWN SQL or rewritten history.

## 5. Transaction, logical fencing and recovery

A has no external grant, issuer or distributed transaction. Authorization is the explicit
local call for the bound action; only durable SQL COMMIT makes **committed intent**.
Preview, confirmation, a UUID/hash and allocating R are not committed outcomes and confer
no ongoing rights. Later actions require their own explicitly authorized operation.

1. Parse/validate the full-ID format, explicit target and local OS context, then look up
   that full ID under normal read authority **before rebuilding any current source/base
   fingerprint**. If found, validate its stored canonical request/digest/nonce and action/
   transition against the full ID, caller-supplied F/expected R, and current DB/OS identity.
   Return the original actor/target/R, never a new approval or another caller's action.
   Changed current source metadata/R does not invalidate a matching committed receipt.
   Different actor/target/bound input or a corrupt receipt conflicts/fails closed.
   Status reads need neither live source bytes nor fresh confirmation for mutation.
   If absent, carry the full ID and supplied bindings into the writer. Do not finally
   reject a retry from changed current source/base/R before the serialized receipt recheck:
   a concurrent original attempt may commit after this early read. Preview metadata is
   informational; it cannot retarget the confirmed ID or skip the inside-writer checks.
2. For a new attempt, require explicit local apply/confirmation of the supplied full ID,
   digest and DB/F/R/action/transition; this authorizes an attempt, not unchecked contents.
   Obtain `BEGIN IMMEDIATE`
   through the normal store write boundary and **recheck receipt first**, before expected-R
   rejection or source/base reconstruction. Validate a matching race winner's stored request
   as in step 1 and return its result; roll back any unused R. For a new write reconstruct/
   check the full bound request inside this transaction: use its nonce, independently read
   source/base and supplied expected R, recompute the whole canonical digest and require
   exact full-ID equality. Do not excuse changed intended bytes. An early preview is not a fence.
3. If absent, validate nonce conflict, F kind/identity, exact expected Unmanaged/epoch 0,
   source/base and current R (compare against the pre-allocation R). Stale intent is refused,
   never silently retargeted/reconfirmed. Recheck target/context before final commit.
4. Insert ownership + immutable receipt + typed history/availability and R atomically under
   the private context. No sessions, sources, evidence/assertions/memories, spaces/memberships,
   FTS, vectors, document derivations or Git changes. Commit using existing outcome handling.
5. On success return original receipt/R. On known precommit/refusal/definitive noncommit,
   rollback all staged rows/R and return NoCommit **for this attempt**. On uncertain COMMIT,
   disconnect, crash or output failure after COMMIT admission, report Unknown unless the
   durable result is known; permanently disarm/discard uncertain handles. Never return an
   ordinary “rolled back” error after a real commit. Output/audit failure cannot undo it.

### Why this fences uncertain earlier attempts

The logical fence is **inside every writer**, not a preliminary status check. Receipt-first
lookup + exact expected state/base + unique full ID/nonce/F and immutable epoch-1 enrollment
permit at most one committed enrollment effect for F. No rollback-to-Unmanaged/ABA transition
exists. A resumed old process cannot bypass these checks. This is not exactly-once execution:
multiple attempts can prepare, wait, execute SQL and roll back.

| Prior attempt / recovery observation | Required classification/action |
| --- | --- |
| Matching durable receipt | Committed. Return the original result/R without mutation; immutable provenance remains. |
| Ordinary read sees no receipt | NotObserved at that snapshot; earlier attempt can still commit. Never call this rollback or safe cancellation. |
| Old COMMIT/writer still holds SQLite lock | Retry/status barrier may block or time out; keep Unknown, no parallel unmanaged write path. |
| Old process paused before BEGIN | A fresh explicitly authorized exact retry may win BEGIN and commit; old process later sees receipt and does no new work. If old wins, retry sees its receipt. No physical termination claim. |
| Healthy serialized writer sees absence | NotCommittedAtBarrier, not proof that the old invocation will never run. The same exact retry can proceed safely only within this same writer transaction and all rechecks; do not drop the lock then do blind writes. |
| Old process crashed during SQL | SQLite lock/recovery serializes the next writer. Receipt either exists or not; uncertain/corrupt/unavailable DB stays Unknown. No process-local PID cache decides commitment. |
| Different intent for already enrolled F | Conflict/AlreadyEnrolled read status, not a successful receipt for the loser. No revision for refusal/replay. |
| Wrong/replaced physical DB or changed base/R | Refuse mutation; unresolved earlier outcome remains Unknown until original target is inspected. New intent is a separate admin action. |

The earlier uncertain attempt remains Unknown until a receipt or definitive evidence resolves
it. A later exact retry can establish the desired committed effect without claiming which
attempt executed it or that the earlier one rolled back. This **logical** fence meets the
enroll-only invariants; no external session-generation service or per-DB administrative
lock file is required. Such a lock alone would not authenticate a human or fix DB substitution.
If later transitions can undo/reassign ownership, reopen this proof with epochs/CAS and
appropriate authority; do not extrapolate an enroll-only proof to publication/reversion.

Cancellation/revocation distinction: before invoking or before COMMIT, trusted local caller
can cancel and the handler rolls back when cancellation is observed. After COMMIT admission,
termination/lost response may yield Unknown; never promise cancellation won. There is no
separate approval object to expire/revoke and no promise to invalidate another trusted OS
process asynchronously. Restricting account access is OS administration, not a receipt update.
A committed enrollment survives later local cancellation; undo is unsupported in S3b.

## 6. Executable acceptance-test contract

These are **proposed tests**, not implemented/run ownership tests. Use disposable real
SQLite DBs, production CLI parsing and private test-only barriers; no production bypass.
Suggested suites: `tests/ownership_admin.rs`, `tests/ownership_core.rs`,
`tests/ownership_recovery.rs`. Run them locked in debug/release and repeated concurrency
runs once implemented; mutation tests must catch removal of each binding/fence/constraint.

| Proposed test | Executable setup and assertion |
| --- | --- |
| `local_admin_exact_enrollment` | Direct real CLI, explicit DB/F/R/full ID and exact confirmation; one legacy epoch-1 row, immutable receipt/history and one R. No human-authentication assertion. |
| `no_implicit_or_mcp_enrollment` | Default/isolated tools/list unchanged; tools/call invented enrollment name, existing tools with admin/actor fields and direct dispatch all refuse. Ordinary open, metadata and S3 ingest create no enrollment. |
| `explicit_existing_database_required` | Environment/default-only DB, relative/URI/memory/missing target, symlink/hardlink and unsafe parent refuse; missing target is not created. Wrong target after preview cannot silently retarget. |
| `actor_and_confirmation_are_not_client_fields` | Caller principal/UID/name, inherited confirmation env, bare --yes or wrong digest cannot create context. OS actor is recorded. Same-UID trusted script can deliberately apply; test never claims to distinguish it from a human. |
| `wrong_source_state_action_or_transition_refuses` | Document/Git/manual kind, F from another DB, stale R/base, changed expected state or publication action fails without R/control/knowledge effects. |
| `full_id_binds_intent_before_and_after_commit` | Alter each field with original full ID after preview, rollback, process restart and success; always conflict. Bare UUID invalid. Fresh changed full ID is a new operation requiring confirmation, not replay. Committed nonce reuse conflicts. |
| `exact_retry_is_noop` | Exact successful retry after restart/source deletion/later legacy revision returns original receipt before stale-R checks, no extra R/history/owner. Unknown original followed by exact retry has one effect. |
| `concurrent_attempts_are_logically_fenced` | Six independent connections/processes, same full intent and different confirmed intents for F. One effect; same intent gets receipt, other intents conflict; no extra committed R. Repeat with threads on one store. |
| `paused_prebegin_attempt_cannot_undo_retry` | Pause old process before BEGIN, exact retry commits, resume old; original receipt and one effect. Reverse winner order. Changed intent/base cannot bypass fresh checks. No process-death assumption. |
| `inflight_writer_blocks_recovery` | Hold old transaction/COMMIT at barrier; retry times out as Unknown/blocked, never NoCommit for old attempt. Release old writer, recover original outcome or safely retry. |
| `crash_and_lost_ack_are_honest` | Kill before BEGIN, after BEGIN, after inserts, during COMMIT, after durable commit/before output. Pair actual committed-lost-ACK hook with vetoed COMMIT. Reopen and retry; receipt/all-or-none state, never reported rollback for committed case. |
| `local_cancel_is_not_external_revocation` | Observed cancellation before COMMIT rolls back; race after admission accepts Unknown/Committed with durable reconciliation. No fake revoke/TTL/issuer method or claim exists. |
| `metadata_does_not_create_admin_context` | Source label/path/mind_meta edits, ordinary M6 context and raw control INSERT/UPDATE/DELETE fail guards or leave authority unchanged. Forged receipt/digest and missing enforcement are refused. |
| `excluded_states_are_storage_rejected` | With valid M6+admin scope still reject owner=document, epoch>1, D/head/publication state, kind mismatch, inconsistent receipt/R/result and receipt edits/deletion. No enabling CLI/API/broker opcode exists. |
| `populated_upgrade_and_failure_are_atomic` | Populated schema 11→real 12→metadata; old IDs/memberships/FTS/vectors/baseline unchanged. One R_meta/no backfill; late DDL/FK/fence failure and veto/lost ACK; reopen twice, no partial or duplicate migration. |
| `legacy_lifecycle_search_tools_unchanged` | Before/after upgrade/enrollment compare lexical/semantic/hybrid result IDs/ranks, .at(T), source modification/move/absence/reversion, default MCP JSON and Git suites. No new memories or vectors. |

Adversarial cases also include displayed bidi/control injection, malicious plan files,
DB replacement/clone while unresolved (refuse, no silent rebind), raw writes through a
non-admin connection, failed context cleanup and returning a receipt for altered actor.
Test with a sentinel ensuring admin never calls embedding/indexing or file ingestion.
No claim of resistance to malicious same-UID shell/DB rewriting; demonstrate/document that
limit rather than testing a prompt as if it prevented that attacker.

Deferred tests: positive document publication/transfer/deactivation/reversion, active-set
duplicate suppression, strict embedding dispatch, citation enrichment and recorded-R
reconstruction. First slice tests rejection/absence only. Model-B issuer/principal,
revocation/expiry, delegated sessions and broker-death tests require B implementation.

## 7. Implementation boundary and design-gate disposition

Smallest bounded implementation: existing-only admin opener/CLI confirmation and private
context; canonical full intent ID; empty legacy-only metadata/immutable receipts/history
and R_meta migration scope; one atomic enrollment method with receipt-first logical fence;
known/Unknown/replay outcomes and the acceptance matrix. This is independently testable
without an authentication daemon, principal registry, external decision store or broker.
No runtime implementation is authorized by this documentation task.

Assumptions: trusted local account/process/application code, OS credentials, kernel/local
filesystem/SQLite locking and DB integrity; stable protected DB directory during attempts;
no malicious same-UID administrator, arbitrary SQL writer or privileged/root attacker.
No human-identity, external revocation, portable cross-copy authority or eventual completion
promise. Busy/IO uncertainty may remain Unknown indefinitely; do not guess to make progress.

**ACCEPT for the Model-A feasibility/design gate**, with the explicit limitations above.
The new opener/context/schema/receipt path and crash tests remain required implementation,
not features already verified. Model B remains unavailable with existing mechanisms and
is separately gated if the threat scope expands. S3a's future publication requirements
and P1a alternative remain deferred. No retrieval projection, merge, push or S4.
