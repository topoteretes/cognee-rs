# Data Operations: forget, update, prune_data, prune_system

## Status: ❌ Not implemented

## What is missing

Four deletion and cleanup operations:

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.forget(target, opts?)` | `cg_sdk_forget` | `cognee.forget()` | Delete a data item, a whole dataset, or everything |
| `Cognee.update(data_id, new_data, dataset, opts?)` | `cg_sdk_update` | `cognee.update()` | Replace a data item (delete + re-add + re-cognify) |
| `Cognee.prune_data()` | `cg_sdk_prune_data` | `cognee.pruneData()` | Remove all files from storage |
| `Cognee.prune_system(opts?)` | `cg_sdk_prune_system` | `cognee.pruneSystem()` | Selectively wipe graph, vector, metadata, cache |

### `forget` target shapes

```python
# Delete a single data item within a dataset
{"kind": "item", "data_id": str, "dataset": {"name": str} | {"id": str}}

# Delete an entire dataset
{"kind": "dataset", "dataset": {"name": str} | {"id": str}}

# Delete everything for this owner
{"kind": "all"}
```

### `prune_system` options

```python
opts = {
    "prune_graph": bool,      # default True
    "prune_vector": bool,     # default True
    "prune_metadata": bool,   # default False — destructive, wipes relational DB
    "prune_cache": bool,      # default True
}
```

### Result shapes

**forget result**:
```python
{"target": str, "delete_result": {...}}
```

**update result**:
```python
{
    "deleted_data_id": str,
    "delete_result": {...},
    "new_data": [...],
    "cognify_result": dict | None,
}
```

**prune_system result**:
```python
{
    "data_pruned": bool,
    "graph_pruned": bool,
    "vector_pruned": bool,
    "metadata_pruned": bool,
    "cache_pruned": bool,
}
```

## Rationale

Data lifecycle management is essential for production use. Without `forget()`, there is no way to
remove stale data. Without `prune_system()`, there is no way to reset the system state during
development or testing. `update()` is needed when source data changes and the knowledge graph
must be rebuilt for that item.

## Implementation plan

**Prerequisite:** hoist the forget/update/prune op bodies from
`capi/cognee-capi/src/sdk_data.rs` (and the neon counterpart) into
`cognee_bindings_common::ops` — see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0.

### Step 1 — Create `python/src/sdk_data.rs`

```rust
pub fn py_sdk_forget<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    target: Bound<'py, PyAny>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    // Validate "kind" field before dispatch (optional — the op body also validates)
    validate_forget_target(&target)?;
    let target_value = py_to_serde(&target)?;
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = ops::forget(&handle, &target_value, opts_value.as_ref())
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_update<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    data_id: String, new_data: Bound<'py, PyAny>, dataset_name: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let new_data_value = normalise_inputs(&new_data)?;
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = ops::update(&handle, &data_id, &new_data_value, &dataset_name,
                                 opts_value.as_ref())
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_prune_data<'py>(py: Python<'py>, handle: Arc<HandleState>)
    -> PyResult<Bound<'py, PyAny>>
{
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        ops::prune_data(&handle).await.map_err(sdk_error_to_py)?;
        Ok(())
    })
}

pub fn py_sdk_prune_system<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* standard pattern, ops::prune_system */ }
```

### Step 2 — Validate `forget` target

```rust
fn validate_forget_target(target: &Bound<'_, PyAny>) -> PyResult<()> {
    let kind: String = target.get_item("kind")?.extract()?;
    match kind.as_str() {
        "item" | "dataset" | "all" => Ok(()),
        other => Err(CogneeValidationError::new_err(format!(
            "unknown forget target kind '{}'; expected item, dataset, or all", other
        ))),
    }
}
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

### Step 4 — Key normalisation for `prune_system` opts

`prune_metadata` → `pruneMetadata` etc. Use `snake_opts_to_serde` from the marshal module
(defined in [retrieval-ops.md](retrieval-ops.md) Step 3).

### Step 5 — Tests

Add `python/tests/test_data_ops.py`:

```python
async def test_forget_all(cognee_with_data):
    result = await cognee_with_data.forget({"kind": "all"})
    assert result["target"] == "all"

async def test_forget_dataset(cognee_with_data):
    result = await cognee_with_data.forget({"kind": "dataset", "dataset": {"name": "test_ds"}})
    assert result is not None

async def test_forget_bad_kind(cognee):
    with pytest.raises(CogneeValidationError):
        await cognee.forget({"kind": "unknown"})

async def test_prune_data(cognee_with_data):
    await cognee_with_data.prune_data()  # should not raise

async def test_prune_system(cognee_with_data):
    result = await cognee_with_data.prune_system({"prune_graph": True, "prune_vector": True})
    assert result["graph_pruned"] is True

async def test_update(cognee_with_data, data_id):
    result = await cognee_with_data.update(
        data_id, {"type": "text", "text": "Updated content"}, "test_ds"
    )
    assert "deleted_data_id" in result
    assert result["deleted_data_id"] == data_id
```

### Acceptance criteria

- `await cognee.forget({"kind": "all"})` returns a result dict without raising
- `await cognee.prune_data()` returns `None`
- `await cognee.prune_system()` returns a dict with `graph_pruned`, `vector_pruned`, etc.
- `forget` with an unknown `kind` raises `CogneeValidationError`
- `update` returns a dict with `deleted_data_id` and `new_data`
