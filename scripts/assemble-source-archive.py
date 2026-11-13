#!/usr/bin/env python3
"""Build the local source-only candidate from versioned, explicitly selected Git blobs."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import tarfile

ALLOWLIST = 'release/source-files.txt'
ASSEMBLER = 'scripts/assemble-source-archive.py'
VERSION = '0.1.0-alpha.1'
PREFIX = 'mindspace-' + VERSION
FILENAME = PREFIX + '-rc.4-source.tar.gz'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def safe_path(name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or str(path) != name
            or any(c < ' ' or c > '~' for c in name) or '\\' in name
            or any(p in {'.', '..', '.git', '__pycache__', 'target'} for p in path.parts)):
        raise ValueError('unsafe source path')
    return path


def archive_bytes(payload, epoch):
    """payload maps relative names to (bytes, Git mode); no filesystem metadata."""
    directories = {PREFIX}
    members = []
    for name, (data, mode) in sorted(payload.items()):
        full = PREFIX + '/' + name
        parts = safe_path(full).parts
        if mode not in {'100644', '100755'}:
            raise ValueError('only regular Git blobs may be packaged')
        directories.update('/'.join(parts[:i]) for i in range(1, len(parts)))
        members.append(dict(path=full, type='file', mode='0' + mode[-3:],
                            bytes=len(data), sha256=sha(data)))
    members += [dict(path=p, type='directory', mode='0755', bytes=0) for p in directories]
    members.sort(key=lambda e: e['path'])
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w', format=tarfile.USTAR_FORMAT) as tar:
        for entry in members:
            info = tarfile.TarInfo(entry['path'])
            info.uid = info.gid = 0
            info.uname = info.gname = ''
            info.mtime = epoch
            info.mode = int(entry['mode'], 8)
            if entry['type'] == 'directory':
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                data = payload[entry['path'][len(PREFIX) + 1:]][0]
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode='wb', filename='', mtime=0, compresslevel=9) as out:
        out.write(raw.getvalue())
    return compressed.getvalue(), raw.getvalue(), members


def validate_archive(archive, manifest):
    """Verify full inventory and normalized metadata before any extraction."""
    identity = manifest['archive']
    if len(archive) != identity['bytes'] or sha(archive) != identity['sha256']:
        raise ValueError('archive checksum/size mismatch')
    raw = gzip.decompress(archive)
    if len(raw) != identity['uncompressed_tar_bytes'] or sha(raw) != identity['uncompressed_tar_sha256']:
        raise ValueError('tar checksum/size mismatch')
    expected = {e['path']: e for e in manifest['members']}
    if len(expected) != len(manifest['members']):
        raise ValueError('duplicate manifest path')
    seen = set()
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as tar:
        for member in tar:
            safe_path(member.name)
            if member.name in seen or member.name not in expected:
                raise ValueError('unexpected/duplicate archive path')
            seen.add(member.name)
            entry = expected[member.name]
            if (member.pax_headers or member.linkname or member.uid or member.gid
                    or member.uname or member.gname or member.mode != int(entry['mode'], 8)
                    or member.mtime != manifest['normalization']['tar_mtime_epoch']):
                raise ValueError('non-normalized archive metadata')
            if entry['type'] == 'directory':
                if member.type != tarfile.DIRTYPE or member.size:
                    raise ValueError('unexpected directory type/size')
            elif entry['type'] == 'file':
                if member.type != tarfile.REGTYPE or member.size != entry['bytes']:
                    raise ValueError('unexpected file type/size')
                if sha(tar.extractfile(member).read()) != entry['sha256']:
                    raise ValueError('member checksum mismatch')
            else:
                raise ValueError('unexpected manifest type')
    if seen != set(expected):
        raise ValueError('missing archive member')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path('.'))
    parser.add_argument('--ref', default='HEAD')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()

    def git(*argv):
        return subprocess.check_output(['git', '--no-replace-objects', *argv], cwd=args.repo)

    if git('diff', '--name-only') or git('diff', '--cached', '--name-only'):
        raise ValueError('tracked worktree and index must be clean')
    commit = git('rev-parse', '--verify', args.ref + '^{commit}').decode().strip()
    tree = git('rev-parse', commit + '^{tree}').decode().strip()
    epoch = int(git('show', '-s', '--format=%ct', commit))
    tracked = {}
    for raw in git('ls-tree', '-rz', commit).split(b'\0'):
        if raw:
            metadata, name = raw.split(b'\t', 1)
            mode, kind, oid = metadata.decode().split()
            tracked[name.decode()] = (mode, kind, oid)

    def blob(name):
        safe_path(name)
        mode, kind, oid = tracked[name]
        if mode not in {'100644', '100755'} or kind != 'blob':
            raise ValueError('only regular Git blobs may be selected')
        return git('cat-file', 'blob', oid), mode

    if blob(ASSEMBLER)[0] != Path(__file__).read_bytes():
        raise ValueError('running assembler must match selected source commit')
    names = blob(ALLOWLIST)[0].decode('ascii').splitlines()
    if names != sorted(set(names)) or not {ALLOWLIST, ASSEMBLER} <= set(names):
        raise ValueError('allowlist must be sorted, unique and include itself and assembler')
    payload = {name: blob(name) for name in names}
    archive, raw, members = archive_bytes(payload, epoch)
    manifest = dict(
        format='mindspace-source-archive-v2', version=VERSION, candidate='rc.4-fable-remediation',
        publication='local qualification only; publication not authorized',
        baseline=dict(commit=commit, tree=tree),
        archive=dict(filename=FILENAME, bytes=len(archive), sha256=sha(archive),
                     uncompressed_tar_bytes=len(raw), uncompressed_tar_sha256=sha(raw)),
        normalization=dict(format='ustar', tar_mtime_epoch=epoch, gzip_mtime=0,
                           gzip_filename='', gzip_level=9, uid=0, gid=0),
        assembler_sha256=sha(payload[ASSEMBLER][0]),
        allowlist_sha256=sha(payload[ALLOWLIST][0]), members=members,
        source='Every member is an exact regular Git blob from baseline.commit; no overlays.',
    )
    validate_archive(archive, manifest)
    manifest_bytes = encoded(manifest)
    # New output only. Never overwrite a historical candidate or sidecar.
    args.output.mkdir(mode=0o700)
    files = {FILENAME: archive, 'source-archive-manifest.json': manifest_bytes}
    files['SHA256SUMS'] = ''.join(f'{sha(data)}  {name}\n' for name, data in files.items()).encode()
    for name, data in files.items():
        fd = os.open(args.output / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as out:
            out.write(data)
    print(json.dumps(dict(source_commit=commit, source_tree=tree, files=len(payload),
                          archive_sha256=sha(archive), manifest_sha256=sha(manifest_bytes))))


if __name__ == '__main__':
    main()
