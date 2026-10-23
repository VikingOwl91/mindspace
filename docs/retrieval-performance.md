# Retrieval performance and model lifecycle

## Integrity and lifetime

The MCP stdio session owns at most one semantic runtime. It is not a global
registry, query cache, or persistent verification cache. CLI invocations and
the library's explicit `semantic::resolve` API still verify independently.
Lexical requests never inspect model state or artifacts.

Every non-lexical MCP request rereads enablement, model selection, structural
artifact checks, and compatible-index availability. The complete trusted
manifest entry and model directory identify the runtime, not just the model ID
or derivation key. Index contents, query vectors, scores, and results are not
cached. Filtering, vector scanning, ranking and fusion still run per request.

Verification reuse is restricted to Linux on local btrfs, the filesystem
qualified for this change. Other filesystems and platforms retain the uncached
path. A failed filesystem check also prevents caching. This restriction is
deliberately narrow; extending it requires evidence for the filesystem's
metadata coherence and timestamp behavior.

Before first use, every artifact passes the compiled-in SHA-256 checks. The
resolver records device, inode, mode, length, mtime and ctime (including their
nanoseconds) for artifacts and the checked data/models/model directories.
Metadata is a change detector for bytes already verified with SHA-256. Size or
mtime alone is never treated as integrity evidence.

Cache eligibility requires artifact ctimes at least two seconds older than the
wall clock sampled before the prehash snapshot. Recent artifacts can be used
after full verification, but cannot publish a reusable entry. They must pass
another full verification after that margin. This avoids associating a newly
written file with a verification performed inside the same timestamp tick.
Invalid/future timestamps and observed backward clock movement prevent reuse.

Cold positive-limit requests load lazily through the existing query path. Only
after successful embedding and an unchanged post-search snapshot can the
session retain the runtime. Warm requests repeat readiness, filesystem and
identity checks before using it, and check identities again before returning
semantic results. A cold zero-limit request still resolves capability but
neither loads weights nor publishes an entry. A validated warm entry may
survive a zero-limit request.

Unavailability, changed identities, resolution/storage/search failures and
query-embedding failures evict the entry. Auto's successful lexical fallback
after an embedding failure also evicts it. No-semantic-candidates is not a
runtime failure. Replacement drops the old runtime before allocating new
weights. Disabling embeddings or changing selection is observed at the next
non-lexical request; a lexical request does not inspect configuration just to
release ML memory. EOF releases the session's retained weights.

If a successful search observes a changed post-search snapshot, its semantic
results are discarded. Semantic/Hybrid fail with an invalid-artifact reason;
Auto executes the ordinary lexical fallback. There is no semantic retry,
download, indexing pass, background thread, watcher, or service.

## Freshness boundary

This preserves the existing local single-user assumption: model paths are in
the user's own data directory and are not concurrently controlled by an
attacker. Symlink checks and pre/post snapshots are not atomic filesystem
snapshots. A writer acting after a warm check may leave that request using the
previous verified, owned runtime; the next observed change invalidates it.
Candle's safetensors loader reads owned tensor storage, not file-backed mmap,
so a subsequent disk write cannot change those already loaded weights.

The cache assumes coherent local btrfs metadata and ordinary kernel clocks.
It does not defend against privileged ctime manipulation, filesystem rollback,
or a clock excursion that conceals a write and returns between observations.
Two seconds is a timestamp-resolution margin, not a cryptographic guarantee.
Transient configuration changes entirely between requests are not tracked.
Stronger instantaneous freshness would require writer coordination or an
immutable-generation design; neither is introduced here. No verification
stamp survives a process restart.

## Measurement method

Release measurements use existing installed Nomic artifacts and a disposable
database populated with synthetic assertions and normalized vectors. They do
not use or modify the user's database, run model indexing, or download models.
Fresh process means a new OS process, not an evicted filesystem page cache.
Hashing, owned model loading, warm query embedding and vector scanning are
measured separately where the public APIs permit it. MCP measurements include
the real stdio protocol and result serialization.

The trusted Nomic weights are 546,938,168 bytes and its tokenizer is 711,396
bytes: 547,649,564 bytes total. Retaining a runtime trades process-resident
memory for lower warm-request latency; it does not remove inference or the
full filter-qualified vector scan.

## Release measurements (2026-10-23)

Host: Ryzen 7 3700X, Linux 7.2.11-zen, local btrfs, Rust 1.98.1. Baseline:
`04ad0a258bf7968950e609c20675f542f3854bbe`, preserved release binary SHA-256
`ec69f7569584da76c79610dfe0bf6b1b3b650dae07b57a00ca14b2d54ae3ed9c`.
The fixture contains 100 synthetic assertions and 100 manually inserted
768-dimensional unit vectors; its main SQLite file is 1,126,400 bytes. Query:
`synthetic benchmark retrieval`, 10 tokens with prefix and special tokens.

