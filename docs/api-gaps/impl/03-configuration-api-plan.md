# Implementation Plan: Configuration API (Gap 3)

This document provides a detailed step-by-step plan to close the configuration API gap between the Python and Rust SDKs. The goal is to enable runtime-mutable configuration with component reinitialization, matching the Python SDK's `cognee.config.set_*()` API surface.

---

## Phase 1: Mutable Settings via ConfigManager

### Goal

Introduce a `ConfigManager` wrapper around `Settings` that provides thread-safe runtime mutation through setter methods, matching the Python SDK's `config` class.

### Step 1.1: Create ConfigManager struct

**File:** `crates/lib/src/config.rs` (after line 307, before the `Default` impl)

Add a new `ConfigManager` type that wraps `Settings` in `Arc<RwLock<Settings>>`:

```rust
use std::sync::{Arc, RwLock, RwLockReadGuard};

/// Thread-safe mutable configuration manager.
///
/// Wraps `Settings` in `Arc<RwLock<>>` to allow runtime mutation from
/// setter methods. Tracks a monotonically increasing version counter
/// so that `ComponentManager` can detect stale cached components and
/// reinitialize them.
pub struct ConfigManager {
    inner: Arc<RwLock<Settings>>,
    version: Arc<std::sync::atomic::AtomicU64>,
}

impl ConfigManager {
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: Arc::new(RwLock::new(settings)),
            version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Obtain a read-lock on the current settings.
    pub fn read(&self) -> RwLockReadGuard<'_, Settings> {
        self.inner.read().unwrap() // lock poison is unrecoverable
    }

    /// Current config version (monotonically increasing on each mutation).
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Bump the version after any mutation.
    fn bump_version(&self) {
        self.version.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

impl Clone for ConfigManager {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            version: Arc::clone(&self.version),
        }
    }
}
```

### Step 1.2: Add individual setter methods

**File:** `crates/lib/src/config.rs`

Add a second `impl ConfigManager` block with setter methods. Each setter acquires a write lock, mutates the field, and bumps the version. Group by domain to match the Python `config` class.

**LLM setters** (matching Python `set_llm_provider()`, `set_llm_model()`, `set_llm_api_key()`, `set_llm_endpoint()`):

```rust
impl ConfigManager {
    // -- LLM -----------------------------------------------------------------
    pub fn set_llm_provider(&self, provider: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.llm_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_model(&self, model: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.llm_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_api_key(&self, key: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.llm_api_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.llm_endpoint = endpoint.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Embedding -----------------------------------------------------------
    pub fn set_embedding_model(&self, model: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.embedding_model_name = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_dimensions(&self, dims: u32) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.embedding_dimensions = dims;
        drop(s);
        self.bump_version();
    }

    // -- Vector DB -----------------------------------------------------------
    pub fn set_vector_db_provider(&self, provider: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.vector_db_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_url(&self, url: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.vector_db_url = url.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_key(&self, key: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.vector_db_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Graph DB ------------------------------------------------------------
    pub fn set_graph_database_provider(&self, provider: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.graph_database_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Chunking ------------------------------------------------------------
    pub fn set_chunk_strategy(&self, strategy: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.chunk_strategy = strategy.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_chunk_size(&self, size: u32) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.chunk_size = size;
        drop(s);
        self.bump_version();
    }

    pub fn set_chunk_overlap(&self, overlap: u32) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.chunk_overlap = overlap;
        drop(s);
        self.bump_version();
    }

    // -- System paths --------------------------------------------------------
    pub fn set_data_root_directory(&self, path: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        s.data_root_directory = path.to_string();
        drop(s);
        self.bump_version();
    }

    // (see Phase 3 for set_system_root_directory with cascading)
}
```

### Step 1.3: Add bulk setter and generic set() dispatch

**File:** `crates/lib/src/config.rs`

Add a `ConfigError` enum and the `set()` dispatch method, mirroring Python's 22-entry dispatch table in `config.set()`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Unknown config key: {0}")]
    UnknownKey(String),
    #[error("Type mismatch for key '{key}': {reason}")]
    TypeMismatch { key: String, reason: String },
}

