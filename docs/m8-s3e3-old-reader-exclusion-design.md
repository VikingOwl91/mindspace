# M8 S3e.3: old-reader exclusion design reconstruction

**RECONSTRUCTED — original uncommitted design lost. Requires independent review.**

Reconstruction date: 2026-11-11. This is a new design record assembled from
committed predecessors, current source inspection and surviving historical
reports. Neither byte identity nor semantic identity with the lost original is
claimed. Unknown details remain unknown rather than being filled in as history.

**Disposition: publication BLOCKED; historical Decision B retained: the diagnostic
exception was rejected.** Two-key metadata quarantine and a compatibility-floor /
inode-drain protocol are candidates requiring proof and separate acceptance.
Neither is installed, selected as sufficient, or authorized by this document.

## 1. Baseline, scope and evidence vocabulary

The recreated worktree is
`/home/cnachtigall/git/projects/mindspace-wt/m8-s3d2`, branch
`feat/m8-s3d2-semantic-staging`, HEAD
`54e5dff124f87949e23a05e40d65aa22a274a588`. During reconstruction, Git reported a
clean initial worktree and a good OpenPGP commit signature. Its direct predecessors
are S3e.1 `f78e413fe16b4781db5e3594712f45250bf0fa22`, S3e
`a68beb7fe6ea7a45858a7b3106492805dd082fdb` and semantic staging
`254d1ad5794db8ac19dc187a5934910337da9b9e`.

Read with [S3e publication](m8-s3e-publication-design.md),
[S3e.1 writer fencing](m8-s3e1-writer-fencing-design.md) and
[S3e.2 passive infrastructure](m8-s3e2-fencing-infrastructure.md).
The lost S3e.3 file was never committed. The user's recovery brief supplies the
historical findings H01–H10 below. A surviving secondary investigation note
(thread `01a1c50d-e9be-7341-87ea-6e443d0feaed`) also reports Decision B and the
diagnostic/SHM/drain findings; it is not raw experimental evidence.

| Label | Meaning in this document |
| --- | --- |
| STATIC | Source or Git metadata inspected during this reconstruction; no runtime execution implied. |
| CONTRACT | Requirement or design direction in a committed predecessor; not implementation proof. |
| HISTORICAL | User-supplied surviving report, sometimes also recorded in predecessor documentation or the secondary note; not independently rerun here. |
| CANDIDATE | Newly reconstructed proposal or proof obligation, not a recovered accepted protocol. |
| UNKNOWN | Evidence missing, not inspected, or insufficient to decide. |

Only this Markdown document is added. No production code, schema, ordinary write
behavior, retrieval result or MCP API changes. No migration 0018, active fence,
document ownership, publication, merge, push or S4. S3e.3b is recommended below,
not started. No live Mindspace database, real secrets or runtime fixtures were
used. Website-compliance applicability was considered: the inspected product is
a local CLI / stdio MCP service; this document changes no website surface. No
website compliance or legal conclusion is claimed.

## 2. Exclusion objective and retained publication contract

CONTRACT: S3e section 8 and S3e.1 section 7 require supported clients to use one
coherent SQLite snapshot per logical query, including settings, scope, head,
eligibility, ranking and citations. A published asset has at most one eligible
active representation cohort. An unsupported old client must finish an entirely
old pinned snapshot or fail explicitly; it must not return a successful mixed or
partial active result. This is not process termination or retroactive erasure of
already read data.

The committed candidate is a physical critical-relation rename, for example
`memories` to `memory_episodes_pub_v1`, without an old-name compatibility alias.
It requires populated migration and actual old-binary path proof. It is not
implemented. A compatibility floor plus drain witness does **not** silently
replace that selection. Whether it could supplement or replace the rename is
unresolved and requires an explicitly accepted revision of the predecessor
contract. Operator shutdown attestation remains insufficient automatic proof.

