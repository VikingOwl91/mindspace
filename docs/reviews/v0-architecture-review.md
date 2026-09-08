# Mindspace v0 architecture review (adversarial)

Reviewed commit: `b2811bc` (v0 foundation).
Method: full read of docs, schema, engine, and tests; the model was attacked
against long-lived-store scenarios (migration churn, late ingestion, renames,
retracts, corroboration, consolidation, deletion) rather than against the
test suite. Documentation, Rust types, SQL, and tests were cross-checked for
agreement.

Verdict up front: **B - targeted schema corrections**. The
Source -> Evidence -> Assertion -> Memory spine is sound and kept. Two HIGH
defects live in how Assertion identity and supersession lineage work; both
are cheap to correct now and expensive to correct once real data exists.

---

## Findings

### HIGH-1: Assertion identity is not deterministic; the store can accumulate indistinguishable assertions

**Current behavior/model.** `assertions` has no natural key: identity is the
surrogate uuid only. The ingestion path (`get_assertion`) always INSERTs a
new row. Assertions carry no `created_at`. Assertions are 1:1 with memories
(`memories.assertion_id UNIQUE`).

**Why it is dangerous.** In v0, the interpretation of a fragment is a
deterministic function of `(evidence, extraction rule, subject, content)`.
Whenever the lifecycle demands a new memory for an unchanged proposition
(retire then re-ingest, or a file whose content reverts), the store
manufactures a second assertion row that is identical in every observable
column and differs only by uuid. That is semantic garbage in an append-only,
inspectable store: no query can distinguish the copies, corroboration and
dedup logic must fight them, and "assertion = the interpretation" becomes
false. The identity/lifecycle split is attached at the wrong granularity:
the *interpretation* is stable, the *belief episode* is what churns.

**Concrete scenario.**

1. `mind ingest notes.md` with section "Kernel: The scheduler is CFS."
2. `mind retire <id>`
3. `mind ingest notes.md` (unchanged)
4. The store now holds two assertions with identical
   `(kind, subject, subject_normalized, content, extraction, evidence_id)`
   and no other distinguishing metadata. The same happens when a file flips
   A -> B -> A and reverts to byte-identical content.

**Recommended model.** Assertions become deterministically identifiable:
`UNIQUE (evidence_id, extraction, subject_normalized, content_hash)` plus a
`created_at` (extraction transaction time). Assertion -> Memory becomes 1:N:
a memory is a *belief episode* referencing an interpretation; at most one
active episode per assertion (partial unique index on
`memories(assertion_id) WHERE state = 'active'`). Ingestion looks up the
assertion by natural key and inserts only when missing; the FTS row is
written exactly once per assertion. Two *different* fragments with the same
text still yield two assertions (different evidence): corroboration, not
duplication.

**Migration impact.** v0 is pre-alpha with disposable databases: migration
`0001` is corrected in place rather than wrapped in compatibility
machinery. Code impact: core `Assertion` gains `created_at`; the store's
assertion handling becomes lookup-or-insert; retrieval is unchanged (state
filtering already selects episodes).

**Test required.** Recreate-after-retire reuses the same assertion id; a
file flipping A -> B -> A yields three memory episodes where episode 3 shares
episode 1's assertion; `trace`/`inspect` expose the shared interpretation.

### HIGH-2: Content-preserving subject changes break provenance lineage (renamed or moved headings)

**Current behavior/model.** Reconciliation (memory-model 6.2) pairs old and
new fragments strictly within `(location, subject)`. Leftover olds are
retired ("subject no longer present in source at re-ingest"); leftover news
are created. There is no cross-subject matching.

**Why it is dangerous.** The most common edit in long-lived notes is
renaming a heading or moving a paragraph. For that edit the model answers
"What superseded this?" with *nothing*: the old memory is retired and an
unrelated new memory is born. The supersession chain - the spine of
historical inspection - is severed exactly when the proposition survived.
The retire reason is literally true but semantically misleading.

**Concrete scenario.**

1. v1: `# Storage\n\nEverything lives in SQLite.`
2. v2: `# Persistence\n\nEverything lives in SQLite.` (heading renamed)
3. Re-ingest: old memory `Retired`, new memory `Created`,
   `superseded_by = NULL` on both sides. History for "storage" dead-ends;
   history for "persistence" has no origin.

**Recommended model.** Supersession must follow the proposition, not the
label. After per-subject pairing, leftover olds and leftover news are matched
by identical content (olds ordered by `(created_at, id)`, news by ordinal;
first-match, index-wise; fully deterministic) and superseded. Only
unmatched leftovers retire or are created. This also makes the June "returns
to iced" scenario preserve lineage through label changes.

**Migration impact.** None (reconciliation logic only).

**Test required.** Heading rename produces `Superseded` (not `Retired` +
`Created`), `old.superseded_by == new`, content preserved, old memory still
inspectable.

### MEDIUM

- **M1 - Conflict means subject collision, not contradiction.** Two active
  memories sharing a normalized subject in one space are listed as
  conflicting, whether they corroborate, duplicate, or contradict each other;
  genuine contradictions with different subjects are invisible. Disposition:
  keep the v0 behavior (coexistence + inspectability, no semantic claims),
  but docs must call these *candidate conflicts* for human inspection. True
  contradiction detection requires interpretation and stays deferred. No
  schema change: conflicts are a query, not stored state, so the definition
  can evolve freely.
