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

1. `capi/cognee-capi/src/sdk_config.rs` with the 7 functions above, dispatching through
   `state.cm.config()` (the `ConfigManager`), mirroring `js/cognee-neon/src/config.rs`
   including its error mapping.
2. Verify the **version-bump → services-rebuild** path from C: set `MOCK_EMBEDDING`-style
   config, warm, change a setting, confirm next op rebuilds (observable via owner-id
   stability + a debug counter or by switching `system_root_directory` between temp dirs).
3. Header regeneration + doc comments listing the valid keys (point to `Settings` docs rather
   than duplicating the full list).
4. Smoke test `capi/examples/sdk_config_smoke.c`: set/get round-trip, unknown key, type
   mismatch, bulk setter.

## Exit criteria

- [ ] all `Settings` keys settable + readable from C (spot-check one per group: llm,
      embedding, vector, graph, chunking, paths, ontology, misc)
- [ ] error codes verified for unknown key / type mismatch / bad JSON
- [ ] rebuild-on-change asserted in the smoke test
- [ ] `cognee_sdk.h` regenerated; check.sh runs the new smoke test
