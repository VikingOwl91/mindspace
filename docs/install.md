# Install Mindspace v0.1.0-alpha.1

The installer downloads the signed Alpha release, authenticates it, builds it with Rust 1.98.1 and
installs one binary under your home directory. It takes a few minutes and asks you one question in a
terminal (or takes the answer as an option). For the
manual procedure and the operator reference, see the [source-install guide](source-install.md).

## Before you start

- **Linux on x86_64.** Other systems are not qualified.
- **Tools:** `curl`, `gpg` (GnuPG with `--assert-signer` support; the installer checks), `tar`, `gzip`,
  `sha256sum`, and a C compiler with C headers (`gcc` or `cc`). Install Git too: ingesting a
  repository needs it.
- **Rust 1.98.1.** With [rustup](https://rustup.rs): `rustup toolchain install 1.98.1 --profile minimal`.
  The installer checks for it and never installs it for you. It does not use `stable`, and it does not
  use the toolchain pinned on the main branch.
- **About 2 GiB of free space** in `/tmp`, or in the directory named by `TMPDIR`.
- **Network access** to GitHub, `openpgpkey.nachtigall.dev` and, through Cargo, crates.io.

## Install

```sh
git clone https://github.com/VikingOwl91/mindspace.git
cd mindspace
sh install.sh
```

These commands work as written in bash, zsh and Fish. In Fish, run the installer as `sh install.sh`
too; the installer itself is POSIX shell and Fish only starts it.

The clone is only the way to obtain `install.sh`. **The clone is not the Alpha**: public `main` holds
an older implementation, and so do the release tag and GitHub's "Source code" downloads. The installer
never builds any of them. It downloads the seven assets of the
[v0.1.0-alpha.1 release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1), checks
them, and builds the signed `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz` they contain.

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

What the installer then checks, and stops on any failure:

1. All seven release assets have the exact size and SHA-256 that the installer pins.
2. The publisher key fetched from the publisher's own domain (OpenPGP Web Key Directory) has the
   fingerprint you confirmed. The key file bundled in the release must match it too, but it does not
   authenticate anybody.
3. Both detached signatures (archive and manifest) are good, made by that key, and neither the
   signature nor the key is expired or revoked.
4. The signed manifest names this archive and the expected source commit.
5. The archive contains only regular files and directories below `mindspace-0.1.0-alpha.1/`.

The pinned hashes are what binds the installer to exactly these bytes; the signatures show that the
publisher you confirmed signed them.

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
directory, then installs `~/.local/lib/mindspace/0.1.0-alpha.1/bin/mind` (directories `0700`, binary
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
configuration, never opens or migrates any Mindspace database, and never overwrites an existing
install: if the versioned directory exists it stops and leaves it alone.

## After installing

Run the binary by its full path, or add it to your `PATH` yourself:

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
mind --db ~/mindspace-quickstart/mind.db doctor | grep -E '^(PASS|WARN|FAIL|SKIP)'
```

The `sandbox` line matters only for the MCP repository tools. A `WARN` about a different `mind` earlier
on your `PATH` means an older binary shadows this one.

## Options

| Option | Meaning |
| --- | --- |
| `--trust-fingerprint FPR` | Give your confirmed fingerprint up front. Required without a terminal. |
| `--verify-only` | Download and authenticate the release, then stop. Nothing is built or installed. |
| `--assets-dir DIR` | Use seven assets you downloaded yourself. They are verified exactly like downloaded ones. |
| `--key-file FILE` | Use a public key file you obtained independently instead of the publisher's domain. |
| `--keep-work` | Keep the temporary directory for inspection. |

## If it stops

| Message | What to do |
| --- | --- |
| `not running in a terminal` | Confirm the fingerprint independently, then run `sh install.sh --trust-fingerprint <40 hex characters>`. |
| `Cargo would read ... while building here` | Set `TMPDIR` to a private directory (mode 700) that has no `.cargo/config.toml` in it or above it, and run it again. |
| `missing required tools` | Install the named tools with your package manager and run it again. |
| `Rust 1.98.1 is not installed`, `this Alpha builds with Rust 1.98.1 exactly` | `rustup toolchain install 1.98.1 --profile minimal` (without rustup, `rustc` and `cargo` must both be 1.98.1). |
| `An installation already exists` | The installer left it untouched. Move or remove that directory yourself to reinstall. |
| `does not match its pinned SHA-256`, `signature check failed`, `invalid, expired, revoked` | Do not use the files. Retry on a different network; if it persists, report it privately (see [SECURITY.md](../SECURITY.md)). |
| `could not fetch the publisher key` | Retry, or obtain the public key independently and pass `--key-file`. |
| `the build failed` | The last lines of the build log are printed. Nothing was installed. |

## Verify by hand

To authenticate the release without the installer, follow the
[manual verification steps](source-install.md#1-obtain-and-verify-the-source-archive) and the
`VERIFY.txt` asset.

## Uninstall

```sh
rm -- ~/.local/lib/mindspace/0.1.0-alpha.1/bin/mind
rmdir ~/.local/lib/mindspace/0.1.0-alpha.1/bin ~/.local/lib/mindspace/0.1.0-alpha.1
```

Databases live elsewhere and are not removed. See the
[uninstall and troubleshooting](source-install.md#11-limits-troubleshooting-and-uninstall) reference for
registrations made with `mind setup`.

## About wording inside the signed archive

The signed archive's own documents and `VERIFY.txt` were frozen before publication. They still say that
the release is "unpublished" or a "candidate", and that there is no public reporting destination. That
wording is historical. They cannot be edited without invalidating the signatures. Current
instructions are the ones in this repository, and vulnerabilities are reported through
[GitHub private vulnerability reporting](../SECURITY.md#reporting-a-vulnerability).
