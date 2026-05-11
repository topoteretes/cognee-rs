# Task 07-06 — Per-binding analytics plumbing + `COGNEE_HOST_SDK` sentinel

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: [Task 07-01 — Workspace deps](01-workspace-deps.md).
**Blocks**:
- [Task 07-07 — Tests](07-tests.md) (`test_setup_telemetry_idempotent.py`, `test_telemetry_no_double_emit.py`).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 4 (plumbing lands even though bindings don't currently emit), 10 (`COGNEE_HOST_SDK` sentinel scoped to binding-armed emissions), 11 (per-binding default policies), 12 (idempotent singleton pattern).

---

## 1. Goal

Add three new entrypoints — one per binding — that "arm" product
analytics emission, and extend `cognee-telemetry` so its suppression
logic honours the host-SDK sentinel only when armed by a binding:

| Binding | Crate | Symbol | Signature |
|---|---|---|---|
| Python | [`python/`](../../../python/) | `setup_telemetry_analytics` | `() -> bool` (True = armed) |
| JS / Node | [`js/cognee-neon/`](../../../js/cognee-neon/) | `setupTelemetryAnalytics` | `() -> JsBoolean` |
| C | [`capi/cognee-capi/`](../../../capi/cognee-capi/) | `cognee_init_telemetry` | `extern "C" fn() -> c_int` (0 = armed, 1 = suppressed by policy, 2 = lock poison) |

Each entrypoint:

1. Evaluates the binding-specific default policy from decision 11.
   If the policy disallows emission (e.g. PyO3 without
   `COGNEE_RUST_TELEMETRY=1`), the function stashes a sentinel "off"
   state and returns the "not armed" outcome.
2. Otherwise it sets a process-global `BINDING_ARMED` flag inside
   `cognee_telemetry::env` so that
   `cognee_telemetry::env::is_disabled()` knows the calling context
   is a binding (vs. a pure-Rust embedder using `cognee-lib`).
3. Idempotent via the same `OnceLock<Mutex<Option<…>>>` pattern as
   `setup_logging`/`setup_telemetry`.

Concurrent change inside `cognee-telemetry`:

4. Extend
   [`crates/telemetry/src/env.rs`](../../../crates/telemetry/src/env.rs)'s
   `is_disabled()` to also return `true` when `BINDING_ARMED == true`
   AND `COGNEE_HOST_SDK` is set to any non-empty value.
5. Expose `BINDING_ARMED` via `pub fn arm_binding_emission()` /
   `pub fn is_binding_armed()` so bindings can mutate it via a
   stable public API instead of touching a `static`.

## 2. Rationale

- Decision 4 lands the plumbing now so the policy is locked when a
  future gap surfaces `cognee_lib::api::*` through bindings.
  Implementing later would require coordinated changes across all
  three bindings simultaneously; doing it now lets the cross-SDK
  test (decision 13) live behind a skip marker.
- Decision 10 scopes the sentinel to binding-armed emissions. The
  CLI binary uses `cognee-telemetry` the same way bindings do but
  must not be suppressed when an unrelated upstream process has set
  `COGNEE_HOST_SDK`. The `BINDING_ARMED` flag (only set by the
  binding entrypoints) makes the distinction clean.
- Decision 11 codifies the asymmetric defaults: PyO3 defers to the
  upstream `cognee` SDK; Neon owns its ecosystem; C is explicit.

## 3. Pre-conditions

- Task 07-01 committed.
- `cognee_telemetry::env::is_disabled()` is the single chokepoint
  every call site uses
  ([`crates/telemetry/src/env.rs:15`](../../../crates/telemetry/src/env.rs#L15)).
  Verify by `grep -rn 'env::is_disabled\|is_disabled()'
  crates/telemetry/src/`.
- Bindings depend on `cognee-telemetry` (added in 07-01).

## 4. Step-by-step

### 4.1 Extend `cognee_telemetry::env`

Edit
[`crates/telemetry/src/env.rs`](../../../crates/telemetry/src/env.rs).
Add at module scope:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// Set to `true` when a binding (PyO3, Neon, or C API) has called
/// its `setup_telemetry_analytics` / `cognee_init_telemetry`
/// entrypoint and the binding-specific policy allowed emission.
///
/// Gates the `COGNEE_HOST_SDK` sentinel in `is_disabled()`: pure-Rust
/// embedders (CLI, http-server) using `cognee_lib::api::*` do not
/// set this flag and are therefore not suppressed by
/// `COGNEE_HOST_SDK`.
///
/// Mutated exclusively via [`arm_binding_emission`]; read via
/// [`is_binding_armed`] and inside [`is_disabled`].
static BINDING_ARMED: AtomicBool = AtomicBool::new(false);

/// Called from a binding entrypoint after the per-binding policy
/// permits emission. Idempotent.
pub fn arm_binding_emission() {
    BINDING_ARMED.store(true, Ordering::SeqCst);
}

/// Returns the current value of the binding-armed flag.
pub fn is_binding_armed() -> bool {
    BINDING_ARMED.load(Ordering::SeqCst)
}
```

Update the existing `is_disabled()` body:

```rust
pub fn is_disabled() -> bool {
    if let Ok(v) = std::env::var("TELEMETRY_DISABLED")
        && !v.is_empty()
    {
        return true;
    }
    if let Ok(env) = std::env::var("ENV")
        && (env == "test" || env == "dev")
    {
        return true;
    }
    // Decision 10: COGNEE_HOST_SDK only suppresses emissions
    // armed by a binding, never the pure-Rust embedder path.
    if is_binding_armed()
        && let Ok(v) = std::env::var("COGNEE_HOST_SDK")
        && !v.is_empty()
    {
        return true;
    }
    false
}
```

Add unit tests in the same file (alongside the existing `tests`
module — see line 73 onward), all marked `#[serial]`:

```rust
#[test]
#[serial]
fn is_disabled_when_binding_armed_and_host_sdk_set() {
    arm_binding_emission();
    unsafe { std::env::set_var("COGNEE_HOST_SDK", "python"); }
    assert!(is_disabled());
    unsafe { std::env::remove_var("COGNEE_HOST_SDK"); }
    // BINDING_ARMED leak is acceptable across tests (one-shot semantics).
}

#[test]
#[serial]
fn is_not_disabled_when_only_host_sdk_set_without_arming() {
    // Reset: there is no public API to un-arm. This test must run
    // before any test that arms in the same process. Acceptable
    // limitation — document.
    unsafe { std::env::set_var("COGNEE_HOST_SDK", "python"); }
    if !is_binding_armed() {
        assert!(!is_disabled());
    }
    unsafe { std::env::remove_var("COGNEE_HOST_SDK"); }
}
```

The "BINDING_ARMED leak" caveat is the cost of using `AtomicBool`
for a process-lifecycle flag. The implementor MAY use a
`#[cfg(test)] pub fn reset_binding_armed()` to make tests truly
independent — recommended.

### 4.2 PyO3 `setup_telemetry_analytics`

Create `python/src/telemetry_analytics.rs`:

```rust
use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission from this Python process.
///
/// Default policy (decision 11): emission stays OFF unless
/// `COGNEE_RUST_TELEMETRY=1` is set AND `COGNEE_HOST_SDK` is unset.
/// The upstream `cognee` Python SDK owns identity emission; this
/// binding defers to it.
///
/// Returns `True` if analytics were armed by this call (or a
/// previous call). Idempotent.
#[pyfunction]
pub fn setup_telemetry_analytics() -> PyResult<bool> {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return Ok(armed);
    }

    let opt_in = std::env::var("COGNEE_RUST_TELEMETRY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let host_sdk = std::env::var("COGNEE_HOST_SDK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let armed = opt_in && !host_sdk;

    if armed {
        cognee_telemetry::env::arm_binding_emission();
    }
    *lock = Some(armed);
    Ok(armed)
}
```

Wire into [`python/src/lib.rs`](../../../python/src/lib.rs):

```rust
mod telemetry_analytics;
// ...
m.add_function(wrap_pyfunction!(telemetry_analytics::setup_telemetry_analytics, m)?)?;
```

Re-export in
[`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py):

```python
from cognee_pipeline._native import (..., setup_telemetry_analytics)
__all__ = [..., "setup_telemetry_analytics"]
```

### 4.3 Neon `setupTelemetryAnalytics`

Create `js/cognee-neon/src/telemetry_analytics.rs`:

```rust
use std::sync::{Mutex, OnceLock};

use neon::prelude::*;

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

pub fn setup_telemetry_analytics(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return Ok(cx.boolean(armed));
    }

    // Default policy (decision 11): ON unless TELEMETRY_DISABLED,
    // ENV in {test, dev}, or COGNEE_HOST_SDK is set.
    let telemetry_disabled = std::env::var("TELEMETRY_DISABLED")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let env_test_or_dev = std::env::var("ENV")
        .map(|v| v == "test" || v == "dev")
        .unwrap_or(false);
    let host_sdk = std::env::var("COGNEE_HOST_SDK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let armed = !(telemetry_disabled || env_test_or_dev || host_sdk);

    if armed {
        cognee_telemetry::env::arm_binding_emission();
    }
    *lock = Some(armed);
    Ok(cx.boolean(armed))
}
```

Wire into [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs):

```rust
mod telemetry_analytics;
// ...
cx.export_function("setupTelemetryAnalytics", telemetry_analytics::setup_telemetry_analytics)?;
```

TS facade:

```typescript
export const setupTelemetryAnalytics: () => boolean = native.setupTelemetryAnalytics;
```

### 4.4 C `cognee_init_telemetry`

Create `capi/cognee-capi/src/telemetry_analytics.rs`:

```rust
use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

static ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

/// Arm cognee product-analytics emission for this process.
///
/// Default policy (decision 11): C bindings are explicit-only —
/// calling this function arms emission unless the same opt-outs
/// recognized by `cognee_telemetry::env::is_disabled` are set
/// (`TELEMETRY_DISABLED`, `ENV in {test, dev}`, or
/// `COGNEE_HOST_SDK` non-empty).
///
/// Returns:
///   0 — armed (analytics will fire on subsequent `send_telemetry` calls).
///   1 — not armed (policy suppressed emission).
///   2 — internal lock poisoning (should not happen).
#[unsafe(no_mangle)]
pub extern "C" fn cognee_init_telemetry() -> c_int {
    let slot = ARMED.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        Err(_) => return 2,
    };
    if let Some(armed) = *lock {
        return if armed { 0 } else { 1 };
    }

    let telemetry_disabled = std::env::var("TELEMETRY_DISABLED")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let env_test_or_dev = std::env::var("ENV")
        .map(|v| v == "test" || v == "dev")
        .unwrap_or(false);
    let host_sdk = std::env::var("COGNEE_HOST_SDK")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let armed = !(telemetry_disabled || env_test_or_dev || host_sdk);

    if armed {
        cognee_telemetry::env::arm_binding_emission();
    }
    *lock = Some(armed);
    if armed { 0 } else { 1 }
}
```

Wire into [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs):

```rust
pub mod telemetry_analytics;
```

Regenerate the cbindgen header (`bash capi/scripts/check.sh`).

### 4.5 Public surface of `cognee-telemetry`

Edit
[`crates/telemetry/src/lib.rs`](../../../crates/telemetry/src/lib.rs)
to make sure `env::arm_binding_emission` and `env::is_binding_armed`
are reachable from the binding crates. They already are (`pub mod env`
is already exposed; verify), so no change should be needed beyond
adding the new symbols inside `env.rs`.

## 5. Verification

```bash
# 1. cognee-telemetry tests including the new sentinel logic.
cargo test -p cognee-telemetry

# 2. All bindings compile.
cargo check -p cognee-python -p cognee-capi --all-targets
cd js/cognee-neon && cargo check --all-targets && cd -

# 3. cbindgen regen surfaces cognee_init_telemetry.
bash capi/scripts/check.sh
grep -q "cognee_init_telemetry" capi/cognee-capi/include/cognee.h

# 4. PyO3 policy smoke (manual; full test in 07-07).
python -c 'import cognee_pipeline; print(cognee_pipeline.setup_telemetry_analytics())'  # → False
COGNEE_RUST_TELEMETRY=1 python -c 'import cognee_pipeline; print(cognee_pipeline.setup_telemetry_analytics())'  # → True
COGNEE_RUST_TELEMETRY=1 COGNEE_HOST_SDK=python python -c 'import cognee_pipeline; print(cognee_pipeline.setup_telemetry_analytics())'  # → False

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/telemetry/src/env.rs`](../../../crates/telemetry/src/env.rs) —
  add `BINDING_ARMED` + accessors + new branch in `is_disabled`;
  tests.
- `python/src/telemetry_analytics.rs` — NEW.
- [`python/src/lib.rs`](../../../python/src/lib.rs) — module + add_function.
- [`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py) —
  re-export.
- `js/cognee-neon/src/telemetry_analytics.rs` — NEW.
- [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs) — module + export_function.
- `js/src/index.ts` (verify) — TypeScript re-export.
- `capi/cognee-capi/src/telemetry_analytics.rs` — NEW.
- [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs) — module declaration.
- [`capi/cognee-capi/include/cognee.h`](../../../capi/cognee-capi/include/cognee.h) — cbindgen regen.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `BINDING_ARMED` leak across `#[serial]` tests | Acknowledged — see §4.1 caveat. | Add `#[cfg(test)] pub fn reset_binding_armed()`. Implementor MUST add this to keep tests independent. |
| Hosts depending on the CLI's `cognee-telemetry` behaviour observe a behavior change because a stale `COGNEE_HOST_SDK` env var is present | Low — decision 10 scoped sentinel to binding-armed paths; CLI doesn't arm. | Tested explicitly in §4.1's second unit test. |
| Bindings consume `cognee-telemetry` as a direct dep, increasing compile time | Low — `cognee-telemetry` is small. | Accepted. |
| Future binding expansion surfaces `cognee_lib::api::*` but forgets to call `setup_telemetry_analytics` before the first emission | Acknowledged risk. | Document loudly in 07-08 README that this entrypoint MUST be called before any `cognee_lib::api::*` invocation if analytics are desired. |
| Python `cognee` SDK forgets to set `COGNEE_HOST_SDK` and a user does set `COGNEE_RUST_TELEMETRY=1` from inside the SDK process | Out of scope — fix in the upstream Python SDK. | Document in 07-08 README the host-SDK responsibility. |

## 8. Out of scope

- Actually calling `send_telemetry` from inside the bindings.
  Decision 4 explicitly defers the binding-side emission until a
  binding wraps `cognee_lib::api::*`.
- A `disarm_binding_emission()` API. Bindings install once; the
  flag stays set until process exit. No host has asked for early
  disarm.
- A test that wraps a real network endpoint to detect double-emit.
  Decision 13 marks `e2e-cross-sdk/harness/test_telemetry_no_double_emit.py`
  as skipped until a binding actually emits.
