# Gap 7 — Bindings auto-init for tracing & telemetry

> Scope: Python (PyO3), JavaScript (Neon), C API. Android falls under the C API
> story plus its own host harness.

## Overview

Today the cognee-rust language bindings expose pipeline primitives but do **not**
configure any `tracing` subscriber, file logging, OTLP export, or product
analytics on load. An embedder importing `cognee_pipeline` from Python or
`require("cognee-neon")` from Node sees zero log output unless they install a
subscriber themselves — and our `tracing::instrument` spans, ring buffer layer
([01-otlp-exporter.md](01-otlp-exporter.md)), and `send_telemetry`
([02-send-telemetry.md](02-send-telemetry.md)) infrastructure stay dormant.

The Python `cognee` package, by contrast, **eagerly configures structlog +
stdlib logging on import** so that any consumer of the package gets
console + rotating-file output immediately. We need a sensible parity story
across all three bindings without forcing the host application to relinquish
its own logging configuration.

## Python's auto-init pattern

`/tmp/cognee-python/cognee/__init__.py:1-16`:

```python
# ruff: noqa: E402
from cognee.version import get_cognee_version
__version__ = get_cognee_version()

import dotenv
dotenv.load_dotenv(override=True)

# NOTE: Log level can be set with the LOG_LEVEL env variable
from cognee.shared.logging_utils import setup_logging
logger = setup_logging()
```

Key takeaways:

1. Logging is initialised **as a side effect of `import cognee`** — no
   `cognee.init()` call needed.
2. `setup_logging()` (`/tmp/cognee-python/cognee/shared/logging_utils.py:311`)
   is idempotent in spirit: it clears existing root-logger handlers, attaches
   structlog console handler + a `PlainFileHandler` (50 MB rotating, 5 backups)
   and sets `_is_structlog_configured = True`. A second call would simply
   rebuild the configuration.
3. It honours `LOG_LEVEL`, `COGNEE_LOG_FILE`, `COGNEE_LOGS_DIR`,
   `COGNEE_LOG_MAX_BYTES`, `COGNEE_LOG_BACKUP_COUNT`, `LOG_FILE_NAME`.
4. It also installs `sys.excepthook`, suppresses noisy libraries (`litellm`,
   `openai._base_client`), and calls `_log_deferred_info()` to emit a single
   "Logging initialized" record with version banners.
5. **Telemetry** (the `send_telemetry` proxy) is **on by default** — it is gated
   only by `TELEMETRY_DISABLED=1` or `ENV in {test, dev}` (see
   [02-send-telemetry.md](02-send-telemetry.md)). There is no separate opt-in
   step at import time.

## Per-binding current state

### Python — PyO3 (`cognee_pipeline`)

| Item | File:line |
|---|---|
| Cargo manifest | `/home/dmytro/dev/cognee/cognee-rust/python/Cargo.toml:1-21` |
| `#[pymodule]` entrypoint | `/home/dmytro/dev/cognee/cognee-rust/python/src/lib.rs:13-25` |
| Python facade `__init__.py` | `/home/dmytro/dev/cognee/cognee-rust/python/cognee_pipeline/__init__.py` |
| Tracing init | **none** |
| `tracing-subscriber` dep | **not declared** |
| `pyo3-log` dep | **not declared** |

The PyO3 module body just registers classes and exception types, then returns.
There is no global subscriber, no log forwarder, no analytics hook.

### JavaScript — Neon (`cognee-neon`)

| Item | File:line |
|---|---|
| Cargo manifest | `/home/dmytro/dev/cognee/cognee-rust/js/cognee-neon/Cargo.toml:1-26` |
| `#[neon::main]` entrypoint | `/home/dmytro/dev/cognee/cognee-rust/js/cognee-neon/src/lib.rs:28-125` |
| TS facade | `/home/dmytro/dev/cognee/cognee-rust/js/src/index.ts:1-12` (re-exports `init`/`initWithThreads`/`shutdown`) |
| Tokio runtime init | `/home/dmytro/dev/cognee/cognee-rust/js/cognee-neon/src/runtime.rs:26-41` |
| Tracing init | **none** |
| `tracing-subscriber` dep | **not declared** |

