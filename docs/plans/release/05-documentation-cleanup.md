# 05 — Documentation Cleanup

> Wave 1 · Priority P1 · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part A §A2 (A2.1–A2.6); [release-readiness-plan.md](../release-readiness-plan.md) T6.5, T5.3–T5.4

[← Back to index](00-INDEX.md)

## Goal

Bring the user-facing docs to release quality: rewrite the stale `python/README.md` to
cover the full PyCognee SDK (modeled on `js/README.md`), add the logging env-var section
to `.env.example`, fix the CI-workflow drift in `.claude/CLAUDE.md`, relocate internal
task-tracking scratchpads out of the shipped `docs/` tree, delete the empty
`docs/memify-tasks/`, and designate the root `README.md` as the canonical env-var source
with cross-references from the others.

## Background & why

The engineering reached "full PyCognee SDK" (T1–T11 done — see
`docs/python-bindings/STATUS.md`), but `python/README.md` still documents only the legacy
`Pipeline()` engine tier. A user `pip install`-ing the package would have no idea the
40+ SDK ops exist. Meanwhile `.env.example` omits the 8 logging vars the README already
documents, and `.claude/CLAUDE.md` references five CI workflows that no longer exist.
Several engineering scratchpads sit in `docs/` and would ship in a release tarball.

None of this is code; all changes are docs/config files. Verify each file's current state
(commands below) before editing — line numbers were re-checked 2026-06-14.

## Prerequisites

```bash
git checkout -b task/05-documentation-cleanup main
```

Read first:
- `js/README.md` (the structural model for the Python rewrite — full SDK coverage).
- `python/README.md` (current; covers only `Pipeline()`).
- `python/src/sdk*.rs` (the actual exported Python surface — source of truth).
- `README.md` lines 189-213 (the canonical Logging table) and `.env.example`.
- `.claude/CLAUDE.md` lines 54, 137, 286.

## Files to change

| Path | Change |
|---|---|
| `python/README.md` | full rewrite to cover the PyCognee SDK (A2.1) |
| `.env.example` | add a `# Logging` section (A2.2) |
| `.claude/CLAUDE.md` | fix CI-workflow names at lines 54 & 286 (A2.3) |
| `docs/python-bindings/{IMPLEMENTATION-PROMPT.md,STATUS.md}` | move out of `docs/` (A2.4) |
| `docs/cognify-compatibility-implementation-plan.md` | move out of `docs/` (A2.4) |
| `docs/cognify-compatibility/IMPLEMENTATION-PROMPTS.md` | move out of `docs/` (A2.4) |
| `docs/memify-tasks/` | delete empty dir (A2.5) |
| `README.md`, `.env.example`, binding READMEs | designate README canonical + cross-ref (A2.6) |

## Implementation steps

### Step 1 — A2.1: Rewrite `python/README.md` for the full SDK

**Verified current state:** `python/README.md` Quick start shows only
`cognee_pipeline.Pipeline()`. The package actually exports (verified in
`python/src/lib.rs:84-122`):

- Classes: `Cognee` (`PyCognee`), `CogneeConfig`, `CogneeDatasets`, `CogneeSessions`,
  `CogneeNotebooks`, plus the legacy `Pipeline`, `PipelineRunHandle`, `TaskContext`,
  cancellation/progress types.
- Module functions: `serve`, `disconnect`, `setup_logging`, `setup_telemetry`,
  `setup_telemetry_analytics`.

**Verified SDK method surface** (from `python/src/sdk*.rs`):

| Group | Methods (snake_case, as exposed to Python) |
|---|---|
| handle | `Cognee(settings=None)`, `warm()`, `owner_id()`, `config`, `datasets`, `sessions`, `notebooks` |
| pipeline ops | `add(data, dataset)`, `cognify(dataset)`, `add_and_cognify(data, dataset)` |
| retrieval | `search(query, ...)`, `recall(query, ...)` |
| memory | `remember(...)`, `remember_entry(...)`, `memify()`, `improve(...)` |
| data lifecycle | `forget(...)`, `update(...)`, `prune_data()`, `prune_system(...)` |
| datasets | `datasets.list()`, `.list_data(id)`, `.has(id)`, `.status([ids])`, `.empty(id)`, `.delete_data(id, data_id)`, `.delete_all()` |
| sessions | `sessions.get(...)`, `.add_feedback(...)`, `.delete_feedback(...)`, `.get_graph_context(...)`, `.set_graph_context(...)` |
| notebooks | `notebooks.list()`, `.create(...)`, `.update(...)`, `.delete(id)` |
| admin | `reset_pipeline_run_status(...)`, `reset_dataset_pipeline_run_status(...)`, `get_or_create_default_user()` |
| visualization | `visualize()`, `visualize_to_file(...)` |
| config | `config.set(key, value)`, `.set_str(key, value)`, `.set_llm_config(...)`, `.set_embedding_config(...)`, `.set_vector_db_config(...)`, `.set_graph_db_config(...)`, `.get()` |
| cloud (module-level) | `serve(opts=None)`, `disconnect(opts=None)` |
| observability (module-level) | `setup_logging()`, `setup_telemetry()`, `setup_telemetry_analytics()` |

