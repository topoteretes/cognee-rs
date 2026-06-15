# 05 — Documentation Cleanup

> Wave 1 · Priority P1 · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part A §A2 (A2.1–A2.6); [release-readiness-plan.md](../release-readiness-plan.md) T6.5, T5.3–T5.4

[← Back to index](00-INDEX.md)

## Goal

Bring the user-facing docs to release quality: complete the `python/README.md` SDK
coverage by adding the missing Notebooks, Admin, and `remember_entry` sections (the bulk
of the SDK is already documented as of 2026-06-14), add the logging env-var section to
`.env.example`, fix the CI-workflow drift in `.claude/CLAUDE.md`, relocate internal
task-tracking scratchpads out of the shipped `docs/` tree, delete the empty
`docs/memify-tasks/`, and designate the root `README.md` as the canonical env-var source
with a cross-reference note.

## Background & why

The engineering reached "full PyCognee SDK" (T1–T11 done — see
`docs/python-bindings/STATUS.md`), and `python/README.md` was substantially rewritten
(as of 2026-06-14 it is 463 lines covering the full SDK). Three sections are still
missing: **Notebooks**, **Admin/pipeline-run admin**, and `remember_entry` in the Memory
section. A user `pip install`-ing the package would not find those operations documented.
Meanwhile `.env.example` omits the 8 logging vars the README already documents, and
`.claude/CLAUDE.md` references five CI workflows that no longer exist.
Several engineering scratchpads sit in `docs/` and would ship in a release tarball.

None of this is code; all changes are docs/config files. Verify each file's current state
(commands below) before editing — line numbers were re-checked 2026-06-15.

## Prerequisites

```bash
git checkout -b task/05-documentation-cleanup main
```

Read first:
- `js/README.md` (structural model — compare sections against `python/README.md`).
- `python/README.md` (current; 463 lines, largely complete — missing Notebooks, Admin,
  and `remember_entry`).
- `python/src/sdk_admin.rs` (Notebooks + Admin ops source of truth).
- `python/src/sdk_memory.rs:49` (`remember_entry` implementation).
- `README.md` lines 203-220 (the canonical Logging table) and `.env.example`.
- `.claude/CLAUDE.md` lines 54 & 286.

## Files to change

| Path | Change |
|---|---|
| `python/README.md` | add 3 missing sections: Notebooks, Admin, `remember_entry` (A2.1) |
| `.env.example` | add a `# Logging` section (A2.2) |
| `.claude/CLAUDE.md` | fix CI-workflow names at lines 54 & 286 (A2.3) |
| `docs/python-bindings/{IMPLEMENTATION-PROMPT.md,STATUS.md}` | move out of `docs/` (A2.4) |
| `docs/cognify-compatibility-implementation-plan.md` | move out of `docs/` (A2.4) |
| `docs/cognify-compatibility/IMPLEMENTATION-PROMPTS.md` | move out of `docs/` (A2.4) |
| `docs/memify-tasks/` | delete empty dir (A2.5) |
| `README.md` | add canonical env-var note to Logging section (A2.6); binding READMEs already cross-reference it |

## Implementation steps

### Step 1 — A2.1: Complete `python/README.md` SDK coverage

**Verified current state (2026-06-15):** `python/README.md` was largely rewritten (463
lines). It now covers: Quick start, upstream-compat API, Examples table, Programmatic
config, Environment variables, Pipeline ops (add/cognify/add_and_cognify), Search & recall
(15 types), Memory ops (remember/memify/improve), Datasets, Sessions, Data lifecycle,
Visualisation, Cloud: serve/disconnect, Initialisation & observability, Error handling,
and the legacy pipeline-engine appendix.

**Three sections are still missing** (verified by grepping the current file):
- `### Notebooks` — `notebooks.list()`, `.create(...)`, `.update(...)`, `.delete(id)` via
  `PyCogneeNotebooks` (`sdk_admin.rs`).
- `### Users and pipeline-run admin` — `reset_pipeline_run_status(...)`,
  `reset_dataset_pipeline_run_status(...)`, `get_or_create_default_user()` (`sdk_admin.rs`).
