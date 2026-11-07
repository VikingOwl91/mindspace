# Architecture decision log

Short, dated records of choices that shape the v0 foundation. Entries state
the decision, the reasoning, and what would force a revisit.

## ADR-001: Synchronous SQLite via rusqlite; no async runtime (2026-09-08)

**Decision.** Use `rusqlite` (bundled SQLite) and plain synchronous code. Do
not adopt `sqlx`/`tokio` in v0.

**Why.** v0 is a single-binary local CLI. Async adds a runtime, trait-object
hurdles, and dependency weight without a single benefit here; the task
guidance says to add tokio only where actually needed. The storage boundary is
a trait, so a future `mind serve` can add an async adapter or a PostgreSQL
backend without touching the core. The README's mention of Tokio describes
the eventual server stack, not this slice.

**Revisit when.** A network server lands and concurrency requirements are
real.

## ADR-002: Bundled SQLite with FTS5 (2026-09-08)

**Decision.** Compile SQLite statically via the `bundled` feature and rely on
FTS5 for lexical search.

**Why.** FTS5 availability depends on build flags of the system library; the
bundled build guarantees it everywhere and pins behavior. Local-first means
zero external setup.

**Revisit when.** Never, unless a backend without bundled builds appears.

## ADR-003: Hand-rolled migrations via schema_migrations (2026-09-08)

**Decision.** Embedded numbered SQL files, applied in order inside a
transaction, tracked in `schema_migrations`, run on every open.

**Why.** Transparent, dependency-free, and sufficient. Application on open
keeps every command working after an upgrade without a separate migrate step.

**Revisit when.** Migration count or review requirements grow.

## ADR-004: Assertion-Memory split with immutable assertions; supersession creates a new memory (2026-09-08, amended by ADR-012)

**Decision.** Assertions are immutable statements derived from evidence;
memories carry the lifecycle and supersede each other instead of mutating.
Superseding a memory creates a new memory and sets `old.superseded_by = new`.
History is the chain of memories, not a mutable content slot.

**Why.** It keeps assertions immutable all the way through, makes "what was
considered true at T" a pure timestamp query, and avoids mutable
"active assertion" pointers.

**Amendment.** ADR-012 revised the cardinality and identity rules; see there.

**Revisit when.** Aggregation or AI-derived multi-claim memories arrive.

## ADR-012: Assertion identity is the interpretation; Assertion->Memory is 1:N (2026-09-08)

**Decision.** Assertions carry a natural key
`UNIQUE (evidence_id, extraction, subject_normalized, content_hash)` and a
`created_at` (extraction transaction time). Memories are belief episodes
referencing an assertion many-to-one, with a partial unique index enforcing
at most one active episode per assertion. Ingestion looks assertions up by
natural key and inserts only when the interpretation is genuinely new; the
FTS row is written exactly once per assertion.

**Why.** The v0 rule "every new memory gets a fresh assertion" manufactured
rows that differed from existing ones by nothing but a uuid (retire then
re-ingest, or a file reverting to earlier bytes). In an append-only,
inspectable store that is semantic garbage: the interpretation did not
change, only the belief episode did. With 1:N, a flip-flop (iced -> slint ->
iced) reuses the interpretation and keeps one immutable object per meaning,
while each episode keeps its own created_at, so "believed T1..T2 and again
from T3" is directly representable. Corroboration stays distinguishable from
duplication because different fragments (different evidence ids) still yield
different assertions.

**Consequences.** Search may return several episodes of one assertion when
history flags are widened; that is intended for legacy lexical and Semantic
results. M4 Hybrid and Auto fusion instead deduplicate each already-filtered
candidate pool by assertion identity, keeping its first ranked eligible memory
episode as the representative. Fusion ranks, scores, and candidate counts are
per assertion; the returned hit metadata describes the selected episode. The
pool limit is applied before deduplication. If the semantic branch returns no
eligible candidates, the reported lexical fallback preserves episode-level
results. Consolidation of several assertions into one memory remains an additive
future join and is not implied by this ADR.

**Revisit when.** Consolidation arrives, or assertion extraction becomes
nondeterministic per run.

## ADR-005: Reconciliation keyed on (source location, subject), lineage follows content (2026-09-08, amended by the v0 review)

