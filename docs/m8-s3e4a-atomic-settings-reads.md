# M8 S3e.4a: atomic legacy settings reads

**ACCEPT for the bounded atomic-settings-read slice. Publication remains BLOCKED.
Neither metadata-isolation candidate is selected. Changes are uncommitted.**

## Verified baseline and change scope before implementation

Verified clean worktree/index on `feat/m8-s3d2-semantic-staging`, signed HEAD
`6a654138f2409bd4aa5b26e9d84dd4a5d6f72f16`, exact parent
`7f698806c8962d2cedd00b7212ee30158dbffff6`. GPG reported a good signature from
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.

Read with the accepted [architecture Decision A](m8-s3e-architecture-decision.md),
[S3e publication](m8-s3e-publication-design.md),
[S3e.1 fencing](m8-s3e1-writer-fencing-design.md) and
[S3e.3e actual-M6 seam](m8-s3e3e-m6-routing-seam.md).
A read-only Sol planner confirmed the following bounded production scope:

| File / boundary | Authorized change |
| --- | --- |
| `src/core/mod.rs` | Named raw `EmbeddingSettings` pair, preserving two independent `Option<String>` values. Required because the Store contract traffics in core types. |
| `src/storage/mod.rs` | One required object-safe `read_embedding_settings` method; no two-getter default or transaction handle exposed. |
| `src/storage/sqlite.rs` | One private connection-scoped SELECT helper and one Store implementation acquiring the real connection guard once; test-module declaration. |
| `src/embeddings/manager.rs` | Crate-private fallible pair interpretation for operational callers; exact `"true"` semantics. |
| `src/retrieval/semantic.rs` | Fetch once in shared readiness. Existing cache uses this function; no query/cohort/index integration. |
| `src/cli/mod.rs` | Fetch once in `IndexCommand::Run`, propagating storage errors. Preserve manifest/install/runtime checks. |
| Storage/caller tests | Disposable real schema17/M6 fixtures, concurrency and error controls, ordinary-operation regressions. |
| This report | Inventory, executed verification, independent review and remaining blockers. |

Before this change each `get_meta` locks one Store connection for one SELECT.
Two calls can span another connection's commit. `read_enabled`/`read_selected_model`
are separate fallible getters; the display wrappers swallow errors. Semantic
readiness uses the fallible getters separately; CLI index-run incorrectly uses the
display wrappers for an operational decision. Cached readiness invokes the same
shared function. Indexer mechanics receive a selected runtime/derivation and are
not coupled settings readers.

`mind_meta['embedding.enabled']` is enabled only for exact `"true"`.
`embedding.model` is an optional raw identifier; absence, empty/arbitrary text and
invalid storage types are distinct. Model manifest/install validation remains in
existing callers. No default model or disabled state may hide a storage failure.

Current `set_meta` owns real M6 IMMEDIATE, active-R authorization and commit.
It remains unchanged. The Store has one mutex-protected connection, not a pool;
separate Store instances use separate SQLite connections. Reads allocate no R.
There is no metadata read ACL or application quota to introduce. Ownership/admin
permits, guard inventory, schema17, generic keys, model validation and write
formats remain in force.

No changes to best-effort displays, multi-step enable/update writes, diagnostic
inspection/setup, metadata layout, receipts/generations/cache, ownership, memory
visibility, vector routing or MCP endpoints are planned. Public API growth is
limited to the required Store operation and its named core return type; external
Store implementers must implement the coherent operation rather than inherit an
unsafe default. This is a source-compatibility cost, not a new network/MCP API.

## Acceptance contract

1. Each pair comes from one SQLite statement snapshot; the private helper can
   run on a caller-owned read transaction without taking another mutex/transaction.
2. Preserve raw optional/empty/arbitrary values and healthy caller outcomes;
   missing authority, malformed values, BUSY and I/O errors remain errors.
3. Controlled actual M6 pair updates expose whole committed states. Separate
   per-key write transactions may intentionally expose an intermediate committed
   pair; this read API does not make those writes atomic.
