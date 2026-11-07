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

**ADR-014 addendum (2026-11-07): the execution context is pinned (MIND-2).**
"Read-only plumbing" was an assumption. Testing it found that no
config-driven program execution is reachable from the commands Mindspace runs
(13 exec-capable config keys, hooks, includes, filters: none fired; `git
status` does fire them, so the test oracle is not vacuous). It also found two
defects the claim did not cover, both reproduced before fixing:

- An inherited `GIT_DIR` overrode the working directory, so ingesting repo A
  while `GIT_DIR` named repo B recorded B's code under A's identity.
- `refs/replace/*` made `git cat-file --batch` return bytes that do not hash to
  the recorded object id, breaking the evidence chain.

Decision: all git processes are created by one builder (`git_command`) that
removes every inherited `GIT_*` variable, ignores user and system config, sets
`GIT_NO_REPLACE_OBJECTS=1`, and disables hooks, fsmonitor, alternate-refs
commands, pagers, network protocols, optional locks and auto-gc.

**Consequences (behavior changes).**

- Inherited `GIT_*` variables are ignored on purpose. Anyone who relied on
  `GIT_DIR`, `GIT_WORK_TREE`, `GIT_OBJECT_DIRECTORY`, `GIT_CONFIG_*`,
  `GIT_SSH_COMMAND` or similar to steer Mindspace will no longer get that
  effect: the repository is always the one named on the command line. Running
  `mind` from inside a git hook or `git rebase -x` now works as expected
  because those variables no longer leak in.
- User and system Git configuration is not read. Settings such as
  `url.<base>.insteadOf` therefore do not rewrite remote URLs (remotes are
  observed as stored in the repository), and a global `safe.directory` entry
  no longer applies.
- **Not tested:** the `safe.directory` / foreign-owner case. Reasoned only:
  with global config ignored, a repository owned by another user should fail
  with Git's ownership error instead of being read. This was not run, because
  no second user was available; it is unknown whether every Git version
  reports it as an error that Mindspace surfaces cleanly.
- `refs/replace/*` is deliberately not honored: Mindspace records what hashes
  to each object id, never a substituted view.

**Residual risk (not mitigated, not demonstrated).** The repository's own
`.git/config` is still read (Git offers no way around that), and commit-graph,
multi-pack-index, `objects/info/alternates` and shallow files are honored as
written. A hostile repository could use them to misreport reachability or
supply objects from another store. No exploit was built; the risk is reasoned
from Git's documented behavior. Mitigation, if needed, belongs in a separate
decision (for example disabling the commit-graph at a measured cost).

The identity marker `.git/mindspace-repository-id` remains the only write
into a target repository. Regression tests: `tests/git_hardening.rs`.

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
input plus `mind-code@1` (now `@2`, see below), `tree-sitter-rust@0.24.2`, and its grammar identity.
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

## ADR-022: Outline-first targeted ingest, ignore-aware directory walk (2026-11-07)

**Decision.** `mind scan <path> [--depth N]` prints a read-only outline
(files, bytes, ingestable files, kinds per directory) of a git repository at
HEAD or of a directory, and opens no database. `mind ingest --path <subpath>`
(repeatable) interprets only the selected files. For a git repository the
object graph and refs are still observed completely; only derivation is
narrowed. For a plain directory each selected sub-path is its own ingest root,
so moves and disappearances stay scoped to it and unselected files are never
marked absent. Selections must be relative and cannot contain `..`.

Directory walks now use the `ignore` crate: project-local `.gitignore` and
`.ignore` apply (also outside a git work tree), `.git` is never entered, hidden
files are included, symlinks are skipped. Global, `info/exclude`, and parent
ignore files are deliberately off so the result depends only on the contents
of the root. This supersedes "no ignore rules" from ADR-013.

**Fix.** `ingest::git::run` fed all of stdin before reading stdout and
deadlocked once both pipe buffers filled (repositories above roughly 2,800
objects hung without error). Stdin is now written from a scoped thread while
stdout and stderr are drained.

