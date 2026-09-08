# Mindspace

> Local-first, inspectable, durable memory infrastructure for humans, tools, and agents.

Mindspace is an early-stage memory system built around a simple idea: durable memory should be more than disconnected text chunks and embeddings.

It should preserve where knowledge came from, distinguish evidence from interpretation, understand that knowledge changes over time, keep different knowledge domains separate without turning them into isolated silos, and remain useful without requiring an AI model.

The project is currently in the architecture and experimentation phase. The design below is intentionally a starting point rather than a compatibility promise.

## Design principles

### Local first

A useful Mindspace should work on one machine with minimal setup. SQLite is the intended default store and the core system should not require a cloud service.

The same engine should also be able to run as a network service and be accessed through a configured URL or IP address. Remote deployment must be an extension of the local model, not a separate product architecture.

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

Links may eventually support directionality, explicit import/export behavior, and configurable transitivity.

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
           CLI     API     future MCP
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
mind space list
mind space create eragon
mind space link coding linux
mind search "Wayland protocol" [--space coding] [--at RFC3339] [--include-superseded] [--include-retired] [--json]
mind inspect <memory> [--json]
mind trace <memory> [--json]
mind history <subject words...> [--json]
mind conflicts [--json]
mind retire <memory> --reason "why"
```

`--json` on informational commands prints the exact engine structs, so
scripts and humans see the same data. Treat the JSON shape as unstable
pre-alpha output: it may change without notice while the model settles.
Every command honors a global `--db`
flag (or `MINDSPACE_DB`); the default database lives under
`$XDG_DATA_HOME/mindspace/mind.db`.

Intended but not built yet:

```sh
mind recall "why did we choose Slint?"
mind space use project:owlry
mind serve
```

Future interfaces may include an HTTP API, SDKs, and MCP. They should remain adapters over the same core rather than defining the memory architecture themselves.

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
  ingest/      deterministic markdown/text parsing and ingest orchestration
  retrieval/   search orchestration, directed space traversal
  cli/         clap definitions and human/JSON rendering

docs/
  architecture.md
  memory-model.md
  decisions.md

migrations/
  0001_init.sql
tests/
  invariants.rs   domain invariants against real SQLite files
  cli.rs          end-to-end runs of the mind binary
```

## Non-goals for the first implementation

The first implementation does not need to:

- support every database
- require or bundle an LLM
- require embeddings
- implement MCP immediately
- solve every document format
- automatically infer every relationship
- become a hosted cloud product
- prematurely optimize distributed operation

The first job is to establish a durable and inspectable memory model that works locally.

## Status

**v0 foundation implemented.**

The core data model (sources -> evidence -> assertions -> memories, spaces,
temporal state, provenance) is specified in `docs/memory-model.md`, built,
and tested. What works today:

- deterministic ingest of markdown and plain text into a local SQLite
  database (bundled, FTS5 full-text search)
- idempotent re-ingestion with exact reconciliation semantics: reuse,
  supersession, retirement, space completion
- spaces as first-class domains with directed links, cycle-safe traversal,
  and conflict listing
- temporal queries (`search --at`) over the stored timeline
- full provenance chains (`inspect`, `trace`) that never lose history:
  superseded and retired memories stay inspectable forever
- a CLI (`mind`) with human and `--json` output

Explicitly deferred: AI extraction, embeddings, sync, auth, remote or hosted
operation, PostgreSQL implementation, GUI/TUI. See `docs/decisions.md` for
the reasoning behind each choice.
