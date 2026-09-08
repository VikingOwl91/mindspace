# Export and multi-store composition

Status: design direction / future work. This document intentionally records semantics before implementation. It is not a compatibility promise and does not move these features ahead of the current source-lifecycle work.

## Motivation

Mindspace should not become a closed database that can only be consumed through one client or protocol. A useful memory system should make knowledge portable, publishable, composable, and independently inspectable while preserving provenance.

Two related capabilities follow from that goal:

1. **Scoped export**: project an explicitly selected part of a Mindspace into a chosen output format.
2. **Multi-store views**: query knowledge from several local and remote Mindspace stores as one logical view without implicitly copying or merging those stores.

These capabilities are generic memory infrastructure. They are not MCP features, LLM features, code-specific features, or hosted-product features.

## 1. Export is a core capability

Export belongs in the Mindspace engine. CLI, HTTP, SDK, MCP, web applications, and other interfaces should only expose the same export operation through different transports.

```text
                 Mindspace Core
                      |
                 Export Engine
                 /     |     \
             SQLite  JSONL   Markdown
                ^       ^       ^
                |       |       |
              CLI      HTTP    SDK / other adapters
```

A website should eventually be able to expose a button such as "Export Eragon dataset" by invoking the same operation as `mind export` on a server.

## 2. Selection and serialization are separate

An export has two independent concerns:

- **selection**: which knowledge belongs in the export;
- **serialization**: how that selected knowledge is represented.

Candidate selectors include:

- all knowledge;
- one or more spaces;
- one or more sources;
- subjects or memories;
- current knowledge only;
- historical knowledge;
- time ranges;
- a configured multi-store view.

Candidate formats include:

- Mindspace SQLite;
- JSON;
- JSONL / streaming JSON;
- Markdown / human-readable reports;
- additional interoperable formats where they prove useful.

Example future CLI shapes:

```sh
mind export --space project:owlry --format json
mind export --space eragon --format sqlite --output eragon.mind.db
mind export --space eragon --current --format md
mind export --since 2026-01-01 --format jsonl
```

The exact CLI is deliberately unspecified for now.

## 3. Provenance closure

A portable export should not accidentally sever the explanation for exported knowledge.

Selecting a Memory normally implies the provenance closure needed to understand it:

```text
Memory
  -> Assertion
  -> Evidence
  -> Source
```

Related space metadata, temporal state, supersession lineage, and other required references must also be included when the chosen export policy requires them.

Possible provenance policies:

- **full**: preserve the complete available provenance necessary for independent inspection;
- **referenced**: preserve provenance metadata/references while omitting material that policy does not allow or the caller does not request;
- **knowledge-only**: deliberately emit a lossy projection of resulting knowledge.

The safe default should favor inspectability. Loss of provenance should be explicit.

## 4. Native exports and human projections

Not every format has the same fidelity.

A native SQLite export should aim to be a self-contained valid subset of Mindspace that can retain IDs, provenance, history, spaces, and relationships. It should be suitable for archival, transfer, import, and reproducible research.

JSON/JSONL should provide interoperable structured representations with an explicit export/schema version. JSONL is important for streaming large datasets without requiring the entire export in memory.

Markdown is a human projection rather than a lossless backup format. It may present current knowledge, history, conflicts, and source references in a readable form without pretending to encode every database invariant.

## 5. Publishing datasets

Scoped export should make a Mindspace usable as a dataset publisher.

A research or analytics site could expose:

```text
Eragon Deep-Dive Analytics

Download:
  [Mindspace SQLite] [JSONL] [JSON] [Markdown]

  [Export custom subset]
```

The website does not need to maintain a separate dataset implementation. A Mindspace server can compute the requested selection and serialize it in the requested format.

This also enables reproducible snapshots. A publication can identify the exact dataset revision/hash and selection from which an analysis was produced, and readers can download that same snapshot later.

A consumer does not need Mindspace to use an interoperable export. A Mindspace-native export can additionally support future workflows such as:

```sh
mind import eragon.mind.db
```

Import semantics are a separate design problem and are not specified here.

## 6. Multiple stores, one view

A Mindspace client should eventually be able to compose knowledge from multiple independent stores without merging them into one database.

```text
                    Mindspace View
                         |
          +--------------+--------------+
          |              |              |
        local          team          public
         RW             RW              RO
          |              |              |
          +--------------+--------------+
                         |
                    retrieval
```

Examples include:

- a local private store;
- a home-server Mindspace shared across the user's devices;
- a collaborative team/research store;
- one or more public read-only datasets;
- separately administered remote Mindspace servers.

Remote knowledge being visible in a view must **not** mean that it has been copied into or adopted by the local store.

Reading is not importing.

Explicit future operations may support import, adoption, copying, forking, or publication, but those semantics must never happen as a side effect of search.

## 7. Store, space, scope, access, and view are orthogonal

These concepts must remain separate:

- **Store**: where knowledge physically/logically lives and which engine owns its durable state.
- **Space**: which knowledge domain it belongs to or is useful within.
- **Scope**: where information originated, happened, or applies (repository, document, conversation, session, project, etc.).
- **Access**: who or what may read, write, administer, or redistribute it.
- **View**: which stores/spaces a client composes for a particular retrieval context.

For example:

```text
Store: public-eragon
Space: eragon:characters
Scope: source:inheritance-cycle
Access: read-only

Store: local
Space: eragon:notes
Scope: research-session:42
Access: private
```

Space links remain knowledge-import semantics only. They must never be treated as authorization boundaries.

## 8. Store-qualified identity and provenance

Objects from different stores must not become ambiguous merely because their local IDs happen to match.

At composition boundaries, identity is effectively store-qualified:

```text
(store_id, object_id)
```

Search results and traces across a view must preserve the owning store as part of their provenance. A client should always be able to tell whether a result came from local, team, or public knowledge.

A future protocol may use globally stable store identities or canonical object references; the important invariant is that cross-store composition cannot silently collapse identities.

## 9. Querying composed views

A configured view may define the stores normally queried together. Conceptually:

```sh
mind search "Brom"                    # current view
mind search "Brom" --store local
mind search "Brom" --store eragon-public
mind search "Brom" --space eragon
```

A view can include both read-only and read/write stores. Query fan-out and result merging must remain deterministic enough to explain why a result appeared and where it came from.

Writes need an explicit or unambiguous target. A multi-store read view must never imply that writes are broadcast to every participating store.

A possible model is a configured default write target with an explicit override, but the exact UX is deferred.

## 10. Collaboration without mandatory database synchronization

Multi-store composition provides a simple collaboration model before introducing synchronization, CRDTs, federation, or replicated databases.

Several people can each have a private local store while sharing one central read/write project store:

```text
Chris local ----+
                |
Matze local ----+----> shared project Mindspace
                |
other local ----+
```

Each person's view can be `local + shared + public datasets`. Personal data remains local; shared research is written to the shared store; public datasets remain read-only references.

This is deliberately simpler than automatically merging every participant's database.

## 11. Export from composed views

Export and multi-store composition intersect, but visibility does not automatically grant redistribution rights.

A composed view may contain stores with different export policies, licenses, or access restrictions. The export engine must respect those boundaries.

Conceptually a store/dataset may eventually expose policy metadata such as:

```text
local:             export allowed
public dataset:    export allowed with attribution
licensed source:   redistribution prohibited
team:              members only
```

These examples are illustrative, not a policy schema.

An export over a view must either omit disallowed material, retain permitted references instead of content, or fail clearly when the requested export cannot be produced without violating the applicable policy. It must never silently republish everything merely because the caller could retrieve it.

## 12. API and transport

Multi-store composition should not make MCP, HTTP, or any other protocol architectural dependencies.

The intended layering remains:

```text
                    clients
        CLI / Web / SDK / MCP / scripts
                       |
                  API / adapter
                       |
                Mindspace engine
                       |
                store connection(s)
                 /      |       \
              local   remote   remote
```

A local store may be an in-process SQLite-backed engine. A remote store may be reached through a Mindspace network API. From the query layer's perspective both participate in a view while retaining their own identity and capabilities.

## 13. Capability degradation

Mindspace should remain useful when optional pieces are unavailable.

- No remote server: local store still works.
- One remote store offline: behavior should be explicit and the client should know the view is incomplete; local data must not be corrupted.
- No MCP: CLI/API remain usable.
- No LLM or embeddings: export and multi-store retrieval remain valid deterministic infrastructure.
- A store lacks a capability: capability negotiation should eventually make this visible rather than pretending every backend supports every operation.

## 14. Architectural constraints to preserve now

These features do not need implementation yet, but current work should avoid making them unnecessarily difficult.

Preserve these constraints:

1. Export is a core operation; transports are adapters.
2. Selection and serialization are separate concerns.
3. Native scoped exports preserve provenance closure by default.
4. A store owns its data; querying another store does not import it.
5. Cross-store object identity is store-qualified.
6. Store, Space, Scope, Access, and View remain orthogonal concepts.
7. A composed view may read from many stores but writes have one explicit/unambiguous target.
8. Space links are not access control.
9. Export permission/redistribution policy is separate from read permission.
10. Multi-store collaboration must not require database synchronization.
11. HTTP, MCP, SDKs, and websites expose engine capabilities; they do not define them.
12. AI remains optional throughout this architecture.

## 15. Not part of the immediate implementation

Do not let this direction expand the current milestone into distributed-systems work. In particular, this document does **not** require implementing now:

- HTTP or `mind serve`;
- remote-store connections;
- authentication or authorization;
- federation;
- CRDTs or automatic synchronization;
- multi-master replication;
- import/adopt/fork semantics;
- export policy enforcement;
- public dataset hosting;
- MCP;
- LLM or embedding integration.

The immediate source-identity/lifecycle work remains the priority. This document exists so that future export, publishing, remote use, and collaboration grow from the same generic memory architecture instead of becoming special-case subsystems.
