#!/usr/bin/env bash
#
# Drop any ort-sys "downloaded binaries" directory that holds no files.
#
# WHY THIS EXISTS
#
# `ort-sys` (build/main.rs) decides it has already downloaded ONNX Runtime by
# testing whether `<ort-cache>/dfbin/<target>/<sha256>/` EXISTS — it never looks
# inside. When that test passes it emits
#
#     cargo:rustc-link-search=native=<that dir>
#     cargo:rustc-link-lib=static=onnxruntime
#
# and returns successfully. So an *empty* dist dir does not fail the build
# script and does not trip ort-sys's own `link_error_*` diagnostics; it fails
# the LINK, many minutes later, as the bare rustc message
#
#     error: could not find native static library `onnxruntime`
#
# Empty dist dirs are not hypothetical. We deliberately point ORT_CACHE_DIR
# inside the cargo target dir (see the ORT_CACHE_DIR note in ci.yml) so the
# download rides along in the rust-cache tarball. But Swatinem/rust-cache's
# `cleanTargetDir()` walks every unrecognised directory under target/ and
# deletes each *file* it meets while keeping the directory tree — so the
# target-dir cache faithfully preserves the dfbin skeleton and drops
# libonnxruntime.a out of it. Restore that tarball into a job whose separate
# `Cache ORT binary` entry happens to MISS, and the actions/cache post-step
# persists the empty skeleton under the ORT key — permanently, because
# actions/cache never overwrites an existing key+version.
#
# That is exactly how the HTTP Parity lane broke: its ORT entry
# (`ort-v2.0.0-rc.12-cpu-linux-x86_64` for path `cognee-rust/target/ort-cache`)
# was a 464-byte directory skeleton, seeded on 2026-08-31 by a main run that had
# a warm rust-cache. It stayed harmless for a day because every run also
# restored a warm target dir, so ort-sys was never rebuilt — then the 1.3 GB
# rust-cache entry was evicted under the repo's 10 GB quota, the next run built
# ort-sys cold, and the link failed.
#
# Running this immediately after the ORT restore closes both halves of the trap:
# an empty dist dir can no longer break the link (ort-sys re-downloads into it),
# and it can no longer be *saved* (it is gone before the post-step looks).
#
# This is not a retry and it does not disable any cache: it is a one-shot,
# millisecond-scale integrity check that converts an unusable restored artifact
# into a clean cache miss.
#
# Usage: ensure-cache-usable.sh [cache-dir]
#   cache-dir defaults to $ORT_CACHE_DIR.
set -euo pipefail

dir="${1:-${ORT_CACHE_DIR:-}}"
if [ -z "$dir" ]; then
	echo "::error::ensure-cache-usable.sh: no cache dir argument and ORT_CACHE_DIR is unset" >&2
	exit 1
fi

if [ ! -d "$dir" ]; then
	echo "ort-cache-guard: '$dir' does not exist — ort-sys will download."
	exit 0
fi

removed=0
kept=0
if [ -d "$dir/dfbin" ]; then
	# Layout is `dfbin/<rust-target>/<sha256>/` with the libraries flat inside,
	# so the dist dirs are exactly the depth-2 directories under dfbin.
	while IFS= read -r dist; do
		if [ -z "$(find "$dist" -type f -print -quit)" ]; then
			echo "ort-cache-guard: removing empty dist dir '$dist'"
			rm -rf "$dist"
			removed=$((removed + 1))
		else
			echo "ort-cache-guard: keeping '$dist' ($(find "$dist" -type f | wc -l) file(s))"
			kept=$((kept + 1))
		fi
	done < <(find "$dir/dfbin" -mindepth 2 -maxdepth 2 -type d)
fi

# Nothing usable anywhere: drop the whole tree, so that if this job's ORT cache
# key missed, the post-step has nothing to persist under it.
if [ -z "$(find "$dir" -type f -print -quit)" ]; then
	echo "ort-cache-guard: '$dir' holds no files at all — removing it."
	rm -rf "$dir"
fi

echo "ort-cache-guard: done (kept $kept, removed $removed empty dist dir(s))"
