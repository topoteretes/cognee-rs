# Why lbug's C++ thirdparty keeps rebuilding (and the ccache setup that fixes it)

Investigated 2026-06-12 against `lbug` 0.14.1, which always compiled from
source. Since the move to 0.23.0 the default build links a pinned prebuilt
instead (see *Which engine gets linked* below), so this applies only to source
builds (Android, or `LBUG_BUILD_FROM_SOURCE`). The `lbug` crate (Ladybug graph DB) bundles the entire Ladybug C++ source tree and compiles it with CMake from its
`build.rs` into cargo's `OUT_DIR`. One full build is ~2 minutes of C++
compilation (1,008 compiler invocations) and a ~2.6 GB `out/` directory
(17 static libraries: lbug itself plus utf8proc, antlr4, re2, parquet, thrift,
snappy, zstd, mbedtls, brotli, lz4, roaring_bitmap, simsimd, …).

## Root cause

Cargo names the build directory `target/debug/build/lbug-<unit-hash>` and the
unit hash includes the fingerprints of the **build-dependency closure** of the
crate — for lbug that is `cmake`, `cc`, `cxx-build`, `rustversion` and their
transitive deps. Empirically verified:

- Changing a **lib** dependency of lbug in `Cargo.lock` (e.g. `rust_decimal`
  1.41 → 1.40) does **not** change the hash; the existing OUT_DIR is reused.
- Changing a **build** dependency (e.g. `cc` 1.2.60 → 1.2.59) **does** change
  the hash. Cargo creates a brand-new, empty OUT_DIR and `build.rs` runs the
  whole CMake build from scratch. The old 2.6 GB directory is left behind.

`cc` releases several times a month, so any lockfile re-resolution — a fresh
checkout/worktree (no lockfile is committed; this repo ships as an SDK), or
dependency churn like the June 2026 `cookie`/`time = "=0.3.46"` saga (`time`
is also a direct lbug dependency) — silently bumps the closure and forces a
full rebuild. The main checkout accumulated 4 complete lbug builds in a month
(~10 GB of stale artifacts); each Claude agent worktree builds its own copy.

The hash is **path-independent**: a worktree with an identical `Cargo.lock`
and toolchain computes exactly the same `lbug-<hash>`. Only the lockfile and
toolchain matter.

### Where lbug gets built

| Context | Target dir |
|---|---|
| Main workspace | `target/` |
| Each Claude agent worktree | `.claude/worktrees/*/target` |
| `bindings` workspace (Neon + JNI cdylibs) | `bindings/target` |
| `capi` workspace (default-features check) | `capi/target` |
| e2e Docker harness | inside the image |

