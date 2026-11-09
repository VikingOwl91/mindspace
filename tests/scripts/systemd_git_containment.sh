#!/bin/bash
# M7.3b.5d evidence: is a GIT worker's tree (worker + its blocked git child) contained in a systemd
# user service, and does stopping the unit end it after the WATCHDOG was SIGKILLed?
#
# usage: systemd_git_containment.sh <label> <Delegate yes|no>
# needs: a systemd user manager, git at /usr/bin/git, and a mind binary built WITH the test hooks
# (the one `cargo test --no-run` leaves at target/debug/mind).
set -u
LABEL=$1; DELEG=$2
MIND=${MIND:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/mind}
W=$(mktemp -d); W=$(cd $W && pwd -P)
mkdir -p $W/xdg/mindspace $W/repo
export MINDSPACE_DB=$W/xdg/mindspace/mind.db XDG_DATA_HOME=$W/xdg XDG_STATE_HOME=$W/state HOME=$W
$MIND init >/dev/null; $MIND space create p >/dev/null
( cd $W/repo && git init -q -b main && echo '# A' > a.md && git add -A \
  && git -c user.name=t -c user.email=t@e commit -q -m one \
  && echo 0190b2c4-7d5e-7a31-9c11-3f2d5a6b7c8d > .git/mindspace-repository-id )
UNIT=msgit-$LABEL-$$
ENVS="--setenv=MINDSPACE_DB=$MINDSPACE_DB --setenv=XDG_DATA_HOME=$XDG_DATA_HOME --setenv=XDG_STATE_HOME=$XDG_STATE_HOME --setenv=HOME=$HOME --setenv=MINDSPACE_BROKER_GATE=1 --setenv=MINDSPACE_BROKER_GATE_GIT=/usr/bin/git --setenv=MINDSPACE_BROKER_GATE_GIT_MODE=hang --setenv=MINDSPACE_BROKER_GATE_WALL_MS=120000 --setenv=GATE_WALL_MS=120000"
systemd-run --user --unit=$UNIT -p Delegate=$DELEG $ENVS --collect \
  bash -c "(printf 'git-extract $W/repo\n'; sleep 100000) | $MIND mcp --allow-root $W/repo --grants --broker > $W/out.txt 2>$W/err.txt" >/dev/null 2>&1
FE=""; for i in $(seq 1 100); do
  for p in $(pgrep -f "$MIND mcp --allow-root $W/repo"); do [ "$(cat /proc/$p/comm 2>/dev/null)" = mind ] && FE=$p; done
  [ -n "$FE" ] && break; sleep 0.1; done
WD=""; for i in $(seq 1 100); do
  for p in $(pgrep -f "$MIND mcp --allow-root $W/repo"); do
    case "$(cat /proc/$p/comm 2>/dev/null)" in mind-wd*) WD=$p;; esac
  done
  [ -n "$WD" ] && break; sleep 0.1; done
UCG=$(systemctl --user show $UNIT -p ControlGroup --value)
# the operation's cgroup holds exactly the worker and its git child: find them THERE
WK=""; GC=""; for i in $(seq 1 300); do
  OPDIR=$(ls -d /sys/fs/cgroup$UCG/mindspace-*/op-* 2>/dev/null | head -1)
  if [ -n "$OPDIR" ]; then
    for p in $(cat $OPDIR/cgroup.procs 2>/dev/null); do
      c=$(tr "\0" " " < /proc/$p/cmdline 2>/dev/null)
      case "$c" in *git-extraction-worker*) WK=$p;; /usr/bin/git*) GC=$p;; esac
    done
  fi
  [ -n "$WK" ] && [ -n "$GC" ] && break; sleep 0.1; done
BR=$(awk '{print $4}' /proc/$WK/stat 2>/dev/null)
echo "[$LABEL] frontend=$FE watchdog=$WD broker=$BR worker=$WK git-child=$GC"
if [ -z "$WD" ]; then
  sleep 3
  echo "[$LABEL] NO broker tree was created; frontend still running: $([ -d /proc/$FE ] && echo yes || echo no); it answered: [$(head -c 300 $W/out.txt)]; stderr: [$(head -c 300 $W/err.txt)]; unit state: $(systemctl --user is-active $UNIT 2>&1)"
  systemctl --user stop $UNIT 2>/dev/null; rm -rf $W; exit 0
fi
echo "[$LABEL] unit cgroup: $UCG"
for who in "frontend:$FE" "watchdog:$WD" "worker:$WK" "git-child:$GC"; do
  n=${who%%:*}; p=${who##*:}
  cg=$(sed 's/0:://' /proc/$p/cgroup 2>/dev/null)
  case "$cg" in $UCG|$UCG/*) where="INSIDE the unit";; *) where="OUTSIDE the unit";; esac
  echo "[$LABEL] $n $p in: $cg  -> $where"
done
kill -9 $WD
sleep 0.3
st() { awk '{print $3}' /proc/$1/stat 2>/dev/null || echo gone; }
echo "[$LABEL] after SIGKILL of the watchdog: worker $(st $WK), git-child $(st $GC)"
T0=$(date +%s%N); systemctl --user stop $UNIT; T1=$(date +%s%N); sleep 0.3
echo "[$LABEL] after 'systemctl --user stop' ($(( (T1-T0)/1000000 )) ms): worker $(st $WK), git-child $(st $GC), broker $(st $BR)"
kill -9 $WK $GC 2>/dev/null
rm -rf $W
