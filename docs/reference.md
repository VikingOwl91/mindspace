# CLI and MCP reference

The CLI examples below work with the published v0.1.0-alpha.2 unless marked
as development features. The repository ingest, language coverage and deadline
sections describe the current development checkout. The signed archive has its
own [release notes](release-notes.md).

## Database configuration

Database precedence is `--db`, `MINDSPACE_DB`, then
`$XDG_DATA_HOME/mindspace/mind.db` (fallback
`$HOME/.local/share/mindspace/mind.db`). Use the same path for CLI and MCP.
There is no Mindspace configuration file. Initialize a selected new database
with `mind init`; follow [offline recovery](offline-recovery.md) before opening
existing data with a new binary.

## CLI examples

After the install guide's demo ([sections 6 to 9](source-install.md#6-initialize-a-private-demo-database)),
these commands use its temporary database, repository and `demo` space. The
[README quickstart](../README.md#quickstart) shows the core commands against
your own repository.

```sh
mind space list
mind search AlphaGitToken --space demo --json
mind recall AlphaGitToken --space demo
mind symbols alpha_demo_symbol --space demo
mind ingest "$DEMO_ROOT/repo" --space demo
mind doctor
```

Use `mind ingest /path/to/note.md --space demo` for an existing legacy text or
Markdown file. Git ingestion reads committed objects, not dirty/untracked files.
`--history` selects ref tips. To capture a change of HEAD, finish one ingest
before switching revisions, then ingest again.
`mind inspect <memory-id> --json` inspects a returned memory.
Create an explicit memory with `mind memory create "text" --space demo`.
Use `--subject` to set its subject or `-` to read from standard input.
Repeating a creation only adds missing spaces. MCP also supports updating a
memory. To retire it from the CLI, run `mind retire <memory-id>`.

For a trusted PDF or DOCX, the development checkout provides a separate
read-only Python command. It prints cited answers without opening the database.
[Document preview quickstart](../README.md#preview-a-document)

## Repository ingest on main

Git, directory and single-file ingestion accept UTF-8 text up to and including
32 MiB per file. The report identifies eligible paths skipped for invalid
UTF-8 or size. Re-ingest after upgrading to retire memories derived from lossy
or oversized text under the old policy. Earlier evidence remains available.
For skipped filesystem paths, check the reported previous version ID to see
whether earlier eligible content is still searchable.

Across an observed HEAD move from the same worktree,
`mind trace <memory-id>` shows the successor or predecessor when the Git path
and normalized subject match uniquely and all other views remain stable.
Deleted files, renamed paths and ambiguous matches appear as retired.
An isolated Git ingest updates only the requested checkout. Memories and
symbols at other recorded tips stay current, including tips shared by multiple
worktrees. Use an ordinary `mind ingest` to reconcile removed worktrees, changed
ref selections or older aggregate view data.
`mind search <query> --at <timestamp>` can still find the displaced episode.

The opt-in MCP tool `repo_ingest_git_isolated` reads only the requested HEAD.
It supports linked worktrees and local alternate object stores. If Git metadata
or borrowed objects live outside the checkout, the checkout and its dependencies
must fit within one configured static read-write root. Any applicable read-only
restriction still applies. Offline partial clones need the HEAD commit, its
trees and their blobs locally. A missing object returns `missing_local_object`.
The worker never fetches it. See the [Git worker layout example](https://github.com/VikingOwl91/mindspace/blob/77eeff0c028c7a5ee21672377b496660182e6e91/docs/m7.3b5e-mcp-git-tool.md#linked-worktrees-local-alternates-and-partial-clones).

## Symbol language coverage

The development checkout indexes the following syntax. Ingest the selected files
again after upgrading to build their symbol index. Existing Rust records remain
readable. The published Alpha release profile is described below.

| Language filter | File extensions | Symbols |
| --- | --- | --- |
| `rust` | rs | Modules, structs, enums, traits, aliases, functions, methods, constants, statics, fields and imports. Rust also records syntactic relationships. |
| `go` | go | Named functions, receiver methods, interface methods, types, type aliases, named fields, constants and variables in declarations. |
| `javascript` | js, mjs, cjs, jsx | Named function and class declarations, class/object methods, class fields and identifier variable declarations. Variables directly assigned arrow functions or function expressions are indexed as functions. |
| `typescript`, `tsx` | ts, mts, cts / tsx | The JavaScript declarations above plus interfaces, namespaces, enums, type aliases and member signatures. TSX uses the JSX-aware TypeScript grammar. |
| `svelte` | svelte | Declarations in top-level inline instance and module script elements. The Svelte parser locates each script, then JS or TS parses its body at the original component offsets. |

Svelte defaults to JavaScript and accepts static `lang="js"`, `"javascript"`,
`"ts"` or `"typescript"`. External `src` scripts, other script languages and
non-JavaScript MIME types are omitted. The index excludes markup, template
expressions, component usages and CSS. These extractors describe declarations;
import resolution, cross-language relations and type inference are outside
their coverage. Additional languages remain future work.

Use `mind symbols Widget --language typescript` or pass `language` to
MCP `code_search`. An unsupported language produces an error. A supported
query with zero hits means that no indexed declaration matched its filters.
`mind scan --json` and `repo_outline` report counts of observed unsupported file
extensions per directory. Directory scans honor ignore rules; Git scans outline
HEAD. Ingest reports count submitted code inputs by language, including reused
and policy-skipped inputs. Their `code.unsupported_extensions` counts selected
regular files excluded by format, including when the selected directory contains
only unsupported files. Ordinary Git counts each selected (tip, path); the
isolated Git tool derives whole-HEAD counts from its verified tree. Outlines and
ingest reports group unrecognized and extensionless names under `other`. Policy skips and recovered parses can affect results.

## Repository ingest deadlines in the development checkout

`mind ingest ./repo --space demo --ingest-timeout 60` gives one call a
60-second budget. CLI ingestion defaults to unlimited. For MCP use
`mind mcp --ingest-timeout 60` or set `MINDSPACE_MCP_INGEST_TIMEOUT=60`.
The MCP default is 30 seconds. Both options accept 0.001 through 3600 seconds.

The deadline covers extraction and the database write. The writer checks it
between work items and immediately before SQLite COMMIT. On expiry it confirms
rollback before reporting that the current root was rolled back. Earlier
selected plain-directory roots remain committed and the error reports how many.
Git path selections form a single transaction. A first Git ingest may already
have created the repository identity marker during preparation.

Retry with a larger budget or select fewer paths. Path selection still records
the reachable Git object graph. A running SQLite statement, lock wait, rollback
or COMMIT can finish after the deadline. A successful MCP result with
`over_limit: true` means the ingest committed. A reported uncertain COMMIT
requires reopening the database and checking its state. A distinct rollback
confirmation failure means COMMIT was never attempted for that root and the
writer has been discarded. Reopen the database before retrying.

The ordinary MCP tool is synchronous. Give the client enough time to receive
the result, including cleanup. The server does not process MCP cancellation
notifications during the call. See the [first-ingest benchmark](ingest-performance.md).

## Client registration

```sh
mind setup claude-code
# Review the preview before applying:
mind setup claude-code --apply
```

`mind setup codex` provides the Codex workflow. Registration starts with no
repository access. Existing entries are refused rather than overwritten; readback
verifies registration. OpenCode remains preview-only. See [setup and undo](setup.md).

## Manual MCP configuration

Use an absolute installed binary path followed by
`--db /absolute/path/to/data/mind.db mcp`. Without roots, memory tools work and
repository calls are refused. Append
`--allow-root /absolute/path/to/repository:rw` to permit ingestion. A `:ro` root
permits outlining and refuses ingestion. Keep the database outside allowed roots.

Codex TOML, adapted from the qualified reference (replace both absolute paths):

```toml
[mcp_servers.mindspace]
command = "/absolute/path/to/mind"
args = ["--db", "/absolute/path/to/data/mind.db", "mcp"]
enabled = true
```

For an MCP client using `mcpServers` JSON (replace both paths):

```json
{
  "mcpServers": {
    "mindspace": {
      "command": "/absolute/path/to/mind",
      "args": ["--db", "/absolute/path/to/data/mind.db", "mcp"]
    }
  }
}
```

An agent must explicitly create a memory or ingest content. Memory tools include
create/update/retire/get/search/history. Repository tools include `repo_outline`
and `repo_ingest`. The release supports Rust in `code_search`. The checkout
supports the additional languages listed under [symbol coverage](#symbol-language-coverage). Spaces organize retrieval
and are not access controls. MCP is local stdio without authentication.

On `main`, repository policy failures expose a stable category in `structuredContent.error.kind`.
Clients can distinguish `invalid_argument`, `root_not_authorized`, and `read_only_root`.
`too_many_files` identifies a file-count limit. `too_large` identifies a byte, frame, or
Git structure limit. `timeout` identifies a deadline. A successful ingest can include
per-file text exclusions in `skipped_files`. Isolated Git reports `missing_local_object`
when a required object is unavailable locally. Isolated writes also report `commit_state`.
`rollback_unconfirmed` with `NOT_COMMITTED` and `persisted: false` means the server
could establish that the transaction did not commit, but could not confirm rollback. `commit_outcome_unknown` with
`UNKNOWN` and `persisted: "unknown"` means the COMMIT outcome could not be established.

`server_status` (read-only) tells you which database a running server opened: the path (the home
directory is shown as `~`; pass `full_path: true` for the absolute path), what chose it (`--db`,
`MINDSPACE_DB` or the default location), the schema version, when the database was created, the
number of spaces and the server version. `cli_default` says whether a plain `mind` in the same
environment would choose the same database and, if not, how to find out which one is right.


## Limits and security

The supported profile is Linux x86_64 without embedding models. MCP repository
access requires effective Landlock ABI 3+ and a successful startup canary. CLI
ingestion has no equivalent kernel-confinement promise. Do not treat
`--insecure-no-sandbox` as an equivalent workaround. Read the
[architecture](architecture.md#security-boundaries),
[sandbox capabilities](sandbox-capabilities.md) and
[security policy](../SECURITY.md).

The following limits describe the published v0.1.0-alpha.2 archive.

- Linux x86_64 is the qualified platform. MCP repository access needs effective
  Landlock ABI 3+; other operating systems and arbitrary filesystems/containers
  are not qualified by this release.
- MCP is local unauthenticated stdio. Anyone who can access it can use its tools,
  including writes. Spaces organize retrieval; they are not access controls.
- Storage is not encrypted and source deletion is not secure erasure. New
  admitted private DB/sidecar files use `0600`, new private directories `0700`;
  existing permissive files are preserved rather than silently changed.
- Manual content and subject fields each allow 1 MiB of decoded UTF-8. MCP lines
  allow 8 MiB including delimiters. Oversized fields return tool errors;
  oversized lines close the transport. No aggregate storage quota is promised.
- Human-readable ingest/search/recall output replaces terminal controls and bidi
  overrides with `?`. Stored values and JSON remain unchanged.
- Rust is the only structurally parsed language. Recovered parses can be partial.
  Git repositories must use SHA-1. `--history` selects ref tips, not every
  ancestor's file content. Serialize ingests while changing HEAD.
- Repeating an unchanged ingest reuses derived content; session/revision
  journals can still grow.
- Large ingests can take minutes. MCP preparation deadlines do not interrupt a
  database commit. An uncertain commit result requires preserving state and
  investigating; process-kill tests do not certify power-loss recovery.
- Semantic/hybrid retrieval and real-model inference are outside this release
  profile. Package, schema and protocol versions are separate; Alpha command and
  schema compatibility can change.

Confined MCP combines Landlock, descriptor-based path checks and hardened Git
execution, including ignored user/system Git configuration and disabled hooks,
fsmonitor and replace refs. Read access can disclose repository content to the
connected agent; Mindspace's permissions do not restrict the agent's other tools.

Canonical document/PDF ingestion, unfinished M8 publication and real-model
inference are outside the qualified Alpha. There are no native `mind backup`
or `mind restore` commands; use the exact [offline recipe](offline-recovery.md).
