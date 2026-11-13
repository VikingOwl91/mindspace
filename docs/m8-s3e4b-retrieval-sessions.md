# M8 S3e.4b: transaction-bound legacy retrieval sessions

**ACCEPT for transaction-bound legacy response collection. Publication remains BLOCKED.
No metadata-isolation candidate is selected. Changes are uncommitted for review.**

## Baseline and bounded plan

Verified clean worktree/index on `feat/m8-s3d2-semantic-staging`, signed HEAD
`c00c7d1c1ab3706e976c23a4f24d35426c8c887a`, parent
`6a654138f2409bd4aa5b26e9d84dd4a5d6f72f16`, good GPG signature fingerprint
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.

Read [architecture Decision A](m8-s3e-architecture-decision.md),
[S3e.4a](m8-s3e4a-atomic-settings-reads.md),
[S3e §8](m8-s3e-publication-design.md) and
[S3e.1](m8-s3e1-writer-fencing-design.md).
Sol planning selected a narrow read-only `MemoryRead` interface returned by
`Store::read_session`, retaining the existing connection guard and SQLite
`BEGIN DEFERRED`. Checked finish and rollback-on-drop cover success/error/unwind.
No writable Store is exposed inside a session. Required Rust trait growth has an
explicit external-implementer source-compatibility cost; existing method signatures
and public CLI/MCP contracts are retained.

The final response session includes name/prefix resolution and all database data
collection. A per-Store mutex alone does not protect against commits by another
connection. Preflight metadata/coverage are only hints from an earlier short
session on the same Store connection; runtime verification, initialization and
query embedding occur after it closes. Final settings/profile/coverage validation
precedes candidates. A changed state discards the whole session/result; at most
one restart is permitted. Explicit lexical requests avoid model/settings reads.

## Exact dependency and endpoint inventory

| Response / path | Database dependencies and previous gap | Intended retained boundary |
| --- | --- | --- |
| CLI Search/Recall, MCP memory_search, library lexical entrypoint | Caller get_space(s), spaces/links BFS, FTS memories/assertions/evidence/sources, temporal/lifecycle filters, BM25/limits, separate membership enrichment. | Name resolution through completed hit DTO collection. |
| Semantic | Settings, two index COUNTs, scope, vector/assertion/memory/source rows, cosine ranking/limits, separate membership names. | Final validation through returned DTOs; model computation outside transaction. |
| Hybrid/Auto | Readiness, lexical branch, vector branch and RRF; formerly independent Store calls. | Both branches and all enrichment use one retained connection/snapshot. |
| Cached semantic | Previous runtime cache covers filesystem identities only; DB readiness/candidates remain live. Asset-change fallback previously issued another search. | Fresh final DB session per attempt, complete-result discard before bounded restart; no cached DB authority. |
| CLI Inspect/Trace, MCP memory_get | memory_id_prefix_matches (even full UUID), memories, assertions, evidence, sources, logical-source lifecycle/events, memberships/spaces, replacements and Git provenance/refs. | Prefix uniqueness through full MemoryDetail. |
| History / replacement summaries | Memory/assertion/evidence/source rows followed by separately attached spaces. | Complete summary collection. |
| Conflicts | Active-group query followed by member summaries and spaces. | Group discovery through complete members. |
| Space List/counts | One existing correlated-count statement. | Already a coherent statement; same session when composed. |

CLI opens one Store per command; MCP retains one Store and runtime cache per
transport session. No response-stage second connection was found. Rendering and
JSON/protocol serialization consume owned DTOs and perform no SQL; release the
read transaction before output. `code_search`/symbols are separate code views;
no code ownership or ranking change is part of this slice.

Do not wrap the whole MCP dispatcher: create/update/retire/add-spaces and space
creation/linking compose writes and retain existing transaction/permission semantics.
The read-session interface cannot call them. Doctor/setup retain their separate
native diagnostic/recovery boundary.

## Acceptance and evidence standard

Require deterministic actual-M6 atomic fixture changes across scope, candidates,
enrichment and branches; wholly committed responses; held-snapshot/temporal views;
missing/malformed values, BUSY/read-I/O, mismatch/restart exhaustion and cleanup;
zero read revisions, unrelated writer progress and schema17/guard preservation.
A deliberately separate writer commit may expose a real intermediate state; no
read-session claim makes per-key writes atomic. Synthetic model runtimes qualify
orchestration only, not real-weight inference or publication Q1.

