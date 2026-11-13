# M8 S3e.3e: M6 transaction-bound metadata routing seam

**PARTIAL — coherent test-only routing through actual M6 transactions is
demonstrated. Production routing and complete isolation remain unqualified.
Neither Candidate A nor B is selected. Publication remains BLOCKED.**

## Scope and pre-implementation acceptance contract

The experiment may attach a `#[cfg(test)]` child module only to an immutable,
disposable source archive. Existing production function bodies, migrations,
Cargo inputs and worktree production source remain unchanged. The seam must call
actual `connection`, `begin_write`, `WriteTransaction::commit`/Drop and registered
M6 contexts. It must not replace the revision UDF, manufacture an active revision,
insert ledger rows as authorization, disable guards or invent permissions.

A legacy layout and a renamed-table layout exercise transaction routing; this
is not selection of Candidate B. A-shaped new-table coverage is a separate
characterization: actual accounting does not imply new-table write protection.
No new settings receipt infrastructure is planned.

| Oracle | Acceptance condition |
| --- | --- |
| E01 preservation | Preserve all supported key/value entries, arbitrary/empty/absent settings and unrelated populated ordinary state; reject unsupported source before commit. |
| E02 authorization | Real M6 scope permits ordinary metadata DML; inactive/cross-connection contexts refuse. M6 alone does not grant separately permit-protected administration. No invented metadata read ACL. |
| E03 accounting/resources | Exactly R+1 for successful transaction, no committed R for read/refusal/rollback. Report actual absence of application metadata quotas; SQLite resource limits are distinct. |
| E04 concurrency | Layout discovery and pair read share one pinned transaction; concurrent transactions expose complete old or new pairs. |
| E05 queued old setter | Real historical Store setter spanning held IMMEDIATE cannot repopulate old authority after rename or leave a phantom R. |
| E06 statements | Prepare-only, partly stepped and held snapshots are distinguished; entirely OLD observations are allowed, post-release SQL is checked at execution. |
| E07 rollback | Actual precommit failure/veto and transaction Drop restore metadata, layout and ledger without silent retries. |
| E08 lost acknowledgement | Actual commit returns UNKNOWN after durable change/R; inspect separately. Missing acknowledgement is not rollback and does not authorize blind replay. |
| E09 layouts | Both targets, missing target, aliases, malformed schema, missing guard and unsupported NULL source refuse explicitly. |
| E10 pair semantics | Missing authority errors; legitimate optional values remain optional. No mixed pair or manufactured disabled/default setting on routing failure. |
| E11 old diagnostics | Actual historical doctor output is classified; stable fixture results do not clear the accepted d05/Q09/Q10 failures or Decision B. |
| E12 stale retries | Expected R/canonical source checked under actual writer serialization; stale requests refuse without a new committed R. |

PASS of a known-unsafe characterization means its unresolved behavior reproduced,
not successful isolation. Production routing, administrative rollout, complete
capability/cache/indexer equivalence and publication remain separate gates.

## Baseline and accepted predecessors

