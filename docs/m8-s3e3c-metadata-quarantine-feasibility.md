# M8 S3e.3c: two-key metadata quarantine feasibility

**PARTIAL feasibility: same-file legacy-lookup isolation is promising under
explicit conditions; enforceable production old-client exclusion remains
BLOCKED.** The investigation and disposable probes are complete. No candidate
is accepted for installation, and publication remains BLOCKED.

## 1. Baseline, evidence and exact meaning of two keys

Verified on 2026-11-11: clean index/worktree on
`feat/m8-s3d2-semantic-staging` at signed
`9e22ac3894a2c4d534f9bb1103573f220f171be7`, worktree
`/home/cnachtigall/git/projects/mindspace-wt/m8-s3d2`. GPG reported a good signature.
Lineage is S3e.3b `9e22ac3` → reconstructed S3e.3 `91c70b9` → passive infrastructure
`54e5dff`. Read with [S3e](m8-s3e-publication-design.md),
[S3e.1](m8-s3e1-writer-fencing-design.md),
[S3e.2](m8-s3e2-fencing-infrastructure.md),
[reconstructed S3e.3](m8-s3e3-old-reader-exclusion-design.md) and the
[S3e.3b harness/report](m8-s3e3b-adversarial-harness.md).

The name is resolved from source, not treated as an encryption requirement:

| Configuration key | Existing meaning and ownership |
| --- | --- |
| `mind_meta['embedding.enabled']` | Optional TEXT; [manager `read_enabled`](../src/embeddings/manager.rs) enables embeddings only when the value is exactly `"true"`. Absence and arbitrary other strings currently mean disabled to that reader; they are not identical raw values. |
| `mind_meta['embedding.model']` | Optional TEXT selecting a model identifier. The normal manager setter validates manifest membership and installation; generic [Store `set_meta`](../src/storage/sqlite.rs) accepts arbitrary text. Model weights and tokenizer assets are elsewhere; this value is not a cryptographic key or authenticated profile. |

Define **L**, the legacy lookup domain, as these two exact `mind_meta` locations
hard-coded by historical Store and diagnostics. Define **A**, the candidate
authoritative domain, as a single selected generation containing the two optional
raw strings. This is a proposed separation of access paths, not two mutually
untrusted operating-system principals. The local database-owning account remains
the administrative owner of both. Other generic metadata is outside this exact
two-key quarantine claim.

**SOURCE** means inspected committed code; **OBSERVED** means a new executable
probe; **REASONED** means a bounded consequence; **UNKNOWN** identifies missing
proof. Predecessor historical reports are not silently counted as new executions.
The word “private” below means absent from old hard-coded lookup locations, not
encrypted or inaccessible to the same UID.

## 2. Threat and access model

Retain Model A: trust the DB-owning local account, its processes, kernel and
SQLite. No distinct-human authentication, hostile same-UID protection, external
grant issuer, new key manager or new MCP authority is introduced.