**Not decided here.** Parallel parsing is a separate step.

## ADR-023: Git ingest derives the HEAD view by default; history is opt-in (2026-11-07)

**Decision.** The default scope interprets *current views*: HEAD of the ingest
path plus HEAD of every live linked worktree (`git worktree list --porcelain`;
bare and prunable entries skipped). `--branches`, `--remotes`, and `--tags`
add those ref classes as further views (annotated tags are peeled). Only
history reachable from a view is observed and only its files are interpreted.
All refs are still journaled completely, so the ref journal of ADR-015 is
unchanged. Memories of this repository derived at any commit that is not a
current view are retired in the same transaction, with the reason `derived at
<old>, which is no longer a current view (current: ...)`. Repository identity
is shared by all worktrees, so ingesting from any of them yields the same set
of views and no worktree retires another. `mind ingest --history`
(`GitScope::History`) restores ADR-015 behavior: every ref tip is interpreted,
derivations at different commits coexist, and nothing is retired.

**Why.** Per-commit logical sources never supersede (ADR-015), so after every
new commit the old and new versions of a document were both active, and every
branch added another copy. Search could not answer "what is true now". Retiring
is deterministic, reversible in the sense that nothing is deleted, and keeps the
stale memory inspectable.

**Consequences.** Retirement replaces supersession lineage for HEAD moves:
`mind history` shows a retired episode, not a link to its successor. Running a
head-scope ingest after a history-scope one retires the other tips' memories.
Narrowing `--path` on a later ingest does not keep older-commit memories of
unselected paths alive. The object graph reachable from HEAD is still stored
completely, so database size is dominated by commits and tree entries.
`GitObservationReport.stale_retired` counts the retirements.

**Revisit when.** Pairwise supersession across HEAD moves (path and subject
matching), a configurable default ref instead of HEAD, or a depth limit on the
stored graph are wanted.

## ADR-024: Releases are derived from tag refs (2026-11-07)