**Action:** rewrite `python/README.md` following `js/README.md`'s section order. Verify
each Python signature against the `sdk*.rs` source before writing the snippet — do **not**
copy JS arg shapes blindly (Python uses snake_case and may differ). Required sections:

1. **Title + intro** — name `cognee_pipeline`; one-line description of the add→cognify→search
   pipeline (mirror JS intro).
2. **Installation** — `pip install cognee-pipeline` + `maturin develop` for local dev (keep
   existing).
3. **Quick start (SDK)** — the headline example. Replace the `Pipeline()` snippet:

   ```python
   import asyncio
   import cognee_pipeline as cognee

   async def main():
       c = cognee.Cognee({"llm_model": "gpt-4o-mini", "llm_api_key": "..."})
       await c.warm()
       await c.add({"type": "text", "text": "The quick brown fox..."}, "demo")
       await c.cognify("demo")
       results = await c.search("What does the fox do?")
       print(results)

   asyncio.run(main())
   ```

   > Verify the constructor arg form (positional dict vs JSON string vs kwargs) against
   > `sdk.rs:98` `fn new(py, settings: Option<&str>)` — it takes a **JSON string or
   > settings object**; confirm how the Python binding marshals a dict before finalizing
   > the snippet.

4. **Constructor & Config** — `c.config.set(...)`, `.set_str(...)`, the 4 bulk config
   setters, `.get()` (note: Python has the **generic** setters only, no 40 granular ones —
   see task [06](06-bindings-and-examples-cleanup.md) §A3.1; do not invent granular setters).
5. **Pipeline operations** — `add` (text/file/url/binary forms), `cognify`,
   `add_and_cognify`.
6. **Search & recall** — list the 15 search types (copy the SCREAMING_SNAKE list from
   `js/README.md:149-152`, which is binding-agnostic).
7. **Memory operations** — `remember`, `memify`, `improve`, `remember_entry`.
8. **Datasets / Sessions / Notebooks** — the sub-accessor methods above.
9. **Data lifecycle** — `forget`, `update`, `prune_data`, `prune_system`.
10. **Cloud: serve / disconnect** — module-level functions.
11. **Visualization** — `visualize`, `visualize_to_file`.
12. **Initialisation & observability** — keep the existing, well-written
    `setup_logging`/`setup_telemetry`/`setup_telemetry_analytics` content (lines 28-111 of
    the current README) — it is accurate. Move it after the SDK sections.
