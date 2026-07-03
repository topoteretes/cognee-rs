#!/usr/bin/env bash
#
# Pre-seed ort-sys's ONNX Runtime download cache from the cognee-hosted mirror.
#
# `ort-sys` (see build/main.rs) links prebuilt ONNX Runtime from
#     <cache-dir>/dfbin/<target>/<sha256>/
# and skips its network download from cdn.pyke.io entirely when that directory
# already exists. Cloudflare (fronting the pyke CDN) intermittently 403s GitHub
# Actions runner IPs, so we mirror the binaries to a GitHub release we control
# and populate that directory before `cargo build`.
#
# This is a best-effort fast path: on ANY failure (no mirror entry, download or
# extraction error) we exit 0 and let the build fall back to ort-sys's normal
# pyke download — never worse than not running this script.
#
# Usage: prefetch.sh <rust-target> [cache-dir]
#   cache-dir defaults to $ORT_CACHE_DIR, then to ort-sys's own default
#   (~/.cache/ort.pyke.io). Must match the ORT_CACHE_DIR the build uses.
set -uo pipefail

TARGET="${1:?usage: prefetch.sh <rust-target> [cache-dir]}"
CACHE_DIR="${2:-${ORT_CACHE_DIR:-$HOME/.cache/ort.pyke.io}}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCK="$SCRIPT_DIR/runtime.lock"

skip() { echo "ort-prefetch: $1; build will download from pyke" >&2; exit 0; }

[ -f "$LOCK" ] || skip "lock file $LOCK not found"

mirror_tag="$(awk '/^VERSION/ {print $4}' "$LOCK")"
hash="$(awk -v t="$TARGET" '$1==t {print $2}' "$LOCK")"
[ -n "$mirror_tag" ] && [ -n "$hash" ] || skip "no mirror entry for target '$TARGET'"

dest="$CACHE_DIR/dfbin/$TARGET/$hash"
if [ -d "$dest" ] && [ -n "$(ls -A "$dest" 2>/dev/null)" ]; then
	echo "ort-prefetch: cache already warm at $dest; nothing to do"
	exit 0
fi

base="${ORT_MIRROR_BASE:-https://github.com/topoteretes/cognee-rs/releases/download/$mirror_tag}"
asset="ort-$TARGET.tar.gz"
url="$base/$asset"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "ort-prefetch: fetching $url"
curl -fsSL --retry 3 --retry-delay 5 -o "$tmp/$asset" "$url" || skip "mirror download failed"

# Extract into a temp dir first, then move into place, so a partially-written
# cache dir can never fool ort-sys into skipping the download.
staging="$tmp/x"
mkdir -p "$staging"
tar -xzf "$tmp/$asset" -C "$staging" || skip "extraction failed"
[ -f "$staging/libonnxruntime.a" ] || [ -f "$staging/onnxruntime.lib" ] \
	|| skip "mirror archive missing onnxruntime lib (unexpected layout)"

mkdir -p "$(dirname "$dest")"
rm -rf "$dest"
mv "$staging" "$dest"
echo "ort-prefetch: seeded $dest from mirror ($asset)"
