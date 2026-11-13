# M8 S3d.2: isolated semantic staging implementation

This slice constructs immutable, verified **unpublished** semantic artifacts. It
does not add document search, publication, an ownership transition, an MCP tool,
recorded-revision retrieval or a shared embedding cache.

Signed design: `ed91c781d8de74ed0b0a0847015adae096969293`, parent S3d.1
`4982faa3cdaa6f3fa051211be7692df1f2b17463`. Implementation branch
`feat/m8-s3d2-semantic-staging`, worktree `/tmp/mindspace-m8-s3d2`.
The initial implementation handoff was uncommitted for review. The final-review
section below records the subsequent local-commit qualification.

Read with the [semantic design](m8-s3d2-semantic-staging-design.md),
[lexical staging report](m8-s3d-lexical-staging.md),
[chunk/token contract](m8-s3-chunking.md),
[local authority contract](m8-s3b-ownership-core-plan.md) and
[document preparation report](m8-s3c-document-ownership.md).

## Implementation map

| Files | Responsibility |
| --- | --- |
| `migrations/0016_semantic_staging.sql` | Three isolated tables, two indexes, completion/authority/immutability/availability guards |
| `src/embeddings/strict_contract.rs`, `strict.rs`, `strict_contract_tests.rs`, `strict_tests.rs` | Pure versioned descriptor, owned asset verification, strict inference, vector validation and tokenizer/model probes |
| `src/embeddings/budget.rs`, `mod.rs`, `runtime.rs` | Unchanged budget identity through one pure helper; shared effective configuration/forward; private module wiring |
| `src/storage/sqlite/semantic_input.rs`, `semantic_input_tests.rs` | Exact parent/citation reconstruction, ordered input/vector identities and resource validation |
| `src/storage/sqlite/semantic_staging.rs`, `semantic_staging_tests.rs`, `semantic_staging_migration_tests.rs` | Private admission/generation/finalization/inspection, receipts, SQL tests and populated migration evidence |
| `src/storage/sqlite.rs` | Schema-16 migration, private context registration and startup enforcement inventory |
| `src/storage/sqlite/lexical_input.rs`, `lexical_staging.rs` | Canonical-only parent verification, historical descriptor adapter and test-only fixture helpers |
| `src/cli/ownership.rs`, `tests/semantic_staging_admin.rs` | Explicit administrative commands and real-process acceptance/production/performance probes |

Existing migration fixtures adapt expected latest schema and metadata revisions:
`src/storage/sqlite/{ownership,document_preparation,lexical_staging}_migration_tests.rs`
and `tests/{doctor,document_chunks,document_ingest,document_model,embeddings,
lexical_staging_admin,m6_temporal,manual_memory,scopes}.rs`, plus
`tests/support/schema_fixture.rs`. The lexical admin fixture registers the new
denying availability function in its ordinary-write negative/positive controls.
No Cargo/dependency/lock, MCP, active retrieval/indexer or Git ingestion file is
changed.

## Administrative boundary

The existing `ownership` CLI group gains `stage-document-semantic` and
`inspect-semantic-stage`. Both use the existing private `AdminContext` and open
only an existing, initialized, protected local database. The database-owning OS
account is the trusted administrator. The physical canonical database path,
device/inode, effective UID, exact source/artifact identities, nonce, expected
recording revision and action are bound in the intent. A path does not authorize
an association. There is no independently authenticated human principal or
protection against malicious programs under that trusted account.

Preview requires an explicit lexical stage and model. It may discover the current
compiled pipeline key when omitted, and prints the entire contract and bound
intent. It does not load assets or infer readiness. Apply requires the concrete
pipeline key, full operation ID and exact intent confirmation from preview.
Ordinary ingestion, metadata edits, MCP and the active indexer cannot call the
private write-permit path. No model installation or download occurs.

## Model and actual token contract

The only supported fresh contract is Nomic v1.5, registry revision
`e9b6763023c676ca8431644204f50c2b100d9aab`, CPU/F32, dimension 768.

