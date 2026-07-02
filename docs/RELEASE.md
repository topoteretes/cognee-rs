# Release runbook

Releasing cognee-rust is a **two-phase, release-PR-driven** flow:

1. **Open** — label an issue `release:X.Y.Z` → a bot opens a `release/X.Y.Z` PR
   that bumps every version and updates the changelog. Nothing is published.
2. **Verify** — the normal PR CI builds everything and dry-run-packages the
   crates; you review the changelog and the green checks.
3. **Publish** — merging the PR (with a required-reviewer approval on the publish
   job) publishes to **crates.io + npm + the C-API GitHub Release**.

## TL;DR — cut a release

1. Make sure `main` is green and the [required secrets](#required-secrets-and-setup) exist.
2. Open a tracking issue (e.g. "Release 0.1.3") and add a label named exactly
   `release:0.1.3`.
3. A `release/0.1.3` PR opens automatically. Wait for CI, review the `CHANGELOG.md`
   diff, approve, and **merge**.
4. Approve the `release` environment gate on the **release-publish** run. Done.

Only users with write access can add labels or approve the environment, so those
are the authorization gates.

## The flow in detail

### Phase 1 — `release-open.yml` (trigger: `release:X.Y.Z` label on an issue)
Uses `RELEASE_PAT`. Creates branch `release/X.Y.Z`, runs
[`scripts/release/set-version.sh`](../scripts/release/set-version.sh),
regenerates the changelog with `git-cliff` ([`cliff.toml`](../cliff.toml)),
commits, pushes, and opens a PR labelled `autorelease:pending`. The PAT (not
`GITHUB_TOKEN`) is required so the PR triggers CI.

### Phase 1 verification — the PR checks (all pre-existing, plus one)
- `ci.yml` — workspace build/test + `capi-check` (C library) + `ts-check` (Neon
  addon) + `python-check`. Runs on same-repo PRs, so the release branch qualifies.
- `publish-dry-run.yml` — `cargo publish --dry-run` over every crate.
- `release-verify.yml` (release branches only) — asserts every version location
  is consistent ([`assert-version.sh`](../scripts/release/assert-version.sh)) and
  runs an early `npm whoami` to confirm the npm token.

A human reviews the changelog diff + green checks, approves the review, and merges.

### Phase 2 — `release-publish.yml` (trigger: the release PR is merged)
Runs behind the **`release` environment** (required reviewers). Order:
0. **Preflight** — `cargo owner --list -p cognee-models` and `npm whoami` (real
   authenticated calls; `cargo publish --dry-run` does **not** validate a token,
   so this is the only way to fail before anything irreversible).
1. **crates.io** — `cargo publish` each crate in
   [`publish-order.sh`](../scripts/split/publish-order.sh) order (cargo waits for
   the index between dependents; already-published versions are skipped, so
   re-runs are safe).
2. **Tag** — push `vX.Y.Z` with `RELEASE_PAT`, which cascades into:
   - `capi-release.yml` → 5 per-platform C-API tarballs attached to the Release.
   - `ts-prebuild.yml` → prebuilt `.node` binaries + `@cognee/neon-*` and
     `@cognee/cognee-ts` npm packages (gated on `NPM_TOKEN`).
3. **GitHub Release** — created/updated with the changelog section.

Publishing crates.io *before* the tag cascade means a crates.io failure aborts
before any npm / C-API artifact ships.

## Version locations (all bumped by `set-version.sh`)

| Location | How |
|---|---|
| Root `Cargo.toml` + internal `cognee-*` dep reqs in `crates/*/Cargo.toml` | `cargo set-version --workspace` |
| `capi/Cargo.toml` | `cargo set-version --manifest-path capi/Cargo.toml` |
| `ts/cognee-ts-neon/Cargo.toml` | `cargo set-version --manifest-path …` |
| `ts/package.json` + `@cognee/neon-*` pins + `ts/platform-packages/*/package.json` | node script |
| `python/` (`cognee-python`) | **automatic** — inherits `version.workspace`; `pyproject.toml` is `dynamic` |

The Python binding is **not published to PyPI**; users build from source
(`cd python && maturin develop`, or `maturin build --release`).

## Required secrets and setup

| Secret | Scope | Used by |
|---|---|---|
| `RELEASE_PAT` | Repo secret. Fine-grained PAT scoped to this repo with **Contents: Read and write**, **Pull requests: Read and write**, **Issues: Read and write** (or a classic PAT with the `repo` scope). The owner only needs **write** access, not admin — the flow never pushes to `main` (it pushes a `release/*` branch and a tag; `main` advances via the PR merge). It must be a PAT rather than `GITHUB_TOKEN` so the opened PR triggers CI and the pushed tag cascades. | `release-open.yml` (open PR, push branch), `release-publish.yml` (push tag, create Release) |
| `CARGO_REGISTRY_TOKEN` | **Environment** secret on `release`. crates.io token with publish rights for all `cognee-*` crates. | `release-publish.yml` preflight + publish |
| `NPM_TOKEN` | Repo secret. npm token with publish rights to the `@cognee` org. | `release-verify.yml`, `ts-prebuild.yml`, `release-publish.yml` preflight |

**One-time setup:** in **Settings → Environments**, create an environment named
`release`, add the required reviewers who must approve each publish, and store
`CARGO_REGISTRY_TOKEN` as an environment secret there (so it stays encrypted
until a reviewer approves the publish job).

## Manual fallback

```bash
# Bump every version to X.Y.Z (needs cargo-edit + node), then assert + preview:
bash scripts/release/set-version.sh 0.1.3
bash scripts/release/assert-version.sh 0.1.3
git-cliff --unreleased --tag v0.1.3 --strip all      # changelog preview

# Preflight tokens (these actually authenticate; dry-runs do not):
cargo owner --list -p cognee-models                  # crates.io
npm whoami --registry https://registry.npmjs.org     # npm

# Publish crates.io in dependency order, then tag (tag push drives capi/ts):
scripts/split/publish-order.sh | while read -r c; do cargo publish -p "$c"; done
git commit -am "chore: release v0.1.3" && git tag -a v0.1.3 -m "cognee-rust v0.1.3"
git push origin main v0.1.3
```

## Pre-flight checklist

1. `main` is green (`ci.yml`, `publish-dry-run.yml`, cross-SDK parity where applicable).
2. `scripts/check_all.sh` passes locally.
3. `CARGO_REGISTRY_TOKEN` owns (or can create) every `cognee-*` crate name.
4. The `release` environment exists with required reviewers.

## Post-release

1. Verify the GitHub Release has the C-API tarballs attached and the changelog body.
2. Verify installs: `npm install @cognee/cognee-ts@X.Y.Z` and a `maturin develop`
   smoke build for the Python binding.
3. Confirm the `cognee-*` crates are live on crates.io.

## Notes / tradeoffs

- **PR-time build verification is native, not full-matrix.** `ci.yml` builds the
  C library and Neon addon for the runner's platform on the release PR; the full
  5-platform C-API and 4-platform npm matrices run at publish (tag) time. To also
  cross-compile all platforms on the release PR, add a `release/*`-gated
  `pull_request` trigger to `capi-release.yml` / `ts-prebuild.yml` (build only,
  publish gated off) — heavier CI, stronger guarantee.
- **First release after this change (0.1.3):** only `v0.1.0` is tagged, so
  git-cliff's `--unreleased` span includes the never-tagged 0.1.1/0.1.2 commits.
  Review/trim the generated changelog section in the release PR before merging.
- **Merge this infrastructure to `main` via a normal PR before cutting a
  release.** `release-publish.yml` is triggered by `pull_request: closed`, which
  GitHub evaluates from the workflow file on the PR's *base* branch (`main`). If
  it were introduced by a release PR it could not trigger its own publish; once
  it is on `main` (via the setup PR), every subsequent release PR publishes
  correctly.
- **npm publish scope is not fully preflighted.** `npm whoami` proves the token
  authenticates, not that it can publish to `@cognee`. The npm publish itself
  runs in the tag-cascaded `ts-prebuild.yml` (idempotent, not environment-gated),
  so a scope failure there does not roll back crates.io — see recovery below.

## Recovery from a partial release

The publish order is: crates.io → tag push → (cascade) npm + C-API → GitHub
Release. If it fails partway:

- **crates.io loop failed midway** — for a transient failure, use **"Re-run
  failed jobs"** (reuses the same workflow; already-published crates are skipped
  and the tag is only pushed after all crates succeed). If the fix was a change
  to `release-publish.yml` itself, "Re-run" would reuse the *old* file — instead
  land the fix on `main` and re-trigger with
  `gh workflow run release-publish.yml -f version=X.Y.Z` (the `workflow_dispatch`
  path publishes from `main`, still behind the `release` approval gate).
- **Tag pushed but the npm / C-API cascade failed** (e.g. `ts-prebuild.yml` hit a
  transient npm error) — re-running `release-publish` will **not** re-fire the
  cascade (it skips the already-pushed tag). Instead re-run the failed workflow
  directly: `capi-release.yml` (`workflow_dispatch` with the tag) and/or
  `ts-prebuild.yml` (`workflow_dispatch`). Both are idempotent — they skip
  platforms/packages already published.
