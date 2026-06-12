# Visualization Operations

## Status: ❌ Not implemented

## What is missing

| Python name | C API | TS | Feature gate | Description |
|-------------|-------|----|-------------|-------------|
| `Cognee.visualize(opts?)` | `cg_sdk_visualize` | `cognee.visualize()` | `visualization` | Render KG as a self-contained d3.js HTML string |
| `Cognee.visualize_to_file(opts?)` | `cg_sdk_visualize_to_file` | `cognee.visualizeToFile()` | `visualization` | Write visualization HTML to a file and return the path |

Both operations are feature-gated. If the `visualization` Cargo feature is not compiled in, they
raise `CogneeFeatureNotBuiltError` (matching `CG_ERR_FEATURE_NOT_BUILT` in the C API and
`FeatureNotBuiltError` in TS).

### Options

```python
opts = {
    "destination_path": str | None,   # for visualize_to_file, default ~/graph_visualization.html
}
```

### Result shapes

- `visualize()` → `str` — the full HTML document as a Python string
- `visualize_to_file()` → `str` — the absolute path to the written file

## Rationale

Visualization is a key developer experience feature. It is the only way to inspect the knowledge
graph without writing custom graph traversal code. As a feature-gated operation it has zero
runtime cost when the `visualization` feature is disabled.

The implementation is straightforward: the Rust layer already does all the work (d3.js rendering
in `cognee-visualization`). The Python binding only needs to call the async Rust function and
return the resulting string.

## Implementation plan

### Step 1 — Add `visualization` to Python `Cargo.toml` features

In `python/Cargo.toml`:
```toml
[features]
default = ["telemetry", "visualization"]
visualization = ["cognee-lib/visualization"]
```

### Step 2 — Create `python/src/sdk_visualization.rs`

**Prerequisite:** hoist the visualize op bodies from
`capi/cognee-capi/src/sdk_visualization.rs` (and the neon counterpart) into
`cognee_bindings_common::ops` — see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0. The
hoisted functions should return plain `String` (HTML / path), so the C API's quoted-JSON-string
`D9` contract (and its `cg_json_string_decode` helper) is irrelevant for Python — the string
crosses the PyO3 boundary natively.

```rust
pub fn py_sdk_visualize<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let html: String = ops::visualize(&handle, opts_value.as_ref())
            .await.map_err(sdk_error_to_py)?;
        Ok(html)
    })
}

pub fn py_sdk_visualize_to_file<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let path: String = ops::visualize_to_file(&handle, opts_value.as_ref())
            .await.map_err(sdk_error_to_py)?;
        Ok(path)
    })
}
```

### Step 3 — Wire into `PyCognee`

```rust
#[pyo3(signature = (opts=None))]
fn visualize<'py>(&self, py: Python<'py>, opts: Option<Bound<'py, PyAny>>)
    -> PyResult<Bound<'py, PyAny>>
{
    sdk_visualization::py_sdk_visualize(py, Arc::clone(&self.inner), opts)
}

#[pyo3(signature = (opts=None))]
fn visualize_to_file<'py>(&self, py: Python<'py>, opts: Option<Bound<'py, PyAny>>)
    -> PyResult<Bound<'py, PyAny>>
{
    sdk_visualization::py_sdk_visualize_to_file(py, Arc::clone(&self.inner), opts)
}
```

### Step 4 — Feature-not-built path

When the `visualization` feature is disabled, mirror capi/neon: keep the Python methods exported
but have a `#[cfg(not(feature = "visualization"))]` body return `SdkError::FeatureNotBuilt`. The
`sdk_error_to_py` mapper (from [sdk-handle.md](sdk-handle.md)) converts this to
`CogneeFeatureNotBuiltError` automatically.

### Step 5 — Tests

Add `python/tests/test_visualization.py`:

```python
# Smoke test (requires visualization feature compiled in)
async def test_visualize_returns_html(cognee_with_data):
    html = await cognee_with_data.visualize()
    assert isinstance(html, str)
    assert "<!DOCTYPE html>" in html or "<html" in html

async def test_visualize_to_file(cognee_with_data, tmp_path):
    path = await cognee_with_data.visualize_to_file(
        {"destination_path": str(tmp_path / "graph.html")}
    )
    assert path.endswith(".html")
    import os
    assert os.path.isfile(path)
    content = open(path).read()
    assert "<!DOCTYPE html>" in content or "<html" in content

# Feature-not-built test (skip if feature is compiled in)
async def test_visualize_feature_not_built(cognee_no_viz_feature):
    from cognee_pipeline import CogneeFeatureNotBuiltError
    with pytest.raises(CogneeFeatureNotBuiltError):
        await cognee_no_viz_feature.visualize()
```

### Acceptance criteria

- `await cognee.visualize()` returns a non-empty string containing HTML
- `await cognee.visualize_to_file({"destination_path": "/tmp/test.html"})` writes the file and
  returns its path
- When the `visualization` feature is not compiled in, both methods raise
  `CogneeFeatureNotBuiltError`
