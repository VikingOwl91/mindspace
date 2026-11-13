# M8 S3e.1: legacy writer fencing and old-reader compatibility

Status: **design/feasibility gate only**. All new tables, APIs, guards, layouts and
acceptance cases below are proposed. No fence is activated, no publication or
projection exists, and ordinary retrieval is unchanged. The reviewed design is
authorized for a separate signed local documentation commit; implementation is
not authorized by this finalization.

Baseline verified: signed `254d1ad5794db8ac19dc187a5934910337da9b9e`, branch
`feat/m8-s3d2-semantic-staging`, worktree `/tmp/mindspace-m8-s3d2`. Initially only
[S3e](m8-s3e-publication-design.md) was untracked. Its independent review and local
signed finalization are a separate step from this new design. Migration history
ends at 0016; no schema is installed by this task.

Read with S3e and [S3a](m8-s3a-retrieval-ownership.md),
[S3b authority](m8-s3b-ownership-core-plan.md) /
[implementation](m8-s3b-ownership-core.md),
[S3c design](m8-s3c-document-ownership-plan.md) /
[implementation](m8-s3c-document-ownership.md),
[S3d](m8-s3d-staged-index-design.md),
[lexical staging](m8-s3d-lexical-staging.md),
[semantic staging](m8-s3d2-semantic-staging.md) and
[S3 chunking](m8-s3-chunking.md).

## 1. Decision, scope and evidence vocabulary

Select a **passive planning prerequisite**, followed later by permanent,
asset-scoped storage fencing installed **only with complete atomic publication**.
No independently activatable legacy freeze exists. Preparation, staging and a
successful passive-plan receipt cannot deny a legacy write, reserve a locator,
select an owner or change retrieval. Continued ingestion can invalidate the
expected state; it must not be prevented merely to keep a plan fresh.

Old-reader compatibility is a separate database/client protocol boundary. Select
the S3e candidate physical active-memory relation rename without an old-name
alias, conditional on real old-binary and populated migration proof. Version
checks alone are insufficient. The passive slice cannot install that rename or
ordinary-table guard triggers. Publication remains **BLOCKED** until the actual
old-reader boundary and joint publication/writer invariants pass.

Use these labels throughout:

- **Observed**: traced in the baseline source or executed existing tests.
- **Experimental**: a bounded disposable probe with its engine/process limits.
- **Proposed**: an unimplemented contract/test, never a passing publication claim.

The authority is accepted **Model A**: the database-owning local OS account
explicitly invokes a privileged administrative operation against an existing,
initialized database. Trust that account, its processes, kernel and SQLite.
There is no cryptographic human-principal authentication, malicious same-UID
isolation, new MCP authority, generic IAM, external revocation lease or automatic
filesystem authority from a path. An M6 write context is not a publication permit.

A = explicitly enrolled logical asset; F/Fs = legacy logical source/exact version;
D/Ds/X = document/revision/extraction; P = immutable S3c association; C = immutable
chunk set; L/S = exact lexical/semantic artifacts. A is a declared identity
closure, not a universal set of equal-content or physically aliased files.

### Required invariants

1. Before an authorized publication commits, ordinary legacy writes and retrieval
   remain permitted, including the same F after P/L/S or a passive plan exists.
2. In a supported query snapshot, one managed asset has at most one eligible
   active representation cohort. Complete publication replaces a nonempty eligible
   cohort with a complete nonempty one in its required modes/spaces.
3. Effective permanent fencing, legacy retirement, visible complete replacement,
   effective head, indexes and successful receipt commit together. Failed first
   publication leaves legacy **writable and visible**. A prior passive receipt
   grants no authority. No committed fence-only or empty-replacement state exists.
4. After publication, affected mutations cannot enter through another producer,
   new same-locator F, stale worker, membership update or late vector save.
5. Unrelated supported manual/file/Git/code operations remain writable. A mixed
   atomic observation intersecting a protected asset may fail as one unit; this
   is distinct from disabling unrelated operations across the database.
6. Historical evidence and receipts remain immutable. A reversion requires new
   complete episodes, never reopening an old interval or ordinary unfencing.
7. Old clients must finish an entirely old pinned snapshot or fail explicitly;
   they cannot successfully combine old and new active representations. Supported
   clients spanning the layout cutover must remain usable for unrelated work.
8. UNKNOWN is not rollback. Receipt absence, a lock timeout or a refused retry
   proves neither process death nor the fate of an older transaction.

## 2. Actual writer-path inventory

Observed references below are baseline line pointers and named boundaries, not
new implementations. All ordinary mutation entry points converge on
`begin_write` in [sqlite.rs](../src/storage/sqlite.rs) around line 739, using
`TransactionBehavior::Immediate`. The public trait is
[Store](../src/storage/mod.rs). No public generic `Store::record` exists.

