#!/usr/bin/env python3
"""Signed future-release integration fixtures; never use the operator's signing key.

Default: actual installer + GnuPG verification in disposable homes, no network or Rust requirement.
MINDSPACE_TEST_REAL_BUILD=1 additionally builds a tiny dependency-free signed Cargo fixture and
exercises fresh/repeat/upgrade/reinstall with real binaries. This is future-contract behavior,
not compatibility evidence for an unpublished Mindspace production binary or database migration.
"""
from __future__ import annotations

import copy
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
VERSION = '0.2.0-alpha.1'
COMMIT = 'a' * 40
TREE = 'b' * 40
BOOTSTRAP = 'mindspace-release.json'
MANIFEST = 'source-archive-manifest.json'
ARCHIVE = f'mindspace-{VERSION}-source.tar.gz'
ROLES = {'archive': ARCHIVE, 'archive_signature': ARCHIVE+'.asc',
         'source_manifest': MANIFEST, 'source_manifest_signature': MANIFEST+'.asc'}
REAL_BUILD = os.environ.get('MINDSPACE_TEST_REAL_BUILD') == '1'


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def dump(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2)+'\n').encode()


class FutureRelease(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.shared_tmp = tempfile.TemporaryDirectory(prefix='mindspace-future-keys.', dir='/tmp')
        cls.shared = Path(cls.shared_tmp.name)
        cls.gpg_home = cls.shared/'gnupg'; cls.gpg_home.mkdir(mode=0o700)
        cls.gpg = ['gpg', '--no-options', '--homedir', str(cls.gpg_home), '--batch', '--yes',
                   '--pinentry-mode', 'loopback', '--passphrase', '']
        for name in ['Fixture Publisher <publisher@example.invalid>', 'Wrong Fixture Key <wrong@example.invalid>']:
            subprocess.run(cls.gpg+['--quick-generate-key', name, 'ed25519', 'sign', '1d'],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        keys = subprocess.check_output(cls.gpg+['--with-colons', '--list-keys'], stderr=subprocess.DEVNULL).decode()
        cls.fpr, cls.wrong_fpr = [line.split(':')[9] for line in keys.splitlines() if line.startswith('fpr:')]
        cls.key = cls.shared/'publisher.asc'
        cls.wrong_key = cls.shared/'wrong.asc'
        cls.key.write_bytes(subprocess.check_output(cls.gpg+['--armor', '--export', cls.fpr]))
        cls.wrong_key.write_bytes(subprocess.check_output(cls.gpg+['--armor', '--export', cls.wrong_fpr]))

    @classmethod
    def tearDownClass(cls):
        subprocess.run(['gpgconf', '--homedir', str(cls.gpg_home), '--kill', 'all'],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        cls.shared_tmp.cleanup()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='mindspace-future.', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.home = self.base/'home'; self.home.mkdir()
        self.assets = self.base/'assets'; self.assets.mkdir()
        self.tools = self.base/'tools'; self.tools.mkdir()
        self.log = self.base/'curl.log'
        self.metadata_file = self.base/'metadata.json'
        self.installer = self.base/'install.sh'
        original = (ROOT/'install.sh').read_text()
        original, count = re.subn(r'^FPR=[0-9A-F]{40}$', 'FPR='+self.fpr, original, count=1, flags=re.M)
        self.assertEqual(1,count,'test identity substitution must match exactly once')
        original, count = re.subn(r"^FPR_SPACED='[^']*'$", "FPR_SPACED='"+self.fpr+"'", original, count=1, flags=re.M)
        self.assertEqual(1,count)
        self.installer.write_text(original)
        self.source_manifest = self.make_archive()
        self.write_manifest()
        self.refresh_envelope()
        self.make_curl()
        for relative, data in [('.local/share/mindspace/mind.db',b'precious database\x00\xff'),
                               ('.config/mindspace/config.toml',b'[fixture]\nkeep = true\n'),
                               ('.codex/config.toml',b'[mcp_servers.fixture]\ncommand = "/not/executed"\n')]:
            p=self.home/relative; p.parent.mkdir(parents=True,exist_ok=True); p.write_bytes(data)

    def sign(self, name: str, signer: str | None = None):
        subprocess.run(self.gpg+['--armor','--local-user',signer or self.fpr,'--output',str(self.assets/(name+'.asc')),
                                 '--detach-sign',str(self.assets/name)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)

    def make_archive(self, special: str | None = None):
        top=f'mindspace-{VERSION}'
        files={
            'Cargo.toml':f'[package]\nname = "mindspace"\nversion = "{VERSION}"\nedition = "2021"\n\n[[bin]]\nname = "mind"\npath = "src/main.rs"\n'.encode(),
            'Cargo.lock':f'version = 4\n\n[[package]]\nname = "mindspace"\nversion = "{VERSION}"\n'.encode(),
            'rust-toolchain.toml':b'[toolchain]\nchannel = "1.98.1"\nprofile = "minimal"\n',
            'src/main.rs':b'fn main() { println!("mind {}", env!("CARGO_PKG_VERSION")); }\n',
        }
        if special == 'cargo-version':
            files['Cargo.toml']=files['Cargo.toml'].replace(('version = "'+VERSION+'"').encode(),b'version = "9.9.9"')
        members=[]; raw=io.BytesIO()
        with tarfile.open(fileobj=raw,mode='w',format=tarfile.USTAR_FORMAT) as tar:
            for directory in [top,top+'/src']:
                info=tarfile.TarInfo(directory); info.type=tarfile.DIRTYPE; info.mode=0o755; info.mtime=1; tar.addfile(info)
                members.append(dict(path=directory,type='directory',mode='0755',bytes=0))
            for relative,data in files.items():
                info=tarfile.TarInfo(top+'/'+relative); info.mode=0o644; info.mtime=1; info.size=len(data)
                tar.addfile(info,io.BytesIO(data))
                members.append(dict(path=info.name,type='file',mode='0644',bytes=len(data),sha256=sha(data)))
            if special == 'link':
                info=tarfile.TarInfo(top+'/outside'); info.type=tarfile.SYMTYPE; info.linkname='/etc/passwd'; info.mode=0o777; tar.addfile(info)
                members.append(dict(path=info.name,type='symlink',mode='0777',bytes=0))
            elif special == 'traversal':
                data=b'not to be extracted'; info=tarfile.TarInfo(top+'/../escape'); info.mode=0o644; info.size=len(data); tar.addfile(info,io.BytesIO(data))
                members.append(dict(path=info.name,type='file',mode='0644',bytes=len(data),sha256=sha(data)))
        tarbytes=raw.getvalue(); packed=gzip.compress(tarbytes,mtime=0)
        (self.assets/ARCHIVE).write_bytes(packed); self.sign(ARCHIVE)
        return dict(format='mindspace-source-archive-v2',version=VERSION,
                    archive=dict(filename=ARCHIVE,bytes=len(packed),sha256=sha(packed),
                                 uncompressed_tar_bytes=len(tarbytes),uncompressed_tar_sha256=sha(tarbytes)),
                    baseline=dict(commit=COMMIT,tree=TREE),members=members,
                    normalization=dict(format='ustar',uid=0,gid=0,tar_mtime_epoch=1,gzip_mtime=0,
                                       gzip_filename='',gzip_level=9),
                    allowlist_sha256='c'*64,assembler_sha256='d'*64,candidate='signed-test-fixture',
                    publication='test only; never a published release',source='Local dependency-free fixture.')

    def write_manifest(self, raw: bytes | None = None):
        (self.assets/MANIFEST).write_bytes(raw if raw is not None else dump(self.source_manifest)); self.sign(MANIFEST)

    def refresh_envelope(self, mutate=None, raw: bytes | None = None):
        assets={role:dict(filename=name,bytes=(self.assets/name).stat().st_size,sha256=sha((self.assets/name).read_bytes()))
                for role,name in ROLES.items()}
        self.envelope=dict(format='mindspace-release-v1',repository='VikingOwl91/mindspace',tag='v'+VERSION,
                           version=VERSION,platform='linux-x86_64',recipe='cargo-source-no-embeddings-v1',
                           rust_version='1.98.1',source_commit=COMMIT,source_tree=TREE,
                           source_manifest_format='mindspace-source-archive-v2',assets=assets)
        if mutate: mutate(self.envelope)
        (self.assets/BOOTSTRAP).write_bytes(raw if raw is not None else dump(self.envelope)); self.sign(BOOTSTRAP)
        names=[BOOTSTRAP,BOOTSTRAP+'.asc',*ROLES.values()]
        self.metadata=dict(id=200,tag_name='v'+VERSION,draft=False,prerelease=True,published_at='2026-11-14T00:00:00Z',
                           assets=[dict(name=n,state='uploaded',browser_download_url='https://attacker.invalid/'+n,
                                        digest='sha256:'+'0'*64) for n in names])
        self.metadata_file.write_bytes(dump(self.metadata))

    def make_curl(self):
        (self.tools/'curl').write_text('''#!/usr/bin/python3
import json,os,pathlib,sys,urllib.parse
args=sys.argv[1:]; url=args[-1]; output=args[args.index('--output')+1]
with open(os.environ['FIXTURE_CURL_LOG'],'a') as log: log.write(url+'\\n')
p=urllib.parse.urlparse(url)
if p.netloc=='api.github.com':
    release=json.loads(pathlib.Path(os.environ['FIXTURE_METADATA']).read_text())
    if '/releases/tags/' in p.path:
        if p.path.rsplit('/',1)[-1]!=release['tag_name']: sys.exit(22)
        body=json.dumps(release).encode()
    else:
        old=dict(id=1,tag_name='v0.1.0-alpha.1',draft=False,prerelease=True,published_at='2026-11-13T00:00:00Z',assets=[])
        body=json.dumps([old,release]).encode()
elif p.netloc=='github.com' and p.path.startswith('/VikingOwl91/mindspace/releases/download/'+os.environ['FIXTURE_TAG']+'/'):
    asset=pathlib.Path(os.environ['FIXTURE_ASSETS'])/p.path.rsplit('/',1)[-1]
    if not asset.is_file(): sys.exit(22)
    body=asset.read_bytes()
else: sys.exit(22)
limit=int(args[args.index('--max-filesize')+1])
if len(body)>limit: sys.exit(63)
pathlib.Path(output).write_bytes(body)
''')
        (self.tools/'curl').chmod(0o755)

    def snapshot(self, p: Path | None = None):
        p=p or self.home
        return [(str(f.relative_to(p)), f.lstat().st_mtime_ns, f.lstat().st_mode,
                 sha(f.read_bytes()) if f.is_file() else None) for f in sorted(p.rglob('*'))]

    def run_inst(self, *args, verify_only=True, local_assets=True, key=None, preserve=True):
        before=self.snapshot()
        env=dict(HOME=str(self.home),PATH=str(self.tools)+':'+os.environ['PATH'],TMPDIR=str(self.base),
                 FIXTURE_CURL_LOG=str(self.log),FIXTURE_METADATA=str(self.metadata_file),
                 FIXTURE_ASSETS=str(self.assets),FIXTURE_TAG='v'+VERSION,
                 RUSTUP_HOME=os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup')))
        command=['sh',str(self.installer),'--trust-fingerprint',self.fpr,'--key-file',str(key or self.key)]
        if local_assets: command+=['--assets-dir',str(self.assets)]
        if verify_only: command+=['--verify-only']
        proc=subprocess.run(command+list(args),env=env,stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=120)
        if preserve: self.assertEqual(before,self.snapshot(),proc.stdout+proc.stderr)
        self.assertEqual([],list(self.base.glob('mindspace-install.*')),'temporary installer work leaked')
        if self.log.exists(): self.assertNotIn('/download/v0.1.0-alpha.1/',self.log.read_text(),'silent older-release fallback')
        return proc

    def assert_ok(self, proc):
        self.assertEqual(0,proc.returncode,proc.stdout+proc.stderr)

    def assert_refused(self, proc):
        self.assertEqual(1,proc.returncode,proc.stdout+proc.stderr)
        self.assertNotIn('Already up to date',proc.stdout)

    def install_record(self, archive_sha=None, commit=None):
        p=self.home/'.local/lib/mindspace'/VERSION; (p/'bin').mkdir(parents=True)
        binary=p/'bin/mind'; binary.write_text('#!/bin/sh\necho mind '+VERSION+'\n'); binary.chmod(0o755)
        (p/'install-record').write_text('mindspace-install-record 1\n'+
            f'version={VERSION}\nbinary_sha256={sha(binary.read_bytes())}\narchive_sha256={archive_sha or self.envelope["assets"]["archive"]["sha256"]}\n'+
            f'source_commit={commit or COMMIT}\npublisher_fingerprint={self.fpr}\ninstalled_at=2026-11-13T12:00:00Z\n')

    def test_authenticated_future_contract_verifies_without_building(self):
        p=self.run_inst(); self.assert_ok(p); self.assertIn('Authenticated',p.stdout)

    def test_downloads_use_constructed_repository_urls_not_metadata_urls_or_digests(self):
        self.assert_ok(self.run_inst(local_assets=False))
        self.assertNotIn('attacker.invalid',self.log.read_text())
        self.assertIn('/download/v'+VERSION+'/'+BOOTSTRAP,self.log.read_text())

    def test_explicit_future_version_is_bound_to_authenticated_contract(self):
        self.assert_ok(self.run_inst('--version','v'+VERSION))
        self.assertIn('/releases/tags/v'+VERSION,self.log.read_text())

    def test_recorded_existing_future_release_is_an_untouched_noop(self):
        self.install_record(); p=self.run_inst(verify_only=False); self.assert_ok(p); self.assertIn('Already up to date',p.stdout)

    def test_changed_archive_identity_cannot_be_claimed_as_up_to_date(self):
        self.install_record(archive_sha='f'*64); self.assert_refused(self.run_inst(verify_only=False))

    def test_changed_source_identity_cannot_be_claimed_as_up_to_date(self):
        self.install_record(commit='f'*40); self.assert_refused(self.run_inst(verify_only=False))

    def test_receiptless_future_installation_requires_explicit_reinstall(self):
        self.install_record(); (self.home/'.local/lib/mindspace'/VERSION/'install-record').unlink()
        self.assert_refused(self.run_inst(verify_only=False))

    def test_source_binding_failure_is_rejected_before_existing_noop(self):
        self.install_record()
        self.source_manifest['baseline']['tree']='f'*40
        self.write_manifest(); self.refresh_envelope()
        self.assert_refused(self.run_inst(verify_only=False))

    def test_signed_toolchain_mismatch_is_rejected_before_existing_noop(self):
        self.install_record(); self.refresh_envelope(lambda e:e.update(rust_version='1.100.0'))
        self.assert_refused(self.run_inst(verify_only=False))

    def test_signed_cargo_version_mismatch_is_rejected_before_extraction(self):
        self.source_manifest=self.make_archive('cargo-version'); self.write_manifest(); self.refresh_envelope()
        self.assert_refused(self.run_inst())

    def test_bad_envelope_signature_is_rejected_before_existing_noop(self):
        self.install_record(); self.sign(BOOTSTRAP,self.wrong_fpr); self.assert_refused(self.run_inst(verify_only=False))

    def test_wrong_independently_supplied_key_is_rejected(self):
        self.assert_refused(self.run_inst(key=self.wrong_key))

    def test_unsigned_envelope_is_rejected(self):
        (self.assets/(BOOTSTRAP+'.asc')).unlink(); self.assert_refused(self.run_inst())

    def test_archive_hash_mismatch_is_rejected(self):
        with (self.assets/ARCHIVE).open('ab') as out: out.write(b'changed')
        self.assert_refused(self.run_inst())

    def test_manifest_hash_mismatch_is_rejected(self):
        with (self.assets/MANIFEST).open('ab') as out: out.write(b'changed')
        self.assert_refused(self.run_inst())

    def test_archive_detached_signature_is_mandatory_independently_of_envelope(self):
        self.sign(ARCHIVE,self.wrong_fpr); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_manifest_detached_signature_is_mandatory_independently_of_envelope(self):
        self.sign(MANIFEST,self.wrong_fpr); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_unsupported_authenticated_format_does_not_fall_back(self):
        self.refresh_envelope(lambda e:e.update(format='mindspace-release-v999')); self.assert_refused(self.run_inst())

    def test_unsupported_authenticated_platform_does_not_fall_back(self):
        self.refresh_envelope(lambda e:e.update(platform='linux-arm64')); self.assert_refused(self.run_inst())

    def test_unsupported_authenticated_toolchain_does_not_fall_back(self):
        self.refresh_envelope(lambda e:e.update(rust_version='stable')); self.assert_refused(self.run_inst())

    def test_envelope_version_substitution_is_rejected(self):
        self.refresh_envelope(lambda e:e.update(version='9.9.9')); self.assert_refused(self.run_inst())

    def test_envelope_tag_substitution_is_rejected(self):
        self.refresh_envelope(lambda e:e.update(tag='v9.9.9')); self.assert_refused(self.run_inst())

    def test_signed_duplicate_json_keys_are_rejected(self):
        raw=dump(self.envelope).replace(b'{',b'{"format":"mindspace-release-v999",',1)
        (self.assets/BOOTSTRAP).write_bytes(raw); self.sign(BOOTSTRAP); self.assert_refused(self.run_inst())

    def test_source_commit_binding_is_checked_before_extraction(self):
        self.source_manifest['baseline']['commit']='f'*40; self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_source_tree_binding_is_checked_before_extraction(self):
        self.source_manifest['baseline']['tree']='f'*40; self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_source_archive_binding_is_checked_before_extraction(self):
        self.source_manifest['archive']['sha256']='f'*64; self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_source_manifest_duplicate_json_keys_are_rejected(self):
        raw=dump(self.source_manifest).replace(b'{',b'{"format":"wrong",',1)
        self.write_manifest(raw); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_tar_links_are_rejected_before_extraction(self):
        self.source_manifest=self.make_archive('link'); self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())

    def test_tar_traversal_is_rejected_before_extraction(self):
        self.source_manifest=self.make_archive('traversal'); self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())
        self.assertFalse((self.base/'escape').exists())

    def test_declared_uncompressed_archive_limit_is_enforced(self):
        self.source_manifest['archive']['uncompressed_tar_bytes']=100_000_001
        self.write_manifest(); self.refresh_envelope(); self.assert_refused(self.run_inst())

    @unittest.skipUnless(REAL_BUILD,'requires MINDSPACE_TEST_REAL_BUILD=1; real dependency-free Rust build')
    def test_real_build_fresh_and_repeat(self):
        sentinels={p: p.read_bytes() for p in self.home.rglob('*') if p.is_file()}
        self.assert_ok(self.run_inst('--version',VERSION,verify_only=False,preserve=False))
        binary=self.home/'.local/lib/mindspace'/VERSION/'bin/mind'
        self.assertEqual('mind '+VERSION,subprocess.check_output([str(binary),'--version'],text=True).strip())
        self.assert_ok(self.run_inst(verify_only=False))
        for path,data in sentinels.items(): self.assertEqual(data,path.read_bytes())

    @unittest.skipUnless(REAL_BUILD,'requires MINDSPACE_TEST_REAL_BUILD=1; real dependency-free Rust build')
    def test_real_build_side_by_side_upgrade_and_reinstall(self):
        sentinels={p: p.read_bytes() for p in self.home.rglob('*') if p.is_file()}
        # A validated old executable fixture is preserved; the new binary is a real Cargo build.
        old=self.home/'.local/lib/mindspace/0.0.9/bin/mind'; old.parent.mkdir(parents=True)
        old.write_text('#!/bin/sh\necho mind 0.0.9\n'); old.chmod(0o755)
        old_bytes=old.read_bytes()
        self.assert_refused(self.run_inst(verify_only=False))
        self.assert_ok(self.run_inst('--upgrade',verify_only=False,preserve=False))
        prefix=self.home/'.local/lib/mindspace'/VERSION; binary=prefix/'bin/mind'
        self.assertEqual('mind '+VERSION,subprocess.check_output([str(binary),'--version'],text=True).strip())
        self.assertEqual(old_bytes,old.read_bytes())
        self.assert_ok(self.run_inst(verify_only=False))
        previous=binary.read_bytes()
        self.assert_ok(self.run_inst('--reinstall',verify_only=False,preserve=False))
        backups=list((self.home/'.local/lib/mindspace/previous').rglob('bin/mind'))
        self.assertEqual(1,len(backups)); self.assertEqual(previous,backups[0].read_bytes())
        for path,data in sentinels.items(): self.assertEqual(data,path.read_bytes())


if __name__=='__main__':
    unittest.main(verbosity=2)
