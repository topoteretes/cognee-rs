# Why lbug's C++ thirdparty keeps rebuilding (and the ccache setup that fixes it)

Investigated 2026-06-12. The `lbug` crate (Ladybug graph DB, crates.io 0.14.1)
bundles the entire Ladybug C++ source tree and compiles it with CMake from its
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
| `ts/cognee-ts-neon` workspace | `ts/cognee-ts-neon/target` |
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
in the `js`/`capi` workspaces (cargo walks up to the root config). To bypass
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
build. Both binding workspaces set exactly that pair in their `[profile.dev]`
for this reason (`ts/cognee-ts-neon/Cargo.toml` carries the measurement
table); it is the difference between a 12.1 GiB and a 3.3 GiB debug `target/`.

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
| `ts` | dev, cargo defaults | 12.1 GiB |
| `ts` | dev, opt-level 1 + debug 0 | 3.3 GiB |
| `ts` / `java` | release | 2.8 GiB each |

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
  capi/js workspaces' independent resolutions hit the same entries.
  `CCACHE_COMPILERCHECK=content` is set workflow-wide because runner-image
  updates touch `/usr/bin/cc` mtimes, which would invalidate the default
  mtime-based compiler check.
- e2e Docker harness (`e2e-cross-sdk/Dockerfile` + `http-parity.yml`): the
  rust-builder stage installs ccache and sets the CMake launcher env directly
  (the repo's `.cargo/config.toml` and launcher script are not copied into
  the image), compiling into a `--mount=type=cache,target=/ccache` BuildKit
  mount. Locally that mount persists in the builder's state across
  `docker compose build` runs with no further setup. In CI, BuildKit cache
  mounts are not part of exported layer caches and start empty on fresh
  runners, so `http-parity.yml` persists the mount with
  `reproducible-containers/buildkit-cache-dance` + `actions/cache`
  (inject before build, extract after).
- Rust-side equivalent: `sccache` as `RUSTC_WRAPPER` also caches the ~700
  dependency crates across fresh worktree target dirs. Now wired — see
  "sccache for the Rust half" below for how it stays safe on machines that do
  not have it, and for the two ways of wiring it that are *not* safe.

### sccache for the Rust half (wired, opt-in by installation)

`.cargo/config.toml` sets `build.rustc-wrapper = "scripts/rustc-wrapper.sh"`,
which execs `sccache` when installed and is a transparent pass-through
otherwise — the same contract as the ccache launcher above.

The objection previously recorded here (that a hard `RUSTC_WRAPPER` breaks
machines without it) is handled by the pass-through, and toggling the wrapper
does **not** invalidate cargo fingerprints: verified by building, rebuilding
with a pass-through wrapper, then rebuilding without — both follow-ups were
0-crate no-ops. So installing or removing sccache costs no rebuild.

What it will not cache, by design rather than by failure: units built with
`-C incremental` (every first-party crate), proc macros, build scripts, and
the final bin/cdylib/staticlib links. Those fall back to a normal compile. The
registry dependencies are the cached part — the same graph this repo compiles
four times over.

### Escape hatch: prebuilt Ladybug (`LBUG_LIBRARY_DIR`)

`lbug`'s `build.rs` skips the bundled CMake build entirely when
`LBUG_LIBRARY_DIR` + `LBUG_INCLUDE_DIR` are set. All 17 static archives must
be collected into that single lib dir (harvest from one successful
`out/build` tree: `src/liblbug.a` + `third_party/*/lib*.a`). Removes the C++
build from every context permanently, but must be redone on each lbug version
bump; with ccache in place it should not be needed.
