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
   [`e2e-cross-sdk/tests/test_provenance_parity.py`](../../e2e-cross-sdk/tests/test_provenance_parity.py)
   (added in [05-10 §4.5](10-tests.md#45-cross-sdk-parity-test-e2e-cross-sdktestststest_provenance_paritypy))
   runs on the same lane as `test_cognify_structural.py`. Today the
   cross-SDK lane runs all tests under `e2e-cross-sdk/tests/` via
   pytest discovery, so the wiring is automatic; this task is the
   verification that it actually executes there.
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

### 4.2 Confirm the CI lane runs the parity test

Inspect
[`e2e-cross-sdk/`](../../e2e-cross-sdk/) and the GitHub Actions
workflow that runs it:

```bash
grep -rn "e2e-cross-sdk\|cross-sdk\|provenance_parity" .github/workflows/
```

Expected: the existing cross-SDK workflow (`e2e-cross-sdk.yml` or
similar) runs `pytest` over the whole `tests/` directory. Pytest's
default discovery picks up the new `test_provenance_parity.py` for
free.

If the workflow narrows discovery to specific files (e.g.
`pytest tests/test_cognify_structural.py`), edit the workflow to
include the new test:

```yaml
- name: Run cross-SDK tests
  run: |
    cd e2e-cross-sdk
    docker compose run --rm tests pytest tests/test_provenance_parity.py
```

(Better yet, drop the explicit path so pytest discovers everything
under `tests/`.)

### 4.3 Verify locally that the test runs in CI shape

```bash
cd e2e-cross-sdk
docker compose build
docker compose up --abort-on-container-exit
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
  [`e2e-cross-sdk/tests/test_provenance_parity.py`](../../e2e-cross-sdk/tests/test_provenance_parity.py)
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

# 2. Cross-SDK harness still runs end-to-end.
cd e2e-cross-sdk
docker compose up --build --abort-on-container-exit

# 3. The parity test specifically passes.
docker compose run --rm tests pytest tests/test_provenance_parity.py -v

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md)
- [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
- (Conditional) [`.github/workflows/<cross-sdk-workflow>.yml`](../../.github/workflows/)
  if §4.2 finds the workflow narrows pytest discovery.

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
