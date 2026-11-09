#!/bin/bash
# M7.3b.3b evidence: is the broker's cgroup tree contained in a systemd user service, and does
# stopping the unit end an escaped descendant after the WATCHDOG was SIGKILLed?
#
# usage: systemd_unit_containment.sh <label> <Delegate yes|no> <placement default|own>  (default and own now place the tree inside the unit)
# needs: a systemd user manager, and a mind binary built WITH the test hooks, i.e. the one that
# `cargo test --no-run` leaves at target/debug/mind (a plain `cargo build` removes the hooks).
# Prints where each process lives and whether the descendant survives `systemctl --user stop`.
set -u
LABEL=$1; DELEG=$2; PLACE=$3
MIND=${MIND:-$(cd "$(dirname "$0")/../.." && pwd)/target/debug/mind}
W=$(mktemp -d); W=$(cd $W && pwd -P)
mkdir -p $W/xdg/mindspace $W/project
export MINDSPACE_DB=$W/xdg/mindspace/mind.db XDG_DATA_HOME=$W/xdg XDG_STATE_HOME=$W/state HOME=$W
$MIND init >/dev/null; $MIND space create p >/dev/null
echo '# A' > $W/project/a.md
cat > $W/worker.sh <<EOS
#!/bin/sh
printf 'READY\n' >&5
read go <&5
setsid sh -c 'echo \$\$ > $W/desc.pid; exec sleep 600' &
sleep 600
EOS
chmod +x $W/worker.sh
UNIT=msexp-$LABEL-$$
ENVS="--setenv=MINDSPACE_DB=$MINDSPACE_DB --setenv=XDG_DATA_HOME=$XDG_DATA_HOME --setenv=XDG_STATE_HOME=$XDG_STATE_HOME --setenv=HOME=$HOME --setenv=MINDSPACE_BROKER_GATE=1 --setenv=MINDSPACE_BROKER_GATE_WORKER=$W/worker.sh --setenv=MINDSPACE_BROKER_GATE_WALL_MS=120000 --setenv=GATE_WALL_MS=120000 --setenv=GATE_NO_EVIDENCE=1"
[ "$PLACE" = own ] && ENVS="$ENVS --setenv=MINDSPACE_BROKER_GATE_CGROUP_PARENT=own"
systemd-run --user --unit=$UNIT -p Delegate=$DELEG $ENVS --collect \
  bash -c "(printf 'extract $W/project\n'; sleep 100000) | $MIND mcp --allow-root $W/project --grants --broker" >/dev/null 2>&1
for i in $(seq 1 100); do [ -s $W/desc.pid ] && break; sleep 0.1; done
D=$(cat $W/desc.pid 2>/dev/null)
FE=""; for p in $(pgrep -f "$MIND mcp --allow-root $W/project"); do [ "$(cat /proc/$p/comm 2>/dev/null)" = mind ] && FE=$p; done
WD=$(sed "s/0:://" /proc/$D/cgroup | grep -o "mindspace-[0-9]*" | head -1 | cut -d- -f2); echo "[$LABEL] frontend=$FE watchdog=$WD"
UCG=$(systemctl --user show $UNIT -p ControlGroup --value)
echo "[$LABEL] unit cgroup: $UCG"
echo "[$LABEL] frontend $FE in: $(sed 's/0:://' /proc/$FE/cgroup)"
echo "[$LABEL] watchdog $WD in: $(sed 's/0:://' /proc/$WD/cgroup)"
echo "[$LABEL] descendant $D in: $(sed 's/0:://' /proc/$D/cgroup)"
case "$(sed 's/0:://' /proc/$D/cgroup)" in $UCG/*) echo "[$LABEL] tree is INSIDE the unit";; *) echo "[$LABEL] tree is OUTSIDE the unit";; esac
kill -9 $WD
sleep 0.3
echo "[$LABEL] after SIGKILL of the watchdog: descendant $D state: $(awk '{print $3}' /proc/$D/stat 2>/dev/null || echo gone)"
T0=$(date +%s%N)
systemctl --user stop $UNIT
T1=$(date +%s%N)
sleep 0.2
echo "[$LABEL] after 'systemctl --user stop' ($(( (T1-T0)/1000000 )) ms): descendant $D state: $(awk '{print $3}' /proc/$D/stat 2>/dev/null || echo gone)"
kill -9 $D 2>/dev/null
rm -rf $W
