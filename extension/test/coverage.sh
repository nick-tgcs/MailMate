#!/usr/bin/env bash
# coverage.sh — authoritative JS coverage for the extension.
#
# The UI/background scripts are classic (non-module) scripts loaded into jsdom/vm, so the test
# harness tags each eval'd source with a `//# sourceURL` (or vm `filename`) pointing at the real
# file. That lets c8 (V8 coverage) attribute line coverage to extension/*.js. We run from the
# extension dir so every source is in-tree (no --allowExternal quirks), and report all 12 sources.
set -euo pipefail
cd "$(dirname "$0")/.."   # -> extension/
exec ./test/node_modules/.bin/c8 \
  --reporter=text --reporter=text-summary --all \
  --include='*.js' \
  --exclude='test/**' --exclude='node_modules/**' \
  node --test test/*.test.mjs
