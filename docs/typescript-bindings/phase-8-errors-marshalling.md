# Phase 8 — Errors & marshalling

← [Index](../typescript-bindings-plan.md)

**Goal:** one consistent JSON marshalling path and a typed JS error hierarchy, so failures are
inspectable and every value crosses the boundary the same way. Design early; finalize once the
ops exist.

> **Audit note (updated after Phases 0–6 complete):** The sections below reflect what was
> actually built vs. what the original design assumed. Corrections are inline; the overall
> goal is unchanged.

## Current state (post-Phase-6 audit)

### Duplications to eliminate

The same three helper functions (`stringify_js`, `parse_js`, `js_to_value`) are copy-pasted
across **seven** modules:

| Module | Helpers present |
|---|---|
| `sdk_ops.rs` | `stringify_js`, `parse_js`, `js_to_value` (private) |
| `sdk_retrieval.rs` | `stringify_js`, `parse_js`, `js_to_value` (private) |
| `sdk_memory.rs` | `stringify_js`, `parse_js`, `js_to_value`, `read_opts` (pub(crate)) |
| `sdk_data.rs` | re-exports from `sdk_memory` (`parse_js`, `js_to_value`, `read_opts`) |
| `sdk_datasets.rs` | re-exports from `sdk_memory` (`parse_js`, `js_to_value`, `read_opts`) |
| `sdk_admin.rs` | re-exports from `sdk_memory` (`parse_js`, `js_to_value`, `read_opts`) |
| `sdk_visualization.rs` | `stringify_js` + inline opts reader (feature-gated local copy) |
| `sdk_cloud.rs` | `parse_js` + inline opts reader (feature-gated local copy) |
| `config.rs` | `stringify_js`, `parse_js`, `js_to_value`, `js_to_map` (private) |

Additionally, `cognify_result_json` and `marshal_inputs`/`marshal_one` are duplicated between
`sdk_ops.rs` and `sdk_data.rs` / `sdk_memory.rs`.

### How errors are thrown today

All SDK functions use one throw helper:

```rust
// errors.rs
pub fn throw_sdk_error<'cx, T>(cx: &mut impl Context<'cx>, err: SdkError) -> NeonResult<T>
```

This creates a plain JS `Error` and attaches a `code` string property. The thrown object in
Node looks like:

```js
{ message: "validation error: …", code: "VALIDATION_ERROR" }
```

`SdkError` variants and their `code` strings (stable API surface):

| Variant | `code` |
|---|---|
| `Component(_)` | `"COMPONENT_ERROR"` |
| `ServiceBuild(_)` | `"SERVICE_BUILD_ERROR"` |
| `UserBootstrap(_)` | `"USER_BOOTSTRAP_ERROR"` |
| `Runtime(_)` | `"RUNTIME_ERROR"` |
| `Validation(_)` | `"VALIDATION_ERROR"` |
| `Unsupported(_)` | `"UNSUPPORTED"` |
| `FeatureNotBuilt(_)` | `"FEATURE_NOT_BUILT"` |

