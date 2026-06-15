# 06 — Bindings & Examples Cleanup

> Wave 1 · Priority P1 · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part A §A3 (A3.1–A3.5); [release-readiness-plan.md](../release-readiness-plan.md) T2.5/T2.6, T5.5, T6.6

[← Back to index](00-INDEX.md)

## Goal

Close the binding-surface and test/example gaps before release: resolve the config-setter
shape inconsistency (JS 44 granular setters vs C/Python generic-only), add JS tests for the
op groups that drifted behind Python, add runnable examples (a new `python/examples/` plus
cross-op examples), clean the stale local `_native.*.so`, and link the binding package
versions to the Cargo workspace version so they stop drifting.

## Background & why

The three bindings share a `bindings-common::ops` core, so the *operations* are consistent
— but the *ergonomic surface* and *test coverage* have drifted:

- **JS** exposes 40 granular typed config setters; **C** and **Python** expose only generic
  `set`/`set_str` + 4 bulk config setters. All delegate to the same `ConfigManager`, so this
  is sugar — but it's a real API-shape inconsistency to either document or unify.
- **Python** has ~22 test files across all op groups; **JS** has 13 files covering only ~5
  groups, despite exporting every op.
- ~~Neither binding has runnable examples beyond add/cognify/search (C has ~19; JS has 1;
  Python has **none**).~~ **DONE (commit e9ed51d):** `python/examples/` now has 6 annotated
  scripts; `js/examples/` now has 7 scripts (including all the originally planned topics).
  Both READMEs were updated with example discovery tables in commit e80dc2d. Step 3 below is
  updated accordingly.
- `python/pyproject.toml` and `js/package.json` both hardcode `0.1.0`, decoupled from the
  Cargo `[workspace.package] version = "0.1.0"`.

Verify each finding's current state (commands inline) before acting — line numbers
re-checked 2026-06-14.

## Prerequisites

```bash
git checkout -b task/06-bindings-and-examples-cleanup main
```

Read first:
- `js/cognee-neon/src/config.rs` (40 granular setters via macros — `string_setter!`,
  `u32_setter!`, `u16_setter!`, `f64_setter!`, `bool_setter!` invocations; 258 lines total).
- `capi/cognee-capi/src/sdk_config.rs` (generic `set`/`set_str` + 4 bulk).
- `python/src/config.rs` (generic `set`/`set_str` + 4 bulk).
- `js/cognee-neon/src/lib.rs:46-369` (full exported op surface; **note:** the exports
  block now spans lines 46–369, not the 55–126 originally cited — it grew with Phase 5/6 ops).
- `js/__tests__/` and `python/tests/` (the coverage gap).
- `python/pyproject.toml`, `js/package.json`, root `Cargo.toml` (versions).

## Files to change

| Path | Change |
|---|---|
| `docs/bindings/config-setters.md` *(new, or a README note)* | document setter-surface decision (A3.1) — **OR** add sugar to C/Python (alternative) |
| `js/__tests__/{memory,sessions,notebooks,admin,data-ops,cloud,visualization,recall}.test.ts` *(new)* | JS tests for drifted op groups (A3.2) |
| `python/examples/` | ~~new dir~~ **DONE** — 6 annotated scripts added in e9ed51d (A3.3) |
| `js/examples/` | ~~add memify/viz/sessions/datasets~~ **DONE** — 6 new examples added in e9ed51d (A3.3) |
| `python/cognee_pipeline/_native.cpython-39-darwin.so` | delete stale artifact (A3.4) |
| `python/scripts/check.sh` | add a clean step (A3.4, optional) |
| `python/pyproject.toml`, `js/package.json` | link version to workspace (A3.5) |

## Implementation steps

### Step 1 — A3.1: Resolve config-setter surface drift

**Verified surfaces:**

| Binding | Setters |
|---|---|
| JS (`js/cognee-neon/src/config.rs`) | 40 granular: `setLlmModel`, `setEmbeddingProvider`, … + 4 bulk (`setLlmConfig` etc.) + `set`/generic |
| C (`capi/cognee-capi/src/sdk_config.rs`) | `cg_sdk_config_set`, `cg_sdk_config_set_str`, 4 bulk (`set_llm_config`/`set_embedding_config`/`set_vector_db_config`/`set_graph_db_config`), `cg_sdk_config_get` |
| Python (`python/src/config.rs`) | `set`, `set_str`, same 4 bulk config setters, `get` |

