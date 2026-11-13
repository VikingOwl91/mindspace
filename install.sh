#!/bin/sh
# Mindspace signed release installer (latest published release, including prereleases).
#
# Select exactly one published release before checking compatibility. Build only its authenticated
# source assets using an explicitly supported contract; never use GitHub-generated source archives.
# Unsupported or unverifiable newest releases fail without trying older ones.
#
# What it does: discovers one published release, authenticates its supported contract and assets
# with the publisher key you independently confirm, builds with the authenticated exact toolchain
# and installs one binary under $HOME/.local/lib/mindspace/<version>/bin/mind.
# RC.5 retains its original seven hardcoded asset pins and detached-signature requirements.
# What it never does: use sudo, edit shell startup files, touch an agent configuration or an existing
# database, replace an existing install without --reinstall, or fall back to an unverified download.
#
# Usage:  sh install.sh [--trust-fingerprint FPR] [--verify-only] [--assets-dir DIR]
#                       [--key-file FILE] [--upgrade] [--reinstall] [--allow-older] [--keep-work] [--help]

set -eu
umask 077
export LC_ALL=C

VERSION=0.1.0-alpha.1
TAG=v$VERSION
BASE_URL=https://github.com/VikingOwl91/mindspace/releases/download/$TAG
TOPDIR=mindspace-$VERSION
ARCHIVE=mindspace-$VERSION-rc.5-source.tar.gz
MANIFEST=source-archive-manifest.json
ARCHIVE_SHA=b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0
MANIFEST_COMMIT=a027d94680392648b6277dc2fd9bc20906114b91
RUST_VERSION=1.98.1
NEED_KIB=2000000

# Publisher signing key (primary fingerprint). The same value is in the public documentation.
FPR=0E6699CB48F3941AE28CA88D509F78007F62D5EA
FPR_SPACED='0E66 99CB 48F3 941A E28C  A88D 509F 7800 7F62 D5EA'
WKD_URL='https://openpgpkey.nachtigall.dev/.well-known/openpgpkey/nachtigall.dev/hu/rckmfa7rw857sf15hkixzhxx4681turz?l=christian'

# sha256, size in bytes, name of every release asset. All seven are required.
ASSET_TABLE='b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0 1384547 mindspace-0.1.0-alpha.1-rc.5-source.tar.gz
6604a649c6795249ebf168116418825db9bef08ef370b2a1884aee4df190b685 228 mindspace-0.1.0-alpha.1-rc.5-source.tar.gz.asc
903d8fd226593b74083bc8c0a11b7ffafc520eae06b5f054d0565ad1f5dae391 58659 source-archive-manifest.json
f2eda80092b572761e80f381128e75afee4b042a21af66d5a1064d9aabf52f2d 228 source-archive-manifest.json.asc
5fc56e89bed615a7755bfe4e28eb63bd3fe6e855f586bae6142c14754ecb06ef 204 SHA256SUMS
86c667876ad6e91a7dce4341c44d4ea2207fdba7c3ad368d51f74dfdc2a46b84 6280 VERIFY.txt
bcba2140c52fb84befaedba8ae1dfd8b1aa2fb121149a2d6bba5e4736e88879b 680 mindspace-release-signing-key.asc'

REQUESTED_VERSION=
CONTRACT=rc5
ASSETS_READY=0
TRUST_ACCEPTED=0
TMP_BASE=
WORK=
PREFIX=
STAGED=
BACKUP_HOLDER=
INSTALL_OK=0
BUILD_PID=
KEEP_WORK=0
VERIFY_ONLY=0
ASSETS_DIR=
KEY_FILE=
TRUST_FPR=
UPGRADE=0
REINSTALL=0
ALLOW_OLDER=0
MOVED_OLD=0
BACKUP=
STATE=none
REASON=
REPAIRABLE=1

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
die() { printf 'STOP: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Mindspace release installer (Linux x86_64, signed source releases)

  sh install.sh [options]

Options:
  --version VERSION        Select this exact published version (optional v prefix); default: newest
                           publication, including prereleases, excluding drafts. No silent fallback.
  --trust-fingerprint FPR  Your confirmed publisher key fingerprint (40 hex characters). Required when
                           the installer is not run in a terminal. Without it you are asked to type it.
  --verify-only            Download and authenticate the release assets, then stop. Nothing is built or
                           installed.
  --assets-dir DIR         Use the selected release assets you already downloaded into DIR instead of
                           downloading them. They are verified exactly like downloaded ones.
  --key-file FILE          Use this public key file, which you obtained independently, instead of
                           fetching the publisher key from the publisher's domain.
  --upgrade                Install the selected version beside validated older versions; no activation.
  --reinstall              Replace an existing installation of this version. Nothing is replaced
                           until a fresh build has succeeded; the old one is moved to
                           ~/.local/lib/mindspace/previous/, not deleted. Failure before replacement
                           restores it if the original path remains free.
  --allow-older            Install this version although a newer one is already installed. It is
                           installed beside the newer one; your PATH decides which one runs.
  --keep-work              Keep the temporary working directory (for inspection) when finished.
  -h, --help               Show this text.

Run twice, it does nothing the second time: an installation that is present and intact is reported
as "Already up to date" and left untouched.

The installer needs: Python 3.11+, curl, gpg (GnuPG with --assert-signer), tar, gzip, sha256sum, a C compiler and
the exact Rust toolchain required by the authenticated contract. See docs/install.md in the repository for the trust decision and troubleshooting.
EOF
}

cleanup() {
    rc=$?
    trap - EXIT INT TERM HUP
    if [ -n "$BUILD_PID" ]; then
        # The build runs in its own session and process group (its id is recorded by the build
        # wrapper): stop cargo, rustc, build scripts and compilers, not just the first process.
        pg=
        [ -r "$WORK/build.pgid" ] && pg=$(cat "$WORK/build.pgid" 2>/dev/null)
        case "$pg" in '' | *[!0-9]*) pg=$BUILD_PID ;; esac
        # Never signal group 0 or 1 (every process of the user), whatever the file contained.
        [ "$pg" -gt 1 ] 2>/dev/null || pg=$BUILD_PID
        kill -s TERM -- "-$pg" 2>/dev/null || true
        kill -s TERM "$BUILD_PID" 2>/dev/null || true
        sleep 1
        kill -s KILL -- "-$pg" 2>/dev/null || true
        wait "$BUILD_PID" 2>/dev/null || true
    fi
    if [ -n "$WORK" ] && [ -d "$WORK/gnupg" ] && command -v gpgconf >/dev/null 2>&1; then
        gpgconf --homedir "$WORK/gnupg" --kill all >/dev/null 2>&1 || true
    fi
    if [ -n "$STAGED" ] && [ -d "$STAGED" ]; then
        rm -f "$STAGED/bin/mind" "$STAGED/install-record" 2>/dev/null || true
        rmdir "$STAGED/bin" "$STAGED" 2>/dev/null || true
    fi
    if [ "$MOVED_OLD" = 1 ] && [ "$INSTALL_OK" != 1 ] && [ -n "$BACKUP" ] && { [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; }; then
        if ! [ -e "$PREFIX" ] && ! [ -L "$PREFIX" ] && mv -T -- "$BACKUP" "$PREFIX" 2>/dev/null; then
            say "The previous installation was restored at $PREFIX." >&2
        else
            say "The previous installation is kept at $BACKUP. Inspect it before restoring it to $PREFIX." >&2
        fi
    fi
    [ -z "$BACKUP_HOLDER" ] || rmdir "$BACKUP_HOLDER" 2>/dev/null || true
    if [ -n "$WORK" ] && [ -d "$WORK" ]; then
        if [ "$KEEP_WORK" = 1 ]; then
            say "Working directory kept: $WORK"
        else
            case "$WORK" in
                */mindspace-install.*)
                    rm -rf -- "$WORK" 2>/dev/null || { sleep 1; rm -rf -- "$WORK" 2>/dev/null; } ||
                        say "Could not remove the temporary directory $WORK; you can delete it." >&2
                    ;;
            esac
        fi
    fi
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

