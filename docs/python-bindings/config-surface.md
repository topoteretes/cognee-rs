# Configuration Surface

## Status: ❌ Not implemented

## What is missing

There is no way to configure a `Cognee` instance (LLM endpoint, API key, embedding model, vector
DB, etc.) from Python. Both the C API and TS binding provide a full set of granular per-field
setters, bulk setters, and a read-back getter.

### Missing operations

| Category | Python name | C API | TS equivalent |
|----------|-------------|-------|---------------|
| Generic setter | `config.set(key, value)` | `cg_sdk_config_set` | `configSet` |
| String shorthand | `config.set_str(key, value)` | `cg_sdk_config_set_str` | (granular setters) |
| Read-back | `config.get()` | `cg_sdk_config_get` | `getConfig` |
| LLM bulk | `config.set_llm_config(obj)` | `cg_sdk_config_set_llm_config` | `configSetLlmConfig` |
| Embedding bulk | `config.set_embedding_config(obj)` | `cg_sdk_config_set_embedding_config` | `configSetEmbeddingConfig` |
| Vector DB bulk | `config.set_vector_db_config(obj)` | `cg_sdk_config_set_vector_db_config` | `configSetVectorDbConfig` |
| Graph DB bulk | `config.set_graph_db_config(obj)` | `cg_sdk_config_set_graph_db_config` | `configSetGraphDbConfig` |

Granular setters (all present in TS and C API) that would map to individual Python methods:
- LLM: `llm_provider`, `llm_model`, `llm_api_key`, `llm_endpoint`, `llm_api_version`,
  `llm_temperature`, `llm_streaming`, `llm_max_completion_tokens`, `llm_max_retries`,
  `llm_max_parallel_requests`
- Embedding: `embedding_provider`, `embedding_model`, `embedding_dimensions`, `embedding_endpoint`,
  `embedding_api_key`, `embedding_model_path`, `embedding_tokenizer_path`
- Vector DB: `vector_db_provider`, `vector_db_url`, `vector_db_key`, `vector_db_host`,
  `vector_db_port`, `vector_db_name`
- Graph DB: `graph_database_provider`, `graph_model`, `graph_file_path`
- Chunking: `chunk_strategy`, `chunk_engine`, `chunk_size`, `chunk_overlap`
- Paths: `system_root_directory`, `data_root_directory`, `cache_root_directory`,
  `logs_root_directory`
- Ontology: `ontology_file_path`, `ontology_resolver`, `ontology_matching_strategy`
- Misc: `monitoring_tool`, `classification_model`, `summarization_model`

## Rationale

Without a config surface, users cannot point the SDK at their own LLM, embedding provider, or
storage backends. The SDK tier is unusable without this. This is the second prerequisite (after
the SDK handle) that all other operations depend on.

## Implementation plan

### Step 1 — Create `python/src/config.rs`

The config machinery lives on `ConfigManager` (in `cognee_lib::config`), reached through
`HandleState.cm` (the `ComponentManager`). This is exactly how the C API does it — see
`capi/cognee-capi/src/sdk_config.rs:185`: `state.cm.config().set(key_str, value)`. The setters
take a `serde_json::Value`, and errors are `cognee_lib::config::ConfigError`
(`UnknownKey` / `TypeMismatch`) — a separate enum from `SdkError`.

Define a `PyCogneeConfig` PyO3 class that holds an `Arc<HandleState>` (shared with `PyCognee`):