13. **Appendix: legacy pipeline-engine API** — preserve the original `Pipeline()` content
    under an appendix (mirror `js/README.md`'s "Appendix: low-level pipeline API"). Do not
    delete it — the engine tier is still exported.
14. **References** — keep the observability doc links.

### Step 2 — A2.2: Add logging vars to `.env.example`

**Verified:** `README.md:199-206` documents 8 logging vars; `.env.example` (166 lines)
has only `RUST_LOG=info` (line 104) and **none** of the `COGNEE_LOG_*` vars. The vars are
read by `crates/logging/src/config.rs` (not `crates/lib/src/config.rs` as the audit states
— that file does not parse `COGNEE_LOG_*`).

**Action:** add a `# Logging` block to `.env.example` in the TIER 3 "Dev / Debug" area
(after line ~104, next to `RUST_LOG`). Match the existing comment style (`#VAR="default"`
with an inline comment). Use the **exact defaults from `README.md:199-206`**:

```bash
# -- Logging -------------------------------------------------------------------
# Full reference + multi-process caveat: see the root README "Logging" section.
#COGNEE_LOG_FILE=true                 # master toggle (false/0/no disables file logging)
#COGNEE_LOGS_DIR="~/.cognee/logs"     # falls back to /tmp/cognee_logs if unwritable
#COGNEE_LOG_FORMAT=plain              # plain (Python-compatible) | json
#COGNEE_LOG_ROTATION=daily            # daily | hourly | minutely | never
#COGNEE_LOG_BACKUP_COUNT=5            # files kept by the rotation policy
#COGNEE_LOG_MAX_FILES=10              # startup cap; older files removed
#LOG_LEVEL=info                       # fallback level; RUST_LOG wins when both set
#LOG_FILE_NAME=                       # auto-set by parent; children inherit
```

> **Do not** invent `COGNEE_LOG_MAX_BYTES` — `crates/logging/src/config.rs:9` explicitly
> states it is **not** parsed. Stick to the 8 documented vars.

### Step 3 — A2.3 / T6.5: Fix `.claude/CLAUDE.md` CI drift

**Verified:** actual workflows are `ci.yml` + `http-parity.yml` (`ls .github/workflows/`).
Two drift sites:

**3a — line 54** (the tree comment):
```
# before
└── .github/workflows/          # CI: lib-tests.yml, lint.yml, capi-check.yml, js-check.yml, python-check.yml
# after
└── .github/workflows/          # CI: ci.yml (build/lint/test + bindings), http-parity.yml (cross-SDK)
```

**3b — line 286** (the CI section):
```
# before
`lib-tests.yml` runs on push/PR to main: builds, caches embedding models, runs `scripts/run_tests_with_openai.sh` with `OPENAI_KEY` secret. Also runs `cargo doc --no-deps`.
# after
`ci.yml` runs on push/PR to main: builds, caches embedding models, runs `scripts/run_tests_with_openai.sh` with the `OPENAI_KEY` secret, and runs the C/Python/JS binding checks + `cargo doc --no-deps`. `http-parity.yml` runs the cross-SDK Rust↔Python parity suite (currently `workflow_dispatch`; see task 12).
```

> **Verify before writing** what `ci.yml` actually does (`grep -n "name:\|run:" .github/workflows/ci.yml`)
> so the "after" text is accurate — adjust the description to match the real job set.

**3c — extraction status (T6.5).** The audit/plan say CLAUDE.md "understates" extraction.
**Verified false as of 2026-06-14:** line 137 already says *"Extraction implemented for
text, pdf (feature-gated), csv (feature-gated), html (feature-gated `html-loader`), image
(feature-gated), audio (feature-gated)"* and line 165 lists the same. **No change needed
here** — record in the PR description that the extraction status is already accurate and
only the CI-workflow lines needed fixing. (Re-confirm with
`grep -n "Extraction implemented" .claude/CLAUDE.md` before deciding.)

### Step 4 — A2.4: Move internal task-tracking docs out of `docs/`

**Verified present + git-tracked:**
- `docs/python-bindings/IMPLEMENTATION-PROMPT.md`
- `docs/python-bindings/STATUS.md`
- `docs/cognify-compatibility-implementation-plan.md`
- `docs/cognify-compatibility/IMPLEMENTATION-PROMPTS.md`

These are engineering scratchpads, not user docs, and would ship in a source tarball.

**Decision:** move to a `docs/.internal/` tree (dot-prefixed → easy to exclude from
packaging; keeps git history via `git mv`). Do **not** move the legitimate per-op docs in
`docs/python-bindings/` (e.g. `core-pipeline-ops.md`, `README.md`) — those are reference
material; only the two tracking files.

```bash
mkdir -p docs/.internal/python-bindings docs/.internal/cognify-compatibility
git mv docs/python-bindings/IMPLEMENTATION-PROMPT.md docs/.internal/python-bindings/
git mv docs/python-bindings/STATUS.md docs/.internal/python-bindings/
git mv docs/cognify-compatibility-implementation-plan.md docs/.internal/
git mv docs/cognify-compatibility/IMPLEMENTATION-PROMPTS.md docs/.internal/cognify-compatibility/
```

Then fix dangling links: `STATUS.md` references `IMPLEMENTATION-PROMPT.md` and the per-op
docs (`sdk-handle.md`, etc.) which **stay** in `docs/python-bindings/`. Update STATUS.md's
relative links to point back up (`../../python-bindings/sdk-handle.md`). Grep for inbound
references to the moved files and fix them:

```bash
grep -rn "IMPLEMENTATION-PROMPT.md\|python-bindings/STATUS.md\|cognify-compatibility-implementation-plan.md\|cognify-compatibility/IMPLEMENTATION-PROMPTS.md" docs/ README.md .claude/
```

> If packaging later needs it, add `docs/.internal/` to any future `MANIFEST.in` /
> `package.json files` exclude — but those manifests don't currently include `docs/`, so no
> packaging change is required now.

### Step 5 — A2.5: Delete empty `docs/memify-tasks/`

**Verified empty** (`ls -la docs/memify-tasks/` shows only `.`/`..`; not git-tracked since
git doesn't track empty dirs).

```bash
rmdir docs/memify-tasks
```

### Step 6 — A2.6: Designate root README as canonical env-var source

**Verified:** env vars are tabulated in `README.md`, `.env.example`, `.claude/CLAUDE.md`,
and the three binding READMEs (`python/`, `js/`, and the C lib). Currently consistent;
the risk is future drift.

**Action — additive, no content removal:**
1. Add a one-line banner at the top of `.env.example`'s logging block (done in Step 2:
   *"Full reference… see the root README 'Logging' section"*).
2. In the root `README.md`, add a short note at the head of the env-var section:
   *"This README is the canonical reference for cognee environment variables. Binding
   READMEs and `.env.example` link here; update this table first."*
3. In each binding README's "Environment variables" section, ensure there's a closing
   line *"See the root README for the full env-var reference."* — `js/README.md` and the
   rewritten `python/README.md` already cross-link `COGNEE_LOG_*` to the workspace README;
   confirm/normalize the wording.

> Keep the per-binding tables (they list binding-specific vars like
> `COGNEE_BINDING_SUPPRESS_LOGS`) — only add the cross-reference, don't delete tables.

## Verification

```bash
# 1. Python README covers the SDK (not just the engine):
grep -q "class.*Cognee\|cognee.Cognee\|c.cognify\|c.search" python/README.md && echo OK
grep -q "Pipeline" python/README.md && echo "legacy appendix retained"

# 2. Logging vars present in .env.example, all 8:
grep -c "COGNEE_LOG_\|LOG_FILE_NAME\|LOG_LEVEL" .env.example   # >= 8

# 3. No stale workflow names remain:
! grep -n "lib-tests.yml\|capi-check.yml\|js-check.yml\|python-check.yml\|lint.yml" .claude/CLAUDE.md

# 4. Internal docs moved; none left in shipped paths:
test ! -f docs/python-bindings/STATUS.md && echo "STATUS moved"
test -f docs/.internal/python-bindings/STATUS.md && echo "STATUS in .internal"
test ! -d docs/memify-tasks && echo "memify-tasks gone"

# 5. No dangling links to moved files:
grep -rn "docs/python-bindings/STATUS.md\|cognify-compatibility-implementation-plan.md" docs/ README.md .claude/ \
  | grep -v "docs/.internal" && echo "FIX dangling links" || echo "links clean"

# 6. Markdown sanity (optional, if a linter is available):
# npx markdownlint python/README.md README.md
```

**Expected:** checks 1-5 print their OK markers; check 5 prints "links clean".

No automated tests change — these are docs. Optionally run `scripts/check_all.sh` (it does
not lint markdown, so it should be unaffected) to confirm nothing references the moved files
from code/build scripts.

## Acceptance criteria

- [ ] `python/README.md` documents the full PyCognee SDK (handle, config, pipeline,
      retrieval, memory, datasets, sessions, notebooks, lifecycle, cloud, visualization,
      observability) with the legacy `Pipeline()` content preserved in an appendix.
- [ ] All 8 logging vars added to `.env.example` with README-matching defaults; no invented vars.
- [ ] `.claude/CLAUDE.md` lines 54 & 286 reference `ci.yml` + `http-parity.yml`; extraction-status finding recorded (no change if already accurate).
- [ ] The four internal tracking docs live under `docs/.internal/`; `git mv` preserved history; no dangling links.
- [ ] `docs/memify-tasks/` removed.
- [ ] Root README marked canonical for env vars; `.env.example` and binding READMEs cross-reference it.
- [ ] All verification commands pass.

## Gotchas / do-not

- **Verify Python signatures against `sdk*.rs`, not against the JS README** — arg names and
  shapes differ (snake_case; constructor takes a JSON-string/settings object).
- **Do not invent env vars** — only the 8 documented logging vars; `COGNEE_LOG_MAX_BYTES`
  is intentionally unparsed.
- **Do not delete** the legitimate per-op docs in `docs/python-bindings/` — only the two
  tracking files (`IMPLEMENTATION-PROMPT.md`, `STATUS.md`).
- **Do not claim granular Python config setters** — Python exposes only generic + 4 bulk
  setters (cross-ref task 06).
- The "extraction understated" claim is **already fixed** — verify before editing line 137.

## Rollback

Pure doc/config changes; `git revert` the commit. Moved files can be `git mv` back. No
code or build impact.

[← Back to index](00-INDEX.md)