**Decision.** Re-ingest of a changed file supersedes/reuses per-subject
memories derived from the same source location, per the exact algorithm in
`docs/memory-model.md` section 6.2, including retirement of subjects that
disappeared from the file. Cross-source same-subject content is never
auto-superseded; it surfaces as a conflict. Leftover old and new fragments
are additionally matched across subjects by identical content, so a renamed
or moved heading supersedes its predecessor instead of severing the lineage.

**Why.** Location+subject is the only strictly deterministic key available
without interpretation. Auto-superseding across sources would silently
resolve conflicts, which the model forbids. Content-equality matching is
equally deterministic: identical text under a new heading is the same
proposition with a new label, and "what superseded this?" must keep working
through renames, the most common real-world edit.

**Revisit when.** Explicit user-driven supersession commands arrive.

## ADR-006: FTS over immutable assertions, state filtering at query time (2026-09-08)

**Decision.** The FTS5 table indexes assertions once and is never updated.
Default search filters to active memories via joins; flags widen to
superseded/retired; `--at` replaces state filtering with a timeline predicate.

**Why.** No index maintenance on supersession, historical content remains
lexically findable, and search semantics stay explainable ("why was this
returned": rank, snippet, space path, state).

**Revisit when.** Index size becomes a real problem (v0 scale: personal
notes).

## ADR-007: Immutability enforced by database triggers (2026-09-08)

**Decision.** `BEFORE UPDATE` triggers abort updates on `sources`, `evidence`,
and `assertions`.

**Why.** The central invariant ("never rewrite the past") should survive bugs
and future code, not depend on discipline. Raw SQL cannot violate it either.

**Revisit when.** A legitimate rewrite needs a migration that consciously
drops the trigger.

## ADR-008: No confidence or authority in v0 (2026-09-08)

**Decision.** No confidence/authority columns or scores. Search rank is
exposed as a lexical relevance metric only.

**Why.** Deterministic extraction cannot grade trust; fake precision is worse
than none. The model leaves room (additive columns on assertions/sources)
without reinterpretation. Reasoning in `docs/memory-model.md` section 8.

**M4 retrieval amendment.** The additive mode-aware retrieval API exposes a
numeric value only with a mandatory score kind: lexical relevance, cosine
similarity, or reciprocal-rank-fusion score. These are retrieval signals, not
confidence or authority. They must not be compared across kinds. The existing
lexical `SearchHit` contract is unchanged.

**M5 interface amendment.** Explicit CLI and MCP mode requests serialize the
mode-aware outcome with stable lowercase mode and score-kind names, tagged
fallback reasons, and per-branch provenance. Omitting mode selection preserves
the legacy CLI array and MCP `{ "hits": [...] }` response. The new wire shape
does not change score meaning or the legacy `SearchHit` fields.

## ADR-009: UUIDv7 identities, epoch-microsecond timestamps (2026-09-08)

**Decision.** All object ids are UUIDv7. Timestamps are UTC Unix epoch
microseconds (INTEGER) in SQLite, RFC 3339 strings at the CLI/JSON edge.

**Why.** UUIDv7 gives time-ordered, collision-free ids with no coordination.
Integer time is index-friendly and avoids SQLite datetime parsing pitfalls.

## ADR-010: Subject normalization is the matching key (2026-09-08)

**Decision.** `subject_normalized` (lowercase, whitespace-collapsed) keys
history, conflicts, and reconciliation; the display subject is preserved
verbatim. Space names stay case-sensitive.

**Why.** Heading text varies in casing/spacing across edits; matching on raw
text would fragment histories. Display fidelity is kept separately.

## ADR-011: Markdown via pulldown-cmark, no regex parsing (2026-09-08)

**Decision.** Section extraction uses `pulldown-cmark` byte ranges mapped to
line numbers. Plain text is ingested whole (subject = file stem).

**Why.** A real parser handles code fences, setext headings, and nesting
correctly; regex approximations would mis-split exactly where it matters.

**Revisit when.** Front matter or more formats arrive (deferred slice).

## ADR-013: Logical source identity and lifecycle via observations (2026-09-08)

**Decision.** Source identity splits into a mutable **logical source**
(`logical_sources`: stable uuid, presence state, last known location) and
immutable **content versions** (`sources`: one row per distinct content of
a logical source, `UNIQUE (logical_id, content_hash)`). Ingest operations
are sessions over a declared root; directory scans are full observations
that classify every path (unchanged / changed / created / continued) and
journal lifecycle changes (`moved`, `disappeared`) in `source_events`.
Single-file ingests are partial observations: path-local continuity only.

**Why.** With identity tied to `(kind, location, content_hash)`, a rename
manufactured a second logical source, duplicated active memories, and
invented candidate conflicts - normal filesystem operations became fake
knowledge changes. Identical content is deliberately NOT treated as
identity: a move is inferred only when exactly one candidate logical source
with that content is unaccounted for and exactly one new path with that
content appears; copies, multi-way matches, and rename-plus-edit stay
separate (false merging is worse than failing to infer). Disappearance
records absence without retiring memories: a vanished file does not make
its claims false. Location never rewrites history: a version's location is
where that content was first observed; current location lives on the
logical source and its changes are journaled, so "where was this version
observed, where is the source now, when did it move" are all answerable
from immutable facts.

**Consequences.** The lifecycle baseline is derived from logical source
state (root-scoped), so interleaved partial ingests are handled correctly;
session/observation rows remain as raw inspectable history. Moves are
inferred only within one root; root renames look like new boundaries;
symlinks are skipped; no ignore rules. Git-aware ingest later supplies
stronger identity signals through the same adapter shape.

**Revisit when.** Directory ingest UX expands (ignore rules), Git adapters
arrive, or cross-root move inference is wanted.

## ADR-014: Git via the system binary, plumbing only (2026-09-08)

**Decision.** Git repository ingest reads the system `git` binary with
stable, machine-readable plumbing commands only: `for-each-ref --format`
(NUL-separated), `rev-parse`, `symbolic-ref -q`, `rev-list --objects
--no-object-names`, `cat-file --batch-check` / `--batch` (streaming object
payloads), `ls-tree -r -z`, and `merge-base --is-ancestor` (exit-code
semantics). No localized human output is ever parsed. `gix` (gitoxide) was
considered and deferred.

**Why.** The plumbing surface is stable across git versions, requires no new
Cargo dependency tree, and keeps the reader a thin, auditable adapter: every
command is read-only, so hooks, scripts, and the working tree are never
involved (repository contents are treated as untrusted input). Raw object
payload parsing (commit headers, tree entries, tag objects) is parsing of a
documented binary/text object format, not of human output.

**Consequences.** Git is a runtime requirement for git ingest (explicit
`GitUnavailable` error otherwise). Commands run per session, not per object
(`rev-list` once, `batch-check` once, one `--batch` for structural objects,
one for text blob contents, one `ls-tree` per ref tip), so a session is a
fixed number of subprocesses plus batched SQL. gix stays open as a future
swap behind the same reader shape if the subprocess becomes a bottleneck.

**Revisit when.** Very large repositories make the subprocess count or
output size a real problem, or a no-system-dependency build is required.

## ADR-015: Git history is a graph of immutable objects plus a transaction-time ref journal (2026-09-08)

**Decision.** Commits, trees, blobs, and annotated tag objects are stored as
immutable objects keyed `(repository, oid)`; refs (heads, remotes, tags,
HEAD) are mutable pointers observed over transaction time with a journal of
created / fast_forward / non_fast_forward / deleted events. Repository
identity is a Mindspace marker file in the git common dir, so moving a
repository keeps its identity while a fresh clone gets its own; remote URLs
are observations, never identity. Text blobs at ref tips are derived through
the existing evidence/assertion pipeline with `git:`-prefixed extractor ids;
the logical source of a derivation is scoped per `(repository, commit,
path)`, so git-derived memories never auto-supersede across commits.
Fast-forward is a proven ancestor check; everything else is recorded as
non_fast_forward without claiming rebase or force-push.

**Why.** Flattening git into files would destroy exactly what git knows
exactly (topology, ref movements, object identity); deriving beliefs per
branch timeline would invent continuity that ref names do not have (branch
names are not durable identity, ref deletion is not history deletion, ref
name reuse is an observation not a rebirth). Deriving only at tips keeps
"what do we believe" at the heads of development while the full graph stays
provenance-inspectable.

**Explicit non-identities.** Same blob != same logical source; same patch !=
same commit; patch-equivalent != identical; ref name != durable branch
identity; ref disappearance != history deletion; non-fast-forward !=
automatically rebase.

**Revisit when.** Per-branch belief timelines, cross-repository dedup, or
history-rewrite explanation (rebase detection) become real requirements.

## ADR-016: Deterministic code structure is versioned syntax provenance (2026-09-14)

**Decision.** v0.4 parses Rust (`.rs`) with Tree-sitter, using path extension
as deterministic language metadata. A derivation is unique for immutable
input plus `mind-code@1`, `tree-sitter-rust@0.24.2`, and its grammar identity.
Old derivations are immutable, so a future extractor or grammar version adds
a sibling derivation instead of rewriting history. Tree-sitter nodes stay in
`ingest/code`; core and storage use normalized Rust-independent records.

Inputs are content-addressed ordinary-file bytes, or repository-qualified Git
blobs. Git blob input is parsed once per repository/blob/version tuple while
`code_contexts` preserves every observed path and commit. The model stores
module, struct, enum, trait, type alias, function/method, constant, static,
field, and import observations where syntactically available. Entity identity
is derivation-local `syntax_path` (kind/name/start byte), never a claim about
cross-version symbol lineage. Ranges are half-open byte offsets and one-based
line/columns. Parent syntax paths preserve containment. Imports preserve raw
specifier text only; no import or type resolution is attempted.

**Why.** Syntax is structural provenance, not an interpretation or belief
episode. Creating a Memory for every node would corrupt recall and make source
code churn look like user-facing knowledge churn. Tree-sitter parses bytes as
data and recovers from malformed syntax without compiling or executing a
repository.

**Consequences.** A tree containing error nodes is stored as `recovered` with
an error count; unrelated inputs continue. Unsupported and non-text inputs are
not guessed or parsed. Same name, path, AST, or line number does not establish
durable symbol identity. Same blob does not erase its path identity. `mind
structure` is a narrow inspection surface; lexical Memory search is unchanged.

**Revisit when.** Add further grammars, parameter/signature normalization,
cross-file resolution, or conservative symbol lineage. Those are additive
work, not v0.4 semantics.

## ADR-017: Explicit manual memories use their own provenance and cannot rewrite documents (2026-10-22)

**Decision.** Manual writes use `SourceKind::Manual`, `LogicalKind::Manual`,
the stable source location `manual://writes`, and extraction rule
`manual:write`. They flow through the same immutable Source -> Evidence ->
Assertion -> Memory model. An update may supersede only an active manual
memory; the storage layer verifies source kind, logical kind, evidence kind,
and extraction marker. Document-derived updates are rejected with direction to
edit and re-ingest the document.

Exact duplicate subject/content writes reuse their assertion and active
episode, adding only missing spaces. Recreating a retired belief may open a new
episode on the existing assertion. An unchanged normalized subject and
content update is a no-op apart from requested space additions. If the
successor already exists as another active manual episode, it is reused to
avoid duplicate episodes and self-supersession.

**Why.** Agent-authored corrections must not masquerade as edits to the source
that originally established a document-derived memory. Manual provenance is
explicit and inspectable, while immutable rows preserve what was actually
written. Restricting updates in storage prevents non-MCP callers from bypassing
the trust boundary.

**Consequences.** A document must be corrected at its source and re-ingested.
If an unchanged document is re-ingested after retirement, its memory can be
recreated under existing ingestion semantics. Manual spaces must already
exist; an explicit manual write requires at least one space.

## ADR-018: Small synchronous MCP stdio adapter (2026-10-22)

**Decision.** Add `mind mcp` as a synchronous, newline-delimited JSON-RPC 2.0
stdio server implementing MCP `2025-06-18`. The server exposes eleven
memory/space tools over the existing `Store` and retrieval interfaces. It
returns serialized text plus `structuredContent` for successful tool calls
and omits output schemas until response shapes are fixed.

**Why.** Local coding clients need a standard tool interface, but this slice
does not need HTTP, authentication, an async runtime, or a heavyweight MCP
dependency. The small protocol adapter is testable against the compiled
binary and keeps persistence in the storage boundary.

**Consequences.** The process is local-only and has no authentication; users
must configure trusted clients and their tool approvals. Tool execution
failures use MCP error results; protocol errors such as invalid requests,
unknown methods, and unknown tool names use JSON-RPC errors. EOF is the
stdio shutdown signal. Network transport remains deferred.

## ADR-019: Immediate SQLite write transactions with bounded lock wait (2026-10-22)

**Decision.** Use WAL, a five-second busy timeout, and `BEGIN IMMEDIATE` for
write transactions. Map SQLite busy/locked failures to an actionable storage
error. Do not retry writes at the application layer.

**Why.** Immediate transactions acquire the writer reservation before
read-modify-write logic and keep multi-row writes atomic across independent
processes. A bounded SQLite wait provides a chance for short transactions to
finish without an unbounded stall. Blind replay can duplicate or partially
repeat side effects when commit or rollback outcome is uncertain, so callers
receive the error instead.

**Consequences.** Readers can continue during WAL writes, but only one writer
can hold the reservation. A writer that cannot acquire it within five seconds
fails explicitly; callers decide whether and when to make a new request.

## ADR-020: Session-owned verified retrieval runtime (2026-10-23)

**Decision.** The synchronous MCP session may retain one verified, loaded
semantic runtime. Every non-lexical request still rereads model selection,
enablement, index readiness and structural artifact checks. CLI invocations
and explicit library resolution remain uncached. First use always verifies
every artifact against the compiled-in SHA-256 manifest.

Verification reuse is limited to Linux/local btrfs. The complete manifest and
directory path identify a runtime. Device, inode, mode, size, nanosecond mtime
and ctime for artifacts and checked directories detect changes after SHA
verification. Artifact ctimes must be at least two seconds old before the
prehash snapshot, and identities must match after the successful query.
Recent artifacts require another full verification before becoming cacheable.
Unknown filesystem semantics, invalid/future timestamps and observable clock
rollback prevent reuse. No verification record persists across processes.

**Why.** Release measurements separate repeated verification (~291 ms) and
loading (~190 ms) from warm inference (~85 ms) and a 100-vector scan (~0.6 ms).
Keeping the existing per-request runtime wastes most of a warm MCP request.
Session ownership bounds memory and avoids a global registry or background
watcher. Full manifest equality also covers runtime inputs absent from the
derivation key, including the query prefix.

**Consequences.** Metadata is a change detector, not a substitute for the
initial digest check. Unavailability, changed artifacts and resolution/search
errors evict; Auto's embedding-error fallback also evicts. Post-search changes
discard semantic results without retrying semantic work. A verified runtime
owns its weights, so a later file write cannot mutate them. Freshness is
observed at request boundaries, not instantaneously. The existing nonhostile
single-user filesystem assumption remains; privileged ctime manipulation,
filesystem rollback and concealed clock changes are outside that boundary.
Other filesystems/platforms remain uncached. See
[retrieval-performance.md](retrieval-performance.md) for the full policy and
measurements.

**Revisit when.** Qualifying another filesystem, coordinating concurrent
writers, changing to mmap-backed weights, adding persistent verification state,
or changing the synchronous MCP execution model requires a new integrity
decision. None is part of this optimization.

## ADR-021: Temporal recording and history coverage (2026-10-23)

**Decision.** Recording revisions, observation timestamps, and world-validity
time are separate coordinates. `belief_at(R)` and `world_history(R, V)` have
different semantics; legacy `SearchQuery.at` remains unchanged. Revisioned
history starts at an atomic migration baseline, with explicit typed coverage
and fail-closed capability checks. Lifecycle ownership is separate from
assertion derivation identity and manual-write authorization. Database-local
write fencing uses persistent triggers and connection-local revision state,
within the non-hostile database-owner threat model. Derived indexes must be
checked against canonical data and are not historical truth.

**Consequences.** Pre-baseline recording history is unavailable and cannot be
inferred from timestamps. Unsupported temporal dimensions return an explicit
capability error. See [adr-m6-temporal-recording.md](adr-m6-temporal-recording.md)
for the Stage 0/1 contract and boundaries.
