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

## ADR-004: Assertion-Memory is 1:1; supersession creates a new memory (2026-09-08)

**Decision.** A memory wraps exactly one assertion. Superseding a memory
creates a new memory and sets `old.superseded_by = new`. History is the chain
of memories, not a mutable content slot.

**Why.** It keeps assertions immutable all the way through, makes "what was
considered true at T" a pure timestamp query, and avoids mutable
"active assertion" pointers. Aggregating multiple assertions into one memory
later changes only the cardinality.

**Revisit when.** Aggregation or AI-derived multi-claim memories arrive.

## ADR-005: Reconciliation keyed on (source location, subject) (2026-09-08)

**Decision.** Re-ingest of a changed file supersedes/reuses per-subject
memories derived from the same source location, per the exact algorithm in
`docs/memory-model.md` section 6.2, including retirement of subjects that
disappeared from the file. Cross-source same-subject content is never
auto-superseded; it surfaces as a conflict.

**Why.** Location+subject is the only strictly deterministic key available
without interpretation. Auto-superseding across sources would silently
resolve conflicts, which the model forbids.

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
