# Install Mindspace

The installer selects the newest published GitHub release, including prereleases and excluding
drafts. It authenticates one explicitly supported source-release contract, builds it with its exact
Rust toolchain and installs a versioned binary under your home directory. Unsupported or unverifiable
selected releases stop the operation; an older release is never tried silently. For manual RC.5
verification and the operator reference, see the [source-install guide](source-install.md).

## Before you start

- Linux on x86_64; other platforms are unsupported by the current contracts.
- Python 3.11+, curl, GnuPG with `--assert-signer`, tar, gzip, sha256sum, timeout, flock (util-linux),
  and a C compiler with C headers. Git is needed for repository ingestion.
- The exact Rust toolchain named by the supported contract. RC.5 requires Rust 1.98.1:
  `rustup toolchain install 1.98.1 --profile minimal`. Future contracts currently support 1.98.1 or
  1.100.0. The installer prints the required version and never installs a toolchain itself.
- About 2 GiB free in `/tmp` or `TMPDIR`, and access to GitHub, the publisher's key domain and crates.io.

## Install

Confirm the fingerprint independently as explained below **before** using this noninteractive command:

```sh
curl -fsSL https://raw.githubusercontent.com/VikingOwl91/mindspace/main/install.sh | sh -s -- --trust-fingerprint 0E6699CB48F3941AE28CA88D509F78007F62D5EA
```

