# M8 S3e.4c: plan legacy file observations before effects

**ACCEPT — bounded S3e.4c implementation and verification recovery. Publication
remains BLOCKED. Acceptance uses the explicitly pinned runtime described below.**

## Baseline and bounded scope

Verified clean worktree/index on `feat/m8-s3d2-semantic-staging`, signed HEAD
`47c8d3946a60e51e12389fb2aedeed13d7733018`, exact parent
`c00c7d1c1ab3706e976c23a4f24d35426c8c887a`, good GPG signature fingerprint
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.
Read [S3e classification/atomicity](m8-s3e-publication-design.md),
[S3e.1](m8-s3e1-writer-fencing-design.md) and
[architecture R01–R17](m8-s3e-architecture-decision.md).
Sol planning selects one private operation-local file-classification value built
inside the existing M6 IMMEDIATE transaction, before ordinary effects, then
consumed by the existing receiver on that same connection/transaction.

Current transactions already roll back atomically. This is not a claim of an
observed persistent partial-write defect. The plan covers file identity/observation
classification; apply still performs ordinary knowledge reconciliation. It grants
no ownership, alias admission or authority from path/root strings.

## Classification and effect inventory

| Phase / behavior | Existing reads / selection | Ordinary effects retained in apply |
| --- | --- | --- |
| Input/scope | Supplied root, Directory/File scope, observed timestamp, memberships and sorted files. Root is caller text, not a physical witness. | Session UUID/ingest_sessions and additive scope/membership upsert. |
| Full observation | File-kind logical_sources with current_location under root; latest source hashes. Classify present versus absent. | Per-file versions/observations; only unmatched residual present sources become absent. |
| Partial observation | present_logical_at, otherwise absent_logical_at and latest hash; path-local only. | No move inference or disappearance outside supplied file observations. |
| Unchanged/changed/new | Current known location/hash selects existing lineage or creation. | ensure_version, evidence/assertions/reconciliation/FTS, membership completion and observation row. New IDs allocated only during apply. |
| Continue/reappearance/move | Exactly one matching unaccounted old logical source and exactly one new path; copies/ambiguous matches remain separate. Full gone hashes can reuse captured fallible observations. | Reuse version, update current location/presence, moved event and observation. Intentionally no knowledge/membership reconciliation in Continue. |
| Same-location replacement | Partial absent source continues only on matching bytes; changed bytes create new lineage. Full existing location continuity retains existing rules. | Existing source identity/lifecycle policy, no new alias/equal-content authority. |
| Residual disappearance | Full-observation gone set minus inferred moves. | Mark absent and record event; disappearance does not retire memories. |
| Membership/retirement | Scope information is retained as input; application queries existing scope data after upsert. | Unchanged reingest completes memberships and recreates manually retired episodes; Continue does not gain this behavior. |
| Provenance/index/history | Existing helpers choose assertion/evidence provenance and lifecycle transitions. | Existing M6 history, FTS postings and immutable assertion-vector relationships; no new vector/index policy. |

The existing receiver inserted ingest_sessions and upserted scope before the
classification block. Classification itself reads logical_sources/sources, not
the newly upserted scope rows. Moving those ordinary effects after planning needs
no invented scope authority or separate database connection.

Preserve stable sort order and duplicate input-location handling. Do not deduplicate,
change ambiguous-move policy or merge equal-content identities. Existing source IDs
must remain exact; fresh UUIDs/timestamps are compared relationally rather than
blindly erased in compatibility evidence.

## Acceptance contract

- Build the private plan under actual begin_write after its tentative R insertion;
  no ordinary mutation SQL during planning and no additional R allocation.
- Consume it on the same connection/transaction without repeating identity/domain
  classification. Preserve existing session/UUID allocation order and healthy outputs.
- Propagate classification SQL failures; legitimate missing hashes remain None.
  Reuse fallibly captured hashes instead of latest_version_hash(...).ok().flatten().
- Deterministic actual-M6 tests cover pre-apply refusal, no durable effects/R on
  failure, full/partial/move/absence/reconciliation, queued writers and commit outcomes.
- Existing CLI/MCP/library entrypoints keep the same receiver and public contracts.
  No new plan persistence, schema, receipt, guard, denial, ownership or publication.
- Run requested fmt/Clippy/full suite, targeted debug/release tests, ordinary and
  genuine no-embeddings builds, stable behavior comparison and independent review.

Planned checks are not reported as passing evidence.

## Implemented boundary and healthy compatibility

`FileObservationPlan<'conn>` is private and owns the session draft, stable-sorted
observed files, requested spaces, per-file classifications and residual
disappearances. It borrows the original Connection. Application consumes that
value and obtains its connection from it; no alternate handle parameter exists.
There is no Serialize/Clone/replay or persistence API and no new public interface.

`record_observation_tx` now calls the read-only classifier, then the consuming
application helper. Existing public `record_observation` and combined
`record_files_ingest` still acquire the same real M6 `begin_write` and commit once.
The latter retains atomic file-plus-code persistence and its existing commit-time
callback. No new connection, nested transaction or independent classification
snapshot is introduced. The tentative revision insertion remains in existing M6.

