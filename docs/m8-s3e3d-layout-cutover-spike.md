# M8 S3e.3d: disposable metadata layout cutover spike

**BLOCKED — neither candidate satisfies the acceptance contract; no mechanism
selected. Publication remains BLOCKED.**

## Acceptance contract established before experiments

This test-only investigation compares A, the committed S3e.3c two-key generation
fixture, with B, physical relocation of the complete `mind_meta` table in the
same database without any legacy table/view alias. A protects the optional raw
TEXT settings `embedding.enabled` and `embedding.model`; they are not encryption
keys. B relocates every generic metadata entry. Neither mechanism is an
active-memory fence. Model A still trusts the database-owning local account,
its processes, kernel and SQLite; arbitrary same-UID SQL is outside exclusion.

PASS below means the precise bounded oracle. Passing a known-unsafe
characterization reproduces a failure; it does not satisfy an isolation gate.
Required production properties without an authorized integration path remain
UNKNOWN. Any required FAIL or UNKNOWN prevents candidate acceptance.

| Oracle | Required observation |
| --- | --- |
| O1 routing | Transaction-bound fixture routing preserves both settings and arbitrary metadata, with no stale fallback or silent capability disabling. Actual current Store compatibility is evaluated separately. |
| O2 pairs | A pinned transaction reads a complete old or new pair; independent Store calls are not an atomic pair proof. |
| O3 old APIs | Actual historical doctor/getter/setter output never exposes newly protected values; successful old observations are wholly old. |
| O4 queued writes | A setter submitted across an acknowledged held writer cannot repopulate legacy authority or mutate protected authority. Supported unrelated writes remain usable. |
| O5 snapshots | Prepare-only, partly stepped, held read transaction and retry after release are distinct controls. Prior OLD knowledge is allowed. |
| O6 populated fidelity | Copy the serialized existing source; preserve every value, absence, unrelated rows, constraints, indexes and FKs; no B legacy alias. |
| O7 M6 | Preserve real accounting, authorization, inventory, guard bodies/bindings and failure atomicity. A replacement revision UDF is not production permission proof. |
| O8 rollback | Controlled precommit interruption/rejection leaves prior authority and values on restart; no partial cutover or guessed recovery. |
| O9 no acknowledgement | Controlled committed-before-reply interruption retains committed authority. Missing reply is not rollback. Receipts, if claimed, require complete historical integrity. |
| O10 corruption | Missing, malformed, duplicated or conflicting authority refuses explicitly; never choose stale fallback. |
| O11 physical effects | Capture MAIN/WAL/SHM/journal hashes and inode metadata around operations; distinguish normal RO, immutable MAIN, hot recovery and drain. Logical COMMIT is not physical invisibility. |

The oracles derive from [S3e §8](m8-s3e-publication-design.md),
[S3e.1](m8-s3e1-writer-fencing-design.md), [passive S3e.2](m8-s3e2-fencing-infrastructure.md),
[reconstructed S3e.3](m8-s3e3-old-reader-exclusion-design.md),
[S3e.3b](m8-s3e3b-adversarial-harness.md) and
[S3e.3c Q01–Q10](m8-s3e3c-metadata-quarantine-feasibility.md).
Historical Decision B rejects a diagnostic exception; Q10 and the atomic
publication/writer contract remain required independently of this comparison.

## Baseline and method

Verified signed HEAD `5b4c5dc723c2ac192b5003a06d96402d1ca6082c` on
`feat/m8-s3d2-semantic-staging`, with clean index/worktree before this slice.
Its exact parent is `9e22ac3894a2c4d534f9bb1103573f220f171be7`;
GPG reported a good EDDSA signature from fingerprint
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.
Lineage continues through `91c70b9` and passive production `54e5dff`.
Production `src`, migrations and Cargo inputs are unchanged across that lineage.

