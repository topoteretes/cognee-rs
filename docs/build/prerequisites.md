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
| **OpenSSL 3** (`libssl`/`libcrypto`; headers too for a source build) | Ladybug links it dynamically into every binary with `ladybug` | `lbug` ≥ 0.22 |
| **Network + `curl` at build time** | `lbug` downloads its pinned prebuilt engine from GitHub releases (a failed download silently falls back to a source build that fails to link next to LanceDB) | the `ladybug` feature |
| **Python 3** (interpreter only) | Ladybug *source* builds only (Android, `LBUG_BUILD_FROM_SOURCE`) | `lbug` 0.23 |
| **Network at build time** | `ort-sys` downloads an ONNX Runtime build | the `onnx` feature |

Point `ORT_CACHE_DIR` at a writable, persistent path to reuse that download
across builds — the release workflows and both `Cross.toml` files do exactly
this. `--no-default-features` (no `onnx`) skips the fetch.

Ladybug (`lbug` 0.23) needs a C++ standard library that ships `<format>`: on
Linux that is **GCC ≥ 13** (libstdc++ 13; Ubuntu 24.04, Debian 13). This holds
for a prebuilt engine too, because the crate's own C++ bridge includes it.
GCC 11/12 hosts such as Ubuntu 22.04 and Amazon Linux 2023 cannot build the
`ladybug` feature; see [GCC 11/12 hosts](#gcc-1112-hosts-ubuntu-2204-amazon-linux-2023).

### Install

```bash
# Debian / Ubuntu
sudo apt-get update && sudo apt-get install -y build-essential cmake protobuf-compiler libssl-dev

# Fedora / RHEL
sudo dnf install -y gcc-c++ cmake protobuf-compiler openssl-devel

# macOS (Xcode command-line tools supply the compiler)
brew install cmake protobuf openssl@3

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

## GCC 11/12 hosts (Ubuntu 22.04, Amazon Linux 2023)

Since the move to `lbug` 0.23 these hosts cannot build the `ladybug` feature:
its sources and its Rust bridge include `<format>`, which libstdc++ only ships
from GCC 13. Install GCC 13+ (or build in an `ubuntu:24.04` container, as the
x86-64 release images do), or build without `ladybug`.

The SimSIMD workaround this section used to describe
(`CFLAGS`/`CXXFLAGS="-DSIMSIMD_TARGET_SAPPHIRE=0"`, for GCC 11's missing
`__m512h`) is no longer enough on its own, and GCC 13 does not need it.

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
