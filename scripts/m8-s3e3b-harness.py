#!/usr/bin/env python3
"""Build pinned Mindspace binaries and run the opt-in S3e.3b Cargo targets.

PASS of a known_unsafe_characterization means the unsafe behavior reproduced.
No production database is accepted as an argument. Outputs contain synthetic data.
"""

import argparse
import ctypes
import hashlib
import io
import json
import os
import platform
import shutil
import signal
import sqlite3
import subprocess
import sys
import tarfile
import time
from pathlib import Path

OLD = "254d1ad5794db8ac19dc187a5934910337da9b9e"
CURRENT = "91c70b933a2fd769316e2c9b26e52df99d018c00"
TARGETS = ("old_client_adversarial", "sqlite_adversarial")
OVERLAY = (
    "tests/old_client_adversarial.rs",
    "tests/sqlite_adversarial.rs",
    "tests/support/adversarial_common.rs",
    "scripts/m8-s3e3b-harness.py",
)
REPO = Path(__file__).resolve().parent.parent
ACTIVE = None


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def descendants(pid):
    """Only processes descended from our owned child; never match by name."""
    children = {}
    for stat in Path("/proc").glob("[0-9]*/stat"):
        try:
            fields = stat.read_text().rsplit(") ", 1)[1].split()
            children.setdefault(int(fields[1]), []).append(int(stat.parent.name))
        except (OSError, ValueError, IndexError):
            continue
    found, queue = [], [pid]
    while queue:
        parent = queue.pop()
        for child in children.get(parent, []):
            found.append(child)
            queue.append(child)
    return found


def terminate(process):
    # Test helpers use their own groups. Include those descendants on interruption.
    owned = descendants(process.pid)
    for pid in reversed(owned):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)
    # PR_SET_CHILD_SUBREAPER keeps orphaned fixture children ours to reap.
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
            if pid == 0:
                time.sleep(0.002)
            else:
                continue
        except ChildProcessError:
            break


def command(argv, cwd, stdout, stderr, timeout, env=None):
    global ACTIVE
    with Path(stdout).open("wb") as out, Path(stderr).open("wb") as err:
        p = subprocess.Popen(
            [str(a) for a in argv],
            cwd=cwd,
            env=env,
            stdout=out,
            stderr=err,
            start_new_session=True,
        )
        ACTIVE = p
        try:
            return p.wait(timeout=timeout)
        finally:
            if p.poll() is None:
                terminate(p)
            # A normal-exit test can also orphan descendants. This process is a
            # subreaper and runs one command at a time, so these remain exclusively
            # our fixture children (including helpers with their own groups).
            for pid in reversed(descendants(os.getpid())):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    reaped, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not reaped:
                    time.sleep(0.002)
            ACTIVE = None


def archive(revision, destination):
    data = subprocess.check_output(["git", "archive", revision], cwd=REPO)
    destination.mkdir()
    with tarfile.open(fileobj=io.BytesIO(data)) as source:
        source.extractall(destination, filter="data")