**Decision.** Every `refs/tags/*` ref of a git repository becomes one
document-shaped memory, "Release <name>": the tag message (for lightweight
tags the tagged commit's message), tagger or author, date, and the peeled
commit. Rule id `git:tag:release`; no markdown parsing, so changelog headings
in a tag message cannot create colliding subjects across releases. The
derivation uses the normal source -> evidence -> assertion -> memory pipeline
with the virtual path `/refs/tags/<name>` (a leading slash can never be a git
tree path). The stored version's commit is the peeled target commit and its
blob oid is the tag object (annotated) or the commit (lightweight), so a
moved tag is a new version. Releases have their own source kind, `git_tag`,
so source identity does not hang on a false `git_blob` type; the logical
source stays a `git_path` scoped to the virtual path. The tagged commit rows are
stored so provenance loads; their ancestors are not.

Releases are independent of current views: a tag on an old commit is still a
release. They are retired only when their tag is deleted or moved, in every
scope. Head-move retirement (ADR-023) never touches them. A targeted
`--path` ingest skips releases and leaves existing ones untouched. Retirement
happens only when the ingest took a complete tag inventory (an unscoped
ingest); a partial ingest never declares a release gone. A regression test pins
this.

**Not decided here.** Forge releases (GitHub/Forgejo notes and assets) live
outside git. An adapter would need network access and credentials and must be
optional and explicitly invoked.

## ADR-025: Code retrieval is a separate symbol search, not memories (2026-11-07)

*Superseded in part by ADR-028: the `mind_meta` view storage and the "no spaces" limit.*

**Decision.** The code-structure layer (ADR-016) becomes searchable through
its own surface: `mind symbols <query>` and the MCP tool `code_search`. It is
the "later retrieval slice" ADR-016 anticipated. Code definitions stay
immutable syntax facts; no Evidence, Assertion, or Memory is created for them,
so code churn is still not knowledge churn and ordinary recall is unchanged.

A query is whitespace-separated terms. Every term must occur, case-insensitively,
in an entity's name, qualified name, signature, or path. Results are ranked by
match quality (exact name, exact qualified name, name prefix, name substring,
qualified-name substring, signature substring, other) with imports and fields
sorted behind real definitions. The rank is a match class, not a score or
confidence. Filters: entity kind, repository (path substring or id prefix),
path prefix. Hits carry kind, names, signature, visibility, path, line range,
repository, commit, blob, and a `current` flag.

**Currency.** Immutable syntax facts cannot be retired, so currency is
recorded beside them. Each git ingest replaces `git_views:<repository id>` in
`mind_meta` with the commits it derived at (HEAD, worktrees, selected refs;
all tips under `--history`). A git definition is current when its context
commit is in that set. A file definition is current when its logical source is
present and the definition's content is the source's latest version. Stale
definitions are hidden by default and returned with `current=false` under
`--all-versions` / `all_versions`.

**Why `mind_meta`, not a table.** The M6 baseline and write fence are tied to
schema version 8. A new migration would have to rework that coupling, which is
outside this change. `mind_meta` is current-state only and carries no
historical claim, which matches what a view set is. Promote it to a table once
M6 settles.

**Limits.** No spaces: code is not yet attached to spaces, so scoping is by
repository and path. Rust only (the only grammar). Signatures are the first
line of a definition; doc comments and bodies are not indexed, and source text
is not stored, so hits are locations to read with the agent's own file access
or `git show <commit>:<path>`. Matching is substring scanning without an
index, which is fast at repository scale (16 ms on a 318-file Rust workspace)
but will need an FTS index for very large databases. No call or implementation
queries yet, although the relationship observations exist.

**Revisit when.** Spaces for code, more grammars, a symbol FTS index, or
relationship queries (callers, implementors) are wanted.

**ADR-025 addendum: `mind-code@2`.** Two extractor defects found while proving
code search on the `record_manual_write` case. Trait method declarations
without a body (`function_signature_item`) were not entities, so a trait's own
interface was invisible; they are now `method` entities (a bodiless `extern`
function is a `function`). And `impl Trait for Type` named its methods after the
trait (`Store::put`) instead of the implementing type (`SqliteStore::put`),
because the first type identifier was used instead of the `type` field. Both
change derivation output, so the extractor version moves to 2 and v2
derivations sit beside v1 ones; nothing is rewritten. Symbol search and the
relationship resolver read only the newest derivation of each input.

**ADR-025 addendum: `mind-code@3` (MIND-5).** `mind-code@2` decided
"method or function" by searching for the substring `impl` or `trait` in the
syntax path of the enclosing entity. That path contains the enclosing entity's
*name*, so a function nested in `fn impl_helper()`, `fn reimplement()`,
`mod impl_utils`, `mod traits`, `mod r#impl`, or in a method called
`trait_name`, was reported as a method. Reproduced end to end with the `@2`
binary: `inner_fn` and `in_traits_mod` came back as `method`.

Decision: an entity is a method exactly when its direct container is an `impl`
block or a `trait`, decided from the syntax tree (the ancestors, looking
through only the `declaration_list` body), never from names. Syntax paths,
qualified names and everything else are unchanged; only `kind` can differ.
The extractor version moves to 3.

**Upgrade path (verified).** Re-ingesting into a database that holds `@2`
derivations creates `@3` derivations beside them; symbol search shows only the
`@3` classification, also with `--all-versions` (that flag is about current
views, not extractor versions). Nothing is rewritten, consistent with the `@2`
addendum above.

**Not done: pruning old derivations.** The original plan was to delete `@1` and
`@2` derivations. Resolutions (`code_relationship_resolutions`,
`code_resolution_candidates`) reference observations and entities of the old
derivations by foreign key, so removal is a cascade across several tables that
destroys provenance, not a small cleanup. It needs its own decision (tracked
as a separate item); the old rows only cost space.

**What counts as a defect and what does not.** Pinned by
`tests/code_classification.rs` as *unsupported by design*, not as bugs: macro
bodies are not expanded (items a macro would generate are not entities), and
`cfg` attributes are not evaluated (every branch is extracted as written).
Malformed code has no ground truth beyond tree-sitter's recovered tree, and
the test pins the classification observed with `@2`. Looking through parser
`ERROR` nodes was considered and left out: removing it changed nothing across
600+ corrupted variants of the project's sources (3,000+ entities checked
against a name-independent oracle), so it would have been untested code. A
tree shape that needs it may exist; none was found.