Verified branch `feat/m8-s3d2-semantic-staging`, clean index/worktree, signed HEAD
`afe5e43eee0cfa1d680d0ef6c298f428c7ef7aa0`, exact parent
`5b4c5dc723c2ac192b5003a06d96402d1ca6082c`. GPG reported a good signature from
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`. Lineage continues through signed
S3e.3c `5b4c5dc`, S3e.3b `9e22ac3`, reconstructed S3e.3 `91c70b9` and passive
schema17 `54e5dff`.

Read with [S3e](m8-s3e-publication-design.md),
[S3e.1](m8-s3e1-writer-fencing-design.md),
[S3e.2](m8-s3e2-fencing-infrastructure.md),
[S3e.3b](m8-s3e3b-adversarial-harness.md),
[S3e.3c/Q01–Q10](m8-s3e3c-metadata-quarantine-feasibility.md) and
[failed A/B comparison](m8-s3e3d-layout-cutover-spike.md).

Model A trusts the database-owning local account, its processes, kernel and SQLite.
Arbitrary same-UID SQL is outside guaranteed admission-respecting exclusion.
No metadata mechanism clears the old active-memory boundary, asset/alias writer
protection or required joint publication transaction. Entirely old pinned snapshots
remain allowed; no mixed active cohort is accepted. No standalone freeze is added.

## Actual implementation inventory

SOURCE means inspected code; OBSERVED will identify new executed tests;
REASONED and UNKNOWN distinguish conclusions from missing runtime evidence.

| Boundary | Actual enforcement and limitation |
| --- | --- |
| Connection lifetime | [`SqliteStore`](../src/storage/sqlite.rs), lines99–121 and464: one `Mutex<Option<Connection>>`, not a pool. Different Store instances own independent connections. A guard pins its mutex; recursively calling public Store methods while holding it risks deadlock. |
| Admission | `init_conn`, line397: busy timeout5s, WAL, FKs and real contexts, compatibility/migrations/inventory. It is distinct from per-operation layout discovery. |
| Read access | `get_meta`, line3095: lock plus `SELECT mind_meta`. No per-key read ACL, quota or transaction-bound pair API. SQLite locking/snapshots apply; the caller must establish a multi-read transaction. |
| Write access | `set_meta`, line3106: actual `begin_write`, UPSERT, actual commit. Ordinary metadata needs M6 accounting, not ownership/publication permission. |
| M6 scope | `register_revision_context`, line723: connection-local active-R UDF and commit/rollback hooks. `begin_write`, line781, refuses poisoned, nested or unmanaged transactions; IMMEDIATE serializes allocation of exactly one next R. Rows in the ledger alone grant nothing. |
| Commit lifecycle | `WriteTransaction`, lines141–239: commit clears active R; Drop rolls back. Busy/locked/constraint classify as definitive noncommit; other commit failures become UNKNOWN. `spawn-gate` supplies actual commit veto/lost-ACK test hooks. |
| Read-after-UNKNOWN | `connection()` checks retained Option rather than poison. `ConnectionGuard::drop` removes a poisoned handle only while not autocommit. Whether a committed/lost-ACK handle remains readable must be tested; comments promising discard are not sufficient proof. |
| Migration accounting | `MigrationRevisionScope`, line976: allocates actual metadata R inside an owned transaction. It is not an administrative intent/authority constructor; no future migration is allocated here. |
| Guards/inventory | `FENCED_TABLES`, line1174, includes `mind_meta`. Real BEFORE INSERT/UPDATE/DELETE guards require the active R. Inventory line1673 checks names; inspect actual bindings and execute refusals as well. A new table is not covered automatically. |
| History/capabilities | `ROW_AVAILABILITY_KEYS`/`SNAPSHOT_SPECS` omit `mind_meta`. Metadata writes record R but no temporal settings history. Coverage capabilities are explicit; no new historical metadata guarantee follows. |
| Administrative permits | [Ownership](../src/storage/sqlite/ownership.rs), lines16–95 and626+, and later metadata modules require exact private row permits in addition to R. [AdminContext](../src/cli/ownership.rs), line221, is established only by the explicit CLI path/UID/inode checks. The seam must not manufacture it. |
| Quotas/resources | No metadata application quota was found in storage/core paths. SQLite limits, busy timeouts and R overflow are different mechanisms; no invented quota policy may be credited as M6. |
| Statements/retries | Metadata getters prepare ordinary SQL, with no explicit shared layout/statement cache. SQLite may reprepare on execution. Production supplies busy timeout and commit classification, not automatic safe stale-intent replay. |
| Model settings | [Manager](../src/embeddings/manager.rs), line263, reads optional raw strings separately. [Semantic readiness](../src/retrieval/semantic.rs), line150, distinguishes disabled/missing model and propagates storage errors. The seam must preserve optional raw values; it does not qualify these complete production routes. |

Read/write isolation within SQLite and actual M6 guard behavior are enforceable
mechanisms. Pair/session construction, stale-input preconditions and avoiding
recursive mutex access are caller obligations until a supported API exists.

## Reproducible test-only attachment

Sources: [runner](experiments/m8/s3e3e_probe.py) and
[unit-test seam](experiments/m8/s3e3e_routing_seam.rs).
The runner archives exact current `afe5e43` and historical schema16
`254d1ad5794db8ac19dc187a5934910337da9b9e`. In the disposable current archive only,
it copies the new module and unchanged S3e.3b helper beside the existing storage
unit tests and appends exactly:

```rust
#[cfg(test)]
#[path = "sqlite/s3e3e_routing_seam.rs"]
mod s3e3e_routing_seam;
```

The original `sqlite.rs` bytes remain an exact prefix; no production function is
rewritten, exposed or replaced. The worktree `src` tree is untouched. The archive's
`sqlite.rs` is therefore explicitly a test attachment, **not** claimed byte-identical
as a whole. Its original/pinned framework hash and complete composed hash are
recorded and checked. No Cargo or migration overlay is applied.

Cargo builds `--lib --features spawn-gate --no-run` for the current unit-test
executable. The self dev-dependency lets the original common helper use its pure
SHA helper; the seam calls private M6 functions in its own actual parent module.
The historical actor is the unchanged S3e.3c integration endpoint linked against
real schema16 source; actual old production CLI supplies diagnostics. Test builds
and production binaries are labelled separately.

The adapter's `read` owns one actual Store connection guard and a DEFERRED SQLite
read transaction. `observe` validates inventory, exclusive table authority,
columns, genuine metadata guard bodies/bindings, complete supported row decoding
and R within that snapshot. A pair is read from that one observation. `get` returns
its real optional value; missing/ambiguous authority is an error, never None.

The adapter's `write` uses actual `begin_write`, revalidates layout/source within
IMMEDIATE, applies all supplied keys only to that authority and calls actual
`WriteTransaction::commit`. A supplied expected observation binds R, layout and
canonical raw metadata. `cutover` validates source rows before DDL, renames without
an alias, verifies fidelity and commits through the same actual M6 wrapper. No
process-wide layout cache or blanket public Store implementation is added.
Optional keys are allowed; no new paired-presence rule is invented.

Run from the worktree on Linux with Python3.12+, Git, Rust/Cargo and cached locked
dependencies, choosing a nonexistent output directory:

```bash
docs/experiments/m8/s3e3e_probe.py \
  --output /tmp/m8-s3e3e-run-001 \
  --target-dir target/s3e3e-build --repeat 3
