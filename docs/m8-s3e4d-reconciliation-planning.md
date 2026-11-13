# M8 S3e.4d: pre-effect file-memory reconciliation

**PARTIAL — the compatibility-preserving follow-up candidate is rejected. Publication remains BLOCKED.**

**DO NOT APPLY** the [rejected candidate patch](experiments/m8/s3e4d_rejected_candidate.patch)
to any worktree or production database. It is retained only for disposable archive
reproduction. Unused experimental additions remain **UNEXECUTED** evidence.

No production reconciliation seam was installed. This slice establishes the exact
behavior inventory and executable baseline counterexamples. Passing probe cases
mean those baseline behaviors reproduced, not that pre-effect reconciliation or
writer exclusion has been implemented.

## Original baseline investigation (committed evidence)

Verified `feat/m8-s3d2-semantic-staging`, signed HEAD
`3f7fd3422047b40f9dc6fa787d4b523d7d90d857`, initially clean worktree/index.
GPG verified fingerprint `0E6699CB48F3941AE28CA88D509F78007F62D5EA`.
Read the accepted [S3e.4c report](m8-s3e4c-observation-planning.md),
[S3e publication contract](m8-s3e-publication-design.md) and
[S3e.1 fencing design](m8-s3e1-writer-fencing-design.md).

The user subsequently permitted earlier duplicate-location rejection, preserving
its externally observable error contract and complete rollback. Failed operations
need not preserve internal reconciliation or UUID allocation order, **provided no
externally observable effects escape the transaction**. The explicit instruction
was to report PARTIAL if UUID allocation or other intermediate work has such effects.
Successful operations must retain their existing semantics.

The locked UUID generator has process-global state outside SQLite rollback.
A controlled public-API probe demonstrates an encoded component gap after a
failed ingestion and a subsequent successful response in that same UUID stream.
This meets the stated stop condition. A separate baseline case shows that
unconditional duplicate-location rejection can also replace a different existing
constraint diagnostic. Neither issue was silently excluded from equivalence.

The original investigation added this report and two experiment sources, committed
as `6aeb6a7753d2610837edc30f2a190eeccc6dd017`. Its production, migration, Cargo,
existing test and API files were unchanged. The follow-up below is separate evidence.

## Exact decision and effect inventory

References below are to signed baseline `src/storage/sqlite.rs` unless stated.
The existing S3e.4c file classifier is read-only and connection-bound; application
still makes the following knowledge decisions after session/scope effects begin.

| Boundary | Decision reads / ordering | Existing mutation/materialization effects |
| --- | --- | --- |
| M6 entry, 781 | Actual IMMEDIATE serialization; active revision/connection checks. | Tentative integer R insertion. Rollback removes durable R and resets active context; it does not rewind process-global UUID state. |
| File receiver, 2231/2351 | Both call `record_observation_tx` on the retained transaction. Combined receiver later persists optional code and invokes its precommit callback. | One commit/R for the combined operation; early errors roll back. No new receipt exists for legacy ingestion. |
| File classification, 3487 | Stable location sort; current/absent logical selection, latest hashes, moves and residual disappearance. | Existing private plan is consumed at 3725. It does not allocate new UUIDs during classification. |
| Session/scope, 3744/4643 | Existing scope selected by repository/root; requested scope spaces retained. | Session UUID; optional scope UUID; session/scope rows and add-only scope-space entries. These precede current reconciliation. |
| Version/evidence, 3961 | Reuse by logical source/content hash; evidence IDs read in ordinal order. | Otherwise allocate source-version/evidence UUIDs and insert immutable provenance rows. This slice did not move those decisions. |
| Active target selection, 4289 | `load_summaries` joins memory/assertion/evidence/source, filters active logical source + producer family, orders by `(m.created_at, m.id)` (2013), then enriches memberships. | None yet within this helper, but earlier session/scope/version effects have occurred. Each old memory appears once in a reconciliation. |
| Fragment/space selection | Group old/new entries by normalized subject; retain within-group order. Merge repeated MemorySpec space requests in first-seen order. Resolve space names fallibly, including specifications that ultimately add nothing. | No missing-value fallback: absent requested spaces error; legitimate empty membership sets remain empty. |
| Same content | Reuse selected active episode. `add_spaces` (4594) reads its actual current memberships. | Add each missing requested membership once; report Unchanged or Reused and current sorted space names. |
| Different content / supersession, 4545 | Lazy assertion materialization uses evidence, extraction, normalized subject and content hash; cache is per draft index but SQL can reuse a canonical assertion across indices. | Optional assertion UUID + FTS posting, new episode UUID, requested memberships, then old episode supersession. Old memberships are retained; new episodes receive requested spaces, not an automatic union. |
| Content move / remaining fragments | After same-subject pairing, first equal-content pending fragment matches an unmatched old episode; remaining news create and remaining olds retire. | Preserve existing supersession, creation, retirement, provenance and report order. No new ambiguity/identity policy. |
| Manual retirement/recreation | Retired episodes are excluded from active target selection. Explicit unchanged reingestion can reuse assertions but creates fresh episodes. | Never reopen an old lifecycle interval. Continue skips memory reconciliation, although session/scope bookkeeping still runs. |
| Reports / index state | `push_action` and supersession query resulting space names after effects. These are reporting reads, not an independent selection of memory targets. | Existing FTS insertions only for newly materialized assertions. Old assertions/vectors remain; no new vector maintenance or eligibility rule. |
| Observation row | Insert after the file's reconciliation. | Duplicate `(session_id, location)` normally fails here, unless an earlier reconciliation constraint fails first. Entire transaction rolls back. |
| Git wrapper, 5215 | Shared `reconcile_logical` executes at its current per-derivation invocation after Git/version effects. | No shared logic, Git invocation or receiver was refactored in this slice. |

