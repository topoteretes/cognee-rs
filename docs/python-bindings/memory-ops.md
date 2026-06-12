# Memory Operations: remember, remember_entry, memify, improve

## Status: ✅ Implemented

## What is missing

Four higher-level memory management operations:

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.remember(inputs, dataset, opts?)` | `cg_sdk_remember` | `cognee.remember()` | One-call add + cognify + optional self-improvement |
| `Cognee.remember_entry(entry, dataset, session_id, opts?)` | `cg_sdk_remember_entry` | `cognee.rememberEntry()` | Store a typed memory entry (QA, trace, or feedback) |
| `Cognee.memify(opts?)` | `cg_sdk_memify` | `cognee.memify()` | Build triplet embeddings over the entire graph |
| `Cognee.improve(opts)` | `cg_sdk_improve` | `cognee.improve()` | Four-stage session-to-graph bridge |

### `remember` options

```python
opts = {
    "sessionId": str,           # camelCase — matches C API / TS wire shape
    "selfImprovement": bool,    # camelCase
    "tenant": str,              # UUID
}
```

### `remember_entry` entry types

Shapes verified against the C API parser (`capi/cognee-capi/src/sdk_memory.rs`) and the TS type
`CogneeMemoryEntry` (`js/src/types.ts`). Keys are camelCase on the wire:

```python
# QA interaction — all fields optional
{"type": "qa", "question": str, "answer": str, "context": str,
 "feedbackText": str, "feedbackScore": int, "usedGraphElementIds": dict}

# Execution trace — originFunction is REQUIRED, the rest optional
{"type": "trace", "originFunction": str, "status": str,           # status defaults to "success"
 "methodParams": Any, "methodReturnValue": Any,
 "memoryQuery": str, "memoryContext": str, "errorMessage": str,
 "generateFeedbackWithLlm": bool}                                  # defaults to False

# User feedback — qaId is REQUIRED
{"type": "feedback", "qaId": str, "feedbackText": str, "feedbackScore": int}
```

### `memify` options

```python
opts = {
    "tripletBatchSize": int,             # camelCase — matches C API / TS wire shape
    "nodeTypeFilter": str,               # camelCase
    "nodeNameFilter": [str],             # camelCase; array of strings (not a single str)
    "nodeNameFilterOperator": str,       # camelCase; "AND" | "OR"
}
```

### `improve` options (required: `datasetName`)

```python
opts = {
    "datasetName": str,                  # required; camelCase — matches C API / TS wire shape
    "sessionIds": [str],                 # camelCase
    "nodeName": [str],                   # camelCase
    "feedbackAlpha": float,              # camelCase; default 0.1
    "tenant": str,
}
```

### Result shapes

**remember result**: Pass-through JSON dict (complex structure with status, dataset info, session IDs, pipeline run info).

**memify result** (all keys camelCase — matches C API / TS / neon wire shape):
```python
{
    "tripletCount": int,
    "indexedCount": int,
    "batchCount": int,
    "alreadyCompleted": bool,
    "priorPipelineRunId": str | None,
}
```

**improve result** (all keys camelCase):
```python
{
    "stagesRun": [str],
    "memifyResult": dict | None,
    "feedbackEntriesProcessed": int,
    "feedbackEntriesApplied": int,
    "sessionsPersisted": int,
    "edgesSynced": int,
}
```

## Rationale

`remember()` is the primary high-level entry point for conversational memory applications — it
replaces the manual add → cognify → (optionally) memify sequence with a single call. `memify()` is
needed independently whenever triplet-based search (`TRIPLET_COMPLETION`) is required after a
manual cognify. `improve()` closes the feedback loop from user corrections back into the knowledge
graph. `remember_entry()` enables fine-grained memory injection (e.g., storing a QA pair from a
previous conversation).

## Implementation plan

**Prerequisite:** T3 (`ops::pipeline`) is done — `crates/bindings-common/src/ops/` already has
`pipeline.rs`, `data.rs`, `datasets.rs`, and `retrieval.rs`. This task must add a new
`crates/bindings-common/src/ops/memory.rs` module (and register it in `mod.rs`) containing the
shared async bodies `run_remember`, `run_remember_entry`, `run_memify_op`, and `run_improve`
extracted from `capi/cognee-capi/src/sdk_memory.rs` and `js/cognee-neon/src/sdk_memory.rs`.
The sketches below call those hoisted functions as `ops::memory::run_remember`, etc.

> **Note:** The C API's `sdk_memory.rs` uses local `run_*` helpers (not in bindings-common) and
> the neon `sdk_memory.rs` likewise calls `cognee_lib::api::{remember, remember_entry}` and
> `cognee_lib::cognify::run_memify` directly. The hoist is the Step 0 for this task.

### Step 1 — Create `crates/bindings-common/src/ops/memory.rs` and `python/src/sdk_memory.rs`

First, add `pub mod memory;` to `crates/bindings-common/src/ops/mod.rs` and create
`crates/bindings-common/src/ops/memory.rs` by extracting the four `run_*` async helpers from
`capi/cognee-capi/src/sdk_memory.rs` (they already have the right signature and import set).
The `marshal_memory_entry` helper and `memify_result_json` helper also move there.
Export them as `pub async fn run_remember(...)`, `pub async fn run_remember_entry(...)`, etc.

Then create `python/src/sdk_memory.rs` following the same pattern as `sdk_ops.rs`:

```rust
use cognee_bindings_common::ops::memory;
// imports: Arc<HandleState>, py_to_serde, serde_to_py, sdk_error_to_py, opts_to_json (from sdk_ops)

