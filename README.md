# Mindspace

> Local-first memory and code intelligence for AI agents.

In practical terms: a persistent, inspectable memory and code index that AI
coding agents (for example Claude Code, over MCP) can query across sessions.

Mindspace keeps what you and your agents know in one local SQLite file, with
the evidence behind it and its history over time, and lets an agent query it
over [MCP](https://modelcontextprotocol.io). It indexes your notes and your
Git repositories (Rust code is parsed into definitions), answers "where is X
defined?" and "what did we decide about Y?", and can tell you where each
answer came from.

**Status: pre-alpha, version 0.5.0** (the crate version; earlier 0.x numbers
were internal milestones, only `v0.2.0` and `v0.3.0` are tagged). Command-line
output, JSON shapes and the database schema can still change. Read [Known limits](#known-limits) before you
point it at anything you care about.

- **Local.** One binary (`mind`), one SQLite file. No server and no account.
  In Mindspace's own source the only network client is the optional
  embedding-model download (`mind model install`, from Hugging Face, checked
  against a pinned manifest); a search of the source found no other HTTP or
  socket use outside tests. This was not verified for external processes such
  as the `git` binary it runs, nor by observing traffic, so it is not a
  guarantee of network silence.
  The sandbox does not block network access (see [Security model](#security-model)).
- **Deterministic.** Parsing, Git facts and lexical search need no AI model.
  Semantic search is an optional extra.
- **Inspectable.** A memory traces back to its evidence and source. Old
  versions are superseded, never silently overwritten.
- **Sandboxed for agents.** The MCP repository tools restrict their *file
  system* access with a Linux Landlock sandbox to the directories you allow,
  the database directory and what the process needs to run. Network access,
  signals and reading `/proc` are not restricted (details and limits under
  [Security model](#security-model)).

## What it does today

| You want to... | Use | Needs a model? |
| --- | --- | --- |
| Remember a decision or fact and find it later | **Memory**: `memory_create`, `memory_search` (MCP) | No |
| Ask "where is `Foo` defined, at which commit?" | **Repo ingestion** + `code_search` / `mind symbols` | No |
| Search notes and docs by meaning, not keywords | **Semantic search** (`--mode semantic`/`hybrid`) | Yes, optional, about 550 MB |

**Memory.** Three different things feed it, and they are kept apart:
*explicit creation* (you or an agent deliberately write a fact or decision,
for example through the MCP tool `memory_create`), *document-based ingestion*
(`mind ingest` turns Markdown/text files and Git repositories into memories
through deterministic parsing, each tied to its source file and commit) and
*automatic conversation capture*, which **does not exist**: Mindspace does not
record chats or sessions and does not extract memories from conversations by
itself; an agent remembers only what it explicitly writes. Memories belong to *spaces*. A space is a named
knowledge domain such as `coding` or `project-x`; spaces can import each other
so a project can use general knowledge without everything being visible
everywhere (`mind space link coding linux`). Each memory has a
lifecycle: active, superseded by a newer one, or retired. You can ask what was
true at an earlier time (`mind search --at`) and list conflicting memories.

**Repo ingestion.** Reads *committed* Git objects and refs (not your
uncommitted working tree) into the database. Markdown and text files become
searchable documents. Rust files are parsed with Tree-sitter into definitions
(functions, methods, structs, enums, traits, modules, constants, ...) with file, line range and signature, tied to
the commit they came from. Re-ingesting an unchanged repository does no new
work.

**Semantic search (optional).** Search by meaning with a local embedding model
(nomic-embed-text v1.5, run on the CPU through Candle). It is off until you
enable it, install the model and run the indexer. Lexical search (SQLite FTS5)
always works and is the default.

## Install

Requirements: Linux (the MCP repository tools need Linux 6.2 or newer with
Landlock; only Linux has been built and tested), Git, a C compiler (SQLite and
Tree-sitter are built from source) and a Rust toolchain. The repository pins Rust 1.98.1 in
`rust-toolchain.toml`, which `rustup` installs for you (install Rust with
[rustup](https://rustup.rs) if you do not have it).

```sh
git clone https://github.com/VikingOwl91/mindspace.git
cd mindspace
cargo build --release          # produces target/release/mind
```

```sh
export PATH="$PWD/target/release:$PATH"   # so that `mind` works in this shell
mind --version
```

This is the only install path that has been tested; there are no prebuilt
binaries yet. The build takes a few minutes.

## Quickstart (no AI model needed)

This ingests the Mindspace checkout itself, so the example queries have
something to find. Run it from the repository directory after building:

```sh
export MINDSPACE_DB="$HOME/.local/share/mindspace-demo/mind.db"   # any path outside a repository; created if missing
mind init
mind space create demo
mind ingest . --space demo                        # a Git repository with at least one commit, or a folder of notes
mind symbols ingest_confined --space demo         # where is this defined? (Rust definitions)
mind search "sandbox minimum ABI" --space demo    # keyword search over the documents
mind space list                                   # see what is stored
```

To use your own project, replace `.` with its path. Code definitions appear
only for Rust files; Markdown and text files are searchable for any project.
A real run on this repository (commit `5cfacb1`, 0.9 s to ingest) printed, for
`mind symbols`:

```text
1. function ingest_confined
   pub fn ingest_confined<S: AsRef<str>>(
   src/ingest/confined.rs:392-442  @ 5cfacb1c320e in VikingOwl91/mindspace   (file, lines, commit, repository)
   spaces: demo
```

and, for `mind search`, the matching section of the architecture decision
record with its source file and commit, so you can see where it came from.
Add `--json` to any command for machine-readable output (unstable format).

## Use it from Claude Code (MCP)

Mindspace ships an MCP server (`mind mcp`, stdio only). Create the database and
a space first (`mind init`, `mind space create demo`, using the same
`--db`/`MINDSPACE_DB` as in the config below), then add a project-level
`.mcp.json` (or pass the same file with `claude --mcp-config`):

```json
{
  "mcpServers": {
    "mindspace": {
      "command": "/abs/path/to/mind",
      "args": [
        "--db", "/abs/path/to/data/mind.db",
        "mcp",
        "--allow-root", "/abs/path/to/your/repos:rw",
        "--allow-root", "/abs/path/to/reference-repos:ro"
      ]
    }
  }
}
```

Rules that matter:

- The **database directory must be its own directory** and must not contain, or
  lie inside, an allowed root. (The default location qualifies.)
- `:rw` roots may be ingested; `:ro` roots may only be outlined. Always write
  the suffix. Allow specific project folders, not your home directory.
- Without `--allow-root`, `repo_outline` and `repo_ingest` are off; the memory
  tools still work.
- Check the sandbox: every `repo_ingest`/`repo_outline` result (visible in the
  tool-call details of your client) contains a `sandbox` field. It must read `enforced (Landlock, ABI 3 or newer, fully
  enforced, canary verified)`. If the sandbox cannot be established, the
  repository tools switch themselves off and say why.

What was verified: a project-level `.mcp.json` with this content was picked up
by Claude Code 2.1.319 (headless) as a connected project server and answered a
tool call. The same configuration (loaded with `--mcp-config`) was exercised with a real Claude Code 2.1.319 client in headless mode by
[`scripts/mcp-e2e-claude.py`](scripts/mcp-e2e-claude.py): tool discovery,
ingestion, symbol questions checked against the source, refusal of paths
outside the roots, restart, re-ingest, repository changes and crash recovery
(40/40 checks, report in
[`docs/reviews/mind-21-mcp-e2e.md`](docs/reviews/mind-21-mcp-e2e.md)). Not
verified: `claude mcp add` (the same settings, entered on the command line),
the interactive UI, and other MCP clients. A Codex and an OpenCode example are
in [`docs/reference.md`](docs/reference.md).

Typical agent use: `repo_outline` (see what is there), `repo_ingest` with
`paths` for the parts you need, `code_search` for definitions,
`memory_search` for documents and decisions.

## Known limits

Please read these; they are the difference between a surprise and a plan.

- **Only Rust code is indexed.** Other source files are not parsed. A C++
  repository ingests its Markdown/text documents but yields no code
  definitions, and `code_search` finds nothing for its functions.
- **Only committed content is indexed.** Uncommitted and untracked files are not
  read. Symbolic links are skipped when ingesting a folder (checked with a
  link to a file outside the folder) and are not read from Git trees.
- **First ingest of a large repository can take minutes.** `--ingest-timeout`
  (default 30 s) is a deadline for the *preparation* of an ingest, **not** an
  end-to-end timeout. Once the database write starts it always runs to its
  end, so a call can far exceed the limit; the result then says
  `over_limit: true` and is complete. Observed ([report](docs/reviews/mind-21-mcp-e2e.md)): ingesting `llama.cpp`
  (3,003 files, 220 documents, 629 MB `.git`) took 96 to 102 s, and the Claude
  Code client waited for it. There is no progress report, and a client with a short
  tool timeout may give up. Use `repo_outline` and `paths` to ingest a part.
- **A crash during an ingest can leave a partial state.** After `SIGKILL` the
  database stays consistent (SQLite integrity check passes) but may hold Git
  trees without their code entities; re-running the ingest repairs it.
- **Re-ingesting a changed repository re-derives all its documents** (they are
  bound to the commit), which costs time on big repos.
- **Local and unauthenticated.** MCP is stdio only. Anyone who can start or
  talk to the process can call all its tools, including writes. Spaces
  organize retrieval; they are not access control.
- **Platforms.** The repository tools need Linux 6.2+ with Landlock. They have
  been tested on one kernel (7.2) and one distribution. Containers, Nix/Guix,
  overlay, NFS/FUSE, `hidepid` and genuinely old kernels are untested. On
  other operating systems the repository tools stay off and the code paths
  have not been compiled.
- **Pre-alpha.** The database schema has migrations, but formats may change;
  keep a copy of data you cannot recreate.

## Security model

`mind mcp` is meant to be given to an AI agent, so the agent is not trusted
with your disk and repositories you ingest are not trusted either.

1. **Kernel layer (Linux Landlock, ABI 3+).** Before serving, the process
   restricts its file system access to the allowed roots (read-write or read-only), its own
   database directory and what it needs to run, re-executes itself, and then
   *proves* the restriction with probes (reads and writes that must be denied).
   If any step fails, the repository tools are disabled; there is no silent
   downgrade.
2. **Application layer (always on).** Paths are opened and checked as
   descriptors, Git is pinned to the opened directory, `.git` symlinks,
   `gitdir:` pointers, alternates and linked worktrees that point outside the
   roots are refused, and `:ro` roots are never written (not even the
   repository identity marker that `repo_ingest` normally writes into `.git`).
3. **Hardened Git.** Git runs with user and system config ignored, hooks and
   fsmonitor off, replace refs off and `GIT_*` variables from the environment
   removed.

Opt-out: `--insecure-no-sandbox` runs only layer 2. It prints a warning on
every start and every result says `sandbox: DISABLED`. Layer 2 alone leaves one
measured gap: a file that appears in the Git directory *after* the checks
(for example an alternates file) can be read (1 leak in 5,626 randomized
attempts, 0 in 700 targeted ones). Use the opt-out only for trusted input.

Not restricted by the sandbox: network access, signals and reading `/proc`.
The full analysis, test method and residual risks are in ADR-029 (ADR =
architecture decision record) in
[`docs/decisions.md`](docs/decisions.md). Before a release, run
`scripts/security-profile.sh` (about 5 minutes, needs the kernel support).

## Architecture in one page

```text
 files / Git repos                    MCP client (e.g. Claude Code)
        |                                      |  stdio, JSON-RPC
        v                                      v
  deterministic ingest  ---------->   mind mcp  (Landlock sandbox)
  (Markdown, Git graph,                        |
   Tree-sitter for Rust)                       v
        |                          retrieval: FTS5 | vectors | RRF fusion
        v                                      |
 +----------------------- SQLite (WAL) -----------------------+
 | source -> evidence -> assertion -> memory   (+ spaces)     |
 | Git objects, refs journal, code derivations & entities     |
 +-------------------------------------------------------------+
```

- **Rust, single crate**, `rusqlite` with bundled SQLite (FTS5 included), no
  async runtime, `clap` CLI, a synchronous MCP server.
- **Provenance.** The *source* is a file at a commit (or a manual write); the
  *evidence* is the exact text taken from it; an *assertion* is the statement
  extracted from that evidence; a *memory* is an assertion with a lifecycle
  (active, superseded, retired). `mind trace` and
  `mind inspect` show the chain. Derived code facts record the extractor and
  grammar versions that produced them.
- **Temporal memory.** Every state change records its transaction time, so
  `mind search --at <time>` answers what was active then. Valid time (when a
  statement was true in the world) is approximated by the ingestion time.
  Superseded and retired memories stay inspectable, and Git refs are recorded
  as a journal.
- **Spaces** group memories by domain, link as directed imports and are
  traversed cycle-safely.

Details: [`docs/architecture.md`](docs/architecture.md),
[`docs/memory-model.md`](docs/memory-model.md) and the decision log
[`docs/decisions.md`](docs/decisions.md).

## Feature matrix

| Feature | State | Evidence |
| --- | --- | --- |
| Markdown/text ingest, lifecycle, supersession, retirement | Implemented | `tests/lifecycle.rs`, `tests/invariants.rs` |
| Git ingest (objects, refs journal, worktrees) | Implemented | `tests/git_ingest.rs`, `tests/git_hardening.rs` |
| Rust definitions (Tree-sitter), `mind symbols`, `code_search` | Implemented | `tests/code_structure.rs`, `tests/code_search.rs`, `tests/code_classification.rs` |
| Lexical search (FTS5), temporal queries (`--at`), conflicts, spaces | Implemented | `tests/retrieval.rs`, `tests/m6_temporal.rs`, `tests/scopes.rs` |
| MCP server, 14 tools, real-binary protocol tests | Implemented | `tests/mcp*.rs` |
| Separate read-only and read-write roots | Implemented | `tests/mcp_allowlist_modes.rs` |
| Landlock sandbox + application layer for repository tools | Implemented (Linux) | `tests/ingest_confinement.rs`, `tests/sandbox_failure_modes.rs`, `scripts/security-profile.sh` |
| Preparation deadline for ingest (`--ingest-timeout`) | Implemented, see limits | `tests/mcp_ingest_timeout.rs` |
| Semantic and hybrid search (local Candle model) | Experimental: optional, one model, CPU only | `tests/embeddings.rs`, `docs/retrieval-performance.md` |
| Code relationships (imports/references as observations) | Experimental: stored, not queryable yet | `tests/code_structure.rs` |
| Callers / implementors queries | Planned | |
| More languages (TypeScript, then Python) | Planned | |
| Asynchronous ingest jobs / interruptible write phase | Planned, not decided | |
| Document formats beyond Markdown/text, knowledge graph | Planned, scope open | |
| HTTP transport, authentication, PostgreSQL, GUI | Not planned for now | |

"Planned" means intended, not promised, and has no date.

## Benchmarks

All numbers come from `scripts/bench-baseline.sh` (ingest, symbol query, DB
size; median of 5 fresh databases, release build, Linux 7.2.14-zen, one local
machine, warm file cache, no CPU isolation) and from the notes in
[`docs/retrieval-performance.md`](docs/retrieval-performance.md). They describe
this machine on that day, not a guarantee. Reproduce with
`scripts/bench-baseline.sh` (set `MINDSPACE_BENCH_REPO="<repo>..."` to add your
own repositories; only a copy of their `.git` is used).

| Target (at commit) | Git objects | Ingest | Symbol query (net of 3 ms process start) | DB size |
| --- | ---: | ---: | ---: | ---: |
| Synthetic: 2,400 Rust files, 6 revisions, 206 tags | 29,976 | 5.5 s | 42 ms | 52 MiB |
| gpui-shell (`7e6298d`) | 6,575 | 3.7 s | 23 ms | 38 MiB |
| mindspace (`c6df2a9`) | 469 | 0.7 s | 9 ms | 12 MiB |

Other observations (single runs, not baselines): ingesting this repository at
`5cfacb1` took 0.9 s on a fresh database. Ingesting `llama.cpp` (documents
only, no Rust) took about 100 s, see [Known limits](#known-limits); where that time goes has
not been analyzed. Semantic search: in one MCP session a repeated query took
about 78 ms after the model was loaded, against about 590 ms before the
runtime was cached (first query of a process still verifies and loads the
547 MB model); lexical queries took about 2 ms (fixture of 100 assertions).
The benchmarks above were taken before the sandbox was added; the effect of the
sandbox on start-up time has not been measured separately. No comparison with
other tools has been made.

## Develop and test

```sh
cargo test                              # about 430 tests, no model download needed
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings
cargo deny check && cargo audit         # dependencies: advisories, licenses
python3 scripts/check-doc-links.py      # relative links in the docs
scripts/security-profile.sh             # sandbox gate (Linux, ~5 min)
python3 scripts/mcp-e2e-claude.py       # real Claude Code client (needs `claude`, uses your account)
scripts/bench-baseline.sh               # benchmarks
```

## More documentation

- [`docs/reference.md`](docs/reference.md): design principles, the full command
  list, MCP details, search modes, Codex/OpenCode configuration.
- [`docs/architecture.md`](docs/architecture.md), [`docs/memory-model.md`](docs/memory-model.md)
- [`docs/decisions.md`](docs/decisions.md): decision log (ADR-001 to ADR-029)
- [`docs/reviews/`](docs/reviews/): architecture review and the MCP end-to-end report
- [`SECURITY.md`](SECURITY.md) (how to report a vulnerability, scope) and
  [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Roadmap: there is no public roadmap file yet; the "planned" rows of the
  feature matrix are the current intent.

## License

MIT, see [`LICENSE`](LICENSE).
