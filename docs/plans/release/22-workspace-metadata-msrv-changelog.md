# 22 — Workspace metadata + MSRV + CHANGELOG

> Wave 5 · Priority P0 (blocker) · Track A+B · Release-blocking: yes · Effort: 0.5d ·
> Depends on: [02 — Licensing](02-licensing.md), [11 — Collapse DB migrations](11-collapse-db-migrations.md) ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) §4 2A (T2.1–T2.6)

[← Back to index](00-INDEX.md)

## Goal

Every shippable manifest carries complete publishing metadata (description, repository,
homepage, readme, keywords, categories, authors), the workspace declares an MSRV
(`rust-version = "1.85"`), CI verifies that MSRV, a `rust-toolchain.toml` pins a floor,
and a root `CHANGELOG.md` (Keep a Changelog format) documents `0.1.0`. The Python and JS
binding manifests are expanded to match.

## Background & why

The workspace `[workspace.package]` currently has only `edition` + `version`
(verified: [Cargo.toml](../../../Cargo.toml) lines 49–51). For a credible release every
published manifest needs description/repository/etc., an MSRV must be declared (edition
2024 + `resolver = "3"` require Rust ≥ 1.85 — verified `Cargo.toml` lines 47, 50), and
there is **no `CHANGELOG.md` and no `rust-toolchain.toml`** at root (verified by `ls`).
CI uses `dtolnay/rust-toolchain@stable` with **no floor** (verified
[.github/workflows/ci.yml](../../../.github/workflows/ci.yml) lines 43–44, 169, 328…), so
an accidental use of a newer-than-1.85 API would not be caught.

This task is **additive metadata + one CI lane + two docs**. It changes no runtime
behavior and is parity-neutral.

**Why it depends on 02 and 11:**
- **02 (license):** this task adds the *rest* of `[workspace.package]`; the `license`
  field is added by task 02. Land 02 first so both edits touch the same table without
  conflict. If 02 is not yet merged, also add `license` here (see Gotchas).
- **11 (migrations):** the CHANGELOG's "Database" notes should describe the **final**
  single-baseline migration state, not the pre-squash 14+3 chain. Backfill the changelog
  after 11 settles so the 0.1.0 notes match what actually ships.

## Prerequisites — read first

```bash
git checkout -b task/22-workspace-metadata-msrv-changelog

# Confirm current state (re-grep — these are the 2026-06-14 positions):
sed -n '49,51p' Cargo.toml                          # [workspace.package] edition+version (+license after task 02)
ls rust-toolchain.toml CHANGELOG.md 2>&1            # expect: both "No such file"
grep -n 'rust-toolchain@stable' .github/workflows/ci.yml   # the toolchain pins to update
sed -n '1,14p' python/pyproject.toml               # python [project]
sed -n '1,31p' js/package.json                     # js manifest
```

Decide the canonical repository URL up-front (used in every manifest). The Python project
lives at `https://github.com/topoteretes/cognee`; this Rust port's repo URL must be
confirmed with the maintainer. **Use the actual cognee-rust GitHub URL** — placeholder
below is `https://github.com/topoteretes/cognee-rust`; replace if different.

## Files to change

| Path | Change |
|---|---|
| `Cargo.toml` | expand `[workspace.package]`; add `rust-version = "1.85"` |
| `crates/*/Cargo.toml` (×27) + `python/Cargo.toml` | inherit new fields via `.workspace = true` |
| `capi/Cargo.toml` + `capi/cognee-capi/Cargo.toml` | mirror metadata (separate workspace) |
| `js/cognee-neon/Cargo.toml` | add literal metadata (standalone crate) |
| `rust-toolchain.toml` (new, root) | pin `channel = "1.85"` |
| `.github/workflows/ci.yml` | add an MSRV check lane |
| `CHANGELOG.md` (new, root) | Keep a Changelog; backfill `0.1.0` |
| `python/pyproject.toml` | description, authors, repository, keywords, classifiers |
| `js/package.json` | verify/add `repository`, `keywords`, `files` |

## Implementation steps

### Step 1 — Expand the workspace `[workspace.package]`

Open [Cargo.toml](../../../Cargo.toml). Current (lines 49–51, plus `license` if task 02
already merged):

