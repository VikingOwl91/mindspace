# Source installation and operator guide

**New here?** Use the [install guide](install.md): `sh install.sh` does the verification and build
below for you. This page is the operator reference: manual signature verification (section 1), build
requirements (2 and 3), configuration and data locations (5), a complete demo and the MCP manual
example (6 to 9), backup, restore and upgrade STOP rules (10), and limits, troubleshooting and
uninstall (11). The command blocks on this page are POSIX `sh`, not Fish syntax: run them in `sh`
or bash, or save a block to a file and run it with `sh`.

This guide covers the **v0.1.0-alpha.1 Legacy/Git Public Alpha profile**: local
Linux x86_64 SQLite storage, lexical retrieval, explicit memories, legacy
Markdown/text and committed Git content. Canonical document/PDF ingestion and M8
publication are not supported. Embedding inference is outside the qualified profile.

Use the explicitly attached RC.5 archive from the
[official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1). GitHub-generated source archives are
older code, public main is the Alpha source plus later changes, and the release tag points to the older hosting commit `92f68ff646ec9b06ba21df2fb7879dfbc1281838`.
These instructions are adapted from the qualified RC.5 documentation. The archive
retains its original preparation-status wording and unchanged signed bytes. That wording
("unpublished", "candidate", "no public reporting destination") is historical; the current
statements are in this repository's documentation.

## 1. Obtain and verify the source archive

Obtain the complete matching delivery packet: the source `.tar.gz`,
`source-archive-manifest.json`, `SHA256SUMS`, detached `.asc` signatures for both
archive and manifest, `VERIFY.txt`, and the convenience public key. Download only the seven named assets from the official release page;
GitHub's generated "Source code" archives are not part of the packet.

1. Authenticate the publisher through an independently trusted contact/domain.
   The pinned primary fingerprint is
   `0E6699CB48F3941AE28CA88D509F78007F62D5EA`. A key or fingerprint obtained only
   beside the download is not an independent trust anchor.
2. Follow the complete commands in the packet's `VERIFY.txt` in its directory.
   They use a fresh keyring, an independently authenticated key, GnuPG
   `--assert-signer` with all 40 fingerprint characters, and checks for invalid,
   expired or revoked signatures. GnuPG must support `--assert-signer`; do not
   remove that check to accommodate an older installation.
3. Require valid signatures on **both** archive and manifest. Match `SHA256SUMS`
   against the pinned values in `VERIFY.txt`, then run
   `sha256sum --check --strict SHA256SUMS`. This last command checks bytes, not
   publisher identity; it does not replace signature verification.
4. Only after verification, use `archive.filename` and `baseline.commit` from the
   signed manifest. Extract into a new controlled directory with
   `tar -xzf VERIFIED_ARCHIVE -C NEW_EMPTY_DIRECTORY`, replacing the two path
   placeholders with the verified archive and a newly created empty directory.
   Set `SRC` to the absolute extracted `mindspace-0.1.0-alpha.1` directory. Retain
   the original archive and sidecars.

The exact recipient procedure, including fresh-directory extraction, is in
`VERIFY.txt`; it supplies actual filenames and hash pins rather than the path
placeholders above. Stop on any missing, uncertain or mismatched item before
building or opening data. The manifest inventories every payload file and hash;
RC.5 files are exact blobs from its signed source commit, with no overlays.
There is no Git history or model weight bundle. `cargo pkgid` below displays
package identity only; it does not authenticate the source. Cloning the public repository does not
obtain the qualified archive. The signed manifest links the archive to its private
source commit without publishing that Git history.

<!-- identity-install: source-local -->
```sh
: "${SRC:?Set SRC to the absolute authorized candidate source directory}"
cd "$SRC"
cargo pkgid --locked
```

Historical internal versions (`0.5.0`, the removed tags `v0.2.0`/`v0.3.0`) are not the public Alpha identity. Package version alignment does not downgrade database schema17 or change MCP protocol versions.

## 2. Install prerequisites

