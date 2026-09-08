# The Mindspace memory model (v0)

This document defines the conceptual model that v0 implements. It is the
authoritative description of what objects exist, how they relate, what is
immutable, and what the v0 semantics of supersession, conflict, and deletion
are. When code and this document disagree, one of them is wrong; fix both.

Design north star: **sources are evidence, memories are interpretations.**
Historical evidence is never destroyed because newer information superseded it.

## 1. Object overview

```text
Source            an immutable record of external material (a file, later: a
  |               URL, a conversation). Owns the raw content and its hash.
  |
Evidence          an immutable, normalized fragment derived from a source
  |               (a markdown section, or a whole plain document), with an
  |               explicit anchor inside the source.
  |
Assertion         an immutable statement derived from evidence by a named,
  |               deterministic extraction rule (v0: claims only; events and
  |               relations are reserved kinds). Identity is the
  |               interpretation itself: identical evidence, rule, subject,
  |               and content is the same assertion, however many memories
  |               reference it over time.
  |
Memory            the addressable knowledge unit: one belief episode of one
                  assertion. Carries all mutable lifecycle state: which
                  spaces it belongs to, whether it is active, superseded, or
                  retired, and the timestamps of those transitions. The same
                  interpretation may be believed, retired, and believed
                  again; each episode is its own memory, and at most one
                  episode per assertion is active at a time.
```

Every arrow is a foreign-key relationship stored in the database, so the chain
`memory -> assertion -> evidence -> source` is always inspectable with plain
queries and with `mind trace <id>`.

## 2. Objects, identity, mutability

All objects are identified by UUIDv7 (time-ordered). Timestamps are stored as
UTC Unix epoch microseconds and rendered as RFC 3339 at the CLI edge.

| Object    | Identity | Natural key                          | Mutable? |
|-----------|----------|--------------------------------------|----------|
| Source    | uuid     | (kind, location, content_hash)       | never    |
| Evidence  | uuid     | (source_id, kind, ordinal)           | never    |
| Assertion | uuid     | (evidence_id, extraction, subject_normalized, content_hash) | never |
| Memory    | uuid     | — (belief episode)                   | state fields only |
| Space     | uuid     | name (case-sensitive, unique)        | never in v0 |
| SpaceLink | (from, to) | —                                  | never in v0 |

Immutability of Source, Evidence, and Assertion is not a convention: the
SQLite store installs `BEFORE UPDATE` triggers that abort any update to those
tables. The engine API exposes no mutation path for them. If a future change
legitimately requires one, it must arrive as a migration that consciously
drops the trigger.

Mutability of Memory is limited to its lifecycle columns
(`state`, `superseded_by`, `superseded_at`, `retired_at`, `retire_reason`).
A memory's content never changes, because its assertion never changes.
Superseding a memory creates a *new* memory (section 7).

### 2.1 Source

- `kind`: what kind of material. v0 implements `file`.
- `location`: canonical absolute path (for `file`). Part of the natural key.
- `title`: deterministic document title (section 6.4).
- `content`: the raw text of the material. v0 ingests text only.
- `content_hash`: SHA-256 over the raw bytes (hex).
- `media_type`: `text/markdown` or `text/plain`.
- `ingested_at`: transaction time of first ingestion.

Sources are versioned by content: re-ingesting a *changed* file at the same
location produces a *new* source row (new hash). The old source row and
everything derived from it remain. Source-level lineage (an explicit
"previous version" pointer between sources of one location) is deferred; the
information is recoverable from memories' supersession chains and shared
locations.

### 2.2 Evidence

Evidence is the normalized material an interpretation is derived from, with an
anchor that says where inside the source it came from:

- `kind`: `document` (whole file) or `section` (heading-delimited slice).
- `heading_path`: ancestor headings joined with ` > ` (e.g. `Setup > SQLite`);
  `NULL` for `document` evidence and for text before the first heading.
- `start_line` / `end_line`: 1-based, inclusive line range inside the source.
- `ordinal`: position of the fragment within the source (0-based).
- `content` / `content_hash`: the fragment text and its SHA-256.

Markdown sections are extracted with a real markdown parser
(`pulldown-cmark`), never with regex. Sections are flat: each heading opens a
section that extends to the next heading of any level; nested headings only
extend the displayed `heading_path`. Text before the first heading becomes its
own fragment with `heading_path = NULL`. Empty fragments produce no evidence.

### 2.3 Assertion

An assertion is a statement about the world, extracted from evidence by an
explicit, named, deterministic rule:

- `kind`: `claim` in v0. `event` and `relation` are reserved kinds in the
  schema so later extraction rules can fill them without a conceptual change.
