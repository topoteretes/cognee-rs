# 03 — FFI & Neon panic safety

> Wave 1 · Priority P0 (blocker) · Track A · Release-blocking: yes · Effort: 0.5d ·
> Depends on: — · Source: [release-readiness-plan.md](../release-readiness-plan.md) §3 B2 (B2.1–B2.4) + audit

[← Back to index](00-INDEX.md)

## Goal

No `cg_*` C-API or Neon JS function panics on **caller-supplied data**. Specifically:
strings that contain an interior NUL byte (`\0`) and Buffer allocations that V8 rejects
must surface as benign no-ops / JS errors instead of unwinding across the FFI boundary
(which, in `extern "C"` callbacks, is **undefined behavior / process abort**). A
regression test drives a NUL-containing string through the C `ExecStatus` callbacks and
asserts no panic.

## Background & why

`CString::new(s)` returns `Err(NulError)` whenever `s` contains an interior `\0`. The
unwrap then panics. In an `async fn` invoked from an `extern "C"` vtable, a panic
unwinds toward the C frame — UB on most targets, an abort at best. The data flowing
through these callbacks (`data_id`, `pipeline_name`, `error`, `task_name`, `node_set`) is
**not controlled by us** — it originates from documents, user content, and LLM/tool
output — so an interior NUL is reachable in normal operation, not just adversarially.

The **correct fallback pattern already exists** in the codebase at
[capi/cognee-capi/src/util.rs:22-30](../../../capi/cognee-capi/src/util.rs) (`str_to_c_owned`)
and at [capi/cognee-capi/src/watcher.rs:97-100](../../../capi/cognee-capi/src/watcher.rs)
(`to_c`): on a NUL byte they substitute an empty/placeholder string instead of panicking.
We reuse that approach.

> **Path correction:** the release plan cites `capi/src/util.rs` and
> `capi/cognee-capi/src/exec_status.rs:99-153`. The real `util.rs` is at
> `capi/cognee-capi/src/util.rs` (there is no `capi/src/`). Line numbers below were
> **re-grepped on 2026-06-14** and are current; re-confirm with the grep commands in the
> Prerequisites before editing.

This is pure robustness hardening: it changes no schema, no IDs, no parity-relevant
behavior. The only observable change is "panic → empty string / JS error".

## Prerequisites — read first

```bash
git checkout -b task/03-ffi-neon-panic-safety

# Re-confirm the exact current locations before editing:
grep -n 'CString::new' capi/cognee-capi/src/exec_status.rs   # expect lines 99,100,115,116,131,132,133,149,150,151,153
grep -n 'CString::new("")' capi/cognee-capi/src/util.rs capi/cognee-capi/src/error.rs capi/cognee-capi/src/watcher.rs
grep -n 'cx.buffer' js/cognee-neon/src/task.rs               # expect line 355
```

Files (all verified to exist):
- [capi/cognee-capi/src/exec_status.rs](../../../capi/cognee-capi/src/exec_status.rs) — the cluster (B2.1).
- [capi/cognee-capi/src/util.rs](../../../capi/cognee-capi/src/util.rs) — has the reference pattern **and** one literal `CString::new("").unwrap()` (line 27) to fix (B2.3).
- [capi/cognee-capi/src/error.rs](../../../capi/cognee-capi/src/error.rs) — literal `CString::new("...").unwrap()` (line 90) (B2.3).
- [capi/cognee-capi/src/watcher.rs](../../../capi/cognee-capi/src/watcher.rs) — literal `CString::new("").unwrap()` (line 99) (B2.3).
- [js/cognee-neon/src/task.rs](../../../js/cognee-neon/src/task.rs) — `cx.buffer(v.len()).unwrap()` (line 355) (B2.2).

## Files to change

| Path | Change |
|---|---|
| `capi/cognee-capi/src/exec_status.rs` | replace 11 `CString::new(caller_str).unwrap()` with a sanitizing helper; add `#[cfg(test)]` NUL regression test |
| `capi/cognee-capi/src/util.rs` | `CString::new("").unwrap()` → `.expect(...)` (literal, justified) |
| `capi/cognee-capi/src/error.rs` | `CString::new("(error contained null byte)").unwrap()` → `.expect(...)` |
| `capi/cognee-capi/src/watcher.rs` | inner `CString::new("").unwrap()` → `.expect(...)` |
| `js/cognee-neon/src/task.rs` | `cx.buffer(..).unwrap()` → propagate as a JS error (change `to_js` signature to return `JsResult`) |

