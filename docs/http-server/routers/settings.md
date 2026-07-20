# Router: settings

The `/api/v1/settings` router is the LLM/vector-DB settings panel that the Cognee frontend reads
on load and writes when an operator changes a provider, model, endpoint, or API key. Two endpoints
only: `GET` returns the current snapshot together with the list of selectable providers/models,
`POST` partially updates either or both sub-configurations. The endpoint deliberately couples the
LLM and vector-DB configs because the frontend treats them as a single "settings" panel.

Companion docs: [../architecture.md](../architecture.md),
[../auth.md](../auth.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/settings` (Python: [`client.py` L238](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L238)).
- OpenAPI tag: `settings`.
- Router file: `crates/http-server/src/routers/settings.rs`.
- Python source: [`cognee/api/v1/settings/routers/get_settings_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py)
  (104 lines).
- Backing module: [`cognee/modules/settings/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/settings) — `get_settings()`, `save_llm_config()`, `save_vector_db_config()`.

## 2. Endpoints

### 2.1 `GET /` — read current settings

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `SettingsDTO { llm: LLMConfigOutputDTO, vector_db: VectorDBConfigOutputDTO }`. Field-level breakdown in §4.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 401 | `ApiError` (`InvalidCredentials`) | Missing or invalid auth credential. |
  | 500 | `ApiError` (`Internal`) | Failure resolving config from environment / config service. |
- **Side effects**: read-only. Does *not* touch the relational DB. The settings come from
  `LlmConfig` and `VectorDbConfig` in process-local state ([Python source: `get_settings.py` L44-L191](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L44-L191)).
- **Delegation target**: `cognee::settings::get_settings()` (new façade in `cognee`, wrapping
  the existing `cognee_llm::LlmConfig` and `cognee_vector::VectorDbConfig` snapshots).
- **Validation rules**: none.
- **Authorization checks**: authentication only — every authenticated user reads the same global
  settings. Note: this is *server-wide* state in Python; there is no per-tenant override. We
  preserve this in v1 (open question §6.3).
- **OpenAPI**: tag `settings`, response schema `SettingsDTO`.
- **Telemetry**: span `cognee.api.settings.get`. Attrs: `user.id`. Emit `llm.provider`,
  `vector_db.provider` after redaction. **Do not emit `api_key` fields** — they are secrets per
  [../observability.md §5](../observability.md#5-secret-redaction).
- **Python parity notes**:
  - **API-key redaction policy on read**: Python masks the key as `key[0:10] + "*" * (len(key) - 10)` ([`get_settings.py` L94-L96](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L94-L96), [L184-L187](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L184-L187)). Replicate exactly: emit the first 10 chars of the configured key followed by N stars, where N is `len(key) - 10`. If the key is missing/empty, emit `null` for `llm.api_key` (Python's ternary returns `None`); the vector-DB branch in Python crashes when the key is empty, so we must handle the empty case defensively (Python fix candidate — see open question §6.4).
  - **Provider/model lists are server-rendered constants**: Python hard-codes the lists of available providers and per-provider model lists in the response body. We mirror the lists verbatim from [`get_settings.py` L60-L179](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L60-L179) so the Cognee frontend renders identically. **The lists must stay literal-equal to Python.**

### 2.2 `POST /` — save (partial-update) settings

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `SettingsPayloadDTO { llm?: LLMConfigInputDTO, vector_db?: VectorDBConfigInputDTO }`. Both fields optional; only the provided sub-config is saved.
- **Response body**: `200 OK` with **empty body** (Python's handler has no `return` and FastAPI emits `null`/empty depending on `response_model=None`). Match Python: respond with empty `application/json` body (`null`) — Python's [`get_settings_router.py` L74-L102](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L74-L102) annotates `response_model=None` and the function falls off the end, so FastAPI emits the value `null`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Invalid JSON; `llm.provider` not in the allowed Literal set; `vector_db.provider` not in the allowed Literal set; missing required fields when the parent object is present. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 500 | `Internal` | Persistence error. |
- **Side effects**:
  - When `llm` is provided: updates the in-process `LLMConfig` ([`save_llm_config.py` L11-L18](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/save_llm_config.py#L11-L18)) — sets `llm_provider`, `llm_model`, and conditionally `llm_api_key`. The API key is only written when the supplied value (a) does not contain `"*****"` (a redacted-form sentinel) **and** (b) is non-empty after `.strip()`. This is the **echo-back guard**: if the frontend resubmits the value it received from `GET` (which contains stars), we must not overwrite the real key with the masked version.
  - When `vector_db` is provided: updates `VectorDBConfig` ([`save_vector_db_config.py` L12-L19](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/save_vector_db_config.py#L12-L19)) — sets `vector_db_url`, `vector_db_provider`, conditionally `vector_db_key` with the same `"*****"`-and-empty guard. Does **not** persist any of the `endpoint` or `api_version` fields (Python's input DTO has no such fields).
  - Persistence backend: process-singleton, identical to Python. The Rust port writes to in-process `LlmConfig` / `VectorDbConfig` and does **not** persist to a relational table. Python's behavior — settings reset to env-var defaults on restart, and may diverge across workers in a multi-process deployment — is reproduced verbatim. Operators who need durable settings should set them via env vars at boot (the same workaround Python users employ).
- **Delegation target**: `cognee::settings::save_llm_config(...)` and
  `cognee::settings::save_vector_db_config(...)`. Each invoked only when the corresponding
  optional field is `Some`.
- **Validation rules**:
  - `llm.provider ∈ {"openai", "ollama", "anthropic", "gemini", "mistral"}` (Python's `Literal`
    union at [`get_settings_router.py` L23-L31](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L23-L31)). Note: the GET response advertises **`bedrock`** as a provider but `LLMConfigInputDTO`'s
    `Literal` does **not** accept it on save. We replicate this asymmetry; the frontend treats
    `bedrock` as read-only in v1.
  - `vector_db.provider ∈ {"lancedb", "chromadb", "pgvector"}` ([L36-L40](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L36-L40)). Note: the GET advertises only `lancedb` and `pgvector`; the save accepts `chromadb` too.
  - When `vector_db` is present, `url` and `api_key` must be present (Pydantic enforces).
  - When `llm` is present, `provider`, `model`, and `api_key` must be present.
- **Authorization checks**: authentication only. **There is no admin-role gate** — any authenticated user can rewrite the global settings. Document this loudly. Open question §6.2 proposes an admin gate; defer.
- **OpenAPI**: tag `settings`. `200` with empty body. Document the redaction policy and the echo guard explicitly so SDKs do not accidentally re-submit masked keys.
- **Telemetry**: span `cognee.api.settings.save`. Attrs: `user.id`, `llm.provider` (when set), `vector_db.provider` (when set), `llm.api_key.changed` (bool — true iff the post-save key differs from pre-save), `vector_db.api_key.changed`. Never log the raw key.
- **Python parity notes**:
  - The `"*****"` sentinel check is a **substring** check, not an equality check. Any submitted key that contains the literal substring `"*****"` is treated as the redacted echo and dropped. We reproduce this exactly even though it is technically a footgun (a real key with five consecutive stars would be rejected).
  - There is no transaction across the two sub-saves: if `save_llm_config` succeeds and `save_vector_db_config` fails, the LLM half persists. Match Python; open question §6.5.
  - The handler returns `null` (no body); the Cognee frontend treats any 2xx as success.

## 3. Cross-cutting behavior

- **Auth-only gate**: both endpoints require `AuthenticatedUser`; no permission-resolution call to `PermissionsRepository`.
- **In-memory state**: Python's settings live in process state (`get_llm_config()`, `get_vectordb_config()` return process-singleton objects). On a multi-process server they may be inconsistent across workers and reset on restart. The Rust port reproduces this exactly — no DB persistence, no SeaORM table, no startup-restore. Operators wanting durable cross-restart settings configure them via the same env vars Python reads at boot.
- **API key handling**:
  - **Read**: stars-mask first 10 chars + `*` for the rest, never the raw key.
  - **Write**: ignore values that contain `"*****"` (the mask sentinel) or are empty after trim.
  - **Telemetry**: never include the raw key in any span attribute.
  - These three rules are non-negotiable; they implement the redaction contract from [../observability.md §5](../observability.md#5-secret-redaction) and the auth-secrets policy from [../auth.md §14](../auth.md#14-security-considerations).
- **Provider lists are constants**: implemented as `static` arrays in `crates/http-server/src/routers/settings.rs`. Update in lockstep with Python via the cross-SDK parity test.
- **Telemetry**: `cognee.api.settings.<verb>` with the attributes from §2.x. See [../observability.md §3.4](../observability.md#34-span-name-conventions).

## 4. DTO definitions

```rust
// crates/http-server/src/dto/settings.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ── Selectable provider/model lists ────────────────────────────────────────

/// Single `{value, label}` pair. Matches Python's `ConfigChoice`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ConfigChoice {
    pub value: String,
    pub label: String,
}

