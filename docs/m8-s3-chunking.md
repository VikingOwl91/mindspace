# M8 S3: token-safe document chunks

Repository basis: S2 is actually `0bcf447ea95519c657068c1f42c0a13b135514f7`;
the supplied `0bfc447` did not resolve. Verified lineage from `92f68ff` includes
`5e52de0`, `5e22642`, then S2. S2 and the original working tree were clean.
Branch `feat/m8-s3-chunking`, worktree `/tmp/mindspace-m8-s3`, was created at S2.
The initial handoff was uncommitted; the final review below authorizes the local component commit. All 28 registered worktrees were checked; only S3 has source
changes. Main and origin/main remain `92f68ff`. Nothing is merged or pushed.

Implementation boundary decided after repository reconnaissance, before persistence implementation:
**PARTIAL** against the full S3 objective. No retrieval projection or S4.

The approved design section 8 uses a 3,000-byte heuristic and postpones visible truncation;
this request supersedes that heuristic with actual token validation. The design's S3 also
includes assertion projection. Actual `reconcile_logical` isolates both logical id and producer
family, whereas S2 creates separate `file` and `document` identities at a shared location.
Adding `doc:chunk@1` would leave `file:legacy` active. Cross-kind ownership and retirement
would need a separately reviewed lifecycle change. This slice therefore stores derived chunks
only, with no evidence, assertion, memory, vector or search entry. Default ingestion and MCP
remain unchanged. This follows the user's explicit partial-delivery fallback.

The design's cross-revision stable-id assumption is narrowed: identities are revision-bound;
unchanged ranges cannot claim the same citation identity in another extraction.

## Token contract

The only compiled-in embedding configuration is Nomic v1.5, revision
`e9b6763023c676ca8431644204f50c2b100d9aab`. The supported runtime policy limit is
**2,048 total tokens**, although Candle config has 8,192 positions. It is not an
assertion that the model architecture supports only 2,048 positions.

`TokenBudget::from_file` checks the tokenizer's 711,396-byte size and pinned SHA-256
`d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66` before parsing.
The contract disables tokenizer truncation and padding, encodes the complete
`search_document: {source slice}` with special tokens enabled, and checks the result
against 2,048. Query validation uses `search_query: `. Both empty inputs encode to
6 tokens (four prefix tokens and CLS/SEP); the usable payload is assessed by full
encoding, never by subtracting a presumed constant or converting bytes to tokens.
`hello ` repeated 2,041/2,042/2,043 times encodes to 2,047/2,048/2,049 tokens.
An over-budget input yields `Error::InvalidVector` with actual count, limit and
split instruction. Tokenization failure is propagated before any store write.

The budget identity hashes contract version `complete-input@1`, locked tokenizer
engine 0.23.2, model/revision, tokenizer hash, both prefixes, limit and flags. Engine
or policy changes require reviewing this identity/version. Only registered manifest
configurations are accepted; no arbitrary tokenizer or caller-selected limit is trusted.
No model weights, network or inference are needed for chunking. Missing tokenizer
artifacts cause an explicit error. With `--no-default-features`, chunk generation and
the strict embedding API are unavailable; existing document storage remains available.

`CandleRuntime::embed_document_validated` uses the very encoding validated by this
contract and rejects overflow before loading weights. The existing `EmbeddingRuntime`
trait retains legacy truncation behavior and embedding derivation/cache identities.
This compatibility boundary is deliberate: the strict method is explicit and no new
path calls legacy embedding. No vectors are persisted by S3. Future projection must
call the strict method and include the budget/chunk identity in its derivation.

The tokenizer's lowercasing, control handling, accent normalization and WordPiece
`[UNK]` for words over 100 characters remain actual model behavior. They do not modify
canonical text or citation ranges. Complete token coverage does not promise the model
can semantically distinguish every preserved character.

## Chunk representation

