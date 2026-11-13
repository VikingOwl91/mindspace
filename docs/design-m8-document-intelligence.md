# M8.0 Document Intelligence: architecture and design gate

Status: **design and feasibility only.** Nothing in `src/`, `migrations/` or the MCP surface changes in this milestone. Base: `main` = `92f68ff`.
Labels: **Verified** (observed in this repository or in an experiment named here), **Reasoned** (follows from verified facts), **Assumed**, **Unknown**.
Companion files: `adr-m8-document-intelligence.md` (decision and rejected alternatives), `experiments/m8/` (parser probes and how to reproduce them).

Revision history: `818e30d` (M8.0 design) -> M8.0a reconciliation (this revision). The authoritative acceptance criteria and milestone names are those of the Plane charter "M8 · Document Intelligence – Charter & Acceptance Gates". The local Plane endpoint is retired (HTTP 410) and was not retried; Plane Cloud was **not reachable from this environment**, so the criteria are taken from the copy supplied with the M8.0a task (14 criteria, nine milestones M8.0-M8.8). Anything in the charter beyond that copy is Unknown.
Section 0 is the traceability matrix; section 11 maps the canonical milestones to implementation slices.

## 0. Acceptance traceability (M8.0a)

Statuses are about the **design**, never about implementation (nothing is implemented). Gaps of the `818e30d` draft were corrected in this revision; the fourth column says what changed.

| # | Criterion | Evidence / section | Status in `818e30d` | Correction made in M8.0a | Status now |
|---|---|---|---|---|---|
| 1 | Inspect the actual repository architecture | section 2; the lifecycle code (`record_observation_tx`, `reconcile_logical`, `retire_conn`, `ensure_version`) and the broker config were read in M8.0a (sections 9, 7.3) | partial (lifecycle and broker limits unread) | read and cited; findings in 9.1 and 7.3 | satisfied |
| 2 | Content-based physical format detection, not filename trust | section 4.0 | **missing** (said "sniffing", no algorithm) | detection algorithm, precedence, polyglot and mismatch rules, test vectors | satisfied |
| 3 | Logical type and purpose classified separately | 3.1, 5.2, 5.3 | partial (purpose had no label set) | purpose axis and provisional purpose labels | satisfied |
| 4 | Unknown, ambiguous and mixed documents | 3.2, 5.1 | satisfied | - | satisfied |
| 5 | Deterministic, local-first classification, no mandatory LLM | 5.1, ADR | satisfied | - | satisfied |
| 6 | Versioned extraction and classification identities | 3.3, 9.3 | partial (detection unversioned; extraction key ignored the original bytes) | `detector_version`; extraction key includes `source_bytes_hash` | satisfied |
| 7 | Evidence-backed metadata with explicit origin **and confidence** | 3.1, 3.3 | partial (confidence only on classifications) | attribute `confidence` (`exact`, `normalized`, `inferred` + score) | satisfied |
| 8 | Revision-resolvable anchors, honest precision | 3.4, 6.2, 6.3 | partial (anchor did not bind the original bytes; no degradation rules; retention open) | anchor carries the bytes hash; degradation table; retention policy and citation statuses | satisfied |
| 9 | Parser evaluation: accuracy, maintenance, licensing, resources, security | 4.1-4.3 | partial (maintenance Unknown; entity/ZIP/limits untested) | crates.io release and usage data; ZIP/entity/memory-limit experiments; scanned/encrypted detection | satisfied (accuracy was measured only on synthetic one-page fixtures; stated, owned by M8.7) |
| 10 | Explicit resource and trust boundaries | 7 | partial (broker per-operation limits unverified) | broker config read: limits are broker-wide and unset in production; per-`OpKind` limits specified | satisfied |
| 11 | Fixtures, tests, benchmark plan, implementation sequence | 10, 11 | partial (non-canonical names; fixtures not enumerated) | canonical mapping; fixture manifest | satisfied |
| 12 | Bounded first vertical slice, likely files and interfaces | 12 | satisfied | re-bound to canonical milestones (S1-S4) | satisfied |
| 13 | Verified / Reasoned / Assumed / Unknown distinctions | every section | satisfied | new findings are labelled | satisfied |
| 14 | No full feature implementation before design acceptance | git history: docs and a probe source only | satisfied | - | satisfied |

Conditions that are not design gaps: quality thresholds are provisional until a measured baseline exists (section 10); the Plane charter text beyond the 14 criteria was not seen.

## 1. Summary of recommendations

1. **Model first, formats second.** Introduce a format-independent *document extraction* (nodes + text + anchors + attributed metadata + classifications + relations), keyed like the code derivations: content-addressed by source bytes and versioned by extractor identity. Do not widen `evidence`/`assertions` to carry it; project retrievable chunks into the existing assertion spine so lexical, semantic, hybrid, temporal and lifecycle features keep working unchanged.
2. **Classification is evidence-scored and may abstain.** Deterministic structural/lexical signals produce a ranked label set per scope (document or section) with explicit outcomes `classified | ambiguous | unknown`. Embeddings are an optional extra signal. A local classifier or LLM is an optional, explicitly enabled, never-authoritative layer. No generative model is required.
3. **Parsers for binary formats run only in the isolated worker.** The experiments show an unbounded in-process inflate of a 195 KB PDF costing ~400 MB RSS (section 4.3). The existing broker/worker/cgroup/stream machinery fits better than it did for Git: a document worker needs **no executable at all**.
4. **First vertical slice = slices S1-S4 of section 11** (detection + parser-adapter contract + model + Markdown/text + anchors + chunk projection + deterministic classification), with no new parser dependency. First binary format = **DOCX** (`zip` + `roxmltree`). PDF follows behind a parser bake-off gate; HTML via `html5ever` + `markup5ever_rcdom`, **not** `scraper` (MPL-2.0 is rejected by `deny.toml`). The canonical milestone names (M8.0-M8.8) are the Plane ones; the slices are an implementation order across them (section 11).
5. **No claim of source position beyond what the parser provides.** Every anchor carries an explicit precision (`byte_exact | node_exact | page_only | none`).
6. **Source retention default (M8.0a):** the canonical text is always retained (as `sources.content` already is for text formats); original bytes are **not** retained by default and only by explicit opt-in into a content-addressed blob table. Citations always resolve to text; byte/geometry verification needs retained or still-present bytes and reports `source_unavailable` otherwise (3.4).
7. **The existing lifecycle is reused, not duplicated (M8.0a):** chunk assertions go through `reconcile_logical` with their own producer family; supersession, retirement, as-of search and history are the existing ones (9).

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

