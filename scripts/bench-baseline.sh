#!/usr/bin/env bash
# Reproducible benchmark baseline for ingest, code search and DB size (MIND-1).
#
#   scripts/bench-baseline.sh                                  # synthetic fixture
#   MINDSPACE_BENCH_REPO="/a/repo /b/repo" scripts/bench-baseline.sh   # + real repos
#   RUNS=9 scripts/bench-baseline.sh                           # more samples (default 5)
#   BASELINE_FILE=saved.md scripts/bench-baseline.sh           # regression gate
#
# Isolation guarantees (verified by this script, not just assumed):
#  * Git ignores user and system config for the whole run (GIT_CONFIG_GLOBAL,
#    GIT_CONFIG_NOSYSTEM), so e.g. a global tag.gpgsign cannot affect results.
#  * A real repository is never touched: only its .git directory is copied into
#    a temp dir (hooks/ removed from the copy), and a fingerprint of the source
#    .git (path, size, mtime of every file) must be identical before and after.
#
# Metrics per target, each over RUNS fresh databases:
#   ingest (ms)             wall time of `mind ingest`
#   process start (ms)      median of 20 `mind --version` runs (fork/exec/link cost)
#   symbols query (ms)      median wall time of QUERY_REPEAT queries, incl. process start
#   symbols net (ms)        query minus process start (search work only, floored at 0)
#   DB size (KiB)
# Reported as median / min / max over the RUNS values.
#
# Regression gate (BASELINE_FILE): a metric regresses only if BOTH
#   current median > baseline median * (1 + THRESHOLD_PCT/100)   (default 25 %)
#   current min    > baseline median                              (not one outlier)
# and the absolute increase exceeds MIN_ABS_MS (default 5) for time metrics.
# Exit status is non-zero on regression or any failed run.
set -euo pipefail

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS="${RUNS:-5}"
THRESHOLD_PCT="${THRESHOLD_PCT:-25}"
MIN_ABS_MS="${MIN_ABS_MS:-5}"
QUERY_REPEAT="${QUERY_REPEAT:-20}"
START_REPEAT=20
WORK="$(mktemp -d "${TMPDIR:-/tmp}/mind-bench.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

(cd "$ROOT" && cargo build --release --quiet)
MIND="$ROOT/target/release/mind"

# Milliseconds (with fraction) from the shell, no process spawn.
now_ms() { local t="${EPOCHREALTIME/./}"; echo "$((t / 1000))"; }