The [new runner](experiments/m8/s3e3d_probe.py) composes the committed
[Candidate A/Q01–Q10 fixture](experiments/m8/s3e3c_metadata_probe.rs) with the
[new child probe module](experiments/m8/s3e3d_layout_probe.rs) only in a disposable
archived integration-test target. The original A source and helpers remain
unchanged. The [new historical actor](experiments/m8/s3e3d_old_actor.rs) links
archived schema16 source `254d1ad5794db8ac19dc187a5934910337da9b9e`.
It distinguishes real historical Store calls from separate native historical
SQLite statement/snapshot controls. The actual old production CLI supplies
`doctor`. Current production source is pinned to `5b4c5dc`.

The runner uses existing S3e.3b process supervision, Linux subreaping,
45-second child deadlines, 240-second scenario deadlines, and 900-second build
deadlines. Generated DBs use cleared environments and isolated HOME/XDG/TMPDIR;
no database-path input or live data discovery is available. Passing fixtures are
removed; logs, source/binary identities, fingerprints and result records persist.
Unwind failures preserve fixtures; an outer kill can leave only environment/log
artifacts, without a final result record. Unsupported filesystems/leases are
SKIPPED rather than accepted. Finite repetitions do not prove absence of races.

## Actual M6 and routing dependencies

These are source observations, independent of adapter successes:

| Dependency | Existing behavior and implication |
| --- | --- |
| Generic Store API | [Trait](../src/storage/mod.rs) accepts arbitrary keys. Only two checked-in production settings consumers does not narrow that API to two keys. |
| Metadata SQL | [Get/set](../src/storage/sqlite.rs) hard-code `mind_meta`. Neither A nor B implements production routing. A can turn settings into absent values; B can cause explicit missing-table errors. Neither is successful supported compatibility. |
| Settings/capability | [Manager](../src/embeddings/manager.rs) reads optional raw strings through separate calls; semantic resolution/cache/indexer behavior requires separate integration. A fixture pair reader proves no runtime/capability equivalence. |
| Diagnostics | [Inspection](../src/storage/sqlite.rs) reads the legacy table if it exists; [doctor](../src/doctor/mod.rs) can print raw selected-model text after incompatibility. Schema rejection is separate from information exclusion. |
| Accounting | [Private `begin_write`](../src/storage/sqlite.rs) allocates one active connection-local R under IMMEDIATE; [commit](../src/storage/sqlite.rs) handles uncertainty/discard. Native SQLite commit controls do not exercise that production classification. |
| Migration authority | [Private migration scope](../src/storage/sqlite.rs) binds accounting to an owned serialized transaction. The external fixture cannot construct it through public Store methods. No new administrative permit is authorized. |
| Guards | [`FENCED_TABLES`](../src/storage/sqlite.rs) includes `mind_meta`; its three BEFORE DML triggers require the active revision. A table rename is DDL, so DML guards alone do not account it. |
| Inventory | [Validation](../src/storage/sqlite.rs) checks expected trigger names. Names surviving rename do not alone prove correct table binding, body, routing or layout authority. |
| History/availability | `ROW_AVAILABILITY_KEYS` and `SNAPSHOT_SPECS` omit `mind_meta`. Preserve real revisions and existing protected domains; do not invent temporal metadata history. |
| Model A | Actual ownership administrative paths bind existing database identity and local context. A native fixture UDF is a labelled simulation, not evidence of those permissions or a new production administration path. |

The checked-in M6 ADR's statement about older writers concerns writers lacking
its revision UDF. The pinned schema16 client already implements M6; fresh schema
admission and its surviving connection behavior must be measured separately.

## Reproduction

Linux, Python 3.12+, Git, Rust/Cargo and cached locked dependencies are required.
Choose a new output directory. A clean build is independently reproducible:

```bash
docs/experiments/m8/s3e3d_probe.py \
  --output /tmp/m8-s3e3d-run-001 \
  --target-dir target/s3e3d-build --repeat 3
```

To reuse authenticated S3e.3b production binaries while rebuilding the new test
actors, add `--binaries-manifest /path/to/s3e3b/manifest.json`. Their actual
historical/current build pins, hashes and original manifest are recorded; source
and migration/Cargo equivalence to this baseline is checked. Reuse never means
relabeling the original binary as newly compiled. This investigation reused
production current `91c70b9`, whose production inputs are identical to `5b4c5dc`,
and historical `254d1ad`; fresh schema16/17 smoke checks ran again.
Role-specific target caches were reused from `target/s3e3c-build`.

