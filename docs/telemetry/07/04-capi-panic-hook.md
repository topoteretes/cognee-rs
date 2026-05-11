# Task 07-04 — C API panic hook on `cg_init`

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 07-07 — Tests](07-tests.md) (`capi/examples/panic_smoke.c` exercises the hook).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 6 (`cg_init` installs a one-shot panic hook).

---

## 1. Goal

Make Rust panics that cross the C FFI surface debuggable by default
— even when the embedder has not called `cognee_setup_logging()`.

Concretely:

1. From `cg_init` (and `cg_init_with_threads`) install
   `std::panic::set_hook` exactly once.
2. The hook writes a single-line message to stderr containing:
   - `[cognee-capi panic]` prefix (grep anchor).
   - The panic message (`PanicInfo::payload` downcast to `&str` /
     `String`).
   - The panic location (`file:line:column` from `PanicInfo::location`).
3. Guard via `OnceLock<()>` so subsequent calls do not replace the
   hook (a host application may install its own hook for non-cognee
   code paths).
4. The hook is independent of the logging subsystem — it writes
   directly to `std::io::stderr()`. This guarantees panic visibility
   even when `cognee_setup_logging` has not been called and no
   `tracing` subscriber is installed.

## 2. Rationale

- Decision 6 chose this as a low-cost, high-value default. Today a
  Rust panic in a C-embedded process either prints a default Rust
  panic message (if the embedder did not redirect stderr) or aborts
  silently (if stderr is redirected to `/dev/null`). The hook makes
  the location unambiguous.
- Installing from `cg_init` (not from a `static` constructor or
  `#[ctor]`) keeps the side effect explicit: embedders that never
  call `cg_init` still see the default Rust panic message.
- One-shot installation avoids the failure mode where a host
  application has installed its own hook for non-cognee panics —
  we don't want to clobber that on every `cg_init`.

## 3. Pre-conditions

- [`capi/cognee-capi/src/runtime.rs`](../../../capi/cognee-capi/src/runtime.rs)
  defines `cg_init` at lines 24–33 and `cg_init_with_threads` at
  lines 36–49.
- No existing panic hook is set by the C binding.

## 4. Step-by-step

### 4.1 Create `capi/cognee-capi/src/panic_hook.rs`

```rust
//! Panic hook installed by `cg_init`/`cg_init_with_threads`.
//!
//! Writes a single-line `[cognee-capi panic]` record to stderr with
//! the panic message and location. Coexists with
//! `cognee_setup_logging` — both can be active simultaneously.
//!
//! Installation is one-shot: subsequent `cg_init` calls do not
//! replace the hook so a host application may install its own
//! panic handler for non-cognee panics.

use std::io::Write;
use std::sync::OnceLock;

static INSTALLED: OnceLock<()> = OnceLock::new();

pub(crate) fn install_once() {
    INSTALLED.get_or_init(|| {
        std::panic::set_hook(Box::new(|info| {
            // Resolve the panic message into a borrowed &str when
            // possible — &'static str payloads are the most common
            // (panic!("foo") form), with String falling back via
            // downcast.
            let msg: &str = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| {
                    info.payload()
                        .downcast_ref::<String>()
                        .map(|s| s.as_str())
                })
                .unwrap_or("<no message>");

            let loc = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());

            // Single-line stderr write; ignore failures (we are in
            // a panic context, there is nothing to recover to).
            let line = format!("[cognee-capi panic] {msg} at {loc}\n");
            let _ = std::io::stderr().write_all(line.as_bytes());
        }));
    });
}
```

### 4.2 Wire into `cg_init`

Edit [`capi/cognee-capi/src/runtime.rs`](../../../capi/cognee-capi/src/runtime.rs).
At the top of the file add:

```rust
use crate::panic_hook;
```

