# 02 — Licensing & legal

> Wave 1 · Priority P0 (blocker) · Track A+B · Release-blocking: yes · Effort: 0.5d ·
> Depends on: [01 — Release decisions (D2)](01-decisions.md) ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) §3 B1 (B1.1–B1.4)

[← Back to index](00-INDEX.md)

## Goal

Every shippable artifact (Rust crates, the PyPI wheel, the npm package, the C library
distribution) declares a license, and the matching `LICENSE` file(s) exist at the repo
root. After this task `cargo publish --dry-run` no longer errors on a missing license,
and a valid SPDX identifier is present in every published manifest.

## Background & why

The repo currently ships **no `LICENSE` file** and **no `license` field anywhere**
(verified: `ls` at repo root shows no `LICENSE*`/`COPYING*`; `Cargo.toml`
`[workspace.package]` has only `edition` + `version`; `python/pyproject.toml` and
`js/package.json` have no `license` key). That is a hard release blocker for all
channels — crates.io refuses to publish, and PyPI/npm/redistributed C artifacts must
carry a license to be lawfully distributable.

This task only adds metadata + license text; it changes **no runtime behavior** and is
parity-neutral.

## Prerequisites — read first

```bash
git checkout -b task/02-licensing
```

- This task is **gated on decision D2** in [01-decisions.md](01-decisions.md). Do **not**
  guess the license. Read the D2 box and use the recorded choice.
- D2's recommendation is **"match the Python cognee license."** Python `cognee`
  (github.com/topoteretes/cognee) is licensed **Apache-2.0**. Confirm before writing:

  ```bash
  # If you have a clone:
  cat /tmp/cognee-python/LICENSE 2>/dev/null | head -3
  # Otherwise open https://github.com/topoteretes/cognee/blob/main/LICENSE
  ```

- Files you will touch (all verified to exist):
  - [Cargo.toml](../../../Cargo.toml) — `[workspace.package]` at lines 49–51.
  - 27 crate manifests under `crates/*/Cargo.toml` (sample: [crates/models/Cargo.toml](../../../crates/models/Cargo.toml)).
  - [python/pyproject.toml](../../../python/pyproject.toml) — `[project]` at lines 5–8.
  - [js/package.json](../../../js/package.json) — top-level object.
  - [capi/Cargo.toml](../../../capi/Cargo.toml) `[workspace.package]` (lines 12–14) + [capi/cognee-capi/Cargo.toml](../../../capi/cognee-capi/Cargo.toml) `[package]`.
  - [js/cognee-neon/Cargo.toml](../../../js/cognee-neon/Cargo.toml) `[package]` (standalone crate, no workspace inheritance).

> **The steps below assume D2 = `Apache-2.0`** (the recommendation). If D2 resolves to a
> different value, substitute the SPDX string everywhere and add the corresponding
> `LICENSE` file(s) — see the "Alternate licenses" subsection at the end.

## Files to change

| Path | Change |
|---|---|
| `LICENSE` (new, repo root) | Apache-2.0 full text |
| `Cargo.toml` | add `license = "Apache-2.0"` to `[workspace.package]` |
| `crates/*/Cargo.toml` (×27), `python/Cargo.toml`, `e2e-cross-sdk/telemetry-emit/Cargo.toml`, `examples/Cargo.toml` | add `license.workspace = true` under `[package]` |
| `capi/Cargo.toml` | add `license = "Apache-2.0"` to its `[workspace.package]` |
| `capi/cognee-capi/Cargo.toml` | add `license.workspace = true` under `[package]` |
| `js/cognee-neon/Cargo.toml` | add `license = "Apache-2.0"` under `[package]` (standalone — no workspace) |
| `python/pyproject.toml` | add `license` to `[project]` |
| `js/package.json` | add `"license": "Apache-2.0"` |

## Implementation steps

### Step 1 — Add the root `LICENSE` file

