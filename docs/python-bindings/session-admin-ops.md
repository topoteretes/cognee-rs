# Session, Admin, and Notebook Operations

## Status: ❌ Not implemented

## What is missing

### Session operations

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.sessions.get(session_id, opts?)` | `cg_sdk_get_session` | `cognee.sessions.get()` | Retrieve QA history for a session |
| `Cognee.sessions.add_feedback(session_id, qa_id, opts?)` | `cg_sdk_add_feedback` | `cognee.sessions.addFeedback()` | Attach feedback to a QA entry |
| `Cognee.sessions.delete_feedback(session_id, qa_id)` | `cg_sdk_delete_feedback` | `cognee.sessions.deleteFeedback()` | Remove feedback from a QA entry |
| `Cognee.sessions.get_graph_context(session_id)` | `cg_sdk_get_graph_context` | `cognee.sessions.getGraphContext()` | Get stored graph context snapshot |
| `Cognee.sessions.set_graph_context(session_id, context)` | `cg_sdk_set_graph_context` | `cognee.sessions.setGraphContext()` | Store a graph context snapshot |

### Admin operations

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.reset_pipeline_run_status(dataset_id, pipeline_name)` | `cg_sdk_reset_pipeline_run_status` | `cognee.resetPipelineRunStatus()` | Re-arm a pipeline run (insert INITIATED row) |
| `Cognee.reset_dataset_pipeline_run_status(dataset_id)` | `cg_sdk_reset_dataset_pipeline_run_status` | `cognee.resetDatasetPipelineRunStatus()` | Reset all pipeline runs for a dataset |
| `Cognee.get_or_create_default_user()` | `cg_sdk_get_or_create_default_user` | `cognee.getOrCreateDefaultUser()` | Deterministic UUID5 user creation from email |

### Notebook operations

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.notebooks.list()` | `cg_sdk_list_notebooks` | `cognee.listNotebooks()` | List all notebooks for the owner |
| `Cognee.notebooks.create(name, cells?, deletable?)` | `cg_sdk_create_notebook` | `cognee.createNotebook()` | Create a new notebook |
| `Cognee.notebooks.update(id, patch)` | `cg_sdk_update_notebook` | `cognee.updateNotebook()` | Patch name and/or cells |
| `Cognee.notebooks.delete(id)` | `cg_sdk_delete_notebook` | `cognee.deleteNotebook()` | Delete a notebook |

### Options

**`get_session` opts**:
```python
{"last_n": int}   # return only the last N entries
```

**`add_feedback` opts**:
```python
{"feedback_text": str | None, "feedback_score": int | None}
```

### Result shapes

**`get_session`**: list of `SessionQAEntry` dicts.

**`add_feedback` / `delete_feedback`**: `bool` (True if successful).

**`get_graph_context`**: `str | None` (the raw context string).

**`get_or_create_default_user`**: `User` dict with `id`, `email`, and other fields.

**`list_notebooks`**: list of `Notebook` dicts.

**`create_notebook` / `update_notebook`**: `Notebook` dict.

**`delete_notebook`**: `bool` (True if deleted, False if not found).

## Rationale

Sessions are the backbone of conversational AI applications built on cognee: they record Q&A
interactions, store feedback, and maintain graph context per conversation turn. The session
operations are required before `improve()` can be meaningful.

Notebook operations are used by the HTTP server's `/api/v1/notebooks/` routes and are needed for
IDE/Jupyter integrations. Pipeline run status reset is an operational tool for recovering from
partial failures.

## Implementation plan

**Prerequisite:** hoist the session/admin/notebook op bodies from
`capi/cognee-capi/src/sdk_admin.rs` (and the neon counterpart) into
`cognee_bindings_common::ops` — see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0. The
underlying logic is mostly thin calls into `cognee_lib::session` (`get_session`, `add_feedback`,
`delete_feedback`, `get_graph_context`, `set_graph_context`) and `cognee_lib::api`
(`list_notebooks`, `create_notebook`, `update_notebook`, `delete_notebook`,
`reset_pipeline_run_status`, `get_or_create_default_user`) using services from
`handle.services().await`.

### Step 1 — Create `python/src/sdk_sessions.rs`

```rust
#[pyclass(name = "CogneeSessions")]
pub struct PyCogneeSessions {
    inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeSessions {
    #[pyo3(signature = (session_id, opts=None))]
    fn get<'py>(&self, py: Python<'py>, session_id: String,
                opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = ops::get_session(&handle, &session_id, opts_value.as_ref())
                .await.map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    #[pyo3(signature = (session_id, qa_id, opts=None))]
    fn add_feedback<'py>(&self, py: Python<'py>, session_id: String, qa_id: String,
                         opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
        // result is "true"/"false" — deserialise to Python bool
        /* ... */
    }

