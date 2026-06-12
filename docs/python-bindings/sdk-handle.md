# SDK Handle and Lifecycle

## Status: ❌ Not implemented

## What is missing

The `Cognee` class / `CgSdk` handle — the entry point for all SDK-tier operations — has no Python
equivalent. Without it, none of the high-level operations (add, cognify, search, etc.) can be
called from Python.

The C API exposes `CgSdk*` and the TS binding exposes `class Cognee`. Both share the same
underlying `HandleState` (from `cognee-bindings-common`, `crates/bindings-common/src/handle.rs`)
which wraps a `ComponentManager` (config + 6 lazy engines) and lazily builds and caches a
`CogneeServices` bundle, version-invalidated whenever the config changes.

### Missing symbols

| Python name | C API equivalent | TS equivalent |
|-------------|-----------------|---------------|
| `Cognee(settings?)` constructor | `cg_sdk_new(settings_json)` | `new Cognee(settings?)` |
| `Cognee.warm()` | `cg_sdk_warm(sdk, cb, ud)` | `cognee.warm()` |
| `Cognee.owner_id()` | `cg_sdk_owner_id(sdk, cb, ud)` | `cognee.ownerId()` |
| n/a | `cg_sdk_clone(sdk)` | n/a |
| n/a | `cg_api_version()` | n/a |

### The actual `HandleState` API (verified against code)

`HandleState` deliberately exposes only four methods — everything else is built on top of it by
each binding:

```rust
impl HandleState {
    pub fn from_settings(settings: Settings) -> Self;          // sync, no I/O
    pub fn from_env() -> Self;
    pub async fn services(&self) -> Result<Arc<CogneeServices>, SdkError>;  // lazy build + cache
    pub async fn owner_id(&self) -> Result<Uuid, SdkError>;
}
```

There is **no** `warm()` method — "warm" in both existing bindings simply means calling
`state.services().await` and discarding the result (see `cg_sdk_warm` in
`capi/cognee-capi/src/sdk.rs:267`).

There is **no** `from_settings_json` — each binding implements the **3-way settings overlay**
(`defaults < env < JSON object`) itself before calling `from_settings`. The C API reference
implementation is `apply_settings_json_patch` in `capi/cognee-capi/src/sdk.rs:216`: load
`ConfigManager::from_env().read().clone()` as the base, parse the JSON patch object, and apply
each key via the `ConfigManager::set` machinery so unknown keys are tolerated consistently.

## Rationale

Every SDK-tier operation takes a `Cognee` (or `CgSdk`) handle as its first argument. Without this
handle, none of the 40+ SDK operations can be implemented. This is the minimal foundation the
entire SDK tier builds on.

`warm()` is important for production use: it eagerly creates the embedding engine (may download
an ONNX model), the database connection (runs migrations), and resolves the default user. Without
an explicit warm-up call the first `add()` or `cognify()` call will incur a large cold-start
latency that is invisible to the caller.

## Implementation plan

### Step 1 — Add dependencies to `python/Cargo.toml`

The `python/` crate is a member of the root workspace (it sits next to `crates/`, so paths are
one level up):

```toml
[dependencies]
cognee-bindings-common = { path = "../crates/bindings-common" }
cognee-lib = { path = "../crates/lib", default-features = false }
serde_json = { workspace = true }

[features]
# Mirror cognee-neon's default feature set (js/cognee-neon/Cargo.toml):
default = ["visualization", "cloud", "qdrant", "ladybug", "onnx",
           "hf-tokenizer", "tiktoken", "sqlite", "testing"]
visualization = ["cognee-lib/visualization", "cognee-bindings-common/visualization"]
cloud = ["cognee-lib/cloud", "cognee-bindings-common/cloud"]
# ... forward the remaining features the same way cognee-neon does
```

**Build-weight warning:** today the Python crate has a deliberately small dependency set
(cognee-core + database/graph/vector with `testing`). Adding `cognee-lib` with the neon-parity
feature set pulls in embedded Qdrant, Ladybug, and ONNX Runtime — expect a significant build-time
increase for `python/scripts/check.sh`. Verify the maturin build still works in CI before
proceeding to the op layers.

### Step 2 — Create `python/src/sdk.rs`

Define a `PyCognee` PyO3 class wrapping `Arc<HandleState>`. The constructor replicates capi's
3-way overlay:

