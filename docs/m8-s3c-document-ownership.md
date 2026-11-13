# M8 S3c: trusted local document preparation enrollment

Implementation on `feat/m8-s3c-document-preparation`, worktree `/tmp/mindspace-m8-s3c`.
Approved design: signed local commit `ae4b1c9cfdfaceaf38cde6ce392118a57e130e7b`,
parent signed S3b `8d2048ea8e65673f535f8a6df0128d25c8d14af3`.
Contract: [S3c plan](m8-s3c-document-ownership-plan.md). The original S3b branch/worktree
retains the documentation commit. This report accompanies the reviewed local implementation;
its signed commit identity is recorded in the final handoff.

## Authority and supported transition

The explicit direct CLI is `mind ownership enroll-document-preparation`. It reuses
S3b's private `AdminContext` and existing-database administrative opener unchanged.
It requires explicit absolute command-line `--db`, a recognized initialized Mindspace
database, equal real/effective UID and GID, DB-owning local OS account, protected local
file/directory/sidecar context and stable physical DB identity. Missing/empty/foreign
targets are not initialized by this command. Rollback journals and WAL without validated
SHM fail closed pending separate trusted SQLite recovery, exactly as S3b documents.

Model A trusts the account, process, application, kernel/storage and SQLite. This does
not authenticate a distinct human or resist malicious same-UID programs. No remote
authorization, broker grant, delegated authority or asynchronous revocation is added.
MCP, source text, ordinary metadata and ordinary M6 write permission cannot authorize
preparation. A path or equal content does not grant authority or prove physical lineage.

The sole transition requires an existing immutable S3b asset A/F:

```text
LegacyEnrolled(A,F,legacy,1) + NoDocumentPreparation(A,D)
  -> LegacyEnrolled(A,F,legacy,1) + DocumentPreparationEnrolled(A,D,Fs,Ds,X)
```

The one-time A–D association and initial stored snapshot are immutable. F and D remain
different logical sources. Preparation is not document retrieval ownership. There is
no owner change, epoch increment, head, publication state, readiness or selected C.

## Invocation and version binding

Preview after inspecting the exact stored IDs and current recording revision:

```text
mind --db /absolute/existing/mind.db ownership enroll-document-preparation \
  --asset-id A --legacy-source-version Fs --document-id D \
  --document-source-version Ds --extraction-id X --expected-revision R \
  --nonce UUID --json
```

Apply the returned full ID and exact digest with the same bindings:

```text
mind --db /absolute/existing/mind.db ownership enroll-document-preparation \
  --asset-id A --legacy-source-version Fs --document-id D \
  --document-source-version Ds --extraction-id X --expected-revision R \
  --operation-id FULL_ID --apply --confirm-intent DIGEST --json
```

The receiver validates A/F and S3b enrollment receipt/history; Fs→F kind `file`;
Ds→D kind `document`; X→D/Ds; NULL Git identity; actual source-content hashes/lengths;
X's original-byte hash/size; and stored nodes/count/structure hash using the existing
document validation model. Fs hashes original bytes; Ds hashes canonical text.
`Fs.content_hash = X.source_bytes_hash` is required. BOM-stripped canonical Ds may have
a different hash. There is no live source-file read or newest-version inference.
Historical snapshots are valid; “stale” means stale expected R/base or incompatible pins,
not merely an older source row. Reversion can reuse existing source versions.

The canonical domain is `mindspace.s3c.local-document-preparation@1`; full ID is
`local-doc-prepare@1:<nonce>:<SHA256(canonical_request)>`. Ordered, length-prefixed binary
encoding binds physical DB identity, kernel account attribution, A/F and original S3b
enrollment, Fs/D/Ds/X, exact source/extraction provenance, logical bases, expected R,
action and the single transition. No lossy normalization or JSON hashing is used.
The receipt additionally stores a bounded structured `intent_json` BLOB to decode the
original intent without consulting changed source metadata. It is immutable, checked
against the canonical binary bytes and exact typed receipt columns on replay. This is
the schema's only extra decoding field beyond the conceptual design; it is not authority.
Both encodings reject above 64 KiB. The versioned snapshot digest covers immutable
selected versions/extraction; logical presence/location observations are separately
bound in the full intent. Display escaping is separate from source/intent bytes.