Formatting, Clippy, full suite, targeted debug/release concurrency, genuine default
and no-embeddings builds and independent adversarial review will be recorded here.
No planned check is a passing result.

## Implemented interface, lifecycle and compatibility boundaries

`MemoryRead` is a narrow, object-safe read interface. `Store::read_session` returns
one boxed session borrowing its existing Store. SQLite's implementation owns the
existing connection guard, refuses a nested transaction and executes
`BEGIN DEFERRED`; the first database read establishes its snapshot. It opens no
new connection. SQL was extracted into `*_on(&Connection, ...)` helpers rather
than routing recursively through Store. Thirteen existing read methods also use
the session, making their own multi-query DTOs coherent without requiring callers
to change those existing signatures.

Successful response collection calls consuming `finish`, which checks transaction
cleanup before releasing the guard. Early return and unwind use Drop. If cleanup
fails or SQLite leaves an unexpected transaction state, the handle is discarded.
Each session operation checks that SQLite has not silently auto-aborted the
transaction after a failure: it cannot continue in an implicit fresh snapshot.
There is no new write transaction manager, permission scope or revision.

Model preflight returns ephemeral raw settings and relevant derivation-key index
presence. It closes its short session before artifact hashing, runtime creation
or query embedding. Final collection rechecks those facts inside the response
snapshot. The preflight counts are not cached authority; final candidates and
ranking use current rows in that snapshot. Profile mismatch and cached artifact
replacement share one restart budget; results are discarded and resources released
before retry. A second mismatch returns an explicit concurrent-state error.
Strict-mode unavailable decisions are delayed until final validation, so a stale
unavailable hint cannot bypass that check. Legitimate Auto fallback categories,
explicit strict-mode refusals, raw optional settings and zero-limit behavior remain.

Existing public `search`, `search_with_mode`, `resolve_with` and resolving entrypoint
signatures remain. Named-space entrypoints are crate-private. One new required
Store method and public read-only return interface are the unavoidable Rust
source-compatibility addition for external Store implementers; no CLI/MCP schema
or endpoint changes. Existing supplied Ready capabilities retain a private raw
settings/index profile and refuse stale use; custom manifest/runtime lookup remains.

The lower-level `search_with_mode` still accepts a caller-supplied `Unavailable`
as explicit request policy. It guarantees a snapshot for the database data it
collects, not freshness of an argument obtained in an earlier API call.
`resolve_with` followed later by that lower-level call is not an atomic readiness
and query composition. Callers needing live readiness use the resolving entrypoint.
All actual CLI/MCP resolving and cached paths capture and revalidate unavailable
hints as well as Ready hints. This compatibility boundary cannot justify bypassing
P§8 in any future publication-capable path.

Existing model initialization/checking errors can precede an unknown-name error
when several inputs/storage/assets are faulty simultaneously, because preparation
precedes final name resolution. This does not change healthy results, schemas or
fallback meanings; the stable-result comparison below records the tested cases.
No model initialization runs while the response session is held.

## Deterministic regression coverage

New storage tests use disposable populated schema17 databases, real private M6
`begin_write`/commit scopes and existing guards. One fixture transaction changes
multiple related tables; its committed OLD/NEW states form the oracle. Hooks are
cfg(test), per-thread, one-shot, and assert that their named boundary was reached.
Writers on separate actual Store connections commit synchronously at those
boundaries or through bounded channel barriers. There are no arbitrary sleeps.

| Test group | Oracle |
| --- | --- |
| Separate-statement negative control | Independent scope/search calls can combine states; coherent root search cannot return that split result. This passing characterization records a limitation of separately composed calls, not a fixed writer. |
| Scope/name/candidate/enrichment | Entire operational lexical response equals OLD despite atomic writer commit at scope/candidate/name or enrichment boundary; fresh read equals NEW, with changed IDs/attribution where applicable. |
| Ranking and branches | FTS corpus change cannot change the pinned response's scores. Semantic/Hybrid/Auto vector ranking and both branches remain one state; fresh response positively observes the vector change. |
| Prefix/detail/history/conflicts/counts | Prefix resolution through complete detail, summaries plus memberships, and groups plus members retain one state; counts share a held session. |
| WAL writer / temporal | Channel-gated actual M6 writer commits while reader holds OLD; a new session sees NEW; as-of episode predicates and attribution remain pinned. |
| Cleanup | Early errors release transaction without R. Synthetic cancellation/unwind rolls back and preserves existing mutex-poison refusal; healthy reopen permits ordinary writes. Explicit transaction loss is refused rather than silently creating a new snapshot. |
| BUSY/read-I/O | DELETE-mode exclusive lock produces explicit DatabaseBusy. Existing per-file fault adapter injects actual SQLITE_IOERR_READ through the session; no failure becomes absent settings. No generic power-loss/corruption qualification is claimed. |
| Optional/malformed settings | Raw absent/empty/arbitrary values remain distinct; malformed values error; reads allocate no R. |
| Model/cache orchestration | Factory and embedding callbacks can write via the same Store (proving no retained read lock); unavailable→enabled retries correctly for Auto/Semantic/Hybrid; cached/uncached second mismatch exhausts exactly one restart; cache evicts; supplied policy and stale Ready controls retain their documented meanings. |