The following matched run used five fresh CLI processes per binary, then one
MCP process per binary with six calls per mode. Repeat medians exclude each
mode's first call. All 24 structured MCP responses matched exactly between
baseline and candidate, including scores and provenance.

| Operation | Before (ms) | After (ms) |
| --- | ---: | ---: |
| Fresh CLI semantic, median | 617.9 | 629.6 |
| First semantic in MCP | 635.5 | 595.2 |
| Repeated MCP semantic, median | 591.6 | 78.2 |
| Repeated MCP hybrid, median | 556.0 | 80.1 |
| Repeated MCP Auto, median | 559.6 | 78.7 |
| Repeated MCP lexical, median | 1.94 | 2.02 |

Repeated semantic samples ranged from 559.8–603.4 ms before and 76.5–82.0 ms
after. Hybrid and Auto followed semantic in that same session, so their first
calls were already warm after the change (78.9 and 79.5 ms respectively).
Fresh-process searches still verify and load; no cold-search speedup is
claimed. The small lexical/cold differences are not treated as improvements.
Runs were sequential on a shared host, without CPU isolation or page-cache
eviction; these are warm-file-cache release observations, not latency bounds.

Public API component measurements use five separate processes for SHA,
resolution and loading, and seven warm samples for inference/scanning:

| Component | Initial baseline median (ms) | Candidate median (ms) |
| --- | ---: | ---: |
| Full artifact SHA verification | 290.7 | 316.6 |
| Explicit uncached `semantic::resolve` | 294.1 | 307.7 |
| Owned model/tokenizer loading | 189.6 | 218.4 |
| Warm query embedding | 85.0 | 80.8 |
| Direct 100-vector scan with real query vector | 0.568 | 0.576 |

These APIs intentionally remain uncached. Their costs explain the warm MCP
gain, but independently measured medians must not be subtracted to infer a
cache-management duration. Warm readiness/change-check time is included in MCP
latency and was not separately instrumented. With a pre-resolved real runtime,
candidate semantic/hybrid/Auto medians were 80.3/86.4/82.3 ms. With a fake
runtime they were 0.559/2.474/2.452 ms, separating orchestration and scanning
from inference. No vector-scan optimization was made.

`/usr/bin/time` was unavailable. Peak RSS was read from Linux `VmHWM`, sampled
every 5 ms; reported peaks may miss a process's final short-lived allocation.
Matched MCP peaks were 1,095,600 KiB before and 1,079,360 KiB after. After all
24 responses, RSS was 465,396 KiB before and 562,848 KiB after. After the first
semantic response it was 21,628 KiB before and 562,028 KiB after. The baseline
allocator retains some freed memory over repeated loads, so final RSS is not
simply the size of live weights. The optimization retains roughly 550 MiB of
resident process memory between calls; it does not claim to reduce peak RSS.

Raw task evidence and reproducible external harnesses are retained under
`/tmp/opencode/mindspace-perf/` (original baseline) and
`/tmp/opencode/mindspace-perf-final/` (matched run and candidate components).
Key files are `logs/benchmark-results.json`, `matched-results.json`,
`component-after-results.json`, `run_bench.py`, `measure.py`, and
`harness/src/main.rs`. These are temporary local evidence, not repository
artifacts or a persistent application cache.

## Verification

Local gates passed: formatting, strict Clippy for all features and no default
features, full tests in both configurations, no-default build and release
build. The all-feature suite passed 270 tests with one opt-in test ignored;
the no-default suite passed 243 tests. The ignored real-model MCP test was
also run explicitly in release mode and passed. It reads installed artifacts
and uses a disposable synthetic database, with no indexing or downloads:

```sh
cargo test --release --test mcp \
  repeated_real_model_requests_preserve_results_and_observe_enablement -- --ignored
```

Sixteen deterministic cache tests cover cold/warm ownership, session
isolation, complete-manifest changes, disable/re-enable, model switching,
index loss/restoration, first-use and warm corruption, restored mtime,
replacement/symlinks, mutation during construction and warm queries, embedding
errors including Auto fallback, lexical isolation, zero limits, filesystem
qualification, recent artifacts and observed clock rollback. Real release MCP
measurements exercise the production btrfs qualification path. Non-Linux
compilation has not been exercised on this host; its cache implementation is
excluded by target cfg and uses the original uncached adapter instead.