Focused independent execution after building:

```bash
docs/experiments/m8/s3e3d_probe.py \
  --output /tmp/m8-s3e3d-hot-journal-001 \
  --reuse /path/to/s3e3d/manifest.json --case d05 --repeat 1
```

The runner verifies all three new experiment hashes, four committed framework/
Candidate A hashes, six executable roles and the composed test source. `--case`
is a test-name substring; endpoints are excluded. `--repeat` defaults to three.
Baseline output-root, explicit tmpfs and conditional btrfs are root categories,
not necessarily three distinct filesystem types. Unsupported roots are SKIPPED.
The report itself is not a build input. Manifests bind source/toolchain/build
inputs; they do not establish bit-identical builds or hostile-toolchain security.

## Scenario evidence and classifications

These are bounded experimental oracles, not production publication gates.
`R` means regression invariant; `U` means known unsafe characterization.

| Scenario | Class | Observation / limit |
| --- | --- | --- |
| Q01–Q10 | R/U, unchanged | Re-executed committed A controls: old diagnostics, missing current routing, held snapshots, unguarded/guarded queued setters, hot journals, WAL/SHM, drain/raw access, ordering, interrupted prepare, receipt corruption and surviving old active-memory access. Prior execution counts are not added to this slice. |
| d01 | U | Six populated A/B fixtures preserve all copied key/value entries across present, absent and arbitrary/empty settings. Original column/PK/NOT NULL controls, metadata index/FK properties, complete existing space rows and integrity are checked. Actual current getters/setters remain incompatible with each candidate. |
| d02 | R | Actual old Store opened16 survives real migration17. B removes its lookup target: queued setter, later getter and unrelated metadata setter fail; stable old doctor reports incompatibility with no NEW marker. Old unrelated metadata failure is explicit client incompatibility, not supported-client success. |
| d03 | R | Historical-linked native held/partly stepped statements retain OLD. Prepared-only first execution fails after B cutover. SQL executed after releasing the old snapshot/statement also fails. This does not instrument Store's private statement cache. |
| d04 | R | A and B pinned adapter pair reads retain old complete values across a later pair update; fresh snapshots see both new values. Unrelated adapter updates work. A pair snapshot is not proved by two individual Store calls. |
| d05 | R/U | DELETE/WAL controlled precommit crash recovers prior SQL; postcommit/pre-reply crash retains B/NEW and rejects a second cutover. DELETE combined rename-plus-NEW update exposes uncommitted NEW through old doctor before recovery: isolation FAIL, intentionally reproduced. |
| d06 | U | B WAL commit is visible to normal SQL while immutable MAIN retains OLD schema/value. Old WAL-aware doctor omits NEW but recreates missing SHM without MAIN/WAL hash changes. SHM removal occurs only after all owned connections terminate. |
| d07 | R | Both targets, missing target, extra column, old-name view, conflicting A selector and raw NULL key refuse. Rejected DDL rolls back. Minimal fixture floor17 and B rename commit jointly. Full corrupted-schema authentication and production migration are not proved. |
| d08 | U | Actual retained M6 metadata trigger names/bodies bind the appropriate physical table and reject unrevisioned INSERT. Real current Store space creation still succeeds with exactly R+1. A uses a simulated R; bare B DDL changes layout with no R. Production permission/accounting equivalence is UNKNOWN. |
| d09 | R | Comparable tiny-fixture cutover and 100 validated adapter reads are timed. Results are descriptive; no latency/performance acceptance threshold or production claim. |
| d10 | R | B fixture resolves old/new names within each transaction. A read pinned before cutover stays wholly OLD; fresh transaction sees NEW settings and updated unrelated metadata. No process-wide layout cache or alias. Writes use simulated accounting, not a production permit. |
| d11 | U | A real historical process holding a snapshot prevents B inode-drain acquisition. Closing it permits retry; raw same-UID SQL still reads NEW afterward. This is a witness, not future admission, lease handoff or hostile-account protection. |
| d12 | R/U | A CAS rejects a stale expected generation. Naive B adapter allows an unconditional stale pair to overwrite newer settings. This identifies a missing writer precondition in that adapter, not a newly discovered production vulnerability or proof that custom generation tables are required. |

