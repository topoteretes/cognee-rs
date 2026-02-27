#!/usr/bin/env bash
#
# Run a deployed binary (CLI, example, or unit-test binary) on an Android
# device via adb.  Assumes artifacts were already pushed by
# android-build-and-deploy.sh --deploy.
#
# Prerequisites:
#   - Android SDK installed (ANDROID_SDK_ROOT set)
#   - adb available and device connected
#   - Artifacts deployed to the device (run android-build-and-deploy.sh --deploy first)
#
# Usage:
#   ./scripts/android-run.sh [flags] <binary> [-- <binary-args...>]
#
# Flags:
#   --list              List binaries available on the device and exit
#   --log LEVEL         Set RUST_LOG level (default: info)
#   --device-dir DIR    Override device directory (default: /data/local/tmp/cognee)
#   --forward-port PORT Set up 'adb reverse tcp:PORT tcp:PORT' before running,
#                       making device localhost:PORT reach host localhost:PORT.
#                       May be repeated for multiple ports.
#
# Examples:
#   ./scripts/android-run.sh --list
#   ./scripts/android-run.sh cognee -- --help
#   ./scripts/android-run.sh add_example
#   ./scripts/android-run.sh cognee-chunking-<hash> -- --nocapture
#   ./scripts/android-run.sh --log debug cognify_example

set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────────
DEVICE_DIR="/data/local/tmp/cognee"

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

# ── Parse args ─────────────────────────────────────────────────────────────────
RUST_LOG="info"
LIST=false
BINARY=""
BINARY_ARGS=()
FORWARD_PORTS=()
PASSTHROUGH=false

while [[ $# -gt 0 ]]; do
    if [[ "${PASSTHROUGH}" == "true" ]]; then
        BINARY_ARGS+=("$1")
        shift
        continue
    fi
    case "$1" in
        --list)
            LIST=true
            shift
            ;;
        --log)
            [[ $# -lt 2 ]] && { echo "ERROR: --log requires a value" >&2; exit 1; }
            RUST_LOG="$2"
            shift 2
            ;;
        --device-dir)
            [[ $# -lt 2 ]] && { echo "ERROR: --device-dir requires a value" >&2; exit 1; }
            DEVICE_DIR="$2"
            shift 2
            ;;
        --forward-port)
            [[ $# -lt 2 ]] && { echo "ERROR: --forward-port requires a value" >&2; exit 1; }
            FORWARD_PORTS+=("$2")
            shift 2
            ;;
        --)
            PASSTHROUGH=true
            shift
            ;;
        -*)
            echo "ERROR: Unknown flag: $1" >&2
            echo "Usage: $0 [--list] [--log LEVEL] [--device-dir DIR] [--forward-port PORT] <binary> [-- <args...>]" >&2
            exit 1
            ;;
        *)
            if [[ -z "${BINARY}" ]]; then
                BINARY="$1"
            else
                echo "ERROR: Unexpected argument: $1 (binary already set to '${BINARY}')" >&2
                echo "       Pass binary arguments after '--'" >&2
                exit 1
            fi
            shift
            ;;
    esac
done

# ── Validation ─────────────────────────────────────────────────────────────────
if [[ ! -x "${ADB}" ]]; then
    echo "ERROR: adb not found. Set ANDROID_SDK_ROOT or put adb on PATH." >&2
    exit 1
fi
if ! "${ADB}" devices | grep -q "device$"; then
    echo "ERROR: No Android device connected. Connect a device and enable USB debugging." >&2
    exit 1
fi

# ── List mode ─────────────────────────────────────────────────────────────────
if [[ "${LIST}" == "true" ]]; then
    echo "Binaries available on device at ${DEVICE_DIR}/bin/:"
    echo ""
    "${ADB}" shell "ls -1 ${DEVICE_DIR}/bin/ 2>/dev/null || echo '  (none — run android-build-and-deploy.sh --deploy first)'"
    exit 0
fi

# ── Require a binary name ─────────────────────────────────────────────────────
if [[ -z "${BINARY}" ]]; then
    echo "ERROR: No binary specified." >&2
    echo ""
    echo "Usage: $0 [--list] [--log LEVEL] <binary> [-- <args...>]" >&2
    echo ""
    echo "Run with --list to see available binaries on the device." >&2
    exit 1
fi

# ── Run ───────────────────────────────────────────────────────────────────────
DEVICE_BINARY="${DEVICE_DIR}/bin/${BINARY}"

# Build the args string — properly quote each argument for the remote shell
ARGS_STR=""
for arg in "${BINARY_ARGS[@]}"; do
    ARGS_STR="${ARGS_STR} $(printf '%q' "${arg}")"
done

echo "=== Android Run ==="
echo "  Binary:   ${BINARY}"
echo "  Args:     ${ARGS_STR:-<none>}"
echo "  RUST_LOG: ${RUST_LOG}"
echo "  Device:   ${DEVICE_DIR}"
if [[ ${#FORWARD_PORTS[@]} -gt 0 ]]; then
    echo "  Ports:    ${FORWARD_PORTS[*]}"
fi
echo ""

# ── Port forwarding ───────────────────────────────────────────────────────────
for port in "${FORWARD_PORTS[@]}"; do
    echo "  Forwarding: adb reverse tcp:${port} tcp:${port}"
    "${ADB}" reverse "tcp:${port}" "tcp:${port}"
done

"${ADB}" shell \
    "HOME=${DEVICE_DIR} \
     PATH=${DEVICE_DIR}/bin:\$PATH \
     LD_LIBRARY_PATH=${DEVICE_DIR}/lib \
     ORT_DYLIB_PATH=${DEVICE_DIR}/lib/libonnxruntime.so \
     RUST_LOG=${RUST_LOG} \
     ${DEVICE_BINARY}${ARGS_STR}"