**Verified:** 15 tests in `tests/code_classification.rs`, 5 of them failing
against `@2` (nested function in a free function named like `impl`, in
modules named like `impl`/`trait`, in a method named like `trait`, in a
closure, and the corpus oracle); every mutation of the rule is caught by at
least one test; the benchmark gate is unchanged (ingest within noise, DB size
identical).

## ADR-026: Agent-driven ingest behind a restrictive allowlist (2026-11-07)

**Decision.** The MCP server gains `repo_outline` (read-only) and `repo_ingest`
so an agent can look at a repository and ingest chosen parts of it. Because the
server has no authentication, both tools fail closed:

- Allowed roots are configured with `mind mcp --allow-root <DIR>` (repeatable)
  or `MINDSPACE_MCP_ALLOW_ROOTS` (`:`-separated). With none, which is the
  default, both tools refuse every call.
- Roots are canonicalized and validated at startup; a missing root or `/` stops
  the server.
- A requested path must be absolute and, after `canonicalize` (symlinks and
  `..` resolved), lie inside an allowed root. Paths with a `.git` component and
  plain files are refused. A refusal does not reveal whether a path exists.
- An unscoped `repo_ingest` of more than 2000 ingestable files is refused with
  a pointer to `repo_outline`; the call is synchronous and the agent should
  select directories. `repo_outline` returns at most 300 nodes (shallowest
  first) and a `truncated` flag. Ingest results are summarized, not dumped.

`repo_ingest` takes the same scope options as the CLI (`paths`, `branches`,
`remotes`, `tags`, `history`) and existing spaces; it never creates spaces.

**Consequences.** `repo_ingest` writes to the database and, for git, a
repository identity marker into the repository's git common dir. Allowing a
root therefore grants write access of that kind. Choose specific directories,
not a home directory. Synchronous ingest can exceed a client's tool timeout on
very large inputs; scoping is the mitigation. Git is run in allowed
repositories only, with the read-only plumbing of ADR-014.

**Revisit when.** Read-only versus ingest roots need to differ, ingest must run
asynchronously, or HTTP transport and real authentication arrive.

**ADR-026 addendum (2026-11-07): read-only and read-write roots (MIND-3).**
ADR-026 listed "read-only versus ingest roots need to differ" as a revisit
trigger. Until now every allowed root granted ingest, including the write of
the identity marker into the repository, so an agent that was only meant to
look at a directory could change it. (Verified by construction: there was no
way to express a read-only root.)

Decision: `--allow-root DIR[:ro|:rw]`. `repo_outline` accepts any root;
`repo_ingest` needs a read-write root.

- A root without a suffix is **read-write**, kept only for backward
  compatibility. This is *not* least privilege: the safer default would be
  read-only, and a forgotten suffix grants ingest and the identity-marker write.
  Always write the suffix explicitly, and use `:ro` unless ingest is intended.
  Changing the default to read-only would be a breaking change and needs its own
  decision.
- The most specific root governs a path (longest path wins). A read-write
  directory inside a read-only one is writable; a read-only directory inside a
  read-write one is not.
- The same directory listed twice (also through a symlink or trailing slash)
  keeps the more restrictive mode.
- Permission is decided on the canonical path, so a symlink in a read-write
  root cannot move a request into a read-only root.
- An ingest target (the path itself, or each selected `paths` entry) must be
  governed by a read-write root and must not *contain* a read-only root.
  Otherwise ingesting a parent would capture the read-only part. The agent is
  told which root is in the way and can select paths that avoid it.
- The refusal names the root and the missing right (`--allow-root DIR:rw`).
  A request outside every root is still indistinguishable from a missing path.