All delegate to the same `ConfigManager::config()` methods, so the drift is purely
ergonomic.

**Decision (recommended): document as intentional.** Adding 40 FFI functions × 2 bindings
is high-surface, low-value for 0.1.0. The generic `set("llmModel", v)` reaches every key.

**Action (recommended path):**
1. Add a short section to each binding README's Config section (and/or a single
   `docs/bindings/config-setters.md`) stating:
   - C/Python use **generic** `set`/`set_str` + 4 bulk config setters by design.
   - JS adds **granular typed setters** as sugar; every one delegates to the generic setter.
   - The full settable key list is the canonical `Settings` field names (link to root
     README / `crates/lib/src/config.rs`).
2. In `python/README.md` (rewritten in task [05](05-documentation-cleanup.md)) and
   `js/README.md`, ensure the Config section reflects this explicitly so users of C/Python
   aren't surprised by the missing granular setters.

**Alternative (only if a reviewer requires unification):** add the granular setters to
C/Python by generating thin wrappers over `set_str`. This is mechanical but large
(~80 new FFI/PyO3 functions). If chosen, mirror JS's macro-driven approach and keep names
in the binding's idiomatic case (`set_llm_model` for Python, `cg_sdk_config_set_llm_model`
for C). **Prefer the document path for 0.1.0.**

> **Whichever path:** record the decision explicitly in the PR so it isn't re-litigated.

### Step 2 — A3.2: Add JS tests for drifted op groups

**Verified gap.** JS exports all ops (`js/cognee-neon/src/lib.rs:46-369`:
`cogneeForget`, `cogneeUpdate`, `cogneePruneData`, `cogneePruneSystem`, `cogneeRemember`,
`cogneeMemify`, `cogneeImprove`, `cogneeRecall`, `cogneeGetSession`, `cogneeAddFeedback`,
`cogneeListNotebooks`, `cogneeVisualize`, `cogneeServe`/`cogneeDisconnect`, …) but
`js/__tests__/` has only **13** files covering ~5 groups:

```
add  cognify  config  datasets  default_subscriber  errors  logging
pipeline  sdk_handle  search  setup_telemetry  setup_telemetry_analytics  smoke
```

**Missing JS test groups** (Python has all of these — see `python/tests/`):

| Group | Python test (model after) | New JS test file |
|---|---|---|
| memory (remember/memify/improve) | `test_memory_ops.py` | `memory.test.ts` |
| sessions | `test_session_ops.py` | `sessions.test.ts` |
| notebooks | `test_notebook_ops.py` | `notebooks.test.ts` |
| admin (reset status, default user) | (within `test_sdk_handle.py` / `test_dataset_mgmt.py`) | `admin.test.ts` |
| data-ops (forget/update/prune) | `test_data_ops.py` | `data-ops.test.ts` |
| cloud (serve/disconnect) | `test_cloud_ops.py` | `cloud.test.ts` |
| visualization | `test_visualization_ops.py` | `visualization.test.ts` |
| recall | `test_retrieval.py` (recall portion) | `recall.test.ts` |

**Action.** For each new file:
1. Mirror the structure of an existing JS test (e.g. `js/__tests__/datasets.test.ts`) for
   setup/teardown (`init()`, `new Cognee({...})`, `warm()`, temp dirs).
2. Read the corresponding `python/tests/test_*.py` to enumerate the assertions to port
   (same op, same expected shape) — keep parity with what Python already verifies.
3. Gate LLM-dependent assertions behind the same env-var check the existing JS tests use
   (so they skip gracefully without `OPENAI_*`); deterministic ops (forget/prune/sessions
   CRUD/notebooks CRUD/visualize-empty) should run unconditionally.
4. For `cloud.test.ts`: only test **direct mode** `serve({url, apiKey})` and `disconnect()`
   non-interactively (the Auth0 device flow needs a TTY — skip it, as the Python test does).

> **Verify the exact JS method names** from `js/cognee-neon/src/lib.rs` and the TS wrapper
> in `js/lib/`/`js/src/` before writing — the addon exports `cogneeForget` (snake-cased on
> the JS class as `forget`, per `js/README.md`). Use the **public TS API** (`c.forget(...)`,
> `c.sessions.get(...)`) in tests, not the raw `cognee*` addon functions.

```bash
cd js && npm test    # all suites incl. the 8 new files
```

### Step 3 — A3.3: Add runnable examples — **DONE (commit e9ed51d / e80dc2d)**

