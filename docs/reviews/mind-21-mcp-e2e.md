# MIND-21: MCP end-to-end smoke test with a real Claude Code client

Date: 2026-11-07. Binary: `target/release/mind` built from `8988341` (MIND-18). Client: Claude Code 2.1.319 (`claude -p`,
headless, its own MCP client). Kernel 7.2.14, Landlock ABI >= 3, sandbox **enforced** in every run (each ingest result
reports `enforced (Landlock, ABI 3 or newer, fully enforced, canary verified)`).

## What counts as evidence here

- The client is a real Claude Code process, started with **only** the `mindspace` MCP server (`--strict-mcp-config`) and
  with Bash/Read/Write/Edit/Glob/Grep/Web/Agent disabled. Every answer therefore has to come through MCP tool calls.
- Verdicts are computed from the tool calls and tool results in the client's `stream-json` transcript and checked against
  independent oracles: the repository source (grep), raw byte scans of the database and its WAL, file fingerprints
  (content, size, mtime) of the read-only and outside trees, SQLite `integrity_check`. The model's prose is not used.
- **Not** an interactive session. `claude -p` is the same client and MCP stack, but permission prompts, the TUI and
  `/mcp` UI were not exercised (see "What you can run yourself").
- The crash injection (SIGKILL of the server during an ingest) uses a raw stdio driver for the kill only; the recovery
  is then performed and checked through the real client.

## Reproduce

```sh
cargo build --release
python3 scripts/mcp-e2e-claude.py --keep      # ~70 s, uses your Claude account; exit 0 only if all checks pass
```

## Result: 40/40 checks PASS (current binary), 33/40 on the pre-MIND-18 binary (`187806c`)

| # | Area | Checks | Result |
|---|---|---|---|
| S1 | Server connected, 14 tools discovered by the client | 2 | PASS |
| S2 | `repo_outline` + `repo_ingest` through MCP (1.0 s, 57 Rust files, 2,756 entities, 0 parse errors); `git_command`, `ingest_confined`, `GitPin`, `SandboxState`, `enter`: index path and line equal the source | 8 | PASS |
| S3 | Boundaries: outside root, `..`, relative path, `/etc`, `.git` symlink, `gitdir:` pointer, symlinked directory all refused; ro root: outline allowed, ingest refused; no outside or ro bytes in the DB; ro and outside trees byte-identical | 13 | PASS |
| S4 | New server process: data still there, re-ingest of an unchanged repository has 0 new derivations, same entity id, no duplicate | 3 | PASS |
| S5 | New commit (rename, new fn), tracked symlink to `/etc/passwd`, syntax-broken, invalid-UTF-8 and odd-named files, untracked FIFO, untracked 60 MB file, uncommitted edit: ingest 0.45 s; renamed symbol current and old one stale; uncommitted and untracked content absent; `/etc/passwd` not read; working tree unchanged | 7 | PASS |
| S6 | SIGKILL during ingest (verified partial state), then integrity ok and the client's re-ingest completes it | 5 | PASS |

Discrimination: against `187806c` the same script fails 7 checks (sandbox not enforced, `.git` symlink and `gitdir:`
pointer accepted, **outside token written into the database**, **outside tree modified**; the cause was not isolated in this run, it is consistent with the identity marker written through the link, as reproduced in MIND-18).

Additional runs (manual, transcripts in the scratchpad): 24 SIGKILL trials at random delays on fresh databases, all
`integrity_check ok`, final state equal to a clean reference, a second ingest idempotent; a genuine partial state
(git trees stored, code entities missing) was seen 4 times and repaired by re-ingest.

Re-run after the script cleanup (Pyright: 0 errors): 40/40 again.

## Findings

No release blocker found. Observations, in order of importance:

1. **First ingest of a documentation-heavy repository is slow and exceeds the default deadline.** `llama.cpp` (3,003
   files, 220 ingestable documents, no supported source language, 629 MB `.git`): 101.7 s with `--ingest-timeout 120`;
   96.5 s with the default (30 s) and `over_limit: true`, result correct (integrity ok, 35,832 trees). Claude Code kept
   the call open for the whole time and did not time out; no progress signal exists. This is the documented write-phase
   semantics (ADR-026), but it is the main usability risk for a first preview and the input for the MIND-20 decision.
   Not analyzed: where the time goes.
2. **Only Rust is indexed as code.** On `llama.cpp` the index has 0 code entities and `code_search llama_decode`
   returns nothing. The agent reported this correctly instead of guessing. The README should say so plainly.
3. **The ingest result does not tell an agent enough:** `files.sample` is capped at 10 of N entries, so the files with
   parse errors cannot be identified (3 here: broken, invalid-UTF-8, odd-named; confirmed in the DB), and there is no
   explicit HEAD commit field. `code_search` returns an enum but not its variants.
4. **A one-commit change re-derives all documents:** 70 new derivations and `stale_retired: 137` after changing five
   files, because documents are bound to the commit. Correct, but wasteful (MIND-19 territory).
5. **Message inconsistency (cosmetic):** the refusal for a `gitdir:` pointer does not list the allowed roots, the others do.

Tracking: finding 1 is MIND-22 (time profile, no optimization yet) and MIND-20 (job model); the partial state seen after
SIGKILL (git trees without code entities, integrity ok, repaired by re-ingest) is MIND-23; finding 2 is MIND-14;
finding 4 is MIND-19.

## Limits of this test

Verified: everything in the table above, on Linux 7.2 with ABI >= 3. Not tested: other MCP clients, an interactive
Claude Code session, Claude Code's MCP tool timeout configuration, macOS/Windows (the sandbox is Linux-only and
non-Linux protected ingest is disabled by design), repositories larger than ~630 MB, concurrent clients on one
database, and an adversarial agent (the agent here cooperates; the attack coverage is MIND-18's test suite).

## What you can run yourself in a live session

```sh
claude mcp add mindspace -- /abs/path/target/release/mind --db /abs/dedicated-dir/mind.db mcp \
    --allow-root /abs/repos:rw --allow-root /abs/reference:ro
claude        # then: /mcp  (mindspace should be connected), ask it to repo_ingest a repository and answer a symbol question
```

The database directory must be dedicated (not inside an allowed root). The result of every `repo_ingest` shows
`sandbox: enforced ...`; if it says anything else, stop and report it.
