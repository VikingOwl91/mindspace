#!/usr/bin/env bash
# Mandatory security gate for the MCP repository tools (ADR-029, MIND-18).
# Run it before every release and after any change to ingest, git handling,
# the sandbox or the allowlist. It needs a Linux kernel with Landlock ABI 3+.
#
#   scripts/security-profile.sh
#   MINDSPACE_RACE_ATTEMPTS=300 scripts/security-profile.sh   # more attempts
#
# Exit status is non-zero if anything fails.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export MINDSPACE_RACE_ATTEMPTS="${MINDSPACE_RACE_ATTEMPTS:-150}"

echo "== profile: ${MINDSPACE_RACE_ATTEMPTS} race attempts per race test"
cargo build

echo
echo "== 1/4  kernel sandbox + application layer (the default configuration)"
env -u MINDSPACE_TEST_MCP_ARGS cargo test --test ingest_confinement --test sandbox_failure_modes -- --test-threads=4

echo
echo "== 2/4  application layer alone (--insecure-no-sandbox)"
# r4 and r4b are excluded here on purpose: without the kernel, an alternates file
# that appears in the git directory AFTER the check cannot be excluded (measured:
# one leak in 5,626 random attempts, none in 700 directed ones). The static case
# (s3) stays in. See ADR-029, residual risk.
MINDSPACE_TEST_MCP_ARGS=--insecure-no-sandbox cargo test --test ingest_confinement -- --test-threads=4 --skip r4

echo
echo "== 3/4  unit tests (sandbox layout rules, canary probes, marker handling)"
cargo test --lib -- sandbox the_marker

echo
echo "== 4/4  existing MCP tests under the default sandbox"
cargo test --test mcp_repo_tools --test mcp_allowlist_modes --test mcp_ingest_timeout --test repo_info --test agent_e2e

echo
echo "security profile: PASSED"
