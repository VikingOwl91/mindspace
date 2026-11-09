# M8.0 Document Intelligence: architecture and design gate

Status: **design and feasibility only.** Nothing in `src/`, `migrations/` or the MCP surface changes in this milestone. Base: `main` = `92f68ff`.
Labels: **Verified** (observed in this repository or in an experiment named here), **Reasoned** (follows from verified facts), **Assumed**, **Unknown**.
Companion files: `adr-m8-document-intelligence.md` (decision and rejected alternatives), `experiments/m8/` (parser probes and how to reproduce them).

Not available: the Plane M8 module (the local Plane instance answers `410 Gone: Local Plane has been retired`), so the roadmap's own acceptance criteria were **not read**.
Section 11 states the acceptance criteria proposed here; they must be reconciled with Plane Cloud before M8.1 starts. (Unknown.)

## 1. Summary of recommendations

1. **Model first, formats second.** Introduce a format-independent *document extraction* (nodes + text + anchors + attributed metadata + classifications + relations), keyed like the code derivations: content-addressed by source bytes and versioned by extractor identity. Do not widen `evidence`/`assertions` to carry it; project retrievable chunks into the existing assertion spine so lexical, semantic, hybrid, temporal and lifecycle features keep working unchanged.
2. **Classification is evidence-scored and may abstain.** Deterministic structural/lexical signals produce a ranked label set per scope (document or section) with explicit outcomes `classified | ambiguous | unknown`. Embeddings are an optional extra signal. A local classifier or LLM is an optional, explicitly enabled, never-authoritative layer. No generative model is required.
3. **Parsers for binary formats run only in the isolated worker.** The experiments show an unbounded in-process inflate of a 195 KB PDF costing ~400 MB RSS (section 4.3). The existing broker/worker/cgroup/stream machinery fits better than it did for Git: a document worker needs **no executable at all**.
4. **First vertical slice = the model + Markdown/plain text + deterministic classification + provenance**, with no new parser dependency (M8.1–M8.3). First binary format = **DOCX** (`zip` + `roxmltree`, 30 + 2 crates, permissive, real semantic hierarchy). PDF follows behind a parser bake-off gate (M8.6); HTML via `html5ever` + `markup5ever_rcdom`, **not** `scraper` (MPL-2.0 is rejected by `deny.toml`).
5. **No claim of source position beyond what the parser provides.** Every anchor carries an explicit precision (`byte_exact | node_exact | page_only | none`).

## 2. Reconnaissance: what exists (Verified unless noted)

| Area | Fact | Reuse / change |
|---|---|---|
| Text ingestion | `src/ingest/markdown.rs`: `pulldown-cmark` offsets -> flat `Fragment`s (preamble, one section per heading, `heading_path`, 1-based line range); `.txt` = one fragment. Format is chosen **by extension only** (`media_type_of`: `md`, `markdown`, `txt`, `rs`; anything else `UnsupportedFormat`). | Parser reusable as the Markdown front-end. Extension-only routing must become sniffing + extension hint. |
| Storage spine | `sources` (immutable, `content TEXT`, `UNIQUE(logical_id, content_hash)` after 0002) -> `evidence` (kind `document\|section`, `heading_path`, **line** range, ordinal) -> `assertions` (kind, subject, content, `extraction` rule id, unique per evidence+rule+subject+content hash) -> `memories` (belief episodes: active/superseded/retired). `assertion_fts(subject, content)`. | Chunks project here. Evidence has lines but no byte offsets/pages: a schema addition is needed (M8.1). |
| Source identity | `logical_sources` (stable id across moves), `sources` = immutable content versions, `ingest_sessions`/`source_observations`/`source_events` (moves, disappearances). | Document identity = logical source; content identity = `sources.content_hash`. Reuse as is. |
| Derivation precedent | Code uses **side tables** (`code_inputs`, `code_derivations`, `code_entities`, ...) keyed by content hash + parser identity (`mind-code@4`), with announced-known reuse over the broker context exchange (5c/5d). Syntax facts "deliberately do not enter the Evidence/Assertion/Memory spine" (comment in `core/mod.rs`). | The right precedent for structure/provenance. |
| Temporal | `m6_revisions` (one per logical write), `m6_*_history` tables, `TemporalHistoryCapabilities`; one recording revision per ingestion (verified again in 5e). | A document ingestion must be one transaction / one revision. |
| Embeddings | `assertion_embeddings` keyed `(assertion_id, derivation_key)`; default nomic-embed-text-v1.5, 768 d, `max_seq_len` 2048 with **silent truncation** (`truncate to max_seq_len`); no chunking exists anywhere. | Chunks must be sized so truncation never drops content; stale vectors are already ignored via `derivation_key`. |
| Retrieval | `SearchMode`: lexical (default), semantic, hybrid (RRF), auto. `search`, `search_with_mode*`, `search_code`. | Chunks as assertions inherit all of it. Type-aware filtering is new. |
| MCP | Tool list in `mcp/tools.rs` with a per-tool closed argument table; opt-in isolated tools (`repo_ingest_isolated`, `repo_ingest_git_isolated`) are appended only when every gate passes. | M8 adds **no tool** now; a later surface decision (M8.7) must follow the same closed-schema/opt-in pattern. |
| Isolation | Broker authorizes an exact pinned root; worker = Landlock read-only root + seccomp + no-new-privs + cgroup, record stream validated by the receiver (`Kind::Files`, `Kind::GitVerified`), commit lease, revocation ordering. Limits type: `ExtractionLimits {max_files 100k, max_content_bytes 512 MiB, max_path_bytes 4096}`. | Reusable for a `Documents` operation; **no `GitExecutable`-style trust problem** because no child process is needed. |
| Dependencies/licences | `pulldown-cmark`, `tree-sitter`, `rusqlite` (bundled), candle (optional). `deny.toml` allows MIT, Apache-2.0, BSD-2/3, ISC, Unicode-3.0, Unlicense, Zlib, BSL-1.0, 0BSD, CDLA-Permissive-2.0; **MPL-2.0 is not allowed**. | Constrains parser choice (section 4). |
| Tests/fixtures | Integration tests under `tests/`, fixtures generated in-test (temp dirs, `git init`), real-process MCP harnesses; no committed document corpus. | A generated, licence-clean corpus is needed (section 10). |
| Git-specific assumptions that must NOT transfer | HEAD-only scope, object-id identity, commit-lease on a per-repository marker, executable trust, SHA-1. | A document has no repository identity; its identity is (logical source, bytes). |

