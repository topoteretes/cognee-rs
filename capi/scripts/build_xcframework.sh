#!/usr/bin/env bash
# capi/scripts/build_xcframework.sh
#
# Builds CogneeSDK.xcframework from cognee-capi for iOS device + simulator.
# Run from the repo root:
#   ./capi/scripts/build_xcframework.sh
#
# Output: capi/CogneeSDK.xcframework

set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CAPI_DIR="$REPO_ROOT/capi"
MANIFEST="$CAPI_DIR/cognee-capi/Cargo.toml"
INCLUDE_DIR="$CAPI_DIR/include"
OUTPUT="$CAPI_DIR/CogneeSDK.xcframework"
STAGING="$CAPI_DIR/xcframework-staging"

CARGO_FLAGS="--manifest-path $MANIFEST --no-default-features --features sqlite,testing,mock-llm --release"

# ---------------------------------------------------------------------------
# 1. Compile release static libraries for both iOS targets
#    `cargo rustc --crate-type staticlib` overrides the crate-type for this
#    invocation only, so we emit only the staticlib slice (no multi-GB cdylib)
#    regardless of the [lib] crate-type list in Cargo.toml.
# ---------------------------------------------------------------------------
echo "==> Building for aarch64-apple-ios (device) ..."
export SDKROOT="$(xcrun --sdk iphoneos --show-sdk-path)"
cargo rustc --crate-type staticlib --target aarch64-apple-ios $CARGO_FLAGS

echo "==> Building for aarch64-apple-ios-sim (simulator) ..."
export SDKROOT="$(xcrun --sdk iphonesimulator --show-sdk-path)"
cargo rustc --crate-type staticlib --target aarch64-apple-ios-sim $CARGO_FLAGS

# ---------------------------------------------------------------------------
# 2. Stage headers + module map
#    xcodebuild -create-xcframework requires a separate headers directory
#    per library slice (even when the headers are identical).
# ---------------------------------------------------------------------------
for SLICE in ios ios-sim; do
  mkdir -p "$STAGING/$SLICE/Headers"
  cp "$INCLUDE_DIR/cognee.h"          "$STAGING/$SLICE/Headers/"
  cp "$INCLUDE_DIR/cognee_sdk.h"      "$STAGING/$SLICE/Headers/"
  # Use the committed module.modulemap so CI type-checks and the xcframework
  # headers are always in sync (single source of truth).
  cp "$INCLUDE_DIR/module.modulemap"  "$STAGING/$SLICE/Headers/"
done

# ---------------------------------------------------------------------------
# 3. Assemble the xcframework
# ---------------------------------------------------------------------------
DEVICE_LIB="$CAPI_DIR/target/aarch64-apple-ios/release/libcognee_capi.a"
SIM_LIB="$CAPI_DIR/target/aarch64-apple-ios-sim/release/libcognee_capi.a"

echo "==> Assembling XCFramework ..."
rm -rf "$OUTPUT"
xcodebuild -create-xcframework \
  -library "$DEVICE_LIB" -headers "$STAGING/ios/Headers" \
  -library "$SIM_LIB"    -headers "$STAGING/ios-sim/Headers" \
  -output  "$OUTPUT"

rm -rf "$STAGING"

echo ""
echo "==> Done: $OUTPUT"
ls "$OUTPUT"
