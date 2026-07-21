# cognee-capi

C bindings for the [cognee-rs](https://github.com/topoteretes/cognee-rs) AI-memory library.
Exposes two tiers:

- **SDK tier** (`cognee_sdk.h`, `cg_sdk_*`) — the user-facing surface: handle lifecycle,
  add/cognify/search, memory ops, dataset management, config.  All ops are async (callback-based).
- **Engine tier** (`cognee.h`, `cg_*`) — the low-level pipeline-execution primitives.
  Advanced use only; most embedders only need the SDK tier.

Both tiers build as `libcognee_capi.{a,so,dylib}`.

## Quick start

### Build

```bash
cd capi
mkdir -p build
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
```

Link with `-lcognee_capi` (and `-ldl -lm -lpthread` on Linux).

### Three-step pattern: init → warm → ops

```c
#include "cognee_sdk.h"   /* also pulls in cognee.h */
#include <stdio.h>
#include <stdlib.h>
#include <assert.h>

int main(void) {
    /* 1. Init the async runtime (must come before cg_sdk_new). */
    assert(cg_init() == CG_OK);

    /* 2. Create a handle from environment defaults (or a JSON override). */
    CgSdk* sdk = cg_sdk_new(
        "{\"llm_api_key\":\"sk-…\","
        " \"embedding_provider\":\"openai\","
        " \"embedding_model\":\"text-embedding-3-small\"}"
    );
    assert(sdk != NULL);

    /* 3. Warm: build DB connections, bootstrap user, init embedding engine. */
    CgSdkWaiter* w = cg_sdk_waiter_new();
    cg_sdk_warm(sdk, cg_sdk_waiter_callback, w);
    char* result = NULL;
    assert(cg_sdk_waiter_wait(w, &result) == CG_OK);
    cg_string_destroy(result);
    cg_sdk_waiter_destroy(w);

    /* 4. Add text data. */
    w = cg_sdk_waiter_new();
    cg_sdk_add(sdk,
               "{\"type\":\"text\",\"text\":\"The Eiffel Tower is in Paris.\"}",
               "my-dataset",
               NULL,          /* opts_json */
               cg_sdk_waiter_callback, w);
    assert(cg_sdk_waiter_wait(w, &result) == CG_OK);
    printf("add result: %s\n", result);
    cg_string_destroy(result);
    cg_sdk_waiter_destroy(w);

    /* 5. Search. */
    w = cg_sdk_waiter_new();
    cg_sdk_search(sdk, "Where is the Eiffel Tower?", NULL,
                  cg_sdk_waiter_callback, w);
    assert(cg_sdk_waiter_wait(w, &result) == CG_OK);
    printf("search result: %s\n", result);
    cg_string_destroy(result);
    cg_sdk_waiter_destroy(w);

    cg_sdk_destroy(sdk);
    cg_shutdown();
    return 0;
}
```

See [`examples/example_sdk_add.c`](examples/example_sdk_add.c) and
[`examples/example_sdk_add_cognify_search.c`](examples/example_sdk_add_cognify_search.c)
for complete runnable examples.

## Examples

Runnable C examples are in the [`examples/`](examples/) directory:

| Example | What it covers |
|---|---|
| [`example_sdk_add.c`](examples/example_sdk_add.c) | Add text data to a dataset |
| [`example_sdk_add_cognify.c`](examples/example_sdk_add_cognify.c) | Add + cognify |
| [`example_sdk_add_cognify_search.c`](examples/example_sdk_add_cognify_search.c) | Full add → cognify → search pipeline |
| [`example_pipeline.c`](examples/example_pipeline.c) | Low-level pipeline engine |
| [`example_sync_task.c`](examples/example_sync_task.c) | Synchronous task |
| [`example_async_task.c`](examples/example_async_task.c) | Asynchronous task |
| [`example_batch_task.c`](examples/example_batch_task.c) | Batched task |
| [`example_cancellation.c`](examples/example_cancellation.c) | Cancellation |
| [`example_iter_task.c`](examples/example_iter_task.c) | Iterator task |
| [`example_background_task.c`](examples/example_background_task.c) | Background task |

Build the examples alongside the library:

```bash
cd capi
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
./build/example_sdk_add_cognify_search
```

## Async model

All `cg_sdk_*` operations are asynchronous and fire their `CgSdkResultCallback` on a tokio
worker thread — **never** synchronously from the initiating call (D4, R1).

For single-threaded C programs the `CgSdkWaiter` sync bridge provides a blocking wait:

```c
CgSdkWaiter* w = cg_sdk_waiter_new();
cg_sdk_cognify(sdk, "my-dataset", NULL, cg_sdk_waiter_callback, w);
char* json = NULL;
CgErrorCode code = cg_sdk_waiter_wait(w, &json);
/* use json … */
cg_string_destroy(json);   /* always free with cg_string_destroy */
cg_sdk_waiter_destroy(w);  /* single-use — destroy after each wait */
```

Never call `cg_sdk_waiter_wait` from inside a callback — it will deadlock.

## Memory ownership

| Function | Who frees? |
|---|---|
| `cg_sdk_waiter_wait` output (`char**`) | Caller — use `cg_string_destroy` |
| `result_json` inside a `CgSdkResultCallback` | **Do not free** — valid only for the callback's duration; copy if needed |
| `error_message` inside a callback | Same: valid only during the callback |
| `CgSdk*` from `cg_sdk_new` / `cg_sdk_clone` | Caller — use `cg_sdk_destroy` |
| `CgSdkWaiter*` from `cg_sdk_waiter_new` | Caller — use `cg_sdk_waiter_destroy` |

## Error handling

Async ops deliver errors via the callback's `code` and `error_message` parameters:

```c
void my_cb(CgErrorCode code, const char* result_json,
           const char* error_message, void* user_data) {
    if (code != CG_OK) {
        fprintf(stderr, "error %d: %s\n", code, error_message ? error_message : "");
        return;
    }
    /* use result_json … */
}
```

SDK codes (11–18) map to TypeScript `SdkError` kind strings; see `cognee_sdk.h` for the full
mapping table. Engine codes 2 and 4–9 never appear in SDK-tier results (R2).

Callbacks fire on tokio worker threads.  If your host requires thread affinity, marshal back
yourself before touching non-thread-safe state.

## Config

Call synchronous `cg_sdk_config_set` / `cg_sdk_config_set_str` at any time.  Changes take
effect on the next `cg_sdk_warm` (or the next service-requiring op, which warms lazily):

```c
cg_sdk_config_set_str(sdk, "llm_api_key", "sk-…");
cg_sdk_config_set_str(sdk, "embedding_provider", "openai");
cg_sdk_config_set(sdk, "llm_temperature", "0.3");
```

Read back the current (redacted) config:

```c
char* cfg = NULL;
assert(cg_sdk_config_get(sdk, &cfg) == CG_OK);
printf("%s\n", cfg);
cg_string_destroy(cfg);
```

## Feature flags

| CMake flag | Cargo equivalent | Effect |
|---|---|---|
| default | all default features | Full build: visualization, cloud, qdrant, ladybug, onnx, hf-tokenizer, tiktoken, sqlite |
| `-DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON -DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing` | `--no-default-features --features sqlite,testing` | Slim/embedded build; `cg_sdk_visualize` and cloud ops return `CG_ERR_FEATURE_NOT_BUILT` |

## Platform support

Tested on Linux x86_64 (CI) and Android aarch64 (slim build, ONNX local embeddings).

## Environment variables

| Variable | Purpose |
|---|---|
| `OPENAI_URL` | LLM API base URL (OpenAI-compatible endpoint). |
| `OPENAI_TOKEN` | LLM API key. |
| `OPENAI_MODEL` | LLM model name (default: `gpt-4o-mini`). |
| `EMBEDDING_PROVIDER` | Embedding provider: `openai`, `ollama`, `onnx`, `mock`. |
| `EMBEDDING_MODEL` | Embedding model name. |
| `EMBEDDING_DIMENSIONS` | Embedding vector dimensions. |
| `EMBEDDING_ENDPOINT` | Embedding API base URL (falls back to `OPENAI_URL`). |
| `EMBEDDING_API_KEY` | Embedding API key (falls back to `OPENAI_TOKEN`). |
| `MOCK_EMBEDDING` | Set `true` to use zero-vector mock embeddings (no model download). |
| `RUST_LOG`, `LOG_LEVEL` | `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `cognee_setup_logging()` — see docs/configuration.md (Logging section). |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_*` | Consumed by `cognee_init_otlp()`. |
| `COGNEE_PRODUCT_TELEMETRY_ENABLED` | Explicitly opt in to product analytics. |
| `TELEMETRY_DISABLED`, `ENV` | Higher-priority analytics suppressions. |

All env-var values can also be passed programmatically as JSON to `cg_sdk_new()` or
via `cg_sdk_config_set_str()`, which take precedence over environment variables.

## Initialisation helpers

Three optional, idempotent, argument-less init functions extend the base `cg_init()`:

| Function | Effect |
|---|---|
| `cognee_setup_logging()` | File + stdout logging from `COGNEE_LOG_*`, `LOG_LEVEL`, `RUST_LOG` |
| `cognee_init_otlp()` | OpenTelemetry OTLP export from `COGNEE_TRACING_ENABLED` / `OTEL_*` |
| `cognee_init_telemetry()` | Evaluates fail-closed product analytics (explicit opt-in required) |

None of them are required; the C binding installs no default subscriber so you get no noise
unless you call them.

## Low-level pipeline engine

`cognee.h` exposes the underlying task/pipeline/value/cancellation primitives that the SDK tier
is built on.  These are useful for advanced embedders who need custom pipeline orchestration.
See the engine examples under `examples/example_sync_task.c`, `example_pipeline.c`, etc.

## See also

- Headers: [`include/cognee_sdk.h`](include/cognee_sdk.h), [`include/cognee.h`](include/cognee.h)
- Examples: [`examples/`](examples/)
- Observability: [`../docs/observability/opentelemetry.md`](../docs/observability/opentelemetry.md), [`../docs/observability/send_telemetry.md`](../docs/observability/send_telemetry.md)
- Python bindings: [`../python/README.md`](../python/README.md)
- JS/TS bindings: [`../ts/README.md`](../ts/README.md)
- cognee-rs workspace: [`../README.md`](../README.md)