| Access path | Source/runtime evidence and boundary |
| --- | --- |
| Historical schema16 Store | Fresh opens reject a newer registered schema; already-open connections do not repeat compatibility admission on each operation. Real historical `get_meta`/`set_meta` continue using L. Their two individual getters do not constitute one pair snapshot. |
| Current schema17 Store | [Core `connection`, `get_meta`, `set_meta`](../src/storage/sqlite.rs) still use L. Q01 observes both getters returning None after relocation, despite a selected A generation. It is not a quarantine-capable supported client. |
| Diagnostics/setup | `inspect_database` reads schema versions and both legacy settings before doctor reports compatibility. [Doctor](../src/doctor/mod.rs) retains the inspection, outputs raw selected-model text and a parsed enabled boolean. It does not output arbitrary enabled TEXT as such. [Setup](../src/setup/mod.rs) also calls the inspector; its other output paths are not comprehensively audited here. |
| Model/runtime/index routing | [Manager](../src/embeddings/manager.rs), [semantic resolution](../src/retrieval/semantic.rs), caches and index operations depend on current metadata semantics. Separate getters can cross a generation boundary. No production paired reader, cache invalidation or updater has been installed. |
| SQLite recovery | Rollback journals, WAL, SHM, snapshots and immutable MAIN have different visibility and write-coordination behavior. A successful ordinary COMMIT is not a checkpoint, drain or proof that every diagnostic sees the same physical pages. |
| Raw same-UID SQLite | Can query A by table name, and can alter files/schema. This is explicitly outside the admission-respecting Store exclusion model. Q06 reads A after witness release; no secrecy against that access is claimed. |
| Source ownership/aliases | S3b enrollment and passive plans still do not activate ownership transfer or reserve aliases. F/D identity, known-path/physical-alias closure and observation-domain classification are not derived from a settings generation. Nothing here resolves those publication blockers. |
| Active knowledge/publication | Existing memory, assertion, source, vector and code paths need independent protection. Q10 observes an already-open old MCP client reading active knowledge and writing a memory after candidate preparation. No effective head, cohort, source-only refresh, projection or retrieval snapshot protocol is added. |

Keep four outcomes separate:

- **Knowledge disclosure:** active memory/document/index-derived information.
  Two-key lookup suppression cannot establish its exclusion.
- **Metadata disclosure:** model strings, enabled state, generations, errors and
  other diagnostic fields. New canary absence is tested only in specified output.
- **Mutation:** old setters can repopulate L without guards; guarded fixture
  setters are distinct from ordinary source/memory/vector writer fencing.
- **Filesystem effects:** SHM creation/modification, journal recovery, inode and
  timestamp changes can occur without an application-row mutation.

Historical Decision B remains: the diagnostic exception was rejected. Neither
successful incompatibility reporting nor sanitizing only a current executable
permits arbitrary metadata disclosure by a compiled old diagnostic.

## 3. Candidate comparison

| Candidate | Location, access and old visibility | Atomicity, recovery and compatibility | Decision |
| --- | --- | --- | --- |
| C0 keep/sanitize L | Authoritative values remain where old Store/doctor read them, perhaps with value validation or display escaping. | Small migration cost, but escaping preserves information and current-doctor changes do not change compiled old readers. Replacing meaningful values with placeholders changes settings semantics unless a separate authority/routing rule exists. | Reject as isolation. It may be output hygiene, not a substitute for Decision B. |
| C1 same-file authoritative pair/generation | A is a dedicated row or generation; L has neither key. Future supported readers select A coherently. Static exact-key guards prevent old setters repopulating L, while unrelated metadata remains usable. Raw same-UID SQL still sees A. | One SQLite transaction can cover pair, selector, prototype receipt, L removal and guards. Existing values, M6 integration, recognized layout, routed clients and crash/UNKNOWN handling require proof. Old snapshots still expose earlier L; immutable MAIN may lag WAL. | Narrowest useful candidate; conditional feasibility only. Not an accepted schema or rollout. |
| C2 separate file/database | Move A to another file; clients must choose the right file and bind its identity/generation. Old handles still address the old file. | Adds multi-file authority, backup, inode and recovery states. A file rename/pointer swap does not evict handles. WAL transactions across attached DBs lack atomicity as a set. No evidence requires this complexity. | Defer/reject as the initial slice. Not experimentally qualified here. |
| C3 encryption/new key management | Encrypt values or introduce separate decryption authority. | Changes credential storage, recovery, key lifetime and process authority. It does not erase already-read values or repair routing, caches, old snapshots or ownership/publication. Model A supplies no new isolated principal. | No demonstrated need or accepted key-management design. Not implemented/probed. |