The approved stage-1 plan deliberately has no chunk-version parameter or C pin. Missing,
old or corrupt chunk sets do not become readiness; indexing is not evaluated. The user's
“chunk-version validation as specified” is implemented by preserving that boundary.
Later indexing must explicitly select/validate Ds/X/C, exact source coverage, S1 anchors,
the actual tokenizer budget and source-only strict embedding. Nothing here invokes
tokenization or inference or makes generic legacy truncation safe for document indexing.

## Migration 0014 and integrity

`0014_document_preparation.sql` adds `document_preparation_enrollments` and
`document_preparation_receipts`. Mandatory keys are NOT NULL; fixed states/action/version
permit only preparation. Unique asset/document/full operation ID/committed nonce,
positive revisions, same-R/result deferred composite FKs and the original S3b enrollment
FK bind each receipt to exactly its association and immutable pins.

SQL inserts require the normal M6 R and a separate action-specific exact private permit.
Ordinary M6, S3b enrollment permission and metadata cannot arm it. Every new row and
availability birth is in the same transaction. UPDATE/DELETE and PK/alternate-key
replacement cannot rewrite successful control rows. Pinned source versions and extraction
identity are protected against UPDATE/DELETE/REPLACE, including collisions using different
IDs on existing unique keys. Pinned X's nodes are sealed against late insertion/replacement.
Associated D's representation identity is fixed; normal location/presence changes, new
source versions/extractions, later chunk derivation and original-byte blob erasure remain
allowed. SQL enforcement is an application invariant under Model A, not tamper-proof
storage against the account owner rewriting the entire database.

The runner applies empty schema 14 using its existing serialized transaction and private
`MigrationRevisionScope`. DDL, enforcement inventory, registration and the immutable
`document_preparation_schema/local-doc-prepare@1` availability marker commit under one
new R_meta. No backfill/enrollment or original M6 baseline replay occurs. FKs are checked
explicitly before commit; settings/authorization are restored and postcommit failures
remain UNKNOWN. Reopen allocates no additional cutover. A fresh DB now has baseline R1,
S3b cutover R2 and S3c cutover R3. Migration can commit empty metadata before a subsequent
administrative refusal. Older binaries reject schema 14; no destructive down migration.

## Replay, concurrency and failures

Receipt lookup precedes current source/base/R reconstruction, and is repeated inside
`BEGIN IMMEDIATE` before state validation. Exact replay validates the original stored
intent/target/account and returns its original outcome/R, including after later ingestion
or metadata changes. It consumes no recording revision. Changed parameters under the
same full ID, a committed nonce with different intent, another association for A or D,
or a different ID for the same association conflicts. No silent rebasing occurs.

A new writer validates expected R against the pre-allocation revision, all identities,
full intent and unassociated state under the writer lock, then commits the two rows and
availability atomically. Paused/stale writers repeat the inside-writer checks. SQLite
serialization and immutable uniqueness yield one durable effect, not exactly-once
execution. Read-only receipt absence is only NotObserved; it never proves an earlier
process stopped or its transaction rolled back.

Known precommit/definitive commit failures roll back this attempt's rows/R. Uncertain
COMMIT or lost acknowledgement uses existing `CommitOutcomeUnknown`; uncertain
administrative connections are discarded. A healthy fresh exact retry can return the
durable receipt or attempt the unchanged intent under all guards. That does not identify
which attempt committed or prove an earlier UNKNOWN rolled back. Output failure after
commit cannot be reported as rollback. There is no first-slice undo/reassociation.

## Verification and remaining gates

Executed checks on the corrected source tree:

