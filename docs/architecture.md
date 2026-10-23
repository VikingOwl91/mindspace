# Mindspace architecture (v0.5)

This document describes how the v0.5 implementation is built: module boundaries, storage
design, and the seams deliberately left for later growth. The conceptual model
lives in `docs/memory-model.md`; the reasoning behind individual choices lives
in `docs/decisions.md`.

## 1. Shape

```text
mind CLI / mind mcp
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

One crate, one binary (`mind`), no daemon or network service, and no runtime
dependency on anything outside the machine. The optional `mind mcp` child
process is local stdio only. Everything runs synchronously; there is no async
runtime because nothing in v0.5 benefits from one.

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
  embeddings/  trusted manifest and model management; the Candle CPU runtime
                is optional behind the `embeddings` Cargo feature.
  indexing/    bounded, incremental backfill of assertion embeddings through
                the embedding runtime and Store boundary.
  retrieval/   lexical search orchestration, directed space traversal (BFS,
                cycle-safe), and the mode-aware semantic/hybrid retrieval API.
  mcp/         synchronous JSON-RPC 2.0 / MCP stdio lifecycle and tool adapter.
                Depends on core, retrieval, and Store; never SQL.
  cli/         clap definitions and human/JSON rendering. Knows the engine,
                never SQL.
  lib.rs       engine assembly: opening a store, running migrations.
  main.rs      CLI entry.
```

Boundary rules enforced by review and by `use` statements:

- `core` never imports `storage`, `ingest`, `retrieval`, or `cli`.
- Only `storage` knows SQLite. `rusqlite` types do not appear outside it.
- `ingest`, `retrieval`, `mcp`, and `cli` call the `Store` boundary; none
  depends on SQLite implementation details.
- The CLI and MCP server are adapters over the same engine and storage
  contract. MCP tool handlers do not implement persistence themselves.

## 2. The storage boundary

`storage::Store` is a trait whose methods correspond to use cases
(create/list/link spaces, `record_observation`, `get_memory_detail`,
`search`, `list_conflicts`, `retire_memory`, ...), not to tables. All
signatures traffic in owned `core` types.

v0.5 ships one implementation, `storage::sqlite::SqliteStore`. A PostgreSQL
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

### Retrieval modes

`retrieval::search_with_mode` takes one of four modes. `Lexical` is the default
and is what `retrieval::search` runs. It never touches embeddings.

The library API remains available through `retrieval::search_with_mode`. The
CLI `search` and `recall` commands and MCP `memory_search` also accept an
optional mode. If the option is omitted, each adapter calls the original
`retrieval::search` path and preserves its existing output shape. Explicit
`lexical` uses the mode-aware outcome but does not resolve semantic
capability. Explicit `semantic`, `hybrid`, and `auto` check current capability
readiness once per request. MCP can reuse a session-owned verified runtime
under the integrity policy below.

- `Lexical`: FTS5 ranking, unchanged.
- `Semantic`: the query goes through the runtime's query path and is matched
  against vectors under the active derivation key. Fails when unavailable.
- `Hybrid`: both branches run over one expanded scope and are fused with
  reciprocal rank fusion, `score = sum(1 / (k + rank))`, with k = 60 by default.
  Ranks are positions in each assertion-deduplicated candidate pool. Fails when
  unavailable; if no semantic candidate survives the query filters, it reports
  lexical results and a `NoSemanticCandidates` fallback.
- `Auto`: `Hybrid` when the capability is ready and the vector branch returns
  candidates. Otherwise `Lexical`, with the reason in `fallback`.

Mode-aware JSON uses the same structured outcome in the CLI and MCP:
`requested`, `effective`, `fallback`, `lexical_candidates`,
`semantic_candidates`, and `hits`. Mode and score-kind names are stable
lowercase strings. A fallback is `null` or an object tagged by `kind`, with
variant details such as `model` or `detail` where relevant. Each hit contains
the existing serialized hit as `hit`, its `score_kind`, and branch `provenance`
(ranks and branch scores; absent values serialize as `null`). The MCP
`structuredContent` contains this outcome directly. The CLI `--json` value is
the outcome object. Without an explicit mode the CLI JSON remains the original
array and MCP remains `{ "hits": [...] }`.
Recall still displays the highest-ranked stored memory and related evidence;
it does not generate an answer.

