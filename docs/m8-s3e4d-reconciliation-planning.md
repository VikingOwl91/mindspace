# M8 S3e.4d: pre-effect file-memory reconciliation

**PARTIAL — production implementation stopped under the clarified observable-effects condition. Publication remains BLOCKED.**

No production reconciliation seam was installed. This slice establishes the exact
behavior inventory and executable baseline counterexamples. Passing probe cases
mean those baseline behaviors reproduced, not that pre-effect reconciliation or
writer exclusion has been implemented.

## Baseline and scope decision

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

Only this report and two reproducible experiment sources are added. All tracked
production, migration, Cargo, existing test and API files remain unchanged.

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

## Reproducible evidence

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
push or S4. All new evidence files remain uncommitted for review.

## Verification and independent adversarial review

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
