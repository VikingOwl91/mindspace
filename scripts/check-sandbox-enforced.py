#!/usr/bin/env python3
"""Fail loudly unless the real `mind mcp` process runs with the Landlock sandbox ENFORCED.

Starts `mind mcp` WITHOUT --insecure-no-sandbox, calls repo_outline on a temporary
git repository inside an allowed root and requires:
  1. the result's `sandbox` field starts with "enforced" (the server verifies this by
     probes at start-up, see ADR-029), and
  2. a path outside the root is refused.
Used by CI before the security profile so that missing kernel support is a visible
failure and never a silent skip. Usage: check-sandbox-enforced.py [path/to/mind]
"""
import json, os, subprocess, sys, tempfile

mind = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/mind")
env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
with tempfile.TemporaryDirectory(prefix="mind-sbx-") as w:
    root, outside, dbdir = (os.path.realpath(os.path.join(w, d)) for d in ("root", "outside", "db"))
    for d in (root, outside, dbdir):
        os.makedirs(d)
    repo = os.path.join(root, "r")
    os.makedirs(repo)
    for a in (["init", "-q", "-b", "main"], ["-c", "user.name=t", "-c", "user.email=t@e.invalid", "commit", "-q", "--allow-empty", "-m", "i"]):
        subprocess.run(["git", *a], cwd=repo, env=env, check=True, capture_output=True)
    open(os.path.join(repo, "a.rs"), "w").write("pub fn a() {}\n")
    subprocess.run(["git", "add", "-A"], cwd=repo, env=env, check=True)
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@e.invalid", "commit", "-q", "-m", "f"], cwd=repo, env=env, check=True)
    db = os.path.join(dbdir, "mind.db")
    subprocess.run([mind, "--db", db, "init"], check=True, capture_output=True, env=env)
    p = subprocess.Popen([mind, "--db", db, "mcp", "--allow-root", root + ":rw"], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    assert p.stdin is not None and p.stdout is not None and p.stderr is not None
    stdin, stdout, stderr = p.stdin, p.stdout, p.stderr
    n = 0

    def rpc(method, params=None):
        global n
        n += 1
        stdin.write(json.dumps({"jsonrpc": "2.0", "id": n, "method": method, "params": params or {}}) + "\n")
        stdin.flush()
        line = stdout.readline()
        if not line:
            p.kill()
            sys.exit(f"FAIL: mind mcp exited without answering {method}\nstderr: {stderr.read()[:800]}")
        return json.loads(line)

    rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ci", "version": "0"}})
    stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
    stdin.flush()
    ok = rpc("tools/call", {"name": "repo_outline", "arguments": {"path": repo}})["result"]
    bad = rpc("tools/call", {"name": "repo_outline", "arguments": {"path": outside}})["result"]
    p.kill()
    err = ""

text = ok["content"][0]["text"]
if ok.get("isError"):
    sys.exit(f"FAIL: repo_outline inside the root failed (sandbox not established?): {text[:300]}\nstderr: {err[:500]}")
sandbox = json.loads(text).get("sandbox", "")
if not str(sandbox).startswith("enforced"):
    sys.exit(f"FAIL: sandbox is not enforced: {sandbox!r}\nstderr: {err[:500]}")
if not bad.get("isError"):
    sys.exit("FAIL: a path outside the allowed root was NOT refused")
print(f"OK: sandbox {sandbox!r}; outside path refused")
