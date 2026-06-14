# 24 — crates.io publishability

> Wave 5 · Priority P1 (should-fix) · Track B (crates.io only) · Release-blocking: B only ·
> Effort: weeks (fork strategy) · Depends on: [01 — Decisions (D5)](01-decisions.md),
> [22 — Workspace metadata + MSRV + CHANGELOG](22-workspace-metadata-msrv-changelog.md) ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) §2B (T2.7–T2.11)

[← Back to index](00-INDEX.md)

## Goal

Make the intended public crate set publishable to **crates.io**. Concretely: resolve the
git-dependency / `[patch.crates-io]` problem for the qdrant/lbug/litert forks (per **D5**:
make those backends optional and **off** in the published feature set), prune the dead `tar`
patch, add CI that keeps the three hand-maintained `[patch.crates-io]` sections in sync,
add `publish` guards, and establish a leaf-first publish order with a `cargo publish
--dry-run` per crate.

> **Track B only.** This task is in scope **only if [D1](01-decisions.md) ⊇ B** (crates.io
> is part of "release"). For a Track-A release (PyPI/npm/C/GitHub tag) it is **not** a gate —
> bindings ship the compiled artifact and never invoke `cargo publish`. Skip this entire
> task if D1 = A only.

## Background & why

### The core problem

`cargo publish` **refuses** any crate whose dependency graph contains a **git** or **path**
source, or that relies on `[patch.crates-io]` to redirect a dependency. crates.io requires
every dependency to resolve to a registry version, because a published crate must build for
anyone from crates.io alone — git URLs and local patches are not reproducible there.

The root [Cargo.toml](../../../Cargo.toml) currently pulls several backends from git and
patches three crates (verified 2026-06-14):

**Git `[workspace.dependencies]`** (Cargo.toml lines 58–129):
```toml
cognee-litert-lm = { git = "https://github.com/topoteretes/cognee-litert-lm.git" }
common  = { git = "https://github.com/qdrant/qdrant", package = "common",  tag = "v1.17.0" }
edge    = { git = "https://github.com/qdrant/qdrant", package = "edge",    default-features = false, tag = "v1.17.0" }
segment = { git = "https://github.com/qdrant/qdrant", package = "segment", default-features = false, tag = "v1.17.0" }
shard   = { git = "https://github.com/qdrant/qdrant", package = "shard",   tag = "v1.17.0" }
# lbug = "0.14"   ← this one IS a registry crate (Ladybug); check it resolves on crates.io
```

**`[patch.crates-io]`** (Cargo.toml lines 131–139):
```toml
tar   = { git = "https://github.com/qdrant/tar-rs", branch = "main" }          # suspected dead
tonic = { git = "https://github.com/qdrant/tonic", branch = "v0.11.0-qdrant" } # required by qdrant
hyper = { git = "https://github.com/qdrant/hyper", branch = "v0.14.26-qdrant" }# required by qdrant
```

So any crate that transitively depends on the qdrant forks or `cognee-litert-lm` (i.e. the
whole library through the `qdrant`/`android-litert` features) is **unpublishable** as-is.
`lbug` is a registry crate (`lbug = "0.14"`) — confirm it actually exists on crates.io at
that version; if it is a placeholder, the `ladybug` backend needs the same treatment.

### The three resolution options (T2.7)

| Option | What it means | Cost / risk |
|---|---|---|
| **(a) Publish the forks** | Push qdrant's `tonic`/`hyper`/`tar`-rs forks, the qdrant subcrates, and `cognee-litert-lm` to crates.io under your namespace | Highest: you become maintainer of forks of large crates (`tonic`, `hyper`); name squatting / licensing concerns; ongoing upkeep |
| **(b) Vendor them** | Copy the fork sources into the repo and depend by path | Bloats the repo & published tarball; still cannot publish path deps to crates.io (path deps are rejected too) → does **not** actually unblock publish |
| **(c) Make backends optional & OFF by default** *(D5 recommendation)* | Gate `qdrant`/`pgvector-via-qdrant`/`android-litert` behind features that are **not** in the published `default` set, and pull their git deps in **only** under those features | Lowest: the published library builds from crates.io with embedded-free backends (e.g. mock/pgvector/pg-graph). Embedded qdrant/lbug/litert remain available to source builders via opt-in features. |

**Per D5, implement option (c).** The published library is buildable from crates.io with
the heavy embedded backends disabled; users who want embedded qdrant/Ladybug/LiteRT build
from source with the feature on (where git deps are allowed). Option (b) is a trap: cargo
rejects **path** dependencies at publish too, so vendoring alone does not make it
publishable.

