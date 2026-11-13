#!/usr/bin/env python3
"""Focused construction/validation checks; clean-checkout proof is a release gate."""
import copy
import gzip
import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
import tarfile
import unittest

spec = importlib.util.spec_from_file_location('assembler', Path(__file__).with_name('assemble-source-archive.py'))
a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)


class SourceArchiveTests(unittest.TestCase):
    def setUp(self):
        self.payload = {'Cargo.toml': (b'[package]\n', '100644'),
                        'scripts/check.sh': (b'#!/bin/sh\n', '100755')}
        self.archive, raw, members = a.archive_bytes(self.payload, 1234)
        self.manifest = dict(archive=dict(bytes=len(self.archive), sha256=a.sha(self.archive),
            uncompressed_tar_bytes=len(raw), uncompressed_tar_sha256=a.sha(raw)),
            normalization=dict(tar_mtime_epoch=1234), members=members)

    def test_deterministic_complete_payload(self):
        again, _, _ = a.archive_bytes(dict(reversed(list(self.payload.items()))), 1234)
        self.assertEqual(self.archive, again)
        a.validate_archive(self.archive, self.manifest)
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(self.archive))) as tar:
            for name, (data, mode) in self.payload.items():
                entry = tar.getmember(a.PREFIX + '/' + name)
                self.assertEqual(tar.extractfile(entry).read(), data)
                self.assertEqual(entry.mode, int(mode[-3:], 8))

    def test_paths_and_non_regular_git_modes_rejected(self):
        for name in ['', '/absolute', '../outside', 'a/../b', 'a//b', 'a/./b',
                     'a\\b', '.git/config', 'target/debug/mind', 'a\x1b']:
            with self.subTest(name=name), self.assertRaises(ValueError):
                a.safe_path(name)
        with self.assertRaises(ValueError):
            a.archive_bytes({'link': (b'destination', '120000')}, 1234)

    def test_local_replacement_refs_cannot_change_commit_payload(self):
        with tempfile.TemporaryDirectory(prefix='mindspace-archive-test-') as directory:
            root = Path(directory)
            repo = root / 'repo'
            repo.mkdir()
            env = dict(os.environ, GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')

            def git(*args, data=None):
                return subprocess.check_output(['git', '-C', str(repo), *args], input=data, env=env)

            git('init', '-q')
            (repo / 'scripts').mkdir()
            (repo / 'release').mkdir()
            original = b'Versioned source.\n'
            (repo / 'README.md').write_bytes(original)
            shutil.copyfile(Path(a.__file__), repo / a.ASSEMBLER)
            (repo / a.ALLOWLIST).write_text('\n'.join(sorted(['README.md', a.ASSEMBLER, a.ALLOWLIST])) + '\n')
            git('add', '.')
            git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')

            def assemble(name):
                return subprocess.run([sys.executable, str(repo / a.ASSEMBLER), '--repo', str(repo),
                    '--output', str(root / name)], env=env, capture_output=True)

            self.assertEqual(assemble('clean').returncode, 0)
            original_oid = git('rev-parse', 'HEAD:README.md').decode().strip()
            replacement = b'Local replacement, absent from signed tree.\n'
            replacement_oid = git('hash-object', '-w', '--stdin', data=replacement).decode().strip()
            git('replace', original_oid, replacement_oid)
            (repo / 'README.md').write_bytes(replacement)
            self.assertEqual(git('diff', '--name-only'), b'')
            self.assertNotEqual(assemble('replaced-worktree').returncode, 0)
            (repo / 'README.md').write_bytes(original)
            self.assertEqual(assemble('replacement-ref-only').returncode, 0)
            for name in [a.FILENAME, 'source-archive-manifest.json', 'SHA256SUMS']:
                self.assertEqual((root / 'clean' / name).read_bytes(),
                    (root / 'replacement-ref-only' / name).read_bytes())

    def test_inventory_metadata_and_content_rejected(self):
        for change in ['path', 'duplicate', 'missing', 'symlink', 'hardlink', 'fifo',
                       'mode', 'owner', 'mtime', 'content', 'pax']:
            with self.subTest(change=change):
                raw = io.BytesIO()
                with tarfile.open(fileobj=io.BytesIO(gzip.decompress(self.archive))) as src:
                    entries = [(copy.copy(m), src.extractfile(m).read() if m.isfile() else None) for m in src]
                with tarfile.open(fileobj=raw, mode='w', format=tarfile.PAX_FORMAT if change == 'pax' else tarfile.USTAR_FORMAT) as dst:
                    file_index = next(i for i, (m, _) in enumerate(entries) if m.isfile())
                    for i, (entry, data) in enumerate(entries):
                        if i == file_index:
                            if change == 'missing': continue
                            if change == 'path': entry.name = '../escape'
                            if change in ['symlink', 'hardlink', 'fifo']:
                                entry.type = {'symlink':tarfile.SYMTYPE, 'hardlink':tarfile.LNKTYPE, 'fifo':tarfile.FIFOTYPE}[change]
                                entry.linkname = 'outside' if change != 'fifo' else ''
                                entry.size = 0
                                data = None
                            if change == 'mode': entry.mode = 0o777
                            if change == 'owner': entry.uid = 1000
                            if change == 'mtime': entry.mtime += 1
                            if change == 'content': data = b'x' * len(data)
                            if change == 'pax': entry.pax_headers = {'comment':'unexpected'}
                        dst.addfile(entry, io.BytesIO(data) if data is not None else None)
                        if i == file_index and change == 'duplicate': dst.addfile(entry, io.BytesIO(data))
                forged = gzip.compress(raw.getvalue(), mtime=0)
                manifest = copy.deepcopy(self.manifest)
                manifest['archive'].update(bytes=len(forged), sha256=a.sha(forged),
                    uncompressed_tar_bytes=len(raw.getvalue()), uncompressed_tar_sha256=a.sha(raw.getvalue()))
                with self.assertRaises(ValueError): a.validate_archive(forged, manifest)


if __name__ == '__main__':
    unittest.main()
