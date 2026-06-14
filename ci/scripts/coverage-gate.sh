#!/usr/bin/env bash
# Coverage gate.
#
# The architecture mandate (Testing Strategy) requires >=80% coverage before a
# phase advances. The gate is on the workspace TOTAL line coverage, which is the
# figure the mandate measures: compile-time-only constructs (object-safety proof
# functions, `#[derive]` glue) legitimately depress some per-file numbers without
# representing untested behaviour, and the total absorbs them honestly.
#
# Override the threshold with COVERAGE_MIN (default 80) for local experiments;
# CI always uses the mandated 80.
set -euo pipefail

THRESHOLD="${COVERAGE_MIN:-80}"
cd "$(dirname "$0")/../.."

echo "==> Coverage gate: workspace total line coverage must be >= ${THRESHOLD}%"
cargo llvm-cov --workspace --fail-under-lines "${THRESHOLD}" --summary-only
echo "==> Coverage gate PASSED (workspace total >= ${THRESHOLD}%)"