Permanent affected-writer guards, legacy retirement, complete visible document
replacement, indexes, effective head and receipt must commit together. A failed
first publication leaves legacy knowledge writable and visible. Passive plans
cannot reserve aliases, deny writes or grant publication authority. Supported
unrelated work must remain available. A database-wide unsupported-client upgrade
boundary is distinct from asset-scoped writer fencing.

Decision B prevents treating diagnostic metadata as an accepted exclusion
exception. Full knowledge and mutation isolation remains unproved. This does not
assert that old diagnostics have been shown to read active memory/document
contents; the demonstrated category in the surviving report is arbitrary model
setting text, which can itself contain sensitive metadata.

## 3. Current architecture and evidence provenance matrix

The following source paths and symbols were inspected at the stated HEAD.

| ID | Claim and source | Evidence / limitation |
| --- | --- | --- |
| S01 | [SQLite core](../src/storage/sqlite.rs): `CURRENT_SCHEMA_VERSION = 17`, `init_conn`, `reject_future_schema`, `connection`. | STATIC: fresh initialization rejects a higher registered migration version; `connection()` only checks mutex/discarded state, without repeating compatibility admission. WAL setup precedes future-schema rejection; rejection is not a zero-filesystem-effects proof. |
| S02 | [Migration 0017](../migrations/0017_passive_fence_plans.sql), `run_migrations`, [passive plan core](../src/storage/sqlite/fence_plans.rs). | STATIC: three passive plan/binding/receipt tables and guards on those new tables. Serialized IMMEDIATE migration, recording revision, schema marker and inventory validation; no active memory relation rename or old-handle eviction. |
| S03 | [Compatibility tests](../tests/fence_plan_compatibility.rs): `actual_old_open_readers_and_writers_survive_passive_migration_but_fresh_opens_refuse`, queued-write, interrupted-migration and explicit-checkpoint cases. | STATIC: executable tests exist for those behaviors. Their assertions and infrastructure were inspected; none was run in this reconstruction. |
| S04 | `inspect_database` / `read_inspection` in SQLite core, `run`, `check_database`, `check_embeddings`, `render_text` in [doctor](../src/doctor/mod.rs). | STATIC: inspector reads versions and two raw settings before returning; doctor reports incompatibility yet retains inspection for later checks. Embeddings-enabled builds render the raw selected model in details, even when embeddings are configured disabled. |
| S05 | [Administrative opener](../src/storage/sqlite/ownership.rs): `open_admin_read_only`; core `open_fence_plan_inspection`. | STATIC: distinct MAIN-only admission, journal/missing-SHM refusal, then retained native read-only connection and revalidation. These controls do not describe the separate doctor inspector. |
| S06 | [Retrieval](../src/retrieval/mod.rs): `search_with_mode`; SQLite `get_memory_detail`, embedding candidate/status/save methods, `get_meta` / `set_meta`. | STATIC: retrieval branches use separate Store calls; several metadata/indexing methods bypass `memories`. Store metadata values are generic text. A memory-relation rename alone does not cover every observer or mutator. |
| S07 | [Embedding manager](../src/embeddings/manager.rs): `read_enabled`, `read_selected_model`, setters; [setup](../src/setup/mod.rs): `build_plan`. | STATIC: ordinary setting routing uses generic Store metadata; setup also calls the diagnostic inspector. Setup disclosure of those values was not demonstrated here. |
| C01 | S3e sections 8, 13–14; S3e.1 sections 7, 9–10; S3e.2 implemented boundary. | CONTRACT: coherent readers, actual old-client proof, joint publication/fence atomicity and honest UNKNOWN recovery remain prerequisites; schema17 is passive only. |
| H01 | Idle and paused schema-16 clients survived migration. | HISTORICAL: supplied recovery finding; old-open tests also exist. Exact pause locations and per-run traces were not recovered. |
| H02 | Queued legacy writers resumed after migration. | HISTORICAL: supplied finding and S3e.2 report. A submitted request spanning a held commit does not identify its exact blocked instruction. |
| H03 | Checkpoints and schema-version checks did not evict existing connections. | HISTORICAL: supplied finding; checkpoint test source retains an old MCP client that writes afterward. Checkpoint success is not connection closure. |
| H04 | Renaming `memories` blocked tested memory queries while other APIs remained accessible. | HISTORICAL: supplied finding; neither complete endpoint coverage nor populated production cutover safety follows. S06 explains why bypasses need separate audit. |
| H05 | Old doctor printed unrestricted model-setting text after incompatibility. | HISTORICAL: supplied finding and secondary note; S04 and the historical source inspector support the mechanism statically. Actual binaries/output were not rerun or authenticated here. |
| H06 | WAL inspection could create or modify SHM. | HISTORICAL: supplied finding and S3e.2 report; MAIN/WAL application bytes unchanged does not imply unchanged file family. |
| H07 | A synthetic hot-journal fixture exposed uncommitted metadata. | HISTORICAL: supplied finding and secondary note only. Fixture construction, valid crash-state provenance, recovery path and raw output are unrecovered. This is not proof of ordinary SQLite dirty reads. |
| H08 | Held snapshots and prepared statements prevented inode-drain acquisition until released. | HISTORICAL: supplied finding and secondary note. Exact handle states, native calls, lease mode and acquire/release timeline remain unverified. |
| H09 | Raw same-UID SQLite remained outside the admission-respecting Store guarantee. | HISTORICAL and CONTRACT: reported raw reads after witness release do not contradict a narrowly scoped Store admission claim; they do refute a claim of hostile same-UID exclusion. |
| H10 | Experiments were reported on tmpfs and btrfs. | HISTORICAL: filesystem coverage report, not newly verified mount support or a universal environment guarantee. |