> This is the **largest structural item in the whole plan** (estimated weeks). It requires
> threading the qdrant/litert deps to be `optional = true` and only referenced under
> non-default features, and verifying the crate still builds and tests with those features
> off. Do not attempt it until D1 ⊇ B is confirmed and task 22 (metadata) is merged.

## Prerequisites — read first

```bash
git checkout -b task/24-cratesio-publishability

# Re-confirm the git deps + patch table (2026-06-14 positions — re-grep):
sed -n '53,139p' Cargo.toml                      # workspace.dependencies + patch.crates-io
grep -n 'git = ' Cargo.toml                      # every git source
sed -n '25,32p' capi/Cargo.toml                  # capi patch.crates-io
sed -n '81,88p' js/cognee-neon/Cargo.toml        # neon patch.crates-io

# Confirm D5 = "(c) optional + off" and D1 ⊇ B before doing any of this:
sed -n '/### D5/,/Decision:/p' docs/plans/release/01-decisions.md
sed -n '/### D1/,/Decision:/p' docs/plans/release/01-decisions.md

# lbug really on crates.io?
cargo search lbug | head

# Which feature set is published by default (task 22 / lib defaults):
sed -n '1,30p' crates/lib/Cargo.toml
```

Read [22-workspace-metadata-msrv-changelog.md](22-workspace-metadata-msrv-changelog.md)
first — every published manifest must already carry description/repository/etc. and
`publish = false` must be set on non-published crates. This task assumes that metadata is in
place; missing metadata is a separate publish blocker handled by 22.

## Files to change

| Path | Change |
|---|---|
| `Cargo.toml` | make qdrant/litert deps `optional`; keep them out of published `default`; prune dead `tar` patch |
| `crates/vector/Cargo.toml`, `crates/graph/Cargo.toml`, `crates/llm/Cargo.toml` | gate qdrant/lbug/litert behind off-by-default features (verify which crates actually consume them) |
| `crates/lib/Cargo.toml` | confirm published `default` excludes the git-backed backends and the `publish=false` `cognee-cloud` path (see §4 below) |
| `capi/Cargo.toml`, `js/cognee-neon/Cargo.toml` | prune the same dead `tar` patch (sync) |
| `.github/workflows/ci.yml` | add a job asserting the three `[patch.crates-io]` sections are identical |
| `crates/{cli,bench,cloud}/Cargo.toml`, `python/Cargo.toml`, etc. | confirm/add `publish = false` on every non-published crate |
| `docs/RELEASE.md` (from task 07/T6.4, if present) | document the leaf-first publish order |

## Implementation steps

### Step 1 — Make the git-backed backends optional and off in the published default (T2.7, D5)

This is the substantive work. The goal: a `cargo publish`-able crate graph whose **default**
features pull **zero** git/path dependencies.

1. **Inventory** which workspace crates depend on the git sources:
   ```bash
   grep -rln 'segment\|shard\|edge\|common\|qdrant\|cognee-litert-lm\|lbug' crates/*/Cargo.toml
   ```
   Expect the qdrant subcrates in `crates/vector` (the `qdrant` feature), `lbug` in
   `crates/graph` (the `ladybug` feature), and `cognee-litert-lm` in `crates/llm` (the
   `android-litert` feature). Confirm before editing.

2. In each consuming crate, ensure the git dep is declared `optional = true` and referenced
   **only** under its feature (`dep:` syntax). Example pattern in `crates/vector/Cargo.toml`:
   ```toml
   [dependencies]
   segment = { workspace = true, optional = true }
   shard   = { workspace = true, optional = true }
   edge    = { workspace = true, optional = true }
   common  = { workspace = true, optional = true }

   [features]
   qdrant = ["dep:segment", "dep:shard", "dep:edge", "dep:common"]
   ```
   (Do the analogous change for `lbug`/`ladybug` in `crates/graph` and
   `cognee-litert-lm`/`android-litert` in `crates/llm`.)