| Gate | Observed result / evidence |
| --- | --- |
| `cargo fmt --check` | PASS; `/tmp/m8-s3c-fmt-final.log` |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS; `/tmp/m8-s3c-clippy-verified-final.log` |
| `cargo test --locked --no-fail-fast` | **1098 passed, 0 failed, 8 ignored**; `/tmp/m8-s3c-full-tests-verified.log` |
| Targeted locked release integrations | **224 passed, 0 failed, 3 ignored**; `/tmp/m8-s3c-release-tests-verified-final.log` |
| Release preparation storage/migration units | **12 passed, 0 failed**; `/tmp/m8-s3c-release-units-final.log` |
| Repeated real-process concurrency | Six rounds of exact six-writer retries, competing six-writer intents and two paused-writer cases: **24 successful repeats**, no failures; `/tmp/m8-s3c-concurrency-repeats.log` |
| Targeted semantic mutation tests | **3/3 killed** after verified recompilation; clean 8-unit + 23-process baselines passed before/after; `/tmp/m8-s3c-mutations-final/summary.json` |
| Plain default production build | PASS; `/tmp/m8-s3c-production-build-final.log` |
| Build without embedding features | `cargo build --locked --no-default-features --bin mind` PASS; `/tmp/m8-s3c-no-embeddings-build-final.log` |
| Both ordinary binaries, actual-account smoke | Explicit legacy enrollment → document preparation → exact replay after source deletion; one preparation R and unchanged pre-existing tables/search. Ten test-hook markers absent; injected fault/pause variables ignored. `/tmp/m8-s3c-production-smoke-final.log` |

Release/unit/repeat results overlap and are not added to the full-suite count.
The earlier 142/0/3 design checks and S3b 1063/0/8 are historical, not tests of this
implementation. Ignored model-inference/resource tests are not passing evidence.

The process suite contributes 23 cases: actual initialized-target/context refusals,
exact/altered intent and source-version validation, historical pins, no-C enrollment,
BOM/Unicode citation preservation, six-process races, stale writers, live writer blocking,
kill at pre-BEGIN/BEGIN/write/COMMIT/postcommit boundaries, veto, lost ACK/restart,
ordinary M6 authority and REPLACE/alternate-key substitution attacks, unchanged canonical/
FTS/vector/history rows and actual MCP endpoint absence. Tests trust the OS account; they
do not demonstrate hostile same-UID protection or distinct-human authentication.

Migration tests execute real schema-13 migrations with populated legacy data, a valid
S3b ownership receipt/history, document extraction, spaces, FTS and vector bytes before
the real 0014 upgrade. Original rows/search/public temporal capabilities remain unchanged;
exactly one R_meta appears. Late DDL failure, commit veto, lost acknowledgement/reopen and
removal of each required trigger/schema marker are exercised. Existing 11/12 upgrade
tests also traverse the actual 13/14 migrations. No user database or model download is used.

Independent complete-diff review found two LOW items, both corrected: logical bases were
moved outside the immutable snapshot digest into the full intent, and a direct stored-Fs
byte-hash corruption regression was added. The latter intentionally gives Fs and X the
same declared hash while actual Fs bytes differ, without removing guards. Independent
corrected release storage/migration rerun passed **12/0/0**; no review findings remain.

The three manual mutants remove the serialized receipt recheck, actual source-byte hash
comparison, and receipt UPDATE abort. They respectively fail the paused-writer replay,
forged matching declared-hash, and immutable-receipt assertions. Compile failures do not
count as kills. All 160 Cargo/source/migration/test files match the frozen mutation
manifest after the campaign; clean root binary recompilation and 31 clean baseline tests
restore shared artifacts. Manifest SHA-256:
`ad5e3f59ce0c4bf86b128e5d2974f653d0b70c473407ac16e847ab586dd31c11`.
This is three targeted mutations, not exhaustive coverage. Protected ordinary production
copies were not changed by the campaign.

Initial full verification was **1091 passed / 6 failed / 8 ignored**. Failures were old
migration fixtures expecting latest schema 13 or one rather than two metadata cutovers.
A subsequent run was **1095/3/8** while fixture corrections were incomplete: one revision
assertion had been changed at the wrong duplicate occurrence, and the schema-10 fixture
still expected only the three original node triggers. Corrections retain the S2 chunk-gain
one-R assertion, require two cutovers only for upgrades, and assert the exact original plus
three new sealing triggers and two new tables. Original data/rollback/citation assertions
are preserved. These were deterministic fixture failures, not classified as flakes.
The final unchanged source tree's complete rerun passed **1098/0/8**. No existing-test
flake was observed in these runs. No unrelated setup/client assertion was weakened.

