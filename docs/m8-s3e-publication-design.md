# M8 S3e: atomic publication and retrieval visibility

Status: **design and feasibility gate only**. Every new API, table, state, guard
and acceptance case in this document is proposed. Publication remains disabled.
Only this document changes. S3e.1 finalization may commit this reviewed design
locally; no production schema/code, active index, MCP definition, merge, push or
S4 change is delivered. The S3e.1 design remains a separate review artifact.

Verified baseline: signed `254d1ad5794db8ac19dc187a5934910337da9b9e`, parent signed
design `ed91c781d8de74ed0b0a0847015adae096969293`, branch
`feat/m8-s3d2-semantic-staging`, worktree `/tmp/mindspace-m8-s3d2`, initially clean.
The previous **1216/0/17** full suite is historical; this design does not rerun it.

Read with [S3a](m8-s3a-retrieval-ownership.md),
[S3b authority](m8-s3b-ownership-core-plan.md) and
[implementation](m8-s3b-ownership-core.md),
[S3c design](m8-s3c-document-ownership-plan.md) and
[implementation](m8-s3c-document-ownership.md),
[S3d design](m8-s3d-staged-index-design.md),
[lexical staging](m8-s3d-lexical-staging.md),
[semantic design](m8-s3d2-semantic-staging-design.md) and
[implementation](m8-s3d2-semantic-staging.md), and the
[S3 token/citation contract](m8-s3-chunking.md).

## 1. Selected contract and precedence

Select **real projection into the existing evidence/assertion/memory spine**.
Use a permanent managed-writer fence, a separate effective publication head and
one database snapshot per retrieval operation. Private staging stays private.
Do not federate `doc_staged_fts` or staged vectors directly into active search.

For a managed logical asset A and a supported query snapshot Q, the set of
eligible **active representation cohorts** has cardinality at most one. A
successful publication replaces a nonempty eligible old cohort with a complete,
nonempty eligible new cohort atomically, in every required mode and space.
This is not a guarantee that every query string ranks the asset in its top K.
Many chunks in one selected cohort are one representation, not competing owners.
Explicit history queries may return several retired/past cohorts, labeled as
history. Explicit deactivation without replacement is a different, separately
authorized action, never a successful publication disguised as an empty result.

This refines S3a's deferred strategy A. Its old publication issuer/lease proposal
is superseded **explicitly** by the user's Model A decision for this local
administrative operation. There is no verified human principal, external grant
issuer, asynchronous revocation, hostile same-UID isolation or new sandbox
authority. Ordinary M6 recording permission is not publication permission.

Schema 0013's `asset_ownership(owner=legacy, epoch=1)` remains immutable enrollment
evidence. It is not edited to say `document`, and its epoch is not incremented.
A new authoritative effective-representation head/generation controls managed
retrieval. Call its counter **publication generation**, not S3b enrollment epoch.
All current-owner readers must use the new effective view, not contradictory
interpretations of the enrollment row. With no publication head, enrollment and
passive plans leave both ordinary legacy writing and retrieval unchanged. An
effective fence without a complete visible document replacement is forbidden.

The ranking policy remains the existing **all-assertions ordinary FTS corpus**.
Only an actual successful publication may add document assertion/FTS rows.
Unpublished staging has zero shared-index influence. After publication, adding
eligible document assertions can change other assets' BM25 scores/ranks; retired
historical assertions already remain in that corpus today. This design does not
promise active-only statistics or historical ranking reconstruction. If either
is required, this architecture needs a separate active-index design gate.

## 2. Existing architecture and migration boundary

These are observed code facts, not proposed publication behavior.

| Existing boundary | Actual behavior and implication |
| --- | --- |
| [SQLite writer](../src/storage/sqlite.rs), `begin_write`, `record_observation_tx`, `reconcile_logical` | `BEGIN IMMEDIATE` serializes mutations. Reconciliation is logical-source AND producer-family scoped; it cannot transfer F to D. Unchanged file ingestion deliberately recreates manually retired memories. |
| [Legacy extraction](../src/ingest/mod.rs), [persistence adapters](../src/ingest/extract.rs) | File/directory observations reach the same receiver, including move/absence decisions. Full observations may mark unobserved paths absent; dropping managed files from a batch is unsafe. |
| [Ownership](../src/storage/sqlite/ownership.rs), [0013](../migrations/0013_legacy_ownership.sql) | F-only enrollment, immutable successful receipt/history, private admin permit. No writer fence, path reservation, current document owner or publication exists. |
| [Preparation](../src/storage/sqlite/document_preparation.rs), [0014](../migrations/0014_document_preparation.sql) | A–F–D association and initial Fs/Ds/X pins are immutable. Later explicit staging can select compatible newer versions under the same association. Preparation is neither a current head nor approval. |
| [Lexical input/staging](../src/storage/sqlite/lexical_input.rs), [0015](../migrations/0015_lexical_staging.sql) | L binds P/Fs/Ds/X/C, exact coverage/entries/anchors and its receipt. Private FTS is one corpus across staged sets; its historical verification does not establish current cache health or publication authority. |
| [Semantic staging](../src/storage/sqlite/semantic_staging.rs), [0016](../migrations/0016_semantic_staging.sql) | S binds exact L/receipt/R/result and strict model/tokenizer contract; complete vectors/receipt commit atomically. There are no active assertion/vector writes. Inspection distinguishes historical success, canonical/vector integrity and unchecked availability. |
| [Ordinary FTS](../migrations/0001_init.sql), `get_or_create_assertion`, `verify_fts_index`, `rebuild_fts_index` | Every assertion has an FTS row, including historical assertions. Rebuild copies all assertions atomically. Filtering MATCH results cannot remove hidden rows' corpus statistics. |
| [Retrieval](../src/retrieval/mod.rs), `search_with_mode` | Scope expansion, lexical branch, vector branch and decoration use separate Store calls. The Store mutex is not a query-wide SQLite snapshot against other connections. RRF deduplicates assertion IDs, not assets. |
| [Semantic readiness](../src/retrieval/semantic.rs), [query cache](../src/retrieval/semantic/cache.rs) | Readiness uses a single legacy key/count; counts include retired vectors. Cache is process-local query runtime, not persistent staged-vector membership. |
| [Indexer](../src/indexing/mod.rs) | All missing assertions can be selected; input is `subject: content`, generic embedding can truncate, and per-item vector writes happen after inference. This must never process mapped document projections. |
| [Filters](../src/storage/sqlite.rs), `build_search_filters`; [M6](../migrations/0008_temporal_recording.sql) | `.at(T)` filters episode timestamps but uses current spaces/links/index/configuration. Source absence alone does not retire knowledge. Recorded R coverage is different and incomplete for ranking/indexes. |
| [S1 resolver](../src/storage/sqlite/documents.rs), `resolve` | Anchor pins logical D, original hash, detector/extractor/params and canonical range. It never chooses a newest revision. Publication-aware validation must additionally bind selected X/Ds/L and verify actual canonical/payload hashes, using a shared read session. |

Migration history reviewed through 0016:

| Versions | Responsibility retained |
| --- | --- |
| 0001–0002 | Evidence/assertion/memory spine, shared FTS; source versions, logical file lifecycle, sessions/observations/events |
| 0003–0006 | Git identities and ref history; code structure/relationships; separate manual provenance |
| 0007–0009 | Ordinary assertion vectors/settings; M6 baseline/recording history; scopes/space links/repository views |
| 0010–0012 | Document identities/extractions/anchors, Markdown code-block node support, immutable chunk sets/ranges |
| 0013–0016 | Local legacy enrollment, document association, isolated lexical artifacts, isolated semantic artifacts and immutable receipts |

