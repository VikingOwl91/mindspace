# Mindspace

> Local-first, inspectable, durable memory infrastructure for humans, tools, and agents.

Mindspace is an early-stage memory system built around a simple idea: durable memory should be more than disconnected text chunks and embeddings.

It should preserve where knowledge came from, distinguish evidence from interpretation, understand that knowledge changes over time, keep different knowledge domains separate without turning them into isolated silos, and remain useful without requiring an AI model.

The current repository version is 0.5.0. Interfaces and serialized output remain pre-alpha and may change as the memory model develops.

## Design principles

### Local first

A useful Mindspace should work on one machine with minimal setup. SQLite is the intended default store and the core system should not require a cloud service.

The same engine may later run as a network service and be accessed through a configured URL or IP address. Remote deployment should extend the local model, not become a separate product architecture.

### AI is optional

Mindspace should remain operational without an LLM, embedding model, or external AI API.

Prefer deterministic algorithms whenever a result can be obtained reliably without a model. Parsers should parse formats, Git should provide Git facts, ASTs should describe code structure, timestamps and hashes should remain ordinary data, and lexical search should not require semantic inference.

AI can later enrich the system through extraction, summarization, semantic linking, consolidation, reranking, and other optional capabilities.

**AI should enrich memory, not own memory.**

### Sources are evidence; memories are interpretations

Mindspace should preserve the material from which knowledge was derived.

A useful conceptual flow is:

```text
Source
  -> Evidence
  -> Claim / Event / Relation
  -> Memory
```

A memory should be traceable back to its evidence. Derived information should retain provenance about how it was produced, including deterministic rules or model-assisted enrichment where applicable.

### Temporal knowledge is first-class

New information must not blindly overwrite old information.

Mindspace should be able to represent that something was previously true, is currently true, was contradicted, or was explicitly superseded. This should make questions such as these possible:

- What do we use now?
- What did we use six months ago?
- When did this decision change?
- What evidence supports the current memory?

### Avoid the global memory soup

Knowledge belongs to spaces. Example spaces might include:

```text
eragon
coding
linux
rust
wayland
personal
project:owlry
project:tyto
```

Spaces can be linked. A project space may be allowed to use knowledge from `coding`, which in turn may use `linux`, without automatically making every project memory visible from a generic Linux context.

Links are directed imports and traversal is transitive and cycle-safe. Permissions are separate from space links; links are not access-control boundaries.

A single piece of knowledge may belong to multiple spaces; spaces should not require duplicating the underlying memory.

### Provenance and conflicts stay inspectable

Mindspace should be able to answer: **Where do you know that from?**

Potential provenance includes:

- source and source location
- source version, commit, or timestamp
- ingestion time
- effective time
- extraction method
- confidence
- authority
- supersession state

Conflicting evidence should remain inspectable rather than being silently collapsed into one answer.

### Embeddings are an index, not the memory model

Semantic/vector search can be useful, but embeddings should be an optional retrieval signal rather than the canonical representation of memory.

Mindspace should be capable of combining multiple retrieval mechanisms over time, for example:

- structured queries
- lexical/full-text search
- graph traversal
- temporal filtering
- semantic/vector search
- optional reranking

In short: **Mindspace is not a vector database with an LLM attached.**

## Spaces, scopes, and memory types

These concepts are intentionally separate.

A **space** describes the knowledge domain in which a memory is useful, such as `eragon`, `coding`, or `linux`.

A **scope** describes where information originated or where it applies, such as a repository, session, conversation, document, or project.

A **memory type** describes what kind of knowledge is represented. Candidate types include:

- semantic
- episodic
- procedural
- decision
- preference
- source

The exact model is still being designed.

## Deterministic ingest

Basic ingestion should not require AI.

```text
source
  -> parse
  -> normalize
  -> preserve metadata
  -> preserve provenance
  -> identify deterministic structure
  -> store evidence
  -> deterministic extraction where possible
  -> indexes
```

Examples of deterministic structure include Markdown headings, EPUB chapters, chat messages and authors, Git commits, repository paths, source-code syntax trees, timestamps, and document metadata.

Interpretive extraction can be added later as an enrichment stage rather than being required for ingestion.

## Optional intelligence

Model-backed functionality should sit behind explicit capabilities/providers rather than leaking into the core memory model.

Possible future capabilities include:

```text
LLM
  -> claim/entity/relation extraction
  -> summarization
  -> consolidation suggestions
  -> contradiction assistance
  -> semantic linking
  -> reranking

Embedding model
  -> optional vector index
  -> semantic retrieval
```

Model-derived results must remain traceable to their inputs and should be replaceable or recomputable as extraction methods improve.

## Local and remote operation

The intended architecture uses the same memory engine in both modes:

```text
                 clients
             /      |      \
            CLI     MCP     future API
             \      |      /
              connection
              /        \
         local          remote
           |               |
       Mindspace        Mindspace
         engine           server
           |               |
        SQLite        configured store
```

A local installation should be the zero-configuration path. A remote Mindspace instance should be addressable through configuration and use normal secure transport.

