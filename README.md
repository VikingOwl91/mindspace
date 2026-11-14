# Mindspace

Mindspace is a local-first persistent memory and code search tool for AI coding
agents. It is written in Rust, stores its data in a local SQLite database, and is
used through the `mind` command-line tool or as a stdio
[MCP](https://modelcontextprotocol.io) (Model Context Protocol) server.

## What Mindspace does

- **Agents forget between sessions.** Mindspace keeps the decisions and notes an
  agent or you explicitly store, so a later session can search them.
- **Your data stays local.** One SQLite database on your machine. No account, no
  hosted service and no required daemon.
- **No AI model needed.** Parsing and lexical search are deterministic.
- **Results keep their origin.** Each hit shows its source and, for Git content,
  the commit. `mind inspect` shows the full evidence chain.
- **Nothing is recorded automatically.** An agent must explicitly write a memory
  or ingest a source.

## What the Alpha can do

`v0.1.0-alpha.2` is the current source-only public Alpha for Linux x86_64, published as a
[GitHub prerelease](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.2).
It supersedes `v0.1.0-alpha.1`.

| You can | With |
| --- | --- |
| Create, update or retire an explicit memory | MCP `memory_create`, `memory_update`, `memory_retire`; `mind memory create` and `mind retire` on the command line |
| Ingest Markdown/text files and committed Git content | `mind ingest`; MCP `repo_ingest` for allowed repositories |
| Search memories and inspect their evidence and history | `mind search`, `mind recall`, `mind inspect`; MCP memory tools |
| Find Rust definitions with file and commit provenance | `mind symbols`; MCP `code_search` |
| Group content into named spaces | `mind space`; MCP space tools |

Not part of this Alpha: canonical document/PDF ingestion, M8 document
publication, and embedding-based (semantic or hybrid) search. Rust is the only
structurally parsed language. Commands and the database schema can still change.

## Install

After independently authenticating the publisher fingerprint through a trusted channel outside this
download, run:

```sh
curl -fsSL https://raw.githubusercontent.com/VikingOwl91/mindspace/main/install.sh | sh -s -- --trust-fingerprint 0E6699CB48F3941AE28CA88D509F78007F62D5EA
```

The installer selects the newest published release, including prereleases, verifies its supported
signed contract and assets, and builds a versioned binary under `~/.local/lib/mindspace/`. It never
falls back silently when the selected release is unsupported or fails verification. Use
`--version 0.1.0-alpha.2` for an explicit version. The earlier RC.5 (`0.1.0-alpha.1`) retains its original pins and signatures.

It is safe to repeat: an intact selected installation exits successfully without replacement.
`--upgrade` installs a newer selected version beside old binaries; `--reinstall` explicitly replaces
the selected prefix while preserving its previous binary. Neither option changes PATH, databases,
agent configuration or MCP registrations. Run it from Fish as written too.

Prerequisites: Linux x86_64, Python 3.11+, curl, GnuPG, a C compiler, core command-line tools and the
exact Rust toolchain selected by the supported contract (alpha.2: 1.100.0; RC.5: 1.98.1). The
[install guide](docs/install.md) explains trust, script authentication, dependencies, version
selection, recovery and the remaining freshness/reproducibility limits. The fingerprint argument
alone does not establish trust.

The following PATH examples use 0.1.0-alpha.2; substitute the version printed by the installer if different.

Then make `mind` easy to run, or use its full path:

```sh
# bash or zsh
export PATH="$HOME/.local/lib/mindspace/0.1.0-alpha.2/bin:$PATH"
```

```fish
# Fish
fish_add_path ~/.local/lib/mindspace/0.1.0-alpha.2/bin
```

### Build from a checkout

The installer above is the qualified path for the Alpha. To work on Mindspace, or to
build the current `main`, build from a checkout instead:

```sh
git clone https://github.com/VikingOwl91/mindspace
cd mindspace
cargo build --release          # produces target/release/mind
target/release/mind --version  # mind 0.1.0-alpha.2
```

`mind` on your `PATH` may be an older build. `command -v mind` shows which one runs; use
`target/release/mind` explicitly or follow [If `setup` is an unrecognized subcommand](docs/setup.md#if-setup-is-an-unrecognized-subcommand).

You need Linux, Git, a C compiler and **Rust 1.96 or newer** (`rust-version` in
`Cargo.toml`; CI builds with exactly 1.96.0). The repository also contains a
`rust-toolchain.toml` that pins Rust 1.100.0 for development and CI. If you use
rustup, it installs that version for you. To build with another toolchain,
name it explicitly:

```sh
rustup toolchain install 1.96.0 --profile minimal
cargo +1.96.0 build --release
```

The installer is separate. It builds the signed release archive, which pins its own
Rust version (alpha.2: 1.100.0) and says so when it runs. A checkout build is not that
qualified archive, even where the code is the same.

## Your database and a try-out database

Mindspace keeps everything in one SQLite file. Which file a command uses is decided
in this order: `--db <path>`, then the `MINDSPACE_DB` environment variable, then the
default `$XDG_DATA_HOME/mindspace/mind.db` (`~/.local/share/mindspace/mind.db` if
`XDG_DATA_HOME` is not set).

- **Your database** is the default one. Commands without `--db` use it, and
  `mind setup` registers it for your agent. Create it once with `mind init`.
- **A try-out database** is a separate file you can throw away. Use it to learn the
  commands, always with an explicit `--db`. Nothing in it carries over to your own database.

## Quickstart

This indexes a Git repository and searches it in a throwaway database. No AI model or
agent account is needed. The commands work in bash, zsh and Fish.

**Before you start**

- `mind --version` prints `mind 0.1.0-alpha.2`.
- Pick a repository you trust. Command-line ingestion is not sandboxed.
- Ingesting a Git repository writes one small marker file,
  `.git/mindspace-repository-id`, into that repository. Tracked files and the
  working tree are not changed.

```sh
mind --db ~/mindspace-tryout/mind.db init
mind --db ~/mindspace-tryout/mind.db space create myproject
mind --db ~/mindspace-tryout/mind.db ingest /path/to/your/repository --space myproject
mind --db ~/mindspace-tryout/mind.db search retry --space myproject
```

Replace the repository path and the search word with your own. When you are done,
remove the try-out database with `rm -r ~/mindspace-tryout`.

**What to expect**

- `mind init` creates the database and prints its path.
- `mind ingest` prints a summary, then the committed Markdown, text and Rust
  files it read and the memories it created from them. Uncommitted and
  untracked files are skipped. A large repository can take minutes.
- `mind search` prints ranked hits. Each hit shows a snippet, its source file and
  commit, and a memory id that you can pass to `mind inspect <memory-id>`. If
  nothing matches, it prints `no results`.

If the repository contains Rust, look up a definition by name with
`mind --db ~/mindspace-tryout/mind.db symbols parse_config --space myproject`.
To index a single note instead, pass a Markdown or text file to `mind ingest`.
For a complete walk-through with a throwaway repository, re-ingestion after a
commit and creating a memory over MCP, see sections 6 to 9 of the
[source-install guide](docs/source-install.md#6-initialize-a-private-demo-database).

## Use it for real

Leave out `--db` to work in your own database:

```sh
mind init                              # creates your database and prints its path
mind space create myproject
mind ingest /path/to/your/repository --space myproject
mind search retry --space myproject
```

Use this database for the agent too (next section), so the command line and the
agent see the same memories.

## Connect Claude Code or Codex

`mind setup` registers Mindspace as an MCP server with your coding agent. It
shows a preview first and changes the client's configuration only with `--apply`:

```sh
mind setup codex            # preview, changes nothing
mind setup codex --apply    # register
```

Use `claude-code` instead of `codex` for Claude Code. OpenCode is preview-only.
Existing entries are refused rather than overwritten, the change is read back from
the client, and a backup of its configuration is kept. See
[setup and undo](docs/setup.md).

Setup registers the database selected when you run it, in the order described
under [Your database and a try-out database](#your-database-and-a-try-out-database).
Check the `database` line in the preview: it must be your own database, not a
try-out one.

Registration starts with no repository access: memory tools work and repository
tools refuse. To share a repository, undo the registration and run setup again
with `--ro /absolute/path` (recommended: Mindspace can read and outline it, and
cannot change it) or `--rw /absolute/path` (Mindspace can also ingest it, which
includes writing its marker file into the repository's `.git`; setup asks you
to confirm write access). Repository tools need Linux Landlock and stay off if
confinement cannot be confirmed. Sharing a folder lets the connected agent see
its content; it does not restrict the agent's other tools. Keep the database
outside any shared repository. For manual configuration, see the
[MCP configuration examples](docs/reference.md#manual-mcp-configuration).

`mind doctor` checks your installation. Its output is long; start with the
`PASS`, `WARN`, `FAIL` and `SKIP` lines, as the [install guide](docs/install.md#after-installing) explains.

## Security and provenance

The installer builds only the signed source archive attached to the release.
Other sources differ from the signed archive:

| Source | What it contains |
| --- | --- |
| Release asset `mindspace-0.1.0-alpha.2-source.tar.gz` | The qualified Alpha source. The installer verifies and builds this. |
| Public `main` (clone, Download ZIP) | The development line: the release source plus later changes, the current documentation and `install.sh`. Not signed, not the qualified archive. |
| Tag `v0.1.0-alpha.2` and its generated "Source code" ZIP/tar.gz | The release commit of the archive's source baseline, including files that are not part of the archive. |
| Tag `v0.1.0-alpha.1` (superseded) | The older commit `92f68ff646ec9b06ba21df2fb7879dfbc1281838`, which only anchors that release page. |

The archive's SHA-256 is in `SHA256SUMS` and in the release notes of the published release.
Signing key fingerprint: `0E6699CB48F3941AE28CA88D509F78007F62D5EA`. The
[release notes](docs/release-notes.md) describe the provenance, and the
[manual verification steps](docs/source-install.md#1-obtain-and-verify-the-source-archive)
are there if you prefer to verify without the installer. The signed archive's own
documents still carry pre-release wording; that is historical and explained in the
[install guide](docs/install.md#about-wording-inside-the-signed-archive).

Important limitations:

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

The complete list is in the [reference](docs/reference.md#limits-and-security).
The security boundaries are described in the
[architecture overview](docs/architecture.md#security-boundaries) and
[sandbox capabilities](docs/sandbox-capabilities.md).

**Vulnerability reporting:** use
[GitHub private vulnerability reporting](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(GitHub sign-in required). Do not put details in public issues, pull requests or
discussions. The [security policy](SECURITY.md) explains scope and what to
include. Responses are best effort.

## Documentation

- [Install guide](docs/install.md): the installer, the trust decision, troubleshooting
- [Documentation index](docs/index.md)
- [Source installation and operator reference](docs/source-install.md): manual verification, build requirements, backup and recovery, uninstall
- [CLI and MCP reference](docs/reference.md)
- [Client setup and undo](docs/setup.md)
- [Offline backup and recovery](docs/offline-recovery.md)
- [Architecture and security boundaries](docs/architecture.md)
- [Release notes](docs/release-notes.md)
- [Development and checks](CONTRIBUTING.md)

## License

Mindspace is licensed under the MIT License; see [LICENSE](LICENSE). Some
third-party assets (for example a tokenizer test fixture under Apache-2.0) are
distributed under separate licenses. See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [LICENSES/](LICENSES/) for
details. No model weights are included.
