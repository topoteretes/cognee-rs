# Task 06-08 — `setup_logging()` in Python, JS, and C bindings

**Status**: implemented in commit c14ba2a (note: also added `python/tests/test_logging_smoke.py`, `js/__tests__/logging.test.ts`, and a `cognee_setup_logging()` call in `capi/examples/example_pipeline.c` — broader than the original "Files modified" list)
**Owner**: _unassigned_
**Depends on**: [Task 06-05 — init_logging](05-init-logging.md).
**Blocks**:
- [Task 06-10 — Tests](10-tests.md) (cross-SDK parity test calls `setup_logging()` from the Python harness).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 9 (all three bindings expose `setup_logging()`; argument-less; idempotent via singleton `LogGuards`).

---

## 1. Goal

Add an argument-less `setup_logging()` entrypoint to each of the
three binding crates:

| Binding | Crate | Symbol | Signature |
|---|---|---|---|
| Python | [`python/`](../../../python/) | `setup_logging` | `() -> None` (registered on the `_native` module) |
| JS / Node | [`js/cognee-neon/`](../../../js/cognee-neon/) | `setupLogging` | `() -> JsUndefined` (registered as exported function) |
| C | [`capi/cognee-capi/`](../../../capi/cognee-capi/) | `cognee_setup_logging` | `extern "C" fn() -> c_int` (returns 0 on success, non-zero on failure) |

Each entrypoint:

1. Calls `cognee_logging::LoggingConfig::from_env()`. On error, logs
   to stderr and returns (or returns non-zero for C).
2. Calls `cognee_logging::init_logging(cfg, std::iter::empty())`.
3. Stashes the returned `LogGuards` in a static `OnceLock<Mutex<Option<LogGuards>>>`
   (one per binding crate). Subsequent calls are no-ops.

## 2. Rationale

- Decision 9 chose argument-less wrappers because env-var
  configuration is the only surface (decision 8). Wrapping arguments
  in a Python/JS dict would create a second config path that
  diverges from the binaries.
- Stashing `LogGuards` in a singleton is essential: if the guard is
  dropped, the `tracing-appender::non_blocking` worker thread shuts
  down and the file becomes truncated/silent. Bindings cannot ask
  the host application to hold a guard, so they hold it themselves.
- Idempotence (first call wins; subsequent calls are no-ops) matches
  the binaries — `try_init()` silently no-ops a second install
  attempt.

## 3. Pre-conditions

- Task 06-05 committed; `cognee-logging` exports `init_logging`,
  `LoggingConfig`, `LogGuards`.
- Binding crates exist at the paths listed in §1.
- The Python binding's pymodule is named `_native` (verified via
  `python/src/lib.rs:14`). The TypeScript wrapper around the Neon
  binary lives in [`js/lib/`](../../../js/lib/) — task 06-08 also
  re-exports `setupLogging` from the TypeScript surface.

## 4. Step-by-step

### 4.1 Python binding

In [`python/Cargo.toml`](../../../python/Cargo.toml), add:

```toml
cognee-logging = { path = "../crates/logging" }
```

Create [`python/src/logging.rs`](../../../python/src/logging.rs):

```rust
use std::sync::{Mutex, OnceLock};

use cognee_logging::{init_logging, LogGuards, LoggingConfig};
use pyo3::prelude::*;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

#[pyfunction]
pub fn setup_logging() -> PyResult<()> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(()); // idempotent
    }

    let cfg = LoggingConfig::from_env().map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("invalid logging config: {e}"))
    })?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(())
}
```

Then in [`python/src/lib.rs`](../../../python/src/lib.rs):

```rust
mod logging;
// ...
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // ... existing add_class calls ...
    m.add_function(wrap_pyfunction!(logging::setup_logging, m)?)?;
    error::register(m)?;
    Ok(())
}
```

Expose at the package level in
[`python/cognee_pipeline/__init__.py`](../../../python/cognee_pipeline/__init__.py)
(or whatever the top-level Python facade is — verify the exact
filename before editing):

```python
from cognee_pipeline._native import setup_logging
__all__ = [..., "setup_logging"]
```

### 4.2 JS / Node binding

In [`js/cognee-neon/Cargo.toml`](../../../js/cognee-neon/Cargo.toml),
add:

```toml
cognee-logging = { path = "../../crates/logging" }
```

Create [`js/cognee-neon/src/logging.rs`](../../../js/cognee-neon/src/logging.rs):

```rust
use std::sync::{Mutex, OnceLock};

use cognee_logging::{init_logging, LogGuards, LoggingConfig};
use neon::prelude::*;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

pub fn setup_logging(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(cx.undefined()); // idempotent
    }

    let cfg = LoggingConfig::from_env()
        .or_else(|err| cx.throw_error(format!("invalid logging config: {err}")))?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(cx.undefined())
}
```

In [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs)
register the function inside `#[neon::main] fn main`:

```rust
mod logging;
// ...
cx.export_function("setupLogging", logging::setup_logging)?;
```

Mirror on the TypeScript facade in
[`js/lib/index.ts`](../../../js/lib/index.ts) (or whichever file
re-exports native bindings — verify):

```typescript
export const setupLogging: () => void = native.setupLogging;
```

### 4.3 C binding

In [`capi/cognee-capi/Cargo.toml`](../../../capi/cognee-capi/Cargo.toml),
add:

```toml
cognee-logging = { path = "../../crates/logging" }
```

Create [`capi/cognee-capi/src/logging.rs`](../../../capi/cognee-capi/src/logging.rs):

