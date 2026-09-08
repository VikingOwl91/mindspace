# Mindspace architecture (v0)

This document describes how v0 is actually built: module boundaries, storage
design, and the seams deliberately left for later growth. The conceptual model
lives in `docs/memory-model.md`; the reasoning behind individual choices lives
in `docs/decisions.md`.

## 1. Shape

```text
mind CLI
   |
   v
Mindspace engine          (pure domain logic: core, ingest, retrieval)
   |
   v
Store trait               (the storage boundary)
   |
   v
SQLite (rusqlite)         (only implementation in v0)
```

One crate, one binary (`mind`), no daemon, no network, no runtime dependency
on anything outside the machine. Everything runs synchronously; there is no
async runtime because nothing in v0 benefits from one.

### Module map

```text
src/
  core/        domain types (Source, Evidence, Assertion, Memory, Space,
               SpaceLink), ids/timestamps/hashes, the error type.
               Depends on: uuid, sha2, time, serde, thiserror. No SQL.
  storage/     the Store trait (the storage boundary) and its SQLite
               implementation, schema migrations, FTS index, triggers.
               Depends on: rusqlite. Exposes and accepts only core types.
  ingest/      deterministic observation: file and directory scans,
               markdown/text parsing into evidence drafts, subject/title
               rules, and the ingest orchestration that calls the store.
               `ingest/git.rs` reads git repositories through the system
               git binary (plumbing only; ADR-014) and hands normalized
               object/ref data to the store. Pure parsing functions are
               unit-tested without a DB or a repository.
  retrieval/   search orchestration: query filters, directed space traversal
               (BFS, cycle-safe), hit assembly and explanation.
  cli/         clap definitions and human/JSON rendering. Knows the engine,
               never SQL.
  lib.rs       engine assembly: opening a store, running migrations.
  main.rs      CLI entry.
```

Boundary rules enforced by review and by `use` statements:

- `core` never imports `storage`, `ingest`, `retrieval`, or `cli`.
- Only `storage` knows SQLite. `rusqlite` types do not appear outside it.
- `ingest` and `retrieval` are the only callers of `Store` besides `cli`.
- The CLI depends on the engine, not on storage internals; a future
  `mind serve` would sit at the same seam (section 5).

## 2. The storage boundary

`storage::Store` is a trait whose methods correspond to use cases
(create/list/link spaces, `record_observation`, `get_memory_detail`,
`search`, `list_conflicts`, `retire_memory`, ...), not to tables. All
signatures traffic in owned `core` types.

v0 ships one implementation, `storage::sqlite::SqliteStore`. A PostgreSQL
implementation would implement the same trait; nothing else in the codebase
would change. Two deliberate compromises keep the trait honest without
speculative machinery:

1. **Multi-invariant operations are single trait methods.** An observation
   must atomically record the session, per-file observations, source
   identity and lifecycle decisions (new versions, inferred moves,
   disappearances), evidence, assertions, memories, space memberships, and
   supersessions/retirements. Rather than exposing transaction handles
   through the trait (which forces every backend to model interactive
   transactions), the store method `record_observation` owns the
   transaction. The identity, lifecycle, and reconciliation semantics are
   documented in `docs/memory-model.md` sections 2.1 and 6 and belong to
   the contract of that method, whichever engine implements it.
2. **Full-text search is a store capability.** `search` takes a structured
   query (text, space set, state/time filters, limit) and returns hits with
   rank and snippet. SQLite implements it with FTS5; a PostgreSQL backend
   would use tsvector. Lexical index details never leave the storage module.
   The text is tried as an FTS5 match expression; if that is a syntax error,
   the same text is retried as a literal quoted phrase. Both attempts are
   deterministic, so plain text containing punctuation ("O(1)") searches
   fine while FTS5 operators stay available.

## 3. SQLite specifics

- `rusqlite` with the `bundled` feature: a static SQLite with FTS5 compiled
  in, so behavior does not depend on the system's libsqlite3 build flags.
- Foreign keys are enabled on every connection (`PRAGMA foreign_keys = ON`).
- Migrations are embedded SQL files under `migrations/`, applied in order and
  tracked in `schema_migrations(version)`. Migrations run on every open; they
  are idempotent by construction (applied versions are skipped). `mind init`
  additionally creates the database file and parent directories.
- `BEFORE UPDATE` triggers on `sources`, `evidence`, and `assertions` abort
  any mutation, making immutability a property of the store, not a habit of
  the code (memory-model section 2).
- The FTS5 index `assertion_fts(assertion_id UNINDEXED, subject, content)` is
  written once per assertion and never mutated, because assertions are
  immutable. Superseded/retired knowledge stays searchable through explicit
  flags; state filtering happens at query time by joining to `memories`.
- Every write that spans tables happens in one transaction.

### Schema (migration 0001, abbreviated)