Historical totals supplied for S3e.3a are **10 process-test executions, 30
diagnostic observations and four native connection scenarios**. They are reported
counts, not new passing tests, independent sample counts, or complete coverage.
The secondary note describes five process cases on each of tmpfs and btrfs;
raw logs and the complete mapping were not recovered. S3e.2's separately recorded
suite/repeat totals belong to that predecessor and are not added to S3e.3a totals.

The existing compatibility test pins an old production executable by SHA-256
`a5960e6b98cd311f7f00d56bc165bc0ffd155e0b328051a961b86cae308ba31d`.
That constant and S3e.2's historical binary path are references, not proof that
the binary survived. The schema-16 source at committed `254d1ad` has the same
diagnostic inspector, and `src/doctor/mod.rs` is unchanged between that commit and
HEAD (STATIC comparison). This does not authenticate a newly built binary as the
historically tested executable.

## 4. Candidate compatibility floor and exclusive inode-drain witness

This sequence is a **CANDIDATE proof plan**, not a recovered implementation
contract. No compatibility-floor rollout is authorized. It would require
separate explicit acceptance under S3e's compatibility-rollout exception; it
cannot be hidden in ordinary open, passive preparation or schema17.

1. Bind an existing initialized database to validated path, device/inode and
   local administrative context. Inventory MAIN/WAL/SHM/journal and identify
   supported client/build/layout versions. Refuse ambiguous identity or
   unsupported environment; do not create a substitute DB at the same path.
2. Design a durable monotonic compatibility floor which the targeted old Store
   admission actually consults. A new arbitrary metadata key is insufficient.
   Existing old Store code checks `schema_migrations`; an extra protocol marker
   alone cannot change that old code. No future version number, marker encoding
   or migration is selected here. Prove both normal WAL-visible admission and
   any MAIN-only diagnostic/bootstrap route; they are distinct.
3. Serialize a separately authorized floor transition, preserving all ordinary
   knowledge, existing history and passive receipts. Confirm its durable outcome
   using a healthy connection. A necessary trusted checkpoint would be a
   separately explicit maintenance action, never proof that old clients closed.
   A floor cannot activate asset fences or publish knowledge.