4. Reads consume zero R; actual setters/guards/rollback and unrelated memory/FTS
   behavior remain unchanged. No schema migration or implicit write on read.
5. Verify actual coupled operational consumers, default/no-embeddings builds,
   requested fmt/Clippy/full-suite checks and independent review. Record skips,
   failures and unavailable environments explicitly.

The single SELECT is the production snapshot mechanism. No new writer manager,
quarantine, physical relocation, active fence or publication operation exists.

## Production inventory and implementation

| Path | Existing boundary / invariant | Final change |
| --- | --- | --- |
| `SqliteStore::connection` | One mutex and optional SQLite connection per Store; no pool. Discarded handles error. Separate Stores have separate connections. | Unchanged; one guard per pair read. |
| `get_meta` / manager single-key getters | One statement per call; raw optional text; separate calls can span commits. Display wrappers intentionally use best-effort false/None. | Unchanged for independent operations and displays. |
| `read_embedding_settings_on` / `Store::read_embedding_settings` | New bounded read contract. | One SELECT with two scalar subqueries over `mind_meta`; both use the same SQLite statement snapshot. Missing rows produce SQL NULL/`None`; malformed column types or missing table remain errors. |
| `manager::read_settings` | Shared operational interpretation. | Crate-private, fallible; only exact `"true"` enables; raw model remains optional. No catch-to-None. |
| `semantic::readiness` | Shared by cached and uncached resolution, including affected existing MCP query paths. | One pair read before existing readiness/manifest/install checks; cache cannot mask a malformed setting. No new endpoint or retrieval policy. |
| CLI `IndexCommand::Run` | Previously used two display wrappers for an operational decision. | One fallible pair; genuine disabled/no-model/unknown/not-installed outcomes remain. Storage failure is now reported instead of masquerading as disabled. |
| `set_meta`, enable/model setters | Each set has its own real M6 IMMEDIATE transaction and one recording revision R. Model setters retain manifest/install validation. | Byte-identical production bodies; no new pair writer. CLI enable remains a multi-step workflow with separate commits. |
| Actual M6 | `begin_write`, connection-local active-R UDF, persistent trigger inventory, commit/rollback hooks, known-noncommit versus UNKNOWN. An ordinary R is not an administrative permit. | Production transaction, context and commit bodies unchanged. Reads allocate no R; no metadata read ACL/application quota exists to duplicate. |
| Ownership / permission boundaries | Existing exact administrative permits and protected table guards; callers do not gain authority by reading settings. | Unchanged. No new table, permit, inventory entry or implicit migration. |
| Retry / prepared statements | No new retry or statement cache. One `query_row` prepares/steps/finalizes within the guard. Existing write UNKNOWN handling remains. | Read errors flow through existing `dberr`; no retry, receipt or commit inference added. |

The production SELECT is SQLite's existing implicit read transaction, rather
than two autocommit statements inside a process mutex. The private helper also
accepts an already-owned `Connection`/read transaction, demonstrated without a
second lock or hidden transaction. This is not a public query-session API.
Model assets, coverage and subsequent search/index operations are still separate
observations. No entire-query consistency claim follows from the pair API.

The API deliberately reads both fields even when enablement is false. A malformed
model value therefore errors instead of being hidden by a disabled short circuit.
Legitimate absence and arbitrary *text* retain their existing meanings. Explicit
lexical querying remains available without invoking semantic readiness.

Exactly ten files comprise this slice: the six production files listed in the
initial scope, `src/storage/sqlite/settings_tests.rs`,
`src/retrieval/semantic/cache_tests.rs`, `tests/cli.rs`, and this report.
`Cargo.toml`, lockfile, migrations, MCP modules, ownership and M6 implementation
bodies are unchanged. The added malformed-value fixture and SQLite I/O fault
adapter compile only under `cfg(test)`.

## Executable acceptance matrix

All fixtures use generated synthetic text and disposable populated databases.
No test needs the developer's live database, home directory or installed model
weights. The pair-writing fixture invokes actual private M6 `begin_write` and
commit; it is not a production API or fake authorization UDF.

