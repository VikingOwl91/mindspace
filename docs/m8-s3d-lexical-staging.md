# M8 S3d.1: isolated lexical staging and exact canonical citations

Implementation: `feat/m8-s3d1-lexical-staging`, `/tmp/mindspace-m8-s3d1`.
Signed design commit: `0ab3a069591f05c17a0e4e92d8c366b35c9be82e`, parent signed S3c
`4c5d6ba28ddb3621f0cd81c084c98e0a134a8733`. Initial implementation was uncommitted
for review; the final-review section below supersedes that status.
Contract: [S3d design](m8-s3d-staged-index-design.md), [S3c](m8-s3c-document-ownership.md),
[S3b authority](m8-s3b-ownership-core-plan.md), [S3 chunks/tokens](m8-s3-chunking.md).
No merge, push, semantic staging, publication, ownership transfer or S4 work is included.

## Authority and invocation

The new direct commands are `mind ownership stage-document-lexical` and
`mind ownership inspect-lexical-stage`. They reuse S3b's private `AdminContext` and
existing-database administrative opener. Explicit absolute `--db` selects an existing,
initialized database. The DB-owning local account must satisfy real/effective UID/GID,
protected file/directory/sidecar, stable path/device/inode and local filesystem checks.
Missing, empty, unrelated and symlink targets are refused. The command does not initialize
new databases. Existing migration/open behavior may commit an empty metadata migration
before a later request is refused.

Model A trusts the OS account, executable/process, SQLite, kernel and storage. It does
not isolate malicious same-UID programs or authenticate distinct humans. SQL permits
are application invariants, not protection against an account owner rewriting SQLite or
its functions. No new IAM, broker grant, MCP endpoint, ingestion hook or metadata authority
exists. A prepared association authorizes no later operation by itself.

Preview explicitly names all selected identities and the expected recording revision:

```text
mind --db /absolute/existing/mind.db ownership stage-document-lexical \
  --asset-id A --legacy-source-version Fs --document-id D \
  --document-source-version Ds --extraction-id X \
  --chunker-version heading_aware@1 --budget-identity BUDGET --plan-hash HASH \
  --expected-revision R --nonce UUID --json
```

Apply uses the same arguments, replacing `--nonce UUID` with
`--operation-id FULL_ID --apply --confirm-intent DIGEST`. Inspect uses
`ownership inspect-lexical-stage --stage-id STAGE_SHA256 --json`.
The preview exposes the bound DB path/device/inode, account UID, preparation operation,
provenance/logical-base fingerprints, input/result digests and lexical/citation policies.
Human-readable output escapes controls separately from exact stored source bytes;
JSON output preserves machine-readable values, whose presentation consumers must escape.

## Data model and dependency validation

[Migration 0015](../migrations/0015_lexical_staging.sql) adds only:

- `doc_staged_lexical_sets`: immutable manifest, selected preparation/Fs/Ds/X/C, policies,
  input metadata, result digest and recording revision.
- `doc_staged_entries`: exact content, ordinal, chunk identity, canonical byte interval,
  S1 anchor, structural paths, payload hash and citation/original-byte status.
- `doc_staged_fts`: separate external-content FTS5 over those entries, content-only,
  `unicode61 remove_diacritics 1`, full detail, no prefix index.
- `doc_stage_receipts`: immutable complete operation intent and outcome.

There is no ordinary assertion, evidence, memory, membership, vector, active-model,
ownership or shared `assertion_fts` insertion. The SQLite engine version is part of the
lexical descriptor; mixing different descriptors in this single staging cache is refused.
FTS's own token normalization affects its postings, never the canonical stored text.

