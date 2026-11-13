# M8 S3e architecture decision gate

**Decision A — Implement a legacy-layout atomic settings-read prerequisite.**
This selects a bounded next engineering action, **not metadata Candidate A**.
Neither two-key quarantine nor physical metadata relocation is selected.
Publication remains **BLOCKED**. This report performs no implementation and
requires human review before the recommended slice starts.

## 1. Baseline, sources and evidence standard

Verified clean worktree/index on `feat/m8-s3d2-semantic-staging`, signed HEAD
`7f698806c8962d2cedd00b7212ee30158dbffff6`, exact parent
`afe5e43eee0cfa1d680d0ef6c298f428c7ef7aa0`.
GPG reported a good signature from fingerprint
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.
`src`, migrations and Cargo inputs remain unchanged from passive production
`54e5dff`; later accepted commits add documentation and experiment sources.
Schema17 is implemented. Earlier designs' references to a prospective 0017 or
schema16 are their historical baselines, not current migration allocations.

Sources and short references used below:

| Ref | Accepted source and authority |
| --- | --- |
| P | [S3e publication design](m8-s3e-publication-design.md), especially §§1,3–10,14: retained publication requirements. |
| W | [S3e.1 writer fencing](m8-s3e1-writer-fencing-design.md), especially invariants and §§3,6–10,12. |
| I2 | [S3e.2 passive infrastructure](m8-s3e2-fencing-infrastructure.md): implemented schema17 boundary and its limits. |
| R3 | [Reconstructed S3e.3](m8-s3e3-old-reader-exclusion-design.md): explicitly reconstructed; H01–H10 and lost original details remain reported history. |
| b | [S3e.3b harness/report](m8-s3e3b-adversarial-harness.md): P01–P06 and N01–N05; pinned real production binaries and native connections. |
| c | [S3e.3c feasibility](m8-s3e3c-metadata-quarantine-feasibility.md): Q01–Q10; native adapters, real old Store/CLI and specific unsafe counterexamples. |
| d | [S3e.3d layout comparison](m8-s3e3d-layout-cutover-spike.md): d01–d12, failed A/B contract and independent NULL-source finding. |
| e | [S3e.3e real-M6 seam](m8-s3e3e-m6-routing-seam.md): e01–e13, actual private M6 function bodies through an archive-only test attachment. |

**Production** below means current source/implemented paths. **Experiment** means
recorded behavior with committed reproducible sources, not an execution in this
decision turn. **Historical-only** means narrative claims whose recovered raw or equivalent
reusable executable evidence is unavailable here. Lack of production integration
is recorded as a separate limitation; it does not turn a reproducible experiment
into historical-only evidence. **Inference**
is explicitly reasoning; UNKNOWN is a missing guarantee. A characterization PASS
reproduces its unsafe/unresolved condition; it never clears that publication gate.
Counts across different slices are not accumulated into a safety score.

No tests, harnesses or new transient reproductions are run for this decision.
The review relies on accepted reports, experiment source and current source
inspection. Finite runs on one Linux host with tmpfs/btrfs do not qualify every
kernel, mount mode, power-loss condition or old API. Absence of a canary in one
output is narrower than knowledge non-disclosure. Raw same-UID SQL is outside
Model A's admission-respecting exclusion guarantee.

## 2. Evidence-to-requirement matrix

M = mandatory before any applicable publication; C = mandatory for the named
capability/update. “Minimum change” describes a necessary component, not permission
to install a separate active fence. Every M gate still needs joint acceptance.

