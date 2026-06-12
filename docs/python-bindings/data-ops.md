# Data Operations: forget, update, prune_data, prune_system

## Status: ✅ Implemented

## What is missing

Four deletion and cleanup operations:

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.forget(target, opts?)` | `cg_sdk_forget` | `cognee.forget()` | Delete a data item, a whole dataset, or everything |
| `Cognee.update(data_id, new_data, dataset, opts?)` | `cg_sdk_update` | `cognee.update()` | Replace a data item (delete + re-add + re-cognify) |
| `Cognee.prune_data()` | `cg_sdk_prune_data` | `cognee.pruneData()` | Remove all files from storage |
| `Cognee.prune_system(opts?)` | `cg_sdk_prune_system` | `cognee.pruneSystem()` | Selectively wipe graph, vector, metadata, cache |

### `forget` target shapes

The wire shape (after snake→camel normalisation) uses `camelCase` keys. Python callers may
pass either `snake_case` or `camelCase`; the binding normalises top-level keys before dispatch.

```python
# Delete a single data item within a dataset
# NOTE: wire key is "dataId" (camelCase); Python snake_case "data_id" is also accepted
{"kind": "item", "dataId": str, "dataset": {"name": str} | {"id": str}}

# Delete an entire dataset
{"kind": "dataset", "dataset": {"name": str} | {"id": str}}

# Delete everything for this owner
{"kind": "all"}
```

### `prune_system` options

Python callers may pass `snake_case` keys; the binding normalises them to `camelCase` before
dispatch. Wire keys are `camelCase`.

```python
opts = {
    "prune_graph": bool,      # wire: "pruneGraph",    default True
    "prune_vector": bool,     # wire: "pruneVector",   default True
    "prune_metadata": bool,   # wire: "pruneMetadata", default False — destructive, wipes relational DB
    "prune_cache": bool,      # wire: "pruneCache",    default True
}
```

### Result shapes

All result dicts use **camelCase** keys (matching the C API and TS wire shape).

**forget result**:
```python
{"target": str, "deleteResult": {...}}
```

**update result**:
```python
{
    "deletedDataId": str,
    "deleteResult": {...},
    "newData": [...],
    "cognifyResult": dict | None,
}
```

**prune_system result**:
```python
{
    "dataPruned": bool,
    "graphPruned": bool,
    "vectorPruned": bool,
    "metadataPruned": bool,
    "cachePruned": bool,
}
```

## Rationale

Data lifecycle management is essential for production use. Without `forget()`, there is no way to
remove stale data. Without `prune_system()`, there is no way to reset the system state during
development or testing. `update()` is needed when source data changes and the knowledge graph
must be rebuilt for that item.

## Implementation plan

**Prerequisite (Step 0 — not yet done):** hoist the forget/update/prune op bodies from
`capi/cognee-capi/src/sdk_data.rs` (and `js/cognee-neon/src/sdk_data.rs`) into a new
`crates/bindings-common/src/ops/data.rs` submodule, exposing them as
`cognee_bindings_common::ops::data::forget` / `::update` / `::prune_data` / `::prune_system`,
then register `pub mod data;` in `crates/bindings-common/src/ops/mod.rs`.
Both C API and Neon `run_*` private helpers contain the shared async logic; extract it
following the same pattern used for `pipeline.rs` and `retrieval.rs`.
See [core-pipeline-ops.md](core-pipeline-ops.md) Step 0 for the pattern.

### Step 1 — Create `python/src/sdk_data.rs`

The module follows the exact same structure as `python/src/sdk_retrieval.rs`.
Use `crate::json::opts_to_camel_json` (already implemented in T4) for all opts normalisation.
For `new_data` in `py_sdk_update`, define a local `normalise_inputs` helper (copy the private
function from `sdk_ops.rs` or re-export it — do not import directly as it is `fn` not `pub fn`).

```rust
use cognee_bindings_common::ops::data;  // ops::data hoisted in Step 0