def build(output, target_dir):
    if (
        subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
        ).strip()
        != CURRENT
    ):
        raise RuntimeError("wrong HEAD: harness is pinned to " + CURRENT)
    subprocess.run(
        [
            "git",
            "diff",
            "--exit-code",
            CURRENT,
            "--",
            "src",
            "migrations",
            "Cargo.toml",
            "Cargo.lock",
        ],
        cwd=REPO,
        check=True,
    )
    old_source, current_source = output / "source16", output / "source17"
    archive(OLD, old_source)
    archive(CURRENT, current_source)
    for name in OVERLAY:
        shutil.copyfile(REPO / name, current_source / name)
    bins, logs = output / "bin", output / "build-logs"
    bins.mkdir()
    logs.mkdir()
    env = {
        key: os.environ[key]
        for key in (
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "TMPDIR",
        )
        if key in os.environ
    }
    env.update(
        CARGO_TARGET_DIR=str(target_dir),
        RUSTC_WRAPPER="",
        CARGO_BUILD_JOBS="2",
        CARGO_ENCODED_RUSTFLAGS="",
        RUSTFLAGS="",
        CARGO_PROFILE_DEV_DEBUG="0",
    )
    manifest = {
        "python_version": platform.python_version(),
        "schema_smoke_reader_sqlite_version": sqlite3.sqlite_version,
        "historical_revision": OLD,
        "current_revision": CURRENT,
        "historical_binary_kind": "new build from pinned source, not preserved historical executable",
        "overlay_sha256": {name: digest(REPO / name) for name in OVERLAY},
        "lockfiles": {
            "old": digest(old_source / "Cargo.lock"),
            "current": digest(current_source / "Cargo.lock"),
        },
        "toolchain": {
            tool: subprocess.check_output(
                [tool, "--version"], text=True, env=env
            ).strip()
            for tool in ("cargo", "rustc")
        },
        "build_environment_overrides": {
            k: env[k]
            for k in (
                "CARGO_TARGET_DIR",
                "RUSTC_WRAPPER",
                "CARGO_BUILD_JOBS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTFLAGS",
                "CARGO_PROFILE_DEV_DEBUG",
            )
        },
        "build_inherited_environment_names": sorted(
            set(env)
            - {
                "CARGO_TARGET_DIR",
                "RUSTC_WRAPPER",
                "CARGO_BUILD_JOBS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTFLAGS",
                "CARGO_PROFILE_DEV_DEBUG",
            }
        ),
        "build_commands": [],
        "binaries": {},
        "targets": {},
    }
    for name, source, extra in (
        ("old", old_source, []),
        ("current", current_source, []),
        ("driver", current_source, ["--features", "spawn-gate"]),
    ):
        env["CARGO_TARGET_DIR"] = str(target_dir / name)
        argv = ["cargo", "build", "--locked", "--offline", "--bin", "mind", *extra]
        result = command(
            argv, source, logs / (name + ".stdout"), logs / (name + ".stderr"), 900, env
        )
        manifest["build_commands"].append(
            {
                "argv": argv,
                "source": str(source),
                "exit": result,
                "target_dir": env["CARGO_TARGET_DIR"],
                "features": ["embeddings"]
                + (["spawn-gate"] if name == "driver" else []),
            }
        )
        if result != 0:
            write_json(output / "manifest.json", manifest)
            raise RuntimeError(f"{name} build failed ({result}); see {logs}")
        binary = bins / (name + "-mind")
        shutil.copy2(Path(env["CARGO_TARGET_DIR"]) / "debug/mind", binary)
        smoke = output / ("smoke-" + name)
        smoke.mkdir()
        smoke_env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(smoke),
            "XDG_DATA_HOME": str(smoke / "data"),
            "XDG_CONFIG_HOME": str(smoke / "config"),
            "TMPDIR": str(smoke),
        }
        smoke_db = smoke / "mind.db"
        smoke_exit = command(
            [binary, "--db", smoke_db, "init"],
            smoke,
            logs / (name + "-smoke.stdout"),
            logs / (name + "-smoke.stderr"),
            45,
            smoke_env,
        )
        if smoke_exit != 0:
            raise RuntimeError(f"{name} binary initialization smoke failed")
        with sqlite3.connect(smoke_db) as db:
            version = db.execute(
                "SELECT MAX(version) FROM schema_migrations"
            ).fetchone()[0]
        expected_version = 16 if name == "old" else 17
        if version != expected_version:
            raise RuntimeError(
                f"{name} binary provenance smoke expected schema{expected_version}, got{version}"
            )
        manifest["binaries"][name] = {
            "path": str(binary),
            "sha256": digest(binary),
            "initialized_schema_version": version,
        }
    env["CARGO_TARGET_DIR"] = str(target_dir / "driver")
    argv = [
        "cargo",
        "test",
        "--locked",
        "--offline",
        "--no-run",
        "--message-format=json",
    ]
    for target in TARGETS:
        argv += ["--test", target]
    result = command(
        argv, current_source, logs / "tests.jsonl", logs / "tests.stderr", 900, env
    )
    manifest["build_commands"].append({"argv": argv, "exit": result})
    if result != 0:
        write_json(output / "manifest.json", manifest)
        raise RuntimeError(f"integration target build failed ({result}); see {logs}")
    for line in (logs / "tests.jsonl").read_text().splitlines():
        artifact = json.loads(line)
        if (
            artifact.get("reason") == "compiler-artifact"
            and artifact.get("executable")
            and artifact["target"]["name"] in TARGETS
        ):
            target = artifact["target"]["name"]
            binary = bins / target
            shutil.copy2(artifact["executable"], binary)
            manifest["targets"][target] = {
                "path": str(binary),
                "sha256": digest(binary),
            }
    if set(manifest["targets"]) != set(TARGETS):
        raise RuntimeError("missing integration executable in Cargo artifact manifest")
    write_json(output / "manifest.json", manifest)
    return manifest