### The newly observed hot-journal counterexample

The DELETE d05 fixture starts at schema17 with OLD values. One IMMEDIATE transaction
renames `mind_meta` and changes its existing value page to NEW, flushes the cache,
acknowledges the test barrier and is killed before COMMIT. The genuine hot journal
has the SQLite header. Immutable MAIN can still expose the old schema's
`mind_meta` name addressing the flushed NEW value page. Actual old `doctor`
reports schema17 incompatibility and prints NEW as `selected_model`.
Normal SQLite read-triggered journal recovery then restores OLD and the old name.

This is **demonstrated unsafe combined preparation**, not a demonstrated failure
of the pure value-preserving rename by itself. It defeats any claim that DDL alone
hides newly written metadata before physical/layout recovery is qualified.
The normal doctor helper still rejects any NEW/NEXT marker in complete stdout/
stderr; only this explicitly named unsafe case captures and asserts NEW disclosure.
The counterexample is never converted into an acceptable diagnostic exception.
No real secrets are used.

The original fixture incorrectly assumed a cache flush would expose the renamed
schema in MAIN. Its `no such table: probe_relocated_meta` failure prompted actual
layout capture, which found the more consequential old-name disclosure. A later
prepare-only retry assertion was also too strong: SQLite preparation could succeed
before execution refreshed schema. The corrected oracle executes SQL and checks
its failure. Both preliminary failures and the short-lived manifest-field/borrow
build errors are retained in the evidence root; they are not acceptance runs.

## Per-oracle acceptance results

A cell with multiple statuses deliberately separates the bounded adapter result
from a required production guarantee. Neither candidate passes the whole contract.

| Oracle | A | B |
| --- | --- | --- |
| O1 supported routing | Fixture after cutover PASS; actual current Store FAIL; capability/cache/indexer UNKNOWN. | Fixture dual-layout generic routing PASS; actual current Store FAIL; capability/cache/indexer UNKNOWN. |
| O2 coherent pair | Snapshot adapter PASS; all production routes UNKNOWN. | Snapshot adapter before/after cutover and updates PASS; all production routes UNKNOWN. |
| O3 old diagnostics/APIs | Stable tested NEW non-disclosure PASS; all APIs/indirect reconstruction UNKNOWN. | Stable tested non-disclosure and old API refusal PASS; combined DELETE preparation FAIL. |
| O4 old queued/stale writes | Guarded old queue and generation CAS PASS; unguarded control FAIL. | Old queue refusal PASS; naive stale adapter overwrite FAIL; required production stale-writer exclusion UNKNOWN. |
| O5 retained snapshots | Prior native snapshot controls PASS; private historical Store cache UNKNOWN. | Historical-linked native controls and post-release execution PASS; private Store cache UNKNOWN. |
| O6 populated preservation | Tested full key/value multiset and fixture constraints/space row PASS; full production migration UNKNOWN. | Same tested fidelity, no alias, index/FK controls PASS; preexisting unsupported-source validation FAIL (independent NULL-key probe); full production migration UNKNOWN. |
| O7 M6/permissions | Actual ordinary space write and guard control PASS; simulated migration context UNKNOWN. | Guards/ordinary space write PASS; bare DDL accounting FAIL; actual migration/permissions UNKNOWN. |
| O8 rollback/recovery | Controlled precommit prior-state recovery PASS; application validity/power loss UNKNOWN. | DELETE/WAL prior-state recovery PASS; pre-recovery NEW disclosure FAIL; application validity/power loss UNKNOWN. |
| O9 no ACK/receipts | Controlled committed-state retention PASS; corrupt historical-receipt replay FAIL (Q09). | Controlled committed-layout retention PASS; operation attribution, liveness and complete replay UNKNOWN; no receipt supplied. |
| O10 corrupt authority | Tested selected-state refusals PASS; corrupt receipt FAIL; exhaustive validation UNKNOWN. | Enumerated post-cutover layout refusals PASS; preexisting NULL source can commit (FAIL); exact schema authenticity/rollback detection beyond those cases UNKNOWN. |
| O11 files/recovery/drain | Captured effects and witness controls PASS as measurements; physical secrecy/exclusion UNKNOWN. | Captured effects and witness controls PASS as measurements; hot preparation disclosure FAIL; physical secrecy/exclusion UNKNOWN. |

