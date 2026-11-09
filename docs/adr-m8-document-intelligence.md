# ADR-M8: Document intelligence architecture

Status: **proposed** (M8.0 design gate, awaiting review). Date: 2026-11-09. Base: `main` = `92f68ff`.
Full reasoning and evidence: `design-m8-document-intelligence.md`; experiments: `experiments/m8/`.

## Decision

1. **A format-independent document extraction model** (nodes, canonical text, anchors, origin-tagged attributes, scoped classifications, evidence-backed relations), stored in new immutable `doc_*` side tables keyed by source content and extractor identity, following the `code_*` derivation precedent. Retrievable **chunks are projected into the existing assertion spine**, so FTS, embeddings, hybrid fusion, temporal history and memory lifecycle are reused unchanged.
2. **Anchors state their own precision** (`byte_exact | node_exact | page_only | none`) and embed the content hash and extractor identity; PDF geometry (page, bbox) is stored separately from the semantic hierarchy.
3. **Classification is evidence-scored, multi-label, per scope, and may abstain** (`classified | ambiguous | unknown | not_attempted`). Tiers: deterministic signals and rules (required), embedding similarity (optional), local classifier/LLM (optional, explicit opt-in, never authoritative). Filename, extension and the document's own metadata are signals about themselves, never proof of type.
4. **Binary and markup formats other than Markdown/text (DOCX, PDF, HTML) are parsed only in the confined document worker** (new operation kind on the existing broker; same root authorization, cgroup, commit lease; no executable, no network). The receiver validates the stream and recomputes hashes and invariants.
5. **Vertical slice order:** model + Markdown/text + deterministic classification (no new parser dependency) -> worker plumbing -> DOCX (`zip` + `roxmltree`) -> PDF after a bake-off -> HTML (`html5ever` + `markup5ever_rcdom`). No OCR in the first slices.
6. **No new MCP tool or authority in M8.0.** A later surface must be hidden/opt-in with a closed schema, like the isolated ingestion tools.

## Rejected alternatives

| Alternative | Why rejected |
|---|---|
| A. Extract text only and store it as today's `document`/`section` evidence | Loses hierarchy, tables, pages, geometry and type; evidence has line ranges only, so no byte/page anchors; nothing to attach classification or origin to |
| B. Put every extracted fact (clauses, parties, line items) into memories/assertions | Floods the belief spine with derived structure; couples parser versions to memory lifecycle; the code-structure precedent deliberately keeps syntax facts out of it |
| C. Parse PDFs/DOCX in-process | Verified: a 195 KB PDF inflates to ~400 MB RSS in both PDF crates tried, with no error; parser panics and memory use would take down the server |
| D. Shell out to `pdftotext`/LibreOffice/`pandoc` | Requires exec (the confined workers deliberately execute nothing), availability not guaranteed offline, larger attack surface, opaque offsets |
| E. Make a local LLM/SLM mandatory for classification | Violates the deterministic/local-first posture, makes ingestion slow, unreproducible and heavy; cannot be audited per signal |
| F. A single closed enum of document types in the schema | Types are data that must grow (and be versioned) without migrations; mixed and ambiguous documents need multi-label per scope |
| G. Use `scraper` for HTML | Its dependency tree contains four MPL-2.0 crates, which `deny.toml` rejects (verified); `html5ever` + `markup5ever_rcdom` is licence-clean |
| H. Trust filename/extension/`docProps`/PDF Info title for type | Verified misleading in the probe (PDF title "Invoice", body is a paper); also trivially forgeable |
| I. Store original binaries in `sources.content` | `content` is TEXT under immutability triggers and is hashed as text; anchors must resolve after the file moves, which the canonical text guarantees |
| J. Start with PDF/OCR | The model, anchors and classifier must be proven on formats with exact offsets first; PDF parser choice is unresolved (no layout in `lopdf`, young `pdf_oxide`); OCR adds a large, non-deterministic dependency |
| K. Infer relations (supersedes, cites, duplicates) during ingestion | Invents facts: relations require stored evidence and are created by explicit steps only |

## Consequences

* A schema migration (`0010`) and a new `Store` surface are required in M8.1; no change to Git ingestion, grants or MCP defaults.
* Retrieval of documents gets chunking for the first time; chunk size is bounded by the embedding model's 2,048-token silent truncation.
* PDF support is gated on a measured parser bake-off; until then the product claims only Markdown, text and (after M8.5) DOCX.
* Acceptance thresholds for classification are provisional (Assumed) until the M8.3 baseline exists.

## Revisit when

* A real PDF corpus shows neither candidate parser meets the structure/safety budget (then: page-only anchors or a different parser, decided in M8.6).
* Memory-limit enforcement in the document worker proves insufficient (then: per-stream inflate caps inside a vendored/forked reader).
* The Plane M8 acceptance criteria, once read, contradict this scope.