| Entry / producer | Actual path and mutation | Future scoped boundary |
| --- | --- | --- |
| CLI file/directory ingest | [CLI](../src/cli/mod.rs), `Ingest` around 509 → `ingest_paths` → [extract](../src/ingest/extract.rs) `persist_files_gated` around 136/149 → `record_files_ingest` around 2350 | Before session/scope/source/absence/reconciliation effects, classify complete mutation domain; check again against planned new identities. |
| MCP normal repository ingest | [repo_tools](../src/mcp/repo_tools.rs) around 592 → same file/Git Store receivers | Client grant permits extraction, not adoption or publication; storage fence cannot depend on dispatcher choice. |
| Isolated directory/Git worker | [isolated](../src/mcp/isolated.rs) around 366, [isolated_git](../src/mcp/isolated_git.rs) around 296, [worker client](../src/worker/client.rs) around 1100 | Parent validates worker result and holds commit lease; same receiver must enforce ownership. Extraction time is not fence validation time. |
| Public file observations | `record_observation` around 2169 and `record_files_ingest` around 2350 → `record_observation_tx` around 3700 | Draft carries string locations, not F ID or physical identity. Guard old lineage, admitted locator keys, full absence/move domain and resulting classifications. |
| File lifecycle/reconciliation | `logical_at` around 4111, `reconcile_logical` around 4465, `create_memory` around 4802 | Source-and-producer pairing is not ownership transfer. Guard every protected lineage regardless of family string. |
| Git CLI/MCP/library | `record_git_observation` / `record_git_ingest` around 2273/2283 → `record_git_observation_tx` around 5078, Git identity around 5547 | `git_path` + repository/commit/path is distinct from F. Unrelated Git remains allowed; no automatic working-tree/Git equivalence. |
| Manual create/update | MCP [tools](../src/mcp/tools.rs) around 325/345, public `record_manual_write` around 2726; target guard around 4324 | Separate manual provenance; cannot update a file target through manual API. No CLI manual create/update command was found. Preserve unrelated manual writes. |
| Explicit retirement | CLI around 709 / MCP around 360 → `retire_memory` around 3309 | Currently any active memory can be retired. After publication, protected F/D cohort changes need a typed private transition; arbitrary retirement would destroy completeness. |
| Membership addition | MCP around 449 → `add_memory_spaces` around 2866 | Currently any active memory may gain spaces. Guard protected cohort membership; generic source-label matching is inadequate. |
| Embedding indexing | [indexing](../src/indexing/mod.rs) around 32/69/113 → `list_assertions_needing_embedding` around 2921 → inference → `save_embeddings` around 2878 | Selection and inference are outside save transaction; filter candidates/status and enforce again at actual vector INSERT/UPDATE/DELETE. |
| Vector clear | Public Store `clear_embeddings` around 3001; no production CLI/MCP clear caller found | A batch touching mandatory protected vectors refuses atomically, with no partial clearing. Unrelated eligible operations retain baseline behavior. |
| FTS maintenance / metadata | `rebuild_fts_index` around 443, `set_meta` around 3025 | Faithful all-assertions rebuild is a controlled atomic maintenance action. Metadata cannot create bindings, private permits, effective heads or release fences. |
| Document derivation and staging | [documents](../src/storage/sqlite/documents.rs), [lexical](../src/storage/sqlite/lexical_staging.rs), [semantic](../src/storage/sqlite/semantic_staging.rs) | Ds/X/C/L/S create no ordinary memory/assertion/active-vector projection. P and receipts remain preparation evidence. |
| Code observation/resolution | `record_code_observation` around 2339/3617 and resolver around 3529 | Separate syntax facts; no ordinary memory ownership follows. Do not globally disable code persistence. |

No production durable queue/background scheduler was found. Worker subprocesses
extract and the parent commits. External queued commands, caller retries and
long-lived index workers still require stale-result handling; no scheduler is
needed to state that obligation.

### Concrete resurrection and identity counterexamples

Observed `record_observation_tx` around 3918 deliberately reconciles unchanged
content. Retiring F episodes is not a fence: a subsequent unchanged ingest can
create a new active episode. The earlier S3e disposable production probe
`/tmp/m8-s3e-legacy-resurrection.json` demonstrated it; that probe was inspected,
not rerun as a new publication test.

`logical_at` chooses a stored exact-string location. Locations are not unique.
An absent location with changed content can create another F (around 3814–3851).
Therefore guarding just F IDs or just `file:legacy` cannot close same-path
resurrection. M6 fallback families include `legacy:isolated:<memory-id>`; producer
names are not an authority boundary.

Full directory observations inspect all stored Fs under their root (around
3740–3766), infer unique hash moves and record absence (around 4041). Dropping
protected submitted files before reconciliation can mark them absent or affect
move inference. The whole planned mutation domain must be classified. Absence
today does not itself retire knowledge.

## 3. Identity and alias contract

Observed draft types in [core](../src/core/mod.rs), `SessionDraft`, `ObservedFile`
and `IngestObservation` around 920/931/1264, permit string roots/locations. A
caller-provided root alone proves no filesystem resolution or authority.

Normal [ingestion](../src/ingest/mod.rs) canonicalizes root/input and files around
97/374/1044. [Directory traversal](../src/ingest/outline.rs) around 171–194 skips
symlinks with `follow_links(false)`; explicit file-symlink input follows
canonicalization. Hardlinks are not deduplicated by device/inode. Confined
[read_beneath](../src/ingest/confined.rs) verifies the opened descriptor's current
real path against the anchored relative path; worker result validation checks
root equality/containment/no `ParentDir`, not full OS canonicalization. Public
[document draft](../src/core/document.rs) validation around 868 accepts bounded,
nonempty, NUL-free locations; the document CLI canonicalizes them separately.

### Proposed admission rules

| Case | Passive plan / future publication requirement |
| --- | --- |
| Same F or D lineage through any producer | Follow authoritative source/evidence/assertion ancestry; producer relabeling does not escape protection. Include historical source versions and old/new mutation endpoints. |
| Absolute and relative location | Use a versioned namespace plus normalized key. For filesystem namespace require absolute validated adapter context or explicit administrative witness; collapse `.`/redundant separators, reject unresolved `..` and ambiguous root combinations. Preserve original display string. Syntactic normalization is not authorization or physical proof. |
| Known same file through aliases | Administrator explicitly lists exact F/D association and bounded witnessed alias keys. Passive plans are advisory; only publication activates negative reservations. Recheck live evidence/expected rows before publication. |
| Symlink or hardlink | Admit known resolved aliases explicitly using existing permitted reads; no symlink traversal outside granted roots. Device/inode supports a collision check, not a durable universal identity; reuse/staleness can refuse publication. |
| Rename | Existing ordinary rename inference remains unchanged before publication. A plan can become stale. After publication a controlled source-only observation/explicit binding update retains old negative reservations; new conflicting location refuses. |
| Deleted/unavailable file | Retained exact F/Fs/Ds/X/C evidence is not live-file proof. Initial publication requires justified admitted identity closure; unavailable required witness refuses. Do not release a reservation merely because a file vanished. |
| Equal bytes / copied content | Hash equality never grants association or alias identity. Independent Fs/Ds/copies remain distinct unless explicitly and validly admitted. |
| Legacy/document mismatch | Wrong source kind, Fs/Ds hash/extractor pins, association or independently active D knowledge refuses publication. No metadata repair or latest-revision substitution. |
| Git/directory overlap | Git repository/commit/path remains separate. Existing competing Git knowledge in an intended asset's domain blocks adoption until an explicit separately reviewed policy resolves it. Do not suppress Git globally or infer adoption by pathname/hash. |
| Opaque public library locations | Match exact declared namespace/known F and classify results at storage. A private token must not become mandatory for every unrelated ordinary observation. If physical non-aliasing cannot be established for an affected domain, refuse that asset's publication or narrow the explicitly accepted domain; do not assert universal protection. |