The receiver validates the original S3b enrollment and S3c association A/F–D, then the
explicit selected Fs→F, Ds→D, X→D/Ds and exact C triple `(X, chunker_version,
budget_identity)` plus plan hash. `list_document_chunks(X)` is not used to select C.
Actual Fs/Ds bytes, hashes, sizes and X/structural metadata are checked. Fs's original-byte
hash/size must match X, while Ds is canonical text and can differ, including BOM removal.
Historical snapshots are allowed. No latest-head or freshness claim is inferred.

A later matching Fs/Ds/X/C under the same enrolled F/D requires another explicit staging
operation. Ordinary re-ingestion, logical metadata or path equality cannot select it.
Missing tuples, wrong kinds/Git identities, foreign associations, corrupt hashes,
unknown chunk codecs and mismatched plans fail closed.

## Determinism, coverage and citation contract

The existing S3 plan/chunk hash recipes are factored into authoritative shared helpers,
without changing their JSON serialization or persisted identities. S3d rechecks every
selected chunk's identity, ordinal, token-count provenance, context, anchor and range.
Sorted intervals must partition `[0, canonical_text.len())` exactly: no gaps, overlaps,
missing tail, duplicates or split UTF-8 characters. Empty canonical text has zero entries.
S2's ordinary empty/whitespace input rejection remains unchanged; S1-derived empty
canonical text is supported.

One validated canonical buffer and structural snapshot serve the whole batch. No
per-citation public Store call, source/blob SQL read, filesystem read or model invocation
occurs. S1 root-byte anchor construction and UTF-8 slicing are shared with public
resolution. Each entry content is exactly its canonical slice. CRLF, LF, emoji, CJK,
combining marks, zero-width and bidi controls remain byte-for-byte intact. Structural
paths remain metadata; copied heading/context text is never indexed as source content.

An acyclic, versioned, length-prefixed SHA-256 recipe builds:

1. Input manifest over ordered pre-entry records: chunk ID, ordinal/range, text hash,
   anchor and context.
2. Stage ID over A/F/D, preparation operation, selected Fs/Ds/X/C, immutable provenance,
   input manifest and lexical/citation policy descriptors.
3. Entry IDs over stage ID and pre-entry records.
4. Result digest over ordered complete entries and verification statuses.

Physical row IDs, wall-clock times, committed recording revisions, mutable logical bases
and embedding state are excluded from artifact identity. Those contextual values that
matter for authorization are bound in the full operation intent instead. Different
logical documents cannot reuse artifact/entry identities merely because text is equal.
This is snapshot identity, not a claim of cross-revision positional stability.

Persisted entries have `citation_state=canonical_verified` and
`original_bytes_state=not_checked`. Verification attests the immutable canonical snapshot,
not authenticity or current availability of optional original blobs or live files. File
or blob disappearance does not redirect citations to a newer revision. Selected canonical
sources/extractions/nodes/chunks are SQL-sealed; historical receipts remain records of the
commit. The explicit inspector reloads the pinned snapshot, compares complete entries and
runs FTS5 `integrity-check` with `rank=1`. It reports current integrity `verified` or
`corrupt` with an error; incompatible or unavailable canonical inputs fail verification.
It does not repair or record a new revision. Exact operation replay reports historical
success with `current_integrity=not_checked`, never a new readiness assertion.

## Token and readiness boundary

This slice is lexical only. It performs no tokenization and invokes no embeddings,
including without the `embeddings` feature. S3 token counts/budget identities are bound
provenance, not renewed semantic assurance. The enforced fixed value is
`token_assurance=not_performed_lexical`. Existing S3 uses the actual pinned Nomic tokenizer;
future semantic staging must independently validate the exact embedding input, prefix,
special tokens and 2048-token ceiling through the strict nontruncating path. Generic
legacy embedding remains outside this slice.

The only durable state is `verified_lexical_unpublished`: all canonical entries and the
FTS integrity check succeeded in the committing transaction. No durable partial job,
publication eligibility, owner change, epoch increment or visibility state is represented.
A stored row or historical receipt is not proof of current cache integrity or publishability.

## SQL enforcement, atomicity and recovery

