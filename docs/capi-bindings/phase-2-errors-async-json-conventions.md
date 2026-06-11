# Phase 2 — Errors, async & JSON conventions

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the cross-cutting plumbing every SDK op reuses: extended error codes, the
hardened async-callback conventions (D4), the `CgSdkWaiter` sync bridge, and the strict-JSON
in/out contract (D3, D9). After this phase, adding an SDK op is ~30 lines.

## Prerequisites

Phase 1 (`CgSdk`, `cognee_bindings_common::SdkError`, minimal callback + waiter).

## A. Error model

Extend `CgErrorCode` (`capi/cognee-capi/src/error.rs`) — append, never renumber:

| New code | Value | Maps from |
|---|---|---|
| `CG_ERR_COMPONENT` | 11 | `SdkError::Component` |
| `CG_ERR_SERVICE_BUILD` | 12 | `SdkError::ServiceBuild` |
| `CG_ERR_USER_BOOTSTRAP` | 13 | `SdkError::UserBootstrap` |
| `CG_ERR_VALIDATION` | 14 | `SdkError::Validation` (bad JSON shape, missing field, parse failure) |
| `CG_ERR_UNSUPPORTED` | 15 | `SdkError::Unsupported` (s3 input, recursive dataItem) |
| `CG_ERR_FEATURE_NOT_BUILT` | 16 | `SdkError::FeatureNotBuilt` |
| `CG_ERR_UNKNOWN_CONFIG_KEY` | 17 | config dispatch (Phase 3) |
| `CG_ERR_CONFIG_TYPE_MISMATCH` | 18 | config dispatch (Phase 3) |

`SdkError::Runtime` maps to the existing `CG_ERR_RUNTIME`. Implement
`impl From<&SdkError> for CgErrorCode` next to the enum, plus a helper
`set_last_error_from(err: &SdkError) -> CgErrorCode` that stores `err.to_string()` in the
existing thread-local and returns the code. The TS `kind` strings (`"COMPONENT_ERROR"` …)
correspond 1:1 — document the mapping table in the header comment so C and TS consumers can
share error-handling docs.

**Tiering rule (must be stated in `cognee_sdk.h`):** the enum now contains two overlapping
vocabularies (engine `CG_ERR_INVALID_ARGUMENT`/`CG_ERR_MISSING_FIELD`/`CG_ERR_TYPE_MISMATCH`/
`CG_ERR_INVALID_CONFIG` vs SDK `CG_ERR_VALIDATION`/`CG_ERR_CONFIG_TYPE_MISMATCH`). To keep
the contract predictable: **`cg_sdk_*` functions return only** `CG_OK`, the SDK codes
(11–18), and the shared infrastructure codes `CG_ERR_NULL_POINTER`/`CG_ERR_RUNTIME`/
`CG_ERR_UTF8`. Engine codes 2, 4–9 never cross into the SDK tier (all input-shape problems
surface as `CG_ERR_VALIDATION`), and SDK codes never appear from `cg_task_*`/`cg_pipeline_*`
functions. Enforce in the `From<&SdkError>` mapping + a unit test.