1. Create `/Users/dmytro/dev/cognee/cognee-rust/LICENSE` containing the **verbatim
   Apache License 2.0 text** (the standard ~202-line text from
   https://www.apache.org/licenses/LICENSE-2.0.txt). Do not paraphrase.
2. Fill the copyright line in the appendix block at the bottom:
   `Copyright 2026 Topoteretes / cognee contributors`.

> Tip: copy the exact text Python cognee ships in its own `LICENSE` so the two projects
> are byte-identical where the law allows.

### Step 2 — Add `license` to the root `[workspace.package]`

Open [Cargo.toml](../../../Cargo.toml). Current (lines 49–51):

```toml
[workspace.package]
edition = "2024"
version = "0.1.0"
```

Change to:

```toml
[workspace.package]
edition = "2024"
version = "0.1.0"
license = "Apache-2.0"
```

> Note: task [22](22-workspace-metadata-msrv-changelog.md) adds the remaining metadata
> fields (`description`, `repository`, etc.) to this same table. Only add `license` here;
> leave the rest for task 22 to avoid merge churn.

### Step 3 — Inherit `license` in every workspace crate

For **each** of the 27 manifests under `crates/*/Cargo.toml`, add
`license.workspace = true` to the `[package]` table. Example for
[crates/models/Cargo.toml](../../../crates/models/Cargo.toml):

Before:
```toml
[package]
name = "cognee-models"
version.workspace = true
edition.workspace = true
```

After:
```toml
[package]
name = "cognee-models"
version.workspace = true
edition.workspace = true
license.workspace = true
```

Do this for all 27. A scripted edit is acceptable, but **verify each diff** — some
manifests have extra `[package]` keys (e.g. `publish = false` in `crates/cli/Cargo.toml`,
`crates/cloud/Cargo.toml`); insert `license.workspace = true` alongside, not replacing
them. The full member list is in `Cargo.toml` lines 5–46. Confirm the count:

```bash
ls crates/*/Cargo.toml | wc -l   # expect 27
```

> `python/Cargo.toml`, `e2e-cross-sdk/telemetry-emit/Cargo.toml`, and `examples/Cargo.toml`
> are also workspace members (see `Cargo.toml` members list). Add `license.workspace = true`
> to those too — all three have a `[package]` table (verified). Check:
> ```bash
> grep -l '^\[package\]' python/Cargo.toml e2e-cross-sdk/telemetry-emit/Cargo.toml examples/Cargo.toml
> ```
> (`python/`, `e2e-cross-sdk/telemetry-emit/`, and `examples/` all have `publish = false`;
> a license is still good hygiene and quiets `cargo metadata` license checks.)

### Step 4 — The C API workspace (separate workspace)

The `capi/` directory is its **own** workspace (root `Cargo.toml` comment lines 1–3;
`capi/Cargo.toml` `[workspace]`). It does not inherit from the root.

1. Open [capi/Cargo.toml](../../../capi/Cargo.toml). Add `license` to its
   `[workspace.package]` (lines 12–14):

   Before:
   ```toml
   [workspace.package]
   edition = "2024"
   version = "0.1.0"
   ```
   After:
   ```toml
   [workspace.package]
   edition = "2024"
   version = "0.1.0"
   license = "Apache-2.0"
   ```

2. Open [capi/cognee-capi/Cargo.toml](../../../capi/cognee-capi/Cargo.toml). Add
   `license.workspace = true` to `[package]`:

   Before:
   ```toml
   [package]
   name = "cognee-capi"
   version.workspace = true
   edition.workspace = true
   ```
   After:
   ```toml
   [package]
   name = "cognee-capi"
   version.workspace = true
   edition.workspace = true
   license.workspace = true
   ```

3. **C distribution:** the C API ships headers + a built `.so`/`.a`, not a manifest the
   consumer reads. Place a copy of the license alongside the distributed artifacts so the
   tarball is self-describing:

   ```bash
   cp LICENSE capi/LICENSE
   ```

   If `capi/scripts/check.sh` or a packaging script assembles a dist directory, ensure it
   includes `capi/LICENSE` (grep the scripts; if there is a dedicated packaging step, add
   the copy there instead of committing `capi/LICENSE`).

### Step 5 — The Neon (JS) Rust crate (standalone)

[js/cognee-neon/Cargo.toml](../../../js/cognee-neon/Cargo.toml) is a **standalone crate**
(`[workspace]` empty table on its own — verified lines 1–6). It cannot use
`license.workspace = true`. Add the literal:

Before:
```toml
[package]
name = "cognee-neon"
version = "0.1.0"
edition = "2024"
```
After:
```toml
[package]
name = "cognee-neon"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"
```

### Step 6 — PyPI manifest

Open [python/pyproject.toml](../../../python/pyproject.toml). Current `[project]`:

```toml
[project]
name = "cognee-pipeline"
requires-python = ">=3.9"
version = "0.1.0"
```

Add the license. Use the **SPDX-string form** (PEP 639, supported by maturin ≥1.5):

```toml
[project]
name = "cognee-pipeline"
requires-python = ">=3.9"
version = "0.1.0"
license = "Apache-2.0"
license-files = ["LICENSE"]
```

> `license-files` is repo-root-relative. `maturin` includes them in the wheel.
> If `maturin --version` in CI is < 1.5 (it may reject the SPDX string + `license-files`),
> fall back to the classic table form and a classifier instead:
> ```toml
> license = { text = "Apache-2.0" }
> ```
> Task [22](22-workspace-metadata-msrv-changelog.md) adds the `classifiers` list, which
> should include `"License :: OSI Approved :: Apache Software License"`. Coordinate so the
> two tasks do not contradict each other — prefer the SPDX-string form and let 22 add the
> matching classifier.

Copy the license next to the python package source so it ships in the sdist/wheel if
`license-files` is not honored by the installed maturin:

```bash
cp LICENSE python/LICENSE
```

### Step 7 — npm manifest

Open [js/package.json](../../../js/package.json). Add a `"license"` key (top level, e.g.
after `"description"`):

```json
{
  "name": "cognee",
  "version": "0.1.0",
  "description": "Node.js bindings for the cognee AI-memory SDK",
  "license": "Apache-2.0",
  ...
}
```

Add `LICENSE` to the `files` allowlist (currently lines 46–50) so it ships in the package:

```json
  "files": [
    "lib/",
    "cognee_neon.node",
    "LICENSE"
  ]
```

```bash
cp LICENSE js/LICENSE
```

## Verification

```bash
# 1. License field parses everywhere (workspace).
cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import sys,json; pkgs=json.load(sys.stdin)["packages"]; \
    bad=[p["name"] for p in pkgs if not p.get("license") and not p.get("license_file")]; \
    print("MISSING LICENSE:", bad); sys.exit(1 if bad else 0)'
# expect: MISSING LICENSE: []   (publish=false crates may legitimately remain;
#         confirm any printed name is intentionally unpublished)

# 2. capi workspace parses.
cargo metadata --no-deps --manifest-path capi/Cargo.toml >/dev/null && echo "capi OK"

# 3. neon crate parses.
cargo metadata --no-deps --manifest-path js/cognee-neon/Cargo.toml >/dev/null && echo "neon OK"

# 4. Manifests are still valid (cheap compile gate).
cargo check --all-targets

# 5. Dry-run publish on a leaf crate no longer complains about license
#    (it will still fail on git deps for non-leaf crates — that is task 24, Track B).
cargo publish --dry-run -p cognee-models 2>&1 | grep -i license || echo "no license error ✔"

# 6. PyPI + npm manifests are valid JSON/TOML.
python3 -c 'import tomllib;tomllib.load(open("python/pyproject.toml","rb"));print("pyproject OK")'
node -e 'JSON.parse(require("fs").readFileSync("js/package.json"));console.log("package.json OK")'
```

### New test / check to add

No source test. Add a one-line **CI guard** so a future crate added without a license is
caught — fold this into the metadata work in task 22 if convenient, otherwise add a step
to the `lint` job in [.github/workflows/ci.yml](../../../.github/workflows/ci.yml):

```yaml
      - name: License presence (every published crate)
        run: |
          cargo metadata --no-deps --format-version 1 \
          | python3 -c 'import sys,json;\
            p=json.load(sys.stdin)["packages"];\
            bad=[x["name"] for x in p if not x.get("license") and not x.get("license_file")];\
            print("missing license:",bad); sys.exit(1 if bad else 0)'
```

## Acceptance criteria

- [ ] `LICENSE` (Apache-2.0 full text) exists at repo root with a filled copyright line.
- [ ] `Cargo.toml` `[workspace.package]` has `license = "Apache-2.0"`.
- [ ] All 27 `crates/*/Cargo.toml` plus `python/Cargo.toml`, `e2e-cross-sdk/telemetry-emit/Cargo.toml`,
      and `examples/Cargo.toml` carry `license.workspace = true` (all confirmed to have `[package]`).
- [ ] `capi/Cargo.toml` workspace has `license`; `capi/cognee-capi/Cargo.toml` inherits.
- [ ] `js/cognee-neon/Cargo.toml` has a literal `license = "Apache-2.0"`.
- [ ] `python/pyproject.toml` declares the license; `js/package.json` has `"license"`
      and `LICENSE` in `files`.
- [ ] License copy ships with C / Python / JS distributions.
- [ ] `cargo metadata` shows no published crate missing a license.
- [ ] `cargo check --all-targets` passes; the chosen SPDX string matches D2.

## Gotchas / do-not

- **Do not invent the license.** It is gated on D2. If D2 ≠ Apache-2.0, this whole doc's
  SPDX string changes (see below).
- **`capi/` and `js/cognee-neon/` are separate workspaces** — they will NOT pick up the
  root `license.workspace`. They must be edited independently (Steps 4 & 5). Forgetting
  this is the most common miss.
- **SPDX syntax matters.** Dual-licensing is written `MIT OR Apache-2.0` (uppercase
  `OR`, with a space). A bad expression makes `cargo metadata`/`cargo publish` reject it.
- **PEP 639 vs maturin version:** the SPDX-string `license = "..."` form needs maturin
  ≥ 1.5. If CI's maturin is older, use `license = { text = "Apache-2.0" }`. Verify with
  `maturin --version` in the python-check CI job before relying on the new form.
- **Coordinate with task 22.** Task 22 adds the remaining `[workspace.package]` fields and
  the python `classifiers`. Add only `license` here; do not pre-empt 22's fields.
- This task touches no `.rs`, no schema, no IDs — **zero parity risk**. Keep it that way.

### Alternate licenses (if D2 ≠ Apache-2.0)

| D2 value | SPDX string | Root LICENSE file(s) |
|---|---|---|
| MIT | `MIT` | `LICENSE` (MIT text) |
| Apache-2.0 | `Apache-2.0` | `LICENSE` (Apache text) |
| Dual | `MIT OR Apache-2.0` | `LICENSE-MIT` **and** `LICENSE-APACHE` |
| Proprietary | `LicenseRef-Proprietary` + `license-file = "LICENSE"` | `LICENSE` (your terms); set `publish = false` everywhere |

For the dual case, use `license = "MIT OR Apache-2.0"` and put both files at root; the
`files`/`license-files` allowlists must list both.

## Rollback

Pure additive metadata. To revert: `git checkout -- $(git diff --name-only)` and
`rm -f LICENSE capi/LICENSE python/LICENSE js/LICENSE`. No data or schema implications.