Unknown: the maximum file size the current Markdown path accepts (no explicit cap found in `ingest/`); how `ingest_path` retires memories of a *changed* file (the behaviour exists, its exact rules must be read before M8.2).

## 3. The document intelligence model

### 3.1 Separated concepts

| # | Concept | Representation | Authority |
|---|---|---|---|
| 1 | Physical file format | `format: markdown\|text\|pdf\|docx\|html\|unknown` from **magic-byte/structure sniffing**; extension is a hint recorded separately | parser-derived |
| 2 | Logical document type | classification label(s) from a versioned taxonomy | model-inferred (rules/embeddings/LLM) |
| 3 | Intended purpose | optional second classification axis (`reference`, `record`, `proposal`, `instruction`, ...) | model-inferred |
| 4 | Structural hierarchy | tree of `node`s (page, section, heading, paragraph, list, table/row/cell, figure, caption, footnote, reference, appendix) | parser-derived |
| 5 | Extracted metadata | attribute rows with **origin** | per row |
| 6 | Evidence / source location | anchors (section 6) | parser-derived, with precision |
| 7 | Classification confidence | score + basis + calibration status + outcome | model-inferred |
| 8 | Revision / temporal validity | source version (content hash), extraction version, existing m6 revisions; *document-stated* dates are metadata, not validity | store-derived |
| 9 | Relations | typed edges with evidence and origin | per row |

**Origin** is mandatory on every attribute, classification and relation: `source_stated` (the document says it, with an anchor), `parser_derived` (a deterministic function of the bytes, e.g. page count, font size), `model_inferred` (rule score, embedding, classifier or LLM, with its identity), `user_asserted` (a human, later). A `source_stated` value must carry an anchor to the text that states it, otherwise it is downgraded to `parser_derived`/`model_inferred`. Filename, extension and the document's own title/`docProps` metadata are **signals with origin `source_stated` about themselves only** ("the PDF's Title field says Invoice"), never evidence for the type. VERIFIED by experiment: the probe PDF's metadata title is "Invoice" while its body is a research paper.

### 3.2 Multiple, sectional, ambiguous, unknown

* A classification is a row *per scope*: `scope = document` or a node id. A document may hold several document-scope labels (e.g. `technical_specification` 0.62 and `proposal` 0.55) and sections may differ (a contract with an invoice attached as an appendix).
* `outcome` is explicit: `classified` (top score >= tau and margin >= delta), `ambiguous` (several labels pass tau within delta: all are stored), `unknown` (nothing passes tau: stored as an explicit row, not an absent row), `not_attempted` (limits hit, unsupported format).
* A classification is never an input to identity or to a memory's existence; it can be re-run, superseded and compared.

