# CR-1 — C API: eliminate reachable `unwrap()` on FFI paths; enforce panic safety

- **Binding:** C API (`capi/cognee-capi/`)
- **Dimension:** Correctness / Cleanliness
- **Priority:** P0
- **Status:** Not started

## Problem

The project rule (CLAUDE.md) forbids `unwrap()` in non-test code. The C API
violates it on paths reachable from C callers, and a panic there is not merely a
logic bug — with no `catch_unwind` and no `panic = "abort"`, an unwinding panic
crossing the `extern "C"` boundary aborts the host process, which a C host cannot
recover from.

### Reachable `unwrap()` calls

In [capi/cognee-capi/src/exec_status.rs](../../capi/cognee-capi/src/exec_status.rs), the
`VtableExecStatus` trait methods build C strings from runtime data and `unwrap()`:

- `is_completed`: lines 99–100 — `CString::new(data_id)`, `CString::new(pipeline_name)`
- `mark_completed`: lines 115–116
- `mark_failed`: lines 131–133 — including `CString::new(error)` (an **error string**, which can plausibly contain any bytes, including interior NUL)
- `stamp_provenance`: lines 149–153 — including `CString::new(node_set)` (a caller-supplied node-set string)

`CString::new` fails on interior NUL bytes. `error` and `node_set` are not
guaranteed NUL-free, so this is a reachable panic on adversarial or unusual data,
driven by the pipeline engine from inside an async task.

There is also a literal `CString::new("").unwrap()` in
[capi/cognee-capi/src/util.rs:27](../../capi/cognee-capi/src/util.rs#L27) — safe (empty
literal), but should be normalized to the same fallback pattern for consistency.

### No panic firewall

- No `catch_unwind` exists anywhere in `capi/cognee-capi/src/`.
- `capi/Cargo.toml` does not set `panic = "abort"`.
- Mitigation today is only a one-shot panic hook installed by `cg_init`
  ([capi/cognee-capi/src/panic_hook.rs](../../capi/cognee-capi/src/panic_hook.rs)) that
  prints `[cognee-capi panic]` to stderr. The default `extern "C"` behavior on
  unwind is a process abort (defined, not UB), but it is still a hard kill.

The Python and JS bindings already use sanitized fallbacks for the same
conversion (`unwrap_or_else(... )`), so this is the C API lagging behind its
siblings.

## Goal / definition of done

1. No `unwrap()`/`expect()` on any non-test, C-reachable path produces a panic
   on attacker- or data-controlled input. Interior-NUL strings degrade
   gracefully instead of panicking.
2. Panics that do occur (genuine bugs) cannot unwind across the FFI boundary as
   UB — behavior is an explicit, documented abort.
3. `scripts/check_all.sh` (clippy `-D warnings`) and the C API smoke tests pass.

## Implementation plan

### Step 1 — Add a NUL-safe `CString` helper

In [capi/cognee-capi/src/util.rs](../../capi/cognee-capi/src/util.rs), add a helper that
never panics (mirror the JS/Python fallback semantics — strip interior NULs):

```rust
/// Build a C string from a Rust string, replacing interior NUL bytes so the
/// conversion can never fail. Used on FFI callback paths where panicking would
/// abort the host process.
pub(crate) fn cstring_lossy(s: &str) -> std::ffi::CString {
    match std::ffi::CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            let sanitized: String = s.chars().filter(|&c| c != '\0').collect();
            std::ffi::CString::new(sanitized)
                .expect("interior NULs stripped, so this cannot fail")
        }
    }
}
```

Note: the trailing `expect` is on a value that provably has no NUL bytes, with a
message explaining the invariant — this is the allowed `expect` pattern.

### Step 2 — Replace the `exec_status.rs` unwraps