A proposed binding in a passive plan is **not unique across plans** and does not
reserve a source/path. Competing plans may coexist; only a future publisher checks
unique activated bindings under serialization. SQL must not make unrelated
ordinary writes depend on passive plan rows.

Unknown symlinks, hardlinks, external moves, opaque aliases and undisclosed
producer identities cannot be exhaustively found from database strings. The
contract protects the explicitly admitted logical closure and checks discoverable
collisions. If the required guarantee is all physical aliases regardless of
knowledge, publication is **BLOCKED**: the current architecture cannot prove it.
No equality hash, caller flag or administrative claim converts that assumption
into an automatically verified fact. Within Model A the operator must explicitly
accept the bounded namespace/alias inventory; unresolved known collisions refuse.

## 4. State machine and authority

Proposed state relation:

```text
LegacyWritable(A/F, no head, no effective fence)
  -- derive P/C/L/S --> LegacyWritable (no authority/visibility change)
  -- explicit prepare_fence_plan --> LegacyWritable + immutable advisory plan
  -- ordinary legacy update --> LegacyWritable; earlier plan can be stale
  -- failed/cancelled publication --> LegacyWritable (unchanged by that attempt)
  -- authorized COMPLETE publication transaction --> DocumentPublished(g=1)

DocumentPublished(g) + permanent activated bindings/restriction
  -- ordinary affected writer --> explicit atomic refusal
  -- ordinary disjoint writer --> existing ordinary behavior
  -- authorized complete revision/reversion --> DocumentPublished(g+1)
  -- release/freeze-only/restore/deactivate --> unsupported in initial protocol
```

There is no stored `LegacyFrozen`, active-without-head, mutable `enabled` flag or
ordinary metadata authority. A computed stale-plan status is inspection output,
not mutation of an immutable receipt. A plan's authorization is only to persist
that plan, not to activate publication later. Publication needs a separate exact
confirmed local administrative intent.

An active restriction is permanent against ordinary writers, including jobs
prepared before publication. Later rollback is a separately authorized
compensation, not erasure of an earlier successful COMMIT. No temporary release
may resurrect an old queued operation. Deactivation/legacy restoration remain
unsupported until a separately accepted withdrawal/replacement policy satisfies
availability and stale-writer invariants; do not precreate those states now.

After publication ordinary file ingestion cannot provide a new Fs safely because
it invokes reconciliation. Updates need a separately authorized **source-only
refresh**: append exact Fs/observations without evidence/assertion/memory/vector
or head changes, derive new Ds/X/C/L/S, then publish a complete next generation.
This is postpublication work, not a reason to freeze preparation-time ingestion.

## 5. Storage boundary and SQL proposal

### First independently implementable slice: passive only

Propose next additive migration **0017 only if still next at implementation**.
Do not rewrite 0013–0016. Minimum separate records:

| Proposed relation | Minimum content / enforceable rule |
| --- | --- |
| `asset_fence_plans` | Immutable plan ID, action/version `prepare_fence_plan@1`, database binding, A/F/D/P FKs and exact expected Fs/Ds/X pins, expected recording R, bounded canonical input/hash and expected cohort/binding-policy fingerprints. Fixed `passive` kind only; no active/frozen/head/generation column. |
| `asset_fence_plan_bindings` | Immutable plan FK, ordered bounded namespace/key and witness digest/status records. Unique ordinal/key within one plan, **no global exclusive reservation**. Witnesses describe observed evidence, not a grant. |
| `asset_fence_plan_receipts` | Unique operation ID/nonce, exact input hash/plan/result/R and successful action binding. Immutable including REPLACE, alternate keys and DELETE; deferred same-operation/R relation prevents receipt/plan mismatch. No receipt for failure. |

Use appropriate SQLite types/CHECKs, FKs, unique indexes and BEFORE INSERT
collision guards on both primary and alternate keys. Immutable UPDATE/DELETE
triggers alone are insufficient against REPLACE with recursive triggers disabled.
Private administration permit is scoped only to the new tables; ordinary M6,
metadata, ingestion and S3b–S3d permits cannot write plans/receipts. Protect the
new-table insert boundary as established by existing admin implementations.
Plain SQL with FK enforcement disabled or upsert cannot fabricate an authorized
plan through a supported Store connection; a hostile same-UID process controlling
its own raw database connection is excluded, as in S3b.

**Install no new trigger/UDF dependency on ordinary legacy tables, no critical
relation rename, no reserved-location check in ordinary writers and no active
state/API.** A missing UDF may cause preparation failure even with a false trigger
condition; see experiment below. Passive data cannot be used to deny ingestion.

Migration runner remains serialized and atomic, including marker/R handling,
FK/trigger inventory validation and UNKNOWN recovery. Populate schema-16 fixtures
before migration and compare all old rows/FTS/vectors/history/search. No automatic
plan enrollment or writer fence is a migration side effect. Migration failure
leaves the old marker/schema unchanged; UNKNOWN requires healthy reopen and exact
registration/schema inspection, never a destructive down migration.