No existing migration is rewritten. Future migration numbers are rechecked before
implementation; 0017 is the next candidate, not an installed schema in this task.
Older binaries reject newer schemas on open, but this alone does not evict an
already-open old process; the rollout prerequisite in section 8 is mandatory.

## 3. Writer and observer inventory

| Path | Actual convergence / required future guard |
| --- | --- |
| CLI file/directory `ingest` | `ingest_paths` → `persist_files_gated` → `record_files_ingest`; fence complete observation before any session/source/scope/absence/reconciliation write. |
| MCP `repo_ingest` normal and isolated directory paths | [repo_tools](../src/mcp/repo_tools.rs), [isolated](../src/mcp/isolated.rs); validated extraction reaches the same receiver. Client permission/grant is not ownership permission. |
| Git CLI/MCP/isolated worker | [isolated_git](../src/mcp/isolated_git.rs), [worker client](../src/worker/client.rs) → `record_git_ingest` / `record_git_observation`; Git identity is repository/commit/path, not local F. Do not adopt/suppress Git knowledge by matching a pathname or text. |
| Public library observations | `Store::record_observation`, `record_files_ingest` accept public drafts today; no private ownership admission/epoch. No generic `Store::record` API exists. Match protected lineage and admitted locator namespaces at storage; do not disable unrelated observations merely because a managed asset exists. |
| Ordinary memory creation | `create_memory`, via file/Git reconciliation or manual writes; inspect lineage through assertion→evidence→source, not caller producer labels. |
| MCP/CLI explicit lifecycle | `retire_memory` currently accepts any active memory; `add_memory_spaces` currently accepts any active memory. Managed F/D changes require the appropriate private action, not generic lifecycle permission. |
| Manual create/update | `record_manual_write`; update already checks manual source/extraction. Unrelated manual memories remain usable; a managed target cannot be substituted by UUID or assertion reassignment. |
| Incremental indexing | `list_assertions_needing_embedding` → inference → `save_embeddings`; candidate filtering AND storage/SQL enforcement are needed against stale F jobs and new D assertions. |
| Library maintenance | `clear_embeddings`, `set_meta`, `rebuild_fts_index`; clear is not currently a CLI/MCP command. Refuse vector deletion touching protected sets. Atomic faithful all-assertions FTS rebuild may remain supported with integrity checks. Metadata must never grant/release ownership. |
| Space writes | Membership additions/removals on managed memories must be controlled. Ordinary space-link changes remain explicit existing reachability operations, observed in the same query snapshot; they do not transfer ownership. |
| Document derivation | `record_document_extraction` / `record_chunked_document` append Ds/X/C without ordinary knowledge. Optional original-byte erasure does not erase canonical text. These remain derive-only. |
| Code facts | `record_code_observation` records syntax facts, not ordinary memories. `code_search` is a separate view, not another active memory producer. Do not silently assign it document ownership. |

No production durable queue or background scheduler was found. Existing workers
extract/read; the parent commits validated results. External queued CLI/library
jobs are still stale-writer cases. Do not invent a scheduler to fence them.

Active observation paths include `search`, `search_vectors`, all four retrieval
modes, `list_spaces_with_counts`, `list_conflicts`, direct memory details, subject
history, replacement chains, ID lookup, CLI Search/Recall/Inspect/Trace/History,
and existing MCP memory tools. Counts/conflicts currently inspect physical active
episodes. Leaving F physically active under an application-only search filter
would leak a competing representation through those views.

### One Markdown asset

Today, `/notes/a.md` gives F/Fs and `file:legacy` episodes via ordinary ingestion.
Explicit document extraction creates distinct D/Ds/X/C; S3b A and S3c P associate
identities, L/S stage exact content. None changes F's active results.

Proposed publication names A/P/L and optional S, validates the precise current
legacy cohort, aliases and spaces, then commits permanent asset-local fencing,
ordinary D projection, F retirement, effective head and receipt together. A repeat ordinary ingestion
must fail at storage even when the content is unchanged. Otherwise current
`record_observation_tx` would recreate the retired F episodes.

## 4. Logical asset identity and permanent writer fence

A is the explicitly authorized closure of enrolled F, associated D and approved
locator/observed-file bindings. It is not “every file with equal bytes.” Copies,
Git snapshots and separately authored equal text are not automatically one asset.
Paths and observed device/inode information support **negative collision fences**;
they do not grant ownership. Enumerate current and historical known locators,
known aliases and conflicting file/document identities in the preview. Known
ambiguous identities or existing competing F/D knowledge block initial adoption.

A passive plan may record the administrator's proposed identity closure and exact
expected state. It reserves neither locations nor authority and has **zero effect
on ordinary writes or visibility**. Continued legacy writing can make it stale;
refresh/reconfirm the plan rather than freezing the asset.

Only the explicitly authorized **publication transaction** installs the effective,
append-only managed writer fence, together with complete visible D knowledge,
old-cohort retirement, head and successful receipt. No standalone arm action or
committed `ActiveFence + NoDocumentReplacement` state is permitted. Once committed,
ordinary writes intersecting A/F/D or activated locator bindings fail atomically.
The restriction is permanent in the first protocol: no release/reassignment or
temporary unblocking. This avoids an ABA hole where an old queued job becomes
admissible after a later compensation. Restore/deactivation require another gate;
the initial schema cannot encode them.

An F-only check is insufficient: partial ingestion of an absent/replaced location
can create another F at that same location, and file locations are not unique.
Producer-only checks are insufficient: custom/isolated families can still belong
to F. Bind and check both old/new logical identities, source lineage and reserved
canonical locators. Initial adoption requires all active F episodes to be the
recognized `file:legacy` family with a uniform, nonempty space set; D must have no
ordinary active knowledge. Unknown producer families, mixed provenance and
ambiguous per-section membership require a separately reviewed repair/consent
operation, not automatic retirement or union widening.

### Scoped observation classification is a real prerequisite

Current drafts contain location strings, not verified physical identity or a
supplied F ID. Inside `BEGIN IMMEDIATE`, classify the complete observation and
its disappearance/move domain against protected F/D lineage and **activated**
locator bindings. Check both inputs and resulting source/classification plans:
a same-location replacement can otherwise create a new F. The authority for
negative bindings comes from explicit publication, never a path or metadata edit.

Supported adapters may provide validated roots, canonical locations and bounded
file-identity observations using their existing filesystem authority. A naked
`SessionDraft.root`, caller flag or producer string is not such a witness. Public
opaque drafts may still match an admitted exact namespace; no database-wide
private-token requirement or refusal follows from the existence of one managed
asset. Demonstrably disjoint ordinary file, manual, Git and code writes continue.

Do not skip a protected file and then perform directory absence reasoning. Reject
an intersecting atomic observation in full, including otherwise unrelated entries
in that batch. Separate unrelated observations remain writable. Partial batch
progress needs a separately reviewed partitioning API preserving scope, code,
move and absence semantics; it is not promised here.

Unknown physical aliases cannot be discovered universally from stored content.
Explicitly admit known aliases; canonicalization, observed hardlinks and historical
locators can support collision checks but never grant ownership. Unknown or stale
closure evidence refuses publication for that asset/domain, rather than disabling
all opaque writes. Inode reuse may cause conservative refusal. If a producer cannot
classify its affected domain without wider filesystem authority, that domain is
unsupported for publication. No global physical deduplication or protection from
an undisclosed alias is claimed; administrative consent must state the limit.

### Storage enforcement