confirm_trust() {
# ---------------------------------------------------------------- 2. publisher trust decision
[ "$TRUST_ACCEPTED" = 0 ] || return 0
step "Publisher key: your trust decision"

cat <<EOF
Releases are signed by Christian Nachtigall <christian@nachtigall.dev>.
Primary key fingerprint (40 hex characters):

    $FPR_SPACED

This installer carries that fingerprint and also fetches the key from the publisher's own domain
(OpenPGP Web Key Directory, openpgpkey.nachtigall.dev). Both come from sources the project controls, so
neither proves who the publisher is. The release page and the files in it can not authenticate
themselves either.

YOU decide whom to trust: confirm this fingerprint through a channel you trust that is not this
download (for example from the maintainer directly or from a record you already hold). If you cannot
confirm it, stop here; see docs/install.md for the manual procedure.
EOF

if [ -z "$TRUST_FPR" ]; then
    if [ -t 0 ]; then
        say ""
        say "Type the fingerprint you have confirmed (spaces allowed), or press Enter to stop:"
        printf '> '
        IFS= read -r TRUST_FPR || TRUST_FPR=
        [ -n "$TRUST_FPR" ] || { say "No fingerprint entered. Nothing was changed."; exit 2; }
    else
        say "" >&2
        say "STOP: not running in a terminal, so there is nobody to ask. Confirm the fingerprint" >&2
        say "independently, then run: sh install.sh --trust-fingerprint <the 40 hex characters>" >&2
        exit 2
    fi
fi
TRUST_FPR=$(printf '%s' "$TRUST_FPR" | tr -d ' \t:' | tr 'abcdef' 'ABCDEF')
case "$TRUST_FPR" in
    *[!0-9A-F]*) die "the fingerprint you gave contains characters that are not hex digits." ;;
esac
[ "${#TRUST_FPR}" -eq 40 ] || die "a fingerprint has 40 hex characters; you gave ${#TRUST_FPR}."
[ "$TRUST_FPR" = "$FPR" ] || die "the fingerprint you gave does not match the one this installer pins ($FPR). Nothing was installed. If the publisher rotated keys, obtain a current install.sh from a source you trust."
say "Fingerprint accepted: you confirmed $FPR."

TRUST_ACCEPTED=1
}

