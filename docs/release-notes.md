# v0.1.0-alpha.1 release notes

**v0.1.0-alpha.1 establishes the first independently qualified public Alpha
baseline** and is the only published release. The earlier v0.2.0 and v0.3.0
pre-alpha releases and their tags have been removed.

It is published as a GitHub prerelease, not a stable release.

**Download `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz` and its six verification
sidecars from the [official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1). This uploaded archive is the authoritative,
qualified Alpha source distribution.** Verify it before extracting or building.

The repository's `main` implementation represents older development milestones,
including the commits the former `v0.2.0`/`v0.3.0` tags pointed to. The
`v0.1.0-alpha.1` tag points to the older public commit
`92f68ff646ec9b06ba21df2fb7879dfbc1281838` solely to anchor the release page; it
does not identify the RC.5 source tree.
**GitHub's generated “Source code (zip)” and “Source code (tar.gz)” downloads,
and the tag tree contain older source, not RC.5.
Public main includes the RC.5 source baseline (`a027d94`) and the Alpha documentation.**
Historical version numbers do not identify this Alpha or imply a database downgrade.

## What is included

Source-only Linux x86_64, built without embeddings: explicit local memories,
Legacy Markdown/text and committed Git ingestion, lexical retrieval, Rust symbol
search, spaces, CLI and stdio MCP. Canonical document/PDF ingestion, M8 document
publication and real-model inference are outside this qualified Alpha profile.

After verification, read the archive's `README.md`, `docs/source-install.md`,
`docs/release-notes.md` and `docs/offline-recovery.md` for installation, supported
environment, recovery precautions and known limitations. The recommended build is
`cargo build --locked --release --no-default-features --bin mind` with the pinned
Rust/Cargo 1.98.1 toolchain of the archive. There is no bundled binary, dependency vendor tree or
model weight package.

## Verification and provenance

Obtain all seven assets together:

- `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz`
- `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz.asc`
- `source-archive-manifest.json`
- `source-archive-manifest.json.asc`
- `SHA256SUMS`
- `VERIFY.txt`
- `mindspace-release-signing-key.asc`

Archive: **1,384,547 bytes**, SHA-256
`b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0`.
Manifest SHA-256:
`903d8fd226593b74083bc8c0a11b7ffafc520eae06b5f054d0565ad1f5dae391`.
Primary signing fingerprint:
`0E6699CB48F3941AE28CA88D509F78007F62D5EA`.

Authenticate the publisher independently, then follow `VERIFY.txt`. Require both
detached signatures under the pinned signer and both strict checksum checks. A
bundled public key or checksum alone does not authenticate the publisher. The
instructions explain independent publisher/domain trust and WKD key retrieval.

The `install.sh` script on public `main` automates these checks, builds the verified
archive with Rust 1.98.1 and installs one binary; see the [install guide](install.md).
It pins the same hashes and the same key fingerprint, and it asks you to confirm
that fingerprint. The manual steps in `VERIFY.txt` and the
[operator reference](source-install.md#1-obtain-and-verify-the-source-archive) remain
valid.

The signed manifest binds these archive bytes and its 235 source files to source
commit `a027d94680392648b6277dc2fd9bc20906114b91`, tree
`3cc15658d9c840522cbff102b44f6d004d5506bf`. Local qualification was recorded at
signed evidence commit `7e5b26e7321f2ad6aaee24164b40ffc28c81845e`.
Those source/evidence commits and the broader RC.5 Git history are **not published
in this repository**. The public signed manifest is the publisher's provenance
statement; those private Git signatures/tree cannot be independently fetched from
the hosting tag. The archive contains no Git history. Its normal verification and
source build do not require private Git access; the optional reconstruction from
the original Git commit does.

The authenticated archive contains the MIT `LICENSE`, `LICENSES/Apache-2.0.txt`,
`THIRD_PARTY_NOTICES.md` and `tokenizer-provenance.json`. Retain applicable notices
when redistributing. Separate binary/vendor distributions need their own inventory.

## Security reports and frozen-document status

Report vulnerabilities privately using
[Report a vulnerability](https://github.com/VikingOwl91/mindspace/security/advisories/new)
(GitHub sign-in required). Do not put sensitive details in public issues, pull
requests or discussions. Include the exact archive/version, environment, startup
flags and a reproduction using your own temporary data. Handling is best effort,
with no guaranteed response time or maintained Alpha release branch implied.

The frozen RC.5 archive documents, `VERIFY.txt` and the signed manifest record the
preparation state, when publication and a reporting destination were still
pending. The manifest's `publication` field therefore still reads
`local qualification only; publication not authorized`. The release
page and these notes supply the subsequent delivery and reporting information. The archive,
manifest, verification instructions and signatures retain their qualified bytes.

See the [source-install guide](source-install.md), [documentation index](index.md)
and [security policy](../SECURITY.md).
