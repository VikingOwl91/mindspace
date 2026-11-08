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

## 9. Pre-implementation validation (results; added after approval of `p1` defaults)

Approved: `p1` defaults (2 MiB / depth 256 / 32 MiB output), additive `skipped` counter, skip rows
stored as `failed` with a policy-bearing identity, no automatic blacklist, explicit retry/skip.

1. **Budget accounting and allocation order (checked against `code.rs` at `6791b4a`).** The
   allocations in `visit`, in the order they happen, and what must be charged before each:
   * on **every** node: `parent` and `parent_q` are cloned to `String`s from the shared `Rc<str>`.
     Uncharged and O(depth x name length) per node; the implementation should borrow (`&str`).
   * `call_expression`: `raw_target = text(function)` copies the *whole callee expression*.
     **Measured quadratic**, new finding: `a.b().b()...` (40 KB, 10,000 calls) peaks at 205 MiB
     (2,000 calls 14 MiB; 5,000 calls 57 MiB; the memory is the retained `raw_target`s). A 2 MiB
     file of this shape reaches hundreds of GiB (Reasoned, quadratic extrapolation). So the output
     budget must charge `raw_target` by node length (`end_byte - start_byte`) **before** `text()`.
   * `impl_item`: two `text()` copies and two `format!`s: charge by node length and formula.
   * entity: `name` copy, `path` and `q` (`format!`; length `parent_q.len() + 2 + name.len()` is
     known first), and `raw = text(n)`, a copy of the *whole entity* only to read its first
     line and prefix; it can be a borrowed slice (no allocation, and it removes a per-nesting-level
     copy). `use`: `raw` is stored twice (import and signature) plus the targets: charge 2 x length
     before copying.
   Rule for the implementation: `charge(bytes) -> Err(Skip)` is called with the exact or an upper
   bound of each allocation before it is made; a failed charge aborts the walk and drops the draft.
   Budgets constrain the output, not RSS (the tree is outside them).
2. **Skip idempotency (Measured).** A throwaway program wrote a policy-skip row (`failed`,
   `extractor_version` `3+skip(p1:source_bytes>2097152)`) through the real store API twice: the first
   write inserted one derivation, the second reported `reused: 1` and the row count stayed at one.
   `has_code_derivation` compares all four identity columns exactly, so probing a skip identity
   needs no storage change.
3. **Complete-derivation reuse across policy changes (Reasoned from the code).**
   `has_code_derivation(I)` ignores status and policy and runs first; the skip rows have another
   identity and never satisfy it. A lowered budget therefore cannot remove or hide a complete row.
4. **Old entities after a parser-version change (code read, Measured on the compat database).**
   Entity and relationship queries keep, per input, only the derivation with the greatest
   `created_at` across all identities: a newer skip row hides the older version's entities there.
   `structure <path>` lists every derivation of the input: it showed the old `complete` one with its
   entities **and** the new row as `(failed, 0 errors)`. Expected and to be documented: symbols and
   relationships lose the file after the upgrade, `structure` shows both.
5. **Older binaries (Measured).** With the skip row in a real database, `mind structure` and
   `mind symbols` (this build has no skip logic) ran without error, printed the skip as
   `rust <hash> (failed, 0 errors)`, and an ingest with it re-derived normally. No decode failure.
6. **Interrupted or failed extraction.** The stream consumer returns an `Extracted` only after a
   valid closing record (tested on the stream branches), so a worker killed or cancelled mid-parse
   persists nothing. Within persistence, a derivation row and its entities are written in one
   SQLite transaction (`record_code_observation`); the Git observation and the code observation
   are two transactions, so a crash between them leaves text without derivations, which the next
   ingest completes (`known(I)` false). A discarded structural result never reaches the store, so
   partial entities cannot be exposed.

