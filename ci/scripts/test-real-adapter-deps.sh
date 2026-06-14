#!/usr/bin/env bash
# Dependency-hygiene gate.
#
#   (1) Exact-pin discipline: every third-party crate declared in the workspace
#       dependency tables must be pinned to an exact version (`= x.y.z`), mirroring
#       the sibling project idiolect's supply-chain stance. A floating `^`/`>=`/`~`
#       or bare requirement is a violation. (Member crates inherit via
#       `<dep>.workspace = true`, so all third-party version strings live in the
#       root manifest's [workspace.dependencies*] tables.)
#   (2) Manifest-level backend quarantine: the inner-hexagon crates
#       (mailmate-core / -ports / -common) must not DECLARE a forbidden backend.
set -euo pipefail
cd "$(dirname "$0")/../.."

FORBIDDEN=(
  rusqlite libsqlite3-sys sqlx
  tokio mio reqwest hyper
  burn candle-core ort onnxruntime tract-core
  pyo3 numpy
)

fail=0

# (1) Exact-pin enforcement, scoped to the workspace dependency tables.
section=""
while IFS= read -r line; do
  if [[ "${line}" =~ ^\[([^]]+)\][[:space:]]*$ ]]; then
    section="${BASH_REMATCH[1]}"
    continue
  fi
  case "${section}" in
    workspace.dependencies | workspace.build-dependencies | workspace.dev-dependencies) ;;
    *) continue ;;
  esac
  [[ -z "${line//[[:space:]]/}" ]] && continue
  [[ "${line}" =~ ^[[:space:]]*# ]] && continue

  ver=""
  if [[ "${line}" =~ version[[:space:]]*=[[:space:]]*\"([^\"]*)\" ]]; then
    ver="${BASH_REMATCH[1]}"
  elif [[ "${line}" =~ ^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*\"([^\"]*)\" ]]; then
    ver="${BASH_REMATCH[1]}"
  fi
  if [[ -n "${ver}" && "${ver}" != "="* ]]; then
    echo "UNPINNED dependency in [${section}]: ${line## }"
    fail=1
  fi
done < "Cargo.toml"

# (2) Manifest-level backend quarantine for inner-hexagon crates.
for crate in mailmate-core mailmate-ports mailmate-common; do
  manifest="crates/${crate}/Cargo.toml"
  [[ -f "${manifest}" ]] || continue
  for backend in "${FORBIDDEN[@]}"; do
    # A dependency key is `<name> = ...` or `<name>.workspace = ...` at line start.
    if grep -Eq "^[[:space:]]*${backend}([[:space:]]*=|\.workspace)" "${manifest}"; then
      echo "FORBIDDEN backend '${backend}' declared in ${manifest}"
      fail=1
    fi
  done
done

if [[ "${fail}" -ne 0 ]]; then
  echo "==> FAILED: dependency-hygiene violations above."
  exit 1
fi
echo "==> PASSED: all workspace deps exact-pinned; no backend in inner-hexagon manifests."