| Asset | Bytes | SHA-256 |
| --- | ---: | --- |
| `model.safetensors` | 546,938,168 | `9e7d262b1fe5ea350782829496efa831901b77486bbde1cea54a4c822d010d5c` |
| `tokenizer.json` | 711,396 | `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66` |

The `doc-source-slice-strict@1` descriptor binds these exact hashes/sizes, model
architecture/revision, effective Nomic configuration, document/query prefixes,
tokenizer engine 0.23.2, actual resolved Candle core/nn/transformers 0.11.1,
Cargo.lock digest and a versioned runtime fingerprint. The legacy manifest's
0.11.0 scalar and legacy derivation keys remain unchanged.

Asset reads reject symlinks, unexpected types/sizes and unsafe local storage,
authenticate a bounded owned byte buffer, and load the tokenizer/tensors from
those same bytes. The model loader does not reopen an authenticated path.
Replacement after snapshot cannot substitute the model used by that attempt.
Future attempts authenticate again. This is byte integrity against the pinned
registry, not a claim about the semantic quality or authenticity of source files.

`TokenBudget` is the single tokenizer/counting implementation. Its existing S3
budget identity recipe is factored into a pure helper, with unchanged keys. Every
input is exactly `search_document: ` followed by the canonical chunk text,
including tokenizer special tokens; padding and truncation are disabled.
The full encoding must equal C's stored count and contain at most **2048 tokens**.
All inputs are preflighted before weight access. Each forward receives the exact
checked IDs/mask, with a second equality/digest/count check; token-type IDs are
explicitly absent. Empty raw text still has the prefix/special encoding. No
structural heading/context metadata is inserted into input text.

Pooling is attention-mask mean including prefix/specials, followed by L2
normalization. Storage requires 768 finite f32 values, 3072 little-endian bytes,
and a separately computed f64 norm within `0.0001` of one. The encoding digest
binds pipeline/budget/raw-text hash and ordered actual IDs/mask. Canonical source
bytes remain unchanged even when tokenizer normalization maps different text to
the same IDs.

The generic legacy embedding path retains its existing truncation behavior. New
semantic staging calls the private strict encoder directly and cannot fall back
to that API. Only pure Nomic configuration and the forward computation are shared;
the legacy formatter, registry identities, cache and active persistence remain
unchanged.

## Identity, citations and isolated storage

Additive migration **0016** creates only `doc_staged_semantic_sets`,
`doc_staged_vectors`, `doc_semantic_stage_receipts` and their constraints/indexes/
guards. It records one M6 metadata cutover revision. Existing schema-15 data and
retrieval capabilities are retained.