An additive schema bump **does** reject future schema-16 binary opens, even when
new tables are passive. This is an explicit client upgrade requirement, not a
fence or unchanged old-binary promise. Rollout must upgrade supported clients and
communicate the version boundary; already-open old handles are not evicted by it.
No global interruption of supported ordinary writing is authorized. If continued
fresh old-binary opens are a deployment requirement, the passive migration must
not be installed; use read-only planning until a compatible rollout is accepted.

### Finite passive APIs and operation order

Use explicit trusted local administrative operations only:

- `preview_fence_plan(request)` returns bounded canonical passive intent,
  expected affected cohort and proposed binding/witness records. It creates no R.
- `prepare_fence_plan(confirmed_intent, operation_id)` returns the immutable plan
  and durable successful receipt. It grants neither publication nor reservation.
- `inspect_fence_plan(plan_or_operation_id)` returns historical outcome and
  separately labeled current freshness/integrity or unchecked witness status.
  It writes no R and performs no automatic refresh/rebase.

Intent binds the existing Model A database path/device/inode and kernel UID,
action/contract version/nonce, exact A/F/D/P + Fs/Ds/X, expected pre-R/cohort,
bounded namespace/witness records and policy versions. It selects no L/S/head or
active binding. A persisted witness is an observation, not perpetual filesystem
proof. Do not accept a caller-supplied principal in place of the existing context.

Prepare validates context, existing DB and full canonical operation-ID binding,
then looks up exact durable receipt before current freshness/witness checks.
Enter IMMEDIATE, repeat receipt/changed-intent checks first, validate exact
expected state, insert only passive plan/binding/success-receipt records under
its scoped private permit in one R, and commit using existing outcomes. Exact
historical replay returns the original receipt/R even if the plan is now stale;
changed intent under the same nonce conflicts. There is no implicit enrollment,
MCP endpoint, publication capability or ordinary-table guard. Subsequent real
publication must establish a separate exact authorized intent.

### Later joint publication schema: not part of passive slice

The future publisher needs effective asset/binding fences, immutable exact
publication manifests/projection maps/vector origins/generations/receipts and a
CAS head. SQL/private permits must enforce:

- An effective fence/binding names an existing valid A/F/D/P and the **same winning
  operation/generation** as a complete visible document head, receipt and required
  ordinary projection/index/membership evidence. No independent activation API.
- Unique activated F/D/locator ownership, exact expected state and permitted
  document-head transition only; no ordinary metadata grant or unsupported owner.
- Deferred exact relations, inside-writer completeness verification and final
  actual FTS/vector/citation checks; matching counts alone do not prove completeness.
- Once activated, no DELETE/reassignment/unfencing; reversion uses a fresh complete
  generation. Existing S3b legacy enrollment/epoch1 remains immutable evidence.
- Guard old **and** new identity endpoints before mutation. Do not authorize a
  changed assertion/evidence/source simply because OLD belonged to an unmanaged
  source, or skip a protected OLD because NEW points elsewhere.

Affected SQL surface requiring explicit row-guard proof:

| Table/domain | Bypass to prevent / permitted future private action |
| --- | --- |
| `logical_sources`, `sources`, observations/events | Source kind/identity/location substitution, REPLACE and new F at activated key; source-only refresh must not reconcile knowledge. |
| Evidence/assertions and ancestry | Insert/update/delete/reparent/REPLACE into protected lineage or projection; append immutable exact projection only under publication action. |
| Memories, lifecycle and memberships | Create/reactivate/retire/supersede/delete, assertion substitution, membership changes; only a complete controlled generation transition. |
| Ordinary `assertion_embeddings` | Late jobs, wrong payload/key/origin, insertion, overwrite or deletion affecting mandatory vectors; protected vector checks must survive old handles bypassing `memories`. |
| Ordinary assertion FTS / rebuild | No unselected document insertion or partial rebuild. Preserve all-assertions baseline and exact canonical text; do not copy staging postings/statistics. |
| Effective heads/bindings/receipts | Ordinary write/M6 permit, REPLACE/upsert or mutable metadata must not activate, rewrite or release authority. |

Raw FTS virtual/shadow table tampering through arbitrary privileged SQL is not
made impossible by a normal-table trigger. Supported maintenance must use the
private atomic path and verify engine/canonical integrity. Missing guards or
corrupt required indexes refuse publication/query integrity checks. Do not claim
SQLite is a security boundary against its trusted owner.

## 6. Per-write concurrency and recovery contract

Proposed affected operation order after publication exists:

1. Acquire existing write serialization with `BEGIN IMMEDIATE`, before deciding
   current source/head/binding state. Validate the effective relation/layout in
   that same transaction. Do not cache authority from extraction/dispatch time.
2. Resolve old source/evidence/assertion ancestry, input/admitted normalized keys,
   full observation scope/disappearance/move domain and resulting new identities.
   Check the action-specific permit and active fence. A wholly disjoint ordinary
   operation follows existing behavior. A mixed intersecting atomic batch refuses
   in full; silently filtering protected files is prohibited.
3. Perform mutations with SQL final guards, including each stale vector save and
   ordinary lifecycle/membership operation. Exact private publication scope must
   not be a connection-global boolean usable by a different concurrent operation.
4. Final publisher revalidates expected R/head/Fs/Ds/X/C/L/S/cohort/spaces/aliases
   after expensive candidate/Q1 work outside the writer, then installs every
   replacement/index/fence/head/receipt row atomically. No long model inference
   lock. Source/canonical hashes also recheck; R alone is not corruption detection.
