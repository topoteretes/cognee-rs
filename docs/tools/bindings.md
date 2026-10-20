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
`serve()` / `disconnect()` cloud helpers are not part of the OSS bindings. The full per-language method list lives in each
binding's README and its generated docs.

## Per-call cognify options

`cognify` / `addAndCognify` take an options object parsed once, in
`cognee_bindings_common::ops::pipeline::apply_cognify_opts`, for every binding:
`tenant`, `chunkSize`, `chunkOverlap`, `summarization`, `temporalCognify`,
`triplet`, `graphBackend`, `graphSchema` (a custom graph model's JSON Schema, as
an object or a JSON string; alias `graphModel`) and `chunkAttachment`
(`"direct"` or `"all"`). Python accepts the snake_case spellings too, and its
compat `cognify(graph_model=…)` converts a Pydantic model class to its JSON
Schema. A wrongly-typed or invalid value, or a combination `CognifyConfig`
rejects, fails with the `VALIDATION_ERROR` code before the dataset is looked up
(and, for `addAndCognify`, before anything is added); an unknown key is logged
as a warning. See [the custom graph schema guide](../guides/custom-graph-schema.md).

| | Python | TypeScript | Java | C |
|---|---|---|---|---|
| Graph schema | `opts={"graph_schema": …}`; compat `graph_model=` | `{ graphSchema }` | `CognifyOptions.graphSchema(json \| map)` | `"graphSchema"` in `opts_json` |
| Chunk attachment | `opts={"chunk_attachment": "all"}` | `{ chunkAttachment: "all" }` | `CognifyOptions.chunkAttachment("all")` | `"chunkAttachment"` in `opts_json` |

## Process-wide configuration (logging, OTLP, analytics)

Logging, OpenTelemetry export and product analytics each configure a
process-wide singleton, so they are **not** handle settings: a handle's
`config.set(...)` does not reach them. Each binding exposes the same module-level
surface, resolved by one shared path (`cognee_bindings_common::process_config`):
**defaults < environment < options passed from code**. See
[configuration.md § Logging](../configuration.md#logging) and
[§ Observability & telemetry](../configuration.md#observability--telemetry) for
the keys.

| | Python | TypeScript | Java | C |
|---|---|---|---|---|
| Logging | `setup_logging(options=None)` | `setupLogging(options?)` | `Cognee.setupLogging(Map)` | `cognee_setup_logging_with_options(json)` |
| OTLP export | `setup_telemetry(options=None)` | `setupTelemetry(options?)` | `Cognee.initOtlp(Map)` | `cognee_init_otlp_with_options(json)` |
| Analytics off/on | `set_telemetry_enabled(bool)` | `setTelemetryEnabled(bool)` | `Cognee.setTelemetryEnabled(boolean)` | `cognee_set_telemetry_enabled(int)` |
| Analytics state | `is_telemetry_enabled()` | `isTelemetryEnabled()` | `Cognee.isTelemetryEnabled()` | `cognee_is_telemetry_enabled()` |

Analytics are on by default (opt-out). The switch wins over `TELEMETRY_DISABLED` /
`ENV`, which are read when an event is sent (so a `.env` loaded later counts). Logging and OTLP setup latch on their first
call. The argument-less forms (`setup_logging()`, `cognee_setup_logging()`, …) are
unchanged and read the environment only.

## Configuration

All four delegate to the same `ConfigManager` in `cognee-bindings-common`; the
difference is purely ergonomic (design decision A3.1, intentional for 0.1.0):

| Binding | Setter surface |
|---|---|
| **JS** | ~40 granular typed setters (`setLlmModel`, `setEmbeddingProvider`, …) **+** 4 bulk setters (`setLlmConfig`, `setEmbeddingConfig`, `setVectorDbConfig`, `setGraphDbConfig`) **+** generic `set(key, value)` |
| **C** | `cg_sdk_config_set` / `cg_sdk_config_set_str`, the 4 bulk setters, `cg_sdk_config_get` |
| **Python** | `set`, `set_str`, the 4 bulk setters, `get` |
| **Java** | `config().set` / `config().setStr`, the 4 bulk setters, `config().get` |

The generic `set("llm_model", value)` reaches every key in the
[key registry](../../crates/lib/src/config/keys.rs), so C and Python
deliberately ship only the generic + bulk setters; JS adds the granular setters
as thin (infallible) sugar over the same underlying fields. Type errors surface
with the stable `CONFIG_TYPE_MISMATCH` code (C: `CG_ERR_CONFIG_TYPE_MISMATCH`) at
the call site. The full set of settable keys is the canonical `Settings` field
names plus their registry aliases — see [configuration.md](../configuration.md) and
[`crates/lib/src/config/keys.rs`](../../crates/lib/src/config/keys.rs). Field names are
camelCase in JS, snake_case in Python/C/Java (matching the struct).

### Unification path (post-0.1.0)

If full parity is required, mirror the JS macro-driven approach: add
`set_llm_model(value)` / `cg_sdk_config_set_llm_model(handle, value)` etc. as thin
`set_str("llm_model", value)` wrappers in each binding's idiomatic case
(~0.5 d/binding, mechanical).