Guards must cover logical/source INSERT/UPDATE/REPLACE (old AND new identity and
locator), source events/observations and absence/move decisions, evidence/assertion
creation, memory INSERT/UPDATE and assertion substitution, memory-space changes,
and vector INSERT/UPDATE/DELETE. Check source lineage through joins. All supported
write entry points must validate before any effects, with SQL guards as the final
boundary and exact action-private permits in addition to M6 recording context.
BEFORE INSERT collision guards cover REPLACE independent of recursive triggers;
FK-off/UPSERT attacks do not confer a private permit. Already-open old writers
must not bypass affected-row guards. Missing new UDFs may fail statement preparation
even when a trigger's `WHEN` is false: this can reject unrelated old-client writes,
not prove asset-local enforcement. Test supported-client positive unrelated writes
and separately document the database-wide old-client upgrade boundary. Passive
prepublication slices install no guard on ordinary tables or read-relation rename.

Generic indexing must skip fenced F and mapped D assertions in candidates/status,
then recheck at `save_embeddings`/SQL. Otherwise it either starves on protected
old rows or creates wrong `subject: content` document vectors. Generic clear of
a key touching protected vectors refuses atomically; no partial clear. A late
indexing result from before the fence cannot add vectors after publication.
Unrelated manual/Git/unmanaged writes keep their existing authority and lifecycle.

An ordinary legacy ingest has no durable successful-operation receipt today.
If its earlier result was UNKNOWN, refusal of a later fenced retry proves only
that this retry wrote nothing. Neither a publication receipt nor receipt absence
resolves that old legacy transaction's outcome automatically.

## 5. State machine and lifecycle

Let P be the immutable preparation association, L an exact lexical artifact and
S an exact semantic artifact with its stored strict descriptor and result digest.
Every selection names Fs/Ds/X/C/plan; none means “latest extraction.”

```text
Legacy ordinary F
  -- existing explicit S3b enrollment + S3c association --> LegacyPrepared(A,F,D,P)
LegacyPrepared(A,F,D,P)
  -- derive / stage L,S / passive fence plan --> same writes and visibility
  -- authorized atomic publish + activate fence --> DocumentPublished(g=1)
DocumentPublished(g), permanent asset-local fence
  -- new explicit complete publish(L2,S2?) --> DocumentPublished(g+1)
  -- explicit select old L/S --> fresh complete generation/episodes (reversion)
  -- restore / release / deactivate --> refused by initial protocol
```

Before publication, ordinary legacy writes remain allowed, including after a
passive plan or verified L/S exists. Any changed expected cohort/base makes a
candidate stale; retain its immutable derivation, then explicitly refresh/reconfirm.
A valid old snapshot is not proof it covers currently eligible legacy knowledge.
Initial publication requires selected Fs to account for the current recognized F
cohort; mixed source versions or different active content need fresh derivation.
The publication action requires existing A/P/F/D; an unenrolled F, missing P/D or
path-only request refuses. No implicit enrollment or optional-D active fence exists.

No head means no effective fence and ordinary legacy presentation/writes. Passive
plans cannot deny writes. The first publication schema/API represent only complete
DocumentPublished heads with permanent restrictions. Later restore/inactive actions
must introduce their typed transition/receipt/read semantics together, preserve
stale-writer safety and explicitly resolve the replacement/withdrawal policy.
Do not precreate an unguarded `inactive` flag or promise ordinary legacy unfencing.

| Event | Required behavior |
| --- | --- |
| Repeat same full operation | Original immutable receipt first, same result/R; no new episode or revision. |
| New operation, identical selected manifest/head/policy | Verified read-only AlreadyPublished; no fabricated receipt or generation. |
| Different document revision | Retain old active generation while deriving/staging; new explicit publication retires old and creates complete new cohort at one boundary. |
| Reversion | Select exact old immutable L/S; reuse its source/assertion/vector evidence where valid, create new memory episodes, never reopen retired timestamps. |
| File deletion/disappearance | Record only through controlled managed observation; retain knowledge by default, as current legacy absence already does. No implicit withdrawal. |
| Move/alias change | Explicit binding update retaining old reservations; no path-based ownership reassignment. Conflict with another identity refuses. No automatic claim of an unknown alias. |
| Original bytes missing/erased | Retained canonical citation may still resolve; original-byte status is explicit and not upgraded. No newer revision fallback. |
| Incomplete/corrupt candidate | No publication; previous head/visibility stays. Private partial work is not a selected generation. |
| Published required data corrupt/unavailable | Explicit integrity/unavailable response, not silent omission, old-owner resurrection or new-revision substitution. Historical receipt remains historical. |
| Explicit deactivation/restore | Separate later action with exact expected generation, cohort and visibility-reduction/retained-snapshot consent; initial publisher refuses it. Fence is never released. |

### Required managed source-only refresh

After publication, the ordinary fence also blocks creation of a new Fs through legacy ingestion.
Existing document derivation supplies Ds/X/C, not Fs. Therefore later revisions
require a separately authorized managed **source-observation-only** operation:
append/reuse exact Fs under enrolled F, with byte hash/size, presence/location
observations and immutable receipt, without calling `reconcile_logical`, creating
ordinary evidence/assertions/memories or changing active visibility. It may pair
with existing explicit document derivation, then L selects the exact new pins
under the existing immutable P. Do not mutate P's initial Fs/Ds/X or temporarily
unfence ingestion. This API does not exist today; revision-update support remains
blocked until implemented and tested.

## 6. Semantic policy and ranking

Choose one versioned publication policy in the confirmed intent:

| Policy | Mandatory admission and query contract |
| --- | --- |
| `lexical_only@1` | Complete canonical/L evidence and newly built ordinary FTS. No semantic-ready claim. Conservatively refuse initial publication if any ordinary F assertion vector rows exist, including historical or ambiguous origins, so usable prior semantic coverage cannot silently disappear. No-embeddings builds may implement this restricted unindexed-asset path. |
| `semantic_required@1` | All lexical requirements plus exact complete S for L, same Fs/Ds/X/C, actual verified strict contract/vector evidence, and the implemented Q1 profile below. Fresh publication also requires authenticated owned query-runtime preflight outside the writer. No partial set or automatic downgrade after model/vector failure. |

For a managed cohort not covered by the requested current model/profile,
Semantic and Hybrid return explicit `ManagedSemanticCoverageUnavailable`.
Auto may disclose a **whole-query lexical fallback** under its existing fallback
contract; it never falls back to fenced F rows. Do not return an apparently complete
semantic subset after dropping uncovered managed assets. Evaluate coverage over
the requested scope and timestamp-eligible cohorts, not every unrelated asset.
Changing enablement/model settings or removing weights later may make a mode
explicitly unavailable; it does not transfer ownership. Settings never grant
publication or rewrite its mandatory coverage policy.

The coverage floor applies to **every** publication/update/reversion, not only
the first F→D handoff. The new generation's mandatory modes must include those
of the preceding selected cohort. In particular, `semantic_required` D→D cannot
silently become `lexical_only`, even when model files are missing or embeddings
are currently disabled. Intentional coverage reduction requires a separate
explicit availability-reduction/deactivation policy and implementation gate;
it is unsupported by the initial publisher.

The current strict encoder is document-only. Before semantic publication add an
owned strict query encoder/compatibility bridge binding actual assets, effective
configuration, tokenizer, `search_query: `, specials, pooling, normalization and
dimension. Reject queries above 2048 total tokens. Explicit Semantic/Hybrid fail;
Auto may report lexical fallback. Never silently truncate a document-capable
query. Scope with no managed assets preserves the existing legacy behavior.

### Initial Q1 compatibility and availability contract

Define `query_contract@1` from exact model ID/revision and weights bytes/hash,
tokenizer bytes/hash/engine, Nomic architecture/effective config digest, CPU/F32,
query prefix, specials/padding/truncation/token-type policy, 2048 full-input limit,
dimension 768, attention-mask mean, L2/finite verification and query algorithm
version. Q1 uses the official pinned Nomic v1.5 assets/config from S3d.2,
`search_query: ` exactly once, specials enabled, no padding/truncation, token-type
input absent, Candle 0.11.1 and tokenizers 0.23.2. Its versioned tuple/hash is part
of the publication profile/intent; model name and dimension alone never select it.

