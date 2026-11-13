# Mindspace

Local memory and code search for AI coding agents, stored in SQLite and available
through a command-line tool or [MCP](https://modelcontextprotocol.io).

Mindspace keeps explicit decisions, notes and committed Git content across agent
sessions. Search results retain their source and history. Rust definitions can be
found by symbol and file, with commit provenance. Parsing and lexical search need no AI model.
Mindspace does not automatically record conversations; an agent must explicitly
write a memory or ingest a source.

**v0.1.0-alpha.1 is being prepared as a source-only Legacy/Git Public Alpha. It
has not been published.** The supported profile is Linux x86_64 without embedding
models. These checkout documents were revised after the frozen RC.4 archive;
a new reviewed archive must include them before public delivery. See the
[release notes](docs/release-notes.md).

## What this Alpha includes

| Workflow | Interface |
| --- | --- |
| Create, update or retire an explicit memory | MCP `memory_create`, `memory_update`, `memory_retire` |
| Ingest Markdown/text files and committed Git content | `mind ingest`; MCP `repo_ingest` for allowed repositories |
| Search memories and inspect their evidence/history | `mind search`, `mind recall`, `mind inspect`; MCP memory tools |
| Find Rust definitions with file/commit provenance | `mind symbols`; MCP `code_search` |
| Organize retrieval by named spaces | CLI `mind space`; MCP space tools |

Legacy Markdown/text ingestion is included. **Canonical document/PDF ingestion
and M8 document publication are not part of this Alpha.** Private `mind ownership`
staging commands are unsupported and hidden from public help. Embedding-based
search remains optional development capability outside this qualified model-free
release profile.

## Install

Obtain the matching source archive, manifest, checksums, detached signatures and
`VERIFY.txt` through the authorized delivery. No public download URL is available
yet. Authenticate the publisher independently, then follow `VERIFY.txt` to verify
both signatures and the exact checksums **before extracting or building**.
A bundled public key or a checksum alone does not authenticate the publisher.

Build requirements are the pinned **Rust/Cargo 1.98.1**, Git, a C compiler and C
development headers. SQLite/FTS5 and Tree-sitter are bundled. The recommended
profile needs neither model weights nor a C++ compiler; the alternative default
build also requires C++ and libstdc++.

From the verified, extracted source tree:

```sh
cargo build --locked --release --no-default-features --bin mind --target-dir target/alpha-noembed
./target/alpha-noembed/release/mind --version   # must report mind 0.1.0-alpha.1
```

Follow the [source-install guide](docs/source-install.md) to install that binary
at a stable path and configure your shell. It also covers prerequisites, archive
verification, MCP setup, recovery and uninstall. Do not install a test or
`--all-targets` artifact. No prebuilt package has been qualified.

## Quickstart (no AI model needed)

After the guide's installation/PATH step, run this in a dedicated shell. It
creates a fresh demo database and repository, without using your Git identity or
an existing user database. The source archive itself needs no `.git` history.

```sh
set -eu
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
mind recall AlphaGitToken --space demo
mind space list
mind doctor
```

Git ingestion reads committed objects, not dirty or untracked files. Repeating an
unchanged ingest reuses derived content; session/revision journals can still grow.
For an existing Markdown/text file, use `mind ingest /path/to/note.md --space demo`.
There is no CLI memory-create command: the guide includes a standalone Python/MCP
example for explicit memory creation, without a coding-agent account.

## Configure CLI and MCP access

Select the database with `--db`, then `MINDSPACE_DB`, or use the default:
`$XDG_DATA_HOME/mindspace/mind.db`, falling back to
`$HOME/.local/share/mindspace/mind.db`. No Mindspace configuration file is needed.
Use the same database path for the CLI and your MCP client. Before opening
existing data with a new binary, follow the offline pre-upgrade procedure.

`mind setup` previews client registration; `--apply` changes the selected client's
configuration after confirmation. Start with no repository access:

```sh
mind setup claude-code
# After reviewing the preview:
mind setup claude-code --apply
```

`mind setup codex` provides the corresponding Codex workflow. OpenCode setup is
preview-only. Registration is read back, and existing entries are refused rather
than overwritten. See [setup and undo](docs/setup.md).

For manual configuration, the stdio server command is an absolute installed
binary path followed by `--db /absolute/path/to/mind.db mcp`. Without an allowed
root, memory tools work and repository calls are refused. To permit repository
ingestion, append `--allow-root /absolute/path/to/repository:rw`. A `:ro` root
permits outlining but refuses ingestion. Keep the database directory outside all
allowed repository roots. Full client JSON examples are in the
[reference](docs/reference.md).

MCP repository tools require Linux Landlock ABI 3+ and a successful startup
canary. Check `mind doctor` and the tool result's sandbox status. If confinement
fails, repository tools stay off. CLI ingestion has no equivalent confinement
promise. Read access can disclose repository content to the connected agent;
Mindspace's permissions do not restrict the agent's other tools.

## Back up and restore

Use the [offline recovery runbook](docs/offline-recovery.md) and its exact Python
recipe. There is no native `mind backup` or `mind restore` command.

Stop and account for every CLI/MCP/library connection and prevent automatic
restarts. Preserve the old binary before an upgrade. Create a validated backup,
then restore to a fresh directory and check its contents before selecting it.
Never copy only live MAIN, delete WAL/SHM as a repair, or overwrite a backup.
A restore omits later writes; preserve both states if any were accepted.

`STOP: unclassified unobservable process` means the recipe could not establish
its required process inventory. It is not a database-corruption diagnosis.
Stop and assess the reported PID/starttime using the runbook. Do not blanket
allowlist unreadable processes. The recipe uses system Python/SQLite separately
from Mindspace's bundled SQLite and is limited to trusted own data, not untrusted
DB/WAL files or backup bundles.

## Known limits

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
- Large ingests can take minutes. MCP preparation deadlines do not interrupt a
  database commit. An uncertain commit result requires preserving state and
  investigating; process-kill tests do not certify power-loss recovery.
- Semantic/hybrid retrieval and real-model inference are outside this release
  profile. Package, schema and protocol versions are separate; Alpha command and
  schema compatibility can change.

## Security model

Repository content and agent requests can be untrusted. Confined MCP combines
Landlock, descriptor-based path checks and hardened Git execution, including
ignored user/system Git configuration and disabled hooks, fsmonitor and replace
refs. Supported confined profiles also restrict new sockets and selected
cross-process syscalls. Inherited descriptors and other interfaces remain;
this is not complete network/IPC isolation or protection from hostile same-UID
or root processes. CLI and zero-root MCP have different confinement scope.
`--insecure-no-sandbox` removes the kernel boundary and has a documented race
risk; it is outside the security guarantee.

For details, see [SECURITY.md](SECURITY.md), [sandbox capabilities](docs/sandbox-capabilities.md)
and ADR-029 in [the decisions](docs/decisions.md).

**Vulnerability reporting:** once enabled, use **Security → Report a
vulnerability** on the approved public delivery repository. Its exact URL must be
published and externally verified before release. No working public reporting
channel is asserted today. Until then, keep details private; do not post them in
public issues, pull requests or discussions. [The security policy](SECURITY.md)
explains the required report contents and current status.

## More documentation

- [Source installation and operation](docs/source-install.md)
- [Release notes and excluded features](docs/release-notes.md)
- [CLI/MCP reference](docs/reference.md)
- [Offline recovery](docs/offline-recovery.md)
- [Development and checks](CONTRIBUTING.md)

## License

MIT; see [LICENSE](LICENSE). The retained tokenizer fixture has separate
[third-party notices](THIRD_PARTY_NOTICES.md), [Apache 2.0 license text](LICENSES/Apache-2.0.txt)
and `tokenizer-provenance.json`. No model weights are included.
