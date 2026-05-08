# Task 05-11 — Docs + CI + gap closure

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: tasks 01–10 committed.

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: — (closure / housekeeping).

---

## 1. Goal

Close the gap. Three concrete outputs:

1. **`docs/telemetry/gap-analysis.md`** — flip the "Provenance
   stamping per DataPoint" row in section 3 from `Not found` to
   `Implemented (gap 05)`, and add gap 05 to the "Completed work"
   list.
2. **CI lane** — confirm
   [`e2e-cross-sdk/harness/test_provenance_parity.py`](../../e2e-cross-sdk/harness/test_provenance_parity.py)
   (added in [05-10 §4.5](10-tests.md#45-cross-sdk-parity-test-e2e-cross-sdktestststest_provenance_paritypy))
   runs in CI. Reality on this branch is uglier than the original
   plan assumed: the only cross-SDK workflow is
   [`.github/workflows/http-parity.yml`](../../.github/workflows/http-parity.yml),
   it is gated on `workflow_dispatch` only (push/PR triggers are
   commented out pending an upstream Python migration fix — see the
   header comment on that file), and every `pytest` invocation in it
   uses an explicit `-k "test_http_(…)"` filter that **excludes**
   `test_provenance_parity`. The `ci.yml` workflow does not run
   `e2e-cross-sdk` at all. So a one-line additive `docker compose
   run` step must be added to `http-parity.yml` (Phase-2, since it
   needs `OPENAI_KEY`), gated on `HAS_OPENAI_KEY`, that invokes
   `pytest -vs /harness/test_provenance_parity.py` against the
   `e2e-tests` service. See §4.2 below for the exact YAML.
3. **Closure summary** — append a "Closure summary" section to
   [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
   listing every commit in landing order (mirrors
   [gap 04's closure](../04-db-adapter-instrumentation.md#closure-summary)).

## 2. Rationale

- Without flipping the gap-analysis row, the next person doing a gap
  audit will spend time re-investigating closed work.
- Without explicit CI confirmation, the parity test could silently
  not run on PRs and we'd lose the regression signal.
- The closure summary is the durable record of what shipped, what
  did not, and what was deferred.

## 3. Pre-conditions

- Tasks 05-01 through 05-10 committed.
- `scripts/check_all.sh` clean on `main`.
- Cross-SDK harness builds and the parity test passes locally.

## 4. Step-by-step

### 4.1 Update `docs/telemetry/gap-analysis.md`

Edit
[`docs/telemetry/gap-analysis.md`](../gap-analysis.md):

- **Section 3 — Pipeline Run Status Persistence**, the
  "Provenance stamping per DataPoint …" row: change the right-hand
  cell from `Not found` to `Implemented (gap 05)` with a link to
  [`05-datapoint-provenance.md`](../05-datapoint-provenance.md).
- **Prioritized Gap List** — find the existing entry "**Provenance
  stamping on DataPoints** …" (currently item 3) and move it to the
  "Completed work" list at the bottom of the prioritized section,
  with the same wording style as the OTel and DB-spans entries:

  ```markdown
  - ✅ **Provenance stamping on DataPoints** — every DataPoint
    emitted by the pipeline executor now carries `source_pipeline`,
    `source_task`, `source_user`, `source_node_set`,
    `source_content_hash`, mirroring Python. Vector-store payloads
    carry the full DataPoint dump.
    → [05-datapoint-provenance.md](05-datapoint-provenance.md)
    (complete — see the
    [closure summary](05-datapoint-provenance.md#closure-summary)).
  ```

- **Detailed Inventory — Rust Side**, "Things Rust has that Python
  doesn't" — no change.

### 4.2 Add a CI step that runs the parity test

Inspect
[`e2e-cross-sdk/`](../../e2e-cross-sdk/) and the GitHub Actions
workflows that touch it:

```bash
grep -rn "e2e-cross-sdk\|cross-sdk\|provenance_parity" .github/workflows/
```

Reality on this branch: the only workflow that builds the
cross-SDK harness is
[`.github/workflows/http-parity.yml`](../../.github/workflows/http-parity.yml).
Every existing pytest invocation there is `-k`-filtered to
`test_http_(…)` patterns, so pytest discovery alone will **not**
run `test_provenance_parity.py`. Add a new additive step to
`http-parity.yml`, immediately after the existing "Phase-2"
LLM-gated step, that explicitly runs the parity test against the
plain `e2e-tests` service (which does not need the dual-server
`start_servers.sh` entrypoint, only LLM credentials):

```yaml
# ── Cross-SDK provenance parity (LLM-gated) ─────────────────────────
# Asserts gap-05 DataPoint provenance parity between Python and
# Rust SDKs (source_pipeline / source_task / source_user). See
# docs/telemetry/05-datapoint-provenance.md.
- name: Provenance parity (LLM-gated)
  if: ${{ env.HAS_OPENAI_KEY == 'true' }}
  env:
    HAS_OPENAI_KEY: ${{ secrets.OPENAI_KEY != '' }}
    OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}
    OPENAI_URL: https://api.openai.com/v1
    OPENAI_MODEL: gpt-4o-mini
  run: >-
    docker compose
    -f cognee-rust/e2e-cross-sdk/docker-compose.yml
    run --rm e2e-tests
    pytest -vs /harness/test_provenance_parity.py
    --tb=short
```

Do NOT remove or weaken the existing `-k` filters on the other
phases; they are intentional. Add this as a new step only.

Note that `http-parity.yml` is currently `workflow_dispatch`-only
(the push/PR triggers are commented out pending the upstream
Python migration fix tracked in the file header). Re-enabling
push triggers is out of scope of gap 05; document the limitation
in the closure summary instead.

### 4.3 Verify locally that the test runs in CI shape

```bash
cd e2e-cross-sdk
docker compose build e2e-tests
docker compose run --rm e2e-tests \
  pytest -vs /harness/test_provenance_parity.py --tb=short
```

If the test runs and passes, the lane is confirmed.

### 4.4 Append the closure summary

Edit
[`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
and append a new section at the bottom:

```markdown
---

## Closure summary

| Task | SHA | Commit subject |
|---|---|---|
| 05-01 | `<sha>` | telemetry/provenance-05-01: add source_content_hash field on DataPoint |
| 05-02 | `<sha>` | telemetry/provenance-05-02: audit Data.content_hash propagation |
| 05-03 | `<sha>` | telemetry/provenance-05-03: add HasDataPoint trait and stamp_tree |
| 05-04 | `<sha>` | telemetry/provenance-05-04: implement HasDataPoint for model containers |
| 05-05 | `<sha>` | telemetry/provenance-05-05: add provenance_visited and user_email to PipelineContext |
| 05-06 | `<sha>` | telemetry/provenance-05-06: wire stamping into pipeline executor |
| 05-07 | `<sha>` | telemetry/provenance-05-07: plumb User.email into PipelineContext::user_email |
| 05-08 | `<sha>` | telemetry/provenance-05-08: full DataPoint dump in vector payloads |
| 05-09 | `<sha>` | telemetry/provenance-05-09: pre-stamp inside extract_graph_from_data |
| 05-10 | `<sha>` | telemetry/provenance-05-10: add unit, pipeline, cognify, vector, and cross-SDK tests |
| 05-11 | `<sha>` | telemetry/provenance-05-11: docs + CI + gap closure |

### What the gap delivered

- `cognee_models::DataPoint` now carries the full
  `source_pipeline` / `source_task` / `source_user` /
  `source_node_set` / `source_content_hash` provenance set, matching
  Python's pydantic shape.
- A canonical stamping algorithm in
  [`cognee_core::provenance::stamp_tree`](../../crates/core/src/provenance.rs)
  with eight Python-parity unit tests.
- The pipeline executor stamps every emitted DataPoint after every
  successful task — single, iter, and stream task variants — with a
  per-run visited-set keyed on `DataPoint.id` so a DP shared across
  tasks is stamped once with the first task's name.
- `cognify_pipeline` produces graph nodes whose `source_task` covers
  the four expected stages: `classify_documents`,
  `extract_chunks_from_documents`, `extract_graph_from_data`,
  `summarize_text`.
- Vector-store payloads now carry the full DataPoint dump, enabling
  byte-comparable cross-SDK parity.
- New cross-SDK test
  [`e2e-cross-sdk/harness/test_provenance_parity.py`](../../e2e-cross-sdk/harness/test_provenance_parity.py)
  asserts ≥0.5 Jaccard similarity on `source_task` multisets per
  node-type and exact equality of `source_pipeline` and
  non-emptiness of `source_user`.

### Known follow-ups

The gap closes with the following intentional deferrals tracked here so
they aren't lost:

- **Convergence onto a single stamping site for cognify.** Locked
  decision 6 retained the local
  [`stamp_provenance` helper](../../crates/cognify/src/tasks.rs)
  alongside the executor walk so the convenience `cognify()` function
  keeps stamping. A follow-up task should switch `cognify()` to
  internally route through `cognee_core::execute(build_cognify_pipeline(...))`
  and then remove the local helper. Out of scope of gap 05 because
  the routing change has its own parity-risk profile (concurrency,
  retry, watcher events) unrelated to provenance.
- **`HasDataPoint` for `Triplet`.** Triplet does not embed a
  `DataPoint` today (it has its own `id: Uuid`). Adding it would
  unify the stamping path for triplet vectors. Likely follow-up if a
  future feature needs `source_*` lineage on triplet edges.
- **`source_content_hash` lineage queries.** The field is now
  populated end-to-end but no `forget()` / search retriever consumes
  it yet. The "forget every graph node derived from this raw file"
  feature in
  [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md#consumers-in-python)
  is now unblocked but not implemented.
- **OTel attributes for provenance.** Could attach `source_pipeline`
  / `source_task` to span attributes in
  `cognee.pipeline.task` so OTel consumers see provenance at trace
  level. Cheap follow-up; out of scope here.
```

Replace `<sha>` with the actual commit SHAs collected from the
orchestrator's per-task log.

### 4.5 Commit

Stage and commit the docs + workflow changes with the standard
message format:

```
telemetry/provenance-05-11: docs + CI + gap closure

Update gap-analysis.md to mark provenance stamping as implemented.
Confirm the cross-SDK parity test runs in the e2e-cross-sdk CI lane.
Append the closure summary to the parent gap doc.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

## 5. Verification

```bash
# 1. Compile (no Rust changes expected, but be safe).
cargo check --all-targets

# 2. The parity test passes locally in the same shape as CI.
cd e2e-cross-sdk
docker compose build e2e-tests
docker compose run --rm e2e-tests \
  pytest -vs /harness/test_provenance_parity.py --tb=short

# 3. Confirm the new step is well-formed YAML.
yamllint .github/workflows/http-parity.yml   # if available; otherwise eyeball

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md)
- [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
- [`.github/workflows/http-parity.yml`](../../.github/workflows/http-parity.yml)
  — new "Provenance parity (LLM-gated)" step appended after the
  existing Phase-2 step. See §4.2 for the exact YAML.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| The parity test is slow enough that operators disable it on draft PRs | Medium — Docker build is ~10 min | Acceptable; cross-SDK test already takes that long. If it becomes a bottleneck, gate behind a `[skip cross-sdk]` PR label. |
| Closure summary lists the wrong SHAs | Medium if compiled by hand | Sub-agent E should pull SHAs from `git log --oneline --grep="telemetry/provenance"` to collect them. |
| GitHub workflow change breaks an unrelated lane | Low | If §4.2 needs to edit a workflow, add the test path additively rather than replacing the existing pytest invocation. |

## 8. Out of scope

- Wiring `forget()` to use `source_content_hash` (Known follow-up).
- Adding `source_*` to OTel span attributes (Known follow-up).
- Removing the local `stamp_provenance` helper in cognify (locked
  decision 6 prohibits; it's a Known follow-up).
- Updating the visualization template / colour groupings —
  [`crates/visualization/src/lib.rs`](../../crates/visualization/src/lib.rs)
  already reads the four existing `source_*` keys; the new
  `source_content_hash` is metadata-only and does not need a UI hook.