## Comparative decision

**BLOCKED — explicit no-decision. Neither candidate satisfies all required
oracles. Publication remains BLOCKED independently of metadata selection.**

| Dimension | A: two-key generation fixture | B: whole-table relocation |
| --- | --- | --- |
| Authority/routing | Three new tables plus legacy unrelated metadata, selector, digest, receipt and static two-key guards. | One renamed existing table; no old alias, copied values or separate selector table required in this fixture. Still needs validated transaction routing and a rollout boundary. |
| Demonstrated strengths | Pair generation/CAS; tested old unrelated metadata can remain usable; separate target pages avoid the particular combined-root-page update tested against B. No universal physical secrecy claim. | Complete generic metadata stays in one place with original constraints/guards; old hard-coded getters/setters fail explicitly. Narrower layout transformation. |
| Demonstrated weaknesses | Missing production routing, native permission simulation, corrupt-receipt replay counterexample. | Missing production routing/accounted cutover, naive stale overwrite, combined DELETE preparation disclosure. Old unrelated metadata calls also fail. |
| Recovery complexity | Validate selector, generation, digest, legacy absence and full historical receipt links. | Recognize exact old/new layout and supported schema within the transaction; prove preparation ordering/physical recovery. No independent operation receipt/liveness claim. |
| Migration changes required later | New schema, source copy/revalidation, legacy deletion and guards, accounting/inventory/routing. | Physical table mapping change, inventory/routing/accounting and schema admission; guard bindings must remain correct. Neither installs migration 0018 here. |
| Duplicate authority | Legacy keys versus selected generation, plus receipt/history consistency. | One accepted table in a recognized snapshot; duplicated targets refuse. Layout alone does not authenticate arbitrary rollback/restoration or operation identity. |
| Operational cost | More fixture state and replay rules. | Fewer fixture structures, broader client upgrade boundary and physical ordering still unresolved. |
| Performance | Timed only on a tiny populated fixture. | Same; no production-performance selection is justified. |
| Residual exposure | Old snapshots, recovery/sidecar effects, raw same-UID access and demonstrated Q10. | Old snapshots, recovery/sidecar effects, raw same-UID access and combined-preparation hazard. B-specific active-memory access was not executed; Q10 remains an unresolved independent requirement. |

The evidence does **not** establish that two custom keys/generations/receipts are
indispensable. B's one-table mechanism is simpler structurally, but simplicity is
not sufficiency: it lacks accepted accounting/routing and a demonstrated safe
physical preparation protocol. A's CAS advantage needs a stale-writer primitive;
that could potentially bind existing M6 R and canonical source state rather than
introducing separate settings generations. That alternative is REASONED, untested.
A's receipt state is justified only if its additional operation-replay guarantees
are required and fully validated; its current replay is unsafe.

Separate files, encryption or a key manager have no newly demonstrated necessity
under Model A. Current-doctor sanitization cannot repair compiled historical
clients. Restart/checkpoint/drain alone do not replace the accepted automatic
old-reader and joint-publication contract.

## Smallest independently safe prerequisite and gate

Before choosing either mechanism, authorize a **test-only internal metadata
routing/M6 integration seam**, initially exercising B's value-preserving rename.
It must validate supported source values before committing a layout and use
actual private transaction/revision/commit machinery rather than a
replacement authorization UDF, keep production opens/schema unchanged, and verify:

1. All-key dual-layout reads/writes and an atomic settings pair, with real metadata
   R accounting, guard inventory/bindings and positive unrelated writer controls.