**Caveat (thread-local + async):** the existing last-error slot is thread-local, which is
wrong for callbacks firing on runtime threads. Convention: async ops deliver the error
message *through the callback* (`error_message` param); the thread-local is authoritative
only for the sync functions (`cg_sdk_new`, config setters, `cg_sdk_waiter_wait`, functions
returning NULL/handles on the caller's thread).

## B. Async convention (D4 — the only call style)

The callback typedef and `CgSdkWaiter` were introduced in Phase 1; this phase hardens them
into the conventions every op phase reuses.

Uniform op signature:

```c
void cg_sdk_<op>(const CgSdk* sdk,
                 /* op-specific const char* params, */
                 const char* opts_json,        /* NULL = defaults */
                 CgSdkResultCallback cb,
                 void* user_data);
```

Rust-side helper (one for all ops):

```rust
fn spawn_sdk_op<F>(cb: CgSdkResultCallback, ud: *mut c_void, fut: F)
where F: Future<Output = Result<serde_json::Value, SdkError>> + Send + 'static
```

- Requires the global runtime (`cg_init` was made idempotent/auto in `cg_sdk_new`).
- `user_data` crosses threads: wrap in the existing send-pointer pattern used by
  `cg_pipeline_execute_async`.
- Callback fires **exactly once** and is **always deferred**: it never fires synchronously
  from inside `cg_sdk_<op>`, always from a runtime thread. Parse/validation errors detected
  before spawning are still delivered through a spawned task (one trivial spawn). This
  matches the rule mature async C APIs follow (libuv, gRPC completion queues, ONNX Runtime
  `RunAsync`): the initiating call never re-enters user code, so consumers don't have to
  write reentrancy-safe callbacks for a rare path.
- Pointer args are copied/parsed before return; the C caller may free its strings as soon as
  the op function returns.

**Explicit non-goal — cancellation:** SDK ops cannot be aborted once started (`cg_sdk_cognify`
can run minutes). This matches TS parity (the TS bindings have no cancel either) and is a
deliberate v1 cut, not an oversight. The future extension shape is reserved: ops accept an
optional `CgCancellationToken*` (the engine tier's existing type) via a trailing param or an
opts key, or return an op handle mirroring `cg_run_handle_abort`. Document the non-goal in
`cognee_sdk.h` so consumers plan for it.

## C. Sync bridge: `CgSdkWaiter` hardening

- `cg_sdk_waiter_wait` is guarded against deadlock: if called from a tokio runtime thread
  (`tokio::runtime::Handle::try_current()` succeeds — i.e. from inside another op's
  callback), fail fast with `CG_ERR_RUNTIME` "waiting from a runtime/callback thread"
  instead of blocking a worker.
- On error results, `wait` stores the message into the thread-local last-error slot of the
  **waiting** thread, so the classic `cg_last_error_message()` pattern works for sync-style
  callers.
- Optional `cg_sdk_waiter_wait_timeout(waiter, ms, out_json)` — decide during
  implementation; if skipped, record as an explicit non-goal.

## D. JSON contract (D3 + D9)

- All inputs/outputs are UTF-8; invalid UTF-8 → `CG_ERR_UTF8`.
- Wire shapes are **identical to the TS bindings** (camelCase keys, same option field names,
  same result objects — see `js/src/types.ts`). This is a hard rule: docs and tests transfer,
  and cross-SDK comparisons stay trivial.
- **Strict JSON always** (D9): `result_json` is a valid JSON document for every op — `true`/
  `false` for boolean results, quoted strings for uuid/path/html results, `null` for void
  ops, objects/arrays as in TS. Never a bare unquoted value.
- `opts_json` may be NULL or `"{}"` → defaults.
- Strings handed to the caller outside callbacks (e.g. via the waiter) are allocated by the
  library and freed with the existing `cg_string_destroy`; strings passed *into* callbacks
  are valid only for the callback's duration.
- Parsing happens in Rust with the shared `cognee_bindings_common::wire` helpers hoisted in
  Phase 1 (e.g. `SearchType` via `serde_json::from_value(Value::String(s))` with
  `SCREAMING_SNAKE_CASE`) — no capi-local reimplementation.
- The contract is documented in `cognee_sdk.h`'s header comment.

## Exit criteria

- [ ] error mapping implemented + unit-tested; `cognee_sdk.h` documents the code ↔ kind table
      **and the tiering rule** (SDK ops return only SDK + shared infra codes)
- [ ] `spawn_sdk_op` hardened: exactly-once, **always-deferred** (smoke test asserts the
      callback never fires before `cg_sdk_<op>` returns, including on validation errors)
- [ ] waiter deadlock guard: `cg_sdk_waiter_wait` from a callback thread → `CG_ERR_RUNTIME`
- [ ] cancellation non-goal + reserved extension shape documented in `cognee_sdk.h`
- [ ] negative-path smoke test: malformed settings/opts JSON → `CG_ERR_VALIDATION` with a
      useful message via callback `error_message` (and via `cg_last_error_message()` when
      using the waiter)
- [ ] both headers regenerated (new enum values in `cognee.h`; conventions doc in
      `cognee_sdk.h`)

## Risks

- **ABI stability**: appending enum values is ABI-safe; renumbering is not. Codify "append
  only" in a comment above the enum.
- **Callback discipline**: callbacks must be cheap and non-blocking; waiting (or calling
  `cg_sdk_waiter_wait`) inside a callback is rejected with `CG_ERR_RUNTIME`, not deadlocked.