| ID / precondition | Existing production | Recorded experiment | Historical-only | Architectural inference | Remaining unknown | Failure impact | Minimum required production change |
| --- | --- | --- | --- | --- | --- | --- | --- |
| R01 M: authority and intent | Explicit Model A CLI context; exact permits/receipts for enrollment/preparation/passive plans; ordinary M6 is not publication authority (I2). | e02: genuine M6 R still cannot insert ownership without its private permit. | —; P/W requirements are not execution evidence. | Reuse local context plus action-specific intent/permit; do not derive authority from settings/path/staging. | Exact publication-private authorization is absent. | Ordinary recording scope could be mistaken for authority to transfer representation. | A separate exact publication action/permit with DB identity, canonical intent and immutable result binding. |
| R02 M: exact candidate/base | Fs/Ds/X/C/L/S derivation/staging and passive plans exist; plans do not reserve or activate. | bP04/P05 preserve passive migration state; cQ09/d01/e01 copy bounded metadata fixtures, not a publication candidate. | —; existing staging regressions do not test a publisher. | Revalidate source hashes, cohort, membership, aliases and expected head/R inside final serialization after outside-writer work. | Complete current publication-candidate validation is absent. | Stale or mismatched source/cohort could replace the wrong content. | Publisher candidate/admission validation; no “latest” substitution or automatic rebase. |
| R03 M: old diagnostic metadata exclusion | `inspect_database` reads legacy settings independently of Store admission; doctor retains raw selected model after incompatibility. | bP06/N04, cQ01/Q04/Q07, d05: actual old diagnostic disclosure, including NEW uncommitted value in combined rename+write DELETE preparation. e11 only stable WAL-aware non-disclosure. | R3 H05/H07 do not add newly rerun evidence. | Current-doctor sanitization cannot repair compiled old code; logical COMMIT/floor alone is insufficient. | Safe physical preparation/recovery/admission ordering is unqualified. | An incompatible diagnostic can disclose protected metadata, including uncommitted values. | Select and qualify an exclusion/rollout protocol before installation; neither metadata candidate is approved. |
| R04 M: old active-knowledge observations | Fresh old opens reject newer schema; existing handles do not re-admit on every call. No active-memory read fence installed. | bP01/P02: idle/paused clients survive; cQ02/Q10: old snapshots survive and actual old MCP reads/writes active memory after quarantine. | R3 H04 reports tested memory rename; full original trace/API inventory lost. | Metadata isolation cannot supply the separate critical-`memories` relation fence; OLD pinned snapshots remain allowed. | Complete real-old endpoint/branch audit and populated memory-layout migration are absent. | Old active knowledge or mutation paths remain available; competing/mixed cohorts are not excluded. | P§8/W§7 critical-relation fence without alias, supported transaction-bound routing and complete old-path proof, at joint publication or separately accepted compatibility rollout. |
| R05 M: supported snapshot coherence | Separate Store calls for settings, scope, candidates and decoration; mutex is per Store, not query-wide snapshot. | e04/e06 coherent connection-bound controls; e10 demonstrates split **seam** getters; current caller source supports the same risk; cQ01/d01 show missing production routing. | P's earlier Python F/D snapshot illustration is not a supported query session. | Atomic settings read is a safe first component; complete retrieval must later use the same query connection/snapshot. | Settings/coverage/head/branches/citations do not share a full supported session. | A query can combine decisions from different committed generations. | **Selected next slice:** legacy atomic settings pair and real consumers. Later complete P§8 read-session integration remains mandatory. |
| R06 M: affected legacy writers | Existing M6/provenance/manual guards; passive plans impose no affected-source fence. | bP03/cQ03: queued old writes resume/repopulate unguarded L; static fixture guards or removed target stop tested metadata setters; e05 adds genuine accounting. | P/W retirement/reingestion probes show resurrection; not a future asset-fence proof. | Guard lineage and old/new identity, entire observation/absence domain, lifecycle/membership and late vector saves; producer labels alone cannot authorize. | Affected receiver/SQL guards and complete writer-path proof are absent. | Legacy resurrection, late-vector mutation or completeness loss; global lockout would also violate availability. | Exact private action guards, complete classification and positive unrelated-write controls, activated only with complete replacement. |
| R07 M: source/alias closure | Stored locators and immutable association; passive bindings are unverified, advisory, nonexclusive. | No complete physical-alias/publication classifier is demonstrated. | —; source inventories do not establish universal closure. | Admit bounded namespaces/known aliases, detect known collisions and refuse unknown affected domains; equal hashes/inodes alone grant nothing. | Bounded alias/domain closure and collision admission remain unqualified. | Same-locator new F or known alias can bypass an incomplete fence. | Validated adapter/admin witnesses, old/new-domain classifier and atomic activated bindings; refuse unsupported domains. |
| R08 M: M6 accounting/inventory | Real IMMEDIATE, active-R UDF, commit/rollback hooks and fixed guard inventory; metadata has R but no settings history/read ACL/quota. | e01–e09: genuine existing-guard transaction behavior; e13: new A-shaped table mutates without R while inventory passes. d08 bare DDL has no R; c/d fixed-UDF controls are reproducible but not actual permission equivalence. | —. | Reuse real M6; extra physical tables need explicit guard/inventory integration. R is not administrative authority. | Full selected-layout migration/administrative equivalence is absent. | New domains can mutate without expected R/guards or gain authority incorrectly. | Preserve real scopes, guards/bindings/history; integrate only any later selected layout. **No new transaction manager needed now.** |
| R09 M: projection/cohort/lifecycle | Ordinary memory spine, immutable evidence and M6 history exist; no document publication projection/head. | e01/e11 preserve/extend ordinary rows in fixtures, not a complete F→D transfer. | P's ordinary ingest cost proxies are not publication measurements. | Retire full F cohort and create nonempty complete D cohort with consented uniform spaces, correct conflict policy and monotone Tp; reversion creates fresh episodes. | No complete projection/cohort/membership/Tp transfer has executed. | Empty or competing representation, visibility widening or broken lifecycle history. | Exact L-bound projection/map, cohort membership and lifecycle transitions inside publisher; preserve `.at(T)` contract. |
| R10 M: lexical index/corpus | All-assertions FTS; staging isolated; faithful rebuild/integrity paths exist. | e11 verifies ordinary FTS after ingest; no projected ordinary publication index tested. | P's staged-corpus BM25 illustration and performance proxies remain bounded historical evidence. | Keep private staging out of active corpus; construct ordinary postings from exact selected text, verify actual engine and canonical equality. | Publication ordinary-posting/canonical integrity is unproved. | Hidden staging can influence ranks; missing/wrong postings can conceal incomplete projection. | Publication-time exact ordinary FTS integration/verification under declared corpus resource bounds; no active-only corpus redesign. |
| R11 M/C: vector routes; semantic coverage | Strict document staging exists; ordinary indexer uses another payload recipe; readiness/counts and generic saves are separate. | No complete publication vector-route/Q1 proof; e validates settings, not those paths. | Earlier P resource measurements do not establish mixed-space compatibility. | **M:** exclude fenced F/mapped D from generic candidates/status and protect save/clear regardless of publication policy. **C:** strict owned Q1/profile, complete S and exact vector origins for mandatory semantic coverage. | Mandatory generic vector-route integration and applicable strict-Q1 compatibility remain unproved. | Wrong document payload vectors, late mutations, indexer starvation or silent semantic coverage loss. | **M:** candidate/status filtering plus save/atomic-clear enforcement. **C:** strict query/profile bridge, coverage/origin checks whenever selected policy or prior coverage requires them. |
| R12 M: exact citations/details | S1 resolver pins extraction/ranges; no publication-aware ordinary projection/citation path. | Existing fixture canaries do not qualify canonical citation enrichment or every active detail endpoint. | P proposed UTF-8/canonical tests are not publisher execution. | Same read session must bind selected X/Ds/L, exact quote/hash, projection and original-byte status. | Publication-aware same-session citation/detail integration is absent. | Wrong extraction/quote or partial provenance can accompany an apparently valid hit. | Batched canonical validation and ordinary detail/count/history/citation integration; retain legacy output contract. |
| R13 M: atomic publication | No active fence/head/projector/publisher schema/API. Passive schema17 is deliberately inert. | SQLite/M6 rollback/commit controls demonstrate primitives, not a joint publication. | —; P/W J-gates are acceptance requirements, not executed proof. | Fence, old retirement, complete visible replacement, indexes, head and exact receipt must commit together; no fence-only state. | No joint authorized publication operation/schema/completeness proof exists. | Knowledge gaps, competing owners, split authority or a forbidden fence-only state. | One exact authorized bounded publisher with SQL completeness/deferred bindings; no independent arm/visibility release. |
| R14 M: outcome/replay integrity | M6 known-noncommit versus UNKNOWN; existing exact admin receipts; legacy ingestion lacks caller-operation receipts. | cQ08/e07/e08 distinguish rollback and committed-before-ACK. cQ09 corrupt prototype receipt replays nonexistent generation. e08 retains readable poisoned/autocommit handle while writes refuse. | —; old wording is not evidence that every read is denied. | Resolve durable operation intent/result on healthy inspection; missing ACK/receipt is not rollback or death. | Exact future publication replay and arbitrary unknown-I/O attribution are unqualified. | Blind retry or incomplete receipt linkage can report false success or conflicting authority; Q09 is a prototype failure. | Retain existing M6 classification; validate full new publication receipt linkage/intent and exact replay. No settings-specific receipt needed for selected read slice. |
| R15 M: physical compatibility/recovery | Diagnostic immutable-MAIN/native-RO paths differ from the administrative opener; no qualified metadata/physical recovery cutover. | bN01–N05, cQ04–Q07, d05/d06/d11: stale MAIN, SHM effects, hot journals and ephemeral drain witness. | R3 H06/H08/H10 and original counts are historical, not broader environment qualification. | New-open rejection, existing-handle survival, drain acquisition and actual isolation are distinct properties. A native lease/drain is only a candidate witness, not a mandatory mechanism. | Qualified physical recovery is absent; drain handoff is unknown only if that candidate is chosen. | Pre-recovery exposure or split authority can persist despite SQL commit; a lease witness is not durable isolation. | Qualify the selected storage/rollout/recovery protocol and invalid-state refusal. Drain/handoff proof is required only if that protocol uses it; never delete journals/sidecars as recovery. |
| R16 M: bounded complete admission | Existing staging bounds and actual M6 resources differ; no metadata application quota. | e03 large value/query-only control; no SQLite FULL/power-loss coverage or full publisher verification budget. | P resource/ingest/model RSS measurements are proxies, not guaranteed publisher capacity. | Admit whole operations within versioned intent/corpus/query budgets; expensive model work outside writer; explicit refusal rather than clipping. | Actual publisher/query verification budgets and capacity-failure coverage remain unproved. | Oversized work can cause excessive locks, partial effects or silently omitted content. | Enforce/measure accepted publisher/query resource profiles and real FTS cost; no new global scheduler/quota subsystem. |
| R17 C: later revisions/reversion | Document derivation exists; no managed source-observation-only operation or document publisher. | No demonstrated controlled postpublication Fs refresh or complete reversion. | —; source-only refresh is an unimplemented update requirement. | Append exact Fs without legacy reconciliation/unfencing; preserve mandatory coverage and immutable P/history. | Managed source-only refresh and complete update/reversion operation are absent. | Updates cannot safely obtain new pins; temporary unfencing permits ABA resurrection. | Source-only action/receipt before enabling updates; reversion uses new complete episodes. Deactivation/coverage reduction remain separately gated. |

