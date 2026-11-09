# M8 S1: document model, revision identity and citation anchors (implemented, uncommitted)

Status: implemented for review. Labels: **Verified** (observed in a named test or run), **Reasoned**, **Unknown**.
Basis: `design-m8-document-intelligence.md` (3.3, 3.4, 6.2, 6.3, 9) and `adr-m8-document-intelligence.md`.
Scope: the persistent foundation only. **Nothing parses a document**, classifies, chunks, embeds or is exposed through MCP; no existing producer family, memory, assertion or ranking changed.

## 1. Discrepancies with the approved S1 definition (and how they were resolved)

| # | Design said | S1 does | Why |
|---|---|---|---|
| 1 | S1 = detection + adapter trait + text/Markdown adapter + tables | tables, anchors, citation contract and retention only | this task excludes parsing; detection, the adapter trait and the text adapter move to the next slice (they need nothing stored beyond what exists now) |
| 2 | document identity = `logical_sources.id` | same, but with a **new source kind `document`** (`SourceKind::Document`) | **Verified** while reading `record_observation_tx`: every `kind = 'file'` logical source under an observed root takes part in absent/move inference and in reconciliation by content hash. A document stored as `file` would be marked absent by the next directory ingest (binary files are not in its file list) and could be "moved" onto a text file with equal canonical text |
| 3 | (not mentioned) | `logical_at` (file-scope lookup by location) now filters `kind = 'file'` | without it a file ingest of a path equal to a document's location would adopt the document's logical source and reconcile memories onto it. This is the only change to existing lifecycle code; it is a no-op for all existing rows (every existing logical source of a file location has kind `file`). Test: `documents_do_not_enter_the_file_lifecycle_memories_or_retrieval` (mutation M8 fails it) |
| 4 | `doc_texts` table | dropped | the canonical text is `sources.content` of the revision; a copy would duplicate it |
| 5 | `retention` column on `doc_extractions` | dropped; retention is **the existence of a verified `doc_blobs` row** | a column could disagree with the table; the table is the truth, and an extraction can gain or lose retained bytes without rewriting an append-only row |
| 6 | `status`, `refusal_reason`, `limits_json` columns | dropped | design 7: a refused extraction persists nothing, so there is nothing to record |
| 7 | `UNIQUE(source_id, source_bytes_hash, extractor...)` | `UNIQUE(logical_id, source_bytes_hash, detector_version, extractor_id, extractor_version, params_hash)` plus `logical_id`, `text_is_source`, `structure_hash`, `node_count`, `source_size` | the key must not include the canonical-text revision: an extractor that answers differently for one identity must be **refused**, not stored beside the first answer (test `an_extractor_that_answers_differently...`). `text_is_source` is what makes a byte_exact claim about the *original* checkable |
| 8 | `doc_blobs` "only when retention is built" | built (small) | makes retention vs no-retention, erasure and `bytes_resolved` testable now |
| 9 | anchor `mind-doc:v1:<logical>:<bytes_hash[0..16]>:<extractor>@<ver>#...` | full 64-hex bytes hash, the **detector version**, `:<params-hash>` and a mandatory `;q=<precision>` field | a 16-hex prefix cannot name an extraction unambiguously, parameters are part of the identity, and precision cannot be derived from the other fields |
| 10 | node kinds as data | node `kind` is a schema `CHECK` over the design's 14 kinds | a new kind needs a migration; chosen so a corrupt/forged kind cannot enter. Revisit if kinds churn |

Not created (later slices): `doc_attributes`, `doc_classif_runs`/`doc_classifications`, `doc_relations`, `doc_chunks`.

## 2. Schema (`migrations/0010_documents.sql`, schema version 10)

* `doc_extractions`, `doc_nodes`: **append-only** (UPDATE/DELETE abort), CHECK constraints on hashes (64 lowercase hex), lengths, ranges, page numbers, boxes, and on **precision requiring its backing facts** (`byte_exact` needs a range, `page_only` a page and nothing else, `none` nothing).
* `doc_blobs`: content-addressed; deletable (erasure), never updatable; `length(bytes) = size` enforced.
* Indexes: the unique key above doubles as the anchor/extraction lookup index; a partial UNIQUE index `uq_logical_documents_location` allows one `document` logical source per location (it covers only the new kind and touches no existing row); `idx_doc_extractions_source` serves joins by revision.
* Triggers: an extraction must reference a `document` source of its own logical source; a node's text range must lie inside the canonical text. Char-boundary validity and hash agreement cannot be expressed in SQL and are enforced by the writer (and re-checked by the resolver).
* The tables lie outside the M6 write fence and history, like `scopes`. Consequence (**Verified** by reading the fence inventory; **Reasoned** for the rest): the logical source and the revision are recorded in the M6 history and require a recording revision; the doc rows themselves are append-only with `created_at` and are **not** part of M6 temporal reconstruction. There is no temporal coverage dimension for them yet.
* Existing data: the migration only creates objects. **Verified** (`an_existing_database_migrates_and_keeps_everything_it_had`): a schema-9 database with memories, assertions and code structure keeps every table row-for-row, search returns the same hits, `integrity_check` is `ok`, the FTS index verifies, re-opening is a no-op. Four existing tests hard-coded the latest schema version (`embeddings.rs` ×2, `manual_memory.rs`, `scopes.rs`) and `doctor.rs` simulated an older schema by deleting migration 9; they were updated to 10 (and the simulations also drop the document tables). No other test changed.

