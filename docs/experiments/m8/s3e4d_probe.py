#!/usr/bin/env python3
"""Reproduce signed-baseline S3e.4d reconciliation/UUID counterexamples.

DO NOT APPLY the rejected candidate to a working tree. Disposable archive replay
only. Retained success/nonfatal extensions are UNEXECUTED evidence, not acceptance.

PASS means the declared compatibility probes reproduced, not publication safety.
The default compiles a signed-baseline archive. --candidate overlays only the
explicit production/test allowlist and records their input hashes.
--candidate-patch applies the preserved rejected fixture only in that archive.
Neither mode installs production changes or qualifies the rejected design.
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
import uuid
from pathlib import Path

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[3]
PIN = "3f7fd3422047b40f9dc6fa787d4b523d7d90d857"
FINGERPRINT = "0E6699CB48F3941AE28CA88D509F78007F62D5EA"
PROBE = Path(__file__).with_name("s3e4d_reconciliation_probe.rs")
FATAL_PROBE = Path(__file__).with_name("s3e4d_fatal_read_probe.rs")
FATAL_CASES = (("s3e4d_review_fatal_summary_read", "fatal_summary_read"),)
PREFIX = "storage::sqlite::observation_planning_tests::"
CASES = (
    ("s3e4d_baseline_low_uuid_tie_changes_the_second_reconciliation", "low_uuid_order"),
    ("s3e4d_baseline_high_uuid_tie_retains_the_first_reconciliation", "high_uuid_order"),
    ("s3e4d_baseline_duplicate_existing_location_reuses_pending_membership", "pending_membership"),
    ("s3e4d_uuid_observability_fixed_clock", "uuid_observability_fixed_clock"),
    ("s3e4d_success_compatibility_fixed_clock", "success_compatibility_fixed_clock"),
    ("s3e4d_read_failure_compatibility_fixed_clock", "read_failure_compatibility_fixed_clock"),
)
CHECKPOINT = (
    '    #[cfg(test)]\n'
    '    observation_planning_tests::checkpoint("after_reconcile", conn)?;\n'
)
MEMBERSHIP_CHECKPOINT = (
    '    #[cfg(test)]\n'
    '    observation_planning_tests::checkpoint(&format!("membership_read:{memory_id}"), conn)?;\n'
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



UUID_TEXT = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
CANDIDATE_INPUTS = (
    "src/storage/sqlite.rs",
    "src/storage/sqlite/observation_planning_tests.rs",
    "src/storage/sqlite/reconciliation_planning_tests.rs",
)




def select_fatal_probe(content, candidate):
    text = content.decode("utf-8")
    selected = "CANDIDATE" if candidate else "BASELINE"
    begin = "// S3E4D_" + selected + "_BEGIN\n"
    end = "// S3E4D_" + selected + "_END"
    if text.count(begin) != 1 or text.count(end) != 1:
        raise RuntimeError("fatal probe template must have one selected source block")
    prefix, section = text.split(begin, 1)
    del prefix
    section, suffix = section.split(end, 1)
    del suffix
    if not section.strip() or "fn s3e4d_review_fatal_summary_read(" not in section:
        raise RuntimeError("selected fatal probe block lacks the exact native case")
    return section.encode("utf-8"), selected


def validated_patch(content):
    """Accept only the three reviewed plain-file reconciliation fixture paths."""
    text = content.decode("utf-8")
    blocks = re.split(r"(?m)^diff --git ", text)
    if blocks[0] or len(blocks) != len(CANDIDATE_INPUTS) + 1:
        raise RuntimeError("candidate patch must contain exactly the three reviewed diff blocks")
    names = set()
    for block in blocks[1:]:
        lines = block.splitlines()
        header = re.fullmatch(r"a/([^ ]+) b/([^ ]+)", lines[0])
        if not header or header[1] != header[2] or header[1] not in CANDIDATE_INPUTS:
            raise RuntimeError("candidate patch contains an unreviewed or escaped path")
        name = header[1]
        if name in names:
            raise RuntimeError("candidate patch repeats a file")
        names.add(name)
        old_names = [line[4:] for line in lines if line.startswith("--- ")]
        new_names = [line[4:] for line in lines if line.startswith("+++ ")]
        if old_names not in (["a/" + name], ["/dev/null"]) or new_names != ["b/" + name]:
            raise RuntimeError("candidate patch changes file authority or deletes a file")
        for line in lines[1:]:
            if line.startswith(("old mode ", "new mode ", "deleted file mode ",
                                "rename ", "copy ", "similarity ", "dissimilarity ",
                                "GIT binary patch", "Binary files ")):
                raise RuntimeError("candidate patch has unsupported mode/rename/binary metadata")
            if line.startswith("new file mode ") and line != "new file mode 100644":
                raise RuntimeError("candidate patch must create only regular nonexecutable files")
            if line.startswith("index ") and not re.fullmatch(r"index [0-9a-f]+\.\.[0-9a-f]+(?: 100644)?", line):
                raise RuntimeError("candidate patch index mode is not a regular file")
    if names != set(CANDIDATE_INPUTS):
        raise RuntimeError("candidate patch does not match the reviewed three-file scope")
    return names

def canonical_success(result):
    """Retain reference identity/allocation offsets, remove random UUID suffixes.

    This depends on the locked uuid1.26 packing, validated by the adjacent-call
    control. It is not a portable decoder or proof of exact random UUID equality.
    """
    anchor = uuid.UUID(result["uuid_anchor"])
    fixed = result["fixed_unix_millis"]
    if anchor.int >> 80 != fixed or result["adjacent_control_gap"] != 1:
        raise RuntimeError("invalid fixed-clock UUID canonicalization control")
    anchor_slice = (anchor.int >> 36) & ((1 << 26) - 1)

    def replace(match):
        value = uuid.UUID(match.group())
        if value.version != 7 or value.int >> 80 != fixed:
            raise RuntimeError("unexpected UUID outside the controlled stream")
        gap = (((value.int >> 36) & ((1 << 26) - 1)) - anchor_slice) & ((1 << 26) - 1)
        return f"<v7-allocation-offset:{gap}>"

    def visit(value):
        if isinstance(value, str):
            return UUID_TEXT.sub(replace, value)
        if isinstance(value, list):
            return [visit(v) for v in value]
        if isinstance(value, dict):
            return {k: visit(v) for k, v in value.items() if k != "retained_fixture"}
        return value

    return visit(result)


def comparison_projection(name, result):
    if name in ("success_compatibility_fixed_clock", "read_failure_compatibility_fixed_clock"):
        return canonical_success(result)
    if name == "uuid_observability_fixed_clock":
        keys = ("actual_error", "adjacent_gap", "no_effect_gap", "failed_gap",
                "next_session_gap_from_failed_after", "all_rows_rolled_back",
                "autocommit_after_failure", "ledger_before", "ledger_after_failure",
                "final_ledger")
        return {k: result[k] for k in keys}
    # D01-D03 retain their full native semantic assertions. Cross-run comparison
    # additionally checks error origins, rollback, and repeated snapshot identity
    # after bijective renaming; this projection does not assert UUID allocation.
    identities = {}

    def replace(match):
        value = match.group().lower()
        if value not in identities:
            identities[value] = f"<identity:{len(identities)}>"
        return identities[value]

    def visit(value):
        if isinstance(value, str):
            return UUID_TEXT.sub(replace, value)
        if isinstance(value, list):
            return [visit(v) for v in value]
        if isinstance(value, dict):
            return {k: visit(v) for k, v in value.items()
                    if k not in ("retained_fixture", "logical")}
        return value

    return visit(result)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-root", required=True, type=Path,
                        help="fresh private btrfs directory; must not exist")
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--fatal-read", action="store_true",
                        help="run only the expected unsafe fatal-read characterization")
    candidates = parser.add_mutually_exclusive_group()
    candidates.add_argument("--candidate", action="store_true",
                            help="overlay only explicit working-tree fixture input paths")
    candidates.add_argument("--candidate-patch", type=Path,
                            help="apply the reviewed rejected fixture patch in the disposable archive")
    parser.add_argument("--compare-result", type=Path,
                        help="reference baseline result.json; fail on declared projection differences")
    parser.add_argument("--runtime-bin", type=Path,
                        help="optional pinned static-runtime directory; Codex is not executed")
    args = parser.parse_args()
    candidate_mode = args.candidate or args.candidate_patch is not None
    if args.fatal_read and args.compare_result:
        parser.error("--fatal-read cannot use --compare-result: the candidate is an expected unsafe counterexample")
    cases = FATAL_CASES if args.fatal_read else CASES
    work = args.work_root.expanduser().absolute()
    work.mkdir(mode=0o700, parents=True, exist_ok=False)
    logs = work / "logs"
    logs.mkdir()
    status = {"baseline": PIN, "production_status": "PARTIAL",
              "production_implementation_installed": False,
              "candidate_fixture_compiled": candidate_mode,
              "classification": "rejected-fixture compatibility probes, not production/publication/security acceptance",
              "source_kind": "rejected candidate patch" if args.candidate_patch else (
                  "candidate overlay" if args.candidate else "signed baseline"),
              "cases": [], "external_codex_executed": False,
              "fatal_read_mode": args.fatal_read}
    if args.fatal_read:
        status["classification"] = "expected unsafe fatal-read characterization, not isolation or production acceptance"
    save(work / "result.json", dict(status, result="INCOMPLETE"))
    if not sys.platform.startswith("linux"):
        save(work / "result.json", dict(status, result="SKIPPED", reason="requires Linux"))
        return 77
    for tool in ("git", "cargo", "cc"):
        if shutil.which(tool) is None:
            save(work / "result.json", dict(status, result="SKIPPED", reason=f"missing {tool}"))
            return 77
    probe_path = FATAL_PROBE if args.fatal_read else PROBE
    if not probe_path.is_file():
        raise RuntimeError(f"missing companion probe source: {probe_path}")
    template_bytes = probe_path.read_bytes()
    if args.fatal_read:
        probe_bytes, marker = select_fatal_probe(template_bytes, candidate_mode)
        status.update(probe_template_sha256=hashlib.sha256(template_bytes).hexdigest(),
                      selected_probe_marker=marker)
    else:
        probe_bytes = template_bytes
    probe_sha = hashlib.sha256(probe_bytes).hexdigest()
    status["selected_probe_source_sha256"] = probe_sha
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
    # Archive and patch operations must use their explicit cwd, never an
    # inherited repository/index override from a developer shell.
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_PREFIX"):
        host_env.pop(key, None)
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
        overlay = {}
        if args.candidate:
            checked(["git", "rev-parse", "HEAD"], REPO, "candidate-head", host_env, 30)
            checked(["git", "diff", "--binary", "HEAD", "--", *CANDIDATE_INPUTS],
                    REPO, "candidate-diff", host_env, 30)
            checked(["git", "diff", "--name-only", "HEAD", "--", "src", "migrations", "Cargo.toml", "Cargo.lock"],
                    REPO, "candidate-tracked-scope", host_env, 30)
            tracked_scope = set((logs / "candidate-tracked-scope.stdout").read_text().splitlines())
            checked(["git", "ls-files", "--others", "--exclude-standard", "--", "src", "migrations"],
                    REPO, "candidate-untracked-scope", host_env, 30)
            untracked_scope = set((logs / "candidate-untracked-scope.stdout").read_text().splitlines())
            if not (tracked_scope | untracked_scope) <= set(CANDIDATE_INPUTS):
                raise RuntimeError("candidate production/test changes exceed the explicit overlay allowlist")
            for name in CANDIDATE_INPUTS:
                origin = REPO / name
                if not origin.is_file():
                    if name == "src/storage/sqlite/reconciliation_planning_tests.rs":
                        continue
                    raise RuntimeError("missing candidate input: " + name)
                destination = source / name
                content = origin.read_bytes()
                destination.write_bytes(content)
                overlay[name] = hashlib.sha256(content).hexdigest()
            if overlay["src/storage/sqlite.rs"] == before["src/storage/sqlite.rs"]:
                raise RuntimeError("candidate production input equals signed baseline")
            status["candidate_head"] = (logs / "candidate-head.stdout").read_text().strip()
        patch_digest = None
        if args.candidate_patch:
            patch = args.candidate_patch.expanduser().absolute()
            if patch.is_symlink() or not patch.is_file():
                raise RuntimeError("candidate patch must be a regular nonsymlink file")
            patch_bytes = patch.read_bytes()
            names = validated_patch(patch_bytes)
            for name in names:
                destination = source / name
                if destination.is_symlink() or not destination.resolve().is_relative_to(source.resolve()):
                    raise RuntimeError("candidate patch target is a symlink or escapes the archive")
            preserved_patch = work / "candidate.patch"
            preserved_patch.write_bytes(patch_bytes)
            patch_digest = hashlib.sha256(patch_bytes).hexdigest()
            checked(["git", "apply", "--check", str(preserved_patch)], source,
                    "candidate-patch-check", host_env, 30)
            checked(["git", "apply", str(preserved_patch)], source,
                    "candidate-patch-apply", host_env, 30)
            for name in names:
                destination = source / name
                if destination.is_symlink() or not destination.is_file():
                    raise RuntimeError("candidate patch failed regular-file validation")
                overlay[name] = digest(destination)
            status["candidate_patch_sha256"] = patch_digest
        sqlite = source / "src/storage/sqlite.rs"
        original = sqlite.read_text()
        function = "fn apply_reconciliation(" if candidate_mode else "fn reconcile_logical("
        start = original.index(function)
        end = original.index("\nfn get_or_create_assertion(", start)
        body = original[start:end]
        if CHECKPOINT in original or MEMBERSHIP_CHECKPOINT in original:
            raise RuntimeError("source already contains a probe checkpoint")
        if body.count("    Ok(actions)\n") != 1:
            raise RuntimeError("reconciliation return anchor differs")
        instrumented = body.replace("    Ok(actions)\n", CHECKPOINT + "    Ok(actions)\n")
        patched = original[:start] + instrumented + original[end:]
        membership_function = "fn missing_spaces(" if candidate_mode else "fn add_spaces("
        entry = patched.index("{", patched.index(membership_function)) + 1
        patched = patched[:entry] + "\n" + MEMBERSHIP_CHECKPOINT + patched[entry:]
        restored = patched.replace(CHECKPOINT, "", 1).replace("\n" + MEMBERSHIP_CHECKPOINT, "", 1)
        if restored != original:
            raise RuntimeError("unexpected production-body instrumentation")
        if args.fatal_read:
            fault_target = ("fn capture(conn: &Connection, files: &[ObservedFile], plans: &[Plan]) -> Self {"
                            if candidate_mode else "fn reconcile_logical(")
            target_start = patched.index(fault_target)
            entry = patched.index("{", target_start) + 1
            fault_guard = ("\n    #[cfg(test)]\n"
                           "    let _fatal_read_guard = observation_planning_tests::s3e4d_review_fatal_guard(conn);\n")
            patched = patched[:entry] + fault_guard + patched[entry:]
            if patched.replace(fault_guard, "", 1).replace(CHECKPOINT, "", 1).replace("\n" + MEMBERSHIP_CHECKPOINT, "", 1) != original:
                raise RuntimeError("fatal-read instrumentation does not restore source bytes")
            status["fatal_read_instrumentation_boundary"] = fault_target
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
        permitted = expected | set(overlay) | {str(composed_probe.relative_to(source))}
        if not expected <= changed or not (changed | (set(after) - set(before))) <= permitted:
            raise RuntimeError("archive changes exceeded the explicit input overlay")
        if not candidate_mode and (changed != expected or set(after) - set(before)
                                  != {str(composed_probe.relative_to(source))}):
            raise RuntimeError("baseline archive changed beyond test instrumentation")
        if module.read_text().removesuffix(attachment) != module_original:
            raise RuntimeError("existing native test helpers changed")
        save(work / "source-manifest.json", {"baseline": PIN, "before": before,
             "after": after, "changed": sorted(changed), "candidate_overlay": overlay,
             "production_logic_preserved_except_test_checkpoint": not candidate_mode,
             "candidate_patch_sha256": patch_digest,
             "probe_template_sha256": hashlib.sha256(template_bytes).hexdigest(),
             "selected_probe_marker": status.get("selected_probe_marker"),
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
        # Sequential archive runs may share a Cargo target. Force this exact
        # package to rebuild and authenticate that no source byte was altered.
        root_module = source / "src/lib.rs"
        root_digest = digest(root_module)
        old_mtime = root_module.stat().st_mtime_ns
        os.utime(root_module, None)
        if digest(root_module) != root_digest:
            raise RuntimeError("freshness guard changed source bytes")
        status["freshness_guard"] = {"path": "src/lib.rs", "sha256": root_digest,
                                     "previous_mtime_ns": old_mtime,
                                     "touched_mtime_ns": root_module.stat().st_mtime_ns}
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
        if units[0].get("fresh") is not False:
            raise RuntimeError("native package was not recompiled; shared-target freshness is unqualified")
        executable = Path(units[0]["executable"])
        status.update(test_executable=str(executable), test_executable_sha256=digest(executable),
                      features=units[0]["features"], probe_sha256=probe_sha,
                      clock_source_sha256=digest(clock_c), clock_helper_sha256=digest(clock_so))
        save(work / "compiler-artifacts.json", artifacts)
        for test, result_name in cases:
            case_env = dict(env, MINDSPACE_S3E4D_PROBE_OUT=str(work / "results"))
            if args.fatal_read or result_name.endswith("fixed_clock"):
                case_env.update(LD_PRELOAD=str(clock_so),
                                MINDSPACE_S3E4D_FIXED_REALTIME_SECONDS="1800000000",
                                MINDSPACE_S3E4D_FIXED_REALTIME_NANOS="123000000")
            checked([executable, "--exact", PREFIX + test, "--test-threads=1", "--nocapture"],
                    source, result_name, case_env, 60)
            text = (logs / (result_name + ".stdout")).read_text()
            if not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", text):
                raise RuntimeError("exact native test did not execute once: " + test)
            result = json.loads((work / "results" / (result_name + ".json")).read_text())
            if result.get("baseline") != PIN:
                raise RuntimeError("pinned reference baseline missing: " + test)
            if args.fatal_read:
                expected_mode = "candidate" if candidate_mode else "baseline"
                if result.get("mode") != expected_mode:
                    raise RuntimeError("native fatal probe source mode mismatches the requested fixture")
                if not result.get("all_rows_rolled_back") or not result.get("automatic_rollback_after_read_fault"):
                    raise RuntimeError("fatal read did not demonstrate automatic rollback: " + test)
                if result.get("adjacent_control_gap") != 1 or not result.get("fixed_unix_millis"):
                    raise RuntimeError("fatal read fixed-clock control missing: " + test)
                if not isinstance(result.get("failed_uuid_gap"), int) or result["failed_uuid_gap"] < 1:
                    raise RuntimeError("fatal read UUID observation missing: " + test)
                if result.get("subsequent_session_gap") != 1 or not result.get("actual_error"):
                    raise RuntimeError("fatal read subsequent-session/error evidence missing: " + test)
            elif result_name == "success_compatibility_fixed_clock":
                if not result.get("all_integrity_checks_passed") or len(result.get("trace", [])) != 6:
                    raise RuntimeError("successful compatibility trace missing: " + test)
            elif not result.get("all_rows_rolled_back"):
                raise RuntimeError("rollback assertion missing: " + test)
            if result_name == "read_failure_compatibility_fixed_clock" and len(result.get("trace", [])) != 3:
                raise RuntimeError("three read-failure traces required: " + test)
            projection = comparison_projection(result_name, result)
            save(work / "results" / (result_name + ".comparison.json"), projection)
            status["cases"].append({"test": PREFIX + test, "result": "REPRODUCED",
                                     "artifact": str(work / "results" / (result_name + ".json"))})
            save(work / "result.json", dict(status, result="INCOMPLETE"))
        if args.compare_result:
            reference = args.compare_result.expanduser().absolute()
            previous = json.loads(reference.read_text())
            if previous.get("source_kind") != "signed baseline" or previous.get("baseline") != PIN:
                raise RuntimeError("comparison requires the signed-baseline runner result")
            if len(previous.get("cases", [])) != len(cases):
                raise RuntimeError("reference does not contain the same exact case set")
            reference_cases = {case["test"]: case for case in previous["cases"]}
            comparisons = []
            for case in status["cases"]:
                other = reference_cases.get(case["test"])
                if other is None:
                    raise RuntimeError("reference case missing: " + case["test"])
                name = Path(case["artifact"]).stem
                expected = comparison_projection(name, json.loads(Path(other["artifact"]).read_text()))
                actual = comparison_projection(name, json.loads(Path(case["artifact"]).read_text()))
                equal = actual == expected
                comparisons.append({"test": case["test"], "equal": equal})
            save(work / "comparison.json", {"reference": str(reference), "cases": comparisons,
                 "uuid_scope": "validated fixed-clock encoded allocation offsets, not random suffix equality"})
            if not all(case["equal"] for case in comparisons):
                raise RuntimeError("baseline/candidate compatibility projection differs; see comparison.json")
            status["comparison"] = "PASS: all declared projections equal"
        save(work / "commands.json", command_records)
        result_label = ("PASS: expected unsafe fatal-read characterization reproduced" if args.fatal_read
                        else "PASS: declared compatibility probes reproduced")
        save(work / "result.json", dict(status, result=result_label))
        print(json.dumps({"result": result_label,
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