### 3.3 Proposed normalized schema (proposal; migration `0010` in M8.1, not created here)

All new tables are append-only/immutable like `code_*`; only lifecycle rows (memories) change.

```
doc_extractions (id PK, source_id -> sources, format, extractor_id, extractor_version, params_hash,
                 status [complete|partial|refused], refusal_reason, limits_json, text_hash, created_at,
                 UNIQUE(source_id, extractor_id, extractor_version, params_hash))
doc_texts       (extraction_id PK -> doc_extractions, text TEXT)       -- canonical NFC text; for md/txt = sources.content (no copy)
doc_nodes       (id PK, extraction_id, parent_id, ordinal, kind, depth, text_start, text_end,   -- UTF-8 byte offsets into doc_texts, nullable
                 page_no, bbox_json, anchor_precision, attrs_json)
doc_attributes  (id PK, extraction_id, node_id NULL, key, value, origin, evidence_node_id NULL,
                 evidence_start NULL, evidence_end NULL, producer_id, producer_version)
doc_classif_runs(id PK, extraction_id, classifier_id, classifier_version, taxonomy_version, params_hash, created_at, UNIQUE(...))
doc_classifications(run_id, scope_node_id NULL, label, outcome, score, basis_json, evidence_json)
doc_relations   (id PK, extraction_id, from_node, to_kind, to_ref, relation, origin, evidence_json)
doc_chunks      (id PK, extraction_id, first_node, last_node, text_start, text_end, strategy, strategy_version,
                 content_hash, assertion_id -> assertions NULL)
```

* `doc_extractions` is the unit of reuse and of the context exchange (the receiver announces known `(content_hash, extractor identity)` pairs, as 5c/5d do for code).
* Bytes of the original binary are **not** stored in `sources.content` (TEXT, and the immutability guard). `sources.content` for a binary format holds the **canonical text** and `sources.content_hash` the hash of that text; the original bytes' SHA-256 is `doc_extractions.source_bytes_hash` (add to the table). Rationale: anchors must resolve after the file moved or vanished; the canonical text always exists. An optional `--keep-original` blob store is an open question (section 12).
* `doc_relations.to_ref` may reference an existing memory/assertion only when evidence for the link is stored; no row exists "to fill the schema".

## 4. Formats and parsers

### 4.1 Candidate comparison (Reasoned from crate metadata and the probes; "V" = verified here)

| Candidate | Licence (V) | Crates in tree (V) | Structure | Source offsets / layout | Tables | Metadata | Maintenance (Unknown beyond version) | Verdict |
|---|---|---|---|---|---|---|---|---|
| Markdown: `pulldown-cmark` (in tree) | MIT | 0 new | headings, lists, tables, code | byte offsets (V, already used) | GFM tables | front matter not parsed (V) | in tree | **use** |
| Plain text | - | 0 | paragraphs by blank line | byte offsets | no | no | - | **use** |
| DOCX: `zip` 9.4 (no default features + `deflate`) + `roxmltree` 0.21 | MIT / MIT-or-Apache (V: passes `deny.toml`) | 30 + 2 (V) | paragraph styles (`Heading1/2`, `BodyText`), tables, lists via numbering (V: styles and `tbl` seen in probe) | node-exact (paragraph index); no byte offsets into the OOXML (we anchor into the canonical text) | yes (`w:tbl`) | `docProps/core.xml` (source-stated author/title/dates) | zip2: active, rust-version 1.88; roxmltree: stable | **first binary format** |
| DOCX: `docx-rs` 0.4 | MIT | not measured | writer-oriented | - | - | - | Unknown | rejected (write-focused; more code than a 2-crate reader) |
| PDF: `lopdf` 0.47 | MIT (V) | 76 (V) | none (text per page via content-stream operators) | **none** (text only; V: no layout API used; CJK split into runs, reading order = stream order) | no | Info dict | active | fallback / object-level validator |
| PDF: `pdf_oxide` 0.3.78 | MIT-or-Apache-2.0 (V) | 173 (V) | reading-order text; spans carry font size/weight, `mcid` for tagged PDFs | **per-span bbox in points** (V: `extract_spans` returned 26 spans with `Rect`) | layout heuristics (not evaluated) | Unknown | young (0.3.x), Unknown | **candidate for M8.6**, decided by bake-off |
| PDF: `pdf-extract` | MIT | not measured | text | no | no | - | Unknown | not evaluated (heavier, built on lopdf) |
| HTML: `html5ever` + `markup5ever_rcdom` 0.40 | MIT-or-Apache-2.0 (V: `deny.toml` licences ok) | 37 (V) | DOM; headings/tables/lists | node-exact; byte offsets only if the tokenizer is driven for source spans (Unknown) | yes | `<title>`, `<meta>` (source-stated) | servo project, active | **use for HTML (M8.7)** |
| HTML: `scraper` 0.29 | ISC but pulls `cssparser`, `cssparser-macros`, `dtoa-short`, `selectors` = **MPL-2.0 (V: 4 licence errors under `deny.toml`)** | 54 (V) | CSS selectors | - | - | - | - | **rejected by licence policy** |
| External tools (`pdftotext`, LibreOffice) | n/a | - | good | partial | good | good | - | rejected: requires exec, breaks "worker executes nothing", not guaranteed present offline |
| OCR | - | - | - | - | - | - | - | out of scope for the first slice |

