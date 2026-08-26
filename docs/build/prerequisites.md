# Native build prerequisites

Building the default feature set compiles two bundled C/C++ trees from source,
so a Rust toolchain alone is not enough. This page is the single source of
truth for what else has to be on `PATH`; `README.md` and `CONTRIBUTING.md`
link here rather than restating it.

`scripts/check_all.sh` refuses to start when `protoc` or `cmake` is missing and
prints the install line for your platform.

## What you need

| Tool | Needed for | Pulled in by |
|---|---|---|
| **Rust 1.91.1** | everything | `rust-toolchain.toml` selects it automatically |
| **C/C++ compiler** | every build, including slim | `ring`, plus the bundled C++ trees below |
| **`cmake`** | Ladybug (graph DB) and AWS-LC (Bedrock TLS) | `lbug`, `aws-lc-sys` |
| **`protoc`** | LanceDB's Protobuf schemas | `lancedb` → `lance` → `prost-build` |
| **Network at build time** | `ort-sys` downloads an ONNX Runtime build | the `onnx` feature |

Point `ORT_CACHE_DIR` at a writable, persistent path to reuse that download
across builds — the release workflows and both `Cross.toml` files do exactly
this. `--no-default-features` (no `onnx`) skips the fetch.

On x86-64 Linux the C++ compiler must be **GCC ≥ 12** (or Clang ≥ 16) unless
you disable one SIMD kernel — see [GCC 11 hosts](#gcc-11-hosts-amazon-linux-2023)
below. Other platforms need GCC ≥ 11 only, for C++20.

### Install

```bash
# Debian / Ubuntu
sudo apt-get update && sudo apt-get install -y build-essential cmake protobuf-compiler

# Fedora / RHEL / Amazon Linux 2023
sudo dnf install -y gcc-c++ cmake protobuf-compiler

# macOS (Xcode command-line tools supply the compiler)
brew install cmake protobuf

# Windows
choco install cmake protoc --no-progress
```

These are the same packages the CI workflows install; see the `Install build
deps` steps in `.github/workflows/`.

## Which builds need what

Both native dependencies are feature-gated, and the slim build needs neither:

| Build | `cmake` | `protoc` |
|---|---|---|
| `cargo build` (default features) | yes | yes |
| `cargo build -p cognee-cli --no-default-features` | **no** | **no** |
| `--features cognee-cli/android-default` | yes (Ladybug) | **no** (drops LanceDB) |

A C compiler is still required even for the slim build — `ring` survives into
every tree.

> **The slim lane is a compile canary, not a usable CLI.** With no features on,
> no backend factory is registered, while the runtime defaults still name
> `sqlite`, `lancedb` and `ladybug`. The binary links and then fails on the
> first `remember` / `recall` with `Unsupported … provider`. Use it to prove the
> feature gates still subtract (which is what `check_all.sh` does with it), not
> as a way to build a lighter working CLI. To actually run without `cmake` or
> `protoc`, select the backends you do want — the factories have to come from
> somewhere.

Check any feature set yourself rather than trusting this table as it ages —
`-i` prints who pulls the tool in:

```bash
cargo tree -p cognee-cli -e normal,build -i cmake
```

A tree that does not need the tool reports `package ID specification 'cmake'
did not match any packages`. That message means the dependency is absent, which
is the answer — not a broken command.

### Dropping the dependencies

- **`protoc`** comes only from LanceDB. Any build without the `lancedb`
  feature — the slim CLI, `android-default`, the wasm logic crates — needs no
  Protobuf compiler.
- **`cmake`** has *two* independent sources. Ladybug (`lbug`) is the obvious
  one; the second is `aws-lc-sys`, reached through `aws-config` from the
  default-on `bedrock` feature. Dropping `ladybug` alone leaves `cmake`
  required. Drop both features to shed it.

## GCC 11 hosts (Amazon Linux 2023)

Ladybug bundles [SimSIMD](https://github.com/ashvardanian/SimSIMD), whose
dispatch shim force-enables every x86 kernel on Linux regardless of what the
compiler supports:

```c
/*  - Linux: everything is available in GCC 12+ and Clang 16+. */
#if !defined(SIMSIMD_TARGET_SAPPHIRE) && (defined(__linux__))
#define SIMSIMD_TARGET_SAPPHIRE 1
#endif
```

The Sapphire Rapids kernels use `__m512h`, which GCC only gained in 12. On a
GCC 11 host — Amazon Linux 2023 ships 11.4 — the build fails inside SimSIMD.

Either install GCC 12+, or pre-define the macro so the block above leaves it
off:

```bash
export CFLAGS="-DSIMSIMD_TARGET_SAPPHIRE=0"
export CXXFLAGS="-DSIMSIMD_TARGET_SAPPHIRE=0"
cargo build --release
```

This costs the AVX512-FP16 distance kernels, which only Sapphire Rapids and
newer hardware would have dispatched to anyway; every other kernel is
unaffected.

Four release workflows already pull the same lever for the ARM kernels the
cross toolchain cannot assemble — `ts-prebuild.yml`, `java-prebuild.yml`,
`capi-release.yml` and `ort-mirror.yml` set `SIMSIMD_TARGET_SVE=0` and friends
through the per-target `CFLAGS_<target>` / `CXXFLAGS_<target>` variables. These
are preprocessor defines, not CMake cache variables: the `cmake` crate forwards
`CFLAGS`/`CXXFLAGS` into the bundled build.

## See also

- [lbug-rebuilds.md](lbug-rebuilds.md) — why the Ladybug C++ tree rebuilds and
  the ccache setup that fixes it.
- [CONTRIBUTING.md § Cleaning build artifacts](../../CONTRIBUTING.md#cleaning-build-artifacts)
  — the bundled C++ builds are large; `scripts/clean_all.sh` reclaims them.