4. Close the rollout's own native SQLite handles before attempting a bounded
   exclusive **inode-drain witness**. The candidate is a native Linux inode lease
   such as `F_SETLEASE`, not SQLite `BEGIN EXCLUSIVE`, a checkpoint, an advisory
   application mutex or a process list. Exact lease mode, descriptor ownership,
   conflict semantics and filesystem/kernel requirements must be established by
   native tests. They cannot be recovered reliably from the reported outcome.
5. Bind acquisition to the same device/inode and revalidate the locator. Record
   failure/unsupported errno, acquisition and release, competing opens and any
   break notification. Bound the wait and refuse publication on timeout,
   identity change or ambiguity. Never kill operator processes to obtain it.
6. Prove the handoff from that witness to admitted supported SQLite connections.
   Do not assume a lease can be held while another native connection opens, or
   that release preserves exclusion. Competing opens, break handling, automatic
   lease changes and path replacement across the handoff are unresolved. A new
   old diagnostic process may access SQLite independently of Store admission.
7. Only a separately accepted protocol with closed admission/diagnostic gaps
   could become a future publication prerequisite. The publisher would still
   need coherent snapshots, dual-layout routing, all affected-writer guards and
   the single atomic replacement/index/head/receipt transaction from S3e.

Do not collapse these evidence levels:

| Observation | What it establishes | What it does not establish |
| --- | --- | --- |
| Fresh old Store open rejected | That binary's tested admission path rejected the visible layout/version. | No earlier handles, no native side effects, or rejection by diagnostic/raw connections. |
| Old process/connection remains open | Process/handle liveness in that observed interval. | A held read snapshot, permitted next mutation, or knowledge isolation. |
| Exclusive witness acquired | A native acquisition succeeded for one bound inode under the tested conditions and semantics. | All processes dead, no sidecar/mapping access, future admission exclusion, or a durable authorization state. |
| Full knowledge/mutation isolation | Requires path-by-path, time-bounded protocol and affected-write evidence. | It cannot be inferred from any one row above. |

A witness is ephemeral and inode-specific. It does not erase buffered results,
already read values, old WAL snapshots or memory mappings. Exact coverage of open
descriptions, prepared statements, mappings, sidecars, hardlinks and replacement
inodes remains a proof obligation. A successful acquisition after release of a
particular old handle is useful characterization, not complete exclusion proof.

## 5. Diagnostics, metadata and SQLite file effects

STATIC call sequence: `inspect_database` chooses immutable MAIN when it sees no
WAL, otherwise native read-only WAL access. `read_inspection` gathers schema
versions and `mind_meta` values for `embedding.enabled` and `embedding.model` in
one transaction. Only afterward does doctor classify compatibility, retaining the
inspection for embedding checks. A nonzero exit or incompatible status does not
prevent later report fields from carrying the selected model. Text rendering
flattens those details; JSON includes them directly. This is not a query dumping
every `mind_meta` key.

| Output/data category | Evidence and required interpretation |
| --- | --- |
| Schema version/support information | STATIC diagnostic fields. This is compatibility information, not evidence of active-content disclosure. |
| Operational diagnostics | STATIC paths, configuration origins, WAL presence and other checks. They may be informative; no broad diagnostic exception is accepted by Decision B. |
| Arbitrary model-setting values | H05 reports actual output; STATIC raw model text reaches details without manifest membership being required first. The generic Store setter accepts arbitrary text even though the normal model-selection UI validates IDs. |
| Potentially sensitive metadata | A synthetic protected marker placed in a model value would be sensitive by fixture definition. The historical disclosure report establishes a metadata hazard, not real-user secret exposure. |
| Active memory/document contents | Not demonstrated by the reported doctor case. The inspected diagnostic SQL does not select memory/document contents. This is not proof that every surviving old API is content-isolated. |

