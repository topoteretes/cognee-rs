# Core Pipeline Operations: add, cognify, add_and_cognify

## Status: ❌ Not implemented

## What is missing

The three primary SDK operations that form the backbone of the add→cognify→search workflow:

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.add(inputs, dataset, opts?)` | `cg_sdk_add` | `cognee.add()` | Ingest data into a named dataset |
| `Cognee.cognify(dataset, opts?)` | `cg_sdk_cognify` | `cognee.cognify()` | Extract knowledge graph from a dataset |
| `Cognee.add_and_cognify(inputs, dataset, opts?)` | `cg_sdk_add_and_cognify` | `cognee.addAndCognify()` | Combined convenience method |

### Data input types

All three accept a `CogneeDataInput` — a discriminated union on `type`:

```python
# Text literal
{"type": "text", "text": "..."}

# Local file path
{"type": "file", "path": "/absolute/path/to/file.pdf"}

# URL (html-loader feature required)
{"type": "url", "url": "https://..."}

# Binary blob
{"type": "binary", "bytes": b"...", "name": "filename.txt"}
```

`s3` and `dataItem` variants are recognized but return `CogneeUnsupportedError`.

### Result types

**Add result** (maps to `CogneeAddResult`): all keys are **camelCase** (matches TS/C API parity):
```python
{
    "datasetName": str,
    "added": [...],           # list of Data objects
    "addedCount": int,
    "deduplicated": [...],    # items skipped as duplicates
    "deduplicatedCount": int,
}
```

**Cognify result** (maps to `CogneeCognifyResult`): all keys are **camelCase**:
```python
{
    "chunks": int,
    "entities": int,
    "edges": int,
    "summaries": int,
    "embeddings": int,
    "alreadyCompleted": bool,
    "priorPipelineRunId": str | None,
}
```

**Cognify options** (`opts` dict): all keys are **camelCase** (matches C API and TS surface):
- `tenant` (str UUID) — tenant isolation
- `chunkSize` (int)
- `chunkOverlap` (int)
- `summarization` (bool)
- `temporalCognify` (bool)
- `triplet` (bool) — index triplet embeddings during cognify

## Rationale

These are the first operations any user runs after `warm()`. Without them, the Python SDK is
useless for its primary purpose. They are the highest-priority items after the SDK handle and
config surface.

**Important architectural fact (verified against code):** the op bodies do *not* live in
`cognee-bindings-common`. That crate only provides `HandleState`, `CogneeServices`, `SdkError`,
and a handful of wire helpers (`marshal_inputs`, `marshal_one`, `marshal_bytes`,
`cognify_result_json` in `crates/bindings-common/src/wire.rs`). The actual operation logic —
input marshaling, dataset resolution, pipeline invocation, result-JSON assembly — is currently
**duplicated** between the C API (`capi/cognee-capi/src/sdk_ops.rs`, ~565 lines for just these
three ops: `run_add`, `run_cognify`, `run_add_and_cognify`) and the Neon binding
(`js/cognee-neon/src/sdk_ops.rs`). A Python port must not create a third copy.

## Implementation plan

### Step 0 — Hoist the shared op bodies into `cognee-bindings-common`

This is the foundational step for *all* SDK-op plan documents (retrieval, memory, data, datasets,
session-admin, visualization reference it as a prerequisite).

Extract the private `async fn run_add / run_cognify / run_add_and_cognify` helpers (and their
analogues in the other capi `sdk_*.rs` modules) into a new `ops` module in
`cognee-bindings-common`, with JSON-in/JSON-out signatures:

```rust
// crates/bindings-common/src/ops/pipeline.rs
pub async fn add(
    state: &Arc<HandleState>,
    inputs: &serde_json::Value,
    dataset_name: &str,
    opts: Option<&serde_json::Value>,
) -> Result<serde_json::Value, SdkError> { /* moved from capi run_add */ }

pub async fn cognify(...) -> Result<serde_json::Value, SdkError> { /* run_cognify */ }
pub async fn add_and_cognify(...) -> Result<serde_json::Value, SdkError> { /* run_add_and_cognify */ }
```

The capi functions already take JSON values and return JSON, so this is a mechanical move; then
rewrite capi and neon to call the shared functions and delete their local copies. This refactor
must keep `capi/scripts/check.sh` and `js/scripts/check.sh` green (both run in
`scripts/check_all.sh`), which doubles as the regression suite for the move.

If the hoist is deemed too risky as a first step, the fallback is to port the logic a third time
into `python/src/` — acceptable short-term, but it triples the maintenance surface and is exactly
the drift the bindings-common crate was created to prevent.

### Step 1 — Python-to-JSON helpers (already implemented — no action needed)

`python/src/json.rs` already provides `py_to_serde` and `serde_to_py` (and `py_to_serde_map`) as
`pub(crate)` functions using a direct PyObject tree walk (faster and more correct than a
`json.dumps` round-trip). These are the helpers to use in `sdk_ops.rs`:

```rust
// In python/src/sdk_ops.rs:
use crate::json::{py_to_serde, serde_to_py};
```

No new file or new functions are needed for this step.

### Step 2 — Create `python/src/sdk_ops.rs`

```rust
use pyo3::prelude::*;
use std::sync::Arc;
use cognee_bindings_common::{HandleState, ops};
use crate::json::{py_to_serde, serde_to_py};
use crate::sdk_error::sdk_error_to_py;