3. **Remove the git-backed backends from the published `default` feature set.** Today
   `crates/lib/Cargo.toml` `default` includes `qdrant` and `ladybug` (verified lines 7–28).
   For the published crate, the default must not pull git deps. Two viable shapes:
   - **A separate "publish" default**: keep the dev-friendly `default` (with embedded
     backends) for source builders, and document that crates.io consumers must build with
     `--no-default-features` + a registry-only backend set (e.g. `pgvector`, `pggraph`,
     `sqlite`). This is the lowest-churn option but relies on consumers opting out.
   - **Change `default` to a registry-only set** (e.g. `["sqlite", "pgvector", "pggraph",
     "hf-tokenizer", "tiktoken", ...]`) and make `qdrant`/`ladybug`/`android-litert`
     explicit opt-ins. This makes a bare `cargo add cognee-lib` publishable and buildable.
     **Prefer this** for a clean crates.io story, but it changes the default backend for all
     consumers — call it out in the CHANGELOG (task 22) and coordinate with D5.

   > `pgvector`/`pggraph` use Postgres (registry crates) — verify they have **no** transitive
   > git dep before relying on them as the publishable default. `cargo tree -e no-dev
   > --no-default-features -F sqlite,pgvector,pggraph` must show no git source.

4. Verify the publishable feature set has no git/path deps:
   ```bash
   # No git source anywhere in the publishable graph:
   cargo tree -e normal --no-default-features -F sqlite,pgvector,pggraph -p cognee-lib \
     | grep -iE 'git\+|/qdrant|litert|tar-rs' && echo "STILL HAS GIT DEPS" || echo "clean ✔"
   ```

### Step 2 — Prune the dead `tar` patch (T2.8)

The `tar` patch (Cargo.toml line 136, mirrored in capi line 28 and neon line 85) is
**suspected unused**. **Confirm before removing** — do not delete on faith:

```bash
# Cargo prints a warning for patches that don't apply to any resolved dependency:
cargo tree 2>&1 | grep -i 'patch.*was not used\|not used in the crate graph'
# or, more directly:
cargo update --dry-run 2>&1 | grep -i 'tar'
```

- If cargo reports the `tar` patch **was not used**, remove the `tar = { git = ... }` line
  from **all three** files:
  - `Cargo.toml` line 136
  - `capi/Cargo.toml` line 28
  - `js/cognee-neon/Cargo.toml` line 85
  Then re-run `cargo check` in each workspace to confirm nothing breaks.
- If cargo says it **is** used, leave it and document why in the patch-table comment.

> `tonic` and `hyper` patches are **required** by qdrant v1.17.0 (they add
> `http2_max_local_error_reset_streams` etc.). They only matter when the `qdrant` feature is
> on — which, after Step 1, is **not** the published default. They can stay in the manifest
> (they are inert when the feature is off) but the published crate must still not need them;
> Step 1's `cargo tree` check is what proves that.

### Step 3 — CI guard for `[patch.crates-io]` drift (T2.9)

The patch table is hand-copied across three workspaces (root, `capi/`, `js/cognee-neon/`).
The headers already warn "keep in sync" (verified in all three). Add a CI job that fails if
they diverge. Append to [.github/workflows/ci.yml](../../../.github/workflows/ci.yml) under
`jobs:` (model the runner setup on the lightweight `lint`/`msrv` jobs — this needs no Rust
toolchain, just a shell):

```yaml
  # ── Guard: the three hand-maintained [patch.crates-io] sections must match ──
  # (root Cargo.toml, capi/Cargo.toml, js/cognee-neon/Cargo.toml). See T2.9.
  patch-sync:
    name: patch.crates-io sync
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Compare [patch.crates-io] sections
        run: |
          extract() {
            # Print the [patch.crates-io] block (table header to next [section]),
            # dropping comments/blank lines so only the actual patch lines compare.
            awk '/^\[patch\.crates-io\]/{f=1;next} /^\[/{f=0} f' "$1" \
              | sed 's/#.*//' | grep -v '^[[:space:]]*$' | sort
          }
          extract Cargo.toml               > /tmp/patch_root.txt
          extract capi/Cargo.toml          > /tmp/patch_capi.txt
          extract js/cognee-neon/Cargo.toml> /tmp/patch_neon.txt
          if ! diff -u /tmp/patch_root.txt /tmp/patch_capi.txt; then
            echo "::error::capi/Cargo.toml [patch.crates-io] differs from root Cargo.toml"; exit 1
          fi
          if ! diff -u /tmp/patch_root.txt /tmp/patch_neon.txt; then
            echo "::error::js/cognee-neon/Cargo.toml [patch.crates-io] differs from root"; exit 1
          fi
          echo "patch.crates-io sections are in sync ✔"
```

> The `awk`/`sed`/`sort` normalization ignores comments and ordering so cosmetic header
> differences (the comment blocks differ between files) don't false-positive — only the
> actual `name = { git = ... }` lines are compared. Test it locally:
> `bash -c 'awk ... '` on each file before pushing.

