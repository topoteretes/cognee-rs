# Config-Setter Surface — Design Decision (A3.1)

## Summary

The three language bindings expose the same underlying `ConfigManager` through
different ergonomic surfaces.  This document records the **intentional** shape
decision so it is not re-litigated in future reviews.

## Surface comparison

| Binding | Setter surface |
|---|---|
| **JS** (`js/cognee-neon/src/config.rs`) | 40 granular typed setters (`setLlmModel`, `setEmbeddingProvider`, …) **plus** 4 bulk config setters (`setLlmConfig`, `setEmbeddingConfig`, `setVectorDbConfig`, `setGraphDbConfig`) **plus** the generic `set(key, value)` |
| **C** (`capi/cognee-capi/src/sdk_config.rs`) | `cg_sdk_config_set` / `cg_sdk_config_set_str`, the same 4 bulk setters, and `cg_sdk_config_get` |
| **Python** (`python/src/config.rs`) | `set`, `set_str`, the same 4 bulk setters, and `get` |

All three delegate to the same `ConfigManager::config()` methods inside
`cognee-bindings-common`.  The difference is **purely ergonomic**.

## Decision: document as intentional for 0.1.0

Adding the ~40 granular setters to C and Python would introduce ~80 new
FFI/PyO3 wrapper functions (mechanical, but high surface area with no
functional value).  The generic `set("llmModel", value)` already reaches every
key.

For 0.1.0 the decision is:

- **C and Python use generic `set`/`set_str` + 4 bulk config setters** by
  design.  Every config key is reachable; type errors surface with the stable
  `CONFIG_TYPE_MISMATCH` code (C: `CG_ERR_CONFIG_TYPE_MISMATCH`) at the point of
  the call.
- **JS adds granular typed setters as sugar** (`c.config.setLlmModel(…)`).
  Each of those 40 setters is a thin (infallible) wrapper that writes the same
  underlying `ConfigManager` field as the generic setter would reach — there is
  no extra validation or behaviour.
- **The full list of settable keys** is the canonical `Settings` field names
  (see `crates/lib/src/config.rs`).  The field names in JS are camelCase; in
  Python/C they are snake_case (matching the struct field names).

## Unification path (if required post-0.1.0)

If a reviewer requires full parity, mirror the JS macro-driven approach:

- **Python**: add `set_llm_model(value)`, `set_embedding_provider(value)`, …
  as PyO3 `#[pyfn]` wrappers, each calling `set_str("llm_model", value)`.
- **C**: add `cg_sdk_config_set_llm_model(handle, value)`, … as thin `set_str`
  wrappers.

Name the functions in the binding's idiomatic case (`set_llm_model` for Python;
`cg_sdk_config_set_llm_model` for C).  This is mechanical work estimated at
~0.5d per binding.

## Related docs

- JS README `## Config` section — documents `c.config.set*` granular
  setters and their fallibility contract.
- Python README `### Programmatic config` section — documents `cognee.config.set`
  / `set_str` / `get` and references bulk config setters.
- `crates/lib/src/config.rs` — canonical `Settings` struct with all field names
  and types.
