# M8 S3e.2: inert writer-fencing infrastructure

Status: final review completed for the **E1a passive plan core**. Local commit
authorization covers this passive component. Effective writer fencing and
old-reader exclusion remain unimplemented. No document is projected, no locator is reserved and no active owner,
publication head or visibility state is added.

Verified starting point: clean `/tmp/mindspace-m8-s3d2` on
`feat/m8-s3d2-semantic-staging`, signed HEAD
`f78e413fe16b4781db5e3594712f45250bf0fa22`, parent signed S3e
`a68beb7fe6ea7a45858a7b3106492805dd082fdb`, then S3d.2
`254d1ad5794db8ac19dc187a5934910337da9b9e`. Both documentation signatures were
reverified. Existing branches/worktrees are preserved; no merge or push. Final
local commit identity and signature verification are reported separately.

Read with the complete [S3e publication contract](m8-s3e-publication-design.md)
and [S3e.1 fencing contract](m8-s3e1-writer-fencing-design.md). A read-only Sol
planner confirmed that their passive E1a slice is independently safe to implement;
activation and old-reader exclusion remain joint-publication prerequisites.

## Implemented boundary

Migration [0017](../migrations/0017_passive_fence_plans.sql) adds only:

| Table | Durable meaning |
| --- | --- |
| `asset_fence_plans` | Immutable advisory selection of A/F/D/P and exact Fs/Ds/X, source/cohort/binding fingerprints, expected pre-R, operation, policy and creation R. Only state `passive` exists. |
| `asset_fence_plan_bindings` | Explicitly supplied, ordered namespace/key/witness statements, unique only within a plan. Every witness is `unverified_operator_statement`. |
| `asset_fence_plan_receipts` | Immutable successful operation ID/nonce, canonical input, local database/UID binding, plan and winning R. Only action `prepare_fence_plan@1`, expected state `LegacyWritable` and intent `PassivePlanOnly` exist. |

Plans sharing the same A/F/D/path are allowed. No ordinary ingestion, lifecycle,
indexing or search code consults these rows. Another write can make a plan stale;
it is never blocked to keep that plan fresh. Plan IDs bind the operation's
canonical digest, including nonce; they are not cross-operation ownership IDs.
There is no C/L/S selection or claim of publication readiness.

The implementation is [fence_plans.rs](../src/storage/sqlite/fence_plans.rs).
SQLite CHECKs, identity joins, FKs and deferred same-operation/R plan/receipt
relations enforce passive states and exact existing preparation/source/extraction
identity. Receipt insertion requires all expected binding rows. Exact private
row permits supplement M6 write authorization. BEFORE INSERT collision guards
cover REPLACE even with recursive DELETE triggers disabled; UPDATE/DELETE and
UPSERT mutation are refused. Raw SQL cannot grant authority by metadata through
a supported connection. Arbitrary malicious same-UID database modification or
user-supplied replacement UDFs remain outside accepted Model A.

### Accounting adjustment required by actual compatibility evidence

New authorization UDFs are attached **only to the three new tables**. No trigger
is added to a source/memory/index or shared M6 table. In particular, copying the
older per-row `s3*_availability_valid` trigger pattern onto `m6_row_availability`
would make old immutable-source/assertion/session writes depend on an unknown
function during SQLite statement preparation, even when its WHEN predicate is
false. Spaces follow a different history path; a space-only probe does not test
that hazard.

Each new plan/receipt stores its own immutable R foreign key; no per-plan M6
availability/history rows or recorded-revision query coverage are installed.
Migration 17 uses the existing serialized metadata runner, one metadata R and
one schema marker `fence_plan_schema / local-fence-plan@1`. Existing migrations,
M6 baseline and history domains are preserved. Inventory validation requires
all new-table guard names and the marker. Upgrade failure rolls back; uncertain
acknowledgement uses existing commit classification and healthy-reopen inspection.

## Administrative API and trust boundary

