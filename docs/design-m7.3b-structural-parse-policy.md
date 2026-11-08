# Structural parse policy and derivation states (design, nothing implemented)

Basis: `docs/m7.3b-parser-safety.md`, `docs/m7.3b-resource-safety.md`, the code in
`src/ingest/code.rs`, `src/ingest/stream.rs`, `src/storage/sqlite.rs` and migration
`0004_code_structure.sql` at `409dd22`. Labels: **Measured**, **Reasoned**, **Decision**
(needs an owner's choice before implementation).

## 1. Four separate layers

| Layer | Question | Where decided | Deterministic from content? | Outcome when exceeded |
|---|---|---|---|---|
| 1. Text ingestion | May this file or blob be read as text? | producer, `ProducerLimits.max_blob_bytes` (32 MiB, implemented) | yes | operation fails: `BlobTooLarge` (`--path` excludes the file) |
| 2. Structural parsing | May this *source file* be parsed into entities? | producer, `ParsePolicy` (below, proposed) | yes | **text is ingested, structure skipped, reported** |
| 3. Stream record | May this record travel? | writer and consumer (1 MiB frame, 96 MiB record, 512 MiB / 100k frames, implemented) | yes | operation fails |
| 4. Worker resources | May the process continue? | OS: `RLIMIT_AS`, cgroup `MemoryMax`, CPU and wall timeouts, kill | **no** | worker dies or is killed; operation fails, nothing persisted |

Layers 1-3 are policy: deterministic functions of the content (and the configured
numbers), so their results may be cached. Layer 4 is a safety net for what policy
misses; its outcomes depend on the machine and must never be cached as facts about a file.

## 2. Structural parse policy (layer 2): budgets and what counts

```text
ParsePolicy (policy revision p1) {
  max_source_bytes:  2 MiB   = 2,097,152
  max_entity_depth:  256
  max_output_bytes:  32 MiB  = 33,554,432
}
```
`code::parse_with_policy(language, source, &policy) -> ParseOutcome { derivation, skipped: Option<SkipReason> }`;
`code::parse` stays as the unlimited form for tests. The producer passes the policy
(`ProducerLimits` gains it). No new MCP tool, no change to CLI arguments or existing JSON shapes.

**Precise accounting** (the definitions are part of policy revision `p1`; changing them is a
new revision, which changes the skip identity, see 3):
* **Source size.** `source.len()`, the UTF-8 byte length of the text as read (after the lossy
  conversion of Git blobs). Skipped iff `len > max_source_bytes`; exactly 2,097,152 parses.
  Checked before the parser is created. Not counted: tree-sitter's own memory.
* **Entity nesting depth.** The number of enclosing *context levels* including the node itself,
  where a level is an entity (`mod`, `struct`, `enum`, `trait`, `type`, `fn`/signature, `const`,
  `static`, field, `use`) or an `impl` block (it extends qualified names although it is not an
  entity). A top-level entity has depth 1. Skipped iff some entity or impl has depth >
  `max_entity_depth`; depth 256 parses, 257 skips. Tracked as a counter in the traversal context
  (`Ctx`), tested when the level is entered, before its strings are built. Tree depth (nodes)
  is not limited: the walk is iterative.
* **Structural output.** A running total, in bytes, of every string that would be stored: for
  entities `name`, `qualified_name`, `syntax_path`, `parent_syntax_path`, `signature`,
  `visibility`; for imports `raw`, `target`, `alias`; for relationships `source_syntax_path`,
  `raw_target`; plus a fixed 64 bytes per entity, import and relationship record. The total is
  **charged before the strings are allocated** (the length of a qualified name is
  `parent_q.len() + 2 + name.len()`, known before `format!`; text copies are charged by their
  node length). Skipped iff the total would exceed `max_output_bytes`; the walk stops at that
  point.
* **On violation** (any of the three): the whole structural result is discarded (entities,
  imports, relationships, error count), the text path is untouched, and a skip is recorded
  (3). A partial list is never kept.
* **What the budgets do not guarantee.** They bound the *size of the structural output* and the
  *nesting* of entities; they do not bound resident memory. The parse tree (about 300 bytes per
  input byte on hostile input, Measured below) and transient text copies exist outside the output
  budget, and the walk's own working set is not counted. The only measured bound on RSS is the
  process-level limit of layer 4.

**Adversarial and boundary tests** (to write with the implementation):
source exactly at / one byte over 2 MiB; nesting 256 / 257 for `mod`, `fn` and `impl` chains;
output budget at S and S-1 where S is the accounted size of an unlimited run (budget made
configurable in tests); a 1 MiB identifier at depth 3 (qualified-name growth); 100k-entity wide
file; `use` with a 2 MiB list (large `raw`, charged twice); 1M call expressions (relationship
volume); deep nesting that is small in bytes (the 180 KB / depth 20,000 fixture); each in a
child process under `RLIMIT_AS` and checked for the typed skip, not for a crash; plus a
differential test that files under every budget parse exactly as `code::parse` does.

### Why a size cap alone is not enough (Measured, release build)
Files at exactly 2 MiB, `code::parse` (peak RSS / CPU):
| Input (2 MiB) | RSS | CPU |
|---|---|---|
| ordinary code (76k entities) | 104 MiB | 0.6 s |
| array of `1` | 519 MiB | 2.6 s |
| random punctuation | 422 MiB | 2.8 s |
| 1M-term chain | 632 MiB | 2.1 s |
| 1M nested braces | 646 MiB | 1.8 s |
So a 2 MiB cap bounds the *tree-sitter* cost to about 650 MiB and 3 s per file (against
1.5 GiB and 7 s at 5 MiB). The cap skips 15 of 94,212 real `.rs` files (0.016 %), all generated bindings.

But the output grows with nesting depth, not size (Measured, balanced nesting, tiny files):
| Nested `fn` depth | Source | RSS | CPU |
|---|---|---|---|
| 2,000 | 18 KB | 41 MiB | 0.35 s |
| 5,000 | 45 KB | 221 MiB | 2.1 s |
| 10,000 | 90 KB | 852 MiB | 9.4 s |
| 20,000 | 180 KB | **3.4 GiB** | **44.8 s** |
Nested modules: depth 20,000 gives 1.4 GiB. The cause is the output: each entity carries a
qualified name and the nesting makes name lengths grow with depth (O(depth²) bytes), plus
per-level source copies (Reasoned; the exact split was not profiled). A 180 KB file defeats a
2 MiB cap, hence `max_entity_depth` (256 is far above real code; Decision: the number) and
`max_output_bytes`.

## 3. Recording a skip without breaking older binaries

Three things are kept apart:
| Concept | Meaning | Where it lives |
|---|---|---|
| Parser identity `I` | extractor, extractor version, grammar, grammar version: *what code produces entities* | the four identity columns, unchanged (`mind-code / 3 / tree-sitter-rust / 0.24.2`) |
| Resource-policy identity `P` | *which budget refused the file*: revision `p1`, reason, limit | a suffix of `extractor_version` on **skip rows only**: `3+skip(p1:source_bytes>2097152)`, `3+skip(p1:entity_depth>256)`, `3+skip(p1:output_bytes>33554432)`, `3+skip(p1:requested)` |
| Derivation outcome | complete / recovered / failed / skipped | `status` column (existing values only) and, for skips, `P` |

**Status decoding and old binaries.** `CodeParseStatus::parse` rejects unknown values, and the
`structure`/symbol readers use it, so a new `skipped` value would make an older binary fail on
the row. Therefore **no new status value**: a skip row is stored with status `failed`,
`error_count` 0, no entities, and identity `I` + `P` in `extractor_version`. An old binary reads
it as a failed derivation with an unfamiliar version string; it neither crashes nor treats it as
current. New code maps `status = failed AND extractor_version CONTAINS "+skip("` to the outcome
`Skipped{reason}` (an accessor on the read type; the enum used for storage is unchanged). The
accessor, not the column, is the source of truth for reports.
(An old binary that ingests the same input sees `has_code_derivation(I)` false and simply parses
it without a cap; the new `complete` row then supersedes the skip row. That is self-healing,
not corruption.)

**No duplicate skip rows.** `UNIQUE(input_id, extractor, extractor_version, grammar,
grammar_version)` already makes the row idempotent, and the producer avoids resending work:
1. `known(I)`? (`has_code_derivation` with the plain identity) yes: reuse the derivation (as
   today); the policy is not consulted, so lowering a budget never removes existing structure.
2. else evaluate the size budget; if exceeded, `known(P_source)` with the current limit? yes: reuse;
   no: emit the skip derivation (persistence inserts one row).
3. else probe `known(P_depth)` and `known(P_output)` for the current limits (two indexed lookups);
   if either exists the file is known to violate that budget: emit/reuse the skip without parsing.
4. else parse under the policy; a depth/output violation produces the skip derivation with the
   matching `P`; otherwise a normal derivation.
Unchanged policy and unchanged content: step 1-3 hit, zero parsing, zero new rows.

**Reuse and invalidation**
| Row | Reused when | Invalidated (re-evaluated) when |
|---|---|---|
| Complete / recovered, identity `I` | `I` is the current parser identity | parser version/grammar changes (new `I`); never by a policy change |
| Deterministic failure (status `failed`, plain `I`) | `I` current | parser version changes |
| Policy skip, identity `I`+`P` | same reason and same limit value, same parser version, same policy revision | any budget value for that reason changes (different `P`), parser version changes (different `I`), revision changes |
| Transient failure (cancel, timeout, kill, OOM, crash, setup error) | never stored | n/a: simply happens again on the next ingest |
Skip rows are append-only and ordered by `created_at` like every derivation, so a later `complete`
row (cap raised, file now eligible, parser changed) is the one readers show; the skip row stays
as history.

**Hiding on upgrade.** After a parser-version change, a file that violates a budget gets a skip
row under the new `I`, which is newest and hides the old version's entities. This is the
existing latest-wins rule; it is reported (the skip counter) and its text is intact.

**Reporting.** The producer sends the skip as an ordinary `code_input` record whose derivation
carries `status: failed` and the `P`-bearing identity; the frontend counts it. Additive only:
`CodeObservationReport.skipped` and a line in the ingest output ("structure skipped for N source
files: <reason>; their text was ingested"), and the `structure` view prints "skipped (reason)".
Existing fields and shapes are unchanged. Decision: whether the counter name/line is acceptable
as additive output.

**Failed vs transient.** `code::parse` returning no tree is currently stored as `failed`. That
case (language setup error, or a timeout/cancel if one were configured) is not a property of the
content: it must become an operation error and must not be cached under `I`. A CPU backstop
(tree-sitter progress callback) that fires is likewise an operation error. Only deterministic
outcomes are stored.

## 4. Recovery and compatibility

| Event | Result |
|---|---|
| File grows beyond the cap | New content = new code input: skipped under `P`; the old input keeps its derivation; text ingested; reported |
| File shrinks below the cap | New input, parsed normally |
| Cap raised | Inputs with only a skip row: `known(I)` false, `known(P_new)` false, parse runs, `complete` row added, newest wins; those still above get a skip row with the new limit |
| Cap lowered | Existing `I` rows stay valid (step 1); unparsed inputs may now be skipped (new `P`) |
| Parser version change | `I` and `P` change; all re-derived under the current policy |
| Failed derivation retried | Deterministic failures retry only on a new `I`. Transient failures were never stored |
| Worker crash mid-parse | No closing record, `consume` fails, nothing reaches the store (tested for the stream). The next ingest repeats the work; see 5 for the explicit retry |
| Partial database state | None: persist runs after the whole stream validated |
| Text ingestion | Unchanged in all cases |

## 5. Progress record and explicit retry (diagnostic, never automatic)

**Progress record** (new stream record type, additive to the grammar):
```text
Progress { seq: u64, blob_oid: Option<String> /* git */, path: String /* repository-relative, <= 4096 bytes */,
           source_bytes: u64, stage: "structural_parse" }
```
* Emitted by the producer immediately before a structural parse starts, only for sources above a
  threshold (default 64 KiB; smaller files cannot take long), so the stream carries a few records
  per second at most. Bounded: fixed fields, path length checked as for any path, oid format
  checked.
* The consumer keeps **only the latest one** (O(1) memory), overwritten by the next, cleared when
  the operation completes. It is never part of `Extracted`, never persisted, and not counted in
  the closing counts beyond the record total.
* On a failed stream (truncated, worker killed, timeout, resource kill) the typed error includes
  "last reported structural parse: `<path>` (`<oid>`, N bytes)". This is diagnostic text: the
  worker is untrusted and may name an innocent file, and the crash may have another cause
  (memory used earlier, a different thread, the kill by a limit). **It is not proof of
  causation and is never used to skip, blacklist or retry anything by itself.**
* The frontend validates the claim only for display: the path must be a source of the requested
  operation (same checks as any record path).

**Explicit retry without structure** (user decision, not automatic):
* `mind ingest ... --skip-structure <path>` (repeatable; CLI only at first, so a tool call cannot
  silently mask a file; an MCP argument is a later, separate decision). The path is resolved to
  the blob (Git) or content hash (directory) of the operation's current state.
* Effect: for that blob only, structural parsing is not attempted; the text is ingested as
  usual; a skip row with `P = p1:requested` is recorded and counted, so the result is visible in
  `structure` and in the report and the next ordinary ingest reuses it (`known(P_requested)` is
  probed in step 2-3) instead of crashing the worker again.
* Undo: `--retry-structure <path>` evaluates the blob again under the policy, ignoring a
  `requested` skip; a successful parse adds a newer `complete` row.
* Nothing is blacklisted automatically: a crash by itself changes no stored state; only the
  user's explicit flag creates a `requested` skip, and it applies to content (blob/hash), not
  to a path pattern.
* Tests: crash fixture with a progress record naming the file; error text carries it; retry flag
  produces the skip row, text intact, report counts it; the next plain ingest does not parse;
  `--retry-structure` parses again; a worker that names another file changes nothing stored.

## 6. Validation plan (to implement with the cap)
* Unit (`code`): exactly 2 MiB parses; 2 MiB + 1 is skipped with `SourceBytes{size, limit}`;
  depth 256 parses, 257 skips; output budget boundary; ordinary files unchanged (differential test
  against `code::parse`, as for the iterative traversal); pathological 2 MiB inputs from the
  table parse within a stated budget (checked in a child process with `RLIMIT_AS`).
* Persistence: skip row stored as `failed` with `I`+`P`; an older-binary read test (status decode of the row); second ingest reuses it with zero parses and zero new rows; raising the cap adds a
  `complete` row and `code_structure` shows it latest; lowering the cap keeps rows; parser version
  bump re-derives; M6 availability/history tables stay consistent (invariants test).
* Stream: skip `code_input` round trip; `progress` record round trip, bounds and overwrite; counts;
  hostile variants (skip record carrying entities, a `P` that does not match the policy, a progress
  record with a hostile path).
* Git: fast-forward ingest adds a blob above the cap (skipped, text kept, ref events unchanged);
  the same blob reached from many paths is skipped once; repeat ingest sends no parse work.
* Directory ingest: same for files; `--path` selection; a file growing across the cap between
  runs.
* Failure: worker killed mid-parse (child process, SIGKILL) leaves the store untouched; cancel
  before/after a skip; an `RLIMIT_AS` abort mid-parse; deterministic `failed` is cached, transient
  failure is not.
* Reporting: counter `skipped` in CLI and MCP outputs, `structure` output, doctor unaffected.

## 7. Limits of the defaults
* 2 MiB bounds tree-sitter to about 650 MiB / 3 s per file (Measured). With the depth and output
  budgets, the quadratic output cases are cut off; their numbers at the proposed budgets are
  Unknown until implemented.
* Per file sequential cost multiplies: 1,000 hostile files of 2 MiB cost about 47 minutes of CPU;
  the operation deadline and the worker's wall limit still apply (layer 4).
* Parsing time is not bounded inside one parse; layer 4 (kill) remains necessary. `RLIMIT_AS` of
  about 1.25 GiB covers a 2 MiB hostile file (about 650 MiB RSS plus runtime and Git),
  Reasoned from the table, not tested at that value.
* The text path keeps the 32 MiB blob limit, so a 10 MiB `.rs` is ingested as text and skipped
  structurally.

## 8. Schema impact
No migration and no new status value. Code changes: `ParsePolicy`/`SkipReason`,
the `P` suffix on skip identities, an outcome accessor on the read type, `CodeObservationReport.skipped`,
the `progress` stream record, the `--skip-structure`/`--retry-structure` options. Evidence for a
migration: none found. If queryable skip reasons are wanted later, a dedicated column would be a
migration; the identity suffix avoids it at the cost of parsing a version string.
Compatibility: older binaries keep reading the database (skip rows look like failed derivations),
and, unaware of the cap, replace a skip by a parse when they ingest the same input.
