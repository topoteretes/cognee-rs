# Phase 3 — Config surface

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** all `Settings` fields settable from C; real backends selectable; services rebuild
on change. Reference: `js/cognee-neon/src/config.rs` + `crates/lib/src/config.rs`
(`ConfigManager` — already shared, extended with granular setters during TS Phase 2).

## Prerequisites

Phases 1–2.

## Design (decision D7)

The TS binding exposes 39 granular setters because JS ergonomics want named methods. C does
not: `ConfigManager::set(key, &serde_json::Value)` already dispatches every key with
type-checking. The C surface is therefore **generic + bulk**, not 39 extern functions:

| Function | Signature | Wraps |
|---|---|---|
| `cg_sdk_config_set` | `CgErrorCode(const CgSdk*, const char* key, const char* value_json)` | `ConfigManager::set` — `value_json` is any JSON value (`"\"openai\""`, `"0.7"`, `"true"`) |
| `cg_sdk_config_set_str` | `CgErrorCode(const CgSdk*, const char* key, const char* value)` | convenience: wraps the plain C string as a JSON string before dispatch (covers ~80 % of keys without JSON-escaping pain) |
| `cg_sdk_config_set_llm_config` | `CgErrorCode(const CgSdk*, const char* json)` | `set_llm_config` bulk setter |
| `cg_sdk_config_set_embedding_config` | `CgErrorCode(const CgSdk*, const char* json)` | bulk |
| `cg_sdk_config_set_vector_db_config` | `CgErrorCode(const CgSdk*, const char* json)` | bulk |
| `cg_sdk_config_set_graph_db_config` | `CgErrorCode(const CgSdk*, const char* json)` | bulk |
| `cg_sdk_config_get` | `CgErrorCode(const CgSdk*, char** out_json)` | `getConfig` read-back (redacted secrets if `ConfigManager` redacts; match TS behavior exactly) |

All sync (config mutation does no I/O). Key names = the same `Settings` field names TS uses
(`llm_provider`, `embedding_model`, …; confirm casing against `config.rs` — TS `configSet`
uses the Rust field names, not camelCase).

Errors: unknown key → `CG_ERR_UNKNOWN_CONFIG_KEY`; wrong JSON type for the key →
`CG_ERR_CONFIG_TYPE_MISMATCH`; malformed JSON → `CG_ERR_VALIDATION`.

If a consumer later wants typed setters (e.g. for IDE discoverability in C), they are
mechanical one-line wrappers — explicitly deferred, tracked as a non-goal for parity since
the capability is 100 % covered.

## Tasks

1. `capi/cognee-capi/src/sdk_config.rs` with the 7 functions above:
   - Access config via `state.cm.config()` (the `ConfigManager`) — the full call chain is
     `CgSdk.state: Arc<HandleState>` → `HandleState.cm: Arc<ComponentManager>` →
     `ComponentManager::config() -> &ConfigManager`.
   - All 7 functions are **synchronous** (config mutation is in-memory only) — do NOT use
     `spawn_sdk_op` or the async callback pattern for these; return `CgErrorCode` directly.
   - Mirror `js/cognee-neon/src/config.rs` for the logic, but translate JS throws into
     direct `CgErrorCode` returns (and `set_last_error()` for the human-readable message).
   - `ConfigError` from `cognee_lib::config::ConfigError` is NOT a variant of `SdkError`
     and has no entry in `From<&SdkError> for CgErrorCode`. Map it directly:
     - `ConfigError::UnknownKey(_)` → return `CgErrorCode::UnknownConfigKey` (17)
     - `ConfigError::TypeMismatch { .. }` → return `CgErrorCode::ConfigTypeMismatch` (18)
   - For `cg_sdk_config_get`: call `state.cm.settings()` (returns
     `RwLockReadGuard<'_, Settings>`), serialize with `serde_json::to_value`, blank the
     known secret fields in-place before serializing to a C string, then return the JSON
     via the `out_json` out-parameter. Copy the `SECRET_FIELDS` list from
     `js/cognee-neon/src/config.rs` (hardcoded list, NOT `ConfigManager` redaction).
   - As part of this task, replace the `apply_settings_json_patch` macro-based stub in
     `capi/cognee-capi/src/sdk.rs` (lines marked "Phase 3 will replace this") with a call
     to `cm.config().set(key, value)` for each field from the JSON patch; this aligns
     `cg_sdk_new`'s JSON overlay with the generic-setter semantics.
2. Verify the **version-bump → services-rebuild** path from C: warm the handle, call
   `cg_sdk_config_set` to change a setting (e.g. `system_root_directory` pointing to a
   second temp dir), then verify the next `cg_sdk_warm` (or any service-requiring op)
   rebuilds services. The rebuild is observable because `HandleState::services()` compares
   `cm.config().version()` against the cached version; any `set_*` call increments the
   version, invalidating the cache.
3. Header regeneration + doc comments listing the valid keys (point to `Settings` docs rather
   than duplicating the full list).
4. Smoke test `capi/examples/sdk_config_smoke.c`: set/get round-trip, unknown key, type
   mismatch, bulk setter, rebuild-on-change assertion.

## Exit criteria

- [ ] all `Settings` keys settable + readable from C (spot-check one per group: llm,
      embedding, vector, graph, chunking, paths, ontology, misc)
- [ ] error codes verified for unknown key / type mismatch / bad JSON
- [ ] rebuild-on-change asserted in the smoke test
- [ ] `cognee_sdk.h` regenerated; check.sh runs the new smoke test