Authentication is not yet specified. Candidate mechanisms include SSH public-key challenge/signature authentication, API credentials, mTLS, and OIDC. Transport security and authentication should remain separate concerns.

## Storage

SQLite is the intended default because it provides an excellent local-first deployment story.

The storage layer should nevertheless be abstracted so server deployments can use other backends. PostgreSQL is the first likely additional backend.

Structured storage, full-text indexing, and vector indexing should be treated as separate capabilities rather than assuming one database must implement every concern.

Initial direction:

```text
Mindspace engine
      |
      +-- structured store
      |     +-- SQLite (default)
      |     `-- PostgreSQL (planned)
      |
      +-- lexical/search index
      |
      `-- optional vector index
```

Additional storage adapters can be considered once the core model is stable.

## Interfaces

The working project name is **Mindspace** and the CLI command is **`mind`**.

The commands implemented in v0:

```sh
mind init
mind ingest ./notes.md --space coding
mind ingest ~/Documents --space personal   # directory: full source-lifecycle scan
mind ingest ~/src/owlry --space coding     # git repo: committed objects + ref journal
mind space list
mind space create eragon
mind space link coding linux
mind search "Wayland protocol" [--space coding] [--at RFC3339] [--include-superseded] [--include-retired] [--json]
mind recall "why did we choose Slint?" [--space coding] [--limit 5] [--json]
mind inspect <memory> [--json]
mind trace <memory> [--json]
mind history <subject words...> [--json]
mind conflicts [--json]
mind retire <memory> --reason "why"
mind mcp                                      # local MCP server over stdio
```

`--json` on informational commands prints the exact engine structs, so
scripts and humans see the same data. Treat the JSON shape as unstable
pre-alpha output: it may change without notice while the model settles.
Every command honors a global `--db`
flag (or `MINDSPACE_DB`); the default database lives under
`$XDG_DATA_HOME/mindspace/mind.db`.

The MCP server exposes explicit memory writes. It does not automatically extract memories from conversations or documents.

### MCP server

Initialize the database once with `mind init`, create the spaces you need, then run `mind mcp`. The server uses newline-delimited JSON-RPC over stdin/stdout and the MCP protocol version `2025-06-18`. Diagnostics go to stderr; stdout is reserved for protocol messages. EOF on stdin shuts the process down cleanly.

The server exposes eleven tools:

| Tool | Purpose |
| --- | --- |
| `memory_create` | Create an explicit manual memory. Requires existing spaces; the optional subject defaults to the first non-empty content line. |
| `memory_update` | Supersede an active manually created memory. Document-derived memories are rejected. |
| `memory_retire` | Retire an active memory without deleting its history. |
| `memory_get` | Read a memory and its assertion, evidence, source, spaces, and lifecycle. |
| `memory_search` | Search with SQLite FTS5, optional directed space traversal, and transaction-time filtering. |
| `memory_history` | List episodes for a normalized subject. |
| `memory_conflicts` | List active same-subject memories sharing a space. |
| `memory_add_spaces` | Add existing spaces to an active memory. |
| `space_create` | Create a space. |
| `space_list` | List spaces and active-memory counts. |
| `space_link` | Add a directed space import link. |

Manual content is stored as a `manual` source at `manual://writes`, with an immutable evidence row and a `manual:write` assertion extraction marker. The chain remains source -> evidence -> assertion -> memory, and the source kind distinguishes these writes from files. Exact duplicate subject/content writes reuse the assertion and active episode; they only add newly requested spaces. Recreating a retired manual memory makes a new episode on the existing assertion. Updates create or reuse a successor and set the old episode's supersession link. An unchanged subject/content update is a no-op apart from requested missing spaces.

Updates are restricted in storage to active memories whose source and extraction provenance are manual. `memory_update` does not rewrite an ingested file or its provenance. Change a document at its source and re-ingest it instead. If a document-derived memory is retired while its source still contains the claim, re-ingesting that unchanged source can recreate the memory.

Writes use SQLite WAL, a five-second busy timeout, and immediate transactions. Lock contention is returned as an error after the timeout; Mindspace does not blindly retry writes. SQLite transactions keep each multi-row write atomic.

The server is local-only: it has no HTTP transport or authentication. Anyone who can start or communicate with the configured process can call its tools, including persistent write tools. Review client permissions and confirmations accordingly. Space links organize retrieval; they do not enforce confidentiality. Manual content is an explicit user-authored assertion, not an independently verified fact. No confidence score is inferred.

#### Codex configuration

Codex reads MCP servers from `~/.codex/config.toml`. This local stdio example uses the documented `[mcp_servers.<name>]` table:

```toml
[mcp_servers.mindspace]
command = "mind"
args = ["--db", "/absolute/path/to/mind.db", "mcp"]
enabled = true
```

#### OpenCode configuration