Only explicit [local ownership CLI](../src/cli/ownership.rs) constructs the
existing Model A context: database-owning OS account, existing initialized DB,
explicit absolute `--db`, validated path/device/inode and effective operating
context. A path is a locator, not independent authority. Same-UID programs are
trusted together; no distinct-human authentication, IAM or grant lease is added.

Commands:

```text
mind --db /absolute/mind.db ownership prepare-fence-plan \
  --asset-id A --legacy-source-version Fs --document-id D \
  --document-source-version Ds --extraction-id X \
  --expected-revision R --bindings-json '[]' --nonce UUID --json

mind --db /absolute/mind.db ownership prepare-fence-plan \
  --asset-id A --legacy-source-version Fs --document-id D \
  --document-source-version Ds --extraction-id X \
  --expected-revision R --bindings-json '[]' \
  --operation-id PREVIEW_OPERATION --apply --confirm-intent PREVIEW_DIGEST --json

mind --db /absolute/mind.db ownership inspect-fence-plan \
  --operation-id PREVIEW_OPERATION --json
```

A binding is `{ "namespace": "file-absolute@1", "key": "/notes/a.md",
"witness_digest": "<64 lowercase hex digits>" }`, or explicit `opaque@1` text.
Filesystem keys must be syntactically normalized absolute text; this does not
perform a live alias check or grant ownership. There is no filesystem traversal,
automatic alias admission, device/inode inference from equal bytes or global
reservation. Witness digests are unverified operator statements.

Before DB opening, JSON decoding rejects unknown fields, >64 KiB input or >128
bindings. Core validation also bounds individual keys to 4096 bytes, canonical
intent/JSON to 64 KiB and encoded active-cohort material to 1 MiB. It hashes
actual typed ordered memory/assertion/evidence/source/membership rows, including
producer/state and source contents, rather than trusting stored content hashes.
Selected source loading is bounded to 16 MiB and uses existing hash/structure
validation for exact Fs/Ds/X. Oversize inputs refuse whole operations; no clipped
cohort or omitted binding is attested.

S3e.2 preview and inspection both use a read-only opener. They never migrate,
create a missing DB, checkpoint or perform rollback-journal recovery, including
when a request is later refused. Apply retains the existing write-capable Model A
opener, so a separate schema migration may precede an explicitly applied action.
The preview's expected revision must describe the already upgraded database.

Before native SQLite coordination, a **temporary immutable MAIN-only** connection
must recognize initialized schema17, the schema marker and exact passive table/
guard definitions. It is discarded, never used as a live source/freshness view.
Foreign, old and incompatible MAIN is refused without accessing WAL/SHM. A valid
schema17 present only in WAL is also refused: the operator must arrange a separate
trusted upgrade/checkpoint. `init` alone need not checkpoint while another client
remains open. Neither read command performs that operation or blocks legacy writes.

Only after admission does the opener retain a normal `READ_ONLY | NOFOLLOW`
connection with SQLite locking/change detection and revalidate the contract in a
native read transaction. Accepted live readers may create/change WAL/SHM
coordination state, including if a later operation is missing/stale or a concurrent
schema change causes refusal. No universal zero-auxiliary-byte guarantee is made
after admission; database data/schema/R remain read-only. Inspection is a coherent
read transaction and labels `Fresh`, `Stale` or `Unavailable` separately
from the historical receipt; witnesses always remain unchecked. Even `Fresh`
means only that the bound DB snapshot/R has not changed, never publication approval
or live-file authenticity. Unrelated new R conservatively makes the plan stale.

## Serialization, replay and recovery

Canonical intent binds DB path/device/inode, actual kernel UID, action/version,
nonce, target/bindings, exact preparation receipt/revision, selected Fs/Ds/X,
validated provenance/canonical/structure/cohort fingerprints and expected pre-R.

1. Validate local context and input/operation identity; inspect exact durable
   receipt before rebuilding current source state.
2. `BEGIN IMMEDIATE`, repeat receipt check, reject changed-intent nonce replay,
   validate expected R and rebuild exact current bound snapshot. No automatic
   rebase/latest selection occurs.