| Oracle / exact test suffix | Classification | Evidence and limit |
| --- | --- | --- |
| `atomic_settings_preserve_raw_optional_values_and_read_without_recording_revision` | Regression | Absent, one-sided, empty, arbitrary/unicode/control-containing text; schema17, unrelated key and guard inventory preserved; repeated reads add no R. |
| `atomic_settings_do_not_change_write_guards_accounting_or_unrelated_knowledge` | Regression | Actual setters add exactly one R each; unmanaged update rejected; rollback removes pair changes and R; ordinary ingestion and FTS still work. Not a new metadata ACL/quota. |
| `connection_scoped_settings_helper_keeps_the_callers_held_snapshot` | Regression | Read transaction pins OLD while another actual Store commits NEW; after transaction end, NEW is observed. No query-wide supported session is claimed. |
| `reader_sees_old_pair_while_real_m6_writer_has_half_or_all_changes_uncommitted` | Regression | Channels explicitly pause after first update, after second update and after commit. Separate reader sees OLD, OLD, NEW. No sleep establishes ordering. |
| `concurrent_atomic_pair_commits_never_produce_a_split_observation` | Regression | Two positively acknowledged states, then 256 atomic M6 pair commits; every sample must be wholly A or B. Bounded 15-second protocol; finite repetition supports reproducibility, not absence of all races. |
| `coherent_read_does_not_make_separate_setting_writes_atomic` | Limitation characterization | Deliberate separate setters expose the real committed intermediate pair. Separate getters also combine observations across commits. Passing records this limitation, not a repaired writer. |
| `malformed_settings_and_missing_storage_return_errors_not_absent_values` | Regression | BLOB in either setting and dropped authority table return errors, never optional absence; no R added by read. |
| `busy_read_is_reported_without_a_revision_and_recovers_after_lock_release` | Regression | Disposable DELETE-mode connection held EXCLUSIVE, 20ms test timeout: DatabaseBusy; release then successful pair, no extra R. Production WAL/open configuration is unchanged. |
| `actual_sqlite_read_io_error_propagates_instead_of_missing_settings` | Regression | Force MAIN read; per-file test callback returns actual SQLITE_IOERR_READ, with positive callback count and engine extended-code assertion; restoration allows successful reread. Not generic corruption/FULL/power-loss coverage. |
| `malformed_model_setting_is_not_disabled_for_cached_or_uncached_readiness` | Caller regression | Real M6 malformed model fixture after warming cache; uncached and cached Auto/Semantic/Hybrid resolution error, no runtime/query, cache invalidated; lexical still works. |
| `index_run_preserves_legitimate_settings_outcomes` | CLI regression | Seven synthetic absent/disabled/case/arbitrary/empty/unknown/not-installed cases retain diagnostic categories. |
| `index_run_propagates_settings_storage_errors_instead_of_disabled` | CLI regression | Isolated schema17 fixture with deliberately malformed value column errors as storage failure, never disabled/no-model. |

The I/O fixture replaces only one exclusively held connection's file-method table;
it does not register a process-global VFS. It checks the live file pointer and
method version, ties its lifetime to the connection, and restores the original
pointer before freeing its copied table, including unwinding. Independent review
specifically inspected this unsafe test boundary. It exercises an actual read
fault, not a dropped-table error relabeled as I/O.

## Reproduction and verification

