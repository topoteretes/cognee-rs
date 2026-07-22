# Language bindings (Python / C / JavaScript / Java)

cognee-rust ships four language bindings on top of the Rust core. All share the
same SDK-tier implementation via [`cognee-bindings-common`](../../crates/bindings-common/)
(portable op bodies + stable error codes), so their surfaces line up 1:1. Each
exposes the same flow: `warm()` → `add()` → `cognify()` → `search()`.

| Binding | README | Entry type | Async model |
|---|---|---|---|
| **Python** (PyO3) | [python/README.md](../../python/README.md) | `Cognee` (`from cognee_py import Cognee`) | native `async` |
| **C** (FFI) | [capi/README.md](../../capi/README.md) | `cg_sdk_*` over an opaque handle | callback-based (+ optional `CgSdkWaiter` sync bridge) |
| **JavaScript/TS** (Neon) | [ts/README.md](../../ts/README.md) | `Cognee` (`import { Cognee } from 'cognee-ts'`) | Promise-based |
| **Java** (JNI/jni-rs) | [java/README.md](../../java/README.md) | `Cognee` (`import ai.cognee.Cognee`) | `CompletableFuture<T>` |

Module-level helpers exist in each binding for logging/telemetry setup. The
`serve()` / `disconnect()` cloud helpers live in the closed companion packages,
not in the OSS bindings. The full per-language method list lives in each
binding's README and its generated docs.

## Configuration

All four delegate to the same `ConfigManager` in `cognee-bindings-common`; the
difference is purely ergonomic (design decision A3.1, intentional for 0.1.0):

| Binding | Setter surface |
|---|---|
| **JS** | ~40 granular typed setters (`setLlmModel`, `setEmbeddingProvider`, …) **+** 4 bulk setters (`setLlmConfig`, `setEmbeddingConfig`, `setVectorDbConfig`, `setGraphDbConfig`) **+** generic `set(key, value)` |
| **C** | `cg_sdk_config_set` / `cg_sdk_config_set_str`, the 4 bulk setters, `cg_sdk_config_get` |
| **Python** | `set`, `set_str`, the 4 bulk setters, `get` |
| **Java** | `config().set` / `config().setStr`, the 4 bulk setters, `config().get` |

The generic `set("llm_model", value)` already reaches every key, so C and Python
deliberately ship only the generic + bulk setters; JS adds the granular setters
as thin (infallible) sugar over the same underlying fields. Type errors surface
with the stable `CONFIG_TYPE_MISMATCH` code (C: `CG_ERR_CONFIG_TYPE_MISMATCH`) at
the call site. The full set of settable keys is the canonical `Settings` field
names — see [configuration.md](../configuration.md) and
[`crates/lib/src/config.rs`](../../crates/lib/src/config.rs). Field names are
camelCase in JS, snake_case in Python/C/Java (matching the struct).

### Unification path (post-0.1.0)

If full parity is required, mirror the JS macro-driven approach: add
`set_llm_model(value)` / `cg_sdk_config_set_llm_model(handle, value)` etc. as thin
`set_str("llm_model", value)` wrappers in each binding's idiomatic case
(~0.5 d/binding, mechanical).