OpenCode supports local MCP servers in `opencode.json` (or `opencode.jsonc`) with a `type` of `local` and an argument-array `command`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "mindspace": {
      "type": "local",
      "command": ["mind", "--db", "/absolute/path/to/mind.db", "mcp"],
      "enabled": true
    }
  }
}
```

These examples use an explicit database path. Mindspace also honors `MINDSPACE_DB` and otherwise uses `$XDG_DATA_HOME/mindspace/mind.db` (falling back to `$HOME/.local/share/mindspace/mind.db`). Configuration syntax was checked against the [Codex MCP guide](https://developers.openai.com/codex/extend/mcp/) and [OpenCode MCP server guide](https://opencode.ai/docs/mcp-servers/).

Intended but not built yet:

```sh
mind space use project:owlry
mind serve
```

Future interfaces may include an HTTP API and SDKs. They should remain adapters over the same core rather than defining the memory architecture themselves.

## Implementation direction

Rust is the current implementation choice.

v0 is built with deliberately boring components:

- clap for the CLI
- serde for serialization
- rusqlite with bundled SQLite (FTS5 guaranteed, no system dependency)
- pulldown-cmark for deterministic markdown parsing
- UUIDv7 ids and RFC 3339 timestamps at the edges
- no async runtime: nothing in v0 benefits from one; a future server can
  revisit that decision (ADR-001 in docs/decisions.md)

PostgreSQL support should be considered in architectural boundaries from the beginning, but does not need to be implemented before the memory model is proven. The `Store` trait is that boundary; v0 ships exactly one SQLite implementation behind it.

The repository stays a single crate rather than prematurely splitting every subsystem into its own crate. Stable subsystem boundaries can become workspace crates later.

The actual layout is:

```text
src/
  core/        domain types, ids/timestamps/hashes, the error type
  storage/     the Store trait and the SQLite implementation
  ingest/      deterministic document and Rust syntax parsing, ingest orchestration
  retrieval/   search orchestration, directed space traversal
  mcp/         synchronous JSON-RPC MCP protocol and tool adapter
  cli/         clap definitions and human/JSON rendering

docs/
  architecture.md
  memory-model.md
  decisions.md

migrations/
  0001_init.sql
  0002_source_lifecycle.sql
  0003_git_ingest.sql
  0004_code_structure.sql
  0005_code_relationships.sql
  0006_manual_writes.sql
tests/
  invariants.rs   domain invariants against real SQLite files
  lifecycle.rs    source identity and lifecycle scenarios
  cli.rs          end-to-end runs of the mind binary
  git_ingest.rs   Git topology and incremental-ingest scenarios
  manual_memory.rs manual provenance, deduplication, and lifecycle scenarios
  mcp.rs          real-binary protocol and process-concurrency scenarios
```

## Non-goals for the first implementation

The first implementation does not need to:

- support every database
- require or bundle an LLM
- require embeddings
- expose a network or authenticated MCP transport
- solve every document format
- automatically infer every relationship
- become a hosted cloud product
- prematurely optimize distributed operation

The first job is to establish a durable and inspectable memory model that works locally.

## Status

**v0.5.0 explicit manual memories and local MCP server implemented.**

The core data model (sources -> evidence -> assertions -> memories, spaces,
temporal state, provenance) is specified in `docs/memory-model.md`, built,
and tested. What works today:

- deterministic ingest of markdown and plain text into a local SQLite
  database (bundled, FTS5 full-text search), for single files or whole
  directory trees
- idempotent re-ingestion with exact reconciliation semantics: reuse,
  supersession, retirement, space completion
- logical source identity with full lifecycle: renames, moves, copies,
  disappearances, and reappearances are deterministic facts, not new
  knowledge; historical provenance is never rewritten
- spaces as first-class domains with directed links, cycle-safe traversal,
  and conflict listing
- temporal queries (`search --at`) over the stored timeline
- full provenance chains (`inspect`, `trace`) that never lose history:
  superseded and retired memories stay inspectable forever
- git repository ingest: committed objects (commits, trees, blobs, tags)
  as immutable graph nodes, refs as a transaction-time journal, text blobs
  at ref tips derived through the normal evidence/assertion pipeline
- Rust source structure through Tree-sitter: immutable, versioned syntax
  derivations with entities, containment, ranges, and raw imports. Syntax
  facts are not Memories; inspect them with `mind structure <path-or-blob>`.
- a CLI (`mind`) with human and `--json` output
- explicit manual memory creation, updates, retirement, retrieval, and space membership through the MCP tools; document-derived memory updates are rejected
- an eleven-tool MCP server over stdio with real-binary protocol tests
- SQLite WAL, five-second busy timeout, and `BEGIN IMMEDIATE` write transactions, with no automatic retry

What git ingest observes and what it defers: it reads committed objects and
refs only. Uncommitted working-tree content belongs to no ref and is not
modeled (a future working-tree observation would be its own session kind,
not a fake commit). Fast-forward is claimed only when provable by git's own
ancestor check; rewritten history is recorded as non-fast-forward without
guessing rebase or force-push. Same blob at two paths or two branches is one
object with separate derivations; two clones stay two repositories, qualified
by their own identity markers. See `docs/memory-model.md` section 12.

Explicitly deferred: AI extraction, embeddings, sync, authentication, HTTP or
hosted operation, PostgreSQL implementation, GUI/TUI. MCP is local stdio only.
See `docs/decisions.md` for the reasoning behind each choice.