**Consequences (behavior changes).** `:` already separated roots in
`MINDSPACE_MCP_ALLOW_ROOTS`. A mode now belongs to the path before it *within
one value* (`/a:ro:/b:rw`); a lone `--allow-root ro` is an error, not a mode
for the previous argument, and a directory literally named `ro`/`rw` is written
`./ro`. In a value with several parts, paths must be absolute or start with
`./` or `../`, so a mistyped mode (`/a:rx`) fails at startup instead of
becoming a second, relative root. Modes are lowercase. Malformed specs stop
the server; they are never ignored. The `McpConfig` library API keeps
`McpConfig::new` (read-write) and gains `from_specs`.

**Verified** with `tests/mcp_allowlist_modes.rs` (11 tests) against the real
binary: read-only root refuses ingest and writes neither database rows nor the
identity marker (outline on the same repository also writes no marker);
nesting in both directions; duplicate roots in either order; symlink into a
read-only root; prefix collision (`repo` vs `repo-evil`), `..`, missing paths;
trailing slash; environment-variable form; nine malformed specs, each failing
for the spec and not for an unrelated reason. Removing any single rule turns
at least one test red, except the read-only check in `authorize`, which is
deliberately duplicated by `check_ingest_scope` (removing both turns seven
tests red).

**Not tested.** Case sensitivity on a case-insensitive filesystem: the
requested path is canonicalized, so a differently-cased spelling should resolve
to the real root, but the test only asserts the distinction on a case-sensitive
filesystem (it is skipped otherwise, and none was available). Selected `paths`
are checked lexically against the roots; this relies on directory ingest not
following symlinks (tested) and git ingest reading objects instead of the
working tree (design, ADR-014). Read-only does not mean "no process runs":
`repo_outline` still runs read-only git plumbing in the repository (ADR-014).

**Known limitation: time-of-check to time-of-use (not fixed here).**
`authorize` resolves a path once; the ingest then uses that resolved path for
its whole run (several git processes and file reads), so the window is the
duration of the ingest, not an instant. If a path component of the resolved
directory is replaced by a symlink after the check, the ingest follows it
outside the allowed roots. **Reproduced** with a script that renames the
authorized directory away and puts a symlink to an outside repository in its
place while `repo_ingest` runs repeatedly: outside content reached the database
in 5 of 5 runs, after 8 to 10 attempts each, and the identity marker was
written into the outside repository (a write outside every root).

Threat model: the attacker must be able to change directory entries inside an
allowed root while an ingest runs. That is a local process, another user on a
shared directory, or an agent that also has shell or filesystem tools; a
client that can only call the MCP tools cannot do it by itself. Impact:
anything the `mind` process can read as a git repository or directory tree can
be pulled into the database and then read back through search, which defeats
the allowlist; plus one marker file per repository written outside it. A
read-only root is not a defense against this, because the swap happens below
it. `repo_outline` has the same window for reads. Mitigation direction
(separate follow-up, MIND-18): open the authorized directory once
(`O_DIRECTORY|O_NOFOLLOW`), run git and file reads relative to that descriptor
(for example `git -C /proc/self/fd/N`), resolve with `RESOLVE_BENEATH`, and
verify afterwards that the descriptor still lies inside the governing root.
Whether `git -C /proc/self/fd/N` behaves correctly for all commands used here
is untested. Until this is fixed, do not allow roots that untrusted local
processes can write to.

**ADR-026 addendum (2026-11-07): ingest time limit (MIND-4).** `repo_ingest`
is synchronous, so a long ingest can outlive a client's tool timeout, and a
hung git (the original deadlock) stalled without any message.

Decision: `mind mcp --ingest-timeout <SECONDS>` (also
`MINDSPACE_MCP_INGEST_TIMEOUT`, default 30, accepted range 0.001 to 3600;
zero, negative, non-finite and out-of-range values stop the server at startup).
The default is below the 60 s that MCP clients commonly allow; the real
timeouts of specific clients were not verified. **The flag is a deadline for
the preparation of an ingest, not an end-to-end timeout.** It stops what can be
stopped cleanly and does not bound the total duration: the write phase is
allowed to exceed it.