Each set binds the lexical parent's exact `(stage_id, operation_id, created_R,
result_digest)` tuple, preparation and exact Fs/Ds/X/C identities, ordered lexical
entries/chunks, canonical byte ranges, raw payload hashes, S1 anchors and token
counts. The semantic ID binds that input manifest and strict pipeline. Per-vector
keys bind the immutable source span and pipeline, independently of the legacy
assertion-vector namespace. A result digest binds the actual ordered vector bytes
and encoding evidence. No shared vector cache is used.

Canonical verification reconstructs the selected chunk plan and citations in one
bounded source pass. It verifies order, count, contiguous coverage, UTF-8 ranges,
entry text, hashes and anchors. It never resolves against a newer revision. The
semantic verifier is independent of mutable lexical FTS postings. A private
compatibility adapter reconstructs a known lexical policy using its original
recorded SQLite descriptor; fresh lexical construction and current FTS checks
retain their original behavior. It rejects unknown policy formats and compares
the regenerated original identities and receipt.

SQL enforces one fixed `verified_semantic_unpublished` state, 768 dimensions,
token counts 1..2048, vector lengths, unique ordinals/keys, exact parent membership
and complete counts. Deferred circular set/receipt foreign keys require both in
the same committed transaction. Successful sets/vectors/receipts and availability
records reject update/delete/replacement/upsert. Exact private row permits plus
the existing M6 transaction context guard creation, including when a caller
disables foreign keys. Storage startup checks the full trigger inventory and
cutover marker. This does not defend against a trusted administrator rewriting
the schema or impersonating private SQLite functions.

Inspection separately reports historical success and current canonical/vector
integrity. Fresh tokenization, lexical FTS compatibility, encoder availability and
original-source-byte status are explicitly `not_checked`. A successful historical
receipt is not erased or reported rolled back when later corruption is detected.
Canonical retention proves the cited canonical range, not authenticity or
availability of optional original bytes. Source-file disappearance/replacement
does not cause fallback to new content.

## Completion, replay and recovery

Operation IDs use `local-doc-stage-semantic@1:nonce:input_digest`. The input digest
binds the complete intent, including expected pre-R, physical database/account,
source base and resource policy. It does **not** bind future floating-point bytes.
The successful receipt binds the winning actual result digest and R.

1. Validate local context and parse intent; check an existing receipt first.
2. In bounded `BEGIN IMMEDIATE` admission, repeat receipt/nonce checks, validate
   canonical input, expected R and existing artifact; roll back admission.
3. Release every database/connection guard. Authenticate assets and generate the
   complete candidate in memory, one chunk at a time.
4. In a new `BEGIN IMMEDIATE`, repeat receipt/nonce checks and reconstruct/recheck
   source/input/base/R. Verify a concurrent winner independently and reuse it;
   do not compare its floats to a losing candidate.
5. Arm an exact private permit, insert the complete set/vectors/receipt, check
   foreign keys and commit once. No partial artifact is committed.

Exact receipt replay is independent of current assets, model registry/build or
embeddings feature; it returns the original receipt without another revision.
Altered parameters or a committed nonce with different intent conflict. A new
operation targeting an already verified identical artifact returns a read-only
`existing_artifact`, without inference or another receipt/revision. It does not
claim the new operation committed.

On generation failure, one bounded serialized receipt observation can recover a
concurrent winner. Otherwise the error distinguishes `NoCommitThisInvocation`
from operation `NotObserved` or `UNKNOWN/unobservable`. Receipt absence does not
prove another process stopped or rolled back. Actual ambiguous COMMIT results
retain `CommitOutcomeUnknown` and discard the administrative connection. Losing
stdout after a committed receipt also reports UNKNOWN; a read-only status output
failure does not. Retry uses the exact old intent and inspects durable state.
These are idempotent durable effects, not an exactly-once execution claim.

## Resource policy

The fixed profile allows at most 8192 entries, 16 MiB canonical source, 16 MiB
parent metadata, 16,777,216 total full-input tokens, 24 MiB vector payload, 8 MiB
semantic metadata and 64 MiB logical candidate. SQL caps aggregate semantic
vector+metadata storage at 128 MiB, including header charges for empty sets.
Canonical/JSON operation representations each have a 64 KiB bound. One process
loads one owned CPU model and uses batch size one; model/tokenizer load is once
per fresh attempt. Counts/encodings are recomputed per chunk, not over the entire
document per chunk. Admission/final database work excludes model inference.

The 600-second deadline is cooperative between bounded steps; a single kernel
read/model forward is not preempted. These limits are not an RSS or global
cross-process RAM guarantee. Concurrent CLI processes can each load a model.

## Initial implementation verification and measurements

These are the initial handoff observations. The final-review qualification below
supersedes their gate counts and commit disposition.

Fixture vectors exercise storage/protocol/retrieval isolation only; they are not
proof of Nomic inference correctness. Real probes use the cached pinned assets,
with no download. The production encoder and three explicit opt-in inference
tests were executed under the actual local OS account: actual 2048-token forward,
2049-token refusal, owned-assets replacement and representative short inputs.

Measured CPU release timings, one observation each (no throughput guarantee):

| Probe | Tokenizer load | Token preflight | Authenticate/load model | Inference | Serialize/verify |
| --- | ---: | ---: | ---: | ---: | ---: |
| Four short inputs: prose, CJK/Unicode, CRLF/code, empty text | 20.897 ms | 0.151 ms | 529.503 ms | 509.333 ms total | 0.160 ms total |

Combined three real encoder probes took 7.140 s with observed maximum child RSS
1,237,692 KiB, including the full 2048-token case. This is process measurement,
not a predicted RSS bound. The tokenizer test confirms empty text has six total
tokens, `hello` has seven, and `hello ` repeated 2042/2043 times has 2048/2049.

The following end-to-end CLI staging timers include process startup, canonical
validation, asset authentication/load, tokenization, inference and staging commit.
They exclude fixture setup, legacy/document ingestion, preparation, chunking and
lexical staging. Inspection is a separate process without inference. Sources are
byte-preserved, and each probe checks old active rows remain unchanged.

| Source | Bytes | Vectors / payload bytes | Preview | Staging total | Inspection |
| --- | ---: | ---: | ---: | ---: | ---: |
| Prose | 1024 | 2 / 6144 | 32.22 ms | 2028.97 ms | 17.19 ms |
| CJK | 1024 | 1 / 3072 | 25.57 ms | 1825.52 ms | 17.46 ms |
| Unbroken sequence | 1024 | 1 / 3072 | 25.82 ms | 1076.52 ms | 15.91 ms |
| Mixed Markdown | 1024 | 1 / 3072 | 25.45 ms | 1848.95 ms | 17.30 ms |
| Prose | 4096 | 1 / 3072 | 21.85 ms | 2036.11 ms | 18.31 ms |

The 1024-byte group used 17,490–17,898 charged semantic metadata bytes per set
and grew the database file by 12–20 KiB, plus a 32 KiB shared-memory file. The
4096-byte case used 17,554 metadata bytes and 12 KiB database-file growth. SQLite
page allocation and metadata charges are different measures. Source-size alone
does not predict chunk count or inference work. Maximum child RSS was 1,091,880
KiB for the four-case group and 1,092,448 KiB for the 4096-byte case; wall times
7.99/2.33 s include their test-fixture setup. These measurements do not establish
maximum-limit latency, safe global parallelism or durable disk throughput.

A separate ignored release probe ran five fresh in-memory SQLite fixtures with a
48-byte CRLF/combining/CJK/control Markdown source and one synthetic unit vector
(3072 bytes). It excludes source/fixture/token/model/vector generation. Observed
ranges: standalone metadata/vector serialization 216–238 µs; production candidate
insertion including guards and its serialization 912–942 µs; COMMIT 5 µs; receipt
decoding 240–252 µs; canonical revalidation 485–498 µs; stored vector verification
54–55 µs. Charged semantic metadata was 17,169–17,223 bytes. This separates SQLite
work from inference but does not measure disk durability, throughput or RSS.

Executed compatibility/adversarial evidence:

| Gate | Observed result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --no-fail-fast` | **1215 passed, 0 failed, 17 ignored**, exit 0, 67 target summaries |
| No-default-features production Clippy, library + binary, `-D warnings` | Passed |
| Release document chunking/chunks + lexical/semantic admin suites | 70 passed, 0 failed, 6 ignored |
| Focused strict encoder / input / storage suites | 14 / 10 / 15 passed, 0 failed |
| Real opt-in strict inference probes, independently repeated | 3 passed per run; second run 6.59 s, 1,237,452 KiB max child RSS |
| Schema-15 migration suite, repeated three times in release | 15 passes total, 0 failures |
| Five sensitive process races, five repeats each | 25 successful test invocations |
| Ordinary default and no-embeddings production builds | Both passed without warnings |
| Final ordinary production smoke tests | 1 passed each; default uses actual pinned model, no-embeddings refuses fresh generation and preserves historical replay/inspection |
| Test-only marker scan of both final ordinary binaries | None of 38 known fault/gate/fake markers present |
| Explicit storage-only and real-process performance probes | Passed; measurements and limitations above |