impl ConfigManager {
    /// Generic setter matching Python's `config.set(key, value)`.
    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), ConfigError> {
        // ... dispatch table (see Phase 4 details)
    }
}
```

### Files to modify in Phase 1

| File | Lines | Action |
|------|-------|--------|
| `crates/lib/src/config.rs` | After line 307 | Add `ConfigManager` struct and impl blocks |
| `crates/lib/src/lib.rs` | Line 105 | Add `pub use config::ConfigManager;` |
| `crates/lib/src/error.rs` | After line 31 | Add `ConfigError` if not placed in `config.rs` |

---

## Phase 2: Component Reinitialization in ComponentManager

### Goal

Modify `ComponentManager` to accept a `ConfigManager` (instead of a bare `Settings`) and invalidate cached components when the config version changes.

### Step 2.1: Replace Settings with ConfigManager

**File:** `crates/lib/src/component_manager.rs`

Change the `ComponentManager` struct (currently lines 33-41) to hold a `ConfigManager` instead of `Settings`, and replace `OnceCell` with a versioned cache pattern:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock as TokioRwLock;

pub struct ComponentManager {
    config: ConfigManager,
    // Each cached component stores (version_at_creation, component_arc).
    // When the config version advances past the cached version, the
    // component is lazily re-created on next access.
    storage: TokioRwLock<Option<(u64, Arc<dyn StorageTrait>)>>,
    database: TokioRwLock<Option<(u64, Arc<DatabaseConnection>)>>,
    graph_db: TokioRwLock<Option<(u64, Arc<dyn GraphDBTrait>)>>,
    vector_db: TokioRwLock<Option<(u64, Arc<dyn VectorDB>)>>,
    embedding_engine: TokioRwLock<Option<(u64, Arc<dyn EmbeddingEngine>)>>,
    llm: TokioRwLock<Option<(u64, Arc<dyn Llm>)>>,
}
```

### Step 2.2: Versioned accessor pattern

**File:** `crates/lib/src/component_manager.rs`

Replace the current `PipelineContext` impl (lines 290-329). Each accessor checks the current config version against the cached component's version:

```rust
#[async_trait]
impl PipelineContext for ComponentManager {
    async fn storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let current_ver = self.config.version();
        // Fast path: read lock to check cache hit
        {
            let guard = self.storage.read().await;
            if let Some((ver, ref s)) = *guard {
                if ver == current_ver {
                    return Ok(Arc::clone(s));
                }
            }
        }
        // Slow path: write lock to reinitialize
        let mut guard = self.storage.write().await;
        // Double-check (another task may have reinitialized while we waited)
        if let Some((ver, ref s)) = *guard {
            if ver == current_ver {
                return Ok(Arc::clone(s));
            }
        }
        let new = self.init_storage().await?;
        *guard = Some((current_ver, Arc::clone(&new)));
        Ok(new)
    }
    // ... same pattern for database, graph_db, vector_db, embedding_engine, llm
}
```

### Step 2.3: Update constructor and settings() accessor

**File:** `crates/lib/src/component_manager.rs` (lines 43-58)

```rust
impl ComponentManager {
    pub fn new(config: ConfigManager) -> Self {
        Self {
            config,
            storage: TokioRwLock::new(None),
            database: TokioRwLock::new(None),
            graph_db: TokioRwLock::new(None),
            vector_db: TokioRwLock::new(None),
            embedding_engine: TokioRwLock::new(None),
            llm: TokioRwLock::new(None),
        }
    }

    /// Read-only snapshot of current settings.
    pub fn settings(&self) -> std::sync::RwLockReadGuard<'_, Settings> {
        self.config.read()
    }

    /// Access the underlying ConfigManager for runtime mutation.
    pub fn config(&self) -> &ConfigManager {
        &self.config
    }
}
```

### Step 2.4: Update all call sites that use `ComponentManager::new(settings)`

The CLI main entry point (`crates/cli/src/main.rs`, line 23) currently does:
```rust
let cm = Arc::new(ComponentManager::new(settings));
```