2. Canonical source/R preconditions reject stale writers without assuming custom
   settings-generation tables are necessary; no silent capability disabling or
   stale settings fallback.
3. A value-preserving cutover followed by the first protected update cannot
   reproduce d05 disclosure, including interrupted preparation, MAIN/WAL/journals,
   held old snapshots and old doctor before/after normal recovery. Qualify its
   durable boundary; a missing acknowledgement cannot authorize rollback/retry.
4. Exact layout/receipt validation and supported model/cache/indexer semantics;
   either prove required replay integrity or make its absence explicit.

If those cannot pass within a test-only seam, stop and identify the missing
physical-admission or transaction primitive before proposing a production rollout.
This recommendation selects a prerequisite to investigate, not Candidate B for
installation. Q10, the `memories` read fence, every affected writer/vector/source/
alias path, coherent retrieval/projection/citations and complete atomic publication
remain independent blockers. No metadata mechanism grants ownership, disables
legacy writes before authorized publication or weakens Decision B.

## Executed verification and environment evidence

Exact final-source acceptance evidence:
`/tmp/mindspace-s3e3d-ElRUWy/acceptance/{manifest.json,summary.json}`;
console log `/tmp/mindspace-s3e3d-ElRUWy/acceptance.log`.
The invocation was the authenticated-production-binary form above with
`--target-dir target/s3e3c-build --repeat 3`.

| Gate | Executed outcome |
| --- | --- |
| Complete comparison | 22 scenarios × 3 root categories × 3 repetitions = **198 executions**; **432 PASS case results**, comprising 216 regression invariants and 216 unsafe characterizations; 0 failed, 0 skipped. |
| Filesystems | Baseline and explicit `/tmp` both tmpfs; worktree target btrfs. These are two filesystem types, not three independent platforms. |
| Teardown | Parent checked all 432 recorded passing fixture paths were removed; logs/fingerprints retained. Preliminary failed/interrupted fixtures are intentionally preserved. |
| Identity | Parent rehashed all three experiment inputs and six executable roles; framework/composed-source identities verified by runner. A deliberately incorrect experiment digest was refused before any scenario (`verification.json`). |
| Format/lint | Standalone `rustfmt --edition 2021 --check` for both new Rust files; `ruff check --no-cache` and `ruff format --no-cache --check` for the runner: PASS. |
| Build/Clippy | Exact archived native target `metadata_layout_cutover` and historical target `s3e3d_old_actor` built with locked offline dependencies. Both `cargo clippy --locked --offline --test TARGET -- -D warnings` passed; logs `clippy-acceptance-native.log` and `clippy-acceptance-old.log`. |
| Broader suite | Not rerun: no production source, shared infrastructure, Cargo inputs or migrations changed. Existing Q01–Q10 were rerun as part of this comparison. |

Environment: rustc1.98.1, Cargo1.98.1, Python3.14.9, bundled SQLite3.53.2,
Linux7.2.14-zen1-1.1-zen. One host/kernel/engine; no power-loss, network-filesystem,
read-only mount, alternate mount-option or no-embeddings build qualification.
Native normal/read-only, immutable and WAL-aware diagnostic paths are distinct;
immutable access is not evidence of ordinary dirty reads. Fingerprints include
MAIN/WAL/SHM/journal existence, hashes, inode/device, ownership/mode, size and
atime/mtime/ctime. Hashing can itself affect atime. No absence of application-row
writes is reported as absence of filesystem effects.

The tiny d09 workload seeds 37 metadata entries and one additional real space,
then performs cutover and 100 full validated adapter snapshots in debug builds.
Across nine root/repetition samples per candidate, milliseconds min/median/max:

| Candidate | Cutover ms | 100 snapshot reads ms |
| --- | --- | --- |
| A | 9.155 / 10.385 / 16.552 | 15.051 / 15.999 / 17.185 |
| B | 36.926 / 39.680 / 50.211 | 35.697 / 37.503 / 43.069 |

B was slower in this particular implementation/workload despite fewer structures.
The reader implementations perform different validation work. No performance
advantage, production overhead, write-throughput, growth or tail-latency conclusion
is established; no tuning was attempted.

