# T13 — `java-prebuild.yml`: per-platform classifier-jar workflow

## Objective

After this task `.github/workflows/java-prebuild.yml` builds the `cognee_java`
cdylib on the same 4-target matrix as `ts-prebuild.yml` and produces one
**classifier jar** per platform (each containing only
`native/<classifier>/libcognee_java.{so,dylib,dll}`), uploaded as workflow
artifacts. Publishing them to Maven Central is **T14** (blocked: infra).

## Dependencies & preconditions

- **T11 done** (the binding is complete and builds release cleanly). Verify
  `cargo build --release --manifest-path java/cognee-java-jni/Cargo.toml`
  succeeds locally.
- Read `.github/workflows/ts-prebuild.yml` in full — the matrix (linux-x64-gnu,
  linux-arm64-gnu via `cross`, darwin-arm64 on `macos-15`, win32-x64-msvc), the
  per-OS native-dep installs (cmake/protoc; brew on mac; choco + `ilammy/msvc-dev-cmd`
  + the Windows CRT/`ccache` env fixes on windows), the `cross` cross-compile
  path with `ORT_CACHE_DIR=/target/ort-cache` + `COGNEE_REPO_ROOT`, and the ORT
  prefetch step. **Clone all of these mechanics** — they are load-bearing for
  building `cognee-lib` (lbug/qdrant/ort) on each platform.

## Context for this task

**Platform → classifier → library-file map** (must match `NativeLibLoader`
from T01):

| Matrix `platform` | `rust-target` | classifier | lib file in jar |
|---|---|---|---|
| linux-x64-gnu | x86_64-unknown-linux-gnu | `linux-x86_64` | `libcognee_java.so` |
| linux-arm64-gnu | aarch64-unknown-linux-gnu | `linux-aarch_64` | `libcognee_java.so` |
| darwin-arm64 | aarch64-apple-darwin | `osx-aarch_64` | `libcognee_java.dylib` |
| win32-x64-msvc | x86_64-pc-windows-msvc | `windows-x86_64` | `cognee_java.dll` |

**Classifier jar layout:** a jar whose only content is
`native/<classifier>/<libfile>`. Built with the JDK `jar` tool (no Maven plugin
needed): stage the lib under `native/<classifier>/`, then
`jar cf cognee-<version>-<classifier>.jar -C <stagedir> native`. It is later
deployed as `ai.cognee:cognee:<version>:<classifier>` (Maven classifier) in T14.

## Steps

### 1. Create `.github/workflows/java-prebuild.yml`

Model it on `ts-prebuild.yml`. Skeleton (fill the per-OS dep/cross steps by
cloning ts-prebuild verbatim — only the build dir, artifact packaging, and
publish differ):