Unknown: the maximum file size the current Markdown path accepts (no explicit cap found in `ingest/`); how `ingest_path` retires memories of a *changed* file (resolved in M8.0a: see 9.1).

## 3. The document intelligence model

### 3.1 Separated concepts

| # | Concept | Representation | Authority |
|---|---|---|---|
| 1 | Physical file format | `format: markdown\|text\|pdf\|docx\|html\|unknown` from **magic-byte/structure sniffing**; extension is a hint recorded separately | parser-derived |
| 2 | Logical document type | classification label(s) from a versioned taxonomy | model-inferred (rules/embeddings/LLM) |
| 3 | Intended purpose | a **second, independent classification axis** with its own labels (provisional v1: `inform`, `record`, `agree`, `instruct`, `propose`, `regulate`, `correspond`, `reference`); stored in the same tables with `axis = purpose` | model-inferred |
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

### 3.3 Proposed normalized schema (proposal; migration `0010` in slice S1, not created here)

All new tables are append-only/immutable like `code_*`; only lifecycle rows (memories) change.

```
doc_extractions (id PK, source_id -> sources, source_bytes_hash, source_size, retention [none|blob],
                 format, format_hint, detector_version, extractor_id, extractor_version, params_hash,
                 status [complete|refused], refusal_reason, limits_json, text_hash, created_at,
                 UNIQUE(source_id, source_bytes_hash, extractor_id, extractor_version, params_hash))
doc_texts       (extraction_id PK -> doc_extractions, text TEXT)       -- canonical NFC text; for md/txt = sources.content (no copy)
doc_nodes       (id PK, extraction_id, parent_id, ordinal, kind, depth, text_start, text_end,   -- UTF-8 byte offsets into doc_texts, nullable
                 page_no, bbox_json, anchor_precision, attrs_json)
doc_attributes  (id PK, extraction_id, node_id NULL, key, value, origin, confidence [exact|normalized|inferred],
                 score NULL, evidence_node_id NULL, evidence_start NULL, evidence_end NULL,
                 producer_id, producer_version)
doc_classif_runs(id PK, extraction_id, classifier_id, classifier_version, taxonomy_version, params_hash, created_at, UNIQUE(...))
doc_classifications(run_id, axis [type|purpose], scope_node_id NULL, label, outcome, score, basis_json, evidence_json)
doc_blobs       (bytes_hash PK, size, bytes BLOB)   -- only written under retention = blob (3.4)
doc_relations   (id PK, extraction_id, from_node, to_kind, to_ref, relation, origin, evidence_json)
doc_chunks      (id PK, extraction_id, first_node, last_node, text_start, text_end, strategy, strategy_version,
                 content_hash, assertion_id -> assertions NULL)
```

* `doc_extractions` is the unit of reuse and of the context exchange (the receiver announces known `(content_hash, extractor identity)` pairs, as 5c/5d do for code).
* Bytes of the original binary are **not** stored in `sources.content` (TEXT, under the immutability guard). `sources.content` for a binary format holds the **canonical text** and `sources.content_hash` the hash of that text; the SHA-256 of the original bytes is `doc_extractions.source_bytes_hash`. **Why the extraction key includes the bytes hash:** two byte-different files (e.g. re-exported PDFs) can have identical canonical text, hence one `sources` row (`UNIQUE(logical_id, content_hash)`), but different geometry; their extractions must stay distinct or a page/bbox anchor could resolve against the wrong bytes. (Found in M8.0a; the `818e30d` key was `(source_id, extractor...)`.)
* `confidence` on attributes: `exact` = the value is the literal anchored text; `normalized` = deterministically derived from anchored text (a parsed date, a trimmed number); `inferred` = a rule or model produced it, with `score`. A `source_stated` attribute can only be `exact` or `normalized`.
* `doc_relations.to_ref` may reference an existing memory/assertion only when evidence for the link is stored; no row exists "to fill the schema".

### 3.4 Source retention and citation guarantees (M8.0a)

**What exists today (Verified):** for Markdown/text/code the full file text is stored in `sources.content` (immutable, one row per `(logical source, content hash)`). Disappearance of a file marks the logical source `absent` and **does not retire** derived memories (`record_observation_tx`: "disappearance is not falsehood"). So for text formats the system already retains the cited bytes forever. Binary formats have no such precedent.

