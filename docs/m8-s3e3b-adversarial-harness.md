# M8 S3e.3b: reproducible old-client / SQLite adversarial harness

**ACCEPT for this evidence-building slice. Publication remains BLOCKED.**
Passing a known-unsafe characterization means the unsafe or unresolved behavior
was reproduced. It does not mean that behavior is secure, accepted or fixed.
Historical Decision B remains: the diagnostic exception was rejected.

## Baseline and scope

Verified before implementation: clean index/worktree on
`feat/m8-s3d2-semantic-staging`, good OpenPGP signature on HEAD
`91c70b933a2fd769316e2c9b26e52df99d018c00`, worktree
`/home/cnachtigall/git/projects/mindspace-wt/m8-s3d2`.
Read with [S3e](m8-s3e-publication-design.md),
[S3e.1](m8-s3e1-writer-fencing-design.md),
[S3e.2](m8-s3e2-fencing-infrastructure.md) and the
[reconstructed S3e.3](m8-s3e3-old-reader-exclusion-design.md).
Historical reports in those documents are distinct from the new executions here.

Changes are confined to this report and four new test/runner files. Existing
tests, their preserved-binary hash check, production code, Cargo manifest/lockfile
and migrations are unchanged. No metadata quarantine, migration 0018, active
fence, owner transfer, retrieval change, new MCP endpoint or publication exists.
No merge, push or S4. Changes remain uncommitted. Website compliance was assessed
as inapplicable to these local CLI/stdio MCP fixture tests; no website surface or
legal compliance claim is introduced.

## Implementation and invocation

The executable [runner](../scripts/m8-s3e3b-harness.py) builds immutable Git
archives and invokes two opt-in Cargo integration targets:
[old-client processes](../tests/old_client_adversarial.rs) and
[native SQLite](../tests/sqlite_adversarial.rs). Their private
[helper](../tests/support/adversarial_common.rs) is used only by these new targets.
This reuses Cargo, bundled rusqlite, tempfile, existing MCP/process patterns,
row comparison and existing spawn-gate migration hooks. The older process helpers
are private to their integration target and depend on a preserved executable's
exact hash. New targets permit separately authenticated source rebuilds and
artifact collection without weakening that existing test contract. No parallel
test framework or production helper binary is added.

From the worktree, with Rust/Cargo, Python 3.12+, Git, Linux `/proc`, `stat` and
the locked dependencies already cached:

```bash
scripts/m8-s3e3b-harness.py \
  --output /tmp/m8-s3e3b-run-001 \
  --target-dir /tmp/m8-s3e3b-build-cache \
  --repeat 3
```

The output directory must not exist. Builds use `--locked --offline`; missing
dependencies or build errors stop the run, never become a scenario PASS. Each
binary role has a separate Cargo target directory. Every copied binary must
actually initialize its expected schema in a fresh synthetic DB before tests.
The runner records source revisions, lockfile/overlay/script/binary hashes,
toolchain, controlled build inputs, build commands and schema smoke results in
`manifest.json`. Test results, commands, deadlines, filesystem types and artifact
paths are in `summary.json`. Raw stdout/stderr and per-operation JSON snapshots
are retained under `runs/`. Native tests record their bundled SQLite version.
The Python SQLite version used only for binary schema smoke reads is labelled
separately. This is source/build provenance, not bit-identical reproducible-build
or hostile-toolchain proof; trusted local Cargo configuration remains an input.

Rerun one case independently, using the recorded binaries after rechecking all
source/script and binary hashes:

```bash
scripts/m8-s3e3b-harness.py \
  --output /tmp/m8-s3e3b-hot-journal-001 \
  --reuse-build /tmp/m8-s3e3b-run-001/manifest.json \
  --case n04 --repeat 1
```

`--case` selects a scenario-name substring. The runner excludes `native_actor`,
the ignored integration-test subprocess protocol endpoint; it is not a passing
scenario. Ordinary Cargo test runs leave these expensive/environment-dependent
cases explicitly ignored. Test builds and the migration driver contain existing
spawn-gate support; the copied production old/current binaries are separate.