The first compatibility table has only these explicit classes:

| Class | Admission to the same Q1 vector space |
| --- | --- |
| Existing legacy recipe | Exact `derivation_from_manifest(NOMIC_V1_5)` key AND the complete known registry manifest/legacy payload recipe. This retains the existing declared legacy provenance; it does not authenticate weights used by historical individual rows or relabel their stale runtime metadata. |
| Strict document recipe | Selected S/L with supported v1 descriptor, exact official assets, effective config, both prefixes, source-slice payload policy, pooling/normalization/serialization and reviewed CPU/runtime/tokenizer versions matching Q1's space contract. The ordinary vector-origin map must match S's actual winning bytes. |

One authenticated Q1 query vector may serve both admitted groups; eligible legacy
keys and mapped strict document keys are selected in the same read session.
An unrelated lock/build fingerprint is not automatically a new vector space,
but any compatibility exception must be explicitly in the versioned table;
unreviewed settings/runtime/payload changes refuse. Future models/classes require
another reviewed profile. Unknown/incompatible **selected mandatory** groups block
publication or make the affected managed query unavailable. Unselected historical
or custom legacy keys retain their existing ignored status; they do not cause a
global failure of unrelated legacy queries. No unsafe key alias/rewrite occurs.

Fresh semantic-required apply must authenticate/load an owned Q1 runtime and run
a bounded strict query sentinel/finite-dimension-normalization check before its
writer, retaining the owned runtime through commit. Hold the same actual verified
asset/config snapshot; do not authenticate one path then reopen different bytes.
The pure preview still makes no availability claim. Missing feature, files or
unsupported Q1 construction refuses new publication. Historical receipt replay
and read-only AlreadyPublished inspection remain independent of fresh availability
and label unchecked observations accordingly. Later external asset loss can make
a future query explicitly unavailable; publication does not promise perpetual
filesystem/model availability or warm every process's cache.

Numerical cosine compatibility and mixed-payload recall still need executable
acceptance with real assets; no identical ranking promise is made.
RRF retains its existing lexical/semantic rank semantics after eligible candidates
are selected. Deduplication by assertion is not an ownership mechanism.

Do not use staged FTS with a head filter: unpublished entries would still alter
IDF/average length. Build ordinary FTS from exact selected text only inside the
publication transaction, never copy FTS shadow tables/statistics. Retained F/old D
assertions continue contributing to the declared historical ordinary corpus.
Rank changes caused by an explicit publication are expected; changes caused only
by preparation/staging remain prohibited. An active-only corpus alternative is
separate work, not a hidden promise of this design.

## 7. Projection, membership and exact citations

Use a fixed producer family `doc:chunk@1`; extractor/chunker/model changes do not
silently create competing producer families. The projector has its own version.
Refine S3a's earlier chunk-only identity to bind **L and entry identity**:

```text
projection_key = H(doc-projection@1, A, D, L.stage_id, L.entry_id, projector_policy)
vector_origin = (projection_assertion_id, strict_pipeline_key, S.id,
                 S.result_digest, S.entry_id/vector_key, vector_hash)
publication_manifest = H(A, P, exact Fs/Ds/X/C/L/S tuples and result digests,
                        ordered projection/skip records, target spaces,
                        index/query/citation/projector policies)
operation_id = local-doc-publish@1:nonce:H(canonical INPUT intent)
```

L changes can produce different valid S floats under the same chunk/model; using
L-bound assertion identity avoids collision under ordinary `(assertion_id,key)`.
Same L/pipeline has one immutable S winner. Model upgrades use another key.
Store an immutable vector-origin mapping; reuse a preexisting ordinary vector
only if it matches that exact selected origin/hash/bytes. Never compare a new
inference to the winner, silently accept `DO NOTHING` with another origin or
overwrite a vector. Publication performs no document embedding; its bounded Q1
query sentinel, when required, runs outside the writer.

Allocate real ordinary evidence/assertion/memory UUIDs. Evidence content and
assertion content are exactly the canonical entry slice. Use S3's half-open UTF-8
byte interval and root anchor, never invented line positions. Allocate new section
evidence ordinals monotonically per Ds inside the transaction; do not assume
`ensure_version` inserts new evidence for an existing source. Persist the mapping
so exact retries/reversion reuse immutable evidence/assertions. Every distinct
publication generation creates fresh memory episodes after retiring its old cohort;
only exact no-op/replay preserves episodes. Do not fake a one-to-one supersession
between legacy sections and document chunks; use transition-manifest links.

Project every non-whitespace chunk, without trimming or normalizing its content.
Whitespace-only chunks remain covered and cited in the manifest with a typed
non-queryable skip reason; empty effective projection cannot replace searchable
legacy content. Heading/section/file labels may form a bounded, versioned display
and FTS subject, clearly labeled metadata. They are not copied into vector input
or the cited quote. A hit matched via metadata must not imply those labels occur
inside its source interval.

Initial target spaces equal the uniform existing F cohort membership. Bind the
exact space IDs and fingerprint; no implicit union, dropped space or widened
section visibility. The initial publisher refuses an absent or empty active F
cohort. Initial activation without a predecessor needs a separately authorized
policy and acceptance gate; explicit target spaces alone do not authorize it.
Freeze managed membership edits except controlled whole-generation transitions. Existing directed space links still determine authorized reachability
from the same query snapshot; they do not change the asset's owner.

Extend search/detail output with optional document provenance, omitted for legacy
results: A, publication generation, D/Ds/X, L/C/chunk, optional S/pipeline,
canonical range, payload hash, structural context and full S1 anchor. Real memory
IDs keep `memory_get`, inspect/trace/history meaningful. Add no new MCP tool and
do not change default input definitions. Old legacy output stays byte-compatible
where its current tests require it; new document output/citation presentation is
an explicit compatibility gate, not claimed by existing `SearchHit` fields.

The read-session resolver verifies the selected canonical hash and byte-exact
quote against mapping/evidence/entry, batched per Ds, and checks the anchor resolves
to the selected extraction. Retain CRLF, combining characters, CJK, emoji and
bidi/zero-width bytes; escape display controls/HTML without modifying source.
Missing/corrupt canonical inputs return explicit status/error; never resolve a
newer revision. Optional original bytes may be absent, verified or corrupt
independently. A historical receipt or canonical quote proves no live-file or
original-byte authenticity.

Current conflict groups are subject-based, so several raw chunks under one heading
could falsely appear to be conflicting claims. Initial policy excludes mapped raw
document chunks from generic fact-conflict grouping, without changing legacy/manual
conflicts. Counts, direct detail and explicit history still expose their real
episodes. Document claim extraction/conflict classification is not implemented.

## 8. Query snapshots and compatible rollout

Introduce a private SQLite read-session façade used unconditionally by every
supported retrieval endpoint. It pins one connection and explicit `BEGIN DEFERRED`
read transaction; the first read establishes the WAL snapshot. Factor helpers
accepting that session/connection instead of calling Store methods recursively
while holding its mutex. No fallback to an unpinned implementation in a database
eligible for publication.

Within that snapshot: settings/profile, spaces/links and BFS, effective head and
timestamp-eligible cohort, vector eligibility/readiness, both candidate branches,
RRF, limits, counts, decoration and citation enrichment. Visibility/owner predicates
apply **before** ranking/limits. Direct active-count/conflict views and multi-read
memory detail also need coherent sessions. A query sees the complete old or new
generation; it does not promise the newest commit after its snapshot began.