### Step 4 — Publish guards + confirm `cognee-lib` publishability (T2.10)

1. **Set `publish = false` on every non-published crate.** Verified currently set on:
   `crates/bench`, `crates/cli`, `crates/cloud` (`grep -rln 'publish = false' crates/`).
   Add it to any other crate that should never hit crates.io:
   - `crates/test-utils` (test helpers/mocks) → `publish = false`
   - `crates/bindings-common`, `python/` → `publish = false` (consumed via FFI/PyO3, not crates.io)
   - `crates/http-server`, `crates/visualization`, `crates/observability`, `crates/telemetry`
     → decide per crate; if not part of the public registry set, mark `publish = false`.
   ```bash
   grep -rL 'publish = false' crates/*/Cargo.toml   # crates that WILL be published — review each
   ```

2. **Confirm `cognee-lib` itself is publishable in its published feature set.** It depends on
   `cognee-cloud` (a `publish = false` crate) **via the optional `cloud` feature**
   (verified `crates/lib/Cargo.toml` line 29: `cloud = ["dep:cognee-cloud"]`, line 104:
   `cognee-cloud = { path = "../cloud", optional = true }`). Today `cloud` is in `default`
   (line 24). **A published crate must not, in its default features, depend on a
   `publish = false` (path) crate** — cargo will reject it. So either:
   - Drop `cloud` (and `server`, `visualization` if those crates stay `publish = false`)
     from the **published** default feature set, **or**
   - Publish `cognee-cloud`/`cognee-http-server`/`cognee-visualization` too (give them real
     metadata + remove `publish = false`).
   D5's "optional + off" direction implies the former: keep `cloud`/`server`/`visualization`
   as opt-in features that are **not** in the crates.io default. Verify:
   ```bash
   cargo publish --dry-run -p cognee-lib --no-default-features -F sqlite,pgvector,pggraph 2>&1 | tail -30
   # expect: no "path dependency ... has no version" / "git source" errors.
   ```

### Step 5 — Leaf-first publish order + per-crate dry-run (T2.11)

crates.io requires each dependency to already be published before its dependents. Establish
a **leaf-first** order (no-deps crates first, `cognee-lib` last) and dry-run each. Derive the
real order from the dependency graph rather than guessing:

```bash
# Topological-ish ordering of workspace members (leaves first):
cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import sys,json; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]'
```

Approximate leaf-first order (verify against `cargo tree`; publish=false crates skipped):

1. `cognee-utils`, `cognee-models` (no internal deps)
2. `cognee-storage`, `cognee-database`, `cognee-logging`
3. `cognee-llm`, `cognee-embedding`, `cognee-graph`, `cognee-vector`, `cognee-ontology`
4. `cognee-chunking`, `cognee-ingestion`, `cognee-core`, `cognee-session`
5. `cognee-cognify`, `cognee-search`, `cognee-delete`
6. (`cognee-observability`, `cognee-telemetry` — if published)
7. `cognee-lib` (last)