This must change to:
```rust
let config = ConfigManager::new(settings);
let cm = Arc::new(ComponentManager::new(config));
```

All callers that use `cm.settings()` return a `&Settings` today. After the change, `cm.settings()` will return a `RwLockReadGuard<Settings>` which auto-derefs to `&Settings`. Most call sites use it as `cm.settings().field_name` which works the same way through `Deref`. However, any site that stores the return value (e.g. `let settings = cm.settings();`) will need to bind a guard instead.

**Affected call sites** (grep for `cm.settings()` and `ComponentManager::new`):

| File | Lines | Change |
|------|-------|--------|
| `crates/cli/src/main.rs` | 22-23 | Wrap settings in `ConfigManager` |
| `crates/cli/src/commands/cognify.rs` | 15, 98, 105 | `settings` binding is now a guard (works unchanged through Deref) |
| `crates/cli/src/commands/add.rs` | 13, 16 | Same |
| `crates/cli/src/commands/search.rs` | 17 | Same |
| `crates/cli/src/commands/memify.rs` | 15 | Same |
| `crates/cli/src/commands/add_and_cognify.rs` | 18, 30-31 | Same |
| `crates/cli/src/commands/delete.rs` | 27, 30 | Same |
| Integration tests in `crates/cli/tests/` | Multiple | Wrap settings in `ConfigManager` |

### Thread-safety considerations for Phase 2

- `TokioRwLock` (not `std::sync::RwLock`) is used for the component cache because the `init_*` methods are async and hold the lock across `.await` points. Using `std::sync::RwLock` across `.await` would cause deadlocks.
- `std::sync::RwLock` is fine for the `Settings` wrapper in `ConfigManager` because the read/write operations on settings are synchronous and fast (no `.await`).
- The double-checked locking pattern in the accessor prevents redundant initialization when multiple tasks race to reinitialize after a version bump.
- `Arc<AtomicU64>` for the version counter is lock-free and `Sync`.

### Files to modify in Phase 2

| File | Lines | Action |
|------|-------|--------|
| `crates/lib/src/component_manager.rs` | 1-329 | Replace OnceCell with versioned cache, accept ConfigManager |
| `crates/lib/src/context.rs` | 1-29 | No change needed (PipelineContext trait is unchanged) |
| `crates/lib/src/lib.rs` | 104 | Add `pub use config::ConfigManager;` re-export |
| `crates/cli/src/main.rs` | 22-23 | Wrap settings in ConfigManager before ComponentManager |
| `crates/cli/src/commands/*.rs` | Various | Minimal -- RwLockReadGuard auto-derefs |
| `crates/cli/tests/*.rs` | Various | Update ComponentManager construction |

---

## Phase 3: Cascading Path Updates

### Goal

When `system_root_directory` changes, cascade derived path updates to graph and vector DB paths, matching the Python `config.system_root_directory()` behavior.

### Step 3.1: Implement set_system_root_directory with cascading

**File:** `crates/lib/src/config.rs`

```rust
impl ConfigManager {
    /// Set system root directory and cascade derived path updates.
    ///
    /// Matches Python `config.system_root_directory()` (config.py lines 41-67):
    /// - graph_file_path updated if it was under the old system root
    /// - vector_db_url updated if it was under the old system root
    /// - relational_db_url updated if it was under the old system root
    pub fn set_system_root_directory(&self, path: &str) {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        let old_root = s.system_root_directory.clone();
        s.system_root_directory = path.to_string();

        // Cascade graph_file_path
        if s.graph_file_path.is_empty() || s.graph_file_path.starts_with(&old_root) {
            let suffix = if s.graph_file_path.is_empty() {
                "graph".to_string()
            } else {
                s.graph_file_path[old_root.len()..].to_string()
            };
            s.graph_file_path = format!("{path}{suffix}");
        }

        // Cascade vector_db_url (only if it was using the default system root path)
        if s.vector_db_url.is_empty() || s.vector_db_url.starts_with(&old_root) {
            let suffix = if s.vector_db_url.is_empty() {
                "/vectors".to_string()
            } else {
                s.vector_db_url[old_root.len()..].to_string()
            };
            s.vector_db_url = format!("{path}{suffix}");
        }

        drop(s);
        self.bump_version();
    }
}
```