The full suite ran on the final source under the real local account with
`RUSTC_WRAPPER=` and `CARGO_PROFILE_DEV_DEBUG=0`. Disposable Git fixture signing was
disabled only through per-command `GIT_CONFIG_*` environment settings. No
persistent Git configuration was changed. The full-run ignored count is retained
even for opt-in cases subsequently run explicitly. Older unrelated resource
benchmarks and the old generic-runtime opt-in model smoke were not all rerun;
their ignored status is not a pass claim. All new S3d.2 opt-in probes were run.

The populated migration test starts from genuine schema 15 with legacy knowledge,
enrollment, preparation, chunks, lexical entries/FTS/receipt and an ordinary vector.
It compares old table contents, search hits, vector hits, citations and temporal
capabilities after migration. Late DDL failure and commit veto preserve schema 15
exactly. Lost acknowledgement remains UNKNOWN, restart installs no duplicate
cutover, and six real processes install a single metadata revision. Removing any
new enforcement trigger or its marker makes store open fail.

Process comparisons cover all four retrieval modes with scores/ranking, fixed
timestamp-as-of queries, old table/FTS shadow contents, active vector/index counts,
backfill candidates, clear-by-staged-key behavior, ownership/preparation and MCP
tool definitions. Staged keys do not make the active index ready or clear staged
vectors through the ordinary clear API. Byte-exact citations cover CRLF, combining
marks, CJK, emoji, zero-width/bidirectional controls, original source disappearance
and replacement. Canonical verification does not depend on FTS integrity.