## Implementation steps

### Step 1 — Add a sanitizing CString helper for caller data (exec_status.rs)

The 11 unwraps in `exec_status.rs` all wrap **caller-supplied** `&str` (`data_id`,
`pipeline_name`, `error`, `task_name`, `node_set`). These must never panic. Add a private
helper that mirrors the existing `watcher.rs::to_c` fallback. At the top of
[capi/cognee-capi/src/exec_status.rs](../../../capi/cognee-capi/src/exec_status.rs),
after the `use` block (after line 6), add:

```rust
/// Build a `CString` from caller-supplied data without panicking.
///
/// `CString::new` fails only on an interior NUL byte. Callbacks here receive
/// data derived from documents / LLM output, where an interior NUL is reachable
/// in normal operation; panicking would unwind across the `extern "C"` boundary
/// (UB / abort). On a NUL byte we replace the value with a placeholder so the
/// callback still fires with a valid (if degraded) string.
fn safe_cstring(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_else(|_| {
        std::ffi::CString::new("(string contained null byte)")
            .expect("placeholder literal has no interior NUL")
    })
}
```

### Step 2 — Replace the 11 unwraps in exec_status.rs

Swap each `std::ffi::CString::new(X).unwrap()` for `safe_cstring(X)`. The exact current
sites (verified):

**`is_completed`** (lines 99–100):
```rust
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
```
→
```rust
            let did = safe_cstring(data_id);
            let pn = safe_cstring(pipeline_name);
```

**`mark_completed`** (lines 115–116):
```rust
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
```
→
```rust
            let did = safe_cstring(data_id);
            let pn = safe_cstring(pipeline_name);
```

**`mark_failed`** (lines 131–133):
```rust
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let err = std::ffi::CString::new(error).unwrap();
```
→
```rust
            let did = safe_cstring(data_id);
            let pn = safe_cstring(pipeline_name);
            let err = safe_cstring(error);
```

**`stamp_provenance`** (lines 149–153):
```rust
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let tn = std::ffi::CString::new(task_name).unwrap();
            let (uid_ptr, _uid_bytes) = uuid_to_bytes_ptr(user_id);
            let ns_c = node_set.map(|s| std::ffi::CString::new(s).unwrap());
```
→
```rust
            let did = safe_cstring(data_id);
            let pn = safe_cstring(pipeline_name);
            let tn = safe_cstring(task_name);
            let (uid_ptr, _uid_bytes) = uuid_to_bytes_ptr(user_id);
            let ns_c = node_set.map(safe_cstring);
```

After editing, confirm none remain:
```bash
grep -n 'CString::new.*unwrap' capi/cognee-capi/src/exec_status.rs   # expect: no output
```

### Step 3 — Fix the three literal `CString::new("...").unwrap()` (convention compliance)

These wrap **string literals** with no interior NUL, so they are functionally safe — but
the project rule forbids bare `unwrap()` in non-test code; convert to `expect` with a
justification (per CLAUDE.md "Coding Conventions").

**`capi/cognee-capi/src/util.rs:27`** — inside `str_to_c_owned`:
```rust
            CString::new("").unwrap().into_raw()
```
→
```rust
            CString::new("").expect("empty literal has no interior NUL").into_raw()
```

**`capi/cognee-capi/src/error.rs:90`** — inside `set_last_error`:
```rust
        CString::new(s).unwrap_or_else(|_| CString::new("(error contained null byte)").unwrap());
```
→
```rust
        CString::new(s).unwrap_or_else(|_| {
            CString::new("(error contained null byte)")
                .expect("placeholder literal has no interior NUL")
        });
```

**`capi/cognee-capi/src/watcher.rs:99`** — inside `to_c`:
```rust
    std::ffi::CString::new(s).unwrap_or_else(|_| std::ffi::CString::new("").unwrap())
```
→
```rust
    std::ffi::CString::new(s)
        .unwrap_or_else(|_| std::ffi::CString::new("").expect("empty literal has no interior NUL"))
```

> The **outer** `CString::new(s)` in `watcher.rs:99` is already a safe
> `unwrap_or_else` fallback for caller data — leave that fallback in place; only the inner
> literal `.unwrap()` changes.

### Step 4 — Fix the Neon Buffer allocation panic (js/cognee-neon/src/task.rs)