The outcome names the mode that served the results (`effective`) and why a
requested `Auto` or `Hybrid` search was served lexically (`fallback`). A hit's
`score` is meaningful only with its `score_kind`. Lexical relevance, cosine
similarity, and fused values are never compared with each other. After mode
planning succeeds, a zero limit returns `EffectiveMode::NotRun` without running
a retrieval branch or reporting a fallback. The CLI and MCP still resolve
capability once for an explicit non-lexical request before calling the mode
planner, including Auto with a zero limit. MCP may reuse a validated warm
capability; a cold zero-limit request never publishes a cache entry.
Unavailable Semantic and Hybrid
requests fail during planning even with a zero limit. Positive-limit Auto
requests report capability or candidate fallback reasons even when no lexical
hits were found. Auto with a zero limit still pays the capability-resolution
cost although its `NotRun` outcome does not use the result. Readiness checks
are not skipped; warm verification/runtime reuse follows ADR-020.
Candidate counts describe branch inputs, not final hits. Fusion counts
assertions after deduplication; a `NoSemanticCandidates` lexical fallback counts
the memory episodes fetched from the candidate pool, which may exceed `limit`.

Search never downloads, installs, or indexes. Capability resolution reads
metadata, checks for vectors under the active derivation key, verifies each
manifest artifact with a full SHA-256 pass, and constructs a lazy runtime. The
current Nomic manifest artifacts total 547,649,564 bytes (546,938,168-byte
weights file and 711,396-byte tokenizer). Missing enablement, model selection,
or a compatible index returns before hashing. CLI invocations and explicit
library resolution remain uncached.

The synchronous MCP loop owns a single-entry runtime cache (ADR-020). Each
non-lexical request rereads readiness and structural checks. On Linux/btrfs,
a full trusted-manifest SHA pass and a successful query can publish a loaded
runtime only when pre/post artifact and directory identities match. Artifact
ctimes must already be at least two seconds old before verification. Warm
reuse checks the complete manifest, paths, device/inode/mode/size and
nanosecond mtime/ctime; it does not infer integrity from size or mtime alone.
Unknown filesystems, invalid timestamps, observed clock rollback and errors
retain the full-verification path. Configuration, index unavailability,
artifact changes and query-embedding errors evict the entry. Query results
and vectors are not cached.

The runtime owns loaded tensors, so subsequent disk writes cannot mutate
weights already in memory. A change after a warm check can leave that request
using the previous verified runtime; the next observed change invalidates it.
Pre/post checks are not atomic snapshots or a defense against hostile local
writers, privileged metadata manipulation, filesystem rollback or concealed
clock excursions. Other platforms and filesystems retain uncached behavior.
See [retrieval-performance.md](retrieval-performance.md) for the precise
freshness boundary, memory tradeoff and release measurements.
Both branches apply the same space, lifecycle, and temporal filters because they
share one expanded query. Candidate pools default to
`max(limit, clamp(4 * limit, 20, 200))`.

The query prefix (`search_query: `) is applied by the runtime and pinned by the
manifest entry. It is not part of the derivation key. Changing it therefore needs
a manifest change that alters a key input, such as the version, followed by a
re-index.

Known limits: the vector branch has no approximate index and scans every
filter-qualified vector. Malformed stored vectors are skipped silently, as before.
Legacy lexical and Semantic results remain episode-level. Hybrid and Auto fusion
deduplicate each filtered candidate pool by assertion identity and retain the
first ranked eligible episode as the representative. If the semantic pool is
empty, the lexical fallback stays episode-level. The SQL pool limit is applied
before fusion deduplication, so repeated historical episodes can reduce the
number of distinct assertions available to fusion.