- `remember_entry(...)` is missing from the existing `### Memory operations` section
  (`sdk_memory.rs` implements it at line 49).

The full SDK method surface remains (re-verified in `python/src/lib.rs:84-122` and
`python/src/sdk*.rs`):

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

**Action:** add the three missing sections to `python/README.md`. Do **not** rewrite what
already exists — only add gaps. Follow `js/README.md`'s "Notebooks" and "Users and
pipeline-run admin" sections as structural models; verify each Python signature against
`python/src/sdk_admin.rs` and `python/src/sdk_memory.rs` — do **not** copy JS arg shapes
blindly (Python uses snake_case and may differ).

1. **Add `remember_entry` to the existing `### Memory operations` section** — insert it
   after `improve`. Verify the signature against `sdk_memory.rs:159`.
2. **Add `### Notebooks` section** — mirror `js/README.md:233-247`. Verify signatures
   against `python/src/sdk_admin.rs` (the `PyCogneeNotebooks` impl, lines ~44-140).
3. **Add `### Users and pipeline-run admin` section** — mirror `js/README.md:249-261`.
   Verify signatures against `python/src/sdk_admin.rs` (lines ~158-210).

Insert Notebooks immediately after the existing Sessions section, and Admin after
Notebooks — matching the `js/README.md` section order.

> **Do not** touch the existing Quick start, Pipeline ops, Search & recall, Memory,
> Datasets, Sessions, Data lifecycle, Visualisation, Cloud, Observability, Error handling,
> or low-level pipeline appendix sections — they are already accurate.

### Step 2 — A2.2: Add logging vars to `.env.example`

**Verified (2026-06-15):** `README.md:211-220` documents 8 logging vars; `.env.example`
(167 lines) has `RUST_LOG=info` (line 104) and `TOKENIZERS_PARALLELISM=false` (line 105)
in the Dev/Debug block, but **none** of the `COGNEE_LOG_*` vars. The vars are read by
`crates/logging/src/config.rs` (not `crates/lib/src/config.rs` as the audit states
— that file does not parse `COGNEE_LOG_*`).

**Action:** add a `# Logging` block to `.env.example` in the TIER 3 "Dev / Debug" area
(after line ~105, next to `RUST_LOG`/`TOKENIZERS_PARALLELISM`). Match the existing comment
style (`#VAR="default"` with an inline comment). Use the **exact defaults from
`README.md:211-220`** (the `### Logging` table):

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

**Verified (2026-06-15):** actual workflows are `ci.yml` + `http-parity.yml` +
`js-prebuild.yml` (`ls .github/workflows/`). Two drift sites:

**3a — line 54** (the tree comment):
```
# before
└── .github/workflows/          # CI: lib-tests.yml, lint.yml, capi-check.yml, js-check.yml, python-check.yml
# after
└── .github/workflows/          # CI: ci.yml (lint/test/docs + C/Python/JS binding checks), http-parity.yml (cross-SDK, workflow_dispatch), js-prebuild.yml (Neon prebuilds)
```

**3b — line 286** (the CI section):
```
# before
`lib-tests.yml` runs on push/PR to main: builds, caches embedding models, runs `scripts/run_tests_with_openai.sh` with `OPENAI_KEY` secret. Also runs `cargo doc --no-deps`.
# after
`ci.yml` runs on push/PR to main: lint (fmt + check + clippy), tests (with `OPENAI_KEY` secret via `scripts/run_tests_with_openai.sh`), `cargo doc --no-deps`, and C/Python/JS binding checks. `http-parity.yml` runs the cross-SDK Rust↔Python parity suite (`workflow_dispatch` only; see task 12). `js-prebuild.yml` builds Neon prebuilt binaries for multiple platforms.
```

> **Verify before writing** that `ci.yml` jobs match (the lint/test/binding-check
> structure was confirmed in the 2026-06-15 re-read; re-verify with
> `grep -n "^  [a-z].*:" .github/workflows/ci.yml` in case the file has changed).
> `http-parity.yml` is `workflow_dispatch`-only (re-confirmed).
> `js-prebuild.yml` is a Neon prebuild workflow — include it in the comment for
> completeness.

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

