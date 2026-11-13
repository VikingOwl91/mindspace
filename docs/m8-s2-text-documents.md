# M8 S2: Markdown and plain-text document ingestion (implemented, uncommitted)

Status: implemented for review. Labels: **Verified** (observed in a named test or run), **Reasoned**, **Unknown**.
Basis: `design-m8-document-intelligence.md`, `adr-m8-document-intelligence.md`, `m8-s1-document-model.md`.
Scope: Markdown and UTF-8 plain text only. **No PDF/DOCX/HTML parsing, classification, chunks, embeddings, MCP tool, grant change or migration of existing sources.**

## 1. Entry point and coexistence with legacy ingestion

The only entry point is the library function `mindspace::ingest::document::ingest_document_file(store, path, &DocumentIngestOptions)`. The directory ingest, `ingest_path`, the CLI and the MCP tools do not call it, and no CLI command or tool was added: documents are ingested by explicit programmatic invocation until a later slice decides the surface.

**Can one physical path be processed both ways? Yes** (**Verified**: legacy forms its location as `canonicalize(path)`, and so does the document path). The outcome is two logical sources at one location, of different kinds, that never reconcile with each other (S1: documents are kind `document`; every directory/file lookup filters `kind = 'file'`).

**Policy (critical invariant: no two competing active memory sets):**
1. The document path creates **no evidence, assertion, memory, embedding or search entry**. It stores a document, its revision (canonical text), one extraction and structure nodes. A second memory set therefore cannot arise from it.
2. It never rewrites, supersedes or retires legacy memories, and old Markdown/text sources are **not** migrated.
3. The legacy path never touches document rows (directory re-ingest does not absent, move or reconcile them; deleting the file makes the legacy source `absent` and leaves the document `present`).
4. Projection of document structure into memories/chunks is a later slice and will need its own producer family and an explicit coexistence/retirement decision (design 9.2, hazard 1).

Tests: `a_file_ingested_both_ways_never_gets_two_competing_memory_sets` (both orders, plus an edit and re-ingest through both paths: exactly one producer family, `file:legacy`, one active and one superseded memory, memory/assertion/evidence counts unchanged by document ingestion), `directory_ingest_neither_sees_nor_disturbs_documents...`, `default_mcp_tools_and_retrieval_are_unchanged` (real `mind mcp` process: no tool name contains "document", opt-in tools hidden; search results identical before and after documents exist).

## 2. Format detection (`ingest/document/detect.rs`, `DETECTOR_VERSION = "1"`)

Content decides; the extension is a recorded hint (`format_hint`) and never overrides content.

| Check, in order | Result |
|---|---|
| zero bytes | refuse `empty` |
| a signature where a file STARTS (after an optional BOM and blanks): `%PDF-`; `PK\x03\x04`/`PK\x05\x06` (ZIP/OOXML); PNG, JPEG, ELF, gzip, OLE, RTF; `GIF87a`/`GIF89a` followed by a binary screen descriptor; first token `<!doctype html`/`<html` | refuse `unsupported_format`, **named** (a `.md` file that is a PDF is a PDF). Prose that merely quotes `%PDF-` or begins with the characters `GIF89a` is prose: a real PDF or GIF is binary a few bytes later and is refused by the NUL/UTF-8/control checks anyway |
| UTF-16/UTF-32 byte-order marks | refuse `unsupported_encoding` |
| NUL byte | refuse `binary` |
| invalid UTF-8 (overlong, surrogates, truncated) | refuse `not_utf8`, naming the first invalid byte offset (counting a BOM) |
| C0 controls except tab, LF, CR, FF; DEL (so also ESC) | refuse `control_characters`, naming code point and offset |
| nothing but whitespace (or only a BOM) | refuse `empty` |
| larger than the limit (16 MiB default) | refuse `too_large`; never truncated; exactly the limit is accepted |
| not a regular file / non-UTF-8 path | refuse `not_a_regular_file` / `unsupported_path`; the file type is looked at BEFORE opening and the open is non-blocking, so a named pipe is refused instead of hanging the call |
| C1 control characters, zero-width and bidirectional-control characters | **accepted** (valid text; nothing in S2 interprets or renders them) |

A UTF-8 byte-order mark is accepted and **removed** from the canonical text, so `text_is_source` is false for such files (offsets differ by three bytes from the file).

**Dialect** (Markdown vs plain; plain prose is valid Markdown, so this is an interpretation) is decided by hint and content together and reported as a certainty:

| Hint | Markdown syntax in the content | Dialect | Certainty |
|---|---|---|---|
| `md`/`markdown` | any construct | Markdown | `hint_and_content_agree` |
| `md`/`markdown` | none | Markdown | `hint_only` |
| `txt`/`text` | no heading/fence (lists alone do not count) | Plain | `hint_and_content_agree` |
| `txt`/`text` | ATX heading or fence | **Plain** (conservative) | `ambiguous` |
| other or none | ATX heading or fence | Markdown | `content_only` |
| other or none | only list/quote lines | Plain | `ambiguous` |
| other or none | nothing | Plain | `content_only` |

Signals ignore text inside fences and indented code. The certainty is returned in the ingest report (`Detection`); the dialect itself is durable (extractor id `markdown` or `plaintext`, and the hint in `format_hint`). **The certainty value is not stored**: S1's schema has no column for it and none was added without need; recorded as a limitation.

## 3. Structure (`ingest/document/structure.rs`, `EXTRACTOR_VERSION = "1"`)

Every node range is a UTF-8 byte range of the **canonical text**, from the parser's own offsets (`pulldown-cmark` offset iterator, default CommonMark options; line scanning for plain text), stored as `byte_exact` (exact in the canonical text; claims about the original follow S1's `text_is_source` rule).

* **Markdown:** headings open nested `section` nodes (a section runs to the next heading of the same or a higher level; an H3 after an H1 nests under the H1); each section's first child is its `heading`; blocks before the first heading are children of the root; `paragraph`, `list`, `list_item` (nested lists nest under their item) and `code_block` (fenced, indented, unclosed fences to the end). Setext headings are headings. Not nodes, left inside the enclosing range: block quotes (and headings inside them or inside list items), tables, HTML blocks, rules. Nesting beyond 32 levels is not emitted and **nothing under a suppressed ancestor is attached elsewhere**.
* **Plain text:** paragraphs separated by blank lines (CRLF, indentation and trailing spaces handled), and nothing else: no headings, sections, pages, tables or geometry.
* **Title:** the first heading's text (≤ 200 chars), else the file stem.
* **Migration `0011`:** the closed node-kind vocabulary of S1 had no word for a code block. SQLite cannot alter a CHECK, so `doc_nodes` is rebuilt with `code_block` added (rows copied, triggers recreated). This is the only schema change in S2. **Verified** by `the_node_kind_added_by_migration_eleven_survives_an_upgrade...` (a schema-10 table is rebuilt on open; code blocks insert; the three triggers are back; rows stay append-only). Four older tests that hard-code the latest schema version (or simulate older schemas by deleting migration rows) were updated for version 11.

## 4. Guarantees

* **Deterministic:** equal bytes give equal drafts (`structure_hash` equal), whatever the clock; 400 generated documents × 3 hints are checked for a valid tree, ranges inside parents, ordered non-overlapping siblings, each section starting with its heading, depth ≤ 32, determinism (`generated_documents_always_yield_a_valid_ordered_tree`).
* **Unicode-safe:** ranges fall on character boundaries (1-, 2-, 3- and 4-byte characters, regional-indicator pairs); every node of a CJK/emoji/accent document round-trips through an anchor and the store and quotes exactly its text.
* **Honest precision:** nodes claim only `byte_exact` in the canonical text; with a verified original, `byte_exact` is reported only when `text_is_source`, otherwise `node_exact` (BOM test).
* **Identity (S1 model):** repeat ingestion = no row, no recording revision (also through a differently spelled path); changed content = new revision + extraction in the same document, old anchors still quote the old text, reverting reuses the old ones; a new detector or extractor version = a new extraction of the same revision, each version's anchors resolving to its own; same bytes at two locations = two documents; equal canonical text from different bytes (BOM / no BOM) = one revision, two extractions; renaming `.md` to `.txt` = another document and a different extractor id.
* **Retention:** original bytes only with `retain_original`; then `bytes_resolved` survives deleting the file.
* **Atomic:** an injected SQL failure at a middle node leaves every table (including `m6_revisions`) unchanged and the retry stores everything; a vetoed COMMIT is a clean rollback; a lost acknowledgement stays `CommitOutcomeUnknown` while the data is committed, and a retry finds it.

## 5. Limitations

* Library entry only: no CLI command, no MCP tool, no directory integration.
* Dialect certainty is reported, not stored; the stored dialect is the extractor id.
* Tables, block quotes, footnotes and HTML blocks are not nodes; pages and geometry do not exist for text.
* UTF-16/32 are refused, not transcoded; a BOM-prefixed file has `text_is_source = false`.
* The reading of the file and the hashing happen in-process in the caller's context (like the legacy path); isolation of the text adapter in a worker is not part of S2.
* A moved or renamed file is a new document (no move inference), as in S1.
* The 16 MiB input limit and the 32-level depth cap are Assumed values, not measured budgets.