Existing staging is not reclassified as failed or partially implemented. Its
accepted private-artifact guarantees remain intact; none grants publication.

## 3. Metadata mechanisms: retain primitives, remove unsupported additions

The two “keys” are optional raw TEXT `embedding.enabled` and `embedding.model`,
not cryptographic keys. Absence, arbitrary strings, exact `"true"` and empty model
values retain their existing meanings. This decision creates no second authority.

| Mechanism | Evidence and cost | Decision at this gate |
| --- | --- | --- |
| Metadata Candidate A: two-key quarantine | cQ01/Q03 show bounded legacy lookup suppression/static guards; Q09 receipt replay fails. Separate pair/selector/receipt domains add copy, integrity and replay states. e13 demonstrates actual M6 accounting does not automatically guard new tables. | **Unselected.** Its extra generations/receipts are not proved necessary for coherent settings reads. No production adoption or claim of complete isolation. |
| Metadata Candidate B: same-file whole-table relocation | d01/e01 show bounded value fidelity; e seam preserves genuine existing guards and real R. d05 leaks under **combined rename+NEW precommit preparation**; d NULL source can commit unreadable input. e09 adds test-only prevalidation. | **Unselected.** Simpler layout transformation is promising reasoning, not sufficient adoption evidence. Pure value-preserving relocation is not universally disproved; e09 is not a production migration fix. |
| Existing M6/SQLite | Genuine context, accounting and commit lifecycle exercised by e; no new write authority needed for ordinary settings. | **Reuse.** Existing `set_meta` already uses it. No replacement transaction/permission manager. |
| Smaller production prerequisite | One atomic read of the two existing legacy values, with connection-scoped SQL and wired coupled consumers. e10 supplies direct motivation; source confirms real split sites. | **Selected engineering action under Decision A.** No relocation, new schema, floor or current/new authority selector. |
| In-memory lock/cache or reader registry | Per-Store mutex does not span other connections; old binaries do not register or consult new flags. | Reject as isolation proof. Do not add a layout cache or registration service. |
| Separate DB/encryption/key manager | No separate untrusted principal is supplied by Model A; adds multi-file/key/recovery authority while old handles survive. | No demonstrated necessity; eliminate from selected scope. Not a claim that such mechanisms can never solve a different problem. |