The plan allocates no new UUIDs. Apply retains session-first then logical/version/
assertion allocation order. The per-file application arms compare byte-identical
to baseline apart from the loop's local name; subsequent version/reconciliation
helpers and public receiver signatures are unchanged. No changes to schema,
ordinary guards, caller authorization, M6 commit classification, scope rules,
FTS/vector policy or CLI/MCP contracts are included.

For move matching, every `gone` key came from captured `present`, so its previously
fallible hash observation can be reused directly. Initial lookup errors propagate
with `?`; optional missing versions retain None. Planning no longer issues the
redundant suppressed lookup. Residual disappearance is computed as gone minus
matched moves; partial observations naturally yield an empty map.

The plan is complete for the existing file classification, not for every future
publication mutation. Application still determines new logical IDs and performs
ordinary version/assertion/membership reconciliation. Caller roots/locations remain
strings and no passive witness becomes verified. No production pre-apply refusal
policy exists; refusal hooks used below compile only in tests.

## Deterministic tests and failure semantics

The new private test module uses actual M6 transactions and disposable populated
SQLite databases. The planner's no-mutation oracle is installed after M6's existing
tentative revision insertion. SQLite authorizer denial is SQLITE_AUTH, not an I/O
failure; a separate exact per-file adapter verifies SQLITE_IOERR_READ.

| Control | Required observation |
| --- | --- |
| Mutation-free planning | SQLite authorizer rejects all DML during planner; plan succeeds and ordinary state/total changes remain unchanged. Application positively attempts writes afterward. |
| Actual receiver boundaries | Both public file receivers complete planning before ordinary effects; test-only pre-apply refusal rolls back session/scope/data and the tentative R. |
| Failed first hash read | Denied sources.content_hash read is an explicit error, never missing evidence; no durable ordinary effects/R. |
| Captured hash reuse | Permit first hash observation, deny a second during planning: a genuine move still classifies Continued with exactly one hash observation. This challenges the old suppressed reread. |
| Optional/corrupt state | A valid logical source without a version remains optional; malformed lineage errors rather than changing classification. |
| Actual read I/O | Inject SQLITE_IOERR_READ through the existing connection/file handle; restore methods before rollback/close, assert engine error and unchanged durable state. |
| Full/partial/lifecycle | Existing ambiguity, copied content, same-location replacement, reappearance, residual absence and strict partial no-move/no-disappearance rules. |
| Membership/retirement | Continue does not reconcile; subsequent Unchanged adds memberships and recreates retired episodes without reopening old ones. |
| Positive vector/history | Preserve exact old embedding BLOB/key through changed/disappeared/reappeared input, assertion history and vector status; verify FTS, FK, schema17 and guard integrity. |
| Duplicate input | Preserve stable duplicate classifications, then retain existing UNIQUE rejection and complete rollback; no deduplication or widened policy. |
| Queued writer/BUSY | Authorizer BEGIN barrier and bounded channels prove the receiver classifies after a prior committed writer; held writer produces explicit BUSY without planning/durable effects. |
| Commit veto/UNKNOWN | Connection-local veto proves known rollback; lost acknowledgement runs in an exact-test subprocess to isolate the existing process-global test hook, proves durable commit and existing poisoned-write refusal. Missing ACK is not rollback. |

Channels and SQL barriers establish ordering; 5ms polling is used only for bounded
child-process lifecycle, not to widen races. UNKNOWN uses the same test as a child
with an explicit environment marker, not an ignored/unexercised helper endpoint.
Legacy ingestion still has no caller-operation durable receipt; this adds none.

## Initial stable production receiver comparison

An immutable archive of signed baseline
`47c8d3946a60e51e12389fb2aedeed13d7733018` was compiled separately with locked
offline dependencies. Ordinary baseline SHA256:
`6714b15a794b57fcc68039535d088571e314601e25e84fb5519ec4dd5b68a465`;
original S3e.4c ordinary binary SHA256:
`82ec7de831a4bce6854aef0d2a5cd10d8601df49b585227e1445b25b2dbc9ad8`.
Both compiler feature records show exactly default+embeddings, no spawn-gate.

A synthetic populated seed was cloned preserving all existing IDs into separate
baseline/candidate databases. Both binaries observed the same input paths at each
stage under isolated HOME/XDG/TMPDIR. Fifteen paired mutation stages passed:

1. Full unchanged observation.
2. Partial modification, then full after partial.
3. Full move preserving original lineage.
4. Ambiguous equal-content copies remaining separate.
5. Disappearance, partial same-content reappearance, then unchanged membership addition.
6. Explicit retirement followed by recreation through unchanged ingestion.
7. Another disappearance, different-content same-location replacement and rename-plus-edit.
8. Normal sandboxed MCP full and selected-directory ingestion.

These comprise 13 CLI and two MCP mutations. Four subsequent CLI read comparisons
also passed. After every stage, application/history/logical-FTS row graphs match,
FK/integrity checks pass, and each mutation consumes exactly one R. All successful
subprocess stderr outputs were empty.