5. Use existing `WriteTransaction::commit` classification in
   [sqlite.rs](../src/storage/sqlite.rs), around 159–202. Definitive veto/rollback,
   committed outcome and `CommitOutcomeUnknown` remain distinct; discard an
   uncertain connection as current implementation does.

Ordering: a writer that commits first may invalidate the publisher's expected
state. A publisher that commits first causes a delayed affected writer to refuse
inside its storage transaction. If a worker holds a write lock while paused,
publication waits or reports busy; no fence exists until the successful atomic
handoff. Never infer liveness from missing receipts or a timeout.

New plan/publication operations inspect exact immutable receipts **first** at
admission and again under serialization before stale-state rejection. Identical
retry returns the original winning receipt/R; changed intended transition or
nonce reuse conflicts. If no receipt is observable, classify NotObserved/UNKNOWN
as appropriate and serialize before a new attempt; do not assert rollback or
terminate another process. A known noncommit retry may proceed against unchanged
expected state. Historical receipt success does not promise current input/model
availability or freshness of a passive plan.

Legacy ingestion today has **no caller-operation durable receipt**. A publication
receipt or new fence refusal cannot retrospectively resolve an older legacy
UNKNOWN. New exact plan/publication retry guarantees must not be attributed to
unchanged old ingestion. These are idempotent durable effects, not a demonstrated
exactly-once execution system.

## 7. Old-reader compatibility and snapshot policy

### Observed behavior

`SqliteStore::init_conn` around 407 calls `reject_future_schema` around 875–900
before migrations: MAX registered version >16 rejects a fresh open.
`connection` around 422–435 checks the existing connection, not a per-operation
schema/version handshake. An idle schema-16 MCP process is not evicted by a
metadata/version change.

| Existing observer | Actual dependency / compatibility implication |
| --- | --- |
| Lexical and `.at(T)` | `search` around 3207 joins physical `memories`; temporal predicate around 800–845 is episode timestamp filtering. |
| Semantic | `search_vectors` around 3038/3074 joins physical `memories`, scans eligible vectors/cosines and returns buffered results. |
| Hybrid / Auto | [retrieval](../src/retrieval/mod.rs) around 371–390 performs lexical/vector calls separately; no common SQLite snapshot today. Storage failures propagate with `?`. Auto fallback for query-embedding failure/zero semantic candidates is not a missing-table fallback. |
| Detail/history/ID/replacements | `get_memory_detail` around 2205 reads memories, assertions, evidence, source, spaces and summaries separately; `SUMMARY_FROM` around 1920 and history around 3157 use `memories`. No whole-detail snapshot today. |
| Counts/conflicts | Around 2065/3264, inspect physical active memories. An application search-only head filter is insufficient. |
| MCP transport | [tools](../src/mcp/tools.rs) around 281–300 builds whole response; [protocol](../src/mcp/protocol.rs) around 64–78 writes it afterward. No valid successful partial-hit streaming, though transport truncation can invalidate JSON. |
| Surviving old APIs | `list_assertions_needing_embedding`, `save_embeddings`, `embedding_index_status` around 2878–2999 do not join memories; old vector writes/readiness survive a memory relation rename unless separately fenced. Metadata, spaces and code/source methods also need not fail. |

### Proposed enforceable boundary

Supported publication-capable code always uses one explicit read session for
settings/space reachability/head/cohort/lexical/vector/RRF/limit/decorations/citations,
and corresponding coherent count/detail/history behavior. Visibility eligibility
precedes ranking/LIMIT. A pinned precommit snapshot may return the entire old
representation after a publication commits; it must not mix generations.

Candidate old-reader boundary: atomically rename physical `memories` to a
versioned relation such as `memory_episodes_pub_v1`, without a same-name table,
view or alias. Supported code implements both layouts, resolves the exact
protocol marker/relation inside **each** read snapshot or serialized write before
memory SQL, and invalidates cached statements when necessary. No process-wide
stale layout selection. Two supported connections opened before first publication
must keep coherent queries and unrelated writes working across cutover. Retry
only a proved noncommit/schema mismatch, bounded; UNKNOWN requires durable recovery.

An already-open unsupported old autocommit connection must fail its next old-name
active-memory statement. An explicit old read transaction may finish its entirely
old schema/data snapshot; its next old-name access after transaction end must fail.
Pause old Hybrid between branches and old detail between reads to prove they fail
or remain wholly old, never return a valid mixed active result. Cached prepared
and incrementally stepped statements require actual bundled-engine testing.

The physical rename is a **database-wide client compatibility break**, not an
asset-local fence. Unsupported old clients can lose unrelated memory operations;
this cannot be called preserving their write availability. Supported current
clients must remain available for unrelated work. If the deployment requires old
writers/readers to keep working, this mechanism is insufficient and publication
stays BLOCKED pending an alternative accepted compatibility protocol.

Active cutover occurs only in the first authorized complete publication transaction,
or under a separately explicitly accepted compatible-client rollout. It is not
activated by passive plans/migration. Preserve actual FKs/triggers/indexes/IDs and
M6's logical `memories` history/coverage mapping (`SNAPSHOT_SPECS` around 1440–1455).
Never regenerate the M6 baseline or relabel old history just to rename a table.
Guard surviving old vector/FTS mutations separately; do not claim every old method
fails. Any discovered old active-result bypass or destructive migration is a
publication blocker.

### Alternatives rejected as proof

| Mechanism | Reason insufficient |
| --- | --- |
| Future schema/version/feature flag only | Already-open old connections do not re-read it. |
| SQL trigger or new application head filter only | Triggers do not repair SELECTs never consulting new ownership; filters can miss APIs and post-LIMIT eligibility. |
| Same-name view with current-owner filter | Does not retrofit a whole-query snapshot across old Hybrid/detail branches. |
| Missing new UDF in dormant trigger / CASE | Function resolution can fail before evaluating the condition, disabling unrelated old statements; not scoped compatibility negotiation. |
| WAL `BEGIN EXCLUSIVE`, checkpoint, descriptor scan or absent receipt | None proves idle old handles are gone or prevents their next read. |
| Operator restart/quiescence attestation | Useful operational practice, but conditional human/process assumption, not selected automatically enforced proof. Separate acceptance would be needed. |
| Kill/reopen all processes or global ordinary-write lockout | Not authorized, not an asset-local fence and not an independently safe prerequisite. |