Mandatory typed fields, fixed states/action/version, uniqueness, positive revisions and
same-operation/same-result/same-R deferred circular manifest/receipt foreign keys enforce
legal storage. Action-private exact typed permits are required in addition to M6 write
permission. Entry inserts bind to the selected C and require canonical lengths/ranges;
receipt insertion checks count, ordinal continuity and total/tail coverage. UPDATE,
DELETE, replacement and upsert cannot change successful canonical rows, receipts or
availability births. New selected Fs/Ds/X, nodes and C/chunks are sealed against direct
mutation, unique-key replacement and late insertion. Ordinary new versions/different C
and optional original-byte blob erasure remain possible.

FTS virtual/shadow tables cannot have the same immutable triggers. They are explicitly a
mutable derived cache. Canonical rows/receipts remain authoritative; the checker validates
the pinned FTS schema and external-content postings (`rank=1`, not content-row counts).
Posting corruption prevents a current integrity assertion and does not rewrite the
historical receipt. Separate staging cache damage has no route into ordinary retrieval.

The full operation ID is `local-doc-stage-lexical@1:<nonce>:<digest>` in a new domain.
Canonical intent binds the DB path/device/inode, kernel UID, exact target/provenance,
logical base observations, expected pre-R, manifest/result/policies and fixed action /
`NoLexicalArtifact → CreateVerifiedUnpublishedLexicalStage`. Bounded immutable JSON permits
recovery without consulting changed source heads. It is checked against canonical bytes
and exact stored columns on replay; it conveys no new authority.

The builder first uses one consistent read snapshot. Under `BEGIN IMMEDIATE`, the receiver
checks receipts first and reconstructs/revalidates the selected candidate before writing.
This catches changed source/C rows even when an out-of-band fixture has not recorded R.
Expected pre-R and logical bases serialize competing new intents. One transaction writes
manifest, all entries, separate FTS postings, receipt and M6 availability; no partial stage
survives rollback, veto or precommit process death.

Exact retries return the original durable receipt without spending R. Reused nonces/full
IDs with changed intent conflict. A new valid operation selecting an already exact artifact
returns read-only `existing_artifact`, without inventing another receipt or R. Cache/source
verification is performed for that status. A committed nonce cannot be rehashed to obtain
that path with altered intent.

Known rollback is distinguished from `CommitOutcomeUnknown`. Lost acknowledgement or
committed-result output failure reports UNKNOWN; an uncertain administrative handle is
discarded. Reopen and exact retry inspect durable state. A writer lock/timeout or missing
receipt alone proves neither process termination nor rollback. SQLite's serialized writer
barrier fences prior transactions before a retry decides to write. Effects are durable
and idempotent, not a claim of exactly-once process execution.

Migration 15 runs in the existing serialized migration protocol with one R_meta and
`lexical_staging_schema/local-doc-stage-lexical@1` marker. It checks FKs and enforcement
inventory, preserves the original M6 baseline/history and does not enroll/backfill sources.
A fresh DB now has baseline R1 and metadata R2/R3/R4 for schemas 13/14/15. Populated schema
14 upgrades with one extra R; repeat opens do not consume another. Failed DDL/commit veto
rolls back to schema 14. A lost migration acknowledgement remains UNKNOWN and reopening
finds the committed cutover. Older binaries reject schema 15; no destructive downgrade is
provided. Test-only prefix fixtures remove empty metadata to exercise genuine old schemas.

## Resource limits and performance evidence

Admission is bounded to 16 MiB canonical text, 8192 entries, 16 MiB entry metadata,
64 MiB logical candidate, 64 KiB durable input/intent encoding and 256 MiB aggregate
staged text-plus-metadata. These are logical admission bounds, not RSS, SQLite file-size,
postings or WAL quotas. Hashing streams entry manifests/results. Validation builds a
structural event sequence once, rather than scanning the whole node tree per citation.
Two complete bounded candidate passes on a new apply support prevalidation and serialized
revalidation. There is no per-chunk full-document tokenization or model work.