/// Called as a method on PyCognee. Signature:
///   async def add(self, inputs, dataset_name, opts=None) -> dict
pub fn py_sdk_add<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    inputs: Bound<'py, PyAny>,
    dataset_name: &str,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let inputs_value: serde_json::Value = py_to_serde(&inputs)?;
    let opts_value: Option<serde_json::Value> = opts.map(|o| py_to_serde(&o)).transpose()?;
    let dataset = dataset_name.to_owned();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        // ops::add is the function hoisted from capi's run_add in Step 0.
        let result = ops::add(&handle, &inputs_value, &dataset, opts_value.as_ref())
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

// Similar structure for py_sdk_cognify and py_sdk_add_and_cognify
```

Note the helpers convert between Python objects and `serde_json::Value` using the direct tree-walk
functions in `crate::json` — the shared op functions take serde values, not strings, so the Python
layer converts at the boundary and passes typed values through.

### Step 3 — Wire into `PyCognee`

Add `#[pymethods]` to `PyCognee` in `sdk.rs` (or a separate `impl PyCognee` block in
`sdk_ops.rs`):

```rust
#[pymethods]
impl PyCognee {
    #[pyo3(signature = (inputs, dataset_name, opts=None))]
    fn add<'py>(
        &self,
        py: Python<'py>,
        inputs: Bound<'py, PyAny>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        sdk_ops::py_sdk_add(py, Arc::clone(&self.inner), inputs, dataset_name, opts)
    }

    #[pyo3(signature = (dataset_name, opts=None))]
    fn cognify<'py>(
        &self,
        py: Python<'py>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        sdk_ops::py_sdk_cognify(py, Arc::clone(&self.inner), dataset_name, opts)
    }

    #[pyo3(signature = (inputs, dataset_name, opts=None))]
    fn add_and_cognify<'py>(
        &self,
        py: Python<'py>,
        inputs: Bound<'py, PyAny>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        sdk_ops::py_sdk_add_and_cognify(py, Arc::clone(&self.inner), inputs, dataset_name, opts)
    }
}
```

### Step 4 — Handle `inputs` as a single dict or a list of dicts

Python users will naturally pass either a single `{"type": "text", ...}` or a list. Add
normalisation in `py_sdk_add`:

```rust
// If inputs is a dict, wrap in a single-element array before dispatch
let mut inputs_value = py_to_serde(&inputs)?;
if inputs_value.is_object() {
    inputs_value = serde_json::Value::Array(vec![inputs_value]);
}
```

### Step 5 — Add result key normalisation (optional)

The JSON result from Rust uses camelCase keys (`addedCount`, `deduplicatedCount`). Python
convention is snake_case. Consider a thin normalisation pass using a `camel_to_snake` dict
transformer, or document that the returned dict uses camelCase keys (matching TS parity).

The simplest approach (matching TS): return the dict as-is with camelCase keys and document it.

### Step 6 — Tests

Add `python/tests/test_core_ops.py` (requires `MOCK_EMBEDDING=true`, no real LLM needed for add).

Note: `python/tests/conftest.py` currently only has a `ctx` fixture (for PyTaskContext). The test
file must define its own `cognee` fixture (or add one to `conftest.py`):

```python
import os, pytest, cognee_pipeline as cp

@pytest.fixture
async def cognee(tmp_path):
    """Fresh Cognee handle per test, using a tmp dir for isolation."""
    c = cp.Cognee(f'{{"db_path": "{tmp_path}/cognee.db"}}')
    await c.warm()
    return c

@pytest.mark.asyncio
async def test_add_text(cognee):
    result = await cognee.add({"type": "text", "text": "Hello world"}, "test_ds")
    assert result["addedCount"] == 1

@pytest.mark.asyncio
async def test_add_list(cognee):
    inputs = [{"type": "text", "text": "A"}, {"type": "text", "text": "B"}]
    result = await cognee.add(inputs, "test_ds")
    assert result["addedCount"] == 2

@pytest.mark.asyncio
async def test_deduplicate(cognee):
    await cognee.add({"type": "text", "text": "Same"}, "ds")
    result = await cognee.add({"type": "text", "text": "Same"}, "ds")
    assert result["deduplicatedCount"] == 1

@pytest.mark.asyncio
async def test_unsupported_type(cognee):
    with pytest.raises(cp.CogneeUnsupportedError):
        await cognee.add({"type": "s3", "bucket": "foo", "key": "bar"}, "ds")
```

### Acceptance criteria

- `await cognee.add({"type": "text", "text": "hello"}, "demo")` returns a dict with `addedCount`
- `await cognee.cognify("demo")` returns a dict with `chunks`, `entities`, `edges`
- `await cognee.add_and_cognify(...)` returns a dict with `add` and `cognify` sub-keys
- Unsupported input types raise `CogneeUnsupportedError`
- Duplicate content returns `deduplicatedCount > 0` on second call