UUIDs are allocated lazily in ordinary application order: session, optional scope,
logical source/version/evidence as needed, then assertion/episode allocations in
action order. Planning only episode UUIDs early would change their relative order
against those other allocations. Reserving the entire chain would exceed a narrow
file-memory seam; no such reservation mechanism was introduced.

## Sequential dependencies and baseline counterexamples

Ordinary unique-location file entries address disjoint existing logical memory
domains. Within a reconciliation, old targets occur once and repeated membership
specifications are merged. This supports a small action plan for many healthy
inputs, but is not a proof that independent initial-state plans compose generally.

Repeated existing lineages arise through duplicate exact input locations. They
are doomed operations, yet their intervening decisions are sequential. Fresh
memory UUIDs can sort before or after retained old IDs when `created_at` ties.
Thus a pending list that always appends new episodes is not equivalent to SQL
ordering. Controlled IDs below are valid synthetic stored UUIDs, not physical
file identities or newly admitted authority.

| Case | Observed baseline behavior | Meaning |
| --- | --- | --- |
| D01 low old IDs | First reconciliation leaves retained old content before a fresh replacement UUID. The second pairs different contents, reuses an already-active assertion, and fails `UNIQUE constraint failed: memories.assertion_id`. One reconciliation completion was observed. | Unconditional source-observation duplicate diagnostics would change this public error. The existing active-per-assertion constraint is protective, not a defect repaired here. |
| D02 high old IDs | Fresh replacement sorts before retained old content; second reconciliation reuses both episodes. It then fails `UNIQUE constraint failed: source_observations.session_id, source_observations.location`. Two completions were observed. | Same failed-input shape can reach a different constraint according to actual transactional state and UUID ordering. |
| D03 repeated membership | Second reconciliation sees the first one's added membership. It produces no membership-uniqueness failure, then hits the source-observation uniqueness constraint. | A naive pre-state membership delta can introduce an earlier, incorrect error. |
| D04 UUID observability | Under a per-process fixed realtime clock, adjacent public UUID calls have component gap1; a real M6 begin/drop rollback also has gap1. The actual duplicate ingestion has gap6 although all rows/R roll back. The next successful session is the next public UUID in that stream. | SQLite rollback does not contain all observable allocation state. This triggers the user's explicit PARTIAL condition. |

All four cases assert exact table-state/ledger rollback and autocommit cleanup.
D04 additionally verifies a later successful operation consumes exactly one R and
passes integrity checks. No production precommit callback, successful response or committed database row
exposes the discarded episode IDs directly; the counter observation is through later public
UUID values. Source inspection shows the file-plus-code precommit callback is
not reached when file observation fails; this is not a new callback discrepancy.

The first D01 draft expected two completed reconciliations and the observation
constraint. Source review found the earlier active-assertion constraint before
execution; that oracle was corrected. The superseded build was never executed.
Its artifacts remain at `probe-debug-build`; the corrected manual build and final
reusable-runner builds are separately retained. No production code was changed
to satisfy an oracle.

