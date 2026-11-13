# Offline backup and pre-upgrade recovery

Mindspace has no native database backup/restore command. This is a bounded
cooperative offline operator procedure using Python's existing SQLite backup API.
Ordinary Mindspace opens can migrate a database, including queries and `init`.
**Before any new binary opens existing data, STOP, preserve the exact old binary
and make a validated offline pre-upgrade backup.** The demo backup in the
[source-install guide](source-install.md) uses the binary that created its fresh
demo; it is not a substitute for preserving your actual old executable.

## Historical qualification and limits

This sanitized runbook retains the accepted recipe and operator constraints.
Historical qualification used Linux 7.2.14-zen1-1.1-zen on controlled local btrfs,
Python 3.14.10 with SQLite 3.54.1, and ordinary Mindspace builds using bundled
SQLite 3.53.2 and Rust/Cargo 1.98.1. It exercised current schema17 private storage
and the exact schema16 development origin
`254d1ad5794db8ac19dc187a5934910337da9b9e` upgrading to schema17. One author's and
one independent run each reported 12 passing groups, 0 failures and 300 commands.
These are historical observations, not tests newly run for this curated source
archive; historical binaries, private evidence and Git history are not included.
Other historical origins or manually modified schema16 databases remain
unsupported/unqualified. The package version alone does not identify a qualified
binary or database origin.

The retained [qualification harness](../scripts/qualify-alpha-recovery.py) accepts
only separately supplied exact binary/recipe paths and fresh fixture/log locations,
not an existing user database. It needs Git and strace in addition to runtime
prerequisites. Having its source in this archive does not establish that its
historical old binary or environment is available.

## Quiescence and storage contract

Account for every CLI, MCP and embedded-library reader/writer, stop them all and
prevent restarts before backup/upgrade/recovery. Keep clients stopped until
validation and explicit selection of the working path. A process-name search or
missing receipt does not prove exclusion. The recipe scans visible database-family
file descriptors by inode, including aliases; that cannot exclude hidden or future
processes. Unobservable potentially relevant processes are a **STOP**. A per-run
PID/starttime classification of an actually accounted unrelated process is a trusted
operator assertion, not proof that it lacks a database descriptor. Do not add a
blanket exemption, infer safety from names or classify hidden processes automatically.
Hostile/uncooperative same-UID/root actors and hot backup are outside this contract.

Use canonical absolute paths, controlled ancestors and private regular single-link
source files. Fresh output directories are0700 and files0600; the recipe refuses
unsafe ownership, aliases and existing destinations. Do not chmod existing data to
bypass refusal. New application DB/WAL/SHM files are0600; existing permissive files
are not silently changed. Storage is not encrypted and deletion is not secure erasure.

Keep the source's MAIN/WAL/SHM family intact. A crash can leave acknowledged commits
in WAL. Do not delete sidecars or repair by copying only live MAIN. Missing required
WAL cannot generally be detected from MAIN alone: known or uncertain lost sidecars
are a **STOP**. SQLite read-only inspection may create private empty coordination
WAL/SHM files; it does not establish quiescence. A validated complete backup is a
standalone SQLite file with its required provenance and completion evidence.

A backup captures one recovery point. If clients accepted any post-backup writes,
**STOP automatic rollback and preserve both states** for an explicit preservation
decision. Integrity checks do not authorize discarding later writes.

## Operator runbook

This procedure is limited to the private/cooperative profile and exact historical schema pairing described above. It is an operational recipe, not a new Mindspace backup command. The tested commands use Bash/GNU coreutils, Python3 with SQLite backup support, and the preserved ordinary Mindspace binaries; the qualification harness additionally uses local Git and strace. Historical qualification used the Python/SQLite pairing above; other pairings are unqualified. Missing prerequisites or any STOP condition means do not proceed. Use one dedicated Bash shell with `set -eu` for these blocks; a failed command is a STOP, not permission to continue to the next step.

1. Record the physical absolute database path and exact installed binary from the existing client configuration. Stop every relevant CLI/MCP/embedded-library program and prevent restart, including agent-managed MCP restarts. Account for relevant PIDs/connections and classify any invisible processes; leave clients stopped throughout. Preserve the old binary at a stable path and record known memory IDs, current/historical tokens, space and Git source references for later readback. **Do not run an ordinary Mindspace query/init on the original as a pre-upgrade inspection.**
2. Reserve a fresh private work directory under a trusted local parent. Set `REPORT` to this runbook's absolute path. The validated extraction below saves the exact appendix into an exclusive0600 file. Do not reuse a work directory/recipe filename. `RECOVERY_PARENT` must exist and have controlled ancestors; the new work directory must be0700. No existing directories are chmodded.