- `subject` / `subject_normalized`: the topic the statement is about. The
  normalized form (lowercased, whitespace-collapsed) is the join key for
  history, conflicts, and supersession; the display form is preserved verbatim.
- `predicate`: reserved, `NULL` in v0 (subject-predicate-object claims arrive
  with rules that can determine predicates deterministically).
- `content`: what is asserted (in v0, the evidence text).
- `content_hash`: SHA-256 over `content`; part of the assertion's identity.
- `extraction`: the rule identifier, e.g. `markdown:section-heading`,
  `markdown:preamble`, `markdown:document`, or `text:document`. This is the
  provenance of *how* the interpretation was produced, not just where the
  material came from.
- `created_at`: transaction time the interpretation was extracted. Stable
  across belief episodes: re-extracting the same interpretation finds the
  existing assertion instead of duplicating it.
- `evidence_id`: the exact evidence the statement was derived from.

The natural key `(evidence_id, extraction, subject_normalized, content_hash)`
is enforced by a UNIQUE constraint. Two *different* fragments with identical
text are two assertions (they cite different evidence): that is corroboration,
not duplication. A new extraction rule (for example a future AI-assisted one)
produces new assertions from old evidence because the rule identifier is part
of identity; nothing is overwritten.

### 2.4 Memory

A memory is the unit users search, inspect, and organize. It is one *belief
episode* of one assertion: memories reference assertions many-to-one, and at
most one memory per assertion is active at a time (enforced by a partial
unique index). The split is deliberate:

- the assertion answers **what** is claimed, **how it was interpreted**, and
  **from where** (immutable),
- the memory answers **is it currently believed**, **where does it apply**
  (mutable lifecycle, spaces).

A proposition that is believed, retired, and believed again is one assertion
with several memory episodes; each episode keeps its own creation time, so
the store answers "we believed this from T1 to T2, and again from T3". A
later version may aggregate several assertions into one memory (for example
AI-derived claims merged with deterministic ones); that changes the
memory-to-assertion cardinality further and is additive.

Lifecycle state machine:

```text
            ingest                    supersedes(old, new)          retire
  [created] ------> active ----------------------> superseded
                        |                                              ^
                        +---------------- retire ----------------------+
```

- `active`: current knowledge.
- `superseded`: replaced by a specific newer memory (`superseded_by`,
  `superseded_at`). Supersession always points forward to a concrete memory.
- `retired`: invalidated without a successor (`retired_at`, `retire_reason`).
  This is the only form of deletion (section 9).

`created_at` is the transaction time the memory came into existence.

### 2.5 Space and SpaceLink

A space is a knowledge domain (`coding`, `linux`, `project:owlry`). Names are
case-sensitive and unique. A memory belongs to zero or more spaces through the
`memory_spaces` join table; the underlying memory is never duplicated.

A space link is a directed edge `(from -> to)` meaning: **knowledge of `to`
is usable inside `from`** (`from` imports `to`). Self-links are rejected;
duplicate links are rejected; cycles are permitted.

Traversal from a space follows outgoing links only. `project:owlry -> coding
-> linux` therefore makes `linux` knowledge visible from `project:owlry`, but
`project:owlry` knowledge is *not* visible from `linux`. This is the v0
realization of "spaces may use knowledge from other spaces without reverse
visibility". A permissions language, link labels, and configurable
transitivity are deferred.

Traversal algorithm (deterministic): breadth-first from the root, expanding
neighbors in lexicographic name order, with a visited set. Cycles terminate.
The engine records the path by which a space was reached so search results can
explain *why* they matched (e.g. `via project:owlry -> coding`).

`--space X` on `mind search` means: memories directly in X **or** in any space
reachable from X through directed links.

Space links are knowledge-import semantics, nothing more. They must never be
treated as, or grown into, access-control boundaries: permissions need their
own objects and rules, because "who may read" and "which knowledge imports
which" are different questions that happen to both involve graphs.

## 3. Provenance

Every memory can answer "where do you know this from?" through a fully stored
chain:

```text
Memory (id, created_at, state)
  -> Assertion (extraction rule, subject, content)
     -> Evidence (kind, heading_path, line range, content hash)
        -> Source (kind, location, title, content hash, ingested_at)
```

Plus the derivation context: which spaces were requested at ingest time, and
whether the memory was created fresh, reused, or superseded an older one.

The four standing questions map to v0 capabilities like this:

| Question                          | v0 answer |
|-----------------------------------|-----------|
| What is true now?                 | `mind search` (default filters to active memories) |
| What was considered true at T?    | `mind search --at <RFC3339>` / `mind history <subject>` |
| What superseded this?             | `mind inspect` / `mind trace` follow `superseded_by`; `mind history` shows the chain |
| What contradicts this?            | `mind conflicts` lists (space, subject) groups with multiple active memories |
| Where do you know this from?      | `mind trace <id>` renders the full provenance chain |

