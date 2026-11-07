#!/usr/bin/env python3
"""MIND-21: MCP end-to-end smoke test with a REAL Claude Code client.

Claude Code (`claude -p`, headless, own process, own MCP client) is started with
only the `mindspace` MCP server and no built-in file/shell tools, so every
answer has to come through MCP tool calls. Verdicts are computed from the raw
tool calls and tool results in the client's stream-json transcript and checked
against independent oracles (the repository source via grep/git, raw DB scans,
file fingerprints, SQLite integrity), never from the model's prose.

Everything happens in a temporary directory. Nothing is pushed or published.

  scripts/mcp-e2e-claude.py [--mind target/release/mind] [--keep] [--model MODEL]

Needs: `claude` on PATH and logged in (uses your account and tokens), `git`,
Linux with Landlock ABI >= 3 (the sandbox must be ENFORCED, the test checks it).
Exit status 0 only if every check passed.
"""
import argparse, hashlib, json, os, random, shutil, signal, sqlite3, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--mind", default="target/release/mind")
ap.add_argument("--keep", action="store_true")
ap.add_argument("--model", default=None)
args = ap.parse_args()
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MIND = os.path.abspath(args.mind)
W = tempfile.mkdtemp(prefix="mind-e2e-")
RW, RO, OUT, DB, RUN = (os.path.join(W, d) for d in ("rw", "ro", "outside", "db", "run"))
for d in (RW, RO, OUT, DB, RUN):
    os.makedirs(d)
GENV = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
IDENT = ["-c", "user.name=t", "-c", "user.email=t@e.invalid"]
DBF = os.path.join(DB, "mind.db")
results = []


def check(name, ok, evidence=""):
    results.append((name, bool(ok), evidence))
    print(("PASS " if ok else "FAIL ") + name + (f"   [{evidence}]" if evidence else ""), flush=True)


def git(cwd, *a):
    # `git commit` spawns a detached `git maintenance run --auto` that briefly creates
    # objects/maintenance.lock; switch it off so fixture fingerprints cannot race with it.
    return subprocess.run(["git", "-c", "maintenance.auto=false", "-c", "gc.auto=0", *a], cwd=cwd, env=GENV, check=True, capture_output=True, text=True).stdout.strip()


def commit(cwd, msg):
    git(cwd, *IDENT, "commit", "-q", "-m", msg)
    return git(cwd, "rev-parse", "HEAD")


# ---------------------------------------------------------------- world
REPO = os.path.join(RW, "project")
git(W, "clone", "-q", "--no-hardlinks", ROOT, REPO)
git(REPO, "checkout", "-q", git(ROOT, "rev-parse", "HEAD"))
HEAD0 = git(REPO, "rev-parse", "HEAD")
os.makedirs(os.path.join(RO, "notes"))
git(os.path.join(RO, "notes"), "init", "-q", "-b", "main")
open(os.path.join(RO, "notes", "lib.rs"), "w").write("pub fn readonly_probe_symbol() -> u32 { 42 }\n")
git(os.path.join(RO, "notes"), "add", "-A"); commit(os.path.join(RO, "notes"), "init")
BAIT = os.path.join(OUT, "bait")
os.makedirs(BAIT)
git(BAIT, "init", "-q", "-b", "main")
open(os.path.join(BAIT, "s.rs"), "w").write("pub fn OUTSIDE_SECRET_TOKEN_91827() {}\n")
git(BAIT, "add", "-A"); commit(BAIT, "init")
os.makedirs(os.path.join(RW, "evil-symlink")); os.symlink(os.path.join(BAIT, ".git"), os.path.join(RW, "evil-symlink", ".git"))
os.makedirs(os.path.join(RW, "evil-pointer")); open(os.path.join(RW, "evil-pointer", ".git"), "w").write(f"gitdir: {BAIT}/.git\n")
os.symlink(BAIT, os.path.join(RW, "linkdir"))

menv = dict(os.environ, MINDSPACE_DB=DBF)
for a in (["init"], ["space", "create", "preview"]):
    subprocess.run([MIND, *a], env=menv, check=True, capture_output=True)
