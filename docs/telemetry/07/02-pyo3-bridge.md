# Task 07-02 — PyO3 `pyo3-log` bridge for default Rust→Python event routing

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: [Task 07-01 — Workspace deps](01-workspace-deps.md).
**Blocks**:
- [Task 07-05 — Binding OTLP setup](05-binding-otlp-setup.md) (the OTLP layer composes on top of whatever subscriber is already installed; the default install path lands here).
- [Task 07-07 — Tests](07-tests.md) (`test_pyo3_log_bridge.py` exercises this flow).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 1 (hybrid auto-init), 5 (`pyo3-log` is the canonical Python event sink), 12 (idempotent singleton pattern).

---

## 1. Goal

Install a tracing → log → Python `logging` bridge automatically when
the `_native` PyO3 extension module is loaded. Concretely:

1. Inside `#[pymodule] fn _native`, before any class registration,
   call a new `init_default_subscriber(py)` helper.
2. The helper short-circuits when:
   - `COGNEE_BINDING_SUPPRESS_LOGS` is set to any non-empty value, OR
   - a global tracing subscriber is already installed (best-effort
     detection via `tracing_subscriber::registry().try_init()` returning
     `Err`).
3. Otherwise it builds a `tracing_subscriber::Registry` with:
   - an `EnvFilter` reading `RUST_LOG` (falling back to
     `cognee_logging::default_filter()` so the bridge inherits the
     same library-noise suppression as the binaries), AND
   - the global `log` facade configured to forward into Python via
     `pyo3_log::Logger::new(py, pyo3_log::Caching::LoggersAndLevels)?`,
     bridged into `tracing` by `tracing_log::LogTracer::init()`.
4. Calls a module-private `init_default_subscriber` only once via
   `std::sync::Once`. A second `import cognee_pipeline._native`
   inside the same process (e.g. via `importlib.reload`) becomes a
   no-op.

Behaviourally: a Python host that does
`logging.getLogger("cognee").setLevel(logging.DEBUG)` sees Rust
`tracing::debug!` events that pass the env-filter level.
`setup_logging()` (gap 06) remains separately callable and adds the
rotating file appender on top without disturbing the bridge.

## 2. Rationale

- Decision 1 chose the hybrid approach: a cheap default subscriber
  installed on import so events are never dropped, plus
  `setup_logging()` for the heavyweight file/format machinery.
- Decision 5 picks `pyo3-log` as the Python-side sink because the
  upstream `cognee` Python package configures structlog + stdlib
  `logging` and expects `logging.getLogger("cognee")` to govern
  verbosity. Routing through Python `logging` honours that mental
  model.
- The flow is `tracing::event!` → `tracing_log::LogTracer` →
  `log::Log::log` → `pyo3_log::Logger` → Python `logging`. Note that
  `tracing_log::LogTracer::init` installs the `log` facade as a
  `tracing::Subscriber`; we additionally set
  `log::set_boxed_logger(Box::new(pyo3_log::Logger::new(py, …)))` so
  the `log` facade routes into Python.

  **Important:** these two `log`-facade installs conflict. The
  correct order, per `pyo3-log` docs, is to install
  `pyo3_log::Logger` as the global `log::Log` impl **first**, then
  use `tracing-log` only to bridge tracing-emitted events into
  `log::Record`s. We do not call `LogTracer::init()` — instead, the
  `tracing_log::AsLog` adapter is used implicitly by
  `tracing-subscriber` when the `Registry` has no other subscribers
  consuming events. The implementor must validate this routing path
  end-to-end via the test in 07-07.

## 3. Pre-conditions

- Task 07-01 committed; `pyo3-log` and `tracing-log` are direct deps
  of `cognee-python`.
- [`python/src/lib.rs`](../../../python/src/lib.rs) currently
  registers classes and `setup_logging` from gap 06. It does not yet
  install any subscriber on import.
- `cognee_logging::default_filter()` is `pub` (it is — see
  [`crates/logging/src/init.rs`](../../../crates/logging/src/init.rs)
  decision-6 default filter export).

## 4. Step-by-step

### 4.1 Create `python/src/default_subscriber.rs`

New file. Public API:

```rust
//! Default `tracing` subscriber for the PyO3 binding (gap 07).
//!
//! Installed automatically on first import of the `_native`
//! extension module. Routes Rust `tracing` events into Python's
//! standard `logging` module via `pyo3-log`. Hosts that already
//! configured their own subscriber, or that set
//! `COGNEE_BINDING_SUPPRESS_LOGS=1`, get a no-op.
//!
//! This is the minimal "events are never silently dropped" install
//! mandated by gap-07 decision 1. `setup_logging()` (gap 06) and
//! `setup_telemetry()` (gap 07 task 05) continue to layer on top
//! via `tracing_subscriber::Registry::try_init` semantics: only the
//! first init installs; later calls are observed via the singleton
//! guards on the Python side.

use std::sync::Once;

use pyo3::prelude::*;
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

static INIT: Once = Once::new();

/// Install the default bridge subscriber. Idempotent.
///
/// Honours `COGNEE_BINDING_SUPPRESS_LOGS=<any non-empty>` as opt-out.
pub(crate) fn install(py: Python<'_>) {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        // (1) Install pyo3-log as the global `log::Log` impl.
        //     `pyo3_log::try_init` is itself idempotent — if another
        //     log impl is already installed, it returns Err which we
        //     ignore (the host owns logging in that case).
        let _ = pyo3_log::try_init();

        // (2) Build EnvFilter: RUST_LOG > default_filter().
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cognee_logging::default_filter()));

        // (3) Compose a Registry with only the env filter. Events
        //     pass through the global `log::Log` impl (pyo3-log)
        //     via tracing-log's automatic AsLog conversion.
        //     Failure is soft: a host that already installed a
        //     subscriber wins.
        let _ = Registry::default()
            .with(filter)
            .with(tracing_log_layer())
            .try_init();
    });
    // `py` is required to satisfy pyo3_log::Logger's PyHandle
    // constraints inside pyo3_log::try_init in some versions; use
    // it here to keep the function signature future-proof even if
    // the current pyo3-log line doesn't require it.
    let _ = py;
}

/// Returns a `tracing_subscriber::Layer` that forwards every event
/// into the global `log` facade. Combined with `pyo3_log` as the
/// global `log::Log` impl, this routes `tracing::*` calls into
/// Python's `logging` module.
fn tracing_log_layer() -> impl Layer<Registry> + Send + Sync + 'static {
    // `tracing_log::LogTracer::init()` would install the opposite
    // direction (log → tracing); we want tracing → log. The
    // standard way is the `tracing_log::AsLog` adapter, which is
    // built into `tracing_subscriber::fmt::layer` only when JSON
    // is off. The cleanest standalone implementation is a thin
    // `Layer` that calls `log::logger().log(&record)`.
    //
    // pyo3-log >= 0.3 ships a helper for this; see the
    // implementor's note below to pick the right symbol for the
    // installed pyo3-log line.
    tracing_log_compat::layer()
}

/// Wrapper module isolating the pyo3-log version-specific call.
/// Implementor: replace the body with the actual pyo3-log API call
/// after locking the version in task 07-01.
mod tracing_log_compat {
    use tracing_subscriber::{Layer, Registry};

    pub(super) fn layer() -> impl Layer<Registry> + Send + Sync + 'static {
        // For pyo3-log >= 0.4: pyo3_log::Logger is itself a
        // `log::Log`, and tracing-log bridges via the `log` facade
        // installed by `pyo3_log::try_init()`. No tracing Layer
        // needed — the default Registry already forwards
        // unhandled events into the `log` facade when the
        // `tracing-log` cargo feature is enabled on the
        // `tracing` crate.
        //
        // If the `tracing/log` feature is NOT enabled (verify via
        // `cargo tree -e features -p tracing | head`), explicitly
        // add an event-forwarding layer here using `log::logger()`
        // inside `Layer::on_event`.
        //
        // The implementor must verify the tracing build features
        // at task execution time and replace this stub.
        tracing_subscriber::layer::Identity::new()
    }
}
```

### 4.2 Wire into `#[pymodule]`

Edit [`python/src/lib.rs`](../../../python/src/lib.rs):

```rust
mod default_subscriber;
```

Inside `fn _native(m: &Bound<'_, PyModule>) -> PyResult<()>`, as the
**first** statement:

```rust
default_subscriber::install(m.py());
```

