#!/usr/bin/env bash
# check_header_sync.sh — verify that every #[no_mangle] extern "C" fn in the
# Rust source has a matching declaration in the public C headers.
#
# SCOPE: name-level only.  This script detects missing or renamed functions but
# NOT signature changes (wrong argument types, changed return types).  Full
# signature-level checking would require a cbindgen-based approach (out of scope).
#
# EXIT: 0 when fully in sync; non-zero when any export lacks a declaration.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"
SRC_DIR="$CAPI_DIR/cognee-capi/src"
INCLUDE_DIR="$CAPI_DIR/include"
ALLOWLIST="$SCRIPT_DIR/header_sync_allow.txt"

EXPORTS_TMP="$(mktemp)"
DECLARED_TMP="$(mktemp)"
trap 'rm -f "$EXPORTS_TMP" "$DECLARED_TMP"' EXIT

# ── Step 1: collect exported symbols from source ─────────────────────────────
# Match both plain and unsafe extern "C" fn, including digits in names
# (e.g. cg_value_from_i64 would be truncated by [a-z_]+ alone).
grep -rhoE 'pub (unsafe )?extern "C" fn [a-z0-9_]+' "$SRC_DIR" \
    | sed -E 's/.* ([a-z0-9_]+)$/\1/' \
    | sort -u \
    > "$EXPORTS_TMP"

# ── Step 2: collect declared symbols from both public headers ─────────────────
# The (cg|cognee)_ prefix alternation is required — several entry points use
# the cognee_ prefix (cognee_setup_logging, cognee_init_otlp, …).
grep -hoE '\b(cg|cognee)_[a-z0-9_]+[[:space:]]*\(' \
        "$INCLUDE_DIR/cognee.h" \
        "$INCLUDE_DIR/cognee_sdk.h" \
    | sed -E 's/[[:space:]]*\($//' \
    | sort -u \
    > "$DECLARED_TMP"

# ── Step 3: apply allowlist ───────────────────────────────────────────────────
# Each non-blank, non-comment line in the allowlist is removed from EXPORTS_TMP
# so it is not flagged as undeclared.
if [[ -f "$ALLOWLIST" ]]; then
    while IFS= read -r entry; do
        # Skip blank lines and comments.
        [[ -z "$entry" || "$entry" == \#* ]] && continue
        # `sed -i.bak` is portable across GNU and BSD/macOS sed (a bare `sed -i`
        # treats the next argument as the backup suffix on BSD, which breaks the
        # in-place edit). Remove the backup afterwards. Symbol names are
        # [a-z0-9_]+, so there are no regex metacharacters to escape.
        sed -i.bak "/^${entry}$/d" "$EXPORTS_TMP" && rm -f "$EXPORTS_TMP.bak"
    done < "$ALLOWLIST"
fi

# ── Step 4: diff ──────────────────────────────────────────────────────────────
# comm -23: lines in EXPORTS_TMP (sorted) that are absent from DECLARED_TMP.
MISSING="$(comm -23 "$EXPORTS_TMP" "$DECLARED_TMP")"

if [[ -n "$MISSING" ]]; then
    echo "================================================================" >&2
    echo "=== HEADER SYNC FAILURE: exported-but-undeclared symbols =======" >&2
    echo "================================================================" >&2
    echo "" >&2
    echo "The following symbols are exported by the Rust source but have" >&2
    echo "no declaration in capi/include/cognee.h or capi/include/cognee_sdk.h:" >&2
    echo "" >&2
    echo "$MISSING" | sed 's/^/  /' >&2
    echo "" >&2
    echo "Fix by either:" >&2
    echo "  a) Adding the declaration to the appropriate header, or" >&2
    echo "  b) Adding the symbol to capi/scripts/header_sync_allow.txt" >&2
    echo "     (with a comment explaining why it is intentionally internal)." >&2
    echo "" >&2
    exit 1
fi

echo "Header sync OK — all exported symbols are declared in public headers."
