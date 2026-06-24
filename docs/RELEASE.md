# Release runbook

How to cut and publish a cognee-rust release. Two tracks:

- **Track A** — bindings + source: PyPI (`cognee-py`), npm (`cognee`), C-library artifact,
  GitHub source tag. Does **not** require crates.io publishability.
- **Track B** — crates.io: publish the `cognee-*` library crates (gated on removing
  git deps / `[patch.crates-io]` from the dependency graph — a separate, larger effort).

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

## Publish — PyPI (Python binding)

```bash
bash python/scripts/check.sh        # gate
cd python
maturin build --release             # build wheel(s) + sdist
# Dry run first, then publish:
maturin publish --dry-run
maturin publish                     # needs PyPI token (MATURIN_PYPI_TOKEN / ~/.pypirc)
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

## Publish — crates.io (Track B only)

> Blocked until git deps / `[patch.crates-io]` are removed from the published
> dependency graph. Until then, `cargo publish` will refuse non-leaf crates.

```bash
# Dry-run each crate in dependency order (leaves first):
cargo publish --dry-run -p cognee-models
# ... then publish in the same order:
cargo publish -p cognee-models
```

## Post-release

1. Create a GitHub Release from the tag; paste the `CHANGELOG.md` section; attach the C artifact.
2. Verify installs: `pip install cognee-py==X.Y.Z`, `npm install cognee@X.Y.Z`.
3. Open the next `-dev` version bump PR if you use one.
