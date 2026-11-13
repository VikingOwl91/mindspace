#!/usr/bin/env python3
"""Offline contract/discovery regressions. No publisher credentials or network."""
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / 'scripts/installer_release_contract.py'
GENERATOR = ROOT / 'scripts/make-installer-release-manifest.py'
VERSION = '0.2.0-alpha.1'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def release(version=VERSION, ident=2, published='2026-11-13T12:00:00Z', draft=False):
    return {'id': ident, 'tag_name': 'v'+version, 'draft': draft,
            'prerelease': '-' in version, 'published_at': published,
            'assets': [{'name': n, 'state': 'uploaded'} for n in
                       ['source.tar.gz', 'source.tar.gz.asc', 'source.json', 'source.json.asc',
                        'mindspace-release.json', 'mindspace-release.json.asc']]}


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='mindspace-contract-test-')
        self.root = Path(self.tmp.name)
        self.envelope, self.source, self.archive = self.fixture()

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, value):
        p = self.root / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps(value), encoding='utf-8')
        return p

    def run_cli(self, *args, ok=True, text=None):
        p = subprocess.run([sys.executable, '-I', str(HELPER), *map(str, args)],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0 if ok else 1, p.stdout+p.stderr)
        self.assertNotIn('Traceback', p.stderr)
        if text:
            self.assertIn(text, p.stdout+p.stderr)
        return p.stdout

    def fixture(self, member_name=None, member_type=None, pax=False):
        top = 'mindspace-'+VERSION
        files = {top+'/Cargo.toml': ('[package]\nname="mindspace"\nversion="'+VERSION+'"\n').encode(),
                 top+'/Cargo.lock': b'version = 4\n',
                 top+'/rust-toolchain.toml': b'[toolchain]\nchannel = "1.98.1"\n'}
        raw = io.BytesIO()
        members = []
        with tarfile.open(fileobj=raw, mode='w', format=tarfile.PAX_FORMAT if pax else tarfile.USTAR_FORMAT) as tf:
            ti = tarfile.TarInfo(top); ti.type = tarfile.DIRTYPE; ti.mode = 0o755
            tf.addfile(ti); members.append({'path': top, 'type': 'directory', 'mode': '0755', 'bytes': 0})
            for name, data in files.items():
                ti = tarfile.TarInfo(name); ti.mode = 0o644; ti.size = len(data)
                tf.addfile(ti, io.BytesIO(data))
                members.append({'path': name, 'type': 'file', 'mode': '0644', 'bytes': len(data), 'sha256': sha(data)})
            if member_name:
                ti = tarfile.TarInfo(member_name); ti.mode = 0o644
                ti.type = member_type or tarfile.REGTYPE
                ti.linkname = 'elsewhere' if member_type else ''
                if pax: ti.pax_headers = {'comment': 'not part of supported ustar contract'}
                tf.addfile(ti, io.BytesIO(b''))
                members.append({'path': member_name, 'type': 'file', 'mode': '0644', 'bytes': 0, 'sha256': sha(b'')})
        compressed = gzip.compress(raw.getvalue(), mtime=0)
        archive = self.root / 'source.tar.gz'; archive.write_bytes(compressed)
        source = {'format': 'mindspace-source-archive-v2', 'version': VERSION,
                  'archive': {'filename': archive.name, 'bytes': len(compressed), 'sha256': sha(compressed),
                              'uncompressed_tar_bytes': len(raw.getvalue()), 'uncompressed_tar_sha256': sha(raw.getvalue())},
                  'baseline': {'commit': 'a'*40, 'tree': 'b'*40}, 'members': members,
                  'normalization': {'format': 'ustar'}, 'candidate': 'fixture',
                  'publication': 'fixture only', 'source': 'fixture', 'allowlist_sha256': 'c'*64,
                  'assembler_sha256': 'd'*64}
        source_path = self.write('source.json', source)
        (self.root/'source.tar.gz.asc').write_bytes(b'archive signature fixture')
        (self.root/'source.json.asc').write_bytes(b'source signature fixture')
        assets = {}
        for role, name in [('archive', 'source.tar.gz'), ('archive_signature', 'source.tar.gz.asc'),
                           ('source_manifest', 'source.json'), ('source_manifest_signature', 'source.json.asc')]:
            data = (self.root/name).read_bytes()
            assets[role] = {'filename': name, 'bytes': len(data), 'sha256': sha(data)}
        envelope = {'format': 'mindspace-release-v1', 'repository': 'VikingOwl91/mindspace',
                    'tag': 'v'+VERSION, 'version': VERSION, 'platform': 'linux-x86_64',
                    'recipe': 'cargo-source-no-embeddings-v1', 'rust_version': '1.98.1',
                    'source_commit': 'a'*40, 'source_tree': 'b'*40,
                    'source_manifest_format': 'mindspace-source-archive-v2', 'assets': assets}
        return envelope, source, archive

    def contract(self, envelope=None, ok=True, text=None):
        return self.run_cli('contract', self.write('envelope.json', envelope or self.envelope),
                            VERSION, self.root/'out', ok=ok, text=text)

    def source_check(self, source=None, envelope=None, ok=True, text=None):
        s = source or self.source
        self.write('source.json', s)
        e = copy.deepcopy(envelope or self.envelope)
        data = (self.root/'source.json').read_bytes()
        e['assets']['source_manifest'].update(bytes=len(data), sha256=sha(data))
        return self.run_cli('source', self.write('envelope.json', e), self.root/'source.json',
                            self.archive, ok=ok, text=text)

    def test_normalize(self):
        self.assertEqual(self.run_cli('normalize', 'v1.2.3-rc.1+build.4').strip(), '1.2.3-rc.1+build.4')

    def test_invalid_versions(self):
        for value in ['vv1.2.3', '1.2', '01.2.3', '1.2.3-01', '1.2.3/evil', '1.2.3\nother']:
            with self.subTest(value=value): self.run_cli('normalize', value, ok=False)

    def test_page_accepts_prereleases_and_drafts(self):
        self.assertEqual(self.run_cli('page', self.write('page.json', [release(), release(ident=3, draft=True)])).strip(), '2')

    def test_page_rejects_wrong_numeric_and_boolean_types(self):
        for field, value in [('id', True), ('id', -1), ('draft', 1), ('prerelease', 'false')]:
            r = release(); r[field] = value
            with self.subTest(field=field): self.run_cli('page', self.write('page.json', [r]), ok=False)

    def test_bad_dates_are_rejected(self):
        for date in [None, '2026-02-30T00:00:00Z', 'tomorrow', '2026-11-13T12:00:00+00:00']:
            self.run_cli('page', self.write('page.json', [release(published=date)]), ok=False)

    def test_draft_can_have_null_publication(self):
        self.run_cli('page', self.write('page.json', [release(draft=True, published=None)]))

    def test_page_bounds_and_error_object(self):
        self.run_cli('page', self.write('page.json', [release(ident=i+1) for i in range(101)]), ok=False)
        self.run_cli('page', self.write('page.json', {'message': 'API rate limit'}), ok=False)

    def test_duplicate_json_and_nonfinite_rejected(self):
        for content in ['{"id":1,"id":2}', '[NaN]', '[Infinity]', '[[[[']:
            p = self.root/'page.json'; p.write_text(content)
            self.run_cli('page', p, ok=False)

    def test_overflowing_float_and_oversized_json_are_rejected(self):
        p=self.root/'page.json'
        p.write_text(json.dumps([release()])[:-2]+',"ignored":1e999}]')
        self.run_cli('page',p,ok=False,text='non-finite')
        p.write_bytes(b' '*4_000_001)
        self.run_cli('page',p,ok=False,text='byte limit')

    def test_select_by_publication_then_id_not_semver(self):
        self.write('pages/page-1.json', [release('9.0.0', 1, '2026-11-12T12:00:00Z'), release('0.2.0-alpha.2', 2)])
        self.write('pages/page-2.json', [release('0.2.0-alpha.3', 3), release('99.0.0', 4, draft=True)])
        # Complete page sets must use full pages until the last. Fill page 1 to 100.
        p = self.root/'pages/page-1.json'; first=json.loads(p.read_text())
        first += [release('0.0.'+str(i), 100+i, '2026-01-01T00:00:00Z') for i in range(98)]
        p.write_text(json.dumps(first))
        out = self.root/'selected.json'
        self.assertEqual(self.run_cli('select', self.root/'pages', out).strip(), '0.2.0-alpha.3')
        self.assertEqual(json.loads(out.read_text())['id'], 3)

    def test_select_no_fallback_from_unsupported_newest_tag(self):
        r = release(ident=3); r['tag_name']='future/not-supported'
        self.write('pages/page-1.json', [release('0.1.0'), r])
        self.run_cli('select', self.root/'pages', self.root/'selected.json', ok=False, text='selected')

    def test_duplicate_identity_and_tags_rejected(self):
        for records in [[release(), release()], [release(ident=2), release(ident=3)]]:
            self.write('pages/page-1.json', records)
            self.run_cli('select', self.root/'pages', self.root/'selected.json', ok=False)

    def test_missing_or_incomplete_pages_rejected(self):
        self.write('pages/page-2.json', [])
        self.run_cli('select', self.root/'pages', self.root/'selected.json', ok=False)
        (self.root/'pages/page-2.json').unlink()
        self.write('pages/page-1.json', [release('0.0.'+str(i),i+1) for i in range(100)])
        self.run_cli('select', self.root/'pages', self.root/'selected.json', ok=False)

    def test_empty_releases_rejected(self):
        self.write('pages/page-1.json', [])
        self.run_cli('select', self.root/'pages', self.root/'selected.json', ok=False)

    def test_explicit_version_binding(self):
        p=self.write('release.json',release())
        self.assertEqual(self.run_cli('explicit',p,'v'+VERSION,self.root/'selected.json').strip(),VERSION)
        self.run_cli('explicit',p,'1.0.0',self.root/'selected.json',ok=False)
        self.run_cli('explicit',self.write('release.json',release(draft=True)),VERSION,self.root/'selected.json',ok=False)

    def test_assets_required_uploaded_and_unique(self):
        p=self.write('release.json',release())
        self.run_cli('assets',p,'source.tar.gz')
        self.run_cli('assets',p,'missing',ok=False)
        r=release();r['assets'].append(r['assets'][0]);self.run_cli('assets',self.write('release.json',r),'source.tar.gz',ok=False)
        r=release();r['assets'][0]['state']='new';self.run_cli('assets',self.write('release.json',r),'source.tar.gz',ok=False)

    def test_supported_contract_emits_fixed_values(self):
        self.contract()
        self.assertEqual((self.root/'out/contract.values').read_text().splitlines(),
                         ['source.tar.gz',self.envelope['assets']['archive']['sha256'],'source.json','a'*40,'1.98.1','b'*40])
        self.assertEqual(len((self.root/'out/assets.table').read_text().splitlines()),4)

    def test_unsupported_contract_fields(self):
        for field,value in [('format','v99'),('repository','evil/repo'),('tag','v9.9.9'),('platform','windows'),
                            ('recipe','shell-anything'),('rust_version','stable'),('source_manifest_format','v99')]:
            e=copy.deepcopy(self.envelope);e[field]=value
            with self.subTest(field=field): self.contract(e,ok=False)

    def test_unknown_or_missing_contract_fields(self):
        e=copy.deepcopy(self.envelope);e['commands']='evil';self.contract(e,ok=False)
        e=copy.deepcopy(self.envelope);del e['source_tree'];self.contract(e,ok=False)

    def test_asset_bounds_types_paths_collisions_and_hashes(self):
        for field,value in [('bytes',True),('bytes',0),('bytes',50_000_001),('filename','../evil'),
                            ('filename','mindspace-release.json'),('sha256','not-a-digest')]:
            e=copy.deepcopy(self.envelope);e['assets']['archive'][field]=value
            with self.subTest(field=field,value=value):self.contract(e,ok=False)
        e=copy.deepcopy(self.envelope);e['assets']['source_manifest']['filename']='source.tar.gz';self.contract(e,ok=False)
        e=copy.deepcopy(self.envelope);e['assets']['archive_signature']['filename']='other.asc';self.contract(e,ok=False)

    def test_valid_source_archive_contract(self):self.source_check()

    def test_source_build_recipe_is_validated_before_extraction(self):
        e=copy.deepcopy(self.envelope); e['rust_version']='1.100.0'
        self.source_check(envelope=e,ok=False,text='toolchain')

    def test_source_binding_mismatch(self):
        for field,value in [('version','9.0.0'),('format','unsupported')]:
            s=copy.deepcopy(self.source);s[field]=value;self.source_check(s,ok=False)
        for field,value in [('filename','other.tar.gz'),('bytes',1),('sha256','0'*64),('uncompressed_tar_sha256','0'*64),('uncompressed_tar_bytes',1)]:
            s=copy.deepcopy(self.source);s['archive'][field]=value;self.source_check(s,ok=False)
        s=copy.deepcopy(self.source);s['baseline']['tree']='c'*40;self.source_check(s,ok=False)

    def test_manifest_members_match_actual_archive(self):
        for field,value in [('sha256','0'*64),('bytes',999),('mode','0777')]:
            s=copy.deepcopy(self.source);s['members'][1][field]=value;self.source_check(s,ok=False)
        s=copy.deepcopy(self.source);s['members'].append(s['members'][1]);self.source_check(s,ok=False)
        s=copy.deepcopy(self.source);s['members'].pop();self.source_check(s,ok=False)

    def test_tar_traversal_links_and_pax_rejected(self):
        for name,kind,pax in [('../escape',None,False),('mindspace-'+VERSION+'/link',tarfile.SYMTYPE,False),
                              ('mindspace-'+VERSION+'/device',tarfile.CHRTYPE,False),
                              ('mindspace-'+VERSION+'/pax',None,True)]:
            self.envelope,self.source,self.archive=self.fixture(name,kind,pax)
            with self.subTest(name=name):self.source_check(ok=False)

    def test_source_resource_limits(self):
        s=copy.deepcopy(self.source);s['archive']['uncompressed_tar_bytes']=100_000_001;self.source_check(s,ok=False)
        s=copy.deepcopy(self.source);s['members']*=4000;self.source_check(s,ok=False)

    def test_toml_exact_versions(self):
        p=self.root/'src';p.mkdir()
        (p/'Cargo.toml').write_text('[package]\nname="mindspace"\nversion="'+VERSION+'"\n')
        (p/'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.98.1"\n')
        self.run_cli('toml',p,VERSION,'1.98.1')
        self.run_cli('toml',p,'9.9.9','1.98.1',ok=False)
        self.run_cli('toml',p,VERSION,'1.100.0',ok=False)

    def test_corrupt_gzip_has_no_traceback(self):
        self.archive.write_bytes(b'not a gzip stream')
        self.source['archive'].update(bytes=self.archive.stat().st_size,sha256=sha(self.archive.read_bytes()))
        self.envelope['assets']['archive'].update(bytes=self.archive.stat().st_size,sha256=sha(self.archive.read_bytes()))
        self.source_check(ok=False)

    def test_generator_round_trip_without_modifying_inputs(self):
        before={p.name:p.read_bytes() for p in self.root.iterdir() if p.is_file()}
        out=self.root/'mindspace-release.json'
        p=subprocess.run([sys.executable,str(GENERATOR),'--archive',str(self.archive),'--manifest',str(self.root/'source.json'),
                          '--archive-signature',str(self.root/'source.tar.gz.asc'),'--manifest-signature',str(self.root/'source.json.asc'),
                          '--rust-version','1.98.1','--output',str(out)],capture_output=True,text=True)
        self.assertEqual(p.returncode,0,p.stdout+p.stderr)
        self.run_cli('contract',out,VERSION,self.root/'out')
        for name,data in before.items():self.assertEqual((self.root/name).read_bytes(),data)
        # Refuse to overwrite an existing envelope, including any input.
        p=subprocess.run([sys.executable,str(GENERATOR),'--archive',str(self.archive),'--manifest',str(self.root/'source.json'),
                          '--archive-signature',str(self.root/'source.tar.gz.asc'),'--manifest-signature',str(self.root/'source.json.asc'),
                          '--rust-version','1.98.1','--output',str(self.archive)],capture_output=True,text=True)
        self.assertEqual(p.returncode,1,p.stdout+p.stderr)
        self.assertEqual(self.archive.read_bytes(),before[self.archive.name])


if __name__=='__main__':unittest.main()