| Option | Citation fidelity after the file is modified or deleted | Privacy | Storage | Portability | Verdict |
|---|---|---|---|---|---|
| R0 Mutable filesystem source only | none: bytes gone or different; a hash proves a mismatch, it cannot recover the bytes | best (nothing copied) | 0 | worst (the DB is meaningless without the files) | not sufficient alone |
| R1 External immutable reference (path/URI + hash, e.g. a user archive or a git blob) | only if that store is intact; Mindspace cannot verify availability; **a hash is an identity, not a locator** | good | 0 | fragile | kept as a *locator hint*, never as retention |
| R2 Canonical text always retained (extracted spans) | text citations exact forever; geometry (page/bbox) and byte-level checks not re-verifiable; re-extraction of that revision impossible | moderate (text is copied, as today's Markdown) | about the text size | good | **default floor** |
| R3 Content-addressed blob table in SQLite (`doc_blobs`) | full: re-extraction, page rendering, byte verification | worst (full copy of sensitive files; erasure semantics) | up to a cap per file (64 MiB proposed) | best (single-file backup, transactional) | **opt-in** |
| R4 Sidecar blob directory | as R3 | as R3 | as R3 | breaks atomicity with the transaction and single-file backup | rejected |

**Recommended default policy:** R2 always, R1 as a locator hint (the logical source's `current_location`, already stored), R3 only on explicit opt-in (`retention = blob`, per ingest, recorded in `doc_extractions.retention`). It matches the existing text behaviour, keeps the privacy default conservative (original files are not duplicated into the database), and never lets a citation silently point at changed bytes. Assumed: WAL growth for 64 MiB blobs is acceptable; to be measured before R3 is built.

**Citation resolution statuses** (always returned by the resolver):

| Status | Meaning |
|---|---|
| `text_resolved` | the anchored range exists in the retained canonical text of the cited extraction (always available) |
| `bytes_resolved` | original bytes retained (`retention = blob`) and re-verified against `source_bytes_hash` |
| `source_present_verified` | no retention, but the file at the locator hashes to `source_bytes_hash` right now |
| `source_changed` | the file at the locator exists and hashes differently |
| `source_missing` | the locator no longer exists |
| `source_unavailable` | no retention and the locator cannot be checked (unreadable, outside granted roots, remote) |

**Guarantees without retained bytes, after modification or deletion.** Kept: the quoted text, its position in the canonical text, the extraction identity, the classification evidence, and the fact of the mismatch. Lost: rendering or visually verifying a PDF page/bbox, re-running a newer extractor on that revision, byte-level proof that the canonical text is what the file contained. The API must state which statuses apply and must not describe a `page_only`/`node_exact` anchor as verifiable when the status is `source_changed|missing|unavailable`. Verification by hash alone is never reported as retrieval.

**Tests (specified for the implementation milestones).** (1) replace: ingest A, modify the file, re-ingest B; A's citation still yields `text_resolved` with A's text and `source_changed` for the live file; B's citation is a different extraction. (2) delete: remove the file; A's citation is `text_resolved` + `source_missing`; memories stay active (existing semantics); the logical source is `absent`. (3) equal text, different bytes: two extractions, distinct anchors, each resolves against its own bytes hash. (4) round trip: for every stored node `resolve(anchor(node))` returns the stored text slice byte for byte. (5) retention opt-in: after deleting the file `bytes_resolved` holds and a re-extraction with a newer extractor version reproduces the node tree of the old revision. (6) no retention: a geometry anchor never reports `bytes_resolved`. (7) erase: deleting a blob row removes `bytes_resolved` and leaves `text_resolved` (the user-facing erasure policy is open, risk 3).

## 4. Formats and parsers

### 4.0 Physical format detection (content-based; M8.0a)

Detection is a versioned deterministic function `detect(bytes_prefix, size) -> {format, hint_mismatch, flags}` (`detector_version` is stored). The extension is recorded as `format_hint` and may only choose between *interpretations of an already detected text format* (Markdown vs plain text); it can never override a binary detection, and a conflicting hint sets `hint_mismatch`.

| Order | Test (first 4 KiB, then structure) | Result |
|---|---|---|
| 1 | empty file | refuse `empty` |
| 2 | `%PDF-` within the first 1,024 bytes | "suspected pdf", confirmed to `pdf` only when the adapter finds a trailer/xref structure |
| 3 | `PK\x03\x04` local header, readable central directory, `[Content_Types].xml` whose main part is `...wordprocessingml.document.main+xml` | `docx`; a `macroEnabled` main part -> refuse `macro_enabled`; other OOXML or any other ZIP -> refuse `unsupported_container` |
| 4 | valid UTF-8 (optional BOM), no NUL, no control bytes beyond `\t \r \n \f`, and the first non-whitespace token is `<!doctype html`, `<html`, `<head` or `<body` (ASCII case-insensitive) | `html` |
| 5 | other valid UTF-8 text | `text` (interpretation `markdown` if the hint is `md`/`markdown`, else plain) |
| 6 | UTF-16/32 BOM | refuse `unsupported_encoding` (no transcoding in the first release; Assumed) |
| 7 | anything else (NUL bytes, PNG, JPEG, DOC, RTF, ...) | refuse `unsupported_format` |

Polyglots: if more than one of the tests 2-4 matches (a ZIP with a PDF header, a PDF with an appended ZIP directory), the file is refused as `ambiguous_format` (flag `polyglot`); "first match wins" is rejected because parsers disagree on such files, which is an evasion route. Test vectors (generated): each format with a wrong extension; right extension, wrong content (`.md` containing `%PDF-`); PDF+ZIP polyglot; truncated headers; BOM variants; HTML with leading comments/whitespace; text with `%PDF-` mid-file; 1-byte and NUL-containing files.
Reasoned: detection is microseconds over a bounded prefix; it runs in the frontend only for routing and again, authoritatively, inside the confined adapter, so a lying frontend cannot widen what the worker parses. Existing behaviour (Verified): `media_type_of` accepts `.md`, `.markdown`, `.txt`, `.rs` by extension alone and reads the file as text.

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

Maintenance and usage (Verified, crates.io API, 2026-11-09): `lopdf` 0.47.0, 57 releases since 2016, last 2026-11-04, ~14.2 M recent downloads; `pdf_oxide` 0.3.78, **83 releases in 10 months** (created 2025-11-05), last 2026-09-08, ~1.0 M recent downloads (young and fast moving: API churn is likely); `zip` 9.5.0 (tested 9.4.1), 150 releases since 2014, last 2026-11-09, ~101.7 M; `roxmltree` 0.21.2, last 2026-10-24, ~38.5 M; `html5ever` 0.40.1, last 2026-09-14, ~55.5 M; `markup5ever_rcdom` 0.40.1 (**versioned `+unofficial`**: servo-maintained, not the main crate), last 2026-11-05, ~12.4 M; `pulldown-cmark` 0.13.6, last 2026-10-30. Accuracy statements in this document come from one synthetic page per format; they are **not** accuracy measurements and must not be quoted as such (M8.7 owns that).

### 4.2 Recommendation

Slice order (section 11.2): Markdown/text (S1-S4) -> DOCX (S6) -> PDF (S7, after a bake-off on a corpus, because `pdf_oxide` is the only candidate with layout but is the youngest and largest) -> HTML (M8.7). The model does not depend on the PDF choice: the PDF-specific data lives in `page_no`, `bbox_json`, `anchor_precision = page_only|node_exact`.

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

M8.0a experiments (same probe, same machine; fixtures generated as listed in `experiments/m8/README.md`):

| Question | Result (Verified) |
|---|---|
| DOCX XML entities | `roxmltree` default **refuses any DTD** (`XML with DTD detected`) for a billion-laughs and an external-entity (`SYSTEM "file:///etc/hostname"`) document. With `allow_dtd = true` the nested expansion is refused (`entity reference loop`) and the external entity is not resolved (`unknown entity reference`): **no file is read**. Policy: keep the default (no DTD) |
| XML depth and size | 5,000 and 300,000 nested elements parse without recursion failure (36 ms, 35 MB at 300k) under default options; `ParsingOptions::nodes_limit = 100,000` refuses the 300k case (`nodes limit reached`). Policy: set `nodes_limit` |
| ZIP entry count | 100,000 entries (10.6 MB file): 99 ms, 23 MB; `len()` is known before any read -> enforce the entry cap first |
| ZIP path names | `zip` reports `enclosed_name().is_some() == false` for `../../evil.txt`; an absolute name counts as "enclosed" after stripping. We never extract to disk and open only named parts, so names never reach the filesystem |
| ZIP duplicate names (parser differential) | an archive with two `word/document.xml` entries is listed by `zip` with **2 entries where the central directory has 3**, and `by_name` returns the **last**; Python's `zipfile` lists all three. The crate hides the duplicate, so a consumer such as Word could show different text than Mindspace indexed. Policy: own **central-directory pre-scan for duplicate names**, refusal `duplicate_entry` (whether the crate has a strict mode is Unknown) |
| PDF memory amplification, cgroup | `bomb.pdf` under `systemd-run --user -p MemoryMax=128M -p MemorySwapMax=0`: **both parsers are OOM-killed (SIGKILL) at 128 MB within 45-52 ms**; the host is unaffected |
| PDF memory amplification, `RLIMIT_AS` 256 MiB | both abort with `memory allocation of N bytes failed` (allocation failure aborts the process, no panic): the worker dies and nothing is persisted. So `RLIMIT_AS` works **without** cgroup delegation |
| Encrypted PDF | detectable before extraction: `lopdf::Document::is_encrypted()` and `pdf_oxide::PdfDocument::is_encrypted()` are true for `enc.pdf` (pdf_oxide: false for the plain file). Policy: check first, refuse `encrypted` |
| Scanned (image-only) PDF | `scan.pdf` (Ghostscript `pdfimage24` raster of the probe): both parsers return **1 page, empty text, no error**. `lopdf::get_page_images` returns 1 image and the extracted text has 0 chars -> heuristic `images > 0 && text == ""` per page gives `no_text_layer` (page 1 of `paper.pdf`: 0 images, 394 chars -> has text). Policy: such pages are flagged, the document is `not_attempted` for classification with a user-visible reason; text is never silently empty |

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
* **Abstention.** `unknown` is emitted when no label reaches tau; `ambiguous` when more than one label reaches tau within delta. tau/delta start conservative (Assumed: tau 0.5, delta 0.15) and are frozen only after the S4 baseline measurement (M8.7). T4 may only *add* labels or *raise* an `unknown`/`ambiguous` to `classified` if separately recorded; both results remain stored.
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
* Revision correctness: the anchor contains the **original-bytes** hash prefix (geometry depends on bytes, not only on text) and the extractor identity, so it cannot silently resolve against a different revision; a re-extraction produces a new `doc_extractions` row and old anchors keep resolving against the old one. What "resolving" can promise after the file changed is defined by the status table in 3.4.

### 6.3 Precision degradation rules

| Situation | Precision stored | Rule |
|---|---|---|
| Markdown/text node, canonical text equals the source text | `byte_exact` | offsets are into the retained text |
| DOCX paragraph or cell, text assembled from runs | `node_exact` (paragraph index + canonical-text range) | the range is exact in the canonical text, the XML position is not claimed |
| HTML element | `node_exact` | source byte offsets are claimed only if the tokenizer provides them (Unknown) |
| PDF text span with bbox | `node_exact` with `p=`/`b=` | the bbox is the parser's, in points; rotation and crop-box handling are Unknown |
| PDF text without usable geometry | `page_only` | page number only |
| Text reflowed by the parser (hyphenation repaired, columns reordered) | canonical-text range stays exact **in the canonical text**; source precision is downgraded one level and `reflowed` is flagged | a quote is exact, a location in the original is not |
| PDF table cell recovered by layout heuristics | `node_exact` only if cell boundaries came from file structure; else `page_only` + flag `layout_inferred` | heuristic cells are never presented as structural |
| Anything not anchorable | `none` | the claim is stored `model_inferred` without evidence |
| Citation resolved with status `source_changed`, `source_missing` or `source_unavailable` | storage unchanged, **reported as unverifiable** | the API must not call it verified |

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

Proposed default limits (Assumed, to be confirmed on the S5/S6 corpus; every limit has a failure kind and persists nothing on breach): input file 64 MiB; decompressed bytes per OOXML part 16 MiB, total 64 MiB; ZIP entries 2,000; XML depth 256; pages 2,000; nodes 500,000; table cells 200,000; canonical text 32 MiB; wall clock = existing `--ingest-timeout`; worker `memory.max` 1 GiB (Assumed from the 419 MB bomb observation). Failure behaviour: the operation ends with a structured refusal (`too_large`, `too_many_nodes`, `encrypted`, `unsupported_format`, `parse_failed`, `timeout`); no partial extraction is persisted unless `status = partial` is a separately reviewed feature (default: not offered).

### 7.2 Controls verified in M8.0a (see 4.3)

Memory amplification is bounded by an enforced limit, not by parser behaviour: both PDF parsers were killed cleanly at 128 MB by a cgroup `MemoryMax` and aborted cleanly under `RLIMIT_AS`; OOXML parts are bounded by read caps, XML by "no DTD" plus `nodes_limit`, ZIP by an entry cap plus a duplicate-name pre-scan. Encrypted and image-only PDFs are *detected* (not inferred from empty text) and refused or flagged.

### 7.3 Can the existing broker/worker protocol carry document-specific limits? (Verified by reading `worker/broker.rs` and `client.rs`)

* The request vocabulary is closed (`ToBroker` is `deny_unknown_fields`; `Begin` carries kind, root, spaces: **no limit field**). That is correct for authority and stays: limits must be chosen by the broker.
* Limits today are **broker-wide** in `BrokerConfig`: `cleanup: CleanupPolicy::Cgroup{memory_max, pids_max}`, `rlimit_as_mib`, `rlimit_cpu_secs`, `wall_timeout`. In production their defaults are `None`/experimental (`memory_max: None`, `pids_max: None`; only the `spawn-gate` test build sets a memory limit). So today **no worker has a production memory limit**, as already documented for the directory and Git tools.
* If a cgroup `memory_max` is requested but the memory controller is not delegated, `OpCgroup::create` fails (`scope.has("memory")`), i.e. the operation is refused: fail closed.
* Therefore a document operation needs: (1) a per-`OpKind` limits table inside the broker (`limits_for(kind)`), not widening the request; (2) production values for document operations (cgroup `memory.max` where available, otherwise `RLIMIT_AS`, the verified fallback; never "no limit"); (3) a refusal `limits_unavailable` for binary formats when neither can be applied. No change of the protocol vocabulary is needed; one broker-internal change is. Receiver-side `StreamLimits`/`ExtractionLimits` stay as the second bound.

No new authority: the document worker is a new *operation kind* of the existing broker, uses the same root authorization and commit lease, and M8 exposes no new MCP tool until the M8.8 (S10) review.

## 8. Type-aware processing and retrieval

* **General first.** Every document, whatever its classification, is chunked and indexed by the general strategy (heading-aware, sized under the embedding limit). Type-aware processing is additive and never required for retrieval.
* **Chunking.** Strategy `heading_aware@1`: a chunk = a section subtree split at paragraph/list/table-row boundaries to a **deterministic budget of 3,000 UTF-8 bytes**, subject = heading path (+ `#n` for the n-th chunk of a section), content = text, plus the node range recorded in `doc_chunks`. *Why bytes, why 3,000 (Verified: the nomic tokenizer measured on this machine):* tokens per byte were 0.25 (English prose, 3.96 bytes/token), 0.28 (English with tables), 0.30 (Rust code), 0.34 (German), 0.33 (Japanese, 3.00 bytes/token), 0.46 (Russian, 2.19) and **0.50 (a pipe table, 2.02 bytes/token)**. The 2,048-token limit thus corresponds to ~8,100 bytes of English but only ~4,100 bytes of table or Cyrillic text. 3,000 bytes keeps the worst measured case near 1,490 tokens (27% margin) and is independent of whether embeddings are enabled, so chunk identity never changes when they are switched on. It is not a guarantee: byte-fallback tokenization of rare scripts or emoji can approach one token per byte (Assumed, not measured).
* **No silent truncation (acceptance condition for M8.5).** The runtime currently truncates over-long input without any signal (`embeddings/runtime.rs`: `ids.truncate(max_seq_len)`). Before document chunks are embedded the embedding path must either report `truncated = true` per assertion (stored with the derivation and surfaced in search diagnostics) or split the text; a chunk beyond the limit must never look fully embedded. This is a prerequisite recorded for M8.5, not a change made here. Tables are chunked by row groups with the header row repeated. Chunk identity = hash(extraction text range + strategy@version), so an unchanged section keeps its identity across revisions of other sections (stable-chunk goal; Assumed to hold for the append/edit-in-place cases measured in M8.2).
* **Projection.** Each chunk becomes one assertion (`extraction = doc:chunk@N`, new evidence kind `chunk` or reuse `section`) with the existing memory lifecycle and spaces. FTS, embeddings, hybrid fusion and temporal queries therefore work without change (Reasoned; verified in M8.2 tests).
* **Type-aware extractions (M8.4, slice S8 onward), each an attributed, anchored, optional layer:**
  scientific paper -> abstract/method/results/reference sections and a parsed reference list; contract -> parties, effective date, term, clause tree, obligations *as quoted clauses*; invoice -> issuer, line items (table rows), currency, tax, totals with a **checked arithmetic consistency attribute**; API docs -> endpoint, method, parameters, examples, version; minutes -> decisions, owners, actions, deadlines.
  Rules: an attribute exists only if the text supports it (anchor required); a missing field is recorded as absent, not guessed; `origin` is `source_stated` only when a deterministic extraction matched, else `model_inferred`.
* **Retrieval integration.** Search gains optional filters (`doc_type`, `scope=section`) over `doc_classifications` joined to chunks; results carry the anchor and classification outcome. Ranking stays lexical/semantic/hybrid; the type is a filter/boost chosen by the caller, not a hidden re-ranker. Relations to existing memories are created only by an explicit later step with evidence (S9), never during ingestion.

## 9. Incremental ingestion, versioning and lifecycle

### 9.1 What the existing lifecycle does (Verified: `record_observation_tx`, `ensure_version`, `reconcile_logical`, `supersede`, `retire_conn`, `build_search_filters`)

* **Source records.** Each logical source has immutable `sources` versions keyed `(logical_id, content_hash)`; `ensure_version` reuses a version whose hash already exists (including after a file is reverted to old content). Per file the plan is `Unchanged`, `Changed`, `Created` or `Continue` (a unique-content move). Disappearance sets `logical_sources.state = 'absent'` and writes a `source_events` row; **it retires nothing**.
* **Memories.** `reconcile_logical` takes the logical source's *active memories of one producer family* and the new drafts and pairs them **by normalized subject** (then by order). Per pair: same content -> reuse (`Unchanged`, spaces added); different content -> `supersede` (old memory `superseded`, `superseded_by` -> the new memory, new assertion on the new version's evidence). An old subject whose content reappears under another new subject keeps lineage (rename/move of a section). An old subject with no counterpart -> `retire_conn` with reason "subject no longer present in source at re-ingest". A new subject -> `Created`.
* **Producer family** is stored per memory, immutable (a trigger forbids changing it) and `reconcile_logical` filters on it; today `file:legacy` (Markdown/text) and `git:legacy`. Only memories of the family being reconciled are touched.
* **Temporal.** Memories carry `created_at`, `superseded_at`, `retired_at`; `build_search_filters` implements as-of search (`created_at <= at AND (superseded_at IS NULL OR > at) AND (retired_at IS NULL OR > at)`) and include-superseded/retired flags; the m6 history tables record every state change per revision. Old assertions, evidence and sources are immutable, so a superseded memory still points at its original evidence and source version.

### 9.2 Decision: reuse it; documents need a producer family, not a second mechanism

| Concern | Decision | Why the existing mechanism suffices (Reasoned from 9.1) |
|---|---|---|
| Document revisions | a new byte revision -> new `sources` version (canonical-text hash) and a new `doc_extractions` row; chunk drafts go through `reconcile_logical` with family `doc:chunk@1` | pairing by normalized subject (heading path + `#n`) yields supersede for edited sections, reuse for untouched ones, retire for removed ones |
| Assertions and memories | one assertion per chunk, extraction id `doc:chunk@1`; no new lifecycle states | `active/superseded/retired` already express "current / replaced by / no longer in source" |
| Temporal validity | system-time as-of via the existing columns; **document-stated** dates are attributes, never `created_at`/`superseded_at` | keeps the m6 semantics intact |
| Re-derivation | a new chunker or extractor produces drafts under a new family or extraction version; the old family's memories are not reconciled by the new run and are retired by an explicit, reviewed migration step | the family filter prevents silent cross-version mutation (see hazard 1) |
| Search results | results show memory state; as-of works unchanged; hits carry an anchor and its citation status (3.4) | no new filter semantics |
| Historical citations | anchors name an extraction; superseded memories keep their evidence and source version | immutability triggers guarantee it |
| Deletion and retention | file deletion -> `absent`, memories stay active (existing "disappearance is not falsehood"); search shows `source_missing`; retiring is an explicit user action; erasing a retained blob (R3) does not delete text or memories | consistent with the Markdown behaviour users already have |

**Why not a second lifecycle:** nothing in the document case needs a state the three-state machine cannot express; the one genuinely new requirement (several memories per section once it is split into chunks) is solved by the subject suffix. A second mechanism would split search, as-of and history semantics.

**Hazards found (to be tested, not designed around by invention):**
1. *Family hazard.* If Markdown/text were re-routed through the new chunker without retiring `file:legacy` memories, each file would have two active memory sets. Decision: **do not re-route existing Markdown/text ingestion in S1-S4**; the new path serves documents ingested under the new pipeline (new formats first). Any later unification is a reviewed migration with an explicit retirement step and a test that counts active memories before and after.
2. *Subject instability.* Pairing is by subject; if an edit moves a chunk boundary, the `#n` suffixes shift and neighbours pair with the wrong old content (supersede chains stay correct but noisy). Mitigation, measured in S3: split only at paragraph/row boundaries counted from the section start, and track "chunks unchanged by a one-paragraph edit".
3. *Identical subjects.* Duplicate heading paths pair by order; an ordinal for repeated headings plus `#n` makes subjects unique per extraction.
4. *Reverts.* Re-ingesting an older byte revision reuses the old `sources` row and extraction (idempotent) and supersedes the newer memories back; verify that it behaves as `ensure_version` implies.

### 9.3 Identities and reprocessing

* **Identities.** Document identity = `logical_sources.id`. Content identity = `sources.content_hash` (canonical text) + `source_bytes_hash`. Detection identity = `detector_version`. Extraction identity = `(source, source_bytes_hash, extractor_id, extractor_version, params_hash)`. Classification identity = `(extraction, classifier_id, classifier_version, taxonomy_version, params_hash)` per axis. Chunk identity = hash(text range + strategy@version).
* **Reprocessing.** A new extractor or classifier version adds rows; it never rewrites. "Current" = the newest complete extraction for the configured identity; older ones remain for provenance and old anchors. The context exchange announces `(content_hash, extractor identity)` pairs already stored so unchanged content derives nothing (the `mind-code@4` mechanism).
* **Duplicates.** Identical bytes at two locations = one content identity, two logical sources (existing behaviour); duplicates across formats are a `duplicate_of` *relation* with evidence (equal canonical text = `parser_derived`, near-equal = `model_inferred`), never a merge.
* **Supersession and conflict.** Document-stated supersession ("replaces version 2") is a `source_stated` relation with an anchor; filenames and dates alone never imply it. Chunk conflicts follow the existing conflict semantics.
* **Atomic persistence and retries.** One ingestion = one transaction = one m6 recording revision (the 5a/5d model), commit lease through the broker for isolated operations, `CommitOutcomeUnknown` unchanged; a retry re-announces known extractions and adds nothing.

## 10. Fixtures and benchmarks

**Corpus (generated, licence-clean).** `tests/fixtures/documents/<case>/{input.<ext>, gold.json}` produced by committed scripts (Typst for PDF, LibreOffice or hand-built OOXML for DOCX, text templates for the rest) so no third-party copyrighted documents are needed. `gold.json`: format, type labels per scope (with allowed-ambiguous sets), expected metadata with origins and anchors, expected tables, expected references. Adversarial cases are generated in the test (bombs, truncations, encryption) as the probes do.

**Fixture manifest v1** (case ids; each has a `gold.json` and names what it exercises):

| Group | Case ids | Exercises |
|---|---|---|
| Detection | `det-ext-wrong-md-as-pdf`, `det-ext-wrong-pdf-as-txt`, `det-polyglot-pdf-zip`, `det-bom-utf8`, `det-bom-utf16`, `det-html-leading-comment`, `det-pdf-marker-midfile`, `det-empty`, `det-nul-bytes`, `det-xlsx-container`, `det-docm-macro` | content-based detection, refusals, hint mismatch |
| Classification | `cls-paper-misleading-title`, `cls-invoice-as-paper-title`, `cls-contract-clauses`, `cls-minutes`, `cls-cv`, `cls-api-docs`, `cls-spec-vs-manual`, `cls-proposal-embeds-contract` (ambiguous), `cls-mixed-contract-with-invoice-appendix`, `cls-empty-ish` (unknown), `cls-de`, `cls-ja`, `cls-ru`, `cls-boilerplate-lookalike` | type/purpose, abstention, multilingual, mixed |
| Structure | `str-headings-3-levels`, `str-table-simple`, `str-table-merged-cells`, `str-lists-nested`, `str-footnotes`, `str-two-column-pdf`, `str-figure-caption`, `str-references-list` | hierarchy, tables, citations |
| Anchors and retention | `anc-roundtrip-md`, `anc-replace-file`, `anc-delete-file`, `anc-same-text-different-bytes`, `anc-retention-blob`, `anc-reflowed-pdf` | resolution statuses, precision honesty |
| Hostile | `host-pdf-flate-bomb`, `host-pdf-truncated`, `host-pdf-encrypted`, `host-pdf-image-only`, `host-docx-zip-bomb`, `host-docx-billion-laughs`, `host-docx-xxe`, `host-docx-deep-nesting`, `host-docx-duplicate-entry`, `host-docx-many-entries`, `host-docx-traversal-names`, `host-html-script-iframe`, `host-text-injection-prompt` | refusal, limits, parser differentials |
| Lifecycle | `lif-edit-one-paragraph`, `lif-revert-to-old`, `lif-reorder-sections`, `lif-rename-heading`, `lif-duplicate-heading`, `lif-reingest-unchanged` | reuse/supersede/retire, idempotence |
| Retrieval | `ret-queries-v1` (fixed EN/DE query set with judged relevant chunks) | nDCG/recall vs baseline |

The probes in `docs/experiments/m8/` already generate the `host-*` PDF/DOCX cases and seeds for `det-*`/`str-*`; the rest are authored (licence-clean, generated by committed scripts).

**Deterministic gates (must be 100%, hard fail):**

| Gate | Check |
|---|---|
| Hostile refusal | every hostile fixture ends in a structured refusal; no panic; peak RSS under the enforced limit; nothing persisted; encrypted and image-only PDFs are never reported as empty documents |
| Anchor round-trip | every stored anchor resolves to the stored text range; `byte_exact` anchors slice to exactly the quoted text |
| Precision honesty | no output claims a precision above the stored one |
| Idempotence | re-ingesting unchanged input adds 0 rows (beyond the session/observation rows the existing paths add) |
| Determinism | the same bytes yield byte-identical extraction rows across runs |
| Abstention contract | `unknown`/`ambiguous` are stored explicitly; `source_stated` rows all have anchors |
| Backward compatibility | existing Markdown/text/Git/directory tests pass unchanged; default MCP listing byte-identical |
| Atomicity/revision | one ingestion = one recording revision; faulted commit leaves nothing |

**Measured quality metrics (reported per release). Every number in the right-hand column is PROVISIONAL (Assumed): no baseline exists, so none may serve as an acceptance threshold until the first baseline run (taken with S4, owned by M8.7) is recorded and the thresholds are frozen in a reviewed change.**

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

## 11. Canonical milestones and implementation slices

### 11.1 Logical milestones (canonical = the Plane names)

| Milestone | Outcome ("done" means) | Depends on |
|---|---|---|
| **M8.0** Design and architecture | this document, the ADR and the experiments, reviewed | - |
| **M8.1** Format detection and safe parser adapter | content-based detection (4.0); the adapter contract (bytes in -> canonical text + flat typed nodes + flags out, bounded and refusing); the confined document operation (per-`OpKind` limits, receiver-side validation); adapters for text/Markdown, DOCX, PDF (text layer) and HTML, each with its hostile fixtures | M8.0 |
| **M8.2** Document type/purpose classification | taxonomy v1, signals, scorer, abstention, evaluation harness, baseline | M8.1 (needs extraction output) |
| **M8.3** Structural hierarchy and source anchors | tree (parent/depth), tables, metadata attributes with origin/confidence, anchors with precision, resolver, retention policy and citation statuses (3.4, 6) | M8.1 |
| **M8.4** Type-aware extraction and retrieval | typed attribute layers (first two types), retrieval filters by type, results with anchors | M8.2, M8.3, M8.5 |
| **M8.5** Chunking, versions and temporal provenance | `heading_aware@1`, projection to assertions through the existing lifecycle (9), idempotence/revert/edit behaviour, the truncation prerequisite (8) | M8.3 (nodes) |
| **M8.6** OCR / scanned document strategy | a decision record plus detection and user-visible handling of `no_text_layer`; OCR itself only if the decision says so (separate worker, opt-in) | M8.1 |
| **M8.7** Quality and adversarial benchmarks | corpus, gates, metrics, baseline, frozen thresholds; runs from the first slice on | grows with each milestone |
| **M8.8** Opt-in MCP integration | hidden/opt-in, closed-schema tool(s), default listing byte-identical, reviewed | M8.1-M8.5, M8.7 |

### 11.2 Implementation slices (the order work is done; each names the milestone it advances)

A slice completes a milestone only for the formats it covers. A milestone is **done** when all of its gates pass for every format in scope at that time; adding a format re-opens it, it does not redefine it.

| Slice | Content | Advances | Why this order |
|---|---|---|---|
| S1 | detection function + adapter trait + canonical-text/flat-node model + text/Markdown adapter + `doc_extractions`/`doc_texts`/`doc_nodes` (migration `0010`) | M8.1 (text part) | smallest end-to-end proof of the contract; no new dependency |
| S2 | hierarchy, byte anchors, resolver, retention default and citation statuses | M8.3 (text part) | anchors must exist before anything cites |
| S3 | chunking `heading_aware@1` + projection through `reconcile_logical` (family `doc:chunk@1`) + lifecycle tests | M8.5 (text part) | pulled **before** M8.2/M8.4 because nothing is searchable without it; justified by dependency, M8.5's meaning is unchanged |
| S4 | taxonomy v1, signals, scorer, abstention, harness, **baseline** | M8.2, starts M8.7 | needs S1-S3 output |
| S5 | document operation kind in the broker, `Documents` verified stream, per-`OpKind` limits, receiver validation, hostile synthetic streams (Markdown through the worker) | M8.1 (safe-adapter infrastructure) | required before any binary parser |
| S6 | DOCX adapter (zip + roxmltree), tables, `docProps`; duplicate-entry, entity and entry-cap controls | M8.1 (DOCX), M8.3 (DOCX hierarchy/tables) | smallest binary dependency with real semantic structure |
| S7 | PDF adapter after the bake-off (lopdf vs pdf_oxide on a real corpus under the enforced limits): text layer, pages, spans/bbox, `encrypted` refusal, `no_text_layer` flag | M8.1 (PDF), M8.3 (PDF geometry/anchors), M8.6 (detection part) | the parser choice needs evidence first |
| S8 | HTML adapter (`html5ever` + `rcdom`), type-aware layers for two types, retrieval filters | M8.1 (HTML), M8.4 | after structure is trustworthy |
| S9 | OCR decision record; optional embedding/local-classifier signals; relations (`duplicate_of`, source-stated supersession) | M8.6, M8.2 (optional tiers) | optional by design |
| S10 | MCP integration decision and implementation (opt-in) | M8.8 | last: it exposes everything |

**How DOCX and PDF fit without changing meanings.** The adapter (M8.1) promises only *safe, bounded extraction of canonical text and flat typed nodes*. Hierarchy, tables as structure and anchors are M8.3 semantics added per format (DOCX in S6, PDF geometry in S7). Classification (M8.2) and type-aware extraction (M8.4) consume whatever structure exists and abstain where it does not.

## 12. First vertical slice (S1-S4; proposal for review)

Goal: prove detection, the adapter contract, the model, anchors, the existing lifecycle and the classifier end to end on the formats that already work, with no new parser dependency and no isolation change (the confined operation is S5).

* Likely new files: `src/docmodel/mod.rs` (types: `DocumentExtraction`, `DocNode`, `NodeKind`, `Anchor`, `AnchorPrecision`, `Origin`, `Attribute`, `Classification`, `Outcome`), `src/docmodel/anchor.rs` (format/parse/resolve + citation statuses), `src/docmodel/detect.rs` (4.0), `src/ingest/document.rs` (`extract_text_document(bytes, hint) -> DocumentExtraction` reusing `parse_markdown`/`parse_text` offsets), `src/ingest/classify.rs` (T1/T2, taxonomy loader), `taxonomy/v1.toml`, `migrations/0010_documents.sql` (S1; later slices add their own migrations: classification tables in S4, `doc_blobs` only when retention is built), `tests/document_model.rs`, `tests/document_classify.rs`, `tests/fixtures/documents/`.
* Interfaces (sketch): `trait Store { fn record_document_extraction(&self, e: DocumentExtraction) -> Result<DocumentReport>; fn has_document_extraction(&self, content_hash: &str, id: &ExtractorIdentity) -> Result<bool>; fn document_anchor_text(&self, anchor: &Anchor) -> Result<AnchorText>; }` and `fn classify(e: &DocumentExtraction, taxonomy: &Taxonomy) -> ClassificationRun`; `fn detect(prefix: &[u8], size: u64, hint: Option<&str>) -> Detection`; `trait DocAdapter { fn extract(&self, bytes: &mut dyn Read, limits: &DocLimits) -> Result<Extraction, Refusal>; }`.
* Unchanged: Git ingestion, grants, MCP defaults and tools, the existing `evidence`/`assertions` shape (a new `evidence.kind = 'chunk'` plus byte-offset columns is the only touch to existing tables, in M8.1's migration and behind its own review).
* Rough size (Assumed): 1,500-2,500 lines of Rust plus fixtures and tests.

## 13. Risks and open questions

1. **Charter beyond the 14 criteria**, and Plane Cloud access from this environment: not verified. (Unknown)
2. `pdf_oxide` churn (83 releases in 10 months; 173 crates) vs `lopdf` (no layout): bake-off on real PDFs in S7; the fallback is page-only anchors.
3. Blob retention (R3): WAL/size impact of 64 MiB blobs and the user-erasure policy are Assumed/open; the default is no retention.
4. Duplicate-entry detection needs an own central-directory reader (the `zip` crate hides duplicates); that reader is itself attack surface. (Reasoned)
5. The embedding path truncates silently; M8.5 is conditioned on making truncation visible or splitting (section 8). The 3,000-byte chunk budget was measured on seven samples only; rare scripts may exceed it. (Assumed)
6. Producer-family hazard when unifying Markdown/text with the new chunker (9.2): needs a reviewed migration with a before/after count test.
7. Subject-pairing instability for edited sections (9.2): measured in S3; mitigation may need a content-aware pairing key.
8. Per-`OpKind` limits are a broker-internal change, and no worker has a production memory limit today. If neither the memory controller nor `RLIMIT_AS` can be applied, binary formats are refused. (Verified need, Reasoned fix)
9. Classification thresholds and all quality targets are provisional until the S4 baseline exists; calibration claims are forbidden before then.
10. Scanned PDFs are detected and flagged, not read; OCR is a separate decision (S9).
11. Not probed: DOCX footnotes/comments/tracked changes/headers/text boxes; HTML source offsets; PDF page rotation and crop boxes; any real-world PDF corpus.
12. Prompt-injection posture for consumers that feed extracted text to a model is a contract/documentation matter; MCP wording for untrusted content must be checked in S10. (Unknown)
13. `markup5ever_rcdom` is versioned `+unofficial`: acceptable (licence-clean, servo-maintained), its stability is Assumed.

