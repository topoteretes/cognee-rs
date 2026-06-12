# Dataset Management

## Status: ✅ Implemented

## What is missing

Seven dataset and data CRUD operations:

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.datasets.list()` | `cg_sdk_list_datasets` | `cognee.datasets.list()` | List all datasets for the current owner |
| `Cognee.datasets.list_data(dataset_id)` | `cg_sdk_list_data` | `cognee.datasets.listData()` | List data items in a dataset |
| `Cognee.datasets.has_data(dataset_id)` | `cg_sdk_has_data` | `cognee.datasets.has()` | Check if a dataset has any data |
| `Cognee.datasets.status(dataset_ids)` | `cg_sdk_dataset_status` | `cognee.datasets.status()` | Get pipeline status for datasets |
| `Cognee.datasets.empty(dataset_id)` | `cg_sdk_empty_dataset` | `cognee.datasets.empty()` | Remove all data from a dataset and delete the dataset record |
| `Cognee.datasets.delete_data(dataset_id, data_id, opts?)` | `cg_sdk_delete_data` | `cognee.datasets.deleteData()` | Delete a single data item |
| `Cognee.datasets.delete_all()` | `cg_sdk_delete_all_datasets` | `cognee.datasets.deleteAll()` | Delete all datasets |

### `delete_data` options

```python
opts = {
    "soft_delete": bool,               # default False
    "delete_dataset_if_empty": bool,   # default False
}
```

### `dataset_status` result

Returns a `dict[str, str]` mapping dataset UUID → pipeline status string:
```python
{
    "<uuid>": "INITIATED" | "STARTED" | "COMPLETED" | "ERRORED"
}
```

## Rationale

Dataset management is required for any multi-dataset application: listing what data has been
ingested, checking whether a dataset is ready (pipeline status), and cleaning up. These are
also the operations the HTTP server routes expose under `/api/v1/datasets/`.

The TS binding exposes these as a sub-object `cognee.datasets`. Python should use the same
sub-object pattern for discoverability.

## Implementation plan

**Prerequisite:** hoist the dataset op bodies from `capi/cognee-capi/src/sdk_datasets.rs` (and
the neon counterpart `js/cognee-neon/src/sdk_datasets.rs`) into a new
`crates/bindings-common/src/ops/datasets.rs` module, then add `pub mod datasets;` to
`crates/bindings-common/src/ops/mod.rs` — see [core-pipeline-ops.md](core-pipeline-ops.md)
Step 0. The underlying logic is `DatasetManager` from `cognee_lib::api`, so these op bodies are
thin; per-binding porting (duplicating the async logic directly into `python/src/sdk_datasets.rs`
without hoisting) is also viable here if the hoist is deferred.

### Step 1 — Create `python/src/sdk_datasets.rs`

```rust
pub fn py_sdk_list_datasets<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = ops::list_datasets(&handle).await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

// Similarly for list_data, has_data, dataset_status, empty_dataset, delete_data, delete_all_datasets
```

`has_data` returns a JSON bool — convert to Python `bool`.
`dataset_status` returns a JSON object — convert to a Python dict.

### Step 2 — Define `PyCogneeDatasets` sub-object

```rust
#[pyclass(name = "CogneeDatasets")]
pub struct PyCogneeDatasets {
    inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeDatasets {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        sdk_datasets::py_sdk_list_datasets(py, Arc::clone(&self.inner))
    }

    fn list_data<'py>(&self, py: Python<'py>, dataset_id: String)
        -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    fn has<'py>(&self, py: Python<'py>, dataset_id: String)
        -> PyResult<Bound<'py, PyAny>> { /* returns bool */ }

    fn status<'py>(&self, py: Python<'py>, dataset_ids: Bound<'py, PyAny>)
        -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    fn empty<'py>(&self, py: Python<'py>, dataset_id: String)
        -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    #[pyo3(signature = (dataset_id, data_id, opts=None))]
    fn delete_data<'py>(&self, py: Python<'py>, dataset_id: String, data_id: String,
                        opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    fn delete_all<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }
}
```

### Step 3 — Attach to `PyCognee` as a property

Add the field to the struct in `python/src/sdk.rs` and initialise it in `PyCognee::new()`:

```rust
#[pyclass(name = "Cognee")]
pub struct PyCognee {
    pub(crate) inner: Arc<HandleState>,
    config: Py<PyCogneeConfig>,
    datasets: Py<PyCogneeDatasets>,   // ← new
}

#[pymethods]
impl PyCognee {
    #[new]
    #[pyo3(signature = (settings=None))]
    fn new(py: Python<'_>, settings: Option<&str>) -> PyResult<Self> {
        // ... existing overlay logic ...
        let datasets = Py::new(py, PyCogneeDatasets { inner: Arc::clone(&inner) })?;
        Ok(Self { inner, config, datasets })
    }

    #[getter]
    fn datasets(&self, py: Python<'_>) -> Py<PyCogneeDatasets> {
        self.datasets.clone_ref(py)
    }
}
```

Also add `PyCogneeDatasets` to `m.add_class::<...>()` in `python/src/lib.rs` and add
`mod sdk_datasets;` to the module declarations.

### Step 4 — UUID validation helper

```rust
fn validate_uuid(s: &str, field: &str) -> PyResult<Uuid> {
    Uuid::parse_str(s).map_err(|_| {
        CogneeValidationError::new_err(format!("'{}' is not a valid UUID: {}", field, s))
    })
}
```

Use this before passing `dataset_id` / `data_id` to Rust to surface clearer errors.

### Step 5 — Tests

Add `python/tests/test_dataset_management.py`:

```python
async def test_list_datasets_empty(cognee):
    result = await cognee.datasets.list()
    assert isinstance(result, list)

async def test_has_data_false(cognee):
    import uuid
    ds_id = str(uuid.uuid4())
    result = await cognee.datasets.has(ds_id)
    assert result is False

async def test_list_and_has_after_add(cognee):
    await cognee.add({"type": "text", "text": "X"}, "my_ds")
    datasets = await cognee.datasets.list()
    assert any(ds.get("name") == "my_ds" for ds in datasets)

async def test_dataset_status(cognee):
    await cognee.add({"type": "text", "text": "X"}, "my_ds")
    datasets = await cognee.datasets.list()
    ds_id = datasets[0]["id"]
    status = await cognee.datasets.status([ds_id])
    assert ds_id in status

async def test_empty_dataset(cognee):
    await cognee.add({"type": "text", "text": "X"}, "my_ds")
    datasets = await cognee.datasets.list()
    ds_id = datasets[0]["id"]
    result = await cognee.datasets.empty(ds_id)
    assert result is not None

async def test_delete_all(cognee_with_data):
    result = await cognee_with_data.datasets.delete_all()
    assert isinstance(result, list)
```

### Acceptance criteria

- `await cognee.datasets.list()` returns a list
- `await cognee.datasets.has(str(uuid4()))` returns `False` for a non-existent dataset UUID
- `await cognee.datasets.status([...])` returns a `dict` keyed by dataset UUID
- `cognee.datasets` attribute is accessible on every `Cognee` instance