Fault tests exercise partial forward/vector/receipt failures, commit veto,
interruption at admission/inference/final-write/commit phases, lost COMMIT/output
acknowledgement, a live paused writer and restart. Six competing processes can
produce different valid fixture floats but must recover one stored winner. Source
or recording-state changes during unlocked inference refuse stale candidates.
Direct SQL tests include missing/mismatched parents, publication states,
missing/reordered/wrong-token vectors, foreign-key/recursive-trigger toggles,
replacement/upsert, availability forgery and later corruption.

Meaningful mutations were confined to disposable databases or an independent
copied source/build target. Clean focused baselines passed; removing unit-norm
enforcement and removing asset SHA verification each made the intended assertion
fail. Separate SQL probes remove the receipt-completeness or vector-membership
guard and demonstrate the forbidden durable counterexample; independent artifact
verification still rejects those corrupted results. No mutation touched the
qualifying source or shared qualification binaries.

The initial full run reported 1203 passed, 6 failed, 16 ignored while test fixtures
were being completed. Four encoder fixtures had incorrect token expectations
(`actual complete token count 7 differs from stored chunk count 6`, and 2049
versus the expected 2048). One corruption fixture encountered `staged chunk is
immutable` because it had only removed one of two independent seals. One old
raw-SQL lexical fixture lacked the new denying function and reported
`no such function: s3d2_availability_valid`. Expectations and fixtures were
corrected, including a positive assertion that the second seal rejects mutation;
no production guard was weakened. Clippy also caught one constant-size chunk
decoder in an ignored measurement oracle, which now uses `as_chunks`.
Restricted-namespace runs initially failed protected-file checks and were rerun
under the actual OS account, matching the trusted-local threat boundary.

Evidence logs are local `/tmp/m8-s3d2-*` artifacts: `full-final.log`,
`fmt-final.log`, `clippy-final.log`, `no-embeddings-clippy.log`,
`release-targeted.log`, `release-migration-repeats.log`,
`real-encoder-root.log`, `real-process-1024.log`, `real-process-4096-prose.log`,
`storage-phases.log`, `production-*-final-{build,smoke}.log` and the independent
`norm-{baseline,mutant}.log` / `hash-{baseline,mutant}.log` pairs. These logs are
not committed artifacts or promises that temporary files will be retained.