Use Rust/Cargo **1.98.1**, the toolchain this archive pins. Public `main` pins a different development toolchain that does not apply to the Alpha. Historical clean-userspace qualification used Debian 13/trixie x86_64 on a Linux 7.2 host; RC.4 also had eligible-host native builds. Neither result qualifies every Linux distribution, libc, filesystem or container.

| Profile | Build requirements | Runtime requirements |
| --- | --- | --- |
| Recommended no-embeddings Alpha | Rust/Cargo1.98.1, Git, C compiler and C development headers | GNU loader/libc, libm, libgcc_s; Git for repository operations. No embedding weights or C++ runtime required by the built application. |
| Default build, optional embeddings capability | The same prerequisites **plus a C++ compiler** (`g++` on Debian) | Also libstdc++; real semantic retrieval needs separately installed pinned model assets. It is outside the initial qualified model-free support profile. |

SQLite/FTS5 and Tree-sitter are compiled from source; a system SQLite CLI/server is not required by Mindspace. Historical installation qualification observed a default-build failure without `c++` and a successful independent C-only noembed build. These observations are not a new build of this archive.

On Debian, the package sets used inside the clean environment were `git gcc libc6-dev ca-certificates`, adding `g++` for the default profile. Install packages using your system's normal administration process; no host `sudo` command is prescribed here. Python3 is needed only for the optional manual MCP example and offline recovery recipe; strace/binutils are qualification tools, not basic Mindspace runtime dependencies.

The archive's `rust-toolchain.toml` pins 1.98.1. The clean test image already supplied that toolchain. Rustup/bootstrap downloads from an empty machine were not separately qualified. Source builds may download Cargo dependencies. Runtime search never downloads a model automatically.

For MCP repository tools, Linux6.2+ with effective Landlock ABI3+ is required. The actual sandbox canary must succeed; do not substitute `--insecure-no-sandbox` for that guarantee. Ordinary CLI ingestion does not have the MCP kernel-confinement promise.

## 3. Build an ordinary binary

Choose one profile. Separate output directories keep ordinary install artifacts apart from test/dev builds, whose self-dependency enables the test-only spawn-gate. Do not distribute a `cargo test`/`--all-targets` output.

Recommended Alpha profile:

<!-- identity-install: build-noembed -->
```sh
cd "$SRC"
cargo build --locked --release --no-default-features --bin mind --target-dir target/alpha-noembed
export BUILD_BIN="$SRC/target/alpha-noembed/release/mind"
test "$("$BUILD_BIN" --version)" = 'mind 0.1.0-alpha.1'
```

Alternative default build, requiring the additional C++ prerequisites:

<!-- identity-install: build-default -->
```sh
cd "$SRC"
cargo build --locked --release --bin mind --target-dir target/alpha-default
export BUILD_BIN="$SRC/target/alpha-default/release/mind"
test "$("$BUILD_BIN" --version)" = 'mind 0.1.0-alpha.1'
```

Running the alternative changes `BUILD_BIN`; choose the intended artifact before installation. Building embeddings support does not install model weights or qualify real inference. Keep its dependency/model requirements separate from the model-free Alpha.

## 4. Install into a new stable prefix

Keep the old executable and a validated pre-upgrade database backup before replacing any working installation. Ordinary commands can auto-migrate an existing DB when they open it. Do not overwrite your only recovery binary.

The block below installs a new version-specific prefix and stops if it already exists, including a dangling link. Run command blocks in a dedicated shell; `set -eu` makes an error a STOP rather than continuing with an unintended path.

<!-- identity-install: install -->
```sh
set -eu
: "${BUILD_BIN:?Choose the ordinary binary to install}"
export PREFIX="$HOME/.local/lib/mindspace/0.1.0-alpha.1"
if [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; then
    printf '%s\n' 'STOP: installation prefix already exists; preserve it and choose a separate reviewed destination.' >&2
    exit 1
fi
mkdir -p "$HOME/.local/lib/mindspace"
mkdir -m 700 "$PREFIX"
mkdir -m 700 "$PREFIX/bin"
install -m 0755 "$BUILD_BIN" "$PREFIX/bin/mind"
export MIND_BIN="$PREFIX/bin/mind"
export PATH="$PREFIX/bin:$PATH"
mind --version
```