MCP = os.path.join(W, "mcp.json")
json.dump({"mcpServers": {"mindspace": {"command": MIND, "args": [
    "--db", DBF, "mcp", "--allow-root", RW + ":rw", "--allow-root", RO + ":ro", "--ingest-timeout", "120"]}}}, open(MCP, "w"))


def fingerprint():
    h = {}
    for base in (RO, OUT):
        for dp, _dirs, fn in os.walk(base):
            for f in fn:
                p = os.path.join(dp, f)
                st = os.lstat(p)
                h[p] = (st.st_size, st.st_mtime_ns, hashlib.sha256(open(p, "rb").read()).hexdigest() if os.path.isfile(p) and not os.path.islink(p) else "-")
    return h


FP0 = fingerprint()


def db_has(token):
    for suffix in ("", "-wal"):
        try:
            if token.encode() in open(DBF + suffix, "rb").read():
                return True
        except FileNotFoundError:
            pass
    return False


# ---------------------------------------------------------------- client
def run_claude(label, prompt):
    cmd = ["claude", "-p", prompt, "--mcp-config", MCP, "--strict-mcp-config", "--allowedTools", "mcp__mindspace",
           "--disallowedTools", "Bash Read Write Edit Glob Grep WebFetch WebSearch Agent Task", "--permission-mode", "default",
           "--no-session-persistence", "--max-turns", "40", "--output-format", "stream-json", "--verbose"]
    if args.model:
        cmd += ["--model", args.model]
    t0 = time.time()
    p = subprocess.run(cmd, cwd=RUN, capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=900)
    open(os.path.join(RUN, label + ".jsonl"), "w").write(p.stdout)
    calls, init = [], {}
    uses = {}
    for line in p.stdout.splitlines():
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if d.get("type") == "system" and d.get("subtype") == "init":
            init = d
        elif d.get("type") == "assistant":
            for c in d["message"]["content"]:
                if c["type"] == "tool_use":
                    uses[c["id"]] = {"name": c["name"].replace("mcp__mindspace__", ""), "input": c["input"], "text": None, "error": None}
                    calls.append(uses[c["id"]])
        elif d.get("type") == "user":
            for c in d["message"].get("content", []):
                if isinstance(c, dict) and c.get("type") == "tool_result" and c["tool_use_id"] in uses:
                    x = c.get("content")
                    uses[c["tool_use_id"]]["text"] = x if isinstance(x, str) else " ".join(i.get("text", "") for i in (x or []))
                    uses[c["tool_use_id"]]["error"] = bool(c.get("is_error"))
    mcp = [c for c in calls if c["text"] is not None and not c["name"].startswith("ToolSearch") and c["name"] in
           {"code_search", "memory_search", "repo_ingest", "repo_outline", "space_list", "space_create"}]
    print(f"   [{label}] {len(mcp)} mindspace tool calls, {time.time() - t0:.1f}s wall", flush=True)
    return init, mcp


def jv(c):
    try:
        return json.loads(c["text"])
    except Exception:
        return {}


def hits(c):
    return jv(c).get("hits", [])


def src_def(pattern):
    """(path, start_line) of the first definition matching the regex, from the checked-out source."""
    out = subprocess.run(["grep", "-rnE", "--include=*.rs", pattern, "src"], cwd=REPO, capture_output=True, text=True).stdout.splitlines()
    path, line, _ = out[0].split(":", 2)
    return path, int(line)


def find_hit(calls, name, path, line=None):
    for c in calls:
        if c["name"] == "code_search":
            for h in hits(c):
                if h["name"] == name and h["path"] == path and (line is None or h["start_line"] == line):
                    return h
    return None


# ---------------------------------------------------------------- S1 discovery + S2 ingest/questions
init, calls = run_claude("s1", f"Use ONLY the mindspace MCP tools. 1) repo_outline on {REPO}. 2) repo_ingest {REPO} into the space \"preview\". "
    "3) code_search (space preview) for each of: git_command, ingest_confined, GitPin, SandboxState, enter. Report file path and line for each.")