## Deferred publication requirements

No staged set is publishable through this implementation. Separate authorization,
cross-kind writer/owner reconciliation, atomic visibility transfer, active
retrieval integration, cache/vector lifecycle and historical recorded-revision
retrieval remain separate gates. Existing legacy ownership stays `legacy`, epoch
1, and legacy results remain active throughout staging.

## Review disposition

**ACCEPT for isolated semantic staging only.** Independent read-only final review
found no remaining findings, and the final suite exited successfully. At that
handoff the implementation remained unstaged/uncommitted on the branch; only the
design was committed and its signature verified. No merge, push, document
publication or S4 work was performed.

Remaining operational assumptions: the local account and compiled registry are
trusted; SHA-256 provides byte-integrity binding; CPU/OS resources are available.
There is no malicious same-account isolation, hard RSS/global worker budget,
inference preemption, resumable partial-vector cache, staging garbage-collection
API, semantic-quality guarantee or automatic publication eligibility. Historical
receipt success and current source/model availability remain separate facts.

## Final review and local commit

The complete 32-path implementation, including deleted lines, new files,
migrations 0015/0016 and all compatibility fixtures, was reviewed against the
signed design and S3/S3d.1 contracts. Independent read-only Sol reviews covered
model/tokenizer integrity and SQL/authority/recovery. One LOW diagnostic defect
was demonstrated and corrected:

`verify_stored` narrowed SQLite `i64` token counts and dimensions with `as u32`.
After deliberately bypassing CHECKs and immutability to simulate offline corruption,
adding `2^32` could wrap back to the valid original and produce a false current
integrity assertion. This was not reachable through ordinary authorized writes
and is not a malicious-same-account authority bypass. The new regression first
proves the normal guard and CHECK refusal, restores both after simulating corruption,
then checks current verification refusal with an unchanged historical receipt/R.
It failed on the original code at `corrupt token_count must not alias its valid
value during readback` and passes after the correction, including overflowing
dimension and negative ordinal cases. Token/dimension reads now use checked
`Row::get::<_, u32>` inference; ordinal uses checked `u64::try_from(i64)`.
An initial direct `Row::get::<_, u64>` form did not compile because the locked
rusqlite build does not enable `fallible_uint`; explicit conversion avoids any
dependency/feature change. No schema, valid artifact identity or authority policy
changed. The SQL reviewer independently reran the new regression successfully.

### Repeated model and resource observations

Fresh real Nomic tests again passed actual 2048-token inference, 2049-token
refusal and replacement after owned model/tokenizer loading. Four short inputs
measured tokenizer setup 17.016 ms, preflight 0.109 ms, model authentication/load
579.967 ms, four per-chunk forwards totaling 349.264 ms (87.316 ms arithmetic mean),
and vector serialization/verification 0.133 ms. This does not measure each input's
latency distribution or a maximum-size document. All three real probes together
took 7.65 s and observed maximum child RSS 1,237,376 KiB.

The repeated four 1024-byte CLI workloads passed with staging totals
794.19–1483.84 ms and inspection 13.66–17.77 ms. As in the initial measurements,
staging includes startup/load/validation/commit, excludes fixture ingestion,
and performs one model load reused across all chunks. Pure insertion/commit and
verification remain separately measurable through the ignored SQLite-only probe.

Two simultaneous real-model jobs on **different disposable databases** also passed.
For one 4096-byte prose source/vector each, their apply times were
2066.01/2020.00 ms in the corrected run; exact raw observations remain in the logs.
Sampling both complete process trees observed **2,148,300 KiB summed RSS** and
**2,116,802 KiB PSS** at the peak sample, with 59 samples and a minimum 25 ms sleep
between samples. These include harness/setup, shared pages count twice in RSS,
and sampling can miss peaks. They are not maximum-memory or six-worker bounds.
Same-database serialization is tested separately with real processes and fixture
vectors. An initial RSS sampler followed only main-thread children and missed
Rust test-thread model children; that memory figure was explicitly discarded and
the sampler corrected before reporting these observations.