## 4. Temporal state

v0 implements **transaction time** honestly: every state transition records
when it happened. **Valid time** (when the statement was true in the outside
world) is approximated by the ingestion time and can be refined later with an
explicit `valid_from`/`valid_until` pair on assertions; the schema leaves room
without reinterpretation.

A memory is defined to be *active at time T* iff:

```text
created_at <= T
AND (superseded_at IS NULL OR superseded_at > T)
AND (retired_at    IS NULL OR retired_at    > T)
```

`mind search --at T` evaluates exactly this predicate over the stored
timeline, so "what did we believe then?" is a real query, not a narrative.
When `--at` is given it replaces the default state filter (the timeline, not
today's state, decides what is visible).

Two consequences worth naming:

- **Late ingestion is not time travel.** A document written in January and
  ingested in September enters the store with September transaction times.
  `--at` for August will not show it, and must not: the store did not believe
  it in August. Valid-time modeling (when the statement was true in the
  world) is a separate, additive axis (section 11).
- **Historical wording stays historical.** A 2024 source asserting "Foo is
  the current implementation" becomes an active memory when ingested in
  2026, because transaction time says "believed now". Interpreting such
  relative wording is a semantic problem and is deferred; the data model
  keeps the assertion immutable and dated, so a future valid-time or
  re-interpretation pass can correct the view without rewriting history.

## 5. Conflict

Definition (v0, deterministic): two or more **active** memories whose
normalized subjects are equal and that are direct members of the same space.
They coexist; nothing is collapsed. `mind conflicts` lists each group with the
member memories, their sources, and creation times.

These groups are **candidate conflicts**: collisions of subject that deserve
human inspection. v0 cannot and does not claim to detect contradictions
(that needs interpretation): equal subjects may corroborate, duplicate, or
simply be unrelated sections that happen to share a heading, and genuine
disagreements often use different subjects. The guarantee that matters is
negative: nothing is silently merged, ranked away, or dropped, and the full
disagreement history stays reconstructable.

Notable consequences:

- The same subject from two *different* sources is a conflict to be inspected
  (including identical text: Mindspace surfaces the duplication instead of
  silently merging).
- Retired and superseded memories never appear in conflicts; only current
  disagreements matter there. Full disagreement history is visible via
  `mind history`.

Richer contradiction semantics (negation detection, contradiction links) need
interpretation and are deferred; the coexistence rule above already guarantees
nothing is lost.

## 6. Deterministic ingest

v0 ingests plain text (`.txt`) and Markdown (`.md`, `.markdown`) files. No AI
is involved anywhere.

### 6.1 Pipeline

```text
read bytes -> sha256 -> resolve canonical location
  -> detect kind by extension (unsupported => actionable error)
  -> parse (pulldown-cmark for markdown; plain split for text)
  -> evidence fragments (sections / whole document)
  -> assertions (one per fragment, named extraction rule)
  -> reconcile against active memories of the same source location
  -> commit atomically (single transaction)
```

### 6.2 Idempotency and reconciliation (exact semantics)

Ingestion is keyed on `(kind, location, content_hash)`:

- **Same content at same location**: no new source, evidence, or assertions.
  Existing active memories derived from that location are *reused*; any
  requested space the memory does not yet have is added. Ingesting twice with
  the same flags is a full no-op.
- **Changed content at same location**: a new source (and evidence,
  assertions) is recorded; reconciliation runs (below).
- **Same content at a different location**: a genuinely new source; no
  supersession (different provenance).

Reconciliation is scoped per subject `S` in the union of (a) subjects derived
from the new source and (b) subjects of currently active memories derived from
the same location `L`:

1. `olds` = active memories whose provenance source location is `L` and whose
   normalized subject is `S`, ordered by `created_at`, then id.
2. `news` = new fragments with subject `S`, ordered by ordinal.
3. Pair them index-wise (`olds[i]` with `news[i]`):
   - fragment content identical to the old memory's assertion content:
     **reuse** the old memory (add any newly requested spaces);
   - content differs: **supersede** the old memory with a new memory built
     from the new fragment.
4. Leftover olds and news (no same-subject partner) are matched across
   subjects by identical content, olds in `(created_at, id)` order against
   news in ordinal order, first match wins: a match **supersedes** (this is
   how a renamed or moved heading keeps its lineage). Deterministic.
5. Remaining leftover `news`: **create** active memories.
6. Remaining leftover `olds` (subject no longer present and content gone):
   **retire** with reason `subject no longer present in source at re-ingest`.

Consequences worth knowing:

- Editing a file and re-ingesting it updates knowledge *with full history*;
  the old memory remains inspectable at its old id forever.
- Renaming a heading or moving a paragraph does not break the lineage: the
  new memory supersedes the old one across subjects, because the proposition
  is identical.
- Removing a section from a file and re-ingesting retires the corresponding
  memory with an explicit reason (it does not silently vanish).
- Re-ingesting unchanged content after a manual `mind retire` recreates the
  memory: an explicit re-ingest is an explicit request. The recreated memory
  *reuses* the existing assertion, because the interpretation (same
  evidence, rule, subject, content) already exists; what is new is the
  belief episode.
- Multiple fragments sharing one subject inside a single file pair up with
  old memories index-wise; surplus fragments become separate active memories
  and surface in `mind conflicts`. This is deterministic and inspectable
  rather than clever.

All of the above happens inside one transaction, so a crashed ingest leaves no
half-updated state.

### 6.3 Subjects

- Markdown section: the leaf heading text, whitespace-normalized.
- Markdown preamble (text before the first heading): the document title.
- Plain text: the file stem.

`subject_normalized` (lowercase, whitespace-collapsed) is the matching key;
the original text is preserved for display. Normalized subjects are not
unique across sources: equal subjects from different sources coexist and are
reported as conflicts (section 5).

### 6.4 Titles

A source's title is the first heading text of the markdown document (any
level), or the file stem if there is no heading (or for plain text).