```toml
[workspace.package]
edition = "2024"
version = "0.1.0"
license = "Apache-2.0"   # added by task 02
```

Expand to:

```toml
[workspace.package]
edition = "2024"
version = "0.1.0"
rust-version = "1.85"
license = "Apache-2.0"
authors = ["Topoteretes <support@cognee.ai>"]
description = "Rust port of cognee — an AI memory pipeline that turns raw data into queryable knowledge graphs."
repository = "https://github.com/topoteretes/cognee-rust"
homepage = "https://www.cognee.ai"
readme = "README.md"
keywords = ["ai", "knowledge-graph", "memory", "rag", "embeddings"]
categories = ["science", "database", "text-processing"]
```

Notes / constraints (crates.io rules, enforced by `cargo publish`):
- `keywords`: **max 5**, each ≤ 20 chars, lowercase + hyphens only.
- `categories`: must be valid crates.io **slugs** — `science`, `database`,
  `text-processing` are valid. Verify any change against
  https://crates.io/category_slugs.
- `description`: keep ≤ ~300 chars, no trailing newline.
- Replace the `repository`/`homepage`/`authors` with the real values for this project.

### Step 2 — Inherit the new fields in every workspace crate

For **each** `crates/*/Cargo.toml` (×27) and `python/Cargo.toml`, add the inherited keys
to `[package]`. Task 02 already added `license.workspace = true`; append the rest:

```toml
[package]
name = "cognee-models"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
keywords.workspace = true
categories.workspace = true
authors.workspace = true
```

Practical guidance:
- A per-crate generic `description.workspace = true` is acceptable for 0.1.0 (all crates
  share the workspace description). If you want crate-specific descriptions later, set a
  literal `description = "..."` on the crate instead — but that is **not** required now.
- `readme.workspace = true` points every crate at the **root** `README.md`. If a crate has
  its own README and you prefer it, set `readme = "README.md"` (crate-relative) literally
  on that crate instead. For 0.1.0, inheriting the root README is fine.
- **`publish = false` crates** (e.g. `crates/cli/Cargo.toml`, `crates/cloud/Cargo.toml`,
  `python/Cargo.toml`) still benefit from the metadata but are not validated by
  `cargo publish`; add the inherited fields anyway for consistency. Keep their existing
  `publish = false`.

Verify all members updated:
```bash
ls crates/*/Cargo.toml | wc -l    # 27
grep -L 'rust-version.workspace' crates/*/Cargo.toml   # expect: no output (all matched)
```

### Step 3 — The C API workspace (separate)

`capi/` is its own workspace (verified `capi/Cargo.toml` `[workspace]`, lines 8–10). It
does **not** inherit root fields.

1. [capi/Cargo.toml](../../../capi/Cargo.toml) `[workspace.package]` (lines 12–14) — mirror
   the same fields as Step 1 (`rust-version`, `description`, `repository`, `homepage`,
   `readme`, `keywords`, `categories`, `authors`, and `license` from task 02). The C crate
   is `publish = false` in practice; still keep metadata consistent.
2. [capi/cognee-capi/Cargo.toml](../../../capi/cognee-capi/Cargo.toml) `[package]` — add the
   `*.workspace = true` inheritance lines as in Step 2.

> If `capi/Cargo.toml` `[workspace.package]` lacks `readme`, set `readme = "README.md"`
> pointing at a capi-local README, or drop `readme.workspace` in the capi crate. Do not
> reference the root README from the capi workspace (different root dir).

### Step 4 — The Neon crate (standalone)

[js/cognee-neon/Cargo.toml](../../../js/cognee-neon/Cargo.toml) is standalone (empty
`[workspace]`, lines 5–6). Add **literal** metadata to `[package]`:

```toml
[package]
name = "cognee-neon"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"
description = "Neon (Node.js) native binding for the cognee Rust SDK."
repository = "https://github.com/topoteretes/cognee-rust"
publish = false
```

(`publish = false` because it is consumed via npm, not crates.io.)

### Step 5 — Add `rust-toolchain.toml`

Create `/Users/dmytro/dev/cognee/cognee-rust/rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.85"
components = ["rustfmt", "clippy"]
```

