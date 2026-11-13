# Alpha CLI and MCP reference

These examples are selected/adapted from the qualified RC.5 README, reference
and install guide. First [verify and install the RC.5 archive](source-install.md).
The hosting tag and generated source downloads contain older code; public main contains the
Alpha source plus later changes.

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
`--history` selects ref tips, not every ancestor's file content. Serialize ingests
while changing HEAD. `mind inspect <memory-id> --json` inspects a returned memory.
There is no CLI memory-create command; create/update/retire use MCP. The install
guide includes a standalone Python/stdio MCP example. Use the verified binary's
`--help` and subcommand help for full syntax. These examples are retained docs,
not a new runtime qualification.

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
and `repo_ingest`; `code_search` finds Rust definitions. Spaces organize retrieval
and are not access controls. MCP is local stdio without authentication.

## Limits and security

The supported profile is Linux x86_64 without embedding models. MCP repository
access requires effective Landlock ABI 3+ and a successful startup canary. CLI
ingestion has no equivalent kernel-confinement promise. Do not treat
`--insecure-no-sandbox` as an equivalent workaround. Read the
[architecture](architecture.md#security-boundaries),
[sandbox capabilities](sandbox-capabilities.md) and
[security policy](../SECURITY.md).

Known limits of this Alpha:

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