## Independent adversarial review and retained finding

A read-only Sol planner defined the eleven oracles before experiments. The first
final-review agent invocation was rejected by the service before producing a
review; a bounded correctness review with the same Sol reviewer role completed.
No unavailable review was counted as performed.

The reviewer authenticated source/executable hashes and independently reran the
complete comparison once per root at
`/tmp/mindspace-s3e3d-independent-review/{manifest.json,summary.json}`:
**66 executions, 144 PASS cases (72 invariants, 72 unsafe characterizations),
0 failures, 0 skips**. It reproduced DELETE old-doctor NEW disclosure on every
root and checked removal of all 144 passing fixture paths. Parent independently
checked those totals and paths. These are separate from the primary counts.

**MEDIUM, retained fixture finding:** `b_install` checks names and column shape,
but does not prevalidate source row values. Using the exact authenticated
`cutover_actor`, the reviewer relocated legacy metadata containing a NULL key,
received `committed_before_reply`/normal release, and observed the NULL row in the
new sole table. SQLite permits this key under the existing TEXT PRIMARY KEY;
public Store string keys cannot create it. It is unsupported adapter input,
not by itself database corruption or a demonstrated production vulnerability.
The same-fixture read was not executed: reader refusal follows from `try_all`
String decoding and the separately executed d07 NULL-key refusal control.
The committed source-validation gate therefore fails; matching A source-input
qualification was not executed and remains UNKNOWN. This is not an intrinsic
proof that physical relocation cannot support NULL rows with a different adapter.

Evidence: `/tmp/s3e3d-review-corrupt-source-qm9f3zc5/evidence.json`.
A portable reproduction using a manifest from the runner is:

```bash
python3 - /path/to/s3e3d/manifest.json <<'PY'
import hashlib, json, os, sqlite3, subprocess, sys, tempfile
from pathlib import Path
m = json.loads(Path(sys.argv[1]).read_text())
binary = m['executables']['native']
assert hashlib.sha256(Path(binary['path']).read_bytes()).hexdigest() == binary['sha256']
root = Path(tempfile.mkdtemp(prefix='s3e3d-null-source-'))
db = root / 'fixture.db'
with sqlite3.connect(db) as conn:
    conn.executescript('''
      CREATE TABLE mind_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      INSERT INTO mind_meta VALUES('embedding.model','SYNTHETIC_OLD');
      INSERT INTO mind_meta VALUES(NULL,'SYNTHETIC_NULL');
    ''')
env = dict(PATH='/usr/bin:/bin', HOME=str(root), TMPDIR=str(root),
           S3E3D_DB=str(db), S3E3D_ROOT=str(root), S3E3D_MODE='postcommit')
r = subprocess.run([binary['path'], '--ignored', '--exact',
                    'layout_cutover::cutover_actor', '--nocapture'],
                   input='{"command":"release"}\n', text=True,
                   capture_output=True, env=env, timeout=45, check=True)
assert 'committed_before_reply' in r.stdout
with sqlite3.connect(db) as conn:
    assert conn.execute('SELECT COUNT(*) FROM probe_relocated_meta WHERE key IS NULL').fetchone()[0] == 1
    assert conn.execute("SELECT COUNT(*) FROM sqlite_schema WHERE name='mind_meta'").fetchone()[0] == 0
(root / 'endpoint.stdout').write_text(r.stdout)
(root / 'endpoint.stderr').write_text(r.stderr)
print('FAIL source-validation gate: committed unsupported NULL key; evidence:', root)
PY
```

This extra probe intentionally keeps its tiny synthetic database/output as evidence;
all SQLite connections and the bounded subprocess close. It is a counterexample
reproducer, not a passing isolation test. No production source was changed to
repair it. No additional confirmed implementation finding was reported; exhaustive
corrupted-state coverage and private production permissions remain unverified.

Only the report and three experiment sources are new/uncommitted. No migration
0018, production schema, active fence, retrieval/MCP/source ownership change,
publication, commit, merge, push or S4 occurred. This slice stops at BLOCKED.