The PATH export affects this shell. Add the same absolute prefix to your shell's startup configuration deliberately if desired; automated shell-configuration editing is not provided. Keep the installed path stable after MCP registration. The binary can run from another working directory.

## 5. Configuration and data locations

Database selection is `--db` first, then `MINDSPACE_DB`, then `$XDG_DATA_HOME/mindspace/mind.db`, falling back to `$HOME/.local/share/mindspace/mind.db`. Empty/relative HOME/XDG bases are ignored; unresolved defaults fail instead of creating data in the current directory.

No Mindspace configuration file is needed. Embedding selection/enablement is stored in the database. Models stay under the data directory's `models/` independently of `--db`. Grant state uses `$XDG_STATE_HOME/mindspace`, else `$HOME/.local/state/mindspace`. Keep these locations and backups when uninstalling an executable.

The examples below use a **fresh demo workspace**. In the CLI, `mind --db /absolute/path/to/mind.db search QUERY` explicitly selects a database; use that same path in MCP registration. They do not open an existing user database. For your actual default database, first determine whether it already exists; if it does, use the offline pre-upgrade procedure before any ordinary command opens it.

## 6. Initialize a private demo database

<!-- identity-install: initialize -->
```sh
export DEMO_ROOT="$(mktemp -d "$HOME/mindspace-alpha-demo.XXXXXXXX")"
export MINDSPACE_DB="$DEMO_ROOT/data/mind.db"
"$MIND_BIN" init
"$MIND_BIN" space create demo
"$MIND_BIN" doctor
```

New admitted private database directories use0700; new MAIN/WAL/SHM files use0600 from creation. Existing permissive files are preserved, not silently chmodded. Unsafe private parents/sidecars can be refused. This is a local permission-enforcing-filesystem contract, not protection against hostile same-UID/root processes or malicious filesystems. Doctor inspects the user DB without migrating it, but may create SQLite coordination files and runs a temporary sandbox self-test; it never downloads a model.

## 7. Store and retrieve a memory

The CLI can ingest existing legacy Markdown/text and retrieve/inspect its memories. Explicit manual creation/update/retirement is provided by MCP/library interfaces; there is no `mind memory create` CLI command.

<!-- identity-install: legacy-memory -->
```sh
mkdir -m 700 "$DEMO_ROOT/notes"
printf '# Alpha decision\n\nAlphaNotesToken records a local decision.\n' > "$DEMO_ROOT/notes/decision.md"
"$MIND_BIN" ingest "$DEMO_ROOT/notes/decision.md" --space demo
"$MIND_BIN" search AlphaNotesToken --space demo --json
```

This existing legacy workflow is not canonical M8 document ingestion or a publication bypass. Canonical document/PDF ingestion and M8 document publication are not part of this Alpha. Spaces organize retrieval; they are not authentication or ACLs. Retired/superseded history remains stored; source removal is not secure erasure.

For manual memory creation without an external coding-agent account, the following optional Python3 example drives the existing stdio MCP server. It initializes the protocol, discovers tools, creates one synthetic manual memory, searches it, and closes stdin. Stdout is JSON protocol; diagnostics are on stderr.

<!-- identity-install: mcp-manual -->
```sh
python3 - "$MIND_BIN" "$MINDSPACE_DB" <<'PYMCP'
import json, subprocess, sys
messages = [
    {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"alpha-guide","version":"1"}}},
    {"jsonrpc":"2.0","method":"notifications/initialized"},
    {"jsonrpc":"2.0","id":2,"method":"tools/list"},
    {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_create","arguments":{"subject":"Alpha manual decision","content":"AlphaManualToken is an explicit memory.","spaces":["demo"]}}},
    {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"AlphaManualToken","spaces":["demo"],"limit":5}}}
]
p = subprocess.Popen([sys.argv[1], "--db", sys.argv[2], "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
try:
    out, err = p.communicate("".join(json.dumps(m) + "\n" for m in messages), timeout=30)
except subprocess.TimeoutExpired:
    p.kill()
    p.communicate()
    raise SystemExit("STOP: owned MCP example timed out")
assert p.returncode == 0, err
replies = {m["id"]:m for m in map(json.loads, out.splitlines())}
assert set(replies) == {1,2,3,4}
assert all("error" not in r for r in replies.values())
assert replies[1]["result"]["serverInfo"]["version"] == "0.1.0-alpha.1"
assert not replies[3]["result"].get("isError")
assert not replies[4]["result"].get("isError")
r = replies[4]["result"]
body = r.get("structuredContent") or json.loads(r["content"][0]["text"])
assert body["hits"]
print("Manual memory created and retrieved; MCP shut down normally.")
PYMCP
"$MIND_BIN" search AlphaManualToken --space demo --json
```

