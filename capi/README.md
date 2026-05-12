# cognee-capi

C bindings for the [cognee-rust](https://github.com/topoteretes/cognee-rust)
pipeline engine. Builds as a static + shared library plus a public
header (`include/cognee.h`).

## Build

```bash
cd capi
mkdir -p build
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
```

The build emits `libcognee_capi.{a,so,dylib}` and the C examples under
`build/examples/`.

## Quick start

```c
#include <cognee.h>

int main(void) {
    if (cg_init() != CG_OK) return 1;

    /* ... build pipeline, run tasks ... */

    cg_shutdown();
    return 0;
}
```

## Initialisation

cognee's Rust core uses `tracing` for structured diagnostics and
optionally exports spans via OpenTelemetry (OTLP). Unlike the
Python/Node bindings, the C binding installs **no default tracing
subscriber** — embedders must opt in explicitly via
`cognee_setup_logging()`. This avoids surprising C hosts with stderr
noise.

What `cg_init()` does install is a one-shot **panic hook**
(`std::panic::set_hook`) that writes
`[cognee-capi panic] <message> at <file:line:col>` to stderr when a
Rust panic crosses the FFI. This makes panics diagnosable even when
no subscriber is installed. Replace it via `std::panic::set_hook`
from your own Rust glue if you need chained or routed handling.

### Default subscribers and the suppression env var

| Binding | Default subscriber on import |
|---|---|
| Python (`cognee_pipeline`) | `pyo3-log` bridge into Python's `logging` module |
| Node.js (`cognee-neon`) | `tracing-subscriber::fmt` to stderr |
| **C (`cognee-capi`)** | **None — install via `cognee_setup_logging()`** |

For symmetry with the other bindings, `COGNEE_BINDING_SUPPRESS_LOGS=1`
is honoured by `cognee_setup_logging()` itself (and by the other init
calls) — but since no default subscriber exists, the variable has no
practical effect on C unless you want belt-and-braces parity scripts.

### Setup functions

Three idempotent init entrypoints are exposed. Each is argument-less
and reads its configuration from environment variables (matching the
CLI binary's behaviour). Calling order does not matter; calling any
of them more than once is a no-op.

| Function | Effect | Returns |
|---|---|---|
| `cognee_setup_logging()` | Initialises cognee's logging subsystem from env vars (`COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`). Adds the rotating file appender when configured. | `0` on success / idempotent re-call, non-zero on error. |
| `cognee_init_otlp()` | Initialises OpenTelemetry export from env vars (`COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and other `OTEL_*`). Defaults `service.name` to `cognee.capi-binding` when unset. No-config = no-op. | `0` = success / no-op, `1` = lock poison, `2` = init failed. |
| `cognee_init_telemetry()` | Arms product-analytics emission (`https://test.prometh.ai`) subject to the C policy below. | `0` = armed, `1` = not armed (policy suppressed), `2` = lock poison. |

Example with everything on:

```c
#include <cognee.h>
#include <stdio.h>

int main(void) {
    if (cg_init() != CG_OK) return 1;

    if (cognee_setup_logging() != 0) return 2;          /* logging */
    if (cognee_init_otlp() != 0)     return 3;          /* OTLP    */
    int armed = cognee_init_telemetry();                /* analytics */
    fprintf(stderr, "analytics armed=%d\n", armed == 0);

    /* ... run pipelines ... */

    cg_shutdown();
    return 0;
}
```

### Analytics defaults

For the C binding, analytics emission is **explicit-only** — nothing
is sent unless the embedder calls `cognee_init_telemetry()`. Even
then, the same suppression rules as the Node.js binding apply:

| Condition | Behaviour |
|---|---|
| No call to `cognee_init_telemetry()` | Not armed. |
| `cognee_init_telemetry()` with no suppression vars | Armed. Returns `0`. |
| `TELEMETRY_DISABLED=1` | Not armed. Returns `1`. |
| `ENV=test` or `ENV=dev` | Not armed. Returns `1`. |
| `COGNEE_HOST_SDK=<any non-empty>` | Not armed. Returns `1`. |

### v1 limitation: reload-capable subscriber

The C binding builds the OTLP `Layer` via
`cognee_observability::init_telemetry`, but does not compose it into a
`tracing::Subscriber`. The OpenTelemetry SDK's `TracerProvider` still
works, so spans emitted via the SDK directly reach the collector — but
events emitted by Rust `tracing::*` calls inside cognee's crates are
not currently exported through OTLP from the C binding. A
reload-capable C subscriber is a documented follow-up; see the gap-07
closure summary for details.

## Environment variables

| Variable | Purpose |
|---|---|
| `COGNEE_BINDING_SUPPRESS_LOGS` | Symmetry sentinel — honoured by setup calls; the C binding ships no default subscriber so it has no practical effect unless you use the variable for parity scripts. |
| `COGNEE_HOST_SDK` | Suppress binding-armed analytics emission when the host is an embedding SDK (decision 10). |
| `TELEMETRY_DISABLED`, `ENV` | Standard analytics opt-outs honoured by `cognee_init_telemetry()`. |
| `RUST_LOG`, `LOG_LEVEL` | Standard `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `cognee_setup_logging()` — see [gap 06](../docs/telemetry/06-file-logging-rotation.md). |
| `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and other `OTEL_*` vars | Consumed by `cognee_init_otlp()`. |

## References

- Public header: [`include/cognee.h`](include/cognee.h)
- Design doc: [docs/telemetry/07-bindings-auto-init.md](../docs/telemetry/07-bindings-auto-init.md)
- Gap-analysis: [docs/telemetry/gap-analysis.md §6](../docs/telemetry/gap-analysis.md)