```text
spaces(id, name UNIQUE, description, created_at)
space_links(from_space -> spaces, to_space -> spaces, created_at,
            PRIMARY KEY (from_space, to_space), CHECK (from_space <> to_space))
logical_sources(id, kind, state present|absent, current_location, created_at)
sources(id, logical_id -> logical_sources, kind, location, title, content,
        content_hash, media_type, size_bytes, ingested_at,
        UNIQUE (logical_id, content_hash))
ingest_sessions(id, root, scope directory|file, observed_at)
source_observations(session_id -> ingest_sessions, logical_id, location,
        content_hash, observed_at, PRIMARY KEY (session_id, location))
source_events(id, logical_id, kind moved|disappeared, from_location,
        to_location, version_id, session_id, created_at)
evidence(id, source_id -> sources, kind, heading_path, start_line, end_line,
         ordinal, content, content_hash, UNIQUE (source_id, kind, ordinal))
assertions(id, kind, subject, subject_normalized, predicate, content,
           content_hash, extraction, created_at, evidence_id -> evidence,
           UNIQUE (evidence_id, extraction, subject_normalized, content_hash))
memories(id, assertion_id -> assertions, state CHECK active|superseded|retired,
         created_at, superseded_by -> memories, superseded_at,
         retired_at, retire_reason)
  partial UNIQUE index: memories(assertion_id) WHERE state = 'active'
memory_spaces(memory_id -> memories, space_id -> spaces, created_at,
              PRIMARY KEY (memory_id, space_id))
assertion_fts  FTS5(assertion_id UNINDEXED, subject, content)

Git objects and refs (migration 0003; ADR-015):

repositories(id, current_path, created_at)
git_commits(repository_id, oid, tree_oid, author/committer name/email/time,
            message, parents in order via git_commit_parents; PK (repository_id, oid))
git_trees(repository_id, oid) + git_tree_entries(tree, name, mode, target, kind)
git_blobs(repository_id, oid, size, content NULL except text blobs)
git_tags(repository_id, oid, target, target_kind, tagger, message)
git_refs(repository_id, name, target_oid, target_kind, symbolic_target,
         last_observed_at)                 -- latest observed state
git_ref_events(id, repository_id, ref_name, kind created|fast_forward|
         non_fast_forward|deleted, from_oid, to_oid, session_id, created_at)
git_source_versions(source_id, repository_id, blob_oid, path, commit_oid)
logical_sources gains (repository_id, commit_oid, git_path) with a partial
UNIQUE index: git logical identity is per (repository, commit, path).
ingest_sessions.scope gains 'git_repository'.
```

An assertion is the stable interpretation of a piece of evidence; a memory is
one belief episode of that interpretation, so several memory rows may share
an assertion over time (believed, retired, believed again) while at most one
of them is active. The FTS index is written exactly once per assertion.

Indexes exist for the hot lookups: assertions by normalized subject, memories
by state, memory_spaces by space, sources by location.

## 4. Database location

The default database is `$XDG_DATA_HOME/mindspace/mind.db`, falling back to
`$HOME/.local/share/mindspace/mind.db`. Overrides, in precedence order:

1. `--db <PATH>` global CLI flag
2. `MINDSPACE_DB` environment variable

Any command other than `init` on a database that has not been initialized
fails with an actionable error. `mind init` is idempotent.

## 5. Local operation now, remote later

v0 is exactly:

```text
mind CLI -> engine -> SQLite file
```

The intended future shape is documented in the README: a `mind serve` process
wrapping the same engine, with CLI/client/API adapters in front. The seam
already exists: every engine operation is expressed against the `Store`
trait, and the CLI layer is a thin renderer. What v0 deliberately does *not*
build: HTTP transport, TLS, authentication, daemonization. No abstraction was
added "for the server"; the server will need none of them changed.

## 6. Error handling

One error enum (`core::Error`, thiserror) for the whole engine, including
storage errors wrapped underneath. Errors are actionable by contract: they
name the offending object and, where a fix is a command, spell the command
out (e.g. "space 'coding' does not exist; run: mind space create coding").
The CLI prints `error: <message>` and exits non-zero. Usage errors are clap's.

Memory IDs are accepted as full UUIDs or unambiguous prefixes (minimum 4
characters); ambiguous prefixes fail with the candidate list.

## 7. Output

Human output is plain text with stable, grep-friendly lines. Every
informational command also accepts `--json`, serializing the exact structs the
engine returns (timestamps as RFC 3339). The JSON is a projection of the same
data, not a separate formatting path, so scripts and humans can never diverge.

## 8. Testing strategy

- **Unit tests** cover deterministic parsing (markdown sectioning, heading
  paths, code fences, setext headings, subject/title rules) and traversal.
- **Integration tests** run the engine against temporary SQLite databases and
  reopen them between phases to exercise real persistence across process
  boundaries. They assert the domain invariants from the README/task: idempotent
  ingest, provenance chains, multi-space membership, directed traversal,
  cyclic links, superseded knowledge remaining inspectable, coexisting
  conflicts, retirement semantics, and immutability triggers rejecting raw
  UPDATEs.
- **End-to-end tests** invoke the compiled `mind` binary
  (`CARGO_BIN_EXE_mind`) with an isolated `MINDSPACE_DB`, covering init ->
  spaces -> ingest -> search -> trace exactly as a user would.

Tests assert behavior through public engine/CLI surfaces, not table layouts.