## 8. Connect an MCP client

`mind setup` registers an absolute installed executable/DB path through the client's own native command. Default setup grants no repository root; ordinary repository tools remain advertised but refuse calls until allowed. The manual protocol example above is not a coding-agent UI or model-driven integration test.

Native Codex registration/readback/undo was qualified with fresh client configuration, including Codex 0.167.2 in the RC.4 guide checks. This does not certify a client UI or model-driven session. Install/locate your chosen client separately; no credentials or client-download result is supplied here. Preview first. Existing entries are refused rather than overwritten.

<!-- identity-install: mcp-codex -->
```sh
"$MIND_BIN" setup codex
"$MIND_BIN" setup codex --apply --yes
codex mcp get mindspace --json
```

For an actual client registration this modifies that client's configuration. Qualification uses disposable HOME/CODEX_HOME; do not point test commands at your real configuration. Claude Code setup exists and has historical client evidence; it is not a newly qualified interactive UI here. OpenCode is **preview only**, and `--apply` is refused. See [setup details and undo](setup.md) for exact scopes, backup behavior and limits.

To grant repository ingestion, use an exact existing `:rw` root; `:ro` permits outline but refuses ingestion. Keep the database directory outside allowed repository roots. The default kernel sandbox must report enforced; do not use an insecure flag to turn a failed security prerequisite into success.

## 9. Ingest a committed Git sample

This creates a new local repository, not a modification of your project. No pre-existing Git identity or signing setup is required.

<!-- identity-install: git-sample -->
```sh
mkdir -m 700 "$DEMO_ROOT/repo"
git -C "$DEMO_ROOT/repo" init -q -b main
printf '# Git decision\n\nAlphaGitToken is committed evidence.\n' > "$DEMO_ROOT/repo/decision.md"
printf 'pub fn alpha_demo_symbol() {}\n' > "$DEMO_ROOT/repo/lib.rs"
git -C "$DEMO_ROOT/repo" add .
git -C "$DEMO_ROOT/repo" -c user.name='Mindspace Demo' -c user.email='demo@example.invalid' -c commit.gpgsign=false commit -qm initial
"$MIND_BIN" ingest "$DEMO_ROOT/repo" --space demo
"$MIND_BIN" ingest "$DEMO_ROOT/repo" --space demo
"$MIND_BIN" search AlphaGitToken --space demo --json
"$MIND_BIN" symbols alpha_demo_symbol --space demo --json
```

Only committed content is read from Git; dirty/untracked content is not indexed. Re-ingestion reuses derived content but need not be a no-op in every session/revision journal. A changed commit produces a new representation while historical evidence remains inspectable.

<!-- identity-install: git-change -->
```sh
printf '# Git decision\n\nAlphaGitChangedToken replaces the current view.\n' > "$DEMO_ROOT/repo/decision.md"
git -C "$DEMO_ROOT/repo" add decision.md
git -C "$DEMO_ROOT/repo" -c user.name='Mindspace Demo' -c user.email='demo@example.invalid' -c commit.gpgsign=false commit -qm changed
"$MIND_BIN" ingest "$DEMO_ROOT/repo" --space demo
"$MIND_BIN" search AlphaGitChangedToken --space demo --json
"$MIND_BIN" search AlphaGitToken --space demo --include-retired --json
```

Git support is SHA-1 and Rust structural extraction. `--history` interprets selected ref tips, not every ancestor's file contents. Serialize operator-driven changing-HEAD ingests; no newest-HEAD-wins guarantee is made for overlapping extractions. Tree-sitter may retain a recovered partial derivation for valid syntax outside its grammar coverage; current CLI source has a known struct-pattern attribute warning. Check reported parse status instead of assuming complete structural extraction.