**Explicitly excluded complexity:** metadata-specific generation/history tables,
selectors, bespoke settings receipts, a second authoritative settings copy,
generic layout/router framework, independent writer service, read ACL, application
quota, scheduler and new cryptographic boundary. The selected read needs none.
A new pair setter/CAS protocol is also unnecessary: existing per-key writes are
already M6-accounted; changing multi-step enable/update semantics is separate work.

**Still required where applicable:** publication-head generations, immutable
publication/admin receipts, exact private action permits, canonical input/result
binding, and any later recognized compatibility-layout protocol. Existing global
R can contribute to stale-state checks with canonical state; it is neither a
permission nor a demonstrated replacement for those publication records.
Current-doctor output cleanup cannot fix compiled old clients. A shutdown,
checkpoint, compatibility floor or inode witness cannot replace the accepted
old-reader contract. No diagnostic exception is reinstated (historical Decision B).

## 4. Selected next production slice: atomic legacy settings snapshot

This is a recommendation for later authorization, not work performed here.
Implement one required, object-safe Store read returning the two raw
`Option<String>` values from **one SQLite statement/snapshot** in existing
`mind_meta`. Use the real connection guard and a private SQL helper accepting
`&Connection`, so a future complete query session can call it without recursively
locking Store. Do not implement the required method as two independent getters.
Only one real Store implementation exists in the inspected tree; a new required
trait method is still a public-library source-compatibility risk for external
implementers, to be reviewed explicitly. Do not hide that risk with an unsafe
two-getter default.