```sh
set -eu
RECOVERY_PARENT=/absolute/existing/trusted/local/parent
REPORT=/absolute/path/to/docs/offline-recovery.md
WORK=$(mktemp -d "$RECOVERY_PARENT/mindspace-recovery.XXXXXXXX")
python3 - "$REPORT" "$WORK/offline-recipe.py" <<'PYRECIPE'
import os, sys
from pathlib import Path
text = Path(sys.argv[1]).read_text()
start = '<!-- BEGIN QUALIFIED OFFLINE RECIPE -->\n```python\n'
end = '```\n<!-- END QUALIFIED OFFLINE RECIPE -->'
assert text.count(start) == text.count(end) == 1
code = text.split(start, 1)[1].split(end, 1)[0]
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
with os.fdopen(fd, 'w') as f:
    f.write(code)
PYRECIPE
```

3. Set canonical absolute `DB` and preserved `OLD_MIND` paths. Record the **actual** inventory/restart-prevention statement in the per-invocation JSON below. Its example text is a placeholder, not an exclusion mechanism. Initially leave `unobservable` empty; if the recipe reports an unreadable PID/starttime, STOP and assess it. Only a positively classified unrelated process may be listed as `{"pid":123,"starttime":"EXACT_REPORTED_VALUE","classification":"operator-classified-unrelated"}`. Do not automatically copy all hidden PIDs into this list. On identity change, reassess. Successful backup validates integrity/FK/schema/all-table equality and creates a standalone DB plus provenance/COMPLETE last, with file, directory and parent synchronization.

```sh
DB=/absolute/canonical/private/mind.db
OLD_MIND=/absolute/preserved/old/mind
export MINDSPACE_RECOVERY_QUIESCENCE='{"contract":"all-clients-stopped-and-restarts-prevented","operator_assertion":"REPLACE WITH ACTUAL STOPPED CLIENT/PID AND RESTART INVENTORY","unobservable":[]}'
python3 "$WORK/offline-recipe.py" backup "$DB" "$WORK/pre-upgrade-1" "$OLD_MIND"
```

4. **Keep `pre-upgrade-1` immutable. Never open its `database.sqlite` with Mindspace.** Ordinary opens can migrate/change WAL mode and invalidate its saved hash. Upgrade a separate restored working copy; no original database or backup is overwritten. `NEW_MIND` below is the qualified target binary, not an arbitrary future version.

```sh
python3 "$WORK/offline-recipe.py" restore "$WORK/pre-upgrade-1" "$WORK/upgrade-candidate-1" "$OLD_MIND"
NEW_MIND=/absolute/path/to/qualified/new/mind
"$NEW_MIND" --db "$WORK/upgrade-candidate-1/database.sqlite" init
```

5. Before resuming clients, verify expected current/as-of/retired/scoped memories, full Git source references and schema/M6 results against the recorded recovery point; use actual known tokens/IDs/timestamps, not the example placeholders. Historical qualification exercised `search --json`, `--at`, `--include-retired`, `inspect --json` and real MCP memory_get/search/history. Integrity/FK and full-table checks run in the recipe before the application opens the working copy. Example readback syntax:

```sh
"$NEW_MIND" --db "$WORK/upgrade-candidate-1/database.sqlite" search 'YOUR_KNOWN_TOKEN' --space YOUR_SPACE --json
"$NEW_MIND" --db "$WORK/upgrade-candidate-1/database.sqlite" inspect YOUR_KNOWN_MEMORY_ID --json
```

6. On failed upgrade or uncertain outcome, keep original, candidate, logs and backup. Do not delete sidecars or retry over an existing destination. Provided no post-backup writes need preservation, restore a **new** pre-upgrade working copy and verify it with the exact preserved old binary. If later writes occurred, STOP and preserve both states for a recovery decision.

```sh
python3 "$WORK/offline-recipe.py" restore "$WORK/pre-upgrade-1" "$WORK/recovered-1" "$OLD_MIND"
"$OLD_MIND" --db "$WORK/recovered-1/database.sqlite" search 'YOUR_KNOWN_TOKEN' --space YOUR_SPACE --json
```

7. Only after successful validation, deliberately select the verified working path and correct binary for CLI/MCP configuration, then resume clients once. Configuration switching itself is not performed/qualified by this task; native-client release acceptance remains open. A working copy's original manifest records its initial restored state; after application opens/writes it is **not** an immutable backup bundle. Preserve the original validated `pre-upgrade-1` unchanged. On any error, missing success output or interruption, do not infer rollback: revalidate by restoring the apparently completed bundle into another fresh directory, or retry the original operation into another fresh directory. Never overwrite/delete earlier results.

## Failure and STOP conditions

- Unaccounted CLI/MCP/library connections; automatic restart still enabled; scan shows a live database-family reference; an unobservable potentially relevant process cannot be classified; required platform/process information unavailable.
- Ambiguous/noncanonical or symlink/hardlink paths, unsafe/private-mode or ownership checks, source equal to target, pre-existing destination or unexpected sidecars/components. Do not chmod/delete originals to make the recipe proceed.
- Corrupt/truncated/missing/mismatched backup, missing completion evidence, unsupported schema/origin, missing exact preserved binary, failed integrity/FK/logical validation, or known lost source WAL. Do not repair or delete sidecars as a shortcut.
- Permission, full-space, I/O or fsync errors; interruption before validation/completion; an ambiguous result. Preserve original/bundle/partial output. Revalidate any apparently completed result, and retry into another unique location rather than overwrite or assume rollback.
- Any acknowledged post-backup writes without an explicit preservation decision, or any ambiguity about which DB path/binary the client will use after recovery.
- Publication/admin-path obligations outside this legacy recovery profile, hostile local processes, unsupported filesystems, real power-loss/hardware guarantees and unqualified hot backup.

An error, interruption or lost acknowledgment can leave a partial or complete
output. `CommitOutcomeUnknown` and completed output followed by a synchronization
error must not be called rollback. Preserve all states and retry or revalidate in
another fresh directory. Keep validated backups immutable; open only a separately
restored working copy. An old binary must not open an upgraded file as a purported
downgrade. This procedure does not transfer inode/path-bound administration or
publication authority, repair private staging, or qualify canonical publication.
Physical whole-filesystem exhaustion, power loss/hardware behavior, arbitrary
filesystems/containers/Linux versions and general historical compatibility remain
unqualified. No application writer fence or mandatory external lock is introduced.

## Exact qualified offline recipe

The recipe below is unchanged: 11,143 UTF-8 bytes, SHA256
`e9c5de62f4b896946026a2cf61aef4f3cdd4057dc1a023b49e35279dd15f4ff5`.
Preserve its markers when extracting it. No new execution is claimed here.

<!-- BEGIN QUALIFIED OFFLINE RECIPE -->
```python
#!/usr/bin/env python3
"""Qualified offline operator recipe, not a Mindspace command or hot-backup API.

Run only after stopping/accounting for ALL clients and preventing restarts. Local
Linux filesystem, trusted operating UID/root, private canonical paths only.
Usage: python3 offline-recipe.py backup SOURCE.db NEW_DIRECTORY OLD_BINARY
       python3 offline-recipe.py restore COMPLETE_BUNDLE NEW_DIRECTORY OLD_BINARY
OLD_BINARY is preserved provenance, not automatically executed by this recipe.
Any failure: STOP; keep original and partial directory; retry a NEW directory.
"""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import time


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def parents(path):
    p = Path(path)
    if not p.is_absolute() or p.resolve(strict=True) != p:
        raise RuntimeError('STOP: require existing canonical absolute path, no symlinks')
    for a in (p, *p.parents):
        m = a.stat()
        if not stat.S_ISDIR(m.st_mode) or m.st_uid not in (0, os.geteuid()) or m.st_mode & 0o022:
            raise RuntimeError('STOP: uncontrolled or writable parent: ' + str(a))
    if p.stat().st_uid != os.geteuid() or stat.S_IMODE(p.stat().st_mode) != 0o700:
        raise RuntimeError('STOP: immediate directory must be owned0700')
    return p


