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

**Status: unreleased `0.1.0-alpha.1` review candidate.** No public tag or release is asserted. Earlier0.x numbers, including0.5.0 and tags `v0.2.0`/`v0.3.0`, were internal milestones. The initial Legacy/Git profile is source-only Linux without embedding models. Canonical document publication and document-complete Alpha remain blocked; PDF extraction is incomplete. Command-line/JSON shapes and schema can change. Read [Known limits](#known-limits) and the [source-install/operator guide](docs/source-install.md) before using existing data.

- **Local.** One binary (`mind`), one SQLite database. No remote server or
  account is required. The optional model installer explicitly downloads pinned
  Hugging Face assets. Qualified model-free CLI and zero-root MCP search flows
  had no observed network syscalls; this is not a promise that every flow or
  external Git/client process is network-silent. Sandbox startup actively probes
  its denied interfaces. Confined MCP also restricts new sockets and selected
  syscall paths; see [Security model](#security-model).
- **Deterministic.** Parsing, Git facts and lexical search need no AI model.
  Semantic search is an optional extra.
- **Inspectable.** A memory traces back to its evidence and source. Old
  versions are superseded, never silently overwritten.
- **Sandboxed for agents.** The MCP repository tools restrict their *file
  system* access with a Linux Landlock sandbox to the directories you allow,
  the database directory and what the process needs to run. Additional scoped
  syscall controls apply to confined MCP, with residual stdio/inherited-descriptor
  and other interface limits (details under [Security model](#security-model)).

## What it does today

| You want to... | Use | Needs a model? |
| --- | --- | --- |
| Remember a decision or fact and find it later | **Memory**: `memory_create`, `memory_search` (MCP) | No |
| Ask "where is `Foo` defined, at which commit?" | **Repo ingestion** + `code_search` / `mind symbols` | No |
| Search notes and docs by meaning, not keywords | **Semantic search** (`--mode semantic`/`hybrid`) | Yes, optional, about 550 MB |

**Memory.** Three different things feed it, and they are kept apart:
*explicit creation* (you or an agent deliberately write a fact or decision,
for example through the MCP tool `memory_create`), *legacy file ingestion*
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
the commit they came from. Re-ingesting an unchanged repository reuses derived content; session/revision
journals need not be unchanged. This is existing legacy Git ingestion, not M8
canonical document publication.

**Semantic search (optional, outside the initial model-free Alpha profile).** Search by meaning with a local embedding model
(nomic-embed-text v1.5, run on the CPU through Candle). It is off until you
enable it, install the model and run the indexer. Lexical search (SQLite FTS5)
always works and is the default.

## Install

**Public source delivery is still blocked.** The repository address is
`https://github.com/VikingOwl91/mindspace.git`, but anonymous acquisition requested
credentials in the clean-user check. No release-download URL, public tag or
credentials are supplied here. The frozen local RC.3 source archive has qualified
build and provenance evidence, but no public delivery endpoint is available.
Obtain and authenticate that exact reviewed archive before building; otherwise stop.
See the [release-preparation decision](docs/releases/v0.1.0-alpha.1-release-preparation.md)
for recipient verification, remaining reporting/hosting gates and artifact identity.

Use the [source-install/operator guide](docs/source-install.md) for the full
validated local-candidate path, stable version-specific installation/PATH,
configuration, MCP, Git, recovery and uninstall instructions.

Requirements: pinned Rust/Cargo1.98.1, Git, a C compiler and development headers.
The recommended no-embeddings profile needs no weights. The alternative default
build also requires a **C++ compiler** (`g++` on Debian) and libstdc++ at runtime.
SQLite/FTS5 is bundled; a system SQLite server/CLI is not required. Linux is the
qualified platform; MCP repository tools require effective Landlock ABI3+.

From the authorized candidate tree:

```sh
cargo build --locked --release --no-default-features --bin mind --target-dir target/alpha-noembed
./target/alpha-noembed/release/mind --version   # must report mind 0.1.0-alpha.1
```

The guide installs this ordinary binary into a new stable prefix. Do not copy a
test/`--all-targets` artifact; the dev self-dependency enables a test-only entrypoint.
No prebuilt package or public installation channel has been qualified.

## Quickstart (no AI model needed)

After the guide's installation/PATH step, this creates a new demo database and a
small committed repository. It needs no existing Git identity/signing setup and
works even when the authorized source tree was supplied without `.git`.

```sh
export DEMO_ROOT="$(mktemp -d "$HOME/mindspace-alpha-demo.XXXXXXXX")"
export MINDSPACE_DB="$DEMO_ROOT/data/mind.db"
mind init
mind space create demo
mkdir -m 700 "$DEMO_ROOT/repo"
git -C "$DEMO_ROOT/repo" init -q -b main
printf '# Decision\n\nAlphaGitToken records a committed decision.\n' > "$DEMO_ROOT/repo/decision.md"
printf 'pub fn alpha_demo_symbol() {}\n' > "$DEMO_ROOT/repo/lib.rs"
git -C "$DEMO_ROOT/repo" add .
git -C "$DEMO_ROOT/repo" -c user.name='Mindspace Demo' -c user.email='demo@example.invalid' -c commit.gpgsign=false commit -qm initial
mind ingest "$DEMO_ROOT/repo" --space demo
mind ingest "$DEMO_ROOT/repo" --space demo
mind symbols alpha_demo_symbol --space demo
mind search AlphaGitToken --space demo --json
mind space list
mind doctor
```

Explicit manual memory creation/update/retirement uses MCP/library operations;
there is no corresponding CLI create command. The guide includes a small stdio
MCP example. Legacy Markdown/text ingestion and Git notes remain distinct from
blocked canonical document publication. Doctor does not migrate the selected DB
or download models; it may create SQLite coordination files and temporary self-test
storage. Keep the demo path separate from existing user data.

Historical self-ingestion example: at commit `5cfacb1`, the symbol output was:

```text
1. function ingest_confined
   pub fn ingest_confined<S: AsRef<str>>(
   src/ingest/confined.rs:392-442  @ 5cfacb1c320e in VikingOwl91/mindspace   (file, lines, commit, repository)
   spaces: demo
```

That old snapshot is not a promise of current line numbers or complete Rust grammar
coverage. Current self-ingestion reports a recovered struct-pattern attribute in
`src/cli/mod.rs`; recovered derivations may be partial.

## Connect your agent (`mind setup`)

MCP (Model Context Protocol) is how a coding agent talks to local tools. `mind setup` registers Mindspace with
**Claude Code** or **Codex CLI** (and shows the exact command for **OpenCode**), through the client's own `mcp add`.
The default is a **zero-permission setup**: no folder is shared, the registration is complete, the agent gets all the
memory tools and Mindspace can read no repository files.

```sh
mind init                                  # once
mind setup claude-code                     # preview, nothing is changed (or just `mind setup`, guided, in a terminal)
mind setup claude-code --apply             # register after you confirm; the result is read back
mind doctor                                # check it
```

Sharing repository folders is optional, for example `mind setup claude-code --ro ~/projects --apply`
(`~/projects` is only an example and is never created). **`--ro`** lets Mindspace read repository files inside the
folder and not change them (recommended); **`--rw`** lets it read and change them, and you must confirm that
explicitly. These are Mindspace permissions: they say nothing about
what your agent or its other tools may do, and read access is not a confidentiality guarantee towards the agent. Where no
effective sandbox exists the repository tools stay off. Setup never overwrites an existing `mindspace` entry. Details,
the per-client differences, undo and the limits are in [`docs/setup.md`](docs/setup.md).

## Manual MCP configuration (Claude Code)

Mindspace ships an MCP server (`mind mcp`, stdio only). `mind setup` does the following for you; to do it by hand, Create the database and
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
- Without `--allow-root`, ordinary repository tools remain advertised but their
  calls are refused; the memory tools still work.
- Check the sandbox: every `repo_ingest`/`repo_outline` result (visible in the
  tool-call details of your client) contains a `sandbox` field. It must read `enforced (Landlock, ABI 3 or newer, fully
  enforced, canary verified)`. If the sandbox cannot be established, the
  repository tools switch themselves off and say why.

Historical client evidence: a project-level `.mcp.json` with this content was picked up
by Claude Code 2.1.319 (headless) as a connected project server and answered a
tool call. The same configuration (loaded with `--mcp-config`) was exercised with a real Claude Code 2.1.319 client in headless mode by
[`scripts/mcp-e2e-claude.py`](scripts/mcp-e2e-claude.py): tool discovery,
ingestion, symbol questions checked against the source, refusal of paths
outside the roots, restart, re-ingest, repository changes and crash recovery
(40/40 checks, report in
[`docs/reviews/mind-21-mcp-e2e.md`](docs/reviews/mind-21-mcp-e2e.md)). `mind setup` is tested against the real Claude Code 2.1.319 and Codex CLI 0.167.0 in throw-away configuration
directories (registration, read-back, refusal of an existing entry, foreign entries). Not
verified: the interactive UI of the clients, and OpenCode registration (preview only). A Codex and an OpenCode example are
in [`docs/reference.md`](docs/reference.md).

The clean-user qualification additionally exercised Codex0.166.0 native
registration/readback/undo with fresh configuration, not a client UI or
model-driven agent session. Keep the installed executable path stable.

Typical agent use: `repo_outline` (see what is there), `repo_ingest` with
`paths` for the parts you need, `code_search` for definitions,
`memory_search` for documents and decisions.

## Known limits

Please read these; they are the difference between a surprise and a plan.

- **Only Rust code is indexed.** Other source files are not parsed. A C++
  repository ingests its Markdown/text documents but yields no code
  definitions, and `code_search` finds nothing for its functions.
- **Git ingestion reads committed content.** Dirty/untracked worktree content is
  not read through that route. Existing legacy file/directory ingestion reads the
  selected filesystem input instead. Symbolic links are skipped in directory
  ingestion and are not read from Git trees.
- **First ingest of a large repository can take minutes.** `--ingest-timeout`
  (default 30 s) is a deadline for the *preparation* of an ingest, **not** an
  end-to-end timeout. Once the database write starts it always runs to its
  end, so a call can far exceed the limit; the result then says
  `over_limit: true` and is complete. Observed ([report](docs/reviews/mind-21-mcp-e2e.md)): ingesting `llama.cpp`
  (3,003 files, 220 documents, 629 MB `.git`) took 96 to 102 s, and the Claude
  Code client waited for it. There is no progress report, and a client with a short
  tool timeout may give up. Use `repo_outline` and `paths` to ingest a part.
- **Crash/commit outcomes need care.** Current ordinary Git/code persistence is
  one atomic transaction in the qualified failure controls; do not rely on the
  older partial-tree warning. An error can be `CommitOutcomeUnknown`, and a
  process-kill check is not power-loss certification. Preserve state and use the
  [offline recovery procedure](docs/releases/v0.1.0-alpha.1-recovery-qualification.md);
  never delete WAL/SHM or copy only live MAIN as a repair.
- **Re-ingesting a changed repository re-derives all its documents** (they are
  bound to the commit), which costs time on big repos.
- **Models are optional and outside the initial model-free profile.** The default
  build can request the pinned model when you explicitly run `mind embeddings enable`
  (about550MB); indexing is also required. Search never downloads/indexes automatically.
  Noembed strict semantic/hybrid requests refuse and Auto reports lexical fallback.
  Real weights/inference are not newly qualified here; see the [operator guide](docs/source-install.md).
- **Local and unauthenticated.** MCP is stdio only. Anyone who can start or
  talk to the process can call all its tools, including writes. Spaces
  organize retrieval; they are not access control.
- **Platforms.** The repository tools need Linux6.2+ with effective Landlock ABI3+.
  Qualification includes named native/private-btrfs and rootless Debian13/vfs
  profiles on kernel7.2, not arbitrary containers, distributions or kernels.
  Nix/Guix, overlay, NFS/FUSE, `hidepid` and other operating systems are not
  implied qualified by those runs.
- **Private local data.** New admitted DB/WAL/SHM files are0600 and new private
  directories0700. Existing permissive files are not silently changed. Storage
  is not encrypted and history/source text can remain after source deletion.
  Ordinary opens can migrate; preserve a validated backup and old binary before
  an upgrade. See the [operator guide](docs/source-install.md) for STOP conditions.
- **Partial Rust structure.** Tree-sitter can retain a `recovered` derivation,
  including for valid syntax its pinned grammar does not fully cover. Check parse
  status; complete structural coverage is not promised.
- **Unreleased candidate.** Schema17 and protocol versions are independent of the
  deliberate reset from internal0.5.0 to package0.1.0-alpha.1. No public release
  support or arbitrary downgrade/migration compatibility is implied.

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

Confined MCP with roots on the qualified x86_64 profile additionally denies new
sockets, `io_uring_setup`, `pidfd_getfd`, `ptrace`, `process_vm_readv` and
`process_vm_writev`. Ordinary `readv` is not that cross-process operation.
Pipe/socketpair and already-open descriptors such as stdio remain available;
this is not complete network/IPC isolation. CLI and zero-root MCP do not receive
that repository filter. The full analysis, test method and residual risks are in ADR-029 (ADR =
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
| Stdio MCP tools, real-binary protocol tests | Implemented | `tests/mcp*.rs` |
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
cargo test --locked                     # inspect ignored/optional coverage separately
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo deny check && cargo audit         # dependencies: advisories, licenses
python3 scripts/check-doc-links.py      # relative links in the docs
scripts/security-profile.sh             # sandbox gate (Linux, ~5 min)
python3 scripts/mcp-e2e-claude.py       # real Claude Code client (needs `claude`, uses your account)
scripts/bench-baseline.sh               # benchmarks
```

## More documentation

- [Source installation and operator guide](docs/source-install.md): candidate acquisition, prerequisites, stable install, MCP/Git, private storage, recovery and uninstall.

- [`docs/setup.md`](docs/setup.md): `mind setup`, `--ro`/`--rw`, the three clients, undo and limits
- [`docs/doctor.md`](docs/doctor.md): what `mind doctor` checks, its exit codes and JSON format
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
