# Phase 8 — Errors & marshalling

← [Index](../typescript-bindings-plan.md)

**Goal:** one consistent JSON marshalling path and a typed JS error hierarchy, so failures are
inspectable and every value crosses the boundary the same way. Design early; finalize once the
ops exist.

## Scope

- **In:** the serde ↔ JS conversion helpers, the error-mapping layer, the TS error classes, and
  the marshalling conventions (enums, UUIDs, dates).
- **Out:** the operations themselves (they call into these helpers).

## Structures

### `js/cognee-neon/src/json.rs` — the single marshalling path
- `js_to_serde(cx, handle) -> NeonResult<serde_json::Value>` — recursive: null/undefined → Null,
  boolean → Bool, number → Number, string → String, Array → Array, Object → Map. Buffers →
  base64 string or `{ $bytes }` convention (documented).
- `serde_to_js(cx, &serde_json::Value) -> JsResult<JsValue>` — the inverse.
- All `sdk_*` functions use these; remove any JSON-string `JSON.parse` shortcuts taken in earlier
  phases.

### `js/cognee-neon/src/errors.rs` — error mapping
- Map each library error enum to a JS error carrying a stable `code`/`kind` and message:
  `ApiError` (Delete / Ingestion / Cognify / Search / Session / Storage / Graph / Vector /
  Memify / Improve / InvalidArgument / Join), `ComponentError`, `ConfigError`
  (`UnknownKey` / `TypeMismatch`), `DatasetError`.
- Helper `throw_api_error(cx, e)` analogous to the existing `throw_execution_error` for the
  engine. Mirror the existing `PipelineError` hierarchy style from `error.rs`.

### TS error classes (`js/src/errors.ts`)
- `CogneeError` (base) with `code`/`kind`; subclasses `ConfigError`, `ComponentError`,
  `IngestionError`, `CognifyError`, `SearchError`, `SessionError`, `FeatureNotBuiltError`.
- Construct the right subclass from the native error's `kind` field.

## Marshalling conventions (documented + enforced by tests)

- **Enums** → strings matching the serde rename (e.g. `SearchType` SCREAMING_SNAKE_CASE,
  `RememberStatus` PascalCase aliases). Single source of truth = the Rust serde attributes.
- **UUIDs** → lowercase strings.
- **Timestamps** → ISO-8601 strings.
- **Bytes** → documented convention (base64).

## Functionalities

- Uniform: any thrown error in Node is a `CogneeError` subclass with a machine-readable `kind`.
- Uniform: any value in/out goes through `json.rs` — no bespoke per-function conversion.

## Dependencies & ordering

Cross-cutting. Provide a minimal inline version from Phase 1; harden and centralize here once all
error sources exist (after Phase 5/6).

## Risks

- Serde rename drift → enum string mismatch; lock with assertions (shared with Phase 4).
- Error `kind` stability — treat `kind` strings as API; document them.

## Done when

- Every value crosses via `json.rs`; no `JSON.parse` shortcuts remain.
- A Tier-A `errors.test.ts` triggers config/component failures and asserts the correct typed
  subclass + `kind`.