Update `cg_init`:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn cg_init() -> CgErrorCode {
    panic_hook::install_once();
    match AsyncRuntime::new() {
        Ok(rt) => init_runtime(rt),
        Err(e) => {
            set_last_error(e.to_string());
            CgErrorCode::RuntimeError
        }
    }
}
```

Update `cg_init_with_threads` the same way (call `install_once`
before the `if n == 0` guard, so even invalid-arg paths benefit).

### 4.3 Wire the module

Edit [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs):

```rust
mod panic_hook;
```

(Module is `pub(crate)`; no C-visible symbols.)

### 4.4 Document in the cbindgen header header comment

If [`capi/cognee-capi/include/cognee.h`](../../../capi/cognee-capi/include/cognee.h)
is hand-maintained: extend the doc comment of `cg_init` to mention
the hook. If it is cbindgen-generated, the doc comment in
`runtime.rs` flows through automatically — add to `cg_init`'s rustdoc:

```rust
/// Initialize the global async runtime with default settings.
///
/// Also installs a process-wide panic hook (one-shot) that writes
/// `[cognee-capi panic]` records to stderr. Subsequent calls do
/// not replace the hook.
///
/// Must be called before `cg_pipeline_execute_in_background` or
/// `cg_pipeline_execute_async`. Safe to call multiple times (second
/// call returns an error but is harmless; the panic hook is only
/// installed on the first successful call).
#[unsafe(no_mangle)]
pub extern "C" fn cg_init() -> CgErrorCode { ... }
```

## 5. Verification

```bash
# 1. C binding compiles.
cargo check -p cognee-capi --all-targets

# 2. cbindgen regenerates the header.
bash capi/scripts/check.sh

# 3. Manual smoke test — write a tiny C program that calls cg_init
#    and triggers a panic via a deliberately-failing API call, run
#    it, and grep stderr for `[cognee-capi panic]`. (Automated in
#    07-07.)

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- `capi/cognee-capi/src/panic_hook.rs` — NEW.
- [`capi/cognee-capi/src/lib.rs`](../../../capi/cognee-capi/src/lib.rs) —
  module declaration.
- [`capi/cognee-capi/src/runtime.rs`](../../../capi/cognee-capi/src/runtime.rs) —
  call `panic_hook::install_once()` from `cg_init` and
  `cg_init_with_threads`; rustdoc on `cg_init`.
- [`capi/cognee-capi/include/cognee.h`](../../../capi/cognee-capi/include/cognee.h) —
  cbindgen regeneration (if generated) or hand-edit doc comment.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Host application sets its own panic hook *after* `cg_init` and expects to handle all panics | Low — explicitly allowed by the one-shot design. Host's `set_hook` replaces ours. | Document. |
| Host installs its hook *before* calling `cg_init` and expects ours to chain | Medium — `std::panic::set_hook` replaces, doesn't chain. | Document this is non-chaining. If a future user requests chaining, capture the previous hook via `std::panic::take_hook()` and call it inside ours. Defer until requested. |
| Panic during the hook itself (e.g. stderr is closed) | Very low | `let _ = stderr.write_all(...)` ignores the error. The process is already going down. |
| `String` payload format changes across Rust versions | Very low — `panic!("{}", x)` consistently produces a `String` payload; `panic!("static")` produces `&'static str`. The downcast handles both. | Both branches present; the `<no message>` fallback covers exotic payloads. |
| `cbindgen` regen produces noise outside the doc comment | Medium — generator behaviour drifts across versions. | If the diff is unrelated to gap 07, escalate to user (could indicate an upstream cbindgen bump that warrants its own commit). |

## 8. Out of scope

- A `cg_clear_panic_hook` C function to remove our hook. Hosts that
  need their own hook just call `std::panic::set_hook` themselves
  via their own Rust glue — there is no clean C-side way to pass a
  Rust `Fn` closure.
- Chaining with a previously-installed hook (see Risks).
- Forwarding panic records through `tracing::error!`. Doing so would
  require a subscriber, which the hook explicitly cannot assume is
  installed. Direct stderr write is the lowest-coupling choice.
- Backtrace capture. Rust's default panic message already includes
  the location; full backtraces require `RUST_BACKTRACE=1` and are
  beyond gap 07's scope.
