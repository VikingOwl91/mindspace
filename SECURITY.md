# Security policy

Security reports are welcome. Use the private channel below for both the Alpha
archive and the older repository implementation.

## Supported versions

| Version / source | Security-report scope |
| --- | --- |
| `0.1.0-alpha.1` / RC.5 source archive | Current Alpha candidate; best-effort reports accepted. Release is being prepared as a draft, not yet published. |
| Public `main` | Older pre-alpha implementation; reports accepted, but it is not the RC.5 Alpha source. |
| Historical `v0.2.0`, `v0.3.0` and other development snapshots | No maintained release line or backport commitment. |

There is no stable release, LTS branch or guaranteed response time. Qualification
applies to exact source/archive bytes, not every binary with the same version.
Install from the [official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1) when published, using the
[source-install guide](docs/source-install.md). The named RC.5 asset is authoritative;
the hosting tag and GitHub-generated source archives contain older code. RC.5's
broader Git history is not public.

The qualified profile is Linux x86_64, local private storage, CLI/stdio MCP and
Legacy/Git ingestion built without embeddings. Canonical document/PDF ingestion
and unfinished M8 publication are not supported Alpha features.

## Reporting a vulnerability

Use [GitHub Private Vulnerability Reporting](https://github.com/VikingOwl91/mindspace/security/advisories/new) from
Security > Advisories > Report a vulnerability. GitHub sign-in is required;
this is a private channel, not an anonymous submission form. Do not open a public
issue, pull request or discussion containing vulnerability details.

Include the archive hash or commit, Linux kernel/distribution, how you started
`mind mcp` (flags and allowed roots), expected versus actual behavior, impact and
a minimal reproduction using your own temporary data. Remove credentials and
private source/data from examples. Coordinate disclosure through the private
report; please allow time to assess it and discuss a fix before public disclosure.
This is a volunteer project with best-effort handling and no guaranteed schedule.

## What is in scope

The MCP repository tools (`repo_outline`, `repo_ingest`) are designed to be run by an AI agent over
untrusted repositories. Reports about these are the most valuable:

- reading or writing outside the allowed roots (symlinks, `.git` pointers, alternates, worktrees,
  races between check and use, inherited file descriptors);
- writing into a root marked `:ro`;
- the Landlock sandbox reporting `enforced` when it is not;
- running code from a repository (hooks, configuration, Git helpers);
- data from outside the roots ending up in the database or in a tool result.

See the [Alpha architecture](docs/architecture.md), [sandbox capabilities](docs/sandbox-capabilities.md)
and [README limits](README.md#known-limits). These describe the qualified archive,
not an assertion that public main implements every Alpha protection.

## Out of scope / known limitations

- Running with `--insecure-no-sandbox` (documented to have a remaining gap; use for trusted input only).
- Anyone who can already start or talk to the local process: MCP is local stdio without authentication.
- General network/IPC isolation and hostile same-UID/root processes. Sandboxed MCP with allowed roots applies scoped new-socket and selected cross-process syscall restrictions on supported x86_64 Linux; this is not a blanket network guarantee. Inherited stdio, socketpair and permitted signal paths remain outside that claim. CLI and zero-root MCP do not receive this socket filter. See [sandbox capabilities](docs/sandbox-capabilities.md); `/proc` is not generally hidden.
- Platforms other than Linux 6.2+ with Landlock: the repository tools are off there.
- Denial of service by very large repositories (a first ingest can take minutes).