# stats <values...> -> "median min max"
stats() {
  printf '%s\n' "$@" | sort -n | awk '
    { v[NR] = $1 }
    END {
      m = (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2
      printf "%d %d %d\n", m, v[1], v[NR]
    }'
}

fingerprint() { (cd "$1" && find . -type f -printf '%p %s %T@\n' | LC_ALL=C sort | sha256sum | cut -d' ' -f1); }

make_fixture() {
  python3 - "$1" <<'PY'
import os, subprocess, sys
root = sys.argv[1]
env = dict(os.environ,
    GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
    GIT_AUTHOR_NAME="bench", GIT_AUTHOR_EMAIL="bench@example.invalid",
    GIT_COMMITTER_NAME="bench", GIT_COMMITTER_EMAIL="bench@example.invalid")
def git(*a, **kw):
    subprocess.run(["git", *a], cwd=root, env=dict(env, **kw.get("env", {})),
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
os.makedirs(root, exist_ok=True)
git("init", "-q", "-b", "main")
FILES, COMMITS = 2400, 6          # 6 revisions of each file -> ~14k blobs
long_seg = "very_long_directory_name_segment_" * 3
for rev in range(COMMITS):
    for i in range(FILES):
        d = os.path.join(root, "crate%02d" % (i % 24), long_seg + str(i % 7), "m%03d" % (i % 50))
        os.makedirs(d, exist_ok=True)
        body = "".join(
            "pub fn item_%d_%d_r%d(x: u32) -> u32 { x + %d }\n" % (i, k, rev, rev)
            for k in range(3))
        body += "pub struct Type%d_%d;\nimpl Type%d_%d { pub fn run(&self) {} }\n" % (i, rev, i, rev)
        with open(os.path.join(d, "f%d.rs" % i), "w") as f:
            f.write(body)
    with open(os.path.join(root, "README.md"), "w") as f:
        f.write("# Bench\n\nrevision %d\n" % rev)
    git("add", "-A")
    dt = "2026-01-0%dT00:00:00Z" % (rev + 1)
    git("commit", "-q", "-m", "rev %d" % rev, env={"GIT_AUTHOR_DATE": dt, "GIT_COMMITTER_DATE": dt})
    git("tag", "-a", "-m", "release %d" % rev, "v0.%d.0" % rev, env={"GIT_COMMITTER_DATE": dt})
for t in range(200):                      # many lightweight tags
    git("tag", "light-%03d" % t)
PY
}

# Median of 20 `mind --version` wall times: the cost every CLI query pays.
startup_ms() {
  local i t0 t1 v=()
  for i in $(seq 1 "$START_REPEAT"); do
    t0="$(now_ms)"; "$MIND" --version >/dev/null; t1="$(now_ms)"
    v+=("$((t1 - t0))")
  done
  stats "${v[@]}" | cut -d' ' -f1
}

# bench_repo <label> <repo-dir>: prints markdown rows.
bench_repo() {
  local label="$1" repo="$2" objs
  objs="$(git -C "$repo" count-objects -v | awk '/^(count|in-pack):/ {s += $2} END {print s}')"
  local ingest=() start=() query=() net=() sizes=() r
  for r in $(seq 1 "$RUNS"); do
    local db="$WORK/$label-$r.db" t0 t1
    MINDSPACE_DB="$db" "$MIND" init >/dev/null
    MINDSPACE_DB="$db" "$MIND" space create bench >/dev/null
    t0="$(now_ms)"
    MINDSPACE_DB="$db" timeout 600 "$MIND" ingest "$repo" --space bench >/dev/null
    t1="$(now_ms)"
    ingest+=("$((t1 - t0))")
    sizes+=("$(( $(stat -c %s "$db") / 1024 ))")

    # Guard: a benchmark over an empty result set would measure nothing.
    if [ "$(MINDSPACE_DB="$db" "$MIND" symbols fn --limit 20 --json | wc -c)" -lt 50 ]; then
      echo "FAIL: symbols query returned no hits for $label" >&2; return 1
    fi
    local s q=() qm
    s="$(startup_ms)"
    local k
    for k in $(seq 1 "$QUERY_REPEAT"); do
      t0="$(now_ms)"; MINDSPACE_DB="$db" "$MIND" symbols fn --limit 20 >/dev/null; t1="$(now_ms)"
      q+=("$((t1 - t0))")
    done
    qm="$(stats "${q[@]}" | cut -d' ' -f1)"
    start+=("$s"); query+=("$qm"); net+=("$(( qm > s ? qm - s : 0 ))")
    rm -f "$db" "$db-wal" "$db-shm"
  done
  row() { local med mn mx; read -r med mn mx <<<"$(stats "${@:3}")"; echo "| $1 | $2 | $med | $mn | $mx |"; }
  echo "| $label | objects | $objs | - | - |"
  row "$label" "ingest (ms)" "${ingest[@]}"
  row "$label" "process start (ms)" "${start[@]}"
  row "$label" "symbols query incl. start (ms)" "${query[@]}"
  row "$label" "symbols net (ms)" "${net[@]}"
  row "$label" "DB size (KiB)" "${sizes[@]}"
}

# gate <current-table-file> <baseline-file>
gate() {
  python3 - "$1" "$2" "$THRESHOLD_PCT" "$MIN_ABS_MS" <<'PY'
import sys
cur_f, base_f, pct, min_abs = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
def load(p):
    d = {}
    for line in open(p):
        c = [x.strip() for x in line.strip().strip("|").split("|")]
        if len(c) == 5 and c[2].replace(".", "").isdigit() and c[1] != "objects":
            mn = c[3]
            d[(c[0], c[1])] = (float(c[2]), float(mn) if mn.replace(".", "").isdigit() else float(c[2]))
    return d
cur, base = load(cur_f), load(base_f)
bad = 0
for k, (med, mn) in sorted(cur.items()):
    if k not in base: continue
    bmed = base[k][0]
    is_time = "(ms)" in k[1]
    over = med > bmed * (1 + pct / 100) and mn > bmed
    if is_time and med - bmed <= min_abs: over = False
    if over:
        print(f"REGRESSION {k[0]} / {k[1]}: median {med:g} (min {mn:g}) vs baseline {bmed:g}", file=sys.stderr)
        bad = 1
sys.exit(bad)
PY
}

echo "Generating synthetic fixture..." >&2
make_fixture "$WORK/fixture"
: > "$WORK/table.md"
bench_repo synthetic "$WORK/fixture" >> "$WORK/table.md"

n=0
for src in ${MINDSPACE_BENCH_REPO:-}; do
  n=$((n + 1))
  [ -d "$src/.git" ] || { echo "not a git work tree: $src" >&2; exit 2; }
  before="$(fingerprint "$src/.git")"
  copy="$WORK/real-$n-$(basename "$src")"   # unique per repo: same basenames must not collide
  mkdir -p "$copy"
  cp -a "$src/.git" "$copy/.git"
  rm -rf "$copy/.git/hooks"                      # never keep executable hooks in the copy
  git -C "$copy" -c core.fsmonitor=false -c core.hooksPath=/dev/null checkout -q -f HEAD
  head="$(git -C "$copy" rev-parse --short HEAD)"
  bench_repo "$(basename "$(dirname "$src")")/$(basename "$src")@$head" "$copy" >> "$WORK/table.md"
  after="$(fingerprint "$src/.git")"
  if [ "$before" != "$after" ]; then
    echo "FAIL: $src/.git changed during the benchmark" >&2; exit 3
  fi
  echo "isolation ok: $src/.git fingerprint unchanged" >&2
done

echo "Commit: $(git -C "$ROOT" rev-parse --short HEAD)  Date: $(date -u +%F)  Runs: $RUNS  Host: $(uname -sr)"
echo
echo "| target | metric | median | min | max |"
echo "|---|---|---|---|---|"
cat "$WORK/table.md"
if [ -n "${BASELINE_FILE:-}" ]; then
  gate "$WORK/table.md" "$BASELINE_FILE"
fi