```rust
use pyo3::prelude::*;
use std::sync::Arc;
use cognee_bindings_common::HandleState;
use cognee_lib::config::ConfigManager;

#[pyclass(name = "Cognee")]
pub struct PyCognee {
    pub(crate) inner: Arc<HandleState>,
}

#[pymethods]
impl PyCognee {
    #[new]
    #[pyo3(signature = (settings=None))]
    fn new(settings: Option<&str>) -> PyResult<Self> {
        // 3-way overlay: defaults < env < JSON object (mirror
        // apply_settings_json_patch in capi/cognee-capi/src/sdk.rs).
        let base = ConfigManager::from_env().read().clone();
        let settings = match settings {
            None => base,
            Some(json) => apply_settings_json_patch(base, json)
                .map_err(crate::sdk_error::validation_err)?,
        };
        Ok(Self { inner: Arc::new(HandleState::from_settings(settings)) })
    }

    /// Build engines and resolve the default user. Awaitable, returns None.
    fn warm<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            handle.services().await.map_err(crate::sdk_error::sdk_error_to_py)?;
            Ok(())
        })
    }

    fn owner_id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let id = handle.owner_id().await
                .map_err(crate::sdk_error::sdk_error_to_py)?;
            Ok(id.to_string())
        })
    }
}
```

`apply_settings_json_patch` should be copied from (or, better, hoisted out of)
`capi/cognee-capi/src/sdk.rs` into `cognee-bindings-common` so all three bindings share it.

### Step 3 — Expose `HandleState` reference for sibling modules

All downstream operations (add, cognify, search, etc.) will access `self.inner` to call the
shared op functions (see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0 for where those
live). Keep `inner` `pub(crate)` so sibling modules can access it without going through the
Python object.

### Step 4 — Wire exception types

Create `python/src/sdk_error.rs` that converts `SdkError` (from `cognee-bindings-common`,
`crates/bindings-common/src/error.rs`) into the appropriate Python exception. The actual enum has
**seven** variants; the string codes below come from `SdkError::code()` and match what the TS
binding puts in `error.code`:

| SdkError variant | code() string | Python exception |
|-----------------|---------------|-----------------|
| `Component` | `COMPONENT_ERROR` | `CogneeComponentError(CogneeError)` |
| `ServiceBuild` | `SERVICE_BUILD_ERROR` | `CogneeServiceBuildError(CogneeError)` |
| `UserBootstrap` | `USER_BOOTSTRAP_ERROR` | `CogneeUserBootstrapError(CogneeError)` |
| `Runtime` | `RUNTIME_ERROR` | `CogneeRuntimeError(CogneeError)` |
| `Validation` | `VALIDATION_ERROR` | `CogneeValidationError(CogneeError)` |
| `Unsupported` | `UNSUPPORTED` | `CogneeUnsupportedError(CogneeError)` |
| `FeatureNotBuilt` | `FEATURE_NOT_BUILT` | `CogneeFeatureNotBuiltError(CogneeError)` |

Config errors are **not** `SdkError` variants — they are a separate enum,
`cognee_lib::config::ConfigError`, with two variants. Map them separately (used by
[config-surface.md](config-surface.md)):

| ConfigError variant | Python exception |
|--------------------|-----------------|
| `UnknownKey` | `CogneeUnknownConfigKeyError(CogneeError)` |
| `TypeMismatch` | `CogneeConfigTypeMismatchError(CogneeError)` |

All extend a new base `CogneeError(Exception)` so callers can catch broadly. Keep this base
separate from the existing `PipelineError` hierarchy (engine tier) — the two tiers have disjoint
error taxonomies in the C API too (codes 0–10 vs 11–18).

### Step 5 — Register in `python/src/lib.rs`

```rust
mod sdk;
mod sdk_error;

// in _native module init:
m.add_class::<sdk::PyCognee>()?;
sdk_error::register_exceptions(m)?;
```

### Step 6 — Update `python/cognee_pipeline/__init__.py`

```python
from cognee_pipeline._native import (
    Cognee,
    CogneeError, CogneeComponentError, CogneeServiceBuildError,
    CogneeUserBootstrapError, CogneeRuntimeError, CogneeValidationError,
    CogneeUnsupportedError, CogneeFeatureNotBuiltError,
    CogneeUnknownConfigKeyError, CogneeConfigTypeMismatchError,
    # ... existing exports
)
```

### Step 7 — Tests

Add `python/tests/test_sdk_handle.py`:
- `Cognee()` instantiation with no args
- `Cognee(settings_json)` with a JSON override string
- `warm()` is awaitable and returns None
- `owner_id()` returns a valid UUID string
- Instantiation with malformed JSON raises `CogneeValidationError`

### Acceptance criteria

- `cognee = Cognee(); await cognee.warm()` completes without error in a test environment with
  `MOCK_EMBEDDING=true`
- `await cognee.owner_id()` returns a UUID string
- All `CogneeError` subclasses are importable from `cognee_pipeline`
