# Development roadmap

The published Linux x86_64 release, v0.1.0-alpha.2, includes explicit memories,
Markdown and text ingestion, committed Git content, lexical search and Rust
definitions. The sections below describe the current `main` checkout.
[Release notes](release-notes.md) · [Build from a checkout](../README.md#build-from-a-checkout)

## Repository ingest on main

| Area | What you can do |
| --- | --- |
| Text inputs | Ingest UTF-8 files up to 32 MiB through Git, directories or single files. Inspect the paths skipped for invalid text or size. |
| Code definitions | Find declarations in Rust, Go, JavaScript/JSX, TypeScript/TSX and top-level inline Svelte scripts. Git results carry the file, line range, commit and blob. Reports count unsupported selected extensions. |
| History and worktrees | Trace unique same-path, same-subject successors across observed HEAD moves. Refresh one isolated checkout while preserving the current facts for sibling worktrees. |
| Isolated Git | Read linked worktrees and local alternate object stores within authorized roots. Ingest an offline partial clone when all required objects for HEAD are present. |
| Large ingests | Set a CLI or MCP deadline and retry after a confirmed rollback. First-ingest median on the documented mixed fixture fell from 18.1 s to 7.8 s over three runs per version. |
| MCP failures | Distinguish read-only roots, missing local objects, resource limits, confirmed rollback and an uncertain commit through structured responses. |

The [CLI and MCP reference](reference.md) covers the commands and their limits.
The [benchmark](ingest-performance.md) includes the fixture, measurement method
and individual phase timings.

## Document preview

The offline preview reads trusted DOCX and PDF files independently of the
database. Section lookup returns heading paths and cited text, including
candidates when a heading repeats. The paper profile distinguishes enrollment
from analysis counts for a numbered experiment. The bestiary profile follows
supported defense rows across columns and pages. The tests use a project-authored
handbook and bestiary plus two public papers fetched from the publisher.
[Preview commands and limits](https://github.com/VikingOwl91/mindspace/blob/main/docs/document-preview.md)

## Open work

- **Language coverage.** Python, Java, C/C++, C# and shell are candidates. Each
  needs a grammar and license review, a defined set of declarations, and
  file-and-line tests. Svelte coverage currently stops at inline scripts.
- **Git coverage.** Repository ingest uses SHA-1. The isolated worker checks
  object hashes and closure. Independent authentication of the advertised HEAD
  and support for other object formats remain open.
- **Document publication (M8).** The existing reconciliation candidate was
  rejected after native SQLite reader checks. Moving forward requires an
  architecture decision and evidence that older readers and writers see
  consistent data. Canonical document and PDF publication depend on that work.
- **Knowledge graph (M9).** Typed traversal and retrieval across temporal
  evidence are planned after the document and provenance foundations.

The [M8 decision record](https://github.com/VikingOwl91/mindspace/blob/77eeff0c028c7a5ee21672377b496660182e6e91/docs/m8-s3e4d-reconciliation-planning.md#independent-disposition-and-residual-requirements)
describes the reader mismatch and the evidence needed to revisit it.