def private_file(path):
    p = Path(path)
    parents(p.parent)
    m = p.lstat()
    if (not stat.S_ISREG(m.st_mode) or m.st_uid != os.geteuid()
            or stat.S_IMODE(m.st_mode) != 0o600 or m.st_nlink != 1):
        raise RuntimeError('STOP: file must be regular, owned0600, single-link: ' + str(p))
    return p


def family(path):
    files = []
    for suffix in ('', '-wal', '-shm', '-journal'):
        p = Path(str(path) + suffix)
        if p.exists() or p.is_symlink():
            private_file(p)
            files.append(p)
    return files


def process_identity(proc):
    fields = (proc / 'stat').read_text().rsplit(') ', 1)[1].split()
    return {'pid': int(proc.name), 'starttime': fields[19]}


def quiescence(path):
    """Supplement only: cannot exclude later, hidden or hostile clients."""
    wanted = {(p.stat().st_dev, p.stat().st_ino) for p in family(path)}
    ack = json.loads(os.environ.get('MINDSPACE_RECOVERY_QUIESCENCE', '{}'))
    if ack.get('contract') != 'all-clients-stopped-and-restarts-prevented' or not ack.get('operator_assertion'):
        raise RuntimeError('STOP: explicit operator inventory/restart-prevention assertion required')
    held, unobservable = [], []
    for proc in Path('/proc').glob('[0-9]*'):
        try:
            if int(proc.name) == os.getpid() or proc.stat().st_uid != os.geteuid():
                continue
            for fd in (proc / 'fd').iterdir():
                try:
                    m = fd.stat()
                    if (m.st_dev, m.st_ino) in wanted:
                        held.append([int(proc.name), int(fd.name)])
                except FileNotFoundError:
                    pass
        except FileNotFoundError:
            pass  # process exited during this observation
        except PermissionError:
            # Identity acknowledgments are operator assertions, never FD-absence proof.
            identity = process_identity(proc)
            permitted = [x for x in ack.get('unobservable', [])
                         if x.get('pid') == identity['pid'] and x.get('starttime') == identity['starttime']
                         and x.get('classification') == 'operator-classified-unrelated']
            if not permitted:
                raise RuntimeError('STOP: unclassified unobservable process: ' + json.dumps(identity))
            unobservable.append(identity)
    if held:
        raise RuntimeError('STOP: database-family descriptors still open: ' + json.dumps(held))
    for identity in unobservable:
        try:
            if process_identity(Path('/proc') / str(identity['pid'])) != identity:
                raise RuntimeError('STOP: unobservable process identity changed')
        except FileNotFoundError:
            raise RuntimeError('STOP: process inventory changed; reassess')
    return {'contract': ack['contract'], 'operator_assertion': ack['operator_assertion'],
            'unobservable_classified_unrelated': unobservable,
            'limit': 'cooperative operator assertion; not universal FD visibility or future-writer exclusion'}


