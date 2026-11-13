#!/usr/bin/env python3
"""Offline discovery regressions against the real self-contained shell installer."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CURRENT = '0.1.0-alpha.1'
FPR = '0E6699CB48F3941AE28CA88D509F78007F62D5EA'
NAMES = ['mindspace-0.1.0-alpha.1-rc.5-source.tar.gz', 'mindspace-0.1.0-alpha.1-rc.5-source.tar.gz.asc',
         'source-archive-manifest.json', 'source-archive-manifest.json.asc', 'SHA256SUMS', 'VERIFY.txt', 'mindspace-release-signing-key.asc']


def release(version=CURRENT, ident=1, published='2026-11-13T12:00:00Z', draft=False):
    return dict(id=ident, tag_name='v'+version, published_at=published, draft=draft, prerelease='-' in version,
                assets=[dict(name=n, state='uploaded') for n in NAMES] if version == CURRENT else [])


class Discovery(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='mindspace-discovery.', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.home = self.base/'home'; self.home.mkdir()
        self.bin = self.base/'bin'; self.bin.mkdir()
        self.responses = self.base/'responses'; self.responses.mkdir()
        self.log = self.base/'curl.log'
        p = self.home/'.local/lib/mindspace'/CURRENT/'bin'; p.mkdir(parents=True)
        (p/'mind').write_text('#!/bin/sh\necho mind '+CURRENT+'\n'); (p/'mind').chmod(0o755)
        curl = self.bin/'curl'
        curl.write_text('''#!/usr/bin/python3
import json,os,sys,pathlib,urllib.parse
args=sys.argv[1:]; url=args[-1]; dest=args[args.index('--output')+1]
with open(os.environ['TEST_CURL_LOG'],'a') as log: log.write(url+'\\n')
p=urllib.parse.urlparse(url); q=urllib.parse.parse_qs(p.query)
if p.netloc!='api.github.com': sys.exit(22)
name='page-'+q.get('page',['1'])[0]+'.json' if p.path.endswith('/releases') else 'explicit.json'
source=pathlib.Path(os.environ['TEST_RESPONSES'])/name
if not source.is_file(): sys.exit(22)
pathlib.Path(dest).write_bytes(source.read_bytes())
''')
        curl.chmod(0o755)

    def snapshot(self):
        return [(str(p.relative_to(self.home)), p.lstat().st_mtime_ns,
                 hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else None)
                for p in sorted(self.home.rglob('*'))]

    def run_inst(self, *args):
        before = self.snapshot()
        env = dict(HOME=str(self.home), PATH=str(self.bin)+':'+os.environ['PATH'], TMPDIR=str(self.base),
                   TEST_CURL_LOG=str(self.log), TEST_RESPONSES=str(self.responses),
                   RUSTUP_HOME=os.environ.get('RUSTUP_HOME', str(Path.home()/'.rustup')))
        proc = subprocess.run(['sh', str(ROOT/'install.sh'), '--trust-fingerprint', FPR, *args], env=env,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        self.assertEqual(before, self.snapshot(), proc.stdout+proc.stderr)
        self.assertEqual([], list(self.base.glob('mindspace-install.*')), 'temporary work leaked')
        return proc

    def page(self, data, number=1):
        (self.responses/f'page-{number}.json').write_text(json.dumps(data))

    def test_latest_includes_prereleases_and_uses_publication_order(self):
        self.page([release('1.0.0', 2, '2026-11-12T00:00:00Z'), release()])
        p=self.run_inst(); self.assertEqual(0,p.returncode,p.stdout+p.stderr); self.assertIn('Already up to date',p.stdout)
        self.assertIn('/releases?per_page=100&page=1',self.log.read_text())

    def test_drafts_are_excluded(self):
        self.page([release(),release('2.0.0',2,'2026-11-14T00:00:00Z',True)])
        p=self.run_inst(); self.assertEqual(0,p.returncode,p.stdout+p.stderr)

    def test_unsupported_newest_does_not_fall_back(self):
        self.page([release(),release('0.2.0-alpha.1',2,'2026-11-14T00:00:00Z')])
        p=self.run_inst(); self.assertEqual(1,p.returncode); self.assertIn('0.2.0-alpha.1',p.stdout+p.stderr)
        self.assertNotIn('/download/v'+CURRENT,self.log.read_text())

    def test_publication_tie_uses_larger_release_id_before_compatibility(self):
        self.page([release(),release('0.2.0',2)])
        p=self.run_inst(); self.assertEqual(1,p.returncode); self.assertIn('0.2.0',p.stdout+p.stderr)

    def test_explicit_version_accepts_one_v_and_uses_exact_lookup(self):
        (self.responses/'explicit.json').write_text(json.dumps(release()))
        p=self.run_inst('--version','v'+CURRENT); self.assertEqual(0,p.returncode,p.stdout+p.stderr)
        self.assertIn('/releases/tags/v'+CURRENT,self.log.read_text())
        self.assertNotIn('/releases?',self.log.read_text())

    def test_empty_explicit_version_is_rejected_without_discovery(self):
        self.page([release()]); p=self.run_inst('--version','')
        self.assertEqual(1,p.returncode); self.assertFalse(self.log.exists())

    def test_explicit_missing_version_does_not_query_latest(self):
        p=self.run_inst('--version','9.9.9'); self.assertEqual(1,p.returncode)
        self.assertNotIn('/releases?',self.log.read_text())

    def test_explicit_draft_is_refused(self):
        (self.responses/'explicit.json').write_text(json.dumps(release(draft=True)))
        p=self.run_inst('--version',CURRENT); self.assertEqual(1,p.returncode)

    def test_metadata_tag_substitution_is_refused(self):
        (self.responses/'explicit.json').write_text(json.dumps(release('0.0.1')))
        p=self.run_inst('--version',CURRENT); self.assertEqual(1,p.returncode)

    def test_no_discovery_fallback_on_network_error(self):
        p=self.run_inst(); self.assertEqual(1,p.returncode)
        self.assertNotIn('Already up to date',p.stdout)

    def test_malformed_json_and_duplicate_keys_fail(self):
        (self.responses/'page-1.json').write_text('[{"id":1,"id":2}]')
        p=self.run_inst(); self.assertEqual(1,p.returncode)

    def test_unsupported_selected_tag_is_not_skipped(self):
        r=release('2.0.0',2,'2026-11-14T00:00:00Z'); r['tag_name']='unexpected/tag'
        self.page([release(),r]); p=self.run_inst(); self.assertEqual(1,p.returncode)

    def test_complete_pagination(self):
        self.page([release('0.0.1-alpha.'+str(i),i+10,'2026-11-10T00:00:00Z') for i in range(100)])
        self.page([release()],2)
        p=self.run_inst(); self.assertEqual(0,p.returncode,p.stdout+p.stderr)
        self.assertIn('page=2',self.log.read_text())

    def test_incomplete_pagination_fails_without_fallback(self):
        self.page([release('0.0.1-alpha.'+str(i),i+10,'2026-11-10T00:00:00Z') for i in range(100)])
        p=self.run_inst(); self.assertEqual(1,p.returncode); self.assertIn('page=2',self.log.read_text())

    def test_duplicate_release_identity_across_pages_is_refused(self):
        first=[release('0.0.1-alpha.'+str(i),i+10,'2026-11-10T00:00:00Z') for i in range(100)]
        self.page(first); self.page([first[0]],2)
        p=self.run_inst(); self.assertEqual(1,p.returncode)

    def test_explicit_rc5_local_assets_retains_offline_noop(self):
        p=self.run_inst('--version',CURRENT,'--assets-dir',str(self.base/'missing'))
        self.assertEqual(0,p.returncode,p.stdout+p.stderr); self.assertFalse(self.log.exists())

if __name__=='__main__': unittest.main(verbosity=2)