Read sessions do not make independent writer transactions atomic. Snapshot
consistency does not promise the newest state, kill old readers, exclude raw
same-UID SQLite access, establish physical non-disclosure or qualify publication.

## Independent adversarial review

Read-only Sol review found no remaining findings after checking hidden second
connections, transaction loss, recursive mutex use, model work and shared retry
budget, composed caller boundaries, cached/uncached differences, unchanged
writer/M6/schema paths, and test oracles. Independent execution copied the current
unit executable, SHA256
`8936b5d0d16a642ed5e5b66b53445a47bd6047803f72462c70f52b86c792bafd`,
and ran 17 session tests twice plus 23 cache/preparation tests once:
**57 passed, zero failed/ignored**. Artifacts:
`/tmp/mindspace-s3e4b-review-d8drs99p`. Broad-suite/build acceptance is recorded
separately; those targeted results do not replace it.

## Verification setup and recorded failures

Primary local artifacts are `/tmp/mindspace-s3e4b-0lclr0c_`, containing bounded
command records, isolated HOME/XDG/TMPDIR environments and stdout/stderr. The
existing adversarial harness process supervisor is reused with continuous adopted
zombie reaping, excluding Cargo's own wait status. Two build jobs, development
debug information disabled, explicit Cargo/Rustup paths and `--offline` were used.
These are transient evidence locations, not required committed inputs.

The first targeted test compilation failed with E0277 at
`read_session_tests.rs:619`: `Result<Vec<u8>, Error>: ToSql` was not satisfied.
The synthetic fixture had passed `encode_f32_le`'s Result into SQL parameters;
it now unwraps successful fixture encoding. No production fix was involved;
`session-targeted` preserves the failed log, and `session-targeted-2` records
17 passing tests afterward.

The first transient no-embeddings smoke assertion incorrectly expected literal
"feature disabled" in the strict-mode human diagnostic. Actual output was
"requested retrieval mode unavailable: this build has no embedding support;
rebuild with --features embeddings". The probe was corrected to the existing
message and structured Auto `feature_disabled` reason; all four expected outcomes
passed in `noemb-runtime-checked`. No repository code changed for that correction.

## Invocation and verification matrix

