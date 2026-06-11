# Phase 1 — Shared facade & SDK handle (keystone)

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the SDK facade (`HandleState`, `CogneeServices`, `SdkError`, shared wire
helpers) lives in the new **`crates/bindings-common`** crate (`cognee-bindings-common`,
decision D1) consumed by **both** `cognee-neon` and `cognee-capi`; C callers can create,
warm, and destroy an opaque `CgSdk` handle through the minimal async plumbing
(`CgSdkResultCallback` + `CgSdkWaiter`, decision D4); the new `cognee_sdk.h` header and the
API version symbols exist (decision D8).

This is the de-risking keystone, exactly as Phase 1 was for the TS bindings. Once this is
right, Phases 4–7 are mechanical ports of the corresponding `sdk_*.rs` neon modules.

**Land as at least two PRs** (one phase, separately gated changes): **PR-1** = Part A (new
crate + neon refactor), gated on the full JS suite; **PR-2** = Parts B–C (capi handle +
async plumbing + header), gated on the C smoke test. Bundling them would couple a neon
regression risk to capi review churn — the TS Phase-8 incident is the cautionary tale.

## Prerequisites

Phase 0 (capi extracted + cognee-lib linked).

## Part A — Hoist the facade into `crates/bindings-common` (decision D1)

New crate `cognee-bindings-common` (root-workspace member at `crates/bindings-common`,
depending on `cognee-lib` with forwarded features; consumed by path from both the standalone
`js/cognee-neon` and `capi` workspaces). The code to move, currently in
`js/cognee-neon/src/`:

| Source (neon) | Destination (bindings-common) | What stays in neon |
|---|---|---|
| `sdk.rs` → `HandleState` struct + `impl HandleState { services(), owner_id() }` + private `CogneeHandle::new_from_settings()` constructor logic | `src/handle.rs` | `CogneeHandle` struct + `impl Finalize` + `cognee_new` / `cognee_warm` / `cognee_owner_id` Neon exports; `stringify_js` call (converts JS object arg to a JSON string before forwarding to `HandleState::from_settings_json`) |
| `services.rs` → `CogneeServices` struct + `impl CogneeServices { build(), cpu_pool() }` | `src/services.rs` | Nothing from this file; it is already neon-free (only `crate::errors::SdkError` import changes to `cognee_bindings_common::SdkError`) |
| `errors.rs` → `SdkError` enum (7 variants: `Component`, `ServiceBuild`, `UserBootstrap`, `Runtime`, `Validation`, `Unsupported`, `FeatureNotBuilt`) + `impl SdkError { code() }` | `src/error.rs` | `throw_sdk_error` function (uses `neon::prelude::*`); `throw_config_error` in `config.rs` (maps `cognee_lib::config::ConfigError` → JS error, stays in neon `config.rs`) |
| `sdk.rs` → settings-overlay construction (`defaults < env < object`) | `src/handle.rs` as `HandleState::from_settings_json(settings_json: Option<&str>) -> Result<Settings, SdkError>` | Neon-side argument parsing in `cognee_new`: JS-object args are stringified via `stringify_js` before being passed as `Option<&str>` to `from_settings_json` |
| `json.rs` → neon-free serde helpers: `cognify_result_json`, `marshal_one`, `marshal_bytes`, `decode_byte_array`, `marshal_inputs` | `src/wire.rs` | `stringify_js`, `parse_js`, `js_to_serde`, `js_to_value`, `read_opts` — all take neon `Context`/`Handle` params and must stay in neon; they are the JS↔serde bridge halves |

**Cargo plumbing (two places):**

1. Add `crates/bindings-common` to the root `Cargo.toml` `[workspace] members` list.
2. Add `cognee-bindings-common = { path = "../../crates/bindings-common", default-features = false }` to `js/cognee-neon/Cargo.toml` `[dependencies]`, forwarding the same features the neon crate already forwards to `cognee-lib` (listed in its `[features]` section).

Crate features: forward `cognee-lib`'s relevant flags (`visualization`, `cloud`, `qdrant`,
`ladybug`, `onnx`, `hf-tokenizer`, `tiktoken`, `sqlite`, `testing`) so each binding picks its
own set.

**Refactor `cognee-neon`** to `use cognee_bindings_common::…` for `HandleState`,
`CogneeServices`, `SdkError`, and the `wire` helpers, then delete the moved code from the
neon source tree. Residual neon-specific items (`CogneeHandle`, `Finalize`, `throw_sdk_error`,
`throw_config_error`, `stringify_js`, `parse_js`, `js_to_serde`, `js_to_value`, `read_opts`)
stay in neon as before. This must be behavior-neutral: the full JS check
(`js/scripts/check.sh`, 12+ suites) is an exit criterion of this phase, in the same PR.

## Part B — `CgSdk` handle in capi

New module `capi/cognee-capi/src/sdk.rs`:

```rust
pub struct CgSdk {
    pub state: Arc<cognee_bindings_common::HandleState>,
}
```

Exported functions (cbindgen → opaque `typedef struct CgSdk CgSdk;` in **`cognee_sdk.h`**):

