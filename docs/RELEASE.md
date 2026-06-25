# Release runbook

How to cut and publish a cognee-rust release. Two tracks:

- **Track A** — bindings + C artifact: npm (`cognee-ts` + 7 prebuilt platform
  packages), C-library tarballs attached to the GitHub Release, GitHub source
  tag. The Python binding is **not** published to PyPI — users build it from
  source via `cd python && maturin develop` (or `maturin build --release` for a
  local wheel).
- **Track B** — crates.io: publish the 24 OSS `cognee-*` library crates in
  topological order (release-plz drives this). OSS git deps were removed in
  T4, so non-leaf crates are now publishable.

## Pre-flight (all tracks)

1. Ensure `main` is green: `ci.yml` passing, cross-SDK parity (`http-parity.yml`) green
   where applicable.
2. Run the local gate: `scripts/check_all.sh` (fmt → check → clippy -D warnings → binding
   checks).
3. Bump versions:
   - `[workspace.package] version` in `Cargo.toml` (crates inherit via `version.workspace`).
   - `capi/Cargo.toml`, `ts/cognee-ts-neon/Cargo.toml` (separate/standalone — bump manually).
   - `python/pyproject.toml`, `ts/package.json`.
   - Keep all four in sync.
4. Update `CHANGELOG.md` (Keep a Changelog format) with the new version section.
5. Confirm `LICENSE-MIT`, `LICENSE-APACHE`, and license metadata are present.

## Tag

```bash
git checkout main && git pull
git tag -a vX.Y.Z -m "cognee-rust vX.Y.Z"
git push origin vX.Y.Z
```

## Python binding — build from source (no PyPI publish)

The Python binding is not published to PyPI. Users build it locally:

```bash
bash python/scripts/check.sh        # gate
cd python
maturin develop                     # install into the active venv
# or, for a redistributable wheel/sdist:
maturin build --release
```

## Publish — npm (TS binding)

```bash
bash ts/scripts/check.sh            # gate (builds the .node artifact)
cd ts
npm publish --dry-run
npm publish                         # needs npm auth (npm login / NPM_TOKEN)
```

Confirm `package.json` `files` allowlist includes `lib/`, the install scripts
(`scripts/postinstall.js`, `scripts/copy-artifact.js`), and `LICENSE-MIT` +
`LICENSE-APACHE`. The native
`cognee_ts_neon.node` is not shipped in the allowlist — `scripts/postinstall.js` builds it
from source (or fetches a prebuild) on install.

## Publish — C-library artifact

### Automated path

The `.github/workflows/capi-release.yml` GitHub Actions workflow runs on every
`v*` tag push and produces per-platform tarballs (linux-x86_64, linux-aarch64,
macos-x86_64, macos-aarch64, windows-x86_64) attached to the GitHub Release for
the tag. The manual instructions below remain valid for local validation or
custom builds. Both code paths share `capi/scripts/build-release-tarball.sh`.

### Manual path

```bash
bash capi/scripts/check.sh          # gate

# Build the release library (capi/ is its own workspace; build from there).
cargo build --release --manifest-path capi/Cargo.toml

# Assemble headers + LICENSE-MIT + LICENSE-APACHE + README into a dist tarball.
# Args: <tag> <cargo-target-or-empty> <archive-suffix>
bash capi/scripts/build-release-tarball.sh vX.Y.Z "" linux-x86_64
# → dist/cognee-capi-vX.Y.Z-linux-x86_64.tar.gz
```

Attach the resulting tarball (lib + headers + `LICENSE-MIT` + `LICENSE-APACHE`) to the GitHub Release for the tag.

## Publish — crates.io (Track B)

OSS git deps were removed in T4, so `cargo publish` works for non-leaf crates.
Publishing is driven by release-plz (see `.github/workflows/release-plz.yml`)
which walks the 24 OSS crates in topological order. For a manual fallback:

```bash
# Dry-run each crate in dependency order (leaves first):
cargo publish --dry-run -p cognee-models
# ... then publish in the same order:
cargo publish -p cognee-models
```

## Post-release

1. Create a GitHub Release from the tag (the `capi-release.yml` workflow
   auto-attaches per-platform C-API tarballs when triggered by the `v*` tag).
   Paste the `CHANGELOG.md` section into the release body.
2. Verify installs: `npm install cognee-ts@X.Y.Z` and a `maturin develop` smoke
   build for the Python binding.
3. Open the next `-dev` version bump PR if you use one.
