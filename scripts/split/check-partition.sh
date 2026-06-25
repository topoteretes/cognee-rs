#!/usr/bin/env bash
# Plan §5 Phase 0 step 9 + §8 Step 0 partition-manifest gate.
# Asserts:
#   (oss ∪ closed) == git ls-files
#   (oss ∩ closed) == ∅
# Comment lines (^#) and blank lines in the manifest files are skipped.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
OSS="${ROOT}/scripts/split/oss-paths.txt"
CLOSED="${ROOT}/scripts/split/closed-paths.txt"

LSF=$(mktemp)
UNION=$(mktemp)
INTER=$(mktemp)
SORTED_OSS=$(mktemp)
SORTED_CLOSED=$(mktemp)
trap 'rm -f "$LSF" "$UNION" "$INTER" "$SORTED_OSS" "$SORTED_CLOSED"' EXIT

git ls-files | sort -u > "$LSF"
# `grep -v ...` exits 1 when ALL lines match the inverted pattern (i.e. the
# manifest is comment-only). That is a valid state for `closed-paths.txt`,
# so we tolerate rc=1 explicitly. `|| true` keeps `set -e` / `pipefail`
# from killing the script on a benign empty-result grep.
(grep -v -E '^\s*(#|$)' "$OSS"    || true) | sort -u > "$SORTED_OSS"
(grep -v -E '^\s*(#|$)' "$CLOSED" || true) | sort -u > "$SORTED_CLOSED"
sort -u "$SORTED_OSS" "$SORTED_CLOSED" > "$UNION"

# 1. union == git ls-files
if ! diff -u "$LSF" "$UNION" > /tmp/partition.diff; then
  echo "::error::partition manifest does not match git ls-files"
  echo "--- diff (git ls-files vs oss ∪ closed) ---"
  cat /tmp/partition.diff
  echo ""
  echo "Lines starting with '-' are tracked files missing from the manifest."
  echo "Lines starting with '+' are manifest entries that aren't tracked anymore."
  exit 1
fi

# 2. intersection == empty
comm -12 "$SORTED_OSS" "$SORTED_CLOSED" > "$INTER"
if [ -s "$INTER" ]; then
  echo "::error::oss-paths.txt and closed-paths.txt overlap:"
  cat "$INTER"
  exit 1
fi

echo "partition manifest OK ($(wc -l < "$SORTED_OSS") OSS / $(wc -l < "$SORTED_CLOSED") closed / $(wc -l < "$LSF") tracked)"