def filesystem(path):
    if not path.exists():
        return None
    return subprocess.check_output(
        ["stat", "-f", "-c", "%T", str(path)], text=True
    ).strip()


def verify_manifest(manifest):
    if (manifest["historical_revision"], manifest["current_revision"]) != (
        OLD,
        CURRENT,
    ):
        raise RuntimeError("incorrect source revisions in reused manifest")
    for name, expected in manifest["overlay_sha256"].items():
        if digest(REPO / name) != expected:
            raise RuntimeError("test source changed; rebuild required: " + name)
    for item in [*manifest["binaries"].values(), *manifest["targets"].values()]:
        if digest(item["path"]) != item["sha256"]:
            raise RuntimeError("binary identity changed: " + item["path"])


def execute(output, manifest, args):
    verify_manifest(manifest)
    roots = [
        ("baseline", output / "fixtures", None),
        ("tmpfs", Path(args.tmpfs_root), "tmpfs"),
        ("btrfs", Path(args.btrfs_root), "btrfs"),
    ]
    summary = {
        "meaning": "PASS of characterization reproduces unsafe/unresolved behavior, never proves a fix",
        "kernel": platform.release(),
        "platform": platform.platform(),
        "filesystems": [],
        "executions": [],
    }
    output.joinpath("runs").mkdir()
    for label, root, expected in roots:
        if expected is None:
            root.mkdir()
        else:
            # Do not infer filesystem type from the directory name.
            probe = root
            while not probe.exists() and probe != probe.parent:
                probe = probe.parent
            kind = filesystem(probe)
            if kind != expected:
                summary["filesystems"].append(
                    {
                        "label": label,
                        "status": "SKIPPED",
                        "root": str(root),
                        "reason": f"expected {expected}, observed {kind}",
                    }
                )
                continue
            root.mkdir(exist_ok=True, parents=True)
        kind = filesystem(root)
        summary["filesystems"].append(
            {"label": label, "status": "AVAILABLE", "root": str(root), "type": kind}
        )
        for repeat in range(args.repeat):
            for target, item in sorted(manifest["targets"].items()):
                listing = subprocess.check_output(
                    [item["path"], "--list", "--ignored"], text=True
                )
                names = [
                    line.removesuffix(": test")
                    for line in listing.splitlines()
                    if line.endswith(": test")
                ]
                names = [
                    name
                    for name in names
                    if not name.endswith("native_actor")
                    and (not args.case or args.case in name)
                ]
                for name in names:
                    case_output = (
                        output / "runs" / f"{label}-{repeat + 1}-{target}-{name}"
                    )
                    case_output.mkdir()
                    for directory in ("home", "config", "data", "cache", "tmp"):
                        (case_output / directory).mkdir()
                    env = {
                        "PATH": "/usr/bin:/bin",
                        "HOME": str(case_output / "home"),
                        "XDG_CONFIG_HOME": str(case_output / "config"),
                        "XDG_DATA_HOME": str(case_output / "data"),
                        "XDG_CACHE_HOME": str(case_output / "cache"),
                        "TMPDIR": str(case_output / "tmp"),
                        "LANG": "C.UTF-8",
                        "MINDSPACE_ADVERSARIAL_ROOT": str(root),
                        "MINDSPACE_ADVERSARIAL_ARTIFACTS": str(case_output),
                        "MINDSPACE_ADVERSARIAL_OLD": manifest["binaries"]["old"][
                            "path"
                        ],
                        "MINDSPACE_ADVERSARIAL_CURRENT": manifest["binaries"][
                            "current"
                        ]["path"],
                        "MINDSPACE_ADVERSARIAL_DRIVER": manifest["binaries"]["driver"][
                            "path"
                        ],
                    }
                    argv = [
                        item["path"],
                        "--ignored",
                        "--exact",
                        name,
                        "--nocapture",
                        "--test-threads=1",
                    ]
                    try:
                        result = command(
                            argv,
                            REPO,
                            case_output / "test.stdout",
                            case_output / "test.stderr",
                            180,
                            env,
                        )
                    except subprocess.TimeoutExpired:
                        result = 124
                    evidence = [
                        json.loads(path.read_text())
                        for path in case_output.glob("*/result.json")
                    ]
                    status = (
                        "FAIL"
                        if result != 0
                        or not evidence
                        or any(v["status"] == "FAIL" for v in evidence)
                        else (
                            "SKIPPED"
                            if all(v["status"] == "SKIPPED" for v in evidence)
                            else "PASS"
                        )
                    )
                    summary["executions"].append(
                        {
                            "filesystem": label,
                            "repeat": repeat + 1,
                            "target": target,
                            "case": name,
                            "status": status,
                            "exit": result,
                            "argv": argv,
                            "timeout_seconds": 180,
                            "evidence": str(case_output),
                            "results": evidence,
                        }
                    )
                    write_json(output / "summary.json", summary)
                    print(f"{status} {label} repeat={repeat + 1} {name}", flush=True)
    if not summary["executions"]:
        raise RuntimeError("no scenarios selected")
    write_json(output / "summary.json", summary)
    return 1 if any(v["status"] == "FAIL" for v in summary["executions"]) else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="new evidence directory; must not exist",
    )
    parser.add_argument(
        "--target-dir", type=Path, help="optional Cargo build cache (not a database)"
    )
    parser.add_argument(
        "--reuse-build",
        type=Path,
        help="reuse authenticated manifest binaries; sources and hashes are rechecked",
    )
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument(
        "--case", help="optional scenario-name substring for independent execution"
    )
    parser.add_argument("--tmpfs-root", default="/tmp")
    parser.add_argument("--btrfs-root", default=str(REPO / "target/s3e3b-fixtures"))
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error(
            "this harness currently requires Linux; filesystem baseline is portable across local Linux filesystems"
        )
    if args.repeat < 1:
        parser.error("repeat must be positive")
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    # Linux PR_SET_CHILD_SUBREAPER: reap only fixture descendants on interruption.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot establish child subreaper")

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    if args.reuse_build:
        manifest = json.loads(args.reuse_build.read_text())
        write_json(args.output / "manifest.json", manifest)
    else:
        target = (args.target_dir or args.output / "target").resolve()
        manifest = build(args.output, target)
    return execute(args.output, manifest, args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (
        OSError,
        RuntimeError,
        ValueError,
        subprocess.SubprocessError,
        KeyboardInterrupt,
    ) as error:
        print(f"HARNESS ERROR: {error}", file=sys.stderr)
        sys.exit(2)