- **Enforced:** every step of the preparation (listing refs and objects,
  reading contents, parsing, deriving), the git processes themselves (a git
  that is still running at the limit is killed), and a last check immediately
  before the first database write. Aborting there changes nothing in the
  database. The error names the phase and points to `paths`, `repo_outline`
  and the flag. For several selected paths of a plain directory, which are
  independent ingest roots, the finished ones are kept and the error says how
  many.
- **Not interrupted:** the database write and what follows it (code structure,
  relationship resolution). Because of that, a successful call can finish after
  the limit, and it reports three fields:
  - `elapsed_ms`: time from the start of the call (authorization and size check
    included) to its end;
  - `limit_ms`: the configured deadline;
  - `over_limit`: `true` exactly when the call finished after the deadline had
    passed. It describes timing only. It does **not** mean that anything failed,
    was rolled back, or was cut short: the ingest is complete. It is `false`
    when the call finished within the limit. A call that the deadline did stop
    is an error and carries none of these fields.
- **Not covered:** `repo_outline` (including its repository facts), walking a
  very large plain directory, and processes that git itself might start.
  Client cancellation (`notifications/cancelled`) is not handled; whether any
  client sends it was not checked.
- **Requests** are processed one after the other on one thread. A second
  request sent while the first runs is queued and answered in order (tested);
  its own limit starts when it starts.

**Why writes are not interrupted (measured, not assumed).** Interrupting a
fenced write transaction was tried on the real fence code in a throwaway
experiment: SQLite rolls the transaction back by itself, the transaction's
`Drop` then fails its own rollback, the store is marked poisoned, and the next
write of the session fails with "connection was discarded after an uncertain
transaction outcome". A timeout would therefore disable every later write of
the session. Changing the M6 fence is a separate, security-relevant decision.
Also, the write is followed by two more transactions (code structure,
resolution); stopping between them would leave memories without their code
layer.

**What this means in numbers (one repository, so treat it as an example).** A
first ingest of the `gpui-shell` fork (6,597 objects, 3.9 s): preparation
0.57 s, the main write 1.9 s, code structure and resolution 1.5 s, so about
85 % of the time is in the part the limit cannot stop. A second, unchanged
ingest took 0.3 s. The limit with the 30 s default has no measurable cost
(3.9 s with and without it).

**Correction to the paragraph above ("scoping is the mitigation").** `paths`
helps but does not remove the cost of the history: ingesting that repository
with a single file took 1.5 s instead of 4.0 s and a 19 MB instead of a 43.6 MB
database, because the whole object graph is still recorded. For a large
repository the first ingest can therefore exceed a client timeout even with
`paths`. The honest answer to that is an asynchronous job (start, poll), which
is not built. It is tracked as MIND-20 together with the open question of
interrupting writes, and becomes necessary when a real client times out on a
first ingest.

**Verified:** `tests/mcp_ingest_timeout.rs` (9, real binary) and the checkpoint
sweeps in `src/ingest/mod.rs` (2): the limit expires at each of nine git and
four directory checkpoints in turn, and every table of the database is
unchanged each time; an aborted ingest retires nothing and the retry converges;
a later write in the same session still works; a hung `git` is killed (also for
a scoped ingest); invalid values stop the server. Eight deliberate mutations
were each caught by at least one test: the pre-write check in both pipelines,
three other checkpoints, the kill, and the hand-over of the deadline to the
scan and to the ingest's reader (the last one only after a test was added: the
first version of the hung-git test never reached the reader). The remaining
checkpoints are covered by the sweep by construction (a removed checkpoint lets
the ingest succeed, which the sweep rejects) but were not mutated one by one.

## ADR-027: Repository slugs are observations from remotes; no fork claims (2026-11-07)

**Decision.** `mind repo info <path>` (and a `repository` block in MCP
`repo_outline`) describes a git repository read-only: identity (only if a marker
already exists; it is never created), HEAD, configured remotes, a primary slug,
and worktrees. A slug is `host/owner/name`, lowercased as the comparison key and
`Owner/Name` as written for display; nested groups keep their path.