Load authenticated model runtimes and compute query vectors before the short read
transaction where practical. Treat prior readiness/config reads as hints only.
Inside the snapshot, revalidate the exact query-model/profile/settings fingerprint;
if changed, discard that session and allow at most one bounded restart, then return
explicit concurrent-state/unavailable error. Cache only authenticated query runtimes,
not heads, coverage, owner eligibility or candidate membership. Model inference
must not run under a SQLite writer. Candidate/citation resource refusal is an
explicit error, not partial silent omission.

### Required old-reader fence

A schema version check on open cannot retrofit snapshot behavior into an idle
old MCP/Store connection. WAL `BEGIN EXCLUSIVE`, a successful checkpoint, missing
receipts or an empty new-reader registry does not prove that process has stopped.
Select an independently tested **critical-relation read-fence migration** before
publication, with effective cutover in the first authorized publication transaction
(or a separately accepted explicit compatibility rollout). The passive prerequisite
must not install it. Candidate mechanism: rename physical `memories` to a versioned name
such as `memory_episodes_pub_v1`, update supported SQL to that name, and provide
**no old-name compatibility alias**. Existing active-memory queries all traverse
`memories`; old idle/autocommit handles then fail on their next statement, including
cached prepared statements. An already-pinned old read transaction can finish its
consistent old schema/data snapshot; after ending it, new old-name SQL must fail.
Do not claim this kills the process or blocks arbitrary same-account raw SQL.

This is a proposed related compatibility migration, not an existing feature or
an authorization bit. Before selecting it for implementation, prove every old
active search/count/detail/branch path either remains wholly on its prior snapshot
or fails without returning mixed/partial active results. Verify actual old binaries,
not just toy SQL. An old path that can bypass the critical relation is a blocker.
New supported handles use the read-session protocol unconditionally and check the
installed schema/layout. A supported client implements both prepublication
`memories` and the versioned published layout. Resolve the physical relation and
protocol marker inside each read snapshot or serialized write transaction before
memory SQL; never use a stale process-wide layout cache. Already-open supported
connections must continue coherent reads and unrelated writes across cutover,
invalidating prepared statements where necessary. A bounded retry is allowed only
for proved noncommit/schema mismatch; UNKNOWN still requires durable recovery.
Future old opens also reject the newer schema. This is
an explicit database-wide **client upgrade** boundary, not an asset-local fence:
old unrelated memory operations may fail too. Supported clients must retain
unrelated writing; no global interruption of supported ordinary writers is allowed.

Some old methods bypass `memories`: assertion indexing, vector save/counts,
metadata and code/source operations. The rename does not stop every old API.
Separate storage guards must prevent late old vector writes changing protected
vectors or readiness, and preserve faithful shared FTS maintenance. A single
missing-table probe is neither a complete old-writer fence nor a complete reader
audit. No guard using a missing UDF may be called harmless merely because its
activation predicate appears false.

The rename must preserve all row values/IDs, SQLite FK references, triggers,
indexes and M6 logical `memories` history/availability domain. Update physical-table
inventory mapping without reinstalling the M6 baseline or relabeling historical
coverage. Prove this on populated schema 16 with enforcement and unknown-outcome
tests. An alias/view exposing old SQL would defeat the mechanism unless it had a
separately proved fail-closed protocol gate; no such alias is proposed.

Operator quiescence/restart remains good rollout practice, but **is not the chosen
proof**. A weaker maintenance-attestation alternative would depend on truthful
complete closure of all old handles, which a checkpoint, FD scan or receipt cannot
prove. It needs a separately accepted operational assumption; if unknown, deny
publication. Until the automatic read-fence migration and full path audit pass,
publication is blocked. No read fence or rollout occurs in this design task.

## 9. Temporal meaning and lifecycle boundaries

Use one effective lifecycle timestamp Tp for every old retirement/new creation in
a committed publication. Choose it inside the writer under a versioned clock
policy, strictly after relevant affected lifecycle timestamps and the preceding
managed transition. Unsafe future/backdated legacy values or clock regression
refuse; do not rewrite history or invent a timestamp. Bind the selection policy
in input intent and the winning actual Tp in the result/receipt.

Old eligibility is half-open before Tp; new eligibility begins at Tp. A replay
returns the original Tp/R. Distinct document chunks need no fabricated semantic
supersession. Strict monotonic boundaries avoid A→B→A overlap under clock rollback.
Tp is neither exact COMMIT time nor a physical-file event time.

For `.at(T)`, select episodes/cohorts eligible at T in the query's current database
snapshot. **Never filter historical F using today's document head.** Retain old
vectors/projection maps for those episodes and apply an explicit compatible current
query profile. Existing current-membership/link/index/corpus semantics remain;
there is no promise of original historical memberships, ranking or model settings.
Preserve frozen generation memberships to avoid accidental managed membership
loss, without pretending this implements complete world/event-time history.

M6 recorded R records when a generation/receipt became known. Do not add a query
parameter pretending R is T, and do not describe this as recorded-revision retrieval.
Recorded eligibility, memberships/configuration/index history and historical
ranking require another gate. Future/late recorded observations are not silently
backdated into a complete historical world.

## 10. Proposed data model, authorization and atomic commit

### Additive schema bundles

| Bundle | Minimum records and invariants |
| --- | --- |
| Next candidate 0017: passive plan only | Immutable plan/receipt binds A/F/P/D, expected cohort/source/alias evidence and policies. Proposed bindings are nonexclusive advisory data, not active reservations. No active fence/head/publication state, ordinary-table guard or read-relation rename. |
| Source-only refresh extension | Immutable receipt binding managed A/F, exact Fs and raw hash/size/observations. Exact private source-only permit; ordinary knowledge creation forbidden. Reuse S1/S2 derivation without modifying P. |
| Read-protocol compatibility extension | Versioned physical active-memory relation with no old-name alias; supported SQL/read sessions and physical/logical M6 mapping updated together at the authorized compatibility/publication boundary. No passive activation; actual old-handle behavior must be proved. |
| Publication extension, only with reader/write gates | Effective permanent asset/locator fence installed only with complete visible replacement; immutable publication manifest and ordered entry→real evidence/assertion mapping, vector-origin mapping, generation/cohort membership, append-only transition/receipt, and CAS effective head. Unique current generation per A; exact deferred same-operation/result/R bindings. Initially only complete DocumentPublished is representable. |
| Later lifecycle extension | Typed restore/deactivation transitions, fresh episodes, retained evidence and explicit withdrawal semantics installed with their APIs/read tests. No reservation-release operation. |

Do not preassign later migration numbers or overwrite 0013–0016. Every metadata
cutover uses the existing serialized runner and one R_meta, validates populated
FKs/trigger inventory and preserves baseline/history. No automatic fence, enrollment,
publication, assertion/vector backfill or changed visibility during schema upgrade.
Known failure rolls back the cutover; unknown outcome resolves registration/marker
on healthy reopen. Older binaries reject newer databases; no destructive down migration.

SQL must enforce typed/non-null identity, fixed action/state/policy versions,
unique asset/head/nonce/manifest membership, positive monotone generation/R,
complete projection/vector/cohort counts, immutable successful records and exact
private permits. Compare row values before allowing inserts. Head update, all
episode transitions and same-R receipt must be tied by deferred relations and
inside-writer completeness checks. Ordinary M6/metadata/S3b/S3c/S3d permits cannot
arm publication. Guard primary and alternate-key replacement and old/new lineage;
do not rely only on Rust conditions, row counts or a UI hiding unsupported states.

### Proposed finite APIs

`preview_fence_plan` / `prepare_fence_plan` / `inspect_fence_plan`,
`record_managed_source_snapshot` (postpublication),
`preview_document_publication` / `publish_document_generation`,
`inspect_document_publication`, and private `query_snapshot`/connection-based
helpers. These names describe use cases, not a generic scheduler or index framework.
Use the existing direct local administrative CLI/context and existing initialized
database opener. No MCP publication opcode, auto-ingestion hook or public arbitrary
publication permit. A read-session API change is a prerequisite, not an API the
current public Store trait already supplies.