Configured embeddings disabled differs from compiling without the embeddings
feature: in the former, the model is still included in diagnostic details; in the
latter, the embedding output check is skipped, while the inspector still reads
the keys. Neither build is a universal exclusion mechanism. Size, Unicode/control
rendering, error output and alternate entry routes need explicit synthetic tests.

The broad comments in [doctor documentation](doctor.md) and SQLite inspection
source describing read-only access as creating nothing new are not accepted as
a file-family invariant. H06 reports SHM creation/modification; S3e.2 already
distinguishes admitted native coordination from application data writes. The
administrative MAIN admission path S05 is narrower than doctor and must not be
credited to it. Immutable MAIN is an admission tool only under its stated
conditions, not a safe substitute for a changing database's live WAL snapshot.

H07 is especially limited: a synthetic hot-journal observation is not proof of
read-uncommitted mode or normal recovery correctness. The later harness must
show how the journal was produced, whether it represents a reachable crash
state, what recovery was attempted, which values were committed and which were
observed. Until then, uncertainty about MAIN-only inspection around rollback
journals blocks a zero-disclosure claim. Never bypass recovery or delete a live
journal to obtain a convenient result.

Evidence must separate MAIN/WAL logical row changes from creation/deletion,
inode identity, size, byte hashes, mtime/ctime, permissions and observable SHM
changes. Record atime separately with mount/measurement caveats. Hashing can
itself affect atime; another native open/checkpoint can conceal the original
side effects. Capture before any such follow-up. No-data-write and no-filesystem-
effect are different claims.

## 6. Model A and the unaccepted two-key quarantine candidate

CONTRACT: the DB-owning local OS account explicitly invokes administration;
that account, its processes, kernel and SQLite are trusted. No hostile same-UID
protection, distinct-human authentication, generic IAM, external revocation or
new MCP authority is introduced. Admission-respecting Store connections are the
intended compatibility boundary. Diagnostic entry points must be analyzed
explicitly because they use a separate opener. Arbitrary raw SQLite by the same
UID is outside the guaranteed exclusion model; granting raw access cannot then
be advertised as prohibited by a lease or new schema name.

Here **two-key metadata quarantine** refers only to a possible relocation or
isolation of `embedding.enabled` and `embedding.model` from the legacy diagnostic
lookup. These are metadata keys, not encryption keys. This is a reconstructed
candidate description; the lost original's exact table/layout and semantics are
UNKNOWN. It is not an accepted solution, an implemented migration, or a reason
to relax Decision B.

Before considering that candidate, prove:

- Migration of existing absent/valid/arbitrary/oversize values preserves the
  agreed authoritative settings and M6 accounting/history without exposing them
  through legacy lookup, views, errors or fallback. Define the permitted legacy
  diagnostic result explicitly; merely deleting keys can change enabled/model
  semantics for surviving clients.
- Every settings reader/writer is routed consistently: Store metadata, embedding
  manager, query capability/cache, indexer, CLI/MCP and diagnostic/setup paths,
  including already-open supported processes and both feature configurations.
  No stale cache, dual authority, raw fallback or old queued setter can repopulate
  an exposed authoritative value. Assess other metadata paths independently.
- Copy/removal, new routing marker and version/layout change have a coherent
  transaction boundary. Supported readers see an entire old or new state;
  concurrent setting changes are serialized or refused without loss. No retrieval
  result or semantic coverage change is silently accepted as compatibility work.
- MAIN versus WAL visibility, missing SHM, journal recovery and stale snapshots
  cannot reveal the supposedly quarantined values through targeted old diagnostics.
  Logical relocation is not secure erasure of WAL/journal/free-page bytes and does
  not protect against arbitrary raw same-UID access.
- Crashes before/during/after COMMIT, lost acknowledgement, concurrent migrators
  and exact retries resolve durably without unsafe key recreation, marker rollback
  or bypass of the original publication prerequisites.

Updating only current doctor cannot change the behavior of compiled old doctor.
If any required route or migration/recovery property remains unproved, quarantine
remains a candidate and publication stays BLOCKED.