Every scenario creates a new temporary DB and synthetic input, with explicit
absolute `--db`, cleared test/subprocess environments and isolated HOME/XDG/TMPDIR.
The runner accepts filesystem roots, never an existing database path. These
tests do not discover or open developer Mindspace data. IPC acknowledgements,
migration COMMIT gates, `waitpid(WUNTRACED)` and native lock probes establish
relevant states. Polling waits have deadlines; a queued request's 150 ms no-reply
observation does not identify its exact blocked instruction. Per-child waits are
bounded (45 seconds in the shared helper); an outer 180-second scenario deadline
also covers the test process. Builds have 900-second deadlines.

Owned children use distinct process groups. Rust guards terminate/reap live
children; the Linux runner is a subreaper and also cleans owned orphan descendants
on normal exit, timeout and interruption. Passing fixtures are removed.
Assertion/unwind failures preserve fixtures and record their paths in `result.json`. An outer timeout or handled interruption
can kill the integration process before destructors run; in those cases the
fixture path is available in the already written `environment.json`, while a
per-case `result.json` may be absent. Logs/snapshots persist on either outcome.
Controlled supervisor fault checks verified separately grouped descendant cleanup after normal parent exit and forced timeout. These finite
checks do not prove all possible process races are absent.

## New execution evidence

Primary run: `/tmp/mindspace-s3e3b-4q_xf6yj/final-run`, with
`manifest.json`, `summary.json` and sibling `final-run.log`.
Independent reviewer rerun:
`/tmp/mindspace-s3e3b-review-independent-final`, with its own summary/artifacts.
These are local disposable evidence paths, not committed release artifacts.

Pinned historical source: `254d1ad5794db8ac19dc187a5934910337da9b9e` (schema16).
Pinned current source: `91c70b933a2fd769316e2c9b26e52df99d018c00` (schema17).
The previously preserved executable is absent at its recorded path. The new old
binary is a source rebuild, not that historical executable. No assertion of
binary identity with the lost historical experiment is made.

| Role | SHA-256 in primary manifest | Synthetic initialization |
| --- | --- | --- |
| Historical production | `7f63cf40fcb27588e7753a82cfc104b79a0e26a1ce558c8cb1e7770a38e2bb6c` | Schema16 |
| Current production | `8d21a26731dddeffd45c3933b1415ebd6af9f5d0cdca7ff64eedeceb5b23a617` | Schema17 |
| Current spawn-gate driver | `e7fa901b87b0029579fd9d9957b12cbefa429385a59310dc0a375d3073cfe490` | Schema17 |

Production old/current binaries lack the migration-pause hook literal; the driver
is built explicitly with `spawn-gate`. Executed with Rust/Cargo 1.98.1, bundled
SQLite **3.53.2**, kernel `7.2.14-zen1-1.1-zen`, debug profile with debug info off.
The exact toolchain versions and compiler controls are retained in the manifest.
These are this run's environment observations, not minimum supported versions.

Primary result: **99 PASS, 0 FAIL, 0 SKIPPED**: 11 scenarios × 3 repetitions ×
3 execution roots. Of these, 36 executions are regression invariants and 63 are
known-unsafe characterizations. Independent rerun: **33 PASS, 0 FAIL, 0 SKIPPED**:
all 11 scenarios on the same three root categories once, with fresh fixtures.
Repetition establishes observed reproducibility only.

| Root category | Observed filesystem | Primary executions |
| --- | --- | --- |
| Portable local baseline under the output directory | tmpfs | 33 |
| Explicit `/tmp` tmpfs selection | tmpfs | 33 |
| Worktree `target/s3e3b-fixtures` | btrfs | 33 |

Baseline and explicit tmpfs share a filesystem type; this is **two filesystem
types, not three independent environments**. The current implementation requires
Linux; “portable baseline” means local Linux filesystems rather than cross-OS
support. Other kernels, mount options, read-only mounts and network filesystems
are not qualified. Mount options are not automatically captured. Native lease
capability is probed per fixture: known unsupported/permission errno cases are
SKIPPED with errno/reason; unexplained contention after a successful clean
baseline fails. No such skip occurred in the primary or reviewer runs.

A separate skip-path check deliberately supplied mismatched filesystem roots:
one baseline scenario passed and the tmpfs/btrfs selections were **SKIPPED** with
their actual observed types. It verifies classification, not filesystem absence
on this host. Evidence: `/tmp/mindspace-s3e3b-4q_xf6yj/skip-check/summary.json`.

