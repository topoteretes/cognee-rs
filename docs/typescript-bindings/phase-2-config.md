# Phase 2 — Config surface

← [Index](../typescript-bindings-plan.md)

**Goal:** make every `Settings` field reachable from TypeScript so real backends (LLM,
embedding, vector, graph, relational, storage, chunking, ontology, sessions) can be selected
without environment-variable gymnastics. Unblocks real-backend tests for Phase 3.

## Scope

- **In:**
  - **`crates/lib/src/config.rs` (shared crate) — Option B widening:** add the missing granular
    setters and extend the generic `set(key, value)` dispatch + the bulk setters so the keys that
    currently lack coverage become settable (enumerated under Structures → "Config-surface widening
    (Option B)"). Each new/extended setter **must call `bump_version()`** so `CogneeServices`
    rebuild-on-change keeps working.
  - **`js/cognee-neon` binding:** `ConfigManager` setters (granular + bulk + generic) exposed to TS,
    `Settings` construction from a JS object and from env (with the Option B object > env > defaults
    overlay), config read-back, version-bump semantics.
- **Out:** the operations that consume config (Phases 3–6).

> **Cross-crate note:** Because Option B edits the shared `crates/lib/src/config.rs`, this phase's
> verification is **two-pronged**: the workspace commands `cargo check --all-targets` and
> `cargo test` (covering the `config.rs` changes, including the existing unit tests in that file)
> **in addition to** `bash js/scripts/check.sh` for the binding. Both must pass.

## Structures

### `js/cognee-neon/src/config.rs`
Native functions that take the `CogneeHandle` `JsBox` and forward to
`handle.state.cm.config().set_*`. **Note the access path:** Phase 1 boxed the handle as
`CogneeHandle { state: Arc<HandleState> }` and `cm: Arc<ComponentManager>` lives on
`HandleState`, so the `ConfigManager` is reached via `handle.state.cm.config()` (NOT
`handle.cm`). `ComponentManager::config()` returns `&ConfigManager`.

Each setter **bumps the config version** (verified: every `ConfigManager` setter and bulk
setter calls `bump_version()`), which causes `HandleState::services()` — keyed on
`self.cm.config().version()` — to rebuild on the next op (Phase 1). That is the propagation
mechanism; no manual re-wiring.

**Granular setters that EXIST in `ConfigManager` today** (expose exactly these; verified
against `crates/lib/src/config.rs`):
- LLM: `set_llm_provider`, `set_llm_model`, `set_llm_api_key`, `set_llm_endpoint`.
- Embedding: `set_embedding_provider`, `set_embedding_model`, `set_embedding_dimensions`,
  `set_embedding_endpoint`, `set_embedding_api_key`.
- Vector DB: `set_vector_db_provider`, `set_vector_db_url`, `set_vector_db_key`.
- Graph DB: `set_graph_database_provider`, `set_graph_model`.
- Chunking: `set_chunk_strategy`, `set_chunk_engine`, `set_chunk_size`, `set_chunk_overlap`.
- Paths: `set_system_root_directory` (cascades `graph_file_path` + `vector_db_url`),
  `set_data_root_directory`.
- Other: `set_monitoring_tool`, `set_classification_model`, `set_summarization_model`.

**Config-surface widening (Option B — DECIDED).** The original plan listed setters/keys that do
NOT exist in `ConfigManager` today. The decision is to **extend `crates/lib/src/config.rs`** so
they all exist, then expose them in the binding. Concretely:

1. **ADD these granular setters** to `impl ConfigManager` (mirror the existing setter shape:
   acquire `self.inner.write()`, mutate the field, `drop(s)`, then `self.bump_version()`; use the
   established `set_<area>_<field>` naming and `&str`/`u32`/`f64`/`bool` arg types matching the
   `Settings` field type):
   - **LLM tuning:** `set_llm_api_version(&str)` → `llm_api_version`; `set_llm_temperature(f64)` →
     `llm_temperature`; `set_llm_streaming(bool)` → `llm_streaming`;
     `set_llm_max_completion_tokens(u32)` → `llm_max_completion_tokens`;
     `set_llm_max_retries(u32)` → `llm_max_retries`;
     `set_llm_max_parallel_requests(u32)` → `llm_max_parallel_requests`.
   - **Embedding paths:** `set_embedding_model_path(&str)` → `embedding_model_path`;
     `set_embedding_tokenizer_path(&str)` → `embedding_tokenizer_path`.
   - **Vector DB endpoint parts:** `set_vector_db_host(&str)` → `vector_db_host`;
     `set_vector_db_port(u16)` → `vector_db_port`; `set_vector_db_name(&str)` → `vector_db_name`.
   - **Graph DB granular path:** `set_graph_file_path(&str)` → `graph_file_path` (a granular
     counterpart to the `set_graph_db_config` bulk path; do NOT cascade — unlike
     `set_system_root_directory`, this is a direct field write).
   - **Paths:** `set_cache_root_directory(&str)` → `cache_root_directory`;
     `set_logs_root_directory(&str)` → `logs_root_directory`.
   - **Ontology:** `set_ontology_file_path(&str)` → `ontology_file_path`;
     `set_ontology_resolver(&str)` → `ontology_resolver`;
     `set_ontology_matching_strategy(&str)` → `ontology_matching_strategy`.
   - Every one of these **MUST `bump_version()`** (the propagation contract).

2. **WIDEN the generic `set(key, value)` dispatch** (`config.rs::set`) to route these additional
   keys to the new setters, reusing the `as_string` / `as_u32` / `as_f64` / `as_bool` helpers
   (add an `as_bool` and an `as_u16` helper if not already present, following the `as_u32`
   pattern). New keys to handle: `llm_api_version`, `llm_temperature`, `llm_streaming`,
   `llm_max_completion_tokens`, `llm_max_retries`, `llm_max_parallel_requests`,
   `embedding_model_path`, `embedding_tokenizer_path`, `vector_db_host`, `vector_db_port`,
   `vector_db_name`, `graph_file_path`, `cache_root_directory`, `logs_root_directory`,
   `ontology_file_path`, `ontology_resolver`, `ontology_matching_strategy`. Keys not in the union
   still return `ConfigError::UnknownKey`.

3. **WIDEN the relevant bulk setters** so each area's allowlist covers its newly-granular fields:
   - `set_llm_config`: add `llm_streaming`, `llm_max_retries`, `llm_max_parallel_requests`
     (already accepts provider/model/api_key/endpoint/api_version/temperature/
     max_completion_tokens).
   - `set_embedding_config`: add `embedding_model_path`, `embedding_tokenizer_path`.
   - `set_vector_db_config`: add `vector_db_host`, `vector_db_port`, `vector_db_name`.
   - `set_graph_db_config`: already accepts `graph_file_path`; no change needed there.
   - There is **no** ontology bulk setter today and Option B does not add one — ontology fields are
     reachable via the new granular setters and the widened `set`. (If a `set_ontology_config`
     bulk setter is desired for symmetry it is optional, not required by this phase.)
   - Each widened bulk setter keeps its single `bump_version()` at the end.

4. Keep all additions **parity-aligned** with the CLI / Python surface and the existing naming
   conventions already in `config.rs`. Do not rename or change existing setters.

After this widening, the binding exposes the full granular + bulk + generic surface (no field is
"unreachable"). The list of "setters that EXIST today" above is the pre-widening baseline; treat
the union of that list plus the items added in (1) as the granular surface to expose in the
binding.

**Bulk setters** (exist exactly as named; each takes `&HashMap<String, serde_json::Value>` and
returns `Result<(), ConfigError>`): `set_llm_config`, `set_embedding_config`,
`set_vector_db_config`, `set_graph_db_config`. Expose as `setLlmConfig(obj)` /
`setEmbeddingConfig(obj)` / `setVectorDbConfig(obj)` / `setGraphDbConfig(obj)`; marshal the JS
object → `HashMap<String, serde_json::Value>` and pass by reference. **Each bulk setter accepts
only a restricted key subset** (the post-widening subsets from "Config-surface widening" item 3)
and returns `UnknownKey` for anything outside it. After widening: `set_llm_config` accepts
provider/model/api_key/endpoint/api_version/temperature/max_completion_tokens/streaming/
max_retries/max_parallel_requests; `set_embedding_config` accepts provider/model(_name)/
dimensions/endpoint/api_key/model_path/tokenizer_path; `set_vector_db_config` accepts
provider/url/key/host/port/name; `set_graph_db_config` accepts provider/graph_model/
graph_file_path.

**Generic setter:** `set(key, value)` — ALREADY EXISTS on `ConfigManager`
(`fn set(&self, key: &str, value: serde_json::Value) -> Result<(), ConfigError>`) and
dispatches by key, surfacing `ConfigError` (`UnknownKey` / `TypeMismatch`). The binding just
forwards to it; no key mapping needs to be (re)built in the binding. **`ConfigError` is
re-exported from `cognee_lib`** (`cognee_lib::config::ConfigError`). Map it to a typed JS error
(full marshalling is Phase 8); a minimal throw with a `code` field (mirroring `errors.rs`) is
acceptable now. The set of keys `set` accepts is the **widened** subset in `config.rs::set` (per
"Config-surface widening" item 2) — it now DOES cover ontology fields, `cache_root_directory` /
`logs_root_directory`, vector `host`/`port`/`name`, the LLM tuning fields, embedding
`model_path`/`tokenizer_path`, and granular `graph_file_path`. Anything outside the union still
returns `UnknownKey`.

### Settings construction (Q2 — object > env > defaults overlay, DECIDED)
- `cogneeNew(settingsJson?)` accepts a JS object **or a JSON string** whose keys are the
  **`Settings` field names** (the struct is `#[serde(default)]`). The serde field names
  (e.g. `embedding_model_name`, `graph_database_provider`) are the authoritative object keys — they
  match the Python env-var names only loosely.
- **Precedence is a true 3-way overlay: `defaults < env < object`.** Phase 2 changes
  `cogneeNew` (in `js/cognee-neon/src/sdk.rs` — a Phase-1 file, edited here because Phase 2 owns the
  config-construction contract) so a **partial object composes on top of the env-derived
  `Settings`** instead of overlaying object-over-defaults only:
  - with no / `null` / `undefined` argument → `ConfigManager::from_env()` (env over defaults), as
    today.
  - with an argument → **start from the env-derived `Settings`** (`ConfigManager::from_env()` →
    `Settings`, i.e. defaults + env overlay), then **apply only the keys the object actually
    provides** on top. Fields ABSENT from the object keep their env (or default) value; fields
    PRESENT in the object win.
- **Implementation requirement (correct partial overlay).** `serde(default)` deserialization of a
  partial object cannot distinguish "absent" from "explicitly equal to the default," so a plain
  `from_str::<Settings>` over an env-derived base would silently reset absent fields to defaults.
  Implement the overlay at the JSON-`Value` level:
  1. Parse the argument (object or JSON string) into a `serde_json::Value` (must be a JSON
     **object**; reject otherwise with a clear error). For the object case, reuse the existing
     `stringify_js` round-trip, then `serde_json::from_str::<serde_json::Value>`.
  2. Serialize the env-derived `Settings` to a `serde_json::Value` (a JSON object of all fields):
     `serde_json::to_value(&base_settings)`.
  3. **Merge the provided keys onto the base object** — iterate the argument object's entries and
     overwrite the matching base keys (shallow, per top-level field; `Settings` has no nested
     objects, so a flat key-overwrite is sufficient and correct).
  4. `serde_json::from_value::<Settings>(merged)` to get the final `Settings`. Surface a clear
     `invalid settings` error on failure (keep the existing `cx.throw_error` style).
  - Net effect: env vars and a partial object **compose**; the object only overrides the keys it
    names. Document this `defaults < env < object` precedence in the doc-comment on `cognee_new`
    and the `native.ts` JSDoc for `cogneeNew`.
- Phase 2 adds no new construction entry point; it only changes `cogneeNew`'s overlay semantics and
  adds the post-construction setters.

## Functionalities

- `getConfig(handle) -> object` — return a JSON snapshot of the current `Settings`
  (`serde_json::to_value(&*handle.state.cm.settings())`) for inspection. **Do NOT rely on the
  `redact` util for this:** `cognee_utils::redact` only matches secret-*shaped* substrings
  (`sk-…`, `api_key=…`, `Bearer …`, `password=…`); a plain field value such as
  `"llm_api_key": "abc123"` is NOT caught, and `redact` is not re-exported by `cognee_lib`
  (would require adding a `cognee-utils` dep). Instead, **explicitly blank/omit the known secret
  fields** before serializing: `llm_api_key`, `embedding_api_key`, `vector_db_key`,
  `vector_db_password`, `graph_database_key`, `graph_database_password`, `db_password`,
  `cache_password`, `default_user_password`, `otel_exporter_otlp_headers`. (Replace with `""` or `"***REDACTED***"`,
  or drop the keys.) This is a deterministic, testable rule.
- Each setter returns `void` and is synchronous (config mutation is cheap; no async needed).
  Generic `set` and the bulk setters return `void` on success and **throw** the mapped
  `ConfigError` (`UnknownKey` / `TypeMismatch`) — they are fallible.
- Key-naming convention: prefer the canonical `Settings`/env names to match the Python SDK; the
  TS ergonomics layer (Phase 7) can also offer camelCase method names.

## Marshalling

Values cross as `serde_json::Value`; reuse the Phase 8 `json.rs` helpers (a minimal inline
version is acceptable until Phase 8 lands). A reusable JS-object→`HashMap<String,
serde_json::Value>` conversion is needed for the bulk setters and a JS-value→`serde_json::Value`
for the generic `set`; the existing `JSON.stringify` round-trip used by `cogneeNew`
(`sdk.rs::stringify_js`) is a fine starting point if `json.rs` is not yet present.

## Native export registration

New native functions must be registered in `js/cognee-neon/src/lib.rs` (`cx.export_function(...)`)
and declared in `js/src/native.ts`'s `NativeBindings` interface, alongside the Phase-1 entries
(`cogneeNew`/`cogneeWarm`/`cogneeOwnerId`). The `config.rs` module must be added with
`mod config;` in `lib.rs`.

## Dependencies & ordering

Needs Phase 1 (handle). Unblocks Phase 3 real-backend paths and the Tier-A config tests.

Because Option B edits the shared `crates/lib/src/config.rs`, verification spans both the workspace
(`cargo check --all-targets`, `cargo test`) and the binding (`bash js/scripts/check.sh`). Land the
`config.rs` widening first (it is self-contained and workspace-verifiable on its own), then the
binding exposure that depends on it.

## Risks

- Secret handling — never echo api keys back through `getConfig`. The `redact` util is
  insufficient here (see Functionalities); use the explicit secret-field blanklist instead.
- Key-name drift between the binding and `Settings` — do NOT centralize a new mapping in the
  binding; forward to the existing `ConfigManager::set` / bulk setters and test against the real
  `ConfigError`.
- Bulk-setter key subsets — each bulk setter rejects keys outside its (post-widening) allowlist; a
  test feeding a genuinely out-of-subset key — e.g. a vector key like `vector_db_url` to
  `set_llm_config`, or `chunk_size` to any of them — should expect `UnknownKey`, not success. (Do
  NOT use `llm_streaming` as the negative example: Option B adds it to `set_llm_config`'s
  allowlist, so it now succeeds.)
- Shared-crate change risk (Option B) — editing `crates/lib/src/config.rs` touches a crate every
  other crate depends on. Keep the additions purely additive (new setters + widened match arms),
  do not alter existing setter behavior, and run `cargo check --all-targets` + `cargo test` to
  confirm no workspace regression alongside `bash js/scripts/check.sh`.
- "Observable rebuild" test must avoid a live network/model build — `services()` builds real
  engines (ONNX embedding, qdrant, ladybug, SeaORM). For the rebuild-on-change Tier-A test prefer
  asserting `cm.config().version()` advanced and that a second `services()`/warm rebuilds, rather
  than booting a heavy backend; or set `MOCK_EMBEDDING=true` + sqlite in-memory + a temp dir.
  Changing `vector_db_url`/`system_root_directory` is observable but only after a successful warm.

## Done when

- **Shared crate (`crates/lib/src/config.rs`) — Option B widening landed:** the granular setters
  in "Config-surface widening" item 1 exist (each bumps the version), the generic `set` dispatch
  (item 2) and bulk setters (item 3) are widened, and the file's existing unit tests plus any new
  coverage pass. Verified by `cargo check --all-targets` and `cargo test` on the workspace.
- A Tier-A `config.test.ts` covers: a representative sample of granular setters across each area
  **including the newly-added Option B ones** (e.g. an LLM tuning field, an ontology field, a
  vector host/port, a path field), one bulk setter (incl. an out-of-subset `UnknownKey` case), the
  generic `set(key, value)` (incl. a newly-covered key succeeding AND an `UnknownKey` error from a
  key like `"nonexistent_key"`), `getConfig` returning a snapshot with secret fields blanked,
  Settings-from-object and from-env via `cogneeNew`, **the `defaults < env < object` overlay**
  (set an env var, pass a partial object that omits it, assert the env value survives while an
  object-provided field wins), and that a setter bump causes a `services()` rebuild (observable via
  `version()` advance, or a changed engine path under a mock-friendly config). LLM-gated paths must
  remain skippable without `OPENAI_*`/model env (no embedding/LLM I/O in the Tier-A config tests).
- **Both verification gates green:** `cargo check --all-targets` + `cargo test` (for the shared
  `config.rs` changes) AND `bash js/scripts/check.sh` (for the binding).
