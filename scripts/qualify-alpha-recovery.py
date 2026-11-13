#!/usr/bin/env python3
"""Disposable Alpha recovery qualification; never accepts an existing user DB.

All databases are generated below a NEW private run directory. No cleanup is
performed. --recipe points at the exact report recipe under qualification.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import traceback


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


class Suite:
    def __init__(self, args, logfd):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix='run-', dir=args.work_parent))
        self.cases = []
        self.commands = []
        self.procs = []
        self.report = Path(args.log)
        self.logfile = os.fdopen(logfd, 'w', encoding='utf-8')
        self.log_identity = os.fstat(self.logfile.fileno())
        self.namespace = {'__name__': 'offline_recipe'}
        exec(compile(Path(args.recipe).read_text(), args.recipe, 'exec'), self.namespace)

    def persist(self):
        result = {'root': str(self.root), 'recipe_sha256': sha(self.args.recipe),
                  'binary_sha256': {p: sha(p) for p in [self.args.binary, self.args.production, self.args.old_binary]},
                  'python': sys.version, 'sqlite': sqlite3.sqlite_version,
                  'cases': self.cases, 'commands': self.commands}
        current = self.report.lstat()
        if (current.st_dev, current.st_ino) != (self.log_identity.st_dev, self.log_identity.st_ino):
            raise RuntimeError('owned log path changed; STOP without touching replacement')
        self.logfile.seek(0)
        self.logfile.truncate()
        self.logfile.write(json.dumps(result, indent=2) + '\n')
        self.logfile.flush()
        os.fsync(self.logfile.fileno())

    def case(self, name, fn):
        try:
            detail = fn()
            self.cases.append({'name': name, 'status': 'PASS', 'evidence': detail})
        except Exception as e:
            self.cases.append({'name': name, 'status': 'FAIL', 'error': repr(e), 'traceback': traceback.format_exc()})
        finally:
            for p in self.procs:
                if p.poll() is None:
                    p.kill()
                p.wait(timeout=10)
            self.procs.clear()
            self.persist()
            print(name, self.cases[-1]['status'], flush=True)

    def run(self, argv, check=True, env=None, mask=None):
        kwargs = {'capture_output': True, 'text': True, 'timeout': 60}
        if mask is not None:
            kwargs['umask'] = mask  # child-only; parent never changes umask
        e = os.environ.copy()
        e.update({'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1'})
        # Per-invocation fixture ledger. No unrelated process is inspected beyond
        # descriptor visibility/identity metadata. This is an operator assertion,
        # not proof that an unobservable process has no database descriptor.
        unknown = []
        for proc in Path('/proc').glob('[0-9]*'):
            try:
                if proc.stat().st_uid != os.geteuid() or int(proc.name) == os.getpid():
                    continue
                for fd in (proc / 'fd').iterdir():
                    try:
                        fd.stat()
                    except FileNotFoundError:
                        pass
            except FileNotFoundError:
                continue
            except PermissionError:
                try:
                    identity = self.namespace['process_identity'](proc)
                except FileNotFoundError:
                    continue
                unknown.append({**identity, 'classification': 'operator-classified-unrelated'})
        e['MINDSPACE_RECOVERY_QUIESCENCE'] = json.dumps({
            'contract': 'all-clients-stopped-and-restarts-prevented',
            'operator_assertion': 'Disposable source created in this run; only tracked owned children are configured to open it; relevant children are stopped for positive cases, no external restart configuration exists. Negative held-client cases deliberately challenge this assertion.',
            'unobservable': unknown})
        if env:
            e.update(env)
        p = subprocess.run([str(x) for x in argv], env=e, **kwargs)
        self.commands.append({'argv': [str(x) for x in argv], 'exit': p.returncode,
                              'stdout': p.stdout, 'stderr': p.stderr, 'child_umask': mask})
        if check and p.returncode:
            raise AssertionError(self.commands[-1])
        return p

    def mind(self, db, *args, binary=None, check=True):
        return self.run([binary or self.args.binary, '--db', db, *args], check=check)

    def git(self, repo, *args):
        return self.run(['git', '-C', repo, '-c', 'user.name=Recovery Fixture',
                         '-c', 'user.email=recovery@example.invalid', '-c', 'commit.gpgsign=false',
                         '-c', 'tag.gpgsign=false', *args]).stdout.strip()

    def directory(self, name):
        p = self.root / name
        p.mkdir(mode=0o700)
        return p

    def newdb(self, name, binary=None):
        folder = self.directory(name)
        db = folder / 'mind.db'
        fd = os.open(db, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        os.close(fd)  # needed only for historical binary's pre-private-creation policy
        self.mind(db, 'init', binary=binary)
        for name in ['alpha', 'beta', 'empty']:
            self.mind(db, 'space', 'create', name, binary=binary)
        return db

    def rows(self, db, sql):
        c = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
        try:
            return c.execute(sql).fetchall()
        finally:
            c.close()

    def snapshot(self, db):
        c = sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)
        try:
            return self.namespace['logical'](c)
        finally:
            c.close()

    def recipe(self, action, source, target, binary=None, check=True, mask=None, prefix=None, wrapper=None, extra_env=None):
        argv = [sys.executable, self.args.recipe, action, source, target, binary or self.args.binary]
        if wrapper:
            argv = [sys.executable, '-c', wrapper, self.args.recipe, action, str(source), str(target), binary or self.args.binary]
        if prefix:
            argv = [*prefix, *argv]
        return self.run(argv, check=check, mask=mask, env=extra_env)

    def mcp(self, db, binary=None):
        p = subprocess.Popen([binary or self.args.binary, '--db', str(db), 'mcp'],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.procs.append(p)
        count = [0]
        def request(method, params=None):
            count[0] += 1
            msg = {'jsonrpc': '2.0', 'id': count[0], 'method': method}
            if params is not None:
                msg['params'] = params
            p.stdin.write(json.dumps(msg) + '\n')
            p.stdin.flush()
            sel = selectors.DefaultSelector()
            sel.register(p.stdout, selectors.EVENT_READ)
            try:
                if not sel.select(15):
                    raise RuntimeError('MCP response timeout')
                line = p.stdout.readline()
            finally:
                sel.close()
            if not line:
                raise RuntimeError('MCP exited: ' + p.stderr.read())
            answer = json.loads(line)
            self.commands.append({'mcp_pid': p.pid, 'request': msg, 'response': answer})
            if 'error' in answer:
                raise RuntimeError(answer)
            return answer['result']
        request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                              'clientInfo': {'name': 'recovery-qualification', 'version': '1'}})
        p.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
        p.stdin.flush()
        def tool(name, args):
            result = request('tools/call', {'name': name, 'arguments': args})
            if result.get('isError'):
                raise RuntimeError(result)
            return result.get('structuredContent', json.loads(result['content'][0]['text']))
        return p, tool

    def stop(self, p, kill=False):
        if kill:
            p.kill()
        else:
            p.stdin.close()
        p.wait(timeout=15)
        self.procs.remove(p)

    def populated(self, name, binary=None):
        db = self.newdb(name, binary)
        p, tool = self.mcp(db, binary)
        one = tool('memory_create', {'subject': 'Recovery policy', 'content': 'HistoricManualToken alpha policy', 'spaces': ['alpha']})
        at = datetime.datetime.now(datetime.timezone.utc).isoformat()
        time.sleep(0.01)
        two = tool('memory_update', {'id': one['memory_id'], 'content': 'CurrentManualToken bounded policy', 'spaces': ['alpha', 'beta']})
        retired = tool('memory_create', {'subject': 'Retired recovery', 'content': 'RetiredManualToken historical', 'spaces': ['alpha']})
        tool('memory_retire', {'id': retired['memory_id'], 'reason': 'recovery fixture'})
        self.stop(p)
        folder = self.directory(name + '-repo')
        self.git(folder, 'init', '-q', '-b', 'main')
        (folder / 'doc.md').write_text('# Recovery evidence\n\nHistoricGitToken old policy.\n')
        (folder / 'lib.rs').write_text('pub fn recovery_fixture_symbol() {}\n')
        self.git(folder, 'add', '.')
        self.git(folder, 'commit', '-qm', 'old')
        old_commit = self.git(folder, 'rev-parse', 'HEAD')
        self.mind(db, 'ingest', folder, '--space', 'alpha', binary=binary)
        git_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
        time.sleep(0.01)
        (folder / 'doc.md').write_text('# Recovery evidence\n\nCurrentGitToken current policy.\n')
        self.git(folder, 'add', '.')
        self.git(folder, 'commit', '-qm', 'new')
        self.mind(db, 'ingest', folder, '--space', 'alpha', binary=binary)
        commit = self.git(folder, 'rev-parse', 'HEAD')
        blob = self.git(folder, 'rev-parse', 'HEAD:doc.md')
        note = self.directory(name + '-legacy') / 'legacy.md'
        note.write_text('# Legacy recovery\n\nLegacyRecoveryToken remains available.\n')
        self.mind(db, 'ingest', note, '--space', 'beta', binary=binary)
        return db, {'manual_at': at, 'git_at': git_at, 'manual_old_id': one['memory_id'],
                    'manual_current_id': two['memory_id'], 'retired_id': retired['memory_id'],
                    'git_commit': commit, 'git_old_commit': old_commit, 'git_blob': blob}

    def appcheck(self, db, fixture, binary=None):
        def search(token, *more):
            return json.loads(self.mind(db, 'search', token, '--space', 'alpha', '--json', *more, binary=binary).stdout)
        assert len(search('CurrentManualToken')) == 1
        assert not search('HistoricManualToken')
        assert len(search('HistoricManualToken', '--at', fixture['manual_at'])) == 1
        assert len(search('RetiredManualToken', '--include-retired')) == 1
        assert len(search('HistoricGitToken', '--at', fixture['git_at'])) == 1
        current = search('CurrentGitToken')
        assert len(current) == 1
        detail = json.loads(self.mind(db, 'inspect', current[0]['memory_id'], '--json', binary=binary).stdout)
        assert (detail['git']['commit_oid'], detail['git']['blob_oid'], detail['git']['path']) == (fixture['git_commit'], fixture['git_blob'], 'doc.md')
        assert json.loads(self.mind(db, 'search', 'CurrentGitToken', '--space', 'empty', '--json', binary=binary).stdout) == []
        p, tool = self.mcp(db, binary)
        assert tool('memory_get', {'id': fixture['manual_current_id']})['memory']['id'] == fixture['manual_current_id']
        assert tool('memory_search', {'query': 'CurrentGitToken', 'spaces': ['alpha'], 'limit': 5})['hits']
        assert len(tool('memory_history', {'subject': 'Recovery policy'})['memories']) == 2
        self.stop(p)
        return {'git_reference': detail['git'], 'manual_history': 2}

    def modes(self, path):
        result = {}
        for p in [path, *path.iterdir()]:
            result[p.name] = stat.S_IMODE(p.lstat().st_mode)
        assert result[path.name] == 0o700
        assert all(v == 0o600 for k, v in result.items() if k != path.name)
        return {k: oct(v) for k, v in result.items()}

    def clean(self, binary, name):
        db, fixture = self.populated(name, binary)
        self.appcheck(db, fixture, binary)
        before = self.snapshot(db)
        rev = self.rows(db, 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions')
        parent = self.directory(name + '-copies')
        backup = parent / 'backup'
        self.recipe('backup', db, backup, binary)
        restored = parent / 'restored'
        self.recipe('restore', backup, restored, binary)
        assert self.snapshot(restored / 'database.sqlite') == before == self.snapshot(db)
        self.appcheck(restored / 'database.sqlite', fixture, binary)
        assert self.snapshot(restored / 'database.sqlite') == before
        assert self.rows(restored / 'database.sqlite', 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions') == rev
        return {'schema': self.rows(db, 'SELECT MAX(version) FROM schema_migrations'),
                'm6': rev, 'tables_equal': len(before), 'backup_modes': self.modes(backup),
                'restore_modes': self.modes(restored), 'fixture': fixture}

    def masks(self):
        db = self.newdb('masks')
        root = self.directory('masks-copies')
        results = []
        for mask in [0o000, 0o022, 0o077]:
            backup = root / ('backup-' + oct(mask))
            self.recipe('backup', db, backup, mask=mask)
            restored = root / ('restore-' + oct(mask))
            self.recipe('restore', backup, restored, mask=mask)
            results.append({'mask': oct(mask), 'backup': self.modes(backup), 'restored': self.modes(restored)})
        return results

    def quiescence(self):
        db = self.newdb('quiescence')
        copies = self.directory('quiescence-copies')
        p, tool = self.mcp(db)
        tool('memory_create', {'content': 'QuiescenceToken', 'spaces': ['alpha']})
        rejected = self.recipe('backup', db, copies / 'held-mcp', check=False)
        assert rejected.returncode and 'descriptors still open' in rejected.stderr
        assert not (copies / 'held-mcp').exists()
        self.stop(p)
        # An inode alias is checked, not just the canonical filename.
        alias = self.root / 'held-alias'
        os.link(db, alias)
        # Restore single-link policy before backup: the child retains a descriptor
        # to the same inode even after this fixture-only alias is unlinked.
        code = 'import os,sys,time; f=open(sys.argv[1],"rb"); print("READY",flush=True); time.sleep(60)'
        holder = subprocess.Popen([sys.executable, '-c', code, str(alias)], stdout=subprocess.PIPE, text=True)
        self.procs.append(holder)
        assert holder.stdout.readline().strip() == 'READY'
        alias.unlink()
        rejected_alias = self.recipe('backup', db, copies / 'held-reader', check=False)
        assert rejected_alias.returncode and 'descriptors still open' in rejected_alias.stderr
        self.stop(holder, kill=True)
        self.recipe('backup', db, copies / 'quiet')
        return {'mcp_writer_pid_exited': p.pid, 'reader_alias_pid_exited': holder.pid,
                'writer_error': rejected.stderr, 'reader_error': rejected_alias.stderr}

    def crash_wal(self):
        db = self.newdb('crash-wal')
        p, tool = self.mcp(db)
        token = 'AcknowledgedCrashWalToken'
        created = tool('memory_create', {'content': token, 'spaces': ['alpha']})
        wal = Path(str(db) + '-wal')
        assert wal.stat().st_size > 32
        self.stop(p, kill=True)
        assert wal.exists() and wal.stat().st_size > 32
        # Generated negative control: raw MAIN-only copy omits the acknowledged WAL.
        isolated = self.directory('crash-main-only') / 'main-only.db'
        fd = os.open(isolated, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as f:
            f.write(db.read_bytes())
        raw = self.rows(isolated, "SELECT COUNT(*) FROM assertions WHERE content LIKE '%AcknowledgedCrashWalToken%'")
        assert raw == [(0,)]
        before = self.snapshot(db)
        copies = self.directory('crash-copies')
        self.recipe('backup', db, copies / 'backup')
        self.recipe('restore', copies / 'backup', copies / 'restored')
        restored = copies / 'restored/database.sqlite'
        assert self.snapshot(restored) == before
        hits = json.loads(self.mind(restored, 'search', token, '--json').stdout)
        assert hits and hits[0]['memory_id'] == created['memory_id']
        assert not Path(str(copies / 'backup/database.sqlite') + '-wal').exists()
        return {'wal_bytes': wal.stat().st_size if wal.exists() else 0, 'main_only_acknowledged_rows': raw,
                'acknowledged_memory_id': created['memory_id'], 'restored_hits': len(hits)}

    def path_refusals(self):
        db = self.newdb('path-refusals')
        before = self.snapshot(db)
        parent = self.directory('path-refusals-copies')
        results = []
        def reject(name, source, target):
            p = self.recipe('backup', source, target, check=False)
            assert p.returncode, name
            results.append({'name': name, 'error': p.stderr})
        reject('same source target', db, db)
        old = parent / 'existing'
        old.mkdir(mode=0o700)
        (old / 'precious').write_text('unchanged')
        reject('existing destination', db, old)
        assert (old / 'precious').read_text() == 'unchanged'
        link = parent / 'source-link'
        link.symlink_to(db)
        reject('source symlink', link, parent / 'source-symlink')
        targetlink = parent / 'target-link'
        targetlink.symlink_to(old)
        reject('target symlink', db, targetlink)
        parentlink = parent / 'parent-link'
        parentlink.symlink_to(parent)
        reject('symlink parent component', db, parentlink / 'bad')
        alias = parent / 'hardlink'
        os.link(db, alias)
        reject('source hardlink', db, parent / 'hardlink-refusal')
        alias.unlink()  # only remove harness-created alias, never backup/database
        unsafe = parent / 'unsafe'
        unsafe.mkdir(mode=0o777)
        unsafe.chmod(0o777)  # fixture only, before any database write
        reject('world writable parent', db, unsafe / 'bad')
        denied = parent / 'denied'
        denied.mkdir(mode=0o500)
        reject('inaccessible parent', db, denied / 'bad')
        denied.chmod(0o700)  # fixture-only test setup restored for final enumeration
        reject('relative target', db, Path('relative-refused'))
        punct = parent / 'literal space?#%'
        self.recipe('backup', db, punct)
        self.recipe('restore', punct, parent / 'literal-restored')
        assert self.snapshot(parent / 'literal-restored/database.sqlite') == before == self.snapshot(db)
        return results

    def damaged_bundles(self):
        db = self.newdb('damaged')
        parent = self.directory('damaged-copies')
        original = parent / 'valid'
        self.recipe('backup', db, original)
        preserved = {p.name: sha(p) for p in original.iterdir()}
        results = []
        for kind in ['missing-database', 'missing-manifest', 'missing-complete', 'truncated', 'corrupt', 'manifest-mismatch', 'unexpected-wal', 'hash-matched-corrupt', 'hash-matched-truncated']:
            bundle = parent / kind
            bundle.mkdir(mode=0o700)
            for source in original.iterdir():
                fd = os.open(bundle / source.name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                with os.fdopen(fd, 'wb') as f:
                    f.write(source.read_bytes())
            if kind.startswith('missing-'):
                n = {'missing-database': 'database.sqlite', 'missing-manifest': 'provenance.json', 'missing-complete': 'COMPLETE.json'}[kind]
                (bundle / n).unlink()  # remove only generated damaged copy component
            elif kind == 'truncated':
                with (bundle / 'database.sqlite').open('r+b') as f:
                    f.truncate(100)
            elif kind == 'corrupt':
                with (bundle / 'database.sqlite').open('r+b') as f:
                    f.write(b'corrupted fixture')
            elif kind == 'manifest-mismatch':
                (bundle / 'provenance.json').write_bytes(b'{}')
            elif kind.startswith('hash-matched-'):
                with (bundle / 'database.sqlite').open('r+b') as f:
                    if kind == 'hash-matched-corrupt':
                        f.write(b'corrupted fixture')
                    else:
                        f.truncate(100)
                info = json.loads((bundle / 'provenance.json').read_text())
                info['database_sha256'] = sha(bundle / 'database.sqlite')
                (bundle / 'provenance.json').write_bytes(self.namespace['encode'](info))
                (bundle / 'COMPLETE.json').write_bytes(self.namespace['encode']({'manifest_sha256': sha(bundle / 'provenance.json')}))
            else:
                fd = os.open(bundle / 'database.sqlite-wal', os.O_WRONLY | os.O_EXCL | os.O_CREAT, 0o600)
                os.close(fd)
            result = self.recipe('restore', bundle, parent / (kind + '-restored'), check=False)
            assert result.returncode
            if kind.startswith('hash-matched-'):
                assert not (parent / (kind + '-restored') / 'COMPLETE.json').exists()
            else:
                assert not (parent / (kind + '-restored')).exists()
            results.append({'kind': kind, 'error': result.stderr})
        assert {p.name: sha(p) for p in original.iterdir()} == preserved
        return results

    def interruptions(self):
        db, _ = self.populated('interruptions')
        parent = self.directory('interruptions-copies')
        before = self.snapshot(db)
        valid = parent / 'valid-backup'
        self.recipe('backup', db, valid)
        valid_hashes = {p.name: sha(p) for p in valid.iterdir()}
        results = []
        for action, source in [('backup', db), ('restore', valid)]:
            target = parent / (action + '-open-EACCES')
            trace = self.root / (action + '-open-EACCES.log')
            p = self.recipe(action, source, target, check=False,
                            prefix=['strace', '-qq', '-yy', '-o', trace,
                                    '-P', str(target / 'database.sqlite'), '-e', 'trace=openat',
                                    '-e', 'inject=openat:error=EACCES:when=1'])
            assert p.returncode and not (target / 'database.sqlite').exists()
            assert 'INJECTED' in trace.read_text() and 'EACCES' in trace.read_text()
            assert not (target / 'COMPLETE.json').exists()
            self.recipe(action, source, parent / (action + '-open-EACCES-retry'))
            results.append({'fault': action + ' exclusive target create EACCES', 'error': p.stderr, 'trace': str(trace)})
        for action, source in [('backup', db), ('restore', valid)]:
            for inject in ['error=ENOSPC', 'error=EACCES', 'signal=SIGKILL']:
                label = action + '-' + inject.split('=')[1]
                trace = self.root / ('trace-' + label + '.log')
                target = parent / label
                p = self.recipe(action, source, target, check=False,
                                prefix=['strace', '-qq', '-yy', '-o', trace,
                                        '-P', str(target / 'database.sqlite'),
                                        '-P', str(target / 'database.sqlite-journal'),
                                        '-e', 'trace=pwrite64', '-e', 'inject=pwrite64:' + inject + ':when=1'])
                assert p.returncode and not (target / 'COMPLETE.json').exists()
                trace_text = trace.read_text()
                assert 'INJECTED' in trace_text or 'SIGKILL' in trace_text
                assert str(target) in trace_text
                assert self.snapshot(db) == before
                assert {p.name: sha(p) for p in valid.iterdir()} == valid_hashes
                self.recipe(action, source, parent / (label + '-retry'))
                results.append({'fault': label, 'exit': p.returncode, 'error': p.stderr, 'trace': str(trace),
                                'partial_modes': {p.name: oct(stat.S_IMODE(p.lstat().st_mode)) for p in target.iterdir()}})
        wrapper = 'import sys,os; n={"__name__":"recipe_test"}; exec(compile(open(sys.argv[1]).read(),sys.argv[1],"exec"),n); n["transfer"](*sys.argv[2:]); os._exit(73)'
        for action, source in [('backup', db), ('restore', valid)]:
            ambiguous = parent / (action + '-ack-lost')
            p = self.recipe(action, source, ambiguous, check=False, wrapper=wrapper)
            assert p.returncode == 73 and (ambiguous / 'COMPLETE.json').exists()
            self.recipe('restore', ambiguous, parent / (action + '-ack-lost-validated'))
            assert self.snapshot(parent / (action + '-ack-lost-validated/database.sqlite')) == before
            retry = self.recipe(action, source, ambiguous, check=False)
            assert retry.returncode
            results.append({'fault': action + ' completion acknowledgment lost', 'exit': p.returncode,
                            'full_revalidation_and_distinct_restore': True, 'same_target_retry_refused': retry.stderr})
        for action, source in [('backup', db), ('restore', valid)]:
            target = parent / (action + '-parent-fsync-error')
            trace = self.root / (action + '-parent-fsync-error.log')
            p = self.recipe(action, source, target, check=False,
                            prefix=['strace', '-qq', '-yy', '-o', trace, '-P', str(parent),
                                    '-e', 'trace=fsync', '-e', 'inject=fsync:error=EIO:when=1'])
            assert p.returncode and (target / 'COMPLETE.json').exists()
            assert 'EIO' in trace.read_text() and 'INJECTED' in trace.read_text()
            self.recipe('restore', target, parent / (action + '-parent-fsync-revalidated'))
            assert self.snapshot(parent / (action + '-parent-fsync-revalidated/database.sqlite')) == before
            results.append({'fault': action + ' parent directory fsync EIO after completion',
                            'error': p.stderr, 'trace': str(trace), 'revalidated_before_use': True})
        return results

    def remaining_contracts(self):
        db = self.newdb('remaining-contracts')
        parent = self.directory('remaining-contracts-copies')
        original = self.snapshot(db)
        noack = self.recipe('backup', db, parent / 'no-inventory', check=False,
                            extra_env={'MINDSPACE_RECOVERY_QUIESCENCE': '{}'})
        assert noack.returncode and 'operator inventory' in noack.stderr
        assert not (parent / 'no-inventory').exists()
        valid = parent / 'valid'
        self.recipe('backup', db, valid)
        wrongbinary = self.recipe('restore', valid, parent / 'wrong-binary', binary=self.args.production, check=False)
        assert wrongbinary.returncode and 'binary does not match' in wrongbinary.stderr
        assert not (parent / 'wrong-binary').exists()
        failures = []
        for label, statement in [('future-schema', 'UPDATE schema_migrations SET version=99 WHERE version=17'),
                                 ('noncontiguous-schema', 'DELETE FROM schema_migrations WHERE version=16')]:
            work = parent / label
            self.recipe('restore', valid, work)
            candidate = work / 'database.sqlite'
            c = sqlite3.connect(candidate)
            try:
                c.execute(statement)
                c.commit()
            finally:
                c.close()
            digest_before = sha(candidate)
            p = self.recipe('backup', candidate, parent / (label + '-backup'), check=False)
            assert p.returncode and 'unsupported or noncontiguous schema' in p.stderr
            assert not (parent / (label + '-backup') / 'COMPLETE.json').exists()
            assert sha(candidate) == digest_before
            failures.append({'case': label, 'error': p.stderr})
        badparent = self.directory('corrupt-source')
        corrupt = badparent / 'mind.db'
        fd = os.open(corrupt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as f:
            f.write(b'not a SQLite database')
        badhash = sha(corrupt)
        p = self.recipe('backup', corrupt, parent / 'corrupt-source-backup', check=False)
        assert p.returncode and 'file is not a database' in p.stderr and sha(corrupt) == badhash
        failures.append({'case': 'corrupt-source', 'error': p.stderr})
        # Function-return regression: sqlite3's transaction context alone does
        # NOT close connections. The operator recipe must explicitly close it.
        wrapper = ("import sys,os,pathlib; n={'__name__':'recipe_test'}; "
                   "exec(compile(open(sys.argv[1]).read(),sys.argv[1],'exec'),n); "
                   "n['transfer'](*sys.argv[2:]); s=pathlib.Path(sys.argv[3]).stat(); "
                   "fds=[p for p in pathlib.Path('/proc/self/fd').iterdir() if p.exists()]; "
                   "assert not any((p.stat().st_dev,p.stat().st_ino)==(s.st_dev,s.st_ino) for p in fds if p.exists()),'source FD leaked'")
        self.recipe('backup', db, parent / 'closed-return1', wrapper=wrapper)
        self.recipe('backup', db, parent / 'closed-return2', wrapper=wrapper)
        assert self.snapshot(db) == original
        return {'missing_inventory_refused': noack.stderr, 'wrong_binary_refused': wrongbinary.stderr,
                'source_failures': failures, 'source_connection_closed_at_function_return': True}

    def harness_log_safety(self):
        db = self.newdb('log-safety')
        original = sha(db)
        before = {p.name for p in Path(self.args.work_parent).iterdir()}
        base = [sys.executable, str(Path(__file__).resolve()), '--work-parent', self.args.work_parent,
                '--recipe', self.args.recipe, '--binary', self.args.binary,
                '--production', self.args.production, '--old-binary', self.args.old_binary]
        logs = [self.report, db]
        alias = self.root / 'log-symlink'
        alias.symlink_to(db)
        logs.append(alias)
        observations = []
        for target in logs:
            result = self.run([*base, '--log', target], check=False)
            assert result.returncode and 'File exists' in result.stderr
            assert {p.name for p in Path(self.args.work_parent).iterdir()} == before
            assert sha(db) == original
            observations.append({'target': str(target), 'error': result.stderr})
        return observations

    def sealed_bundle_safety(self):
        db = self.newdb('sealed-bundle')
        parent = self.directory('sealed-bundle-copies')
        first, other = parent / 'first', parent / 'other'
        self.recipe('backup', db, first)
        self.recipe('backup', db, other)
        preserved = {str(b): {p.name: sha(p) for p in b.iterdir()} for b in [first, other]}
        errors = []
        for action in ['backup', 'restore']:
            source = first / 'database.sqlite' if action == 'backup' else first
            for label, sealed in [('same-input-bundle', first), ('other-bundle', other)]:
                target = sealed / ('nested-' + action)
                result = self.recipe(action, source, target, check=False)
                assert result.returncode and 'inside a sealed or partial recovery bundle' in result.stderr
                assert not target.exists()
                for b in [first, other]:
                    assert {p.name: sha(p) for p in b.iterdir()} == preserved[str(b)]
                errors.append({'action': action, 'target_kind': label, 'error': result.stderr})
        self.recipe('restore', first, parent / 'sibling-restored')
        assert self.snapshot(parent / 'sibling-restored/database.sqlite') == self.snapshot(db)
        for b in [first, other]:
            assert {p.name: sha(p) for p in b.iterdir()} == preserved[str(b)]
        return {'refusals': errors, 'all_sealed_entries_and_bytes_unchanged': True,
                'subsequent_sibling_restore_succeeded': True}

    def historical(self):
        old = self.args.old_binary
        db, fixture = self.populated('schema16', old)
        assert self.rows(db, 'SELECT MAX(version) FROM schema_migrations') == [(16,)]
        before = self.snapshot(db)
        old_revisions = self.rows(db, 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions')[0]
        parent = self.directory('schema16-copies')
        original = parent / 'preupgrade'
        self.recipe('backup', db, original, old)
        backup_hash = sha(original / 'database.sqlite')
        self.recipe('restore', original, parent / 'upgrade-clone', old)
        upgraded = parent / 'upgrade-clone/database.sqlite'
        self.mind(upgraded, 'init')
        assert self.rows(upgraded, 'SELECT MAX(version) FROM schema_migrations') == [(17,)]
        self.appcheck(upgraded, fixture)
        upgraded_revisions = self.rows(upgraded, 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions')[0]
        assert upgraded_revisions == (old_revisions[0] + 1, old_revisions[1] + 1)
        # Preserve acknowledged post-backup state separately: rollback is a
        # point-in-time choice, never recovery of these later writes.
        process, tool = self.mcp(upgraded)
        later = tool('memory_create', {'content': 'AcknowledgedPostBackupToken', 'spaces': ['alpha']})
        self.stop(process)
        assert json.loads(self.mind(upgraded, 'search', 'AcknowledgedPostBackupToken', '--json').stdout)
        self.recipe('restore', original, parent / 'restored-old', old)
        self.appcheck(parent / 'restored-old/database.sqlite', fixture, old)
        assert self.snapshot(parent / 'restored-old/database.sqlite') == before
        assert not json.loads(self.mind(parent / 'restored-old/database.sqlite', 'search', 'AcknowledgedPostBackupToken', '--json', binary=old).stdout)
        assert json.loads(self.mind(upgraded, 'search', 'AcknowledgedPostBackupToken', '--json').stdout)
        # Synthetic damaged clone: CREATE TABLE collides with migration17.
        self.recipe('restore', original, parent / 'failed-clone', old)
        failed = parent / 'failed-clone/database.sqlite'
        c = sqlite3.connect(failed)
        c.execute('CREATE TABLE asset_fence_plans (synthetic_collision TEXT)')
        c.commit()
        c.close()
        fault_before = self.snapshot(failed)
        failure = self.mind(failed, 'init', check=False)
        assert failure.returncode and 'already exists' in failure.stderr
        assert self.rows(failed, 'SELECT MAX(version) FROM schema_migrations') == [(16,)]
        assert self.snapshot(failed) == fault_before
        failure_again = self.mind(failed, 'init', check=False)
        assert failure_again.returncode and self.snapshot(failed) == fault_before
        self.recipe('restore', original, parent / 'failed-recovered', old)
        self.appcheck(parent / 'failed-recovered/database.sqlite', fixture, old)
        assert self.snapshot(parent / 'failed-recovered/database.sqlite') == before
        assert sha(original / 'database.sqlite') == backup_hash
        writable = parent / 'failed-recovered/database.sqlite'
        restored_revisions = self.rows(writable, 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions')[0]
        process, tool = self.mcp(writable, old)
        after_restore = tool('memory_create', {'content': 'PostRestoreAllocatorToken', 'spaces': ['alpha']})
        self.stop(process)
        postwrite_revisions = self.rows(writable, 'SELECT COUNT(*),MAX(revision_id) FROM m6_revisions')[0]
        assert postwrite_revisions == (restored_revisions[0] + 1, restored_revisions[1] + 1)
        assert json.loads(self.mind(writable, 'search', 'PostRestoreAllocatorToken', '--json', binary=old).stdout)
        assert self.rows(writable, 'PRAGMA integrity_check') == [('ok',)] and self.rows(writable, 'PRAGMA foreign_key_check') == []
        assert sha(original / 'database.sqlite') == backup_hash
        return {'origin': '254d1ad5794db8ac19dc187a5934910337da9b9e', 'old_binary_sha256': sha(old),
                'upgrade': '16->17', 'restored_preupgrade_schema': 16,
                'm6_before_upgrade': old_revisions, 'm6_after_metadata_upgrade': upgraded_revisions,
                'postbackup_acknowledged_memory_preserved_in_upgraded_clone': later['memory_id'],
                'postbackup_token_absent_from_preupgrade_snapshot': True,
                'm6_before_postrestore_write': restored_revisions, 'm6_after_postrestore_write': postwrite_revisions,
                'postrestore_memory_id': after_restore['memory_id'],
                'failed_migration_error': failure.stderr, 'repeated_failure_error': failure_again.stderr,
                'source_and_backup_unchanged': self.snapshot(db) == before}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--work-parent', required=True)
    p.add_argument('--log', required=True)
    p.add_argument('--recipe', required=True)
    p.add_argument('--binary', required=True)
    p.add_argument('--production', required=True)
    p.add_argument('--old-binary', required=True)
    args = p.parse_args()
    parent = Path(args.work_parent)
    namespace = {'__name__': 'offline_recipe_validation'}
    exec(compile(Path(args.recipe).read_text(), args.recipe, 'exec'), namespace)
    namespace['parents'](parent)
    report = Path(args.log)
    namespace['parents'](report.parent)
    if not report.is_absolute():
        raise SystemExit('log must be a new absolute path')
    # Reserve before creating ANY fixture. Existing/symlink logs are never opened.
    logfd = os.open(report, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    suite = Suite(args, logfd)
    suite.case('current-no-embeddings-populated', lambda: suite.clean(args.binary, 'current-noembed'))
    suite.case('current-production-populated', lambda: suite.clean(args.production, 'current-production'))
    suite.case('private-modes-three-child-umasks', suite.masks)
    suite.case('held-writer-reader-and-inode-alias', suite.quiescence)
    suite.case('acknowledged-crash-wal-and-main-only-negative', suite.crash_wal)
    suite.case('wrong-target-path-symlink-hardlink-permission-refusals', suite.path_refusals)
    suite.case('missing-corrupt-truncated-backup-refusals', suite.damaged_bundles)
    suite.case('io-interruption-and-ambiguous-completion', suite.interruptions)
    suite.case('explicit-inventory-schema-binary-and-source-close-contracts', suite.remaining_contracts)
    suite.case('exclusive-harness-log-safety', suite.harness_log_safety)
    suite.case('sealed-bundle-containment-and-preservation', suite.sealed_bundle_safety)
    suite.case('actual-schema16-upgrade-failure-and-preupgrade-restore', suite.historical)
    return 1 if any(x['status'] != 'PASS' for x in suite.cases) else 0


if __name__ == '__main__':
    raise SystemExit(main())