The oracle preserves 24 seed IDs exactly; 55 fresh UUID pairs and 30 fresh timestamp
pairs are mapped bijectively, preserving timestamp equality/order. It does not
replace every UUID with a generic token. Compound encoded M6 entity keys are
compared through that same mapping. In-memory oracle controls rejected a wrong
FK-valid memory→assertion edge and UUID identity collapse; those controls mutated
no database and are not compiled-source mutation-test claims.

Limits: MCP elapsed_ms is excluded; physical FTS shadow BLOBs are not byte-compared.
Process fixtures have zero vector rows, so positive vector preservation comes from
the new native test instead. Isolated MCP is not exercised by this comparison;
existing broader suites provide their own explicitly bounded coverage.

Artifacts: `/tmp/mindspace-s3e4c-_qihc2my/comparison/run5/result.json`,
`oracle-controls.json`, per-stage logs, and transient `comparison/compare.py`.
For independent repetition, compile the exact signed baseline and current source,
seed and clone a disposable database with full/partial/move/copy/absence cases,
run the sequence above with explicit DBs, and compare existing IDs and relationally
mapped fresh graphs after each operation. The reusable repository regressions are
the new native module plus existing lifecycle/persistence/M6/CLI/MCP suites; the
transient comparison script is evidence rather than a parallel test framework.

## Recorded failures and probe corrections

The first native targeted run had 15 passes and one failed duplicate-input oracle:
`called Result::unwrap() on an Err value: Storage("UNIQUE constraint failed:
source_observations.session_id, source_observations.location")` at the original
test line736. The test incorrectly expected successful duplicate application.
It now asserts the existing constraint and full rollback; production was not
changed to permit duplicates. Original logs remain in `planning-targeted`;
`planning-targeted-2` passed all 16 after correction.

Transient comparison probes also stopped on incomplete oracle/fixture setup:
encoded compound M6 references were not initially mapped; human retirement times
needed the same timestamp bijection; sandboxed ingestion correctly refused an
input root overlapping database directories; a selected-file MCP request was
identically refused by both binaries. The final fixture uses sibling database
and input directories and a supported selected-directory request. Sandbox controls
were retained. Failure artifacts remain at `comparison/failure.json` and earlier
run directories; no production source changed to satisfy these probes.

## Independent adversarial review

Read-only Sol review found no remaining findings after inspecting hidden planning
mutations, captured lineage/hash safety, connection/transaction continuity,
application equivalence, ambiguity and duplicate policies, cleanup and M6 outcomes.
It independently identified the initial duplicate-location test-oracle mistake;
that test was corrected without relaxing production behavior.

The reviewer copied the compiled unit executable, SHA256
`9c7c617db4018a37557c440f4d834060cb5b46fd6b95741e2a9b981a36dc5371`,
and executed all 16 planning tests twice: **32 passed, zero failed/ignored**.
Source hashes stayed unchanged. Artifacts:
`/tmp/mindspace-s3e4c-review-o_ns08qd/result.json`. Broad-suite/build acceptance
remains separately recorded below.

## Initial invocation and verification

