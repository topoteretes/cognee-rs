# Phase 2 — Config surface

← [Index](../typescript-bindings-plan.md)

**Goal:** make every `Settings` field reachable from TypeScript so real backends (LLM,
embedding, vector, graph, relational, storage, chunking, ontology, sessions) can be selected
without environment-variable gymnastics. Unblocks real-backend tests for Phase 3.

## Scope

- **In:** `ConfigManager` setters (granular + bulk + generic), `Settings` construction from a JS
  object and from env, config read-back, version-bump semantics.
- **Out:** the operations that consume config (Phases 3–6).

## Structures

### `js/cognee-neon/src/config.rs`
Native functions that take the `CogneeHandle` `JsBox` and forward to `handle.cm.config().set_*`.
Each setter **bumps the config version**, which causes `services()` to rebuild on the next op
(Phase 1) — that is the propagation mechanism; no manual re-wiring.

**Granular setters** (mirror `ConfigManager`):
- LLM: provider, model, api_key, endpoint (and api_version / temperature / streaming / max
  tokens / retries as needed).
- Embedding: provider, model, dimensions, endpoint, api_key, model_path, tokenizer_path.
- Vector DB: provider, url, key (+ host/port/name where relevant).
- Graph DB: provider, file_path.
- Chunking: strategy, engine, size, overlap.
- Paths: system_root_directory (cascades graph + vector paths), data_root, cache_root, logs_root.
- Ontology: file_path, resolver, matching_strategy.

**Bulk setters:** `setLlmConfig(obj)`, `setEmbeddingConfig(obj)`, `setVectorDbConfig(obj)`,
`setGraphDbConfig(obj)` — each takes a JS object → `HashMap<String, serde_json::Value>`.

**Generic setter:** `set(key, value)` — dispatches by key; surfaces `ConfigError`
(`UnknownKey` / `TypeMismatch`) as typed JS errors (Phase 8).

### Settings construction
- `cogneeNew(settingsJson?)` (Phase 1) accepts a JS object whose keys are the **`Settings` field
  names / Python env-var names** (parity), merges over `Settings::default()` + `from_env()`.
- Document the precedence: explicit object > env > defaults.

## Functionalities

- `getConfig(handle) -> object` — return a JSON snapshot of the current `Settings` for inspection
  (redact secrets via the existing `redact` util).
- Each setter returns `void` and is synchronous (config mutation is cheap; no async needed).
- Key-naming convention: prefer the canonical `Settings`/env names to match the Python SDK; the
  TS ergonomics layer (Phase 7) can also offer camelCase method names.

## Marshalling

Values cross as `serde_json::Value`; reuse the Phase 8 `json.rs` helpers (a minimal inline
version is acceptable until Phase 8 lands).

## Dependencies & ordering

Needs Phase 1 (handle). Unblocks Phase 3 real-backend paths and the Tier-A config tests.

## Risks

- Secret handling — never echo api keys back through `getConfig`.
- Key-name drift between the binding and `Settings` — centralize the mapping and test the
  generic `set` against the real `ConfigError`.

## Done when

- A Tier-A `config.test.ts` covers: every granular setter, a bulk setter, the generic
  `set(key, value)` (incl. an `UnknownKey` error), Settings-from-object and from-env, and that a
  setter bump causes `services()` to rebuild (observable via a changed engine, e.g. vector-db
  path).