Config errors (thrown by `config.rs`'s `throw_config_error`) follow the same pattern but are
NOT routed through `SdkError`:

| `ConfigError` variant | `code` |
|---|---|
| `UnknownKey(_)` | `"UNKNOWN_CONFIG_KEY"` |
| `TypeMismatch { .. }` | `"CONFIG_TYPE_MISMATCH"` |

The legacy engine errors (`error.rs`) use `throw_execution_error` / `throw_core_error` with
their own `code` strings — those are out of scope for this phase (covered by `error.rs`).

### What the `kind` field means

**The existing thrown errors carry only `code`, not `kind`.** The plan's requirement for a
`kind` field is new work. Implementation choices:

1. Add a `kind` property alongside `code` — set it to the same value as `code` but document it
   as the stable API identifier (treat `code` as the legacy alias). This is the least-disruptive
   path and avoids breaking existing tests that assert `code`.
2. Alternatively, use `kind` as the canonical field and keep `code` as an alias pointing to the
   same string.

**Recommendation:** use option 1 — both `kind` and `code` carry the same string value. Existing
tests (`config.test.ts`) assert `code`; new `errors.test.ts` can assert `kind`. No breakage.

### What "typed JS error subclasses" means with Neon 1.1

Neon 1.x `JsError` can construct `Error`, `TypeError`, and `RangeError` native instances — it
**cannot** construct arbitrary Error subclass instances (e.g. `CogneeValidationError`). Neon has
no `napi_define_class` / `napi_call_function` path that would let Rust call a JS constructor
defined in TS.

**Consequence:** the TS layer must reconstruct the subclass. The pattern is:

1. Rust throws a plain `Error` with `code` and `kind` properties.
2. `js/src/errors.ts` exports `CogneeError` and subclasses that extend `Error`.
3. A wrapper function in the TS layer (or an `errors.ts` utility) re-wraps the thrown native
   error: `catch(e) { throw wrapNativeError(e); }`.
4. `wrapNativeError` reads `e.kind` (falling back to `e.code`) and constructs the correct
   subclass instance, copying `message`, `stack`, `kind`, and `code`.

This means **Tier-A tests that assert typed subclasses must go through the TS wrapper, not
directly through `native.*` calls.** Tests calling `native.*` directly will receive a plain
`Error`; only tests going through the TS `Cognee` class (Phase 7) will receive subclasses.

The `errors.test.ts` Tier-A test can therefore be structured two ways:
- Assert `code`/`kind` on the raw native error (no TS wrapper needed, simpler, works today).
- Assert `instanceof CogneeValidationError` on the wrapped error (requires Phase 7 TS class).

**Recommended approach for Tier-A CI:** assert `code`/`kind` on the raw thrown error from
`native.*` calls. The instanceof check belongs in Phase 7 or Phase 9 Tier-B.

## Scope

- **In:** the serde ↔ JS conversion helpers, the error-mapping layer, the `kind` field addition,
  the TS error classes and re-wrapping utility, and the marshalling conventions (enums, UUIDs,
  dates).
- **Out:** the operations themselves (they call into these helpers).

## Structures

### `js/cognee-neon/src/json.rs` — the single marshalling path (new file)

Extract the duplicated helpers into one canonical module:

- `pub fn js_to_serde<'cx>(cx: &mut FunctionContext<'cx>, val: Handle<'cx, JsValue>) -> NeonResult<serde_json::Value>`
  — round-trip via `JSON.stringify` (identical to existing `js_to_value`, renamed for clarity).
- `pub fn serde_to_js<'cx, C: Context<'cx>>(cx: &mut C, v: &serde_json::Value) -> JsResult<'cx, JsValue>`
  — round-trip via `JSON.parse` (identical to existing `parse_js`, renamed for clarity).
- `pub fn read_opts<'cx>(cx: &mut FunctionContext<'cx>, idx: usize) -> NeonResult<serde_json::Value>`
  — read optional JS argument (identical to existing `sdk_memory::read_opts`).

**Note on `serde_to_js` approach:** The existing `parse_js` uses `JSON.parse(JSON.stringify(v))`
round-tripped through a Rust string. This is correct and simpler than a full recursive Neon
value-tree construction (which would require calling into the JS engine for each node). Keep
the string round-trip approach; rename the function and centralize it.

**Migration:** replace all private copies in `sdk_ops.rs`, `sdk_retrieval.rs`,
`sdk_visualization.rs`, `sdk_cloud.rs`, and `config.rs` with `use crate::json::*`. The
`sdk_data.rs`, `sdk_datasets.rs`, `sdk_admin.rs` re-exports from `sdk_memory` become
re-exports from `json`. After migration, remove `pub(crate)` helpers from `sdk_memory.rs`.

Also extract shared result-building helpers:
- `pub fn cognify_result_json(result: &CognifyResult) -> serde_json::Value` — currently
  duplicated between `sdk_ops.rs` and `sdk_data.rs`.
- `pub fn marshal_inputs(value: &serde_json::Value) -> Result<Vec<DataInput>, SdkError>` —
  currently triplicated across `sdk_ops.rs`, `sdk_memory.rs`, `sdk_data.rs`.

These can live in a new `sdk_helpers.rs` (alongside `json.rs`) or be kept in `json.rs` if the
file stays focused; the choice is implementation discretion.

### `js/cognee-neon/src/errors.rs` — add `kind` field

Current state: `throw_sdk_error` attaches only `code`. Required change: also attach `kind` with
the same value. Extend `throw_sdk_error` and `throw_config_error` in `config.rs`:

```rust
js_err_obj.set(cx, "code", code_val)?;
let kind_val = cx.string(code); // same string as code
js_err_obj.set(cx, "kind", kind_val)?;
```

No new error variants are needed for Phase 8 itself. The plan's `ApiError` / `throw_api_error`
hierarchy is **not required** — the existing `SdkError` variants already cover the needed
discriminants. Do not add new error types unless a concrete gap is found.

### TS error classes (`js/src/errors.ts`) — new file

```typescript
export class CogneeError extends Error {
  readonly kind: string;
  readonly code: string;
  constructor(message: string, kind: string) {
    super(message);
    this.name = "CogneeError";
    this.kind = kind;
    this.code = kind; // alias
  }
}
export class ConfigError extends CogneeError { constructor(msg: string) { super(msg, "CONFIG_TYPE_MISMATCH"); this.name = "ConfigError"; } }
export class UnknownConfigKeyError extends CogneeError { constructor(msg: string) { super(msg, "UNKNOWN_CONFIG_KEY"); this.name = "UnknownConfigKeyError"; } }
export class ComponentError extends CogneeError { constructor(msg: string) { super(msg, "COMPONENT_ERROR"); this.name = "ComponentError"; } }
export class ServiceBuildError extends CogneeError { constructor(msg: string) { super(msg, "SERVICE_BUILD_ERROR"); this.name = "ServiceBuildError"; } }
export class ValidationError extends CogneeError { constructor(msg: string) { super(msg, "VALIDATION_ERROR"); this.name = "ValidationError"; } }
export class RuntimeError extends CogneeError { constructor(msg: string) { super(msg, "RUNTIME_ERROR"); this.name = "RuntimeError"; } }
export class UnsupportedError extends CogneeError { constructor(msg: string) { super(msg, "UNSUPPORTED"); this.name = "UnsupportedError"; } }
export class FeatureNotBuiltError extends CogneeError { constructor(msg: string) { super(msg, "FEATURE_NOT_BUILT"); this.name = "FeatureNotBuiltError"; } }

const KIND_TO_CLASS: Record<string, new (msg: string) => CogneeError> = {
  CONFIG_TYPE_MISMATCH: ConfigError,
  UNKNOWN_CONFIG_KEY: UnknownConfigKeyError,
  COMPONENT_ERROR: ComponentError,
  SERVICE_BUILD_ERROR: ServiceBuildError,
  VALIDATION_ERROR: ValidationError,
  RUNTIME_ERROR: RuntimeError,
  UNSUPPORTED: UnsupportedError,
  FEATURE_NOT_BUILT: FeatureNotBuiltError,
};

/** Re-wrap a native thrown error into the correct CogneeError subclass. */
export function wrapNativeError(e: unknown): CogneeError {
  if (e instanceof CogneeError) return e;
  if (e && typeof e === "object" && ("kind" in e || "code" in e)) {
    const kind = (e as { kind?: string; code?: string }).kind ?? (e as { code?: string }).code ?? "RUNTIME_ERROR";
    const msg = (e as { message?: string }).message ?? String(e);
    const Cls = KIND_TO_CLASS[kind] ?? CogneeError;
    const wrapped = new Cls(msg);
    // Preserve original stack if available.
    if ((e as { stack?: string }).stack) wrapped.stack = (e as { stack: string }).stack;
    return wrapped;
  }
  return new CogneeError(String(e), "RUNTIME_ERROR");
}
```

Export all classes from `js/src/index.ts`.

## Marshalling conventions (documented + enforced by tests)

These are already correctly implemented in the existing code. Phase 8 documents them:

- **Enums** → strings matching the serde rename (`SearchType` SCREAMING_SNAKE_CASE,
  `RememberStatus` PascalCase aliases). Single source of truth = the Rust `#[serde]` attributes.
- **UUIDs** → lowercase strings (Rust `Uuid` serializes to lowercase by default with serde).
- **Timestamps** → ISO-8601 strings (via `chrono`'s serde feature).
- **Bytes** → base64 string (for `DataInput::Binary`); plain number arrays also accepted as
  input (see `marshal_bytes` in `sdk_ops.rs`).

## Functionalities

- Uniform: any thrown SDK error carries `code` and `kind` (same string; `kind` is the stable
  API identifier, `code` is the alias for backwards compatibility).
- Uniform: all JS ↔ serde conversion goes through `json.rs`; no private copies remain in
  individual modules.
- TS: `CogneeError` and subclasses in `js/src/errors.ts`; `wrapNativeError` for use in the
  Phase-7 `Cognee` class methods.
- The Phase-7 `Cognee` class wraps each `native.*` call in a try/catch and re-throws via
  `wrapNativeError` (responsibility of Phase 7, not Phase 8).

## Dependencies & ordering

Cross-cutting. Minimal stub was provided in Phase 1 (`throw_sdk_error` with `code` only).
Phase 8 adds `kind`, centralizes `json.rs`, and adds the TS error classes. The Phase-7 TS
class applies `wrapNativeError`. Phase 9 adds the `errors.test.ts` Tier-A test.

## Risks

- **`kind` vs `code` field name** — existing tests (`config.test.ts`) assert `code`. Adding
  `kind` as an additional field (same value) is fully backwards-compatible; do not rename `code`.
- **Serde rename drift** → enum string mismatch; lock with assertions (shared with Phase 4).
- **`kind` stability** — treat `kind` strings as API; document them (table above is the source
  of truth).
- **`wrapNativeError` coverage** — must be applied at every call site in Phase 7; easy to miss.
  Enforce by convention (all `Cognee` methods must wrap) and by Tier-A testing.

## Done when

- `json.rs` exists with `js_to_serde`, `serde_to_js`, `read_opts`; all duplicate copies
  removed from `sdk_*` modules (verified by `grep` in CI or by compiler — no unused imports).
- `throw_sdk_error` and `throw_config_error` both set `kind` alongside `code`.
- `js/src/errors.ts` exports `CogneeError`, all subclasses, and `wrapNativeError`.
- `js/src/index.ts` re-exports the error classes.
- A Tier-A `errors.test.ts` calls `native.configSet(handle, "nonexistent_key", "x")` (known to
  throw `UNKNOWN_CONFIG_KEY`) and `native.configSet(handle, "chunk_size", "bad")` (throws
  `CONFIG_TYPE_MISMATCH`) and asserts both `code` and `kind` on the thrown error.