Ordinary production artifacts are preserved separately from Cargo test builds, which
enable the existing `spawn-gate` test feature:

- `/tmp/m8-s3c-production-mind`, SHA-256
  `9a99acf5d730f8ab4a29611447d39c50a157e4d36c2349b3972cc4f7cd6d15ee`.
- `/tmp/m8-s3c-no-embeddings-mind`, SHA-256
  `c44e0ccb676110ad9b51ad70271ddfb269e3fe4f43dfd6848100115db74dae26`.

Durability depends on SQLite/kernel/local-storage behavior. SIGKILL and injected commit
outcomes do not emulate real power loss, disk exhaustion or all I/O faults. Unsupported
or uncertain target/recovery contexts fail closed. These are assumptions/limits, not
untested stronger security guarantees.

## Final local commit review

The commit review verified signed design HEAD, the dedicated branch/worktree and all 19
modified/new files, including deleted lines. A fresh independent complete-diff review and
a separate authority/concurrency review found no remaining findings. No production-code
correction was needed in this gate. Independent preparation/migration tests passed
**12/0/0** (`/tmp/m8-s3c-commit-independent-units.log`). The report was updated to distinguish
this commit review from the earlier uncommitted implementation handoff.

Fresh final-review checks on the same source:

| Gate | Observed result / evidence |
| --- | --- |
| Required format and Clippy commands | PASS; `/tmp/m8-s3c-commit-review-fmt.log`, `/tmp/m8-s3c-commit-review-clippy.log` |
| Required full locked suite | **1098 passed, 0 failed, 8 ignored**; `/tmp/m8-s3c-commit-review-full-tests.log` |
| Targeted release integrations | **224 passed, 0 failed, 3 ignored**; `/tmp/m8-s3c-commit-review-release-tests.log` |
| Release preparation/migration units | **12 passed, 0 failed**; `/tmp/m8-s3c-commit-review-release-units.log` |
| Six rounds of four real-process concurrency/stale-writer tests | **24 passed**; `/tmp/m8-s3c-commit-review-concurrency.log` |
| Fresh run of the three targeted semantic mutants | **3/3 killed** after verified recompilation; clean 8-unit + 23-process baseline and restoration both passed; `/tmp/m8-s3c-commit-review-mutations/summary.json` |
| Default and no-embeddings production builds/smoke | PASS; `/tmp/m8-s3c-commit-review-production-build.log`, `/tmp/m8-s3c-commit-review-no-embeddings-build.log`, `/tmp/m8-s3c-commit-review-production-smoke.log` |

The fresh builds' protected copies are `/tmp/m8-s3c-commit-review-production-mind` and
`/tmp/m8-s3c-commit-review-no-embeddings-mind`; their hashes match the ordinary binaries
above. Actual-account smoke tests confirm unchanged legacy data/search, exact replay
after source deletion, ignored fault/pause variables and ten absent test-hook markers.
Additional probes enroll a valid retained snapshot after the live source was deleted
or made unreadable, preserving search and receipts:
`/tmp/m8-s3c-commit-review-unavailable-source-smoke.log`. This is deliberate historical
snapshot behavior, not physical continuity, current-head selection or live-file access.
No existing-test flake was observed in the fresh final-review run. Counts overlap earlier
verification and must not be added together.
The fresh mutation campaign verified the same 160-file manifest hash recorded above and
explicitly rebuilt the clean root binary before its final 31 clean tests. These repeat
the same three mutation targets; they do not add three distinct coverage targets.

Publication remains unauthorized and unimplemented. Its blockers include an explicit
selected-head/lifecycle policy, all-entry legacy writer fencing, provenance-sensitive
assertion projection, membership consent, strict unpublished indexing/routing, compatible
vector readiness, shared citations, consistent retrieval snapshots and a separately
reviewed publication authority. Recorded-R retrieval remains a separate gate; existing
timestamp `.at(T)` semantics are unchanged. No S4 or indexing stage is started.

**ACCEPT for S3c preparation enrollment and local commit only**, under the stated Model-A
boundary, with the signed design commit preserved. The local commit SHA is in the handoff.
No document indexing/projection/publication, legacy deactivation, new MCP definition,
recorded-revision retrieval, merge, push or S4. Stop for review.