`runtime::init` builds a Tokio runtime but never installs a subscriber. The TS
`init()` wrapper is documented as a runtime starter, not a logger setup.

### C API (`cognee-capi`)

| Item | File:line |
|---|---|
| Cargo manifest | `/home/dmytro/dev/cognee/cognee-rust/capi/cognee-capi/Cargo.toml:1-22` |
| Module root | `/home/dmytro/dev/cognee/cognee-rust/capi/cognee-capi/src/lib.rs:1-30` |
| `cg_init` (runtime only) | `/home/dmytro/dev/cognee/cognee-rust/capi/cognee-capi/src/runtime.rs:25-33` |
| Tracing init | **none** |
| `tracing-subscriber` dep | **not declared** |

`cg_init` initialises only the global `AsyncRuntime`. There is no
`cg_init_logging` / `cg_init_telemetry`.

## Detailed gap analysis

What every binding lacks today:

1. **No subscriber** — every `tracing::info!`, `error!`, `#[instrument]` span
   in the cognee-rust crates is dropped. Embedders using `cognee_pipeline`
   from Python see no diagnostics for failed pipelines, no LLM call traces,
   nothing.
2. **No file logging** — Python users coming from the upstream `cognee`
   package expect `/tmp/cognee_logs/<timestamp>.log`. The Rust binding writes
   nowhere.
3. **No OTLP / observability layer** — `OTEL_*` env vars
   ([gap-analysis.md §1](gap-analysis.md), [01-otlp-exporter.md](01-otlp-exporter.md))
   are no-ops in bindings even once the OTLP work lands, because nothing wires
   the layer in.
4. **No analytics emission** — `send_telemetry`
   ([02-send-telemetry.md](02-send-telemetry.md)) cannot fire without a runtime
   client, which today no binding constructs.
5. **No host hand-off** — the C API gives the embedder no way to ask for a
   custom log sink (file path, syslog, custom callback). Same for Neon.
6. **Double-emission risk** — when the Python `cognee` SDK calls into
   `cognee_pipeline`, both layers will independently send `send_telemetry`
   events to `https://test.prometh.ai`, doubling counts.

## Proposed design — PyO3

### Logging: bridge `tracing` → Python `logging` via `pyo3-log`

