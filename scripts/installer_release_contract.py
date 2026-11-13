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