This pins local + CI builds to the MSRV floor by default. Contributors get a consistent
toolchain; combined with the MSRV CI lane (Step 6) it makes the declared
`rust-version = "1.85"` real.

> **Interaction with CI:** the existing CI jobs use `dtolnay/rust-toolchain@stable`, which
> **overrides** `rust-toolchain.toml` (it installs and selects stable explicitly). That is
> fine — keep the main lanes on stable for full coverage, and add a dedicated 1.85 lane
> (Step 6) for the floor. Do **not** switch the whole CI to 1.85; you want both "MSRV
> builds" and "stable builds" signals.

### Step 6 — Add an MSRV CI lane

Open [.github/workflows/ci.yml](../../../.github/workflows/ci.yml). Add a new job after the
`lint` job (it is the lightest gate — `cargo check`, no tests, no network LLM). Model it on
the existing `lint` job's setup steps (mold, protoc, free-disk, ccache, rust-cache, ORT
cache) so it benefits from the same caching. Insert under `jobs:` (e.g. after the `lint`
block, before `test`):

```yaml
  # ── MSRV floor: build against the declared rust-version (T2.3) ─────────
  # The other lanes use @stable (which overrides rust-toolchain.toml); this
  # lane pins 1.85 so a newer-than-MSRV API is caught before release.
  msrv:
    name: MSRV (1.85)
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust 1.85
        uses: dtolnay/rust-toolchain@1.85.0

      - name: Install mold linker
        uses: rui314/setup-mold@v1
        with:
          make-default: true

      - name: Install protoc
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler

      - name: Free disk space
        uses: jlumbroso/free-disk-space@main
        with:
          tool-cache: false
          android: true
          dotnet: true
          haskell: true
          large-packages: false
          swap-storage: false
          docker-images: true

      - name: ccache (lbug bundled C++, restore-only)
        uses: hendrikmuhs/ccache-action@v1.2
        with:
          key: lbug-cxx
          max-size: 1G
          save: false

      - name: Cache cargo/target (msrv)
        uses: Swatinem/rust-cache@v2
        with:
          shared-key: workspace-msrv-v1
          cache-on-failure: true

      - name: Cache ORT binary
        uses: actions/cache@v4
        with:
          path: target/ort-cache
          key: ort-v2.0.0-rc.12-cpu-linux-x86_64

      - name: Check (MSRV)
        run: cargo +1.85.0 check --all-targets
```

Notes:
- Use the **separate Swatinem key** `workspace-msrv-v1` so the 1.85 artifacts don't poison
  the stable lanes' caches (mirrors the existing lint/test key separation rationale, ci.yml
  lines 86–96).
- `cargo check` (not `clippy`/`test`) keeps this lane fast and avoids the OpenAI secret.
- If the workspace fails to build on 1.85, **bump `rust-version`** (and this lane) to the
  lowest version that builds rather than working around it — the declared MSRV must be the
  real floor. Edition 2024 + resolver 3 guarantee ≥ 1.85; a dependency may push it higher.

### Step 7 — Add the root `CHANGELOG.md`

Create `/Users/dmytro/dev/cognee/cognee-rust/CHANGELOG.md` using
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. Backfill 0.1.0 from the
implemented-features list (source the "Implemented" section of `.claude/CLAUDE.md` and
`git log` for the headline items). Skeleton:

```markdown
# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-XX

Initial public release. A Rust port of the Python
[cognee](https://github.com/topoteretes/cognee) AI-memory pipeline, aiming for
drop-in cross-SDK compatibility.

### Added
- Full `add → cognify → search` pipeline (Python-compatible content hashing,
  deterministic UUID5 IDs, `text_<md5>.txt` naming, `file://` URIs).
- SQLite metadata DB (SeaORM) with a single baseline migration per chain.   <!-- see task 11 -->
- Text chunking (word → sentence → paragraph) with pluggable token counters
  (WordCounter, HuggingFace, tiktoken).
- Cognify knowledge-graph extraction (classify → chunk → extract → summarize →
  index → DLT FK edges); memify graph-enrichment pipeline.
- 15 search types (GraphCompletion default, RagCompletion, Chunks, Summaries,
  Temporal, Cypher, TripletCompletion, …).