pub fn py_sdk_remember<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    inputs: Bound<'py, PyAny>, dataset_name: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let inputs_value = normalise_inputs(&inputs)?;   // dict → [dict], same helper as sdk_ops.rs
    let opts_value = opts_to_json(opts)?;            // opts_to_json is in sdk_ops.rs — move to json.rs or duplicate
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = memory::run_remember(&handle, inputs_value, &dataset_name, &opts_value)
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_remember_entry<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    entry: Bound<'py, PyAny>, dataset_name: String, session_id: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* same pattern, memory::run_remember_entry */ }

pub fn py_sdk_memify<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* same pattern, memory::run_memify_op */ }

pub fn py_sdk_improve<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Bound<'py, PyAny>,           // required, not Option — validate datasetName is present
) -> PyResult<Bound<'py, PyAny>> { /* memory::run_improve */ }
```

> **Note on helpers**: `normalise_inputs` and `opts_to_json` currently live in `sdk_ops.rs` as
> `pub(crate)`. Either make them `pub(crate)` in `json.rs` (cleaner) or re-import from `sdk_ops`.

Note: `improve()` takes `opts` as a required argument (must contain `datasetName`). This mirrors
the C API and TS binding. Validate that `datasetName` is present and raise `CogneeValidationError`
if missing.

### Step 2 — Wire into `PyCognee`

```rust
#[pyo3(signature = (inputs, dataset_name, opts=None))]
fn remember<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

#[pyo3(signature = (entry, dataset_name, session_id, opts=None))]
fn remember_entry<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

#[pyo3(signature = (opts=None))]
fn memify<'py>(...) -> PyResult<Bound<'py, PyAny>> { /* ... */ }

fn improve<'py>(&self, py: Python<'py>, opts: Bound<'py, PyAny>)
    -> PyResult<Bound<'py, PyAny>> { /* ... */ }
```

### Step 3 — Validate `remember_entry` entry types (optional)

The hoisted op body already rejects unknown entry types with `SdkError::Validation` (the capi
parser does this today), so Python-side validation is optional. If eager validation before the
async dispatch is desired:

```rust
fn validate_entry_type(entry: &Bound<'_, PyAny>) -> PyResult<()> {
    let type_val: String = entry.get_item("type")?.extract()?;
    match type_val.as_str() {
        "qa" | "trace" | "feedback" => Ok(()),
        other => Err(sdk_error_to_py(SdkError::Validation(
            format!("unknown entry type '{}'; expected qa, trace, or feedback", other)
        ))),
    }
}
```

### Step 4 — Tests

Add `python/tests/test_memory_ops.py`:

```python
async def test_memify(cognee_with_data):
    result = await cognee_with_data.memify()
    assert "tripletCount" in result or "alreadyCompleted" in result   # camelCase keys

async def test_remember(cognee):
    result = await cognee.remember({"type": "text", "text": "Fact A"}, "mem_ds")
    assert result is not None

async def test_remember_entry_qa(cognee):
    entry = {"type": "qa", "question": "What?", "answer": "This."}
    result = await cognee.remember_entry(entry, "ds", "session-1")
    assert result is not None

async def test_remember_entry_bad_type(cognee):
    with pytest.raises(CogneeValidationError):
        await cognee.remember_entry({"type": "unknown"}, "ds", "s")

async def test_improve_missing_dataset(cognee):
    with pytest.raises(CogneeValidationError):
        await cognee.improve({})   # missing "datasetName" (camelCase) → CogneeValidationError
```

### Acceptance criteria

- `await cognee.memify()` returns a dict with `tripletCount` (may be 0 on empty graph; all keys camelCase)
- `await cognee.remember({"type": "text", "text": "..."}, "ds")` does not raise
- `await cognee.remember_entry({"type": "qa", ...}, "ds", "session-id")` does not raise
- Unknown entry type in `remember_entry` raises `CogneeValidationError`
- `improve({})` (missing `datasetName`) raises `CogneeValidationError`
