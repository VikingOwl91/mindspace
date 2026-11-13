# `mind setup`: connecting a coding agent

`mind setup` registers Mindspace as an MCP server with **Claude Code**, **Codex CLI** or **OpenCode**.

For Alpha installation, first follow the [source-install guide](source-install.md)
and verify the seven RC.5 release assets. Public main contains older code; these
setup instructions describe the qualified Alpha archive. The release is being
prepared as a draft. Keep the installed executable at its stable absolute path.
Native client registration/readback was qualified with Codex CLI 0.166.0; this does
not certify client interactive screens or a model-driven session. OpenCode remains
preview-only. Setup sends nothing to a network service.

## The short path: zero-permission setup (the default)

```sh
mind init                                  # once: creates the local database
mind setup claude-code --apply             # registers Mindspace; no folder is shared
mind doctor                                # checks the installation, including the sandbox
```

**Setup without any folder is complete.** The agent gets all the memory tools (store and search notes, decisions and
facts, list spaces) and Mindspace can read **no** repository files. Sharing repository folders is an optional extra; you
do not need it to use Mindspace with an agent. To share folders later, undo the registration (see "Undo") and run setup
again with `--ro`.

`mind setup` in a terminal guides you and says this first; sharing folders is one optional question there, and its default
answer is "no".

Optionally, with folders (they are only **examples**: not required and never created):

```sh
mind setup claude-code                            # preview: shows exactly what would happen, changes nothing
mind setup claude-code --ro ~/projects --apply    # also let Mindspace read ~/projects, after you confirm
mind setup codex --apply --yes                    # no terminal (script): --yes replaces the question
```

`mind setup` on its own, in a terminal, guides you; without a terminal it lists the clients it finds on `PATH` and
changes nothing. `mind setup --help` has the same explanations as this page.

## What you share: `--ro` and `--rw`

These are **Mindspace** permissions for the folders you name.