## 7. Crash, rollback and publication blockers

Floor migration, checkpoint, native drain and future publication are different
steps; no process-local lease can make them one SQLite transaction. After a
crash, an earlier drain observation is invalid as a current witness. Re-establish
identity and admission, inspect durable state through a healthy supported path,
and reacquire any required witness. No receipt, PID list or checkpoint substitutes
for that revalidation.

Known COMMIT failure differs from `CommitOutcomeUnknown`. Receipt absence means
NotObserved, not rollback or process death. Concurrent migration attempts must
serialize and revalidate after acquiring the writer; stale intent refuses. If a
floor committed but later drain fails, do not report the overall attempt rolled
back or lower a schema marker to resume old clients. The exact floor operation,
receipt format and recovery state machine remain undesigned. Supported ordinary
work must not be indefinitely disabled by an abandoned attempt.

Never restore an old-name alias, delete successful receipts/fences, remove
journals, replace an inode or kill existing clients as an unreviewed rollback
shortcut. A future failed first publication retains the old writable/visible
cohort; a completed publication needs a separately authorized compensation,
not an assertion that COMMIT was undone.

Publication remains BLOCKED by all applicable predecessor gates, including:

- Accepted and actually proved old-reader protocol, every surviving observer
  and mutator audited, diagnostic exposure resolved consistently with Decision B.
- Proved populated layout migration preserving IDs, FKs, triggers, indexes, M6
  logical history/availability and receipts; coherent supported dual-layout
  readers and unrelated writers spanning cutover.
- Complete asset/alias/mutation-domain guards, late vector/index maintenance
  protection, source-only refresh, exact projection/citations and required
  lexical/semantic coverage.
- One authorized atomic fence/retirement/replacement/index/head/receipt commit,
  including concurrent intent, crash, unknown outcome and resource refusal proof.

This document clears none of those implementation gates. A missing-table error,
a floor marker or a successful drain witness cannot stand in for them.

## 8. Reconstruction limitations

Unrecoverable from the supplied evidence: original wording and section order;
complete intent and alternatives; exact floor marker/version; native lease mode,
descriptor/signal/handoff protocol; complete old API and binary inventory;
historical fixture/helper source and raw output; hot-journal construction and
reachability; exact scenario-to-count mapping and repetition independence;
kernel/SQLite/build/mount details; timeouts and complete synchronization traces;
the original review's reasoning and any qualifications omitted from summaries.

The surviving secondary note says the lost document had an independent review
with no remaining findings. That is HISTORICAL, not review of this reconstruction
or approval of a floor/quarantine design. Reported tmpfs/btrfs results are not
portable support claims. Static source agreement with a report does not recover
its runtime measurements. No assertion of equivalence to the lost file is made.

## 9. Recommended S3e.3b harness requirements

The smallest next slice is **reproduction and characterization only**, separately
authorized. Reuse [compatibility process tests](../tests/fence_plan_compatibility.rs),
[doctor tests](../tests/doctor.rs), [passive admin tests](../tests/fence_plan_admin.rs)
and existing spawn-gate / migration hooks before proposing a parallel framework.
No quarantine, production hooks, fence activation or future migration is implied.