From the worktree:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --no-fail-fast
cargo test --locked --lib storage::sqlite::observation_planning_tests -- --test-threads=1 --nocapture
cargo test --locked --release --lib storage::sqlite::observation_planning_tests -- --test-threads=1 --nocapture
cargo test --locked --release --test lifecycle --test persist_atomicity --test m6_temporal -- --test-threads=1 --nocapture
cargo build --locked --bin mind --message-format=json
CARGO_TARGET_DIR=target/s3e4a-no-embeddings cargo build --locked --no-default-features --bin mind --message-format=json
```

Use isolated HOME/XDG/TMPDIR and explicit disposable DB paths. The self
dev-dependency can re-enable embeddings/test hooks for tests; genuine production
features are checked in Cargo compiler JSON, not inferred from command labels.
The no-embeddings target's old cache name is not a source revision claim.

Primary artifacts: `/tmp/mindspace-s3e4c-_qihc2my`. Runs additionally used offline
locked dependencies, two build jobs, development debug information disabled and
explicit Cargo/Rustup paths. Existing adversarial process supervision is reused,
with continuous adopted-zombie reaping that excludes Cargo's own wait status.
No user/global configuration was edited; fixture Git signing is disabled only in
the verification process environment. Rust/Cargo1.98.1, bundled SQLite3.53.2,
Linux7.2.14-zen; disposable fixtures use tmpfs and btrfs; build/source caches use btrfs.
These environments do not qualify every filesystem or power-loss recovery mode.

| Gate | Executed result |
| --- | --- |
| Formatting / Git whitespace | PASS. |
| Clippy all targets, warnings denied | PASS. |
| Corrected targeted debug planning | 16 passed, zero failed/ignored. |
| Optimized planning/concurrency | 16 passed, zero failed/ignored. |
| Existing optimized lifecycle/M6/persistence | 40 passed, zero failed/ignored: 16 lifecycle, 19 M6 temporal, 5 persistence. |
| Full locked suite | NOT PASS: first run 1,305 passed / 6 failed / 34 ignored; btrfs run 1,301 passed / 10 failed / 34 ignored. Exact failures and bounded retests below. |
| Ordinary production build | PASS; root library/binary default+embeddings, no spawn-gate. |
| Genuine no-embeddings build | PASS; root library/binary feature lists exactly `[]`. |
| Genuine no-embeddings runtime | Init/space/created/unchanged/search passed; one R per observation and one active hit. |
| Paired production comparison | PASS: 15 mutation stages, four CLI reads, logical/history/FTS graphs and integrity/accounting. Limits above. |
| Independent review/repetition | No source/test findings; 32 passed, zero failed/ignored. Report calibration reviewed separately. |

## Initial scope and remaining publication blockers

At initial delivery, only `src/storage/sqlite.rs`, new
`src/storage/sqlite/observation_planning_tests.rs` and this report change.
There is no schema, Cargo/dependency, core-format, Store interface, CLI/MCP,
retrieval, ownership or test-framework change. Application arms and subsequent
version/reconciliation helpers were compared against the signed source archive;
existing public Store receivers and M6 machinery remain unchanged.

This supplies a necessary file-classification boundary toward R06/R07, not a
complete protected mutation-domain classifier or active writer fence. The
[architecture R01–R17 matrix](m8-s3e-architecture-decision.md) remains authoritative:

- R01/R02: exact publication-private authorization, candidate/base and serialized
  validation remain absent.
- R03/R04/R15: old diagnostic disclosure, old active-knowledge access/Q10 and
  physical preparation/recovery/compatibility are unresolved. No read-relation
  rename or metadata mechanism is selected or installed.
- R05/R12: legacy read sessions remain implemented; future head/cohort/layout/
  eligibility and publication-bound canonical citation integration remain absent.
- R06/R07: actual affected-row/receiver guards, validated bounded source/alias
  closure, producer/absence domains and stale vector/lifecycle/membership paths
  still need joint qualification. Raw root/location strings are not authority;
  passive bindings remain unverified and nonexclusive.
- R08–R11: selected future-domain guard/accounting equivalence, complete projection/
  memberships/lifecycle, verified ordinary FTS, mandatory vector-route protection
  and applicable strict Q1 coverage remain gated. Existing private staging is not
  reclassified as failed.
- R13/R14: complete authorized atomic replacement/fence/index/head/receipt and
  exact durable publication recovery/replay remain absent. Existing legacy UNKNOWN
  cannot be resolved by a new receipt or refused retry.
- R16/R17: qualified complete resource profiles and source-only refresh/update/
  reversion before enabling those capabilities remain required.

Model A's trusted-local-account boundary is unchanged. Same-UID raw SQLite is
outside admission-respecting exclusion. A successful planner, revision, stable
comparison or test repetition proves neither old-reader exclusion nor physical
non-disclosure. No active guard, write-denial policy, alias admission, upgraded
witness, relocation, quarantine, persisted plan/receipt, ownership or publication
is introduced. No merge, push or S4. Changes remain uncommitted for review.

## Initial full-suite environment failures and controlled retest

The first complete full-suite command exited101: **1,305 passed, six failed,
34 ignored**. It is not a passing gate. Failures retained in `full-test`:

- `verify_file_detects_checksum_mismatch`: raw file write returned
  `Os { code: 122, kind: QuotaExceeded, message: "Disk quota exceeded" }`.
- `cli_enable_noninteractive_requires_yes`: `storage error: disk I/O error`.
- `an_extremely_large_blob_is_refused_without_being_read`: Git could not write a
  loose object, `Disk quota exceeded`.
- `a_file_over_the_default_limit_is_refused_by_the_directory_producer`:
  `Storage("disk I/O error")` during fixture initialization.
- `a_blob_exactly_at_the_limit_is_read_and_one_byte_more_fails_the_operation`:
  `CommitOutcomeUnknown { detail: "disk I/O error" }` during fixture initialization.
- `real_codex_registers_backs_up_keeps_foreign_servers_and_never_overwrites`:
  `/usr/bin/codex: error while loading shared libraries: libssl.so.4: cannot open
  shared object file: No such file or directory` (exit127).

`/tmp` has tmpfs user quotas enabled (`usrquota`); global free space did not imply
available per-user quota. The direct EDQUOT and Git diagnostics establish quota
pressure. With no source changes, the same models and record_stream targets under
isolated btrfs scratch passed **41 tests, zero failed/ignored**. This supports
classifying those five failures as environment/capacity failures, not a planning
regression. No unrelated files or old evidence were deleted to free space.

The Codex dependency is separate: isolated `/usr/bin/codex --version` independently
exits127 with the same missing-library message, recorded in
`external-client-check.json`. No global configuration, binary or system library
was modified, substituted or hidden from tests. Existing setup failed closed;
its real-client integration is not verified by the passing ingestion tests.

The second unchanged full command used HOME/XDG/TMPDIR in disposable
`target/s3e4c-verification/full-test` on btrfs with default parallelism and no
filtered targets. It exited101: **1,301 passed, ten failed, 34 ignored**.
`full-test-btrfs/summary.json` records the complete result. Eight seccomp fixtures
failed at UnixListener construction (`tests/seccomp_isolation.rs:560`) with
`Error { kind: InvalidInput, message: "path must be shorter than SUN_LEN" }`:

- `a_filter_that_cannot_be_installed_aborts_the_child_before_any_probe`
- `landlock_alone_fences_files_but_not_sockets`
- `seccomp_alone_stops_new_sockets_but_not_existing_ones_or_files`
- `the_parent_is_not_affected_by_the_childs_filters`
- `landlock_plus_seccomp_closes_both_and_normal_work_continues`
- `x32_and_32_bit_entry_points_are_killed_not_filtered_around`
- `the_verification_checks_the_policy_not_just_the_filter_mode`
- `the_unconfined_baseline_reaches_everything`

The remaining failures were the same external Codex loader failure and
`semantic_staging_admin::competing_operation_ids_cannot_replace_the_committed_artifact`:
`error: storage error: legacy enrollment refused: WAL without an existing
shared-memory file requires separate SQLite recovery`. That concurrent staging
refusal is an observed unresolved result, not demonstrated to be caused or fixed
by this change. Recovery/admission code was not modified or relaxed.

An unchanged targeted recheck used short btrfs scratch `/var/tmp/m4c-qp4`:

```sh
cargo test --locked --offline --test seccomp_isolation --test semantic_staging_admin -- --nocapture
```

It passed **34 tests, zero failed, two existing ignored** (10 seccomp and 24
staging passes), recorded in `short-btrfs-targeted`. This supports the path-length
diagnosis and records one successful staging retest; finite repetition does not
resolve the earlier concurrent refusal or establish absence of races. The targeted
results are not combined into a claimed passing full suite. No further full rerun
would remove the independently confirmed unavailable Codex dependency.

### Runtime coverage limits

The 34 full-suite ignored tests remain existing categories: six model/weights,
ten resource/heavy, four preserved-schema16-binary, twelve adversarial/native
actor, and two production-artifact staging tests. No new planning test is ignored.
In addition, the full run explicitly reported:

- `skipped: set MINDSPACE_E2E_REPO=<repo dir> to run against a real repository`
- `SKIPPED: no watchdog appeared` (twice)
- `UNKNOWN: embeddings not exercised (set M72D_MODEL_SRC to an installed models directory)`

Source inspection also found `models::candle_runtime_prefix_distinction` returns
without exercising real models when assets are absent; that runtime was not
qualified. These gaps are distinct from compile success, synthetic vector
preservation, and no-embeddings runtime evidence. No real model weights or live
Mindspace data were used.

### Initial acceptance interpretation

The implementation, deterministic planning regressions, release regressions,
stable production comparison and source review support the bounded planning
boundary. At initial delivery the status was **PARTIAL**, because the required full-suite gate
failed, a real-client dependency is unavailable, and the concurrent staging refusal
has only a successful bounded retest. There is no production-readiness or
publication approval. The three reviewed files were left uncommitted with an empty
index. Further work requires review of this evidence, not a silent relaxation of
recovery, sandbox or publication rules.

## Verification recovery: failure attribution and bounded remediation

This section extends, rather than replaces, the initial failed runs and review
evidence above. Recovery began at the same signed HEAD with exactly three files
and an empty index. The original observation-planning source and native tests
remain byte-identical to their reviewed hashes.

Recovery artifacts: `/tmp/m4cr-80leude6`; controlled native-close artifacts:
`/tmp/m4c-unlink-psms6ld2`. `prior-logs.json` hashes complete original stdout/stderr;
`ten-failures.json` binds each actual FAILED status to its executable and panic.
The expected caught cancellation panic is not counted as a failed test.

### Exact ten-failure inventory

All ten occurred in the original btrfs full run with default libtest parallelism,
two build jobs, isolated HOME/XDG/TMPDIR and an overlong scratch pathname. The
eight socket fixtures use distinct TempDirs; their common parent pathname is the
shared cause, not a shared socket or database. The staging test shares one unique
populated DB between six contenders. The setup test isolates configuration but
executes the installed system Codex.

| Failed test | Executable | Attribution / root signature |
| --- | --- | --- |
| `a_filter_that_cannot_be_installed_aborts_the_child_before_any_probe` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `landlock_alone_fences_files_but_not_sockets` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `seccomp_alone_stops_new_sockets_but_not_existing_ones_or_files` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `the_parent_is_not_affected_by_the_childs_filters` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `landlock_plus_seccomp_closes_both_and_normal_work_continues` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `x32_and_32_bit_entry_points_are_killed_not_filtered_around` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `the_verification_checks_the_policy_not_just_the_filter_mode` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `the_unconfined_baseline_reaches_everything` | `seccomp_isolation-eb6bf93b6c20e9dd` | Environmental prerequisite; `path must be shorter than SUN_LEN`; exact baseline reproduction. |
| `competing_operation_ids_cannot_replace_the_committed_artifact` | `semantic_staging_admin-46f2e3a16a5c637c` | Baseline administrative-open defect class reproduced; `WAL without an existing shared-memory file requires separate SQLite recovery`. Original interleaving remains unknown. |
| `real_codex_registers_backs_up_keeps_foreign_servers_and_never_overwrites` | `setup-d712a2ee2c0d7c43` | Environmental prerequisite; `/usr/bin/codex` loader missing `libssl.so.4`; exact baseline reproduction. |

No demonstrated introduced S3e.4c regression was identified among these ten.
This is a bounded attribution, not proof that every timing or storage failure is
harmless. The first run's five quota-related failures remain separately recorded
above; no host storage was deliberately exhausted or unrelated artifacts removed.

### Runtime dependency provenance and supported verification runtime

`/usr/bin/codex` resolves to `/usr/lib/openai-codex/bin/codex`, owned by
`openai-codex 0.167.1-1`. Its package manifest identifies
`0.167.1+arch.1`, `x86_64-unknown-linux-gnu`; package build date is Nov10 and
installation Nov11. ELF build ID is
`95f920737d15aa4388b51ae4c09e14921ae36524`; SHA256 is
`da92c374da753af9d15023f1e6ea3d5899fd7aeccd572c9b895096b6e9fa920a`.
Both `libssl.so.4` and `libcrypto.so.4` are NEEDED. Installed OpenSSL
`3.6.5-1.1` provides ABI3 instead. This is an incompatible installed runtime pair,
not a Mindspace linkage failure; the evidence does not assign blame to a package
maintainer or establish why those package versions were combined. Package ownership,
manifest, ELF linkage, hashes and baseline loader failure are independently recorded.

The already-installed static PIE runtime at
`/home/cnachtigall/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl/bin/codex`
reports `codex-cli 0.166.0`; package metadata identifies that version/target. SHA256:
`b8a11fedf24b2dfb59a70c9e52b030f9a508dcd49cd2dd0d41956f4c785156b8`.
It has no dynamic NEEDED entries. This is an explicitly pinned real runtime, not
a stub, library symlink, system downgrade or substituted test assertion. No packages,
system libraries, shell PATH or global Codex configuration were changed. The
system 0.167.1 binary remains unusable on this host. Verification qualifies the
pinned runtime and its exercised MCP configuration contract, not every Codex release.

The original real-client fixture reintroduced `/usr/bin:/bin` after selecting a
client from PATH. When the pinned runtime was selected, production correctly saw
two different Codex programs and refused `AMBIGUOUS`. The test now limits its
Codex PATH to the selected runtime directory and positively asserts production
detection finds exactly that canonical executable. Actual registration/readback,
backup, foreign-server preservation and overwrite-refusal assertions are unchanged.
Production ambiguity handling and dedicated ambiguity tests remain unchanged.

### Native root cause and baseline comparison

Locked bundled SQLite3.53.2 last-close checkpoints, calls `walIndexClose` /
`unixShmUnmap` to unlink SHM, then deletes WAL. Administrative recognition formerly
checked WAL existence and immediately refused missing SHM before SQLite locking.
The two filesystem observations can encounter legitimate last-close cleanup.

The controlled helper calls the real libc `unlink` first and pauses only after
successful unlink of the exact synthetic DB's SHM. Pipes positively order the
admin attempt and closer release; no SQLite sidecar is manufactured or deleted by
the parent. Captured unmodified signed-baseline and original S3e.4c ordinary
binaries reproduce the exact refusal three times each with a `space list` closer
(empty WAL), plus once each with a real `space create` writer (28,872-byte WAL).
Releasing close allows the same preview intent to succeed. Refusal leaves captured
storage state unchanged; writer cases commit exactly R8→9, ownership remains empty,
and integrity checks pass. These observations prove the baseline availability
defect class; the original failed six-writer fixture and per-child schedule cannot
be reconstructed from its discarded TempDir. No claim of exact retrospective
attribution or absence of other interleavings follows.

Helper source SHA256:
`72079c75fc2a06908953a0f689cb1083be5fc254258f78b40ac4815eb6c454eb`;
helper binary SHA256:
`0c623a183978b2753cfb57fb44d7bb8b5a158976e6c64797ad93d30ff3a14370`.
Read probe SHA256:
`700bba624776bed7d483f18d8a998552bc1a67fdf19a32cd6b4401e6f776ef30`;
writer probe SHA256:
`8d03c3318c30264fd99316bfc7dd2dd8bc5e294307c264c2028d1926926a4cd4`.
The retained README documents clean-directory reproduction. Initial sandbox-remapped
ancestry refusal is preserved separately; successful probes used the actual host
filesystem without disabling ancestry checks. Python SQLite3.54.1 performs only
fixture inspection; the actors use bundled SQLite3.53.2.

Separate disposable clones check out exact signed baseline HEAD, leaving current
uncommitted files intact. The first long-path baseline target reproduced all
eight socket failures (2 passed / 8 failed). An isolated baseline real-Codex case
reproduced the loader failure (0 passed / 1 failed). The first full baseline clone
under `/var/tmp` recorded 1,288 passed / 7 failed / 34 ignored: six trust fixtures
correctly rejected world-writable `/var/tmp` ancestry and one refused alternate-
runtime ambiguity. Those are additional verification-environment failures, not
concealed passes. The corrected baseline clone is under private trusted cache
ancestry. No policy, test expectation or signed-baseline source was changed.

### Narrow remediation and regression boundaries

- Private `Recognition::{Ready, MissingShm}` distinguishes definite SHM metadata
  NotFound from SQL, identity, journal, permission and nonregular-file errors.
- Only MissingShm retries, with a five-second deadline and 5ms backoff. Every
  attempt repeats the existing context/journal/recognition protocol, including
  the post-immutable-read WAL-appearance check. Missing SHM never triggers a
  SQLite open, recovery, SHM creation or read/write admission. Stable absence
  expires with the original refusal. This is a bounded polling policy, not a
  hard real-time scheduling guarantee.
- Three reusable real-CLI tests cover successful exact-intent retry with one R,
  stable refusal with unchanged captured storage, and identity/journal/nonregular/
  permission hazards while waiting. Their zero-byte WAL transition is explicitly
  synthetic and quiescent; it is renamed while the child is paused before any
  SQLite open. Native unlink evidence separately establishes actual teardown.
- The six-contender staging test collects every child wait result before any
  result assertion, retains the failed fixture before writing individual logs,
  then preserves the existing success, artifact uniqueness, receipt and R checks.
  This repairs teardown after a failed child status. It does not establish robust
  cleanup for every existing spawn/pipe/wait error elsewhere in the suite.
- The real-Codex PATH change is test-only; no production client discovery changes.

The retry fixes this administrative availability window; it does not qualify the
physical publication cutover protocol. Existing native read-only SHM effects,
MAIN/WAL observation races outside this narrow state, hot-journal hazards, old
open readers and same-UID raw SQLite boundaries remain. Measured state includes
content, inode/device, size, mode, UID/GID and mtime/ctime, not atime. Do not infer
universal absence of filesystem effects from those snapshots.

### Recovery verification results

- Unchanged private-cache baseline full suite: **1,294 passed / 1 failed / 34 ignored**.
  Remaining failure is the real-Codex fixture's alternate-runtime ambiguity.
  The concurrent staging case passed in this finite run; this does not negate
  its demonstrated baseline counterexample.
- Changed-source targeted debug: **475 passed / 0 failed / 7 existing ignored**.
  Includes 362 unit, 26 ownership, 10 socket, 24 semantic staging and 53 setup passes.
- Post-fix native close probes: **2/2 PASS**, using copied spawn-gate test driver
  SHA256 `ad07483c90ff1670e206cc654960383990404be4f393a420a3f17de9c46110dd`.
  Positive missing-SHM marker precedes real closer release and successful admin
  completion. Without that pause hook, stable native missing SHM refuses after
  **5.007 seconds**, storage unchanged; identical intent succeeds after close.
  Both actual writers commit exactly R8→9, ownership stays empty and integrity passes.
  This is test-driver evidence, not ordinary-build feature provenance.
- Changed-source controlled full suite: **1,314 passed / 0 failed / 34 existing ignored**, exit0.
- Clippy `--locked --offline --all-targets -- -D warnings`: **PASS**.
- Release ownership/staging/socket/setup: **113 passed / 0 failed / 2 existing ignored**.
- Release observation planning: **16 passed / 0 failed / 0 ignored**.
- Both final ordinary production builds: **PASS**. Compiler JSON verifies library
  and binary features exactly `[default, embeddings]` and `[]`, respectively;
  neither has spawn-gate. Their native deadline/retry checks also passed below.

Native results: `postfix-host-run/result.json` under the native artifact root;
probe SHA256 `8868a9d5a706c86abfc3705b5a1d88a9ad1b9a8fdc907de71de27ae99202c795`.
The write-capable administrative opener is repaired narrowly; the passive read
opener is unchanged. No mutation or UNKNOWN commit is retried by this loop.

### Recovery scope

Seven files now comprise the combined uncommitted change:

1. `src/storage/sqlite.rs` — original reviewed observation plan, unchanged.
2. `src/storage/sqlite/observation_planning_tests.rs` — original 16 tests, unchanged.
3. `src/storage/sqlite/ownership.rs` — bounded typed preflight retry.
4. `tests/ownership_admin.rs` — three deterministic recovery regressions.
5. `tests/semantic_staging_admin.rs` — wait before assertion and retain failure evidence.
6. `tests/setup.rs` — isolate and verify the selected real Codex runtime.
7. This report — original evidence preserved and recovery findings appended.

No migration, schema, dependencies, active fencing, ownership policy, publication,
retrieval/MCP contract or new feature is introduced. Existing R01–R17 publication
blockers above remain. All changes are uncommitted; no merge, push or S4.

### Independent recovery review and negative control

Read-only Sol review found no confirmed production or assertion-suppression
findings. It verified the original two source/test hashes and the unchanged
registration/staging assertions. Runtime gates are recorded above and final evidence review is recorded below.

An independent negative control executed the copied current semantic integration
executable and real copied test driver. A declared transient wrapper injected
exit42 into exactly one of the six staging contenders; the other five executed
the real driver and exited0. The test correctly exited101 rather than accepting
the failed contender. All six PID/starttime identities were gone afterward, and
the DB plus all 18 child stdout/stderr/status logs were retained. This is a
successful test-oracle control, not a passing normal staging run or a Codex stub.
Spawn/OS-wait failures were not simulated.

Evidence: `/tmp/m4cr-teardown-review-o7s8k0i_/result.json`; wrapper SHA256
`0c176c726f069c4284ffd5d0bc2e0c8341c3e1ba945656df872287e198980e3c`;
probe SHA256 `5f28f41200bc0e03a37ae60ed8c541213be7b7f6520500d8510cadc0890a189f`.

### Ordinary-build runtime and final environment

Final ordinary default binary SHA256:
`1cf01510798d7b8aae48d361d38d80b656950fa6c10cdc517b4c277e172eca79`;
no-embeddings binary SHA256:
`95ca7d7b02e83e392d9e2efa23d5769b62e36a3b33bbb571fa4003484b8b7585`.
These are distinct from the initial comparison and spawn-gate drivers above.
`production-features.json` in
`/home/cnachtigall/.cache/m4cr-80leude6/final-evidence` binds compiler feature
records to both copied executable hashes.

Both ordinary variants executed the nonempty-WAL native close control without
administrative pause hooks. Each refused at the bounded deadline without changing
captured storage, then succeeded with identical preview intent after real closer
release. Writer R increases exactly once and integrity checks pass. This verifies
ordinary native runtime behavior without claiming all journal or publication
recovery modes. Artifacts: `ordinary-default/result.json` and
`ordinary-no-embeddings/result.json` in that final evidence directory.

All recovery Cargo runs use locked offline dependencies, two build jobs,
Rust/Cargo1.98.1, development debug information disabled and the same dependency
cache. Full suites run one at a time; default libtest parallelism is retained
(`RUST_TEST_THREADS` unset; 16 available CPUs). Short private btrfs fixture roots
avoid tmpfs user quotas, Unix socket pathname limits and untrusted ancestry.
No cgroup, seccomp, filesystem trust or client ambiguity requirement was relaxed.
HOME/XDG/TMPDIR and client configuration remain isolated; no live Mindspace data
or credentials enter fixtures.

The final binary archival attempt into `/tmp` again produced
`OSError: [Errno 122] Disk quota exceeded` during `shutil.copyfile` of the
no-embeddings executable. It occurred after all Cargo gates had passed and before
the ordinary runtime probes started. The partial copy was retained; remaining
copies/probes use private btrfs storage. No source/test changed or result was
counted as passing because of this failure. The quota also prevented a subsequent
normal tool sandbox from creating `/tmp/.git` (`Disk quota exceeded`); final
scoped checks use the authorized host runner, without changing sandbox policy.
No files were deleted to regain quota. Final Git signature verification also
needed private btrfs TMPDIR after its detached-signature temporary file hit the
same quota; the unchanged signed HEAD then verified successfully.
`archive-quota-failure.json` retains these artifact/verification-stage failures
separately from test results.

The full run retains the same 34 ignored tests and the same explicit real-repo,
watchdog and embedding runtime gaps listed above. The silent real-model early
return is still unqualified. These are limitations of the bounded acceptance,
not completed runtime coverage or publication approval. The broken system GNU
Codex remains a host prerequisite for anyone choosing that binary; this gate
instead exercises the pinned actual static runtime, with no environmental waiver.

### Independent recovery recheck and final acceptance

The reviewer directly reran the `missing_shm` filter without Cargo: **4 passed,
0 failed, 0 ignored** (all three new regressions plus the existing foreign-WAL
case). Test executable and driver hashes matched before/after. Evidence:
`/tmp/m4cr-recovery-check-xfa8ng2n/postcheck.json`. Its transient runner initially
expected only three matches and failed its own postcondition after Rust exited0;
exact-name validation corrected that count without rerunning tests, editing source
or relaxing assertions. The original raw result remains retained.

All required recovery gates now pass under the recorded environment. Status is
**ACCEPT for this bounded implementation and verification recovery**; publication
remains **BLOCKED**. Exact original six-child timing remains unrecoverable; neither
finite passing runs nor this retry prove absence of other SQLite/admission races.
No quarantine, new schema, active fencing or ownership/publication authority is
selected or activated. The original observation-planning source/tests are unchanged;
all seven combined files remain uncommitted with an empty index for review.

Final independent report/diff review: **no remaining findings**. The reviewer
checked exact artifact counts, ordinary feature/hash provenance, baseline/source
identity, seven-file scope, empty index, links and bounded acceptance wording.
No source edits or repeated suites were performed during that final review.

To repeat the full gate on this host from the worktree, use a fresh short private
btrfs root and the retained supervisor (which reuses repository process helpers):

```sh
verification_root=$(mktemp -d "$HOME/.cache/m4cr-repeat.XXXXXX")
cp /home/cnachtigall/.cache/m4cr-80leude6/final-evidence/check.py "$verification_root/check.py"
python3 "$verification_root/check.py" --name full --scratch-root "$verification_root/fixture" --runtime-path /home/cnachtigall/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl/bin --timeout 1800 -- cargo test --locked --offline --no-fail-fast -- --nocapture
```

The supervisor records argv/environment/status, clears unrelated runtime variables,
isolates configuration, bounds execution and reaps adopted children. Its path and
Cargo/Rustup cache paths are host-specific. Do not reuse an existing run directory,
put the checkout below world-writable ancestry for trust fixtures, or interpret the
system GNU Codex as repaired. The native helper/probe sources and pinned hashes
above provide separate close-order reproduction, including known failures.