3. Install exact scoped row permits, insert plan/bindings/receipt and commit in
   one new recording revision. No ordinary evidence/memory/index row is written.
4. Existing definitive COMMIT veto/constraint/busy failure differs from
   `CommitOutcomeUnknown`. An uncertain administrative connection is discarded.
   Receipt-first exact retry returns the winning historical receipt without R
   consumption, even after ingestion changes or live source deletion.

Multiple new plans serialize normally; same operation races share one receipt.
A stale distinct attempt refuses and leaves ordinary writing enabled. Missing
receipt means NotObserved, not process death or rollback. Live paused writers,
busy timeouts and crash recovery use durable state and SQLite serialization.
Old ingestion has no caller-operation receipt; these new plan receipts do not
retrospectively resolve an old ingestion UNKNOWN.

Test-only pause/fault hooks are under `cfg(feature="spawn-gate")`, including a
migration pause inside the existing SQLite COMMIT hook. They are absent from
ordinary production builds; they confer no production authority.

## Old-client findings and policy

The preserved actual schema-16 production binary is authenticated by SHA-256:
`a5960e6b98cd311f7f00d56bc165bc0ffd155e0b328051a961b86cae308ba31d` at
`/tmp/mindspace-m8-s3d2-review-production-mind`. Only documentation changed between
its production source baseline and the verified starting HEAD.

| Case | Implemented/observed boundary |
| --- | --- |
| Fresh v16 open after upgrade | Existing MAX schema-version check rejects version17. This is a client upgrade boundary, not an asset fence. |
| Already-open v16 MCP/Store | No per-operation version eviction exists. The additive migration deliberately preserves ordinary reads/writes; private new-table functions are never required by old ordinary SQL. |
| Old request queued during migration COMMIT | SQLite serialization delays it; once migration commits the ordinary request must succeed. Submission/reply timing proves a request spans the lock, not the exact old instruction at which it waited. |
| Old writer already holding IMMEDIATE | New migration waits or explicitly reports busy; there is no authority transition. |
| Old deferred snapshot upgrading after another commit | `SQLITE_BUSY_SNAPSHOT` is an honest stale-transaction failure; start a new transaction. It is not a fence or evidence of rollback elsewhere. |
| Current schema17 writer | Existing ordinary path is unchanged; passive metadata is not consulted. |

Automatic already-open old-reader exclusion is **not implemented or established**.
An unsupported old client must eventually fail closed against a published layout;
this cannot follow merely from a schema marker.

| Future alternative | Failure mode / operational cost |
| --- | --- |
| Explicit shutdown/restart | Useful maintenance procedure but Model A operator/process assumption. No receipt, FD scan or checkpoint proves every idle handle closed. |
| Schema/database-generation flag | Rejects cooperating/fresh clients; an old open binary never consulting it ignores it. |
| Registration/leases | Old clients do not register; a lease cannot retroactively constrain SELECTs. Adds lifecycle/session infrastructure without solving that gap alone. Not introduced. |
| Separate DB/cutover | Can isolate new index state, but old handles still address the old DB. Source ownership/query routing/writer fencing across two DBs needs a larger atomicity design; copying a path/pointer is insufficient. |
| S3e physical critical-relation rename, no alias | Candidate automatic exclusion; requires actual old-reader path audit, supported dual-layout snapshots, FK/trigger/M6 preservation and joint authorized publication tests. Not installed here. |

No global old-reader/process kill or schema-name change occurs in this slice.
Fresh old opens require upgrading the client; already-open old clients remain
legacy-only. If uninterrupted future old opens are a deployment requirement, do
not install schema17 until an accepted client rollout exists.

## Writer/storage inventory retained

The full observed path inventory is in S3e.1 section2. Fresh source review
confirms these mutation boundaries remain unchanged:

- CLI directory/file ingestion and normal/isolated MCP repository ingestion
  converge on `record_files_ingest` / `record_observation_tx`; full observation
  includes scope, move and disappearance effects, not just submitted files.
