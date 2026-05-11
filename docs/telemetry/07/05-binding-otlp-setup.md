# Task 07-05 — Per-binding OTLP setup entrypoint

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**:
- [Task 07-01 — Workspace deps](01-workspace-deps.md) (binding crates depend on `cognee-observability` with the `telemetry` feature).
- [Task 07-02 — PyO3 bridge](02-pyo3-bridge.md) (OTLP layer composes on top of the default subscriber).
- [Task 07-03 — Neon default subscriber](03-neon-default-subscriber.md) (same).

**Blocks**:
- [Task 07-07 — Tests](07-tests.md) (`test_setup_telemetry_idempotent.py`, `setup_telemetry.test.ts`).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 2 (OTLP gets its own entrypoint per binding), 3 (`telemetry` feature on by default), 8 (binding-specific `OTEL_SERVICE_NAME` defaults), 12 (idempotent singleton pattern).

---

## 1. Goal

Add a per-binding OTLP setup function that the host calls when they
want OTEL export. Three entrypoints, all argument-less:

| Binding | Crate | Symbol | Signature |
|---|---|---|---|
| Python | [`python/`](../../../python/) | `setup_telemetry` | `() -> None` (registered on the `_native` module) |
| JS / Node | [`js/cognee-neon/`](../../../js/cognee-neon/) | `setupTelemetry` | `() -> JsUndefined` |
| C | [`capi/cognee-capi/`](../../../capi/cognee-capi/) | `cognee_init_otlp` | `extern "C" fn() -> c_int` (0 / non-zero) |

Each entrypoint:

1. Checks `cognee_observability::is_tracing_enabled(&settings)`
   ([`crates/observability/src/init.rs:38`](../../../crates/observability/src/init.rs#L38)).
   If `false`, returns success without installing anything (idle
   case: no `OTEL_EXPORTER_OTLP_ENDPOINT`, no `COGNEE_TRACING_ENABLED=true`).
2. Applies the binding-specific `OTEL_SERVICE_NAME` default
   (decision 8): if `OTEL_SERVICE_NAME` is unset/empty, set it to
   `cognee.python-binding` / `cognee.node-binding` /
   `cognee.capi-binding` via `std::env::set_var` **before**
   constructing `EnvSettingsView`. This mutates process env — wrap
   in `unsafe { }` per Rust 2024 rules.
3. Calls
   `cognee_observability::init_telemetry::<tracing_subscriber::Registry>(&settings)`
   to obtain `(BoxedTelemetryLayer<Registry>, TelemetryGuard)`.
4. Adds the layer to the global subscriber. This is the **only**
   structurally subtle bit: the default subscriber from 07-02/07-03
   has already called `Registry::try_init`, so we cannot install a
   second `Registry`. The OTEL layer must be added via
   `tracing_subscriber::reload` — see §4.4 below.
5. Stashes `TelemetryGuard` in a binding-local
   `static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>>`.
6. Idempotent: subsequent calls are no-ops.

## 2. Rationale

- Decision 2 keeps OTLP setup separate from `setup_logging` so each
  concern has its own seam, matching the CLI/server pattern.
- Decision 8 default service-name avoids ambiguity in dashboards
  when multiple SDKs share a collector. Service name comes from the
  OTEL `Resource`; setting via env is the cleanest single-line
  change (vs. building a custom `SettingsView` impl).
- Decision 12 enforces idempotence via the same `OnceLock<Mutex<Option<_>>>`
  pattern that gap 06 task 08 used for `setup_logging`.
- The `tracing-subscriber::reload` requirement (§4.4) is the only
  novel constraint: composing layers after `try_init` requires the
  initial Registry to have installed a `reload::Layer` placeholder.

## 3. Pre-conditions

- Tasks 07-01, 07-02, 07-03 committed.
- `cognee_observability::init_telemetry`,
  `cognee_observability::EnvSettingsView`,
  `cognee_observability::is_tracing_enabled`,
  `cognee_observability::BoxedTelemetryLayer`, and
  `cognee_observability::TelemetryGuard` are public — verified at
  [`crates/observability/src/lib.rs:96-99`](../../../crates/observability/src/lib.rs#L96-L99).
- Tasks 07-02 and 07-03 installed `Registry` subscribers via
  `try_init`. **This task must update those tasks' subscribers to
  install a `reload::Layer` placeholder for the OTEL layer.**
  See §4.4 — this is a coordinated change.

## 4. Step-by-step

### 4.1 Add reload-capable OTEL slot to PyO3's default subscriber

Revisit `python/src/default_subscriber.rs` from 07-02. Change the
subscriber install path to reserve a reload-capable slot for the
OTEL layer:

```rust
use tracing_subscriber::reload;

// New: process-global handle for adding/removing the OTEL layer
// at runtime.
pub(crate) static OTEL_RELOAD_HANDLE: std::sync::OnceLock<
    reload::Handle<
        Option<cognee_observability::BoxedTelemetryLayer<tracing_subscriber::Registry>>,
        tracing_subscriber::Registry,
    >,
> = std::sync::OnceLock::new();

pub(crate) fn install(py: Python<'_>) {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        let _ = pyo3_log::try_init();

        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cognee_logging::default_filter()));

        // Reload slot for the OTEL layer. Starts empty (None);
        // setup_telemetry() swaps in a real layer.
        let (otel_slot, handle) = reload::Layer::new(None::<
            cognee_observability::BoxedTelemetryLayer<tracing_subscriber::Registry>,
        >);
        let _ = OTEL_RELOAD_HANDLE.set(handle);

        let _ = Registry::default()
            .with(filter)
            .with(otel_slot)
            .with(TracingToLogLayer)
            .try_init();
    });
    let _ = py;
}
```

The same shape applies to `js/cognee-neon/src/default_subscriber.rs`
— add a `pub(crate) static OTEL_RELOAD_HANDLE: OnceLock<reload::Handle<…>>`
and install a `reload::Layer::new(None)` instead of the bare fmt
layer. The fmt layer stays composed alongside the reload slot:

```rust
let (otel_slot, handle) = reload::Layer::new(None::<cognee_observability::BoxedTelemetryLayer<Registry>>);
let _ = OTEL_RELOAD_HANDLE.set(handle);

let _ = Registry::default()
    .with(filter)
    .with(otel_slot)
    .with(fmt::layer().with_writer(std::io::stderr))
    .try_init();
```

### 4.2 PyO3 `setup_telemetry`

Create `python/src/telemetry_otlp.rs`:

```rust
use std::sync::{Mutex, OnceLock};

use cognee_observability::{
    EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled,
};
use pyo3::prelude::*;
use tracing_subscriber::Registry;

use crate::default_subscriber::OTEL_RELOAD_HANDLE;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.python-binding";

#[pyfunction]
pub fn setup_telemetry() -> PyResult<()> {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(()); // idempotent
    }

    // Decision 8: apply binding-specific default service name.
    apply_default_service_name(SERVICE_NAME_DEFAULT);

    let settings = EnvSettingsView::from_env();
    if !is_tracing_enabled(&settings) {
        // Not configured — leave the reload slot empty, no guard.
        // Stash a sentinel guard so a re-call short-circuits.
        *lock = Some(TelemetryGuard::noop());
        return Ok(());
    }

    let (layer, guard) = init_telemetry::<Registry>(&settings).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("init_telemetry failed: {e}"))
    })?;

    // Swap the OTEL layer into the reload slot. If the slot was
    // never set (COGNEE_BINDING_SUPPRESS_LOGS=1), warn and skip.
    if let Some(handle) = OTEL_RELOAD_HANDLE.get() {
        if let Err(err) = handle.modify(|opt| *opt = Some(layer)) {
            eprintln!("cognee-python: failed to install OTEL layer: {err}");
        }
    } else {
        eprintln!(
            "cognee-python: setup_telemetry() called but the default subscriber \
             is suppressed; OTLP export disabled"
        );
    }

    *lock = Some(guard);
    Ok(())
}

fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        // SAFETY: set_var is unsafe in Rust 2024 because env is
        // process-global. We document at the call site that this
        // mutation happens once at setup_telemetry() time.
        unsafe { std::env::set_var("OTEL_SERVICE_NAME", default); }
    }
}
```

Wire into [`python/src/lib.rs`](../../../python/src/lib.rs):

```rust
mod telemetry_otlp;
// ...
m.add_function(wrap_pyfunction!(telemetry_otlp::setup_telemetry, m)?)?;
```

And expose in
[`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py):

```python
from cognee_pipeline._native import (
    ...,
    setup_logging,
    setup_telemetry,
)
__all__ = [..., "setup_logging", "setup_telemetry"]
```

### 4.3 Neon `setupTelemetry`

Create `js/cognee-neon/src/telemetry_otlp.rs`:

```rust
use std::sync::{Mutex, OnceLock};

use cognee_observability::{
    EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled,
};
use neon::prelude::*;
use tracing_subscriber::Registry;

use crate::default_subscriber::OTEL_RELOAD_HANDLE;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();
const SERVICE_NAME_DEFAULT: &str = "cognee.node-binding";

pub fn setup_telemetry(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(cx.undefined());
    }

    apply_default_service_name(SERVICE_NAME_DEFAULT);

    let settings = EnvSettingsView::from_env();
    if !is_tracing_enabled(&settings) {
        *lock = Some(TelemetryGuard::noop());
        return Ok(cx.undefined());
    }

    let (layer, guard) = init_telemetry::<Registry>(&settings)
        .or_else(|e| cx.throw_error::<_, ()>(format!("init_telemetry failed: {e}")))?;

    if let Some(handle) = OTEL_RELOAD_HANDLE.get() {
        let _ = handle.modify(|opt| *opt = Some(layer));
    } else {
        eprintln!(
            "cognee-neon: setupTelemetry() called but the default subscriber \
             is suppressed; OTLP export disabled"
        );
    }
    *lock = Some(guard);
    Ok(cx.undefined())
}

fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        unsafe { std::env::set_var("OTEL_SERVICE_NAME", default); }
    }
}
```

Wire into [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs):

```rust
mod telemetry_otlp;
// ...
cx.export_function("setupTelemetry", telemetry_otlp::setup_telemetry)?;
```

TypeScript declaration (`js/src/index.ts` or facade — verify):

```typescript
export const setupTelemetry: () => void = native.setupTelemetry;
```

### 4.4 C `cognee_init_otlp`

Create `capi/cognee-capi/src/telemetry_otlp.rs`:

```rust
use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

use cognee_observability::{
    EnvSettingsView, TelemetryGuard, init_telemetry, is_tracing_enabled,
};
use tracing_subscriber::Registry;

static OTEL_GUARD: OnceLock<Mutex<Option<TelemetryGuard>>> = OnceLock::new();

const SERVICE_NAME_DEFAULT: &str = "cognee.capi-binding";

/// Initialize OpenTelemetry export from environment variables.
///
/// Reads `COGNEE_TRACING_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and related
/// `OTEL_*` env vars. If neither
/// `COGNEE_TRACING_ENABLED=true` nor a non-empty
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is present, returns 0 without
/// installing anything (no-config is treated as success).
///
/// Returns:
///   0 — success (including idempotent re-call and no-config skip).
///   1 — internal lock poisoning (should not happen).
///   2 — observability init failure (collector unreachable, etc.).
///
/// Safe to call multiple times. The first non-noop call wins.
///
/// Unlike `cognee_setup_logging`, this function does **not**
/// install a `tracing` subscriber on its own — it adds the OTEL
/// layer to whichever subscriber the C host has already installed
/// (via `cognee_setup_logging` or by other means). If no
/// subscriber is installed, spans are dropped.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_init_otlp() -> c_int {
    let slot = OTEL_GUARD.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        Err(_) => return 1, // lock poison is unrecoverable
    };
    if lock.is_some() {
        return 0;
    }

    apply_default_service_name(SERVICE_NAME_DEFAULT);

    let settings = EnvSettingsView::from_env();
    if !is_tracing_enabled(&settings) {
        *lock = Some(TelemetryGuard::noop());
        return 0;
    }

    let (_layer, guard) = match init_telemetry::<Registry>(&settings) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("cognee_init_otlp: {err}");
            return 2;
        }
    };

    // NOTE: C binding does NOT install a reload-capable subscriber
    // by default (no equivalent of 07-02/07-03 for C). The OTEL
    // layer is dropped here; the global TracerProvider installed
    // by init_telemetry still works for any `tracing::*` callers
    // *if* a subscriber has been installed via the `tracing_log`
    // bridge through `cognee_setup_logging`. Spans created by
    // `tracing::instrument` annotations propagate through the
    // OpenTelemetry SDK's `Tracer` regardless of subscriber state.
    //
    // This is the documented v1 limitation; a follow-up could
    // install a reload-capable C-side subscriber to mirror the
    // PyO3 / Neon shape.
    let _ = _layer;

    *lock = Some(guard);
    0
}