- Multi-provider embeddings (ONNX/BGE-Small, OpenAI-compatible, Ollama, Mock).
- LLM abstraction (OpenAI-compatible adapter; LiteRT for Android).
- Embedded graph (Ladybug) and vector (Qdrant) backends.
- Sessions, ontology resolution, cascading deletion, cloud serve/disconnect.
- HTTP server (axum) mirroring the Python FastAPI surface under `/api/v1/*`.
- Knowledge-graph visualization (self-contained d3.js HTML).
- Observability (OpenTelemetry/OTLP) and opt-out product telemetry.
- Language bindings: C API, Python (PyO3), JavaScript (Neon), Android runner.

### Notes
- MSRV: Rust 1.85 (edition 2024 + resolver 3).
- Known gaps tracked for follow-up: S3 input, full `unstructured` office-format
  extraction, crates.io publishability (Track B).

[Unreleased]: https://github.com/topoteretes/cognee-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/topoteretes/cognee-rust/releases/tag/v0.1.0
```

> Backfill the bullets against the real feature set; do **not** claim items the audit lists
> as gaps. Re-check task 11's outcome for the exact "single baseline migration" wording.

### Step 8 — Expand `python/pyproject.toml`

Open [python/pyproject.toml](../../../python/pyproject.toml). Current `[project]` (lines
5–8, plus `license` from task 02). Expand:

```toml
[project]
name = "cognee-pipeline"
requires-python = ">=3.9"
version = "0.1.0"
description = "Python bindings for the cognee Rust SDK — AI memory pipeline (add → cognify → search)."
authors = [{ name = "Topoteretes", email = "support@cognee.ai" }]
license = "Apache-2.0"
license-files = ["LICENSE"]
readme = "README.md"
keywords = ["ai", "knowledge-graph", "memory", "rag", "embeddings"]
classifiers = [
    "Development Status :: 4 - Beta",
    "License :: OSI Approved :: Apache Software License",
    "Programming Language :: Python :: 3.9",
    "Programming Language :: Python :: 3.10",
    "Programming Language :: Python :: 3.11",
    "Programming Language :: Python :: 3.12",
    "Programming Language :: Rust",
    "Topic :: Scientific/Engineering :: Artificial Intelligence",
]

[project.urls]
Homepage = "https://www.cognee.ai"
Repository = "https://github.com/topoteretes/cognee-rust"
```

> `license`/`license-files` are added by task 02 — if 02 is already merged they exist;
> keep them. Do not duplicate. With PEP 639 SPDX `license = "Apache-2.0"`, do **not** also
> add the deprecated `License ::` classifier *and* SPDX if your maturin version rejects the
> combination — pick the form task 02 settled on. If task 02 used the SPDX string, keep the
> classifier list above but drop the `License :: OSI Approved` line if `maturin`/twine
> warns about the conflict.

### Step 9 — Verify/expand `js/package.json`

Open [js/package.json](../../../js/package.json). It already has `name`, `version`,
`description`, `main`, `types`, `engines`, `dependencies`, `files` (verified). Add the
missing `license` (task 02), `repository`, and `keywords`:

```json
{
  "name": "cognee",
  "version": "0.1.0",
  "description": "Node.js bindings for the cognee AI-memory SDK",
  "license": "Apache-2.0",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/topoteretes/cognee-rust.git"
  },
  "homepage": "https://www.cognee.ai",
  "keywords": ["ai", "knowledge-graph", "memory", "rag", "embeddings"],
  "main": "lib/index.js",
  ...
}
```

Confirm the `files` allowlist (lines 27–30) already lists what ships (`lib/`,
`cognee_neon.node`, and `LICENSE` from task 02). No build-artifact globs.

## Verification

```bash
# 1. Workspace manifest parses; every published crate has description + repository.
cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import sys,json;\
    p=json.load(sys.stdin)["packages"];\
    bad=[x["name"] for x in p if not x.get("description") or not x.get("repository")];\
    print("incomplete metadata:",bad); sys.exit(1 if bad else 0)'
# expect: incomplete metadata: []  (publish=false crates may be excluded if intentional)

# 2. MSRV is declared.
grep -n 'rust-version' Cargo.toml                      # expect 1.85
test -f rust-toolchain.toml && echo "toolchain file present"

# 3. Workspace still compiles.
cargo check --all-targets

