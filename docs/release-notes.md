# v0.1.0-alpha.2 release notes

**v0.1.0-alpha.2 is the current public Alpha.** It is published as a GitHub
prerelease, not a stable release, and supersedes v0.1.0-alpha.1 (RC.5). The earlier
v0.2.0 and v0.3.0 pre-alpha releases and their tags have been removed.

**Download `mindspace-0.1.0-alpha.2-source.tar.gz` and its verification files from the
[official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.2).
This uploaded archive is the authoritative, qualified Alpha source distribution.**
Verify it before extracting or building. GitHub's generated "Source code (zip)" and
"Source code (tar.gz)" downloads are the whole repository at the release commit, not the
verified payload.

## What changed since alpha.1

- `mind memory create` creates an explicit memory from the command line. It follows the
  rules of the MCP tool `memory_create`: repeating the same subject and content creates
  nothing new, and `-` reads the text from standard input.
- The MCP tool `server_status` (read-only) shows which database a live server opened, how
  it was chosen (`--db`, `MINDSPACE_DB` or the default location) and whether a plain `mind`
  in the same environment would choose the same one. The MCP server now has 16 tools.
- An MCP server that keeps running after a newer `mind` migrated the database refuses
  every call with a message that tells you to restart the agent session. Before, its reads
  kept working and its writes failed with "no such function".
- The MCP server creates its private temporary directory with mode 0700.
- Documentation: a try-out database is separated from your own database in the
  quickstart, and the three Rust version numbers (minimum 1.96, development pin, archive pin)
  are explained.
- The archive pins Rust 1.100.0 (alpha.1: 1.98.1).
- The database schema is unchanged (17). Moving between alpha.1 and alpha.2 runs no migration.

## What is included

Source-only Linux x86_64, built without embeddings: explicit local memories,
Legacy Markdown/text and committed Git ingestion, lexical retrieval, Rust symbol
search, spaces, CLI and stdio MCP. Canonical document/PDF ingestion, M8 document
publication and real-model inference are outside this qualified Alpha profile.

After verification, read the archive's `README.md`, `docs/source-install.md`,
`docs/release-notes.md` and `docs/offline-recovery.md` for installation, supported
environment, recovery precautions and known limitations. The recommended build is
`cargo build --locked --release --no-default-features --bin mind` with the pinned
Rust/Cargo 1.100.0 toolchain of the archive. There is no bundled binary, dependency vendor tree or
model weight package.

## What was checked

The release is published only after all of the following hold for the release commit and
this archive:

- the public CI of that exact commit passes: format, clippy with warnings denied, the full
  test suite, `cargo audit`, `cargo deny`, the documentation link check, the installer tests,
  the build with the minimum Rust version and the Landlock security profile;
- the archive builds to identical bytes when it is assembled twice from that commit;
- the archive, extracted into an empty directory, builds with
  `cargo build --locked --release --no-default-features --bin mind`, and the built `mind`
  reports `0.1.0-alpha.2` and runs the quickstart commands.

`install.sh` discovers only published releases, so its run against this release cannot happen
before publication. CI tests it against signed fixtures. The result of the real run is recorded
on the release page after publication.

**Not repeated for alpha.2:** the clean-userspace installation, the offline-recovery qualification
and the independent security audit that were done for alpha.1. Treat alpha.2 as an
Alpha with this narrower evidence. The retained documents describe the qualified RC.5 state.

## Verification and provenance

The release carries these assets:

- `mindspace-0.1.0-alpha.2-source.tar.gz` and `.asc`
- `source-archive-manifest.json` and `.asc`
- `mindspace-release.json` and `.asc` (the envelope that `install.sh` reads)
- `SHA256SUMS`
- `VERIFY.txt`
- `mindspace-release-signing-key.asc`

The archive size and SHA-256 are in `SHA256SUMS`. This document is part of the archive and
cannot contain the archive's own hash. Primary signing fingerprint:
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.

Authenticate the publisher independently, then follow `VERIFY.txt`. Require both
detached signatures under the pinned signer and both strict checksum checks. A
bundled public key or checksum alone does not authenticate the publisher.

The `install.sh` script on public `main` automates these checks and builds the verified
archive with the Rust version the archive pins. See the [install guide](install.md) in the
repository. The manual steps in `VERIFY.txt` and the
[operator reference](source-install.md#1-obtain-and-verify-the-source-archive) remain
valid.

The signed manifest binds the archive bytes and its source files to the source commit and
tree it records. That commit is the release commit on public `main`, so its Git history
and signatures can be fetched and checked. The archive itself contains no Git history.

The authenticated archive contains the MIT `LICENSE`, `LICENSES/Apache-2.0.txt`,
`THIRD_PARTY_NOTICES.md` and `tokenizer-provenance.json`. Retain applicable notices
when redistributing. Separate binary/vendor distributions need their own inventory.

## Security reports

Report vulnerabilities privately using
[Report a vulnerability](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(GitHub sign-in required). Do not put sensitive details in public issues, pull
requests or discussions. Include the exact archive/version, environment, startup
flags and a reproduction using your own temporary data. Handling is best effort,
with no guaranteed response time or maintained Alpha release branch implied.

## The previous release: v0.1.0-alpha.1 (RC.5)

alpha.1 was the first independently qualified public Alpha. Its archive
`mindspace-0.1.0-alpha.1-rc.5-source.tar.gz` (1,384,547 bytes, SHA-256
`b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0`) keeps its original pins
and signatures, and `install.sh --version 0.1.0-alpha.1` still installs it. Its tag points to
the older hosting commit `92f68ff646ec9b06ba21df2fb7879dfbc1281838`.

See the [source-install guide](source-install.md), [documentation index](index.md)
and [security policy](../SECURITY.md).