Integrate these coupled operational consumers in the same slice:

| Path | Current source evidence | Required resulting behavior |
| --- | --- | --- |
| Uncached semantic readiness | [semantic.rs](../src/retrieval/semantic.rs), lines150–155, reads enablement and model separately. | Fetch once, interpret both fields from that observation; retain genuine disabled/no-model/unknown-model outcomes and propagate storage errors. |
| MCP cached semantic readiness | [cache.rs](../src/retrieval/semantic/cache.rs), line181, invokes the same readiness function. | Shared readiness uses the same pair operation; do not cache settings/head authority. Existing filesystem asset identity checks remain. |
| CLI `index run` | [cli/mod.rs](../src/cli/mod.rs), lines878–885, calls the error-swallowing display helpers. | Select once via fallible pair. A storage error must not become “disabled” or “no model”. Keep manifest/install validation. |

Keep [manager](../src/embeddings/manager.rs) raw option semantics and enablement
interpretation exactly `value == "true"`. `status_in` and index-status displays
are currently best-effort/nonfallible and are **not admission authority**; do not
silently widen this slice into a display API/error-policy redesign. Record their
remaining scope explicitly. A later requirement for coherent status output can use
the pair with separately reviewed display semantics. The multi-step enable workflow
and indexer runtime/derivation ownership are unchanged.

Touch points are the Store contract, SQLite helper/implementation, manager
interpretation, shared semantic readiness and CLI index-run consumer plus their
existing tests. Keep generic `get_meta`/`set_meta` and the existing actual M6 write
entrypoint; introduce no writer manager, new guard or migration. Doctor/setup
already inspect schema and settings in one native read transaction but remain
separate diagnostic/recovery boundaries; do not route them through a live
migrating Store as an incidental refactor.