def readonly(path):
    return sqlite3.connect(Path(path).as_uri() + '?mode=ro', uri=True, timeout=2)


def logical(c):
    """Canonical all-table digest; values remain private, never printed."""
    result = {}
    for name, sql in c.execute("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"):
        q = '"' + name.replace('"', '""') + '"'
        rows = sorted(repr(row) for row in c.execute('SELECT * FROM ' + q))
        result[name] = {'rows': len(rows), 'sha256': hashlib.sha256((str(sql) + '\n' + '\n'.join(rows)).encode()).hexdigest()}
    return result


def validate(c):
    if c.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
        raise RuntimeError('STOP: integrity_check failed')
    if c.execute('PRAGMA foreign_key_check').fetchall():
        raise RuntimeError('STOP: foreign_key_check failed')
    versions = [v for v, in c.execute('SELECT version FROM schema_migrations ORDER BY version')]
    if versions not in [list(range(1, 17)), list(range(1, 18))]:
        raise RuntimeError('STOP: unsupported or noncontiguous schema')
    return versions


def write_private(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def load_bundle(bundle):
    b = parents(Path(bundle))
    expected = {'database.sqlite', 'provenance.json', 'COMPLETE.json'}
    if {p.name for p in b.iterdir()} != expected:
        raise RuntimeError('STOP: incomplete bundle or unexpected component')
    for name in expected:
        private_file(b / name)
    complete = json.loads((b / 'COMPLETE.json').read_bytes())
    if complete['manifest_sha256'] != digest(b / 'provenance.json'):
        raise RuntimeError('STOP: provenance digest mismatch')
    info = json.loads((b / 'provenance.json').read_bytes())
    if info['database_sha256'] != digest(b / 'database.sqlite'):
        raise RuntimeError('STOP: database digest mismatch')
    return b / 'database.sqlite', info


def transfer(action, source, target, binary):
    if action not in ('backup', 'restore'):
        raise RuntimeError('STOP: action must be backup or restore')
    prior = None
    if action == 'restore':
        source, prior = load_bundle(source)
    source = private_file(Path(source))
    family(source)
    observed_quiescence = quiescence(source)
    binary = Path(binary)
    if not binary.is_absolute() or binary.resolve(strict=True) != binary or not binary.is_file():
        raise RuntimeError('STOP: preserve exact canonical old binary')
    binary_hash = digest(binary)
    if prior and binary_hash != prior['binary_sha256']:
        raise RuntimeError('STOP: old binary does not match backup provenance')
    target = Path(target)
    parents(target.parent)
    if not target.is_absolute() or target.name in ('', '.', '..'):
        raise RuntimeError('STOP: invalid target directory')
    # A bundle is sealed, including against adding new nested directories.
    # This also protects another retained bundle accidentally chosen as parent.
    if any((a / name).exists() or (a / name).is_symlink()
           for a in target.parents for name in ('COMPLETE.json', 'provenance.json')):
        raise RuntimeError('STOP: target is inside a sealed or partial recovery bundle')
    # mkdir is exclusive: an existing file, directory or symlink is never touched.
    os.mkdir(target, 0o700)
    parents(target)
    output = target / 'database.sqlite'
    write_private(output, b'')
    source_main_before = digest(source)
    source_wal = Path(str(source) + '-wal')
    wal_before = digest(source_wal) if source_wal.exists() and source_wal.stat().st_size else None
    with closing(readonly(source)) as src:
        src.execute('BEGIN')  # one coherent read snapshot for checks and backup
        versions = validate(src)
        expected = logical(src)
        if prior and (versions != prior['schema_versions'] or expected != prior['tables']):
            raise RuntimeError('STOP: restored source does not match recorded contents')
        dst = sqlite3.connect(output.as_uri() + '?mode=rw', uri=True, timeout=2)
        try:
            src.backup(dst, pages=64)
            # Make the completed output self-contained: no backup WAL dependency.
            if dst.execute('PRAGMA journal_mode=DELETE').fetchone() != ('delete',):
                raise RuntimeError('STOP: output journal-mode normalization failed')
            if validate(dst) != versions or logical(dst) != expected:
                raise RuntimeError('STOP: backup differs from source snapshot')
        finally:
            dst.close()
    if digest(source) != source_main_before or (digest(source_wal) if source_wal.exists() and source_wal.stat().st_size else None) != wal_before:
        raise RuntimeError('STOP: source MAIN/WAL changed; quiescence not established')
    if any(Path(str(output) + s).exists() for s in ('-wal', '-shm', '-journal')):
        raise RuntimeError('STOP: backup unexpectedly needs auxiliary files')
    family(source)  # read-only SQLite may create private empty WAL/SHM; no deletion
    private_file(output)
    with output.open('rb') as f:
        os.fsync(f.fileno())
    info = {'recipe': 'mindspace-offline-recovery-v1', 'action': action,
            'source': str(source), 'source_main_sha256': source_main_before,
            'source_wal_sha256': wal_before, 'schema_versions': versions,
            'tables': expected, 'database_sha256': digest(output),
            'binary': str(binary), 'binary_sha256': binary_hash,
            'quiescence': observed_quiescence,
            'python_sqlite': sqlite3.sqlite_version, 'time_ns': time.time_ns()}
    write_private(target / 'provenance.json', encode(info))
    sync_dir(target)
    write_private(target / 'COMPLETE.json', encode({'manifest_sha256': digest(target / 'provenance.json')}))
    sync_dir(target)
    sync_dir(target.parent)  # persist the newly created directory entry, too
    return info


if __name__ == '__main__':
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    result = transfer(*sys.argv[1:])
    print(json.dumps({'complete_bundle': sys.argv[3], 'schema': result['schema_versions'][-1], 'sha256': result['database_sha256']}))
```
<!-- END QUALIFIED OFFLINE RECIPE -->