## 10. Offline backup, restore and pre-upgrade STOP rules

**Recovery engine boundary.** Python recovery uses the system SQLite independently of Mindspace's bundled engine. This remediated candidate links SQLite3.54.1, but the historical Debian Python recovery runtime used SQLite 3.46.1 (a separate earlier host qualification used 3.54.1); the Rust dependency update does not patch that runtime, and Cargo advisory checks do not cover it. Use this procedure only for your trusted private Mindspace database and retained own validated backups. **STOP before opening** any externally supplied, less-trusted or uncertain-origin database, WAL or bundle. Hashes establish consistency, not authenticity. Such inputs require separately patched and qualified tooling and are outside this recovery profile. Qualification does not establish malicious-file resistance or system-library security clearance.

Use the [offline recovery runbook and exact recipe](offline-recovery.md#operator-runbook). There is no native Mindspace backup/restore command. The supported contract is cooperative offline operation: stop and account for every CLI/MCP/library reader/writer and prevent restarts. An unobservable potentially relevant process is a STOP; a process-name search or missing receipt is not writer exclusion.

Before a new binary first opens existing data, preserve the exact old binary and a validated pre-upgrade backup. Ordinary reads can migrate. Never copy only live MAIN when acknowledged state may be in WAL; never delete WAL/SHM as cleanup. Restore to a fresh separate directory, validate integrity/FK/schema/logical contents and known retrieval/provenance, and keep the original backup immutable. Do not open the backup file with Mindspace.

The following extracts the already accepted recipe and backs up **only the fresh demo**; it does not add or alter a backup mechanism. `DEMO_RECOVERY_BIN` is the executable that created this demo. It is not a substitute for preserving the actual old binary before a real upgrade. For an existing installation, stop and use the accepted pre-upgrade runbook before any new binary opens that data. The demo's clients must have exited before setting the assertion. Set `QUIESCENCE_INVENTORY` to your actual stopped-client/PID/restart inventory before running the block; a statement is not proof of quiescence. If the recipe reports an unobservable process, follow the accepted runbook's STOP/classification procedure instead of automatically exempting it. The demo block deliberately starts with an empty `unobservable` list. `STOP: unclassified unobservable process` means descriptor access was denied before backup/restore began; it is not a corruption result. Stop and use the full runbook to assess the exact PID/starttime. Do not populate the list automatically or claim a new recovery PASS from the refused run.

<!-- identity-install: backup-extract -->
```sh
export RECOVERY_WORK="$(mktemp -d "$HOME/mindspace-recovery.XXXXXXXX")"
python3 - "$SRC/docs/offline-recovery.md" "$RECOVERY_WORK/offline-recipe.py" <<'PYRECIPE'
import os, sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
start = '<!-- BEGIN QUALIFIED OFFLINE RECIPE -->\n```python\n'
end = '```\n<!-- END QUALIFIED OFFLINE RECIPE -->'
assert text.count(start) == text.count(end) == 1
code = text.split(start, 1)[1].split(end, 1)[0]
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
with os.fdopen(fd, 'w') as f:
    f.write(code)
PYRECIPE
export DEMO_RECOVERY_BIN="$MIND_BIN"
```

<!-- identity-install: backup-restore -->
```sh
: "${QUIESCENCE_INVENTORY:?Record the actual stopped-client/PID/restart inventory first}"
export MINDSPACE_RECOVERY_QUIESCENCE="$(python3 - "$QUIESCENCE_INVENTORY" <<'PYQUIET'
import json, sys
print(json.dumps({"contract":"all-clients-stopped-and-restarts-prevented","operator_assertion":sys.argv[1],"unobservable":[]}))
PYQUIET
)"
python3 "$RECOVERY_WORK/offline-recipe.py" backup "$MINDSPACE_DB" "$RECOVERY_WORK/backup-1" "$DEMO_RECOVERY_BIN"
python3 "$RECOVERY_WORK/offline-recipe.py" restore "$RECOVERY_WORK/backup-1" "$RECOVERY_WORK/restored-1" "$DEMO_RECOVERY_BIN"
"$DEMO_RECOVERY_BIN" --db "$RECOVERY_WORK/restored-1/database.sqlite" search AlphaManualToken --space demo --json
```

A valid snapshot omits later writes. If clients accepted post-backup writes, **STOP automatic rollback and preserve both states** for a recovery decision. Errors/interruption/lost acknowledgment may leave a private partial or complete output: retain it, revalidate before use, and retry in another new directory. Do not overwrite existing backups or candidates. No hot-backup, arbitrary historical upgrade, schema downgrade, publication recovery or power-loss guarantee is made. The historical qualification covers the exact private schema16 development origin summarized in the [recovery runbook](offline-recovery.md#historical-qualification-and-limits); it does not qualify arbitrary old databases.

## 11. Limits, troubleshooting and uninstall

| Symptom / limit | Action |
| --- | --- |
| Source handoff is missing or verification fails | Stop. Obtain all seven matching RC.5 assets from the official release; do not substitute the generated source archive. |
| `c++` missing | Use the noembed profile or install the additional C++ prerequisites for the default profile. Do not change the lockfile to bypass the build failure. |
| Version is still0.5.0 | Stop; you are building an older tree or using a different executable. Check source provenance, chosen BUILD_BIN and first `mind` on PATH. |
| Database missing | Initialize a new selected path only. If an existing path/data was expected, stop and investigate before recreating anything. |
| Unsafe private path/sidecar or permission failure | Stop and preserve bytes/modes. Do not silently chmod existing user databases or delete sidecars. Use a supported controlled local path. |
| Concurrent init returns BUSY | Initialize before starting clients; stop/account for clients and retry the same path. Do not delete the database or weaken locking. |
| Strict semantic/hybrid unavailable | Expected in noembed/no-model operation; lexical and explained Auto fallback remain available. Model installation/inference is optional and outside this initial profile. |
| Kernel sandbox unavailable | Repository tools refuse; memory tools remain usable. Use supported Linux/kernel policy; do not add `--insecure-no-sandbox` to claim equivalent protection. |
| Large repository takes time | Scope ingestion. The preparation deadline is not an end-to-end write deadline; no hard aggregate-memory ceiling or full hostile-input certification is promised. |

New private storage does not encrypt data or implement secure erasure. History/source text can remain after source deletion. Linux/local permission paths were tested; NFS/FUSE/overlay, arbitrary containers/distros/kernels and other operating systems are not implied supported by one rootless Debian/btrfs qualification. Sandboxed MCP with roots denies new sockets and selected cross-process/io_uring operations; stdio/inherited descriptors/socketpair and other interfaces mean this is not complete network/IPC isolation. CLI and zero-root MCP do not receive that repository filter.

To undo a **disposable Codex registration**, use the native client. For other clients follow the linked setup undo instructions.

<!-- identity-install: mcp-codex-undo -->
```sh
codex mcp remove mindspace
```

Uninstall only the deliberately selected executable after stopping its clients. The block below targets the new prefix created in section4, refuses a symlink, checks its version and removes **only that executable**. Keep the source/build artifact and old recovery binary if reinstall/recovery is needed. PATH entries can then be removed from your shell configuration manually. DBs, models, grant state and backup directories are not removed.

<!-- identity-install: uninstall -->
```sh
test "$MIND_BIN" = "$PREFIX/bin/mind"
test -f "$MIND_BIN"
test ! -L "$MIND_BIN"
test "$("$MIND_BIN" --version)" = 'mind 0.1.0-alpha.1'
rm -- "$MIND_BIN"
```

The command blocks are retained from the qualified RC.5 guide. This documentation
update does not constitute a new runtime or dependency qualification. Native
client registration/readback does not certify a client UI or a model-driven session.

## 12. Security reporting

Use [GitHub private vulnerability reporting](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(GitHub sign-in required). Do not post vulnerability details in public issues,
pull requests or discussions. See [SECURITY.md](../SECURITY.md) for scope,
responsible disclosure and report contents.
