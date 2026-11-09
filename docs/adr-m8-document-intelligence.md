# ADR-M8: Document intelligence architecture

Status: **proposed** (M8.0 design gate; amended by M8.0a, awaiting review). Date: 2026-11-09. Base: `main` = `92f68ff`.
Full reasoning and evidence: `design-m8-document-intelligence.md`; experiments: `experiments/m8/`.

## Decision

1. **A format-independent document extraction model** (nodes, canonical text, anchors, origin-tagged attributes, scoped classifications, evidence-backed relations), stored in new immutable `doc_*` side tables keyed by source content and extractor identity, following the `code_*` derivation precedent. Retrievable **chunks are projected into the existing assertion spine**, so FTS, embeddings, hybrid fusion, temporal history and memory lifecycle are reused unchanged.
2. **Anchors state their own precision** (`byte_exact | node_exact | page_only | none`) and embed the content hash and extractor identity; PDF geometry (page, bbox) is stored separately from the semantic hierarchy.
3. **Classification is evidence-scored, multi-label, per scope, and may abstain** (`classified | ambiguous | unknown | not_attempted`). Tiers: deterministic signals and rules (required), embedding similarity (optional), local classifier/LLM (optional, explicit opt-in, never authoritative). Filename, extension and the document's own metadata are signals about themselves, never proof of type.
4. **Binary and markup formats other than Markdown/text (DOCX, PDF, HTML) are parsed only in the confined document worker** (new operation kind on the existing broker; same root authorization, cgroup, commit lease; no executable, no network). The receiver validates the stream and recomputes hashes and invariants.
5. **Implementation order** (slices S1-S10 of the design doc, section 11; the milestone names are the canonical Plane M8.0-M8.8): model + Markdown/text + anchors + chunks + deterministic classification (no new parser dependency) -> worker plumbing -> DOCX (`zip` + `roxmltree`) -> PDF after a bake-off -> HTML (`html5ever` + `markup5ever_rcdom`). No OCR in the first slices.
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

* A schema migration (`0010`) and a new `Store` surface are required in slice S1; no change to Git ingestion, grants or MCP defaults.
* Retrieval of documents gets chunking for the first time; chunk size is bounded by the embedding model's 2,048-token silent truncation.
* PDF support is gated on a measured parser bake-off; until then the product claims only Markdown, text and (after M8.5) DOCX.
* Acceptance thresholds for classification are provisional (Assumed) until the S4 baseline (M8.7) exists.

## Revisit when

* A real PDF corpus shows neither candidate parser meets the structure/safety budget (then: page-only anchors or a different parser, decided in S7).
* Memory-limit enforcement in the document worker proves insufficient (then: per-stream inflate caps inside a vendored/forked reader).
* The Plane M8 acceptance criteria, once read, contradict this scope.

## Amendment M8.0a (2026-11-09): decisions added after reconciliation

7. **Physical format is detected from content** (versioned `detector_version`); the extension is a recorded hint that may only choose a text interpretation and never overrides a binary detection; polyglots are refused (`ambiguous_format`).
8. **Source retention default:** the canonical text is always retained (as `sources.content` already is for text formats); original bytes are **not** retained by default; `retention = blob` is an explicit opt-in into a content-addressed `doc_blobs` table. A hash is an identity, never a locator. Citation resolution returns an explicit status (`text_resolved`, `bytes_resolved`, `source_present_verified`, `source_changed`, `source_missing`, `source_unavailable`). A sidecar blob directory is rejected.
9. **The extraction key includes the original-bytes hash** (`source_bytes_hash`), and anchors embed it, because equal canonical text does not imply equal geometry.
10. **The existing memory lifecycle is reused unchanged.** Document chunks are assertions reconciled by `reconcile_logical` under their own immutable producer family (`doc:chunk@1`); supersession, retirement, as-of search and history are the existing ones. A second lifecycle mechanism is rejected because the three-state machine already expresses every needed transition. Existing Markdown/text ingestion is **not** re-routed through the new chunker without a reviewed migration (two active families per file would result).
11. **Resource limits are chosen by the broker per operation kind**, never by the request (the request vocabulary stays closed). Production currently sets no worker memory limit; document operations require a cgroup `memory.max` or, failing that, `RLIMIT_AS`, else binary formats are refused (`limits_unavailable`).
12. **Chunk budget is a deterministic 3,000-byte limit** (independent of embedding availability); embedding truncation must become visible before chunks are embedded (prerequisite of M8.5).
13. **Quality thresholds stay provisional** until the first measured baseline (S4) is recorded and frozen in a reviewed change.

### Additional rejected alternatives (M8.0a)

| Alternative | Why rejected |
|---|---|
| L. Trust the filename extension to route to a parser | content decides; a `.md` file containing `%PDF-` must not reach the Markdown path as text, and conversely |
| M. "First match wins" for polyglot files | PDF readers and ZIP readers disagree on such files; it is an evasion route, so the file is refused |
| N. Store original binaries by default | privacy and size cost; text citations already resolve from the canonical text; opt-in covers users who need byte-level guarantees |
| O. Rely on a hash as the citation locator | a hash identifies bytes, it cannot retrieve them; statuses make the difference explicit |
| P. A new document lifecycle / state machine | the existing active/superseded/retired lifecycle with a producer family covers revisions, removal and as-of queries (design 9.2) |
| Q. Detect scanned or encrypted PDFs by "empty text" | verified unsafe: both parsers return empty text without error for both cases; explicit `is_encrypted` and per-page image/text checks are used |
| R. Let the frontend send resource limits with the request | would widen the broker's authority surface; limits are broker policy per operation kind |
