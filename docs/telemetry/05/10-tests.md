# Task 05-10 — Tests (unit + pipeline integration + cross-SDK parity)

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (`stamp_tree` + extract helpers).
- [Task 05-06 — Pipeline executor integration](06-pipeline-executor-integration.md) (executor stamps in production).
- [Task 05-07 — User-label plumbing](07-user-label-plumbing.md) (`source_user` carries email).
- [Task 05-08 — Vector payload full dump](08-vector-payload-full-dump.md) (parity test reads vector payloads).
- [Task 05-09 — Cognify pre-stamp](09-cognify-prestamp.md) (pre-stamp shapes are part of the parity assertions).

**Blocks**:
- [Task 05-11 — Docs + CI](11-docs-and-ci.md) (CI lane wiring depends on tests existing).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #10 — cross-SDK parity test in scope of this gap.

---

## 1. Goal

Three layers of test coverage:

| Layer | File | Purpose |
|---|---|---|
| Unit | `crates/core/tests/provenance.rs` | The eight Python-parity cases on `stamp_tree`. Already partially in place from [05-03 §4.6](03-provenance-core.md#46-unit-tests-port-from-python-parity-suite); this task fleshes out anything left as a stub. |
| Pipeline integration | `crates/core/tests/provenance_pipeline_integration.rs` (NEW) | A 3-task in-memory pipeline that emits DocumentChunk → Entity → Triplet, run via `cognee_core::execute`, asserting every output DataPoint has the expected `source_pipeline` / `source_task` / `source_user` / `source_node_set` / `source_content_hash`. |
| Cognify E2E | `crates/cognify/tests/provenance_e2e.rs` (NEW) | Run a real `cognify_pipeline` against a small fixture and assert the four expected `source_task` values (`classify_documents`, `extract_chunks_from_documents`, `extract_graph_from_data`, `summarize_text`) appear on the resulting graph nodes. |
| Vector payload regression | `crates/vector/tests/provenance_payload.rs` (NEW) | Index a freshly stamped DataPoint via the cognify add-data-points path; pull it back via `search_similar`; assert all five `source_*` keys land in the metadata payload. |
| Cross-SDK parity | `e2e-cross-sdk/harness/test_provenance_parity.py` (NEW) | Run identical fixtures through Python and Rust SDKs in the existing Docker harness; assert the multiset of `source_task` values per node-type overlaps ≥90% across the two SDKs. (Pytest discovery roots at `e2e-cross-sdk/harness/`, NOT `tests/`.) |

## 2. Rationale

- **Unit tests** prove the algorithm is correct in isolation.
- **Pipeline integration** proves the executor wiring actually mutates
  values during a real run.
- **Cognify E2E** covers the production code path that operators use,
  including pre-stamp interactions (05-09) and provenance inheritance
  across the four cognify stages.
- **Vector regression** catches a silent regression on the payload
  shape (decision 5) without requiring a full cross-SDK run.
- **Cross-SDK parity** is the only signal that the Rust algorithm
  actually matches Python end-to-end. Without it, the gap can close
  silently and a regression slip in once Python's algorithm evolves.

Locked decision 10 puts the parity test in scope of this gap.

## 3. Pre-conditions

- All implementation tasks (01–09) committed.
- Clean `cargo check --all-targets` and `scripts/check_all.sh` on
  `main`.
- Docker harness in
  [`e2e-cross-sdk/`](../../e2e-cross-sdk/) builds cleanly:
  `cd e2e-cross-sdk && docker compose build` succeeds.

## 4. Step-by-step

### 4.1 Finish the unit tests in `crates/core/tests/provenance.rs`

The eight Python-parity cases were stubbed in
[05-03 §4.6](03-provenance-core.md#46-unit-tests-port-from-python-parity-suite).
Verify each one runs and passes. The drift-guard test from
[05-04 §4.5](04-has-datapoint-impls.md#45-update-the-drift-guard-test-from-05-03)
now exercises every implemented `HasDataPoint` type.

If any of the eight cases is still incomplete, finish it here using
the Python source as a guide:

```bash
cat /tmp/cognee-python/cognee/tests/unit/modules/pipelines/test_provenance_stamping.py
```

(Clone the repo first per the project guide if not present.)

### 4.2 Pipeline integration test (`crates/core/tests/provenance_pipeline_integration.rs`)

Build a minimal pipeline using mock backends:

```rust
use std::sync::Arc;

use cognee_core::{
    NoopExecStatusManager, NoopWatcher, PipelineBuilder, PipelineContext,
    RayonThreadPool, Task, TaskContextBuilder, execute,
};
use cognee_database::connect;
use cognee_graph::MockGraphDB;
use cognee_models::{DataPoint, DocumentChunk};
use cognee_vector::MockVectorDB;
use uuid::Uuid;

#[tokio::test]
async fn pipeline_stamps_every_emitted_datapoint() {
    // 1. Build a 3-task pipeline:
    //    - emit_chunks: input is unit; emits a Vec<DocumentChunk> via SyncIter
    //    - tag_chunks: identity (proves stamp survives across tasks)
    //    - mark_chunks: another identity, just to cover three-deep
    //
    // The first task is wrapped to emit DPs with the same `id` repeatedly
    // so the visited-set short-circuit can be observed on multiple tasks.

    let task_a = Task::sync_iter_typed(|_input: &(), _ctx| {
        let mut chunks = Vec::new();
        for i in 0..3 {
            let mut dp = DataPoint::new("DocumentChunk", None);
            dp.id = Uuid::new_v4();
            chunks.push(Box::new(DocumentChunk {
                base: dp,
                text: format!("chunk-{i}"),
                /* … other DocumentChunk fields … */
            }));
        }
        Ok(chunks.into_iter())
    });
    let task_b = Task::sync_typed(|c: &DocumentChunk, _ctx| Ok(Box::new(c.clone())));
    let task_c = Task::sync_typed(|c: &DocumentChunk, _ctx| Ok(Box::new(c.clone())));

    let pipeline = PipelineBuilder::new_with_task("test_pipeline", task_a)
        .add_task(task_b)
        .add_task(task_c)
        .with_name("test_pipeline")
        .build();

    // 2. Build a TaskContext with a populated PipelineContext.
    let db = connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let pipeline_ctx = PipelineContext {
        pipeline_id: pipeline.id,
        pipeline_name: "test_pipeline".into(),
        user_id: Some(Uuid::new_v4()),
        tenant_id: None,
        dataset_id: None,
        current_data: None,
        run_id: None,
        user_email: Some("alice@example.com".into()),
        provenance_visited: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    };
    let (_handle, ctx) = TaskContextBuilder::new()
        .thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
        .database(Arc::new(db))
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .pipeline_context(pipeline_ctx)
        .exec_status(Arc::new(NoopExecStatusManager))
        .build()
        .unwrap();

    // 3. Execute and inspect outputs.
    let outputs = execute(
        &pipeline,
        vec![Arc::new(()) as Arc<dyn cognee_core::Value>],
        Arc::new(ctx),
        &NoopWatcher,
    )
    .await
    .unwrap();

    assert_eq!(outputs.len(), 3);
    for arc in outputs {
        let chunk = arc.as_any().downcast_ref::<DocumentChunk>().unwrap();
        assert_eq!(
            chunk.base.source_pipeline.as_deref(),
            Some("test_pipeline")
        );
        // The DocumentChunk was emitted by task_a (the first task).
        // task_b's identity emits a clone of the same DP with a NEW Uuid
        // (since `c.clone()` allocates a fresh DP via Clone derive)…
        // OR the same Uuid if the test wraps Arc instead of cloning.
        // The visited-set short-circuit kicks in when the Uuid is the same.
        // Adjust assertion to reflect the chosen semantic.
        assert!(matches!(
            chunk.base.source_task.as_deref(),
            Some("emit_chunks") | Some("tag_chunks") | Some("mark_chunks")
        ));
        assert_eq!(
            chunk.base.source_user.as_deref(),
            Some("alice@example.com")
        );
    }
}
```

The exact assertion shape depends on whether the test pipeline keeps
the same `DataPoint.id` across tasks. Two test variants are useful:

- **`pipeline_stamps_every_emitted_datapoint`** — distinct UUIDs per
  task. Every task gets a chance to stamp.
- **`visited_set_keeps_first_task_attribution`** — same UUID
  preserved across tasks. The first task's name wins (per locked
  decision 2).

Implement both; together they prove the algorithm is correct under
both fan-out shapes.

### 4.3 Cognify E2E test (`crates/cognify/tests/provenance_e2e.rs`)

This test depends on an LLM call. Use the existing project pattern of
gating on `OPENAI_TOKEN`:

```rust
#[tokio::test]
async fn cognify_e2e_stamps_with_expected_task_names() {
    let openai_token = match std::env::var("OPENAI_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("skipping: OPENAI_TOKEN not set");
            return;
        }
    };

    // Build the cognify pipeline against a 1-document fixture.
    // … fixture setup (mirrors crates/cognify/tests/cognify_e2e.rs if present) …

    // Run via `build_cognify_pipeline` + `cognee_core::execute`
    // (NOT the convenience `cognify()` — we want the executor walk to
    // be the source of truth for stamping).

    // After the run, query the MockGraphDB / real graph DB for nodes
    // and assert the multiset of `source_task` values:
    let mut tasks_seen = std::collections::HashSet::new();
    for node in nodes {
        if let Some(t) = node.attributes.get("source_task").and_then(|v| v.as_str()) {
            tasks_seen.insert(t.to_string());
        }
    }
    let expected = [
        "classify_documents",
        "extract_chunks_from_documents",
        "extract_graph_from_data",
        "summarize_text",
    ];
    for t in expected {
        assert!(tasks_seen.contains(t), "missing source_task: {t}");
    }
}
```

If the cognify crate already has an E2E test file, append; otherwise
create the new file.

### 4.4 Vector payload regression (`crates/vector/tests/provenance_payload.rs`)

```rust
#[tokio::test]
async fn vector_point_carries_full_datapoint_dump() {
    use cognee_models::DocumentChunk;
    use cognee_vector::{MockVectorDB, VectorPoint};

    let mut chunk = DocumentChunk { /* … */ };
    chunk.base.source_pipeline = Some("cognify_pipeline".into());
    chunk.base.source_task = Some("extract_chunks_from_documents".into());
    chunk.base.source_user = Some("alice@example.com".into());
    chunk.base.source_node_set = Some("text_nodes".into());
    chunk.base.source_content_hash = Some("md5:abcdef".into());

    // Build the point exactly as crates/cognify/src/tasks.rs would.
    let mut point = VectorPoint::new(chunk.base.id, vec![0.0; 384]);
    for (k, v) in chunk.base.vector_metadata() {
        point = point.with_metadata(k, v);
    }

    let db = MockVectorDB::new();
    db.create_collection("DocumentChunk", "text", 384).await.unwrap();
    db.index_points("DocumentChunk", "text", &[point]).await.unwrap();

    let stored = db.get_payload("DocumentChunk", "text", chunk.base.id).await.unwrap();
    assert_eq!(stored.get("source_pipeline").and_then(|v| v.as_str()), Some("cognify_pipeline"));
    assert_eq!(stored.get("source_task").and_then(|v| v.as_str()), Some("extract_chunks_from_documents"));
    assert_eq!(stored.get("source_user").and_then(|v| v.as_str()), Some("alice@example.com"));
    assert_eq!(stored.get("source_node_set").and_then(|v| v.as_str()), Some("text_nodes"));
    assert_eq!(stored.get("source_content_hash").and_then(|v| v.as_str()), Some("md5:abcdef"));
    assert_eq!(stored.get("type").and_then(|v| v.as_str()), Some("DocumentChunk"));
}
```

If `MockVectorDB` does not have a `get_payload` method, add one in
`cognee-test-utils` (single-line addition, returns the
`HashMap<String, Value>` for a given point ID).

### 4.5 Cross-SDK parity test (`e2e-cross-sdk/harness/test_provenance_parity.py`)

Add a new pytest file under
[`e2e-cross-sdk/harness/`](../../e2e-cross-sdk/harness/) (the harness
directory is the actual pytest discovery root in this repo — there is
no `e2e-cross-sdk/tests/` directory) that:

1. Runs `python -m cognee add` then `python -m cognee cognify` on a
   fixed corpus.
2. Runs `cognee-rust add` then `cognee-rust cognify` on the same
   corpus, with the same `owner_id` / `tenant_id` so UUID5 outputs
   match.
3. Exports both graphs (Python via the existing graph-export helper,
   Rust via `cognee-rust visualize` or a dedicated dump command).
4. For each backend, builds the multiset
   `{(node_type, source_task) → count}`.
5. Asserts:
   - **Per-node-type Jaccard similarity ≥ 0.5** on the set of
     `source_task` values seen for that node type. (Tolerance band
     because LLM extraction is non-deterministic.)
   - **`source_pipeline` is exactly `"cognify_pipeline"`** on every
     node in both backends.
   - **`source_user` is non-empty and identical between Python and
     Rust** for the configured user.
   - **`source_content_hash`** when set on a Rust DocumentChunk
     equals the Python equivalent for the same `(document_id,
     chunk_index)`.

Use the existing `Dockerfile` build path; mirror
[`test_cognify_structural.py`](../../e2e-cross-sdk/harness/test_cognify_structural.py)
for the fixture-loading boilerplate (and reuse the `python_workspace`
/ `rust_workspace` / DB-helper fixtures defined in
[`harness/conftest.py`](../../e2e-cross-sdk/harness/conftest.py)).

```python
# e2e-cross-sdk/harness/test_provenance_parity.py
import os
import pytest

PARITY_THRESHOLD = 0.5  # Jaccard similarity per node type

@pytest.mark.docker
def test_provenance_parity(rust_cognified_graph, python_cognified_graph):
    """
    Asserts Python and Rust stamp DataPoints with overlapping
    source_task multisets per node type.
    """
    rust_tasks_by_type = group_by_type(rust_cognified_graph, "source_task")
    py_tasks_by_type = group_by_type(python_cognified_graph, "source_task")

    for node_type in set(rust_tasks_by_type) & set(py_tasks_by_type):
        rust_set = set(rust_tasks_by_type[node_type])
        py_set = set(py_tasks_by_type[node_type])
        jaccard = len(rust_set & py_set) / max(1, len(rust_set | py_set))
        assert jaccard >= PARITY_THRESHOLD, (
            f"source_task Jaccard for {node_type}: {jaccard:.2f} "
            f"(rust={rust_set}, python={py_set})"
        )

    for backend, graph in (("rust", rust_cognified_graph), ("python", python_cognified_graph)):
        for node in graph["nodes"]:
            assert node["attributes"]["source_pipeline"] == "cognify_pipeline", (
                f"{backend}: node {node['id']} has unexpected source_pipeline "
                f"{node['attributes']['source_pipeline']}"
            )
            assert node["attributes"].get("source_user"), (
                f"{backend}: node {node['id']} has empty source_user"
            )
```

### 4.6 Wire the parity test into the existing pytest discovery

`pytest` already discovers `e2e-cross-sdk/harness/test_*.py` via the
existing harness configuration / `harness/conftest.py`. No new wiring
needed in this task; CI lane confirmation is in
[05-11](11-docs-and-ci.md).

## 5. Verification

```bash
# 1. All new Rust tests pass.
cargo test -p cognee-core --test provenance_pipeline_integration
cargo test -p cognee-cognify --test provenance_e2e -- --ignored \
  # OPENAI_TOKEN-gated; runs locally with .env in place
cargo test -p cognee-vector --test provenance_payload

# 2. The eight unit tests still pass.
cargo test -p cognee-core provenance

# 3. Cross-SDK harness builds and the new parity test passes.
cd e2e-cross-sdk
docker compose up --build --abort-on-container-exit

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/tests/provenance.rs`](../../crates/core/tests/provenance.rs)
  — finish any stubbed cases.
- [`crates/core/tests/provenance_pipeline_integration.rs`](../../crates/core/tests/provenance_pipeline_integration.rs)
  — NEW. Two tests covering the executor wiring.
- [`crates/cognify/tests/provenance_e2e.rs`](../../crates/cognify/tests/provenance_e2e.rs)
  — NEW. One LLM-gated cognify E2E.
- [`crates/vector/tests/provenance_payload.rs`](../../crates/vector/tests/provenance_payload.rs)
  — NEW. One payload-shape regression test.
- (Conditional) [`crates/test-utils/src/lib.rs`](../../crates/test-utils/src/lib.rs)
  — `MockVectorDB::get_payload` if the test needs it.
- [`e2e-cross-sdk/harness/test_provenance_parity.py`](../../e2e-cross-sdk/harness/test_provenance_parity.py)
  — NEW.
- (Conditional) [`e2e-cross-sdk/harness/conftest.py`](../../e2e-cross-sdk/harness/conftest.py)
  if a new fixture (`rust_cognified_graph`, `python_cognified_graph`)
  needs to be added.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Cognify E2E flake on LLM non-determinism | Medium | Tolerance band on Jaccard similarity (≥0.5). The assertion is on the **set** of seen `source_task` values, not specific entity counts. |
| Cross-SDK Docker harness slow to iterate (~10 min per build) | Medium | `docker compose build` caches layers; iterate locally on the pytest file with `docker compose run` once the image is built. |
| `MockVectorDB::get_payload` does not exist and needs adding | Medium | Add a single-line accessor in `cognee-test-utils`; trivial. |
| Pipeline integration test's mock backends do not exercise the executor's stream/iter stamping branches | Low — the test pipeline includes a `SyncIter` task | The two test variants in §4.2 cover both. |

## 8. Out of scope

- Adding metric assertions (e.g. "exactly N entities").
- Performance benchmarks of `stamp_tree`.
- Failure-injection tests (what if `Arc::get_mut` fails). The
  helper logs `tracing::warn!` and degrades; covered by code review,
  not a test.
- Asserting `source_content_hash` matches across SDKs in 100% of
  cases. The cross-SDK test asserts on a single spot-checked chunk
  → entity chain (per the [parent doc §Cross-SDK parity test
  bullet 5](../05-datapoint-provenance.md)).
