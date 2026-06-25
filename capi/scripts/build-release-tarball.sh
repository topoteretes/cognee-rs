#!/usr/bin/env bash
# build-release-tarball.sh — assemble the C-API release tarball.
#
# Usage: build-release-tarball.sh <tag> <cargo-target> <archive-suffix>
#   tag             — e.g. v0.1.0 (used in archive name)
#   cargo-target    — e.g. x86_64-unknown-linux-gnu (used to locate target/ output;
#                     pass empty string "" to use the host-default target/release/)
#   archive-suffix  — e.g. linux-x86_64 (used in archive name + windows-detection)
#
# Output:
#   dist/cognee-capi-<tag>-<archive-suffix>.tar.gz (or .zip on Windows)
#
# Used by:
#   .github/workflows/capi-release.yml — automated path
#   docs/RELEASE.md "Publish — C-library artifact" — manual path
set -euo pipefail

TAG="${1:?tag required (e.g. v0.1.0)}"
CARGO_TARGET="${2-}"
ARCH_SUFFIX="${3:?archive suffix required (e.g. linux-x86_64)}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$CAPI_DIR")"
cd "$REPO_ROOT"

NAME="cognee-capi-${TAG}-${ARCH_SUFFIX}"
DIST_DIR="${REPO_ROOT}/dist"
STAGE_DIR="${DIST_DIR}/${NAME}"

echo "── build-release-tarball.sh ────────────────────────────────────"
echo "  tag           = $TAG"
echo "  cargo target  = ${CARGO_TARGET:-(host default)}"
echo "  arch suffix   = $ARCH_SUFFIX"
echo "  staging       = $STAGE_DIR"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/lib" "$STAGE_DIR/include"

# Locate the built library files. Prefer the explicit --target dir when set;
# fall back to the host-default release dir for manual/dry-run builds.
if [ -n "$CARGO_TARGET" ] && [ -d "capi/target/${CARGO_TARGET}/release" ]; then
    BUILT_DIR="capi/target/${CARGO_TARGET}/release"
else
    BUILT_DIR="capi/target/release"
fi
echo "  build dir     = $BUILT_DIR"

if [ ! -d "$BUILT_DIR" ]; then
    echo "::error::Build directory does not exist: $BUILT_DIR" >&2
    echo "        Run \`cargo build --release --manifest-path capi/Cargo.toml\` first." >&2
    exit 1
fi

# Copy lib files: shared + static, across all platforms.
#   Linux:   libcognee_capi.{so, a}
#   macOS:   libcognee_capi.{dylib, a}
#   Windows: cognee_capi.dll + cognee_capi.dll.lib + cognee_capi.lib
shopt -s nullglob
COPIED_ANY=0
for lib in \
    "$BUILT_DIR"/libcognee_capi.so* \
    "$BUILT_DIR"/libcognee_capi.a \
    "$BUILT_DIR"/libcognee_capi.dylib* \
    "$BUILT_DIR"/cognee_capi.dll \
    "$BUILT_DIR"/cognee_capi.dll.lib \
    "$BUILT_DIR"/cognee_capi.lib \
    "$BUILT_DIR"/cognee_capi.pdb; do
    cp "$lib" "$STAGE_DIR/lib/"
    echo "    copied $(basename "$lib")"
    COPIED_ANY=1
done
shopt -u nullglob

if [ "$COPIED_ANY" -ne 1 ]; then
    echo "::error::No library files found in $BUILT_DIR — build may have failed." >&2
    ls -la "$BUILT_DIR" >&2 || true
    exit 1
fi

# Copy headers.
cp capi/include/cognee.h     "$STAGE_DIR/include/"
cp capi/include/cognee_sdk.h "$STAGE_DIR/include/"
echo "    copied headers (cognee.h, cognee_sdk.h)"

# Copy license + readme.
cp capi/LICENSE-MIT     "$STAGE_DIR/"
cp capi/LICENSE-APACHE  "$STAGE_DIR/"
cp capi/README.md       "$STAGE_DIR/"
echo "    copied LICENSE-MIT, LICENSE-APACHE, README.md"

# Sanity-check: at least one library file must be present.
if [ -z "$(ls -A "$STAGE_DIR/lib")" ]; then
    echo "::error::Staging lib/ directory is empty after copy." >&2
    exit 1
fi

# Package.
cd "$DIST_DIR"
case "$ARCH_SUFFIX" in
    windows-*)
        # Windows users typically prefer zip; fall back to tar.gz if zip is absent.
        if command -v zip >/dev/null 2>&1; then
            zip -r "${NAME}.zip" "$NAME" > /dev/null
            echo "  archive       = ${DIST_DIR}/${NAME}.zip"
        else
            tar -czf "${NAME}.tar.gz" "$NAME"
            echo "  archive       = ${DIST_DIR}/${NAME}.tar.gz"
        fi
        ;;
    *)
        tar -czf "${NAME}.tar.gz" "$NAME"
        echo "  archive       = ${DIST_DIR}/${NAME}.tar.gz"
        ;;
esac

# Clean up the staging directory — only ship the archive.
rm -rf "$STAGE_DIR"

echo "── dist/ contents ──────────────────────────────────────────────"
ls -la "$DIST_DIR"
echo "── done ────────────────────────────────────────────────────────"