// ── GET response ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct LLMConfigOutputDTO {
    /// Currently configured provider (e.g. `"openai"`).
    pub provider: String,
    /// Currently configured model name (e.g. `"gpt-4o-mini"`).
    pub model: String,
    /// Optional non-default endpoint (Ollama, Azure, vLLM, …). Python returns
    /// the raw value or `None`.
    pub endpoint: Option<String>,
    /// Azure-only API version. Python returns the raw value or `None`.
    pub api_version: Option<String>,
    /// **Redacted**: `key[0..10] + "*" * (len(key) - 10)`, or `null` if no key
    /// is configured. Mirrors Python's [`get_settings.py` L94-L96](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L94-L96).
    pub api_key: Option<String>,
    /// All providers the frontend should render in the dropdown. Hard-coded
    /// list mirroring Python's `llm_providers` array.
    pub providers: Vec<ConfigChoice>,
    /// Provider → model list. Keys must include all `providers` entries.
    /// Hard-coded list mirroring Python's `models` dict.
    pub models: std::collections::BTreeMap<String, Vec<ConfigChoice>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct VectorDBConfigOutputDTO {
    pub provider: String,
    pub url: String,
    /// **Redacted** with the same masking policy as `LLMConfigOutputDTO::api_key`.
    pub api_key: String,
    pub providers: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SettingsDTO {
    pub llm: LLMConfigOutputDTO,
    pub vector_db: VectorDBConfigOutputDTO,
}

// ── POST request body ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct LLMConfigInputDTO {
    /// One of `"openai" | "ollama" | "anthropic" | "gemini" | "mistral"`.
    /// Note: `"bedrock"` is **not** accepted on save even though the GET
    /// advertises it. Match Python's Literal union at
    /// [`get_settings_router.py` L23-L31](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L23-L31).
    pub provider: LlmProvider,
    pub model: String,
    /// May be a redacted echo from the GET response. Drop the value if it
    /// contains the literal `"*****"` substring or is empty after trim.
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    Openai,
    Ollama,
    Anthropic,
    Gemini,
    Mistral,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct VectorDBConfigInputDTO {
    /// One of `"lancedb" | "chromadb" | "pgvector"`.
    pub provider: VectorDbProvider,
    pub url: String,
    /// Same echo-guard rule as `LLMConfigInputDTO::api_key`.
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VectorDbProvider {
    Lancedb,
    Chromadb,
    Pgvector,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SettingsPayloadDTO {
    #[serde(default)]
    pub llm: Option<LLMConfigInputDTO>,
    #[serde(default)]
    pub vector_db: Option<VectorDBConfigInputDTO>,
}
```

The redaction helper is shared:

```rust
/// Mirrors Python's `(key[0:10] + "*" * (len(key) - 10)) if key else None`.
pub fn redact_api_key(key: Option<&str>) -> Option<String> {
    let key = key.filter(|k| !k.is_empty())?;
    if key.chars().count() <= 10 {
        // Python slices bytes, not chars; for ASCII keys this is identical.
        return Some(format!("{key}{}", "*".repeat(0)));
    }
    let mut head: String = key.chars().take(10).collect();
    let stars = key.chars().count() - 10;
    head.push_str(&"*".repeat(stars));
    Some(head)
}

/// Returns `true` if the submitted key should be persisted. Mirrors the
/// `'*****' not in key and len(key.strip()) > 0` guard in Python.
pub fn should_persist_api_key(submitted: &str) -> bool {
    !submitted.contains("*****") && !submitted.trim().is_empty()
}
```

## 5. Implementation tasks

1. Add DTO structs and the redaction/echo-guard helpers in `crates/http-server/src/dto/settings.rs`.
2. Add the static `LLM_PROVIDERS`, `VECTOR_DB_PROVIDERS`, and `MODELS` lists in
   `crates/http-server/src/routers/settings.rs` (literal-equal to Python — the cross-SDK parity test
   compares as JSON).
3. Add `cognee::settings` façade exposing `get_settings()`, `save_llm_config(LLMConfigInput)`,
   `save_vector_db_config(VectorDbConfigInput)`. Wraps existing `LlmConfig`/`VectorDbConfig`.
4. Add handlers in `crates/http-server/src/routers/settings.rs`. Both are `#[tracing::instrument(skip(state))]`.
5. OpenAPI annotations; explicitly document the redaction/echo policy in the description.
6. Unit tests: `redact_api_key()` empty/short/long; `should_persist_api_key()` for `""`, `"   "`, `"sk-real-key"`, `"sk-prefix*****abc"`, `"AAAAAAAAAA*****"`.
7. Integration tests in `crates/http-server/tests/test_settings.rs`:
   - `GET` with no key configured → `llm.api_key == null`.
   - `GET` with key `"sk-1234567890XYZ"` → `"sk-12345678***"` (10 chars + 5 stars).
   - `POST` with `api_key: "sk-real"` then `GET` → mask reflects `"sk-real"`.
   - `POST` with `api_key: "sk-12345***"` (echo) → key is *not* overwritten.
   - `POST` with only `llm` → `vector_db` unchanged.
   - `POST` with `provider: "bedrock"` → `400` (literal-not-accepted).
8. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_settings.py`: GET response from Python
   and Rust must be byte-equal modulo the API-key portion (which depends on configured key).

## 6. Open questions

1. **Vector-DB key empty-handling** — Python's [`get_settings.py` L184-L187](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L184-L187) does *not* short-circuit on empty `vector_config.vector_db_key`, which would crash on `len("") - 10 = -10` followed by `"*" * -10 == ""`. Python coincidentally gets away with returning the empty string + zero stars; Rust matches (returns empty string rather than `null`) to avoid divergence.
2. **Atomicity across sub-saves** — `save_llm_config` and `save_vector_db_config` are independent; a failure on the second leaves a half-applied state in the process-singleton. Python has the same behavior; Rust matches. No fix proposed.
3. **`endpoint` and `api_version` fields are read-only** — surfaced on `GET` but not in the input DTO. Match Python exactly: input DTO does not accept these fields.
4. **`bedrock` asymmetry** — `bedrock` is in the GET-advertised providers but not the POST `Literal`. Replicate the asymmetry verbatim. The frontend treats `bedrock` as read-only.
5. **No admin gate** — Python lets any authenticated user rewrite the global LLM / vector-DB config. Rust matches. The cross-SDK parity test confirms a non-superuser can save without 403.
6. **Settings-singleton placement** — *Resolved during P5 (commit 2652aea)*: the spec called for a `cognee::settings` façade that the router thinly wraps, but `cognee`'s `server` feature already gates `cognee-http-server`, so a back-edge from `cognee::settings` to the router would create a feature cycle. The process-singleton `SettingsStore` therefore lives directly in `crates/http-server/src/routers/settings.rs`. Wire shape, redaction policy, and provider/model lists still match Python verbatim. If a non-HTTP consumer ever needs these settings, lift the singleton into a sibling `cognee-settings` crate without churning HTTP code.

## 7. References

- Python router: [`get_settings_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py).
- Python implementations:
  [`get_settings.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py),
  [`save_llm_config.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/save_llm_config.py),
  [`save_vector_db_config.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/save_vector_db_config.py).
- Mount in Python: [`client.py` L238](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L238).
- Auth extractor: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Secret redaction policy: [../observability.md §5](../observability.md#5-secret-redaction).
- Error mapping: [../architecture.md §9](../architecture.md#9-error-handling).
