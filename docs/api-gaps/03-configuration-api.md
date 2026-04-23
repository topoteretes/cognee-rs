# Gap 3: Configuration API

Status: **Implemented**
Implementation plan: [impl/03-configuration-api-plan.md](impl/03-configuration-api-plan.md)

This document details the verified differences between the Python SDK's runtime-mutable configuration system and the Rust SDK's environment-only configuration.

---

## Python Configuration Architecture

**File:** `cognee/api/v1/config/config.py` (lines 18-563)

The Python SDK provides a `config` class with 33 public static setter methods that mutate singleton configuration objects at runtime:

```python
cognee.config.set_llm_model("gpt-4o-mini")
cognee.config.set_embedding_provider("openai")
cognee.config.set_vector_db_provider("qdrant")
cognee.config.system_root_directory("/data/cognee")
```

### Configuration Domains

Each domain is backed by a `pydantic.BaseSettings` singleton (cached via `@lru_cache`):

| Domain | Python Config Class | Setter Methods |
|--------|-------------------|----------------|
| **Base/System** | `BaseConfig` | `system_root_directory()`, `data_root_directory()`, `monitoring_tool()` |
| **LLM** | `LLMConfig` | `set_llm_provider()`, `set_llm_model()`, `set_llm_api_key()`, `set_llm_endpoint()`, `set_llm_config()` |
| **Embedding** | `EmbeddingConfig` | `set_embedding_provider()`, `set_embedding_model()`, `set_embedding_dimensions()`, `set_embedding_endpoint()`, `set_embedding_api_key()`, `set_embedding_config()` |
| **Vector DB** | `VectorDBConfig` | `set_vector_db_provider()`, `set_vector_db_url()`, `set_vector_db_key()`, `set_vector_db_config()` |
| **Graph DB** | `GraphConfig` | `set_graph_database_provider()`, `set_graph_model()`, `set_graph_db_config()` |
| **Relational DB** | `RelationalDBConfig` | `set_relational_db_config()`, `set_migration_db_config()` |
| **Chunking** | `ChunkConfig` | `set_chunk_strategy()`, `set_chunk_engine()`, `set_chunk_size()`, `set_chunk_overlap()` |
| **ML Models** | `CognifyConfig` | `set_classification_model()`, `set_summarization_model()` |
| **Translation** | `TranslationConfig` | `set_translation_provider()`, `set_translation_target_language()`, `set_translation_config()` |

### Setter Mechanics

- All bulk setters use `object.__setattr__(config_obj, key, value)` via `_update_config()` (lines 189-215), raising `InvalidConfigAttributeError` for unknown keys
- `set_embedding_dimensions()` coerces `str` to `int` with positive-value validation (lines 272-278)
- `system_root_directory()` cascades updates to relational DB path, graph file path, and vector DB URL (lines 55-67)
- Generic `set(key, value)` method with a 22-entry dispatch table plus embedding fallback (lines 503-562)

---

## Rust Configuration Architecture

**File:** `crates/lib/src/config.rs` (lines 7-381)

The Rust SDK uses a `Settings` struct with 57 public fields, initialized from defaults then overlaid with environment variables:

```rust
pub struct Settings {
    pub llm_provider: String,
    pub llm_model: String,
    pub llm_api_key: String,
    // ... 54 more fields
}

impl Settings {
    pub fn load_from_env() -> Self { ... }     // Defaults + env overlay
    pub fn overlay_from_env(&mut self) { ... } // Apply env vars on top
}
```

`Settings` itself is not inherently immutable (all fields are `pub`, it derives `Clone`), but there are **no setter methods** on it and no mechanism for runtime mutation once it is consumed by `ComponentManager`.

### ComponentManager

**File:** `crates/lib/src/component_manager.rs`

```rust
pub struct ComponentManager {
    settings: Settings,                                  // Owned, no mutation API
    storage: tokio::sync::OnceCell<Arc<dyn StorageTrait>>,   // Lazy, one-shot init
    database: tokio::sync::OnceCell<Arc<DatabaseConnection>>,
    graph_db: tokio::sync::OnceCell<Arc<dyn GraphDBTrait>>,
    vector_db: tokio::sync::OnceCell<Arc<dyn VectorDB>>,
    embedding_engine: tokio::sync::OnceCell<Arc<dyn EmbeddingEngine>>,
    llm: tokio::sync::OnceCell<Arc<dyn Llm>>,
}
```

- Components are initialized via `OnceCell::get_or_try_init()` -- exactly once, cached forever.
- `tokio::sync::OnceCell` has no `reset()` or `take()` method, so there is no mechanism to reinitialize components after first access.
- `ComponentManager::settings()` returns `&Settings` (immutable borrow).

### CLI Config Persistence

**File:** `crates/cli/src/config_store.rs`

The CLI has a file-based config store (`config.json`) with `set_value()` and `unset_key()` functions that can modify a `&mut Settings` before it is passed to `ComponentManager`. This allows `cognee-cli config set llm_model gpt-4o` to persist settings between invocations, but this is a **pre-construction** mutation (the settings are modified before `ComponentManager::new()` is called). It does not support runtime mutation of an already-initialized pipeline.

### Embedding Config Bypass

**File:** `crates/embedding/src/config.rs`

The `ComponentManager::init_embedding_engine()` calls `EmbeddingConfig::from_env()` directly, bypassing the `Settings` struct entirely. The `Settings` struct lacks `embedding_provider`, `embedding_endpoint`, and `embedding_api_key` fields. Even if runtime mutation were added to `Settings`, changes to these fields would not flow through to the embedding engine without refactoring this initialization path.

