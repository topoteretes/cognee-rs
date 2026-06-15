# 03 — FFI & Neon panic safety

> Wave 1 · Priority P0 (blocker) · Track A · Release-blocking: yes · Effort: 0.5d ·
> Depends on: — · Source: [release-readiness-plan.md](../release-readiness-plan.md) §3 B2 (B2.1–B2.4) + audit

[← Back to index](00-INDEX.md)

> **Status note (verified 2026-06-14, re-verified against live code):** The C-API
> work (B2.1–B2.4) is **already implemented**. `exec_status.rs` uses
> `crate::util::cstring_lossy` throughout; `util.rs`, `error.rs`, and `watcher.rs`
> have no bare `CString::new(...).unwrap()` on caller data; the NUL-byte inline
> regression tests exist and pass. The Neon Buffer fix (B2.2) was resolved with an
> `.expect(...)` that documents the OOM-is-unrecoverable rationale rather than with
> the `JsResult` signature change described below. Only the Neon part may warrant
> re-evaluation (see Step 4 below). **Implementer: run the Verification commands
> first — if they all pass, mark this task ✅ without any code changes.**

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

The **correct fallback pattern** now lives in
[capi/cognee-capi/src/util.rs](../../../capi/cognee-capi/src/util.rs) as
`cstring_lossy(s: &str) -> CString` (lines 11–19): on a NUL byte it strips the
interior NULs (via `chars().filter()`) and rebuilds the CString. All four VtableExecStatus
methods and the VtableWatcher `to_c` helper delegate to this function.

> **Path correction:** the original audit cited `capi/src/util.rs` and a
> `str_to_c_owned` pattern at lines 22–30. The real path is
> `capi/cognee-capi/src/util.rs` (there is no `capi/src/`). `str_to_c_owned`
> was refactored to simply call `cstring_lossy(s).into_raw()` (line 40).
> Additionally, `capi/cognee-capi/src/sdk.rs` wraps all async SDK operations
> in `std::panic::AssertUnwindSafe(fut).catch_unwind()` (around line 714)
> as an additional defensive layer.

This is pure robustness hardening: it changes no schema, no IDs, no parity-relevant
behavior. The only observable change is "panic → empty string / JS error".

## Prerequisites — read first

```bash
git checkout -b task/03-ffi-neon-panic-safety

# Run verification first — if all checks pass, the task is already done:
grep -rn 'CString::new.*\.unwrap()' capi/cognee-capi/src/   # expect: no output (only doc comment on line 203)
grep -n 'cx.buffer.*unwrap' js/cognee-neon/src/task.rs       # expect: no output (uses .expect())
grep -n 'interior_nul\|stamp_provenance_nul' capi/cognee-capi/src/exec_status.rs  # expect: test names present
```

Files (all verified to exist as of 2026-06-14):
- [capi/cognee-capi/src/exec_status.rs](../../../capi/cognee-capi/src/exec_status.rs) — **already done**: all 11 call sites use `crate::util::cstring_lossy`; two NUL regression tests inline (B2.1, B2.4).
- [capi/cognee-capi/src/util.rs](../../../capi/cognee-capi/src/util.rs) — **already done**: defines `cstring_lossy` (lines 11–19); `str_to_c_owned` delegates to it (line 40). No bare `unwrap` on caller data.
- [capi/cognee-capi/src/error.rs](../../../capi/cognee-capi/src/error.rs) — **already done**: `set_last_error` calls `crate::util::cstring_lossy` (line 91). No bare unwrap on caller strings.
- [capi/cognee-capi/src/watcher.rs](../../../capi/cognee-capi/src/watcher.rs) — **already done**: `to_c` delegates to `crate::util::cstring_lossy` (line 101). No bare unwrap.
- [capi/cognee-capi/src/sdk.rs](../../../capi/cognee-capi/src/sdk.rs) — **additional defense**: all async SDK ops wrapped in `catch_unwind` (~line 714).
- [js/cognee-neon/src/task.rs](../../../js/cognee-neon/src/task.rs) — **partially done**: `cx.buffer(v.len())` uses `.expect(...)` with justification (~line 355–357) rather than the `JsResult` propagation described in Step 4 below (B2.2).

## Files to change

> **All C-API files are already done.** Only the Neon file may require attention
> depending on the decision in Step 4.

| Path | Change | Status |
|---|---|---|
| `capi/cognee-capi/src/exec_status.rs` | replace 11 `CString::new(caller_str).unwrap()` with a sanitizing helper; add `#[cfg(test)]` NUL regression test | ✅ done |
| `capi/cognee-capi/src/util.rs` | `CString::new("").unwrap()` → `cstring_lossy` / `.expect(...)` (literal, justified) | ✅ done |
| `capi/cognee-capi/src/error.rs` | use `cstring_lossy` in `set_last_error` | ✅ done |
| `capi/cognee-capi/src/watcher.rs` | `to_c` delegates to `cstring_lossy` | ✅ done |
| `js/cognee-neon/src/task.rs` | `cx.buffer(..).unwrap()` → `.expect(...)` with OOM justification (done); optionally upgrade to `JsResult` propagation (see Step 4) | ⚠️ done via expect |

