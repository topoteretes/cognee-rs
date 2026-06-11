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

**Cargo plumbing (one place):**

Add `cognee-bindings-common` as a dependency in `capi/cognee-capi/Cargo.toml`
(the crate's `[dependencies]`, NOT the workspace `members`):

```toml
cognee-bindings-common = { path = "../../crates/bindings-common", default-features = false }
```

Forward the same feature flags already present in `cognee-capi`'s `[features]`
section (the set mirrors `cognee-neon`).  The path `../../crates/bindings-common`
is relative to `capi/cognee-capi/`, which resolves correctly to the root-workspace
member at `crates/bindings-common`.

Also add `serde_json` as a direct dep (needed in `sdk.rs` for JSON parsing in
the settings overlay and for serialising results).  `serde_json` is NOT yet in
`capi/Cargo.toml`'s `[workspace.dependencies]`, so add it there first:

```toml
# in capi/Cargo.toml [workspace.dependencies]
serde_json = "1"
```

then reference it from `cognee-capi/Cargo.toml`:

```toml
serde_json = { workspace = true }
```

New module `capi/cognee-capi/src/sdk.rs` and add `pub mod sdk;` to
`capi/cognee-capi/src/lib.rs`:

```rust
pub struct CgSdk {
    pub state: Arc<cognee_bindings_common::HandleState>,
}
```

**Runtime init in `cg_sdk_new`:** The global tokio runtime lives in
`crate::runtime::GLOBAL_RUNTIME` (a `OnceLock<AsyncRuntime>`); the helper
`crate::runtime::global_runtime()` is `pub(crate)`.  `cg_sdk_new` must call
`cg_init()` (or replicate its idempotent-init logic inline) when the runtime
has not yet been initialised.  The simplest correct approach is:

```rust
if crate::runtime::global_runtime().is_none() {
    let code = crate::runtime::cg_init_impl();  // extract the body of cg_init
    if code != CgErrorCode::Ok { /* propagate */ }
}
```

Alternatively, expose a `pub(crate) fn ensure_runtime() -> CgErrorCode` in
`runtime.rs` that calls `cg_init()` idempotently.  Either way, keep the
`OnceLock` as the single source of truth; do not create a second runtime.

Exported functions (hand-written in **`cognee_sdk.h`** — see header note in Part C):

| Function | Signature | Semantics |
|---|---|---|
| `cg_sdk_new` | `CgSdk* cg_sdk_new(const char* settings_json)` | `settings_json` NULL → `HandleState::from_env()`; non-NULL → parse the JSON string, merge over env-loaded `Settings`, call `HandleState::from_settings(merged)`. The 3-way overlay logic (`defaults < env < json`) lives entirely in `cg_sdk_new` — `HandleState::from_settings` expects a fully-overlaid `Settings` struct. Note: Part A's plan referenced a `HandleState::from_settings_json` method that was NOT implemented; the actual API is `HandleState::from_settings(Settings)` + `HandleState::from_env()`. Use `cognee_lib::config::ConfigManager::from_env()` to get the env-overlaid Settings, then apply the JSON patch on top. Sync, no I/O. Ensures the global runtime exists (idempotent `cg_init` semantics). Returns NULL + last-error on failure. **Ordering footgun (document in `cognee_sdk.h`):** because the runtime is a process-wide OnceLock, `cg_init_with_threads(n)` called *after* the first `cg_sdk_new` silently no-ops — consumers wanting a custom thread count must call it first. |
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

**SDK error codes:** `CgErrorCode` in `capi/cognee-capi/src/error.rs` must be
extended with 8 SDK variants (values 11–18) per decision D5.  The Rust enum uses
PascalCase variant names; the C header uses screaming-snake (e.g. Rust
`CgErrorCode::Component` → C `CG_ERR_COMPONENT`).  Because the header is
hand-maintained (see Part C), you must add both the Rust variant **and** the C
`#define` / enum entry by hand.  Append to the existing enum block in
`error.rs`:

```rust
// SDK tier (values 11–18, append-only per D5)
Component = 11,
ServiceBuild = 12,
UserBootstrap = 13,
SdkValidation = 14,
Unsupported = 15,
FeatureNotBuilt = 16,
UnknownConfigKey = 17,
ConfigTypeMismatch = 18,
```

Add a `From<&SdkError> for CgErrorCode` impl in `sdk.rs` (or `error.rs`) that
maps each `SdkError` variant to its code (R2: `cg_sdk_*` functions must only
emit SDK codes 11–18 + `CG_OK`/`CG_ERR_NULL_POINTER`/`CG_ERR_RUNTIME`/
`CG_ERR_UTF8`; engine codes 2, 4–9 must never cross tiers).

Note: `SdkValidation` (= 14) is the SDK-tier validation code delivered through
the callback; it maps to `SdkError::Validation`.  The existing engine-tier
`CG_ERR_INVALID_ARGUMENT` (= 2) and `CG_ERR_TYPE_MISMATCH` (= 9) are NOT
reused by the SDK tier per R2.

Header/versioning:

- `cognee_sdk.h` is **hand-written and committed** to `capi/include/cognee_sdk.h`,
  following the same convention as the existing `capi/include/cognee.h`.  The
  existing `capi/cognee-capi/build.rs` is a no-op (the comment reads: "Header is
  maintained manually at capi/include/cognee.h / cbindgen was useful for
  bootstrapping but the manual header gives better control over the C API surface").
  Do NOT attempt to wire cbindgen into `build.rs` for this phase — write the
  header by hand.  The `cbindgen.toml` in `capi/cognee-capi/` exists as a
  bootstrapping artefact; it is not invoked during the build.
- `cognee_sdk.h` must open with `#include "cognee.h"` (for `CgErrorCode`,
  `cg_string_destroy`), its own include-guard (`COGNEE_SDK_H`), and an
  `extern "C"` block.
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
- [ ] `CgErrorCode` extended with SDK codes 11–18 in `error.rs`; `From<&SdkError> for CgErrorCode` impl in `sdk.rs`
- [ ] `CgSdkResultCallback` + `CgSdkWaiter` working; new C smoke test
      (`capi/examples/sdk_handle_smoke.c`): new (env + JSON settings) → warm → owner_id →
      clone → destroy via the waiter, using `MOCK_EMBEDDING=true` + temp dirs
- [ ] `capi/examples/CMakeLists.txt` updated to build `sdk_handle_smoke`; `capi/scripts/check.sh` updated to run it
- [ ] `cognee_sdk.h` hand-written + committed to `capi/include/cognee_sdk.h`; `cg_api_version()` returns `(1 << 16) | 1` (major=1, minor=1)

## Risks

- **Neon regression** — the TS Phase-8 incident (files deleted by a later phase) shows
  cross-surface refactors bite; gate on the full JS suite in the same PR.
- **Crate scope creep** — `bindings-common` is the *bindings facade*, not a new user-facing
  Rust API (that remains `cognee_lib::api`); document this in the crate-level docs and keep
  binding-specific types (Neon/FFI) out of it.
- **Three-workspace dance** — `bindings-common` is a root-workspace member consumed by path
  from two standalone workspaces (js, capi); its dependency versions must stay compatible
  with both patch tables (Phase 0's mirroring rule covers this).
