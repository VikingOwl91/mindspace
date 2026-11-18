#!/bin/sh
# Run one CI feature profile. Record only test names and the prerequisites exercised.
set -eu
export MINDSPACE_STRICT_TESTS=1
report=$(mktemp "${TMPDIR:-/tmp}/mindspace-prerequisites.XXXXXX")
trap 'rm -f "$report"' EXIT
export MINDSPACE_TEST_PREREQUISITE_LOG="$report"

# Use the delegation already granted to the runner when available. Otherwise
# request a user scope with an empty parent for per-operation memory limits.
group=$(sed -n 's|^0::/||p' /proc/self/cgroup)
group=/sys/fs/cgroup/$group
has_controllers() {
    [ -w "$1" ] && grep -qw memory "$1/cgroup.subtree_control" && grep -qw pids "$1/cgroup.subtree_control"
}
parent=${group%/*}
if [ -w "$group" ] && [ -f "$group/cgroup.kill" ] &&
   { has_controllers "$group" || { [ "$parent" != /sys/fs/cgroup ] && has_controllers "$parent"; }; }; then
    cargo test --locked "$@"
else
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    systemd-run --user --scope --property=Delegate=yes --property=DelegateSubgroup=tests \
        cargo test --locked "$@"
fi

test -s "$report"
awk '{ print $1, $2 }' "$report" | sort -u | wc -l |
    awk '{ print "Tests that exercised a runtime prerequisite:", $1 }'