No actual relation rename has been tested on populated schema 16 here. Abstract
rename evidence is mechanism support only. **Automatic old-reader enforcement
remains unproved, hence publication remains BLOCKED.**

## 8. Temporal and retrieval semantics

Keep existing timestamp-as-of semantics explicit: current database snapshot,
episode timestamps and current membership/link/index/configuration corpus, not
recorded-revision reconstruction. A successful publication later uses one strictly
ordered half-open retirement/creation boundary Tp and stores R in its receipt.
Historical F at T<Tp must not be filtered by today's document head. Retain exact
old anchors/projection maps and label historical cohorts.

Preparation/staging/passive planning leaves lexical/semantic/hybrid/Auto scores,
active FTS/vector rows and ordinary memory state unchanged. Actual publication may
change baseline all-assertions FTS statistics; old retired assertions already
remain in that corpus. No active-only or historical ranking promise is added.
Private staged FTS/vectors are never active query sources.

Exact S1 D/Ds/X anchor + C/L/S selection and half-open canonical UTF-8 range remain
mandatory. A source move, deletion, re-ingest or original-byte erasure cannot make
an old citation resolve against a newer revision. Canonical availability is not
live-file/original-byte authenticity. Source absence does not automatically
withdraw current knowledge or release identity reservations.

Semantic coverage policy, authenticated strict query compatibility and optional
lexical-only admission remain S3e prerequisites; lexical verification never implies
semantic readiness. No-feature builds can persist/inspect passive plans, and no
new default MCP tool or input schema is authorized.

## 9. Executable adversarial acceptance matrix

Every new case below is **proposed**. Existing regression tests executed in this
task are listed separately in section 11. Use real populated schema-16 fixtures,
multiple SQL connections and production CLI/MCP/library routes. Assertions must
compare complete rows, visibility, results/scores, receipts/R and rollback state,
not just match an error string.

### Passive slice: executable before any publisher exists

| ID | Setup and required oracle |
| --- | --- |
| P01 authority/binding | Exact local admin/account/existing DB/A/F/D/P/version/nonce/intent succeeds; missing context, wrong DB/source/kind/association/expected R refuses. Paths or metadata alone never create plans. No database is created by the command. |
| P02 replay | Same operation returns exact original receipt/R after restart; altered bindings, policy, source or intent under same nonce conflicts. New operation can create another advisory plan; it cannot reserve the source against ingestion. |
| P03 ordinary compatibility | Populate file, manual, Git, code, memberships/vectors and staged data. Before/after plan compare all ordinary rows/FTS stats, lexical/semantic/hybrid/Auto/as-of results/ranks/scores/MCP definitions. Reingest protected-candidate F with unchanged/changed bytes, retire/add spaces/index; behavior stays baseline and the plan becomes stale where appropriate. |
| P04 passive constraints | Direct UPDATE/DELETE, alternate-key substitution, FK off, recursive triggers off, INSERT OR REPLACE/upsert and metadata flags cannot mutate immutable successful plan/receipt or create active owner/head/fence/publication state. Supported private permit scope is exact. |
| P05 concurrency | Same/different intent concurrent processes, paused writer holding IMMEDIATE, stale expected state and source changes between preview/commit. One exact receipt winner or explicit conflict; no locking-out ordinary legacy after transaction end. |
| P06 atomicity/recovery | Nth plan/binding/receipt insert failure, COMMIT veto/lost ACK, process kill before/after commit, live prior writer timeout, restart exact retry. Known failure vs UNKNOWN remains honest; absent receipt is not liveness/rollback proof. |
| P07 migration | Populated16→candidate17 preserves old data/history/FTS/vectors; late DDL/marker/guard/FK failure rolls back. Healthy reopen resolves unknown marker outcome. New-table guards do not attach new UDF triggers to ordinary tables. |
| P08 old opens | Actual v16 fresh open rejects newer marker without mutation; already-open v16 process remains legacy-only before publication. Passive plan neither renames active relation nor promises continued future v16 opens after schema upgrade. |
| P09 resources/build | Bound input/alias count/manifest allocation; ordinary/no-embeddings builds, no test hooks, no MCP enrollment/publication endpoint. Publication/arm/release requests cannot be represented by passive API/schema. |

### Later joint publisher/read-fence gate: cannot pass in passive slice

