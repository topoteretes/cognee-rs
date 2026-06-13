# PKG-2 — JS: prebuild matrix / source-build fallback for the native addon

- **Binding:** JS/TS (`js/`)
- **Dimension:** Distribution
- **Priority:** P1
- **Status:** Not started

## Problem

[js/package.json](../../js/package.json) ships `files: ["lib/", "cognee_neon.node"]`
— a **single committed prebuilt `.node`** — and has **no `postinstall` /
`prebuild` / `node-gyp` step** and no prebuild matrix. So `npm install` only
works on the exact platform/arch/Node-ABI the published `.node` was built for;
any other target gets a broken install with no fallback-to-source build. Also
`engines.node: ">=16"` does not pin a Node-API/ABI version, so the single binary
is implicitly tied to one ABI.

For a native addon this is the single biggest distribution gap — it makes the
package effectively single-platform, while the Python (maturin wheels per
platform) and C API (build-from-source) stories are platform-portable.

## Goal / definition of done

`npm install` produces a working addon on the common targets (Linux x64/arm64,
macOS x64/arm64, Windows x64) — either by downloading a matching prebuilt binary
or by building from source as a fallback — without the consumer manually
compiling Rust.

## Design decision: prebuild tooling

Neon projects typically use one of:

- **`@neon-rs/load` + `cargo-cp-artifact` + a CI prebuild matrix** (the
  Neon-native path). Per-platform `.node` files are published as separate
  optional-dependency packages (`@cognee/neon-linux-x64`, etc.), and the main
  package selects the right one at load time. This is the modern Neon
  recommendation.
- **`prebuildify` + `node-gyp-build`** (the prebuild-or-build-from-source path):
  commit/ship prebuilds for common targets under `prebuilds/`, and fall back to
  a source build via `node-gyp-build` when no prebuild matches.

Recommend the **`@neon-rs/load` optional-dependency** model since the project is
already Neon-based; it gives clean per-platform packages and Node-ABI selection.

## Implementation plan

### Step 1 — Make the loader ABI/platform-aware

Replace the hard `require("../cognee_neon.node")` in
[js/src/native.ts:480](../../js/src/native.ts#L480) with `@neon-rs/load` (or
`node-gyp-build`), which resolves the correct binary for the current
platform/arch/ABI at runtime. Keep a clear error if no matching binary is found,
pointing at the build instructions.

### Step 2 — Build a CI prebuild matrix

Add a GitHub Actions workflow (or extend `.github/workflows/js-check.yml`) that,
on release tags, cross-compiles the Neon addon for the target matrix:

| OS | arch | notes |
|---|---|---|
| linux | x64, arm64 | use `cross` or native runners |
| macos | x64, arm64 | universal or per-arch |
| windows | x64 | |

Each job runs `npm run build:rust` (release), renames the artifact via
`cargo-cp-artifact`, and publishes the per-platform package (or uploads the
`.node` to the `prebuilds/` set).

### Step 3 — Wire optional dependencies / package layout

For the `@neon-rs/load` model, generate the per-platform packages and list them
as `optionalDependencies` in the main `package.json`. npm installs only the one
matching the consumer's platform. Document this in
[js/README.md](../../js/README.md).

### Step 4 — Source-build fallback

Ensure that when no prebuilt/optional package matches, install falls back to a
source build (requires Rust toolchain). Either:

- a `postinstall` that runs `npm run build:rust` when no binary is found, or
- `node-gyp-build`'s built-in "build if no prebuild" behavior.

Gate the fallback so it only triggers on a missing binary, not on every install
(building Rust on every `npm install` is unacceptable).

### Step 5 — Pin Node-API version

Decide and pin the Node-API (N-API) version the addon targets (Neon supports
N-API; pick e.g. NAPI 6 → Node ≥ 14, or align with `engines.node`). Document the
supported Node range precisely instead of the open-ended `">=16"`.

### Step 6 — Update `files` and `.npmignore`

Remove the committed `cognee_neon.node` from version control (it is a build
artifact; it is already gitignored per the leftover-artifact note in the review).
Ensure `files`/`.npmignore` ship only what each package needs (the JS in `lib/`
for the main package; the `.node` for each platform package).

## Verification

```bash
cd js && npm run build && npm test         # local build still works
# In CI: matrix jobs each produce a loadable .node
# Smoke: in a clean container per target, `npm install <packed tarball>` then
node -e "const {Cognee}=require('cognee'); new Cognee(); console.log('loaded')"
```

Verify load on at least Linux x64 and one other target (macOS arm64 or Windows
x64) using `npm pack` tarballs in clean environments.

## Risks / notes

- Cross-compiling Rust + native addons in CI is the bulk of the effort; the
  qdrant/lbug native deps (`crates/vector`, `crates/graph`) may complicate
  cross-builds (cmake/C deps). Validate each matrix target builds the *full*
  feature set the JS binding enables, or document reduced-feature platform
  packages.
- Per-platform optional-dependency packages add release-process complexity
  (publishing N packages atomically). Document the release runbook.
- This task does not change the JS API surface — purely packaging/distribution.
