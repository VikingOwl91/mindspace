# M7.3b.1: the internal record stream, and its memory policy

Status: implemented as an internal interface (`src/ingest/stream.rs`), not wired
into any tool, worker or the in-process ingest. Labels: **Measured** (observed in
a run named below), **Estimated** (derived, not run).

## Shape
* Frames: `u32` length + JSON `{v, op, seq, records[]}`; 1 MiB per frame,
  512 MiB and 100,000 frames per operation, all enforced by the writer and the
  consumer; length checked before any allocation.
* Typed records (no trial decoding); a grammar enforced by a state machine
  (header, refs, objects, blobs, ..., body, end); duplicates, order, hashes, paths,
  object ids, sizes, counts and the closing counts are checked as records arrive.
* Content once per blob in chunks (128 KiB), shared by derivations and the code
  input; large vectors in parts (256 KiB); anything else too big for a frame is
  sent oversized (`spill`) in pieces and reconstructed exactly; at most
  `MAX_RECORD` = 32 MiB per record.
* Nothing reaches the store before `consume` returns; `persist` is unchanged.

## Which records can exceed 1 MiB (audit of variable-length fields)
| Record | Variable field | Handling |
|---|---|---|
| commit | message (unbounded) | spill |
| tree | entries (any number) | spill |
| tag | message | spill |
| ref, ref event, remote | names, url (small in practice) | spill if ever |
| file / derivation | title, subject, path (path bounded 4 KiB), assertion/evidence content (a whole file for a file without headings) | parts split vectors; a single item over a frame spills |
| code input | entities, imports, relationships | parts; a single item spills |
| blob / file content | text of any size | chunks |
| view/current commits, release pairs | counts | batches of 1000, spill if big |
Tested: 3 MiB single-section file, 1.5 MiB heading, 2 MB commit and tag
messages, a tree of 25,000 entries (about 3 MB), each streamed and compared to
the in-process result.

## Memory
### Producer (worker)
* **Measured** (wgpu clone: 71,066 trees and commits, about 21k text blobs, 44 MiB
  of counted content): peak RSS 51-52 MiB, PSS 46-47 MiB, against 346 MiB RSS for the
  in-process extraction.
* **Estimated** bound: structural identifiers and the derivation key list
  (O(objects and derivations), roughly 100-200 bytes each), plus 3 copies of
  one blob batch (8 MiB, so about 24 MiB), plus the largest single record
  (up to 32 MiB, 2-3 copies while it is spilled: about 100 MiB).
* **Not bounded today:** one very large text blob. Git's output for it and its
  `String` are held whole (and chunks copy it), so the producer needs about 3x the
  largest blob. A per-blob size limit (decision needed: skip with a recorded
  reason, or refuse) is required before the worker takes untrusted repositories.
  Tree-sitter memory on hostile input is not measured.
### Consumer (frontend)
* **Measured:** 278 MiB RSS (256 PSS) for the same repository, i.e. about 6.3x the 44 MiB of
  counted content; the result is the same `Extracted` the in-process path builds
  (derivations, blob rows and code inputs each hold their own copy of the text),
  plus 127k records of structure. The wire form is never held whole (one frame,
  one content slot, at most one spilled record).
* **Estimated worst case:** about 6-7x `content_bytes` plus structure. At the default
  limit of 512 MiB of content that is above 3 GiB, which is why the limit that
  matters in practice is the frontend's own memory budget, not the transfer limit.
* **Spill reassembly:** at most `max_record` (32 MiB) of text plus its decoded
  form (estimated 2-3x): about 100 MiB transient.
* **Prefetch** (optional reader thread): `depth` x 256 KiB; no effect on the bound.
### Worst-case deserialization and reconstruction risks
* A declared length is trusted only up to its limit; `serde_json` allocates what a
  record contains, so one record is bounded by 32 MiB x small factor.
* Many small records are bounded by the 100,000-frame budget only indirectly
  (a frame holds many records): the 512 MiB byte budget is what limits them;
  127k records were 168 MB.
* Reconstruction clones shared content for each derivation and code input:
  `content_bytes` is counted when each is attached, so the content limit
  includes the copies.
### Required worker resource limits (Estimated starting values; to be measured)
`RLIMIT_AS` about 2 GiB, `RLIMIT_CPU` per operation, `RLIMIT_FSIZE` 0 (no writes),
`RLIMIT_NPROC` for Git children only, `RLIMIT_NOFILE` small; wall-clock timeout from
the operation deadline; kill of the process group on any error.
### The frontend must reject an operation when
the framing, grammar, ordering, uniqueness, hash, path or id checks fail; a
budget (bytes, frames, record size, files, content bytes) would be exceeded; the
declared operation, kind, root or version differ; the stream ends early, repeats,
reorders or continues after its end; its closing counts differ; the operation is
cancelled or times out; or the projected `Extracted` memory (about 6-7x content)
would exceed the frontend's configured budget (not implemented yet).

## Performance (release, wgpu clone, 5 runs, medians)
| | in-process | stream before optimization (fa9224d) | stream now |
|---|---|---|---|
| producer-only wall | 4.69 s | 5.37 s (1.14x) | 5.26 s (1.12x) |
| CPU, user+sys incl. Git | 5.15 s | 5.78 s | 5.69 s |
| producer RSS / PSS | 346 / 340 MiB | 51 / 46 | 52 / 47 |
| consumer decode from file | - | 926 ms | 880 ms |
| consumer RSS / PSS | - | 279 / 256 | 279 / 256 |
| bytes | - | 168.5 MB | 168.5 MB |
| pipeline end to end, 64 KiB pipe | - | 1.29x | 1.28x |
| pipeline, 1 MiB pipe | - | 1.30x | 1.19x |
| pipeline, read-ahead 16 / 64 / 256 buffers (4 / 16 / 64 MiB) | - | - | 1.23x / 1.20x / 1.18x |
Profile of the producer (perf, flat): tree-sitter 36 %, Git subprocesses 15 %,
allocator 9 %, hex formatting 9 % (both paths), serde_json 3.7 %. Timed inside
the producer: emitting 71k structural records 0.19 s, emitting bodies 0.09 s,
reading blobs from Git 0.07 s. JSON cost is not the bottleneck; in-place
serialization and a larger Git batch count changed nothing measurable.
The gap to the in-process path is the encode time (about 0.3 s) plus consumer
back-pressure on the pipe. A pipe of 1 MiB or a read-ahead thread recovers most of
it. The 1.15x target is reached for the producer (1.12x) and not quite end to end
(1.18-1.20x with read-ahead).