There is still no machine-wide model exclusion or hard process-RSS ceiling.
Per-attempt logical/work caps, cooperative 600-second checks and batch size one
do not prevent concurrent trusted processes from each allocating a full model.

### Final qualification

| Post-correction final-tree gate | Observed result |
| --- | --- |
| `cargo fmt --check`, `git diff --check`, documentation links | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --no-fail-fast` | **1216 passed, 0 failed, 17 ignored**, 67 target summaries, exit 0 |
| Release semantic input/storage/migration/cache units | 32 passed, 0 failed, 1 ignored |
| Nine relevant release integration suites | 181 passed, 0 failed, 7 ignored |
| Three additional release schema-15 migration/recovery repetitions | 15 passes total, 0 failures |
| Five process races repeated three times using the preserved final-source CLI | 15 passes total, 0 failures |
| Actual strict-model opt-in probes on the final release artifact | 3 passed, including real 2048 forward / 2049 refusal and owned asset replacement |
| Ordinary release production and no-embeddings builds | Passed without warnings |
| Actual final ordinary-binary smoke tests | 1 passed each; stage/replay/inspect and no-feature historical recovery checked |
| No-embeddings production library/binary Clippy, `-D warnings` | Passed |
| Both ordinary binaries scanned for test-only hooks | All 38 recognized fault/gate/fake markers absent |
| Isolated compiled mutations after a clean baseline | 3/3 killed at intended assertions: unit norm, asset SHA, raw-payload encoding binding |
| SQLite-only resource probe | Passed separately; synthetic vector fixture, no model claim |

The release integration suites cover chunks, chunking, token budget, lexical and
semantic administrative operations, embeddings, indexing, retrieval and M6
temporal behavior. Counts overlap the full suite and are not added to it. The
seventeen default-run ignored cases remain ignored; all new S3d.2 opt-in cases
were explicitly run, while unrelated older resource probes remain outside this
review's pass claims. The additional scalar corruption regression accounts for
the increase from the pre-correction 1215 to 1216 full-suite passes.

The final real-model rerun measured tokenizer setup 20.980 ms, preflight 0.172 ms,
model authentication/load 528.098 ms, four forwards totaling 707.662 ms
(176.916 ms mean), and serialization/verification 0.160 ms. Its three probes took
8.90 s with maximum child RSS 1,237,868 KiB. Repeated timings vary with concurrent
build/test activity and are not an optimization comparison. The final in-memory
SQLite-only probe observed serialization 222–232 µs, production insertion
886–919 µs, commit 4 µs, receipt decoding 241–260 µs, canonical revalidation
487–500 µs and stored-vector verification 55–60 µs. These retain the previously
stated fixture and disk/RSS limitations.

Final raw evidence is under `/tmp/m8-s3d2-review-*`: `scalar-before.log`,
`scalar-after.log`, `final-full.log`, `final-fmt.log`, `final-clippy.log`,
`final-release.log`, `final-release-units.log`, `final-repeats.log`,
`final-process-repeats.log`, `final-real-inference.log`, `final-storage-phases.log`,
`final-{production,no-embeddings}-{build,smoke}.log`,
`final-no-embeddings-clippy.log`, `final-hook-scan.json`,
`parallel-model-v2.json`, and `mutations.json` with its individual logs. Mutation
source and Cargo targets are independent of qualification; the three source
mutants were restored after execution. Compilation/infrastructure failures are
not mutation kills. No model assets or live user database were changed.

**Final decision: ACCEPT for S3d.2 isolated semantic staging and exact citations.**
Independent read-only reviews accept both scopes after the scalar correction.
The signed local implementation commit is authorized by the user's final-review
request after these gates. Its SHA is reported separately rather than embedded
in its own tree. No merge, push, document publication, active visibility transfer
or S4 is included. Stop for review after the signed local commit and clean-tree
verification.