### 4.2 Recommendation

Slice order: Markdown/text (M8.2) -> DOCX (M8.5) -> PDF (M8.6, after a bake-off on a corpus, because `pdf_oxide` is the only candidate with layout but is the youngest and largest) -> HTML (M8.7). The model does not depend on the PDF choice: the PDF-specific data lives in `page_no`, `bbox_json`, `anchor_precision = page_only|node_exact`.

### 4.3 Experiments (Verified; `docs/experiments/m8/`; release build, one run each)

| Case | Result |
|---|---|
| `paper.pdf` (24 KB, 1 page, German/Japanese/Russian text, table) | lopdf 1 ms/6 MB: text complete but fragmented per run (table cells and the CJK sentence split into separate lines); pdf_oxide 2 ms/9 MB: lines joined, table rows as `A 10 0.91`, CJK sentence intact; `extract_spans` gives 26 spans with bbox, font size, weight |
| `enc.pdf` (AES-256, user password) | lopdf: `encrypted=true`, 0 pages, empty text; pdf_oxide: 1 page, **empty text, no error** -> an encrypted document can look like an empty one; the worker must test encryption explicitly and refuse |
| `trunc.pdf` (first 1500 bytes) | lopdf: error `failed parsing cross reference table`; pdf_oxide: error per page; no panic |
| `bomb.pdf` (195 KB, one Flate stream of 200 MB of zeros) | both parsers **inflated it fully**: 154-157 ms, **393-419 MB peak RSS**, no error. No built-in cap was found in the calls used (Unknown: whether configuration exists). **Conclusion: in-process PDF parsing is not safe; an enforced cgroup memory limit and a pre-parse stream-length policy are required.** |
| `memo.docx` (6.5 KB, from HTML via LibreOffice) | `zip`+`roxmltree`: `Heading1`, `Heading2`, `BodyText`, `TableHeading`/`TableContents`, 1 table, list items as `BodyText` (numbering lives elsewhere), 0 ms/5 MB |
| `bomb.docx` (3 MB on disk, `document.xml` declares 1,020,000,113 bytes) | a **read cap** (`take(16 MiB + 1)`) refused at the cap in 10 ms/25 MB. The declared size is attacker-controlled; the cap, not the declaration, is the control |
| `memo.html` | scraper (licence-rejected) extracted title/headings/cells/items: the DOM route works; `html5ever` + `rcdom` is licence-clean (37 crates) but was only licence-checked, not run |

Not tested (Unknown): real-world PDFs (scanned, tagged, forms, multi-column), PDF malformed-object fuzzing, DOCX with external relationships/macros/embedded objects (`.docm`, `word/embeddings`), XML entity expansion limits in `roxmltree`, behaviour under a real cgroup limit, throughput on large files.

## 5. Classification

### 5.1 Architecture (simplest sufficient)

```
T0 format sniffing        magic bytes + structure; extension = hint            deterministic
T1 signals                counted, anchored features (see below)              deterministic
T2 rule scorer            transparent per-label weights over T1 signals       deterministic, versioned taxonomy
T3 embedding signal       similarity to per-label prototype texts (optional)  uses the existing local embedding model when enabled
T4 learned/LLM (optional) local classifier or SLM, or an LLM, only when explicitly enabled; output is `model_inferred` and cannot override T2 `classified` results
```

