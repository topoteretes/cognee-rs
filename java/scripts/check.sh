#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAVA_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$JAVA_DIR")"

cd "$JAVA_DIR"

echo "================================================================"
echo "=== Java: Checking version parity with Cargo workspace ==="
echo "================================================================"
WS_VERSION=$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
POM_VERSION=$(sed -n 's:.*<version>\(.*\)</version>.*:\1:p' "$JAVA_DIR/pom.xml" | head -1)
if [ "$WS_VERSION" != "$POM_VERSION" ]; then
  echo "error: version drift — workspace Cargo.toml=${WS_VERSION}, java/pom.xml=${POM_VERSION}" >&2
  exit 1
fi
echo "version ok (${POM_VERSION})"

# ── Graceful no-op when no JDK/Maven toolchain is present ────────────
if ! command -v mvn >/dev/null 2>&1 || ! command -v java >/dev/null 2>&1; then
  echo ""
  echo "SKIP: 'mvn' or 'java' not found — skipping Java binding check."
  echo "      (CI installs a JDK via actions/setup-java; local devs without a"
  echo "       JDK are not blocked. Install a JDK 11+ and Maven to run it.)"
  exit 0
fi

echo ""
echo "================================================================"
echo "=== Java: cargo fmt / clippy (shim crate) ==="
echo "================================================================"
cargo fmt --manifest-path "$JAVA_DIR/cognee-java-jni/Cargo.toml" -- --check
cargo clippy --manifest-path "$JAVA_DIR/cognee-java-jni/Cargo.toml" --all-targets -- -D warnings

echo ""
echo "================================================================"
echo "=== Java: Building native cdylib (debug) ==="
echo "================================================================"
cargo build --manifest-path "$JAVA_DIR/cognee-java-jni/Cargo.toml"

# Resolve the built library path across platforms.
LIBDIR="$JAVA_DIR/cognee-java-jni/target/debug"
for cand in \
  "$LIBDIR/libcognee_java.so" \
  "$LIBDIR/libcognee_java.dylib" \
  "$LIBDIR/cognee_java.dll"; do
  if [ -f "$cand" ]; then
    COGNEE_JAVA_LIB_PATH="$cand"
    break
  fi
done
if [ -z "${COGNEE_JAVA_LIB_PATH:-}" ]; then
  echo "error: could not find built cdylib in $LIBDIR" >&2
  exit 1
fi
export COGNEE_JAVA_LIB_PATH
echo "using COGNEE_JAVA_LIB_PATH=$COGNEE_JAVA_LIB_PATH"

echo ""
echo "================================================================"
echo "=== Java: mvn verify (compile, test, package) ==="
echo "================================================================"
mvn -q -f "$JAVA_DIR/pom.xml" verify

echo ""
echo "================================================================"
echo "=== Java check passed ==="
echo "================================================================"