`heading_aware@1` partitions canonical text into contiguous nonempty half-open UTF-8 byte
ranges. Empty text has an empty completed set; whitespace is source content. No overlap,
inserted separator, heading copy, trimming or newline normalization occurs. CRLF remains CRLF.
BOM handling is S2's existing extraction behavior, before this stage. Section paths containing
the entire chunk are metadata; headings outside the range are never attributed to the quote.
Every chunk payload is exactly its canonical source slice. A bounded 16 KiB candidate window
limits tokenizer-call work; this is a work bound, not a token estimate. Oversized windows halve
on character boundaries until they fit. Within that fitting window the latest section boundary
is preferred, then a block boundary; otherwise nearby whitespace is preferred. All changed
candidates are counted again: WordPiece token counts are not monotonic. A final validation
checks every emitted chunk. Long uninterrupted tokens may be split on character boundaries.
S3 additionally requires ranged sections to form a consistent hierarchy: overlapping
siblings/crossing intervals and descendants outside a ranged ancestor are refused before
persistence. Empty ranges add no context. This bounds active context to one ancestor chain
(at most S1's 64 path components; S2 headings have at most six levels). S1 draft storage
validation and legacy ingestion are unchanged.

Controls including zero-width and bidi characters are preserved. This library adds no display
surface; any future renderer must escape controls for display without changing citation bytes.

## Retrieval projection contract (deferred)

A later proposal must establish one owner for each physical file across both ingestion paths,
including legacy-after-document ingestion; validate aliases and location changes; explicitly
retire competing producer families and versions; bind each projected assertion to a chunk and
S1 anchor; reconcile spaces, source removal and temporal history through the existing lifecycle;
and atomically publish assertions and optional vectors only after all embedding work succeeds.
It must test both ingestion orders and version upgrades. Merely choosing another producer
family does not satisfy these requirements. No broad migration is authorized here.

## Applicability

Local Rust library/storage feature, no new web application, external data processor, MCP tool,
sandbox authority or inference requirement. Website-compliance skill reviewed: no website
surface is changed, and operator/legal facts are not invented. No release/deployment requested.

## Persistence and identity

Explicit library APIs: `ingest_chunked_document_file` reads/extracts a file;
`Store::record_chunked_document(draft, budget)` builds and validates the plan before
starting the SQLite write. `SqliteStore::list_document_chunks(extraction_id)` returns
stored ranges, context, token counts and root S1 anchors (root allows spans across nodes).
Existing `ingest_document_file` keeps its S2 behavior.

Migration **0012** adds `doc_chunk_sets` (one completed derivation per extraction,
chunker version and budget identity) and `doc_chunks` (ordinal, range, token count,
context, anchor). Both are immutable under update/delete triggers. A range trigger
rejects ranges beyond canonical text using SQLite byte-length metadata (`octet_length`),
without fetching the entire canonical TEXT for each inserted chunk. The set persists even if it contains no chunks.
No text payload is duplicated: canonical text is referenced by extraction.

Chunk ID is SHA-256 of canonical JSON containing logical document UUID, original-byte
hash, canonical-text hash, detector/extractor versions and id, extractor parameters,
structure hash, chunker version, budget identity, range and context. Timestamps and
random extraction/source UUIDs are excluded. Equal text at different logical documents
has different IDs. Equal derivation at the same logical document reuses IDs. A revision,
extractor or chunker change cannot silently reuse them. This is not a portable identity
across separately created databases with different logical document UUIDs, nor a
cross-revision stability promise. A reversion reuses the original immutable extraction.

The same `begin_write`/commit-outcome machinery stores document, source revision,
extraction, nodes, retained bytes (if opted in), set and chunks as one recording revision.
Repeat prechecks compare text, structure and plan; an inside-transaction recheck closes
the independent-connection race, rolling back the unused recording revision. Errors
roll back everything. Unknown commit outcomes remain `CommitOutcomeUnknown`; reopen
and repeat is idempotent. No retry guesses whether a commit happened.

S3 creates no competing producer family and does not retire or rewrite `file:legacy`.
Lexical/semantic/hybrid/temporal retrieval continue to see the existing assertion spine;
chunks are not searchable in this partial slice. No default MCP definition changed.

## Measured resources (release, local machine)

These observations are workload-specific, not enforced latency or memory guarantees.
The ignored measurement tests reproduce them; normal tests require no installed weights.

| Canonical input (16,777,216 bytes) | Whole tokenization | Chunk generation | Chunks | Summed encoded chunk tokens | Range/context JSON bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| English prose | 5.967 s | 20.975 s | 2,057 | 3,285,942 | 54,867 |
| CJK prose | 7.912 s | 42.766 s | 4,145 | 5,495,700 | 110,565 |
| Uninterrupted `x` | 3.284 s | 5.737 s | 1,024 | 7,168 | 24,240 |

Extraction before these timings: 143/92/37 ms respectively. Whole-document tokenization
is a separate benchmark operation, never performed by the chunker. The measurement
process peak RSS reached **2,733,552 KiB**; it includes the deliberately whole-document
tokenizer measurement and allocator retention across cases, so it does not isolate the
chunker's peak. The first prose peak was 1,886,408 KiB. No worker memory limit is added.
The bounded 16 KiB windows avoid encoding the entire document per chunk; structural
boundaries use ordered sets and section-context events rather than rescanning all nodes.

A 1,048,530-byte document produced 129 chunks / 195,501 encoded tokens. Derivation and
write took 1,553 ms; the document-only database was 5,722,112 bytes and adding chunk
storage added 94,208 bytes (SQLite page accounting; not a universal compression ratio).
Embedding work for larger inputs is the chunk count shown above, not measured full
16 MiB inference. Actual strict-path inference on pinned local weights returned eight
finite 768-dimensional vectors: warm prose/code/Unicode/control samples took
294/341/232/125 ms, at 142/198/118/30 tokens. First prose call including load took
608 ms. That separate test process peaked at 1,092,200 KiB RSS.

An initial tiny-paragraph CJK 16 MiB fixture hit S2's 500,000-node limit and was refused
before chunking. The measured CJK fixture above uses longer paragraphs. This demonstrates
that 16 MiB is an input-byte ceiling, not a guarantee every structure at that size is
accepted. An initial inference measurement test failed to compile due to SHA output
formatting; it was corrected and rerun successfully. Full-suite linking initially hit
`LLVM ERROR: IO failure on output stream: Disk quota exceeded`; generated debug artifacts
were moved intact from tmpfs to the workspace SSD before rerunning. No existing worktree
or branch was removed.

The tokenizer test fixture is the exact Apache-2.0 Nomic tokenizer from the pinned
manifest revision and hash above, copied from the already installed verified artifact.

## Verification and limitations

Required commands use `RUSTC_WRAPPER=` because the configured sccache is unavailable
inside this sandbox. The final full-suite run uses a fresh SSD target and
`CARGO_PROFILE_DEV_DEBUG=0` to bound debug artifact size. Disposable Git fixture signing
is disabled via `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign
GIT_CONFIG_VALUE_0=false`; no user/repository Git setting is edited. Tests needing
loopback sockets/process confinement run with the authorized test harness. Restricted
runs returned `Operation not permitted (os error 1)` and a signing-agent read-only error;
these are excluded from pass claims. A shared debug artifact directory was also exposed
to an early mutation snapshot; its test outputs are excluded. Definitive mutations use
both a distinct package identity and an independent target directory; final full-suite
verification uses another fresh directory.

Generated tests check exact reconstruction, positive ranges, UTF-8 slicing, no gaps or
overlap, actual final token counts and deterministic context on 100 generated Markdown
documents, plus adversarial paragraphs/lists/code, nested and tiny sections, unusual
scripts, emoji, combining characters, CRLF, bidi and zero-width controls. Empty canonical
text has an empty completed set; S2's file adapter continues to refuse empty/whitespace
input. Anchors after replacement/deletion resolve retained canonical text, and reject
wrong extractor identities. Storage tests exercise both legacy ingestion orders and
actual explicit file ingestion. SQL failure at a middle chunk, vetoed commit, unknown
commit acknowledgement, migration-12 rollback and a populated schema-11 upgrade are
covered. Repeated six-writer concurrency ran **30/30** successfully in release.

Independent Sol review found one LOW test gap in prior-version coverage; it was fixed
with populated prior-version rows. The exact test passes and a mutation omitting the
chunker version from chunk ID fails with a unique-key collision. Final review has no
remaining static findings.

Definitive mutation checks: **9/9 killed**, all compiled and reached their intended
assertion. Mutations introduced a coverage gap, overlap, bypassed initial/final budget
checks, omitted prefix, omitted special tokens, omitted document identity, omitted
extractor version, swallowed a chunk insertion error, and omitted chunker version.
The unmutated baseline passed. Earlier shared-target and disk-quota mutation attempts
are excluded. Reproduction script and individual logs are under the original workspace
`target/m8-s3-mutation-evidence/` (local generated evidence, not committed source).

Remaining limitations: no retrieval projection or temporal history of chunks, no memory
migration, no parser beyond S2, no inferred graph/classification, no semantic stability
claim across revisions. Boundaries may split grapheme clusters or uninterrupted code
identifiers while retaining complete UTF-8 characters. Tokenizer normalization/UNK can
lose model distinctions even though citations retain bytes. Chunking requires the pinned
tokenizer and the `embeddings` feature, but not model weights or inference. The canonical
text/chunk sets inherit S1's immutable retention policy. The measurements do not establish
a hard RSS/latency guarantee or a full-corpus embedding benchmark. No S4 work is included.

Schema compatibility is forward migration from 11 to 12; no down-migration is provided.
Rollback evidence refers to failed upgrades and failed ingestion transactions, not opening
a schema-12 database with an older binary. No live user database was migrated in this run.

## Changed files

- Token contract/runtime: `src/embeddings/budget.rs`, `src/embeddings/mod.rs`, `src/embeddings/runtime.rs`.
- Chunker/explicit file entry: `src/ingest/document/chunk.rs`, `src/ingest/document/mod.rs`.
- Store API/SQLite implementation: `src/storage/mod.rs`, `src/storage/sqlite.rs`, `src/storage/sqlite/documents.rs`.
- Schema: `migrations/0012_doc_chunks.sql`.
- New verification: `tests/document_chunking.rs`, `tests/document_chunks.rs`, `tests/token_budget.rs`, `tests/fixtures/nomic-tokenizer.json`.
- Existing schema-version/upgrade fixtures: `tests/doctor.rs`, `tests/document_ingest.rs`, `tests/document_model.rs`, `tests/embeddings.rs`, `tests/manual_memory.rs`, `tests/scopes.rs`.
- This report: `docs/m8-s3-chunking.md`.

A plain `cargo build --locked --release --bin mind` succeeded. Its binary contained
none of the scanned spawn/storage/Git-worker/document-chunk gate marker strings;
new S3 production code adds no test hook. The existing commit hooks remain behind
`spawn-gate`. `cargo check --locked --no-default-features` also passed. Required
strict Clippy passed in the final independent build directory.

## Initial handoff decision and gate results

**PARTIAL — ready for review of chunking/persistence only.** Retrieval projection is
deferred under the explicit fallback, for the producer-ownership reasons above.

Final source verification:

- `cargo fmt --check`: passed; `git diff --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed in fresh independent target.
- `cargo test --locked --no-fail-fast`: **1,015 passed, 0 failed, 8 ignored**, exit 0,
  fresh SSD target `target/m8-s3-clean` with the environment described above.
- Targeted release suites: **202 passed, 0 failed, 5 ignored** across document chunking,
  chunk storage, token budget, S1/S2 documents, embeddings, Git persistence atomicity,
  general persistence atomicity, lifecycle, M6 temporal, MCP and retrieval. The final
  strengthened storage suite was rerun in release: **13 passed, 0 failed, 1 ignored**.
- Three new ignored resource measurements were each run explicitly in release and passed:
  16 MiB chunk generation, 1 MiB storage overhead, real pinned-model inference.
  The five inherited ignored tests remain excluded from default-suite claims.
- Release concurrency: **30/30** runs of six independent connections passed.
- Mutations: **9/9** killed in isolated builds after clean baselines.
- Independent read-only Sol review: no remaining findings.

Local logs: `/tmp/m8-s3-clean-full.log`, `/tmp/m8-s3-clean-clippy.log`,
`/tmp/m8-s3-release-tests.log`, `/tmp/m8-s3-release-storage-final.log`,
`/tmp/m8-s3-perf-final.log`, `/tmp/m8-s3-storage-perf.log`, and
`/tmp/m8-s3-concurrency.log`. Earlier shared debug results are excluded. Trusted-executable
fixtures use a private SSD directory via the ignored worktree `target` link, avoiding
world-writable `/tmp` ancestry; the actual trust policy is unchanged. No existing build
artifacts were deleted to recover space.

At the initial handoff all implementation changes remained uncommitted.
The following final-review section supersedes the initial commit status and gate counts.


## Final review: findings and fixes

The full 20-file initial diff, including deletions and the complete pinned tokenizer JSON,
was independently reviewed. Two narrow issues were then demonstrated and fixed:

1. **Repeated whole-text reads in the new SQL range trigger.** `length(CAST(s.content AS BLOB))`
   materialized canonical text once per inserted chunk. The final migration uses
   `octet_length(s.content)` instead. Bundled SQLite **3.53.2** supports it and the actual
   INSERT trigger's EXPLAIN bytecode reads byte-length metadata (`Column P5=192`). A new
   regression tests that mechanism and rejects an out-of-range Unicode byte offset.
2. **Contradictory public-draft section trees.** S1 accepts distinct root section nodes
   covering the same range. With 16/64/256 such sections on 64 KiB prose, the initial
   chunker emitted 128/512/2,048 context paths for eight chunks, a node-count × chunk-count
   product. `validate_section_ranges` now refuses contradictory overlaps and misplaced
   descendants at the S3 boundary, before tokenization or database writes. Equal-range
   ancestors, adjacent siblings, empty ranges and S2-generated structure remain accepted.
   Regressions verify refusal and unchanged database row counts. No source text is dropped
   to enforce the bound. Valid-input chunk boundaries, payloads and identities are unchanged.

Repeated tokenizer work itself is bounded: each input window is at most 16 KiB; geometric
shrinking encodes less than two windows, and preferred/final checks add at most two more.
There is no full-document encoding per chunk. For C chunks, M nodes and fixed window W,
tokenization is O(C·W), section preparation is O(M log M) with bounded path depth, and
context maintenance is O(M + C·D), D ≤ 64. Some inputs are encoded repeatedly by a constant
factor; removing the final validation was not needed to fix the demonstrated causes.

The **pre-existing S1 node range trigger** also reads full text per node. It is unchanged
and remains a possible ingestion bottleneck. These findings do not establish linear
end-to-end ingestion performance. No unrelated schema/runtime optimization was included.

### Final isolated chunk scaling

Measured with the final release library, three fresh processes per case, no inference or
whole-document tokenizer benchmark. Values are median chunk-generation milliseconds;
RSS is the maximum process high-water mark immediately after generation across the three
runs, including tokenizer loading, extraction and retained output. Coverage, real token
counts and context containment are then checked outside the timed operation. Other build
and test jobs ran concurrently, so these are observations, not latency thresholds.

| Input | 256 KiB ms | 1 MiB ms | 4 MiB ms | 4 MiB chunks | 4 MiB peak RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Prose | 327.971 | 1,291.996 | 5,402.468 | 515 | 41,612 |
| CJK | 753.632 | 2,755.745 | 12,232.993 | 1,037 | 34,824 |
| Uninterrupted text | 91.409 | 355.125 | 1,420.130 | 256 | 24,764 |
| Mixed Markdown | 707.332 | 2,594.354 | 11,612.997 | 1,031 | 91,672 |

A fourfold input increase took approximately 3.7–4.5× chunk-generation time in these runs;
that is consistent with bounded repeated tokenization, not a proof for arbitrary inputs.
Every measured chunk passed the full prefixed token check (largest observed count 2,046).
The separate boundary suite exercises exactly 2,048 and rejection of 2,049. Context is
paths only and adds **zero text** to the embedding payload. A regression confirms that
adding heading text to an exactly full payload requires a fresh combined-input validation
and is rejected when it exceeds the ceiling. No context text is falsely included in a quote.

### Final isolated SQL scaling

Actual migration SQL, bundled SQLite 3.53.2, in-memory minimal prerequisite tables,
three fresh processes per case, fixed 8 KiB ranges. Median insertion milliseconds:

| Source MiB / synthetic chunk rows | Original CAST trigger | Final octet_length trigger |
| --- | ---: | ---: |
| 1 / 128 | 4.533 | 0.600 |
| 2 / 256 | 16.462 | 1.231 |
| 4 / 512 | 63.025 | 2.484 |
| 8 / 1,024 | 671.108 | 4.936 |
| 16 / 2,048 | 4,129.853 | 10.290 |

This isolates chunk insertion; it excludes S1 structure writes, tokenization, embedding,
assertions and storage on disk. The old/new expression alone is toggled in the probe,
without changing a real database. It demonstrates the removed mechanism without making
an end-to-end throughput claim. Source and reproduction instructions are committed in
`docs/experiments/m8/s3_scaling.rs` and `docs/experiments/m8/s3-scaling.md`.
Raw local observations: `/tmp/m8-s3-final-scaling-fixed.jsonl`, `/tmp/m8-s3-final-sql.jsonl`.

### Bounded follow-up: ownership and retrieval projection

This is a separate reviewable follow-up, not S4 or an implementation in this commit:

1. Decide and document an ownership key across `file` and `document` logical identities,
   scopes/spaces, aliases and moves. Content equality alone cannot establish ownership.
   Specify both legacy-first and document-first behavior, including later legacy re-ingest.
2. Design one atomic ownership handover and active-state reconciliation: retire competing
   producer families/versions explicitly and prevent subsequent ingestion from resurrecting
   a second active set. Require dry-run counts and rollback/unknown-outcome acceptance tests.
3. Project approved chunks into the existing assertion/memory spine with S1 anchors,
   spaces, strict full-payload embeddings and versioned derivation identity. Reuse existing
   lexical/semantic/hybrid ranking. Publish only after required derivations succeed.
4. Specify temporal behavior for edits, removal, reversion and producer-version changes;
   preserve old citations/as-of history through the existing lifecycle. Test both ingestion
   orders, retries, concurrency and failures. S3's immutable side tables alone do not provide
   this active/as-of retrieval contract.

Acceptance requires exactly one active memory set for an owned file in both ingestion
orders, explicit handling of pre-existing legacy memories, correct as-of results and old
citations, and no change to default MCP definitions. Existing `reconcile_logical` filters
both logical id and immutable producer family, so a `doc:chunk@1` run cannot retire
`file:legacy` rows attached to another logical id. No such handover is attempted here.

### Final-tree acceptance and local commit

**Chunking/persistence: ACCEPT. Overall S3: PARTIAL** because retrieval projection and
cross-kind lifecycle policy remain deferred. This final review authorizes a local commit
of the component; it does not authorize merge, push, deployment or S4.

Final verification executed after both fixes:

- `cargo fmt --check`, standalone probe `rustfmt --check`, and `git diff --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked --no-fail-fast`: **1,019 passed, 0 failed, 8 ignored**, exit 0.
  The four new regression tests explain the increase from the initial 1,015.
- Fifteen targeted release suites (chunking, chunk storage, token budget, S1/S2 documents,
  retrieval, persistence/Git atomicity, lifecycle, M6 temporal, embeddings, MCP, doctor,
  scopes and manual memory): **260 passed, 0 failed, 5 ignored**, exit 0.
- Fresh release concurrency: **30/30** six-connection runs passed.
- Final isolated mutation baseline: **25 passed, 0 failed, 3 ignored**. **11/11 mutants**
  compiled and reached their intended failing assertion, including bypass of section
  validation and restoration of the full-text CAST trigger. No infrastructure failure
  was counted as a killed mutant. Scripts/logs are in the original workspace's
  `target/m8-s3-final-mutation-evidence/`.
- Final explicit 1 MiB storage measurement: 129 chunks, 195,501 encoded tokens,
  1,425 ms derivation/write, 94,208 bytes added to a 5,722,112-byte document database.
  The final 36-case chunk scaling and 30-case bundled-SQLite matrix above also passed
  their coverage/token assertions. These do not claim full 16 MiB embedding coverage.
- Plain release production build and gate-marker scan passed. No-default-features check
  passed. No test-only S3 hooks were added to production code.
- Independent read-only review of every file/deletion and the final fixes: no remaining
  findings. Benchmark arithmetic and raw-data correspondence were independently checked.

The same authorized local test environment, signing-disabled disposable Git fixtures,
independent SSD build directories and pinned tokenizer described above were used.
No live user database was changed. Migration 0012 remains a pre-publication migration,
with no schema downgrade. New reproducible benchmark files are the only additions to
this report's earlier changed-file list.

Final logs: `/tmp/m8-s3-final-review-full.log`, `/tmp/m8-s3-final-review-clippy.log`,
`/tmp/m8-s3-final-review-release.log`, `/tmp/m8-s3-final-concurrency.log`,
`/tmp/m8-s3-final-storage-perf.log`. Commit SHA is reported separately so this document
does not embed its own commit identifier. Stop for review after the local commit.
