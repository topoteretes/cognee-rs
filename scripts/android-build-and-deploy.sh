#!/usr/bin/env bash
#
# Build the entire cognee-rust workspace for Android (aarch64, e.g. Pixel 6)
# and deploy all resulting binaries, shared libraries, and model files to the
# device via adb.
#
# Prerequisites:
#   - Android NDK installed (ANDROID_NDK_HOME set)
#   - Android SDK installed (ANDROID_SDK_ROOT set)
#   - Rust target: rustup target add aarch64-linux-android
#   - adb available and device connected
#   - NDK toolchain bin on PATH (for the linker)
#
# Usage:
#   ./scripts/android-build-and-deploy.sh [--debug]

set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────────
TARGET="aarch64-linux-android"
DEVICE_DIR="/data/local/tmp/cognee"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Android SDK / NDK paths
ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-${NDK_HOME:-}}"
ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"

# Find adb: try ANDROID_SDK_ROOT first, then common subdirectory layouts, then PATH
if [[ -x "${ANDROID_SDK_ROOT}/platform-tools/adb" ]]; then
    ADB="${ANDROID_SDK_ROOT}/platform-tools/adb"
elif [[ -x "${ANDROID_SDK_ROOT}/Sdk/platform-tools/adb" ]]; then
    ADB="${ANDROID_SDK_ROOT}/Sdk/platform-tools/adb"
elif command -v adb &>/dev/null; then
    ADB="$(command -v adb)"
else
    ADB="${ANDROID_SDK_ROOT}/platform-tools/adb"  # will fail at validation with a clear message
fi

NDK_TOOLCHAIN="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64"
ANDROID_API_LEVEL="${ANDROID_API_LEVEL:-27}"

# Parse args
PROFILE="release"
CARGO_PROFILE_FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE="debug"
    CARGO_PROFILE_FLAG=""
fi

TARGET_DIR="${PROJECT_DIR}/target"
BINARY_DIR="${TARGET_DIR}/${TARGET}/${PROFILE}"
MODEL_DIR="${TARGET_DIR}/models"

# ── Validation ─────────────────────────────────────────────────────────────────
echo "=== Android Build & Deploy: workspace ==="
echo "  Target:   ${TARGET}"
echo "  Profile:  ${PROFILE}"
echo "  NDK:      ${ANDROID_NDK_HOME}"
echo "  SDK:      ${ANDROID_SDK_ROOT}"
echo ""

if [[ -z "${ANDROID_NDK_HOME}" ]]; then
    echo "ERROR: ANDROID_NDK_HOME (or NDK_HOME) is not set." >&2
    exit 1
fi
if [[ -z "${ANDROID_SDK_ROOT}" ]]; then
    echo "ERROR: ANDROID_SDK_ROOT (or ANDROID_HOME) is not set." >&2
    exit 1
fi
if [[ ! -x "${ADB}" ]]; then
    echo "ERROR: adb not found at ${ADB}" >&2
    exit 1
fi
if ! "${ADB}" devices | grep -q "device$"; then
    echo "ERROR: No Android device connected. Connect a device and enable USB debugging." >&2
    exit 1
fi
if [[ ! -d "${NDK_TOOLCHAIN}/bin" ]]; then
    echo "ERROR: NDK toolchain not found at ${NDK_TOOLCHAIN}" >&2
    exit 1
fi

# Ensure the Rust target is installed
if ! rustup target list --installed | grep -q "${TARGET}"; then
    echo "Installing Rust target ${TARGET}..."
    rustup target add "${TARGET}"
fi

# ── Put NDK toolchain on PATH ─────────────────────────────────────────────────
export PATH="${NDK_TOOLCHAIN}/bin:${PATH}"
export ANDROID_NDK_HOME
export ANDROID_SDK_ROOT
export ANDROID_API_LEVEL

# Tell CMake where the NDK lives.  cmake 3.28's Android-Determine.cmake reads
# $ENV{ANDROID_NDK} to locate the NDK when CMAKE_SYSTEM_NAME=Android is set
# (which is what the `cmake` crate sets for aarch64-linux-android targets).
# Without this, the build fails with:
#   "Android: Neither the NDK or a standalone toolchain was found."
#
# Do NOT set CMAKE_TOOLCHAIN_FILE here.  The NDK's android.toolchain.cmake
# defaults to android-legacy.toolchain.cmake which resets CMAKE_C_COMPILER
# based on ANDROID_ABI (defaulting to armeabi-v7a when unset).  That overrides
# the aarch64 cross-compiler injected by .cargo/config.toml, producing 32-bit
# ARM objects that the aarch64 linker rejects with "incompatible" errors.
export ANDROID_NDK="${ANDROID_NDK_HOME}"