> **Status as of 2026-06-14:** this step is fully complete. No further action needed.

**Current state (verified):**

- `python/examples/` contains 6 annotated scripts (added in e9ed51d):
  `add_cognify_search.py`, `memify_recall.py`, `datasets.py`, `sessions.py`, `config.py`,
  `visualize.py`. Each has a header comment with required env vars and run instructions, uses
  `async`/`asyncio.run(main())`, and gracefully skips when `OPENAI_*` are absent.
- `js/examples/` contains 7 scripts (1 pre-existing + 6 added in e9ed51d):
  `add-cognify-search.ts`, `memify-recall.ts`, `datasets.ts`, `sessions.ts`, `config.ts`,
  `visualize.ts`, `pipeline-engine.ts`.
- Both READMEs were updated in e9ed51d/e80dc2d with example discovery tables and
  credential-gated smoke steps were added to both `check.sh` scripts.

**Original plan vs delivered names** (for reference — the intent is fully covered):

| Originally planned | Delivered | Notes |
|---|---|---|
| `python/examples/memify_triplet_search.py` | `python/examples/memify_recall.py` | Covers memify + `TRIPLET_COMPLETION` search |
| `python/examples/sessions_recall.py` | `python/examples/sessions.py` | Covers session create + recall |
| `python/examples/datasets_management.py` | `python/examples/datasets.py` | Covers list/status/delete |
| `js/examples/memify-triplet-search.ts` | `js/examples/memify-recall.ts` | Covers memify + TRIPLET_COMPLETION |
| `js/examples/visualization.ts` | `js/examples/visualize.ts` | Covers visualizeToFile |

> **Examples must compile/parse but need not be CI-gated.** The credential-gated smoke
> steps already added to `python/scripts/check.sh` and `js/scripts/check.sh` run the
> headline example when `OPENAI_*` are present and print `SKIP` otherwise.

### Step 4 — A3.4: Clean the stale `_native.*.so`

**Verified:** `python/cognee_pipeline/_native.cpython-39-darwin.so` still exists (16 MB,
Apr 11) and is **untracked** (`*.so` is in `python/.gitignore`; `git ls-files python/` shows
no `.so`). It will not ship, but importing a stale Python 3.9 binary during local dev
masks rebuilds. The `python/scripts/check.sh` clean step was **not** added in e9ed51d (only
the example smoke step was added there).

**Action:**
1. Delete it locally:
   ```bash
   rm -f python/cognee_pipeline/_native.cpython-39-darwin.so
   ```
2. Add a clean step to `python/scripts/check.sh` **before** the `maturin develop` call:
   ```sh
   rm -f cognee_pipeline/_native*.so   # drop any stale prebuilt extension
   ```
   The file currently does `cd "$PYTHON_DIR"` before `maturin develop`, so this line goes
   between the `cd` and the maturin invocation (around line 10 of the current file).
3. No `.gitignore` change needed — `*.so` is already ignored.

### Step 5 — A3.5 / T2.5-T2.6: Link binding versions to the workspace

**Verified hardcodes:** `python/pyproject.toml:8` `version = "0.1.0"` (no `dynamic`);
`js/package.json:3` `"version": "0.1.0"`; root `Cargo.toml:51` `version = "0.1.0"`. All
three are independent literals → drift hazard at the next bump. Neither the `dynamic`
switch nor the JS version-drift guard was introduced in the recent commits (e9ed51d,
e80dc2d). `python/Cargo.toml` confirms `version.workspace = true` (ready for 5a).

**5a — Python (maturin supports Cargo-derived version).** Make maturin read the version
from `python/Cargo.toml` (which inherits `version.workspace = true`):
```toml
# python/pyproject.toml — before
[project]
name = "cognee-pipeline"
requires-python = ">=3.9"
version = "0.1.0"
# after
[project]
name = "cognee-pipeline"
requires-python = ">=3.9"
dynamic = ["version"]    # version comes from Cargo.toml via maturin
```
> **Verify** `python/Cargo.toml` has `version.workspace = true` (it should — workspace
> member). Then `cd python && maturin build` and confirm the wheel is `…-0.1.0-…`. If
> maturin errors on `dynamic`, fall back to keeping the literal but add a CI assertion that
> it matches the workspace version (see 5c).

