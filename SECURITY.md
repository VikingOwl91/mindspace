# Security policy

Security reports are welcome. Please report privately, as described below.

## Supported versions

| Version / source | Reports |
| --- | --- |
| `v0.1.0-alpha.1` (the RC.5 source archive attached to the release) | Accepted, best effort. This is the current Alpha. |
| Public `main` | Accepted, best effort. It contains the Alpha source plus later changes, but is not the qualified archive. |
| Earlier development snapshots | Not maintained; no backports. |

There is no stable release, no LTS branch and no guaranteed response time.
Qualification applies to the exact archive bytes, not to every binary that
reports the same version. The supported profile is Linux x86_64, built from
source without embeddings.
Install the Alpha from the
[official release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1)
using the [install guide](docs/install.md). The named release asset
is the authoritative source. The release tag and GitHub's generated "Source code"
archives point to the older hosting commit and are not the Alpha.

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(Security > Advisories > Report a vulnerability). You need to be signed in to
GitHub; the report is private, not anonymous.

Do not open a public issue, pull request or discussion with vulnerability
details.

## What to include

- The archive SHA-256 or the commit you tested.
- Linux distribution and kernel version.
- How you started `mind mcp`: flags and allowed roots.
- Expected and actual behavior, and the impact.
- A minimal reproduction that uses your own temporary data.

Remove credentials and private source or data from examples.

## Disclosure

Please coordinate disclosure through the private report and allow time to assess
the issue and discuss a fix before publishing. This is a volunteer project:
handling is best effort, without a guaranteed schedule.

## In scope

The MCP repository tools (`repo_outline`, `repo_ingest`) are meant to be run by
an AI agent over untrusted repositories. Reports about them are the most
valuable:

- reading or writing outside the allowed roots (symlinks, `.git` pointers,
  alternates, worktrees, races between check and use, inherited file descriptors);
- writing into a root marked `:ro`;
- the sandbox reporting `enforced` when it is not;
- running code from a repository (hooks, configuration, Git helpers);
- a flaw in `install.sh` that lets a modified, unsigned or wrongly signed release be built or installed;
- data from outside the roots ending up in the database or in a tool result.

## Out of scope

- Running with `--insecure-no-sandbox`. It removes the kernel boundary, has a
  documented race risk and is for trusted input only.
- Anyone who can already start or talk to the local process. MCP is local stdio
  without authentication.
- General network or IPC isolation, and hostile same-user or root processes. On
  supported x86_64 Linux hosts the MCP sandbox restricts new sockets and selected
  cross-process system calls for servers started with allowed roots. Inherited
  descriptors and other channels stay open, `/proc` is not hidden, and the
  command line and MCP without roots are not confined this way.
- Platforms other than Linux 6.2+ with Landlock. The repository tools are off
  there.
- Denial of service by very large repositories. A first ingest can take minutes.

## Technical details

The sandbox and its limits are documented separately:

- [Sandbox capabilities](docs/sandbox-capabilities.md): what is enforced and
  which channels stay open.
- [Architecture: security boundaries](docs/architecture.md#security-boundaries)
- [Limits](docs/reference.md#limits-and-security)

These pages describe the qualified Alpha archive. Public `main` contains the same source plus
later changes, so details can differ.