**Verified (2026-06-15):** env vars are tabulated in `README.md`, `.env.example`,
`.claude/CLAUDE.md`, and the binding READMEs (`python/`, `js/`). Currently consistent;
the risk is future drift.

**Partial progress already in place:** `python/README.md` line 154 already says
`COGNEE_LOG_*, LOG_FILE_NAME | Consumed by setup_logging() — see the workspace README's
"Logging" section.` and `js/README.md` line 354 already says a similar cross-reference.
Neither binding README needs further changes on this front.

**Remaining action — additive, no content removal:**
1. The `.env.example` logging block cross-reference is done in Step 2 (the `# Full
   reference…` banner in the new `# Logging` block).
2. In the root `README.md`, add a short note at the head of the `### Logging` table
   (currently at line 211):
   *"This table is the canonical reference for cognee logging environment variables.
   Binding READMEs and `.env.example` link here; update this table first when adding
   new logging vars."*

> Keep the per-binding tables (they list binding-specific vars like
> `COGNEE_BINDING_SUPPRESS_LOGS`) — only add the cross-reference note to root README,
> don't delete or rewrite binding tables.

## Verification

```bash
# 1. Python README covers the three previously-missing sections:
grep -q "notebooks.list\|notebooks.create" python/README.md && echo "Notebooks OK"
grep -q "reset_pipeline_run_status\|get_or_create_default_user" python/README.md && echo "Admin OK"
grep -q "remember_entry" python/README.md && echo "remember_entry OK"
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

- [ ] `python/README.md` documents the full PyCognee SDK — the existing sections cover
      handle, config, pipeline, retrieval, memory (remember/memify/improve), datasets,
      sessions, data lifecycle, visualisation, cloud, observability, error handling, and
      the legacy `Pipeline()` appendix. This criterion requires adding the **three missing
      sections**: `remember_entry` in Memory, Notebooks (`notebooks.*`), and Admin
      (`reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`,
      `get_or_create_default_user`).
- [ ] All 8 logging vars added to `.env.example` with README-matching defaults; no invented vars.
- [ ] `.claude/CLAUDE.md` lines 54 & 286 reference `ci.yml`, `http-parity.yml`, and
      `js-prebuild.yml`; extraction-status finding recorded (no change needed — already
      accurate as confirmed in 2026-06-15 re-read).
- [ ] The four internal tracking docs live under `docs/.internal/`; `git mv` preserved history; no dangling links.
- [ ] `docs/memify-tasks/` removed.
- [ ] Root README `### Logging` section has canonical-source note; `.env.example` logging
      block has cross-reference banner (done in Step 2); binding READMEs already
      cross-reference the workspace README (no further change needed).
- [ ] All verification commands pass.

## Gotchas / do-not

- **Verify Python signatures against `sdk_admin.rs` and `sdk_memory.rs`**, not against the
  JS README — arg names and shapes differ (snake_case; `Cognee()` constructor takes a JSON
  string, not a dict).
- **Do not invent env vars** — only the 8 documented logging vars; `COGNEE_LOG_MAX_BYTES`
  is intentionally unparsed (`crates/logging/src/config.rs` line 9 documents this).
- **Do not delete** the legitimate per-op docs in `docs/python-bindings/` — only the two
  tracking files (`IMPLEMENTATION-PROMPT.md`, `STATUS.md`).
- **Do not claim granular Python config setters** — Python exposes only generic + 4 bulk
  setters (cross-ref task 06).
- **Do not rewrite** the already-correct sections of `python/README.md` — only add the
  three missing sections (Notebooks, Admin, `remember_entry`).
- The "extraction understated" claim is **already fixed as of 2026-06-14** — confirmed in
  re-read; `.claude/CLAUDE.md` line 137 is correct, no change needed.
- `js-prebuild.yml` is a third workflow that must appear in the CLAUDE.md fix (Step 3) —
  the original task only mentioned `ci.yml` + `http-parity.yml`.

## Rollback

Pure doc/config changes; `git revert` the commit. Moved files can be `git mv` back. No
code or build impact.

[← Back to index](00-INDEX.md)