## UUID mechanism, measured scope and limits

`src/core/mod.rs:21` publicly reexports `uuid::Uuid`. Cargo.lock pins uuid1.26.0
with checksum `b5772d71c9be8a8a6ac2117d949c5b224c1b72241bb611d9a3012edcf8af7812`.
The exact dependency source shows:

- `src/v7.rs:17`: `now_v7` uses `Timestamp::now(shared_context_v7())`.
- `src/timestamp.rs:712`: a process-global mutex stores the context.
- `src/timestamp.rs:816` onward: same/backward-millisecond calls advance stored
  counter state; UUID construction incorporates an encoded component in bytes.
- M6 rollback resets recording context, not this dependency's process-global state.

The probe uses only public UUID values. It checks the known fixed Unix millisecond
and validates an adjacent-call delta of1 before measuring
`(uuid.as_u128() >> 36) & ((1 << 26) - 1)`, with modular subtraction. It does **not**
claim to decode all42 counter bits or that counter gaps are a documented stable
UUID API. Monotonicity is the dependency's guarantee. The relevant result is that
returned bytes are observably affected in this locked implementation, not that
applications should depend on that encoding or every later real-clock UUID
necessarily shows a distinguishable gap.

The C helper overrides CLOCK_REALTIME only inside the explicitly launched D04
process. CLOCK_MONOTONIC and other clocks delegate to libc, so execution timeouts
continue normally. The host clock is unchanged. No generator reset, rewind,
guessed UUID burning or allocation-count simulation is performed. A transaction-only
control is not represented as an implemented early-rejection candidate.

This is a supported library-process observation, separate from arbitrary raw
same-UID SQLite access. No new threat model or security vulnerability is claimed.
A caller/concurrent thread may share the generator; restoring its state would
also affect that caller. SQLite filesystem effects remain a separate unknown:
row rollback does not establish byte/inode/WAL/SHM equivalence or absence of spill.
Those stronger guarantees were not tested or assumed.

## Original reproducible evidence

The commands and four-case counts in this section describe the original committed
runner and its recorded hashes. The current default runner retains two additional
unexecuted success/nonfatal traces (six cases total); those additions are not
qualified by the original results. Only the follow-up `--fatal-read` invocations
below were newly verified. The original four native probe bodies remain unchanged.

Sources: [runner](experiments/m8/s3e4d_probe.py) and
[native probe](experiments/m8/s3e4d_reconciliation_probe.rs).
Run from the worktree with fresh, nonexisting private btrfs directories:

```sh
python3 docs/experiments/m8/s3e4d_probe.py --work-root /home/cnachtigall/.cache/m4d-new-debug --target-dir target/s3e3c-build/native --runtime-bin /home/cnachtigall/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl/bin
python3 docs/experiments/m8/s3e4d_probe.py --work-root /home/cnachtigall/.cache/m4d-new-release --target-dir target/s3e3c-build/native --runtime-bin /home/cnachtigall/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl/bin --release
```

The runner verifies signed baseline3f7 with the expected key, archives that exact
commit, and authenticates the existing repository process supervisor. It adds only
a cfg(test) read checkpoint after baseline reconciliation plus an include in the
existing private native-test module; it copies the new probe source into that
archive. All other Rust/migration/Cargo bytes are checked unchanged. This is an
instrumented baseline unit test, **not an unmodified production binary or a new
production implementation**. Fixtures use actual M6 begin/write/rollback machinery,
not replacement authorization UDFs. The checkpoint only performs native read
observations during these tests.

Each exact case runs in a fresh process with isolated HOME/XDG/TMPDIR, a 60-second
timeout and retained logs/results/disposable databases. Only D04 receives the
clock helper. The runner records input/archive/helper/executable hashes, effective
features, tool versions and environment. Characterization PASS is separate from
production PARTIAL. Missing required platform/tools report SKIPPED, never PASS;
failed signature verification fails the run. No live database or real secrets are
used.

Primary artifacts: `/home/cnachtigall/.cache/m4d-trmf0xr3/verify-debug` and
`verify-release`. Actual debug result: **4 reproduced, 0 failed, 0 skipped**.
Release result: **4 reproduced, 0 failed, 0 skipped**. Independent replay also
reproduced all four cases, as recorded below.
The debug executable had default+embeddings+spawn-gate test features. An ordinary
signed-source baseline binary was separately built and authenticated before any
overlay; it has default+embeddings and no spawn-gate. It is not misreported as the
executable used for private native instrumentation.