This works from bash, zsh and Fish. Passing a fingerprint is a trust decision, not evidence that the
identity was authenticated. The command also relies on trusted distribution of the installer through
GitHub and TLS; verify a signed installer checkout before execution if you need to authenticate the
script itself, using the [procedure below](#optional-check-the-installer-itself).

To inspect the script and use its interactive fingerprint prompt instead:

```sh
git clone https://github.com/VikingOwl91/mindspace.git
cd mindspace
sh install.sh
```

To select a version explicitly, append `--version 0.1.0-alpha.1` (one optional `v` prefix is accepted).
This chooses the authenticated version binding. For a fresh client it does not establish historical
byte immutability if a publisher re-signs that version; retain an independently authenticated digest
for stronger reproducibility. Explicit RC.5 with local `--assets-dir` retains offline verification.
The installer never builds GitHub-generated source ZIPs or the code in the checkout itself.

## The one decision you make: trusting the publisher

Checksums and signatures only prove that files match what the publisher signed. Whether that
publisher is the right one is your decision. The installer shows the publisher key's fingerprint:

```
0E66 99CB 48F3 941A E28C  A88D 509F 7800 7F62 D5EA
```

It will not continue until you type that fingerprint (or pass it with `--trust-fingerprint`). Do this
only after confirming the fingerprint through a channel you trust that is **not this download**, for
example from the maintainer directly or from a record you already hold. The release page, the README
and the installer all come from the same GitHub project, so they cannot vouch for each other.

The GitHub API is used for discovery, not artifact authentication. Releases are ordered by
`published_at`, then numeric release ID for a tie, across complete bounded pagination. Selection
happens before compatibility checks. API errors, malformed/conflicting metadata, missing assets or
release deletion fail without falling back. Unsigned discovery cannot prove global freshness or
detect every omitted/replayed release, and pagination is not an atomic snapshot.

For RC.5 (`v0.1.0-alpha.1`), the installer retains the original seven exact asset names, sizes and
SHA-256 pins, detached archive and source-manifest signatures, publisher fingerprint and source
commit. The bundled key is checked for consistency and never establishes identity.

Future releases require a signed `mindspace-release-v1` envelope with the explicitly supported
platform, recipe, exact toolchain and four asset roles. The installer authenticates that envelope
before using its fields, verifies all declared sizes/hashes, verifies detached archive and source
manifest signatures, and cross-checks the manifest's source/archive identities and bounded USTAR
members before extraction. It rejects unknown contracts, key/signature expiry/revocation and a
changed publisher fingerprint. Key rotation requires an independently authenticated installer update.
The [contract and trust decisions](installer-release-selection-proposal.md) describe the limits and
future publisher workflow. Published RC.5 assets are unchanged.

## Running it again

You can run the installer as often as you like.

| What it finds | What it does |
| --- | --- |
| Nothing | Installs. |
| An intact install of the selected version | Says `Already up to date`, changes no installed files, exits 0. Rust is unnecessary. Default selection still needs discovery access; future contracts are authenticated before no-op validation. |
| RC.5 without an install record | Checks layout and reported version; returns a no-op with an explicit authenticity caveat. Future-version prefixes without receipts fail safely and require explicit `--reinstall`. |
| A directory that is not a valid install (missing or non-executable binary, a binary that reports another version or no longer matches its record, a symbolic link, a file) | Stops with exit 1 and tells you what is wrong. A directory that merely exists is never taken as proof of an install. |
| A validated older version, with this version absent | Stops with exit 1 and explains `--upgrade`. With that option, installs beside the old version, preserving it. |
| A newer version installed beside this one | Stops with exit 1 unless the intact install of this version is already there. `--allow-older` installs this version beside the newer one. |

`--reinstall` replaces an existing install of this version, only on request. The replacement is built,
copied into a private staging directory on the installation filesystem, and checked first. Only then
is the old directory moved to `~/.local/lib/mindspace/previous/` (kept, not deleted) and the new one put in its place. If anything fails after the move, the old install is moved
back if its original path is still free; otherwise its backup path is printed for recovery.
Concurrent installer publication is serialized using a lock on the installation directory. Protected
prefix content is rechecked after staging. The lock does not coordinate arbitrary same-user programs.
If interrupted after replacement, the complete new prefix and previous backup are retained; retrying
validates the new prefix. There is no separate automatic activation step. Catchable signals
trigger cleanup; SIGKILL or power loss can leave a staging directory or a backup under `previous/`,
which should be inspected before retrying. Databases, agent configuration and MCP registrations are
never touched, so a registration made with `mind setup` keeps pointing at the same path. Unexpected files inside a prefix, including
databases or configuration, block replacement even with `--reinstall`: inspect and back up those files
first. Linked parent directories are refused. Symlinked files and non-regular receipts are invalid.

Versions are installed side by side, each in `~/.local/lib/mindspace/<version>/`. The installer
never changes your `PATH` or switches between them, and it never installs an older version over a newer
one on its own: which `mind` runs is decided by your `PATH`. `--upgrade` stages the requested version
side by side; it does not activate it. Existing MCP registrations continue to use their original
versioned binary. Switching PATH or a registration is a separate explicit user action. Prerelease
ordering follows semantic versioning, so `0.1.0-alpha.2` is newer than `0.1.0-alpha.1`, `0.1.0-alpha.10` is newer than
`0.1.0-alpha.2`, and `0.1.0` is newer than any `0.1.0-` prerelease.

The `install-record` file in the install directory lists the version, the SHA-256 of the installed
binary, the archive SHA-256, the source commit and the publisher fingerprint. It catches accidental
damage or a half-finished copy. It does not protect against someone who can already write to your home
directory. A receipt is parsed as data, never executed. Recorded binaries are hashed before their
version command is run; legacy binaries have no receipt to check first. Other version prefixes are
validated too, and incomplete or invalid ones stop the operation with their path and a reason.

A newer available release requires `--upgrade` when an older installation is present. Installation
is side by side and does not activate the new binary. A newer installed version blocks an older
selection unless `--allow-older` is explicitly requested. A receipt mismatch for changed signed
archive/source content at the same version requires inspection or explicit reinstall; it never
silently replaces the existing installation.

## Optional: check the installer itself

The hashes and the key fingerprint that the installer pins are in `install.sh`, which comes from the
clone, so the clone is the root of the whole check. The project's commits are signed with the same
publisher key. If you want to authenticate the clone as well, fetch the key from the publisher's domain
and check the signature of the commit you cloned (the fingerprint it prints must be the one you
confirmed):

```sh
gpg --auto-key-locate clear,nodefault,wkd --locate-external-keys christian@nachtigall.dev
git verify-commit HEAD
```

A "good signature" with the confirmed fingerprint means that `install.sh` is what the publisher
committed. GnuPG will warn that the key is not certified; your own confirmation of the fingerprint
replaces that.

## What it does and does not do

It builds with `cargo build --locked --release --no-default-features --bin mind` in a private temporary
directory, then installs `~/.local/lib/mindspace/<version>/bin/mind` (directories `0700`, binary
`0755`) and prints its path, version and SHA-256. It removes its temporary files, also when you
interrupt it.

Cargo runs with a private home inside the temporary directory. Your `~/.cargo` (configuration,
registry, cache) is neither read nor written, so the locked dependencies are downloaded on every
install, and proxy settings that live in `~/.cargo/config.toml` do not apply; use the usual
environment variables such as `https_proxy`. The installer also refuses to build when a
`.cargo/config.toml` exists in the temporary directory or in any directory above it, because Cargo
would read it and it could make the build run someone else's code (set `TMPDIR` to a private
directory if `/tmp` or a parent has one).