| Function | Signature | Semantics |
|---|---|---|
| `cg_sdk_new` | `CgSdk* cg_sdk_new(const char* settings_json)` | `settings_json` NULL → env-only construction (`ConfigManager::from_env()`); otherwise the 3-way overlay `defaults < env < json`. Sync, no I/O. Ensures the global runtime exists (idempotent `cg_init` semantics). Returns NULL + last-error on failure. **Ordering footgun (document in `cognee_sdk.h`):** because the runtime is a process-wide OnceLock, `cg_init_with_threads(n)` called *after* the first `cg_sdk_new` silently no-ops — consumers wanting a custom thread count must call it first. |
| `cg_sdk_warm` | `void cg_sdk_warm(const CgSdk*, CgSdkResultCallback, void* user_data)` | Async (D4): builds/caches `CogneeServices` (DB connect, user bootstrap, engine init). Callback gets `result_json = "null"` (D9). |
| `cg_sdk_owner_id` | `void cg_sdk_owner_id(const CgSdk*, CgSdkResultCallback, void* user_data)` | Async; warms lazily; `result_json` is the quoted UUID string (strict JSON, D9). |
| `cg_sdk_clone` | `CgSdk* cg_sdk_clone(const CgSdk*)` | Arc clone (matches `cg_task_context_clone` convention). Sync. |
| `cg_sdk_destroy` | `void cg_sdk_destroy(CgSdk*)` | Drops the Arc. In-flight async ops keep the state alive via their own clones; callbacks may still fire after destroy — document. |

## Part C — Minimal async plumbing + new header (decisions D4, D8)

Needed in this phase so warm/owner-id are callable; hardened in Phase 2:

```c
typedef void (*CgSdkResultCallback)(CgErrorCode code,
                                    const char* result_json,    /* valid JSON or NULL */
                                    const char* error_message,  /* NULL on success */
                                    void* user_data);           /* ptrs valid only inside the callback */

CgSdkWaiter* cg_sdk_waiter_new(void);
void cg_sdk_waiter_callback(CgErrorCode, const char*, const char*, void*); /* pass as cb, waiter as user_data */
CgErrorCode cg_sdk_waiter_wait(CgSdkWaiter*, char** out_result_json);      /* blocks; result freed via cg_string_destroy */
void cg_sdk_waiter_destroy(CgSdkWaiter*);
```

The waiter blocks on a channel/condvar (no tokio involvement) and copies the result/error out
of the callback, storing the error message for `cg_last_error_message()` on the waiting
thread. **The waiter is single-use** (locked by review): one waiter ↔ one op, matching the
exactly-once callback contract; passing an already-consumed waiter as `user_data` again, or
calling `wait` twice, returns `CG_ERR_VALIDATION`. Resettable waiters invite reuse races for
no real ergonomic gain (creation is one malloc).

Header/versioning:

- Second cbindgen configuration emitting `capi/include/cognee_sdk.h` for the SDK tier
  (`cognee.h` stays engine-only and unchanged); `cognee_sdk.h` includes `cognee.h` for
  `CgErrorCode`/`cg_string_destroy`.
- `CG_API_VERSION_MAJOR`/`CG_API_VERSION_MINOR` defines + `uint32_t cg_api_version(void)`
  (`(major << 16) | minor`); MINOR bumps each phase that ships symbols.

Semantics locked by the TS decision log (inherit verbatim):
- `owner_id` = Python default-user semantics: `uuid5(NAMESPACE_OID, default_user_email)` via
  `get_or_create_default_user`; idempotent across warms.
- Services cache is version-invalidated: a config bump (Phase 3) triggers a full rebuild on
  next use.
- LLM resolved strictly at build: keyless warm requires a dummy `llm_api_key`.

## Exit criteria

- [ ] `cognee-bindings-common` compiles; unit test builds `HandleState` + `services()` with
      mock embedding + temp dirs (Tier-A, no network)
- [ ] `cognee-neon` consumes the shared facade; `js/scripts/check.sh` fully green
- [ ] `scripts/check_all.sh` green (fmt, clippy, capi/python/js binding checks)
- [ ] `CgSdkResultCallback` + `CgSdkWaiter` working; new C smoke test
      (`capi/examples/sdk_handle_smoke.c`): new (env + JSON settings) → warm → owner_id →
      clone → destroy via the waiter, using `MOCK_EMBEDDING=true` + temp dirs
- [ ] `cognee_sdk.h` generated + committed; `cg_api_version()` returns 1.1 (or chosen scheme)

## Risks

- **Neon regression** — the TS Phase-8 incident (files deleted by a later phase) shows
  cross-surface refactors bite; gate on the full JS suite in the same PR.
- **Crate scope creep** — `bindings-common` is the *bindings facade*, not a new user-facing
  Rust API (that remains `cognee_lib::api`); document this in the crate-level docs and keep
  binding-specific types (Neon/FFI) out of it.
- **Three-workspace dance** — `bindings-common` is a root-workspace member consumed by path
  from two standalone workspaces (js, capi); its dependency versions must stay compatible
  with both patch tables (Phase 0's mirroring rule covers this).