In [capi/cognee-capi/src/exec_status.rs](../../capi/cognee-capi/src/exec_status.rs),
replace every `std::ffi::CString::new(x).unwrap()` (lines 99, 100, 115, 116, 131,
132, 133, 149, 150, 151, and the `node_set` map at 153) with
`crate::util::cstring_lossy(x)`. For `node_set`:

```rust
let ns_c = node_set.map(crate::util::cstring_lossy);
```

### Step 3 — Normalize the `util.rs` literal

Replace the `CString::new("").unwrap()` at
[capi/cognee-capi/src/util.rs:27](../../capi/cognee-capi/src/util.rs#L27) with a call to
`cstring_lossy("")` (or keep the literal but add a comment; prefer the helper for
uniformity). Audit `watcher.rs:99` and `error.rs:90` to confirm they already use
the fallback shape and, if so, route them through `cstring_lossy` too so there is
a single conversion path.

### Step 4 — Grep-gate the rule

Run a workspace grep to confirm no remaining reachable unwraps in the crate:

```bash
grep -rnE '\.unwrap\(\)' capi/cognee-capi/src/ | grep -vE 'lock\(\)|tests?|#\[cfg\(test\)\]'
```

The only acceptable remainders are `Mutex/RwLock::lock().unwrap()` (add the
`// lock poison is unrecoverable` comment where missing).

### Step 5 — Make panic-across-FFI an explicit abort

Two layers:

1. **`panic = "abort"` for the C API build.** The C API is its own Cargo
   workspace (`capi/Cargo.toml`). Add to both profiles:

   ```toml
   [profile.release]
   panic = "abort"

   [profile.dev]
   panic = "abort"
   ```

   Verify this does not break the workspace's existing `[profile.release]
   debug = true` setting (keep both keys). Confirm the staticlib/cdylib still
   build and the examples link.

2. **Defensive `catch_unwind` at the callback trampolines.** For the async SDK
   callback paths (`spawn_sdk_op` in `sdk.rs` and the task/exec-status
   trampolines), wrap the user-facing dispatch body in
   `std::panic::catch_unwind(AssertUnwindSafe(...))` and convert a caught panic
   into the existing error-code path (e.g. an internal-error code + last-error
   message) rather than relying on abort. This keeps a single panicking op from
   killing the process when `panic = "abort"` is not in effect (e.g. a host that
   statically links a panic=unwind build). Document the chosen guarantee in the
   header preamble.

   > Decision note: `panic = "abort"` and `catch_unwind` are partially redundant.
   > If the team prefers a single mechanism, `panic = "abort"` alone is the
   > simpler, stronger guarantee (no UB possible) but offers no recovery;
   > `catch_unwind` allows the host to keep running. Recommended: ship both —
   > `panic = "abort"` as the floor, `catch_unwind` for graceful degradation in
   > the SDK tier where an error code already exists.

### Step 6 — Test the fallback

Add a smoke test alongside the existing
[capi/scripts/check.sh](../../capi/scripts/check.sh) panic test that exercises an
exec-status callback (or a `mark_failed`-style path) with an error string
containing an interior NUL byte, asserting the process does not abort and the
callback receives a sanitized string. If wiring a full exec-status vtable from C
is heavy, add a Rust `#[cfg(test)]` unit test in `exec_status.rs` that calls the
trait methods with `"a\0b"` and asserts no panic.

## Verification

```bash
# from capi/
cargo clippy --all-targets -- -D warnings
bash scripts/check.sh           # builds + runs all C examples and smoke tests
# from repo root
scripts/check_all.sh
```

Plus the new interior-NUL test from Step 6.

## Risks / notes

- `panic = "abort"` changes unwinding semantics for the whole C API workspace;
  confirm no test relies on catching a panic via unwinding (the smoke test in
  `check.sh` checks the *hook* output, which still fires under abort).
- `cstring_lossy` silently drops NUL bytes; that is the same lossy behavior the
  JS/Python bindings already accept and is preferable to a crash. Note it in the
  helper's doc comment.