```yaml
name: Java Prebuild

# Build per-platform classifier jars containing the cognee_java cdylib.
# Matrix matches ts-prebuild.yml and the classifiers in
# java/src/main/java/ai/cognee/internal/NativeLibLoader.java.

on:
  push:
    tags:
      - "v[0-9]+.[0-9]+.[0-9]+"
  workflow_dispatch:

concurrency:
  group: java-prebuild-${{ github.ref }}
  cancel-in-progress: false

env:
  CARGO_PROFILE_RELEASE_DEBUG: "0"
  CARGO_INCREMENTAL: "0"
  ORT_CACHE_DIR: ${{ github.workspace }}/target/ort-cache
  CCACHE_COMPILERCHECK: content

jobs:
  build-platform:
    name: Build ${{ matrix.platform }}
    timeout-minutes: 90
    strategy:
      fail-fast: false
      matrix:
        include:
          - platform: linux-x64-gnu
            runner: ubuntu-latest
            rust-target: x86_64-unknown-linux-gnu
            cross: false
            classifier: linux-x86_64
            libfile: libcognee_java.so
          - platform: linux-arm64-gnu
            runner: ubuntu-latest
            rust-target: aarch64-unknown-linux-gnu
            cross: true
            classifier: linux-aarch_64
            libfile: libcognee_java.so
          - platform: darwin-arm64
            runner: macos-15
            rust-target: aarch64-apple-darwin
            cross: false
            classifier: osx-aarch_64
            libfile: libcognee_java.dylib
          - platform: win32-x64-msvc
            runner: windows-latest
            rust-target: x86_64-pc-windows-msvc
            cross: false
            classifier: windows-x86_64
            libfile: cognee_java.dll

    runs-on: ${{ matrix.runner }}

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.rust-target }}

      # --- CLONE FROM ts-prebuild.yml verbatim: ---
      #   * Install cross (matrix.cross)
      #   * mold linker (Linux native)
      #   * native deps: apt (Linux), brew (macOS), choco (Windows)
      #   * MSVC dev cmd + Windows C/C++ env fixes (win32)
      #   * ccache (non-Windows), Swatinem rust-cache (workspaces: java/cognee-java-jni -> target)
      #   * ORT cache + prefetch step
      # (Copy those step blocks, changing working-directory to java/cognee-java-jni
      #  and the cache keys' prefix from `ts-neon`/`ort-...-${platform}` to `java-...`.)

      - name: Set up JDK (for the jar tool)
        uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: "17"

      - name: Build cdylib (native)
        if: "!matrix.cross"
        working-directory: java/cognee-java-jni
        env:
          ORT_CACHE_DIR: ${{ github.workspace }}/java/cognee-java-jni/target/ort-cache
        run: cargo build --release --target ${{ matrix.rust-target }}

      - name: Build cdylib (cross)
        if: matrix.cross
        working-directory: java/cognee-java-jni
        env:
          ORT_CACHE_DIR: /target/ort-cache
          COGNEE_REPO_ROOT: ${{ github.workspace }}
        run: |
          # Clone the SIMSIMD SVE-disable flags + CFLAGS/CXXFLAGS from ts-prebuild.
          SVE="-DSIMSIMD_TARGET_NEON_I8=0 -DSIMSIMD_TARGET_NEON_F16=0 -DSIMSIMD_TARGET_NEON_BF16=0 -DSIMSIMD_TARGET_SVE=0 -DSIMSIMD_TARGET_SVE_I8=0 -DSIMSIMD_TARGET_SVE_F16=0 -DSIMSIMD_TARGET_SVE_BF16=0 -DSIMSIMD_TARGET_SVE2=0"
          TGT_US=$(echo "${{ matrix.rust-target }}" | tr '-' '_')
          export CFLAGS_${TGT_US}="$SVE" CXXFLAGS_${TGT_US}="$SVE"
          cross build --release --target ${{ matrix.rust-target }}

      - name: Stage native lib and pack classifier jar
        shell: bash
        run: |
          VER=$(sed -n 's:.*<version>\(.*\)</version>.*:\1:p' java/pom.xml | head -1)
          SRC="java/cognee-java-jni/target/${{ matrix.rust-target }}/release/${{ matrix.libfile }}"
          STAGE="staging/native/${{ matrix.classifier }}"
          mkdir -p "$STAGE"
          cp "$SRC" "$STAGE/${{ matrix.libfile }}"
          jar cf "cognee-${VER}-${{ matrix.classifier }}.jar" -C staging native
          echo "built cognee-${VER}-${{ matrix.classifier }}.jar"

      - name: Upload classifier jar
        uses: actions/upload-artifact@v4
        with:
          name: java-${{ matrix.classifier }}
          path: cognee-*-${{ matrix.classifier }}.jar
          retention-days: 7
```

> The `cross` leg needs a `java/cognee-java-jni/Cross.toml` (clone
> `ts/cognee-ts-neon/Cross.toml`: mount the repo via `COGNEE_REPO_ROOT` so the
> `../../crates` path-deps resolve inside the container, select the gcc-11 image
> for the arm64 target). Create it in this task alongside the workflow.

### 2. Create `java/cognee-java-jni/Cross.toml`

Clone `ts/cognee-ts-neon/Cross.toml` verbatim (it is crate-relative;
`COGNEE_REPO_ROOT` volume-mounts the whole repo and `/target/ort-cache` is the
ORT cache mount). Verify the paths match this crate's location
(`java/cognee-java-jni`).

### 3. Local approximation (the matrix cannot run locally)

Document in the workflow file's header comment that the only locally-verifiable
leg is the host platform. The executor runs the host-platform equivalent to
prove the packaging step:

```bash
cargo build --release --manifest-path java/cognee-java-jni/Cargo.toml
VER=$(sed -n 's:.*<version>\(.*\)</version>.*:\1:p' java/pom.xml | head -1)
mkdir -p staging/native/linux-x86_64
cp java/cognee-java-jni/target/release/libcognee_java.so staging/native/linux-x86_64/
jar cf "cognee-${VER}-linux-x86_64.jar" -C staging native
jar tf "cognee-${VER}-linux-x86_64.jar"   # should list native/linux-x86_64/libcognee_java.so
```

## Verification

1. YAML lints: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/java-prebuild.yml'))"`.
2. Local host-platform packaging (step 3) produces a jar whose `jar tf` output
   contains `native/linux-x86_64/libcognee_java.so`.
3. `scripts/check_all.sh` → still green (workflow files don't affect it).
4. The classifier and lib-file values in the matrix exactly match
   `NativeLibLoader`'s `platformClassifier()`/`libFileName()` (cross-check both).

## Out of scope

- Publishing/signing/deploying the jars to Maven Central → **T14** (blocked).
- Android AAR / additional targets (darwin-x64, musl) → post-v1 (match the
  ts-prebuild 4-target set exactly).
- Bundling all platforms into one fat jar → not the model; classifier jars are
  resolved per-platform by consumers (os-detector or explicit classifier).