srv = {s["name"]: s["status"] for s in init.get("mcp_servers", [])}
check("S1 Claude Code connected to the mindspace MCP server", srv.get("mindspace") == "connected", str(srv))
check("S1 Claude Code discovered the tools", all(f"mcp__mindspace__{t}" in init.get("tools", []) for t in ("repo_ingest", "repo_outline", "code_search", "memory_search")))
ing = [c for c in calls if c["name"] == "repo_ingest"]
check("S2 repo_ingest was called through MCP and succeeded", len(ing) == 1 and not ing[0]["error"], ing[0]["text"][:80] if ing else "no call")
check("S2 sandbox ENFORCED (Landlock) in the real server", ing and str(jv(ing[0]).get("sandbox", "")).startswith("enforced"), str(jv(ing[0]).get("sandbox")) if ing else "")
check("S2 repo_outline called", any(c["name"] == "repo_outline" and not c["error"] for c in calls))
for name, pat in (("git_command", r"^fn git_command\("), ("ingest_confined", r"^pub fn ingest_confined"),
                  ("GitPin", r"^pub struct GitPin"), ("SandboxState", r"^pub enum SandboxState"), ("enter", r"^pub fn enter\(")):
    p, l = src_def(pat)
    check(f"S2 {name}: index answer == source ({p}:{l})", find_hit(calls, name, p, l) is not None)

# ---------------------------------------------------------------- S3 boundaries
init, calls = run_claude("s3", "Use ONLY the mindspace MCP tools. Security boundary test, no workarounds. One call each: "
    f"(1) repo_outline {BAIT} (2) repo_ingest {BAIT} into preview (3) repo_outline {RO}/notes (4) repo_ingest {RO}/notes into preview "
    f"(5) repo_outline {RW}/../outside/bait (6) repo_ingest {RW}/evil-symlink into preview (7) repo_ingest {RW}/evil-pointer into preview "
    f"(8) repo_ingest {RW}/linkdir into preview (9) repo_outline outside/bait (10) repo_outline /etc (11) code_search OUTSIDE_SECRET_TOKEN_91827.")


def call_for(name, frag):
    for c in calls:
        if c["name"] == name and frag in json.dumps(c["input"]):
            return c


def refused(name, frag):
    c = call_for(name, frag)
    return c is not None and c["error"] is True


check("S3 outline outside every root refused", refused("repo_outline", f'"{BAIT}"'))
check("S3 ingest outside every root refused", refused("repo_ingest", f'"{BAIT}"'))
c = call_for("repo_outline", f"{RO}/notes")
check("S3 outline in the ro root allowed", c is not None and not c["error"])
c = call_for("repo_ingest", f"{RO}/notes")
check("S3 ingest in the ro root refused (read-only)", c is not None and c["error"] and "read-only" in c["text"], (c or {}).get("text", "")[:60])
check("S3 '..' escape refused", refused("repo_outline", "/../outside/bait"))
check("S3 .git symlink to outside refused", refused("repo_ingest", "evil-symlink"))
check("S3 gitdir pointer to outside refused", refused("repo_ingest", "evil-pointer"))
check("S3 symlinked directory to outside refused", refused("repo_ingest", "linkdir"))
check("S3 relative path refused", refused("repo_outline", '"outside/bait"'))
check("S3 /etc refused", refused("repo_outline", '"/etc"'))
check("S3 oracle: no outside token in the database (raw scan)", not db_has("OUTSIDE_SECRET_TOKEN_91827"))
check("S3 oracle: no ro-root content in the database (raw scan)", not db_has("readonly_probe_symbol"))
check("S3 oracle: ro and outside trees byte-identical (content, size, mtime)", fingerprint() == FP0)

