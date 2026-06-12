# Cloud Operations: serve, disconnect

## Status: ✅ Implemented

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
{"connected": True, "serviceUrl": str}   # camelCase — matches capi/neon shape
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

### ~~Step 1 — Add `cloud` to Python `Cargo.toml` features~~ (DONE)

`python/Cargo.toml` already has `cloud` in its `default` feature list and the
`cloud = ["cognee-lib/cloud", "cognee-bindings-common/cloud"]` line. No action needed.

### Step 2 — Create `python/src/sdk_cloud.rs`

`ServeConfig` is a plain builder struct (`#[derive(Debug, Default, Clone)]` — it is **not**
`Deserialize`). Build it field-by-field from the opts object, exactly like capi's
`build_serve_config` in `capi/cognee-capi/src/sdk_cloud.rs` — or, better, hoist that helper into
`cognee-bindings-common` and call it from both bindings.

**Preferred path (mirrors T9 visualization pattern):** Create
`crates/bindings-common/src/ops/cloud.rs` with `run_serve` and `run_disconnect` async functions
(feature-gated on `#[cfg(feature = "cloud")]`), register it in
`crates/bindings-common/src/ops/mod.rs`, then call through from `python/src/sdk_cloud.rs`. The
existing `capi/cognee-capi/src/sdk_cloud.rs` inner module is a direct copy of what belongs there.

**Key implementation notes:**
- `cognee_lib::serve` is re-exported from `crates/lib/src/api/serve.rs` and returns
  `CloudResult<Arc<CloudClient>>`. `CloudClient` has a `pub service_url: String` field.
- `cognee_lib::disconnect` takes a `wipe_credentials: bool` and returns `CloudResult<()>`.
- The serve result JSON shape used by both capi and neon is `{"connected": true, "serviceUrl": "…"}`
  (camelCase). **The Python result must also use `"serviceUrl"` (camelCase) to match.** The
  acceptance criteria below and the test assertion use `"service_url"` — **update both to
  `"serviceUrl"`** for consistency with capi/neon. See `serve_result_to_py` note below.
- Use `opts_to_camel_json` (from `crate::json`) to normalize Python `snake_case` opts keys to
  `camelCase` before passing to the shared Rust op — same pattern as `sdk_visualization.rs`.
- For errors, use `sdk_error_to_py` from `crate::sdk_error` (maps `SdkError::FeatureNotBuilt` →
  `CogneeFeatureNotBuiltError`, `SdkError::Runtime` → `CogneeRuntimeError`, etc.).
- `serve_result_to_py` in the snippet below is pseudocode for assembling the result dict.
  In practice, build it directly with `serde_json::json!` then convert via `serde_to_py` from
  `crate::json`, or call `cognee_bindings_common::ops::cloud::run_serve` which returns a
  `serde_json::Value` and convert that.
- For `py_disconnect`, return `Python::with_gil(|py| Ok(py.None()))` after a successful
  `disconnect()` call (the same pattern used in `sdk_data.rs` for void ops like `prune_data`).

```rust
use cognee_lib::{serve as rust_serve, disconnect as rust_disconnect, ServeConfig};

#[cfg(feature = "cloud")]
fn build_serve_config(opts: &serde_json::Value) -> ServeConfig {
    // Mirrors capi's build_serve_config: presence of `url` selects direct mode.
    let url = opts.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let mut config = match url {
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
        let opts_value = opts_to_camel_json(opts)?;
        return pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let config = build_serve_config(&opts_value);
            let client = rust_serve(config).await
                .map_err(|e| sdk_error_to_py(SdkError::Runtime(format!("serve failed: {e}"))))?;
            // client.service_url is a pub String field on Arc<CloudClient>
            let result = serde_json::json!({ "connected": true, "serviceUrl": client.service_url });
            Python::with_gil(|py| serde_to_py(py, &result))
        });
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = (py, opts);
        Err(sdk_error_to_py(SdkError::FeatureNotBuilt(
            "cloud feature not compiled in; rebuild with --features cloud".to_string()
        )))
    }
}

pub fn py_disconnect<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(feature = "cloud")]
    {
        let opts_value = opts_to_camel_json(opts)?;
        return pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let wipe = opts_value.get("wipeCredentials").and_then(|v| v.as_bool()).unwrap_or(false);
            rust_disconnect(wipe).await
                .map_err(|e| sdk_error_to_py(SdkError::Runtime(format!("disconnect failed: {e}"))))?;
            Python::with_gil(|py| Ok(py.None()))
        });
    }

    #[cfg(not(feature = "cloud"))]
    {
        let _ = (py, opts);
        Err(sdk_error_to_py(SdkError::FeatureNotBuilt(
            "cloud feature not compiled in; rebuild with --features cloud".to_string()
        )))
    }
}
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
    assert "serviceUrl" in result   # camelCase — matches capi/neon shape

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
- `await serve({"url": "http://..."})` in direct mode returns `{"connected": True, "serviceUrl": ...}` (camelCase key, matching capi/neon)
- When the `cloud` feature is not compiled in, both raise `CogneeFeatureNotBuiltError`