# Wipe any cached C/C++ library build outputs that were compiled for the wrong
# architecture.  This can happen when cmake previously ran with a toolchain
# file that defaulted to a wrong ABI (e.g. armeabi-v7a).  Because lbug's
# build.rs does not declare ANDROID_NDK in cargo:rerun-if-env-changed, cargo
# caches the cmake-built static libraries and will reuse wrong-arch objects
# on subsequent builds.  Deleting the build directory forces cargo to re-run
# the build script so cmake produces fresh aarch64 objects.
echo ">>> Purging cached C/C++ build outputs for ${TARGET}/${PROFILE}..."
rm -rf "${TARGET_DIR}/${TARGET}/${PROFILE}/build/lbug-"*

# ── Step 1: Build ──────────────────────────────────────────────────────────────
echo ">>> Step 1: Building workspace for ${TARGET} (${PROFILE})..."
echo ""

cargo build \
    --workspace \
    --target "${TARGET}" \
    --features onnx_dynamic_library \
    ${CARGO_PROFILE_FLAG}

echo ""

# ── Step 2: Collect binaries and shared libraries ───────────────────────────────
echo ">>> Step 2: Collecting binaries and shared libraries..."

STAGING_DIR="${TARGET_DIR}/${TARGET}/${PROFILE}/android-deploy"
rm -rf "${STAGING_DIR}"
mkdir -p "${STAGING_DIR}/bin" "${STAGING_DIR}/lib" "${STAGING_DIR}/models"

# Discover all ELF executables directly under BINARY_DIR (not in subdirectories)
# These are the top-level binary targets (e.g. cognee CLI).
BINARIES=()
while IFS= read -r -d '' bin; do
    if file "${bin}" 2>/dev/null | grep -q "ELF.*executable"; then
        BINARIES+=("${bin}")
    fi
done < <(find "${BINARY_DIR}" -maxdepth 1 -type f -not -name "*.so" -not -name "*.d" -not -name "*.rlib" -not -name "*.rmeta" -print0)

# Also collect example binaries
EXAMPLE_BINARIES=()
if [[ -d "${BINARY_DIR}/examples" ]]; then
    while IFS= read -r -d '' bin; do
        if file "${bin}" 2>/dev/null | grep -q "ELF.*executable"; then
            EXAMPLE_BINARIES+=("${bin}")
        fi
    done < <(find "${BINARY_DIR}/examples" -maxdepth 1 -type f -not -name "*.d" -print0)
fi