From this worktree:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --no-fail-fast
cargo test --locked --lib storage::sqlite::read_session_tests -- --test-threads=1 --nocapture
cargo test --locked --lib retrieval::semantic::cache::tests -- --test-threads=1 --nocapture
cargo test --locked --release --lib storage::sqlite::read_session_tests -- --test-threads=1 --nocapture
cargo test --locked --release --lib retrieval::semantic::cache::tests -- --test-threads=1 --nocapture
cargo build --locked --bin mind --message-format=json
CARGO_TARGET_DIR=target/s3e4a-no-embeddings cargo build --locked --no-default-features --bin mind --message-format=json
```

The dedicated no-embeddings target directory is an existing cache name, not a
source/version claim. Cargo JSON compiler artifacts must show root library/binary
features exactly `[]` for that build and `["default", "embeddings"]` for ordinary
production, with no spawn-gate. The self dev-dependency can re-enable default
features for tests; a no-default-features test label would not prove this.

| Check | Result |
| --- | --- |
| Formatting | PASS. |
| Initial all-target compile | PASS before final added tests; the subsequent fixture compile correction is recorded above. |
| New storage/session tests, debug | 17 passed, zero failed/ignored. |
| Cache/preparation tests, debug | 23 passed, zero failed/ignored. |
| All-target Clippy, warnings denied | PASS. |
| Full locked regression suite | Exit 0: 1,295 reported passes, zero failures, 34 ignored across 71 target summaries including doctests. Runtime gaps below are not exercised passes. |
| Optimized storage/session concurrency | 17 passed, zero failed/ignored. |
| Optimized cache/preparation | 23 passed, zero failed/ignored. |
| Genuine no-embeddings production build | PASS; root library and binary feature lists `[]`. |
| Genuine no-embeddings runtime | Four expected outcomes verified on disposable populated DB: lexical hits; Auto disclosed feature_disabled lexical fallback; Semantic/Hybrid explicit no-support refusal and empty stdout. |
| Default production build / stable binary comparison | PASS; root library/binary features exactly `["default", "embeddings"]`, no spawn-gate. 42 CLI cases byte-equal and 13 parsed MCP responses equal to the independently compiled signed baseline. |
| Independent review | No findings; 57 targeted executions passed, zero failed/ignored. |

Verification uses Rust/Cargo 1.98.1, bundled SQLite 3.53.2 and Linux 7.2.14-zen.
New fixtures run on tmpfs; source/build cache is on btrfs. This does not qualify
btrfs/tmpfs recovery across power loss or all filesystem/kernel configurations.
No live database, user model directory or secret was supplied. Synthetic runtime
coverage establishes session orchestration, not real-weight model compatibility.

## Stable populated-database comparison

A separate immutable `git archive` of signed baseline
`c00c7d1c1ab3706e976c23a4f24d35426c8c887a` was built as an ordinary production
binary using locked offline dependencies. Baseline SHA256:
`17c83547257110cd0f9eb556e60f6db6b8838d02a849a6b5b9564cfdef933ec6`.
Current ordinary production binary SHA256:
`6714b15a794b57fcc68039535d088571e314601e25e84fb5519ec4dd5b68a465`.
Both root library/binary feature lists are exactly default+embeddings; no test hooks.

Seed a disposable DB using baseline CLI: create `root` and `child` spaces, link
root→child, and ingest two synthetic files into child. Both files have heading
`Shared` and content containing `sessionstablemarker`, with differing text.
Use the **same populated database and isolated HOME/XDG/TMPDIR** for both binaries,
so UUIDs, timestamps, source paths and scores are directly comparable.

Reproduction recipe (no live DB or model installation required):

1. Archive that exact baseline revision into a fresh directory and build with
   `cargo build --locked --manifest-path <baseline>/Cargo.toml --bin mind --message-format=json`
   using a separate target directory. Build the current binary normally and retain
   compiler feature records for both.
2. Seed the fixture above with explicit `--db <fixture>/mind.db`. Compare complete
   `(exit code, stdout, stderr)` for Search/Recall with mode omitted and each of
   Lexical/Semantic/Hybrid/Auto, in text and JSON; temporal search, include-retired,
   zero-limit Auto and missing-space error; Inspect/Trace with full, unique-prefix
   and ambiguous-prefix IDs, in text/JSON; History, Conflicts and Space List in
   text/JSON. These are the 42 CLI cases recorded here.
3. Run each binary's MCP server with the same fixture and explicit DB. Send
   initialize, initialized notification, tools/list, five memory_search mode cases,
   three memory_get ID cases, memory_history, memory_conflicts and space_list.
   Compare parsed JSON responses including errors: 13 equal responses.
4. Hash every row of each non-SQLite-internal application/index table before and
   after the probes. All recorded table digests are unchanged. This is logical
   application/index preservation, not proof of filesystem or journal immutability.

All comparisons passed. The fixture has no selected model, so those process probes
qualify exact lexical/default behavior and explicit unavailable/fallback contracts,
not real-weight successful inference. Existing retrieval regressions plus the new
synthetic-runtime concurrency tests cover successful semantic/fusion scores and
orchestration. Logs/results are in `stable-fixture`, with the local probe recipe
in `compare.py`; these transient files are evidence, not required repository tools.
The repository unit/integration suites remain the reusable regression infrastructure.

## Ignored tests and runtime coverage limits

The full run retained 34 ignored cases: six model/weights-dependent cases, ten
opt-in resource/heavy cases, four separately preserved schema16-binary cases,
twelve S3e.3b scenario/native-actor entrypoints and two independently built
production-artifact staging probes. Exact names/reasons are in
`full-test/summary.json` and stdout. No new session/preparation test is ignored.
These unchanged gates were not promoted into newly executed evidence.

Runtime logs also retain exactly these coverage messages:

- `skipped: set MINDSPACE_E2E_REPO=<repo dir> to run against a real repository`.
- Twice in `mcp_git_isolated`: `SKIPPED: no watchdog appeared`.
- `UNKNOWN: embeddings not exercised (set M72D_MODEL_SRC to an installed models directory)`.

These branches are not covered merely because their test returned OK. No external
repository or real installed model directory was provided. The expected
no-embeddings feature refusal is distinct from an unavailable test environment.
The deliberate caught panic in the cleanup test is a passing cancellation oracle,
not a failed full-suite test. No all-environment, real-model Q1, physical recovery
or old-client exclusion acceptance follows from this run.

## Exact change inventory

| Files | Responsibility |
| --- | --- |
| `src/storage/mod.rs` | Narrow public MemoryRead and required Store session factory. |
| `src/storage/sqlite.rs`, new `sqlite/read_session.rs` | Existing-connection lifecycle, SQL extraction, coherent standalone getters, cfg(test) boundaries. All other existing Store method bodies compare byte-identical to signed baseline. |
| New `src/storage/sqlite/read_session_tests.rs` | 17 deterministic session/M6/endpoint/error controls. |
| `src/storage/sqlite/settings_tests.rs` | Only sibling visibility for existing test-only per-file I/O adapter; no production fault hook. |
| `src/retrieval/mod.rs` | Response collection/session routing, precomputed vectors, named scope and bounded revalidation. |
| `src/retrieval/semantic.rs`, `semantic/cache.rs`, `semantic/cache_tests.rs` | Coherent preflight, external preparation, private ephemeral profile, cached/uncached retries and six new regressions. |
| `src/lib.rs` | Prefix-plus-detail response helper; existing public prefix resolver retains behavior. |
| `src/cli/mod.rs`, `src/mcp/tools.rs` | Integrate only memory read responses; no whole-dispatch wrapper, mutation or definition change. |
| This report | Inventory, verification, explicit limitations and remaining blockers. |

Thirteen files total. Migration/Cargo inputs, core stored formats, owner tables,
M6 authorization/context/commit/rollback machinery and all existing Store mutation
bodies remain unchanged. No active index write or implicit ownership effect occurs
on read. No test assertion treats separate committed setter updates as one atomic
writer operation.

## Remaining publication blockers

This advances the existing legacy-response component of R05; it does not implement
future publication heads, owner/cohort predicates, projection maps, strict query
profiles, document citations or transaction-bound dual-layout selection.
[Architecture R01–R17](m8-s3e-architecture-decision.md) remains authoritative:

- R01/R02: exact publication-private intent/authorization and candidate/base validation.
- R03/R04: old diagnostic metadata disclosure, existing old-client active-knowledge
  access and Q10; no metadata mechanism or critical-memory cutover is selected here.
- R05/R12: future head/eligibility/projection/citation validation within the supported
  response session still needs integration; current legacy provenance is not new
  document-citation acceptance.
- R06/R07: complete affected receiver/SQL guards, stale jobs and bounded source/alias closure.
- R08: full guard/inventory/administrative equivalence for any later selected domain/layout.
- R09/R10/R11: exact complete projection/membership/lifecycle/FTS, mandatory generic
  vector-route guards, and strict Q1 compatibility wherever semantic coverage requires it.
- R13/R14: authorized atomic replacement/fence/index/head/receipt operation and
  exact durable outcome/replay integrity. Missing acknowledgement remains UNKNOWN,
  never evidence of rollback or process death.
- R15/R16: qualified physical preparation/recovery and admitted complete resource
  profiles; WAL retention and worst-case SQL work are not globally bounded by this slice.
- R17: source-only refresh and complete updates/reversion before enabling those capabilities.

Model A still trusts the local account/process/kernel/SQLite. Raw same-UID SQLite
is outside admission-respecting old-client exclusion. No new safety claim follows
for old handles, diagnostic output or physical files from a successful read snapshot.
No migration 0018, quarantine, active fence, publication, ownership change, new
strict profile, MCP endpoint, merge, push or S4 is part of this work.

## Final verification boundary

No production or test source changed during final targeted, broad, release, build
and independent-review verification; recorded source hashes are checked again at
completion. The only later edits complete this report. Final checks retain signed
baseline HEAD/branch, an empty index, exact thirteen-file scope, valid local links
and clean Git whitespace. All implementation/documentation changes are left
uncommitted for review.