Canonical immutable input intent binds physical DB path/device/inode, kernel UID,
nonce/domain/action, A/F/D/P, fence/reservation/read-protocol versions and digests,
exact L/receipt/R/result and Fs/Ds/X/C/plan, optional S/contract/result, expected
head/generation and global pre-R, full retirement cohort/families/spaces fingerprint,
target spaces and all projector/index/query/clock/resource/citation policies.
Preview prints affected rows, semantic capability, directory-fence consequences
and membership/availability changes. Paths/metadata/possession of L/S never grant
publication. Use bounded canonical/decoding encodings and changed-intent conflicts
as in S3b–S3d.2. Preview is not a durable grant.

### Atomic operation

1. Validate Model A context/DB; parse full ID/confirmation and check a matching
   immutable receipt **before** current state/model inspection. Replay historical
   outcome with current integrity explicitly not checked.
2. Use a bounded serialized admission transaction: receipt first again, then
   nonce/complete input-ID binding and canonical current-head inspection using
   pure stored descriptors. An identical current manifest/cohort/index mapping
   may return verified read-only AlreadyPublished **before any Q1 feature/file/
   runtime preflight**, with no receipt/R or unchecked availability claim.
   Otherwise validate the expected fresh state/base/R, roll back the unused
   revision and release all database guards. This is not a reservation or durable
   approval; another publisher may win during subsequent computation.
3. Validate/build a bounded candidate outside the writer: canonical L/entries,
   citations, projection manifest, required S/vector evidence, proposed cohort and
   spaces. For semantic-required creation, perform the Q1 query-runtime preflight
   above. Missing model files do not invalidate historical S but refuse fresh
   semantic-required publication. No document embedding or assertion inference.
4. Enter final `BEGIN IMMEDIATE`; receipt first again; reject changed nonce/intent;
   repeat the verified identical-head read-only check before stale-state rejection,
   without a new receipt/R or current availability claim.
   Otherwise revalidate expected R/head, passive plan or existing fence/bindings,
   compatibility protocol, source/canonical/required
   indexes, exact current retirement set and membership. Detect changes even if
   a corrupt fixture failed to advance R. No automatic rebase/latest selection.
5. Choose valid Tp; allocate/reuse immutable projection evidence/assertions and
   insert ordinary FTS canonical rows. Copy exact approved vectors under the
   selected strict key and vector-origin map. Perform no model installation,
   runtime loading or external index build here.
6. Retire all old active cohort episodes, create the complete D episode/space set,
   verify actual ordinary FTS postings/canonical rows and required vectors, record
   generation/transition, activate the permanent fence and bindings if this is the
   first publication, perform required old-reader cutover, CAS effective head,
   insert successful receipt and M6 availability/history. Check FKs and the
   no-competing/no-empty cohort invariant, including fence→complete visible head.
   Newly active rows and head remain uncommitted together throughout.
7. Revalidate local context and commit using existing outcome classification.
   On known noncommit roll back every ordinary/staging-control effect. On ambiguous
   COMMIT discard the handle and report UNKNOWN. Output loss after a committed
   effect is UNKNOWN, not rollback.

If Q1/candidate work fails after the request was fully bound, make one bounded
serialized receipt observation before returning failure: a matching concurrent
winner may already resolve the operation. Otherwise distinguish no COMMIT by
this publication attempt from operation NotObserved or UNKNOWN/unobservable.
Do not convert a malformed/changed-intent request into unrelated success, and
do not treat receipt absence as proof that another admitted publisher stopped.

Mandatory lexical verification refers to the **actual ordinary index being
published**, not trusting L's old cache flag. Malformed/missing/incomplete canonical
L entries, citations or newly built ordinary postings refuse. Historical private
FTS cache damage/SQLite incompatibility is reported separately; it is never queried
or copied, so it need not block a canonical rebuild. No false current-private-cache
verification is implied. Final active FTS verification must include real engine
integrity plus canonical row/text equality, not just counts. Existing global
integrity cost must be measured/admitted; it is not a constant per-candidate check.

Only COMMIT of all these SQLite rows makes publication committed intent. No
independent vector daemon/file-index pointer/cache update is part of initial
visibility. Query runtimes are immutable assets, eligibility is read fresh in the
same DB snapshot. If a later implementation adds an external index, DB COMMIT
alone no longer proves visibility and this protocol must be redesigned.

Receipt absence is NotObserved, never proof of process death/rollback. All new
attempts serialize before deciding. Busy/unobservable earlier commits remain
UNKNOWN. Exact retries return the winner without another R, episode or generation;
different competing intents conflict. After a committed publication, rollback is
not undoing COMMIT: a later explicitly authorized compensating transition is needed.
Cancellation is best effort before COMMIT; there is no external revocation lease.

## 11. Alternatives and implementation slices

| Alternative | Decision |
| --- | --- |
| Ordinary spine, physical old-cohort retirement and atomic new projection | Selected: real IDs/details/history/memberships and one SQLite atomicity domain. Requires real storage fences, read sessions and strict vector routing, not a bare reconcile call. |
| Leave F active, suppress by a head predicate | Reject initially: every search/count/detail/index path needs perfect suppression; current conflict/count views leak, and post-limit filtering creates gaps. |
| Query private staged FTS/vectors directly | Reject: hidden staging changes corpus statistics; lacks ordinary memory IDs/membership/episode semantics and current readiness compatibility. |
| Separate document endpoint only | Cannot transfer ownership safely while unchanged ordinary endpoints expose F or omit D. Federated tagged retrieval plus a second lifecycle would be larger. |
| Merge F and D identities / rewrite legacy producer | Reject: immutable identities/provenance and source pairing do not establish equivalence; broad legacy migration is not this gate. |
| Active-only FTS corpus | Potential later requirement, not selected. Changes baseline ranking/rebuild/history policy and requires another complete index-membership design. |
| Global physical-file discovery / temporary legacy unfencing | Reject: unsupported identity/auth claims or ABA resurrection. Permanent negative reservations and explicit source-only refresh are smaller and safer. |

Implementation must be divided into independently testable gates:

1. **E1 passive plan/inspection only:** candidate 0017 immutable plan/receipts and
   scoped classification previews. No active state, negative reservation, ordinary
   SQL guard, read-relation rename or write denial. Ordinary ingestion can stale a
   plan; exact retries recover its historical receipt without new R.
2. **E1b managed source-only refresh:** needed for postpublication Fs updates,
   without knowledge reconciliation or temporary unfencing. Implement/test only
   as a bounded prerequisite, with no new active authority before publication.
3. **E2 coherent sessions/output/index routing and compatibility proof:** implement
   and test supported snapshot queries, protected vector routing, optional citation
   projection and real old-client migration behavior. Do not activate the physical
   read fence in the passive slice. Effective old-reader cutover requires E4 or a
   separately accepted explicit compatibility rollout; truthful quiescence alone
   is not proof under the selected automatically enforced protocol.
4. **E3 strict query/profile readiness:** required before semantic publication;
   real-model prefix/token/model-compatibility and mixed-key tests. Lexical-only
   first publication is limited to assets with no semantic coverage withdrawn.
5. **E4 atomic publisher/projector and permanent fencing:** install active
   records/guards and exact publication-private action together; complete fence,
   retirement, projection/ordinary-index/head/receipt transaction. No independently
   activatable half-publisher, fence-only state or visibility-only release.
6. **E5 managed updates/reversion/restore/deactivation:** updates/reversion preserve
   complete replacement and permanent restrictions. Restore/deactivation need a
   separately reviewed withdrawal/availability policy; initially refuse.