(`target/check-noop` from `check_all.sh` and the capi slim check don't build
lbug — `cognee-telemetry` and the `sqlite,testing` feature set don't pull it.)

## The fix: ccache on the bundled CMake builds (wired into the repo)

CMake (≥ 3.17) initializes `CMAKE_<LANG>_COMPILER_LAUNCHER` from the
environment at first configure, and the `cmake` crate inherits the cargo
process environment. The repo's `.cargo/config.toml` `[env]` section points
those variables at `scripts/ccache-launcher.sh`, which uses ccache when
installed and is a transparent pass-through otherwise — machines without
ccache build exactly as before.

Two non-obvious settings are required (also in `[env]`):

- `CCACHE_NOHASHDIR=true` — these are `-g` builds; by default ccache hashes
  the compile cwd (it ends up in debug info as `DW_AT_comp_dir`), and the cwd
  is exactly the per-unit-hash OUT_DIR that keeps shifting. Without this,
  every entry misses (measured: 6/1008 hits).
- `CCACHE_BASEDIR=<checkout root>` — lbug's own `src/` compilations pass
  `-I<OUT_DIR>/build/src/include` for generated headers; basedir rewrites
  those to cwd-relative paths so they match across OUT_DIRs and worktrees.

Measured on Apple Silicon (cargo build -p lbug, debug profile), simulating
the churn by pinning `cc` to successive versions so cargo picks a fresh
OUT_DIR each time:

| Scenario | Wall time | ccache hits |
|---|---|---|
| Cold cache, fresh OUT_DIR | 1m 52s | 0 / 1008 |
| Warm cache, fresh OUT_DIR | **15.6s** | 1000 / 1008 (99.2%) |

The residual 15s is CMake configure + archiving/linking the static libs,
which ccache cannot cache.

### Per-machine setup

```bash
brew install ccache          # macOS; apt/dnf install ccache on Linux
ccache --max-size 20G        # optional headroom; one lbug tree is ~0.3 GiB compressed
```

Nothing else — the committed launcher + `[env]` config picks it up
automatically, including in worktrees (each worktree carries the config) and
in the `bindings`/`capi` workspaces (cargo walks up to the root config). To bypass
per-shell: `CMAKE_CXX_COMPILER_LAUNCHER="" cargo build …` (a set env var wins
over `[env]`).

Caveat: the launcher is a POSIX `sh` script; on Windows set the
`CMAKE_*_COMPILER_LAUNCHER` env vars to `ccache` directly or to empty.

## The cargo profile decides how big the C++ tree is (2.9 GiB vs 213 MiB)

Investigated 2026-07-31, separately from the rebuild-frequency problem above:
the *size* of each `out/` tree is set by the cargo profile of whatever is
being built, and the mapping is not the obvious one.

Cargo hands every build script an `OPT_LEVEL` and a `DEBUG` env var derived
from the profile of the package it belongs to. The `cmake` crate turns that
pair into a `CMAKE_BUILD_TYPE`, so lbug's Ladybug tree inherits the Rust
profile:

| `opt-level` | `debug` | CMake build type | `out/` size |
|---|---|---|---|
| 0 | any | `Debug` | 2.9 GiB |
| ≥ 1 | non-zero (incl. `"line-tables-only"`) | `RelWithDebInfo` | 2.6 GiB |
| ≥ 1 | `0` | `Release` | **213 MiB** |

Two consequences that are easy to get wrong:

- **`debug = "line-tables-only"` does nothing for the C++ side.** Any non-zero
  debug level reads as "wants debug info", so the tree stays at 2.6 GiB. It is
  a good setting for the Rust half and irrelevant to this one.
- **`opt-level = 1` alone does nothing either** — at `debug = 2` (the dev
  default) it only moves Debug to RelWithDebInfo, 2.9 → 2.6 GiB.

Only `opt-level >= 1` **and** `debug = 0` together reach a plain Release C++
build. The `bindings/` workspace (the Neon + JNI cdylibs) sets exactly that pair
in its `[profile.dev]` for this reason, and `bindings/Cargo.toml` carries the
measurement table; it is the difference between a 12.1 GiB and a 3.3 GiB debug
`target/`.

### Escaping the trade-off: pin lbug, not the whole graph

Wanting debug info for Rust dependencies while *not* paying for it in the
vendored C++ is a normal thing to want, and the two are separable — the rule
above keys off lbug's own profile, so overriding that one package is enough:

```toml
[profile.release.package."*"]
debug = "line-tables-only"   # every Rust dependency keeps line tables

[profile.release.package.lbug]
debug = 0                    # …except lbug, whose build script builds the C++
```

Measured in `capi` (release, aarch64-apple-darwin, from clean):

| Third-party `debug` | lbug `out/` | `capi/target` | `libcognee_capi.a` |
|---|---:|---:|---:|
| `0` | 213 MiB | 4.7 GiB | 0.78 GiB |
| `"line-tables-only"`, lbug included | 2.6 GiB | 16.2 GiB | 3.12 GiB |
| `"line-tables-only"`, lbug pinned to `0` | 213 MiB | 10.5 GiB | 2.02 GiB |

The pin costs only the line tables of lbug's own Rust binding layer — generated
FFI glue nobody steps through — and saves 5.7 GiB of `target/` plus 1.1 GiB of
static library. `capi/Cargo.toml` ships this configuration.

Measured from clean on aarch64-apple-darwin, one workspace at a time. The
per-workspace totals that follow from it:

| Workspace | Profile | `target/` |
|---|---|---|
| `capi` | release, `debug = true` | 21.7 GiB |
| root | dev, `--all-targets` | 19.9 GiB |
| `bindings`, Neon only | dev, cargo defaults | 12.1 GiB |
| `bindings`, Neon only | dev, opt-level 1 + debug 0 | 3.3 GiB |
| `bindings`, both cdylibs | dev, opt-level 1 + debug 0 | 3.9 GiB |
| `bindings`, either cdylib | release | 2.8 GiB |

The "both cdylibs" row is the one the shared workspace buys: adding the second
cdylib to a dev tree that already holds the first costs 0.54 GiB and 7 crates
rather than a second 3.3 GiB engine build. It only applies when both are built
in the *same* profile — `ts/scripts/check.sh` builds release and
`java/scripts/check.sh` builds dev, so a `check_all.sh` run gets no sharing.

## Complementary measures

### Keep resolutions stable (no committed lockfile)

This repo intentionally does not commit `Cargo.lock` (SDK). To reduce churn
frequency anyway:

- Don't run bare `cargo update`; bump specific crates with
  `cargo update -p <crate>`.
- If churn becomes painful again, lbug's volatile build-dep closure can be
  stabilized with exact pins in `[workspace.dependencies]` the same way
  `time = "=0.3.46"` is pinned today (e.g. `cc = "=1.2.60"` declared as a
  build-dependency of `cognee-graph`). Trade-off: exact pins propagate to
  SDK consumers if/when crates are published, so prefer ccache.

### Reclaim disk

Only the newest `lbug-<hash>` matches the current lock; stale siblings are
~2.6 GB each:

```bash
ls -dt target/debug/build/lbug-*/out | tail -n +2 | xargs rm -rf
```

Stale agent worktrees each hold a 13–18 GB target dir; remove with
`git worktree remove <path>`.

### CI and Docker (both wired)

- GitHub Actions (`ci.yml`): every job runs `hendrikmuhs/ccache-action`,
  which installs ccache (the committed launcher picks it up automatically)
  and persists the cache via actions/cache. Only the `lint` job — the root
  of the job DAG — saves; the five downstream jobs restore-only, so each run
  pushes one ccache blob instead of six near-identical ones into the 10 GB
  repo cache quota. This matters more in CI than locally: no lockfile is
  committed, so a `cc`/`cmake` release on crates.io invalidates the Swatinem
  target caches of **all** jobs at once, and GitHub runners have only 4
  vCPUs for the from-scratch C++ build. ccache keys on
  compiler + flags + source content (not cargo's unit hash), so the
  capi/bindings workspaces' independent resolutions hit the same entries.
  `CCACHE_COMPILERCHECK=content` is set workflow-wide because runner-image
  updates touch `/usr/bin/cc` mtimes, which would invalidate the default
  mtime-based compiler check.
- e2e Docker harness (`e2e-cross-sdk/Dockerfile` + `http-parity.yml`): no
  ccache. The image no longer compiles Rust (#133); the binaries are built by
  `e2e-cross-sdk/build-rust-binaries.sh` in the Dockerfile's `rust-build-env`
  stage with the checkout bind-mounted, so lbug's C++ objects live in the
  host target dir, which `http-parity.yml` persists with rust-cache. (The
  earlier rust-builder stage with a ccache BuildKit mount and
  buildkit-cache-dance is gone.)
- Rust-side equivalent: `sccache` as `RUSTC_WRAPPER` also caches the ~700
  dependency crates across fresh worktree target dirs. Now wired — see
  "sccache for the Rust half" below for how it stays safe on machines that do
  not have it, and for the two ways of wiring it that are *not* safe.

### sccache for the Rust half (wired, opt-in by installation)

`scripts/check_all.sh` exports `RUSTC_WRAPPER=sccache` when that binary is on
PATH, and leaves it unset otherwise. A caller-set `RUSTC_WRAPPER` still wins.

Deliberately *not* done two more obvious ways, both of which are broken in ways
that only surface off a macOS dev machine:

- **A `#!/bin/sh` wrapper script**, mirroring `ccache-launcher.sh`. A shell
  interposed between cargo and rustc drops every environment variable whose
  name is not a valid shell identifier. Linux `/bin/sh` is dash, which discards
  `CARGO_BIN_EXE_cognee-cli`, so `crates/cli/tests` fails to compile;
  `env CARGO_BIN_EXE_cognee-cli=X dash wrapper.sh env | grep -c cognee-cli`
  prints 0 under dash and 1 under bash. This was tried and turned PR CI red on
  every Linux runner while passing locally.
- **`build.rustc-wrapper` in `.cargo/config.toml`.** That applies on Windows
  too, where CreateProcess cannot launch a `.sh` at all, breaking every Windows
  release leg — the same hazard `capi-release.yml` already resets the CMake
  launcher variables for.

Toggling sccache does **not** invalidate cargo fingerprints: verified by
building, rebuilding with a wrapper, then rebuilding without — both follow-ups
were 0-crate no-ops. So installing or removing it costs no rebuild.

The trade against the config.toml approach is scope: this covers
`check_all.sh` runs rather than every `cargo` invocation. Export it yourself
for interactive builds.

What it will not cache, by design rather than by failure: units built with
`-C incremental` (every first-party crate), proc macros, build scripts, and
the final bin/cdylib/staticlib links. Those fall back to a normal compile. The
registry dependencies are the cached part — the same graph this repo compiles
three times over (root, capi, bindings; the two cdylibs in bindings share one
build only when both use the same profile).

### Escape hatch: prebuilt Ladybug (`LBUG_LIBRARY_DIR`)

`lbug`'s `build.rs` skips the bundled CMake build entirely when
`LBUG_LIBRARY_DIR` + `LBUG_INCLUDE_DIR` are set. All 17 static archives must
be collected into that single lib dir (harvest from one successful
`out/build` tree: `src/liblbug.a` + `third_party/*/lib*.a`). Removes the C++
build from every context permanently, but must be redone on each lbug version
bump; with ccache in place it should not be needed.

## Which engine gets linked

From 0.17 on, the `lbug` crate's `build.rs` does not compile its bundled C++
by default: it runs `scripts/download_lbug.sh`, which fetches a prebuilt static
`liblbug` from the LadybugDB GitHub releases. With `LBUG_VERSION` unset it asks
for the **latest** release, so the crate version in `Cargo.toml` does not
decide which engine you link, and the result changes whenever upstream tags a
release. While triaging SDK-1128, tests that claimed 0.19.0 fixed a bug had in
fact linked the latest prebuilt, v0.21.2.

`.cargo/config.toml` therefore sets `LBUG_VERSION = "0.23.0"` (`force =
false`), matching `lbug = "=0.23.0"`. Keep the two in step.

**Why the prebuilt and not a source build (`LBUG_BUILD_FROM_SOURCE`).** Both
archives bundle zstd and lz4. Upstream's release archive is one combined
object with those symbols made local; a source build exports them
(`ZSTD_compressBound`, `LZ4_compress_default`, …), and any binary that also
links LanceDB's `zstd-sys` / `lz4-sys` fails to link on Linux with `rust-lld:
duplicate symbol`. The versions differ (Ladybug's zstd 1.5.7 vs `zstd-sys`'s
1.6.0), so `--allow-multiple-definition` would silently mix two zstd builds and
is not an option. lbug's `LBUG_LOCALIZE_BUNDLED_SYMBOLS` only post-processes a
*downloaded* archive. 0.14.1 had none of this because it compiled zstd inside a
C++ namespace (`lbug_zstd::`).

What the prebuilt costs:

- **Network and `curl` at build time**, from
  `github.com/LadybugDB/ladybug/releases`, with no checksum. If the download
  fails for any reason (no network, no `curl` in a build container), lbug
  *silently* falls back to a source build, and the failure only surfaces at
  link time as `duplicate symbol: ZSTD_*`. That error means "the prebuilt was
  not fetched", not a code problem. Point `LBUG_PREBUILT_CACHE_DIR` at a persistent directory to
  reuse it across builds.
- **The archive is chosen by the build host's `uname`, not the Rust target.**
  Cross builds (`cross` for Linux arm64) need the target's archive fetched
  explicitly and passed in through `LBUG_LIBRARY_DIR` + `LBUG_INCLUDE_DIR`.
- **No Android or iOS archives exist.** Those targets build from source
  (`LBUG_BUILD_FROM_SOURCE=1`, which `scripts/android-build-and-deploy.sh`
  sets). That is supported, but only safe in a binary that does not also link
  `zstd-sys`/`lz4-sys` (`android-default` drops LanceDB). The source build
  needs an OpenSSL 3 built for the target: point `OPENSSL_ROOT_DIR` at its
  prefix (the NDK ships none), and ship its `libssl.so`/`libcrypto.so` next to
  the binary.
- The macOS arm64 archive is built with `minos 13.3`.

The ccache setup above only matters for source builds now.

**Storage version.** The pin is exact because the engine version is also the
on-disk storage version. 0.23.0 writes storage code 53; it is the Ladybug the
Python SDK moves to in its next release, and the two SDKs can open each
other's stores only while they agree on it. Ladybug upgrades an older store's
header in place on its first checkpoint, after which older engines cannot open
it. Bump `lbug` together with Python cognee's `ladybug` pin (and its
`ladybug_version_mapping`), never alone.

**Toolchain and runtime.** 0.23.0 needs a C++ standard library with
`<format>`: libstdc++ 13 (GCC 13+) on Linux. That holds for the prebuilt too,
because the crate's own C++ bridge includes `lbug.hpp`, which includes
`<format>`; GCC 11/12 hosts (Ubuntu 22.04, Amazon Linux 2023) cannot build the
`ladybug` feature. Unlike 0.14.1 the engine also links OpenSSL 3 (`libssl`,
`libcrypto`) dynamically, and `libatomic` on Linux. A source build additionally
needs OpenSSL 3 headers and a Python 3 interpreter.
