# Alpha architecture

This overview describes the 0.1.0-alpha.2 archive (carried over from the qualified RC.5 documents).
Public main is the development line: the release source plus later changes.

## Local engine

```text
CLI commands or stdio MCP client
              |
         Mindspace engine
       core / ingest / retrieval
              |
          Store boundary
              |
         SQLite + FTS5
```

Mindspace is a Rust library and the `mind` binary. `mind mcp` serves a local stdio
connection. There is no required daemon, hosted account or HTTP transport. SQLite
stores local data and the lexical index. The qualified build uses no embedding
model or external AI API. Source builds may download Cargo dependencies.

## Data and provenance

Explicit memories record user/agent-authored assertions with history. Legacy
Markdown/text and committed Git content are ingestion inputs. Git ingestion reads
committed objects, not dirty or untracked files. Rust definitions retain file and
commit provenance. Re-ingesting unchanged input reuses derived content; journals
can still grow. Lexical retrieval uses SQLite FTS5. Spaces organize retrieval;
they are not authorization boundaries. Provenance identifies an origin, not
independent proof that the content is true. Conversation recording is not automatic.

Canonical document/PDF ingestion and M8 document publication are excluded. Private
ownership/staging administration is unsupported. Package, schema and MCP protocol
versions are separate. A package version does not qualify arbitrary builds/data.

## Storage and configuration

Database precedence is `--db`, then `MINDSPACE_DB`, then
`$XDG_DATA_HOME/mindspace/mind.db` (fallback
`$HOME/.local/share/mindspace/mind.db`). Use the same database for CLI and MCP.
No Mindspace configuration file is required. New admitted private DB/sidecar files
use `0600` and new private directories use `0700`; existing permissive files are
not silently changed. Storage is not encrypted, deletion is not secure erasure,
and no aggregate storage quota is promised. Ordinary opens can migrate data.
Preserve the old binary and follow [offline recovery](offline-recovery.md) before
an upgrade.

## Security boundaries

MCP is unauthenticated local stdio; anyone who can communicate with the process
can call its tools, including writes. Without allowed roots, memory tools work
and repository calls are refused. Read-only roots permit outlining; read-write
roots permit ingestion. Keep the DB directory outside allowed roots.

On supported Linux hosts, MCP repository tools combine Landlock ABI 3+,
descriptor-based checks and hardened Git execution. Runtime canaries must confirm
confinement; repository tools refuse if it is unavailable. Confined profiles also
restrict new sockets and selected cross-process system calls. Inherited descriptors,
other interfaces and hostile same-UID/root processes remain outside that guarantee.
This is not complete network or IPC isolation. CLI ingestion and zero-root MCP
have different confinement scope. `--insecure-no-sandbox` removes the kernel
boundary and has a documented race risk; it is outside the security guarantee.

See the [security policy](../SECURITY.md), [sandbox capabilities](sandbox-capabilities.md),
[CLI/MCP reference](reference.md) and [setup guide](setup.md). The supported
profile is Linux x86_64 without embeddings. Other operating systems, arbitrary
filesystems/containers and real-model inference are not qualified by this baseline.