| ID | Setup and required oracle |
| --- | --- |
| J01 atomic activation | No standalone fence SQL/API. Inject failure at every fence/binding/retirement/projection/FTS/vector/head/receipt step: failed transaction leaves F writable and visible; successful authorized transaction yields complete D with no active F and one receipt. Fence without visible complete replacement cannot commit. |
| J02 paused/stale writers | Pause real CLI/MCP/worker/library ingest before BEGIN, inside transaction and after extraction; include queued retries. Writer wins first→publisher stale refusal; publisher wins first→affected writer fails at storage. Unrelated manual/file/Git/code positive writes still commit. |
| J03 aliases | Same locator creates new F after absence; relative/absolute/dot forms, known symlink/hardlink, rename, deleted input, duplicate content, Git overlap, opaque API draft and old/new source-ID substitution. Known activated aliases cannot bypass. Unknown closure refuses affected publication; no global opaque-draft prohibition. |
| J04 whole directory | Full-root observation includes protected observed or missing/moved F. Refuse atomic intersecting batch with no session/scope/absence/code/R partial effects. Separate disjoint observation succeeds. Do not silently skip files. |
| J05 SQL integrity | REPLACE/upsert, FK off, recursive off, source/evidence/assertion/memory reparent, alternate key, producer change, active/superseded toggles, membership mutation and vector overwrite/delete. Exact storage guards deny affected mutations; positive unmanaged equivalent succeeds. |
| J06 late indexing/maintenance | Old and new indexers select F/D assertions before cutover and resume save after it. No protected vector mutation/readiness drift. Clear refuses intersecting batch; supported faithful FTS rebuild preserves actual active projection/canonical corpus. Missing guard/FTS corruption blocks admission. |
| J07 old actual binary | Open production v16 MCP/Store before cutover. Pause lexical/vector Hybrid branches, Auto, timestamp-as-of, detail/history/replacements/counts/conflicts/ID reads and cached statements. Pinned old snapshot finishes wholly old or operation fails; no valid mixed/partial success. Old index/status/vector methods surviving rename cannot alter protected data. |
| J08 new supported handles | Two new-client handles opened on prepublication layout; first publication by third connection. Already-open handles resolve layout in their read/write transactions and keep unrelated writing and coherent all-mode/detail retrieval. No unbounded retry or blind UNKNOWN replay. |
| J09 snapshots | Multiple concurrent readers/publishers on real WAL fixture: one query sees old or complete new cohort with same-snapshot scope/settings/eligibility/ranking/citations. Long explicit old read transaction remains old until it ends, then old SQL fails. |
| J10 migration/cutover | Actual bundled engine, populated16 + all proposed metadata, FKs/triggers/indexes/M6 inventory/baseline/history preserved, integrity_check/foreign_key_check. Read-fence DDL and first complete publication commit or roll back together; no old-name alias. Failed/lost-ACK outcome inspected, not guessed. |
| J11 authorization/replay | Wrong local operation/DB/head/A/P/Fs/Ds/X/C/L/S/alias/cohort/spaces/profile/nonce refuses. Exact successful retry returns original receipt; changed intent conflicts. Concurrent winner/stale source and absent receipt handled under IMMEDIATE. |
| J12 lifecycle | New revision, reversion, deletion/move, missing original bytes, corrupt/stale indexes/citations. Old visible until complete replacement; fresh episodes on reversion and exact old anchors. No temporary unfence, automatic deactivation or old UNKNOWN rollback claim. |
| J13 coverage/temporal | All modes/required spaces, no-vector lexical-only policy, semantic-required query compatibility/no-feature refusal, T before/at/after Tp, clock issues, current corpus limitations. No silent semantic coverage loss or claimed R retrieval. |

Meaningful mutations should remove source/locator checks independently, classify
only submitted files (ignoring disappearance), install an ordinary UDF trigger
in passive schema, permit active-without-head, release fence on compensation,
skip late vector check, reuse stale layout, split snapshots, filter after LIMIT,
omit receipt and swallow commit ambiguity. Each compiled/disposable mutant must
reach its specific test oracle; a compile failure is not a killed invariant.
No such new mutation suite has been implemented or run in this design task.

## 10. Smallest implementation and operational boundaries

1. **E1a passive plan core only:** exact existing-admin/context binding,
   additive passive schema/constraints, bounded preview/classifier and immutable
   plan/receipt/retry APIs. No ordinary write guards, active states, shared indexes
   or critical relation rename. P01–P09 are its acceptance gate. Creating such a
   plan does not imply later activation is authorized.
2. **E1b joint-fence preparation in isolation:** implement/test row classifier and
   proposed guard/cutover code against disposable fixtures, without exposing an
   active operation on user databases. A mergeable artifact must not install or
   activate ordinary-table guard dependencies until the joint publisher exists.
3. **E2 coherent supported readers + actual old-reader proof:** dual-layout
   routing/snapshot code and populated bundled-engine/old-process tests. The
   candidate rename is not accepted as proven from Python experiments.
4. **E3 strict query/index routing and source-only refresh:** S3e prerequisites;
   do not widen the current token/model/ownership contracts or implement in E1a.
5. **E4 joint authorized publisher:** install effective guards/unique activated
   bindings/read fence and complete visibility transfer together; J01–J13 pass.
   No standalone arm/freeze endpoint. Only then may a real fence be activated.

These are proposals, **not authorization to implement or activate** any slice in
this task. Independent acceptance of this contract permits review preparation,
not document publication. A later implementation request must select its scope.

### Cost and recovery implications

Passive planning adds no checks to ordinary writes; it writes only bounded new
metadata under a short serialized transaction. Proposed initial input limits:
64 KiB canonical intent, 128 admitted alias records, 1 MiB total decoded witness/
cohort manifest, one asset per operation. Refuse above-bound operations explicitly,
never truncate aliases or retirement membership. These are reviewable initial
admission limits, not measured performance ceilings.

Future active checks need indexed F/D/locator equality and ancestry lookups on
both endpoints, plus whole-directory mutation-domain classification. Cost scales
with affected inputs/rows/aliases, not simply one query per asset. Add indexes for
activated binding namespace/key and F/D, assertion/evidence/source ancestry and
protected vector origins; verify plans with actual EXPLAIN QUERY PLAN and real
populated writers before claiming a bound. Do not scan every managed plan on each
unrelated write. Passive plans never participate in ordinary write authorization.

IMMEDIATE serialization exists already. Additional checks/verification lengthen
write lock occupancy; expensive hashing/candidate/model work belongs outside it
with exact revalidation inside. Directory mixed-batch refusal may require an
operator to use disjoint supported roots/operations; automatic partitioning is
deferred. Long query snapshots can retain WAL pages. No migration time, lock p99,
CPU/RSS ceiling or global semantic model concurrency limit is established here.
The prior two-worker ~2.05 GiB observation is not a memory bound.