## Acceptance disposition

| Requested gate | Disposition |
| --- | --- |
| Existing decision/effect inventory | Established by source audit and baseline probes. |
| Sequential overlap and public error distinction | D01–D03 reproduced, not repaired or reclassified as secure. |
| UUID observable-effects condition | D04 reproduced; production stop condition met. |
| New private production reconciliation plan | NOT IMPLEMENTED. |
| Both file receivers consuming new decisions | NOT IMPLEMENTED. |
| New pre-effect summary/space/membership read-failure tests | NOT IMPLEMENTED; no new planner exists to exercise. Existing S3e.4c evidence is not substituted for this gate. |
| Candidate CLI/MCP/Git stable-result comparison | NOT RUN; no production candidate was installed. |
| Candidate queued writers, veto, UNKNOWN and index/vector equivalence | NOT RUN; existing behavior is unchanged, not newly qualified. |
| Full candidate suite / production and no-embeddings builds | NOT RUN; production code and shared test infrastructure remain unchanged after the explicit stop. Earlier accepted S3e.4c gates remain prior evidence. |

Formatting, focused debug/release probe checks and independent review results are
reported separately. A successful baseline characterization cannot turn these
unimplemented production gates green.

## Residual R06/R07 blockers and stopping point

R06 still lacks the complete pre-effect file-memory decision seam, scope effects,
combined code/Git mutation domains, direct lifecycle/membership protection, stale
vector-save/key-wide-clear protection, and future receiver/SQL guards. R07 still
lacks qualified bounded old/new locator and alias/collision closure. Root strings,
hashes and passive witnesses confer no new authority. Existing old-reader,
diagnostic and physical recovery publication blockers remain unchanged.

The explicit next prerequisite is an accepted observational/allocator contract
that can address this measured global-state escape, or a separately reviewed
broader design. This slice does not propose weakening the current contract.
There is no claim that every purpose-specific reconciliation planner is impossible;
the narrow implementation was withheld rather than silently adding broader ID
reservation, a symbolic branch interpreter, fabricated SQLite errors or a fallback
mislabelled as complete planning.

No migration, active guard/write denial, source/alias admission, metadata quarantine,
memory relocation, ownership/publication change, new receipt, public API, merge,
push or S4. The original evidence was committed separately; follow-up evidence
remains uncommitted for review.

## Original verification and independent adversarial review

| Executed check | Result and scope |
| --- | --- |
| Signed baseline identity | PASS; exact3f7, expected signing fingerprint. Archive sources authenticated. |
| Ordinary baseline binary build | PASS; signed-source build, default+embeddings, no spawn-gate; SHA256 `1cf01510798d7b8aae48d361d38d80b656950fa6c10cdc517b4c277e172eca79`. No candidate binary is claimed. |
| Focused debug native cases | 4 reproduced / 0 failed / 0 skipped. |
| Focused optimized native cases | 4 reproduced / 0 failed / 0 skipped. |
| `cargo fmt --check` | PASS on the unchanged production tree. |
| Standalone probe `rustfmt --edition 2021 --check` | PASS. |
| Python AST / Git whitespace | PASS. |
| `cargo clippy --locked --offline --all-targets -- -D warnings` | PASS on the disposable baseline overlay, so the appended native probe is included. |
| Independent replay | 4 passed / 0 failed / 0 ignored, without Cargo or repository edits. |
| New production feature acceptance | NOT ACHIEVED; explicit PARTIAL stop. |

The read-only Sol reviewer independently verified that removing the single test
checkpoint restores baseline sqlite.rs exactly, the existing native test module
changes only by its include, and 197 other protected inputs are unchanged.
It checked executed runner/probe hashes, the actual error distinction, membership
state, UUID controls and the limits of the inference. It then executed the copied
debug binary in fresh isolated processes: low/high/membership cases reproduced,
and adjacent/control/failed UUID gaps were1/1/6 with next-success session gap1.
Fixture and row/revision/autocommit assertions passed. Executable/helper hashes
were authenticated before and after. No edits or additional Cargo runs occurred.