**5b — JS (no native Cargo link; assert in CI/build).** npm has no Cargo-derived version
mechanism. Add a sync check to `js/scripts/check.sh` (or `js/scripts/copy-artifact.js`)
that reads the workspace version and fails if `js/package.json` disagrees:
```sh
WS_VERSION=$(grep -m1 '^version' ../Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
PKG_VERSION=$(node -p "require('./package.json').version")
[ "$WS_VERSION" = "$PKG_VERSION" ] || { echo "version drift: workspace=$WS_VERSION pkg=$PKG_VERSION"; exit 1; }
```
> Keep the literal in `package.json` (npm needs it static) but **gate** it so a bump that
> forgets `package.json` fails the binding check. Place this in the same `check.sh` that
> `scripts/check_all.sh` already invokes for JS.

**5c — (If 5a falls back) add the same assertion for Python** in `python/scripts/check.sh`.

> Also covered by release task 22 (workspace metadata) and the manifest fields in T2.5/T2.6
> (description/license/keywords/classifiers) — those are out of scope **here**; this task
> only links the **version**. Note the overlap in the PR so task 22 doesn't redo it.

## Verification

```bash
# 1. Config-setter decision recorded (doc path):
ls docs/bindings/config-setters.md 2>/dev/null || grep -qi "granular\|generic setter" js/README.md python/README.md && echo "documented"

# 2. JS tests for the 8 new groups exist and pass:
ls js/__tests__/{memory,sessions,notebooks,admin,data-ops,cloud,visualization,recall}.test.ts
cd js && npm test && cd ..

# 3. Examples exist (ALREADY DONE — verify only):
ls python/examples/*.py   # should list 6 files
ls js/examples/*.ts       # should list 7 files

# 4. Stale .so gone:
test ! -f python/cognee_pipeline/_native.cpython-39-darwin.so && echo "so cleaned"

# 5. Version link/assertion:
grep -q 'dynamic = \["version"\]' python/pyproject.toml && echo "py dynamic version" \
  || echo "py: literal + CI assertion"
grep -q "version drift" js/scripts/check.sh && echo "js version guard"

# 6. Full binding gate:
scripts/check_all.sh   # runs capi + python + js check.sh
```

**Expected:** new JS suites pass; examples already present (6 py / 7 ts); `.so` gone;
version guard present; `check_all.sh` green.

**New tests:** 8 JS test files (Step 2). Examples are not auto-run tests.

## Acceptance criteria

- [ ] Config-setter drift resolved: documented as intentional (recommended) **or** sugar
      added to C/Python; decision recorded in the PR.
- [ ] JS test files added for memory/sessions/notebooks/admin/data-ops/cloud/visualization/recall;
      `npm test` green; deterministic ops run unconditionally, LLM ops skip without `OPENAI_*`.
- [x] ~~`python/examples/` created with 3-4 annotated scripts; `js/examples/` extended with
      memify/viz/sessions/datasets; both linked from their READMEs.~~ **DONE (e9ed51d,
      e80dc2d):** 6 Python + 7 JS examples present; both READMEs link them; credential-gated
      smoke steps added to both `check.sh` scripts.
- [ ] Stale `_native.cpython-39-darwin.so` deleted; `python/scripts/check.sh` cleans stale `.so`.
- [ ] Binding versions linked to the workspace version (maturin `dynamic` for Python; CI
      assertion for JS), drift fails the binding check.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **The `.so` is untracked** — deleting it has no git effect; the value is preventing stale
  imports. Don't add a `.gitignore` rule (already covered by `*.so`).
- **Use the public TS/Python API in tests** (`c.forget(...)`, `c.sessions.get(...)`), not
  the raw `cognee*` addon exports.
- **Cloud tests: direct mode only** — the Auth0 device flow needs a TTY; skip it (Python
  does too).
- **Don't expand binding *manifest* metadata here** (description/license/keywords) — that's
  task 22; this task only links the **version** to avoid double-work/conflicts.
- **Prefer documenting** the config-setter drift over adding ~80 FFI/PyO3 wrappers for 0.1.0.
- Verify `python/Cargo.toml` inherits `version.workspace = true` before switching pyproject
  to `dynamic` — otherwise maturin can't resolve the version.

## Rollback

Tests and examples are additive — delete the new files to revert. The `pyproject.toml`
`dynamic` switch and `check.sh` guards are single-line reverts. Re-creating the deleted
`.so` is a `maturin develop` away. No production code changes, so no functional risk.

[← Back to index](00-INDEX.md)
