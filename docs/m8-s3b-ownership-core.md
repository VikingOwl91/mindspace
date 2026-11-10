# M8 S3b: trusted local legacy enrollment

Implementation review on `feat/m8-s3b-legacy-enrollment`, worktree
`/tmp/mindspace-m8-s3b`, based on accepted design commit
`0ab10f4332a0f21924c7f4cb51056acb95ff72dd`. This report accompanies the reviewed
local implementation commit; the commit identity is recorded in the handoff.
Contract: [S3b plan](m8-s3b-ownership-core-plan.md); future/deferred alternatives:
[S3a](m8-s3a-retrieval-ownership.md). No document ownership or retrieval projection.

## Authority and target

The direct `mind ownership enroll-legacy` CLI is the only administrative entry. A private,
nonclone context retains an existing DB descriptor and kernel-derived UID. It requires
explicit absolute command-line `--db`, equal real/effective UID and GID, DB ownership,
regular single-link files, protected directory ancestors and safe SQLite sidecars. Linux
local filesystem validation refuses unknown/network/FUSE filesystems. There is no MCP
route, public Store enrollment method, default ingestion change or generic principal system.

This is Model A: the local OS account/process/application and DB integrity are trusted.
Same-UID programs can intentionally invoke the command. No distinct human authentication,
malicious same-UID isolation, external grant/revocation or delegated authority is claimed.
Physical path/device/inode binding is local; DB relocation/cloning/restoration while an
operation is unresolved is unsupported and cannot silently rebind an old intent.

Non-writing recognition precedes every writable administrative SQLite handle. Any existing
rollback-journal entry is refused without recovery. Closed databases with no WAL are
recognized through an internally encoded immutable read-only URI, avoiding deliberate
auxiliary-file creation. With WAL present, recognition requires an existing validated SHM
file and uses a read-only transaction; it rechecks if WAL appears during immutable inspection.
Derived SHM cache state can change, and presence checks do not promise auxiliary-file
quiescence during normal concurrent connection cleanup.

Recognition requires initialized Mindspace state: actual baseline tables/columns and
nonempty contiguous supported migration history. Only then does the writable open use
READ_WRITE/NOFOLLOW without CREATE or caller URI semantics, followed by another initialized
state check before WAL setup/migrations. Empty/foreign files are refused without initialization
or rollback recovery. Target identity, journal state and permissions are revalidated
around opening and target/context are revalidated immediately before enrollment COMMIT.
Stable trusted directories/kernel locking remain assumptions, not race-proof protection
against a hostile account replacing every pathname.

## Operation and receipts

Preview:

```text
mind --db /absolute/mind.db ownership enroll-legacy \
  --source-id FULL_FILE_UUID --expected-revision R --nonce UUID --json
```

Apply uses the exact `operation_id` and `digest` returned by preview:

```text
mind --db /absolute/mind.db ownership enroll-legacy \
  --source-id FULL_FILE_UUID --expected-revision R \
  --operation-id FULL_ID --apply --confirm-intent DIGEST --json
```

A full ID is `local-enroll@1:<nonce>:<SHA256(canonical_request)>`. Its canonical encoding
binds physical DB identity, OS actor, exact F, source/base fingerprint, expected R,
action and the only transition `Unmanaged(F) → LegacyEnrolled(F, legacy, epoch=1)`.
Strings are length-prefixed UTF-8, integers fixed-width big-endian, UUIDs canonical bytes,
nullable locations explicitly tagged; signed source timestamps retain i64 encoding.
Requests over 64 KiB reject. A nonce alone is not an operation identity or authority.

After BEGIN IMMEDIATE, the writer checks durable receipts before current source/base/R.
Matching committed retries validate the stored canonical request and return the original
receipt/asset/R, including after source changes, without consuming another revision.
Altering bound parameters with the same full ID conflicts even before a prior commit.
Committed nonce reuse with different intent conflicts. New full IDs need a new explicit
administrative invocation; precommit previews are not durable approval decisions.

No receipt in a read snapshot means NotObserved, never proven rollback/process death.
Every writer rechecks receipt and Unmanaged/base/R under its own writer transaction.
The monotone epoch-1 state and uniqueness logically fence delayed/competing attempts;
there is no physical-termination or exactly-once execution claim. Busy/uncertain previous
writers may remain UNKNOWN. Known precommit/definitive COMMIT failure rolls back this
attempt; ambiguous COMMIT or lost acknowledgement returns CommitOutcomeUnknown. Uncertain
administrative connections/scopes are discarded. An exact newly invoked retry resolves
the durable result or safely attempts the same effect under the same transactional guards.
Output failure after COMMIT cannot be reported as rollback. Local cancellation is not
asynchronous approval revocation; undo/deactivation is unsupported.

## Schema 13 and atomicity

`0013_legacy_ownership.sql` adds only `asset_ownership`, `asset_control_intents` and
`asset_ownership_history`. CHECKs restrict owner=legacy, epoch=1, action/expected/intended
state and request version. Unique F/full ID/committed nonce and deferred composite FKs
bind source, asset, operation and committed R across ownership/receipt rows.