Independent artifacts: `/home/cnachtigall/.cache/m4d-review-fp8jy_dt/result.json`.
Debug executable SHA256:
`a7134359e623cbe8bbba4610a407f81b2e327dc04c0366e80bdfb5cb02521d36`.
Runner SHA256:
`ddebe16409af18c734ad410106185b85aaa2f588548442c42b317203615dafc4`.
Native probe SHA256:
`7780a1bffdf71762381843cb63d8a22888b7ab9dd2ab22eef83a780ee70ca45e`.
The per-run result and source manifests bind optimized executable and clock-helper
hashes too. Repetitions are reproducibility evidence, not proof that every
possible narrow design fails or that all timing conditions have been explored.

All runs used private btrfs storage, isolated process configuration, two build jobs
and locked offline dependencies. The optional pinned static Codex directory was
recorded on PATH for consistency, but these exact native tests do not execute
Codex or require model weights. Their success does not qualify CLI/MCP runtime
integration. The prior system Codex/OpenSSL incompatibility and exhausted `/tmp`
quota were not repaired or requalified. No host clock or global configuration was
changed. Linux/libc clock interposition and the controlled UUID fixtures limit the
reproducer's environment; unsupported platform/tools are explicit SKIPPED cases.

Final independent report review found one wording qualification: the test-only
checkpoint deliberately observes transaction-local rows/IDs, so the absence of
direct ID exposure is scoped to production precommit callbacks, successful
responses and committed rows. That wording was corrected without changing any
probe or rerunning tests. No other findings were reported. The low/high controlled
ID fixtures are separate from D04, which uses ordinary Store ingestion without
controlled episode-ID inserts.


## Compatibility-preserving follow-up: rejected bounded candidate

Current baseline: signed `6aeb6a7753d2610837edc30f2a190eeccc6dd017`, branch
`feat/m8-s3d2-semantic-staging`, initially clean index/worktree. The expected GPG
fingerprint verified. Its production source is identical to the original signed
3f7 baseline used by the reproducer. This follow-up does not amend the accepted
publication or UUID compatibility contracts.

**Earlier generic duplicate rejection is not an accepted compatibility-preserving
optimization.** D01–D04 remain binding. No fabricated SQLite error, UUID burning,
allocator rewind, new transaction, persisted plan or receipt is accepted here.

The attempted private seam captured ordered Reuse/Supersede/Create/Retire actions,
requested spaces and reuse-membership deltas only for operations with distinct
existing logical targets and unique exact locations. A new source, duplicate
location or repeated logical target kept the whole operation on ordered execution.
Continue retained its source-only behavior. Both file receivers used the same M6
transaction; the shared Git invocation stayed in place. One selection algorithm
served captured and ordered execution. UUID allocation remained lazy in the existing
session/version/assertion/memory helpers. No physical identity was inferred.

Summary/requested-space failures were retained until the original reconciliation
entry after `ensure_version`. Reuse-membership failures were retained until their
original action position; new-memory membership reads remained lazy. These rules
preserved tested nonfatal failures but overlooked transaction-ending read faults.

The rejected candidate and five focused regression additions are preserved only
in [this disposable patch](experiments/m8/s3e4d_rejected_candidate.patch), SHA256
`51831b1eedb29a1545938b3108400bbdaca22dc431c2de5e20613a0ae333524e`.
It changes exactly `sqlite.rs`, the existing private observation test include and
one new private test source **inside an archive**. The active production files were
restored byte-for-byte to HEAD, with their complete rejected versions backed up.
The patch is not an implementation proposal selected for production.

### Concrete conflicting invariant

The locked `libsqlite3-sys 0.38.2` amalgamation, SQLite 3.53.2,
`sqlite3.c:91065–91134`, rolls back the whole transaction on a read-only IOERR;
`sqlite3RollbackAll` closes savepoints and sets `autoCommit=1`. M6's rollback hook
clears its active recording revision. A saved Rust `Err` cannot preserve that
transaction. Authorization-denial tests do not exercise this behavior.

An independent native xRead probe and actual public `Store::record_observation`
pair demonstrated the resulting incompatibility:

| Oracle | Signed baseline | Rejected candidate | Disposition |
| --- | --- | --- | --- |
| Actual file xRead failure | IOERR_READ, extended code266 | Same | Reproduced native fault, not an invented callback error. |
| Transaction immediately after fault | Automatically rolled back | Automatically rolled back | A read-only planner is not transaction-neutral on this failure. |
| Returned error | `disk I/O error` | `revisioned write required` | FAIL: original error is replaced by a later M6 refusal. |
| Fixed-clock failed UUID component gap | 4 | 2 | FAIL: externally observable allocation progression differs. |
| Full table rows and M6 revision | Rolled back | Rolled back | PASS for durable logical rollback; not sufficient compatibility. |
| Cleanup / healthy reopened next session | Autocommit / next gap1 | Autocommit / next gap1 | PASS for this controlled recovery, not a universal I/O guarantee. |
| FTS/FK/passive inventory after next success | Valid | Valid | Does not repair the error or UUID mismatch. |

