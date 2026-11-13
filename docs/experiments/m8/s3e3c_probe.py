#!/usr/bin/env python3
"""Disposable S3e.3c feasibility experiments; never installs production schema."""

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
import platform
import shutil
import signal
import sqlite3
import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[3]
OLD = "254d1ad5794db8ac19dc187a5934910337da9b9e"
PIN = "9e22ac3894a2c4d534f9bb1103573f220f171be7"
EXPERIMENTS = (
    "docs/experiments/m8/s3e3c_old_store_actor.rs",
    "docs/experiments/m8/s3e3c_metadata_probe.rs",
    "docs/experiments/m8/s3e3c_probe.py",
)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument(
        "--binaries-manifest",
        type=Path,
        help="optional authenticated S3e.3b manifest, otherwise rebuild pinned production binaries",
    )
    parser.add_argument(
        "--reuse",
        type=Path,
        help="reuse this probe's manifest with source and executable hash checks",
    )
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--case")
    parser.add_argument(
        "--btrfs-root", type=Path, default=REPO / "target/s3e3c-fixtures"
    )
    args = parser.parse_args()
    if not sys.platform.startswith("linux") or args.repeat < 1:
        parser.error("requires Linux and repeat >= 1")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot establish child subreaper")

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    spec = importlib.util.spec_from_file_location(
        "s3e3b", REPO / "scripts/m8-s3e3b-harness.py"
    )
    base = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(base)
    logs = output / "logs"
    logs.mkdir()

    def checked(argv, cwd, label, env, timeout=900):
        code = base.command(
            argv,
            cwd,
            logs / (label + ".stdout"),
            logs / (label + ".stderr"),
            timeout,
            env,
        )
        if code:
            raise RuntimeError(f"{label} failed exit{code}; see {logs}")

    def pinned_framework():
        identities = {}
        for name in (
            "scripts/m8-s3e3b-harness.py",
            "tests/support/adversarial_common.rs",
        ):
            committed = subprocess.check_output(
                ["git", "show", f"{PIN}:{name}"], cwd=REPO
            )
            expected = hashlib.sha256(committed).hexdigest()
            if sha(REPO / name) != expected:
                raise RuntimeError("committed probe infrastructure changed: " + name)
            identities[name] = expected
        return identities

    framework = pinned_framework()

    def verify(manifest):
        if manifest["framework_sha256"] != framework:
            raise RuntimeError("framework identity differs from pinned source")
        if manifest["historical_source"] != OLD or manifest["current_source"] != PIN:
            raise RuntimeError("wrong pinned sources")
        for path, digest in manifest["experiment_sha256"].items():
            if sha(REPO / path) != digest:
                raise RuntimeError("experiment changed; rebuild required: " + path)
        for item in manifest["executables"].values():
            if sha(item["path"]) != item["sha256"]:
                raise RuntimeError("executable changed: " + item["path"])

    if args.reuse:
        manifest = json.loads(args.reuse.read_text())
        verify(manifest)
    else:
        subprocess.run(
            ["git", "merge-base", "--is-ancestor", PIN, "HEAD"], cwd=REPO, check=True
        )
        subprocess.run(
            [
                "git",
                "diff",
                "--exit-code",
                PIN,
                "--",
                "src",
                "migrations",
                "Cargo.toml",
                "Cargo.lock",
            ],
            cwd=REPO,
            check=True,
        )
        sources = {"old": output / "source16", "current": output / "source17"}
        snapshots = {name: (REPO / name).read_bytes() for name in EXPERIMENTS}
        for name, revision in (("old", OLD), ("current", PIN)):
            base.archive(revision, sources[name])
        (sources["old"] / "tests/s3e3c_old_store_actor.rs").write_bytes(
            snapshots[EXPERIMENTS[0]]
        )
        (sources["current"] / "tests/metadata_quarantine_feasibility.rs").write_bytes(
            snapshots[EXPERIMENTS[1]]
        )
        build_root = (args.target_dir or output / "build").resolve()
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
            RUSTC_WRAPPER="",
            CARGO_BUILD_JOBS="2",
            CARGO_ENCODED_RUSTFLAGS="",
            RUSTFLAGS="",
            CARGO_PROFILE_DEV_DEBUG="0",
        )
        manifest = {
            "historical_source": OLD,
            "current_source": PIN,
            "experiment_sha256": {
                name: hashlib.sha256(data).hexdigest()
                for name, data in snapshots.items()
            },
            "framework_sha256": framework,
            "shared_helper_sha256": sha(REPO / "tests/support/adversarial_common.rs"),
            "executables": {},
            "builds": [],
            "kernel": platform.release(),
            "python": platform.python_version(),
            "toolchain": {
                tool: subprocess.check_output(
                    [tool, "--version"], env=env, text=True
                ).strip()
                for tool in ("cargo", "rustc")
            },
            "lockfiles": {
                name: sha(source / "Cargo.lock") for name, source in sources.items()
            },
            "compiler_controls": {
                key: env[key]
                for key in (
                    "RUSTC_WRAPPER",
                    "CARGO_BUILD_JOBS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "RUSTFLAGS",
                    "CARGO_PROFILE_DEV_DEBUG",
                )
            },
        }
        bins = output / "bin"
        bins.mkdir()
        if args.binaries_manifest:
            prior = json.loads(args.binaries_manifest.read_text())
            base.verify_manifest(prior)
            subprocess.run(
                [
                    "git",
                    "diff",
                    "--exit-code",
                    prior["current_revision"],
                    PIN,
                    "--",
                    "src",
                    "migrations",
                    "Cargo.toml",
                    "Cargo.lock",
                ],
                cwd=REPO,
                check=True,
            )
            for role in ("old", "current", "driver"):
                manifest["executables"][role] = dict(prior["binaries"][role])
            manifest["reused_binaries_manifest"] = str(args.binaries_manifest.resolve())
        else:
            for role, source, extra in (
                ("old", sources["old"], []),
                ("current", sources["current"], []),
                ("driver", sources["current"], ["--features", "spawn-gate"]),
            ):
                env["CARGO_TARGET_DIR"] = str(build_root / (role + "-production"))
                argv = [
                    "cargo",
                    "build",
                    "--locked",
                    "--offline",
                    "--bin",
                    "mind",
                    *extra,
                ]
                checked(argv, source, "build-" + role, env)
                binary = bins / role
                shutil.copy2(Path(env["CARGO_TARGET_DIR"]) / "debug/mind", binary)
                manifest["executables"][role] = {
                    "path": str(binary),
                    "sha256": sha(binary),
                }
                manifest["builds"].append(
                    {
                        "argv": argv,
                        "source": str(source),
                        "target": env["CARGO_TARGET_DIR"],
                    }
                )
        for role in ("old", "current", "driver"):
            smoke = output / ("smoke-" + role)
            smoke.mkdir()
            smoke_env = {
                "PATH": "/usr/bin:/bin",
                "HOME": str(smoke),
                "XDG_DATA_HOME": str(smoke / "data"),
                "TMPDIR": str(smoke),
            }
            checked(
                [
                    manifest["executables"][role]["path"],
                    "--db",
                    smoke / "mind.db",
                    "init",
                ],
                smoke,
                "smoke-" + role,
                smoke_env,
                45,
            )
            with sqlite3.connect(smoke / "mind.db") as db:
                version = db.execute(
                    "SELECT MAX(version) FROM schema_migrations"
                ).fetchone()[0]
            if version != (16 if role == "old" else 17):
                raise RuntimeError("binary schema smoke mismatch: " + role)
            manifest["executables"][role]["schema_smoke"] = version
        for role, source, target in (
            ("old_actor", sources["old"], "s3e3c_old_store_actor"),
            ("native", sources["current"], "metadata_quarantine_feasibility"),
        ):
            env["CARGO_TARGET_DIR"] = str(build_root / role)
            argv = [
                "cargo",
                "test",
                "--locked",
                "--offline",
                "--test",
                target,
                "--no-run",
                "--message-format=json",
            ]
            checked(argv, source, "build-" + role, env)
            found = []
            for line in (logs / ("build-" + role + ".stdout")).read_text().splitlines():
                item = json.loads(line)
                if (
                    item.get("reason") == "compiler-artifact"
                    and item.get("executable")
                    and item["target"]["name"] == target
                ):
                    found.append(item["executable"])
            if len(found) != 1:
                raise RuntimeError("missing or duplicate Cargo test executable")
            binary = bins / role
            shutil.copy2(found[0], binary)
            manifest["executables"][role] = {"path": str(binary), "sha256": sha(binary)}
            manifest["builds"].append(
                {"argv": argv, "source": str(source), "target": env["CARGO_TARGET_DIR"]}
            )
        verify(manifest)
    save(output / "manifest.json", manifest)
    roots = [
        ("baseline", output / "fixtures", None),
        ("tmpfs", Path("/tmp"), "tmpfs"),
        ("btrfs", args.btrfs_root.resolve(), "btrfs"),
    ]
    summary = {
        "meaning": "test-only adapter feasibility and counterexamples; no production exclusion claim",
        "filesystems": [],
        "executions": [],
    }
    native = manifest["executables"]["native"]["path"]
    names = [
        line.removesuffix(": test")
        for line in subprocess.check_output(
            [native, "--list", "--ignored"], text=True
        ).splitlines()
        if line.endswith(": test")
    ]
    names = sorted(
        name
        for name in names
        if not name.endswith("candidate_actor") and (not args.case or args.case in name)
    )
    for label, root, expected in roots:
        probe = root
        while not probe.exists() and probe != probe.parent:
            probe = probe.parent
        kind = base.filesystem(probe)
        if expected and kind != expected:
            summary["filesystems"].append(
                {
                    "label": label,
                    "status": "SKIPPED",
                    "reason": f"expected {expected}, observed {kind}",
                }
            )
            continue
        root.mkdir(parents=True, exist_ok=True)
        summary["filesystems"].append(
            {
                "label": label,
                "status": "AVAILABLE",
                "type": base.filesystem(root),
                "root": str(root),
            }
        )
        for repeat in range(args.repeat):
            for name in names:
                case = output / "runs" / f"{label}-{repeat + 1}-{name}"
                case.mkdir(parents=True)
                for part in ("home", "config", "data", "cache", "tmp"):
                    (case / part).mkdir()
                test_env = {
                    "PATH": "/usr/bin:/bin",
                    "HOME": str(case / "home"),
                    "XDG_CONFIG_HOME": str(case / "config"),
                    "XDG_DATA_HOME": str(case / "data"),
                    "XDG_CACHE_HOME": str(case / "cache"),
                    "TMPDIR": str(case / "tmp"),
                    "MINDSPACE_ADVERSARIAL_ROOT": str(root),
                    "MINDSPACE_ADVERSARIAL_ARTIFACTS": str(case),
                    "MINDSPACE_ADVERSARIAL_OLD": manifest["executables"]["old"]["path"],
                    "MINDSPACE_ADVERSARIAL_CURRENT": manifest["executables"]["current"][
                        "path"
                    ],
                    "MINDSPACE_ADVERSARIAL_DRIVER": manifest["executables"]["driver"][
                        "path"
                    ],
                    "S3E3C_OLD_ACTOR_BINARY": manifest["executables"]["old_actor"][
                        "path"
                    ],
                }
                argv = [
                    native,
                    "--ignored",
                    "--exact",
                    name,
                    "--nocapture",
                    "--test-threads=1",
                ]
                try:
                    code = base.command(
                        argv,
                        REPO,
                        case / "test.stdout",
                        case / "test.stderr",
                        240,
                        test_env,
                    )
                except subprocess.TimeoutExpired:
                    code = 124
                results = [
                    json.loads(p.read_text()) for p in case.glob("*/result.json")
                ]
                status = (
                    "FAIL"
                    if code
                    or not results
                    or any(x["status"] == "FAIL" for x in results)
                    else (
                        "SKIPPED"
                        if all(x["status"] == "SKIPPED" for x in results)
                        else "PASS"
                    )
                )
                summary["executions"].append(
                    {
                        "case": name,
                        "filesystem": label,
                        "repeat": repeat + 1,
                        "status": status,
                        "exit": code,
                        "argv": argv,
                        "evidence": str(case),
                        "results": results,
                    }
                )
                save(output / "summary.json", summary)
                print(f"{status} {label} repeat={repeat + 1} {name}", flush=True)
    if not summary["executions"]:
        raise RuntimeError("no selected scenarios")
    save(output / "summary.json", summary)
    return int(any(x["status"] == "FAIL" for x in summary["executions"]))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (
        OSError,
        ValueError,
        RuntimeError,
        subprocess.SubprocessError,
        KeyboardInterrupt,
    ) as error:
        print(f"PROBE ERROR: {error}", file=sys.stderr)
        sys.exit(2)