The `m.py()` GIL handle is borrowed from the module bound; passing
it to `install` keeps the function safe under future `pyo3-log`
versions that require a `Python<'_>` for logger construction.

### 4.3 Verify tracing→log feature is enabled

Run:

```bash
cargo tree -e features -p tracing | grep -E "log$|tracing-log" | head
```

If `tracing` is not built with the `log` feature, events emitted via
`tracing::event!` will NOT reach the global `log` facade. In that
case, the implementor must add a small `Layer` that explicitly
forwards events:

```rust
struct LogForwarder;
impl<S> Layer<S> for LogForwarder
where S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        // Build a log::Record from the tracing event and call
        // log::logger().log(&record).
        // See `tracing_log::format_trace` for a complete example.
    }
}
```

This is the path the test in 07-07 ultimately verifies — if events
don't reach Python `logging`, the bridge has not landed correctly.

### 4.4 Re-export the opt-out env var name in `cognee_pipeline/__init__.py`

So Python users have one obvious place to discover the env vars.
Edit
[`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py)
to add a module-level constant:

```python
COGNEE_BINDING_SUPPRESS_LOGS = "COGNEE_BINDING_SUPPRESS_LOGS"
"""Env-var name that suppresses the auto-installed tracing bridge.

Set this to any non-empty value *before* importing
``cognee_pipeline`` if the host application already owns its
``logging``/``tracing`` configuration."""
```

(No `__all__` change — it's a documentation re-export, not a public
API.)

## 5. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Python wheel builds.
cd python && maturin develop --release && cd -

# 3. Bridge smoke test (manual; full test lands in 07-07).
python - <<'PY'
import logging, os
logging.basicConfig(level=logging.DEBUG)
import cognee_pipeline  # triggers default_subscriber::install
# Trigger a Rust tracing event:
p = cognee_pipeline.Pipeline()
# (Pipeline construction emits one tracing::info! on the
# cognee_core::pipeline target — verify it lands in stderr at INFO.)
PY

# 4. Suppression works.
COGNEE_BINDING_SUPPRESS_LOGS=1 python - <<'PY'
import cognee_pipeline
# No stderr output expected.
PY

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`python/src/lib.rs`](../../../python/src/lib.rs) — module
  declaration + one-line `install(m.py())` call.
- `python/src/default_subscriber.rs` — NEW.
- [`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py) —
  docs-only `COGNEE_BINDING_SUPPRESS_LOGS` constant.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `tracing` workspace pin lacks the `log` feature, so events never reach `log::Log` | Medium | §4.3 validation step. Add the explicit forwarder Layer if so. |
| `pyo3_log::try_init` returns `Err` because another `log` impl is already installed (e.g. host called `logging.basicConfig` *with* a `pyo3_log`-incompatible adapter) | Low | We `ignore` the result; the host's prior setup wins. Document in 07-08 that hosts who install their own `log` impl should call `setup_logging()` instead. |
| GIL re-entrancy under heavy Rust→Python event forwarding (Python `logging.Handler.handle` re-enters the GIL) | Low — pyo3-log uses `LoggersAndLevels` caching to amortize GIL acquisition. | Document; if a user reports contention, fall back to `Caching::Nothing`. |
| Calling `install` from `_native` import races a Python-side `logging.basicConfig` set up by a competing module | Low — both are process-global; whoever runs first wins. | `Once` makes our install idempotent; pyo3-log's `try_init` makes the `log` global impl install idempotent too. |
| `tracing_log_compat::layer()` stub is wrong for the pinned `pyo3-log` version | Medium — that whole module is a placeholder pending implementor verification. | Sub-agent A flags this as a `needs-decision` if the implementor cannot confirm the routing path; sub-agent B runs the test in 07-07 to validate. |

## 8. Out of scope

- File logging, rotation, custom plain formatter — those live in
  `setup_logging()` (gap 06 task 08). The bridge installed here is
  the *default* subscriber; `setup_logging` is the *upgrade*.
- Mapping Rust span hierarchy into Python logger names. `pyo3-log`
  preserves `target` as the logger name; nested-span context is not
  forwarded. Document.
- Hot-reload of filter levels at runtime. The bridge reads
  `RUST_LOG` once at install time. Hosts that want per-logger level
  changes use Python's standard `logging.Logger.setLevel`.
- Removing `setup_logging()`. Both subscribers coexist.