# ---------------------------------------------------------------- S4 restart + re-ingest
init, calls = run_claude("s4", f"Use ONLY the mindspace MCP tools, NEW session. 1) space_list 2) code_search ingest_confined in preview 3) repo_ingest {REPO} into preview 4) code_search ingest_confined again.")
cs = [c for c in calls if c["name"] == "code_search"]
check("S4 data persisted across the server restart (hit before any ingest)", len(cs) >= 1 and len(hits(cs[0])) == 1 and calls[0]["name"] == "space_list")
ing = [c for c in calls if c["name"] == "repo_ingest"][0]
i0 = jv(ing)["ingested"][0]
check("S4 re-ingest of an unchanged repository is idempotent", i0["derivations_new"] == 0 and i0["objects_new"] == 0 and i0["stale_retired"] == 0, f'new={i0["derivations_new"]} reused={i0["derivations_reused"]}')
check("S4 same entity id before and after (no duplicate)", len(cs) >= 2 and hits(cs[0])[0]["entity_id"] == hits(cs[-1])[0]["entity_id"] and len(hits(cs[-1])) == 1)

# ---------------------------------------------------------------- S5 mutations + unexpected files
src = open(os.path.join(REPO, "src/ingest/git.rs")).read()
open(os.path.join(REPO, "src/ingest/git.rs"), "w").write(src.replace("fn git_command(", "fn git_command_renamed(").replace("git_command(", "git_command_renamed("))
open(os.path.join(REPO, "src/ingest/confined.rs"), "a").write("\npub fn COMMITTED_NEW_FN_5521() {}\n")
os.symlink("/etc/passwd", os.path.join(REPO, "tracked_link.rs"))
open(os.path.join(REPO, "src/broken_syntax.rs"), "w").write("pub fn broken( {{{ \n")
open(os.path.join(REPO, "src/invalid_utf8.rs"), "wb").write(b"pub fn \xff\xfe_invalid_utf8() {}\n")
open(os.path.join(REPO, os.fsdecode(b"odd\xe9name.rs")), "wb").write(b"x")
git(REPO, "add", "src/ingest/git.rs", "src/ingest/confined.rs", "tracked_link.rs", "src/broken_syntax.rs", "src/invalid_utf8.rs")
subprocess.run(["bash", "-c", "git add -- odd*name.rs"], cwd=REPO, env=GENV, check=True)
HEAD1 = commit(REPO, "mutation: rename, new fn, oddities")
open(os.path.join(REPO, "src/ingest/mod.rs"), "a").write("\npub fn UNCOMMITTED_ONLY_FN_7733() {}\n")
open(os.path.join(REPO, "src/untracked.rs"), "w").write("pub fn UNTRACKED_FN_8844() {}\n")
os.mkfifo(os.path.join(REPO, "src/untracked_fifo.rs"))
with open(os.path.join(REPO, "big_untracked.bin"), "wb") as f:
    f.truncate(60_000_000)
status0 = git(REPO, "status", "--porcelain")
init, calls = run_claude("s5", f"Use ONLY the mindspace MCP tools. repo_ingest {REPO} into preview. Then code_search (all_versions=true) for each of: git_command_renamed, git_command, COMMITTED_NEW_FN_5521, UNCOMMITTED_ONLY_FN_7733, UNTRACKED_FN_8844.")
ing = [c for c in calls if c["name"] == "repo_ingest"]
check("S5 ingest succeeds with FIFO, 60 MB untracked file, broken/invalid-UTF-8/odd-named/symlink files", len(ing) == 1 and not ing[0]["error"], ing[0]["text"][:60] if ing else "")
j = jv(ing[0])["ingested"][0] if ing else {}
check("S5 recorded as fast-forward to the new HEAD", any(e.get("to_oid") == HEAD1 and e.get("ref_name") == "HEAD" for e in j.get("events", {}).get("sample", [])))
check("S5 ingest stayed fast (no hang on the FIFO)", jv(ing[0]).get("elapsed_ms", 10**9) < 20000 if ing else False, str(jv(ing[0]).get("elapsed_ms")) if ing else "")


def by_name(n):
    return [h for c in calls if c["name"] == "code_search" and json.dumps(c["input"]).find(f'"{n}"') >= 0 for h in hits(c) if h["name"] == n]


