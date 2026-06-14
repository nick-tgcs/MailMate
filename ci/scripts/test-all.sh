#!/usr/bin/env bash
# Full local validation — the same gates CI enforces, runnable in one command.
# Run this BEFORE and AFTER any change (the project's standing rule).
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "==> [1/7] rustfmt"
cargo fmt --all --check

echo "==> [2/7] clippy (warnings denied)"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> [3/7] build"
cargo build --workspace --all-targets

echo "==> [4/7] test (unit + integration + e2e/harness)"
cargo test --workspace

echo "==> [5/7] source no-backend-leakage guard"
ci/scripts/test-interface-no-backend-leakage.sh

echo "==> [6/7] dependency-hygiene guard"
ci/scripts/test-real-adapter-deps.sh

echo "==> [7/7] coverage gate (>=80%)"
ci/scripts/coverage-gate.sh

echo "==> ALL LOCAL GATES PASSED"