pub fn py_sdk_forget<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    target: Bound<'py, PyAny>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    // Normalise snake_case keys (e.g. "data_id" → "dataId") before dispatch.
    // The shared op body also validates the "kind" field.
    let mut target_value = py_to_serde(&target)?;
    snake_to_camel_keys(&mut target_value);  // top-level key normalisation
    let opts_value = opts_to_camel_json(opts)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::forget(&handle, target_value, &opts_value)
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_update<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    data_id: String, new_data: Bound<'py, PyAny>, dataset_name: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let new_data_value = normalise_inputs(&new_data)?;  // wrap single object in array
    let opts_value = opts_to_camel_json(opts)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::update(&handle, &data_id, new_data_value, &dataset_name, &opts_value)
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_prune_data<'py>(py: Python<'py>, handle: Arc<HandleState>)
    -> PyResult<Bound<'py, PyAny>>
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        data::prune_data(&handle).await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| Ok(py.None()))
    })
}

pub fn py_sdk_prune_system<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts_to_camel_json(opts)?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::prune_system(&handle, &opts_value)
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}
```

### Step 2 — Validate `forget` target (optional early check)

The shared op body (`ops::data::forget`) already validates the `kind` field and returns
`SdkError::Validation`, which `sdk_error_to_py` converts to `CogneeValidationError`. An
explicit pre-check in the Python layer is optional but can improve the error message:

```rust
// Optional: use sdk_error_to_py indirectly by returning SdkError early,
// or simply let the shared op validate (recommended — avoids duplication).
// If an early check is desired, use:
//   Err(crate::sdk_error::CogneeValidationError::new_err(format!(...)))
// Note: CogneeValidationError requires `use crate::sdk_error::CogneeValidationError;`
```

### Step 3 — Wire into `PyCognee`

```rust
#[pyo3(signature = (target, opts=None))]
fn forget<'py>(&self, py: Python<'py>, target: Bound<'py, PyAny>,
               opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

#[pyo3(signature = (data_id, new_data, dataset_name, opts=None))]
fn update<'py>(&self, py: Python<'py>, data_id: String, new_data: Bound<'py, PyAny>,
               dataset_name: String, opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

fn prune_data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

#[pyo3(signature = (opts=None))]
fn prune_system<'py>(&self, py: Python<'py>, opts: Option<Bound<'py, PyAny>>)
    -> PyResult<Bound<'py, PyAny>> { /* ... */ }
```

### Step 4 — Key normalisation for all opts

`prune_metadata` → `pruneMetadata`, `data_id` → `dataId`, etc. Use
`crate::json::opts_to_camel_json` (implemented in T4, in `python/src/json.rs`).
The function named `snake_opts_to_serde` in retrieval-ops.md Step 3 was implemented as
`opts_to_camel_json` — use that name. Also wire `mod sdk_data;` into `python/src/lib.rs`
(add it alongside `mod sdk_ops;` and `mod sdk_retrieval;`).

### Step 5 — Tests

Add `python/tests/test_data_ops.py`:

```python
# NOTE: all result dicts use camelCase keys (matching C API / TS wire shape).
# The test helper _make_cognee and fixture patterns follow test_core_ops.py conventions.

async def test_forget_all(cognee_with_data):
    result = await cognee_with_data.forget({"kind": "all"})
    assert result["target"] == "all"  # "target" is a plain string in the wire shape

async def test_forget_dataset(cognee_with_data):
    result = await cognee_with_data.forget({"kind": "dataset", "dataset": {"name": "test_ds"}})
    assert result is not None

async def test_forget_bad_kind(cognee):
    with pytest.raises(cp.CogneeValidationError):
        await cognee.forget({"kind": "unknown"})

async def test_prune_data(cognee_with_data):
    await cognee_with_data.prune_data()  # should not raise (returns None)

async def test_prune_system(cognee_with_data):
    result = await cognee_with_data.prune_system({"prune_graph": True, "prune_vector": True})
    assert result["graphPruned"] is True   # camelCase key

async def test_update(cognee_with_data, data_id):
    result = await cognee_with_data.update(
        data_id, {"type": "text", "text": "Updated content"}, "test_ds"
    )
    assert "deletedDataId" in result      # camelCase key
    assert result["deletedDataId"] == data_id
```

### Acceptance criteria

- `await cognee.forget({"kind": "all"})` returns a result dict without raising
- `await cognee.prune_data()` returns `None`
- `await cognee.prune_system()` returns a dict with `graphPruned`, `vectorPruned`, etc. (camelCase)
- `forget` with an unknown `kind` raises `CogneeValidationError`
- `update` returns a dict with `deletedDataId` and `newData` (camelCase)