Observed release measurements on this host (three runs, medians, milliseconds):

| Approximately 256 KiB | Chunks | Full candidate validation | Batched anchor/range pass | CLI preview | CLI apply | CLI inspect | DB growth, first run |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Prose | 32 | 6.11 | 0.063 | 38.02 | 52.54 | 31.19 | 356,352 B |
| CJK | 64 | 6.27 | 0.111 | 31.20 | 55.86 | 29.58 | 376,832 B |
| Uninterrupted text | 16 | 1.02 | 0.032 | 19.06 | 22.79 | 15.57 | 823,296 B |
| Mixed Markdown | 64 | 35.77 | 0.124 | 49.51 | 86.05 | 44.70 | 430,080 B |

Candidate and process fixtures use different representative text seeds; see the two
ignored test harnesses. Actual byte lengths are 262,144 except CJK (262,143 and 262,142,
respectively, because truncation selects a valid UTF-8 boundary). Logical entry metadata
is 15,329 / 30,663 / 7,663 / 30,663 bytes for the four process fixtures. Whole benchmark
peak child RSS was 31,620 KiB. In-process high-water observations were 27,156–33,096 KiB;
these include setup and previously allocated fixture data, not isolated candidate RSS.

A separate one-run **16,777,216-byte** uninterrupted-text process fixture produced
1,024 entries and 494,255 metadata bytes. Observed preview/apply/inspect were
96.24 / 703.01 / 209.18 ms; DB grew 21,356,544 bytes (85,676,032 to 107,032,576).
Whole benchmark peak child RSS was 188,212 KiB and elapsed setup-plus-run time 7.57 s.
This tests one actual input at the configured maximum; it does not establish worst-case
memory/time for all 16 MiB documents or admitted corpora. All observed WAL/SHM file sizes
were zero at the recorded before/after checkpoints; this is not peak WAL measurement.

No semantic embedding work occurs. The separate batched-citation timing starts after
loading/validating the source; full candidate timing includes source/structure/hash checks.
Setup/extraction/S3 tokenization is excluded from CLI timings; whole benchmark resource
high-water observations include that setup. SQLite file deltas include
page allocation/cache overhead and are not a portable storage ratio. Global staging FTS
integrity scans can grow with the admitted corpus, so this is not a claim of constant-time
incremental staging. Repair, GC and asynchronous resumable indexing are deferred.

## Acceptance evidence

The executable tests cover exact and altered replay, nonce reuse, actual process races,
stale/paused writers, process death before/during/after commit, live UNKNOWN writers,
rollback after partial entries/FTS/receipt, veto/lost ACK, new selected revisions,
SQL REPLACE/upsert attempts, source/hash/C/anchor corruption, unavailable originals,
UTF-8 generated partitions, real S3 chunk sets, populated schema-14 upgrade and migration
failure/concurrency. The production smoke test is opt-in and receives an independently
built production executable; it verifies staging/inspection/replay while fault environment
variables are present and checks the binary for test-hook markers.

Isolation uses a populated database with real compatible vector rows and a deterministic
query encoder: lexical, semantic, hybrid, auto and timestamp-as-of results (including raw
scores/order) are compared before/after staging. Every pre-existing table and shared FTS
shadow row is also compared. Thus shared FTS ranking-statistics drift, evidence/memory/
vector mutation and duplicate active results are checked, beyond merely looking for
staging queries. The real MCP tool list is compared and attempted staging/publication
routes are refused. No production MCP/retrieval/indexer/Git ingestion code is changed.

## Verification record

The final source checks use `RUSTC_WRAPPER=` and local cached Cargo targets. Process,
sandbox and administrative tests run outside the outer tool sandbox so they observe
the actual OS account boundary. Disposable Git fixtures disable signing only through
per-process `GIT_CONFIG_COUNT`; repository signing configuration is unchanged.