All control rows and their availability are immutable. INSERT guards require both an M6
recording revision and an exact transaction-private validated administrative permit;
ordinary metadata/M6 contexts cannot arm it. Typed history and availability births occur
in the enrollment transaction. Enrolled logical sources cannot change kind or acquire
Git identity through later metadata edits, including INSERT OR REPLACE with recursive
triggers disabled. An existing schema marker cannot be inserted/replaced either. Normal
location/presence changes remain allowed.
Open-time inventory requires every new enforcement trigger and the schema-birth marker.

Migration 13 owns one R_meta inside the runner's existing transaction through a private
RAII scope. DDL, enforcement, schema registration and marker commit together, with explicit
foreign_key_check while migration FKs are disabled. No nested writer, fence bypass, owner
backfill or M6-baseline replay occurs. Settings/authorization are restored on exit;
postcommit verification/restoration errors are UNKNOWN. Reopening applies nothing twice.
Fresh DBs therefore have baseline R=1 and metadata cutover R=2. Migration/open may commit
this empty metadata cutover even if the subsequent enrollment is refused; refusal spends
no enrollment revision. Existing supported databases allocate exactly one new R_meta.

The cutover uses existing `m6_row_availability`:
`('asset_ownership_schema','local-enroll@1',R_meta)`. It is immutable versioned schema
availability, with real control/history births at enrollment R. No unknown dimension is
inserted into `m6_history_coverage`: the existing capability parser rejects such dimensions.
All public temporal dimensions/capability rows and original baseline stay unchanged.
This realizes the plan's coverage cutover without implying recorded-revision retrieval.

## Compatibility and verification

Enrollment writes only the new control/history/availability rows and its recording R.
Legacy sources, evidence, assertions, memories/states/spaces, FTS and embeddings are not
projected, retired or duplicated. Legacy ingestion, Git and default MCP definitions are
unchanged. Old migration fixtures now explicitly remove only empty later metadata when
constructing genuine prior-schema scenarios; fixed latest-version expectations become 13.

Implementation-stage verification (historical, before the final review corrections):