* **T1 signals** (examples): heading vocabulary and order (`Abstract`/`Methodology`/`References`; `Whereas`/`Parties`/`Termination`; `Invoice no`/`VAT`/`Total`); presence of numeric tables with currency and totals; clause numbering `1.1`/`(a)`; date and address patterns; salutation/closing forms; endpoint-like tokens (`GET /v1/...`); `Attendees`/`Action items`; document length and page structure; PDF font-size outline; DOCX style names. Each signal stores *where* it was found.
* **Why not mandatory ML:** the repository's stated posture is deterministic, local-first, optional AI (README "Deterministic" principle; embeddings optional and never default). Rules plus anchored signals are auditable and cost milliseconds. T3 reuses `nomic-embed-text-v1.5` already shipped as an optional capability; prototype texts are fixtures, not model weights.
* **Confidence representation.** Each label carries `score` in [0,1] (T2: normalized weighted evidence; **not a probability** unless `calibration` says so), `basis` (the signals and weights that fired, with anchors), and `calibration: none | measured{corpus_id, metric, n}`. API consumers get a three-level bucket (`strong`, `moderate`, `weak`) derived from thresholds that are part of the taxonomy version, plus the raw score.
* **Abstention.** `unknown` is emitted when no label reaches tau; `ambiguous` when more than one label reaches tau within delta. tau/delta start conservative (Assumed: tau 0.5, delta 0.15) and are frozen only after the M8.3 baseline measurement. T4 may only *add* labels or *raise* an `unknown`/`ambiguous` to `classified` if separately recorded; both results remain stored.
* **Mixed documents.** Classification runs over section subtrees in addition to the document; the document-level result is the aggregate and may be `ambiguous` while each part is `classified`.

### 5.2 Taxonomy v1 (labels; each needs positive fixtures and look-alike negatives)

`scientific_paper`, `contract`, `invoice_or_receipt`, `technical_specification`, `api_documentation`, `software_manual`, `meeting_minutes`, `cv_resume`, `correspondence`, `policy_or_regulation`, `proposal`, `knowledge_base_article`, plus the explicit outcomes `unknown`, `ambiguous`. The taxonomy is data (versioned file), not an enum compiled into the schema; adding a label never needs a migration.

### 5.3 Evaluation fixtures (see section 10)

Must include: misleading titles and metadata (the PDF probe is the template), the same text under different formats, ambiguous documents (a proposal that embeds a contract), multilingual (German, Japanese, Russian were already extracted correctly by `pdf_oxide`), mixed documents, near-empty documents, very long documents, templated boilerplate that mimics another type.

## 6. Structure and provenance

### 6.1 What is extracted

Pages and boundaries (PDF), sections/headings (all), paragraphs and spans, tables with rows and cells, figures and captions (caption text and location; no image analysis), footnotes and appendices, citations and a references list (detected, parsed only to the level the text supports), and document metadata with versions. Absent structure is **not synthesized**: a plain-text document has paragraphs only.

### 6.2 Anchors

A citation anchor is a string that resolves against **a specific extraction**:

```
mind-doc:v1:<logical_source_id>:<source_content_hash[0..16]>:<extractor_id>@<ver>#<node_path>[;t=<start>-<end>][;p=<page>][;b=<x0>,<y0>,<x1>,<y1>]
```

* `node_path` is the ordinal path in the tree (`3/2/5`), stable for a given extraction; `t=` is a UTF-8 byte range in the canonical text (`byte_exact`); `p=`/`b=` are PDF page and bbox in points (`page_only`/`node_exact`).
* Precision is stored per node: `byte_exact` (md/txt, and any node whose text maps to the canonical text), `node_exact` (DOCX paragraph/cell, HTML element), `page_only` (PDF text without bbox), `none`. **The API must never print a position more precise than the stored precision.**
* PDF bbox and the semantic hierarchy are separate columns: a PDF span has *both* a bbox and a place in the heading tree when headings can be inferred (`parser_derived` from font size, with the font-size rule recorded), otherwise only the bbox.
* Every derived attribute/classification/relation stores `evidence` (node id + optional byte range) *in the same extraction*; a claim that cannot be anchored is stored as `model_inferred` without evidence, never as `source_stated`.
* Revision correctness: the anchor contains the content hash and the extractor identity, so it cannot silently resolve against a different revision; a re-extraction produces a new `doc_extractions` row and old anchors keep resolving against the old one.

## 7. Trust boundaries and resource limits

Documents are untrusted. Model: the hostile document is the attacker; the worker is confined; the receiver validates.