It never uses `sudo`, never edits shell startup files, never touches Codex or Claude Code
configuration, never opens or migrates any Mindspace database, and never replaces an existing
install unless you ask with `--reinstall`.

## After installing

Run the binary at the full path printed by the installer, or add that version to your `PATH` yourself.
The examples below use RC.5; substitute the installed version if different:

```sh
# bash or zsh (add to ~/.bashrc or ~/.zshrc to keep it)
export PATH="$HOME/.local/lib/mindspace/0.1.0-alpha.1/bin:$PATH"
```

```fish
# Fish (stores the entry in your universal path)
fish_add_path ~/.local/lib/mindspace/0.1.0-alpha.1/bin
```

Check it, then follow the [README quickstart](../README.md#quickstart):

```sh
mind --version
```

`mind doctor` prints a lot of detail. Start with the lines that begin with `PASS`, `WARN`, `FAIL` or
`SKIP`, and read the indented lines under a `WARN` or `FAIL` only:

```sh
mind doctor | grep -E '^(PASS|WARN|FAIL|SKIP)'
```

The `sandbox` line matters only for the MCP repository tools. A `WARN` about a different `mind` earlier
on your `PATH` means an older binary shadows this one.

## Options

| Option | Meaning |
| --- | --- |
| `--version VERSION` | Select an exact published version; default includes prereleases, ordered by publication time. |
| `--trust-fingerprint FPR` | Give your confirmed fingerprint up front. Required without a terminal. |
| `--verify-only` | Download and authenticate the release, then stop. Nothing is built or installed. |
| `--assets-dir DIR` | Use local assets for the selected contract. With explicit RC.5 this also permits offline verification; signatures and hashes are still required. |
| `--key-file FILE` | Use a public key file you obtained independently instead of the publisher's domain. |
| `--upgrade` | Install this version beside validated older versions; preserve binaries and existing registrations. Activation is separate. |
| `--reinstall` | Replace an existing install of this version, after a successful build. The old one is kept in `~/.local/lib/mindspace/previous/`. |
| `--allow-older` | Install this version although a newer one is already installed (side by side). |
| `--keep-work` | Keep the temporary directory for inspection. |

## If it stops

| Message | What to do |
| --- | --- |
| `not running in a terminal` | Confirm the fingerprint independently, then run `sh install.sh --trust-fingerprint <40 hex characters>`. |
| `Cargo would read ... while building here` | Set `TMPDIR` to a private directory (mode 700) that has no `.cargo/config.toml` in it or above it, and run it again. |
| `missing required tools` | Install the named tools with your package manager and run it again. |
| `Rust 1.98.1 is not installed`, `this Alpha builds with Rust 1.98.1 exactly` | `rustup toolchain install 1.98.1 --profile minimal` (without rustup, `rustc` and `cargo` must both be 1.98.1). |
| `Already up to date` | Nothing to do. The install is intact. |
| `it has no install record` | The install runs but cannot be checked. Run `sh install.sh --reinstall` for a verified build. |
| `is not a valid installation` | The reason is printed. Nothing was changed. `sh install.sh --reinstall` stages a fresh build and preserves the old prefix under `previous/`. Unexpected data must be inspected and backed up first. |
| `a newer version is already installed` | Update your checkout (`git pull`) for the current installer, or pass `--allow-older`. |
| `does not match its pinned SHA-256`, `signature check failed`, `invalid, expired, revoked` | Do not use the files. Retry on a different network; if it persists, report it privately (see [SECURITY.md](../SECURITY.md)). |
| `could not fetch the publisher key` | Retry, or obtain the public key independently and pass `--key-file`. |
| `the build failed` | The last lines of the build log are printed. Nothing was installed. |

## Verify by hand

To authenticate the release without the installer, follow the
[manual verification steps](source-install.md#1-obtain-and-verify-the-source-archive) and the
`VERIFY.txt` asset.

## Uninstall

The example removes RC.5; use the selected version printed by your installation:

```sh
rm -f -- ~/.local/lib/mindspace/0.1.0-alpha.1/bin/mind ~/.local/lib/mindspace/0.1.0-alpha.1/install-record
rmdir ~/.local/lib/mindspace/0.1.0-alpha.1/bin ~/.local/lib/mindspace/0.1.0-alpha.1
```

Installs replaced by `--reinstall` are in `~/.local/lib/mindspace/previous/`; delete them when you no longer
want them. Databases live elsewhere and are not removed. See the
[uninstall and troubleshooting](source-install.md#11-limits-troubleshooting-and-uninstall) reference for
registrations made with `mind setup`.

## About wording inside the signed archive

The signed archive's own documents and `VERIFY.txt` were frozen before publication. They still say that
the release is "unpublished" or a "candidate", and that there is no public reporting destination. That
wording is historical. They cannot be edited without invalidating the signatures. Current
instructions are the ones in this repository, and vulnerabilities are reported through
[GitHub private vulnerability reporting](../SECURITY.md#reporting-a-vulnerability).

## Installer regression checks

Run the offline prefix-validation and publication tests with:

```sh
sh tests/scripts/install_lifecycle_test.sh
sh tests/scripts/install_publish_test.sh
```

The publication suite uses a fixture executable and the actual installer helpers and publication
code; it exercises filesystem changes, rollback, signals and locks. It does not authenticate or build
a release. It requires Python 3 to extract those sections for testing. The lifecycle suite does not
need a Rust toolchain. Both run in disposable temporary HOME directories.

`sh tests/scripts/install_sh_test.sh` verifies the real published signatures, integrity refusals and
other preflight behavior without compiling. Add `--with-build` for real RC.5 builds, installation,
repeat installation, reinstall, injected staging/publication failures and build interruption. That
run needs the tools and network described above. Synthetic version-transition tests do not prove
compatibility with a future unpublished release or a database migration.

The strict parser, actual-installer discovery tests and signed future-release fixtures run with:

```sh
python3 -B tests/scripts/test_installer_release_contract.py
python3 -B tests/scripts/install_discovery_test.py
python3 -B tests/scripts/install_future_release_test.py
python3 scripts/embed-installer-contract.py --check
```

The default future-fixture suite authenticates synthetic releases with an ephemeral test key and
needs no Rust toolchain. Its optional real-build mode is documented in the test script. Fixtures do
not modify published releases or establish compatibility with an unpublished production binary.