- **M2 - Retire is not atomic.** `retire_memory` reads state, then updates
  unconditionally. Fix included: conditional
  `UPDATE ... WHERE id = ? AND state = 'active'` + rows-affected check.
- **M3 - File rename/move duplicates active knowledge.** Same bytes at a new
  location is a new source; the old location's memories stay active, so the
  store holds both and `conflicts` invents a collision. Move detection is
  document-diff territory and deferred, but the model does not block it:
  sources of one location share `content_hash` values, so a future
  move/version link is additive. Documented, not fixed.
- **M4 - No purge path.** Retire, forget, and purge must not collapse into
  one mechanism. Immutability triggers block UPDATE, deliberately not
  DELETE, so privacy/legal hard deletion stays possible; a future purge must
  remove FTS rows and respect foreign keys. Documented now, implemented
  later.
- **M5 - Temporal naming.** All v0 timestamps are transaction time. Late
  ingestion (a January document ingested in September) must never be read as
  valid-from September; `--at` answers "what did the store believe at T",
  not "what was true in the world". Already implemented honestly; docs now
  say it with the late-ingestion example. Valid-time columns remain
  additive.
- **M6 - `--json` is unstable.** It serializes engine structs directly.
  Documented as unstable in the README; no DTO framework is warranted in
  v0.
- **M7 - Space links are not security boundaries.** Traversal is
  knowledge-import semantics. Access control must be a separate mechanism
  that never reuses space edges. Documented in the model.

### LOW

- **L1 - Evidence identity is positional per source version.** Inserting a
  paragraph shifts ordinals and duplicates unchanged fragments as new
  evidence rows in the new version. Memory continuity is preserved by
  content matching (strengthened by HIGH-2); future diff-based evidence
  mapping across versions is additive. Documented.
- **L2 - `assertion.content` duplicates `evidence.content` in v0.**
  Deliberate seam: future rules (AI included) may interpret rather than
  copy. Keep.
- **L3 - `superseded_by` is single-successor.** Sufficient for
  reconciliation-produced supersession; a future manual `mind supersedes`
  may want many-to-one, which is an additive schema change.

### GOOD / KEEP

- The Source -> Evidence -> Assertion -> Memory spine; immutable upstream,
  lifecycle only on memories.
- Sources keyed by `(kind, location, content_hash)`: re-ingest dedup, one
  version per change, old versions never touched.
- Immutability as `BEFORE UPDATE` triggers, not convention.
- Reconciliation inside a single transaction.
- Spaces on memories (interpretations), not assertions (evidence-derived
  statements); N:M membership; directed import links; deterministic
  cycle-safe BFS with path explanation.
- FTS over assertions, index written once, state filtering at query time.
- Conflicts coexist; nothing is collapsed; history is append-only.
- No confidence/authority fabrication; `--at` is honest transaction time.
- Single crate, `Store` trait as the only storage seam, no SQLite types
  outside storage.

### Questioned and rejected (with reasons)

- **A Claim entity now.** The concrete consumers are (a) corroboration
  visibility - derivable at query time from `subject_normalized` +
  content equality, and (b) cross-source proposition identity - no v0 use
  case. Consolidation (N assertions -> 1 memory) is additive later
  (memory-assertion join); assertions are independent of memories today, so
  nothing blocks it. Adding Claim now would be speculation.
- **Assertions belonging to spaces instead of memories.** Membership is a
  property of where a *belief* is useful, not of where evidence lives; the
  same evidence can legitimately feed memories in several domains.
- **Search over evidence instead of assertions.** Retrieval answers "what do
  we believe"; evidence text remains one `trace` away. The assertion index
  also keeps future interpreted (non-copied) content searchable.

---

## Cardinality table (corrected model)

| Relationship                    | Cardinality                  | Rationale |
|---------------------------------|------------------------------|-----------|
| Source -> Evidence              | 1:N                          | one version yields many anchored fragments |
| Evidence -> Assertion           | 1:N                          | one span can support several propositions (future rules); v0 emits one |
| Assertion -> Memory             | 1:N, at most 1 active per assertion | assertion = stable interpretation; memory = belief episode |
| Memory <-> Space                | N:M                          | multi-domain membership without duplication |
| Space -> Space                  | directed M:N                 | import edges; no reverse visibility; not access control |
| Memory -> Memory (superseded_by)| N:1                          | each superseded memory has at most one successor |
| Evidence -> Source location     | N:1 via source               | versions share location, differ by hash |

---

## Recommendation: B - targeted schema corrections

Not A: HIGH-1 leaves the immutable store accumulating rows that differ by
nothing but a uuid, corrupting the "assertion = interpretation" concept the
whole model rests on, and HIGH-2 severs provenance for the most common real
edit. Both are cheap now: no released databases, no compatibility promise.

Not C: the four-layer spine survives every torture scenario thrown at it
(corroboration, retraction, flip-flops, late ingestion, AI-derived claims,
conflicts). The rejected alternatives above would add entities without
concrete semantic consumers. Correcting identity granularity and supersession
lineage keeps the model small *and* honest.
