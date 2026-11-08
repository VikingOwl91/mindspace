# M7.3b production worker: design update (gate, nothing implemented)

Status: DESIGN. Basis: `docs/design-m7.3b-dynamic-workers.md`, the worker spike
(6633748), `docs/spikes/m7.3b-transport-evaluation.md`,
`docs/m7.3b-hardening-findings.md`, `docs/sandbox-runtime-audit.md`. The protocol
is **not** production-ready until the tests in section 11 pass.

1. **Record-level JSON framing.** Every frame has an explicit `type`
   (`header`, `file`, `derivation`, `blob`, `code_input`, `ctx_request`, `end`,
   `error`) and a sequence number. Types are an externally tagged envelope (no
   serde internal tagging, so a binary codec stays possible). The consumer
   rebuilds `Extracted` from the records; the final `end` frame carries counts and
   a checksum of the counts.
2. **Limits.** 1 MiB per frame (a larger record is split into `chunk` frames of
   one record id); 512 MiB and 100k frames per operation. Violation: the frontend
   kills the worker and drops everything.
3. **Bounded parsing and accounting.** Length is read and checked before any
   allocation; strings are bounded per field; the running total of bytes, frames,
   records per type and decoded content is accounted at the frontend and the
   operation fails at the first excess. The worker runs under `RLIMIT_AS` (value to
   be derived from measurement; UNKNOWN today) as a second line.
4. **Worker-side incremental production.** The worker writes each record as
   soon as it is extracted, so its peak is not the whole result (spike B: 354 MiB
   after extraction, 547 MiB peak with the encoded copy). Requires extraction to
   emit through a sink instead of returning a `Vec`; this is the largest change to
   existing code.
5. **Minimal context exchange.** The worker may send `ctx_request` frames
   (`space_id`, `git_refs_current`, `has_code_derivation`); the frontend answers
   from the store with read-only calls and a per-operation request cap. The worker
   has no database access and no database file descriptor.
6. **`has_code_derivation` stays.** Dropping it cost 8x on repeat ingests. Per
   blob a request is too chatty: the worker sends batches (up to N blob keys per
   frame) and receives a bitmap; the frontend sends the known set for the
   repository up front only if it fits the 1 MiB frame. Acceptance: repeat ingest
   of unchanged input within 1.5x of in-process.
7. **Read-only Git and identity.** The root is granted read-only. The marker is
   created at grant time (or by the frontend on a `needs_identity` reply), never
   by the worker. Linked worktrees and external `commondir` get their own
   read-only anchors.
8. **Non-dumpable frontend and inherited roots.** The worker and the frontend
   set `PR_SET_DUMPABLE=0`. Git children get the anchor descriptors inherited
   (CLOEXEC cleared in the child) and paths `/proc/self/fd/N` with `-C`,
   `GIT_DIR`, `GIT_COMMON_DIR` (VERIFIED experimentally, findings D1). The test
   that inspects `/proc/<server>/fd` is rewritten to use another channel.
9. **Lifecycle.** One worker per operation (or per repository grant), never
   shared across grants. Timeouts: startup, per-frame, total. Cancellation:
   frontend closes the channel and kills the worker's process group, waits, and
   discards the partial record stream; nothing is persisted before `end` and
   `Extracted::validate` pass. Crash: the same path, reported as a typed error.
   The frontend never trusts a worker exit code as success without `end`.
10. **F1/F2 compatibility.** The worker's rules are `Plan::rules()` minus the
    database and models directories, plus read-only roots. It uses the F1
    runtime directories (never `/etc`) and Landlock signal scope where the ABI
    is 6 or newer; on older ABIs `doctor` reports signals as not enforced and the
    worker design makes no signal-isolation claim. The audit follow-ups in
    `docs/sandbox-runtime-audit.md` apply to the worker first (narrow `/proc`).

## Measurable acceptance criteria (on the wgpu clone and this repository)
| Criterion | Target |
|---|---|
| Frontend peak RSS | at most 60 % of the current whole-value path (682 MiB), i.e. under 410 MiB; stretch 310 MiB |
| Worker peak RSS | at most 450 MiB (spike: 547 MiB); with incremental production, under 1.3x the content size |
| IPC bytes | within 5 % of the spike's JSON (196 MB) |
| End-to-end latency | within 1.15x of in-process for first ingest, 1.5x for repeat ingest |
| Startup of the worker | under 50 ms median (to be measured; spike value UNKNOWN here) |
| Over-limit input | operation fails with a typed error before the frontend exceeds 512 MiB of decoded data |
| Cancellation | worker and children gone within 1 s of the request, no rows written |

## 11. Not ready until these are tested
Hostile frames (oversize length, bad type, wrong order, duplicate ids, truncated
stream, huge count), memory-limit behaviour with `RLIMIT_AS`, cancellation at each
phase, worker crash at each phase, a hostile repository for Git and tree-sitter,
repeat-ingest performance with batched context requests, and the full MCP end to
end path with a real client.