Blockers before implementing the cap: (a) the borrowed/charged allocation rewrite of `visit`
described in 1, including the `raw_target` charge, (b) the three tests listed in 2 of section 6 for
the method-chain and wide-callee shapes, (c) the decision on how `structure` should present a
file whose old entities are hidden (section 4 of this list), (d) confirmation of the
`--skip-structure` CLI shape.

## 10. Final policy decisions (recorded)

| Decision | Value |
|---|---|
| Source size budget | 2 MiB (`> 2,097,152` bytes skips; exactly 2 MiB parses) |
| Entity nesting depth | 256 (`> 256` skips) |
| Structural output budget | 32 MiB (accounting in 2 and 9) |
| On any violation | the **entire** structural derivation is discarded; the text path is untouched |
| Reporting | additive `skipped` counter in the ingest report, a line in the ingest output, `structure` marker |
| Storage of a skip | existing status `failed`, `error_count` 0, no entities, identity `I` + policy suffix (`P`); no new status, no migration |
| Blacklisting | none automatic; only the explicit `--skip-structure` below creates a manual skip |

### The effective derivation (one rule for every reader)
Per code input, the **effective derivation** is the row with the greatest `(created_at, id)` over
all its derivations, whatever the identity; `id` (UUIDv7) breaks ties, so exactly one row is
effective. Today the readers disagree at the edges: entity, symbol and relationship queries use
`d.created_at = (SELECT MAX(created_at) ...)`, which returns *several* rows on a tie; `structure
<path>` ignores the rule and prints every derivation. Design:
* One shared SQL fragment (a constant used by every reader):
  `d.id = (SELECT d2.id FROM code_derivations d2 WHERE d2.input_id = d.input_id ORDER BY d2.created_at DESC, d2.id DESC LIMIT 1)`.
  `symbols`, relationship observations/resolution and `structure` all use it.
* `structure` keeps listing every derivation of the input (history stays visible, which is what
  it does today) but marks exactly one `effective` and prints, for a skip, `skipped (<reason>)`
  from the identity suffix (reasons: `source_bytes>N`, `entity_depth>N`, `output_bytes>N`,
  `requested`). Old derivations keep their entity list, marked `superseded`.
* A newer skip hides older entities from `symbols`/relationships (they are not effective); it
  never deletes them; `structure` still shows them.
* Test: for a database with complete, then skip, then complete rows of one input, the three
  readers return the same derivation id; with equal `created_at` they still return one.

## 11. Manual `--skip-structure <path>` and `--retry-structure <path>` (design only)

Both are options of `mind ingest` (CLI only, repeatable, **explicit paths**: no globs, no
directories, no implicit "last crashed file"). They are not available through MCP.

**Path normalization.** Git: repository-relative, `/` separators, no leading `/`, no `.`/`..`/empty
components, at most 4,096 bytes, matched exactly against the source files (blob entries that are
ingestable) of the commits the operation derives (HEAD and worktree heads, or every tip with
`--history`). Directory: relative to the ingested root, or absolute and then required to lie below
the canonical root; canonicalized the same way the walk does (no symlink following). A path that
matches no ingestable source file of the operation is an error, never ignored. In history scope a
path may resolve to several blobs (different commits); the decision applies to every distinct blob
it resolves to, and the report lists them.

**Unit and identity.** The decision attaches to the **code input** (the existing key): `(repository
id, blob oid)` for Git (the repository id is the marker UUID, so it survives moving the
repository; a fresh clone has another id and carries no decisions), `(NULL, content hash)` for
directories. The path only selects; the same blob under another path is skipped too (content
addressed), and a changed file (new blob) is a new input with no decision.

**Persistence.** `--skip-structure` appends a skip derivation with identity `I` + `skip(p1:requested)`
(status `failed`, 0 entities), in the same observation as the text, so text ingestion proceeds
unchanged. No limit value in the suffix: a manual decision is independent of budgets.

