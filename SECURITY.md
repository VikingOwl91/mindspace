# Security policy

This source carries the **unreleased `0.1.0-alpha.1` candidate** identity (see the README). Security reports are welcome and taken seriously.

## Supported versions

There is no published or maintained `0.1.0-alpha.1` release yet. Development support follows the latest commit on `main`; no maintained release branch is implied by this candidate identity. Qualification reports apply to their exact source/artifact inputs, not every build carrying the same version string.

The intended initial profile is Linux, local private storage, ordinary CLI/stdio MCP and Legacy/Git ingestion, built without embeddings. See the [source-install and operator guide](docs/source-install.md) for source-access blockers, qualified prerequisites, recovery and support limits. Canonical document publication remains blocked under the accepted R01–R17 contracts; private staging is not public knowledge ingestion.

## Reporting a vulnerability

Please do **not** open a public issue, pull request or discussion for a vulnerability.

Report it privately through GitHub: **Security > Report a vulnerability** on the repository page
(GitHub private vulnerability reporting). This is the only reporting channel; it keeps the report
visible to the maintainer alone until a fix is ready.

Please include: the version or commit, your Linux kernel and distribution, how you started `mind mcp`
(flags, allowed roots), and a reproduction. Use temporary directories and your own repositories only.

This is a volunteer project: reports are answered on a best-effort basis, with no guaranteed response time.

## What is in scope

The MCP repository tools (`repo_outline`, `repo_ingest`) are designed to be run by an AI agent over
untrusted repositories. Reports about these are the most valuable:

- reading or writing outside the allowed roots (symlinks, `.git` pointers, alternates, worktrees,
  races between check and use, inherited file descriptors);
- writing into a root marked `:ro`;
- the Landlock sandbox reporting `enforced` when it is not;
- running code from a repository (hooks, configuration, Git helpers);
- data from outside the roots ending up in the database or in a tool result.

The threat model, the tests behind it and the known residual risks are in ADR-029 in
`docs/decisions.md`. The README lists the known limits.

## Out of scope / known limitations

- Running with `--insecure-no-sandbox` (documented to have a remaining gap; use for trusted input only).
- Anyone who can already start or talk to the local process: MCP is local stdio without authentication.
- General network/IPC isolation and hostile same-UID/root processes. Sandboxed MCP with allowed roots applies scoped new-socket and selected cross-process syscall restrictions on supported x86_64 Linux; this is not a blanket network guarantee. Inherited stdio, socketpair and permitted signal paths remain outside that claim. CLI and zero-root MCP do not receive this socket filter. See [sandbox capabilities](docs/sandbox-capabilities.md); `/proc` is not generally hidden.
- Platforms other than Linux 6.2+ with Landlock: the repository tools are off there.
- Denial of service by very large repositories (a first ingest can take minutes).