`pyo3-log` ([crates.io](https://crates.io/crates/pyo3-log)) provides a
`tracing::Subscriber`-compatible `log` facade that forwards events into the
host CPython `logging` module. Combined with the
[`tracing-log`](https://crates.io/crates/tracing-log) `LogTracer`, the flow is:

```
tracing::event!  →  tracing_subscriber::Registry  →  tracing_log::LogTracer
                                                  → log::Log::log(...)
                                                  → pyo3_log::Logger
                                                  → Python `logging` module
```

This means the **host application's** `logging.basicConfig` /
`logging.dictConfig` controls level, format, and handlers. A Python user who
runs `logging.getLogger("cognee").setLevel(logging.DEBUG)` then sees Rust
spans at DEBUG without touching `RUST_LOG`.

#### Sketch

`crates/.../python/Cargo.toml`:

```toml
[dependencies]
pyo3-log         = "0.12"
tracing          = { workspace = true }
tracing-subscriber = { workspace = true, features = ["env-filter", "registry"] }
tracing-log      = "0.2"
once_cell        = "1"
```

`crates/.../python/src/lib.rs`:

```rust
use pyo3::prelude::*;
use std::sync::Once;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

static INIT: Once = Once::new();

fn init_telemetry(py: Python<'_>) {
    INIT.call_once(|| {
        // 1. Forward Rust `tracing` events into Python's `logging`.
        //    pyo3-log installs as the global `log` impl;
        //    tracing-log's LogTracer bridges tracing -> log.
        let _ = pyo3_log::try_init();              // fine if already installed
        let _ = tracing_log::LogTracer::init();    // tracing -> log

        // 2. Optional EnvFilter for COGNEE_LOG / RUST_LOG so Python users
        //    can override beyond what their Python `logging` config does.
        let filter = EnvFilter::try_from_env("COGNEE_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));

        // 3. A pure-Registry subscriber (no fmt layer): events leave via
        //    tracing-log → pyo3-log so we don't double-print on stderr.
        let _ = tracing_subscriber::registry()
            .with(filter)
            .try_init();

        // 4. Optional OTLP / span buffer / file logging — gated on
        //    COGNEE_TRACING_ENABLED / OTEL_EXPORTER_OTLP_ENDPOINT
        //    (see 01-otlp-exporter.md, 06-file-logging.md).
        // crate::observability::install_optional_layers();

        // 5. Decide telemetry posture (see "Telemetry defaults" below).
        // crate::analytics::maybe_init();
    });
}

#[pymodule]
fn _native(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    init_telemetry(py);

    m.add_class::<pipeline::PyPipeline>()?;
    // ... existing registrations ...
    error::register(m)?;
    Ok(())
}
```

#### Behaviour when the host has no logging config

`pyo3-log` simply forwards events to the named Python logger; if no handler is
attached, Python defaults apply (a `lastResort` stderr handler at WARNING and
above). This matches expected Python ergonomics — silent at INFO, audible at
WARN/ERROR. If we want INFO by default (matching upstream cognee), embedders
can call `cognee_pipeline.configure_logging(level="INFO")` — a thin Python
helper exposed via the facade that calls `logging.getLogger("cognee").setLevel`.

#### OTLP / file logging

Eager subscriber installation should **only** wire OTLP / file layers when the
governing env var is set:

- `OTEL_EXPORTER_OTLP_ENDPOINT` ⇒ install OTLP layer (Gap 1)
- `COGNEE_LOG_FILE != "false"` and `COGNEE_LOGS_DIR` resolvable ⇒ install
  rotating file layer (Gap 6)
- Otherwise these are skipped — keeps cold-start overhead near zero.

## Proposed design — Neon (JS)

The Node.js ecosystem has no de-facto bridge between `tracing` and JS console
or to a structured logger like `pino`. Two pragmatic options:

### Option A (recommended for MVP) — auto fmt-to-stderr, gated by env

In `#[neon::main]`, install `tracing-subscriber::fmt` writing to stderr,
matching what the CLI binary already does. Make it opt-out via an env var so
embedders that already capture stderr or don't want noise can suppress it.

```rust
fn install_default_subscriber() {
    if std::env::var_os("COGNEE_NEON_SUPPRESS_LOGS").is_some() {
        return;
    }
    let filter = EnvFilter::try_from_env("COGNEE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .try_init();
}

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    install_default_subscriber();
    // ... existing exports ...
}
```

### Option B (follow-up) — explicit JS callback bridge

Expose `cognee.setLogger((level, target, message, fields) => …)` that
registers a JS function held in a `tokio::sync::mpsc` channel; a Rust
`Layer::on_event` enqueues records and a JS thread drains them. Avoids
blocking the V8 thread while letting hosts route into `pino` / Winston.

This requires careful Neon `Channel` work (see Neon docs on `Channel::send`)
and is heavier than Option A. Defer until users ask for it.

### Telemetry / OTLP

Same gating as PyO3 — only install OTLP if `OTEL_EXPORTER_OTLP_ENDPOINT` set;
only install file logging if `COGNEE_LOG_FILE` says so. Wire all of this in
`install_default_subscriber` once Gap 1 / Gap 6 land.

## Proposed design — C API

C embedders need explicit, idempotent init functions because there is no
"module load hook" equivalent (the dlopen path runs no Rust code we control
beyond static `cdylib` ctors, which we deliberately avoid).

### Public API additions

In `capi/cognee-capi/src/observability.rs` (new module) and re-exported via
`lib.rs`:

```c
/**
 * Initialise process-wide tracing.
 *
 *   level  — "trace" | "debug" | "info" | "warn" | "error"
 *            (NULL → uses RUST_LOG / COGNEE_LOG, fallback "info,ort=warn").
 *   file   — UTF-8 path. NULL → stderr only.
 *
 * Idempotent: subsequent calls return CG_ERROR_ALREADY_INITIALIZED but do
 * not change state. Safe to call from multiple threads.
 */
CgErrorCode cg_init_logging(const char *level, const char *file);

/**
 * Initialise OTLP/OpenTelemetry export. Reads env vars
 *   OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_EXPORTER_OTLP_HEADERS,
 *   OTEL_SERVICE_NAME (defaulting to "cognee").
 * Returns CG_ERROR_INVALID_CONFIG if no endpoint configured.
 */
CgErrorCode cg_init_otlp(void);

/**
 * Initialise product analytics (`send_telemetry`) — see Gap 2.
 * Honours TELEMETRY_DISABLED, ENV={test,dev}.
 *   anon_id_path        — NULL → default ".anon_id" in cwd.
 *   persistent_id_path  — NULL → default "~/.cognee/.persistent_id".
 */
CgErrorCode cg_init_telemetry(const char *anon_id_path,
                              const char *persistent_id_path);
```

### Default behaviour when not called

- No `tracing` subscriber ⇒ all events are dropped. This is the current
  behaviour and is the right C-API default: do not surprise embedders with
  stderr noise.
- Caveat: we should at minimum install a **panic hook** on `cg_init` that
  writes the panic message to stderr, so that a Rust panic crossing the FFI
  is debuggable. (Compatible with `cg_init_logging` — both can coexist.)

### Implementation notes

- Use `OnceLock<()>` to enforce single-init for each function, returning a
  new error code `CG_ERROR_ALREADY_INITIALIZED` (see
  `capi/cognee-capi/src/error.rs`).
- File output uses `tracing-appender`'s `RollingFileAppender` with the same
  rotation knobs as Gap 6 (`COGNEE_LOG_MAX_BYTES`, etc.).
- All three init functions must be callable in any order; they share an
  internal `tracing_subscriber::Registry` built lazily.

## Telemetry (analytics) defaults per binding

`send_telemetry` (Gap 2) hits `https://test.prometh.ai`. The risk is
**double-emission** when the Python `cognee` SDK uses `cognee_pipeline` under
the hood — both layers will fire `Pipeline Run Started`, etc.

### Proposed policy

| Binding | Default | Suppression mechanism |
|---|---|---|
| Python (PyO3) | **Off** unless `COGNEE_RUST_TELEMETRY=1` | — |
| C API | **Off** until `cg_init_telemetry()` called | explicit init |
| Neon (JS) | **On** unless `TELEMETRY_DISABLED=1` or `ENV in {test,dev}` (matches Python SDK semantics) | env var |

Rationale:

- **PyO3 default off** because the Python `cognee` package is the canonical
  identity owner — it manages `.anon_id`, `~/.cognee/.persistent_id`, the
  PBKDF2 key tracking ID — and already calls `send_telemetry` from
  `run_tasks_with_telemetry.py`. Re-emitting from Rust would inflate counts
  and could disagree on identifiers. Hosts that embed `cognee_pipeline`
  *without* the Python SDK (i.e. as a standalone PyO3 wheel) opt in via
  `COGNEE_RUST_TELEMETRY=1`.
- **C API off** by default because there is no convention; the host opts in.
- **Neon on** by default because there is no upstream JS cognee SDK doing
  redundant emission; the binding is the canonical sender.

### Detection of "running inside Python cognee SDK"

The Python `cognee/__init__.py` can set a sentinel env var before importing
`cognee_pipeline`:

```python
os.environ.setdefault("COGNEE_HOST_SDK", "python")
```

The Rust binding's analytics initialiser checks `COGNEE_HOST_SDK` and
suppresses emission when set, regardless of `COGNEE_RUST_TELEMETRY`. This
covers the case where a user *did* set the opt-in but is also using the
Python SDK. Documented as: "if you are using `cognee` (Python), do not set
`COGNEE_RUST_TELEMETRY`."

## Action items

### Python (PyO3)

1. Add `pyo3-log = "0.12"` and `tracing-log = "0.2"` to
   `python/Cargo.toml`.
2. Add a `init_telemetry(py)` helper in `python/src/lib.rs`, called once
   from the `#[pymodule]` body using `Once`.
3. Wire env-gated OTLP layer (Gap 1) and file layer (Gap 6) into the same
   helper.
4. Expose a `configure_logging(level: str = "INFO", file: str | None = None)`
   Python helper (in `cognee_pipeline/__init__.py`) that maps to a Rust fn so
   embedders can override after import.
5. Default product analytics to **off** in Python; opt-in via
   `COGNEE_RUST_TELEMETRY=1`.
6. Suppress analytics when `COGNEE_HOST_SDK=python` is present.
7. Document interaction with `cognee.shared.logging_utils.setup_logging` in
   `python/README.md` (host's `logging` config governs).

### Neon (JS)

1. Add `tracing-subscriber` (with `env-filter`, `fmt`) to
   `js/cognee-neon/Cargo.toml`.
2. Install a stderr fmt subscriber in `#[neon::main]` (Option A).
3. Honour `COGNEE_NEON_SUPPRESS_LOGS` env var to disable.
4. Add an `init_telemetry()` JS export (mirrors `init()`) that wires the
   OTLP + file + analytics layers under the same env-gating.
5. Default product analytics to **on** with the standard
   `TELEMETRY_DISABLED` / `ENV` opt-out.
6. (Stretch) Option B JS callback bridge.

### C API

1. New module `capi/cognee-capi/src/observability.rs` exporting
   `cg_init_logging`, `cg_init_otlp`, `cg_init_telemetry`.
2. Add `CG_ERROR_ALREADY_INITIALIZED` to `error::CgErrorCode`.
3. Install panic hook in `cg_init` so panics across FFI go to stderr even
   without `cg_init_logging`.
4. Update generated `cognee.h` (cbindgen) — verify `cbindgen.toml` exports
   the new symbols.
5. Document calling order in `capi/README.md`: `cg_init` →
   (optional) `cg_init_logging` → (optional) `cg_init_otlp` →
   (optional) `cg_init_telemetry`.

### Cross-cutting

1. Honour `COGNEE_HOST_SDK` env var across all three bindings to suppress
   double-emission.
2. Add an `e2e-cross-sdk` test that imports `cognee_pipeline` from the
   Python `cognee` SDK environment and asserts only **one** `send_telemetry`
   POST is observed (mock the proxy URL).
3. Bump `gap-analysis.md` §6 status when each binding lands (left for a
   separate PR — do not edit it here).

## Open questions

1. **Default analytics in PyO3 standalone use** — should we go further and
   leave it off even for non-SDK users, requiring opt-in always? Python's
   default is on; but the Python SDK is the source of identity. Discuss
   with product before flipping default.
2. **Should `COGNEE_RUST_TELEMETRY` apply globally, or per-binding?** A
   single env var is simpler. Currently treated as global.
3. **JS subscriber: stderr vs `console.error`?** stderr is what most Node
   logging libs already capture, but some hosts redirect only `console.*`.
   Default to stderr; revisit if users complain.
4. **C API: should we ship a convenience `cg_init_all(level, otlp_endpoint,
   telemetry_disabled)`?** Reduces boilerplate but couples concerns. Lean
   no for now.
5. **`pyo3-log` MSRV / PyO3 0.23 compat** — verify the chosen version pairs
   with our `pyo3 = "0.23"` pin (see `python/Cargo.toml:15`).
6. **OTEL service name in bindings** — should we override `OTEL_SERVICE_NAME`
   to e.g. `"cognee.python-binding"` so dashboards distinguish embedded use
   from the HTTP server? Probably yes; document the resource attribute.

## Testing strategy

### PyO3

- **Unit test (Python)**: in `python/tests/test_logging_bridge.py`:
  ```python
  import logging, cognee_pipeline
  cognee_pipeline.configure_logging(level="DEBUG")
  records = []
  logging.getLogger("cognee").addHandler(
      logging.Handler(level=0, emit=records.append))
  cognee_pipeline.Pipeline().run(...)  # triggers Rust span
  assert any(r.name.startswith("cognee") for r in records)
  ```
- **Idempotency**: call `configure_logging` twice; assert no duplicate
  handlers and no panic.
- **Analytics gating**: with `COGNEE_HOST_SDK=python` set, run a pipeline,
  assert no HTTP POST to `https://test.prometh.ai` (use `pytest-httpserver`
  to point the proxy URL at a local mock).

### Neon

- **Smoke test** in `js/__tests__/logging.test.ts`: spawn a child Node
  process with `COGNEE_LOG=debug`, run a pipeline, assert stderr contains
  `tracing` lines.
- **Suppression**: with `COGNEE_NEON_SUPPRESS_LOGS=1`, assert stderr is
  empty.

### C API

- **Idempotency** in `capi/scripts/check.sh` smoke binary: call
  `cg_init_logging("info", "/tmp/cg.log")` twice; second returns
  `CG_ERROR_ALREADY_INITIALIZED`; log file exists.
- **Default silence**: build a tiny C program that calls only `cg_init`
  and runs a pipeline; assert stderr is empty.
- **OTLP wiring**: with `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`
  pointing at a mock collector, call `cg_init_otlp()`, run a pipeline,
  assert spans received.

### Cross-SDK

- New test in `e2e-cross-sdk/test_telemetry_no_double_emit.py`: install both
  Python `cognee` and the local `cognee_pipeline` wheel; mock
  `https://test.prometh.ai`; run `cognee.add(...)`; assert exactly one POST
  per logical event, all carrying Python-SDK identifiers.

## References

- Python entry point: `/tmp/cognee-python/cognee/__init__.py:1-16`
- Python logging setup: `/tmp/cognee-python/cognee/shared/logging_utils.py:311-568`
- PyO3 module init: `/home/dmytro/dev/cognee/cognee-rust/python/src/lib.rs:13-25`
- Neon module init: `/home/dmytro/dev/cognee/cognee-rust/js/cognee-neon/src/lib.rs:28-125`
- C API module root: `/home/dmytro/dev/cognee/cognee-rust/capi/cognee-capi/src/lib.rs:1-30`
- C API runtime init: `/home/dmytro/dev/cognee/cognee-rust/capi/cognee-capi/src/runtime.rs:25-57`
- Existing CLI subscriber pattern: `crates/cli/src/main.rs:50-58`
- Existing HTTP-server subscriber pattern: `crates/http-server/src/main.rs:100-118`
- Related: [01-otlp-exporter.md](01-otlp-exporter.md),
  [02-send-telemetry.md](02-send-telemetry.md),
  [06-file-logging.md](06-file-logging.md)
- External: [`pyo3-log` docs](https://docs.rs/pyo3-log),
  [`tracing-log` docs](https://docs.rs/tracing-log),
  [Neon `Channel` API](https://docs.rs/neon/latest/neon/event/struct.Channel.html)