Both runs validate Unix millis1800000000123 and adjacent public UUID component
gap1 before measurement. The low26-bit interpretation has the same locked-version
limits as D04. The host clock is unchanged. No artificial allocation reproduces
an expected count. The baseline allocates real session/version/evidence IDs before
its failing summary read; the candidate reaches that read before those effects,
then consumes a session ID while attempting application after automatic rollback.
This is an error/observable-state incompatibility, **not a demonstrated durable
row escape or new security vulnerability**.

Immediate propagation would avoid application after transaction loss but still
omit the baseline's preceding genuine UUID allocations. Deferral already fails.
A savepoint does not contain this SQLite IOERR branch; a replacement transaction
would lose the required lifetime/serialization boundary. Artificial UUID consumption
is explicitly prohibited. No small mechanism satisfying all those constraints was
demonstrated. This rejects this candidate, not every conceivable future design.

### Reproducible fatal-read evidence

The [existing runner](experiments/m8/s3e4d_probe.py) now selects one marked block
from the [native fatal-read template](experiments/m8/s3e4d_fatal_read_probe.rs).
It authenticates signed 3f7 sources and the existing process supervisor. Optional
`--candidate-patch` accepts only the three reviewed regular-file patch paths and
applies them inside a fresh disposable archive. It never changes the worktree.

A test-only RAII guard is inserted at the original reconciliation entry, or at
candidate capture entry. It releases clean pager pages, installs the existing
connection-local xRead fault, records automatic rollback, and restores the real
file methods. The test calls the actual public Store receiver. Removing the
instrumentation must restore all protected source bytes. Neither M6 authorization
nor the allocator is replaced. Each exact native case has a 60-second timeout,
private configuration/storage and retained logs, source archives and structured
results. The fatal-read native database is disposed by its TempDir after the
asserted recovery; no surviving database fixture is claimed.

Use fresh private directories, sequentially; the target may be shared only with
no other build running:

```sh
verification_root=$(mktemp -d "$HOME/.cache/m4d-compat.XXXXXX")
python3 -B docs/experiments/m8/s3e4d_probe.py --work-root "$verification_root/baseline" --target-dir target/s3e3c-build/native --fatal-read
python3 -B docs/experiments/m8/s3e4d_probe.py --work-root "$verification_root/rejected" --target-dir target/s3e3c-build/native --fatal-read --candidate-patch docs/experiments/m8/s3e4d_rejected_candidate.patch
```

`PASS` means the expected unsafe characterization reproduced. It is not an
isolation guarantee. Equality comparison is refused in fatal-read mode because
the observed mismatch is the rejection oracle. `--release` selects an optimized
build; unsupported platform/tool prerequisites are SKIPPED, not PASS.

Shared-target Cargo freshness proved material: an initial independent candidate
run actually executed a cached baseline binary. That run was rejected and retained,
not counted. Qualified replays force the disposable root module's mtime newer,
verify unchanged bytes, require Cargo `fresh:false`, bind executable hashes and
assert the emitted mode. The first public-probe compilation also found a private
test-helper visibility error; it was corrected before that test ran. Those failed
verification attempts remain separate from qualified observations.

### Executed verification and limits

Evidence root: `/home/cnachtigall/.cache/m4dp-zebka02w`.