## Scenario evidence matrix

Each row passed all nine primary executions and three independent reruns.
`P` names refer to functions in the process target; `N` names match native tests.

| ID / scenario | Category | Required observation / expected unsafe condition |
| --- | --- | --- |
| P01 `old_idle_connection_survives_upgrade_while_fresh_store_rejects` | Known unsafe | An admitted old MCP handle retains reads and ordinary writes after16→17 and a successful fixture TRUNCATE checkpoint; a fresh old Store rejects17. Complete ordinary rows survive migration; exact M6 accounting adds only the migration revision/marker. Rejection does not imply zero filesystem effects. |
| P02 `old_paused_process_resumes_after_schema_migration` | Known unsafe | Stop acknowledged by `waitpid`, migration completes, SIGCONT resumes old reads/writes. The stopped client was idle; no instruction within a logical query is claimed. |
| P03 `queued_old_writer_resumes_after_held_migration_commit` | Known unsafe | Acknowledged held COMMIT and native BUSY probe; submitted old write has no reply before release, then commits and is searchable. The precise old wait location is unknown. |
| P04 `concurrent_migrators_preserve_ordinary_rows_and_single_schema17_inventory` | Regression | Two overlapping migrator process lifetimes converge to exactly versions1..17, empty passive tables, one new accounting revision/marker, preserved old rows and no FK violation. The second process's exact SQLite wait instruction is not proved. |
| P05 `controlled_migration_crash_rolls_back_then_restart_preserves_old_client` | Regression | Kill only the fixture migrator at its acknowledged precommit gate; schema16, ordinary rows and accounting remain intact, with no partial passive schema. Restart reaches17 and old handle remains usable. This is not arbitrary crash/lost-ACK recovery coverage. |
| P06 `old_doctor_discloses_model_setting_after_incompatibility_without_observed_content_canaries` | Known unsafe | Text/JSON × configured embeddings on/off: incompatibility is reported and the arbitrary synthetic model marker is still emitted. Positive controls verify stored active memory/document source canaries; neither is observed in these outputs. Ordinary rows remain equal; file-family effects are captured separately. |
| N01 `n01_read_only_wal_creates_shm_without_application_write` | Known unsafe | Genuine committed WAL remains after fixture writer termination. Native READ_ONLY changes existing SHM observations and reconstructs deliberately missing SHM; MAIN/WAL bytes remain identical in the reconstruction phase. Missing SHM is removed only with all fixture connections closed. |
| N02 `n02_delete_read_only_and_immutable_wal_visibility` | Regression | DELETE read-only preserves MAIN and creates no WAL/SHM/journal. Immutable MAIN sees the earlier committed marker while native WAL reader sees the later committed marker. Separate snapshots attribute effects to each opener. This protects the distinction, not immutable-reader freshness. |
| N03 `n03_snapshot_retention_prepared_steps_and_stale_write` | Regression | Held read transaction retains old committed values and WAL; TRUNCATE reports busy, stale write returns `SQLITE_BUSY_SNAPSHOT`. After release checkpoint succeeds. Preparation alone and a partly stepped SELECT have different observed snapshot boundaries. |
| N04 `n04_hot_journal_diagnostic_exposure_and_native_recovery` | Known unsafe | Actual DELETE transaction updates metadata, `cache_flush()` acknowledges spilled uncommitted MAIN, then only its fixture writer is killed. SQLite-written journal header remains. Before normal recovery, old doctor discloses that marker; native RO refuses with `SQLITE_READONLY_ROLLBACK` (776); RW recovery restores the committed marker and `integrity_check=ok`. |
| N05 `n05_inode_drain_open_connection_states_release_and_retry` | Known unsafe | Clean inode lease succeeds; acknowledged idle/snapshot/prepared/partly stepped native connections each produce EAGAIN11; connection closure permits retry. Lease is immediately released; raw same-UID SQLite then reads normally. It proves retained-connection behavior, not prepared-statement causality or enduring exclusion. |