`cx.buffer(v.len())` can fail (OOM / V8 ArrayBuffer size limit) and currently
`.unwrap()`s into the JS runtime. The enclosing method is `OwnedValue::to_js` (verified
lines 349–360), which returns `Handle<'cx, JsValue>` (infallible). To propagate the error
we change its signature to `JsResult<'cx, JsValue>` and fix the single call site.

1. Change the method signature and all arms. Current (lines 349–360):

```rust
    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> Handle<'cx, JsValue> {
        match self {
            OwnedValue::F64(v) => cx.number(*v).upcast(),
            OwnedValue::Bool(v) => cx.boolean(*v).upcast(),
            OwnedValue::Str(v) => cx.string(v).upcast(),
            OwnedValue::Bytes(v) => {
                let mut buf = cx.buffer(v.len()).unwrap();
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
        }
    }
```

Replace with (return `JsResult`, `?` on the fallible buffer alloc, `Ok(..)` the rest):

```rust
    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> JsResult<'cx, JsValue> {
        Ok(match self {
            OwnedValue::F64(v) => cx.number(*v).upcast(),
            OwnedValue::Bool(v) => cx.boolean(*v).upcast(),
            OwnedValue::Str(v) => cx.string(v).upcast(),
            OwnedValue::Bytes(v) => {
                // Buffer allocation can fail (OOM / V8 size limit). Surface it
                // as a JS exception instead of panicking into the runtime.
                let mut buf = cx.buffer(v.len())?;
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
        })
    }
```

2. Fix the call site. Find it:
   ```bash
   grep -n '\.to_js(' js/cognee-neon/src/task.rs
   ```
   At each call, the result is now a `JsResult`. If the caller is already inside a
   `JsResult`-returning fn (typical for Neon callbacks), append `?`:
   ```rust
   let v = owned.to_js(&mut cx);     // before
   let v = owned.to_js(&mut cx)?;    // after
   ```
   If the caller is a closure passed to `channel.send`/`deferred.settle_with` that returns
   `NeonResult<_>`, `?` works directly. Inspect the surrounding fn signature to confirm it
   returns a `Result` Neon understands; if not, map the error (`.or_else(|e| ...)`) — but
   in this file the conversion happens inside a settle closure that already returns
   `JsResult`, so `?` is the right fix. Verify it compiles (Step 6).

3. Confirm `JsResult` is imported (it is part of the standard `use neon::prelude::*;`
   glob — check the top of the file; if a narrow import list is used, add `JsResult`).

### Step 5 — Add the NUL-byte regression test (B2.4)

`VtableExecStatus` and its `ExecStatusManager` impl are **private** to `exec_status.rs`
(verified: `struct VtableExecStatus` has no `pub`), so the test must live **inline** in
that file — an external `capi/cognee-capi/tests/*.rs` file cannot reach the private type.

