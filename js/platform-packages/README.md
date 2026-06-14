# Platform packages for `cognee`

This directory contains one sub-package per supported build target.  Each
package is published to npm as an **optional dependency** of the main `cognee`
package with the naming convention:

```
@cognee/neon-<platform>
```

where `<platform>` is a key produced by `@neon-rs/load`'s `currentPlatform()`
helper (e.g. `linux-x64-gnu`, `darwin-arm64`, `win32-x64-msvc`).

## Supported targets

| Package name | Rust target triple | Notes |
|---|---|---|
| `@cognee/neon-linux-x64-gnu` | `x86_64-unknown-linux-gnu` | |
| `@cognee/neon-linux-arm64-gnu` | `aarch64-unknown-linux-gnu` | |
| `@cognee/neon-linux-x64-musl` | `x86_64-unknown-linux-musl` | Alpine / static |
| `@cognee/neon-linux-arm64-musl` | `aarch64-unknown-linux-musl` | Alpine / static |
| `@cognee/neon-darwin-x64` | `x86_64-apple-darwin` | |
| `@cognee/neon-darwin-arm64` | `aarch64-apple-darwin` | Apple Silicon |
| `@cognee/neon-win32-x64-msvc` | `x86_64-pc-windows-msvc` | |

## Layout of each platform package

```
platform-packages/<platform>/
  package.json          — name, version, os/cpu restrictions
  index.js              — loads cognee_neon.node and re-exports it
  README.md             — per-platform notes
  cognee_neon.node      — the compiled binary (populated by CI, not in git)
```

The `.node` files are **not committed to git**.  They are placed here by the
CI prebuild workflow (`js-prebuild.yml`) before `npm publish` is run for each
platform package.

## Release process

See [../../docs/bindings-parity/07-js-distribution.md] for the full release
runbook.  The short version:

1. CI builds a matrix of (`os` × `arch`) Rust releases via
   `.github/workflows/js-prebuild.yml`.
2. Each job copies its `.node` artifact into the matching
   `platform-packages/<platform>/` directory and runs
   `npm publish --access public` for `@cognee/neon-<platform>`.
3. After all platform packages are published, the main `cognee` package is
   published with `npm publish --access public` from `js/`.

## Local development

For local development you do not need the per-platform packages.  Run:

```bash
cd js
npm run build:rust   # compiles cognee-neon and copies cognee_neon.node here
npm run build:ts
npm test
```

The `postinstall` fallback in the main `cognee` package detects the absence of
a matching prebuilt optional dep and builds from source automatically.
