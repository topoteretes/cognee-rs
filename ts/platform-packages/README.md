# Platform packages for `cognee-ts`

This directory contains one sub-package per supported build target.  Each
package is published to npm as an **optional dependency** of the main
`cognee-ts` package with the naming convention:

```
@cognee-ts/neon-<platform>
```

where `<platform>` is a key produced by `@neon-rs/load`'s `currentPlatform()`
helper (e.g. `linux-x64-gnu`, `darwin-arm64`, `win32-x64-msvc`).

## Supported targets

| Package name | Rust target triple | Notes |
|---|---|---|
| `@cognee-ts/neon-linux-x64-gnu` | `x86_64-unknown-linux-gnu` | |
| `@cognee-ts/neon-linux-arm64-gnu` | `aarch64-unknown-linux-gnu` | |
| `@cognee-ts/neon-darwin-arm64` | `aarch64-apple-darwin` | Apple Silicon |
| `@cognee-ts/neon-win32-x64-msvc` | `x86_64-pc-windows-msvc` | |

## Layout of each platform package

```
platform-packages/<platform>/
  package.json          — name, version, os/cpu restrictions
  index.js              — loads cognee_ts_neon.node and re-exports it
  README.md             — per-platform notes
  cognee_ts_neon.node   — the compiled binary (populated by CI, not in git)
```

The `.node` files are **not committed to git**.  They are placed here by the
CI prebuild workflow (`ts-prebuild.yml`) before `npm publish` is run for each
platform package.

## Release process

The short version:

1. CI builds a matrix of (`os` × `arch`) Rust releases via
   `.github/workflows/ts-prebuild.yml`.
2. Each job copies its `.node` artifact into the matching
   `platform-packages/<platform>/` directory and runs
   `npm publish --access public` for `@cognee-ts/neon-<platform>`.
3. After all platform packages are published, the main `cognee-ts` package is
   published with `npm publish --access public` from `ts/`.

## Local development

For local development you do not need the per-platform packages.  Run:

```bash
cd ts
npm run build:rust   # compiles cognee-ts-neon and copies cognee_ts_neon.node here
npm run build:ts
npm test
```

The `postinstall` fallback in the main `cognee-ts` package detects the absence of
a matching prebuilt optional dep and builds from source automatically.