if [[ ${#BINARIES[@]} -eq 0 && ${#EXAMPLE_BINARIES[@]} -eq 0 ]]; then
    echo "WARNING: No ELF executables found in ${BINARY_DIR}" >&2
else
    for bin in "${BINARIES[@]}"; do
        name="$(basename "${bin}")"
        cp "${bin}" "${STAGING_DIR}/bin/"
        echo "  Binary: ${name}  ($(du -h "${bin}" | cut -f1))"
    done
    for bin in "${EXAMPLE_BINARIES[@]}"; do
        name="$(basename "${bin}")"
        cp "${bin}" "${STAGING_DIR}/bin/"
        echo "  Example: ${name}  ($(du -h "${bin}" | cut -f1))"
    done
fi

# Copy libonnxruntime.so (built by build.rs or pre-existing)
ORT_LIB="${BINARY_DIR}/libonnxruntime.so"
if [[ ! -f "${ORT_LIB}" ]]; then
    ORT_LIB=$(find "${TARGET_DIR}/${TARGET}" -name "libonnxruntime.so" -type f 2>/dev/null | head -1)
fi
if [[ -n "${ORT_LIB}" && -f "${ORT_LIB}" ]]; then
    cp "${ORT_LIB}" "${STAGING_DIR}/lib/"
    echo "  Found libonnxruntime.so: $(du -h "${ORT_LIB}" | cut -f1)"
else
    echo "WARNING: libonnxruntime.so not found. Binaries that use ONNX may fail at runtime." >&2
fi

# Copy NDK libc++_shared.so (required for C++ dependencies)
LIBCXX="${NDK_TOOLCHAIN}/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so"
if [[ -f "${LIBCXX}" ]]; then
    cp "${LIBCXX}" "${STAGING_DIR}/lib/"
    echo "  Found libc++_shared.so"
else
    echo "WARNING: libc++_shared.so not found at ${LIBCXX}" >&2
fi

# Scan all collected binaries for additional NDK shared library dependencies
echo "  Checking for additional NDK shared library dependencies..."
ALL_BINS=("${BINARIES[@]}" "${EXAMPLE_BINARIES[@]}")
for bin in "${ALL_BINS[@]}"; do
    NEEDED_LIBS=$(readelf -d "${bin}" 2>/dev/null | grep NEEDED | awk -F'[][]' '{print $2}' || true)
    for lib in ${NEEDED_LIBS}; do
        case "${lib}" in
            libc.so|libm.so|libdl.so|liblog.so|libz.so|libstdc++.so|libandroid.so) continue ;;
        esac
        if [[ -f "${STAGING_DIR}/lib/${lib}" ]]; then
            continue
        fi
        FOUND=$(find "${NDK_TOOLCHAIN}/sysroot/usr/lib/aarch64-linux-android" -name "${lib}" -type f 2>/dev/null | head -1)
        if [[ -n "${FOUND}" ]]; then
            cp "${FOUND}" "${STAGING_DIR}/lib/"
            echo "  Found ${lib}"
        fi
    done
done

# ── Step 3: Copy model files ──────────────────────────────────────────────────
echo ""
echo ">>> Step 3: Collecting model files..."

MODEL_FILES=(
    "qwen3-0.6b-q4.onnx"
    "qwen3-tokenizer.json"
    "BGE-Small-v1.5-model_quantized.onnx"
    "bge-small-tokenizer.json"
)

MODELS_FOUND=0
for f in "${MODEL_FILES[@]}"; do
    src="${MODEL_DIR}/${f}"
    if [[ -f "${src}" ]]; then
        cp "${src}" "${STAGING_DIR}/models/"
        echo "  ${f}: $(du -h "${src}" | cut -f1)"
        MODELS_FOUND=$((MODELS_FOUND + 1))
    else
        echo "  (skipping ${f} — not present in ${MODEL_DIR})"
    fi
done
if [[ "${MODELS_FOUND}" -eq 0 ]]; then
    echo "  WARNING: No model files found. Run 'cargo build' first to download models." >&2
fi

# ── Step 4: Push to device ─────────────────────────────────────────────────────
echo ""
echo ">>> Step 4: Pushing files to device at ${DEVICE_DIR}..."

"${ADB}" shell "mkdir -p ${DEVICE_DIR}/bin ${DEVICE_DIR}/lib ${DEVICE_DIR}/models"

# Push binaries
if ls "${STAGING_DIR}/bin/"* 1>/dev/null 2>&1; then
    "${ADB}" push "${STAGING_DIR}/bin/." "${DEVICE_DIR}/bin/"
    # Make all binaries executable
    "${ADB}" shell "chmod 755 ${DEVICE_DIR}/bin/*"
fi

# Push shared libraries
if ls "${STAGING_DIR}/lib/"*.so 1>/dev/null 2>&1; then
    "${ADB}" push "${STAGING_DIR}/lib/." "${DEVICE_DIR}/lib/"
fi

# Push model files
if ls "${STAGING_DIR}/models/"* 1>/dev/null 2>&1; then
    "${ADB}" push "${STAGING_DIR}/models/." "${DEVICE_DIR}/models/"
fi

# ── Step 5: Print run instructions ─────────────────────────────────────────────
echo ""
echo "=== Deploy complete! ==="
echo ""
echo "To run on device:"
echo ""
echo "  ${ADB} shell"
echo "  export PATH=${DEVICE_DIR}/bin:\$PATH"
echo "  export LD_LIBRARY_PATH=${DEVICE_DIR}/lib"
echo "  export ORT_DYLIB_PATH=${DEVICE_DIR}/lib/libonnxruntime.so"
echo "  export RUST_LOG=debug"
echo "  cognee --help"
echo ""
echo "Or as a one-liner (replace <binary> with the target name):"
echo ""
echo "  ${ADB} shell \"cd ${DEVICE_DIR} && PATH=${DEVICE_DIR}/bin:\\\$PATH LD_LIBRARY_PATH=${DEVICE_DIR}/lib ORT_DYLIB_PATH=${DEVICE_DIR}/lib/libonnxruntime.so RUST_LOG=debug bin/<binary>\""
echo ""