This advances R05 in actual callers and stops CLI operational storage failures
being mistaken for user configuration. It does **not** complete R05: index coverage
at semantic.rs167 and query scope/head/branches/citations are later reads. Cache's
post-search check validates filesystem assets, not database settings/cohorts.
A pair captured before inference is not a query-wide publication snapshot;
P§8 still requires revalidation and all reads in the eventual supported session.

### Acceptance gate for that future implementation

1. A controlled second actual Store connection atomically switches both raw
   settings; each new pair read returns wholly OLD or wholly NEW. Prove actual
   operational callers use one returned pair, not merely that an unused helper works.
2. Exercise absent, empty, arbitrary, enabled-without-model and valid model states.
   Preserve raw values and stable-database readiness/results. No routing/storage
   error becomes false/None, no lexical fallback is added for a storage failure.
3. Both cached/uncached readiness and CLI index selection use the new operation.
   Keep manifest/install/runtime failure categories and existing cache asset-change
   checks; include a storage-failure control for CLI's current display-helper misuse.
4. Reads consume zero R; existing setters keep one R per successful transaction,
   existing guard/rollback/UNKNOWN behavior and unrelated ordinary ingestion.
   No new setting write, capability toggle or ownership effect occurs on read/open.
5. The helper works with an already owned connection/read transaction and cannot
   introduce recursive mutex locking or a second hidden snapshot. Standalone pair
   reads do not claim that a later model/index query is in the same snapshot.
6. Run appropriate default and genuine no-embeddings builds/tests plus targeted
   settings, readiness/cache, index selection, M6 and retrieval regressions; broader
   checks follow any shared trait/caller changes. Verify effective features:
   [Cargo.toml](../Cargo.toml) enables embeddings by default and its self dev-dependency
   can request defaults again; a `--no-default-features` label alone is not evidence.
7. Diff shows no schema/floor/table rename/alias, new metadata authority, active
   fence, publication, vector eligibility/ranking policy or MCP contract change.
   Generic keys remain supported; no blanket unrelated-writer restriction.
8. Document remaining R01–R17 gates. Reject a completion claim that declares old
   diagnostic exclusion, all-API old-reader exclusion or publication safe.

Reject an unused accessor; a two-getter default; pair-valued old getters still
called twice; process-local layout caching; error-to-disabled conversion; incidental
pair-writer/receipt infrastructure; or a claim that this implements P§8 in full.
These gates concern production integration, not another exploratory harness.

## 5. Publication critical path and sequencing

The selected read prerequisite may proceed independently of physical metadata
isolation; it activates nothing. The remaining paths converge before publication:

1. **Supported coherence:** selected atomic settings read → complete P§8 query
   sessions across settings, scope, head, eligibility, both branches, ranking,
   counts/details and citations. Runtime work outside writers; fingerprint recheck
   in the selected query snapshot. No stale global layout/head cache.
2. **Old-client/physical boundary:** metadata diagnostic/recovery protocol AND
   separately proved old active-memory read fence/whole-response audit. Fresh
   refusal, old-handle safety and prepared/held-snapshot behavior need separate proof.
   Native inode drain is an unselected candidate witness; acquisition/handoff proof
   is required only if a later accepted protocol uses it. d05/Q10 remain positive
   blockers. Selecting a metadata mechanism cannot clear Q10.
3. **Mutation/identity boundary:** admitted bounded source/alias closure and complete
   receiver/SQL guards, including stale vector jobs and membership/lifecycle paths.
   Exclude fenced F/mapped D from generic index candidates/status and guard vector
   save/clear for every published cohort, including lexical-only publication.
   Demonstrably unrelated supported work stays usable; intersecting atomic batches
   refuse as a unit, not by silently dropping protected entries.
4. **Publication data/index/authority:** exact source/lexical/canonical projection,
   membership/monotone lifecycle/citations, applicable strict semantic profile and
   actual FTS/vector verification within resource bounds. Exact private publication
   permit plus all complete replacement/fence/index/head/receipt effects commit in
   one M6 transaction. No independently activatable freeze or fence-only migration.
