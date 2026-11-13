# Mindspace

Mindspace is a local-first persistent memory and code search tool for AI coding
agents. It is written in Rust, stores its data in a local SQLite database, and is
used through the `mind` command-line tool or as a stdio
[MCP](https://modelcontextprotocol.io) (Model Context Protocol) server.

> **Status:** `v0.1.0-alpha.1` is a source-only public Alpha for Linux x86_64,
> published as a GitHub prerelease. It is the only published release.
> [Download it from the release page](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1).

## Why use it

- **Agents forget between sessions.** Mindspace keeps the decisions and notes an
  agent or you explicitly store, so a later session can search them.
- **Your data stays local.** One SQLite database on your machine. No account, no
  hosted service and no required daemon.
- **No AI model needed.** Parsing and lexical search are deterministic.
- **Results keep their origin.** Each hit shows its source and, for Git content,
  the commit. `mind inspect` shows the full evidence chain.
- **Nothing is recorded automatically.** An agent must explicitly write a memory
  or ingest a source.

## Features in this Alpha

| You can | With |
| --- | --- |
| Create, update or retire an explicit memory | MCP `memory_create`, `memory_update`, `memory_retire` |
| Ingest Markdown/text files and committed Git content | `mind ingest`; MCP `repo_ingest` for allowed repositories |
| Search memories and inspect their evidence and history | `mind search`, `mind recall`, `mind inspect`; MCP memory tools |
| Find Rust definitions with file and commit provenance | `mind symbols`; MCP `code_search` |
| Group content into named spaces | `mind space`; MCP space tools |

Not part of this Alpha: canonical document/PDF ingestion, M8 document
publication, and embedding-based (semantic or hybrid) search. Rust is the only
structurally parsed language. There is no CLI command to create a memory; that
goes through MCP.

## Install

The Alpha is distributed as a signed source archive attached to the
[v0.1.0-alpha.1 release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1).
**Do not build from a clone, Download ZIP or the "Source code" archives on the
release page.** They contain older code:

| Source | What it contains |
| --- | --- |
| Release asset `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz` | The qualified Alpha source (RC.5). Verify and build this. |
| Public `main` (clone, Download ZIP) | An older implementation plus the current documentation. |
| Tag `v0.1.0-alpha.1` and its generated "Source code" ZIP/tar.gz | The older commit `92f68ff646ec9b06ba21df2fb7879dfbc1281838`, which only anchors the release page. |

1. Download all seven assets from the release page.
2. Authenticate the publisher's signing key through a channel you trust, then
   follow `VERIFY.txt` to check both signatures and the checksums **before
   extracting or building**. A key or checksum found next to the download does
   not authenticate the publisher on its own.
3. Build from the verified, extracted source tree. You need Rust/Cargo 1.98.1
   (the toolchain the archive pins), Git, a C compiler and C development
   headers. Public `main` uses a different development toolchain; see
   [CONTRIBUTING](CONTRIBUTING.md#rust-toolchain).

   ```sh
   cargo build --locked --release --no-default-features --bin mind --target-dir target/alpha-noembed
   ./target/alpha-noembed/release/mind --version   # must report mind 0.1.0-alpha.1
   ```

4. Install the binary at a stable path and add it to your `PATH`, as described
   in the [source-install guide](docs/source-install.md#4-install-into-a-new-stable-prefix).

Archive SHA-256: `b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0`.
Signing key fingerprint: `0E6699CB48F3941AE28CA88D509F78007F62D5EA`.
The [release notes](docs/release-notes.md) have the full provenance details.
No prebuilt package has been qualified.

## Quickstart

This indexes a Git repository you already have and searches it. No AI model or
agent account is needed.

**Before you start**

- `mind --version` prints `mind 0.1.0-alpha.1`.
- Pick a repository you trust. Command-line ingestion is not sandboxed.
- Ingesting a Git repository writes one small marker file,
  `.git/mindspace-repository-id`, into that repository. Tracked files and the
  working tree are not changed.
- The example uses a new database. If you already have a Mindspace database,
  read [offline recovery](docs/offline-recovery.md) before opening it with a
  new binary.

```sh
export MINDSPACE_DB="$HOME/mindspace-quickstart/mind.db"

mind init
mind space create myproject
mind ingest /path/to/your/repository --space myproject
mind search "retry" --space myproject
```

Replace the repository path and the search word with your own.

**What to expect**

- `mind init` creates the database and prints its path.
- `mind ingest` prints a summary, then the committed Markdown, text and Rust
  files it read and the memories it created from them. Uncommitted and
  untracked files are skipped. A large repository can take minutes.
- `mind search` prints ranked hits. Each hit shows a snippet, its source file and
  commit, and a memory id that you can pass to `mind inspect <memory-id>`. If
  nothing matches, it prints `no results`.

If the repository contains Rust, look up a definition by name:

```sh
mind symbols parse_config --space myproject
```

To index a single note instead, pass a Markdown or text file to `mind ingest`.
For a complete walk-through with a throwaway repository, re-ingestion after a
commit and creating a memory over MCP, see sections 6 to 9 of the
[source-install guide](docs/source-install.md#6-initialize-a-private-demo-database).

## Use it from an AI agent (MCP)

`mind setup` registers Mindspace as an MCP server with your coding agent. It
shows a preview first and changes the client's configuration only with `--apply`:

```sh
mind setup claude-code           # preview
mind setup claude-code --apply   # register
```

`mind setup codex` does the same for Codex CLI. OpenCode is preview-only.
Existing entries are refused rather than overwritten.

Registration starts with no repository access: memory tools work and repository
tools refuse. To share a repository, undo the registration and run setup again
with `--ro /absolute/path` (recommended: Mindspace can read and outline it, and
cannot change it) or `--rw /absolute/path` (Mindspace can also ingest it, which
includes writing its marker file into the repository's `.git`; setup asks you
to confirm write access). Repository tools need Linux Landlock and stay off if
confinement cannot be confirmed; `mind doctor` shows the status. Sharing a
folder lets the connected agent see its content; it does not restrict the
agent's other tools.

Setup registers the database that is selected when you run it: `--db`, then
`MINDSPACE_DB`, otherwise the default `$XDG_DATA_HOME/mindspace/mind.db`. Check
the `database` line in the preview. Use the same database path for the CLI and the MCP client, and keep
the database outside any allowed repository.

See [setup and undo](docs/setup.md) and the
[manual MCP configuration](docs/reference.md#manual-mcp-configuration) examples.

## Documentation

- [Documentation index](docs/index.md)
- [Source installation, verification and uninstall](docs/source-install.md)
- [CLI and MCP reference](docs/reference.md)
- [Client setup and undo](docs/setup.md)
- [Offline backup and recovery](docs/offline-recovery.md)
- [Architecture and security boundaries](docs/architecture.md)
- [Release notes](docs/release-notes.md)
- [Development and checks](CONTRIBUTING.md)

## Important limitations

- **Linux x86_64 only.** Other platforms are not qualified.
- **MCP is local, unauthenticated stdio.** Anyone who can reach the process can
  use its tools, including writes. Spaces organize retrieval; they are not
  access controls.
- **The sandbox has a defined scope.** It confines the MCP repository tools. It
  is not full network or IPC isolation and does not protect against hostile
  same-user or root processes. Command-line ingestion is not confined.
  `--insecure-no-sandbox` removes the kernel boundary and is outside the
  security guarantee.
- **Storage is not encrypted**, and deleting a source is not secure erasure.
- **No built-in backup.** There is no `mind backup` or `mind restore`. Before an
  upgrade, follow the [offline recovery runbook](docs/offline-recovery.md) and
  keep the old binary. The recovery procedure is for your own trusted database
  only; see the [stop rules](docs/source-install.md#10-offline-backup-restore-and-pre-upgrade-stop-rules).
- **Alpha compatibility.** Commands and the database schema can change.

The complete list, with sizes and edge cases, is in the
[reference](docs/reference.md#limits-and-security). The security boundaries are
described in the [architecture overview](docs/architecture.md#security-boundaries)
and [sandbox capabilities](docs/sandbox-capabilities.md).

## Security reporting

Report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(GitHub sign-in required). Do not put details in public issues, pull requests or
discussions. See the [security policy](SECURITY.md) for scope and what to
include. Responses are best effort.

## License

MIT; see [LICENSE](LICENSE). The Alpha archive includes a tokenizer test fixture
under Apache 2.0, covered by the [third-party notices](THIRD_PARTY_NOTICES.md)
and the [Apache 2.0 license text](LICENSES/Apache-2.0.txt). No model weights are
included.
