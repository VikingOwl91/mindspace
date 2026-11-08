# Performance finding: plain-directory ingest is superlinear (about O(n^2))

Status: recorded, **separate from the structural-policy work**; not optimized, root cause not
established.

## Reproduction
1. Build `main` (d9ce010 or later) in release mode.
2. Generate N files `d{i%100}/f{i}.md` (any N; 30,000 used originally with 300 directories) each
   containing `# F{i}\n\nbody\n`.
3. `MINDSPACE_DB=<fresh path> mind init; MINDSPACE_DB=<same> mind space create p;
   time MINDSPACE_DB=<same> mind ingest <dir> --space p`.
Host: Linux 7.2 (zen), 16 CPUs, temp storage under /tmp (filesystem not recorded), SQLite bundled
with rusqlite 0.40, binary built with `cargo build --release --locked`.

## Observed scaling (Measured)
| Files | Wall time |
|---|---|
| 500 | 0.4 s |
| 1,000 | 1.0 s |
| 2,000 | 2.9 s |
| 4,000 | 10.1 s |
| 30,000 (300 dirs) | 547 s (542 s with the parser-safety backport) |
Each doubling costs 2.5-3.5x; the 30,000 run fits the curve. Extraction of the same directory takes
0.46 s (`stream_bench inproc dir`), so the cost is in persistence (`record_observation`). Not caused
by the stream, the parser or the policy work: it reproduces on `main`.

## Profile and suspected query
* `perf` (4,000 files): SQLite internals dominate: `sqlite3VdbeExec` 14 %, `sqlite3BtreeIndexMoveto`
  9 %, `btreeParseCellPtr` 8 %, `vdbeRecordCompareString` 5 %. Many index seeks and comparisons, not
  Rust-side work.
* Candidate found by `EXPLAIN QUERY PLAN` on the resulting database: `logical_at`
  (`SELECT id FROM logical_sources WHERE current_location = ?1 AND state = ?2`, called from
  `present_logical_at`/`absent_logical_at` in `record_observation_tx`, `src/storage/sqlite.rs` around
  lines 3211-3300 and the helper definitions at 3611-3625) is `SCAN logical_sources`: there is no index on `current_location`.
  That is O(n) per call, O(n^2) if it runs once per file.
* **That hypothesis was tested and not supported.** A scratch database with
  `CREATE INDEX ON logical_sources(current_location, state)` made the plan `SEARCH ... USING INDEX`,
  yet ingesting the same 4,000 files still took 10.7 s (10.1 s without the index). So this query is
  not the dominant cost, whatever else it costs.

## Evidence still required to establish the root cause
1. A call-graph profile with Rust frames (release build with debug info, `perf record --call-graph
   dwarf`) to attribute the SQLite time to statements and their callers.
2. Per-statement counters: `sqlite3_stmt_status` (full-scan steps, sorts, auto-indexes) or SQLite's
   trace/profile hook, for N = 1,000 / 2,000 / 4,000, to find the statement whose step count grows
   quadratically.
3. Count of calls per statement per ingest (instrumentation), to separate "slow query" from "query
   run O(n) times per file".
4. The same measurement with the M6 temporal triggers (221 triggers, 63 on INSERT, each calling
   `m6_write_revision()` and writing `m6_row_availability`) isolated: copy the database, drop the
   triggers, ingest again; and with `sources`/`evidence`/`assertions` writes timed per row.
5. `ANALYZE` effect and the plans of `latest_version`, the `sources (logical_id, content_hash)`
   lookup (~line 3656) and the memory/assertion supersession lookups on a populated database.
6. Whether Git ingest of the same file count shares the behaviour (60k files extracted in 1.5 s;
   persistence was not timed on its own).
Nothing in this finding should be fixed before items 1-3 identify the statement.
