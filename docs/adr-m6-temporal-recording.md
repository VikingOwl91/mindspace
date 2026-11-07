# ADR-021: Temporal recording and history coverage

**Status:** Accepted for Stage 0 and Stage 1. Stage 2 query work is deferred.
**Date:** 2026-10-23

## Context

Mindspace has observation and lifecycle timestamps, but they do not establish a
complete record of when the database learned or changed each fact. A timestamp
can be supplied by an observer, have coarse precision, or describe a source's
world rather than the database's knowledge. The current `SearchQuery::at` also
has established timestamp semantics that must not silently change.

## Decision

### Three separate time coordinates

- **Observation time** is the source or observer's wall-clock time, such as
  `observed_at`. It says when an observation claims to have occurred. It is not
  a serialization order or proof of a database commit.
- **Recording revision `R`** is a database-local, monotonically increasing
  committed revision. It orders knowledge writes in this database only. One
  successful knowledge-write transaction receives one revision; a rolled-back
  or uncertain transaction does not authorize a later write with that revision.
- **World-validity `V`** is the time or interval for which a typed fact is
  claimed to hold in the world. It is independent of both observation time and
  recording revision. It is recorded only when supported by an explicit source
  or domain rule, never inferred from ingestion time.

`belief_at(R)` means the database's recorded belief state at revision `R`.
`world_history(R, V)` means the world-validity state at `V` as it was known by
the database at revision `R`. They are distinct axes and must not be conflated.
These names define the future query semantics, not APIs delivered in Stage 0 or
Stage 1.

Keep legacy `SearchQuery --at` behavior unchanged. Today it filters memory
episodes using their timestamp fields (`created_at`, `superseded_at`, and
`retired_at`). It is not an `R` or `V` query and must not be reinterpreted as
one.

### Atomic v8 history baseline

The v8 cutover creates a single atomic baseline revision `B`. The migration
captures the extant state and records its baseline in the same transaction as
the schema cutover. Legacy rows found at cutover are known to be present at
`B`; this does not prove they were created at `B`. Their earlier creation,
changes, and missing transitions remain unknown. No pre-`B` intervals,
observations, or events may be fabricated from legacy timestamps or current
state. Full recording history begins at `B`.

### Minimum typed coverage

Stage 0 and Stage 1 establish typed, append-only recording history for:

- memory belief lifecycle transitions;
- spaces, memory-space memberships, and directed space links;
- logical-source location and presence;
- repository path and Git ref state, alongside the existing ref events;
- availability of immutable canonical rows in covered domains, including
  sources, evidence, assertions, and Git objects; code-structure-derived rows
  remain unsupported until their own coverage is added;
- observations and events, including their observation time and recording
  revision.

Each history is domain-typed and records the state or transition needed to
reconstruct its covered values at `R`. `V` is stored separately and only for
domains with an explicit valid-time claim. Existing `created_at`, `observed_at`,
and event timestamps remain provenance fields, not substitutes for revision
history. This is not generic event sourcing: it does not require every internal
operation to become an event, and it does not infer missing history.

Code structure and relationship derivations, vectors and embedding indexes,
FTS rankings, and world-validity queries are unsupported until their required
temporal coverage is explicitly added. Embedding derivation keys retain their
existing meaning. FTS ranking is a derived result, not historical truth.

### Write revision and enforcement contract

Knowledge writes use `BEGIN IMMEDIATE` and allocate exactly one `R` per
transaction. The allocation and all corresponding state/history changes commit
atomically. Persistent SQLite triggers on covered canonical and temporal state
tables require the connection-local `m6_write_revision()` authorization for
the active transaction. A row in the revision table, including a previously
committed revision, is never by itself write authorization.

Each history entry identifies its producer/lifecycle-owner family, such as
file ingest, Git ingest, or manual writes. This family says which subsystem
owns the lifecycle transition; it is independent of derivation identity and
is not an authorization credential.

The store owns transaction-private authorization state and a hook that exposes
`m6_write_revision()` only while the allocated revision is active. RAII cleanup
clears that state on every exit. On rollback or uncertain commit/rollback
outcome, the authorization is discarded and must not be reused or replayed;
the connection is not trusted for another authorized write until its state is
known clean. Existing immutability and write-authorization checks remain in
force.

An older binary or connection that lacks `m6_write_revision()` therefore fails
closed when it attempts an ordinary write to a guarded knowledge table. This
is compatibility protection against uninstrumented application writers, not a
security boundary against hostile SQL or a database owner who can alter the
schema, register functions, or modify the database directly. New binaries must
reject a database whose recorded schema version is newer than they support;
they must not skip unknown migrations and proceed.

`mind_meta` is fenced like the knowledge tables: writing a database setting
through this storage layer allocates a recording revision, so an older binary
cannot change settings such as embedding enablement on a migrated database.
Its changes record no temporal history, and the configuration dimension stays
unsupported for historical queries. `mind_meta` must not be treated as a
source of temporal truth or used to authorize knowledge writes.

FTS5 virtual and shadow tables are also outside trigger-based write fencing.
They are derived indexes, not canonical history. The store checks FTS counts
against canonical assertions on every open and runs full agreement plus FTS5
internal integrity checks on demand and around every rebuild; the index can be
rebuilt from the canonical assertions. No claim is made that triggers fence
FTS writes or preserve past rankings. The same distinction applies to other
derived indexes.

### Capability and failure behavior

The store exposes an explicit temporal-history capability status with the
baseline and covered domains. Until the required domain coverage is complete,
the capability reports partial or unsupported. A request that needs uncovered
history returns an explicit typed unsupported/incomplete-history error. It
must not silently answer from current state, legacy timestamps, or a partial
timeline. Stage 0 and Stage 1 establish recording and capability reporting;
Stage 2 adds historical query surfaces.

## Invariants preserved

- Assertion natural identity, evidence provenance, immutability, and
  Assertion-to-Memory episode semantics remain as in ADR-004 and ADR-012.
- Manual writes retain the provenance and storage-layer authorization guards
  in ADR-017. Producer/lifecycle-owner family does not replace the manual
  write checks or grant manual-write authorization.
- Embeddings remain derived by the complete derivation key and do not become
  canonical knowledge through this ADR.
- No automatic conflict arbitration or LLM resolver is introduced.

## Scope and references

This ADR specifies an implementation contract, not completed code or schema.
Stage 0 and Stage 1 cover the v8 baseline, typed recording, write fencing, and
capability reporting. Stage 2 historical query implementation is deferred.

Relevant decisions in [decisions.md](decisions.md): ADR-003 (migrations),
ADR-004 and ADR-012 (assertions and belief episodes), ADR-006 (FTS), ADR-007
(immutability triggers), ADR-009 (timestamps), ADR-013 (source lifecycle),
ADR-015 (Git refs), ADR-016 (derived code structure), ADR-017 (manual writes),
and ADR-019 (`BEGIN IMMEDIATE`).
