# DataPoint Provenance Stamping

## Overview

Python `cognee` decorates every `DataPoint` produced by a pipeline task with five
provenance attributes — `source_pipeline`, `source_task`, `source_user`,
`source_node_set`, `source_content_hash` — so that downstream consumers
(visualization, `forget`, lineage queries, dedup) can answer "where did this
node come from and which raw input produced it?".

Rust has the **fields** on [`DataPoint`](../../crates/models/src/data_point.rs)
(four of the five), but the **stamping mechanism is largely missing**:

- The pipeline executor in [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
  does not walk task outputs and mutate provenance on emitted `DataPoint`s.
- A different, name-shadowed `stamp_provenance` exists in
  [`crates/core/src/exec_status.rs`](../../crates/core/src/exec_status.rs) — but
  it is a *per-data-id audit-log* function (records "this data_id finished
  task T") and never touches DataPoint fields.
- Only [`crates/cognify/src/tasks.rs::stamp_provenance`](../../crates/cognify/src/tasks.rs#L1694)
  actually mutates `DataPoint.source_*`, and only for the local `cognify()`
  convenience function — pipeline-driven cognify (and every other crate) gets
  no stamping.
- Recursive traversal into nested `DataPoint`s (Python recurses into every
  `model_fields` value), the `visited` set that is **persisted across tasks**,
  and propagation of `node_set` / `content_hash` from inputs are all absent.
- `source_content_hash` is **not even a field** on the Rust `DataPoint`.

This breaks parity with Python: a Rust-cognified graph cannot be visualized
with the same `source_*` colour groupings as Python's, and any future
content-hash-aware `forget`/lineage features will be unable to filter Rust
nodes.

---

## Python implementation

### Where stamping runs

[`/tmp/cognee-python/cognee/modules/pipelines/operations/run_tasks_base.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py)

The executor calls `_stamp_provenance` **after every yield from every task**,
inside `handle_task` (lines 142-191):

```python
async for result_data in running_task.execute(args, kwargs, next_task_batch_size):
    if isinstance(result_data, list):
        result_count += len(result_data)
    else:
        result_count += 1

    _stamp_provenance(
        result_data,
        pipe_name,
        task_name,
        visited=provenance_visited,
        node_set=input_node_set,
        user_label=user_label,
        content_hash=input_content_hash,
    )

    async for result in run_tasks_base(leftover_tasks, result_data, user, ctx):
        yield result
```

Inputs to `_stamp_provenance` are pulled before the loop:

- `pipe_name = ctx.pipeline_name if ctx else None`
- `input_node_set = _extract_node_set(args)` — finds the first
  `DataPoint.source_node_set` in the input list/tuple, propagated as default
  for outputs.
- `input_content_hash = _extract_content_hash(args)` — finds the first
  `Data.content_hash` (raw `Data` row) **or** `DataPoint.source_content_hash`
  in inputs, propagated as default for outputs.
- `user_label = user.email or str(user.id)`
- `provenance_visited = ctx._provenance_visited` — see [PipelineContext.py:31](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineContext.py#L31).
  This `Set[int]` is reused across **all** tasks in the pipeline, so a
  DataPoint that is shared between task 1's output and task 2's output (e.g.
  a `DocumentChunk` passed through several tasks) is stamped exactly once
  with the **earliest** task's name.

### `_stamp_provenance` signature & body

```python
def _stamp_provenance(
    data, pipeline_name, task_name, visited=None,
    node_set=None, user_label=None, content_hash=None,
):
    if visited is None:
        visited = set()

    if isinstance(data, DataPoint):
        obj_id = id(data)
        if obj_id in visited:
            return
        visited.add(obj_id)

        if data.source_pipeline is None:
            data.source_pipeline = pipeline_name
        if data.source_task is None:
            data.source_task = task_name
        if data.source_user is None and user_label is not None:
            data.source_user = user_label

        # node_set & content_hash: prefer the value already on this DP,
        # otherwise inherit the parent context's value.
        current_node_set = node_set
        if data.source_node_set is not None:
            current_node_set = data.source_node_set
        elif current_node_set is not None and data.source_node_set is None:
            data.source_node_set = current_node_set

        current_hash = content_hash
        if data.source_content_hash is not None:
            current_hash = data.source_content_hash
        elif current_hash is not None and data.source_content_hash is None:
            data.source_content_hash = current_hash

        for field_name in data.model_fields:
            field_value = getattr(data, field_name, None)
            if field_value is not None:
                _stamp_provenance(
                    field_value, pipeline_name, task_name, visited,
                    current_node_set, user_label, current_hash,
                )

    elif isinstance(data, (list, tuple)):
        for item in data:
            _stamp_provenance(
                item, pipeline_name, task_name, visited,
                node_set, user_label, content_hash,
            )
```

Key behaviours:

1. **Recurses into nested DataPoints** via Pydantic `model_fields`. A
   `DocumentChunk` whose `contains: list[Entity]` field holds five `Entity`
   nodes will see all five stamped in one call.
2. **Idempotent / non-clobbering** — every assignment is guarded by
   `if dp.source_X is None`, so a downstream task never overwrites an
   upstream stamp.
3. **Visited-set is keyed on Python `id(obj)`** (object identity). Cross-task
   persistence (`ctx._provenance_visited`) ensures a DataPoint that survives
   multiple tasks is stamped exactly once, with the earliest task's name.
4. **Node-set / content-hash inheritance** is two-way:
   - if the DP already has a value, it overrides the parent context;
   - otherwise the parent context flows down into the DP.

### Other Python stamping site

[`cognee/tasks/graph/extract_graph_from_data.py:31-50`](https://github.com/topoteretes/cognee/blob/main/cognee/tasks/graph/extract_graph_from_data.py#L31)
defines `_stamp_provenance_deep` — a near-duplicate that only sets
`source_pipeline` and `source_task`. Used inside `integrate_chunk_graphs`
when LLM-extracted entities are constructed before the pipeline executor
gets a chance to walk them. Effectively a "pre-stamp" so the
run_tasks_base recursion is a no-op for those nodes.

### DataPoint schema

[`cognee/infrastructure/engine/models/DataPoint.py:55-62`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/engine/models/DataPoint.py#L55):

```python
source_pipeline: str | None = None
source_task: str | None = None
source_node_set: str | None = None
source_user: str | None = None
source_content_hash: str | None = None
```

These are regular Pydantic fields, **persisted as graph node attributes** (the
graph adapters serialize the whole pydantic model to a property dict) and
included in the JSON payload sent to the vector store.

---

## Provenance schema

| Attribute              | Type            | Source                                                                    | Notes |
|------------------------|-----------------|---------------------------------------------------------------------------|-------|
| `source_pipeline`      | `str`           | `PipelineContext.pipeline_name` (e.g. `"cognify_pipeline"`)               | Stamped after every task; never overwritten. |
| `source_task`          | `str`           | `running_task.executable.__name__`                                         | Stamped after every task; never overwritten. |
| `source_user`          | `str`           | `user.email` or `str(user.id)`                                             | Falls back to `id` when no email; persists across tasks. |
| `source_node_set`      | `str` or `None` | Extracted from input args — first `DataPoint.source_node_set` found       | Inherited when child DP has none; child's value overrides for further recursion. |
| `source_content_hash`  | `str` or `None` | Extracted from input args — first `Data.content_hash` or input DP's hash  | Same inheritance rules; ties graph nodes back to the raw ingestion artefact. |

---

## Consumers in Python

| Consumer                                                                                                                         | Reads                                                                            | Purpose |
|----------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|---------|
| [`modules/visualization/cognee_network_visualization.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py) | `source_task`, `source_pipeline`, `source_node_set`, `source_user` | Builds colour-coded HTML graph; legend groups nodes by each attribute. Lines 85-88 build colour maps; lines 1046-1049 render per-node panels. |
| The visualizer also **patches** `source_user` post-hoc when missing (lines 124-147), using the user-email as fallback.            | `source_user`                                                                    | Belt-and-braces because old data may predate stamping. |
| [`tasks/graph/extract_graph_from_data.py::_stamp_provenance_deep`](https://github.com/topoteretes/cognee/blob/main/cognee/tasks/graph/extract_graph_from_data.py#L31) | `source_pipeline`, `source_task`                                                 | Pre-stamps freshly LLM-constructed entities so subsequent recursion is a no-op. |
| Test scaffolding for vector-store schema migration                                                                               | `source_task`                                                                    | [`tests/unit/infrastructure/databases/vector/test_lancedb_schema_migration.py:316-328`](https://github.com/topoteretes/cognee/blob/main/cognee/tests/unit/infrastructure/databases/vector/test_lancedb_schema_migration.py#L316) — confirms vector payload round-trips the field. |
| `forget` / lineage queries                                                                                                       | None directly today                                                              | `source_content_hash` and `source_node_set` are designed for future content-hash-keyed deletion ("forget every graph node derived from this raw file") but are **not yet wired into `forget()`** — see [`api/v1/forget/forget.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py) which currently routes through dataset / data_id / pipeline_status, not provenance. |

So provenance today is primarily a **visualization & debugging** signal, with
`source_content_hash` reserved for upcoming lineage-aware forget. Any Rust
implementation must produce identical stamps so the same UI works on both
engines.

---

## Rust current state

### `DataPoint` struct

[`crates/models/src/data_point.rs:34-84`](../../crates/models/src/data_point.rs#L34):

```rust
pub struct DataPoint {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    pub ontology_valid: bool,
    pub version: i32,
    pub topological_rank: Option<i32>,
    pub metadata: HashMap<String, serde_json::Value>,
    #[serde(rename = "type")]
    pub data_type: String,
    pub belongs_to_set: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pipeline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_user: Option<String>,
    pub feedback_weight: f64,
}
```

**Missing**: `source_content_hash`.

### Existing stamping

1. **`crates/cognify/src/tasks.rs:1694`** — local helper `stamp_provenance(dp,
   pipeline, task, user)`. Sets only `source_pipeline`/`source_task`/
   `source_user`. **No** `node_set`/`content_hash`. **No** recursion. Called
   manually six times in
   [`cognify()`](../../crates/cognify/src/tasks.rs#L1742) (the all-in-one
   convenience function), each call stamps a single `&mut DataPoint` field
   inside the surrounding container struct (`doc.base`, `chunk.base`,
   `pair.entity.base`, …). This means:
   - The pipeline-driven path through [`build_cognify_pipeline`](../../crates/cognify/src/lib.rs)
     gets **zero** stamping.
   - Memify, ingestion, search and any future custom pipelines get zero
     stamping.

2. **`crates/core/src/exec_status.rs:48`** — trait method
   `ExecStatusManager::stamp_provenance(data_id, pipeline_name, task_name,
   user_id, node_set)` is a name collision. It is an **audit hook** invoked
   from [`pipeline.rs:1023`](../../crates/core/src/pipeline.rs#L1023) after
   every successful task call. The default impl (`NoopExecStatusManager`) is a
   no-op. There is no production impl that mutates DataPoints — it is purely a
   per-`(data_id, task)` book-keeping mechanism.

### How DataPoints flow through the executor

[`crates/core/src/task.rs:16-148`](../../crates/core/src/task.rs#L16):

- Tasks return type-erased values via `Arc<dyn Value>` (single),
  `Box<dyn Iterator<Item = Box<dyn Value>>>` (sync iter), or
  `BoxStream<'static, Box<dyn Value>>` (async stream).
- The executor loop in [`pipeline.rs::execute_from`](../../crates/core/src/pipeline.rs#L722)
  resolves into `Resolved::Single | Iter | Stream` and pushes to
  [`process_iter`](../../crates/core/src/pipeline.rs#L903) /
  [`process_stream`](../../crates/core/src/pipeline.rs#L931) which collect a
  batch then call [`dispatch_batch`](../../crates/core/src/pipeline.rs#L843).
- `Tagged<T>` / `TaggedMeta` ([`task.rs:64`](../../crates/core/src/task.rs#L64))
  attach a `node_set` string to a value but do **not** carry `DataPoint`
  semantics — they're only consulted by the no-op exec_status hook.

### What is persisted today

- **Graph DB (Ladybug)** — [`crates/graph/src/ladybug.rs::serialize_to_node_props`](../../crates/graph/src/ladybug.rs#L263)
  serializes each `DataPoint`-shaped JSON into a `properties` JSON-string
  attribute. Because the four existing `source_*` fields use
  `skip_serializing_if = "Option::is_none"`, they pass through automatically
  **iff they are set**. Today they are only ever set from `cognify()`.
- **Vector DB** — [`crates/cognify/src/tasks.rs:2304-…`](../../crates/cognify/src/tasks.rs#L2300)
  builds `VectorPoint` payloads by hand with explicit `with_metadata("type",
  …)` / `with_metadata("name", …)` calls. **No** `source_*` keys are
  copied into the payload. Vector-store payloads therefore carry zero
  provenance even when the DataPoint had it.
- **Relational DB** — `pipeline_runs` table tracks per-run lifecycle, but
  there is no per-DataPoint provenance row.
- **Visualization** — [`crates/visualization/src/lib.rs:86`](../../crates/visualization/src/lib.rs#L86)
  expects `source_pipeline`/`source_task`/`source_node_set`/`source_user`
  attributes on graph nodes. Today these are populated only for the subset of
  nodes that came through the convenience `cognify()` function.

---

## Detailed gap analysis

| Concern                                                | Python           | Rust                                                       | Severity |
|--------------------------------------------------------|------------------|------------------------------------------------------------|----------|
| Stamping happens for **every** task in **every** pipeline | Yes (in `handle_task`) | No — only six manual calls inside `cognify()`            | High     |
| Recursive walk into nested `DataPoint`s                | Yes              | No                                                         | High     |
| Visited-set persisted across tasks (keyed on identity) | Yes              | No                                                         | Medium   |
| `source_content_hash` field                            | Yes              | **Missing on struct**                                      | High     |
| `source_node_set` propagation from inputs              | Yes              | Partial — `TaggedMeta` carries it but never written to DP | Medium   |
| `source_content_hash` propagation from `Data.content_hash` | Yes          | No                                                         | High     |
| Vector-store payload includes `source_*`               | Yes (full DP serialized) | No (hand-built payload omits them)                       | Medium   |
| Graph-store node attributes include `source_*`         | Yes              | Auto-passes IF stamped (rarely is)                         | High*    |
| Visualization colour-grouping works                    | Yes              | Almost always degrades to "Unknown"                        | High     |
| `extract_graph_from_data` pre-stamp                    | Yes              | Only inside `cognify()` convenience fn                     | Medium   |

*The graph adapter is wired correctly; the upstream stamping is what's missing.

---

## Proposed design

### 1. Add the missing field

[`crates/models/src/data_point.rs`](../../crates/models/src/data_point.rs):

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub source_content_hash: Option<String>,
```

Initialise to `None` in `DataPoint::new` and `with_metadata`. Update inline
tests.

### 2. Centralise stamping in `cognee-core`

Create [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs)
with:

```rust
use std::collections::HashSet;
use uuid::Uuid;

/// What we know at the call site of `stamp_provenance`.
pub struct ProvenanceContext<'a> {
    pub pipeline_name: &'a str,
    pub task_name: &'a str,
    pub user_label: Option<&'a str>,
    pub node_set: Option<&'a str>,
    pub content_hash: Option<&'a str>,
}

/// Trait every type that wraps or *is* a DataPoint implements so the executor
/// can recurse without dynamic Pydantic-style introspection.
pub trait HasDataPoint {
    fn data_point(&self) -> &DataPoint;
    fn data_point_mut(&mut self) -> &mut DataPoint;
    /// Walk owned `DataPoint`-bearing children. Default = no children.
    fn for_each_child_mut(&mut self, _visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {}
}

/// Apply provenance to a tree of DataPoints; idempotent.
pub fn stamp_tree(
    root: &mut dyn HasDataPoint,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) { /* mirror Python algorithm using DataPoint.id as identity key */ }
```

**Identity key** — Python uses `id(obj)` (Python object identity). In Rust we
key on `DataPoint.id: Uuid` instead, which:

- survives clones (a DP cloned across tasks still gets stamped once);
- does not require pinning the value in memory;
- matches the practical semantic the Python visited-set is providing
  (DataPoints have stable UUIDs).

`stamp_tree` is `&mut`-based; the pipeline executor will need to obtain
mutable access. See `Mutability strategy` below.

### 3. Implement `HasDataPoint` for every container model

For each model in [`crates/models/src/`](../../crates/models/src/) that wraps
a `DataPoint`-shaped struct (`Entity`, `EntityType`, `EdgeType`,
`DocumentChunk`, `Document`, `Triplet`, `TextSummary`, …) add:

```rust
impl HasDataPoint for Entity {
    fn data_point(&self) -> &DataPoint { &self.base }
    fn data_point_mut(&mut self) -> &mut DataPoint { &mut self.base }
    fn for_each_child_mut(&mut self, visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {
        // e.g. Entity has `entity_type: Box<EntityType>` — recurse:
        visit(&mut *self.entity_type);
    }
}
```

This replaces Python's reflective `model_fields` walk with explicit, compile-
time-checked traversal. Cheaper at runtime, refactor-proof, and lets us
ignore non-DataPoint fields (which Python wastes cycles iterating).

### 4. Hook into the pipeline executor

[`crates/core/src/pipeline.rs::call_with_retry`](../../crates/core/src/pipeline.rs#L960)
already runs after every successful task — extend it to do per-task
DataPoint stamping (in addition to today's `exec_status.stamp_provenance`
audit-log call):

```rust
// After resolve_call() succeeds, but before returning Ok(resolved):
if let Some(prov_ctx) = build_provenance_ctx(env, task_name, &resolved, &input) {
    let mut visited = env.ctx.pipeline_ctx
        .as_ref()
        .and_then(|p| p.provenance_visited.lock().ok())
        .expect("PipelineContext provenance_visited present");
    walk_resolved_and_stamp(&mut resolved, &prov_ctx, &mut *visited);
}
```

Where `walk_resolved_and_stamp` matches on `Resolved::Single | Iter | Stream`:

- **Single** — try `Arc::get_mut`; if shared, fall back to type-erased
  `as_any_mut`. Tasks that emit `Arc<dyn Value>` already own the value at the
  point of emission, so `Arc::get_mut` succeeds in practice. For streams,
  stamping happens **inside** [`process_stream`](../../crates/core/src/pipeline.rs#L931)
  on each `Box<dyn Value>` *before* it is converted to `Arc` for downstream
  dispatch (still owned at that point).
- **Iter / Stream** — map each `Box<dyn Value>` through a
  `try_downcast_mut::<T: HasDataPoint>` adapter. The set of `T`s the
  executor is willing to handle is a small static list (the public model
  types). Any value whose concrete type is not registered is passed through
  unchanged (matches Python's "if not DataPoint, do nothing" branch).

A registry-style downcast keeps the executor free of dependencies on
`cognee-models`: register adapters from the lib crate at startup.

### 5. Mutability strategy & avoiding clone explosion

- Tasks emit `Arc<dyn Value>` because the executor needs cheap clone for
  retries/fan-out. Once a task **succeeds**, the executor holds the only Arc
  briefly between `resolve_call` returning and `dispatch_batch` consuming.
  We exploit that window to call `Arc::get_mut`. If `get_mut` returns `None`
  (some task pre-shared the Arc), we log a warning and skip stamping for
  that item; this is acceptable because a well-behaved task will not leak
  Arcs of its outputs.
- For `Stream`/`Iter`, items arrive as `Box<dyn Value>` — uniquely owned, so
  `Box::as_any_mut()` always works.
- No deep clones are needed. `DataPoint` stamping mutates five `Option<String>`
  fields in place.

### 6. Threading the `ProvenanceContext`

Already mostly available:

- `pipeline_name` — `env.pipeline_name` in `ExecEnv` / `PipelineContext::pipeline_name`.
- `task_name` — passed into `call_with_retry`.
- `user_label` — derive from `pipeline_ctx.user_id`. **Today** Rust has only
  `user_id: Uuid`; Python prefers `email`. We add a `user_email: Option<String>`
  field to [`PipelineContext`](../../crates/core/src/task_context.rs#L22)
  (populated by callers that have a `User`) and fall back to
  `user_id.to_string()` when absent — this matches Python's
  `user.email or str(user.id)`.
- `node_set` — extract from input args before the task runs (mirror Python's
  `_extract_node_set`): walk the input `Arc<dyn Value>`, downcasting through
  the same registry to pull the first non-`None` `source_node_set`.
- `content_hash` — same pattern, but additionally allow the value to come
  from a raw `cognee_models::Data.content_hash` (no DataPoint wrapper).

### 7. Visited-set persistence

Add to [`PipelineContext`](../../crates/core/src/task_context.rs#L22):

```rust
pub provenance_visited: Arc<Mutex<HashSet<Uuid>>>,
```

Initialised once in `execute()` ([`pipeline.rs:486`](../../crates/core/src/pipeline.rs#L486))
and shared by every task's `call_with_retry` invocation, mirroring
`PipelineContext._provenance_visited` in Python.

Mutex (not RwLock) because writes vastly outnumber reads.

### 8. Vector-store payload parity

Update vector-payload construction in
[`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) (and any
other site that builds `VectorPoint`) to copy stamped `source_*` fields from
the DataPoint into the metadata map. Centralise in a new helper
`vector_metadata_from_dp(&DataPoint) -> HashMap<String, Value>`. Mirrors
Python's behaviour where the entire pydantic model is dumped into the
vector-store payload.

### 9. Pre-stamp inside extract_graph_from_data

Rust's
[`extract_graph_from_data`](../../crates/cognify/src/tasks.rs) constructs
fresh `Entity`/`EntityType` DataPoints from LLM JSON. To keep parity with
Python's `_stamp_provenance_deep`, stamp them at construction with the
already-known `("cognify_pipeline", "extract_graph_from_data")` so the
executor's recursive walk is a no-op for those nodes (cheap optimisation;
also gives correct stamps when running outside `execute()`).

---

## Schema impact

| Backend                                  | Change                                                                                                       | Migration |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------|-----------|
| Graph (Ladybug) node properties JSON     | Adds two keys (`source_node_set`, `source_content_hash`) when set; the JSON column is schemaless             | None      |
| Vector store payload                     | Adds five optional keys to the metadata map                                                                  | None — Qdrant payloads are schemaless. |
| Relational DB                             | None — provenance is not relationally indexed                                                                | None      |
| `cognee_models::DataPoint` Rust struct   | New `source_content_hash` field                                                                              | Bincode/JSON callers tolerate absence (`#[serde(default)]`). Requires bumping any persisted `models` payloads, but no on-disk DataPoint is currently stored outside graph/vector adapters which already round-trip through JSON. |
| `cognee_core::PipelineContext` Rust struct | New `provenance_visited` and `user_email` fields (both with sensible defaults)                              | None — additive, internal API. |

---

## Action items

1. **`crates/models/src/data_point.rs`** — add `source_content_hash`,
   initialise in constructors, extend tests.
2. **`crates/core/src/provenance.rs` (new)** — define `HasDataPoint`,
   `ProvenanceContext`, `stamp_tree`, plus the input-extraction helpers
   (`extract_node_set_from_value`, `extract_content_hash_from_value`).
3. **`crates/core/src/lib.rs`** — re-export the new module.
4. **`crates/core/src/task_context.rs`** — extend `PipelineContext` with
   `provenance_visited: Arc<Mutex<HashSet<Uuid>>>` and `user_email: Option<String>`.
5. **`crates/core/src/pipeline.rs::execute`** — initialise the visited set
   when building the per-run `PipelineContext`; thread it through `ExecEnv`
   if not already reachable via `ctx.pipeline_ctx`.
6. **`crates/core/src/pipeline.rs::call_with_retry`** — after `resolve_call`
   succeeds, walk the resolved value and stamp via the new helper.
7. **`crates/core/src/pipeline.rs::process_stream` / `process_iter`** —
   if items can carry DataPoints, stamp at consumption time too (covers
   stream-yielded items that arrive after the task call has already
   returned).
8. **`crates/models/src/{entity,entity_type,edge_type,document,document_chunk,triplet,...}.rs`** —
   implement `HasDataPoint`, including `for_each_child_mut` for nested
   container fields.
9. **`crates/cognify/src/tasks.rs`** — remove the local `stamp_provenance`
   helper; rely on the executor. Keep the pre-stamp inside
   `integrate_chunk_graphs` (rename to use the new `ProvenanceContext`).
10. **Vector payload helpers** — add `vector_metadata_from_dp` and call it
    from every `VectorPoint::new(...)` site that originates from a
    DataPoint.
11. **`crates/lib/src/api/...`** — when the user has an email, populate
    `PipelineContext::user_email` so visualization labels match Python's.
12. **Update [`docs/telemetry/gap-analysis.md`](./gap-analysis.md)** —
    mark "Provenance stamping per DataPoint" row from "Not found" to
    "Implemented" once 1-11 ship. *(Out of scope for this doc.)*

---

## Cross-SDK parity test

Add a test under [`e2e-cross-sdk/tests/`](../../e2e-cross-sdk/tests/) called
`test_provenance_parity.py` that:

1. Runs the **same** Python ingestion + cognify on a fixed corpus.
2. Runs the **same** Rust ingestion + cognify on the same corpus.
3. For each backend, exports the graph nodes and asserts that for every
   node:
   - `source_pipeline == "cognify_pipeline"`
   - `source_task ∈ {classify_documents, extract_chunks_from_documents,
     extract_graph_from_data, summarize_text}` (the set Python emits).
   - `source_user` matches the configured user email.
   - `source_content_hash` (when set) equals the `Data.content_hash` of the
     ingested file.
4. Computes the **multiset** of `source_task` values per node-type and
   asserts ≥90 % overlap between Python and Rust outputs (the existing
   structural-similarity tolerance bands).
5. Spot-checks a `DocumentChunk` and an `Entity` cluster: the chunk's
   `source_content_hash` must equal the entity's, demonstrating that
   propagation across the chunk → entity-extraction task boundary
   produces the same lineage chain in both SDKs.

This becomes the canonical "provenance parity" gate, runnable in CI alongside
the existing `test_cognify_structural.py`.

---

## Open questions

1. **Where does `user.email` come from in Rust?** Python pulls it off the
   `User` SQLAlchemy model. Rust has [`cognee_models::User`](../../crates/models/src/user.rs);
   confirm whether the lib API surface that builds `PipelineContext` (e.g.
   in [`crates/lib/src/api/cognify.rs`](../../crates/lib/src/api/cognify.rs))
   has access to the email or only the `user_id`. If only `user_id`, plumb
   the email through the same constructors.
2. **Should `Resolved::Stream` items be stamped lazily inside the stream
   adapter, or eagerly when batched in `process_stream`?** Eager is simpler;
   lazy preserves backpressure semantics. Recommend eager (small per-item
   cost, identical to Python's per-yield call).
3. **`Arc::get_mut` failure semantics** — in practice tasks should not
   pre-share their output Arcs, but if a custom user task does, we silently
   skip stamping. Should we log at `warn!` level or fail the run? Python
   tolerates `_stamp_provenance` always succeeding, so logging is the
   parity-preserving choice.
4. **Does `Data.content_hash` actually propagate through Rust ingestion?**
   The model has the column ([`crates/models/src/data.rs`](../../crates/models/src/data.rs))
   but verify the ingestion pipeline writes it before cognify reads it for
   provenance propagation.
5. **Should non-pipeline call sites (CLI `add`, ingestion crate, memify)
   also stamp?** Python only stamps inside `run_tasks_base`, so anything
   emitted outside that path is unstamped in Python too. Mirror that
   conservatively.

---

## Testing strategy

1. **Unit tests for `stamp_tree`** in `crates/core/tests/provenance.rs`:
   port the eight cases from
   [`/tmp/cognee-python/cognee/tests/unit/modules/pipelines/test_provenance_stamping.py`](https://github.com/topoteretes/cognee/blob/main/cognee/tests/unit/modules/pipelines/test_provenance_stamping.py):
   - bare DataPoint stamping
   - non-overwrite of existing values
   - nested DataPoint recursion
   - list / tuple containers
   - visited-set short-circuits cycles
   - node_set inheritance / override
   - content_hash inheritance / override
   - mixed None and Some inputs.
2. **Pipeline integration test** in
   [`crates/core/tests/`](../../crates/core/tests/) — build a 3-task
   pipeline with mock tasks that emit DataPoints, run via `execute()`, and
   assert every output DataPoint has the expected `source_pipeline`/
   `source_task`/`source_user` after the run.
3. **Cognify E2E** — extend
   [`crates/cognify/tests/`](../../crates/cognify/tests/) to assert that
   `cognify_pipeline → cognify` produces graph nodes whose `source_task`
   covers all four expected stages.
4. **Cross-SDK parity** — see "Cross-SDK parity test" above.
5. **Vector-payload regression** — add an assertion in the existing vector-DB
   integration test that a freshly indexed point's payload contains
   `source_pipeline`, `source_task`, etc.

---

## References

- Python `_stamp_provenance` and the per-yield call site:
  [run_tasks_base.py](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py)
- Python DataPoint schema:
  [DataPoint.py](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/engine/models/DataPoint.py)
- Python visited-set:
  [PipelineContext.py](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineContext.py)
- Pre-stamp inside graph extraction:
  [extract_graph_from_data.py](https://github.com/topoteretes/cognee/blob/main/cognee/tasks/graph/extract_graph_from_data.py)
- Visualization consumer:
  [cognee_network_visualization.py](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py)
- Rust DataPoint:
  [crates/models/src/data_point.rs](../../crates/models/src/data_point.rs)
- Rust pipeline executor:
  [crates/core/src/pipeline.rs](../../crates/core/src/pipeline.rs)
- Rust local stamping helper:
  [crates/cognify/src/tasks.rs:1694](../../crates/cognify/src/tasks.rs#L1694)
- Rust audit-only `ExecStatusManager::stamp_provenance`:
  [crates/core/src/exec_status.rs:48](../../crates/core/src/exec_status.rs#L48)
- Rust `TaskContext` / `PipelineContext`:
  [crates/core/src/task_context.rs](../../crates/core/src/task_context.rs)
- Rust visualization template (consumer):
  [crates/visualization/src/lib.rs](../../crates/visualization/src/lib.rs)
- Existing telemetry gap analysis:
  [./gap-analysis.md](./gap-analysis.md)
