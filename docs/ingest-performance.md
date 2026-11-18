# First Git ingest benchmark

The mixed fixture contains 35,832 Git trees, 71,881 tree entries and 256
Markdown documents with eight sections each. A second fixture uses the same
documents in a single directory and contains two Git trees. Both produce 2,048
memories in each fresh database.

The example generates the repository with Git fast-import and opens a temporary
database for each run. It prints one JSON record per run. The timer starts with
the ordinary in-process Git extraction. The final pre-COMMIT hook separates
database writing from COMMIT and return. Fixture creation, initialization,
database close and report rendering are outside the timers. Use a temporary
directory on the filesystem you want to measure.

```sh
cargo run --locked --release --no-default-features --example ingest_profile
cargo run --locked --release --no-default-features --example ingest_profile -- --directories 0
```

## Measurement on 2026-11-18

The same example ran against base `b69e0d3` and change `5b88c1c` on one Linux
x86_64 workstation with Rust 1.100.0, bundled SQLite 3.54.1 and embeddings
disabled. Three fresh databases per fixture and version were measured with
alternating execution order. Values are individual medians in milliseconds.

| Fixture | Revision | Extraction | Write before COMMIT | COMMIT and return | Total |
| --- | --- | ---: | ---: | ---: | ---: |
| Mixed | Base | 509 | 17,317 | 309 | 18,115 |
| Mixed | Change | 444 | 6,906 | 287 | 7,788 |
| Documents | Base | 37 | 1,665 | 39 | 1,825 |
| Documents | Change | 34 | 759 | 35 | 830 |

Mixed totals ranged from 17,956 to 18,743 ms at the base and 7,624 to 7,920 ms
with the change. Each mixed database was 78,950,400 bytes. The document-only
databases were 7,258,112 bytes each.

The Git writer reuses prepared INSERT statements for trees, entries and blobs.
The document writer also reuses statements for evidence, assertions, FTS,
memories and space membership. Preparing a new statement for each row had also
recompiled its SQLite triggers. For a freshly created logical source the writer
can skip loading prior memories. On the populated document fixture, SQLite's
query plan for that empty lookup started with the active-memory state index
and joined through assertions and evidence. The result above measures these
changes together. First-ingest time for other repositories depends on their
object graph, document contents and filesystem.

## MCP client check

Python MCP SDK 2.3.0 negotiated protocol 2025-06-18 with the sandboxed stdio
server. With the ordinary 30-second server budget and a 60-second client
timeout the mixed fixture completed in 7.861 seconds and persisted 2,048
memories. On a fresh document-only fixture a 0.75-second server budget expired
while writing source evidence. The client received the rollback error in 0.757
seconds, the database had zero memories, and a subsequent manual write in the
same session succeeded. Retrying the ingest with a larger budget added the
2,048 expected memories.