| Check | Actual result / scope |
| --- | --- |
| Candidate targeted debug observation/reconciliation tests | 21 passed, 0 failed, 0 ignored; five new regressions plus existing receiver/queued-writer/veto/UNKNOWN/history cases. Log: `targeted-debug`. |
| Candidate full `cargo test --locked --offline --no-fail-fast -- --nocapture` | Exit0; 71 result summaries: 1,319 passed, 0 failed, **34 ignored**. Log: `full`. This passing gate did not detect the fatal-read incompatibility. |
| Independent bundled SQLite core xRead test | IOERR_READ automatically rolls back tentative ledger write. Log/source: `reviewer-ioerr`. |
| Independent actual-M6 component pair | Baseline error/gap4 versus candidate refusal/gap2, both clean durable rollback. Qualified results: `reviewer-m6-ioerr/qualified-results.json`. |
| Independent actual public Store pair | Same mismatch; fixed-clock controls, distinct fresh binary hashes, cleanup and subsequent integrity verified. `reviewer-public-ioerr/qualified-results.json` and `qualified-postcheck.json`. |
| Final reusable fatal-read replay | Parent executed `replay-baseline` and `replay-rejected`: each exactly1 passed, 0 failed, 0 ignored. Actual public receiver reproduces error/gap4 versus error/gap2; original extended code266, rollback, clock, mode and next-session assertions passed. |
| New populated-success/nonfatal compatibility traces in experiment source | NOT RUN. Their original D01–D04 prefix is unchanged; additional sources/options are retained, not acceptance evidence. |
| Final formatting / source checks | `cargo fmt --check`, standalone fixture rustfmt, Python AST, `git diff --check`, all74 Markdown link files, original D01–D04 prefix equality, empty index and production-byte equality: PASS. |
| New candidate release/full-Clippy/production/no-embeddings gates | NOT RUN after the mandatory compatibility rejection. The active production source is restored; prior build evidence is not relabelled as this candidate's coverage. |

All executed gates use private btrfs fixtures, locked offline dependencies and two
build jobs. The broad suite uses the pinned static Codex0.166.0 runtime on PATH.
The native fatal-read test needs no model or Codex invocation. The system
Codex/OpenSSL incompatibility and prior `/tmp` quota problem were not repaired or
qualified. Linux/libc interposition and DELETE-journal fault control bound this
reproducer; WAL/SHM/physical-file equivalence is not asserted.

The five new focused tests prove no planning mutation SQL, retained reuse additions,
no second target selection in both file receivers, explicit ordered eligibility,
summary/space error deferral and membership-error deferral after prior genuine
supersession. Their successful nonfatal results do not qualify fatal errors.
The broad suite covers existing CLI/MCP/library/Git behavior only to its executed
coverage; the known runtime gaps and 34 ignored tests remain. No final feature,
optimized compatibility, novel M6 permissions/accounting or universal recovery
acceptance is claimed.

Final reusable replay executable SHA256:

- Baseline: `e08aefc08e2b2957e9eab35684d0f5b09321dbcfea7b41fc6b79d0da9cbe93e8`.
- Rejected fixture: `05609f45070cef596eb0e8ad133de5e0230adc34aef6736b85ee62cc2233eabc`.

The selected template, runner, patch, archive and post-overlay inputs are bound in
each `source-manifest.json`/`result.json`. Replayed native template SHA256:
`a287ac7a05ff04246d31751c9da618fa681b97121d039de6cac671024038a738`.

### Independent disposition and residual requirements

Final independent evidence review verified the source/artifact hashes and result
counts. Two LOW wording findings were corrected: historical default-runner scope
and disposal of the new fatal-read database fixture. No executable source or
executed oracle changed for those corrections.

Read-only Sol review found one HIGH rejection: fatal read deferral loses the M6
transaction and changes both error origin and UUID effects. It found no additional
ordinary action-order mismatch in the bounded algorithm. Sol planning independently
confirmed that early propagation, savepoints, transaction replacement and fake
allocations do not satisfy the agreed invariants. Production was withheld.

R06 still requires a complete compatible pre-effect file-memory footprint, including
new/repeated targets and combined session/scope/code/Git domains, followed by
separately authorized storage guards and stale-vector/lifecycle coverage. R07 still
requires qualified source/locator/alias closure. This attempt grants no source
admission or physical identity, adds no guard, and changes no publication authority.
Old-reader isolation, diagnostic metadata, writer exclusion, atomic publication,
retrieval/index coherence and ambiguous-outcome recovery remain publication blockers.

The smallest next decision is the exact acceptable boundary for fatal-read
observability: the original reconciliation-entry split preserves the original
ordering but occurs after session/scope/version effects; accepting that narrower
prerequisite would need explicit review. Alternatively, a distinct mechanism must
prove pre-effect classification without changing the required error/UUID contract.
Neither option is silently accepted here. Further happy-path tests cannot settle
this concrete conflict. **PARTIAL; publication BLOCKED.**

Automatic approval review refused removal of generated bytecode and reduction of
this task's unused experiment extensions. Those are retained for review rather
than deleted; `scripts/__pycache__/m8-s3e3b-harness.cpython-314.pyc` is an untracked
execution artifact, not a deliverable or source. The complete rejected production
candidate is preserved in the patch and private backups; only hash-verified changes
created by this task were removed from active production source. The index stays
empty. The implementation attempt performed no commit, migration, fencing, alias admission,
ownership change, publication, merge, push or S4. The separately authorized evidence
preservation below permits one signed local evidence commit.