Append to the **end** of
[capi/cognee-capi/src/exec_status.rs](../../../capi/cognee-capi/src/exec_status.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cognee_core::ExecStatusManager;
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static MARK_FAILED_CALLS: AtomicUsize = AtomicUsize::new(0);

    // Reads back every string arg via CStr (asserts they are valid C strings)
    // and records that the callback fired.
    unsafe extern "C" fn capture_mark_failed(
        _state: *mut c_void,
        data_id: *const c_char,
        pipeline_name: *const c_char,
        _dataset_id: *const u8,
        error: *const c_char,
    ) {
        // None of these may be null; all must be NUL-terminated C strings.
        assert!(!data_id.is_null());
        assert!(!pipeline_name.is_null());
        assert!(!error.is_null());
        let _ = unsafe { CStr::from_ptr(data_id) }.to_bytes();
        let _ = unsafe { CStr::from_ptr(pipeline_name) }.to_bytes();
        let _ = unsafe { CStr::from_ptr(error) }.to_bytes();
        MARK_FAILED_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn vtable_with_mark_failed() -> CgExecStatusManagerVtable {
        CgExecStatusManagerVtable {
            is_completed: None,
            mark_completed: None,
            mark_failed: Some(capture_mark_failed),
            stamp_provenance: None,
            destroy: None,
        }
    }

    // Passing strings with interior NUL bytes through the callbacks must NOT
    // panic / abort. Before the fix, CString::new(..).unwrap() aborted here.
    #[tokio::test]
    async fn nul_byte_strings_do_not_panic() {
        MARK_FAILED_CALLS.store(0, Ordering::SeqCst);
        let mgr = VtableExecStatus {
            state: std::ptr::null_mut(),
            vtable: vtable_with_mark_failed(),
        };

        // Interior NUL in every caller-supplied arg.
        mgr.mark_failed("data\0id", "pipe\0line", None, "err\0or")
            .await
            .expect("mark_failed must not error on NUL input");

        // Also exercise the other callbacks with NUL input (no panic = pass).
        let mgr2 = VtableExecStatus {
            state: std::ptr::null_mut(),
            vtable: CgExecStatusManagerVtable {
                is_completed: None,
                mark_completed: None,
                mark_failed: None,
                stamp_provenance: None,
                destroy: None,
            },
        };
        let _ = mgr2.is_completed("a\0b", "c\0d", None).await;
        let _ = mgr2.mark_completed("a\0b", "c\0d", None).await;
        let _ = mgr2
            .stamp_provenance("a\0b", "c\0d", "t\0n", None, Some("n\0s"))
            .await;

        assert_eq!(MARK_FAILED_CALLS.load(Ordering::SeqCst), 1);
    }
}
```

> If `cognee-capi`'s `[dev-dependencies]` lacks `tokio` with the macros feature, the
> `#[tokio::test]` won't compile. The crate already depends on `tokio` with
> `["rt-multi-thread","sync"]` (verified line 68). Add a dev-dependency enabling `macros`
> + `rt` to `capi/cognee-capi/Cargo.toml` if `cargo test` complains:
> ```toml
> [dev-dependencies]
> tokio = { workspace = true, features = ["macros", "rt"] }
> ```

## Verification

```bash
# 1. No bare unwrap on CString in the C API anymore (the test module is allowed).
grep -rn 'CString::new.*\.unwrap()' capi/cognee-capi/src/   # expect: no output

# 2. No buffer().unwrap() in neon.
grep -n 'cx.buffer.*unwrap' js/cognee-neon/src/task.rs       # expect: no output

# 3. C API compiles + the regression test passes.
cargo test --manifest-path capi/Cargo.toml -p cognee-capi exec_status -- --nocapture
# expect: test exec_status::tests::nul_byte_strings_do_not_panic ... ok

# 4. Full capi check (compile gate, both feature sets, CMake examples).
bash capi/scripts/check.sh

# 5. Neon crate compiles and clippy is clean.
cargo clippy --manifest-path js/cognee-neon/Cargo.toml --all-targets -- -D warnings

# 6. Workspace clippy still clean (the helper must not trip unwrap_used once task 23 lands).
cargo clippy --all-targets -- -D warnings
```

## Acceptance criteria

- [ ] `safe_cstring` helper added; all 11 caller-data unwraps in `exec_status.rs` use it.
- [ ] The 3 literal `CString::new(..).unwrap()` (util.rs:27, error.rs:90, watcher.rs:99 inner)
      converted to `.expect("...no interior NUL")`.
- [ ] `OwnedValue::to_js` returns `JsResult`; `cx.buffer(..)?` propagates; call site fixed.
- [ ] Inline `#[cfg(test)]` NUL-byte regression test added and passing.
- [ ] `grep -rn 'CString::new.*unwrap()' capi/cognee-capi/src/` returns nothing.
- [ ] `bash capi/scripts/check.sh` and both clippy runs pass with `-D warnings`.

## Gotchas / do-not

- **Never let a panic cross an `extern "C"` boundary.** The whole point: `unwrap()` inside
  a function reachable from a C callback is UB on unwind. `safe_cstring` must itself never
  panic — its `expect` is on a constant literal that provably has no NUL.
- **Do not change the placeholder text format casually if any consumer parses it** — these
  callbacks are debugging/provenance signals, not structured data; an empty or
  `"(string contained null byte)"` placeholder is fine. (No cross-SDK schema is involved
  — this is the in-process callback path, not on-disk data.)
- **The `to_js` signature change ripples.** Neon's `Handle` vs `JsResult` is a type change;
  every caller must add `?`. Don't `.unwrap()` the new `JsResult` to "make it compile" —
  that just reintroduces the panic. Trace each call site (Step 4.2) and propagate.
- **`watcher.rs:99` outer fallback stays.** Only the inner empty-literal `.unwrap()`
  changes; the outer `unwrap_or_else` is already the correct caller-data guard.
- **Test placement:** the test must be inline (private types). An external integration
  test in `capi/cognee-capi/tests/` will fail to compile against `VtableExecStatus`.
- Parity-neutral: no schema, IDs, hashes, chunking, prompts, or collection names change.

## Rollback

All changes are localized to four `.rs` files. `git checkout -- <file>` per file reverts
cleanly. The inline test is additive and can be deleted independently. No data migration.