- Git CLI/MCP/library ingestion uses separate repository/commit/path identity
  and `record_git_ingest` / `record_git_observation_tx`.
- Manual creation/update is MCP/public Store; file/Git reconciliation and
  `create_memory` create/supersede ordinary knowledge. Unchanged ingestion can
  recreate retired F episodes, so retirement alone is not a future fence.
- `retire_memory`, `add_memory_spaces`, `save_embeddings`, `clear_embeddings`
  and faithful FTS rebuild have independent storage mutation boundaries. Index
  selection/inference occurs before vector save; late results must eventually
  revalidate. No actual production durable queue/scheduler was found; queued
  external commands and asynchronous extraction/index work still matter.
- Public observations carry location strings, not verified F/physical identity.
  Same-locator new F, arbitrary producer family and undeclared aliases defeat a
  producer-only/path-only future fence. Equal hashes do not establish identity.
- Document extraction/chunk/staging persistence stays private; code facts stay
  separate syntax knowledge. Ordinary metadata updates confer no new authority.

Future active guards must cover actual old/new source ancestry, assertions,
episodes/memberships, vectors and entire observation mutation domains, with
explicit activated alias bindings. None is silently preinstalled here.

## Verification evidence

Verification results and performance below are recorded separately from proposed
publication acceptance tests. No test in this slice requires an active fence,
publication, projection, semantic model inference or visibility switch.

The independent full-diff review found one correctness defect, now fixed:
retaining an immutable recognition connection could ignore a writer's later WAL.
Inspection now retains normal SQLite read-only locking. The closed/no-WAL reader
regression passes, and a compiled mutant restoring immutable mode fails that
regression. Focused independent re-review reports no remaining findings.

Earlier diagnostic runs exposed environment restrictions (`Operation not
permitted` for loopback and protected-account paths) and hardcoded schema16/R
expectations in old migration tests. Final gates use the real account context;
fixture Git signing is disabled only per test command, preserving repository
signing settings. Old fixture expectations/removal helpers now include schema17;
no old migration or producer logic is rewritten.

### Final commit-review findings and corrections

The final adversarial review found two additional MEDIUM issues, both corrected:

1. CLI preview used the write-capable opener before dispatch. A real rejected
   schema16 preview advanced schema16/R5 to schema17/R6. Preview now selects the
   read-only route; separate upgrade/checkpoint is required. Reproduction:
   `/tmp/m8-s3e2-review-preview-reproduction.json`.
2. The retained read-only connection opened before layout admission. Rejected
   schema16 inspection created WAL/SHM, and rejected foreign WAL inspection changed
   SHM. MAIN-only admission now precedes any native coordination, with native
   snapshot revalidation afterward. Reproduction:
   `/tmp/m8-s3e2-review-inspection-reproduction.json`.

New tests compare MAIN/WAL/SHM/journal inventory, hashes and modification times
before any following SQLite open can conceal side effects. They cover valid
foreign SQLite, journals, missing SHM, malformed inputs, schema16, corrupted
immutable payloads after restoring guards, and same-name altered guards/table
shapes. Exact fixed-schema validation rejects substituted definitions, not only
missing names. An actual old MCP process proves MAIN16/WAL17 refusal and continued
ordinary writing; a separate fixture checkpoint enables admitted reads. Native
read locking remains mandatory, and the old immutable-reader regression remains.

Two schema16 regressions failed against the preserved unfixed production binary,
then passed against corrected code. The independent complete diff review and
focused correction review found no additional outstanding defects.