Dry-run each, leaf-first:
```bash
for c in cognee-utils cognee-models cognee-storage cognee-database cognee-logging \
         cognee-llm cognee-embedding cognee-graph cognee-vector cognee-ontology \
         cognee-chunking cognee-ingestion cognee-core cognee-session \
         cognee-cognify cognee-search cognee-delete cognee-lib; do
  echo "=== $c ==="
  cargo publish --dry-run -p "$c" --no-default-features -F sqlite 2>&1 | tail -5
done
```
Adjust the `-F` set per crate (a crate without a `sqlite` feature will error on the flag —
use that crate's actual publishable feature set, or no `-F` for featureless crates).

Record the final order in `docs/RELEASE.md` (created by task 07 / T6.4). The real `cargo
publish` (no `--dry-run`) is a separate, gated step done at tag time, leaf-first, waiting
for each crate to index on crates.io before the next.

## Verification

```bash
# 1. Publishable graph has zero git/path deps in the published feature set:
cargo tree -e normal --no-default-features -F sqlite,pgvector,pggraph -p cognee-lib \
  | grep -iE 'git\+|/qdrant|litert|tar-rs' && echo "FAIL: git deps remain" || echo "clean ✔"

# 2. Dead tar patch removed from all three workspaces (if confirmed unused):
grep -rn 'tar = { git' Cargo.toml capi/Cargo.toml js/cognee-neon/Cargo.toml   # expect: no output

# 3. The three [patch.crates-io] sections match (same logic as the CI job):
for f in Cargo.toml capi/Cargo.toml js/cognee-neon/Cargo.toml; do
  awk '/^\[patch\.crates-io\]/{f=1;next} /^\[/{f=0} f' "$f" | sed 's/#.*//' | grep -v '^[[:space:]]*$' | sort
  echo "--- ($f) ---"
done   # the patch lines (between the dividers) must be identical across files

# 4. publish guards in place; published crates reviewed:
grep -rln 'publish = false' crates/*/Cargo.toml python/Cargo.toml

# 5. Per-crate dry-run is green for the public set (no version/git/path errors):
cargo publish --dry-run -p cognee-models 2>&1 | tail -5
cargo publish --dry-run -p cognee-lib --no-default-features -F sqlite,pgvector,pggraph 2>&1 | tail -10

# 6. CI workflow valid YAML with the new patch-sync job:
python3 -c 'import yaml,sys; d=yaml.safe_load(open(".github/workflows/ci.yml")); \
  assert "patch-sync" in d["jobs"], "patch-sync job missing"; print("ci.yml OK; patch-sync present")'

# 7. The default (source) build still works for source consumers:
cargo check --all-targets   # embedded backends still build from git via the opt-in features
```

Expected:
- #1 prints `clean ✔`; #2 prints nothing; #5 dry-runs show no `has no version` / `git source`
  / `path dependency` errors; #6 confirms the CI job; #7 still compiles the full featured
  workspace from source.

## Acceptance criteria

- [ ] Per **D5**, qdrant/lbug/litert backends are `optional` and **excluded** from the
      crates.io-published default feature set; the published `cognee-lib` graph has **no**
      git or path dependencies.
- [ ] Dead `tar` patch confirmed unused (`cargo tree` warning) and removed from `Cargo.toml`,
      `capi/Cargo.toml`, and `js/cognee-neon/Cargo.toml` (or kept with a documented reason if used).
- [ ] CI `patch-sync` job fails when the three `[patch.crates-io]` sections diverge.
- [ ] `publish = false` set on every non-published crate; `cognee-lib`'s published default
      does **not** depend on the `publish=false` `cognee-cloud` (and `http-server`/`visualization`).
- [ ] A documented leaf-first publish order exists (in `docs/RELEASE.md`), and
      `cargo publish --dry-run -p <crate>` is green for each crate in that order.
- [ ] `cargo check --all-targets` (full source build) still passes.

## Gotchas / do-not

- **Track B only.** Skip entirely if D1 = A only. Do not block a Track-A release on this.
- **Vendoring (option b) does NOT unblock publish** — cargo rejects **path** deps at publish
  the same as git deps. Only registry versions (option c, or actually publishing the forks)
  work.
- **Confirm `tar` is unused before deleting it.** Removing a live patch silently changes the
  resolved `tar` version and can break the qdrant build. Gate the deletion on the
  `cargo tree ... was not used` warning.
- **Keep `tonic`/`hyper` patches** — they are required by qdrant v1.17.0. They are inert when
  the `qdrant` feature is off, so they can remain in the manifest; the publishable-graph
  `cargo tree` check (Verification #1) is what proves the published crate doesn't need them.
- **Edit the patch table in all three files together** — the CI `patch-sync` job will fail if
  you change one and forget the others. That is the point.
- **`lbug` may or may not be a real crates.io crate.** `lbug = "0.14"` looks like a registry
  dep, but verify with `cargo search lbug`. If it is unpublished/placeholder, the `ladybug`
  backend needs the same off-by-default treatment as qdrant.
- **Changing `cognee-lib`'s `default` features changes the default backend for every
  consumer.** If you go that route (Step 1 option), document it loudly in the CHANGELOG
  (task 22) — it is a behavior change for source consumers, even though parity (IDs/schema/
  hashes) is untouched.
- **This is a weeks-scale effort**, dominated by Step 1. Do not start until D1 ⊇ B is
  confirmed and [task 22](22-workspace-metadata-msrv-changelog.md) (metadata) has landed —
  missing metadata is its own publish blocker and is handled there, not here.
- **Parity-neutral:** no schema, IDs, hashes, prompts, or collection names change. Feature
  gating and manifest edits only.

## Rollback

All changes are manifest edits (feature gating, patch removal, `publish` flags), one CI job,
and a doc. Revert with `git checkout -- Cargo.toml capi/Cargo.toml js/cognee-neon/Cargo.toml
crates/ python/ .github/workflows/ci.yml docs/RELEASE.md`. If a published crate later needs
to be yanked, use `cargo yank --vers <v> <crate>` (crates.io is append-only; you cannot
delete a published version). No data/schema impact.