| Gate | Observed result |
| --- | --- |
| `cargo fmt --check`, `git diff --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --no-fail-fast` | 1146 passed, 0 failed, 11 ignored (66 test/doc-test targets; exit 0) |
| Release lexical/migration units | 26 passed, 0 failed, 1 ignored |
| Release lexical administrative process suite | 25 passed, 0 failed, 2 ignored |
| Five additional complete release process repetitions | 125 passed total, 0 failed |
| Adjacent release document/chunk/S3b/S3c regressions | 119 passed, 0 failed, 2 ignored across five suites |
| Ordinary release production build and end-to-end smoke | Passed |
| Ordinary release `--no-default-features` build and end-to-end smoke | Passed |
| Explicit resource harnesses | Candidate batch, process batch and 16 MiB process all passed |

The ordinary binaries are independently built without test features, copied before test
builds overwrite the Cargo output path, then supplied to the opt-in smoke test. It checks
absence of staging/store gate markers and actual staging/retry/inspection while injected
fault/pause variables are ignored. A separate source-derived binary scan found all 29
recognized fault/gate environment markers absent from both preserved production binaries. Test fixture helpers are `cfg(test)`; fault injection
is `cfg(feature="spawn-gate")`, absent from both inspected production binaries.

Six meaningful mutations in a **disposable copied source tree** were killed by failing
acceptance assertions: omit contiguous coverage, omit anchor identity comparison, weaken
external-content FTS integrity checking, remove the newly-selected-source replacement
seal, remove both receipt replay checks, and write a hidden entry to shared `assertion_fts`.
A preliminary mutation removing only the redundant pre-lock receipt shortcut survived,
because the serialized receipt check still returned the original outcome; that is not
reported as a killed mutation or a missing correctness fix. Compilation failures do not
count as mutation kills. The implementation tree was never mutated for these experiments.

During development, the new later-version deliberate-corruption fixture initially failed
with `no such function: s3d_availability_valid`. Registering the ordinary fixture's denied
availability functions fixed the test setup, without granting a private action permit.
The targeted corrected case passed before final full verification. Review also removed a
redundant UID encoding from the new intent recipe, exposed bound preview metadata and
added later-version process/seal coverage; these introduced no legacy behavior changes.

One intermediate full-suite run is **invalidated**: a disposable mutation build reused
its Cargo output directory while tests were running and overwrote the exact CLI/test
binary paths. Eleven replay process tests then failed with
`lexical staging refused: committed nonce has different intent`, matching the deliberate
both-replay-checks mutant. Source files in the implementation tree retained both checks.
The shared binary paths are recorded in both logs. Final qualification forces a rebuild
from unmutated source and runs the complete suite without any competing builds; mutation
work must use an independent target directory in future runs.

Local raw logs remain under `/tmp/m8-s3d1-*`: qualified-full-tests.log, final-clippy.log,
release-units.log, release-process.log, release-regression.log, final-process-repeats.log,
production-smoke.log, perf-candidate.log, perf-process.log, near-limit.log and mutation logs.
These are session evidence, not permanent or portable benchmark artifacts. The executable
harnesses and measured results above remain in the review tree.

## Changed files and migration compatibility fixtures

Runtime changes are confined to `src/cli/ownership.rs`, `src/storage/sqlite.rs`,
`src/storage/sqlite/{lexical_input,lexical_staging}.rs`, the narrow shared S1/S3 codecs in
`documents.rs`, and the validated snapshot reader in `document_preparation.rs`.
The latter also has `cfg(test)` fixture helpers. No Store trait, MCP, active retrieval,
indexer, embedding runtime, Git ingestion, Cargo dependency or lockfile change is needed.