### Files to modify in Phase 3

| File | Lines | Action |
|------|-------|--------|
| `crates/lib/src/config.rs` | Phase 1 additions | Add `set_system_root_directory()` |

---

## Phase 4: Bulk Config and Generic set() Dispatch

### Goal

Implement `set_llm_config(HashMap)`, `set_embedding_config(HashMap)`, and the generic `set(key, value)` dispatch, matching the Python bulk setter pattern.

### Step 4.1: Bulk setters

**File:** `crates/lib/src/config.rs`

```rust
impl ConfigManager {
    /// Bulk-update LLM config from a map. Matches Python `config.set_llm_config()`.
    pub fn set_llm_config(&self, values: &HashMap<String, serde_json::Value>) -> Result<(), ConfigError> {
        let mut s = self.inner.write().unwrap(); // lock poison is unrecoverable
        for (key, value) in values {
            match key.as_str() {
                "llm_provider" => s.llm_provider = as_string(key, value)?,
                "llm_model" => s.llm_model = as_string(key, value)?,
                "llm_api_key" => s.llm_api_key = as_string(key, value)?,
                "llm_endpoint" => s.llm_endpoint = as_string(key, value)?,
                "llm_api_version" => s.llm_api_version = as_string(key, value)?,
                "llm_temperature" => s.llm_temperature = as_f64(key, value)?,
                "llm_max_completion_tokens" => s.llm_max_completion_tokens = as_u32(key, value)?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        drop(s);
        self.bump_version();
        Ok(())
    }

    // Similarly: set_embedding_config, set_vector_db_config, set_graph_db_config
}
```

### Step 4.2: Generic set() dispatch table

**File:** `crates/lib/src/config.rs`

Match Python's 22-entry dispatch table from `config.py` lines 526-549:

```rust
impl ConfigManager {
    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), ConfigError> {
        match key {
            "llm_provider" => self.set_llm_provider(value.as_str().ok_or_else(|| ...)?),
            "llm_model" => self.set_llm_model(value.as_str().ok_or_else(|| ...)?),
            // ... all 22 Python dispatch entries ...
            _ => return Err(ConfigError::UnknownKey(key.to_string())),
        }
        Ok(())
    }
}
```

### Files to modify in Phase 4

| File | Lines | Action |
|------|-------|--------|
| `crates/lib/src/config.rs` | Phase 1 additions | Add bulk setters and `set()` dispatch |

---

## Phase 5: Expose Configuration API in Bindings

### Goal

Expose the `ConfigManager` setter methods through the Python (PyO3), JavaScript (Neon), and C (FFI) bindings so that downstream users of those bindings get the same `cognee.config.set_*()` API as the Python SDK.

### Step 5.1: Python bindings

**File:** `python/src/lib.rs` (new `config` submodule)

```rust
#[pyclass]
struct Config {
    inner: Arc<ConfigManager>,
}

#[pymethods]
impl Config {
    fn set_llm_model(&self, model: &str) { self.inner.set_llm_model(model); }
    fn set_llm_api_key(&self, key: &str) { self.inner.set_llm_api_key(key); }
    // ... all setters
}
```

### Step 5.2: JavaScript bindings

**File:** `js/src/lib.rs` (new config functions)

### Step 5.3: C API bindings

**File:** `capi/src/lib.rs` (new `cognee_config_set_*` functions)

### Files to modify in Phase 5

| File | Action |
|------|--------|
| `python/src/lib.rs` | Add Config pyclass with setter methods |
| `js/src/lib.rs` | Add config setter Neon functions |
| `capi/src/lib.rs` | Add `cognee_config_set_*` C functions |

---

## Phase 6: Missing Settings Fields

### Goal

Add fields to the Rust `Settings` struct that exist in the Python config system but are currently absent.

### Missing fields identified