### 6.5 Spaces at ingest

`--space` is required and may be repeated. Requested spaces must already
exist; the error names the missing space and the command that creates it.
This keeps ingest side-effect-free with respect to the space taxonomy.

## 7. Supersession

Supersession is a relation between two memories: `old.superseded_by = new`,
`old.superseded_at = T`. The old memory is not modified in content, not
deleted, and stays fully traceable (`mind inspect <old-id>` keeps working; the
default search excludes it, `--include-superseded` includes it).

v0 creates supersessions in exactly one place: the reconciliation rule of
section 6.2, in two shapes: same subject with changed content, and identical
content under a changed subject (rename/move). Manual `mind supersedes <a>
<b>` is a natural future command; the schema already supports it.

Many old memories may point at one new memory (e.g. after duplicate-subject
fragments collapsed); a memory is superseded by at most one successor.

## 8. Confidence and authority

Deliberately **absent in v0**. Deterministic extraction does not produce
meaningful graded trust, and a fabricated 0.87 would be worse than none.
When confidence/authority arrive they will be explicit columns on assertions
(per-rule reliability) or sources (kind-based authority), added by migration.
Nothing in the v0 model needs reinterpretation to accommodate them.

## 9. Deletion

There is no destructive deletion in v0, by design:

- Invalidating a memory = `mind retire <id> [--reason]` (soft, timestamped,
  inspectable). Retiring a non-active memory is an error.
- Deleting the underlying file on disk changes nothing inside Mindspace; the
  stored source and its memories remain.
- When source-level removal is introduced later, its semantics are already
  defined by this model: it may retire derived memories, never destroy
  evidence. Historical rows are append-only.

Derived data (evidence, assertions, memories) is recomputable from a source
by re-running the deterministic rules; hashes make drift detectable.

Three invalidation words must never collapse into one mechanism:

- **retire** (implemented): a memory episode ends, timestamped and reasoned;
  everything upstream and the row itself remain inspectable.
- **forget** (future): a source or location is withdrawn; its derived
  memories are retired, never destroyed. The chain memory -> assertion ->
  evidence -> source defines exactly what is derived, so impact is
  computable.
- **purge** (future, privacy/legal): hard deletion of specific rows. The
  immutability triggers block UPDATE only; DELETE is deliberately not
  blocked, so true erasure stays possible. A purge must remove FTS rows in
  the same transaction and respect foreign keys; it is a separate, explicit,
  audited operation, never a side effect of ingest or retire.

## 10. Scope versus space

Kept separate, as the README requires:

- **Space** (implemented): which knowledge domain a memory is useful in.
- **Scope** (deferred): where information originated or applies (repository,
  session, document, project).

v0 does not need a first-class Scope object because provenance already
records origin concretely: source kind, canonical location, title, and
evidence anchors. Introducing `Scope` later means adding a table that sources
or memories may reference; it does not require reinterpretation of anything
defined here.

## 11. Explicitly deferred (model level)

- Events and relations as assertion kinds (schema reserved).
- Predicates / subject-predicate-object claims.
- Valid-time intervals distinct from ingestion time.
- Confidence, authority, trust weighting.
- A first-class Scope object.
- Space link labels, exclusions, configurable transitivity.
- Manual supersession commands.
- Un-retiring memories.
- Source-level lineage pointers and source removal semantics.
- Any AI-assisted extraction (the extraction column is designed to carry
  model-assisted rule identifiers when that day comes, without schema or
  conceptual changes).