| Threat | Control | Status |
|---|---|---|
| Parser memory/CPU exhaustion (decompression bomb, huge pages/tables) | per-operation cgroup `memory.max` + `pids.max` (existing broker mechanism), wall clock (existing), **and** pre-parse policy: input size cap, per-stream/entry decompressed cap enforced by a read limiter | Verified need (bomb.pdf 419 MB); cgroup + limiter design Reasoned |
| ZIP bomb / path traversal in OOXML | open only named parts (`word/document.xml`, `docProps/core.xml`, `word/numbering.xml`, `word/styles.xml`); per-part read cap (probe refused 1.02 GB at 16 MiB in 10 ms); entry count cap; ignore entry names with `..`/absolute; never extract to disk | Verified (cap), Reasoned (rest) |
| Embedded objects/external references (OLE, `.docm` macros, PDF JavaScript/Launch/EmbeddedFiles, external relationships, HTML `<script>`/`<iframe>`/`<link>`) | never executed, never fetched, never extracted; presence recorded as `flags` attribute (`parser_derived`) | Reasoned; worker has no network (seccomp/Landlock) Verified for existing workers |
| Encrypted PDF | detect explicitly, refuse with `encrypted` (pdf_oxide returns empty text silently, Verified) | Verified |
| Malformed input / parser panic | worker process boundary; panic = worker death = `parse_failed`, nothing persisted | Reasoned (same as current workers) |
| Symlinks, traversal, root authorization | unchanged: broker authorizes the exact root, `openat2` pinned root, no frontend-supplied descriptors | Verified for current workers |
| Prompt injection via extracted text | extracted text is data: never executed or interpreted; any T4 prompt delimits it and receives no tools; MCP results that return document text are marked untrusted (existing contract to be confirmed) | Reasoned; Unknown: current MCP wording for untrusted content |
| Classifier DoS | classifier is a bounded computation over already-bounded structure; T4 is off by default, timeboxed | Reasoned |
| Receiver trust in the worker | the frontend recomputes text hash, checks every node's offsets/parent/ordinal/kind invariants, caps counts, and never accepts a worker-supplied classification as authority (classification runs in the frontend or is re-derivable) | Reasoned (mirrors 5c verified transport) |

**Placement decision.** Markdown/plain text may keep running where they run today (in-process in the CLI, in the directory worker when isolated). **All of DOCX, PDF and HTML run only in a confined document worker.** There is no in-process fallback for them.

Proposed default limits (Assumed, to be confirmed on the M8.4/M8.5 corpus; every limit has a failure kind and persists nothing on breach): input file 64 MiB; decompressed bytes per OOXML part 16 MiB, total 64 MiB; ZIP entries 2,000; XML depth 256; pages 2,000; nodes 500,000; table cells 200,000; canonical text 32 MiB; wall clock = existing `--ingest-timeout`; worker `memory.max` 1 GiB (Assumed from the 419 MB bomb observation). Failure behaviour: the operation ends with a structured refusal (`too_large`, `too_many_nodes`, `encrypted`, `unsupported_format`, `parse_failed`, `timeout`); no partial extraction is persisted unless `status = partial` is a separately reviewed feature (default: not offered).

No new authority: the document worker is a new *operation kind* of the existing broker, uses the same root authorization and commit lease, and M8 exposes no new MCP tool until M8.7's review.

## 8. Type-aware processing and retrieval

* **General first.** Every document, whatever its classification, is chunked and indexed by the general strategy (heading-aware, sized under the embedding limit). Type-aware processing is additive and never required for retrieval.
* **Chunking.** Strategy `heading_aware@1`: a chunk = a section subtree split at paragraph/list/table-row boundaries to <= ~1,500 tokens (Reasoned: under the 2,048 truncation limit with margin; the tokenizer count is measured in M8.2), subject = heading path, content = text, plus the node range recorded in `doc_chunks`. Tables are chunked by row groups with the header row repeated. Chunk identity = hash(extraction text range + strategy@version), so an unchanged section keeps its identity across revisions of other sections (stable-chunk goal; Assumed to hold for the append/edit-in-place cases measured in M8.2).
* **Projection.** Each chunk becomes one assertion (`extraction = doc:chunk@N`, new evidence kind `chunk` or reuse `section`) with the existing memory lifecycle and spaces. FTS, embeddings, hybrid fusion and temporal queries therefore work without change (Reasoned; verified in M8.2 tests).
* **Type-aware extractions (M8.7+), each an attributed, anchored, optional layer:**
  scientific paper -> abstract/method/results/reference sections and a parsed reference list; contract -> parties, effective date, term, clause tree, obligations *as quoted clauses*; invoice -> issuer, line items (table rows), currency, tax, totals with a **checked arithmetic consistency attribute**; API docs -> endpoint, method, parameters, examples, version; minutes -> decisions, owners, actions, deadlines.
  Rules: an attribute exists only if the text supports it (anchor required); a missing field is recorded as absent, not guessed; `origin` is `source_stated` only when a deterministic extraction matched, else `model_inferred`.
* **Retrieval integration.** Search gains optional filters (`doc_type`, `scope=section`) over `doc_classifications` joined to chunks; results carry the anchor and classification outcome. Ranking stays lexical/semantic/hybrid; the type is a filter/boost chosen by the caller, not a hidden re-ranker. Relations to existing memories are created only by an explicit later step with evidence (M8.8+), never during ingestion.