- **`--ro` (read-only, recommended):** Mindspace may read repository files inside the folder, and it cannot change them.
- **`--rw` (read-write):** Mindspace may read **and change** repository files inside the folder, within the functions it
  supports (for example it writes its repository identity marker into a repository's `.git` directory when ingesting).
  Setup asks you to confirm this explicitly (in a terminal you type the word `yes`).

Rules:

- **Nothing is shared by default.** Without `--ro`/`--rw` the agent only gets the memory tools; the repository tools stay off.
- Only folders you name are covered. The permissions do not extend to the rest of the file system.
- A mode is never changed for you: a folder you give as `--ro` is registered as read-only, and naming the same folder
  as both `--ro` and `--rw` is refused instead of guessed.
- Setup shows the effective **absolute** path of every folder (links are resolved) before anything happens.

Refused, with the reason (in the preview as well): a relative path, a folder that does not exist (setup never creates
one), a file, `/`, the system folders themselves (`/bin /boot /dev /etc /home /lib* /proc /root /run /sbin /sys /tmp /usr
/var`), your home directory (or anything that contains it), anything that overlaps the database directory, paths that are
not valid UTF-8 (they could not be registered faithfully), and paths containing `:` or control characters, also after
links are resolved (a `:` would break the `DIR:mode` syntax; control characters could write escape sequences to your
terminal). A folder that other users can write to gets a warning.

These rules apply to what a person types here. Folders **inside** a system folder (`/usr/src/app`, `/var/www/site`) and
common project locations such as `/workspace`, `/srv`, `/opt`, `/mnt` or `/data` are allowed. `mind mcp --allow-root`
itself, which you can still write by hand, refuses only `/` and an overlap with the database directory.

Good to know:

- Read access is **not a confidentiality guarantee** towards the connected agent: whatever Mindspace can read in a shared
  folder, the agent can ask Mindspace about.
- `--ro`/`--rw` describe what Mindspace may do. They do **not** limit what your agent or its other tools (a shell, an
  editor plugin, other MCP servers) may do.
- The repository tools run inside a kernel sandbox (Linux Landlock). **Where no effective sandbox exists they stay
  switched off** (fail-closed); the memory tools keep working. Setup reports this before you confirm, and the
  registration never contains `--insecure-no-sandbox`.

## The three clients

| Client | `--apply` | How | Checked afterwards |
| --- | --- | --- | --- |
| Claude Code | yes | `claude mcp add -s user\|local mindspace -- …` | `claude mcp get`: scope, command and arguments must match; Claude's own start check must say *Connected* |
| Codex CLI | yes | `codex mcp add mindspace -- …` (user level) | `codex mcp get --json`: command and arguments must match exactly (Codex does not start the server to check it) |
| OpenCode | **no, preview only** | you run `opencode mcp add --global mindspace -- …` | not checked by Mindspace |

Setup never edits a client's configuration file itself; it only runs the client's own `mcp add`. It looks at `PATH`
for the client and does **not** run the client for a preview (even `opencode --version` writes a log file, and the first
`claude mcp get` creates Claude's configuration in an empty home).

Versions the behaviour was observed with: Claude Code 2.1.319, Codex CLI 0.167.0, OpenCode 2.2.5.

### Claude Code

- Scope `user` (default): all your Claude Code sessions. Scope `local` (`--scope local`): only sessions started in the
  directory you ran setup from. The scope `project` is **not offered**: it writes a `.mcp.json` that is meant to be shared
  with a team, and setup would put your absolute paths in it.
- An existing `mindspace` entry in any scope stops setup: it is not overwritten.

### Codex CLI

- `codex mcp add` **overwrites** an existing entry of the same name without asking. Setup therefore asks `codex mcp get`
  first and stops if an entry exists.
- Codex rewrites other servers' entries when it adds one: the content stays, but comments inside those entries can be
  lost. Setup copies `config.toml` first to `config.toml.mindspace-backup-<unix time>` and tells you where. The config is
  `$CODEX_HOME/config.toml`, else `~/.codex/config.toml`. The copy is made defensively:
  - it is read from one open file (a symbolic link to a config kept by a dotfile tool is followed, the backup lands next
    to the link) and read again if the file changes while it is read;
  - it is created exclusively and never follows a link: a file, an older backup or a planted symbolic link at the name
    is left alone and the next name (`…-<time>-1`, `-2`, …) is used;
  - it is created `0600`, gets the config's permissions after the content is written, and is flushed to disk.
  If the copy cannot be made, nothing is registered. Backup files are never deleted by setup.

### OpenCode: preview only, and why

`opencode mcp add` overwrites an existing entry silently, and there is no reliable way to verify the result: `opencode mcp
list` goes through a background service (every OpenCode read command starts one, and it keeps running), and in testing it
did not list a server that was in the configuration (three cold starts in a row said "No MCP servers configured"; one earlier
run said *connected*). Because setup must not report success before the registration is checked, it shows the exact command
for OpenCode and leaves running it to you. Use `--global` so no `opencode.json` is dropped into a project.

## Confirmation and scripts

`--apply` shows the preview and asks `Register Mindspace with <client> now? [y/N]`. With `--rw` it first asks you to
type `yes` for write access. **Without a terminal nothing is changed unless you add `--yes`**; with `--rw` folders,
`--yes` means you accept write access. `--apply` for OpenCode is refused.

Setup reports success only after reading the registration back through the client. If the client reports something
different, cannot start the server, or cannot be read, setup exits with an error and says so; it does not say
"done".

## Undo and recovery

- Claude Code: `claude mcp remove mindspace -s user` (or `-s local`).
- Codex: `codex mcp remove mindspace`. To restore the whole file: copy the `config.toml.mindspace-backup-…` file back.
- OpenCode: remove the `mindspace` entry from the OpenCode config, or see `opencode mcp --help`.
- To replace an existing registration (for example after moving the `mind` binary), remove it as above and run setup again.

If a setup fails half-way, the error message says whether an entry exists now and gives the undo command. `mind doctor`
shows whether Mindspace itself works.

## Limits

- The registration points at the **absolute path of the `mind` binary that ran setup** and at the database it used. Moving
  or deleting the binary breaks it; run setup again from the installed binary.
- "Does the entry already exist?" and "add it" are two steps, not one atomic operation. Claude Code's own `add` refuses an
  existing name, so a race ends in its error. Codex's `add` would overwrite silently, so setup asks once more right
  before it (after the backup); a window of a few milliseconds remains, and the backup holds the state from just before it.
- After setup, a folder you shared is resolved by the server at every start like any other `--allow-root` path; replacing
  the folder with a link later changes where it points, and the kernel sandbox, not setup, is what confines the server.
- Only Linux is supported. Windows and macOS are not tested.
- Not covered by tests: the interactive screens of the clients themselves, and OpenCode registration (preview only).
- Setup writes no Mindspace configuration file and makes no network connection.