# The parser is embedded for curl|sh; its source and embedding parity are tested in CI.
release_json() {
    python3 -I - "$@" <<'MINDSPACE_RELEASE_PY'
# BEGIN EMBEDDED RELEASE CONTRACT
#!/usr/bin/env python3
"""Strict data-only parsing for the self-contained release installer.

This file is embedded verbatim in install.sh. Signature verification is the shell caller's
responsibility and must precede use of a future envelope. No metadata supplies commands or URLs.
"""
import datetime
import gzip
import hashlib
import json
import math
from pathlib import Path
import re
import sys
import tarfile
import zlib
if sys.version_info < (3, 11):
    print("STOP: Python 3.11 or later is required", file=sys.stderr)
    raise SystemExit(1)
import tomllib

MAX_PAGE = 4_000_000
MAX_ENVELOPE = 64_000
MAX_MANIFEST = 5_000_000
MAX_ARCHIVE = 50_000_000
MAX_TAR = 100_000_000
MAX_MEMBERS = 10_000
MAX_PAGES = 100
ROLES = ('archive', 'archive_signature', 'source_manifest', 'source_manifest_signature')
BOOTSTRAP = {'mindspace-release.json', 'mindspace-release.json.asc'}
SEMVER = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?', re.ASCII)


class ContractError(ValueError):
    """Unsupported or inconsistent release data; never a reason to choose another release."""


def require(condition, message):
    if not condition:
        raise ContractError(message)


def keys(value, expected, label):
    require(type(value) is dict and set(value) == set(expected), f'{label} has missing or unsupported fields')


def number(value, maximum, label, minimum=1):
    require(type(value) is int and minimum <= value <= maximum, f'{label} must be an integer in {minimum}..{maximum}')
    return value


def digest(value, length, label):
    require(type(value) is str and re.fullmatch('[0-9a-f]{'+str(length)+'}', value) is not None, f'{label} is not a supported digest')
    return value


def text(value, label, maximum=512):
    require(type(value) is str and 0 < len(value) <= maximum and all(ord(c) >= 32 and ord(c) != 127 for c in value), f'{label} is not valid text')
    return value


def normalize(value):
    text(value, 'version', 128)
    value = value[1:] if value.startswith('v') else value
    match = SEMVER.fullmatch(value)
    require(match is not None, 'version must be SemVer, optionally prefixed with one v')
    if match.group(4):
        require(not any(part.isdigit() and len(part) > 1 and part[0] == '0' for part in match.group(4).split('.')), 'numeric prerelease identifiers cannot have leading zeros')
    return value


def safe_name(value):
    require(type(value) is str and len(value) <= 200 and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]*', value, re.ASCII) is not None, 'asset filename must be a safe basename')
    require(value not in BOOTSTRAP, 'asset filename collides with a bootstrap envelope')
    return value


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate JSON object key')
        result[key] = value
    return result


def reject_constant(value):
    raise ContractError('non-finite JSON number is not supported')


def finite_float(value):
    result = float(value)
    require(math.isfinite(result), 'non-finite JSON number is not supported')
    return result


def read_bytes(path, maximum):
    path = Path(path)
    require(not path.is_symlink() and path.is_file(), f'{path.name} is not a regular file')
    with path.open('rb') as stream:
        data = stream.read(maximum + 1)
    require(len(data) <= maximum, f'{path.name} exceeds the supported byte limit {maximum}')
    return data


def load_json(path, maximum):
    data = read_bytes(path, maximum)
    try:
        return json.loads(data.decode('utf-8'), object_pairs_hook=unique_pairs, parse_constant=reject_constant, parse_float=finite_float)
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise ContractError(f'{Path(path).name} is not supported JSON: {type(error).__name__}') from None


def metadata(value):
    require(type(value) is dict, 'release metadata must be an object, not an API error response')
    number(value.get('id'), 2**63-1, 'release ID')
    text(value.get('tag_name'), 'release tag', 200)
    require(type(value.get('draft')) is bool and type(value.get('prerelease')) is bool, 'release draft/prerelease flags must be booleans')
    date = value.get('published_at')
    if value['draft'] and date is None:
        pass
    else:
        require(type(date) is str and re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z', date) is not None, 'published release needs an exact UTC published_at timestamp')
        try:
            datetime.datetime.strptime(date, '%Y-%m-%dT%H:%M:%SZ')
        except ValueError:
            raise ContractError('published_at is not a real UTC date') from None
    assets = value.get('assets')
    require(type(assets) is list and len(assets) <= 1000, 'release assets must be a bounded list')
    names = set()
    for asset in assets:
        require(type(asset) is dict, 'release asset must be an object')
        name = text(asset.get('name'), 'metadata asset name', 255)
        require(name not in names, 'duplicate metadata asset filename')
        names.add(name)
        text(asset.get('state'), 'metadata asset state', 32)
    return value


def page(path):
    values = load_json(path, MAX_PAGE)
    require(type(values) is list and len(values) <= 100, 'release page must be a list with at most 100 entries')
    for value in values:
        metadata(value)
    return values


def selected(value):
    require(not value['draft'], 'selected release is a draft; choose a published release explicitly')
    tag = value['tag_name']
    try:
        version = normalize(tag)
        require(tag == 'v'+version, 'selected release tag must be exactly vVERSION')
    except ContractError as error:
        raise ContractError(f'selected release has an unsupported tag: {error}; no older release was selected') from None
    return version


def select(directory):
    directory = Path(directory)
    files = []
    for path in directory.glob('page-*.json'):
        match = re.fullmatch(r'page-([1-9][0-9]*)\.json', path.name)
        require(match is not None, 'unsupported pagination filename')
        files.append((int(match.group(1)), path))
    files.sort()
    require(0 < len(files) <= MAX_PAGES, 'release pagination is empty or exceeds the supported limit')
    require([n for n, _ in files] == list(range(1, len(files)+1)), 'release pagination is incomplete')
    releases = []
    ids, tags = set(), set()
    for index, (_, path) in enumerate(files):
        values = page(path)
        if index < len(files)-1:
            require(len(values) == 100, 'release pagination has data after a completed page')
        else:
            require(len(values) < 100, 'release pagination is incomplete; fetch its next page')
        for value in values:
            require(value['id'] not in ids and value['tag_name'] not in tags, 'duplicate/conflicting release identity across pagination; retry discovery')
            ids.add(value['id']); tags.add(value['tag_name'])
            if not value['draft']:
                releases.append(value)
    require(releases, 'no eligible published releases are available')
    # Compatibility is checked only after selecting one release; never skip unsupported newest data.
    result = max(releases, key=lambda value: (value['published_at'], value['id']))
    selected(result)
    return result


def explicit(path, requested):
    value = metadata(load_json(path, MAX_PAGE))
    version = normalize(requested)
    require(selected(value) == version and value['tag_name'] == 'v'+version, 'selected release tag does not match the explicit version')
    return value


def require_assets(path, names):
    value = metadata(load_json(path, MAX_PAGE))
    actual = {a['name']: a['state'] for a in value['assets']}
    for name in names:
        require(actual.get(name) == 'uploaded', f'selected release is missing uploaded asset {name}; no fallback is allowed')


def validate_contract(value, version):
    keys(value, {'format', 'repository', 'tag', 'version', 'platform', 'recipe', 'rust_version', 'source_commit', 'source_tree', 'source_manifest_format', 'assets'}, 'release envelope')
    version = normalize(version)
    require(value['format'] == 'mindspace-release-v1', 'selected release has an unsupported manifest format; update the installer, do not use an older release silently')
    require(value['repository'] == 'VikingOwl91/mindspace', 'release envelope repository identity mismatch')
    require(value['version'] == version and value['tag'] == 'v'+version, 'release envelope tag/version mismatch')
    require(value['platform'] == 'linux-x86_64', 'selected release platform is unsupported')
    require(value['recipe'] == 'cargo-source-no-embeddings-v1', 'selected release build recipe is unsupported')
    require(value['rust_version'] in ('1.98.1', '1.100.0'), 'selected release Rust toolchain is unsupported; update the installer')
    require(value['source_manifest_format'] == 'mindspace-source-archive-v2', 'selected source manifest format is unsupported')
    digest(value['source_commit'], 40, 'source commit'); digest(value['source_tree'], 40, 'source tree')
    keys(value['assets'], ROLES, 'release assets')
    names = set()
    for role in ROLES:
        asset = value['assets'][role]
        keys(asset, {'filename', 'bytes', 'sha256'}, f'{role} asset')
        name = safe_name(asset['filename'])
        require(name not in names, 'release asset filenames must be distinct')
        names.add(name)
        limit = MAX_ARCHIVE if role == 'archive' else MAX_MANIFEST if role == 'source_manifest' else 16_000
        number(asset['bytes'], limit, f'{role} bytes')
        digest(asset['sha256'], 64, f'{role} SHA-256')
    assets = value['assets']
    require(assets['archive']['filename'].endswith('.tar.gz'), 'supported source archive must declare a .tar.gz basename')
    require(assets['source_manifest']['filename'].endswith('.json'), 'supported source manifest must declare a .json basename')
    for payload, signature in [('archive', 'archive_signature'), ('source_manifest', 'source_manifest_signature')]:
        require(assets[signature]['filename'] == assets[payload]['filename']+'.asc', 'signature asset filename does not match its declared payload')
    return value


def contract_file(path, version):
    return validate_contract(load_json(path, MAX_ENVELOPE), version)


def write_contract(value, directory):
    directory = Path(directory); directory.mkdir(parents=True, exist_ok=True)
    a = value['assets']
    values = [a['archive']['filename'], a['archive']['sha256'], a['source_manifest']['filename'], value['source_commit'], value['rust_version'], value['source_tree']]
    (directory/'contract.values').write_text('\n'.join(values)+'\n', encoding='ascii')
    (directory/'assets.table').write_text(''.join(f"{a[role]['sha256']} {a[role]['bytes']} {a[role]['filename']}\n" for role in ROLES), encoding='ascii')


def file_hash(path, maximum):
    require(not Path(path).is_symlink() and Path(path).is_file(), f'{Path(path).name} is not a regular file')
    size = 0; result = hashlib.sha256()
    with Path(path).open('rb') as stream:
        while block := stream.read(65_536):
            size += len(block)
            require(size <= maximum, 'asset exceeds the supported byte limit')
            result.update(block)
    return size, result.hexdigest()


def member_path(value, root):
    text(value, 'archive member path', 255)
    require('\\' not in value and not value.startswith('/') and all(p not in ('', '.', '..') for p in value.split('/')), 'archive has unsafe member path')
    require(value == root or value.startswith(root+'/'), 'archive member escapes its expected source root')
    return value


def source(envelope, manifest_path, archive_path):
    value = load_json(envelope, MAX_ENVELOPE)
    require(type(value) is dict and type(value.get('version')) is str, 'release envelope version is missing')
    value = validate_contract(value, value['version'])
    manifest = load_json(manifest_path, MAX_MANIFEST)
    require(type(manifest) is dict, 'source manifest is not an object')
    require(manifest.get('format') == value['source_manifest_format'] and manifest.get('version') == value['version'], 'source manifest format/version mismatch')
    baseline = manifest.get('baseline')
    keys(baseline, {'commit', 'tree'}, 'source baseline')
    require(baseline['commit'] == value['source_commit'] and baseline['tree'] == value['source_tree'], 'source manifest commit/tree mismatch')
    archive = manifest.get('archive')
    keys(archive, {'filename', 'bytes', 'sha256', 'uncompressed_tar_bytes', 'uncompressed_tar_sha256'}, 'source archive metadata')
    a = value['assets']['archive']
    require(all(archive.get(key) == a[key] for key in ('filename', 'bytes', 'sha256')), 'source manifest archive identity mismatch')
    require(type(archive['bytes']) is int, 'source archive bytes must be an integer')
    raw_size = number(archive['uncompressed_tar_bytes'], MAX_TAR, 'uncompressed tar bytes')
    raw_sha = digest(archive['uncompressed_tar_sha256'], 64, 'uncompressed tar SHA-256')
    require(Path(archive_path).name == a['filename'], 'archive path basename disagrees with authenticated envelope')
    require(file_hash(archive_path, MAX_ARCHIVE) == (a['bytes'], a['sha256']), 'archive bytes/hash mismatch')
    ma = value['assets']['source_manifest']
    require(Path(manifest_path).name == ma['filename'] and file_hash(manifest_path, MAX_MANIFEST) == (ma['bytes'], ma['sha256']), 'source manifest bytes/hash mismatch')
    normalization = manifest.get('normalization')
    require(type(normalization) is dict and normalization.get('format') == 'ustar', 'source archive normalization format is unsupported')
    entries = manifest.get('members')
    require(type(entries) is list and 0 < len(entries) <= MAX_MEMBERS, 'source member count exceeds the supported limit')
    root = 'mindspace-'+value['version']
    declared = {}
    for entry in entries:
        require(type(entry) is dict, 'source member metadata must be an object')
        kind = entry.get('type')
        require(kind in ('file', 'directory'), 'source member has unsupported type')
        keys(entry, {'path', 'type', 'mode', 'bytes', 'sha256'} if kind == 'file' else {'path', 'type', 'mode', 'bytes'}, 'source member')
        name = member_path(entry['path'], root)
        require(name not in declared, 'duplicate source manifest member')
        number(entry['bytes'], MAX_TAR, 'source member bytes', minimum=0)
        require(entry['mode'] in (('0644', '0755') if kind == 'file' else ('0755',)), 'source member mode is unsupported')
        if kind == 'directory': require(entry['bytes'] == 0, 'directory member must have zero bytes')
        else: digest(entry['sha256'], 64, 'source member SHA-256')
        declared[name] = entry
    require(root in declared and declared[root]['type'] == 'directory', 'source manifest lacks its root directory')
    for name in declared:
        parent = name.rpartition('/')[0]
        while parent:
            require(parent in declared and declared[parent]['type'] == 'directory', 'source member has an undeclared/non-directory parent')
            parent = parent.rpartition('/')[0]
    # Parse raw USTAR headers directly: tarfile's high-level iterator hides PAX/GNU extension headers.
    total = 0; raw_digest = hashlib.sha256(); seen = set()
    recipe_paths = {root+'/Cargo.toml': MAX_MANIFEST, root+'/rust-toolchain.toml': 64_000}
    recipe_data = {}
    with gzip.open(archive_path, 'rb') as stream:
        def read(count):
            nonlocal total
            require(count <= 65_536, 'internal unsupported read size')
            data = stream.read(count); total += len(data)
            require(total <= raw_size and total <= MAX_TAR, 'decompressed archive exceeds its supported declared limit')
            raw_digest.update(data)
            return data
        while True:
            header = read(512)
            require(len(header) == 512, 'archive has truncated tar headers')
            if header == bytes(512):
                trailing = 0
                while block := read(65_536):
                    trailing += len(block)
                    require(not any(block), 'archive has nonzero data after its tar terminator')
                require(trailing >= 512 and trailing % 512 == 0, 'archive has incomplete tar terminator')
                break
            require(header[257:265] == b'ustar\x0000', 'archive has unsupported non-USTAR headers')
            try:
                member = tarfile.TarInfo.frombuf(header, 'utf-8', 'strict')
            except (tarfile.TarError, UnicodeError, ValueError):
                raise ContractError('archive has an invalid tar header') from None
            require(member.type in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE), 'archive links/special/PAX members are unsupported')
            name = member_path(member.name, root)
            require(name not in seen and len(seen) < MAX_MEMBERS, 'archive has duplicate or too many members')
            seen.add(name)
            require(name in declared, 'archive member is absent from the authenticated source manifest')
            entry = declared[name]
            kind = 'directory' if member.type == tarfile.DIRTYPE else 'file'
            require(kind == entry['type'] and member.size == entry['bytes'] and f'{member.mode:04o}' == entry['mode'], 'archive member type/size/mode mismatch')
            number(member.size, MAX_TAR, 'tar member bytes', minimum=0)
            capture = None
            if kind == 'file' and name in recipe_paths:
                require(member.size <= recipe_paths[name], 'build recipe metadata exceeds its byte limit')
                capture = bytearray()
            result = hashlib.sha256(); remaining = member.size
            while remaining:
                block = read(min(remaining, 65_536))
                require(block, 'archive member is truncated')
                remaining -= len(block); result.update(block)
                if capture is not None: capture.extend(block)
            if kind == 'file': require(result.hexdigest() == entry['sha256'], 'archive member SHA-256 mismatch')
            if capture is not None: recipe_data[name] = bytes(capture)
            padding = (-member.size) % 512
            require(read(padding) == bytes(padding), 'archive member padding is invalid')
    require(total == raw_size and raw_digest.hexdigest() == raw_sha, 'uncompressed tar bytes/hash mismatch')
    require(seen == set(declared), 'source manifest and archive member sets disagree')
    require(set(recipe_data) == set(recipe_paths), 'source archive lacks required build recipe metadata')
    validate_toml_bytes(recipe_data[root+'/Cargo.toml'], recipe_data[root+'/rust-toolchain.toml'], value['version'], value['rust_version'])
    return value


def validate_toml_bytes(package_data, toolchain_data, version, rust_version):
    require(rust_version in ('1.98.1', '1.100.0'), 'selected release Rust toolchain is unsupported')
    package = tomllib.loads(package_data.decode('utf-8'))
    toolchain = tomllib.loads(toolchain_data.decode('utf-8'))
    require(type(package.get('package')) is dict and package['package'].get('name') == 'mindspace' and package['package'].get('version') == normalize(version), 'Cargo package name/version differs from the selected release')
    require(type(toolchain.get('toolchain')) is dict and toolchain['toolchain'].get('channel') == rust_version, 'source Rust toolchain differs from the supported signed recipe')


def validate_toml(directory, version, rust_version):
    directory = Path(directory)
    validate_toml_bytes(read_bytes(directory/'Cargo.toml', MAX_MANIFEST), read_bytes(directory/'rust-toolchain.toml', 64_000), version, rust_version)


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    try:
        require(sys.version_info >= (3, 11), 'Python 3.11 or later is required')
        require(args, 'a release parser operation is required')
        operation, *args = args
        if operation == 'normalize' and len(args) == 1:
            print(normalize(args[0]))
        elif operation == 'page' and len(args) == 1:
            print(len(page(args[0])))
        elif operation in ('select', 'explicit') and len(args) == (2 if operation == 'select' else 3):
            value = select(args[0]) if operation == 'select' else explicit(args[0], args[1])
            Path(args[-1]).write_text(json.dumps(value, sort_keys=True)+'\n', encoding='utf-8')
            print(selected(value))
        elif operation == 'assets' and len(args) >= 2:
            require_assets(args[0], args[1:])
        elif operation == 'contract' and len(args) == 3:
            write_contract(contract_file(args[0], args[1]), args[2])
        elif operation == 'source' and len(args) == 3:
            source(*args)
        elif operation == 'toml' and len(args) == 3:
            validate_toml(*args)
        else:
            raise ContractError('unsupported release parser operation or argument count')
        return 0
    except (ContractError, OSError, ValueError, TypeError, KeyError, OverflowError, RecursionError, EOFError, zlib.error) as error:
        print(f'STOP: {error}. No fallback was attempted; inspect the selected release or update the installer.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
# END EMBEDDED RELEASE CONTRACT
MINDSPACE_RELEASE_PY
}

fetch() {
    curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --connect-timeout 15 --max-time 300 --max-filesize "${3:-5000000}" --output "$2" "$1"
}
sha_of() { sha256sum "$1" | awk '{print $1}'; }
ensure_work() {
    if [ -z "$WORK" ]; then
        WORK=$(mktemp -d "$TMP_BASE/mindspace-install.XXXXXX") || die "cannot create a temporary directory."
        chmod 700 "$WORK"
        mkdir "$WORK/assets" "$WORK/gnupg" "$WORK/src"
        chmod 700 "$WORK/gnupg"
    fi
}
gpgx() { gpg --no-options --homedir "$WORK/gnupg" --batch --no-tty --no-autostart --no-auto-key-retrieve "$@"; }
load_key() {
    ensure_work
    if [ -n "$KEY_FILE" ]; then
        [ -f "$KEY_FILE" ] || die "key file $KEY_FILE not found."
        cp -- "$KEY_FILE" "$WORK/publisher-key"
    else
        fetch "$WKD_URL" "$WORK/publisher-key" || die "could not fetch the publisher key. Retry or use --key-file with an independently obtained key."
    fi
    gpgx --import "$WORK/publisher-key" >/dev/null 2>&1 || die "the publisher key could not be imported."
    PRIMARY=$(gpgx --with-colons --fingerprint "$FPR" 2>/dev/null | awk -F: '$1 == "fpr" {print $10; exit}')
    [ "$PRIMARY" = "$FPR" ] || die "the key does not have the pinned fingerprint. It is not the publisher's key."
}
verify_signature() {
    sig=$1
    payload=$2
    status=$WORK/signature.status
    gpgx --assert-signer "$FPR" --status-fd 1 --verify "$sig" "$payload" >"$status" 2>/dev/null ||
        die "signature check failed for $payload. Do not use it; no fallback was attempted."
    if grep -Eq '^\[GNUPG:\] (BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|KEYEXPIRED|SIGEXPIRED|KEYREVOKED|NO_PUBKEY|FAILURE|ERROR)( |$)' "$status"; then
        die "the signature or key for $payload is invalid, expired, revoked or unavailable."
    fi
    grep -q '^\[GNUPG:\] GOODSIG ' "$status" || die "no good signature found for $payload."
    awk -v fpr="$FPR" '$1 == "[GNUPG:]" && $2 == "VALIDSIG" && $NF == fpr {ok = 1} END {exit ok ? 0 : 1}' "$status" ||
        die "$payload is not signed by the primary key $FPR."
}
get_assets() {
while read -r want_sum want_size name; do
    dest=$WORK/assets/$name
    if [ -n "$ASSETS_DIR" ]; then
        if ! [ -f "$ASSETS_DIR/$name" ] || [ -L "$ASSETS_DIR/$name" ]; then
            die "$ASSETS_DIR/$name is missing (or is a link). All seven assets are required."
        fi
        size=$(wc -c <"$ASSETS_DIR/$name" | tr -d ' ')
        [ "$size" = "$want_size" ] || die "$name has $size bytes, expected $want_size. Do not use it."
        cp -- "$ASSETS_DIR/$name" "$dest"
    else
        say "  downloading $name"
        fetch "$BASE_URL/$name" "$dest" "$want_size" || die "could not download selected release asset $BASE_URL/$name; no fallback was attempted."
    fi
    size=$(wc -c <"$dest" | tr -d ' ')
    [ "$size" = "$want_size" ] || die "$name has $size bytes, expected $want_size. Do not use it."
    got=$(sha_of "$dest")
    [ "$got" = "$want_sum" ] || die "$name does not match its pinned SHA-256 (got $got). Do not use it."
done <<EOF
$ASSET_TABLE
EOF
say "All required assets match their authenticated sizes and SHA-256 values."
ASSETS_READY=1
}

get_bootstrap() { # fixed supported bootstrap name, destination, bounded size
    if [ -n "$ASSETS_DIR" ]; then
        if [ ! -f "$ASSETS_DIR/$1" ] || [ -L "$ASSETS_DIR/$1" ]; then
            die "selected release $TAG: missing supported manifest asset $1. Nothing was replaced."
        fi
        size=$(wc -c <"$ASSETS_DIR/$1" | tr -d ' ')
        [ "$size" -le "$3" ] || die "selected release $TAG: $1 exceeds the supported size limit."
        cp -- "$ASSETS_DIR/$1" "$2"
    else
        fetch "$BASE_URL/$1" "$2" "$3" || die "selected release $TAG: cannot fetch supported manifest asset $1. Retry or explicitly select another version; no fallback was attempted."
    fi
}
select_release() {
    if [ -n "$REQUESTED_VERSION" ]; then
        REQUESTED_VERSION=$(release_json normalize "$REQUESTED_VERSION") || die "invalid --version. Use a supported semantic version."
    fi
    # This one historical, published release retains explicit offline verification.
    if [ "$REQUESTED_VERSION" = 0.1.0-alpha.1 ] && [ -n "$ASSETS_DIR" ]; then
        VERSION=$REQUESTED_VERSION
    else
        command -v curl >/dev/null 2>&1 || die "missing required tools: curl. Install it and retry."
        ensure_work
        if [ -n "$REQUESTED_VERSION" ]; then
            fetch "https://api.github.com/repos/VikingOwl91/mindspace/releases/tags/v$REQUESTED_VERSION" "$WORK/explicit.json" 4000000 ||
                die "cannot discover published release v$REQUESTED_VERSION. It may be unavailable, deleted or rate limited. Retry; no fallback was attempted."
            VERSION=$(release_json explicit "$WORK/explicit.json" "$REQUESTED_VERSION" "$WORK/selected.json") ||
                die "explicit release discovery failed. Nothing was replaced; no fallback was attempted."
        else
            page=1
            while :; do
                fetch "https://api.github.com/repos/VikingOwl91/mindspace/releases?per_page=100&page=$page" "$WORK/page-$page.json" 4000000 ||
                    die "release discovery page $page failed (network, deletion or rate limit). Retry; no fallback was attempted."
                count=$(release_json page "$WORK/page-$page.json") || die "invalid release discovery page $page. Nothing was replaced."
                [ "$count" = 100 ] || break
                [ "$page" -lt 100 ] || die "release discovery exceeds 100 pages. Cannot establish latest; no fallback was attempted."
                page=$((page + 1))
            done
            VERSION=$(release_json select "$WORK" "$WORK/selected.json") || die "cannot select the newest published release. Nothing was replaced; no fallback was attempted."
        fi
    fi
    TAG=v$VERSION
    BASE_URL=https://github.com/VikingOwl91/mindspace/releases/download/$TAG
    TOPDIR=mindspace-$VERSION
    PREFIX=$INSTALL_ROOT/$VERSION
    say "Selected published release: $TAG"
    if [ "$VERSION" = 0.1.0-alpha.1 ]; then
        if [ -f "$WORK/selected.json" ]; then
            while read -r want_sum want_size name; do
                release_json assets "$WORK/selected.json" "$name" || die "selected RC.5 release is missing required asset $name; no fallback was attempted."
            done <<EOF
$ASSET_TABLE
EOF
        fi
        return 0
    fi
    CONTRACT=envelope-v1
    release_json assets "$WORK/selected.json" mindspace-release.json mindspace-release.json.asc ||
        die "selected release $TAG has no supported signed manifest contract. Update the installer or explicitly select another version; no fallback was attempted."
    for c in gpg cp mkdir chmod; do
        command -v "$c" >/dev/null 2>&1 || die "missing required tools: $c. Install it and retry."
    done
    gpg --dump-options 2>/dev/null | grep -q -- '--assert-signer' || die "GnuPG must support --assert-signer. Update it; do not skip verification."
    confirm_trust
    get_bootstrap mindspace-release.json "$WORK/envelope.json" 65536
    get_bootstrap mindspace-release.json.asc "$WORK/envelope.json.asc" 16384
    load_key
    verify_signature "$WORK/envelope.json.asc" "$WORK/envelope.json"
    release_json contract "$WORK/envelope.json" "$VERSION" "$WORK" || die "selected release $TAG has an unsupported or invalid authenticated contract. Nothing was replaced; no fallback was attempted."
    {
        IFS= read -r ARCHIVE
        IFS= read -r ARCHIVE_SHA
        IFS= read -r MANIFEST
        IFS= read -r MANIFEST_COMMIT
        IFS= read -r RUST_VERSION
        IFS= read -r SOURCE_TREE
    } <"$WORK/contract.values"
    ASSET_TABLE=$(cat "$WORK/assets.table")
    say "Authenticated source: $MANIFEST_COMMIT (tree $SOURCE_TREE)"
    while read -r want_sum want_size name; do
        release_json assets "$WORK/selected.json" "$name" || die "selected release $TAG is missing declared asset $name; no fallback was attempted."
    done <<EOF
$ASSET_TABLE
EOF
    # A receipt cannot establish that a newly selected signed contract is internally consistent.
    # Authenticate every declared payload before accepting a future-version no-op.
    get_assets
    verify_signature "$WORK/assets/$ARCHIVE.asc" "$WORK/assets/$ARCHIVE"
    verify_signature "$WORK/assets/$MANIFEST.asc" "$WORK/assets/$MANIFEST"
    release_json source "$WORK/envelope.json" "$WORK/assets/$MANIFEST" "$WORK/assets/$ARCHIVE" ||
        die "selected release $TAG source manifest/archive mismatch. Nothing was replaced; no fallback was attempted."
}

# ---------------------------------------------------------------- arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            [ $# -ge 2 ] || die "--version needs a value"
            REQUESTED_VERSION=$2
            [ -n "$REQUESTED_VERSION" ] || die "--version needs a nonempty semantic version; no default was selected."
            shift 2
            ;;
        --trust-fingerprint)
            [ $# -ge 2 ] || die "--trust-fingerprint needs a value"
            TRUST_FPR=$2
            shift 2
            ;;
        --assets-dir)
            [ $# -ge 2 ] || die "--assets-dir needs a directory"
            ASSETS_DIR=$2
            shift 2
            ;;
        --key-file)
            [ $# -ge 2 ] || die "--key-file needs a file"
            KEY_FILE=$2
            shift 2
            ;;
        --verify-only) VERIFY_ONLY=1; shift ;;
        --upgrade) UPGRADE=1; shift ;;
        --reinstall) REINSTALL=1; shift ;;
        --allow-older) ALLOW_OLDER=1; shift ;;
        --keep-work) KEEP_WORK=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# ---------------------------------------------------------------- 1. this system