| Area | Required observable evidence |
| --- | --- |
| Binary provenance | Pin full historical/current commits, source differences, lockfile/build flags/toolchain, bundled SQLite version and executable hashes. Distinguish rebuilt historical binaries from the preserved executable, if located and authenticated; do not silently bypass the existing hash assertion. Separate production binaries from test-hook drivers. |
| Isolation and lifecycle | Fresh disposable per-case DB and synthetic input only; explicit absolute DB and isolated HOME/XDG/config environment. Bounded startup/RPC/lock/exit waits, explicit handshakes or acknowledged gates, process-group teardown and child reaping on success, assertion failure and interruption. Preserve useful logs and fixture evidence on failure without retaining live processes. |
| Diagnostic reproduction | Incompatible old doctor in text/JSON, configured embeddings on/off and feature variants; synthetic setting, operational and active-content canaries. Classify actual stdout/stderr and absence observations by data category. A nonzero exit is not non-disclosure. Never seed real secrets. |
| Native SQLite | Read-only/immutable opens, existing WAL snapshot, SHM missing/present, journal and synthetic crash cases, committed versus uncommitted metadata. Capture MAIN/WAL/SHM/journal bytes and inode/metadata before/after prior to cleanup or another native open. Record SQLite flags and fixture provenance. |
| Old connections and concurrency | Idle and acknowledged paused processes, cached/prepared/partly stepped statements, held read transactions and WAL retention, queued writes, concurrent migrators. Acquire/fail/release/retry inode witness around explicit handle termination/release, without equating acquisition with full isolation. Request timing alone must not assert the exact blocked instruction. |
| Environment | Run on independently identified tmpfs and btrfs when available; record mount options/kernel/native lease capability and root path. Unsupported or unavailable cases are SKIPPED with reason, never PASS or simulated filesystem coverage. |
| Oracles and repetition | Regression invariants protect passive behavior; known-failure characterizations pass only when the documented unsafe/unresolved condition is reproduced and labeled as such. Separate admission rejection, handle liveness, drain acquisition and actual knowledge/mutation outcomes. Repeated runs measure reproducibility, not absence of races. |
| Review and report | Scenario-by-scenario evidence matrix, exact invocations/status/exit codes, artifacts, skips, limitations and remaining quarantine unknowns. Independent adversarial review for false positives/negatives, fixture contamination, races, unsupported environments and overclaimed test names. |

That harness must first reproduce the known failures; it must not turn green by
relabeling them secure. Its output would supply evidence for deciding the next
small candidate prototype. Migration/routing/atomicity/recovery work on two-key
quarantine cannot be selected as successful before that evidence and separate
design acceptance exist.

## 10. Reconstruction verification and independent review

New verification is confined to Git/signature/lineage, source inspection and
documentation checks. No Rust suite, real old/current binary, SQLite connection
probe, tmpfs/btrfs experiment or S3e.3b harness was executed. Historical totals
above are not part of the current verification result.

An independent read-only Sol reviewer inspected the entire new document against
the recovery requirements, predecessor contracts and cited source. The review
reported no BLOCKER, HIGH or MEDIUM findings and one LOW finding:

| Severity | Finding | Disposition |
| --- | --- | --- |
| LOW | The harness requirements called the historical executable unavailable although its survival was unverified. | Corrected to “the preserved executable, if located and authenticated.” Availability remains UNKNOWN. |

The reviewer independently checked baseline/draft identity, the single-file
scope, all ten historical rows, the reconstruction notice, relative links,
whitespace and historical/current inspector equality. Review covers the accuracy
and boundaries of this reconstruction; it establishes no runtime exclusion,
floor/quarantine acceptance or semantic identity with the lost design.

Executed documentation checks:

- `python3 scripts/check-doc-links.py`: passed, 65 Markdown files checked.
- Python assertions: passed for reconstruction notice, ten historical rows,
  evidence terminology, heading/list spacing, table column consistency, balanced
  code fences, trailing whitespace, tabs and final newline. No standalone
  Markdown linter was available; this is structural checking plus manual review.
- `git diff --check`, staged-diff and new-file checks: passed. Because the file
  is untracked, `git diff --no-index --check /dev/null
  docs/m8-s3e3-old-reader-exclusion-design.md` also checked its whitespace;
  status 1 with no diagnostics denotes the added-file difference.
- Historical/current inspector block and doctor-file comparison, schema16/17
  constants and predecessor ancestry: passed as static source/Git checks.

Only this untracked document is delivered; HEAD is unchanged and nothing is
staged or committed. Reconstruction-document acceptance does not clear the
BLOCKED publication decision or start S3e.3b.