New acceptance suites are `src/storage/sqlite/{lexical_input_tests,lexical_staging_tests,
lexical_staging_migration_tests}.rs` and `tests/lexical_staging_admin.rs`.
Existing `ownership_migration_tests.rs`, `document_preparation_migration_tests.rs`,
`tests/support/schema_fixture.rs`, `tests/{doctor,document_chunks,document_ingest,
document_model,embeddings,m6_temporal,manual_memory,scopes}.rs` change only to account for
the additive schema-15 inventory/cutover or construct genuine old-schema fixtures.
The populated migration checks preserve original row bytes and M6 baseline semantics;
they do not weaken compatibility assertions or erase populated ownership/preparation.

## Publication blockers and deferred work

Publication remains unauthorized and unimplemented. It needs explicit cross-kind owner
policy, fencing/reconciliation of all legacy writers, active episode/membership lifecycle,
atomic visibility transfer and ordinary retrieval provenance/citation mapping. Optional
semantic staging additionally needs strict token-safe payload/model descriptors, vector
identity/cache compatibility and its own complete verification. Recorded-revision search
is another stage; current `.at(T)` is timestamp-based lifecycle search. No complete,
verified lexical artifact alone establishes any of those invariants.


Final classification: **ACCEPT for S3d.1 lexical staging and exact canonical citations**.
Independent final read-only review found no actionable findings. All required final-tree
gates passed. The eleven ignored full-suite cases include the three new opt-in resource /
production probes, which were executed separately as recorded above; the eight inherited
ignored tests remain reported as ignored, not as passing. At that initial handoff the
implementation was uncommitted. The original worktrees/history are preserved. Publication
remains unimplemented.


## Final review and local commit

The complete 23-path implementation diff, including deletions, migration 0015, all new
acceptance suites and schema-prefix fixtures, was reviewed against the signed design and
S3/S3b/S3c contracts. Independent Sol planning and read-only review found no blocking
runtime defect. Two acceptance gaps were corrected without changing production behavior:

1. `concurrent_inspection_waits_for_staging_commit_or_rollback_without_partial_readiness`
   pauses the real stage writer after its canonical rows, FTS entries and receipt have
   been written but remain uncommitted. A separate inspector process must open the actual
   DB and remain pending. After commit it reports a verified complete artifact, with no
   extra recording revision; after writer death it refuses the nonexistent stage and no
   partial artifact/receipt/R survives. This proves end-to-end CLI serialization, including
   the administrative opener's writer barrier; it does not isolate the inspector method's
   lock from that opener.
2. The populated lexical/semantic/hybrid/auto isolation comparison now passes one fixed
   timestamp captured **before staging** into both as-of query batches. Previously each
   batch independently chose a future timestamp. The revised test compares identical
   query inputs and the actual pre-stage cutoff, including scores/order and pre-existing
   table/shared-FTS shadow bytes.

SQL immutability, exact version/range/citation identity, private action permits,
replacement/upsert refusal, same-result/R receipt FKs, all-or-nothing completion and
receipt-first recovery passed review and rerun tests. Successful exact replay remains a
historical receipt with current integrity `not_checked`; current inspection is separate.
Missing receipts do not prove rollback or process death. There is no semantic assurance,
publication/visibility state, active document row or legacy ownership change.

### Repeated measurements

Final-source release probes repeated three runs per approximately 256 KiB fixture.
Values are median milliseconds; separate candidate and process fixtures retain their
existing seeds and valid UTF-8 byte lengths described above.

| Fixture | Candidate validation | Batched anchor/range pass | CLI preview | CLI apply | CLI inspect |
| --- | ---: | ---: | ---: | ---: | ---: |
| Prose | 6.49 | 0.061 | 25.41 | 35.48 | 20.93 |
| CJK | 6.73 | 0.113 | 31.77 | 56.38 | 29.36 |
| Uninterrupted text | 1.01 | 0.033 | 29.52 | 33.47 | 24.05 |
| Mixed Markdown | 37.45 | 0.139 | 53.00 | 92.64 | 47.15 |