step "Checking this system"

[ "$(uname -s 2>/dev/null)" = Linux ] || die "this Alpha is qualified for Linux only (found $(uname -s 2>/dev/null || echo unknown))."
[ "$(uname -m 2>/dev/null)" = x86_64 ] || die "this Alpha is qualified for x86_64 only (found $(uname -m 2>/dev/null || echo unknown))."

case "${HOME:-}" in
    /*) ;;
    *) die "HOME must be set to an absolute path." ;;
esac
if ! [ -d "$HOME" ] || ! [ -w "$HOME" ]; then die "HOME ($HOME) is not a writable directory."; fi
INSTALL_ROOT=$HOME/.local/lib/mindspace
PREFIX=$INSTALL_ROOT/$VERSION

MISSING=
need_cmd() { command -v "$1" >/dev/null 2>&1 || MISSING="$MISSING $1"; }
for c in sha256sum awk grep tr cut mv date timeout python3 mktemp mkdir chmod wc; do need_cmd "$c"; done
if [ -n "$MISSING" ]; then
    die "missing required tools:$MISSING. Install them with your system's package manager, then run this again."
fi

# ---------------------------------------------------------------- 1b. an existing installation
# Compare two versions (MAJOR.MINOR.PATCH with an optional -prerelease, as in semantic versioning).
# Prints -1, 0 or 1. A prerelease is lower than the release; numeric identifiers compare as numbers.
ver_cmp() {
    awk -v a="$1" -v b="$2" '
    # Compare unbounded decimal strings without floating point rounding.
    function numcmp(a, b) {
        sub(/^0+/, "", a); sub(/^0+/, "", b)
        if (length(a) != length(b)) return length(a) < length(b) ? -1 : 1
        return "x" a == "x" b ? 0 : ("x" a < "x" b ? -1 : 1)
    }
    function parse(v, o,   i, core) {
        sub(/\+.*/, "", v)
        i = index(v, "-")
        o["has"] = i ? 1 : 0; o["pre"] = i ? substr(v, i + 1) : ""
        split(i ? substr(v, 1, i - 1) : v, core, ".")
        o[1] = core[1]; o[2] = core[2]; o[3] = core[3]
    }
    function cmp(a, b,   x, y, i, c, n, pa, pb, na, nb, ia, ib, ra, rb) {
        parse(a, x); parse(b, y)
        for (i = 1; i <= 3; i++) {c = numcmp(x[i], y[i]); if (c) return c}
        if (!x["has"] && !y["has"]) return 0
        if (!x["has"]) return 1
        if (!y["has"]) return -1
        na = split(x["pre"], pa, "."); nb = split(y["pre"], pb, ".")
        n = na < nb ? na : nb
        for (i = 1; i <= n; i++) {
            ia = pa[i]; ib = pb[i]
            ra = (ia ~ /^[0-9]+$/); rb = (ib ~ /^[0-9]+$/)
            if (ra && rb) {c = numcmp(ia, ib); if (c) return c}
            else if (ra) return -1
            else if (rb) return 1
            else if ("x" ia != "x" ib) return "x" ia < "x" ib ? -1 : 1
        }
        return na == nb ? 0 : (na < nb ? -1 : 1)
    }
    BEGIN {print cmp(a, b)}'
}