# 4. MSRV builds (locally, if 1.85 is installed):
rustup toolchain install 1.85.0 --profile minimal 2>/dev/null || true
cargo +1.85.0 check --all-targets    # expect: success; if it fails, raise rust-version

# 5. CHANGELOG + manifests valid.
test -f CHANGELOG.md && echo "CHANGELOG present"
python3 -c 'import tomllib;tomllib.load(open("python/pyproject.toml","rb"));print("pyproject OK")'
node -e 'JSON.parse(require("fs").readFileSync("js/package.json"));console.log("package.json OK")'

# 6. CI workflow is valid YAML and has the msrv job.
python3 -c 'import yaml;d=yaml.safe_load(open(".github/workflows/ci.yml"));\
  assert "msrv" in d["jobs"], "msrv job missing"; print("ci.yml OK; msrv job present")'

# 7. Dry-run publish on a leaf crate: metadata no longer the blocker
#    (git deps still block non-leaf crates — that is Track B / task 24).
cargo publish --dry-run -p cognee-models 2>&1 | grep -iE 'description|repository|rust-version' \
  || echo "no metadata-related publish error ✔"
```

### New tests / checks

- The CI `msrv` job (Step 6) is the new automated guard.
- The "incomplete metadata" `cargo metadata` snippet (Verification #1) can be added as a
  CI step in the `lint` job to prevent regression — optional but recommended.

## Acceptance criteria

- [ ] `[workspace.package]` has description, repository, homepage, readme, keywords,
      categories, authors, **and** `rust-version = "1.85"`.
- [ ] All 27 workspace crates (+ `python/Cargo.toml`) inherit via `*.workspace = true`.
- [ ] `capi/` workspace + crate and `js/cognee-neon/` standalone crate carry metadata.
- [ ] `rust-toolchain.toml` pins `channel = "1.85"`.
- [ ] CI has a passing `msrv` (1.85) lane that builds with `cargo +1.85.0 check`.
- [ ] Root `CHANGELOG.md` (Keep a Changelog) documents `0.1.0`, consistent with task 11.
- [ ] `python/pyproject.toml` has description/authors/repository/keywords/classifiers;
      `js/package.json` has license/repository/keywords/files.
- [ ] `cargo check --all-targets` and the MSRV check pass; `cargo metadata` shows no
      published crate missing description/repository.

## Gotchas / do-not

- **Order vs task 02:** task 02 adds `license`; this task adds everything else to the same
  `[workspace.package]`. If 02 isn't merged yet, add `license` here too (don't ship without
  it), but coordinate so the two edits don't conflict. Likewise the python/js `license`
  keys.
- **Order vs task 11:** write the CHANGELOG's DB notes after the migration squash lands so
  "single baseline migration" is accurate. Don't describe the pre-squash 14+3 chain.
- **`rust-toolchain.toml` vs CI `@stable`:** the file pins local builds to 1.85, but the
  `dtolnay/rust-toolchain@stable` steps in the existing jobs override it (intentional —
  full coverage on stable). The dedicated `msrv` lane is what enforces the floor. Do not
  delete the file thinking CI ignores it (contributors rely on it locally).
- **crates.io metadata rules are validated at publish:** ≤ 5 keywords (each lowercase,
  ≤ 20 chars), categories must be valid slugs, description non-empty. A bad value fails
  `cargo publish --dry-run` — run #7 above before tagging.
- **Three separate workspaces** (root, `capi/`, `js/cognee-neon/`) — none inherit from each
  other. Metadata must be set in all three (Steps 1, 3, 4). Easy to forget capi/neon.
- **Bump, don't hack, the MSRV** if 1.85 fails to build: set `rust-version` to the true
  floor. A declared MSRV that doesn't actually build is worse than none.
- **Parity-neutral:** this task touches no `.rs`, schema, IDs, hashes, prompts, or
  collection names. Keep it purely metadata/docs/CI.

## Rollback

All changes are additive metadata, two new files (`CHANGELOG.md`, `rust-toolchain.toml`),
and one CI job. Revert with `git checkout -- <files>` and `rm CHANGELOG.md
rust-toolchain.toml`; delete the `msrv:` job block from `ci.yml`. No data/schema impact.