N01 captured existing SHM hash and mtime changes on both tested filesystem types;
it also asserts missing-SHM creation. Metadata snapshots include dev/inode, size,
permissions, UID/GID, byte hash, mtime/ctime and atime measured before hashing.
Hash reads can themselves affect atime. Captures precede subsequent native opens
or cleanup that might hide an effect. Absence of application-data writes is
explicitly distinct from absence of filesystem effects.

P06 yields 36 primary diagnostic invocations, plus nine N04 invocations: **45**
new primary old-doctor observations, separate from the historical report's 30.
Observed output categories are schema/support versions, operational diagnostics,
and arbitrary synthetic model-setting text (potentially sensitive by fixture
definition). Incompatibility/exit1 does not suppress the setting. No real secret
was supplied. Lack of memory/document canaries in these exact outputs does not
prove universal non-disclosure or exclusion through other old APIs. The diagnostic
inspector reads settings before reporting incompatibility; the report then emits
later diagnostic output. No accepted diagnostic exception follows.

N04 is a reachable SQLite-generated crash fixture, rather than fabricated journal
bytes. It is still a minimal synthetic layout, not a populated Mindspace recovery
or migration proof. The observation concerns immutable MAIN inspection bypassing
normal recovery; it is not evidence that ordinary isolated SQLite reads expose
another transaction's uncommitted values.

## Historical findings: newly reproduced versus not exercised

| Reconstructed finding | New evidence / limit |
| --- | --- |
| H01 idle/paused old clients survive | P01/P02 reproduced with rebuilt historical production binary. |
| H02 queued legacy writers resume | P03 reproduced within the explicitly stated scheduling limit. |
| H03 checkpoint/version check does not evict | P01 reproduced; N03 separately measures held native snapshots. |
| H04 memory-relation rename blocks some paths but leaves others | **Not exercised.** No rename/future-layout prototype or complete old API inventory is delivered. |
| H05 diagnostic model-setting disclosure | P06 reproduced for synthetic Unicode marker, both output formats and configured enabled/disabled. Historical huge/control/bidi payload variants were not repeated. No no-embeddings build was run. |
| H06 WAL/SHM effects | N01 reproduced creation and captured modification; N02 distinguishes immutable/native views. |
| H07 hot-journal metadata exposure | N04 reproduced with a documented genuine cache-flushed fixture and explicit recovery controls. It does not recover the original historical fixture. |
| H08 snapshots/prepared handles obstruct drain until closure | N05 reproduced connection-lifetime obstruction across those native states; N03 distinguishes snapshot/statement behavior. Not evidence that a statement alone causes the obstruction. |
| H09 raw same-UID access outside guarantee | N05 raw reopen/read observed after lease release. This is outside Model A's proposed admission-respecting Store guarantee. |
| H10 tmpfs/btrfs experiments | Both types newly exercised; historical counts/conditions are not adopted as new evidence. |

Actual old binaries cover admitted idle/stopped/queued Store behavior. Held SQL
transactions/prepared statements use a native actor linked to bundled SQLite,
not an instrumented historical Store binary. There is no proof here about every
old endpoint, an old query paused between Hybrid branches, lease-break handling,
mapped/aliased/replaced inodes, or a lease-to-supported-SQLite handoff. A successful
drain witness is neither a durable admission rule nor knowledge/mutation isolation.
Model A still trusts the DB-owning local account and processes; arbitrary raw
same-UID SQLite is outside the guaranteed exclusion model.

## Verification and independent review

| Gate | Executed result |
| --- | --- |
| New harness | Primary 99 and independent 33 PASS; no failure/skip in those runs. |
| `cargo fmt --check` | Passed. |
| `ruff check --no-cache` and `ruff format --no-cache --check` on runner | Passed; Python syntax parsed successfully. |
| `cargo clippy --locked --offline --test old_client_adversarial --test sqlite_adversarial -- -D warnings` | Passed. |
| `cargo test --locked --offline --test doctor --test fence_plan_admin --test fence_plan_compatibility` | **64 passed, 0 failed, 5 ignored**: doctor 37; passive admin 26+1 ignored; compatibility 1+4 ignored. |
| Supervisor fault checks | Parent and independent reviewer each verified controlled normal-exit and timeout cleanup of separately grouped fixture descendants. |
| Documentation/scope | Passed: 66 Markdown files linked, new-file whitespace/structure and classification checks, exactly five untracked files, empty index and unchanged HEAD. |