---

## Gap Analysis

| Capability | Python | Rust | Gap |
|------------|--------|------|-----|
| Runtime config mutation | 33 setter methods on `config` class | None (pre-construction CLI set only) | **All setters missing** |
| Cascading path updates | `system_root_directory()` cascades to 3 backends | N/A | **Missing** |
| Bulk config | `set_llm_config(dict)`, `set_embedding_config(dict)`, etc. | N/A | **Missing** |
| Generic key-value setter | `set(key, value)` with 22-entry dispatch + fallback | N/A | **Missing** |
| Config validation | `InvalidConfigAttributeError` at runtime | Compile-time types (field names checked at compile time) | Different approach (Rust is stricter) |
| Type coercion | `str` to `int` for embedding_dimensions | N/A | **Missing** (less needed in Rust's type system) |
| Config persistence | In-memory singletons (no disk persistence) | CLI persists to `config.json` (pre-construction only) | Rust has more persistence, Python has more runtime flexibility |
| Component reinitialization | Not needed (Python recreates providers on access) | `OnceCell` -- no reinitialization possible | **Missing** |
| Embedding provider config | In `Settings`-equivalent (`EmbeddingConfig`) | Separate `EmbeddingConfig::from_env()`, bypasses `Settings` | **Split config path** |
| Translation config | 3 setter methods | Translation not implemented in Rust | **Not applicable yet** |
| Missing Settings fields | N/A | `embedding_provider`, `embedding_endpoint`, `embedding_api_key` absent from `Settings` | **Missing fields** |

---

## Complete Setter Method List (Python -> Rust Mapping)

Methods that need to be implemented on a new `ConfigManager` type in Rust, matching the Python `config` class:

| Python Method | Maps to Rust Settings Field | Priority | Notes |
|---|---|---|---|
| `set_llm_provider(str)` | `llm_provider` | High | |
| `set_llm_model(str)` | `llm_model` | High | |
| `set_llm_api_key(str)` | `llm_api_key` | High | |
| `set_llm_endpoint(str)` | `llm_endpoint` | High | |
| `set_llm_config(dict)` | Multiple `llm_*` fields | Medium | Bulk setter |
| `set_embedding_provider(str)` | `embedding_provider` (NEW field needed) | High | Field missing from Settings |
| `set_embedding_model(str)` | `embedding_model_name` | High | |
| `set_embedding_dimensions(int)` | `embedding_dimensions` | High | |
| `set_embedding_endpoint(str)` | `embedding_endpoint` (NEW field needed) | Medium | Field missing from Settings |
| `set_embedding_api_key(str)` | `embedding_api_key` (NEW field needed) | Medium | Field missing from Settings |
| `set_embedding_config(dict)` | Multiple `embedding_*` fields | Medium | Bulk setter |
| `set_vector_db_provider(str)` | `vector_db_provider` | High | |
| `set_vector_db_url(str)` | `vector_db_url` | Medium | |
| `set_vector_db_key(str)` | `vector_db_key` | Medium | |
| `set_vector_db_config(dict)` | Multiple `vector_db_*` fields | Medium | Bulk setter |
| `set_graph_database_provider(str)` | `graph_database_provider` | High | |
| `set_graph_model(str)` | `graph_model` | Medium | |
| `set_graph_db_config(dict)` | Multiple `graph_*` fields | Medium | Bulk setter |
| `set_relational_db_config(dict)` | Multiple `db_*`/`relational_*` fields | Low | Bulk setter |
| `set_migration_db_config(dict)` | `migration_db_url` | Low | Bulk setter |
| `set_chunk_strategy(str)` | `chunk_strategy` | Medium | |
| `set_chunk_engine(str)` | `chunk_engine` | Medium | |
| `set_chunk_size(u32)` | `chunk_size` | Medium | |
| `set_chunk_overlap(u32)` | `chunk_overlap` | Medium | |
| `set_classification_model(str)` | `classification_model` | Low | |
| `set_summarization_model(str)` | `summarization_model` | Low | |
| `system_root_directory(str)` | `system_root_directory` (with cascading) | High | Cascades to graph/vector paths |
| `data_root_directory(str)` | `data_root_directory` | High | |
| `monitoring_tool(object)` | `monitoring_tool` | Low | |
| `set_translation_provider(str)` | N/A (translation not in Rust) | Deferred | |
| `set_translation_target_language(str)` | N/A (translation not in Rust) | Deferred | |
| `set_translation_config(dict)` | N/A (translation not in Rust) | Deferred | |
| `set(key, value)` | Generic dispatch | Low | 22-entry dispatch table |

---

## Rust Files Involved

| File | Current Role |
|------|-------------|
| `crates/lib/src/config.rs` | `Settings` struct (57 fields), `load_from_env()`, `overlay_from_env()` |
| `crates/lib/src/component_manager.rs` | `ComponentManager` with `OnceCell` caching, accepts owned `Settings` |
| `crates/lib/src/context.rs` | `PipelineContext` trait (component accessor methods) |
| `crates/lib/src/error.rs` | `ComponentError` enum (no `ConfigError` variant yet) |
| `crates/lib/src/lib.rs` | Re-exports `Settings`, `ComponentManager`, `PipelineContext` |
| `crates/cli/src/config_store.rs` | CLI config persistence (`set_value`, `unset_key`, `known_keys`) |
| `crates/cli/src/main.rs` | Constructs `ComponentManager::new(settings)` |
| `crates/embedding/src/config.rs` | `EmbeddingConfig::from_env()` -- bypasses `Settings` |