# Version names are SemVer, including hyphenated prerelease identifiers and build metadata.
is_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$' || return 1
    printf '%s\n' "$1" | awk '
        {sub(/\+.*/, ""); sub(/^[^-]*-?/, ""); n=split($0, a, ".")
         for (i=1; i<=n; i++) if (a[i] ~ /^0[0-9]+$/) exit 1}'
}

check_install_parents() {
    for parent in "$HOME/.local" "$HOME/.local/lib" "$INSTALL_ROOT"; do
        [ ! -L "$parent" ] || die "$parent is a symbolic link. Inspect this installation path before proceeding; nothing was replaced."
        if [ -e "$parent" ] && [ ! -d "$parent" ]; then
            die "$parent is not a directory. Inspect this installation path before proceeding."
        fi
    done
}

# Sets STATE to none, valid (intact, with an install record), legacy (runs and reports the right
# version but has no record) or invalid, and REASON for invalid. A directory that merely exists is
# never treated as an installation. The record is a consistency check against accidental damage; it
# is not protection against someone who can already write to your home directory.
inspect_existing() { # PREFIX EXPECTED_VERSION; never execute a binary with a mismatching receipt.
    iprefix=$1
    expected=$2
    STATE=none
    REASON=
    REPAIRABLE=1
    if ! [ -e "$iprefix" ] && ! [ -L "$iprefix" ]; then return 0; fi
    STATE=invalid
    if [ -L "$iprefix" ]; then REASON="it is a symbolic link"; return 0; fi
    if ! [ -d "$iprefix" ]; then REASON="it is not a directory"; return 0; fi
    ibin=$iprefix/bin/mind
    # Check for data before any repairable layout/binary failure returns.
    # Never follow a linked bin directory to perform this inspection.
    for directory in "$iprefix" "$iprefix/bin"; do
        if [ "$directory" != "$iprefix" ]; then
            if [ ! -d "$directory" ] || [ -L "$directory" ]; then continue; fi
        fi
        for entry in "$directory"/* "$directory"/.[!.]* "$directory"/..?*; do
            [ -e "$entry" ] || [ -L "$entry" ] || continue
            case "$entry" in
                "$ibin" | "$iprefix/install-record")
                    if [ -d "$entry" ] && [ ! -L "$entry" ]; then
                        REASON="unexpected directory $entry; inspect and back up its contents before repair"
                        REPAIRABLE=0
                        return 0
                    fi
                    continue ;;
                "$iprefix/bin") continue ;;
            esac
            REASON="unexpected entry $entry; inspect and back up your data before repairing this prefix"
            REPAIRABLE=0
            return 0
        done
    done
    if [ -L "$iprefix/bin" ] || ! [ -d "$iprefix/bin" ]; then
        REASON="bin is not a real directory; bin/mind is missing or is not a regular file"; return 0
    fi
    if [ -L "$ibin" ] || ! [ -f "$ibin" ]; then REASON="bin/mind is missing or is not a regular file"; return 0; fi
    if ! [ -x "$ibin" ]; then REASON="bin/mind is not executable"; return 0; fi
    irec=$iprefix/install-record
    if [ -L "$irec" ]; then REASON="install-record is a symbolic link"; return 0; fi
    has_record=0
    if [ -e "$irec" ]; then
        if ! [ -f "$irec" ]; then REASON="install-record is not a regular file"; return 0; fi
        # Read as data, never source a receipt; reject duplicates, missing and unknown fields.
        if ! awk '
            NR == 1 {if ($0 != "mindspace-install-record 1") bad=1; next}
            {i=index($0, "="); k=substr($0, 1, i-1); v=substr($0, i+1)
             if (!i || !length(v) || index(v, "=") || seen[k]++) bad=1
             if (k == "installed_at" && v !~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z$/) bad=1
             if (k !~ /^(version|archive_sha256|binary_sha256|source_commit|publisher_fingerprint|installed_at)$/) bad=1}
            END {if (NR != 7) bad=1; exit bad ? 1 : 0}' "$irec"; then
            REASON="install-record has an unknown format"; return 0
        fi
        rv=$(awk -F= '$1 == "version" {print $2}' "$irec")
        ra=$(awk -F= '$1 == "archive_sha256" {print $2}' "$irec")
        rb=$(awk -F= '$1 == "binary_sha256" {print $2}' "$irec")
        rf=$(awk -F= '$1 == "publisher_fingerprint" {print $2}' "$irec")
        rc=$(awk -F= '$1 == "source_commit" {print $2}' "$irec")
        if [ "$rv" != "$expected" ] || [ "$rf" != "$FPR" ] ||
            { [ "$expected" = "$VERSION" ] && { [ "$ra" != "$ARCHIVE_SHA" ] || [ "$rc" != "$MANIFEST_COMMIT" ]; }; }; then
            REASON="install-record describes a different version or build"; return 0
        fi
        if ! printf '%s\n' "$ra" | grep -Eq '^[0-9a-f]{64}$' ||
            ! printf '%s\n' "$rc" | grep -Eq '^[0-9a-f]{40}$'; then
            REASON="install-record has an unknown format"; return 0
        fi
        if [ "$rb" != "$(sha256sum "$ibin" | awk '{print $1}')" ]; then
            REASON="bin/mind no longer matches its install record (modified or damaged)"; return 0
        fi
        has_record=1
    fi
    igot=$(timeout -k 1 10 "$ibin" --version 2>/dev/null) || igot=
    if [ "$igot" != "mind $expected" ]; then
        REASON="bin/mind does not run, or reports '${igot:-nothing}' instead of 'mind $expected'"
        return 0
    fi
    STATE=legacy
    [ "$has_record" = 0 ] || STATE=valid
}

check_existing() {
    check_install_parents
    inspect_existing "$PREFIX" "$VERSION"
    if [ "$CONTRACT" != rc5 ] && [ "$STATE" = legacy ]; then
        STATE=invalid
        REASON="no install record binds this binary to the authenticated release; use --reinstall for a verified replacement"
    fi
    if [ "$REINSTALL" != 1 ]; then
        case "$STATE" in
            valid | legacy)
                say "Already up to date: mind $VERSION runs at $PREFIX/bin/mind. Nothing was changed."
                if [ "$STATE" = legacy ]; then
                    say "It has no install record: legacy validation checks its layout and reported version only."
                    say "Its build authenticity cannot be re-established offline. For a freshly verified build, run: sh install.sh --reinstall"
                else
                    say "It matches its install record (a local consistency check, not independent authentication)."
                fi
                exit 0
                ;;
            invalid)
                say "$PREFIX is not a valid installation: $REASON." >&2
                say "Nothing was replaced. Inspect and back up this prefix first." >&2
                say "For a prefix containing only installer files, run: sh install.sh --reinstall" >&2
                say "The previous installation is kept under $INSTALL_ROOT/previous/, not deleted." >&2
                exit 1
                ;;
        esac
    fi
    NEWER=
    OLDER=
    # Validate sibling installations too; names alone cannot authorize a version transition.
    for d in "$INSTALL_ROOT"/*; do
        [ -e "$d" ] || [ -L "$d" ] || continue
        n=${d##*/}
        is_version "$n" || continue
        [ "$n" = "$VERSION" ] && continue
        inspect_existing "$d" "$n"
        [ "$STATE" != invalid ] || die "$d is not a valid installation: $REASON. Inspect and back up that prefix before retrying; nothing was replaced."
        if [ "$(ver_cmp "$n" "$VERSION")" = 1 ]; then NEWER="$NEWER $n"; else OLDER="$OLDER $n"; fi
    done
    inspect_existing "$PREFIX" "$VERSION"
    [ "$REPAIRABLE" = 1 ] || die "$PREFIX is not a valid installation: $REASON. Nothing was replaced."
    [ -z "$NEWER" ] || say "Newer versions installed:$NEWER"
    [ -z "$OLDER" ] || say "Older versions installed:$OLDER"
    if [ -n "$NEWER" ] && [ "$ALLOW_OLDER" != 1 ]; then
        die "a newer version is already installed ($NEWER). Nothing was replaced. Obtain the current installer, or use --allow-older to install $VERSION beside it; PATH and MCP registrations remain unchanged."
    fi
    if [ -n "$OLDER" ] && [ "$UPGRADE" != 1 ] && [ "$STATE" = none ]; then
        die "an older installation exists ($OLDER). Run sh install.sh --upgrade to install $VERSION beside it. Previous binaries, PATH and MCP registrations are preserved."
    fi
    if [ "$REINSTALL" = 1 ] && [ "$STATE" != none ]; then
        say "--reinstall: after staging and verification the existing installation is moved to $INSTALL_ROOT/previous/, not deleted."
    fi
}