    fn delete_feedback<'py>(&self, py: Python<'py>, session_id: String, qa_id: String)
        -> PyResult<Bound<'py, PyAny>> { /* bool result */ }

    fn get_graph_context<'py>(&self, py: Python<'py>, session_id: String)
        -> PyResult<Bound<'py, PyAny>> {
        // result is "\"<ctx>\"" (quoted JSON string) or "null" — deserialise appropriately
        /* ... */
    }

    fn set_graph_context<'py>(&self, py: Python<'py>, session_id: String, context: String)
        -> PyResult<Bound<'py, PyAny>> { /* returns None */ }
}
```

### Step 2 — Create `python/src/sdk_admin.rs`

Separate module for admin and notebook ops to keep file size manageable:

```rust
// Admin ops wired directly as methods on PyCognee (not a sub-object)
pub fn py_sdk_reset_pipeline_run_status<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }
pub fn py_sdk_reset_dataset_pipeline_run_status<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }
pub fn py_sdk_get_or_create_default_user<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

// Notebook ops as PyCogneeNotebooks sub-object
#[pyclass(name = "CogneeNotebooks")]
pub struct PyCogneeNotebooks { inner: Arc<HandleState> }

#[pymethods]
impl PyCogneeNotebooks {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    #[pyo3(signature = (name, cells=None, deletable=true))]
    fn create<'py>(&self, py: Python<'py>, name: String,
                   cells: Option<Bound<'py, PyAny>>,
                   deletable: bool) -> PyResult<Bound<'py, PyAny>> {
        let cells_value = cells.map(|c| py_to_serde(&c)).transpose()?;
        /* ... */
    }

    fn update<'py>(&self, py: Python<'py>, id: String, patch: Bound<'py, PyAny>)
        -> PyResult<Bound<'py, PyAny>> { /* ... */ }

    fn delete<'py>(&self, py: Python<'py>, id: String)
        -> PyResult<Bound<'py, PyAny>> { /* returns bool */ }
}
```

### Step 3 — Attach sub-objects to `PyCognee`

```rust
pub struct PyCognee {
    inner: Arc<HandleState>,
    config: Py<PyCogneeConfig>,
    datasets: Py<PyCogneeDatasets>,
    sessions: Py<PyCogneeSessions>,
    notebooks: Py<PyCogneeNotebooks>,
}

// Getter properties for sessions, notebooks
// Direct methods on PyCognee for admin ops:
fn reset_pipeline_run_status<'py>(&self, py, dataset_id, pipeline_name) -> ...
fn reset_dataset_pipeline_run_status<'py>(&self, py, dataset_id) -> ...
fn get_or_create_default_user<'py>(&self, py) -> ...
```

### Step 4 — JSON result handling for nullable and bool results

The C API's D9 wire contract serialises these as raw JSON text (`"true"`, `"null"`, quoted
strings) because its boundary is `char*`. With the hoisted serde-value ops, the Python layer gets
`serde_json::Value` directly, so the mapping is trivial and `serde_to_py` already covers it:

- `Value::Bool(b)` → Python `True` / `False` (add_feedback, delete_feedback, delete_notebook, has_data)
- `Value::Null` → Python `None` (set_graph_context, get_graph_context when unset, void ops)
- `Value::String(s)` → Python `str` (get_graph_context when set)

No extra helpers are needed beyond `serde_to_py`.

### Step 5 — Tests

Add `python/tests/test_session_ops.py`:

```python
async def test_get_session_empty(cognee):
    result = await cognee.sessions.get("nonexistent-session")
    assert isinstance(result, list)
    assert len(result) == 0

async def test_get_graph_context_none(cognee):
    result = await cognee.sessions.get_graph_context("nonexistent-session")
    assert result is None

async def test_set_and_get_graph_context(cognee):
    await cognee.sessions.set_graph_context("my-session", "some context")
    result = await cognee.sessions.get_graph_context("my-session")
    assert result == "some context"

async def test_get_or_create_default_user(cognee):
    user = await cognee.get_or_create_default_user()
    assert "id" in user
```

Add `python/tests/test_notebook_ops.py`:

```python
async def test_list_notebooks_empty(cognee):
    result = await cognee.notebooks.list()
    assert isinstance(result, list)

async def test_create_and_delete_notebook(cognee):
    nb = await cognee.notebooks.create("My Notebook")
    assert nb["name"] == "My Notebook"
    deleted = await cognee.notebooks.delete(nb["id"])
    assert deleted is True

async def test_delete_nonexistent_notebook(cognee):
    import uuid
    deleted = await cognee.notebooks.delete(str(uuid.uuid4()))
    assert deleted is False
```

### Acceptance criteria

- `await cognee.sessions.get("any-id")` returns a list (may be empty)
- `await cognee.sessions.get_graph_context("any-id")` returns `None` for a new session
- `await cognee.sessions.set_graph_context("s", "ctx")` then `get_graph_context("s")` returns `"ctx"`
- `await cognee.get_or_create_default_user()` returns a dict with an `id` field
- `await cognee.notebooks.create("name")` returns a Notebook dict
- `await cognee.notebooks.delete(id)` returns `True` or `False`
