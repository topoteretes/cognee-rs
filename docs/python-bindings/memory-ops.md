# Memory Operations: remember, remember_entry, memify, improve

## Status: ❌ Not implemented

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
    "session_id": str,
    "self_improvement": bool,
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
    "triplet_batch_size": int,
    "node_type_filter": str,
    "node_name_filter": str,
    "node_name_filter_operator": str,    # "AND" | "OR"
}
```

### `improve` options (required: `dataset_name`)

```python
opts = {
    "dataset_name": str,                 # required
    "session_ids": [str],
    "node_name": [str],
    "feedback_alpha": float,             # default 0.1
    "tenant": str,
}
```

### Result shapes

**remember result**: Pass-through JSON dict (complex structure with status, dataset info, session IDs, pipeline run info).

**memify result**:
```python
{
    "triplet_count": int,
    "indexed_count": int,
    "batch_count": int,
    "already_completed": bool,
    "prior_pipeline_run_id": str | None,
}
```

**improve result**:
```python
{
    "stages_run": [str],
    "memify_result": dict | None,
    "feedback_entries_processed": int,
    "feedback_entries_applied": int,
    "sessions_persisted": int,
    "edges_synced": int,
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

**Prerequisite:** the shared op bodies must first be hoisted from
`capi/cognee-capi/src/sdk_memory.rs` (and its neon counterpart) into a
`cognee_bindings_common::ops` module — see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0.
The sketches below call those hoisted functions (`ops::remember`, `ops::memify`, …).

### Step 1 — Create `python/src/sdk_memory.rs`

```rust
pub fn py_sdk_remember<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    inputs: Bound<'py, PyAny>, dataset_name: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let inputs_value = normalise_inputs(&inputs)?;          // dict → [dict], then py_to_serde
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = ops::remember(&handle, &inputs_value, &dataset_name, opts_value.as_ref())
            .await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_remember_entry<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    entry: Bound<'py, PyAny>, dataset_name: String, session_id: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* same pattern, ops::remember_entry */ }

pub fn py_sdk_memify<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* same pattern, ops::memify */ }

pub fn py_sdk_improve<'py>(
    py: Python<'py>, handle: Arc<HandleState>,
    opts: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> { /* opts is required here, not optional; ops::improve */ }
```

Note: `improve()` takes `opts` as a required argument (must contain `dataset_name`). This mirrors
the C API and TS binding. Validate that `dataset_name` is present and raise `CogneeValidationError`
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
    assert "triplet_count" in result or "already_completed" in result

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
        await cognee.improve({})
```

### Acceptance criteria

- `await cognee.memify()` returns a dict with `triplet_count` (may be 0 on empty graph)
- `await cognee.remember({"type": "text", "text": "..."}, "ds")` does not raise
- `await cognee.remember_entry({"type": "qa", ...}, "ds", "session-id")` does not raise
- Unknown entry type in `remember_entry` raises `CogneeValidationError`
- `improve({})` (missing `dataset_name`) raises `CogneeValidationError`