All process cases retained 32/64/16/64 chunks and 15,329/30,663/7,663/30,663 metadata
bytes. First-run DB growth was 356,352/376,832/823,296/425,984 bytes, respectively.
The separate 16,777,216-byte uninterrupted fixture again produced 1,024 chunks and
494,255 metadata bytes. One-run preview/apply/inspection: **93.11/668.68/198.56 ms**;
DB growth: 21,368,832 bytes. Peak observed child RSS reached **188,052 KiB** after the
large case; earlier small probes stayed below that value. Resource accounting is the
harness's cumulative maximum over waited children, not simultaneous summed RSS or an
isolated staging allocation. The whole large-fixture harness took **7.52 s**, including
legacy/document ingestion, S3 token chunking, preparation and test setup. This is not an
isolated production document-ingestion latency measurement. CLI timers start after setup;
source loading/validation is included in candidate validation but excluded from the small
batched anchor-only timer. Other verification/build jobs ran concurrently. No timing or
RSS upper-bound guarantee follows from these observations.

### Final qualification

| Final-tree gate | Observed result |
| --- | --- |
| `cargo fmt --check`, `git diff --check`, local documentation links | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --no-fail-fast` | 1147 passed, 0 failed, 11 ignored across 66 targets; exit 0 |
| Release lexical/migration suites | 26 passed, 0 failed, 1 ignored |
| Release administrative process suite | 26 passed, 0 failed, 2 ignored |
| Adjacent release document/chunk/S3b/S3c suites | 119 passed, 0 failed, 2 ignored |
| Five further complete process-suite runs | 130 passed, 0 failed |
| Ordinary production and no-embeddings builds and end-to-end smoke | Passed |
| Explicit candidate/process/16 MiB resource probes | Passed |
| Independent-target mutations | 6/6 compiled mutants killed by acceptance oracles; clean baselines passed |

Both production executables were freshly built and copied before test-feature builds
could replace the Cargo output path. Both passed actual stage/apply/exact replay/inspect
with fault/pause environment variables injected but inactive; a source-derived scan found
all 29 recognized fault/gate environment markers absent from both binaries.

Mutation execution uses `/tmp/mindspace-m8-s3d1-commit-mutations` and the **independent**
Cargo target `/tmp/mindspace-m8-s3d1-commit-mutation-target`; copied artifacts have distinct
inodes. It cannot overwrite the exclusive debug qualification target or ordinary release
probe binaries. Unmutated unit and process baselines pass before applying the six mutants.
Compilation errors and infrastructure failures are excluded from mutation-kill claims.
The implementation tree is never mutated by this execution.

Final raw evidence is under `/tmp/m8-s3d1-commit-*`: final-clippy.log,
final-full-tests.log, release-units.log, release-process.log, release-regression.log,
concurrent-inspection.log, process-repeat-1.log through process-repeat-5.log,
smoke-default.log, smoke-no-embeddings.log, perf-candidate.log, perf-process.log,
near-limit.log and mutation logs/results. The earlier implementation verification record
and its invalidated intermediate run remain historical; they are not substituted for
these fresh qualification results.

Residual limits remain Model A's trusted-account/process boundary, canonical-snapshot
verification without original-file authenticity, logical rather than physical resource
caps, global staged-FTS integrity-scan cost, fixed lexical/SQLite descriptor compatibility
and immutable retention without repair/GC. Semantic staging, publication authority,
producer/writer fencing, active lifecycle transfer and recorded-revision retrieval remain
separate, unauthorized work.

Final decision: **ACCEPT for S3d.1 lexical staging and exact canonical citations; signed local commit authorized after these gates**. The signed commit SHA is reported separately
so this report does not embed its own commit identity. No merge, push, semantic staging,
publication or S4 is authorized by this local commit. Stop for review.