The slug comes from a configured remote URL, never from the repository's
identity (ADR-015): remotes are added, renamed, and mirrored, and two clones of
one project share a slug but are different repositories. The primary slug is the
`origin` remote's; without `origin` it is the one slug all remotes agree on;
otherwise it stays unnamed and the candidates are listed. Hosts are kept as
observed (`ssh.example.dev` is not guessed to be `example.dev`), so matching by
`owner/name` is host-independent.

Remote URLs can embed credentials. `ingest::remote::parse` is the only reader
of a raw URL; its result never contains userinfo (including the scp-style `git@`),
so no downstream code can store or show a secret. In MCP, worktree paths outside
the allowed roots are redacted.

**No fork or mirror claims (for now).** Git cannot prove that a repository is a
fork, and remote names (`upstream`, `mirror`, `origin-github`) are conventions.
Mindspace lists remotes and says nothing about their relationship.

**Still to discuss: upstream hints.** Whether to surface a hint such as "has an
`upstream*` remote with a different slug than `origin`" is undecided. It was
deliberately left out of this slice, not rejected. Open questions: whether a
heuristic hint belongs in the model at all, how to keep it clearly separate from
fact, how mirrors of the same slug on other hosts (as in a repository with
`origin`, `origin-github`, `upstream`, `upstream-github`) must not read as forks,
and whether authoritative fork data from a forge API (optional, networked, with
credentials) is wanted instead.

**Consequences.** Nothing is persisted yet; slugs are computed per call. Storing
remote observations and attaching repositories to spaces (a Scope object,
separate from Space) follows with the next migration.

## ADR-028: Scopes attach code to spaces; views and remotes become tables (2026-11-07)

**Decision.** Migration 0009 introduces a minimal **Scope**: where ingested
material came from, kept separate from **Space**, the knowledge domain (README,
memory-model section 10). A scope is a git repository (keyed by its Mindspace
identity) or an ingest root (a directory, or a single file). `scope_spaces`
attaches scopes to spaces; every ingest adds the requested spaces, never removes
any, like `memory_spaces`. Spaces stay free-form (`project:owlenshell`); a
repository's `owner/name` is not encoded in a space name.

Code definitions reach spaces only through their scope: a git definition through
its repository, a file definition through the ingest root that contains its
path. `mind symbols --space X` and MCP `code_search` `spaces` follow directed
space links exactly like memory search. Hits report their `spaces` and the
repository's primary `Owner/Name` slug.

`repository_remotes` stores remotes as observed: credential-free URL, slug key
and display slug, first and last observation, and `is_current` for the latest
observation; older rows keep their history. `--repo` matches any observed
slug, a path substring, or an id prefix. `git_views` replaces the interim
`mind_meta` entries as the record of a repository's current views (ADR-025);
nothing is backfilled, so re-ingesting attaches scopes and views to old data.

**M6.** The baseline and write fence now run when the M6 migration (version 8)
is applied, via `M6_VERSION`, not when the latest migration is. The new tables
are current-state associations and observations, outside the fence inventory
and history; the CodeStructure temporal dimension stays unsupported. An older
binary refuses the database by version, so the fence is not needed to protect
them.

**Limits.** A single-file root and a directory root that contains it are
separate scopes; a definition matches any scope whose root contains its path.
Scopes are not removed when a repository moves or an ingest root disappears.
Upstream and fork hints remain undecided (ADR-027).

**Revisit when.** Scope-level operations are needed (detach a space, forget a
repository), memories should derive their spaces from scopes, or scope history
must be queryable.

## ADR-029: Confinement of MCP ingest and outline: kernel sandbox plus anchored access (2026-11-07)

**Status: proposed; amended at the end of MIND-18 with what was measured.**