From the worktree, run:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --no-fail-fast
cargo test --locked --lib storage::sqlite::settings_tests -- --test-threads=1 --nocapture
cargo test --locked --release --lib storage::sqlite::settings_tests -- --test-threads=1 --nocapture
cargo test --locked --lib malformed_model_setting_is_not_disabled -- --nocapture
cargo test --locked --test cli index_run_ -- --nocapture
cargo build --locked --bin mind --message-format=json
CARGO_TARGET_DIR=target/s3e4a-no-embeddings cargo build --locked --no-default-features --bin mind --message-format=json
```

For broad process tests use a normally reaping init or a subreaper that continuously
reaps adopted zombie descendants; contain and terminate only owned processes.
Run with disposable HOME/XDG/TMPDIR, preserve the real toolchain through explicit
CARGO_HOME/RUSTUP_HOME, and pass explicit disposable `--db` paths to manual CLI
probes. Tests managing fixture Git repositories may need process-local signing
configuration disabled; no user/global Git configuration was changed here.

This run additionally used `--offline`, two Cargo build jobs, debug information
disabled for development builds, and the existing adversarial harness's bounded
process-group supervisor. Logs, command/environment records and outcomes are in
`/tmp/mindspace-s3e4a-T9OvT8`; this is a local transient evidence directory, not a
committed artifact or prerequisite for independently running the tests. Toolchain:
Rust/Cargo 1.98.1, bundled SQLite 3.53.2, Linux 7.2.14-zen. New default test fixtures
used isolated tmpfs temporary directories; no btrfs/power-loss qualification is
claimed for the new tests.

| Gate | Executed result |
| --- | --- |
| `cargo fmt --check` / `git diff --check` | PASS. |
| Clippy, locked/offline, all targets, warnings denied | PASS. |
| New storage tests, debug, serial | 9 passed; zero failed/ignored. |
| Corrected full suite, locked/offline, no-fail-fast, default test parallelism plus nocapture | Exit 0: 1,272 reported passes, zero failures, 34 ignored across 71 target summaries including doctests. Runtime skip/unknown branches below are not independently exercised passes. |
| Broker supervision recheck | 15 passed, zero failed/ignored, serial; corrected full parallel broker target also passed all 15. |
| Targeted optimized storage/concurrency | 9 passed, zero failed/ignored; release profile, 0.57s test runtime. |
| Default production binary / effective features | PASS; root library and `mind` features are exactly `["default", "embeddings"]`, no spawn-gate. The two CLI controls also pass against this ordinary production binary. |
| Genuine no-embeddings production binary | PASS; compiler-artifact feature lists for root library and `mind` are both `[]`, including no spawn-gate. |
| Genuine no-embeddings runtime | Isolated init, doctor (explicit no-embeddings skip classification), expected disabled index-run; separate init/space/ingest/lexical-search flow passed. No inference claimed. |
| Independent targeted repetitions | 30 passes, zero failed/ignored; see review section. |

The 34 ignored cases are: six model/weights-dependent cases, ten opt-in resource
or heavy measurements, four separately preserved schema16-binary compatibility
cases, twelve S3e.3b process/native scenario or actor entrypoints, and two
independently built production-binary staging probes. Their exact names/reasons
are preserved in `full-test-reaped/summary.json` and stdout. These unchanged
specialized harness/model/resource gates were not run by this slice; historical
claims were not promoted to new evidence. The new settings/caller tests have no
ignored cases.

Additionally the full-run logs explicitly record:

- `skipped: set MINDSPACE_E2E_REPO=<repo dir> to run against a real repository`.
- Twice, in `mcp_git_isolated`, `SKIPPED: no watchdog appeared`; those optional
  process-start paths were not exercised despite Rust reporting their tests OK.
- `UNKNOWN: embeddings not exercised (set M72D_MODEL_SRC to an installed models directory)`.

No external repository or local model directory was supplied to close these
coverage gaps. Effective build features come from Cargo JSON compiler artifacts,
not from the command name: the repository's self dev-dependency can re-enable
default embeddings/spawn-gate for tests, so a no-default-features test label
alone would not establish a genuine no-embeddings build.

### Initial supervision failure, retained evidence

The first full-suite attempt was interrupted and is **not** a passing run.
Six broker lifecycle tests failed: `a_start_that_fails_after_the_fork_leaves_no_watchdog_broker_or_tree`
("watchdog or broker outlived the failed start"),
`watchdog_sigkill_is_a_double_fault_with_a_bounded_leak_that_the_next_start_sweeps`
("the sweep did not end the descendant"),
`broker_sigkill_leaves_nothing_running_and_needs_no_restart`
("processes or cgroup tree left behind"),
`no_zombie_watchdog_after_normal_shutdown_or_a_broker_crash`
(`settle(...).is_some()` assertion),
`frontend_sigkill_leaves_nothing_running` ("processes or cgroup tree left behind"),
and `sequential_operations_clean_up_after_each_one_and_shutdown_leaves_nothing`
("watchdog/broker/tree left"). Full diagnostics remain in `full-test` logs.

The transient supervisor adopted child processes but deferred zombie reaping until
Cargo exited. `/proc` metadata captured in `initial-orphan-observation.json`
confirmed failed-test watchdog/descendant zombies parented to that supervisor;
a dead watchdog retained as a zombie also prevented the next-start sweep.
The owned run was stopped with its teardown handler and those PIDs were verified
gone. Only the transient supervisor was corrected to reap its adopted zombie
children continuously, explicitly excluding Cargo's own wait status. No repository
source/test was changed to obtain a pass. The broker target then passed all 15
tests serially; a fresh full-suite run uses normal test parallelism and captures
skip diagnostics with `--nocapture`.

## Independent adversarial review

A read-only Sol reviewer inspected the final production/test diff and acceptance
contract without implementation reasoning, including hidden transaction edges,
error-to-None conversion, M6 boundaries and the test-only unsafe I/O lifetime.
No findings remained. Independent execution copied the compiled unit binary,
verified identical SHA256
`5083e2872d1f2341b57a56502453e84fee38eafe0e0a94e10252ebefb869a0de`,
and ran all nine storage tests three times (27 passes), the cached/uncached caller
control once and the two CLI controls: **30 targeted passes, zero failures or
ignored cases**. Independent artifacts are
`/tmp/mindspace-s3e4a-independent-u3f0lqq8`. The reviewer also checked the report claims and found no corrections. Broad-suite/
build acceptance is recorded separately above; targeted success is not a substitute.

## Remaining publication blockers

[Architecture Decision A's R01–R17 matrix](m8-s3e-architecture-decision.md)
remains authoritative. This slice only supplies the settings-pair component of
R05 and preserves existing R08 machinery. It clears none of the joint publication
gates:

- R01/R02: exact publication authority, intent, candidate/base validation.
- R03/R04: old diagnostic metadata disclosure and old active-knowledge access;
  Q10 is unresolved. Neither metadata-isolation Candidate A nor B is selected.
- R05: same-session settings, scope, candidates, coverage, head and citation/detail
  coherence across the full query remains unimplemented.
- R06/R07: affected legacy writers, late vector writes and complete source/alias
  closure still require qualification and exact guards.
- R08/R09: any future selected-layout administrative/guard equivalence and complete
  projection/cohort/lifecycle transfer remain unqualified.
- R10/R11/R12: publication FTS, mandatory vector-route protections, applicable strict
  Q1 coverage and exact same-session citations/details remain unimplemented.
- R13/R14: joint atomic publication and exact outcome/replay linkage remain absent;
  no missing acknowledgement is treated as rollback.
- R15/R16: physical recovery/compatibility and bounded complete admission remain
  unqualified. This read fault test does not qualify journal recovery or power loss.
- R17: source-only refresh and complete later update/reversion remain separately
  gated before those capabilities are enabled.

No active fence, migration 0018, metadata layout, ownership/visibility change,
publication or new MCP surface is introduced. All changes remain uncommitted for
review. The smallest follow-up remains a separately scoped decision about the
full supported read-session boundary; it cannot be inferred from this pair API.

## Final scope verification

Final checks retain baseline HEAD/parent/branch and good signature, an empty index,
and exactly the ten-file scope above. The nine production/test source hashes
recorded after review match the final files. Relative Markdown links and Git
whitespace checks pass. No production source changed during the broad/release
verification; only this evidence report was completed afterward. Full-suite
supervision artifacts and environmental skips remain disclosed above.