## Rejected-candidate evidence preservation

The preservation baseline is signed `6aeb6a7753d2610837edc30f2a190eeccc6dd017`,
branch `feat/m8-s3d2-semantic-staging`, with an initially empty index. All five
uncommitted evidence-file hashes matched the previous final-scope record before
this task added labels. Production source, migrations and Cargo inputs remain
byte-identical to HEAD; no rejected implementation is being retried.

The [artifact inventory](experiments/m8/s3e4d_evidence_inventory.json) records every
initial Git-visible modified/untracked artifact, original and preservation hashes,
retained private evidence locations, exact native result objects and proposed
exclusions. It is included as the sixth justified evidence file:

| Artifact under `docs/` | Preservation scope / reason |
| --- | --- |
| `m8-s3e4d-reconciliation-planning.md` | Report, failure attribution, coverage limits, publication blockers and explicit DO NOT APPLY labels. |
| `experiments/m8/s3e4d_probe.py` | Existing reproducible archive harness, strict candidate patch scope and freshness/mode safeguards; DO NOT APPLY notice added to its docstring only. |
| `experiments/m8/s3e4d_reconciliation_probe.rs` | Original D01–D04 prefix plus both unused success/nonfatal source additions retained in full. Comment marks additions UNEXECUTED / DO NOT APPLY; no test body changed. |
| `experiments/m8/s3e4d_fatal_read_probe.rs` | Public-receiver baseline/candidate counterexamples. DO NOT APPLY comment is outside both selected blocks; executed block bytes remain exact. |
| `experiments/m8/s3e4d_rejected_candidate.patch` | **DO NOT APPLY TO A WORKTREE.** Raw rejected code and five focused tests, preserved byte-for-byte at SHA256 `51831b1eedb29a1545938b3108400bbdaca22dc431c2de5e20613a0ae333524e`. Disposable replay is its only documented use. |
| `experiments/m8/s3e4d_evidence_inventory.json` | Provenance, precise counterexamples, source-body preservation and explicit commit/exclusion inventory. |

**Proposed exclusion for human review, retained untracked:**
`scripts/__pycache__/m8-s3e3b-harness.cpython-314.pyc`, 26,000 bytes, SHA256
`98c676333df096db78c33899bc818eb2efa7c3a8beb710b0d4ecdf8914f13632`.
Its header matches the signed supervisor source's timestamp/size. Read-only parsing
compared all executable/metadata fields across16 code objects to compilation of
that source: structurally identical, without execution. Raw marshal serialization
bytes differ; no raw payload identity is claimed. It is generated interpreter cache,
not authored evidence or a production modification. It is neither deleted nor staged.
Its final presence is intentional and must be reported after the evidence commit.

Ignored `target/` build outputs and the previously referenced private btrfs evidence
root remain in place, outside the proposed commit. The inventory identifies these
as explicit generated/host-local exclusions, including unused-source backups,
rejected-source backups, invalid/failed verification attempts, qualified logs and
replay manifests. Nothing is deleted or silently discarded. Raw local outputs are
not newly represented as portable artifacts; the retained executable sources,
patch and recorded result objects support independent reproduction.

No unchanged test suite was rerun. This task's checks concern inventory, labels,
source-body equality, existing result/artifact hashes, syntax, links, whitespace
and signed commit scope. Successful prior test execution remains separate from
the demonstrated compatibility failure. The preserved public-receiver values are
exactly **baseline `disk I/O error`, gap4**, and **rejected candidate
`revisioned write required`, gap2**. Both still have logical rollback and cleanup;
that does not grant compatibility or publication safety. Status remains **PARTIAL**;
publication remains **BLOCKED**.

Staging the raw patch exposes eight expected whitespace reports at its lines
6, 102, 119, 163, 222, 496, 498 and 536. Each is exactly one space: the required
unified-diff marker for a blank context line. The patch SHA256 remains unchanged.
The check excludes only this raw fixture and requires every other staged evidence
file to pass; no repository-wide whitespace policy or test assertion is relaxed.
The exact fixture bytes and all eight exceptions are verified separately. This
was a pre-signing scope check, not a GPG failure or a new test execution.