| Gate | Observed result |
| --- | --- |
| `cargo fmt --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS; `/tmp/m8-s3b-clippy-final.log` |
| `cargo test --locked --no-fail-fast` | **1,059 passed, 0 failed, 8 ignored**; `/tmp/m8-s3b-full-tests-final.log` |
| Targeted release integration suites | **209 passed, 0 failed, 2 ignored** across ownership_admin, document_chunks/model/ingest, M6, embeddings, manual_memory, scopes, file/Git atomicity and retrieval; `/tmp/m8-s3b-release-final.log` |
| Targeted release admin/storage/migration units | **19 passed, 0 failed**; `/tmp/m8-s3b-release-unit-final.log` |
| Final six-process concurrency repeats | Six rounds each of identical and competing intents: **12 successful repeat cases**, no failures; `/tmp/m8-s3b-concurrency-final.log` |
| Targeted semantic mutation tests | **3/3 killed** by test assertions after verified recompilation; clean 21-process-test and receipt-unit baselines passed before/after; `/tmp/m8-s3b-mutations-final/summary.json` |
| Without embedding features | `cargo check --locked --no-default-features --lib --bin mind` PASS; `/tmp/m8-s3b-no-embeddings-check.log` |
| Plain production build/smoke | Default release build PASS; test-hook identifiers absent. Injected test fault/pause environment is ignored; actual enrollment/replay preserves search and spends one revision. `/tmp/m8-s3b-production-build.log` records the build; smoke was observed directly. |

Release/repeat/unit results overlap the full suite and must not be added to its count.
Ignored tests are not passing evidence. Fixtures use disposable databases/grant stores;
no user database, credentials, remote authority or mandatory model inference is involved.
Real account/process/cgroup suites run outside the outer execution sandbox. A confined
unit attempt failed the intended ancestor-UID policy with `database ancestor is not
controlled by root or the operating UID`; the actual-host full and release reruns pass.

The 21 process acceptance tests include six byte/size/mtime-preserving invalid existing
DB refusals; source/kind/context/target/intent refusal; exact replay after source changes;
six-process same/different intent races; a paused pre-BEGIN writer; writer-busy recovery;
SIGKILL before BEGIN, after BEGIN, after writes, inside COMMIT and after durable COMMIT
before acknowledgement; veto and committed lost-ACK/restart. Tests compare all legacy
canonical/FTS/vector rows and real MCP vocabulary across enrollment. Unit tests additionally
exercise valid M6 scopes without admin permission, all mutable receipt fields, matching
private scopes with forbidden owner/epoch, deferred-FK rollback of owner/history/receipt/R,
signed Unicode/control-preserving base encoding, and enrolled-source representation guards.

Genuine populated schema-11/12 fixtures run the actual older migrations and legacy ingest,
then upgrade with unchanged search/capabilities/baseline and one metadata revision.
Late DDL failure, COMMIT veto, actual metadata COMMIT with lost acknowledgement and reopen,
missing enforcement/marker and repeated open are covered. This does not emulate actual
power loss, disk exhaustion or every filesystem failure: SQLite/OS durability remains
part of the trusted boundary; ambiguous I/O must remain UNKNOWN.

Independent adversarial review found and corrected: existing empty-file initialization,
later metadata changes invalidating an enrolled file kind, and ordinary error reporting
after metadata commitment. Counterexample/regression tests exercise the first two; the
postcommit classification fix is source-reviewed, while generic known/unknown COMMIT paths
have executed fault coverage. The first full run recorded 1,046 passed / 7 failed /
8 ignored: all seven failures were old-schema fixture/latest-version/R assertions, fixed
narrowly without weakening their baseline/rollback oracles. Final gates above supersede it.

The three manual mutants removed full-intent comparison, inside-writer receipt lookup,
and receipt UPDATE immutability. They respectively caused forbidden clone enrollment,
incorrect stale-writer refusal, and successful receipt rewrite. Compile failures and an
initial stale artifact run were excluded. This is targeted mutation evidence, not an
exhaustive mutation score.

## Review boundary and disposition

The implementation-stage review described above passed its earlier counterexamples.
The final commit review then found and corrected three additional concrete gaps:

- Ordinary M6 INSERT OR REPLACE could change an enrolled source's kind/Git identity without
  firing the UPDATE-only guard. A new BEFORE INSERT guard closes that path.
- The same SQLite behavior allowed replacing the immutable schema-availability marker.
  Its insertion guard now refuses an existing marker. Both SQL regressions exercise
  recursive triggers off and on, with ordinary M6 authority and no admin permit.
- Writable preflight could recover a rejected foreign database's hot rollback journal.
  Recognition now runs before any RW handle, with the journal/WAL policy above. Real
  process regressions preserve hot/cold journal and foreign main/WAL bytes on refusal.

Fresh final-review gates on the corrected source tree:

| Gate | Observed result |
| --- | --- |
| Formatting / Clippy | Both required commands PASS; `/tmp/m8-s3b-final-review-clippy-corrected.log` |
| Full locked suite | **1,063 passed, 0 failed, 8 ignored**; `/tmp/m8-s3b-final-review-tests-final.log` |
| Targeted release integrations | **211 passed, 0 failed, 2 ignored**; `/tmp/m8-s3b-final-review-release.log` |
| Release admin/storage/migration units | **21 passed, 0 failed**; `/tmp/m8-s3b-final-review-release-units.log` |
| Corrected-tree process acceptance | **23 passed, 0 failed**; `/tmp/m8-s3b-final-review-adversarial.log` (debug) |
| Concurrency repeats | Both six-process cases repeated six times: **12 successful cases**; `/tmp/m8-s3b-final-review-repeats.log`. Independent review also repeated the exact-replay case 30 times successfully. |
| No embeddings build | PASS; `/tmp/m8-s3b-final-review-no-embeddings.log` |
| Plain production binary | Fresh default release build PASS; eight test-hook identifiers absent and fault/pause env ignored. Actual enrollment/replay/search, both REPLACE regressions and real hot-journal refusal PASS; `/tmp/m8-s3b-final-review-production-smoke.log`. |

These counts overlap, and historical mutation results remain the earlier three-mutant
campaign rather than a new exhaustive score. The first corrected full run had **1,062
passed, 1 failed, 8 ignored**: existing test
`real_claude_code_local_scope_is_keyed_to_the_directory_setup_runs_in` got empty stdout
from the external client's `mcp get`, failing its `Local config` assertion. One isolated
retry, five further isolated repetitions, and the unchanged final full rerun passed.
Cause is unconfirmed; no setup code/assertion was changed. Evidence is retained in
`/tmp/m8-s3b-final-review-tests-corrected.log` and `...-setup-repeat(s).log`.

Independent complete-file review and a separate adversarial architecture review found
no remaining defect in the corrected paths. The original foreign-journal counterexample
was independently rerun against the rebuilt debug binary and refused without mutation;
the final plain production smoke independently exercises all three new regression paths.

**ACCEPT for S3b legacy-only enrollment and local commit.**
Model-A trusted local account/process, stable protected local DB and kernel/SQLite
assumptions remain explicit. There is no independent human authentication, portable
clone/restore authority, asynchronous revoke/undo, or guaranteed resolution of ambiguous
I/O. Unsupported filesystem/permission/target contexts fail closed. Rollback journals and
WAL-without-SHM also fail closed pending normal trusted SQLite recovery. Missing SHM can
arise during normal startup or early process death, so automatic administrative recovery
at every possible crash point is not claimed. Exact retry remains idempotent once the
original database is healthy and inspectable.

No document ownership, cross-kind transfer, chunk publication/indexing, active retrieval
visibility change, recorded-revision retrieval, new MCP tool, generic IAM or S4 is delivered.
No merge or push. Stop for review.