# END INSTALLATION INSPECTION HELPERS
python3 -I -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' || die "Python 3.11 or later is required. Install it and retry."
TMP_BASE=${TMPDIR:-/tmp}
if ! [ -d "$TMP_BASE" ] || ! [ -w "$TMP_BASE" ]; then die "temporary directory $TMP_BASE is not writable (set TMPDIR)."; fi
TMP_BASE=$(cd "$TMP_BASE" && pwd -P) || die "cannot resolve the temporary directory $TMP_BASE."
select_release
if [ "$VERIFY_ONLY" != 1 ]; then check_existing; fi

# A relative TMPDIR would break the build; resolve it. Cargo reads a .cargo/config.toml from every
# parent directory of the build, and anybody who can write one of them (for example /tmp/.cargo on a
# shared machine) could make the build run their code. Refuse to build below such a directory.
TMP_BASE=${TMPDIR:-/tmp}
if ! [ -d "$TMP_BASE" ] || ! [ -w "$TMP_BASE" ]; then die "temporary directory $TMP_BASE is not writable (set TMPDIR)."; fi
TMP_BASE=$(cd "$TMP_BASE" && pwd -P) || die "cannot resolve the temporary directory $TMP_BASE."
if [ "$VERIFY_ONLY" != 1 ]; then
    d=$TMP_BASE
    while :; do
        for f in "$d/.cargo/config.toml" "$d/.cargo/config"; do
            if [ -e "$f" ] || [ -L "$f" ]; then
                die "Cargo would read $f while building here. Set TMPDIR to a private directory that has no .cargo/config.toml in it or above it, for example TMPDIR=\"\$HOME/tmp-install\" (create it first, mode 700)."
            fi
        done
        [ "$d" = / ] && break
        d=${d%/*}
        [ -n "$d" ] || d=/
    done
fi

MISSING=
for c in tar gzip gpg sed cmp cp mktemp install df wc; do need_cmd "$c"; done
[ -n "$ASSETS_DIR" ] || need_cmd curl
if [ "$VERIFY_ONLY" != 1 ]; then
    for c in setsid env tail sleep flock; do need_cmd "$c"; done
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        MISSING="$MISSING cc(C-compiler)"
    fi
    if command -v rustup >/dev/null 2>&1; then
        :
    else
        need_cmd cargo
        need_cmd rustc
    fi
fi
if [ -n "$MISSING" ]; then
    die "missing required tools:$MISSING. Install them with your system's package manager, then run this again."
fi
gpg --dump-options 2>/dev/null | grep -q -- '--assert-signer' ||
    die "your GnuPG is too old: it must support --assert-signer. Update GnuPG; do not skip the check."

# Use the exact supported toolchain from the authenticated contract, never "stable" or main.
CARGO_RUN=
if [ "$VERIFY_ONLY" != 1 ]; then
    if command -v rustup >/dev/null 2>&1; then
        rustup toolchain list 2>/dev/null | grep -q "^$RUST_VERSION-" ||
            die "Rust $RUST_VERSION is not installed. Run: rustup toolchain install $RUST_VERSION --profile minimal"
        RUSTUP_AUTO_INSTALL=0 rustup run "$RUST_VERSION" rustc --version 2>/dev/null | grep -q "^rustc $RUST_VERSION " ||
            die "rustup could not run Rust $RUST_VERSION."
        CARGO_RUN=rustup
    else
        if rustc --version 2>/dev/null | grep -q "^rustc $RUST_VERSION " &&
            cargo --version 2>/dev/null | grep -q "^cargo $RUST_VERSION "; then
            CARGO_RUN=plain
        else
            die "this Alpha builds with Rust $RUST_VERSION exactly; found: $(rustc --version 2>/dev/null || echo none). Install rustup and run: rustup toolchain install $RUST_VERSION --profile minimal"
        fi
    fi
fi

if [ "$VERIFY_ONLY" != 1 ]; then
    AVAIL=$(df -Pk "$TMP_BASE" 2>/dev/null | awk 'NR==2 {print $4}')
    case "$AVAIL" in
        '' | *[!0-9]*) die "cannot determine the free space in $TMP_BASE (set TMPDIR to a directory on a disk with about 2 GiB free)." ;;
    esac
    [ "$AVAIL" -ge "$NEED_KIB" ] || die "the build needs about 2 GiB of free space in $TMP_BASE; found $((AVAIL / 1024)) MiB. Set TMPDIR to a larger directory."
fi
say "Linux x86_64: ok. Tools: ok. Install prefix: $PREFIX"

# ---------------------------------------------------------------- 2. publisher trust decision
confirm_trust

# ---------------------------------------------------------------- 3. assets
ensure_work
step "Getting the authenticated release assets"

if [ "$ASSETS_READY" = 0 ]; then get_assets; fi

# ---------------------------------------------------------------- 4. signatures
step "Verifying the signatures"

load_key
if [ "$CONTRACT" = rc5 ]; then
    # The bundled key is consistency data, never the publisher-identity authority.
    BUNDLED=$(gpg --no-options --homedir "$WORK/gnupg" --batch --no-tty --no-autostart --show-keys --with-colons "$WORK/assets/mindspace-release-signing-key.asc" 2>/dev/null |
        awk -F: '$1 == "fpr" {print $10; exit}')
    [ "$BUNDLED" = "$FPR" ] || die "the key file inside the release has a different fingerprint ($BUNDLED). Do not use this release."
fi
for f in "$ARCHIVE" "$MANIFEST"; do
    verify_signature "$WORK/assets/$f.asc" "$WORK/assets/$f"
    say "  $f: good signature by $FPR"
done

if [ "$CONTRACT" = rc5 ]; then
# The two pinned checksums must also be what the (pinned, unsigned) SHA256SUMS file says.
printf '%s\n' \
    "$ARCHIVE_SHA  $ARCHIVE" \
    "903d8fd226593b74083bc8c0a11b7ffafc520eae06b5f054d0565ad1f5dae391  $MANIFEST" >"$WORK/expected-SHA256SUMS"
cmp -s "$WORK/expected-SHA256SUMS" "$WORK/assets/SHA256SUMS" || die "SHA256SUMS differs from the pinned values."
(cd "$WORK/assets" && sha256sum --check --strict SHA256SUMS >/dev/null) || die "sha256sum --check failed."
# The signed manifest must describe exactly this archive and the expected source commit.
grep -qF "\"sha256\": \"$ARCHIVE_SHA\"" "$WORK/assets/$MANIFEST" || die "the signed manifest does not describe this archive."
grep -qF "\"commit\": \"$MANIFEST_COMMIT\"" "$WORK/assets/$MANIFEST" || die "the signed manifest names an unexpected source commit."
else
    release_json source "$WORK/envelope.json" "$WORK/assets/$MANIFEST" "$WORK/assets/$ARCHIVE" ||
        die "selected release $TAG source manifest/archive mismatch. Nothing was replaced; no fallback was attempted."
fi
say "Authenticated: publisher signatures and declared artifact identities agree."

if [ "$VERIFY_ONLY" = 1 ]; then
    say ""
    say "--verify-only: stopping here. Nothing was built or installed."
    exit 0
fi

# ---------------------------------------------------------------- 5. unpack safely
step "Unpacking the verified archive"

tar -tzf "$WORK/assets/$ARCHIVE" >"$WORK/names" 2>/dev/null || die "the archive cannot be listed."
tar -tzvf "$WORK/assets/$ARCHIVE" 2>/dev/null | cut -c1 >"$WORK/types" || die "the archive cannot be listed."
[ "$(wc -l <"$WORK/names")" = "$(wc -l <"$WORK/types")" ] || die "the archive listing is inconsistent."
awk -v top="$TOPDIR" '
    $0 == top || $0 == top "/" {next}
    index($0, top "/") != 1 {bad = 1}
    $0 ~ /(^|\/)\.\.(\/|$)/ {bad = 1}
    END {exit bad ? 1 : 0}' "$WORK/names" || die "the archive contains paths outside $TOPDIR/."
if grep -qv '^[-d]$' "$WORK/types"; then
    die "the archive contains links or special files, which this release does not have."
fi
tar -xzf "$WORK/assets/$ARCHIVE" -C "$WORK/src" --no-same-owner --no-same-permissions || die "extraction failed."
SRC=$WORK/src/$TOPDIR
if ! [ -f "$SRC/Cargo.toml" ] || ! [ -f "$SRC/Cargo.lock" ]; then die "the archive does not look like the Mindspace source."; fi
if [ "$CONTRACT" = rc5 ]; then
    grep -q "^version = \"$VERSION\"" "$SRC/Cargo.toml" || die "the archive's version is not $VERSION."
    grep -q "^channel = \"$RUST_VERSION\"" "$SRC/rust-toolchain.toml" || die "the archive pins a different Rust toolchain than $RUST_VERSION."
else
    release_json toml "$SRC" "$VERSION" "$RUST_VERSION" || die "the authenticated source does not match its supported build recipe."
fi
say "Unpacked to a private temporary directory."

# ---------------------------------------------------------------- 6. build
step "Building with Rust $RUST_VERSION (a few minutes; Cargo downloads the locked dependencies into the temporary directory)"

(
    cd "$SRC"
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS RUSTDOCFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTC CARGO_BUILD_TARGET CARGO_PROFILE_RELEASE_LTO
    # An empty value overrides a rustc wrapper (for example sccache) set in a Cargo config file.
    export CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=''
    # A private Cargo home: your ~/.cargo (its configuration, registry and cache) is neither read
    # nor written; the locked dependencies are downloaded into the temporary directory.
    unset CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR
    mkdir -p "$WORK/cargo-home"
    export CARGO_HOME="$WORK/cargo-home"
    if [ "$CARGO_RUN" = rustup ]; then
        set -- env RUSTUP_AUTO_INSTALL=0 RUSTUP_TOOLCHAIN="$RUST_VERSION" cargo build --locked --release --no-default-features --bin mind --target-dir "$WORK/target"
    else
        set -- cargo build --locked --release --no-default-features --bin mind --target-dir "$WORK/target"
    fi
    # Its own session and process group (its id is written to build.pgid), so an interruption can stop
    # the whole build tree: cargo, rustc, build scripts and the C compiler.
    # shellcheck disable=SC2016  # the inner shell, not this one, expands $$ and "$@"
    exec setsid -w sh -c 'echo "$$" >"$0"; exec "$@"' "$WORK/build.pgid" "$@"
) >"$WORK/build.log" 2>&1 &
BUILD_PID=$!
if wait "$BUILD_PID"; then
    BUILD_PID=
else
    BUILD_PID=
    say "The last lines of the build log:" >&2
    tail -n 25 "$WORK/build.log" >&2 || true
    die "the build failed. Nothing was installed."
fi
BIN=$WORK/target/release/mind
[ -x "$BIN" ] || die "the build did not produce a mind binary."
GOT_VERSION=$("$BIN" --version 2>/dev/null) || die "the built binary does not run."
[ "$GOT_VERSION" = "mind $VERSION" ] || die "the built binary reports '$GOT_VERSION', expected 'mind $VERSION'."
say "Built: $GOT_VERSION"

# ---------------------------------------------------------------- 7. install
step "Installing"

# Lock the real directory inode, avoiding a lock file on the read-only repeat path.
check_install_parents
(umask 022 && mkdir -p "$INSTALL_ROOT") || die "cannot create $INSTALL_ROOT."
exec 9<"$INSTALL_ROOT"
flock -n 9 || die "another installer is publishing into $INSTALL_ROOT. Let it finish, then retry."
# Repeat all checks under the publication lock: an install may have appeared during the build.
check_existing
STAGED=$(mktemp -d "$INSTALL_ROOT/.install.XXXXXX") || die "cannot stage the installation."
mkdir -m 700 "$STAGED/bin"
install -m 0755 "$BIN" "$STAGED/bin/mind" || die "cannot stage the binary; the previous installation is untouched."
[ "$("$STAGED/bin/mind" --version 2>/dev/null)" = "mind $VERSION" ] || die "the staged binary does not report 'mind $VERSION'."
INSTALLED_SHA=$(sha_of "$STAGED/bin/mind")
[ "$INSTALLED_SHA" = "$(sha_of "$BIN")" ] || die "the staged binary differs from the verified build."
{
    printf 'mindspace-install-record 1\n'
    printf 'version=%s\n' "$VERSION"
    printf 'binary_sha256=%s\n' "$INSTALLED_SHA"
    printf 'archive_sha256=%s\n' "$ARCHIVE_SHA"
    printf 'source_commit=%s\n' "$MANIFEST_COMMIT"
    printf 'publisher_fingerprint=%s\n' "$FPR"
    printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$STAGED/install-record" || die "cannot write the install record."
chmod 600 "$STAGED/install-record"
# Staging can take time: refuse to move data added to the existing prefix meanwhile.
if [ "$REINSTALL" = 1 ]; then check_existing; fi
if [ "$REINSTALL" = 1 ] && [ "$STATE" != none ]; then
    BACKUP_DIR=$INSTALL_ROOT/previous
    if [ -L "$BACKUP_DIR" ] || { [ -e "$BACKUP_DIR" ] && [ ! -d "$BACKUP_DIR" ]; }; then
        die "$BACKUP_DIR is not a real directory. Inspect it before retrying; nothing was replaced."
    fi
    if [ ! -d "$BACKUP_DIR" ]; then mkdir -m 700 "$BACKUP_DIR"; fi
    BACKUP_HOLDER=$(mktemp -d "$BACKUP_DIR/$VERSION.XXXXXX") || die "cannot reserve a backup directory."
    BACKUP=$BACKUP_HOLDER/install
    # Set intent first so cleanup handles a signal delivered immediately after the rename.
    MOVED_OLD=1
    mv -T -- "$PREFIX" "$BACKUP" || die "cannot move the existing installation aside."
    say "The previous installation is kept at $BACKUP."
fi
if [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; then die "$PREFIX appeared meanwhile; it was left untouched."; fi
# -T prevents nesting a replacement inside an unexpectedly created destination directory.
if ! mv -T --no-clobber "$STAGED" "$PREFIX"; then
    # Coreutils versions differ on the exit status of a skipped no-clobber rename.
    if [ -d "$STAGED" ] && { [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; }; then
        die "$PREFIX appeared meanwhile; it was left untouched."
    fi
    die "cannot publish the staged installation."
fi
[ ! -d "$STAGED" ] || die "$PREFIX appeared meanwhile; it was left untouched."
INSTALL_OK=1
STAGED=
flock -u 9

cat <<EOF

Installed and verified.

  binary   $PREFIX/bin/mind
  version  mind $VERSION
  sha256   $INSTALLED_SHA

Nothing else was changed: no PATH entry, no shell startup file, no agent configuration, no database.
${BACKUP:+The previous installation is kept at $BACKUP.
}
Next steps
  1. Try it without changing your shell (works in sh, bash, zsh and fish):
       $PREFIX/bin/mind --version
       $PREFIX/bin/mind --db "\$HOME/mindspace-demo/mind.db" init
  2. Optional: put it on your PATH yourself.
       bash/zsh:  export PATH="$PREFIX/bin:\$PATH"
       fish:      fish_add_path $PREFIX/bin
  3. Follow the quickstart: https://github.com/VikingOwl91/mindspace#quickstart
  4. Connect an agent (shows a preview first, changes nothing until you add --apply):
       $PREFIX/bin/mind setup codex
       $PREFIX/bin/mind setup claude-code

To remove it later: rm -- "$PREFIX/bin/mind" "$PREFIX/install-record" && rmdir "$PREFIX/bin" "$PREFIX"
Your databases live elsewhere and are not removed.
EOF