## 3. SQLite specifics

- `rusqlite` with the `bundled` feature: a static SQLite with FTS5 compiled
  in, so behavior does not depend on the system's libsqlite3 build flags.
- Foreign keys are enabled on every connection (`PRAGMA foreign_keys = ON`).
- Migrations are embedded SQL files under `migrations/`, applied in order and
  tracked in `schema_migrations(version)`. Migrations run on every open; they
  skip already applied versions. `mind init` additionally creates the database
  file and parent directories.
- Connections enable WAL and a five-second SQLite busy timeout. Write
  transactions use `BEGIN IMMEDIATE`, so lock acquisition happens before
  dependent reads and multi-row writes stay atomic. SQLite busy/locked errors
  are surfaced as database-busy errors. There is no application-level retry;
  this avoids replaying a write whose commit/rollback outcome is uncertain.
- `BEFORE UPDATE` triggers on `sources`, `evidence`, and `assertions` abort
  any mutation, making immutability a property of the store, not a habit of
  the code (memory-model section 2).
- The FTS5 index `assertion_fts(assertion_id UNINDEXED, subject, content)` is
  written once per assertion and never mutated, because assertions are
  immutable. Superseded/retired knowledge stays searchable through explicit
  flags; state filtering happens at query time by joining to `memories`.
- Every state-changing store operation is transaction-scoped; multi-table
  writes commit atomically. `BEGIN IMMEDIATE` serializes writers across
  processes while WAL permits concurrent readers.
- Migration `0006_manual_writes.sql` enforces a single manual logical source
  identity at `manual://writes` without changing existing rows.

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

## 5. Local MCP operation; remote later

The current process shape is:

```text
Codex / OpenCode / other MCP client
            |
            | JSON-RPC 2.0, newline-delimited stdio
            v
       mind mcp -> engine -> Store -> SQLite file
```

`mind mcp` is a synchronous child process using MCP version `2025-06-18`. It
implements initialization, initialized notification, ping, tool listing and
calls, then exits on stdin EOF. The stdio transport accepts and emits one
newline-delimited JSON-RPC message per line; stdout contains only protocol
messages and diagnostics stay on stderr. Tool success returns both
`structuredContent` and serialized text for compatibility. Output schemas are
omitted until every response shape is contractually fixed. Tool execution
failures return MCP tool results with `isError: true`; malformed requests,
unknown methods, and unknown tool names are JSON-RPC errors (`-32700`, `-32601`,
and `-32602` respectively).

The server has no HTTP transport, authentication, daemon mode, or remote
operation. Anyone able to launch or communicate with the local process can
invoke its read and write tools. Client permissions and confirmation prompts
are part of the trust boundary. Space links are retrieval imports, not
authorization. Future remote interfaces require separate transport and
security decisions.

The eleven tools are `memory_create`, `memory_update`, `memory_retire`,
`memory_get`, `memory_search`, `memory_history`, `memory_conflicts`,
`memory_add_spaces`, `space_create`, `space_list`, and `space_link`. Manual
creation and update use the same `Store` boundary as the CLI and ingestion;
details and write restrictions are specified in `docs/memory-model.md`.

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

Code structure is a parallel provenance layer: `ingest/code` owns Tree-sitter
adapters and normalizes their output before it crosses into `core`. SQLite
stores immutable code inputs, versioned derivations, entities, imports, and
path/commit contexts. It does not store Tree-sitter values or raw AST JSON.
These records never enter Evidence, Assertion, Memory, FTS, or recall.

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
- **MCP tests** invoke the real `mind mcp` binary over piped stdio for
  initialization, discovery, tool lifecycle, malformed input, request/tool
  errors, notifications, stdout purity, EOF shutdown, and uninitialized DB
  behavior. Independent server processes exercise idempotent concurrent
  writes and bounded lock contention against one SQLite file.

Tests assert behavior through public engine/CLI surfaces, not table layouts.