## Implementation steps

> **If running verification (Acceptance criteria) shows all checks pass, skip
> directly to Step 6 (Verification) and mark the task done. Steps 1–5 are
> retained as documentation of what was implemented, and for reference if a
> regression is found.**

### Step 1 — Add a sanitizing CString helper for caller data (exec_status.rs)

**Already implemented.** The 11 unwraps in `exec_status.rs` were replaced by
`crate::util::cstring_lossy(s)` (defined in `util.rs` lines 11–19). Rather than a
file-local `safe_cstring` helper (as originally planned), the fix was lifted into the
shared `util.rs` module so all C-API files can reuse it.

The actual implementation in `util.rs` uses `chars().filter(|&c| c != '\0')` to strip
NUL bytes (instead of replacing with a placeholder string). Either approach is valid;
the implemented approach is slightly more faithful to the original string content.

For reference, the original plan proposed a file-local helper:

```rust
fn safe_cstring(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_else(|_| {
        std::ffi::CString::new("(string contained null byte)")
            .expect("placeholder literal has no interior NUL")
    })
}
```

The actual `cstring_lossy` in `util.rs`:
```rust
pub(crate) fn cstring_lossy(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            let sanitized: String = s.chars().filter(|&c| c != '\0').collect();
            CString::new(sanitized)
                .expect("interior NULs stripped, so this cannot fail")
        }
    }
}
```

### Step 2 — Replace the 11 unwraps in exec_status.rs

**Already implemented.** The exact current state of `exec_status.rs` (lines 99–153):

**`is_completed`** (lines 99–100):
```rust
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
```

**`mark_completed`** (lines 115–116):
```rust
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
```

**`mark_failed`** (lines 131–133):
```rust
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
            let err = crate::util::cstring_lossy(error);
```

**`stamp_provenance`** (lines 149–153):
```rust
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
            let tn = crate::util::cstring_lossy(task_name);
            let (uid_ptr, _uid_bytes) = uuid_to_bytes_ptr(user_id);
            let ns_c = node_set.map(crate::util::cstring_lossy);
```

Verify:
```bash
grep -n 'CString::new.*unwrap' capi/cognee-capi/src/exec_status.rs   # expect: no output (line 203 is a doc comment, not code)
```

### Step 3 — Fix the three literal `CString::new("...").unwrap()` (convention compliance)

**Already implemented.** None of these files contain bare `CString::new(...).unwrap()` on
literal strings any more:

- **`capi/cognee-capi/src/util.rs`**: `str_to_c_owned` now calls `cstring_lossy(s).into_raw()` (line 40); no literal unwrap.
- **`capi/cognee-capi/src/error.rs`**: `set_last_error` calls `crate::util::cstring_lossy(&s)` (line 91); no literal unwrap.
- **`capi/cognee-capi/src/watcher.rs`**: `to_c` calls `crate::util::cstring_lossy(s)` (line 101); no literal unwrap.

Verify:
```bash
grep -rn 'CString::new.*\.unwrap()' capi/cognee-capi/src/   # expect: no output
```

### Step 4 — Fix the Neon Buffer allocation panic (js/cognee-neon/src/task.rs)

**Partially implemented — decision required.** The `cx.buffer(v.len())` call in
`OwnedValue::to_js` (lines 354–358) was changed from `.unwrap()` to `.expect(...)` with
the justification: `"buffer allocation cannot fail for a known-length byte slice unless
the JS engine is OOM, which is unrecoverable"`. The method signature remains infallible
(`Handle<'cx, JsValue>`).

**Current state** (lines 349–362):
```rust
    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> Handle<'cx, JsValue> {
        match self {
            OwnedValue::F64(v) => cx.number(*v).upcast(),
            OwnedValue::Bool(v) => cx.boolean(*v).upcast(),
            OwnedValue::Str(v) => cx.string(v).upcast(),
            OwnedValue::Bytes(v) => {
                let mut buf = cx
                    .buffer(v.len())
                    .expect("buffer allocation cannot fail for a known-length byte slice unless the JS engine is OOM, which is unrecoverable");
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
        }
    }
```

**Decision:** The `.expect` approach is acceptable under CLAUDE.md conventions when the
reason explains *why* the call cannot fail at runtime. OOM in a JS engine is indeed
unrecoverable — there is no meaningful error surface to return to. However, if stricter
Neon error propagation is desired (surfaces the failure as a JS exception rather than
a process abort), upgrade to the `JsResult` approach:

```rust
    fn to_js<'cx>(&self, cx: &mut impl Context<'cx>) -> JsResult<'cx, JsValue> {
        Ok(match self {
            OwnedValue::F64(v) => cx.number(*v).upcast(),
            OwnedValue::Bool(v) => cx.boolean(*v).upcast(),
            OwnedValue::Str(v) => cx.string(v).upcast(),
            OwnedValue::Bytes(v) => {
                // Buffer allocation can fail (OOM / V8 size limit). Surface it
                // as a JS exception instead of aborting.
                let mut buf = cx.buffer(v.len())?;
                buf.as_mut_slice(cx).copy_from_slice(v);
                buf.upcast()
            }
        })
    }
```

