# cognee-neon

Node.js bindings for the [cognee-rust](https://github.com/topoteretes/cognee-rust)
pipeline engine, built with [Neon](https://neon-bindings.com/).

## Installation

```bash
npm install cognee-neon
```

## Quick start

```ts
import { init, Pipeline } from "cognee-neon";

init();
const pipeline = new Pipeline();
// ... configure tasks, run, etc.
```

## Initialisation

cognee's Rust core uses `tracing` for structured diagnostics and
optionally exports spans via OpenTelemetry (OTLP). When the Neon
addon is loaded, a minimal default subscriber is installed so events
are never silently dropped: a `tracing-subscriber::fmt` layer writing
to **stderr** with `EnvFilter` defaults of `info,ort=warn` (overridable
via `RUST_LOG` / `LOG_LEVEL`).

### Opt-out

Set `COGNEE_BINDING_SUPPRESS_LOGS=1` **before** `require`ing the
module to skip the default subscriber. The host then owns subscriber
setup.

```bash
COGNEE_BINDING_SUPPRESS_LOGS=1 node my_app.js
```

### Optional upgrades

Three idempotent setup functions are exported from `cognee-neon`.
Each one composes additional layers on top of the default
subscriber. Calling order does not matter; calling any of them more
than once is a no-op.

| Call | Effect | Idempotent |
|---|---|---|
| `setupLogging()` | Adds the rotating file appender (default `~/.cognee/logs/<ts>.log`, daily rotation, configurable via `COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`). | Yes |
| `setupTelemetry()` | Composes an OTLP exporter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set; reads all standard `OTEL_*` env vars. Defaults `service.name` to `cognee.node-binding` when unset (the user's explicit value always wins). Returns `void`. | Yes |
| `setupTelemetryAnalytics()` | Arms product-analytics emission (`https://test.prometh.ai`) per the Node.js policy below. Returns `true` if armed by this call (or a prior call), `false` if the policy suppressed emission. | Yes |

Example with everything on:

```ts
import {
  init,
  setupLogging,
  setupTelemetry,
  setupTelemetryAnalytics,
} from "cognee-neon";

init();
setupLogging();            // file logging
setupTelemetry();          // OTLP export
const armed = setupTelemetryAnalytics(); // analytics
console.log(`analytics armed: ${armed}`);
```

### Analytics defaults

For the Node.js binding, analytics emission is **ON by default** —
Neon is the canonical sender of `send_telemetry` events in the JS
ecosystem (there is no upstream JS cognee SDK to defer to).

| Condition | Behaviour |
|---|---|
| No suppression vars set | Armed. Returns `true`. |
| `TELEMETRY_DISABLED=1` | Not armed. Returns `false`. |
| `ENV=test` or `ENV=dev` | Not armed. Returns `false`. |
| `COGNEE_HOST_SDK=<any non-empty>` | Not armed. Returns `false`. |

## Environment variables

| Variable | Purpose |
|---|---|
| `COGNEE_BINDING_SUPPRESS_LOGS` | Suppress the auto-installed stderr fmt subscriber. |
| `COGNEE_HOST_SDK` | Suppress binding-armed analytics emission when the host is an embedding SDK (decision 10). |
| `TELEMETRY_DISABLED`, `ENV` | Standard analytics opt-outs honoured by `setupTelemetryAnalytics()`. |
| `RUST_LOG`, `LOG_LEVEL` | Standard `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `setupLogging()` — see [gap 06](../docs/telemetry/06-file-logging-rotation.md). |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and other `OTEL_*` vars | Consumed by `setupTelemetry()`. |

## References

- Design doc: [docs/telemetry/07-bindings-auto-init.md](../docs/telemetry/07-bindings-auto-init.md)
- Gap-analysis: [docs/telemetry/gap-analysis.md §6](../docs/telemetry/gap-analysis.md)
