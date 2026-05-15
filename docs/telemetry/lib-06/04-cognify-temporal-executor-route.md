# LIB-06-04 — Route cognify temporal branch through `pipeline::execute`

**Status**: implemented in commit 8ffcd88 (user-locked option (a) on 2026-05-15: temporal-branch Document/DocumentChunk source_pipeline shifted from "cognify_pipeline" to "cognify", matching the shared in-body stamping constant from LIB-06-03; temporal pipeline_name stays "temporal-cognify" at the run-row level via build_temporal_cognify_pipeline's with_name)
**Owner**: _unassigned_
**Depends on**: LIB-06-03 (standard cognify executor route + helpers).
**Blocks**:
- [LIB-06-05 — Cleanup TODOs](05-cleanup-todos.md) — temporal branch must also route through `execute()` before the `TODO(LIB-06 follow-up)` at `crates/cognify/src/tasks.rs:1762` can be removed.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 2 (temporal branch is a separate `Pipeline`; selection happens before `execute()`), 3 (provenance equivalence verified), 5 (`extract_dlt_fk_edges` does NOT run for the temporal branch — confirm), 9 (downcast helper reused), 11 (`NoopWatcher`), 14 (pipeline name byte-stable).

---

## 1. Problem statement

The temporal cognify branch (`config.temporal_cognify == true`) at
[`crates/cognify/src/tasks.rs:1874-1892`](../../crates/cognify/src/tasks.rs#L1874-L1892)
runs a different two-task DAG:

1. `extract_temporal_events` (lines 1878-1879).
2. `add_temporal_data_points` (lines 1887-1889).

It does **not** run `extract_dlt_fk_edges` (no DLT for temporal data).

A builder already exists: `build_temporal_cognify_pipeline`
([line 2908](../../crates/cognify/src/tasks.rs#L2908)). The non-temporal
branch (LIB-06-03) selects between standard and temporal *before*
`execute()`; this sub-task wires the temporal path through `execute()`
with `build_temporal_cognify_pipeline`.

## 2. Locked decisions consulted

- **Decision 2** — Temporal is a separate `Pipeline`. The convenience
  function selects via:

  ```rust
  let pipeline = if effective_config.temporal_cognify {
      build_temporal_cognify_pipeline(...)
  } else {
      build_cognify_pipeline(...)
  };
  ```

  Then one `execute()` call.
- **Decision 3** — Provenance equivalence. **Correction (audit
  2026-05-15):** `cognify_temporal_inline` already stamps provenance
  inline on `Document` and `DocumentChunk` DataPoints (see
  `crates/cognify/src/tasks.rs:2002` and `:2026`), but uses the literal
  `"cognify_pipeline"` for `source_pipeline` (not `"cognify"` and not
  `"temporal-cognify"`). The two temporal-specific stages
  (`extract_temporal_events`, `add_temporal_data_points`) do **not**
  stamp — `add_temporal_data_points` writes raw `serde_json::Value`
  graph nodes and vector points rather than `DataPoint` structs, so
  `stamp_provenance` (which mutates `DataPoint`) is not applicable
  there.

  **Implication for the refactor:** LIB-06-03 discovered that
  `stamp_tree_dyn` cannot walk wrapper struct outputs
  (`ClassifiedDocuments`, `ExtractedChunks`, etc.) and added per-task
  in-body stamping inside each `make_*_task` (see
  `make_classify_documents_task` at `:2851` and `make_extract_chunks_task`
  at `:2873`). Since `build_temporal_cognify_pipeline` reuses
  `make_classify_documents_task` and `make_extract_chunks_task` from
  the standard branch (confirmed at `tasks.rs:3243-3253`), the
  `Document` / `DocumentChunk` DataPoints will **already be stamped**
  post-refactor — but with `source_pipeline = "cognify"` (the constant
  `COGNIFY_PIPELINE_STAMP_NAME` at `:2832`), **not**
  `"temporal-cognify"`. This is a byte-stable *regression* vs. today's
  inline stamping which uses `"cognify_pipeline"`. **Decision needed
  before sub-agent B starts:** either (a) keep
  `COGNIFY_PIPELINE_STAMP_NAME = "cognify"` everywhere — the standard
  branch already uses it and there are no tests asserting
  `"cognify_pipeline"` — and document the temporal stamp value as
  `"cognify"`; or (b) thread a per-pipeline stamp constant through
  `make_classify_documents_task` / `make_extract_chunks_task` so the
  temporal branch can stamp `"temporal-cognify"` instead. **Sub-agent A
  recommends (a)**: simpler, matches the standard branch literally,
  and the `with_name("temporal-cognify")` on the builder still gives
  the run-row layer the distinct pipeline name for telemetry.

  `make_extract_temporal_events_task` and
  `make_add_temporal_data_points_task` (`tasks.rs:3189-3224`) do not
  currently stamp; their outputs (`ExtractedTemporalEvents`,
  `CognifyResult`) are wrappers that the executor's `stamp_tree_dyn`
  also cannot walk. However, since neither produces `DataPoint`
  instances (graph nodes are raw JSON; vector points carry no
  provenance columns today), **no in-body stamping is needed for these
  two tasks**. Sub-agent B confirms by inspection during the refactor.
- **Decision 5** — Confirm `extract_dlt_fk_edges` does NOT run for
  temporal. Today it doesn't (the temporal branch early-returns at line
  1891). Post-refactor:

  ```rust
  if effective_config.temporal_cognify {
      let outputs = execute(temporal_pipeline, ...).await?;
      return extract_cognify_outputs(outputs);  // ← no DLT teardown
  }
  ```

- **Decision 9** — Reuse `extract_cognify_outputs` from LIB-06-03.
- **Decision 11** — `NoopWatcher`.
- **Decision 14** — Pipeline name: `build_temporal_cognify_pipeline`
  sets `with_name("temporal-cognify")`. Today temporal does not stamp
  provenance inline so there is no "byte stability" requirement from
  prior behaviour, but the *new* stamping will produce
  `source_pipeline = "temporal-cognify"` on every temporal DataPoint —
  document this in the commit body. If parity with Python's temporal
  cognify is required, sub-agent A audits Python's pipeline name
  (`/tmp/cognee-python/cognee/api/v1/cognify/...`) and aligns.

## 3. Pre-conditions

- LIB-06-03 committed and verified (cognify E2E + cross-SDK passing on
  the standard branch).
- `cargo check --all-targets` passes on HEAD.
- A temporal-cognify test already exists at
  [`crates/cognify/tests/temporal_cognify.rs`](../../crates/cognify/tests/temporal_cognify.rs)
  (audit 2026-05-15) covering Event/Timestamp node creation and
  Event-name vector indexing. Sub-agent B treats §4.4 as a no-op (the
  fixture is in place) and only **adds** an assertion gating Decision
  3: after a temporal cognify run, `chunks[*].base.source_pipeline ==
  Some("cognify")` and `chunks[*].base.source_task ==
  Some("extract_chunks_from_documents")` — equivalent to the inline
  stamps today (modulo the `"cognify_pipeline"` → `"cognify"` literal
  change called out in §2 Decision 3).

## 4. Step-by-step

### 4.1 Pipeline-name audit

```bash
rg "stamp_provenance.*temporal\|temporal_cognify\|temporal-cognify" crates/cognify/
```

Determine:

- Does the existing temporal path stamp provenance (no, as of `205bc8a`).
- Does Python's temporal path stamp provenance and with what
  `source_pipeline` value.
- Should the post-refactor `with_name(...)` be `"temporal-cognify"`
  (current builder) or `"cognify"` (matching the standard branch for
  cross-SDK parity) or something else.

**Locked decision (2026-05-13):** keep `with_name("temporal-cognify")`
on the builder. The temporal branch produces a distinct task DAG and
should have a distinct `source_pipeline` value so users can tell which
branch wrote a DataPoint. The provenance-stamping addition (§2
Decision 3) applies — every temporal DataPoint will read
`source_pipeline = "temporal-cognify"` post-refactor. Sub-agent A
documents this in the commit body.

### 4.2 Wire the selection

In the convenience function (post-LIB-06-03 shape):

```rust
let pipeline = if effective_config.temporal_cognify {
    build_temporal_cognify_pipeline(
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&llm),
        Some(Arc::clone(&database)),
        effective_config.clone(),
    )
} else {
    build_cognify_pipeline(
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&llm),
        Some(Arc::clone(&database)),
        Arc::clone(&ontology_resolver),
        effective_config.clone(),
    )
};
```

Then a single `execute()` call follows for both branches.

### 4.3 Skip DLT teardown on temporal

```rust
let result = extract_cognify_outputs(outputs)?;
if !effective_config.temporal_cognify {
    extract_dlt_fk_edges(&result.chunks_for_dlt, &result.documents_for_dlt, Arc::clone(&graph_db)).await?;
}
Ok(result)
```

Or equivalently, populate `documents_for_dlt` as empty for the
temporal branch — `make_add_temporal_data_points_task` calls
`add_temporal_data_points` which returns `CognifyResult::empty()`
(`tasks.rs:1035`) or populates only `event`-side fields, so
`documents_for_dlt` is always empty on the temporal branch (the
chunk-side input to `extract_dlt_fk_edges` is `result.chunks` which is
also empty on temporal — `add_temporal_data_points` does not propagate
chunks). In that case `extract_dlt_fk_edges` runs unconditionally and
is a no-op on empty inputs. **Locked preference**: explicit branch —
easier to read.

### 4.4 Test fixture

[`crates/cognify/tests/temporal_cognify.rs`](../../crates/cognify/tests/temporal_cognify.rs)
already exists with two tests:
`temporal_cognify_creates_event_and_timestamp_nodes` and
`temporal_cognify_populates_event_name_vector_collection`. Sub-agent B
runs both pre- and post-refactor to confirm no regression. The §2
Decision 3 stamping change is gated by adding one provenance assertion
to one of the existing tests — no new test file is needed.

### 4.5 Leave the `TODO(LIB-06 follow-up)` comment

Decision 13 keeps it until LIB-06-05.

## 5. Files modified

- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs):
  - Add `build_temporal_cognify_pipeline` selection inside `cognify`.
  - Skip `extract_dlt_fk_edges` on the temporal branch.
- `crates/cognify/tests/temporal_cognify.rs` (new, if none exists).
- Possibly `crates/cognify/src/temporal/` files if temporal-specific
  result handling changes (unlikely; LIB-06-03's `CognifyResult` changes
  already cover both branches via `#[serde(skip)]` empty fields for
  temporal).

## 6. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Cognify unit tests (includes temporal).
cargo test -p cognee-cognify

# 3. Targeted temporal test (Decision 3 gate for temporal).
bash scripts/run_tests_with_openai.sh test_temporal

# 4. Full cognify E2E (regression check — standard branch must still pass).
bash scripts/run_tests_with_openai.sh

# 5. Cross-SDK (regression).
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -

# 6. Full check.
scripts/check_all.sh
```

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Temporal branch *changes* provenance stamping literal post-refactor: `source_pipeline` flips from `"cognify_pipeline"` (current inline value) to `"cognify"` (the `COGNIFY_PIPELINE_STAMP_NAME` constant reused from LIB-06-03's `make_classify_documents_task` / `make_extract_chunks_task`) | **Low (accepted)** | No test asserts the `"cognify_pipeline"` literal today (`rg "cognify_pipeline" crates/` returns only the source line and this doc). Sub-agent B documents the change in the commit body. The temporal pipeline's distinct identity is preserved via `with_name("temporal-cognify")` on the builder, which feeds the pipeline-runs row, not the per-DataPoint stamp. |
| Temporal-specific tasks (`extract_temporal_events`, `add_temporal_data_points`) skip in-body stamping | Low | Their outputs do not contain `DataPoint` instances — graph nodes are raw `serde_json::Value`, vector points have no provenance columns. Sub-agent B verifies during refactor. |
| ~~No temporal-cognify test exists~~ | n/a | Fixture exists at `crates/cognify/tests/temporal_cognify.rs` (audit 2026-05-15). §4.4 is a no-op except for the new provenance assertion. |
| `build_temporal_cognify_pipeline` already takes `Option<Arc<DatabaseConnection>>` whereas the standard `build_cognify_pipeline` does too — the convenience function's `database: Arc<DatabaseConnection>` (required) gets wrapped in `Some(...)` to pass through. No semantic change. | Low | Trivial. |
| Cross-SDK regression on the standard branch due to a refactor side effect | Low — LIB-06-03 already covered this gate | Re-run sub-agent C's full verification for LIB-06-03. |
| Temporal pipeline output shape differs from `CognifyResult` | Medium | `make_add_temporal_data_points_task` returns `CognifyResult` already (confirmed at line 2887 of `tasks.rs`). `extract_cognify_outputs` reuses cleanly. |

## 8. Out of scope

- Wiring `DbPipelineWatcher` — gap 08-07.
- Adding temporal-specific watcher hooks (e.g. "events extracted: N").
- Splitting `CognifyResult` into separate standard vs temporal types.
- Renaming `build_temporal_cognify_pipeline` or its tasks.
- Aligning temporal pipeline naming with Python beyond what sub-agent
  A's audit recommends.
