# T02 — `java/scripts/check.sh` + wire into `check_all.sh` + `ci.yml` `java-check` job

## Objective

After this task the Java binding is part of the repo's check pipeline:
`java/scripts/check.sh` builds the cdylib and runs `mvn verify` against it
(gracefully no-op'ing when no JDK/Maven is installed), `scripts/check_all.sh`
invokes it as a new stage after the TS check, and `.github/workflows/ci.yml`
gains a `java-check` job (gated like the other binding jobs and wired into the
`notify` aggregator).

## Dependencies & preconditions

- **T01 done.** Verify:
  - `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` succeeds and
    produces `java/cognee-java-jni/target/debug/libcognee_java.so`.
  - `test -f java/pom.xml && test -f java/src/test/java/ai/cognee/internal/NativeLoadTest.java`.
- Read `ts/scripts/check.sh` (the closest model: version-parity check, toolchain
  check, build, test, credential-gated example, clear section banners).
- Read `scripts/check_all.sh` (stages: fmt → check → clippy → telemetry check →
  no-default check → wasm → telemetry noop test → capi → python → ts). The Java
  stage goes **after** the TS stage, before the final "All checks passed".
- Read `.github/workflows/ci.yml`: the binding jobs `capi-check`,
  `python-check`, `ts-check` all `needs: lint`, set up their toolchain, and the
  `notify` job lists every job in `needs:` and in its status `if`.

## Context for this task

**Graceful no-op (design §7).** `java/scripts/check.sh` must exit 0 with a clear
`SKIP` message when `java`/`mvn` is absent, so a developer without a JDK is not
blocked; CI is the enforcing environment (it installs Temurin 17). This mirrors
how the TS/capi example steps skip on missing credentials.

**Version parity.** Like `ts/scripts/check.sh`, fail if `java/pom.xml`'s
`<version>` has drifted from the root `Cargo.toml` workspace version.

**The cdylib feeds the tests via `COGNEE_JAVA_LIB_PATH`.** `check.sh` builds the
debug cdylib, then runs `mvn` with that env var pointing at it, so the tests use
the freshly built library (the classifier-jar path is not exercised until T13).

## Steps

### 1. Create `java/scripts/check.sh`

```bash
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
  echo "       JDK are not blocked. Install a JDK 17+ and Maven to run it.)"
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
```

Make it executable: `chmod +x java/scripts/check.sh`.

### 2. Wire into `scripts/check_all.sh`

Insert a Java stage **after** the existing TS stage (the block that runs
`"$REPO_ROOT/ts/scripts/check.sh"`) and **before** the final `=== All checks
passed! ===` banner:

```bash
echo ""
echo "================================================================"
echo "=== Java: Building bindings and running tests ==="
echo "================================================================"
"$REPO_ROOT/java/scripts/check.sh"
```

### 3. Add the `java-check` job to `.github/workflows/ci.yml`

Add a job modeled on `ts-check` (which `needs: lint`). Insert it in the
"Stage 3: Language binding checks" section, after `ts-check`:

```yaml
  java-check:
    name: Java Bindings Check
    needs: lint
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: rui314/setup-mold@v1
        with:
          make-default: true
      # cmake is required by the lbug crate (via cognee-lib's ladybug feature).
      - run: sudo apt-get update && sudo apt-get install -y cmake protobuf-compiler
      - name: Set up JDK (Temurin 17)
        uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: "17"
          cache: maven
      - name: ccache (lbug bundled C++, restore-only)
        uses: hendrikmuhs/ccache-action@v1.2
        with:
          key: lbug-cxx
          max-size: 1G
          save: false
      # Dedicated cache key for the standalone JNI workspace (mirrors ts-check).
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: workspace-java-v1
          workspaces: |
            . -> target
            java/cognee-java-jni -> target
          cache-on-failure: true
      - uses: actions/cache@v4
        with:
          path: target/ort-cache
          key: ort-v2.0.0-rc.12-cpu-linux-x86_64
      - run: bash java/scripts/check.sh
```

> Temurin 17 on CI matches the source floor of 17 (the public API uses `record`
> types, which require Java 16+); `maven.compiler.release=17` guarantees
> 17-compatible bytecode.

### 4. Wire `java-check` into the `notify` aggregator

In the `notify` job at the end of `ci.yml`:

- Add `java-check` to its `needs:` list.
- Add a `"${{ needs.java-check.result }}" == "success"` clause to the status
  `if [[ ... ]]` conditional.

## Verification

1. `bash java/scripts/check.sh` → runs the full Java check and prints
   `=== Java check passed ===` (with a JDK present); or prints the `SKIP`
   message and exits 0 (JDK absent — verify by temporarily removing `mvn` from
   `PATH`, e.g. `PATH=/usr/bin bash java/scripts/check.sh` if `mvn` is elsewhere).
2. `scripts/check_all.sh` runs to completion including the new Java stage.
3. YAML lints clean: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))"`.
4. Confirm `notify.needs` contains `java-check` and the status `if` references
   `needs.java-check.result`.

## Out of scope

- The prebuild / classifier-jar workflow (`java-prebuild.yml`) → **T13**.
- Any new native methods or Java classes → **T03+**.
- `-Xcheck:jni` surefire wiring → **T05** (added with the async machinery).
- Fork-safe community workflow mirroring: the existing `community.yml` split is
  unchanged; do not touch it (the Java job lives in the keyed `ci.yml` like the
  other binding jobs).