## 6. Final review findings (before the local commit)

Defects found by challenging the implementation, each with a test that fails without the fix:
1. **A named pipe hung the ingest.** `File::open` on a FIFO blocks until a writer appears, and the regular-file check came after the open. Now the type is checked first and the open is `O_NONBLOCK`. Test `a_named_pipe_is_refused_without_blocking` (reproduced as a timeout before the fix; mutants E5 and E6).
2. **Ordinary text was refused as a binary format.** `%PDF-` anywhere in the first KiB and any text beginning with `GIF8` were treated as PDF/GIF, so a note quoting the PDF header or starting with "GIF89a and GIF87a are..." was rejected. Signatures now count only at the start of the file, and the GIF one only with a binary descriptor. Test `ordinary_text_that_merely_mentions_a_binary_signature_is_text` (mutants E1, E2, E4). Consequence, stated plainly: a hand-written ASCII-only file that starts with `%PDF-` is refused as a PDF, and a PDF with leading junk that is entirely valid text would be read as text.
3. **The migration 0011 test was too weak.** The first version dropped the rows instead of upgrading them. It was replaced by an upgrade of a **populated schema-10 database** built from the real text of migration 0010 (pages, boxes, byte ranges, tables, two documents): every node survives column for column, extractions are untouched, `PRAGMA foreign_key_check` and `integrity_check` are clean (the migration runner runs with foreign keys off and nothing checks them afterwards), the three triggers and the two-column primary key are present, append-only and range checks still fire, `code_block` is accepted while `byte_exact` without a range and unknown kinds are still refused, existing anchors resolve, and reopening changes nothing. A second test sabotages the migration late (a view on `doc_nodes` makes the final `RENAME` fail after the table was rebuilt): the schema afterwards is identical to the state before the attempt, migration 11 is not recorded, the old rules still apply, and the retry succeeds.

Mutation analysis of the one earlier survivor ("a suppressed SECTION leaks its children to the root"): **equivalent, not an untested reachable state.** A section's parent is always the previous open section, levels strictly increase, so a section path has at most six components; the depth cap is 32. The generated-document test now asserts `section path length <= 6` for every section it produces (36,000 documents in the ignored heavy variant), which is the executable form of that argument. The reachable analogue, a suppressed list item, is covered by `nothing_under_a_depth_suppressed_item_escapes_to_the_root`.

Further counterexamples added: CRLF vs LF (same tree, each file's own offsets, no carriage return at a node end, line endings are content and change the canonical hash), heading-like lines inside backtick fences, tilde fences and indented code, and a heavier generated run (`cargo test --test document_ingest -- --ignored`, 8 seeds × 1,500 cases × 3 hints).

## 7. Verification record (final tree)

* `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings` (with and without `spawn-gate`): clean.
* `cargo test --locked --no-fail-fast`: **994 passed, 0 failed, 5 ignored** (baseline 964; 30 new tests in `tests/document_ingest.rs`, 1 further ignored heavy generated run). The ignored run (`-- --ignored`: 8 seeds × 1,500 documents × 3 hints) passes in debug and release.
* Release targeted suites (document ingestion and model, M6 temporal, doctor, scopes, embeddings, manual memory, lifecycle, invariants, persistence atomicity, Git persistence atomicity, retrieval, CLI, MCP, scan/select, ingest confinement): all pass. `document_ingest` and `document_model` 30/30 repeated runs each with a hang timeout: 0 failures, 0 hangs.
* Release binary: no test-hook strings.
* Mutation checks (about 40 mutants over detection, structure, file reading, identity, and migration 0011: rows not copied, triggers missing, `code_block` missing, foreign key dropped, precision check dropped): all caught except three **equivalent or redundant** ones, each argued rather than ignored:
  1. a depth-suppressed *section* leaking to the root: unreachable (sections have at most six path components, the cap is 32; the generated tests assert it);
  2. removing only the pre-open file-type check, or only the `O_NONBLOCK`, from the file reader: two redundant layers, each sufficient alone for a stable pipe (removing **both** makes `a_named_pipe_is_refused_without_blocking` time out, checked by hand);
  3. running migration 11's SQL through `conn` instead of `tx`: the same connection inside the same open transaction, so identical behaviour; atomicity is shown by the late-failure rollback test instead;
  and plain-text CRLF scanning, where `trim_end()` already strips the carriage return.
