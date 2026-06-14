#!/usr/bin/env bash
# Source-level no-backend-leakage guard (complementary to `mailmate-arch-test`).
#
# The authoritative guard is the cargo-metadata test in `mailmate-arch-test`,
# which proves no forbidden backend reaches `mailmate-core` through ANY edge
# (direct, transitive, optional, or dev). This script is a fast, defence-in-depth
# source scan: it asserts the hexagon's inner crates never even *name* a backend
# via a `use` / `extern crate` import. A doc comment that mentions "Burn" or
# "Tokio" is fine; an actual import is not — so we match import statements only,
# which never appear inside doc comments.
set -euo pipefail
cd "$(dirname "$0")/../.."

# Forbidden backend / engine / runtime crates (kept in sync with
# mailmate-arch-test::FORBIDDEN_BACKENDS).
FORBIDDEN=(
  rusqlite libsqlite3_sys sqlx
  tokio mio reqwest hyper
  burn candle_core ort onnxruntime tract_core
  pyo3 numpy
)

# Inner-hexagon crates that must stay backend-free.
INNER=(crates/mailmate-core crates/mailmate-ports crates/mailmate-common)

# Build an alternation like (rusqlite|tokio|burn|...).
alt="$(IFS='|'; echo "${FORBIDDEN[*]}")"

# Match `use <backend>...` and `extern crate <backend>` only — import sites,
# not prose. `::`-suffixed and `;`/whitespace-suffixed forms both covered.
pattern="^[[:space:]]*(use|extern[[:space:]]+crate)[[:space:]]+(${alt})([[:space:]]|::|;|,)"

violations=0
for crate in "${INNER[@]}"; do
  if matches="$(grep -REn --include='*.rs' "${pattern}" "${crate}/src" 2>/dev/null)"; then
    echo "BACKEND IMPORT LEAK in ${crate}:"
    echo "${matches}"
    violations=1
  fi
done

if [[ "${violations}" -ne 0 ]]; then
  echo "==> FAILED: inner-hexagon crate imports a forbidden backend."
  exit 1
fi
echo "==> PASSED: no backend import in mailmate-{core,ports,common} source."