| Python field | Python config class | Rust equivalent needed |
|---|---|---|
| `embedding_provider` | `EmbeddingConfig` | Add to `Settings`; currently only in `EmbeddingConfig::from_env()` |
| `embedding_endpoint` | `EmbeddingConfig` | Add to `Settings`; currently only in `EmbeddingConfig::from_env()` |
| `embedding_api_key` | `EmbeddingConfig` | Add to `Settings`; currently only in `EmbeddingConfig::from_env()` |
| `translation_provider` | `TranslationConfig` | Not yet in Rust at all |
| `translation_target_language` | `TranslationConfig` | Not yet in Rust at all |

**Note:** The embedding provider/endpoint/api_key fields currently live in `cognee_embedding::EmbeddingConfig` which reads directly from env vars (bypassing `Settings`). The `ComponentManager::init_embedding_engine()` (line 214-219) calls `EmbeddingConfig::from_env()` ignoring the `Settings` struct entirely. This must be unified so that runtime config changes via `ConfigManager` flow through to the embedding engine.

### Step 6.1: Add fields to Settings

**File:** `crates/lib/src/config.rs` (within the `Settings` struct, after line 78)

```rust
pub embedding_provider: String,    // default: "onnx"
pub embedding_endpoint: String,    // default: ""
pub embedding_api_key: String,     // default: ""
```

### Step 6.2: Wire embedding init through Settings

**File:** `crates/lib/src/component_manager.rs` (lines 214-219)

Change `init_embedding_engine` to construct `EmbeddingConfig` from `Settings` fields rather than calling `EmbeddingConfig::from_env()`.

### Files to modify in Phase 6

| File | Lines | Action |
|------|-------|--------|
| `crates/lib/src/config.rs` | 78 | Add missing embedding fields |
| `crates/lib/src/config.rs` | Default impl | Add defaults for new fields |
| `crates/lib/src/config.rs` | `overlay_from_env()` | Add env var reads for new fields |
| `crates/lib/src/component_manager.rs` | 214-219 | Use Settings instead of EmbeddingConfig::from_env() |
| `crates/cli/src/config_store.rs` | Various | Add new keys to known_keys, as_flat_map, set_value, unset_key |

---

## Implementation Order and Dependencies

```
Phase 1 (ConfigManager struct + setters)
  |
  v
Phase 2 (ComponentManager versioned cache) -- depends on Phase 1
  |
  v
Phase 3 (Cascading paths) -- can be done during Phase 1 or 2
  |
  v
Phase 4 (Bulk setters + generic dispatch) -- depends on Phase 1
  |
  v
Phase 5 (Bindings) -- depends on Phases 1-4
  |
  v
Phase 6 (Missing fields) -- can be done in parallel with Phases 2-4
```

Phases 1 and 6 can be started in parallel. Phase 3 and 4 build on Phase 1. Phase 2 is the most complex change. Phase 5 is the final step.

---

## Testing Strategy

1. **Unit tests** for `ConfigManager` setters: verify field mutation, version bump, cascading.
2. **Integration tests** for versioned `ComponentManager`: change config, verify old component is dropped and new one created.
3. **CLI E2E tests**: extend existing `config_set`/`config_get` roundtrip tests.
4. **Thread-safety tests**: concurrent reads/writes on `ConfigManager` from multiple tokio tasks.
5. **Binding tests**: verify setter methods work from Python/JS/C.

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Breaking change to `ComponentManager::new()` signature | Phase 2 must update all call sites atomically in a single commit |
| `RwLockReadGuard` lifetime issues at call sites | Most call sites use `cm.settings().field` which works through `Deref`; sites that bind to a variable just need a `let guard = cm.settings();` rename |
| Embedding engine init bypasses Settings | Phase 6 addresses this by wiring embedding init through Settings |
| Performance regression from RwLock on hot path | Read-heavy access pattern; `RwLock` allows concurrent reads; version check is lock-free `AtomicU64` |
| Component teardown on reinit | Old `Arc<dyn Trait>` is dropped when the last reference goes away; no explicit teardown needed |
