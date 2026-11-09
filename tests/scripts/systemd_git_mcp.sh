#!/bin/bash
# M7.3b.5e evidence: the real `mind mcp --isolated-git-ingest --git-exe /usr/bin/git` under a systemd
# user service (Delegate=yes|no): does the tool work end to end, where does the worker run, and does
# stopping the unit (SIGTERM) leave no process behind?
#
# usage: systemd_git_mcp.sh <label> <Delegate yes|no>
# needs: a systemd user manager, python3, git at /usr/bin/git, a mind binary (MIND=, default target/debug/mind).
set -u
LABEL=$1; DELEG=$2
MIND=${MIND:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/mind}
W=$(mktemp -d); W=$(cd $W && pwd -P)
mkdir -p $W/xdg/mindspace $W/repo
export MINDSPACE_DB=$W/xdg/mindspace/mind.db XDG_DATA_HOME=$W/xdg XDG_STATE_HOME=$W/state HOME=$W
$MIND init >/dev/null; $MIND space create p >/dev/null
( cd $W/repo && git init -q -b main && printf 'pub fn f() {}\n' > a.rs && git add -A \
  && git -c user.name=t -c user.email=t@e commit -q -m one \
  && echo 0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d > .git/mindspace-repository-id )
cat > $W/client.py <<PY
import json, subprocess, sys, time
p = subprocess.Popen(["$MIND","mcp","--allow-root","$W/repo","--grants","--isolated-git-ingest","--git-exe","/usr/bin/git"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open("$W/err.txt","w"), text=True)
def rpc(i, m, params):
    p.stdin.write(json.dumps({"jsonrpc":"2.0","id":i,"method":m,"params":params})+"\n"); p.stdin.flush()
    return json.loads(p.stdout.readline())
rpc(1,"initialize",{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}})
p.stdin.write(json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"})+"\n"); p.stdin.flush()
names=[t["name"] for t in rpc(2,"tools/list",{})["result"]["tools"]]
r=rpc(3,"tools/call",{"name":"repo_ingest_git_isolated","arguments":{"path":"$W/repo","spaces":["p"]}})["result"]
open("$W/result.json","w").write(json.dumps({"tools":names,"isError":r.get("isError"),"s":r.get("structuredContent")}))
open("$W/ready","w").write(str(p.pid))
time.sleep(100000)
PY
UNIT=msgitmcp-$LABEL-$$
ENVS="--setenv=MINDSPACE_DB=$MINDSPACE_DB --setenv=XDG_DATA_HOME=$XDG_DATA_HOME --setenv=XDG_STATE_HOME=$XDG_STATE_HOME --setenv=HOME=$HOME"
systemd-run --user --unit=$UNIT -p Delegate=$DELEG $ENVS --collect python3 $W/client.py >/dev/null 2>&1
for i in $(seq 1 200); do [ -e $W/ready ] && break; sleep 0.1; done
if [ ! -e $W/ready ]; then echo "$LABEL Delegate=$DELEG: NO RESULT (stderr: $(head -c 300 $W/err.txt))"; systemctl --user stop $UNIT 2>/dev/null; exit 0; fi
UCG=$(systemctl --user show $UNIT -p ControlGroup --value)
python3 - <<PY
import json
d=json.load(open("$W/result.json")); s=d["s"] or {}
print("$LABEL Delegate=$DELEG: tool listed:", "repo_ingest_git_isolated" in d["tools"], "| isError:", d["isError"],
      "| commit_state:", s.get("commit_state"), "| worker cgroup:", (s.get("isolation") or {}).get("worker",{}).get("cgroup"),
      "| contained:", (s.get("isolation") or {}).get("worker",{}).get("contained_in_server_cgroup"),
      "| error:", (s.get("error") or {}).get("kind"))
PY
echo "  unit cgroup: $UCG"
PROCS_BEFORE=$(cat /sys/fs/cgroup$UCG/cgroup.procs 2>/dev/null | wc -l)
T0=$(date +%s%N); systemctl --user stop $UNIT; T1=$(date +%s%N)
sleep 0.3
LEFT=$(pgrep -f "mind-wd|$W/repo" | tr '\n' ' ')
echo "  processes in the unit before stop: $PROCS_BEFORE; stop took $(( (T1-T0)/1000000 )) ms; leftover matching processes: [${LEFT}]; cgroup dir gone: $([ -d /sys/fs/cgroup$UCG ] && echo no || echo yes)"