## 9. Incremental ingestion and versioning

* **Identities.** Document identity = `logical_sources.id` (survives moves). Content identity = `sources.content_hash` (canonical text) plus `source_bytes_hash` (original). Extraction identity = `(source, extractor_id, extractor_version, params_hash)`. Classification identity = `(extraction, classifier_id, classifier_version, taxonomy_version, params_hash)`. Chunk identity as in section 8.
* **Revisions.** A changed file creates a new `sources` version and a new extraction; the previous extraction and its anchors remain. Memories derived from the old version follow the existing source-lifecycle rules (superseded/retired in the same transaction; the current rule set to be read in M8.2, Unknown).
* **Reprocessing.** Bumping an extractor or classifier version never rewrites rows: it adds a new extraction/run; "current" is chosen by the newest *complete* extraction for the configured identity; older ones stay for provenance. The context exchange announces `(content_hash, extractor identity)` pairs already stored so a re-ingest of unchanged content derives nothing (same mechanism as `mind-code@4`).
* **Duplicates.** Identical bytes at two locations = one `sources` content identity, two logical sources (existing behaviour); duplicate detection across formats (same text as PDF and DOCX) is a *relation* (`duplicate_of`, origin `parser_derived` when canonical text hashes are equal, `model_inferred` when near-equal), never a merge.
* **Supersession/conflict.** Document-stated supersession ("replaces version 2") is a `source_stated` relation with an anchor; the system never infers supersession from filenames or dates alone. Conflicts between chunks follow the existing conflict semantics for memories (coexist, never collapsed).
* **Deletion/replacement.** Disappearance = existing `source_events`; extractions are retained as history (consistent with immutability triggers).
* **Atomic persistence and retries.** One ingestion = one SQLite transaction = one m6 recording revision (the 5a/5d model), commit lease through the broker for isolated operations, `CommitOutcomeUnknown` semantics unchanged; a retry re-announces known extractions so it adds nothing.

## 10. Fixtures and benchmarks

**Corpus (generated, licence-clean).** `tests/fixtures/documents/<case>/{input.<ext>, gold.json}` produced by committed scripts (Typst for PDF, LibreOffice or hand-built OOXML for DOCX, text templates for the rest) so no third-party copyrighted documents are needed. `gold.json`: format, type labels per scope (with allowed-ambiguous sets), expected metadata with origins and anchors, expected tables, expected references. Adversarial cases are generated in the test (bombs, truncations, encryption) as the probes do.

**Deterministic gates (must be 100%, hard fail):**

| Gate | Check |
|---|---|
| Hostile refusal | every hostile fixture ends in a structured refusal; no panic; peak RSS under the cgroup limit; nothing persisted |
| Anchor round-trip | every stored anchor resolves to the stored text range; `byte_exact` anchors slice to exactly the quoted text |
| Precision honesty | no output claims a precision above the stored one |
| Idempotence | re-ingesting unchanged input adds 0 rows (beyond the session/observation rows the existing paths add) |
| Determinism | the same bytes yield byte-identical extraction rows across runs |
| Abstention contract | `unknown`/`ambiguous` are stored explicitly; `source_stated` rows all have anchors |
| Backward compatibility | existing Markdown/text/Git/directory tests pass unchanged; default MCP listing byte-identical |
| Atomicity/revision | one ingestion = one recording revision; faulted commit leaves nothing |

**Measured quality metrics (reported per release, thresholds frozen after the first baseline; targets below are Assumed):**

| Metric | Initial target |
|---|---|
| Format identification | >= 99% on the corpus (sniffing is deterministic) |
| Type classification | macro-F1 reported; **precision of non-abstained predictions >= 0.95**, abstention rate reported (never optimized away) |
| Misleading-title cases | no worse than the control set (metadata must not move the label) |
| Metadata precision | >= 0.95 for source-stated fields on the gold set; recall reported |
| Structure | heading-tree edit distance and table cell accuracy reported per format |
| Citation/reference correctness | detected reference entries match gold; precision >= 0.95 |
| Retrieval | nDCG@10 / recall@10 on a fixed query set vs. the pre-M8 baseline; must not regress for existing Markdown |
| Budgets | per-format CPU time, peak RSS (cgroup-measured), throughput; limits from section 7 confirmed or revised |
| Incremental | an edit of one section re-derives only that section's chunks (count measured) |

## 11. Implementation sequence

