# v0.1.0-alpha.1 release notes

**v0.1.0-alpha.1 establishes the first independently qualified public Alpha
baseline.** Earlier v0.2.0 and v0.3.0 releases are historical pre-alpha development
snapshots.

**Draft Prerelease: not published.** The tag name and pinned hosting target are
reserved in this draft; no published tag is asserted. These instructions apply
to the attached assets available to authorized draft readers and to their future
public availability after separate publication authorization.

**Download `mindspace-0.1.0-alpha.1-rc.5-source.tar.gz` and its six verification
sidecars from the [official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1) when published. This uploaded archive is the qualified Alpha
source distribution.** Verify it before extracting or building.

The repository's `main` implementation and historical
`v0.2.0`/`v0.3.0` tags describe older development milestones. The
`v0.1.0-alpha.1` draft has its hosting target pinned to approved older commit
`92f68ff646ec9b06ba21df2fb7879dfbc1281838` solely to anchor this release page.
**GitHub's generated “Source code (zip)” and “Source code (tar.gz)” downloads,
a future tag tree and a repository clone contain older source, not RC.5.
Public main now includes the Alpha documentation alongside its older implementation.**
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
Rust/Cargo 1.98.1 toolchain. There is no bundled binary, dependency vendor tree or
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

The frozen RC.5 archive documents and `VERIFY.txt` record the preparation date,
when publication and a reporting destination were still pending. This release
page supplies the subsequent delivery and reporting information. The archive,
manifest, verification instructions and signatures retain their qualified bytes.

See the [source-install guide](source-install.md), [documentation index](index.md)
and [security policy](../SECURITY.md).
