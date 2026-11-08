# `mind doctor`

A read-only, local check of an installation. It needs no network, no model and no external API.

```sh
mind doctor          # human-readable
mind doctor --json   # one JSON document on stdout, nothing else
```

## What it checks

| id | Looks at | Not a failure when |
| --- | --- | --- |
| `binary` | version, path of the running executable, whether another `mind` comes first on `PATH` | (a different `mind` first on `PATH` is a `warn`) |
| `database` | path and its origin, existence, schema version against what this binary supports | |
| `config` | effective database path, data directory, model directory, `MINDSPACE_MCP_ALLOW_ROOTS`, `MINDSPACE_MCP_INGEST_TIMEOUT`, each with its origin | an unset optional value |
| `git` | `git --version` | git is missing (`warn`: repositories cannot be ingested, documents still work) |
| `embeddings` | enabled, selected model, whether its files are present (hashes are not re-checked; use `mind embeddings status`) | disabled (`skip`), enabled without an installed model (`warn`) |
| `sandbox` | a self-test of the MCP repository tools, see below | the sandbox is unavailable (`warn`, the tools are off and fail closed) |

## Statuses and exit codes

`pass`, `warn`, `fail`, `skip`. Warnings never change the exit code.

| exit code | meaning |
| --- | --- |
| 0 | no blocking failure (warnings allowed) |
| 1 | at least one check failed (for example: database not initialized, newer schema than this binary, invalid configuration, inconsistent sandbox evidence) |
| 2 | the diagnosis itself could not be carried out (invalid command line, the self-test could not start or timed out, the report could not be written). The check that could not run is `fail` and carries `"diagnostic_error": true` in the JSON (the key is absent otherwise) |

## JSON structure (`"format": "mind-doctor/1"`)

```json
{
  "format": "mind-doctor/1",
  "mind_version": "0.5.0",
  "summary": {"pass": 4, "warn": 1, "fail": 0, "skip": 1, "exit_code": 0},
  "checks": [
    {
      "id": "database",
      "title": "Database",
      "status": "pass",
      "summary": "schema version 9 (current)",
      "details": {"path": "...", "source": "environment variable XDG_DATA_HOME", "default": true,
                  "schema_version": 9, "supported_schema_version": 9, "size_bytes": 655360,
                  "write_ahead_log_present": false},
      "remedy": null
    }
  ]
}
```

`details` differs per check and may gain keys; `status`, `id`, `summary` and `remedy` are the stable part.
Like all JSON output of `mind` this is pre-alpha and may change; the `format` tag changes when it does.

## Where paths come from

The database path is, in this order: `--db`, the environment variable `MINDSPACE_DB`, then `mind.db` in the data
directory. The data directory is `$XDG_DATA_HOME/mindspace`, else `$HOME/.local/share/mindspace`. An empty or
relative `XDG_DATA_HOME` or `HOME` is not used (and the doctor says so). If nothing usable remains, `mind` stops with
an error instead of using the current working directory. The model directory is `<data directory>/models`; it does
not follow `--db`.

## The database is never changed

Opening a database normally runs migrations, so the doctor reads it differently: when no `-wal` file exists it opens the
file `immutable` (SQLite then creates no auxiliary files); when a `-wal` file exists (a server may be writing) it opens
`mode=ro`, which creates nothing new. If the file cannot be read while a `-wal` file exists (a server may be writing, or a crash left a log), that is a `warn`, not a `fail`: it is no evidence of damage. An older schema is reported as `warn`; it is migrated by the next ordinary command,
not by the doctor. The result is a snapshot: a writer that starts between the check and the open is not noticed.

## The sandbox self-test and what it proves

On Linux the doctor starts the real `mind mcp` as a child process, **without** `--insecure-no-sandbox`, with a throw-away
database and one throw-away read-write root under the system temp directory (removed on every exit path, also after a
timeout; the child is killed). It then asks, over MCP:

1. `repo_outline` inside the root: must work, and its `sandbox` field says what the server established at start;
2. `repo_outline` outside the root and through `..`: must be refused.

The report keeps two kinds of evidence apart:

- `kernel_evidence`: the **server's own report** (`enforced`, or `unavailable` with the reason). The server restricts
  itself with Landlock and verifies that with probes that must be denied (ADR-029) before it says `enforced`. The doctor
  cannot observe the kernel state of another process from outside, so this is the only kernel evidence there is.
- `application_validation`: whether inside access works and outside access is refused. This is the **application
  layer** (the allowlist) and does not by itself prove the kernel sandbox.

A path counts as refused only if the answer is the allowlist's own message ("outside the allowed roots"); any other error does not count. `pass` needs `enforced` and all application checks as expected. `unavailable` is a `warn` (the repository tools are off,
memory tools and search are unaffected; this fail-closed state is safe). `enforced` with an outside path that was not
refused, a `DISABLED` report, or no usable report is a `fail`. The doctor never suggests `--insecure-no-sandbox`.
The result is a snapshot of this machine now; the authoritative check is the one `mind mcp` does at every start.
On other platforms the check is `skip`: the repository tools are always off there.

## Sandbox capabilities (additive)

`checks[id=sandbox].details.capabilities` (Linux) separates what the kernel supports from what Mindspace declares and
what the self-test proved: per channel (`filesystem`, `tcp`, `udp`, `unix_path_sockets`, `unix_abstract_sockets`,
`signals`) a `status` of `enforced`, `supported_not_enforced`, `not_supported` or `unknown`, the mechanisms
(`available_via`) and the basis. The probes are read-only; the doctor process is not confined. The socket channels are `enforced` only with the typed `seccomp` evidence of the self-test child ([sandbox-seccomp.md](sandbox-seccomp.md)). The status of the check
and the exit codes do not depend on it. Details and limits: [sandbox-capabilities.md](sandbox-capabilities.md).

## Limits

- The self-test kills the server it started and waits for it, but it does not track everything that server started
  itself. A grandchild (for example a `git` from the server's own start-up probe) is not supervised; it ends by its
  own deadlines (the server bounds its git calls). The doctor does not wait for such a process, so it cannot delay the
  deadline, but it may briefly outlive the doctor.
- It does not verify model file hashes (that takes seconds); `mind embeddings status` does.
- It does not look at MCP client configuration files and cannot tell whether a client is set up.
- Nothing is checked about a running server; start-up messages of `mind mcp` go to its stderr.