```rust
use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

use cognee_logging::{init_logging, LogGuards, LoggingConfig};

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem from environment variables.
///
/// Returns 0 on success (including idempotent re-call), non-zero on
/// configuration error (an invalid env-var value).
///
/// Safe to call multiple times; the second and later calls are
/// no-ops and return 0.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_setup_logging() -> c_int {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        Err(_) => return 1, // lock poison is unrecoverable
    };
    if lock.is_some() {
        return 0; // idempotent
    }

    let cfg = match LoggingConfig::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cognee_setup_logging: {err}");
            return 2;
        }
    };
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    0
}
```

Wire the module into [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs):

```rust
pub mod logging;
```

Update the cbindgen-generated header
([`capi/cognee-capi/include/cognee.h`](../../../capi/cognee-capi/include/cognee.h)
— path may differ; verify) by running `capi/scripts/check.sh` (which
is part of `scripts/check_all.sh` and regenerates the header). If
the header is hand-maintained, add the prototype manually:

```c
/// Initialize cognee's logging subsystem from environment variables.
/// Returns 0 on success (or idempotent re-call), non-zero on error.
int cognee_setup_logging(void);
```

### 4.4 Smoke tests per binding

Add a minimal smoke test in each binding's existing test harness:

**Python** —
[`python/tests/test_logging_smoke.py`](../../../python/tests/test_logging_smoke.py)
(new file):

```python
import os
import tempfile
from cognee_pipeline import setup_logging

def test_setup_logging_creates_file(tmp_path, monkeypatch):
    monkeypatch.setenv("COGNEE_LOGS_DIR", str(tmp_path))
    monkeypatch.delenv("LOG_FILE_NAME", raising=False)
    setup_logging()
    # at least one *.log present
    assert any(p.suffix == ".log" for p in tmp_path.iterdir())

def test_setup_logging_is_idempotent(tmp_path, monkeypatch):
    monkeypatch.setenv("COGNEE_LOGS_DIR", str(tmp_path))
    setup_logging()
    setup_logging()  # must not raise
```

**JS** — [`js/__tests__/logging.test.ts`](../../../js/__tests__/logging.test.ts)
(new file), pattern mirroring existing tests in
[`js/__tests__/`](../../../js/__tests__/).

**C** — extend the example/integration runner under
[`capi/examples/`](../../../capi/examples/) with a call to
`cognee_setup_logging()`. A new dedicated test file is not required —
the existing CI step in `capi/scripts/check.sh` covers the symbol
being exported.

## 5. Verification

```bash
# 1. Each binding crate compiles.
cargo check -p cognee-pipeline --all-targets
cargo check -p cognee-neon --all-targets
cargo check -p cognee-capi --all-targets

# 2. Python smoke test.
cd python && pytest tests/test_logging_smoke.py

# 3. JS smoke test.
cd js && npm test -- logging

# 4. C check runs cbindgen + the example build.
bash capi/scripts/check.sh

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- `python/Cargo.toml` — new dep.
- `python/src/lib.rs` — register `setup_logging`.
- `python/src/logging.rs` — NEW.
- `python/cognee_pipeline/__init__.py` (or equivalent) — re-export.
- `python/tests/test_logging_smoke.py` — NEW.
- `js/cognee-neon/Cargo.toml` — new dep.
- `js/cognee-neon/src/lib.rs` — register `setupLogging`.
- `js/cognee-neon/src/logging.rs` — NEW.
- `js/lib/index.ts` (or equivalent) — TypeScript re-export.
- `js/__tests__/logging.test.ts` — NEW.
- `capi/cognee-capi/Cargo.toml` — new dep.
- `capi/cognee-capi/src/lib.rs` — module declaration.
- `capi/cognee-capi/src/logging.rs` — NEW.
- `capi/cognee-capi/include/cognee.h` (or generated) — symbol added.
- `capi/examples/<existing>.c` (optional) — call new symbol.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Python's `monkeypatch.setenv` doesn't reach the Rust `std::env::var` call (in some pytest plugin configurations) | Low — pytest-monkeypatch is OS-level; Rust sees the same env. | Smoke test reads `COGNEE_LOGS_DIR` back via `os.environ` first to confirm the patch took. |
| Neon's `JsResult` requires the closure to *return* errors via `cx.throw_error`, which is a `Result` not a `Throws` | Medium | The sketch uses `or_else(|err| cx.throw_error(...))`. Implementor verifies against Neon's current API; the pattern is standard in `js/cognee-neon/src/runtime.rs`. |
| C header drift between hand-edited and cbindgen-generated | Medium | `capi/scripts/check.sh` is part of `scripts/check_all.sh`. If the header is generated, edits go via the build; if hand-edited, the script will diff and fail. |
| Loading binding crate triggers `_native` import which races with another import installing a subscriber | Low | `OnceLock + Mutex` is process-global; `init_logging`'s `try_init` is idempotent at the tracing level. |
| Binding consumers expect `setup_logging` to accept kwargs (Python users especially) | Documented — decision 8 + 9 | Add a `# Note` to the docstring: "All configuration is via env vars. Set them *before* calling setup_logging." Document in README too (06-11). |

## 8. Out of scope

- Per-binding optional args (`level=`, `logs_dir=`, etc.). Decision
  9 locked argument-less.
- Async wrappers for Python (`async def setup_logging`). `init_logging`
  is synchronous; bindings call it directly. Holding `LogGuards` in
  a singleton means the worker thread is created once and lives for
  the process — no async coordination needed.
- A `shutdown_logging()` C/Python/JS function. Drop happens at
  process exit when the binding's module unloads. Adding an explicit
  shutdown invites use-after-free if anything still emits.