The C2 atomicity limit is documented by SQLite for attached WAL databases;
single-file and cross-file claims must not be conflated.
[SQLite ATTACH](https://sqlite.org/lang_attach.html),
[SQLite WAL](https://sqlite.org/wal.html).

C1's disposable adapter uses three experimental tables: `probe_q_pair`,
`probe_q_state`, `probe_q_receipt`. They make generations, selection and replay
observable; they are not a selected production layout. A combined row/receipt
layout could be smaller if it satisfies the same tests. No scheduler, service,
second database, encryption key or new production helper binary is needed by
these probes.

## 4. What the disposable adapter does and does not prove

Only generated fixtures receive experimental DDL. The adapter serializes with
IMMEDIATE; inserts an optional-TEXT pair and digest, selector/protocol and receipt;
removes both L keys; and optionally installs static BEFORE INSERT/UPDATE guards
for either old or new key identity. Controlled REPLACE, UPSERT and key-renaming
attempts are included. There is no new UDF dependency on unrelated old statements.

The selected reader joins generation, pair, receipt, digest and L-row count in
one SQL statement/snapshot. It refuses the tested missing/substituted state and
conflicting L values, with no L fallback. Updates compare expected generation
under the writer lock. Exact receipt replay is tested separately from current
state validation. **It is intentionally incomplete:** corrupting a stored receipt
generation to 99 makes replay return 99 without a corresponding pair, while the
selected-state reader rejects that same inconsistency (Q09).

Full Mindspace fixtures retain their existing M6 guards. A clearly labelled raw
test migration context registers `m6_write_revision`, allocates a revision and
performs candidate writes inside the transaction. This is **not** the production
private-permit mechanism, accepted schema migration, real administrative intent
binding, recording-history qualification or inventory validation. Most native
atomicity/crash probes use explicitly minimal schema-shaped fixtures. Success
does not establish a populated production migration or current Store routing.

The initial preparation API accepts a supplied pair; one sample actually copies
existing optional/arbitrary L values. That sample does not prove concurrent source
revalidation, complete value-domain compatibility or a permitted model change.
The digest checks self-consistency, not hostile-account authenticity. Guard
inventory and all corruption combinations are not qualified.

## 5. Reproducible experiment sources and invocation

Durable experiment sources are left uncommitted with this report:

- [Executable runner](experiments/m8/s3e3c_probe.py).
- [Disposable candidate/native probes](experiments/m8/s3e3c_metadata_probe.rs).
- [Historical Store actor](experiments/m8/s3e3c_old_store_actor.rs).

The runner reuses committed S3e.3b archive/process supervision and test-only
Case/fingerprint helpers. It verifies them against Git
`9e22ac3894a2c4d534f9bb1103573f220f171be7`. It compiles snapshots of the experiment
sources into separate disposable archived targets, never into production source.
`candidate_actor` and `old_store_actor` are ignored test protocol endpoints and
are not counted as scenarios. Real old Store calls come from archived
`254d1ad5794db8ac19dc187a5934910337da9b9e`; they are not native SQL imitations.
The historical actor is a test build against that library, distinct from the
actual historical production CLI used for doctor/MCP. No Store method is patched.

From the worktree, on Linux with Git, Python 3.12+, Cargo/Rust and cached locked
dependencies, choose a new output directory:

```bash
docs/experiments/m8/s3e3c_probe.py \
  --output /tmp/m8-s3e3c-run-001 \
  --target-dir /tmp/m8-s3e3c-build-cache \
  --repeat 3
```

Without `--binaries-manifest`, the runner rebuilds pinned production old/current
and the existing spawn-gate driver. In this investigation it reused authenticated
S3e.3b binaries and rechecked actual fresh-DB schema16/17 initialization:

```bash
docs/experiments/m8/s3e3c_probe.py \
  --output /tmp/m8-s3e3c-reuse-001 \
  --binaries-manifest /path/to/s3e3b/manifest.json \
  --target-dir /tmp/m8-s3e3c-build-cache --repeat 3
```

For independently repeatable focused execution after a build:

```bash
docs/experiments/m8/s3e3c_probe.py \
  --output /tmp/m8-s3e3c-replay-001 \
  --reuse /path/to/s3e3c/manifest.json --case q09 --repeat 1
```

Each test and subprocess uses generated paths, explicit DB arguments, cleared
environments and isolated HOME/XDG/TMPDIR. Existing binaries and all new source,
framework and executable hashes are checked. There is no live DB discovery or
database-path input. Compiler controls/toolchain, archived source pins and lockfile
hashes are recorded; trusted local Cargo/toolchain configuration remains an input,
so no bit-identical or hostile-toolchain build guarantee is claimed.

The committed S3e.3b fresh-build path still requires HEAD `91c70b9`. It is unchanged.
This runner archives the pinned S3e.3c baseline or authenticates prior artifacts;
it does not bypass or rewrite that committed guard. New experiment sources are
under docs rather than auto-discovered production/test targets; only disposable
archives receive the additional Cargo targets.

## 6. New execution evidence and matrix

Primary: `/tmp/mindspace-s3e3c-npb05ekt/corrected/manifest.json` and `summary.json`.
Independent rerun: `/tmp/mindspace-s3e3c-review-independent/summary.json`.
Raw stdout/stderr and per-case JSON/fingerprints are under each `runs/` directory.
These local artifacts are disposable; the retained source/instructions permit
reconstruction if they disappear.

Observed: **90/90 primary scenario executions PASS**, producing **144 case
results**: 45 prototype invariants and 99 known-unsafe characterizations.
Independent: **30/30 scenario executions PASS**, producing **48 case results**.
Neither run had a failed or skipped scenario. Ten scenario functions produce
16 case variants per root; there are three root categories and three primary
repetitions, one independent repetition. The baseline and explicit `/tmp` roots
are both tmpfs; the third root is actual btrfs under worktree
`target/s3e3c-fixtures`. This is two filesystem types on one host, not three
independent environments. Native lease support was available in both.

Observed environment: Rust/Cargo 1.98.1, bundled SQLite 3.53.2, kernel
`7.2.14-zen1-1.1-zen`. Production old/current/driver hashes match the S3e.3b report;
fresh smokes observed16/17/17. New historical actor SHA-256:
`6f66ec228cc93d8f16714fd227e795f432fcd91fe94df4f3cb4b8b9ab2285fb1`.
New candidate target SHA-256:
`23aacaf037bf68bb3574668818f5ad57be172d4ab1f4cc8934658ec7b4e9d548`.
The manifest distinguishes the historical production binary from the new
historical-library actor and current test-feature builds.

| Probe | New observation and exact limit |
| --- | --- |
| Q01 doctor and current routing | Baseline old doctor discloses L model. After stable candidate preparation, the new A markers are absent across tested stdout/stderr; both L keys are absent. Real current17 Store still returns None for both keys despite selected A generation 1: routing is missing, not supplied by relocation. |
| Q02 held/prepared readers | Pre-cutover read transaction and partly stepped old `mind_meta` SELECT retain OLD after candidate DDL commits. Prepared-but-unstepped old lookup first executed afterward returns None. Private-table prepared/stepped controls distinguish later generation snapshots. These are native bundled-engine controls, not an instrumented old Store statement cache. |
| Q03 queued actual old setter | An actual historical Store opened16 before real migration17; its queued model setter repopulates L without guards. With static guards it returns `fixture legacy setting quarantined`; selected A remains generation 1. Unrelated old metadata setting succeeds. Timing proves a request spans held IMMEDIATE, not its exact blocked instruction. |
| Q04 hot-journal boundaries | Genuine DELETE/cache-flush/child-kill initial preparation: immutable old diagnostic can see temporarily removed keys, but normal recovery restores prior L disclosure and no committed candidate marker. Later private-only update crash leaves old lookup empty; native RO reports776, RW restores generation 1. These are controlled process crashes, not power-loss qualification or ordinary dirty-read behavior. |
| Q05 WAL/SHM | A cutover committed in WAL has empty fresh L while immutable MAIN still has OLD. WAL-aware old doctor omits A marker and recreates missing SHM; MAIN/WAL hashes stay unchanged in that read phase. Sidecar effects survive logical read-only access. |
| Q06 drain/raw access | Held snapshot connection prevents an exclusive inode lease with EAGAIN11. Release permits acquisition/release; raw same-UID SQL then reads A. No durable exclusion or lease-to-SQLite handoff is established. |
| Q07 ordering | Floor-first split commit yields incompatible-but-disclosing diagnostics. Quarantine-first yields relocated settings with no newer floor. One minimal native transaction commits17 plus A/L transition together; explicit floor, empty L and incompatibility assertions pass. A deliberately impossible fixture sentinel999 rejects fresh actual16/17 Store opens but still permits diagnostic L disclosure. It is not an allocated version, migration 0018 or populated migration proof. |
| Q08 interrupted preparation/ACK | Known precommit termination restores old L and no candidate state. A controlled postcommit/pre-result termination leaves generation 1 and a prototype receipt; uncorrupted exact replay creates no second generation. The separate control acknowledgement locates the test kill point; it is not production UNKNOWN/liveness proof. |
| Q09 conflicts/corruption | One actually copied legacy arbitrary/optional pair is preserved. Expected-generation CAS, changed-intent refusal, native REPLACE/UPSERT/key-rename guards and five selected-state corruptions pass their narrow oracles. A separate unsafe case changes receipt generation to 99: selected-state read refuses, but exact replay falsely returns 99 without a pair. Complete historical-receipt validation is missing. |
| Q10 knowledge/mutation boundary | Actual old production MCP remains able to search existing active memory and create another memory after candidate preparation. Two-key relocation does not supply the original knowledge/mutation exclusion or asset writer fence. |

PASS means the stated oracle was observed. For counterexamples it means unsafe
or unresolved behavior was reproduced, never that a vulnerability was fixed.
Prototype “regression invariants” are adapter transaction/control properties;
they are not new production Store guarantees. Historical S3e.3a/3b counts are
not added to these new totals. Finite repetition is not proof of race absence.

The full-output canary checks constrain these exact diagnostics, not every possible
inference. Q01's model markers, optional-value sample and old setter controls do
not enumerate all enabled/model combinations, old APIs, error formatting, cached
configuration or redundant model information in vectors/artifacts. No real secret
was used. Active memory access is demonstrated separately by Q10, not inferred
from diagnostic output.

## 7. Safety, ordering and recovery decision

The achievable bounded claim is: after a recognized, successfully committed
candidate transition, a tested fresh legacy two-key lookup need not return a new
authoritative marker; a tested static guard can stop that old key setter from
recreating its legacy location. Neither statement establishes production
admission, complete metadata noninterference or existing-reader eviction.

Required conceptual states are **legacy**, **candidate transaction uncommitted**,
**candidate committed but coordination incomplete**, and **qualified supported
operation**. The middle states are not publication permission. Floor, checkpoint,
drain and reopen are separate events; a partial-success maintenance operation
must not be called rolled back because a later event fails.

| Property | Finding / missing mechanism |
| --- | --- |
| One authority | A must be selected by one recognized marker in the reader's snapshot. Conflicting L/A, incomplete schema or ambiguous generation refuses; post-cutover fallback to L is forbidden. Production routing/inventory checks are missing. |
| Atomicity across domains | Single-file transaction controls are positive. Complete populated migration with actual M6 accounting, exact source revalidation, constraints/guard inventory and ordinary read/write compatibility remains unproved. |
| Durable ordering | A coherent logical commit can coexist with older MAIN pages, old snapshots or hot journals. Do not equate native visibility with safe immutable diagnostic admission. |
| Failure/acknowledgement | Known precommit failure and a controlled committed-before-reply case are distinct. Missing receipt is NotObserved, not death or rollback. Corrupt-receipt replay currently demonstrates why complete durable intent/linkage validation is required. |
| Stale writers | Static guards block tested legacy-key writes and leave unrelated metadata usable. There is no actual supported-settings updater, cache protocol, generation-bound admin permit or source/memory/vector fence. |
| Old readers | Existing snapshots retain old values. No retroactive erasure is possible. Fresh diagnostics bypass Store admission, and a schema floor alone does not stop them. Actual17 is also incompatible with the proposed new routing and needs a separately reviewed rollout boundary. |
| Corruption/untrusted state | Selected-reader checks cover specific malformed states only; historical replay accepts a corrupt generation. Hashes are not an authenticity boundary against a trusted account that can rewrite both data and checks. Exact production schema/receipt validation is missing. |
| Recovery without publication | Tested precommit rollback restores prior SQL state, which may again expose legacy values and still needs application validation. Tested postcommit recovery retains the committed candidate. Neither establishes arbitrary-state validity or publication permission; retain journals and do not recreate legacy authority after a committed cutover. |

SQLite documents that immutable access skips locking/change detection and may
give incorrect results if its immutability assertion is false. This makes a
general immutable diagnostic on a changing file unsuitable as an exclusion proof.
[SQLite URI parameters](https://sqlite.org/uri.html).
Ordinary separate connections normally see committed transactions, and WAL
readers can retain earlier snapshots. The probes do not disprove that isolation
contract. [SQLite isolation](https://sqlite.org/isolation.html).
Rollback recovery deliberately restores original pages from the retained journal;
deleting it or changing the file identity is not a quarantine recovery protocol.
[SQLite atomic commit/recovery](https://sqlite.org/atomiccommit.html).
These primary references were checked during this investigation; native probe
results remain specific to the recorded local engine/environment.

## 8. Acceptance and rejection criteria

Accept only the bounded experimental observations above. Before C1 can be
accepted as an enforceable production compatibility component, require:

1. A reviewed definition of protected generations and allowed old snapshots;
   no claim to erase values already learned. Retain Decision B for diagnostics.
2. Supported pair reads, writers, model manager, semantic capability/cache,
   indexer, generic Store and diagnostic/setup routing all agree on A within
   the needed snapshot. Absence, invalid strings and unknown models preserve
   agreed semantics or refuse explicitly; no silent semantic coverage change.
3. One exact-context, populated migration with real M6/inventory/receipt integration
   preserves existing state and validates the legacy source inside serialization.
   No separately activatable asset freeze or hidden publication is introduced.
4. Fresh incompatible16 and17 clients are dealt with by an explicitly reviewed
   future compatibility boundary. No actual future migration number is selected
   by sentinel999. Existing handles, prepared statements, diagnostics and native
   recovery must satisfy that boundary independently of fresh Store rejection.
5. Complete exact replay validates the historical receipt's canonical intent,
   referenced generation/pair and integrity; concurrency, crashes and uncertainty
   cannot select a false or conflicting authority. Q09 currently fails this bar.
6. A tested coordination/identity/reopen protocol covers MAIN/WAL/journal state
   and any drain witness without asserting unsupported lease handoff or hostile
   same-UID protection. Unsupported environments are refused or explicitly skipped.

Reject a claim of complete isolation based only on absent markers, a newer schema,
successful checkpoint/drain, one atomic toy transaction or passing reproducer.
Reject an alternative that requires unrelated writer lockout, unreviewed alias
replacement, a second database or encryption without a demonstrated requirement.

## 9. Independent review, verification and limitations

Independent Sol review challenged the candidate and experiment oracles. It found:

| Finding | Disposition |
| --- | --- |
| Historical-receipt replay trusts an inconsistent generation. | Preserved as Q09 unsafe counterexample and an explicit adoption blocker; not hardened into a production solution. |
| Optional-value test initially proved supplied storage, not copied legacy fidelity. | Seeded actual legacy optional/arbitrary values and copied them; result remains a single sample, not a migration proof. |
| Prepared controls initially began after cutover. | Added old `mind_meta` prepared-only and partly stepped statements before DDL. |
| Joint-transaction oracle could pass without asserting floor insertion. | Added explicit MAX version 17, empty L and actual old-doctor incompatibility checks. |
| Diagnostic negative oracle could confuse a missing field with null. | Required complete expected report/field/exit shapes and checked both output streams for new markers. |
| Shared helper/base identity was outside manifest verification. | Verified against pinned Git bytes and checked on reuse; exact experiment byte snapshots are hashed before archival builds. |
| Report wording initially called recovered prior state valid without qualification. | Restricted the claim to observed precommit rollback versus postcommit retention; application validity still requires independent checks. |

The reviewer then authenticated the corrected manifests and independently ran
all ten scenarios across the three root categories: 30 executions/48 case results,
all passing their stated oracles. No remaining confirmed code/runtime finding was
reported. The known receipt counterexample remains in the evidence by design.

Executed checks: standalone rustfmt for both experimental Rust files, Ruff lint
and format for the Python runner, and `cargo clippy --locked --offline --test ...
-- -D warnings` for both disposable archived targets all passed. Exact commands
and logs are under `/tmp/mindspace-s3e3c-npb05ekt/checks.json` and sibling Clippy
logs. No complete or unrelated Rust suite was rerun: no production/shared committed
test infrastructure changed. Prior 64-test/132-execution S3e.3b results are historical
here, not a new S3e.3c regression run.

Environment limits: one host/kernel/engine, local tmpfs and btrfs; no mount-option,
network-filesystem, read-only-mount, unsupported-lease or power-loss qualification.
The runner reports mismatched/unavailable filesystem cases as SKIPPED and probes
lease availability; no primary/independent scenario was skipped on this host.
Native held statements are not historical Store query-cache instrumentation.
The actor paths and supervision operate on owned fixtures; no hostile same-UID
path/alias race protection or real-user-data recovery guarantee is implied.
Normal fixture cleanup is checked; supervisor SIGKILL/power loss cannot be treated
as having run destructor-based artifact preservation. Logs/environment snapshots
remain useful when a hard-terminated case has no final result record.

The first build detected source drift while a final oracle correction was being
made and stopped with `experiment changed; rebuild required`. It is not acceptance
evidence. Only the corrected primary and independent run count. Final checks passed: 67 Markdown files linked, report structure and new-file
whitespace checked, exactly four new untracked files, no tracked/staged changes
and unchanged HEAD. Parent and independent reviewer verified removal of all
192 primary/reviewer passing fixture paths. Manifest/source/executable identities
were independently rechecked after the runs.

## 10. Residual publication blockers and smallest safe next slice

C1 does not resolve automatic old-reader exclusion, coherent supported query
sessions/dual-layout routing, whole-observation and source/alias classification,
permanent affected-writer/vector/maintenance guards, source-only refresh, strict
lexical/semantic coverage, ordinary projection/citations, or the one authorized
fence/retirement/replacement/index/head/receipt transaction required by S3e.
Q10 positively demonstrates why those requirements cannot be replaced by metadata
quarantine. A passive plan or metadata receipt grants no ownership or publication.

The smallest safe follow-up is a **test-only supported Store settings-pair routing
and migration/recovery integration slice**, preceded by acceptance of its precise
contract. Use a disposable populated17 fixture, real M6 revision/permit/inventory
machinery and an explicitly versioned pair-read/update interface. Prove all reader/
writer/cache routes, copied-value source revalidation, historical-receipt integrity
and crash outcomes; keep normal opens and production schema unchanged. Stop rather
than silently disabling current embeddings when routing is unavailable.

Only after that slice and the separately accepted compatibility/coordination
protocol pass could a production migration be proposed. This report allocates no
migration 0018, installs no quarantine, activates no fence or publication, changes
no source ownership/retrieval/MCP behavior, and authorizes no merge/push/S4.
The report and three experiment files remain uncommitted for review.