## 3. Identities (never conflated)

| Thing | Identity | Where |
|---|---|---|
| Document | logical source, kind `document`, one per location (`current_location`) | `logical_sources` |
| Revision | `(logical source, sha256 of the canonical text)` | `sources` (kind `document`, `content` = canonical text) |
| Original bytes | sha256 of the file as read; stored as a hash, bytes only on opt-in | `doc_extractions.source_bytes_hash`, `doc_blobs` |
| Extraction | `(document, original-bytes hash, detector version, extractor id, extractor version, params hash)`; also records `format`/`format_hint` | `doc_extractions` |

Consequences, each tested: equal text with different original bytes = one revision, two extractions (`equal_text_with_different_original_bytes...`); same bytes with a new detector version, extractor version or parameters = a new extraction, same revision, old anchors keep resolving (`the_same_bytes_with_a_new_extractor...`); changed text at the same location = a new revision of the same document, the old one and its citations remain (`a_new_revision_of_a_document...`); an identical repeat adds no row and consumes no recording revision. Moving a document to another location creates another document (no move inference in S1; limitation).

## 4. Anchors (`src/core/document.rs`)

`Anchor { logical_id, source_bytes_hash, detector_version, extractor_id, extractor_version, params_hash, node_path, text_range, page, bbox, precision }` with a single canonical text form `mind-doc:v1:<logical>:<bytes>:<detector>:<extractor>@<ver>:<params>#<path>;q=<precision>[;t=a-b][;p=n][;b=x0,y0,x1,y1]`.