If you make this change, fix the one call site (line 392):
```bash
grep -n '\.to_js(' js/cognee-neon/src/task.rs   # single site: line ~392
```
```rust
let js_val = item.to_js(&mut cx);    // before
let js_val = item.to_js(&mut cx)?;   // after (closure already returns JsResult/Ok(()))
```
`JsResult` is available via the existing `use neon::prelude::*;` glob import.

**If skipping the JsResult upgrade:** the `.expect(...)` already satisfies CLAUDE.md
conventions and the acceptance criteria — no bare `.unwrap()` remains.

### Step 5 — Add the NUL-byte regression test (B2.4)

**Already implemented.** Two inline `#[cfg(test)]` tests exist at the end of
[capi/cognee-capi/src/exec_status.rs](../../../capi/cognee-capi/src/exec_status.rs)
(lines 193–288):

- `interior_nul_does_not_panic` — drives `mark_failed` with `"data\0id"` and `"error\0msg"`;
  verifies the C callback receives the sanitized (NUL-stripped) strings.
- `stamp_provenance_nul_node_set_does_not_panic` — drives `stamp_provenance` with
  `Some("node\0set")`; verifies no panic and the pointer is valid.

Both tests use `#[tokio::test]`. The crate `[dev-dependencies]` in `capi/cognee-capi/Cargo.toml`
already includes `tokio` with macros support.

The tests cover more than the original plan (which only required a single `mark_failed`
assertion) — they also validate the sanitized content reaching the C side.

## Verification

```bash
# 1. No bare unwrap on CString in the C API (line 203 is a doc comment — expected).
grep -rn 'CString::new.*\.unwrap()' capi/cognee-capi/src/   # expect: no output

# 2. No buffer().unwrap() in neon (uses .expect() instead).
grep -n 'cx.buffer.*unwrap' js/cognee-neon/src/task.rs       # expect: no output

# 3. C API compiles + both regression tests pass.
cargo test --manifest-path capi/Cargo.toml -p cognee-capi exec_status -- --nocapture
# expect:
#   test exec_status::tests::interior_nul_does_not_panic ... ok
#   test exec_status::tests::stamp_provenance_nul_node_set_does_not_panic ... ok

# 4. Full capi check (compile gate, both feature sets, CMake examples).
bash capi/scripts/check.sh

# 5. Neon crate compiles and clippy is clean.
cargo clippy --manifest-path js/cognee-neon/Cargo.toml --all-targets -- -D warnings

# 6. Workspace clippy still clean (cstring_lossy must not trip unwrap_used once task 23 lands).
cargo clippy --all-targets -- -D warnings
```

## Acceptance criteria

- [x] `cstring_lossy` helper in `util.rs`; all 11 caller-data call sites in `exec_status.rs` use `crate::util::cstring_lossy`.
- [x] No bare `CString::new(...).unwrap()` in `util.rs`, `error.rs`, or `watcher.rs` — all delegate to `cstring_lossy`.
- [x] `OwnedValue::to_js` uses `.expect(...)` with an OOM-is-unrecoverable justification (no bare `.unwrap()`). Optional upgrade to `JsResult` propagation possible but not required to ship.
- [x] Inline `#[cfg(test)]` NUL-byte regression tests (`interior_nul_does_not_panic`, `stamp_provenance_nul_node_set_does_not_panic`) present and passing.
- [x] `grep -rn 'CString::new.*\.unwrap()' capi/cognee-capi/src/` returns nothing (line 203 is a doc comment).
- [ ] `bash capi/scripts/check.sh` and both clippy runs pass with `-D warnings` (run to confirm).

## Gotchas / do-not

- **Never let a panic cross an `extern "C"` boundary.** The whole point: `unwrap()` inside
  a function reachable from a C callback is UB on unwind. `cstring_lossy` must itself never
  panic — its `expect` is on a derived string that provably has no NUL (all NULs were
  filtered out by `chars().filter()`).
- **NUL-stripping vs placeholder:** the implemented `cstring_lossy` strips NUL bytes
  (e.g. `"data\0id"` → `"dataid"`), unlike the originally planned placeholder approach
  (`"(string contained null byte)"`). The regression tests assert the strip behaviour —
  do not change to placeholder without updating the tests.
- **`to_js` signature:** if you later upgrade `to_js` to return `JsResult`, every call site
  must add `?`. Don't `.unwrap()` the new `JsResult` to "make it compile" — that
  reintroduces the panic. There is currently one call site at line 392.
- **Test placement:** NUL-byte tests must be inline (private types). An external integration
  test in `capi/cognee-capi/tests/` cannot reach `VtableExecStatus`.
- **`sdk.rs` catch_unwind is defense-in-depth only.** It covers async SDK ops but not
  sync FFI callback paths. The primary fix (no panicking code on those paths) is still required.
- Parity-neutral: no schema, IDs, hashes, chunking, prompts, or collection names change.

## Rollback

All changes are localized to four `.rs` files. `git checkout -- <file>` per file reverts
cleanly. The inline test is additive and can be deleted independently. No data migration.