The four existing ignored compatibility cases require the separately preserved
binary's exact hash; it was not bypassed for rebuilt binaries. The ignored admin
case is a performance measurement. No full/unrelated Rust suite was run because
production code and preexisting shared infrastructure were unchanged. Neither
real model inference nor a no-embeddings production build is claimed.

Independent review found and corrected:

| Severity | Finding | Correction / evidence |
| --- | --- | --- |
| HIGH | Shared Cargo output could label stale schema16 binary as current on a cached build. | Separate role targets plus actual per-binary16/17 smoke gates. Reviewer independently demonstrated the original mismatch; pre-correction runs are invalid acceptance evidence. Primary/reviewer runs authenticate distinct correct artifacts. |
| MEDIUM | Inherited Mindspace hooks could contaminate in-process fixture Store operations. | Test process and subprocess environments isolated; only explicit fixture/harness settings enter. |
| MEDIUM | Unrecorded inherited compiler flags/profile settings weakened declared provenance. | Build environment allowlist, explicit recorded wrapper/flags/jobs/profile controls and toolchain versions. Trusted local Cargo configuration is still a stated input. |
| LOW | Teardown could signal a reaped PID/group identity. | Do not signal reaped child identities; outer subreaper handles owned descendants. Controlled normal/timeout checks rerun independently. |
| LOW | Immutable/native side effects lacked a separating capture. | Dedicated post-immutable/pre-native fingerprint. |
| LOW | A row-comparison comment overclaimed separate M6 accounting. | Exact accounting assertions/artifacts added for isolated migration/crash/refusal phases. |
| LOW | The report implied `result.json` always survives failure, including hard-terminated test processes. | Qualified unwind failure versus outer termination; existing environment evidence carries the fixture path when destructor output is absent. |

Earlier compile failures included an unsupported SHA digest formatting operation
and then an incorrect utility import; both were corrected in the new helper.
The initial environment's `sccache` failed with `Operation not permitted (os
error 1)`; scoped builds explicitly disable that wrapper and record the override.
The corrected intermediate build rejected changed overlay hashes rather than
silently reusing them. Earlier logs/fixtures are retained under the parent evidence
directory, but **only final-run and the independent final rerun count here**.

Independent source/correction review found no remaining confirmed source issue;
the independent final runtime rerun passed 33 cases. Final report review found only
the failure-artifact wording issue above, now corrected. Documentation and scope
checks passed. All 132 primary/reviewer passing fixture paths were absent after
teardown, and no harness binary process remained at inspection. Only normal-exit and timeout descendant cleanup were separately exercised.
Handled SIGINT/SIGTERM cleanup, supervisor SIGKILL and power loss were not
separately tested; no universal process-lifecycle guarantee is claimed.

## Two-key metadata quarantine: remaining questions and next slice

No quarantine solution is accepted or implemented. Before considering relocation
of `embedding.enabled` and `embedding.model`, resolve the following in a separately
reviewed design/prototype:

- How existing missing/valid/arbitrary values migrate without altering enabled,
  model, M6 history or supported retrieval semantics; what old diagnostic output
  may contain under Decision B.
- Complete settings reader/writer/cache routing, including already-open supported
  clients, old queued setters, Store metadata, manager, indexer, diagnostics and
  setup; no dual authority or fallback that recreates exposed authoritative values.
- Atomic copy/removal/marker visibility under MAIN/WAL/snapshots/journal recovery,
  concurrent writes/migrators, failures, crashes and lost acknowledgement.
- Honest receipt/UNKNOWN recovery and post-drain handoff; no schema-marker lowering,
  journal deletion, alias restoration or silent restoration of legacy access.
- How it composes with the still-unproved old-reader floor/relation boundary,
  coherent supported snapshots and the joint publication/writer invariants.

The smallest subsequent slice is a **design-only quarantine routing/migration and
recovery contract grounded in these fixtures**, followed only after acceptance
by a disposable prototype that leaves production paths unchanged. It must state
any remaining old API or diagnostic gap. No floor, lease or quarantine candidate
clears publication's other blockers; finite successful reproductions are not
proof that races, metadata leakage or all old access paths are excluded.
