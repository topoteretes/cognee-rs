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
| `js/cognee-neon` workspace | `js/cognee-neon/target` |
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

### CI and Docker (follow-ups, not wired)

- GitHub Actions: install ccache and cache `~/Library/Caches/ccache` /
  `~/.cache/ccache` keyed per OS + toolchain; the committed launcher then
  makes lbug rebuilds cache-hit in CI too.
- e2e Docker harness: `RUN --mount=type=cache,target=/root/.cache/ccache`
  plus `ccache` in the builder stage achieves the same for image rebuilds.
- Rust-side equivalent: `sccache` as `RUSTC_WRAPPER` would also cache the
  ~700 dependency crates across fresh worktree target dirs. Not wired because
  a hard `RUSTC_WRAPPER` breaks machines without sccache; revisit if worktree
  warm-up (not lbug) becomes the bottleneck.

### Escape hatch: prebuilt Ladybug (`LBUG_LIBRARY_DIR`)

`lbug`'s `build.rs` skips the bundled CMake build entirely when
`LBUG_LIBRARY_DIR` + `LBUG_INCLUDE_DIR` are set. All 17 static archives must
be collected into that single lib dir (harvest from one successful
`out/build` tree: `src/liblbug.a` + `third_party/*/lib*.a`). Removes the C++
build from every context permanently, but must be redone on each lbug version
bump; with ccache in place it should not be needed.
