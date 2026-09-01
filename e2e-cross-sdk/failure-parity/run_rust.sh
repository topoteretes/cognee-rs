#!/usr/bin/env bash
# Drive the Rust half of the matrix.
#
# One process for all cells: unlike cognee's Python engines, nothing here is
# process-global — every cell builds its own in-memory SQLite, its own mock
# stores and its own dataset. Cells run sequentially so the shared rayon pool
# is never a source of cross-cell interference.
#
# The output path is absolute because a cargo test's working directory is the
# crate root, not this one.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OUT="${1:-$HERE/observations_rust.jsonl}"
cd "$REPO"
COGNEE_FAILURE_PARITY_OUT="$OUT" \
  cargo test -p cognee-cognify --test failure_parity_probe -- --nocapture >"$HERE/.rust_probe.log" 2>&1
wc -l "$OUT" >&2
