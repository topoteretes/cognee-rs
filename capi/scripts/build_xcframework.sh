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
# ---------------------------------------------------------------------------
echo "==> Building for aarch64-apple-ios (device) ..."
cargo build --target aarch64-apple-ios $CARGO_FLAGS

echo "==> Building for aarch64-apple-ios-sim (simulator) ..."
cargo build --target aarch64-apple-ios-sim $CARGO_FLAGS

# ---------------------------------------------------------------------------
# 2. Stage headers + module map
#    xcodebuild -create-xcframework requires a separate headers directory
#    per library slice (even when the headers are identical).
# ---------------------------------------------------------------------------
for SLICE in ios ios-sim; do
  mkdir -p "$STAGING/$SLICE/Headers"
  cp "$INCLUDE_DIR/cognee.h"     "$STAGING/$SLICE/Headers/"
  cp "$INCLUDE_DIR/cognee_sdk.h" "$STAGING/$SLICE/Headers/"
  # module.modulemap lets Swift import this as `import CogneeSDK`
  cat > "$STAGING/$SLICE/Headers/module.modulemap" <<'MODULE'
module CogneeSDKCore {
    umbrella header "cognee_sdk.h"
    export *
}
MODULE
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