```

Without a manifest this builds all pinned roles. To reuse authenticated S3e.3b
production binaries while compiling the new unit target, add
`--binaries-manifest /path/to/s3e3b/manifest.json`. Actual original production build
pins/hashes are retained with a copied provenance manifest; production src,
migrations and Cargo equivalence to `afe5e43` is checked. The new runner does not
relabel the earlier current binary as a fresh build. Fresh schema16/17 smoke tests
execute again. Separate role target caches may be reused.

Focused independent execution after building:

```bash
docs/experiments/m8/s3e3e_probe.py \
  --output /tmp/m8-s3e3e-ack-001 \
  --reuse /path/to/s3e3e/manifest.json --case e08 --repeat 1
```

The runner authenticates both experiment inputs, four framework inputs, composed
source and all five executable roles. Only the new unit-module scenario names are
selected; each executes in its own process, so one-shot fault globals cannot
contaminate other cases. It reuses existing subreaping/process supervision and
Case/fingerprint/cleanup infrastructure. Child waits are 45s, scenario deadlines 240s,
build deadlines 900s. Environments are cleared and HOME/XDG/TMPDIR isolated; only
generated databases are accepted. Passing fixtures are removed; outputs and failure
artifacts persist. An outer hard termination may leave environment/log files
without a final result record. Unsupported filesystem roots are SKIPPED.

## Acceptance and evidence matrix

R = regression invariant; U = counterexample/unresolved behavior characterization.
U PASS never means secure behavior. SOURCE claims above remain distinct from
OBSERVED executions below and predecessor reports that were not rerun.

| Test / oracle | Observed bounded result | Remaining guarantee |
| --- | --- | --- |
| e01 / E01 | PASS (R): actual file-ingested populated state and complete supported metadata preserved; real cutover R+1, pair R+1 and arbitrary-key R+1. Existing ordinary rows/history unchanged by cutover. | Public Store getter still fails after rename (explicitly asserted); complete production routing UNKNOWN. |
| e02 / E02 | PASS (R): actual inactive/foreign contexts refuse guarded metadata writes; active R alone cannot insert S3b ownership without its genuine private permit. Raw read succeeds, as expected under Model A. | No metadata read ACL exists; administrative cutover authority/intent equivalence UNKNOWN. |
| e03 / E03 | PASS (R): reads and actual SQLite query-only write failure commit no R; a 73,737-byte metadata value succeeds with exactly R+1. | Application metadata quota is absent, not simulated. Capacity exhaustion/disk-full/resource guarantees UNKNOWN. The unrelated admin 64KiB request bound is not a metadata quota. |
| e04 / E04 | PASS (R): an existing supported seam reader stays on its whole OLD snapshot across cutover; fresh snapshot sees NEW. A queued seam writer resolves new authority after writer release and gets the next R. | Full production query sessions/model caches UNKNOWN; no process-wide layout cache is used. |
| e05 / E05 | PASS (R): actual old Store opened16, then real upgrade17; setter spans actual M6 writer transaction and fails after old lookup removal without phantom R or changed authority. | 150ms no-reply observation identifies an interval across IMMEDIATE, not the exact blocked instruction. No universal old-writer/API exclusion. |
| e06 / E06 | PASS (R): prepared-only old-name execution on an actual Store-owned connection fails after cutover; partly stepped statement retains OLD and next old-name execution fails. Seam getter returns NEW. | This is the real private connection, not comprehensive historical Store cache instrumentation or an all-API audit. |
| e07 / E07 | PASS (R): actual Drop after DDL/error and actual commit-hook veto restore rows/layout/R; subsequent successful cutover gets exactly the next R. | No process-kill, power-loss or arbitrary I/O recovery qualification here. |
| e08 / E08 | PASS (U): actual LOSE_ACK returns UNKNOWN after committed pair/R. Same Store refuses another write but its poisoned/autocommit connection remains readable; fresh Store agrees. | A blanket claim that all operations fail or the handle is physically gone is disproved. This is not itself a newly demonstrated confidentiality flaw or a violated read-ban policy. No operation receipt/liveness proof. |
| e09 / E09 | PASS (R): both names, missing table, view alias, extra column, missing guard and preexisting SQL-permitted NULL key refuse read/write/cutover; no committed R or partial cutover. | Enumerated corruption coverage only; complete schema authenticity, damaged-file/VFS behavior and arbitrary state restoration UNKNOWN. |
| e10 / E10 | PASS (U): optional/empty/arbitrary values preserved. Two independent getters can return an enabled/model combination spanning two committed generations; one observation gives a complete pair. | M6 alone does not make separate settings calls atomic. Full model/capability pipeline UNKNOWN; missing authority is tested separately as error. |
| e11 / E11 | PASS (R): actual old doctor reports17-vs16 incompatibility, discloses OLD before cutover, omits stable NEW afterward; full stdout/stderr checked for NEW/NEXT. Actual post-cutover file ingest/M6/FTS verification succeeds. | Stable WAL-aware controls only. S3e.3d combined DELETE preparation disclosure, Q09 receipt failure and Q10 old active-memory access remain unresolved; not rerun or fixed here. |
| e12 / E12 | PASS (R): stale expected observation refuses and leaves R/state unchanged; a separately refreshed coherent observation permits a new operation R+1. | No automatic UNKNOWN replay or complete production stale-worker exclusion. |
| e13 / A coverage | PASS (U): actual M6 transaction creates/accounts A-shaped table. Later raw update changes it without R while current inventory passes; supported seam refuses that authority. | A's additional tables are outside actual fixed guard inventory. This minimal A-shaped control is not the complete three-table A protocol or a production vulnerability. Guard extension would require separately authorized work. |

The seam is equivalent to ordinary M6 metadata writes for the **tested existing
real guard/context and accounting boundaries**. Its layout/source validator is
additional test-only routing logic, not a production permission system. It does
not demonstrate administrative rollout authorization, complete supported-client
compatibility, model semantic equivalence or Candidate A's new-table permissions.

## Recovery state contract

| State | Actual observation / permitted conclusion |
| --- | --- |
| Before selection | The seam owns a connection/read or write transaction; discovery and settings access must use that same transaction. No cached authority grants permission. |
| Prepared, uncommitted | Actual IMMEDIATE and active M6 R exist; other readers keep prior snapshots. That tentative R and layout are not publication or durable success. |
| Committed, acknowledged | Actual commit clears active authorization; metadata/layout and one R are visible together to subsequent transactions. Retained old snapshots remain old. |
| Committed, acknowledgement lost | Actual test hook commits, returns `CommitOutcomeUnknown`, poisons writes. Committed state/R survive healthy reopen. Reads on the retained autocommit handle can still succeed; they are not independent receipt/recovery authority. |
| Invalid/ambiguous authority | The seam errors and rolls back any tentative write R. It neither returns stale legacy values nor invents disabled capability/default model. |

Known Drop/veto rollback permits the separately attempted new operation. Stale
input refusal requires explicit refresh. UNKNOWN is not rollback: do not restore
aliases, lower markers, blindly retry or infer process death from receipt absence.
No new receipt system is introduced; the controlled hook proves its exact commit
location, not arbitrary unknown-I/O operation attribution or writer liveness.

The preexisting test named `discarded_connection_refuses_writes_but_keeps_reads`
in `tests/m6_temporal.rs` actually exercises an early busy failure and subsequent
clean write. Its name is not lost-ACK evidence. The new e08 reaches the actual
commit-before-ack path. The M6 ADR requires discarding uncertain write authorization;
it does not supply a universal metadata read ban. Keep those statements separate.

## Decision and smallest safe follow-up

**PARTIAL for this integration investigation.** Actual M6 transaction-bound metadata
routing is feasible in the isolated seam without production refactoring. This
removes the earlier need to simulate revision authorization for these particular
existing-guard tests. It does not accept a production metadata layout.

Neither Candidate A nor Candidate B is selected. The accepted S3e.3d BLOCKED decision
and publication BLOCKED remain intact: accounting inside a writer cannot repair
physical pre-recovery diagnostic disclosure, automatically guard new A tables,
make independent getters atomic or establish complete old-reader/memory exclusion.
No new encryption, database file, key manager, receipt subsystem or quota was needed
for this bounded result.

The smallest safe next slice is a **test-only first-protected-update and recovery
boundary probe using this real M6 seam**. Exercise a value-preserving cutover and
first settings update under DELETE/WAL, actual commit uncertainty and process
interruption, with old doctor before normal recovery, while preserving held
snapshots. Determine what physical/admission ordering is missing to prevent the
accepted d05 disclosure; do not assume R accounting closes it. A remains conditional
on real new-table guard/inventory integration and Q09 replay integrity. Production
API/cache/model integration and any UNKNOWN read-use policy remain separately
reviewed prerequisites, not implied implementation authority.

The separate `memories` read fence, affected source/vector/alias protections,
coherent retrieval/projection/citations, and one complete authorized publication
transaction remain required. No active fence or standalone freeze is authorized
by this seam. No production migration/schema/behavior, retrieval/MCP API, ownership,
publication, commit, merge, push or S4 is changed.

## Executed verification and limitations

Primary evidence:
`/tmp/mindspace-s3e3e-YKQYB2/primary/{manifest.json,summary.json}` and `primary.log`.
Build/smoke inputs and immutable archives are under the sibling `initial/` directory;
primary reused that authenticated manifest. The initial 39 executions are separate
smoke/runtime evidence and are not added to the primary total.

| Check | Executed result |
| --- | --- |
| Seam scenarios | **13 scenarios × 3 root categories × 3 repetitions  = 117 executions; 171 PASS cases** (144 regression invariants, 27 characterizations), 0 failures, 0 skips. |
| Environment | Baseline output-root and explicit `/tmp` are both tmpfs; worktree target root is btrfs. Two filesystem types on one host, not three independent platforms. |
| Teardown | Parent checked all 171 passing primary fixture paths removed. Stdout/stderr, JSON results, fingerprints and source/executable provenance retained. |
| Format | `rustfmt --edition 2021 --config skip_children=true --check` on the new module: PASS. Child helper is archive-only; `skip_children` avoids resolving a nonexistent worktree copy. |
| Python | `ruff check --no-cache` and `ruff format --no-cache --check` on new runner: PASS. |
| Clippy/build | Exact archived `cargo clippy --locked --offline --lib --tests --features spawn-gate -- -D warnings`: PASS, including cfg(test) seam code. Logs `clippy.log`; initial native unit target and historical actor compiled with locked offline dependencies. |
| Targeted existing regressions | Existing `m6_temporal` integration executable: **19 passed, 0 failed, 0 ignored**, isolated HOME/XDG/TMPDIR and 180s outer timeout. Logs `m6-regression.{stdout,stderr,json}` and build JSONL retained. |
| Wider suites | Not run: worktree production code, migrations, Cargo inputs and shared test infrastructure are unchanged. Existing unrelated API/retrieval suites were not represented as passing. |

Recorded environment: rustc 1.98.1, Cargo 1.98.1, Python 3.14.9, bundled SQLite 3.53.2,
Linux 7.2.14-zen1-1.1-zen. No unsupported filesystem root was encountered; the runner
retains explicit SKIPPED branches. Network filesystems, read-only mounts, alternate
mount options, power loss, supervisor SIGKILL and no-embeddings builds are untested.
SQLite FULL/capacity exhaustion is not covered by the large-value/query-only case.
No new data-at-rest secrecy, hot-journal or inode-drain guarantee follows; those
predecessor findings remain limitations. MAIN/WAL/SHM/journal fingerprints capture
hashes, inode/device, ownership/mode and timestamps, with hashing's atime caveat.
Finite repetitions do not prove race absence or general production equivalence.

## Independent review

A read-only Sol planner checked the actual implementation and approved the
archive-only test attachment approach before implementation. A separate read-only
Sol reviewer inspected the final three-file change, actual function use, source
attachment, transaction boundaries, accounting/permit claims, optional values,
recovery assumptions and evidence scope. It reported **no confirmed findings
requiring changes** and agreed that PARTIAL/no-selection/publication BLOCKED is
calibrated.

The reviewer independently authenticated both experiment hashes and all five
executable hashes, verified the original production-source prefix and declared
cfg(test) attachment, and reran all 13 scenarios once per root:
**39 executions / 57 PASS cases (48 invariants, 9 characterizations), 0 failures,
0 skips**. Evidence is under
`/tmp/mindspace-s3e3e-independent-review/{manifest.json,summary.json}`.
It confirmed all 57 passing fixture paths removed and no tracked/staged changes.
Parent independently checked those totals and cleanup. These counts are separate
from the primary run and are not proof of race absence.

Parent also compared every other pinned production/Cargo/migration input in the
current archive: 109 files were byte-identical, with only the exact recorded
cfg(test) suffix added to `sqlite.rs`. Records:
`/tmp/mindspace-s3e3e-YKQYB2/source-overlay-verification.json`.
A deliberately incorrect experiment digest was rejected before any scenario;
`manifest-rejection.json` records that negative control. Markdown links and new-file
whitespace were checked. No source correction was needed after the initial build.

The reviewer did not independently rebuild production binaries, rerun broader
suites or test capacity exhaustion/power loss. Administrative rollout authority,
complete model/caller integration, physical recovery and Candidate A guard coverage
remain outside the verified equivalence claim. The final changes are confined to
this report and two new experiment files and remain uncommitted for review.