* **Construction validates**: hashes, identifier spelling, depth, finite non-inverted boxes, page ≥ 1, and the shape each precision requires (byte_exact: a range; node_exact: a node; page_only: a page and nothing more; none: nothing).
* **Round trip is exact** (floats use Rust's shortest round-trip form) and **one spelling per anchor**: parsing re-renders and rejects any difference (case, order, leading zeros, spacing, duplicates, unknown fields). Tests: `anchors_round_trip...`, `malformed_and_non_canonical_anchors_are_refused` (24 refusals).
* **`downgrade` only lowers**: information is dropped; raising precision, or reaching for a page/node the anchor never had, is an error (`precision_can_be_downgraded_but_never_raised_or_invented`).
* **Resolution** (`resolve_document_anchor`): `None` if no extraction has the anchor's identity (wrong bytes/extractor/parameters/document: never resolved against a neighbour); `InvalidAnchor` if the anchor claims more than the stored extraction supports: a missing node, a page the extraction does not store, a bounding box the node does not store, a range outside the node or splitting a character, a precision above the node's. A byte range is exact **in the canonical text** wherever the node stores a range; the node's own precision is never upgraded.

## 5. Citation outcomes

`CitationStatus`: `text_resolved`, `bytes_resolved`, `source_present_verified`, `source_changed`, `source_missing`, `source_unavailable`. A `Resolution` reports `text_resolved` (the quoted text always comes from the database) together with exactly one **original** status. The store never reads the filesystem: the caller passes a `LiveObservation` (`Hash`, `Missing`, `Unavailable(reason)`) or none, so S1 adds no file authority.

* Retained and re-hashed -> `bytes_resolved` (no live check needed). A retained row that does not hash to its key is ignored and reported with a limitation.
* Otherwise: matching live hash -> `source_present_verified`; different -> `source_changed`; absent -> `source_missing`; not checked/unreadable -> `source_unavailable` **with the reason**. A hash is only ever compared, never used to retrieve anything.
* `source_precision` (what may be claimed about positions in the **original**) is `None` unless the original was verified. With a verified original: the stored precision, except that byte offsets of the canonical text count as byte offsets of the original only when `text_is_source`; otherwise `byte_exact` is reported as `node_exact` (or `none` with no node). Tests: `a_citation_reports_replacement_deletion_and_unavailability...`, `byte_exact_claims_about_the_original_need_the_text_to_be_the_original`.

## 6. Source retention and erasure

* Default: the canonical text is retained (it is the revision); original bytes are **not**. No bytes are stored unless the draft carries `retain_bytes`, which must have the declared size and hash and be ≤ 64 MiB.
* Opt-in retention is content-addressed (one row per hash, shared by every extraction of those bytes) and can be added later for an existing extraction without creating another one.
* `erase_document_bytes(hash)` removes the blob. Extractions, text, nodes and anchors stay; citations stop reporting `bytes_resolved` and say `source_unavailable` with the reason. It is not a recorded write (no recording revision). Test: `original_bytes_are_kept_only_on_request...`.
* **Not erasable in S1:** the canonical text (`sources` is immutable by design) and the extraction rows. Erasing a document's text/metadata for privacy needs a reviewed mechanism; recorded as an open limitation, not silently promised.
* The API never claims historical byte retrieval for an unretained original: `source_unavailable`/`source_changed`/`source_missing` are explicit, and no code path returns original bytes except from a verified blob.

## 7. Atomicity

One call = one `begin_write` transaction = one recording revision: logical source, revision, extraction, nodes and (optionally) the blob. An identical repeat is detected before a revision is allocated. Verified by injecting **real SQLite failures** with `RAISE(ABORT)` triggers at five points (sources, extraction, first node, a middle node, blob): in every case all table counts, including `m6_revisions`, equal the pre-call state, the handle stays usable, and the retry after removing the fault stores everything. A vetoed COMMIT (`FAIL_COMMIT`) rolls back cleanly; a lost acknowledgement (`LOSE_ACK`) is reported as `CommitOutcomeUnknown` while the data is in fact committed, and a retry after reopening is idempotent. 25 hand-made invalid drafts (bad hashes, NUL, ranges outside or splitting a character, inverted/NaN boxes, missing parents, duplicate paths, precision without backing facts, text_is_source lies, wrong retained bytes) are rejected before any write and change no row.

## 8. Verification executed

See the final report section of the task; commands: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` (and with `--features spawn-gate`), `cargo test --locked --no-fail-fast`, `cargo test --test document_model`, the migration/doctor/scopes/embeddings/manual_memory suites, and 14 mutation runs against the new guards (section 9).

## 9. Final review (before the local commit)

The whole diff was read, including every deleted line. The deletions are: the import lists that gained the new types, the schema-version constant, and the file-scope location query (now `kind = 'file'`); tests that simulate older schemas were updated (they now also drop the document tables and the new index). Defects found by challenging the implementation with counterexamples, each now covered by a test that fails without the fix:

1. **The lifecycle view could not read a document source.** `get_source_lifecycle` parses `logical_sources.kind` into `LogicalKind`, which had no `Document`; reading any document failed with a column-type error. Fixed (`LogicalKind::Document`); test `the_lifecycle_view_of_a_document_source_works_like_any_other_source` (mutation M17).
2. **The detector version was recorded but not part of the extraction identity**, although the S1 contract requires extractor AND detector versions to participate. A detector change returned the old extraction silently. Now part of the unique key, the lookup and the anchor; test `a_new_detector_version_is_a_new_extraction...` (M1b).
3. **Free-text draft fields were unbounded** (a 100 MB title was accepted) and `format`/`format_hint` accepted any characters. Now bounded and restricted; test `free_text_fields_are_bounded...` (M16).
4. **Nothing guaranteed one document per location**; only application code did. A partial UNIQUE index now does; test checks it is unique and partial (M15 survived a name-only check and was strengthened).

5. **A flaky test, not a product defect.** One run in about fourteen failed two tests: `FAIL_COMMIT`/`LOSE_ACK` are process-global one-shot switches, so a commit of any concurrently running test could consume the fault armed by the veto test (and the veto test then saw a successful commit). Every test now holds a shared lock (inside `Db`), the hook-arming test holds it exclusively. 80 consecutive runs of the binary passed afterwards; the 14th run of a 40-run loop had failed before.

Counterexamples that held without changes (now permanent tests): 4-byte characters and every offset inside a multibyte character, as start and as end, for anchors and for drafts; 64 levels of `u32::MAX` ordinals stored, 65 refused; six concurrent writers of one extraction produce exactly one; byte-identical files at two locations share one retained blob, and erasing it removes the byte-level resolution for both while the text citations and anchors survive and no stale claim about the original remains; after erasure only an independent live check can restore `source_present_verified`.

**Blob sharing, stated plainly:** retention is by content. A second document with identical bytes that did not opt in reports `bytes_resolved` while the first document's blob exists, and `erase_document_bytes` removes the content for every document that has it.

Mutation checks of the guards: 22 mutants (identity ignoring bytes, detector, params; no pre-check; structure disagreement accepted; any blob accepted as verified; original-position claims ignoring `text_is_source` or the verification; non-canonical anchors; downgrade raising; the file-scope filter removed; fabricated page; character boundaries ignored in draft validation and in the resolver; a live hash mismatch reported as verified; unverified retained bytes; the document-source trigger, the append-only trigger and the location index removed; free-text bounds removed; `LogicalKind::Document` removed; erasure that does nothing): all caught.

## 10. Remaining S1 limitations

* No detection, adapter trait or adapter (next slice); drafts are built by hand in tests.
* No move inference for documents; no disappearance state; documents are not part of the observation/session tables.
* Document rows are not covered by M6 temporal reconstruction.
* Canonical text and extractions cannot be erased.
* Node kinds are a closed schema vocabulary.
* The resolver verifies retained bytes by hashing the whole blob on every call (≤ 64 MiB); a cache is a later optimisation.
* Anchor `bbox` coordinates are stored as 64-bit floats from 32-bit values and compared exactly; rotation/crop-box semantics are the adapters' business.