Final commit-review verification on the corrected source:

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | Passed. |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed; `/tmp/m8-s3e2-commit-clippy.log`. |
| `cargo test --locked --no-fail-fast` | **1,260 passed, 0 failed, 22 ignored**, exit0; `/tmp/m8-s3e2-commit-full-test.log`. |
| Ten targeted release suites | **177 passed, 0 failed, 9 ignored**; `/tmp/m8-s3e2-commit-release.log`. |
| Actual old-client compatibility | **5 passed**, followed by five whole-suite repeats: **25 additional passing executions**. Includes rejected-read fingerprints and explicit checkpoint bootstrap; `/tmp/m8-s3e2-fence-plan-compatibility.log`, `/tmp/m8-s3e2-final-compat-repeats.json`. |
| Read-path red/green | Both schema16 regressions fail against the unfixed production binary; expanded corrected admin suite **26 passed, 0 failed, 1 ignored**. `/tmp/m8-s3e2-read-regressions-red.log`, `/tmp/m8-s3e2-fence-plan-admin-final.log`. |
| Source/SQL mutation campaign | Six clean baselines passed; **6/6 compiled mutants killed** at their intended oracles. Added preview write-opener and omitted MAIN-admission mutants to prior permit/completeness/REPLACE/immutable-reader cases. `/tmp/m8-s3e2-commit-mutation-results.json`. |
| Production configurations | Normal and no-embeddings release builds passed, test-hook literals absent; **10 real production CLI smoke cases passed** across both binaries. `/tmp/m8-s3e2-commit-production-smokes.json`. |

The final suite adds eight ordinary read/corruption regressions and one opt-in
old-client case. Opt-in cases and measurements were explicitly executed as above;
preexisting ignored real-inference/performance tests are not newly claimed as run.
Fresh complete independent Sol review and focused correction review found no
remaining confirmed source defects. Scope remains passive-only; supported legacy
production paths and active retrieval implementations are unchanged.


### Prior implementation-stage verification (before these review corrections)

| Gate | Executed result / evidence |
| --- | --- |
| `cargo fmt --check` | Passed. |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed; `/tmp/m8-s3e2-final-clippy.log`. |
| `cargo test --locked --no-fail-fast` | **1,252 passed, 0 failed, 21 ignored**, 69 targets including doctests, exit0; `/tmp/m8-s3e2-final-test.log`. |
| Targeted locked release suites | **169 passed, 0 failed, 8 ignored** across ten suites; `/tmp/m8-s3e2-final-release.log`. Includes existing lifecycle/M6/retrieval/indexing/atomicity/staging regressions and new plan/compatibility suites. |
| Actual old-client release cases, including opt-in tests | **4 passed**, final-source `/tmp/m8-s3e2-final-compatibility.log`. Five final-source repeats add **20 passing executions**. |
| Final-source admin concurrency/recovery repeats | Three rounds each of exact six-process replay, distinct concurrent intent, delayed replay and veto/UNKNOWN: **12 passing executions**. Combined repeat manifest `/tmp/m8-s3e2-final-repeats.json`. |
| Compiled mutations in independent disposable source/target | Four clean baselines passed; **4/4 compiled mutants killed** by their intended tests: private-permit bypass, missing binding completeness, REPLACE bypass and retained immutable reader. No compile error counted as a kill. `/tmp/m8-s3e2-mutation-results.json`. |
| Production and no-embeddings builds | Both `cargo build --locked --release --bin mind` and its `--no-default-features` variant passed. Preserved binaries have no spawn/fence-fault/pause/migration-hook literals. |
| Ordinary production/no-embeddings CLI smoke | **6 passed**: each binary runs passive prepare/exact retry, nonexclusive bindings/continued ingestion and unchanged default MCP definitions. `/tmp/m8-s3e2-production-smoke-results.json`. |
| Documentation/scope | Repository doc-link checker passed, this report's links/fences/whitespace passed, and `git diff --check` passed. No MCP, ingestion, retrieval, embeddings, existing migration or lockfile production edit. |

The implementation-stage ordinary-suite ignored cases were three actual-old-binary tests requiring
`MINDSPACE_SCHEMA16_BINARY` and one measurement test; they were explicitly run
above. Other preexisting ignored tests retain their prior scope. Fixture vectors
exercise semantic/Hybrid/Auto ranking and byte-identical pre/post outcomes; they
are not real-inference correctness evidence. No new model path is implemented.

Key reproducible additional invocations:

```bash
MINDSPACE_SCHEMA16_BINARY=/path/to/authenticated/schema16/mind \
  cargo test --release --locked --test fence_plan_compatibility -- --include-ignored
cargo test --release --locked --test fence_plan_admin \
  measure_actual_passive_plan_operations_and_ordinary_ingest -- --ignored --nocapture
```

### Changed-file scope

Production additions are migration0017, `sqlite/fence_plans.rs`, explicit CLI
variants, SQLite state/migration/inventory wiring and the native-locking read-only
administrative opener in `sqlite/ownership.rs`. New tests are
`fence_plan_migration_tests.rs`, `fence_plan_admin.rs` and
`fence_plan_compatibility.rs`, plus CLI/core unit cases. Historical fixture helpers
and current-version/R expectations were adjusted in existing migration, document,
embedding, M6, manual, scope and doctor tests. Those are test compatibility edits,
not changes to legacy producers or active search.

**Technical gate: ACCEPT for this passive E1a component. Effective fencing,
old-reader exclusion and publication remain BLOCKED by the later gates below.**

### Actual passive-operation measurements

One release run using the ordinary production binary after the inspection fix,
76-byte document, one explicit binding, 20 distinct advisory plans; CLI times
include startup/admin open, no inference:

| Operation | Median ms | Observed range ms |
| --- | ---: | ---: |
| Preview | 21.038 | 19.654–24.158 |
| Prepare + successful receipt | 17.041 | 16.608–18.134 |
| Read-only inspect | 16.656 | 15.122–19.191 |

Allocated DB pages grew **98,304 bytes** for the 20 plans/bindings/receipts and
recording revisions. Twelve in-process ordinary unchanged-file ingestion samples
had median **0.504 ms** before plans (0.438–1.393) and **0.468 ms** afterward
(0.431–0.725). Warming/noise prevents interpreting this as a speedup or an upper
bound. This measures actual passive operations; it is separate from S3e.1's
Python lookup-only proxy. It does not measure an effective fence, large-corpus
classifier, migration p99, contention limit or hard RSS ceiling.

Log: `/tmp/m8-s3e2-commit-production-performance.log` (supersedes earlier
implementation-stage and test-driver read-path timings); test
`measure_actual_passive_plan_operations_and_ordinary_ingest` is explicitly ignored
in normal suites and run separately for measurements.

### Actual production migration observations

Five fresh populated schema16 databases per workload, actual old binary seeds
and current production `init` performs migration17. No plan is prepared:

| Sections | Input bytes | Upgrade + startup median ms | Range ms | Warm reopen median ms |
| --- | ---: | ---: | ---: | ---: |
| 1 | 47 | 13.549 | 13.336–14.169 | 10.606 |
| 128 | 6,308 | 14.233 | 13.657–15.308 | 10.474 |

Each sample adds exactly one metadata R, zero plans and **69,632 allocated DB
bytes**, preserving legacy memory counts. These include startup/open and occurred
on one local host with other verification activity; they are not isolated lock
duration, contention p99 or a latency ceiling. Results:
`/tmp/m8-s3e2-commit-migration-performance.json`. A prior run during concurrent
qualification had medians 39.495/25.679 ms and maximum 41.407 ms; it is retained as
`/tmp/m8-s3e2-commit-migration-performance-during-qualification.json`. These
observations are not a size-scaling law or overhead ceiling.

## Remaining publication blockers

Effective permanent fencing and document publication remain BLOCKED pending:
all scoped storage mutation/alias guards; proved old-reader exclusion plus
supported dual-layout coherent snapshots; exact ordinary projection and citations;
strict all-mode semantic/lexical coverage and vector maintenance; source-only
refresh for updates; and one authorized complete replacement/fence/index/head/
receipt transaction. Unknown physical aliases remain a bounded trust/coverage
limitation, not a solved identity problem. No recorded-revision retrieval is
claimed. Passive plans cannot authorize any later transition.

Final acceptance covers only passive infrastructure. Local signing and the final
Git state are reported separately; no merge, push, activated fence, document
publication, indexing stage or S4 is authorized by this review.