fn apply_default_service_name(default: &str) {
    let current = std::env::var("OTEL_SERVICE_NAME").unwrap_or_default();
    if current.is_empty() {
        unsafe { std::env::set_var("OTEL_SERVICE_NAME", default); }
    }
}
```

Wire into [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs):

```rust
pub mod telemetry_otlp;
```

The cbindgen-generated header will pick up `cognee_init_otlp` from
`#[unsafe(no_mangle)] pub extern "C"`. Run `bash capi/scripts/check.sh`
to regenerate.

### 4.5 PyO3 / Neon: `init_telemetry` Layer composability check

`init_telemetry::<Registry>(&settings)`
([`crates/observability/src/init.rs:90`](../../../crates/observability/src/init.rs#L90))
returns `(BoxedTelemetryLayer<Registry>, TelemetryGuard)` where
`BoxedTelemetryLayer<S>` is the type alias
`Box<dyn Layer<S> + Send + Sync + 'static>` defined at
[`crates/observability/src/init.rs:30`](../../../crates/observability/src/init.rs#L30).
The reload slot type is `Option<BoxedTelemetryLayer<Registry>>`.

Verify the implementor pins the same `tracing-subscriber` version
across:
- workspace `Cargo.toml`
- `js/cognee-neon/Cargo.toml`
- transitive deps used by `cognee-observability`

A mismatch produces an opaque `Layer` trait-mismatch error.

The `tracing_subscriber::reload` module is gated behind the `std`
feature, which is part of `tracing-subscriber`'s default feature
set. Workspace pins (`features = ["env-filter", "fmt", "json"]`,
`features = ["env-filter", "fmt"]` in Neon) do NOT disable
defaults, so `reload::Layer` / `reload::Handle` are available
without any `Cargo.toml` change. If the workspace ever moves to
`default-features = false`, add `"std"` (or `"reload"` explicitly
if upstream extracts it) to the feature list.

## 5. Verification

```bash
# 1. All bindings compile.
cargo check -p cognee-python -p cognee-capi --all-targets
cd js/cognee-neon && cargo check --all-targets && cd -

# 2. cbindgen regenerates with cognee_init_otlp visible.
bash capi/scripts/check.sh
grep -q "cognee_init_otlp" capi/cognee-capi/include/cognee.h

# 3. Manual smoke (full test in 07-07).
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
  python -c 'import cognee_pipeline; cognee_pipeline.setup_telemetry()'
# Expect no error; collector receives spans.

# 4. Service-name default applied.
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
  python -c 'import os, cognee_pipeline
cognee_pipeline.setup_telemetry()
print(os.environ["OTEL_SERVICE_NAME"])'  # → cognee.python-binding

# 5. No-config case is a silent no-op.
python -c 'import cognee_pipeline; cognee_pipeline.setup_telemetry()'
# Expect no stderr, no exception.

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`python/src/default_subscriber.rs`](../../../python/src/default_subscriber.rs) —
  add `OTEL_RELOAD_HANDLE` static and reload-capable slot.
- `python/src/telemetry_otlp.rs` — NEW.
- [`python/src/lib.rs`](../../../python/src/lib.rs) — module
  declaration + `add_function` for `setup_telemetry`.
- [`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py) —
  re-export.
- [`js/cognee-neon/src/default_subscriber.rs`](../../../js/cognee-neon/src/default_subscriber.rs) —
  add `OTEL_RELOAD_HANDLE` static and reload-capable slot.
- `js/cognee-neon/src/telemetry_otlp.rs` — NEW.
- [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs) —
  module declaration + `export_function` for `setupTelemetry`.
- `js/src/index.ts` (or facade — verify) — TypeScript re-export.
- `capi/cognee-capi/src/telemetry_otlp.rs` — NEW.
- [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs) —
  module declaration.
- [`capi/cognee-capi/include/cognee.h`](../../../capi/cognee-capi/include/cognee.h) —
  cbindgen regen (or hand-edit doc).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `reload::Layer` type signature drifts across `tracing-subscriber` versions | Medium — the type is `reload::Layer<L, S>`; getting `L` right is fiddly when `L` is `Option<BoxedLayer>`. | The boxed-`Option` pattern is the standard idiom (see `tracing-subscriber` docs §reload). Sub-agent C runs `cargo check` which surfaces signature mismatches immediately. |
| `std::env::set_var` is `unsafe` under Rust 2024 (process-global UB if mutated concurrently) | Low at install time (single thread on first import), Medium if a host calls `setup_telemetry()` from multiple threads. | Wrap in `unsafe` block with documentation. Idempotent slot guarantees only one mutation per process. |
| C API installs the OTEL layer but no subscriber consumes the layer object | Acknowledged — see comment in §4.4. The `TracerProvider` install by `init_telemetry` produces spans through the OpenTelemetry SDK independently of the `tracing` subscriber. Spans emitted via `#[tracing::instrument]` flow through `tracing-opentelemetry`'s `OpenTelemetryLayer` only if it's installed in a subscriber. | Document the v1 limitation; follow-up could add a reload-capable C-side subscriber for parity. |
| `cognee-observability` API change (e.g. `init_telemetry` return type) breaks binding code | Low — gap 01 is closed and the API is stable. | If it changes, sub-agent A flags `needs-update`. |
| Mixing `unsafe { std::env::set_var }` with other tests that read `OTEL_SERVICE_NAME` in parallel | Medium in tests. | Tests in 07-07 are marked `#[serial_test::serial]` or use pytest `monkeypatch`. |
| Embedder calls `setup_telemetry` before `setup_logging` and expects logs to flush via the OTEL provider | Misuse — `setup_telemetry` only wires the OTEL bridge, not file logging. | Document the matrix in 07-08 README. |

## 8. Out of scope

- Mirroring `OTEL_RELOAD_HANDLE` into the C binding. Reload-capable
  C subscribers are a v2 enhancement.
- Exposing `shutdown_telemetry()` to force-flush early. `TelemetryGuard`
  drops on process exit; v1 has no early-shutdown hook.
- Custom `Resource` attribute setting beyond `service.name`. Hosts
  set additional `OTEL_RESOURCE_ATTRIBUTES` via env directly.
- Capturing logs as OTEL log signals. Gap 07 is trace-only.
