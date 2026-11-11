#!/usr/bin/env python3
"""Reproduce signed-baseline S3e.4d reconciliation/UUID counterexamples.

PASS means the baseline characterizations reproduced. No reconciliation planner,
writer guard, publication mechanism or production schema is installed.
"""

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import signal
import sys
import tarfile
from pathlib import Path

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[3]
PIN = "3f7fd3422047b40f9dc6fa787d4b523d7d90d857"
FINGERPRINT = "0E6699CB48F3941AE28CA88D509F78007F62D5EA"
PROBE = Path(__file__).with_name("s3e4d_reconciliation_probe.rs")
PREFIX = "storage::sqlite::observation_planning_tests::"
CASES = (
    ("s3e4d_baseline_low_uuid_tie_changes_the_second_reconciliation", "low_uuid_order"),
    ("s3e4d_baseline_high_uuid_tie_retains_the_first_reconciliation", "high_uuid_order"),
    ("s3e4d_baseline_duplicate_existing_location_reuses_pending_membership", "pending_membership"),
    ("s3e4d_uuid_observability_fixed_clock", "uuid_observability_fixed_clock"),
)
CHECKPOINT = (
    '    #[cfg(test)]\n'
    '    observation_planning_tests::checkpoint("after_reconcile", conn)?;\n'
)
CLOCK_SOURCE = r'''#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <time.h>

/* Scoped to one explicitly launched test process. The host clock is unchanged;
 * every non-REALTIME clock, including monotonic timeout clocks, is forwarded. */
int clock_gettime(clockid_t which, struct timespec *out) {
    int (*real_clock_gettime)(clockid_t, struct timespec *) =
        dlsym(RTLD_NEXT, "clock_gettime");
    if (!real_clock_gettime) { errno = ENOSYS; return -1; }
    const char *secs = getenv("MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS");
    const char *nanos = getenv("MINDSPACE_S3E4D_FIXED_REALTIME_NANOS");
    if (which == CLOCK_REALTIME && secs) {
        char *end = NULL;
        errno = 0;
        long long s = strtoll(secs, &end, 10);
        if (errno || !end || *end || s < 0) { errno = EINVAL; return -1; }
        long ns = 0;
        if (nanos) {
            end = NULL;
            errno = 0;
            ns = strtol(nanos, &end, 10);
            if (errno || !end || *end || ns < 0 || ns >= 1000000000L) {
                errno = EINVAL; return -1;
            }
        }
        out->tv_sec = (time_t)s;
        out->tv_nsec = ns;
        return 0;
    }
    return real_clock_gettime(which, out);
}
'''


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def protected_inputs(root):
    return {
        str(p.relative_to(root)): digest(p)
        for p in root.rglob("*")
        if p.is_file()
        and (p.suffix == ".rs" or "migrations" in p.relative_to(root).parts
             or p.name in ("Cargo.toml", "Cargo.lock"))
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-root", required=True, type=Path,
                        help="fresh private btrfs directory; must not exist")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--runtime-bin", type=Path,
                        help="optional pinned static-runtime directory; Codex is not executed")
    args = parser.parse_args()
    work = args.work_root.expanduser().absolute()
    work.mkdir(mode=0o700, parents=True, exist_ok=False)
    logs = work / "logs"
    logs.mkdir()
    status = {"baseline": PIN, "production_status": "PARTIAL",
              "production_implementation_installed": False,
              "classification": "baseline characterizations, not feature/security acceptance",
              "cases": [], "external_codex_executed": False}
    save(work / "result.json", dict(status, result="INCOMPLETE"))
    if not sys.platform.startswith("linux"):
        save(work / "result.json", dict(status, result="SKIPPED", reason="requires Linux"))
        return 77
    for tool in ("git", "cargo", "cc"):
        if shutil.which(tool) is None:
            save(work / "result.json", dict(status, result="SKIPPED", reason=f"missing {tool}"))
            return 77
    if not PROBE.is_file():
        raise RuntimeError(f"missing companion probe source: {PROBE}")
    probe_bytes = PROBE.read_bytes()
    probe_sha = hashlib.sha256(probe_bytes).hexdigest()
    runner_sha = digest(__file__)
    supervisor_path = REPO / "scripts/m8-s3e3b-harness.py"
    spec = importlib.util.spec_from_file_location("s3e4d_supervisor", supervisor_path)
    supervisor = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(supervisor)
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        save(work / "result.json", dict(status, result="SKIPPED",
             reason=f"cannot establish owned-child subreaper: errno {ctypes.get_errno()}"))
        return 77

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    host_env = dict(os.environ, TMPDIR=str(work))
    command_records = []

    def checked(argv, cwd, label, env, timeout):
        record = {"argv": [str(x) for x in argv], "cwd": str(cwd), "timeout_seconds": timeout}
        save(logs / (label + ".command.json"), record)
        code = supervisor.command(argv, cwd, logs / (label + ".stdout"),
                                  logs / (label + ".stderr"), timeout, env)
        record["exit"] = code
        save(logs / (label + ".command.json"), record)
        command_records.append(record)
        if code:
            raise RuntimeError(f"{label} failed exit {code}; retained logs: {logs}")

    try:
        checked(["git", "verify-commit", "--raw", PIN], REPO, "signature", host_env, 30)
        signature = (logs / "signature.stderr").read_text()
        if "[GNUPG:] VALIDSIG " + FINGERPRINT + " " not in signature:
            raise RuntimeError("baseline signature did not match the required key")
        checked(["git", "show", f"{PIN}:scripts/m8-s3e3b-harness.py"],
                REPO, "framework", host_env, 30)
        if digest(supervisor_path) != digest(logs / "framework.stdout"):
            raise RuntimeError("existing process supervisor differs from the pinned baseline")
        checked(["git", "archive", "--format=tar", PIN], REPO, "archive", host_env, 60)
        source = work / "source"
        source.mkdir()
        with tarfile.open(logs / "archive.stdout") as archive:
            archive.extractall(source, filter="data")
        before = protected_inputs(source)
        sqlite = source / "src/storage/sqlite.rs"
        original = sqlite.read_text()
        start = original.index("fn reconcile_logical(")
        end = original.index("\nfn get_or_create_assertion(", start)
        body = original[start:end]
        if body.count("    Ok(actions)\n") != 1:
            raise RuntimeError("pinned reconciliation return anchor differs")
        instrumented = body.replace("    Ok(actions)\n", CHECKPOINT + "    Ok(actions)\n")
        patched = original[:start] + instrumented + original[end:]
        if patched.replace(CHECKPOINT, "", 1) != original:
            raise RuntimeError("unexpected production-body overlay")
        sqlite.write_text(patched)
        module = source / "src/storage/sqlite/observation_planning_tests.rs"
        module_original = module.read_text()
        attachment = '\ninclude!("s3e4d_reconciliation_probe.rs");\n'
        module.write_text(module_original + attachment)
        composed_probe = module.with_name("s3e4d_reconciliation_probe.rs")
        composed_probe.write_bytes(probe_bytes)
        after = protected_inputs(source)
        changed = {name for name in before if before[name] != after.get(name)}
        expected = {"src/storage/sqlite.rs", "src/storage/sqlite/observation_planning_tests.rs"}
        if changed != expected or set(after) - set(before) != {str(composed_probe.relative_to(source))}:
            raise RuntimeError("archive changes exceeded the declared test-only overlay")
        if module.read_text().removesuffix(attachment) != module_original:
            raise RuntimeError("existing native test helpers changed")
        save(work / "source-manifest.json", {"baseline": PIN, "before": before,
             "after": after, "changed": sorted(changed),
             "production_logic_preserved_except_test_checkpoint": True,
             "runner_sha256": runner_sha, "probe_sha256": probe_sha,
             "supervisor_sha256": digest(supervisor_path),
             "archive_sha256": digest(logs / "archive.stdout")})
        target = (args.target_dir or work / "target").expanduser().absolute()
        original_home = Path.home()
        env = {k: os.environ[k] for k in ("PATH", "RUSTUP_TOOLCHAIN") if k in os.environ}
        for name in ("home", "config", "data", "cache", "tmp", "results"):
            (work / name).mkdir()
        env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"),
                   XDG_DATA_HOME=str(work / "data"), XDG_CACHE_HOME=str(work / "cache"),
                   TMPDIR=str(work / "tmp"),
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(original_home / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(original_home / ".rustup")),
                   CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS="2", CARGO_PROFILE_DEV_DEBUG="0",
                   RUSTC_WRAPPER="", RUSTFLAGS="", CARGO_ENCODED_RUSTFLAGS="",
                   GIT_TERMINAL_PROMPT="0", GIT_CONFIG_COUNT="2",
                   GIT_CONFIG_KEY_0="commit.gpgsign", GIT_CONFIG_VALUE_0="false",
                   GIT_CONFIG_KEY_1="tag.gpgsign", GIT_CONFIG_VALUE_1="false")
        if args.runtime_bin:
            env["PATH"] = str(args.runtime_bin.absolute()) + ":" + env.get("PATH", "/usr/bin:/bin")
        save(work / "environment.json", {"environment": env, "kernel": platform.release(),
             "libc": platform.libc_ver(), "python": platform.python_version(),
             "release": args.release, "target": str(target)})
        for tool in ("cargo", "rustc", "cc"):
            checked([tool, "--version"], work, tool + "-version", env, 30)
        clock_c = work / "fixed_clock.c"
        clock_c.write_text(CLOCK_SOURCE)
        clock_so = work / "fixed_clock.so"
        checked(["cc", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-O2",
                 "-o", clock_so, clock_c, "-ldl"], work, "clock-build", env, 60)
        build = ["cargo", "test", "--locked", "--offline", "--lib", "--no-run", "--message-format=json"]
        if args.release:
            build.append("--release")
        checked(build, source, "build", env, 1800)
        artifacts = []
        for line in (logs / "build.stdout").read_text().splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if item.get("reason") == "compiler-artifact" and item.get("manifest_path") == str(source / "Cargo.toml"):
                artifacts.append(item)
        units = [x for x in artifacts if x.get("executable") and x["profile"].get("test")
                 and x["target"]["name"] == "mindspace"]
        if len(units) != 1:
            raise RuntimeError("could not identify exactly one compiled baseline unit executable")
        executable = Path(units[0]["executable"])
        status.update(test_executable=str(executable), test_executable_sha256=digest(executable),
                      features=units[0]["features"], probe_sha256=probe_sha,
                      clock_source_sha256=digest(clock_c), clock_helper_sha256=digest(clock_so))
        save(work / "compiler-artifacts.json", artifacts)
        for test, result_name in CASES:
            case_env = dict(env, MINDSPACE_S3E4D_PROBE_OUT=str(work / "results"))
            if test == "s3e4d_uuid_observability_fixed_clock":
                case_env.update(LD_PRELOAD=str(clock_so),
                                MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS="1800000000",
                                MINDSPACE_S3E4D_FIXED_REALTIME_NANOS="123000000")
            checked([executable, "--exact", PREFIX + test, "--test-threads=1", "--nocapture"],
                    source, result_name, case_env, 60)
            text = (logs / (result_name + ".stdout")).read_text()
            if not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", text):
                raise RuntimeError("exact native test did not execute once: " + test)
            result = json.loads((work / "results" / (result_name + ".json")).read_text())
            if result.get("baseline") != PIN or not result.get("all_rows_rolled_back"):
                raise RuntimeError("baseline/rollback result missing: " + test)
            status["cases"].append({"test": PREFIX + test, "result": "REPRODUCED",
                                     "artifact": str(work / "results" / (result_name + ".json"))})
            save(work / "result.json", dict(status, result="INCOMPLETE"))
        save(work / "commands.json", command_records)
        save(work / "result.json", dict(status, result="PASS: baseline characterizations reproduced"))
        print(json.dumps({"result": "PASS: baseline characterizations reproduced",
                          "cases": len(status["cases"]), "production_status": "PARTIAL",
                          "artifacts": str(work)}, indent=2))
        return 0
    except BaseException as error:
        save(work / "commands.json", command_records)
        save(work / "result.json", dict(status, result="FAILED", error_type=type(error).__name__,
                                       error=str(error)))
        raise


if __name__ == "__main__":
    raise SystemExit(main())
