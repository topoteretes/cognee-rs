# LIB-06-04 — Route cognify temporal branch through `pipeline::execute`

**Status**: not yet implemented (⬜)
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
- **Decision 3** — Provenance equivalence is verified. The temporal
  branch today does not appear to call `stamp_provenance` inline (audit
  via `rg "stamp_provenance" crates/cognify/src/tasks.rs | grep -i
  temporal`). The executor's `stamp_tree_dyn` is the only stamping path
  on this branch post-refactor, so temporal-cognify DataPoints *gain*
  `source_pipeline` / `source_task` / `source_user` etc. stamping for
  the first time. **Locked decision (2026-05-13): this behaviour change
  is accepted.** It improves parity with the non-temporal branch and
  with Python (which stamps provenance on temporal data too). Sub-agent
  B notes the change loudly in the commit body; no escalation is
  required.
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
- A temporal-cognify test exists (or is added): the implementor
  identifies an existing temporal test fixture in `crates/cognify/tests/`
  or in `e2e-cross-sdk/`. If none exists, sub-agent B adds the small
  fixture described in §4.4 as part of this task — verification is not
  meaningful without one. No escalation required.

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

Or equivalently, populate `chunks_for_dlt` / `documents_for_dlt` as
empty for the temporal branch (LIB-06-03's `make_add_data_points_task`
already only populates them for the standard branch — sub-agent A
confirms `make_add_temporal_data_points_task` does NOT populate them).
In that case `extract_dlt_fk_edges` runs unconditionally and is a no-op
on empty inputs. **Locked preference**: explicit branch — easier to
read.

### 4.4 Test fixture

If no temporal-cognify test exists today, add one:

```rust
#[tokio::test]
#[serial_test::serial]
async fn cognify_temporal_branch_routes_through_executor() {
    // Build minimal cognify inputs (one Data item).
    // Set config.temporal_cognify = true.
    // Run cognify(...).await.
    // Assert: result is Ok, vector DB has the expected temporal data
    //   collection, graph DB has expected event nodes.
}
```

The fixture lives in `crates/cognify/tests/` or alongside the existing
fact-extraction tests. Use the existing `OPENAI_*` env vars for LLM
access; gate with `#[ignore]` if no key is set so CI continues to work.

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
| Temporal branch *gains* provenance stamping post-refactor — behaviour change | **High (but accepted)** | Locked 2026-05-13: the change is accepted (Decision 3 in this sub-doc). Sub-agent B documents it loudly in the commit body. No escalation required; no opt-out. |
| No temporal-cognify test exists — verification is impossible | Medium | Sub-agent A audits via `rg "temporal_cognify\|TemporalCognify" crates/cognify/tests/`. If none, sub-agent B adds the §4.4 fixture as part of this task (locked 2026-05-13). |
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