**Precedence in the producer** (each step an existence probe, no new context method):
1. `known(I)` (a derivation exists for the current parser identity): reuse. A manual skip is not
   applied over an existing complete/recovered/failed derivation: `--skip-structure` on such a
   blob is a no-op with the message "structure already derived; nothing to skip". Reason: rows are
   append-only and keyed uniquely per identity, so a skip could not be made effective over an
   older `I` row, and a derivation that exists cannot crash the worker (it is not re-run).
2. `known(I + requested)`: skip (reuse the row), report reason `requested`.
3. policy probes and evaluation (sections 3 and 2).
`--retry-structure <path>` runs the operation with step 2 disabled for the resolved blob(s): the
parse is attempted under the policy. Outcomes: complete/recovered/failed derivation `I` appended
(newest, effective); policy skip appended (effective, reason now the budget); the worker dies
again: nothing is stored and the requested skip row stays as it was (the user can retry again or
leave it).

**Interaction table**
| Situation | `--skip-structure` | `--retry-structure` |
|---|---|---|
| no derivation yet | appends `requested` skip | parses under the policy (default behaviour) |
| complete/recovered/failed `I` exists | no-op, says so | no-op (nothing to retry), says so |
| policy skip exists | appends `requested` skip (stays skipped if a budget is later raised) | re-evaluates; a raised budget now parses, otherwise a new policy skip row if the limit changed, else reuse |
| requested skip exists | idempotent no-op | parses; outcome as above |
**Parser upgrade.** `I` and the skip identities embed `extractor_version`, so a manual skip
recorded under the old parser no longer matches: the blob is evaluated again with the new parser.
This is deliberate (the new parser may not crash); if it does, the failed stream names the file
again (progress record) and the user repeats the explicit skip. Carrying manual skips across
versions was rejected: it would silently hide a fixed problem.
**Idempotency.** `--skip-structure` twice: second run reuses the row (`reused`), no new row.
`--retry-structure` twice: the second finds either a derivation (no-op) or repeats the attempt.
Neither option changes anything for other files, and neither is remembered between runs except
through the stored rows. Tests: the matrix above, path errors, history-scope multi-blob paths,
copy of the same blob under another path, repository move (marker), two runs.

## 12. Implementation plan for the structural cap (not started)

Order of work, each step with its tests first:
1. **Reference and policy type.** `ParsePolicy{max_source_bytes, max_entity_depth, max_output_bytes}`
   with `unlimited()` for tests; `SkipReason`; `code::parse_with_policy`. Keep the current
   `code::parse` and the test-only recursive reference.
2. **Allocation hardening of `visit`** (behaviour-preserving for accepted input):
   * `raw` becomes a borrowed `&str` slice (`&s[start..end]`) for the signature line, `pub`/`async`
     prefix checks and the `use` handling; only the first line (signature) and, for `use`, the
     stored `raw` are copied.
   * `Ctx` is passed by reference; `parent`/`parent_q` are used as `&str` (no per-node `String`
     clones); strings are built only where an entity or relationship is produced.
   * `charge(n)` is called before each allocation with an exact or upper-bound length: names,
     `path` and `q` (computed from lengths), `signature`, the `use` raw (twice) and its targets,
     `raw_target` (callee text length, `end_byte - start_byte`) and the impl targets; plus 64 per
     record. First failure returns `Err(SkipReason)`; the traversal stops; the draft is dropped.
   * Depth counter in `Ctx`, checked on entering a level (entity or `impl`).
   * Differential test: for every accepted input the result equals `code::parse` (all real sources,
     corrupted variants, nested fixtures); the 85-file corpus stays the equivalence oracle.
3. **Source-size check** before the parser is created.
4. **Producer wiring** (stream and in-process): precedence of section 11, `P` identities, skip
   `code_input` records, the `skipped` counter, `structure` marker and the shared effective-derivation
   SQL (section 10). Progress record (section 5).
5. **CLI options** `--skip-structure` / `--retry-structure` (section 11).