7. **Recorded-revision retrieval:** another design/implementation gate, not S4
   started here and not inferred from storing publication R.

None of E1–E5 is implemented by accepting this document. Publication stays blocked
until every applicable write/read/index/authorization prerequisite passes together.

## 12. Resources and measured feasibility

Proposed conservative first publication profile, subject to measured implementation
review: one asset; canonical text ≤1 MiB; ≤256 queryable chunks; ≤1024 retiring
episodes; ≤64 uniform target spaces; ≤4 MiB manifest/context charge; ≤64 KiB intent
and decoding form. Required vectors ≤786,432 bytes (256×3072). Above-bound whole
operations refuse; no skipped non-whitespace chunks or partial publication.
This is narrower than S3d.2's 16 MiB/8192-entry staging limits, not a storage downgrade.

For a first implementation using global ordinary FTS integrity verification,
also admit only ordinary subject+content corpus ≤64 MiB / ≤65,536 assertions
under the writer. This is a publication admission bound, **not** a page limit or
restriction on existing unrelated ingestion. Larger databases need a separately
measured verification profile or bounded equivalent verifier; do not replace the
check with row counts. Capacity/profile policy is versioned in the intent.

One read session adds a transaction and keyed projection/head joins. Lexical
eligibility must remain before LIMIT; vector search still evaluates all eligible
candidates before ranking. Batch citation validation by Ds; propose a 200-result
managed-query profile and 16 MiB distinct canonical-validation budget, with explicit
resource errors above it, never silent clipping. Existing unmanaged query behavior
is retained. Long read snapshots can retain WAL pages and delay checkpoints;
release before rendering/output, measure and test under writer contention.

There is no inference in a publication transaction. Model load/query-vector work
runs outside writers. Staging still reuses one model per job; concurrent processes
can multiply RAM. The previous 2.05 GiB sampled two-job RSS is not a guaranteed
upper bound, six-worker bound or OS admission mechanism. No global compute lock,
hard RSS/preemption, scheduler or generic performance rewrite is proposed here.

### Executed evidence in this design task

1. Existing locked release suites: lifecycle 16, M6 temporal 19, retrieval 29,
   indexing 22, persistence atomicity 5, Git persistence atomicity 9, lexical admin
   26 and semantic admin 24: **150 passed, 0 failed, 4 ignored**. These are current
   regression tests, not executions of proposed publication APIs. Process tests
   ran under the actual OS-account context; fixture signing was disabled only
   per command. Log: `/tmp/m8-s3e-existing-targeted.log`.
2. Actual unchanged production CLI, disposable schema-16 DB: one active F memory
   was retired, then unchanged file ingestion created a second episode, active,
   with the old episode still retired. This demonstrates why retirement is no
   fence. No publication was performed. `/tmp/m8-s3e-legacy-resurrection.json`.
3. Abstract two-connection WAL experiment on **Python SQLite 3.54.1**, not the
   bundled production engine: independent reads returned F then D across one
   atomic update; an explicit pinned read transaction returned F then F, and D
   after ending it. This illustrates the read contract, not an implemented Store
   session or complete publication test.
4. Isolated FTS5 experiment on that same Python engine: one selected MATCH hit's
   BM25 changed from `-0.000001` to `-7.394011499749896` after inserting 128
   unpublished/nonmatching corpus rows, even with a selected-artifact filter.
   It demonstrates statistics leakage, not a mutation of Mindspace's active FTS.