5. **Recovery acceptance:** the joint publisher, each admitted physical environment
   and its outcome/receipt/healthy-inspection protocol pass known noncommit,
   committed-before-reply, corruption, stale writer and concurrent operation gates.
   Absence of a receipt or rejection of a retry proves no earlier rollback/death.

Semantic Q1 is conditional on mandatory semantic coverage, not optional when that
coverage exists. Initial lexical-only admission conservatively requires no
ordinary F assertion vector rows, including historical or ambiguous origins.
It cannot downgrade existing F or earlier D semantic coverage. Source-only refresh
is mandatory before enabling postpublication revision updates. Reversion preserves
coverage and creates new episodes; deactivation/coverage reduction, recorded-R
retrieval, active-only FTS statistics, universal unknown-alias discovery and larger
resource profiles are separate gates, not prerequisites to silently invent now.

No further generic A/B metadata prototype is needed to justify the selected
legacy reader change: e10 and real caller source already establish its target.
Another prototype would not wire production consumers. Conversely, physical
recovery evidence still matters before choosing/installing an isolation protocol;
this recommendation does not dismiss that unknown or claim the next production
slice settles it. The prior e report's suggested physical probe remains a possible
later narrowly specified evidence task, **not the selected next action here**.

## 6. Decision challenge and review

The strongest objection is that pair reads are useful hygiene while publication's
largest blockers remain. The bounded answer is operational: source inspection identifies separate reads
in real readiness/index callers; e10 demonstrates the failure mode through the
real-M6 seam, not a separately executed production-readiness race. Coherent
settings are an
explicit component of P§8. Wiring those consumers removes a real production
ambiguity without creating an incomplete compatibility cutover or authority state.
If the slice only adds an API, or claims completion of the whole read session,
this justification fails. Selection is based on that causal link, not test totals
or a claim that the prototype with the most PASS results wins.

Decision B (redesign) is not required for this bounded read integration: no new
layout has been installed or selected to redesign. Decision C (no production
implementation justified) would ignore the demonstrated e10/source-level gap.
This does not make either physical metadata candidate suitable; those remain
unselected with exact missing evidence listed above.

Independent review and documentation verification follow. No implementation,
migration, new experiment or test suite is performed by this decision gate.
Only this report is added, uncommitted for human review.

### Independent adversarial review and disposition

A read-only Sol planner and a bounded source explorer independently supported the
legacy atomic-read prerequisite. A separate read-only Sol reviewer challenged the
recommendation against the accepted designs, evidence reports and actual callers.
It found no HIGH/BLOCKER objection to Decision A and required three corrections:

| Finding | Disposition |
| --- | --- |
| MEDIUM: strict-Q1 conditionality was incorrectly shared by vector candidate/status/save/clear protection. | Corrected R11 and the critical path: vector-route protection is mandatory regardless of publication policy; strict Q1/semantic coverage is conditional on the applicable policy/prior coverage. |
| LOW: drain/reopen looked like a selected mandatory gate. | Corrected R15 and sequencing: physical recovery qualification is mandatory; an inode-drain mechanism is unselected and its handoff proof is conditional on later selection. |
| LOW: “historical-only” mixed evidence provenance with missing production maturity. | Corrected the definition and matrix: reproducible c/d simulations remain experimental evidence with an integration limitation; design requirements are not historical execution claims. |

The reviewer confirmed that the narrow production prerequisite advances
supported-reader coherence **only with actual coupled consumers wired**. It also
confirmed retention of publication BLOCKED, unselected metadata candidates and
the separate physical, old-reader, writer and joint-publication gates. Review was
source/document inspection only: no suite, harness or new reproduction was run.

Documentation checks cover relative links, table/heading structure, whitespace,
current signed baseline, empty index, unchanged production inputs and a diff
confined to this one new report. No implementation is claimed as delivered or
authorized merely by the reviewer's agreement. The human remains final authority.

The reviewer rechecked the corrected report and reported **no remaining findings**.
Gate result: **ACCEPT for this bounded architecture recommendation, Decision A**;
publication remains **BLOCKED**. Neither metadata candidate is selected. Acceptance
of this report is not implementation, rollout or publication authorization.
