# Visualization Operations

## Status: ✅ Implemented

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

Use `opts_to_camel_json` (from `crate::json`) when converting `opts` in the
binding layer — it normalises `snake_case` keys to `camelCase` so Python
`"destination_path"` reaches the Rust op as `"destinationPath"`, matching
the key the C API and Neon parse.

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

### ~~Step 1 — Add `visualization` to Python `Cargo.toml` features~~ (DONE)

`python/Cargo.toml` already has:
```toml
visualization = ["cognee-lib/visualization", "cognee-bindings-common/visualization"]
```
and `visualization` is in the `default` feature list. No action needed.

### Step 2 — Create `python/src/sdk_visualization.rs`

**Note on the "ops hoist" prerequisite:** Unlike the other op groups, no
`crates/bindings-common/src/ops/visualization.rs` module was created during T3.
The C API (`capi/cognee-capi/src/sdk_visualization.rs`) and Neon
(`js/cognee-neon/src/sdk_visualization.rs`) each have their own private
`inner::run_visualize` / `inner::run_visualize_to_file` helpers. Two options:

1. **(Preferred — full hoist)** Create `crates/bindings-common/src/ops/visualization.rs`
   by extracting the `inner` logic from capi/neon (feature-gated on
   `#[cfg(feature = "visualization")]`), register it in
   `crates/bindings-common/src/ops/mod.rs`, and rewrite capi/neon to call through. Then
   `sdk_visualization.rs` calls `cognee_bindings_common::ops::visualization::visualize`
   and `::visualize_to_file`.

2. **(Acceptable short-term — inline)** Port the `inner` logic directly into
   `python/src/sdk_visualization.rs` (same pattern as capi/neon), calling
   `cognee_lib::visualization::render` and `cognee_lib::visualize` directly.
   Triples the maintenance surface but avoids touching capi/neon.

Both Rust functions to call are already exported from `cognee-lib` (feature-gated):
- `cognee_lib::visualization::render(&dyn GraphDBTrait) -> Result<String, VisualizationError>`
- `cognee_lib::visualize(&dyn GraphDBTrait, Option<&Path>) -> Result<PathBuf, VisualizationError>`

The concrete `graph_db` is obtained via `state.services().await?.graph_db` (an
`Arc<dyn GraphDBTrait>`).

The final `sdk_visualization.rs` should look like (option 1 shown; adapt for option 2):

```rust
pub fn py_sdk_visualize<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let html: String = ops::visualization::visualize(&handle, opts_value.as_ref())
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
        let path: String = ops::visualization::visualize_to_file(&handle, opts_value.as_ref())
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

Add `python/tests/test_visualization.py`.

**Important fixture note:** There is no `cognee_with_data` fixture in
`python/tests/conftest.py`; the only global fixture is `ctx`. Tests that need
a warm `Cognee` handle follow the pattern in `test_dataset_mgmt.py` and
`test_core_ops.py`: create a local `_make_cognee(tmp_path)` async helper that
builds a `cp.Cognee(...)` with an isolated `relational_db_url`, `data_root`,
etc., then call `await c.warm()`. The `cognee_with_data` and
`cognee_no_viz_feature` names below are **placeholders** — implement them as
local async helpers or `@pytest.mark.asyncio` fixtures inside the test file.
Use `MOCK_EMBEDDING=true` (guard with `pytest.mark.skipif`) for the warm path.

The `test_visualize_feature_not_built` test requires a build compiled
**without** the `visualization` feature, which cannot be tested in the standard
`python/scripts/check.sh` run (which builds with defaults). Treat it as a
documentation test only, or skip it unconditionally in CI.

```python
# Smoke test (requires visualization feature compiled in)
async def test_visualize_returns_html(tmp_path):
    c = await _make_cognee(tmp_path)
    html = await c.visualize()
    assert isinstance(html, str)
    assert "<!DOCTYPE html>" in html or "<html" in html

async def test_visualize_to_file(tmp_path):
    c = await _make_cognee(tmp_path)
    out = str(tmp_path / "graph.html")
    path = await c.visualize_to_file({"destination_path": out})
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