5. Abstract critical-relation rename on that Python engine: an already-open
   autocommit reader's cached `SELECT ... FROM memories` failed `no such table:
   memories`; an old explicit read transaction continued seeing F until COMMIT,
   then failed on its next old-name read. A new-name reader saw D. This supports
   the mechanism only, not real schema-16 migration, FK/M6 correctness or every
   old application path. `/tmp/m8-s3e-read-fence-probe.json`.
6. Existing production CLI persistence proxies, each on a fresh schema-16 DB:

| Markdown sections / ordinary assertions | Input bytes | CLI ingest ms | DB growth bytes |
| --- | ---: | ---: | ---: |
| 16 | 12,123 | 15.22 | 69,632 |
| 128 | 96,935 | 53.68 | 700,416 |
| 1024 | 776,127 | 533.83 | 5,427,200 |

The proxies include startup/extraction/ordinary transaction and no vectors,
publication manifests/head/fence/citation map or publisher verification. They are
**not publication transaction duration**, throughput guarantees or lock-budget
measurements. Snapshot/FTS/proxy details: `/tmp/m8-s3e-design-probes.json`.
No actual publisher exists, so publication lock duration, mixed-profile query
cost and large-corpus verification remain unmeasured implementation gates.

## 13. Executable adversarial acceptance matrix

All cases below are **proposed, unimplemented**. Use populated real schema-16
fixtures with legacy episodes/vectors/spaces, A/P and several L/S versions, then
the candidate migrations. Compare complete old/new rows, ranking and citations;
error strings or mocked head toggles alone are insufficient.

| ID / slice | Executable setup and required oracle |
| --- | --- |
| PLAN1 / E1 | Explicit local plan vs wrong context/DB/account/F/D/P/action/state/nonce/intent. Only authorized passive metadata commits. Exact receipt-first retry, changed-intent conflict, concurrent plans and UNKNOWN recovery; no authority or visibility granted. |
| PLAN2 / E1 | After passive plan, every existing CLI/MCP/library ingestion/lifecycle/index/maintenance writer and search retains baseline behavior, including unrelated and same-asset updates. Plan becomes stale, never blocks. No ordinary-table UDF guard or physical rename. |
| FENCE1 / E4 | Only authorized complete publication activates permanent restrictions. Missing replacement/index/head/receipt, prepublication arm or unsupported release cannot commit. Failed transfer leaves legacy writable and visible. |
| FENCE2 / E4 | Every CLI/MCP/direct observation, lifecycle/membership, late vector save/clear and SQL old/new-lineage substitution. FK/recursive toggles, REPLACE/upsert and metadata owner flags cannot bypass. Supported unrelated ordinary writes still succeed. |
| FENCE3 / E4 | Same locator new F after absence, admitted aliases/hardlinks, opaque observations, moves and full-root disappearance domain. Refuse entire intersecting atomic batch, no session/R/scope/absence partial effects; disjoint batches remain writable. Unknown physical closure refuses affected publication, not unrelated ingestion. |
| FENCE4 / E4 | Jobs extracted before publication, paused before/inside BEGIN, queued jobs, broker lease and retries after UNKNOWN. Prior committed writers stale the publication plan; publication winner makes delayed affected writes refuse. New receipt never proves old legacy UNKNOWN rolled back. |
| FENCE5 / E4+E5 | Initial release/deactivate/restore unavailable. Any later compensation must retain stale-writer protection and satisfy its reviewed replacement/withdrawal policy; never reopen an old ordinary job. |
| SRC1 / E1b | Append/reuse Fs under explicit source-only action, derive new Ds/X/C/L/S. No ordinary evidence/assertion/memory/vector or visibility change; wrong bytes/hash/association/expected head refuses; never temporarily unfence. |
| READ1 / E2 | Two/three SQL connections and a real process publisher pause between lexical/vector/scope/decorate calls. One read session returns wholly old or wholly new eligible cohort, including scores/space attribution/citations; no F+D mix. |
| READ2 / E2 | Actual schema-16 binary/process and cached statements opened before the read-fence migration, plus old explicit read transactions. Old active paths fail closed or finish their consistent pinned old snapshot; new-name paths see the complete current cohort. Prove FKs/triggers/M6 domain/data preservation and no alias bypass. A counterexample blocks publication. Process termination is never inferred. Two already-open supported connections must continue coherent reads and unrelated writes across cutover using same-snapshot/transaction layout resolution; never blindly retry UNKNOWN. |
| BASE1 / E2 | Legacy-only unmanaged DB and managed-but-unpublished DB: exact current lexical/semantic/hybrid/auto/as-of results/scores, counts/conflicts, MCP definitions and Git/code behavior remain unchanged. |
| AUTH1 / E4 | Valid fully staged candidate without matching publication admin action, altered full intent, changed membership/head/profile/source, arbitrary metadata or reused nonce. No ordinary/projection/head/R effects. Exact permitted case succeeds. |
| PUB1 / E4 | Legacy retrieval before P/L/S, after each, then authorized publish: first phases byte-identical visibility/ranking; publication has exactly one complete D active cohort and no active F cohort in every required mode/space/count. Nonempty eligible cohort survives the switch; absent/empty predecessor refuses in the initial protocol, even with explicit target spaces. |
| PUB2 / E4 | Concurrent publishers with same/different intents, stale plans and already-published identical manifest, then remove assets/use no-embeddings binary. One winner; exact replay returns original receipt/Tp/R; new-ID identical current artifact returns verified read-only status before Q1 preflight and no availability claim. Failure after admission can recover matching receipt once. Conflicting intents refuse without comparing recomputed floats. |
| PUB3 / E4 | Nth evidence/assertion/FTS/vector/memory/membership/head/receipt write failure and COMMIT veto; full rollback of new index/state/control rows and no retirement/visibility loss. |
| PUB4 / E4 | Kill before/during/after COMMIT, lose ACK/stdout, restart exact retry, live paused writer and lock timeout. Only known noncommit is called rollback; UNKNOWN resolves via durable receipt; absent receipt never proves process death. |
| IDX1 / E3+E4 | Incomplete/wrong L/P/Fs/Ds/X/C/anchor/coverage, missing vector, wrong S parent/model/prefix/config/count/dimension/norm/hash or selected origin. Refuse with old head intact. Count equality alone is not verification. |
| IDX2 / E4 | Corrupt/missing required ordinary FTS posting after proposed insertion. Commit refuses. Damage to unused private staged FTS is separate; canonical L must still verify and fresh active postings must pass. Hidden staging never changes ordinary rows/stats. |
| IDX3 / E2–E4 | Prepublication index candidate resumes after transfer; generic indexer encounters D; clear touches protected vectors. Storage rejects entire mutation. Exclusions avoid candidate starvation; no subject/context augmentation or legacy-key relabeling. |
| SEM1 / E3+E4 | Real strict-query assets and compatible legacy/doc groups, 2048/2049 query tokens, cold/warm runtimes, model/tokenizer replacement and unknown keys. Exact compatibility/overflow policy; no truncated doc query or cross-model vector reuse. |
| SEM2 / E3+E4 | Lexical-only attempt over any F vectors refuses. D semantic-required→D lexical-only update/reversion also refuses, including disabled/missing-model cases. Missing mandatory S/Q1 preflight refuses fresh publication; later missing feature/weights/profile yields explicit Semantic/Hybrid unavailable, disclosed whole-query Auto lexical fallback, never old F or silently incomplete doc subset. |
| RANK1 / E4 | Unrelated staged L/S additions do not affect active scores. Actual publish may change baseline all-assertions BM25; test eligibility before limits and deterministic tie/RRF order. Do not assert pre/post-publication rank equality. |
| LIFE1 / E5 | New revision, identical text/new X or L, old L/S reversion, update failure and delete/move. Old stays until successful complete switch; fresh episodes on reversion, exact old/new citations, fence retained. Initial unsupported actions refuse safely. |
| TIME1 / E4–E5 | T before/at/after Tp, clock rollback/future legacy episodes, A→B→A and fixed query T across retries. Half-open cohorts, strict ordering/refusal; no current-owner filter on old history. Current memberships/index/corpus limits disclosed. |
| TIME2 / separate | R supplied where T expected or unsupported exact history/ranking request. Explicitly unsupported; recording publication R is not an R-search implementation. |
| CITE1 / E2–E5 | Unicode/CRLF/combining/bidi/zero-width, metadata-only subject match, missing original bytes, source replacement, invalid canonical hash/UTF-8 range. Exact bound D/Ds/X/L/C quote or explicit unavailable; no fabricated context/newer revision/authenticity. |
| VIEW1 / E2–E4 | Real memory_get/inspect/trace/history/ID lookup, active counts/conflicts and multiple spaces/links. Real UUID provenance; historical records remain reachable/labeled, raw chunks do not become false fact conflicts. |
| MIG1 / E1–E4 | Populated schema-16 upgrade preserves all old rows/FTS/vectors/M6 baseline and query outputs; empty metadata only. Late DDL/FK/marker failure, veto/lost ACK, concurrent migrators, missing guards and older binaries fail/resolve honestly. |
| BUILD1 / every slice | Ordinary and no-embeddings binaries; no test hooks, unchanged default MCP input definitions. No new publication tool. No-feature lexical-only eligibility and historical inspection behave explicitly. |
| PERF1 / E2–E4 | At/below/above candidate/corpus/query quotas, writer contention and long read snapshots. No silent subset/omission or unbounded model work in writer; record actual lock/RSS/CPU/WAL observations before enabling profile. |

Meaningful independent-source/target mutations should omit each source/locator
fence, relax the stale writer check, release the fence on restore, filter after
LIMIT, split query sessions, query staged FTS directly, relabel a vector, omit
one required row/receipt/retirement or swallow COMMIT ambiguity. Clean baseline
must pass; each compiled mutant must reach its intended invariant oracle.

## 14. Gate disposition and remaining blockers

**Design direction: ACCEPT, conditional on the explicit logical-asset, Model A,
baseline-corpus and compatible-reader assumptions above. Publication: BLOCKED
until implementation and joint acceptance of the applicable prerequisites.**
This does not classify existing staging as partial or imply any publication exists.

Required blockers to clear before any successful publication: complete asset-scoped
receiver/SQL fencing activated only with visible replacement and classified
observation coverage (including generic library APIs),
known alias/collision resolution, source-only refresh for later revisions, coherent
read sessions and the proved old-reader relation fence, real projection/citation maps and cohort
membership consent, protected vector maintenance/backfill, strict query compatibility
where semantic coverage is required, and one atomic verified index/head/lifecycle/
receipt transaction. Missing or ambiguous proof refuses publication while retaining
the old selected cohort. Larger quota profiles, automatic release/alias reassignment,
active-only ranking and recorded-revision retrieval need separate review.

Observed current tests/probes establish the gaps and feasibility mechanisms, not
these proposed guarantees. Model A trusts the local account/process/kernel/SQLite
and registry; SHA binding assumes collision resistance. Unknown physical aliases,
unsupported producers and uncontrolled old readers must not be advertised as
covered. No automatic safety follows merely from accepting this design document.

S3e.1 reconciliation supersedes the earlier independent-arm/global-admission
proposal. It removes `LegacyFrozen` before publication, any database-wide opaque
observation refusal and any standalone active-fence API. Passive plans remain
non-authoritative. Old-client upgrade rejection is distinct from supported-client
asset-local availability. The previous independent design review predates these
corrections. Fresh complete read-only Sol review accepted the corrected design
after adding transaction-bound dual-layout resolution for supported open handles.
The new S3e.1 run also passed `cargo fmt --check`, 33 local links and existing
locked release regression suites: **150 passed, 0 failed, 4 ignored**. These are
existing tests, not executions of the proposed publisher or fences.

Evidence above was executed during S3e and is not a new S3e.1 run. S3e.1 final
review/verification is recorded in its separate writer-fencing design and final
report. No actual publisher, proposed migration or publication acceptance test is
claimed as executed here.
