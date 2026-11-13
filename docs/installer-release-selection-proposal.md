# Dynamic release selection: compatibility and security proposal

Status: implemented design and trust decisions for the separately reviewed discovery change.
Publication and acceptance depend on its executed verification gates. No published release assets
are changed. The original proposal below records the compatibility rationale; the pre-implementation
decisions and runtime contract reference specify the implemented behavior.

## Required behavior

Default to the newest published GitHub release, including prereleases and excluding drafts.
`--version VERSION` selects a particular published release for repeatable source selection.
Select exactly one release before deciding compatibility. If that release is unsupported, malformed,
unavailable or unverifiable, stop with its tag, the failed check and recovery guidance. Never try an
older release silently. Preserve existing installations on every discovery, compatibility, trust,
verification or build failure; retain the lifecycle rules in [install.md](install.md).

Proposed interpretation of "newest published": greatest `published_at`, with release ID as a
stable tie-breaker, across all pages. This is publication order, not SemVer order. A newly published
older version can therefore be selected, but the installation's independent no-downgrade check
still stops it. `--allow-older` remains an explicit side-by-side exception.

## Discovery does not establish authenticity

Use the public list-releases API with bounded responses and complete pagination, explicitly filtering
`draft == false` and requiring a valid publication timestamp. Keep prereleases. Fail if pagination is
incomplete, malformed or rate limited rather than claiming a possibly stale release is latest.
GitHub's `/releases/latest` endpoint excludes prereleases and uses `created_at`, so it cannot meet
this requirement. These endpoint behaviors are documented by
[GitHub's Releases API](https://docs.github.com/en/rest/releases/releases#list-releases) and
[Get the latest release](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
(reviewed 2026-11-13).

For explicit selection, normalize an optional `v` prefix, require a valid supported SemVer, and fetch
the exact `vVERSION` release; reject drafts, missing publication timestamps and ambiguous matches.
Do not fall back from a missing explicit version to latest. Never execute or interpolate release
metadata as shell code. Restrict download locations to the expected repository/release and HTTPS,
with a documented redirect policy and byte/time limits. Match assets by exact supported contracts,
rejecting duplicates. Do not trust the API's asset digest for authenticity.

A hostile discovery service can hide newer releases or replay old metadata. Signatures authenticate
content and identity, not global freshness. Existing-version downgrade protection and explicit
version selection limit this risk; an authenticated freshness mechanism would be a separate design.

## Reviewed RC.5 compatibility

The existing installer pins all seven RC.5 assets by exact name, size and SHA-256, and requires good
detached signatures for both archive and manifest by the independently confirmed publisher
fingerprint `0E6699CB48F3941AE28CA88D509F78007F62D5EA`. Preserve this complete contract for
`v0.1.0-alpha.1`; no relaxed generic adapter, rebuilt signature, renamed asset or amended manifest.

The existing signed `source-archive-manifest.json` has format `mindspace-source-archive-v2`, version
`0.1.0-alpha.1`, and an `archive` object with filename, bytes, SHA-256, uncompressed tar bytes and
SHA-256. It also declares baseline commit/tree and member metadata. It lacks an explicit install
platform and supported build recipe/toolchain contract. Its historical `publication` wording is
qualification metadata; do not reinterpret it as the release discovery timestamp or modify it.
The existing installer supplies Linux x86_64, Rust 1.98.1 and the fixed build command from its pinned
adapter. These facts were inspected in the local RC.5 assets and installer, not inferred from tags.

## Proposed future contract: additive signed release envelope

Before implementing discovery for future releases, review a minimal new `mindspace-release-v1`
envelope. Add it only to future releases, retaining the source archive manifest format and verifier.
Proposed fixed bootstrap names: `mindspace-release.json` and `mindspace-release.json.asc`.
Authenticate the envelope's detached signature against the independently confirmed publisher
fingerprint **before using its contents**. The downloaded public key must match that fingerprint;
a key bundled beside artifacts is never an identity authority.

The proposed envelope binds repository identity, exact tag and semantic version, a supported install
platform, a known build-recipe identifier, and a Rust toolchain version supported by that recipe.
It lists the exact filename, size and SHA-256 of the archive, its detached signature, the source
manifest and its detached signature. It identifies the supported source-manifest format and source
commit/tree. This is a proposed field set, not a finalized JSON schema or publisher authorization.
Signature sidecars do not hash themselves; hashes of them are declared by the separate signed envelope.

The client dispatches only explicitly implemented envelope formats, source-manifest formats and
build recipes. Manifest values cannot supply shell commands, arbitrary Cargo flags, URLs, filenames
with path components or unsupported toolchains. Avoid guessing asset names from a version suffix.
A strict JSON parser must reject duplicate keys, type errors and unknown required semantics; select
and document the parser dependency before implementation rather than parsing JSON with grep.

After envelope authentication: validate schema and tag/version/platform bindings; fetch only declared
assets; verify all declared byte counts and hashes; require publisher signatures on source manifest
and archive; cross-check archive identity and source provenance against the authenticated manifest;
then validate member paths/types and extraction limits. Only after all checks may extraction and the
known build recipe run. Authentication failures never trigger another release or another contract.

## Separate security review and acceptance gates

The follow-up change must include a finalized schema and publisher generation/validation tests,
with local signed fixtures for future releases; no edits to the published RC.5 assets. Review the
bootstrap script's own distribution trust, parser, URL handling, key rotation/revocation/expiry,
release-to-manifest binding, archive resource limits, and downgrade/rollback behavior independently.
The mandatory fingerprint confirmation remains on every path that installs or replaces a binary.
An offline no-op against an existing valid installation does not perform new authentication.

Required regression cases:

- Stable and prerelease discovery, drafts, pagination, out-of-order timestamps, ties, empty responses,
  API errors/rate limits, malformed or oversized JSON, and exact `--version` lookup.
- Newest unsupported contract/platform/toolchain, newest bad signature/hash, missing/duplicate assets:
  fail for that selected release and prove there was no fallback to an older valid fixture.
- Wrong publisher key, revoked/expired signatures, unsigned or malformed envelopes, duplicate JSON
  keys, tag/version mismatch, asset substitution, path traversal, links and oversized extraction.
- Exact pinned RC.5 verification still accepts original assets and rejects every modified asset.
- Existing valid/legacy/invalid prefixes, explicit upgrade/reinstall, no automatic downgrade, failed
  verification/build/publication and interruption: previous binary/data/config/MCP registrations
  remain preserved. A fixture upgrade is not evidence of compatibility with an unpublished binary.

Release decision for dynamic selection remains pending this review, schema agreement, implementation
and executed gates. The current lifecycle change makes no dynamic-discovery claim. Website legal
controls are not applicable to this CLI installer and documentation-only proposal.

## Pre-implementation challenge and decisions (2026-11-13)

Phase 1 is published separately at `ff707b6e9b00b98c843afe625c3c75aa055abfc4`; its full public CI
passed. An independent design review found no blocker to the following scoped implementation.

Eligibility means published and non-draft, before compatibility checks. Select the greatest
`(published_at, numeric release ID)` across complete bounded pagination; validate the selected tag
and contract afterwards. A malformed/unsupported newest release is an error, never a reason to try
an older release. Explicit versions accept exactly one optional `v`, strict SemVer and exact tag
lookup. Reject duplicate/conflicting identities, draft explicit results, missing or invalid dates,
incomplete/rate-limited responses and deletion during fetch. URLs are constructed for the fixed
repository, never taken from metadata; use HTTPS, disable curl configuration, bound bytes and time.

The runtime remains a self-contained shell installer. Python 3.11 or later, standard library only,
is an explicit prerequisite for strict JSON and TOML parsing. Reject duplicate keys, non-finite
constants, bool-as-integer values, unexpected envelope fields and unsupported semantics. Never
source/eval parser output. Parsed contract fields have fixed positions and constrained character sets.

The additive `mindspace-release-v1` contract binds exactly: repository, tag, version, platform,
recipe, rust_version, source_commit, source_tree, source_manifest_format and assets, plus its format.
Assets have exactly four roles: archive, archive_signature, source_manifest and
source_manifest_signature; each declares filename, bytes and SHA-256. All filenames are distinct safe
basenames distinct from bootstrap `mindspace-release.json` and `.asc`. Signature sidecars must use
their corresponding payload basename plus `.asc`. Only Linux x86_64, the fixed Cargo source recipe
without embeddings, Rust 1.98.1 or 1.100.0, and source manifest `mindspace-source-archive-v2` are
supported initially. Other contracts require an installer update. The signed source manifest must
agree on version, archive name/size/hash, baseline commit/tree; declared tar bytes/hash and member
paths/types/sizes/hashes are validated before extraction. Source and archive detached signatures
remain mandatory. Resource limits are part of this supported contract, not publisher-provided code.

Authenticate future envelopes before using their declared archive/source identity to evaluate an
existing installation. A changed archive or source commit under the same version invalidates the
receipt and requires explicit inspection/reinstall. The RC.5 adapter retains every current asset pin,
size and signature rule. Explicit RC.5 plus local `--assets-dir` retains offline verification.

No automatic key rotation: a new primary fingerprint, expiry or revocation fails. Trusting another
key requires an independently authenticated installer update; metadata cannot choose a key.
The operator's pre-existing ultimately trusted local publisher key, signing configuration and
verified signed history provide the independent identity basis in this run. A download or command
argument alone does not authenticate identity.

Unresolved limits, accepted as limits of this scoped implementation rather than security guarantees:

- Unsigned discovery cannot prove global freshness or detect all hidden/replayed releases. Existing
  version checks prevent automatic downgrade, but do not solve freeze attacks.
- `--version` binds a signed version, not historical immutable bytes for a fresh client if a publisher
  re-signs that version. Receipts detect changed identity for existing installs; stronger fresh-client
  reproducibility requires an immutable publication policy or a prior authenticated digest.
- Pagination is not atomic. Detect observable conflicts and fail/retry; no atomic-snapshot claim.
- Only one public release exists in the inspected listing. Signed fixture transitions prove code
  behavior, not compatibility between two public production binaries or database schemas.

Implementation plan: (1) strict discovery/contract tests observed failing; (2) embed parser and
signed-envelope adapter, publisher manifest generator and hostile signed fixtures; (3) lifecycle and
RC.5 regression; (4) independent security/correctness review, signed focused commit, normal push,
exact-SHA public CI. Phase 3 begins only after that publication is verified.

## Runtime contract and future publisher preparation

The exact versioned schema and limits are enforced in
[`scripts/installer_release_contract.py`](../scripts/installer_release_contract.py), embedded verbatim
in `install.sh`; `python3 scripts/embed-installer-contract.py --check` checks parity. This avoids a
runtime download of companion code. GPG verification precedes parsing a future envelope. Every
future payload, source binding and archived Cargo/toolchain TOML is validated before either a no-op
or extraction/build. Future receiptless prefixes require explicit reinstall; the historical RC.5
operational legacy exception is not extended to new contracts.

Resource limits: 4,000,000 bytes per discovery page, at most 100 pages; 64,000 bytes per envelope;
50,000,000 compressed archive bytes; 5,000,000 source-manifest bytes; 16,000 per declared signature;
100,000,000 uncompressed tar bytes and 10,000 members. Only plain USTAR files/directories are
supported; links, PAX/GNU extensions, traversal, missing/extra members, mode/size/hash disagreement
and mismatched Cargo/toolchain metadata fail. Sizes are integers, not booleans or floats.

For a future release only, prepare a qualified source v2 archive and source manifest with their two
detached publisher signatures, then generate a new unsigned envelope:

```sh
python3 scripts/make-installer-release-manifest.py --archive SOURCE.tar.gz --manifest source-archive-manifest.json --archive-signature SOURCE.tar.gz.asc --manifest-signature source-archive-manifest.json.asc --rust-version 1.98.1 --output mindspace-release.json
```

The generator validates source/archive bindings and hashes the two supplied signature files; it does
**not** authenticate those sidecars or sign anything. Independently verify both publisher signatures
and review the generated fields before signing the envelope with the confirmed publisher key. Supply
`mindspace-release.json.asc` as a detached armored signature; publish the two bootstrap files plus
the four declared assets together in a future release. This task does not create a release, sign
production future artifacts or change RC.5. Same-version republishing and automatic key rotation
are not recommended or implied by this generator.