| Stage | Content | Depends on | Exit evidence |
|---|---|---|---|
| **M8.1** | Model types (`src/docmodel/`), migration `0010_documents.sql`, `Store` trait methods (`record_document_extraction`, queries), anchor type + resolver, schema/immutability triggers, m6 history coverage declared | design review | schema + trigger tests; no ingestion behaviour change |
| **M8.2** | Markdown/plain text through the model: nodes, byte anchors, canonical text, `heading_aware@1` chunks projected to assertions; reuse/idempotence; revisions | M8.1 | existing md/txt tests unchanged; anchor round-trip; retrieval parity |
| **M8.3** | Format sniffing; taxonomy v1 + T1/T2 classifier + abstention; evaluation harness; fixtures v1; **baseline measurement** | M8.2 | gates + baseline report; tau/delta frozen |
| **M8.4** | Document worker operation kind, `Documents` verified stream, receiver-side validation, limits and failure kinds, cgroup memory limit; exercised with Markdown/text and hostile synthetic streams | M8.1 | extends the existing broker/worker suites; no new tool |
| **M8.5** | DOCX: `zip` + `roxmltree` in the worker; headings via styles, tables, lists, `docProps`; ZIP/XML limits | M8.4 | DOCX fixtures + hostile DOCX |
| **M8.6** | PDF: parser bake-off (lopdf vs pdf_oxide on a real corpus incl. scanned/tagged/multi-column, measured under cgroup), then born-digital text layer with pages/spans/bbox; encrypted refusal | M8.4 | bake-off report; PDF fixtures + hostile PDF |
| **M8.7** | HTML (`html5ever` + `rcdom`); type-aware extraction layers (anchored attributes) for first two types; retrieval filters; **MCP/CLI surface decision** (closed-schema, opt-in, reviewed) | M8.3, M8.4 | per-type gold metrics; tool-listing invariants |
| **M8.8** | Optional T3/T4 classifier signals, relations (`duplicate_of`, source-stated supersession), reprocessing/cache invalidation hardening, benchmark freeze, docs | M8.3+ | frozen thresholds; release checks |

## 12. First vertical slice (proposal for the M8.1-M8.3 review)

Goal: prove the model end to end on the formats that already work, with no new parser dependency and no isolation change.

* Likely new files: `src/docmodel/mod.rs` (types: `DocumentExtraction`, `DocNode`, `NodeKind`, `Anchor`, `AnchorPrecision`, `Origin`, `Attribute`, `Classification`, `Outcome`), `src/docmodel/anchor.rs` (format/parse/resolve), `src/ingest/document.rs` (`extract_text_document(bytes, hint) -> DocumentExtraction` reusing `parse_markdown`/`parse_text` offsets), `src/ingest/classify.rs` (T1/T2, taxonomy loader), `taxonomy/v1.toml`, `migrations/0010_documents.sql`, `tests/document_model.rs`, `tests/document_classify.rs`, `tests/fixtures/documents/`.
* Interfaces (sketch): `trait Store { fn record_document_extraction(&self, e: DocumentExtraction) -> Result<DocumentReport>; fn has_document_extraction(&self, content_hash: &str, id: &ExtractorIdentity) -> Result<bool>; fn document_anchor_text(&self, anchor: &Anchor) -> Result<AnchorText>; }` and `fn classify(e: &DocumentExtraction, taxonomy: &Taxonomy) -> ClassificationRun`.
* Unchanged: Git ingestion, grants, MCP defaults and tools, the existing `evidence`/`assertions` shape (a new `evidence.kind = 'chunk'` plus byte-offset columns is the only touch to existing tables, in M8.1's migration and behind its own review).
* Rough size (Assumed): 1,500-2,500 lines of Rust plus fixtures and tests.

## 13. Risks and open questions

1. Plane acceptance criteria unread (local Plane retired) -> reconcile before M8.1. (Unknown)
2. `pdf_oxide` maturity (0.3.x, 173 crates) vs `lopdf` (no layout): bake-off on real PDFs; fallback is page-only anchors.
3. Whether to store original bytes (`--keep-original`): privacy/size trade-off; default proposed: no.
4. Current file-change/retire rules for memories in the Markdown path must be read before M8.2. (Unknown)
5. Tokenizer-measured chunk size vs the 2,048 truncation; embedding of tables.
6. `roxmltree` entity/DTD behaviour under hostile XML not tested. (Unknown)
7. Classification thresholds are Assumed until the M8.3 baseline exists; calibration claims are forbidden before then.
8. Prompt-injection posture for any consumer that feeds extracted text to a model is a documentation/contract issue outside this repository's control.
9. Scanned PDFs (no text layer) will classify as `unknown` / `not_attempted`; users must be told, not silently given empty text.
10. DOCX features not probed: footnotes, comments, tracked changes (do they enter the canonical text?), headers/footers, text boxes.