**Planned regression fixtures** (each runs in a child process with a modest `RLIMIT_AS` and asserts
a typed skip or a normal result, never a crash):
| Fixture | Expected |
|---|---|
| method chain `a.b().b()...` 2,000 / 10,000 / 100,000 calls | accepted while the charged `raw_target` total stays under 32 MiB; skipped (`output_bytes`) beyond; memory far below the unbounded 205 MiB at 10,000 |
| wide callee: `f(...)(...)` chains and `x.y.z...` field chains | same accounting |
| nested entities: `fn`, `mod`, `impl` chains at depth 255 / 256 / 257 / 20,000 | 256 parses, 257 and 20,000 skip (`entity_depth`) |
| large identifier: one 1 MiB name at depth 3; 2 MiB name | qualified-name growth charged before `format!`; skip when over budget |
| large import list: `use a::{b0, b1, ...}` with 2 MiB of items; many imports | raw counted twice plus targets; skip over budget |
| boundaries: source 2,097,152 and 2,097,153 bytes; output S and S-1 bytes where S is the accounted size of the unlimited run; depth 256 / 257 | exact |
| the existing parser-safety fixtures (braces, parentheses, chain, modules) | still parse |
| ordinary 2 MiB code, 76k entities | accepted, equal to `code::parse` |
Acceptance for the allocation work: no peak-RSS regression on accepted inputs (compare the 85-file
corpus and the wgpu ingest before/after), and the method-chain 10,000 fixture drops from 205 MiB.

## 13. Remaining blockers before implementation
1. Agreement that `structure` marks `effective`/`superseded` rather than listing only the effective
   derivation (section 10).
2. The no-op rule for `--skip-structure` over an existing derivation (section 11) and the choice not
   to carry manual skips across parser versions.
3. The exact byte accounting constants (the fixed 64 per record) are provisional; they are only
   tested for self-consistency, not calibrated against RSS (the budgets do not bound RSS).
4. Section 4 of the earlier list: how hidden entities are presented after a parser upgrade (covered by
   the marker in section 10, to be confirmed).

## 14. Implementation notes (M7.3b.1p, uncommitted)

* `code::ParsePolicy` (`p1` defaults), `code::parse_with_policy` -> `ParseOutcome{derivation, skipped,
  accounted_bytes, transient}`, `code::skip_identity` / `skip_derivation` / `skip_reason_of`;
  `code::parse` is the unlimited form. `ingest::structural_derivation` implements the producer order of
  section 3 (known identity, known skip, size, parse under policy) and is used by the in-process
  directory and Git paths (default policy) and by the stream producers (`ProducerLimits.parse_policy`).
* Budgets are enforced inside `visit`: the depth when an entity or `impl` level is entered, the size
  before the parser exists, the output by `Budget::charge` before each stored or context string is
  allocated. `raw` is a borrowed slice; the parent/qualified-name context is a shared `Rc<str>`.
  A function's `signature` is its first *line*; on a one-line file that is the rest of the file, and
  it is charged as such (this is what stops deeply nested one-line input).
* A parser that cannot run (no grammar, no tree) is an error of the operation, not a stored derivation.
* Skip rows: status `failed`, 0 errors, no entities, `extractor_version` `3+skip(p1:<reason>)`. No new
  status, no migration.
* `EFFECTIVE_DERIVATION` (`ORDER BY created_at DESC, id DESC LIMIT 1`) is used by entity listing,
  symbol search, relationship observation/resolution and `structure`.
* Reporting (additive): `CodeObservationReport.skipped` and `.skip_reasons`; `ObservationReport.code`
  (directory ingests now return their code report instead of dropping it); `CodeDerivation.effective`
  and `.skip_reason`; ingest output line "structure skipped for N source file(s) (...)"; `structure`
  prints `effective`/`superseded` and `skipped: <reason>`.
* Known gaps: the in-process paths cannot be configured (default policy); the stream producer can.
  Each not-yet-known source costs up to two extra existence probes (depth, output): in the worker
  design these are context requests. No CLI options, progress record or retry (excluded).