Upgraded supported clients must be available before first publication. Fresh old
opens fail after the passive schema bump; old already-open handles require the
later real compatibility boundary. Never restore an old-name alias, lower a
schema marker, delete receipt/fence rows or restart/kill user processes as an
unreviewed recovery shortcut. After UNKNOWN, use a healthy connection/receipt
inspection; busy/unavailable means wait/report uncertainty, not guess rollback.

## 11. Executed verification and bounded probes

### Current existing-code checks

- Fresh HEAD/branch/status/signature/lineage inspection confirmed the stated S3d.2
  baseline and only the original untracked S3e design before edits.
- `cargo fmt --check` passed. Existing locked **release** suites: lifecycle 16,
  M6 temporal 19, retrieval 29, indexing 22, persistence atomicity 5, Git persistence
  atomicity 9, lexical admin 26, semantic admin 24: **150 passed, 0 failed,
  4 ignored**. Actual account process tests ran with fixture Git signing disabled
  only for their command. Log `/tmp/m8-s3e1-existing-targeted.log`.
- Initial test invocation used nonexistent `m8_lexical_admin` / `m8_semantic_admin`
  target names and failed before executing tests. Verified actual names, reran the
  eight suites above successfully. That failed selection is not a test failure.
- Unchanged production binary rebuilt with `cargo build --release --locked --bin
  mind`; no feature/test hook changes. No full suite, Clippy, new mutation tests or
  no-embeddings build rerun is claimed for this documentation-only gate. Prior
  full **1216/0/17** is historical S3d.2 evidence.

### Fresh disposable experiments

Script `/tmp/m8-s3e1-probes.py`, results
`/tmp/m8-s3e1-design-probes.json` (local ephemeral evidence; source findings/tests
above and matrix specify reproducible oracles). No real asset fence activated.

1. **Actual baseline production CLI/MCP, disposable schema16:** initialized DB,
   created space, ingested one Markdown F, opened MCP and queried a canary. Added
   only a future schema-migration marker 99 through separate SQLite connection.
   Fresh production search exited 1 with `database schema version 99 is newer
   than supported version 16`; existing MCP returned an identical lexical result.
   Registry stayed 1–16 plus the injected marker. This proves fresh-open vs
   already-open behavior, not any proposed migration/read-fence correctness.
   Binary SHA-256:
   `a5960e6b98cd311f7f00d56bc165bc0ffd155e0b328051a961b86cae308ba31d`.
2. **Python SQLite 3.54.1 toy:** a BEFORE INSERT trigger with a new unknown UDF and
   `WHEN EXISTS(empty_activation_table)` still failed an unrelated insert with
   `no such function: new_missing_guard`. Both activation and ordinary row counts
   stayed zero. This demonstrates why dormant ordinary-table UDF guards are not
   harmless; it is not a test of the bundled engine or installed production guard.
3. **Python in-memory indexed lookup proxy:** 10,000 namespace-like primary keys,
   seven rounds ×20,000 lookups. Median miss **1.304 µs** (round range
   1.256–1.438), hit **1.588 µs** (1.572–1.670). These include Python/SQLite call
   overhead only; no actual fence, normalization, ancestry, disk, lock contention,
   migration or complete write overhead is measured. Do not project a throughput
   or per-ingest latency guarantee from them.

Earlier inspected S3e Python rename/WAL/FTS probes remain bounded mechanism evidence:
cached old-name read fails after rename, pinned read stays old until end; separate
branch reads can mix snapshots; filtered hidden FTS rows affect BM25. Their
engine was Python SQLite 3.54.1, not the bundled Rust runtime. No actual old-binary
postpublication test, populated read-fence migration or publisher exists here.

### Final review and local commit outcome

Fresh independent read-only Sol review accepted both reconciled S3e and this
complete contract. Its supported-handle layout clarification and passive API
clarity finding were incorporated and focused re-review reported no outstanding
findings. Final local-link checks passed (S3e 33, S3e.1 32), balanced fences and
whitespace checks passed, and `cargo fmt --check` passed again.

The authorized signed S3e-only commit waited over eight minutes in GPG's local
pinentry without completing. The task cancelled only its own pending signer;
Git exited 128 with `fatal: failed to write commit object`. No commit was created,
no unsigned fallback or signing-configuration change occurred. At that stopping
point HEAD remained `254d1ad5794db8ac19dc187a5934910337da9b9e`, S3e was staged and
this S3e.1 file was untracked/uncommitted. Signing then blocked documentation
finalization independently of design acceptance.

The operator subsequently unlocked the existing signing key locally. A bounded,
prompt-free GPG invocation using that key created the separate S3e commit
`a68beb7fe6ea7a45858a7b3106492805dd082fdb`; its signature, exact parent and
S3e-only scope were verified. This S3e.1 design is finalized separately, retaining
the earlier failure as historical evidence. No persistent signing settings or
production behavior changed; publication remains blocked by the gates below.

## 12. Acceptance and remaining publication blockers

**S3e.1 design: ACCEPT after independent read-only review. Publication and
effective fencing: BLOCKED pending implementation and joint proof.**
The passive slice is deliberately smaller than a usable writer fence. It can be
implemented/tested without causing a legacy visibility gap or denying writes.
It cannot independently prove publication safe.

Blockers remain: complete scoped storage guards and same-locator classification;
explicit acceptable alias closure; surviving old vector/FTS writers; a proved
actual old-reader boundary plus supported dual-layout snapshots; complete exact
projection/citations/memberships; strict semantic query/coverage compatibility
where required; source-only refresh before revision updates; and one authorized
atomic replacement/fence/head/index/receipt operation. If any writer or old active
reader bypass is unhandled, publication remains denied with legacy still available.

Unknown physical aliases are an explicit trust/coverage limitation, not a solved
generic identity problem. Continued unsupported-old-client availability is not
promised across a schema/client compatibility upgrade. No malicious same-UID
protection or recorded-revision retrieval is claimed. No production schema/code,
indexing, publication, active fence, MCP definition, merge, push or S4 is changed.
