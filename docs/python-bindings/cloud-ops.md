# Cloud Operations: serve, disconnect

## Status: ❌ Not implemented

## What is missing

| Python name | C API | TS | Feature gate | Description |
|-------------|-------|----|-------------|-------------|
| `serve(opts?)` (module-level) | `cg_sdk_serve` | `serve()` | `cloud` | Connect to Cognee Cloud (Auth0 or direct) |
| `disconnect(opts?)` (module-level) | `cg_sdk_disconnect` | `disconnect()` | `cloud` | Disconnect and optionally wipe credentials |

Both are **module-level** (not methods on `Cognee`) because they operate on a **process-wide
singleton** `CloudClient` — matching the C API and TS pattern. The TS binding exports them as
`serve()` / `disconnect()` from the top-level module.

### `serve` options

```python
# Cloud mode (requires interactive TTY for Auth0 device code flow)
serve()

# Direct mode (headless, for CI / tests)
serve({"url": "http://localhost:8000"})

# Full options
serve({
    "url": str | None,              # direct URL — if set, skips Auth0
    "api_key": str | None,
    "cloud_url": str | None,
    "auth0_domain": str | None,
    "auth0_client_id": str | None,
    "auth0_audience": str | None,
})
```

### `disconnect` options

```python
disconnect()
disconnect({"wipe_credentials": True})   # deletes credential cache
```

### Result shapes

**serve** result:
```python
{"connected": True, "service_url": str}
```

**disconnect** result: `None`

## Rationale

Cloud connectivity is an optional, feature-gated capability. It allows the Rust SDK to connect to
a remote Cognee Cloud instance and delegate storage/computation there. As with visualization, it
has zero cost when the `cloud` Cargo feature is not compiled in. The `CogneeFeatureNotBuiltError`
path is already handled by the shared error mapper.

The process-wide singleton design means these are not instance methods — they match what the CLI
`serve` / `disconnect` subcommands do.

## Implementation plan

### Step 1 — Add `cloud` to Python `Cargo.toml` features

In `python/Cargo.toml`:
```toml
[features]
default = ["telemetry", "visualization", "cloud"]
cloud = ["cognee-lib/cloud"]
```

### Step 2 — Create `python/src/sdk_cloud.rs`

`ServeConfig` is a plain builder struct (`#[derive(Debug, Default, Clone)]` — it is **not**
`Deserialize`). Build it field-by-field from the opts object, exactly like capi's
`build_serve_config` in `capi/cognee-capi/src/sdk_cloud.rs` — or, better, hoist that helper into
`cognee-bindings-common` and call it from both bindings:

```rust
use cognee_lib::{serve as rust_serve, disconnect as rust_disconnect, ServeConfig};

#[cfg(feature = "cloud")]
fn build_serve_config(opts: &serde_json::Value) -> ServeConfig {
    // Mirrors capi's build_serve_config: presence of `url` selects direct mode.
    let mut config = match opts.get("url").and_then(|v| v.as_str()) {
        Some(u) => ServeConfig::direct(u),
        None => ServeConfig::cloud(),
    };
    if let Some(k) = opts.get("apiKey").and_then(|v| v.as_str()) { config = config.api_key(k); }
    if let Some(u) = opts.get("cloudUrl").and_then(|v| v.as_str()) { config = config.cloud_url(u); }
    // ... auth0Domain, auth0ClientId, auth0Audience the same way
    config
}

pub fn py_serve<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(feature = "cloud")]
    {
        let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?
            .unwrap_or(serde_json::Value::Null);
        return pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let config = build_serve_config(&opts_value);
            let result = rust_serve(config).await
                .map_err(|e| CogneeRuntimeError::new_err(e.to_string()))?;
            // Assemble {"connected": true, "service_url": ...} like capi does.
            Python::with_gil(|py| serve_result_to_py(py, &result))
        });
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = (py, opts);
        Err(CogneeFeatureNotBuiltError::new_err(
            "cloud feature not compiled in; rebuild with --features cloud"
        ))
    }
}

pub fn py_disconnect<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* similar pattern, returns None */ }
```

### Step 3 — Expose as module-level functions in `lib.rs`

```rust
#[pyfunction]
#[pyo3(signature = (opts=None))]
fn serve<'py>(py: Python<'py>, opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    sdk_cloud::py_serve(py, opts)
}

#[pyfunction]
#[pyo3(signature = (opts=None))]
fn disconnect<'py>(py: Python<'py>, opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    sdk_cloud::py_disconnect(py, opts)
}

// in module init:
m.add_function(wrap_pyfunction!(serve, m)?)?;
m.add_function(wrap_pyfunction!(disconnect, m)?)?;
```

### Step 4 — Update `python/cognee_pipeline/__init__.py`

```python
from cognee_pipeline._native import serve, disconnect
```

### Step 5 — Tests

Add `python/tests/test_cloud_ops.py`:

```python
# Direct mode (headless, does not need Auth0)
async def test_serve_direct_mode():
    # Requires a running cognee HTTP server at this URL
    pytest.importorskip("cognee_pipeline")
    import os
    server_url = os.environ.get("COGNEE_TEST_SERVER_URL")
    if not server_url:
        pytest.skip("COGNEE_TEST_SERVER_URL not set")
    from cognee_pipeline import serve
    result = await serve({"url": server_url})
    assert result["connected"] is True
    assert "service_url" in result

async def test_disconnect():
    from cognee_pipeline import disconnect
    await disconnect()  # should not raise even if not connected

# Feature-not-built test
async def test_serve_feature_not_built_raises():
    # Only runs in slim builds without cloud feature
    pass  # covered by CI matrix with --no-default-features
```

Note: the Auth0 interactive flow cannot be tested in CI. Only the direct mode (`url` option) is
testable in automated environments.

### Acceptance criteria

- `from cognee_pipeline import serve, disconnect` works
- `await disconnect()` returns `None` without raising (even if not connected)
- `await serve({"url": "http://..."})` in direct mode returns `{"connected": True, "service_url": ...}`
- When the `cloud` feature is not compiled in, both raise `CogneeFeatureNotBuiltError`