```rust
use cognee_lib::config::ConfigError;

#[pyclass(name = "CogneeConfig")]
pub struct PyCogneeConfig {
    inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeConfig {
    /// Generic setter. key is a snake_case Settings field name.
    /// value is a Python object — str, int, float, bool, dict, or list.
    fn set(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let value: serde_json::Value = py_to_serde(value)?;
        self.inner.cm.config().set(key, value)
            .map_err(config_error_to_py)   // ConfigError → CogneeUnknownConfigKeyError / CogneeConfigTypeMismatchError
    }

    /// Convenience: accepts a plain str, wraps as a JSON string value internally.
    fn set_str(&self, key: &str, value: &str) -> PyResult<()> {
        self.inner.cm.config()
            .set(key, serde_json::Value::String(value.to_owned()))
            .map_err(config_error_to_py)
    }

    /// Read back the current config. Secret fields are redacted.
    fn get(&self, py: Python<'_>) -> PyResult<PyObject> {
        // Serialize the Settings snapshot, then redact secret fields the same
        // way capi's cg_sdk_config_get does (see Step 4 note below).
        let settings = self.inner.cm.config().read().clone();
        let mut value = serde_json::to_value(&settings)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        redact_secrets(&mut value);
        serde_to_py(py, &value)
    }

    fn set_llm_config(&self, values: &Bound<'_, PyAny>) -> PyResult<()> {
        let map = py_to_serde_map(values)?;
        self.inner.cm.config().set_llm_config(&map).map_err(config_error_to_py)
    }
    // set_embedding_config / set_vector_db_config / set_graph_db_config: same shape,
    // calling the corresponding ConfigManager bulk setter.
}
```

### Step 2 — Attach `config` as an attribute on `PyCognee`

In `PyCognee.__new__`, after constructing `inner`, create and store a `PyCogneeConfig` that shares
the same `Arc<HandleState>`:

```rust
#[pyclass(name = "Cognee")]
pub struct PyCognee {
    inner: Arc<HandleState>,
    config: Py<PyCogneeConfig>,   // pre-built, returned as a Python object attribute
}
```

Expose via a `@property`:
```rust
#[getter]
fn config(&self, py: Python<'_>) -> Py<PyCogneeConfig> {
    self.config.clone_ref(py)
}
```

Python usage:
```python
cognee = Cognee()
cognee.config.set_str("llm_api_key", "sk-...")
cognee.config.set_llm_config({"llm_model": "gpt-4o", "llm_temperature": 0.0})
cfg = cognee.config.get()
```

### Step 3 — Add granular setter methods (optional convenience layer)

Mirror the TS binding's ~40 named setters as Python methods on `PyCogneeConfig`. Each is a thin
wrapper around `set_str` / `set`:

```rust
fn set_llm_provider(&self, value: &str) -> PyResult<()> {
    self.set_str("llm_provider", value)
}
fn set_llm_temperature(&self, value: f64) -> PyResult<()> {
    self.inner.cm.config()
        .set("llm_temperature", serde_json::json!(value))
        .map_err(config_error_to_py)
}
// ... etc.
```

This step can be deferred — `set()` and `set_str()` cover all cases.

### Step 4 — Error mapping and secret redaction

`CogneeUnknownConfigKeyError` and `CogneeConfigTypeMismatchError` must be registered before this
module is usable (they map from `ConfigError::UnknownKey` / `ConfigError::TypeMismatch`, not from
`SdkError`). Ensure `sdk_error.rs` from [sdk-handle.md](sdk-handle.md) is completed first.

The secret-redaction list for `get()` (replace with `"***REDACTED***"`) currently lives in
capi's `sdk_config.rs`: `llm_api_key`, `embedding_api_key`, `vector_db_key`,
`vector_db_password`, `graph_database_key`, `graph_database_password`, `db_password`,
`cache_password`, `default_user_password`, `otel_exporter_otlp_headers`. Hoist the redaction
helper into `cognee-bindings-common` so all three bindings share one list (same argument as
the op-body hoist in [core-pipeline-ops.md](core-pipeline-ops.md) Step 0).

### Step 5 — Tests

Add `python/tests/test_config.py`:
- `cognee.config.set_str("llm_provider", "openai")` succeeds
- `cognee.config.set("llm_temperature", 0.7)` (float) succeeds
- `cognee.config.set("unknown_key", "value")` raises `CogneeUnknownConfigKeyError`
- `cognee.config.get()` returns a `dict` with `llm_provider` present and secret fields redacted
- `cognee.config.set_llm_config({"llm_model": "gpt-4o"})` succeeds

### Acceptance criteria

- A `CogneeConfig` object is accessible as `cognee.config`
- `cognee.config.set_str("llm_api_key", "sk-test")` does not raise
- `cognee.config.get()` returns a Python dict with all public settings
- Assigning an unknown key raises `CogneeUnknownConfigKeyError`
