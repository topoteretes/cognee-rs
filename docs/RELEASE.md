# Release runbook

How to cut and publish a cognee-rust release. Two tracks (see
[docs/plans/release/00-INDEX.md](plans/release/00-INDEX.md)):

- **Track A** — bindings + source: PyPI (`cognee`), npm (`cognee`), C-library artifact,
  GitHub source tag. Does **not** require crates.io publishability.
- **Track B** — crates.io: publish the `cognee-*` library crates (gated on removing
  git deps / `[patch.crates-io]`; see release task 24).

## Pre-flight (all tracks)

1. Ensure `main` is green: `ci.yml` passing, cross-SDK parity (`http-parity.yml`) green
   where applicable.
2. Run the local gate: `scripts/check_all.sh` (fmt → check → clippy -D warnings → binding
   checks).
3. Bump versions:
   - `[workspace.package] version` in `Cargo.toml` (crates inherit via `version.workspace`).
   - `capi/Cargo.toml`, `js/cognee-neon/Cargo.toml` (separate/standalone — bump manually).
   - `python/pyproject.toml`, `js/package.json`.
   - Keep all four in sync.
4. Update `CHANGELOG.md` (Keep a Changelog format) with the new version section.
   *(Note: `CHANGELOG.md` does not exist yet — it is created by release task 22. Add a stub entry here or skip until task 22 lands.)*
5. Confirm `LICENSE` and license metadata are present (see release task 02).

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

## Publish — npm (JS binding)

```bash
bash js/scripts/check.sh            # gate (builds the .node artifact)
cd js
npm publish --dry-run
npm publish                         # needs npm auth (npm login / NPM_TOKEN)
```

Confirm `package.json` `files` allowlist includes `lib/`, the install scripts
(`scripts/postinstall.js`, `scripts/copy-artifact.js`), and `LICENSE`. The native
`cognee_neon.node` is not shipped in the allowlist — `scripts/postinstall.js` builds it
from source (or fetches a prebuild) on install.

## Publish — C-library artifact

```bash
bash capi/scripts/check.sh          # gate
# Build the release library + assemble headers + LICENSE into a dist dir/tarball.
# (capi/ is its own workspace; build from there.)
```

Attach the resulting tarball (lib + headers + `LICENSE`) to the GitHub Release for the tag.

## Publish — crates.io (Track B only)

> Blocked until release task 24 removes git deps / `[patch.crates-io]` from the published
> dependency graph. Until then, `cargo publish` will refuse non-leaf crates.

```bash
# Dry-run each crate in dependency order (leaves first):
cargo publish --dry-run -p cognee-models
# ... then publish in the same order:
cargo publish -p cognee-models
```

## Post-release

1. Create a GitHub Release from the tag; paste the `CHANGELOG.md` section (added in the
   release-metadata step, task 22); attach the C artifact.
2. Verify installs: `pip install cognee==X.Y.Z`, `npm install cognee@X.Y.Z`.
3. Open the next `-dev` version bump PR if you use one.