check("S5 renamed function is current, the old name is stale", any(h["current"] for h in by_name("git_command_renamed")) and by_name("git_command") and not any(h["current"] for h in by_name("git_command")))
check("S5 committed new function found at the new commit", any(h["current"] and h["commit_oid"] == HEAD1 for h in by_name("COMMITTED_NEW_FN_5521")))
check("S5 uncommitted and untracked content NOT indexed", not by_name("UNCOMMITTED_ONLY_FN_7733") and not by_name("UNTRACKED_FN_8844") and not db_has("UNCOMMITTED_ONLY_FN_7733") and not db_has("UNTRACKED_FN_8844"))
check("S5 symlink target (/etc/passwd) not read into the database", not db_has("root:x:0:0"))
check("S5 working tree untouched by the ingest", git(REPO, "status", "--porcelain") == status0)
check("S5 ro and outside trees still byte-identical", fingerprint() == FP0)

# ---------------------------------------------------------------- S6 crash recovery
def kill_ingest(delay):
    p = subprocess.Popen([MIND, "--db", DBF, "mcp", "--allow-root", RW + ":rw", "--ingest-timeout", "120"], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    assert p.stdin is not None and p.stdout is not None
    pin = p.stdin
    w = lambda o: (pin.write(json.dumps(o) + "\n"), pin.flush())
    w({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "kill", "version": "0"}}})
    p.stdout.readline()
    w({"jsonrpc": "2.0", "method": "notifications/initialized"})
    w({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "repo_ingest", "arguments": {"path": REPO, "spaces": ["preview"]}}})
    time.sleep(delay)
    p.send_signal(signal.SIGKILL); p.wait()


def integrity():
    c = sqlite3.connect(DBF)
    r = (c.execute("pragma integrity_check").fetchone()[0], len(c.execute("pragma foreign_key_check").fetchall()))
    c.close()
    return r


partial = False
attempt = -1
for attempt in range(8):  # need a kill that really lands before the write finished
    open(os.path.join(REPO, "src/ingest/confined.rs"), "a").write(f"\npub fn RECOVERY_FN_{attempt}_6600() {{}}\n")
    git(REPO, "add", "src/ingest/confined.rs"); commit(REPO, f"recovery probe {attempt}")
    kill_ingest(random.uniform(0.08, 0.3))
    c = sqlite3.connect(DBF)
    partial = c.execute("select count(*) from code_entities where name=?", (f"RECOVERY_FN_{attempt}_6600",)).fetchone()[0] == 0
    c.close()
    if partial:
        break
    # the kill came too late; the ingest finished, try again with a new commit
check("S6 database passes integrity_check/foreign_key_check after SIGKILL during ingest", integrity() == ("ok", 0), str(integrity()))
check("S6 kill landed before the ingest finished (a real partial state was tested)", partial, f"attempt {attempt}")
init, calls = run_claude("s6", f"Use ONLY the mindspace MCP tools. A previous server process was killed during an ingest. code_search RECOVERY_FN_{attempt}_6600 (all_versions=true), then repo_ingest {REPO} into preview, then code_search it again.")
cs = [c for c in calls if c["name"] == "code_search"]
check("S6 after the crash the new symbol is absent, then present after the client's re-ingest", len(cs) == 2 and not hits(cs[0]) and any(h["current"] for h in hits(cs[1])))
check("S6 database passes integrity_check after recovery", integrity() == ("ok", 0))
check("S6 ro and outside trees still byte-identical", fingerprint() == FP0)

# ---------------------------------------------------------------- report
bad = [r for r in results if not r[1]]
print(f"\n{len(results) - len(bad)}/{len(results)} checks passed" + ("" if not bad else "; FAILED: " + ", ".join(b[0] for b in bad)))
print("transcripts:", RUN if args.keep else "(removed; use --keep)")
if not args.keep:
    shutil.rmtree(W, ignore_errors=True)
sys.exit(1 if bad else 0)
