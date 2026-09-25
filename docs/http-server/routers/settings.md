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
  - **Provider/model lists are server-rendered constants**: Python hard-codes the lists of available providers and per-provider model lists in the response body. We mirror the lists verbatim from [`get_settings.py` L60-L179](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L60-L179) so the Cognee frontend renders identically. **The lists must stay literal-equal to Python.** They drifted apart once (Rust carried a single stale bedrock model) and were re-aligned in plan task P2; the bedrock entry is now Python's three, in this order: `eu.anthropic.claude-sonnet-4-5-20250929-v1:0` / `Claude 4.5 Sonnet`, `eu.anthropic.claude-haiku-4-5-20251001-v1:0` / `Claude 4.5 Haiku`, `eu.amazon.nova-lite-v1:0` / `Amazon Nova Lite`.

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
  - `llm.provider ∈ {"openai", "ollama", "anthropic", "gemini", "mistral", "bedrock"}` — the
    shipped `LlmProvider` enum in `crates/http-server/src/dto/settings.rs`. Python's `Literal`
    union at [`get_settings_router.py` L23-L31](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L23-L31)
    is no longer the source of truth for this set. Note the one remaining **`bedrock`
    asymmetry**, which now runs the other way: both SDKs advertise `bedrock` on `GET`, Rust
    **accepts** it on save (task R7, `200`), but upstream Python's `Literal` still omits it and
    rejects the value with `400`. Closing that is the upstream edit tracked as
    [`bedrock-provider-plan.md` §5 P1](../../roadmap/bedrock-provider-plan.md) — see open
    question §6.4.
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
    /// One of `"openai" | "ollama" | "anthropic" | "gemini" | "mistral" |
    /// "bedrock"`. Note: `"bedrock"` **is** accepted here — it is advertised
    /// by the GET response and the Bedrock provider is registered by default.
    /// Python's Literal union at
    /// [`get_settings_router.py` L23-L31](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/settings/routers/get_settings_router.py#L23-L31)
    /// does not include it yet, so Python's save still rejects it; that gap is
    /// plan §5 P1 and is the only remaining divergence in this enum.
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
    Bedrock,
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
7. Integration tests in `crates/http-server/tests/test_settings.rs` (the file exists; the bedrock
   cases landed with R7):
   - `GET` with no key configured → `llm.api_key == null`.
   - `GET` with key `"sk-1234567890XYZ"` → `"sk-12345678***"` (10 chars + 5 stars).
   - `POST` with `api_key: "sk-real"` then `GET` → mask reflects `"sk-real"`.
   - `POST` with `api_key: "sk-12345***"` (echo) → key is *not* overwritten.
   - `POST` with only `llm` → `vector_db` unchanged.
   - `test_get_settings_lists_python_bedrock_models` — the three bedrock models from §2.1, asserted
     by `value`, `label`, and order. *(implemented)*
   - `test_post_settings_accepts_bedrock_provider` — `POST` with `provider: "bedrock"` → `200`.
     *(implemented)*
   - `test_post_settings_rejects_unknown_provider` — negative control: the enum is still closed, so
     a value outside it is rejected before the handler runs. *(implemented)*
8. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_settings.py` — *written* as plan task
   [§5 P4](../../roadmap/bedrock-provider-plan.md); it is the guard that keeps the provider/model
   lists and the save-side enum from re-diverging. It runs in the secret-free Phase-1a lane of
   `.github/workflows/http-parity.yml`. It builds on `py_client`/`rs_client` and **not** on
   `authed_clients`, which skips whenever `/api/v1/auth/*` is missing — always true for the OSS
   Rust build, so using it would silently void the gate. Authentication is asymmetric instead: the
   Rust server answers unauthenticated (`require_authentication` defaults to false and
   `start_servers.sh` does not set `REQUIRE_AUTHENTICATION`), while upstream Python's
   `get_authenticated_user` computes `REQUIRE_AUTHENTICATION` as *true* when both
   `REQUIRE_AUTHENTICATION` and `ENABLE_BACKEND_ACCESS_CONTROL` are unset — which is the harness's
   configuration — so it answers `401`. A module-local `py_settings_client` fixture registers and
   logs in on the Python side only, and asserts (rather than skips) if that login fails. Six cases:
   - `test_settings_bedrock_provider_choice_matches` — the `bedrock` entry of `llm.providers` is
     equal on both sides, `value` and `label`. (Rust's label was `"AWS Bedrock"` against Python's
     `"Bedrock"`; the Rust literal was corrected to Python's, which is the parity source of truth.)
   - `test_settings_bedrock_model_list_matches` — `llm.models["bedrock"]` equal element-for-element
     and in order.
   - `test_settings_llm_provider_value_set_matches` — `llm.providers[*].value` identical, same order.
   - `test_settings_get_full_body_parity` — the whole GET body, ignoring the environment-dependent
     scalars (`llm.provider`/`model`/`endpoint`/`apiVersion`/`apiKey`, `vectorDb.provider`/`url`/
     `apiKey` — the two servers run from separate `/py` and `/rs` workspaces). Note the wire keys
     are camelCase on **both** sides (Rust's `#[serde(rename_all = "camelCase")]`; Python's `OutDTO`
     `alias_generator=to_camel` plus FastAPI's default `response_model_by_alias=True`), so the
     `strip_paths` patterns must spell them that way — `$..api_key` would match nothing.
     `xfail(strict=False)`
     for now: the non-bedrock `llm.models.{openai,anthropic,gemini,mistral}` and
     `vector_db.providers` lists diverged long before this plan and are out of its scope; the marker
     comes off when they are aligned.
   - `test_settings_post_bedrock_accepted_by_rust` — `POST provider: "bedrock"` → `200`, reflected on
     the next `GET`.
   - `test_settings_post_bedrock_accepted_by_python` — the same POST against Python,
     `xfail(strict=True)`: upstream returns a 4xx until plan §5 P1 lands and the
     `topoteretes/cognee` pin (`b9014c16`) in `.github/workflows/http-parity.yml` is bumped, at
     which point the case XPASSes loudly and the marker should be deleted.

## 6. Open questions

1. **Vector-DB key empty-handling** — Python's [`get_settings.py` L184-L187](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L184-L187) does *not* short-circuit on empty `vector_config.vector_db_key`, which would crash on `len("") - 10 = -10` followed by `"*" * -10 == ""`. Python coincidentally gets away with returning the empty string + zero stars; Rust matches (returns empty string rather than `null`) to avoid divergence.
2. **Atomicity across sub-saves** — `save_llm_config` and `save_vector_db_config` are independent; a failure on the second leaves a half-applied state in the process-singleton. Python has the same behavior; Rust matches. No fix proposed.
3. **`endpoint` and `api_version` fields are read-only** — surfaced on `GET` but not in the input DTO. Match Python exactly: input DTO does not accept these fields.
4. **`bedrock` asymmetry** — *Resolved on the Rust side during R7 (commit b76a216d)*: the original answer ("replicate Python's asymmetry verbatim; the frontend treats `bedrock` as read-only") is retired. `LlmProvider` now carries a `Bedrock` variant and the router's save arm maps it to `"bedrock"`, so Rust accepts the provider it advertises. The gap that remains is one-way and upstream: Python's `LLMConfigInputDTO.provider` `Literal` still omits `bedrock`, so its POST answers `400`. Adding it there is the edit tracked as [`bedrock-provider-plan.md` §5 P1](../../roadmap/bedrock-provider-plan.md), which cannot land from this repository. The guard against the two sides drifting again is the cross-SDK parity test in §5 item 8 (plan §5 P4), now written as `e2e-cross-sdk/harness/test_http_settings.py`; its `test_settings_post_bedrock_accepted_by_python` case is `xfail(strict=True)` and turns red the moment upstream accepts the value.
5. **No admin gate** — Python lets any authenticated user rewrite the global LLM / vector-DB config. Rust matches. **No test asserts this.** The §5 item 8 parity test saves as an ordinary registered user on the Python side and as the synthetic default user on the Rust side, but its only POST assertion is the status code — and its Python leg is `xfail` today for an unrelated reason (the `bedrock` `Literal` gap). Confirming "a non-superuser can save without 403" would need a dedicated case that logs in as both a superuser and a non-superuser on Python.
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