**Problem.** Repository contents and the directory tree below an allowed root are
untrusted. Without any race, a repository can carry pointers that make Git read
and write outside every allowed root (`.git` as a symlink, a `gitdir:` file,
`objects/info/alternates`, a linked worktree whose main repository is elsewhere);
with a race, any path component can be swapped after it was checked (ADR-026
addendum). Both reached the database and wrote the identity marker outside the
roots. `repo_outline` leaked directory names the same way. Evidence and
experiments: MIND-18, `tests/ingest_confinement.rs` (19 tests).

**Invariants** (stated as outcomes, tested without reference to a mechanism):

- I1 no byte from outside the allowed roots reaches the database or a tool
  response, including `repo_outline`;
- I2 ingest reads only from locations governed by an rw root (target, git dir,
  common dir, object stores, work tree);
- I3 the only filesystem write of an ingest is the identity marker, inside an rw
  root;
- I4 read-only areas, also nested ones, are neither ingested nor changed, also
  under races;
- I5 fail closed: if protection cannot be established and *verified*, the
  repository tools are off, with a reason; nothing falls back silently;
- I6 no regression: plain repositories, linked worktrees whose main repository is
  inside the roots, `paths`, symlinks in a work tree are still not followed;
- I7 no inherited file descriptors survive into the confined process.

**Decision: two layers, because neither alone is enough** (measured, see the
Landlock spike in MIND-18: the kernel layer alone left exactly the four tests
that need ro/rw semantics red).

1. *Kernel layer (Landlock), minimum ABI 3.* Checked at the moment of access, so
   it does not matter which path trick or race redirected an access. It cannot
   express "read-only inside read-write" and cannot stop reading a read-only
   root. Applied by **self re-exec**: stage 1 validates the configuration,
   restricts itself and `exec`s itself; stage 2 starts confined. `restrict_self`
   confines only the calling and later threads (verified: a thread that existed
   before kept reading outside), so restricting a running process is not safe.
   Enforcement is never taken on trust: `FullyEnforced` is required **and**
   canary probes observe the behavior (reading, listing, creating and
   truncating outside must fail with EACCES; ENOENT does not count), in stage 1
   after restricting and again in stage 2, which does not rely on an environment
   marker. A Git probe proves Git works under the sandbox.
2. *Application layer, always on* (also with `--insecure-no-sandbox`). The target
   directory is opened once and the **opened inode** is checked (its real path
   via `/proc/self/fd`), not the name; Git runs with the work tree and `GIT_DIR`
   / `GIT_COMMON_DIR` pinned to `/proc/<pid>/fd/<n>`, so Git does not look up
   `.git` again; `.git` is opened and checked, a `gitdir:` pointer and
   `commondir` are resolved and must lie in rw roots; alternates must lie in the
   governing rw root; every file read in a directory ingest is opened, then its
   real path is checked against the ingest root and the governing mode.

**Library.** The `landlock` crate (official `landlock-lsm` organization,
0.4.x, MSRV 1.71) with `CompatLevel::HardRequirement`, over direct syscalls: it
carries the ABI negotiation that would otherwise be our risk, and it gave results
identical to a raw-syscall launcher on the same tests (15 green, 4 red before the
application layer). Condition: the dependency stays Linux-only and is reviewed
with the lockfile on every update.

**Configuration rules.** The DB directory must be dedicated: it is opened in
full (SQLite keeps `-wal` and `-shm` beside the database), so it must neither
contain nor lie inside an allowed root. Temp files go to a private directory
inside it (`TMPDIR`), not `/tmp`. Linked worktrees whose common directory is
outside the rw roots are refused. Not Linux: the repository tools stay off.

**Failure behavior.**

| Situation | Behavior |
|---|---|
| no roots configured | repository tools are off anyway; no sandbox is set up |
| Landlock missing, ABI < 3, not fully enforced | server runs, repository tools off, reason in every error |
| canary or Git probe fails after restricting | process exits (it is confined but unverified) |
| environment marker set but canary reachable | process exits |
| `--insecure-no-sandbox` | warning on every start, results carry `sandbox: disabled`; application layer stays on |

**Residual risk** is listed at the end of MIND-18 once measured.
