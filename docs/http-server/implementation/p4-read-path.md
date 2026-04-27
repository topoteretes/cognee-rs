# Implementation: P4 — Read path

## 1. Goal

Land the four read-path routers that complete the "core pipeline via HTTP" slice: `/api/v1/search` (GET history + POST semantic search), `/api/v1/recall` (GET history + POST — wire-level alias for search per Python parity), `/api/v1/llm` (`/custom-prompt` + `/infer-schema` LLM utilities), and `/api/v1/visualize` (`GET /?dataset_id=` HTML + superuser-only `POST /multi`). The phase also extends `cognee-visualization` with the missing `render_multi_user(...)` entry point. After P4, every read-path endpoint the cognee-frontend uses today has a Rust counterpart with byte-equivalent shapes and (per Python) three router-specific error envelopes wired into `ApiError`.

## 2. References (read these before starting)

- Phase summary: [plan.md §4 P4](../plan.md#4-implementation-phases).
- Implementation invariants (atomic steps, no rationale duplication, strict-Python-parity): [implementation/README.md](README.md).
- Error model and envelope deviations: [architecture.md §9](../architecture.md#9-error-handling), [routers/README.md §3.1](../routers/README.md#31-error-envelope).
- Auth extractor (`AuthenticatedUser` + the `SuperuserOnly` wrapper this phase introduces): [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Per-router specs (read all four in full):
  - [routers/search.md](../routers/search.md) — 15 `SearchType` wire values, history side-effect, `ErrorResponseDTO {error, detail}` envelope.
  - [routers/recall.md](../routers/recall.md) — wire-level alias of `/search`; three distinct error envelopes (`{error, hint}`, `{error}`, silent `[]`); no `session_id` / `auto_route` on the wire.
  - [routers/llm.md](../routers/llm.md) — `_ALLOWED_LLM_PARAMS` filter, `{error}` envelope.
  - [routers/visualize.md](../routers/visualize.md) — HTML response, single-409 catch-all, superuser gate on `/multi`, `render_multi_user` extension.
- Library-level recall vs HTTP recall distinction: [api-v2/recall.md](../../api-v2/recall.md). The HTTP layer **must not** reach `cognee_lib::api::recall::recall`; auto-routing and session-first dispatch live only in the library API per [routers/recall.md §2.2 and §3.1](../routers/recall.md#22-post-apiv1recall--semantic-search-wire-level-alias-for-search).
- Visualization crate baseline: [crates/visualization/src/lib.rs](../../../crates/visualization/src/lib.rs) — only `render` and `visualize` exist today; `render_multi_user` is added by Step 11 of this phase.

## 3. Prerequisites

- **P0 done** — `crates/http-server/` crate, `AppState`, `ApiError` skeleton, custom `Json` extractor, `build_router` shell.
- **P1 done** — `AuthenticatedUser` extractor, JWT/cookie/X-Api-Key auth, users table.
- **P2 done** — `cognee_lib::datasets()` accessor surface and the `get_authorized` resolver are available; needed by the visualize handler's permission gate.
- P3 is **not** required for P4 — the read path doesn't touch `PipelineRunRegistry` or the WebSocket layer.

## 4. Step-by-step

### Step 1: Add the search DTOs

- **File(s)**: `crates/http-server/src/dto/search.rs`, `crates/http-server/src/dto/mod.rs` (`pub mod search;`).
- **Action**: Define `SearchPayloadDTO`, `SearchHistoryItemDTO`, `SearchResultDTO`, and `ErrorResponseDTO` exactly as specified in [routers/search.md §4](../routers/search.md#4-dto-definitions). Field order, defaults, and `serde(default = "...")` helpers must match the per-router doc verbatim — empty `POST {}` round-trips because of the defaults. Re-export `SearchType` from `cognee_search::types` and bind it to the wire under `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. **Drop the Rust-only `Feedback` variant from the wire-facing enum** per [routers/search.md §6 Q1](../routers/search.md#6-open-questions) and the audit findings — keep the internal `SearchType::Feedback` for library callers, expose only the 14 Python-parity values plus the 15-th `Feedback` Python equivalent (audit: the wire DTO mirrors Python's set verbatim, so the count is 14 over the wire). Add a `flatten_search_response(SearchResponse) -> Vec<SearchResultDTO>` helper that maps `SearchOutput::{Text, Items, GraphQueryRows, Rules, Structured, Ack, Texts}` to the polymorphic `search_result: serde_json::Value` shape per [routers/search.md §4 "Wire shape of search_result"](../routers/search.md#wire-shape-of-search_result-per-retriever). All three DTOs carry `#[derive(ToSchema)]` so utoipa picks them up.
- **Spec reference**: [routers/search.md §4](../routers/search.md#4-dto-definitions).
- **Verify**: `cargo test -p cognee-http-server --lib dto::search::tests` — inline unit tests cover (a) `POST {}` round-trips with all defaults, (b) every `SearchType` wire string deserializes, (c) `flatten_search_response` produces the documented JSON shape for each `SearchOutput` arm.

### Step 2: Extend `ApiError` with the three read-path envelope variants

- **File(s)**: `crates/http-server/src/error.rs`.
- **Action**: Add three variants to the existing `ApiError` enum and their `IntoResponse` arms:
  1. `SearchError { status: StatusCode, error: String, detail: Option<String> }` — emits `{"error": "<error>", "detail": "<detail or null>"}`. Used by `POST /api/v1/search` for 403/422/500 and by `GET /api/v1/search` for 500. Match Python's [`ErrorResponse`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py) two-field shape.
  2. `RecallError { status: StatusCode, body: RecallErrorBody }` where `RecallErrorBody` is the `#[serde(untagged)]` enum from [routers/recall.md §4](../routers/recall.md#4-dto-definitions) (`WithHint { error, hint }` for 422; `JustError { error }` for 409 catch-all and the GET 500). Define `RecallErrorBody` here next to the variant so the enum and its body live in the same file.
  3. `LlmError(StatusCode, String)` — emits `{"error": "<msg>"}` for the 400 / 409 / 422 paths described in [routers/llm.md §2](../routers/llm.md#2-endpoints).
  4. `VisualizeError(StatusCode, String)` — emits `{"error": "<msg>"}` for the 409 catch-all and the 403 superuser-only failure described in [routers/visualize.md §2](../routers/visualize.md#2-endpoints).
  Add `#[doc(hidden)] /// DO NOT NORMALIZE — these envelopes are wire-compatibility constraints. See routers/README.md §3.1.` on the variants. Update the matrix in [routers/README.md §3.1](../routers/README.md#31-error-envelope) so the `recall` / `visualize` / `llm` rows reference these variants by name.
- **Spec reference**: [routers/README.md §3.1](../routers/README.md#31-error-envelope), per-router error tables.
- **Verify**: `cargo test -p cognee-http-server --lib error::tests` — assert each new variant's `IntoResponse` JSON body byte-equals a hand-rolled fixture.

### Step 3: Add the search router

- **File(s)**: `crates/http-server/src/routers/search.rs`, update `crates/http-server/src/routers/mod.rs` (`pub mod search;`).
- **Action**: Implement `pub fn router() -> Router<AppState>` with two routes — `GET /` → `get_search_history`, `POST /` → `post_search`. Both take `AuthenticatedUser`. The GET handler calls `state.lib.search().history(user.id, /* limit = */ None).await` and serializes the rows as `Vec<SearchHistoryItemDTO>`; on `SearchHistoryDb` failure, return `ApiError::SearchError { status: 500, error: "Internal server error", detail: Some(err.to_string()) }` per [routers/search.md §2.1](../routers/search.md#21-get-apiv1search--list-the-callers-search-history). The POST handler:
  1. Decodes the body via the custom `Json<SearchPayloadDTO>` extractor (validation errors flow through the extractor's `ApiError::Validation`).
  2. Builds a `SearchRequest` from the DTO and calls `state.lib.search().run(request).await`. The orchestrator writes the `Query` and `Result` history rows; failure to log is non-fatal (the orchestrator already wraps in best-effort try/catch — see [routers/search.md §2.2 "Side effects"](../routers/search.md#22-post-apiv1search--run-a-semantic-search)).
  3. On `Ok(response)`, calls `flatten_search_response(response)` from Step 1 and returns `Json(Vec<SearchResultDTO>)`.
  4. Maps orchestrator errors:
     - `SearchError::PermissionDenied` → 403 with `error="Permission denied"`.
     - `SearchError::DatasetNotFound | InvalidInput | DatabaseNotCreated` → 422 with `error="Search prerequisites not met, hint: <hint>"`.
     - everything else → 500 with `error="Internal server error"`, `detail=Some(err.to_string())`.
  5. Validate `top_k`: per [routers/search.md §6 Q2](../routers/search.md#6-open-questions), Rust matches Python — if `top_k <= 0`, do **not** return 400; emit `tracing::warn!` and let the orchestrator return `[]`. Document this in a code comment so a future maintainer doesn't "fix" it.
  6. `query` length: no application-level cap (per [routers/search.md §6 Q3](../routers/search.md#6-open-questions)); the body-size limit applies.
  Add `#[utoipa::path(...)]` annotations on both handlers (tag `"v1"` and `"search"`) declaring response shapes for 200, 401, 403, 422, 500.
- **Spec reference**: [routers/search.md §2 / §5](../routers/search.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`; integration coverage in Step 12.

### Step 4: Add the recall DTOs

- **File(s)**: `crates/http-server/src/dto/recall.rs`, update `crates/http-server/src/dto/mod.rs` (`pub mod recall;`).
- **Action**: Implement `RecallPayloadDTO` per [routers/recall.md §4](../routers/recall.md#4-dto-definitions) — a field-for-field copy of `SearchPayloadDTO`. **Do NOT** add `session_id` or `auto_route` fields — Python's HTTP DTO doesn't expose them and Rust matches exactly per [routers/recall.md §2.2 and §3.1](../routers/recall.md#22-post-apiv1recall--semantic-search-wire-level-alias-for-search). Add `pub type RecallHistoryItemDTO = crate::dto::search::SearchHistoryItemDTO;` and `pub type RecallResultDTO = crate::dto::search::SearchResultDTO;` aliases for OpenAPI clarity. Re-export `RecallErrorBody` from `crate::error` (it lives in `error.rs` per Step 2; no duplicate definition). Inline tests cover (a) DTO defaults round-trip, (b) `RecallErrorBody::WithHint` / `JustError` serialize to the documented two-field / one-field envelopes.
- **Spec reference**: [routers/recall.md §4](../routers/recall.md#4-dto-definitions).
- **Verify**: `cargo test -p cognee-http-server --lib dto::recall::tests`.

### Step 5: Add the recall router

- **File(s)**: `crates/http-server/src/routers/recall.rs`, update `crates/http-server/src/routers/mod.rs`.
- **Action**: Implement `pub fn router() -> Router<AppState>` with `GET /` → `get_recall_history`, `POST /` → `post_recall`. Both take `AuthenticatedUser`. The GET handler calls **the same** `state.lib.search().history(user.id, None).await` the search router uses — the two endpoints **share** history rows by design ([routers/recall.md §2.1](../routers/recall.md#21-get-apiv1recall--list-the-callers-recallsearch-history)). On DB error, return `ApiError::RecallError { status: 500, body: RecallErrorBody::JustError { error: "An error occurred while fetching recall history.".into() } }` (note: single-field envelope, NOT `{error, detail}`; Python drops the detail to avoid leaking DB internals). Log the underlying error at `error!` level.

  The POST handler **must** call the same `state.lib.search().run(...)` delegate the search router uses — verbatim with Python's `from cognee.api.v1.search import search as cognee_search`. Do NOT call `cognee_lib::api::recall::recall(...)`; auto-routing and session-first dispatch are library-only per [routers/recall.md §2.2](../routers/recall.md#22-post-apiv1recall--semantic-search-wire-level-alias-for-search). Map errors to recall's three envelopes:
  - `SearchError::PermissionDenied` → return `Ok(Json(Vec<RecallResultDTO>::new()))` — **silent `200 []`**, NOT 403, per [routers/recall.md §2.2](../routers/recall.md#22-post-apiv1recall--semantic-search-wire-level-alias-for-search).
  - `SearchError::{DatasetNotFound, InvalidInput, DatabaseNotCreated, UserNotFound}` → 422 with `RecallErrorBody::WithHint { error: "Recall prerequisites not met".into(), hint: "Run `await cognee.remember(...)` or `await cognee.add(...)` then `await cognee.cognify()` before recalling.".into() }`.
  - everything else → 409 with `RecallErrorBody::JustError { error: "An error occurred during recall.".into() }` and an `error!` log of the underlying cause.
  Add `#[utoipa::path(...)]` annotations (tag `"v1"` and `"recall"`) declaring 200, 401, 409, 422, 500 (the GET 500 uses `RecallErrorBody::JustError`, not `SearchError`).
- **Spec reference**: [routers/recall.md §2 / §5](../routers/recall.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`; integration coverage in Step 13.

### Step 6: Add the LLM DTOs

- **File(s)**: `crates/http-server/src/dto/llm.rs`, update `crates/http-server/src/dto/mod.rs`.
- **Action**: Define the four DTOs from [routers/llm.md §4](../routers/llm.md#4-dto-definitions): `CustomPromptGenerationPayloadDTO`, `CustomPromptGenerationResponseDTO`, `InferSchemaPayloadDTO`, `InferSchemaResponseDTO`. `graph_model` and `parameters` are `serde_json::Value`. Add `pub fn safe_params(input: &Value) -> Value` that filters keys against the constant `pub const ALLOWED_LLM_PARAMS: &[&str] = &["temperature", "max_tokens", "top_p", "seed"];` and silently drops everything else (matches Python's `_safe_params()`). Inline unit tests cover (a) all four allowed keys survive, (b) any other key is dropped without an error, (c) non-object `parameters` round-trips to an empty object.
- **Spec reference**: [routers/llm.md §3 / §4](../routers/llm.md#3-cross-cutting-behavior).
- **Verify**: `cargo test -p cognee-http-server --lib dto::llm::tests`.

### Step 7: Port the four LLM prompt templates

- **File(s)**: `crates/llm/src/prompts/custom_prompt_generation_user.txt`, `crates/llm/src/prompts/custom_prompt_generation_system.txt`, `crates/llm/src/prompts/infer_schema_user.txt`, `crates/llm/src/prompts/infer_schema_system.txt`. Update `crates/llm/src/prompts/mod.rs` (or equivalent) to register them.
- **Action**: Copy the four prompt files verbatim from Python's [`cognee/infrastructure/llm/prompts/`](https://github.com/topoteretes/cognee/tree/main/cognee/infrastructure/llm/prompts). The user-prompt files contain `{{GRAPH_SCHEMA_JSON}}` and `{{SAMPLE_TEXT}}` Jinja-style placeholders respectively; the system-prompt files take no variables. Wire them through the existing `cognee_llm::prompts::render_prompt(name, ctx)` loader. If the loader does not yet exist, add a minimal one that does string substitution on `{{KEY}}` patterns (no need for full Jinja — the Python prompts use only flat substitution). Keep file naming exactly as on the Python side so cross-SDK diffing has zero false positives.
- **Spec reference**: [routers/llm.md §2.1 / §2.2 / §5](../routers/llm.md#21-post-apiv1llmcustom-prompt--synthesize-an-extraction-prompt-from-a-graph-model).
- **Verify**: `cargo test -p cognee-llm --lib prompts::tests` — assert each template loads and renders with a fixture context.

### Step 8: Ensure `graph_schema_to_graph_model` exists in `cognee-cognify`

- **File(s)**: `crates/cognify/src/graph_model.rs` (or wherever the existing graph-model utilities live; check first).
- **Action**: Grep the cognify crate for a `graph_schema_to_graph_model` function. If it exists, no change. If it doesn't, port it from Python's [`cognee/shared/graph_model_utils.py`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/graph_model_utils.py): take a `&serde_json::Value`, validate it has the canonical shape (`entity_types: [...]`, `relationship_types: [...]`, optional `name`), and return `Result<GraphModel, GraphModelError>` where `GraphModelError` is a `thiserror`-derived enum with one variant per validation failure mode. The handler in Step 9 calls this function but does **not** use its successful return value — it only uses the error to distinguish `409` (schema-conversion failure) from `422` (JSON parse failure). The function therefore needs to exist and return a clean error type; the Rust struct shape itself is not on the wire.
- **Spec reference**: [routers/llm.md §2.2](../routers/llm.md#22-post-apiv1llminfer-schema--propose-a-graph-model-from-sample-text).
- **Verify**: `cargo test -p cognee-cognify --lib graph_model::tests`.

### Step 9: Add the LLM router

- **File(s)**: `crates/http-server/src/routers/llm.rs`, update `crates/http-server/src/routers/mod.rs`.
- **Action**: Implement `pub fn router() -> Router<AppState>` mounting `POST /custom-prompt` → `post_custom_prompt` and `POST /infer-schema` → `post_infer_schema`. Both take `AuthenticatedUser` and decode their bodies via the custom `Json<...>` extractor. Handler bodies follow [routers/llm.md §2](../routers/llm.md#2-endpoints) step-by-step:

  `post_custom_prompt`: render the two `custom_prompt_generation_*` templates, substituting `GRAPH_SCHEMA_JSON = serde_json::to_string(&payload.graph_model)`. Filter `payload.parameters` through `safe_params(...)`. Call `state.lib.llm().acreate_structured_output::<String>(user_prompt, system_prompt, safe_params).await`. Wrap the returned string in `CustomPromptGenerationResponseDTO`. Map `LlmError::ValueError(_)` → `ApiError::LlmError(StatusCode::BAD_REQUEST, msg)`; everything else → `ApiError::LlmError(StatusCode::CONFLICT, msg)` (Python uses 409 as a catch-all here).

  `post_infer_schema`: render the two `infer_schema_*` templates with `SAMPLE_TEXT = payload.text`. Call `acreate_structured_output::<String>` as above. Then **two-stage validation**:
  1. `serde_json::from_str::<serde_json::Value>(&output)` — on failure, return `ApiError::LlmError(StatusCode::UNPROCESSABLE_ENTITY, format!("LLM output is not valid JSON: {}", err))`. The exact format string matches Python's `f"LLM output is not valid JSON: {error}"` per [routers/llm.md §2.2](../routers/llm.md#22-post-apiv1llminfer-schema--propose-a-graph-model-from-sample-text).
  2. `cognee_cognify::graph_model::graph_schema_to_graph_model(&parsed)` — on failure, return `ApiError::LlmError(StatusCode::CONFLICT, err.to_string())` (this is "system-fixable", not "user-fixable", per the per-router doc).
  Wrap the parsed JSON in `InferSchemaResponseDTO`. Add `#[utoipa::path(...)]` annotations (tag `"v1"` and `"llm"`).
- **Spec reference**: [routers/llm.md §2 / §5](../routers/llm.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`; integration coverage in Step 14.

### Step 10: Add the `SuperuserOnly` extractor

- **File(s)**: `crates/http-server/src/auth/superuser.rs` (new), update `crates/http-server/src/auth/mod.rs` to `pub mod superuser; pub use superuser::SuperuserOnly;`.
- **Action**: Define `pub struct SuperuserOnly(pub AuthenticatedUser);` implementing `axum::extract::FromRequestParts<AppState>`. Internally, run the existing `AuthenticatedUser` extractor; if `user.is_superuser == false`, return `ApiError::VisualizeError(StatusCode::FORBIDDEN, "Superuser privileges required for multi-user visualization".into())` per [routers/visualize.md §2.2](../routers/visualize.md#22-post-apiv1visualizemulti--render-a-combined-multi-user-visualization). The error text is the visualize-specific one but the extractor is generic — other phases (P5) may reuse it; document the error variant choice with a comment noting that the message is visualize-specific because it's the only superuser endpoint as of P4. Inline unit tests assert both the success case (admin user → struct returned) and the rejection case (regular user → 403 with `{error}` envelope).
- **Spec reference**: [routers/visualize.md §2.2 / §5](../routers/visualize.md#22-post-apiv1visualizemulti--render-a-combined-multi-user-visualization).
- **Verify**: `cargo test -p cognee-http-server --lib auth::superuser::tests`.

### Step 11: Implement `cognee_visualization::render_multi_user`

- **File(s)**: `crates/visualization/src/lib.rs`, optionally a new private module (e.g. `crates/visualization/src/multi.rs`) if the implementation grows past ~80 lines.
- **Action**: Per [routers/visualize.md §3.1](../routers/visualize.md#31-the-cognee-visualization-crate--what-exists-and-whats-missing), add the `render_multi_user` async function. Signature suggestion (the per-router doc leaves the exact shape to the implementer): `pub async fn render_multi_user(pairs: &[(User, Arc<dyn GraphDBTrait>)]) -> Result<String, VisualizationError>`. Internally: for each `(user, gdb)` pair, call `gdb.get_graph_data().await?`, tag every node with a `user_id` attribute (UUID-stringified), accumulate into one `(nodes, edges)` pair, then reuse the existing `serialize::serialize_graph` and `html::build_html` codepath with a flag that tells the template to color-code by `user_id`. The exact d3 color-by-user wiring (proposal: `d3.schemeCategory10` per [routers/visualize.md §6 Q5](../routers/visualize.md#6-open-questions)) belongs in `html.rs` — keep the public surface narrow.

  Add a unit test in `crates/visualization/tests/test_render_multi_user.rs` that builds two `MockGraphDB` instances (from `cognee-test-utils`), seeds them with three nodes apiece, calls `render_multi_user`, and asserts the returned HTML contains six `node` entries plus the `user_id` attribute on each.

  Empty input must produce a valid (but empty) HTML — the visualize POST handler accepts empty arrays per [routers/visualize.md §2.2](../routers/visualize.md#22-post-apiv1visualizemulti--render-a-combined-multi-user-visualization).
- **Spec reference**: [routers/visualize.md §3.1](../routers/visualize.md#31-the-cognee-visualization-crate--what-exists-and-whats-missing).
- **Verify**: `cargo test -p cognee-visualization`.

### Step 12: Add the visualize router

- **File(s)**: `crates/http-server/src/dto/visualize.rs`, `crates/http-server/src/routers/visualize.rs`, update `crates/http-server/src/{dto,routers}/mod.rs`.
- **Action**: DTOs per [routers/visualize.md §4](../routers/visualize.md#4-dto-definitions): `VisualizeQueryDTO { dataset_id: Uuid }` for the GET, `UserDatasetPairDTO { user_id: Uuid, dataset_id: Uuid }` for the POST body. No response DTOs (the body is raw HTML).

  Router with two routes: `GET /` → `get_visualize`, `POST /multi` → `post_visualize_multi`. The GET handler:
  1. Extracts `Query<VisualizeQueryDTO>` and `AuthenticatedUser`.
  2. Resolves and authorizes: `state.lib.datasets().get_authorized(&[query.dataset_id], "read", &user).await`.
  3. (Tenant-context note) When `ENABLE_BACKEND_ACCESS_CONTROL=true`, call the tenant-context shim — see [routers/visualize.md §3.2](../routers/visualize.md#32-tenant-context). Skip when the flag is off.
  4. Calls `cognee_visualization::render(state.lib.graph_db()).await`.
  5. Returns `axum::response::Html(html_string)` — axum sets `Content-Type: text/html; charset=utf-8` automatically.
  6. **Any error** at any step (dataset not found, permission denied, graph DB read error, render failure) is collapsed into `ApiError::VisualizeError(StatusCode::CONFLICT, err.to_string())` per [routers/visualize.md §2.1](../routers/visualize.md#21-get-apiv1visualize--render-a-single-dataset-html-visualization). **Do NOT** return 403 for permission denied — Python's broad `except` swallows it and returns 409. Document the swallow with a code comment so a future maintainer doesn't "fix" it.

  The POST handler takes `SuperuserOnly` (the wrapper from Step 10) and `Json<Vec<UserDatasetPairDTO>>`. Iterates pairs:
  1. `target_user = state.lib.users().get(pair.user_id).await?`.
  2. `dataset = state.lib.datasets().get_authorized(&[pair.dataset_id], "read", &target_user).await?` — permission resolved against the **target** user, not the caller, per [routers/visualize.md §2.2](../routers/visualize.md#22-post-apiv1visualizemulti--render-a-combined-multi-user-visualization). Critical for not elevating access via superuser.
  3. Collect into `Vec<(User, Arc<dyn GraphDBTrait>)>` then call `cognee_visualization::render_multi_user(&pairs).await`.
  4. Wrap in `Html`. Empty arrays return an empty HTML (Python parity).
  5. Any iteration failure → `VisualizeError(StatusCode::CONFLICT, err.to_string())` (no partial success — Python's broad `except` returns 409 for the whole request).
  Add `#[utoipa::path(...)]` annotations (tag `"v1"` and `"visualize"`); declare the `200` response with `content: { "text/html": { schema: { type: "string", format: "html" } } }` and the `403` / `409` JSON envelopes.

  **Mount**: `/api/v1/visualize` per [plan.md §4 P4](../plan.md#4-implementation-phases) — the same prefix as the other routers nested under `/api/v1/*`. Confirm against [architecture.md §7](../architecture.md#7-router-composition) which already lists `nest("/visualize", visualize::router())` in the assembly snippet.
- **Spec reference**: [routers/visualize.md §2 / §3 / §5](../routers/visualize.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`; integration coverage in Steps 15 and 16.

### Step 13: Wire all four routers into `build_router`

- **File(s)**: `crates/http-server/src/lib.rs`.
- **Action**: Inside `build_router`, add the four `.nest(...)` calls under the `/api/v1` sub-router:
  - `.nest("/search",    search::router())`
  - `.nest("/recall",    recall::router())`
  - `.nest("/llm",       llm::router())`
  - `.nest("/visualize", visualize::router())`
  Mount-order must match Python's order in [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py) so OpenAPI tag ordering is stable per [architecture.md §7](../architecture.md#7-router-composition). Register the four routers' `paths(...)` on the root `ApiDoc` `#[derive(OpenApi)]` struct in `src/openapi.rs` so `GET /openapi.json` advertises the new endpoints. No middleware additions needed — the existing CORS / trace / body-limit stack from P0 covers all four.
- **Spec reference**: [architecture.md §7 / §13](../architecture.md#7-router-composition).
- **Verify**: `cargo check -p cognee-http-server`; sanity-curl `GET /openapi.json` and grep for `"/api/v1/search"`, `"/api/v1/recall"`, `"/api/v1/llm/custom-prompt"`, `"/api/v1/visualize"`.

### Step 14: Integration tests — search

- **File(s)**: `crates/http-server/tests/test_search_history.rs`, `crates/http-server/tests/test_search_post.rs`. Reuse `tests/support/mod.rs` (created in P0; extend if needed with a `seed_dataset(...)` helper).
- **Action**: All tests use `tower::ServiceExt::oneshot` against `cognee_http_server::build_router(state).await?`. `test_search_history.rs`: `GET /api/v1/search` returns `[]` for a fresh user; after a `POST /api/v1/search`, the next GET returns at least two rows (one `user="user"`, one `user="system"`) ordered by `created_at` ascending. `test_search_post.rs`: cover **9** of the 15 `SearchType` values per [routers/search.md §2.2](../routers/search.md#22-post-apiv1search--run-a-semantic-search) — `GraphCompletion`, `GraphCompletionCot`, `GraphCompletionContextExtension`, `GraphSummaryCompletion`, `TripletCompletion`, `RagCompletion`, `Chunks`, `Summaries`, `Temporal` (the E2E-tested set per the per-router doc). Each case asserts a 200 response and a non-error body shape; for completion-type searches assert `search_result` is a `String`, for chunk-type searches assert it's an `Array`. Cover the negative paths: `POST` with `dataset_ids=[non_existent_uuid]` → 403 with `{error: "Permission denied", detail: ...}`; `POST` with `search_type="CYPHER"` and a malformed query → 422 with `{error: "Search prerequisites not met...", detail: ...}`; `POST {}` (empty body) → 200 with the documented defaults applied.
- **Spec reference**: [routers/search.md §5 (tasks 7–8)](../routers/search.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-http-server --test test_search_history --test test_search_post`. Tests requiring an actual LLM should gate on `OPENAI_TOKEN`/`OPENAI_URL` env vars and skip when unset (follow the pattern in `crates/cognify/tests/`).

### Step 15: Integration tests — recall

- **File(s)**: `crates/http-server/tests/test_recall.rs`.
- **Action**: Tests:
  1. `POST /api/v1/recall` with `search_type="GRAPH_COMPLETION"` against a populated dataset → 200 with results identical (modulo timestamps) to the same payload posted to `/api/v1/search`.
  2. `POST /api/v1/recall` against a dataset the user can't read → **`200 []`** (NOT 403). This is the headline behavioral difference vs `/search`.
  3. `POST /api/v1/recall` against an empty database → 422 with `{error: "Recall prerequisites not met", hint: "Run `await cognee.remember(...)` ..."}`.
  4. `POST /api/v1/recall` that triggers an arbitrary unhandled error (e.g. inject a failing graph-DB stub) → 409 with `{error: "An error occurred during recall."}`.
  5. `GET /api/v1/recall` after a `POST /api/v1/search` returns the same history rows as `GET /api/v1/search` — assert byte-equality of the two response bodies. This pins the "shared history" contract from [routers/recall.md §2.1](../routers/recall.md#21-get-apiv1recall--list-the-callers-recallsearch-history).
  6. `GET /api/v1/recall` with a forced DB error → 500 with `{error: "An error occurred while fetching recall history."}` (note: single-field envelope, NOT `{error, detail}`).
- **Spec reference**: [routers/recall.md §5 (task 8)](../routers/recall.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-http-server --test test_recall`.

### Step 16: Integration tests — LLM

- **File(s)**: `crates/http-server/tests/test_llm.rs`. Add a `MockLlm` adapter under `tests/support/` (or behind the `testing` feature in `crates/llm/`) that returns canned responses configured per test.
- **Action**: Cover:
  1. `POST /api/v1/llm/custom-prompt` with valid `graph_model` and `MockLlm` returning a canned prompt → 200 + `{custom_prompt: "<canned>"}`.
  2. `POST /api/v1/llm/custom-prompt` with `parameters={"temperature": 0.5, "junk_key": "x"}` → 200; assert the underlying `MockLlm` saw `{"temperature": 0.5}` only (the `junk_key` was filtered by `safe_params`).
  3. `POST /api/v1/llm/infer-schema` with `MockLlm` returning valid JSON-as-string → 200 + parsed `graph_schema` object.
  4. `POST /api/v1/llm/infer-schema` with `MockLlm` returning malformed JSON → 422 with `{error: "LLM output is not valid JSON: ..."}` (assert the prefix; the suffix varies per `serde_json::Error`).
  5. `POST /api/v1/llm/infer-schema` with `MockLlm` returning valid JSON that fails `graph_schema_to_graph_model` (e.g. missing `entity_types`) → 409 with `{error: "..."}`.
  6. `POST /api/v1/llm/custom-prompt` with no auth → 401 with `{detail: "Unauthorized"}` — confirms the auth extractor's canonical envelope is unaffected by the `LlmError` variant.
- **Spec reference**: [routers/llm.md §5 (task 8)](../routers/llm.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-http-server --test test_llm`.

### Step 17: Integration tests — visualize

- **File(s)**: `crates/http-server/tests/test_visualize_single.rs`, `crates/http-server/tests/test_visualize_multi.rs`.
- **Action**:

  `test_visualize_single.rs`:
  1. Seed a dataset with three nodes via `MockGraphDB`. `GET /api/v1/visualize?dataset_id=<uuid>` → 200, `Content-Type: text/html; charset=utf-8`, body contains the canonical d3 root marker (proposal: `<svg` or whatever `html.rs::build_html` produces — pin the exact substring after Step 11 lands).
  2. `GET /api/v1/visualize` (no `dataset_id`) → 422 with the canonical `{detail: [...], body: ...}` validation envelope from the custom `Json/Query` extractor.
  3. `GET /api/v1/visualize?dataset_id=<bogus_uuid>` → 409 with `{error: "..."}` (NOT 404 — match Python's swallow per [routers/visualize.md §2.1](../routers/visualize.md#21-get-apiv1visualize--render-a-single-dataset-html-visualization)).
  4. `GET /api/v1/visualize?dataset_id=<uuid_belonging_to_other_user>` → 409 (NOT 403 — same swallow).

  `test_visualize_multi.rs`:
  1. `POST /api/v1/visualize/multi` as a non-superuser → 403 with `{error: "Superuser privileges required for multi-user visualization"}`.
  2. `POST /api/v1/visualize/multi` as a superuser with a single valid `(user_id, dataset_id)` pair → 200 HTML. Assert the body contains `user_id="<that-uuid>"` somewhere (the multi-user template tags nodes per [routers/visualize.md §3.1](../routers/visualize.md#31-the-cognee-visualization-crate--what-exists-and-whats-missing)).
  3. `POST /api/v1/visualize/multi` as a superuser with `[]` → 200 HTML (empty graph).
  4. `POST /api/v1/visualize/multi` as a superuser where one pair references a non-existent user → 409 with `{error}` (no partial success).
- **Spec reference**: [routers/visualize.md §5 (task 8)](../routers/visualize.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-http-server --test test_visualize_single --test test_visualize_multi`.

## 5. Tests

- `crates/http-server/tests/test_search_history.rs` — empty-history → `[]`; round-trip with a populated POST returning at least two rows ordered by `created_at`.
- `crates/http-server/tests/test_search_post.rs` — 9 of 15 `SearchType` happy-paths (the E2E-tested set), plus 403 (permission denied), 422 (bad Cypher), and the empty-body default-application case.
- `crates/http-server/tests/test_recall.rs` — POST identity vs `/search`; silent `200 []` for permission denied; 422 prerequisite envelope; 409 catch-all envelope; shared history with `/search`; GET 500 single-field envelope.
- `crates/http-server/tests/test_llm.rs` — `custom-prompt` happy-path; `safe_params` filter on the wire; `infer-schema` happy / 422 (bad JSON) / 409 (bad schema-conversion); 401 on no-auth.
- `crates/http-server/tests/test_visualize_single.rs` — happy-path HTML; 422 missing query param; 409 swallows for missing / unauthorized dataset.
- `crates/http-server/tests/test_visualize_multi.rs` — non-superuser 403; superuser happy-path with one pair; superuser empty-array; superuser 409 catch-all.
- Inline unit tests in `dto/{search,recall,llm,visualize}.rs`, `error.rs`, `auth/superuser.rs`, and `crates/visualization/tests/test_render_multi_user.rs` — covered as part of their respective steps.

## 6. Acceptance criteria

- [ ] `cargo check --all-targets -p cognee-http-server` succeeds.
- [ ] `cargo test -p cognee-http-server` passes — including all six integration test files added in Steps 14–17.
- [ ] `cargo test -p cognee-visualization` passes — including the new `render_multi_user` unit test from Step 11.
- [ ] `scripts/check_all.sh` passes (fmt, clippy with `-D warnings`, capi/python/js binding checks unchanged).
- [ ] `cognee_visualization::render_multi_user(...)` is added to `crates/visualization/src/lib.rs` and exposed publicly.
- [ ] `GET /openapi.json` advertises `paths` for `/api/v1/search`, `/api/v1/recall`, `/api/v1/llm/custom-prompt`, `/api/v1/llm/infer-schema`, `/api/v1/visualize`, and `/api/v1/visualize/multi` (manual smoke check via `curl | jq '.paths | keys'`).
- [ ] The new `ApiError::{SearchError, RecallError, LlmError, VisualizeError}` variants emit the documented Python-shaped JSON envelopes (verified by inline `error::tests`).
- [ ] The `Feedback` `SearchType` variant is **not** present on the wire-facing DTO enum (audit's drop-from-wire decision).
- [ ] HTTP recall does **not** call `cognee_lib::api::recall::recall(...)` — confirmed by `grep -rn "api::recall::recall" crates/http-server/` returning zero matches in router code.
- [ ] Status row for **P4** in [implementation/README.md](README.md) flips **Draft → In Progress → Done** in the PR that lands this work.
- [ ] Status rows for **search**, **recall**, **llm**, and **visualize** in [routers/README.md](../routers/README.md) flip **Draft → In Progress → Done** in the same PR.

## 7. Files touched

New (under `crates/http-server/`):

- `src/dto/search.rs`
- `src/dto/recall.rs`
- `src/dto/llm.rs`
- `src/dto/visualize.rs`
- `src/routers/search.rs`
- `src/routers/recall.rs`
- `src/routers/llm.rs`
- `src/routers/visualize.rs`
- `src/auth/superuser.rs`
- `tests/test_search_history.rs`
- `tests/test_search_post.rs`
- `tests/test_recall.rs`
- `tests/test_llm.rs`
- `tests/test_visualize_single.rs`
- `tests/test_visualize_multi.rs`

New (outside the http-server crate):

- `crates/llm/src/prompts/custom_prompt_generation_user.txt`
- `crates/llm/src/prompts/custom_prompt_generation_system.txt`
- `crates/llm/src/prompts/infer_schema_user.txt`
- `crates/llm/src/prompts/infer_schema_system.txt`
- `crates/visualization/tests/test_render_multi_user.rs`
- (Possibly) `crates/visualization/src/multi.rs` — only if Step 11's implementation grows past ~80 lines; otherwise inline into `lib.rs`.

Modified:

- `crates/http-server/src/dto/mod.rs` — add `pub mod {search, recall, llm, visualize};`.
- `crates/http-server/src/routers/mod.rs` — add `pub mod {search, recall, llm, visualize};`.
- `crates/http-server/src/auth/mod.rs` — add `pub mod superuser; pub use superuser::SuperuserOnly;`.
- `crates/http-server/src/error.rs` — add `SearchError`, `RecallError`, `LlmError`, `VisualizeError` variants and the `RecallErrorBody` enum.
- `crates/http-server/src/lib.rs` — `build_router` mounts the four routers; `ApiDoc` registers their paths.
- `crates/http-server/src/openapi.rs` — register the new handlers' `paths` on the `ApiDoc` `#[derive(OpenApi)]` struct.
- `crates/http-server/tests/support/mod.rs` — extend with `seed_dataset(...)`, `MockLlm` (or wire to a feature-gated mock in `crates/llm/`), and superuser-aware fixture helpers.
- `crates/visualization/src/lib.rs` — add `pub async fn render_multi_user(...)`. Possibly also `crates/visualization/src/html.rs` to support the color-by-user mode.
- `crates/llm/src/prompts/mod.rs` (or equivalent) — register the four new templates; possibly add a minimal `render_prompt(name, ctx)` helper if not already present.
- `crates/cognify/src/graph_model.rs` (or wherever the existing graph-model utilities live) — only if `graph_schema_to_graph_model` doesn't already exist. Verify first via `grep -rn graph_schema_to_graph_model crates/cognify/`.
- `docs/http-server/implementation/README.md` — flip P4 status row.
- `docs/http-server/routers/README.md` — flip the `search`, `recall`, `llm`, `visualize` status rows; extend the §3.1 envelope-deviation table to reference the new `ApiError::{SearchError, RecallError, LlmError, VisualizeError}` variants by name.

Out of scope (do NOT touch in this phase):

- Anything under `/api/v1/permissions`, `/api/v1/settings`, `/api/v1/configuration` — those are P5.
- The `PipelineRunRegistry` and the `/cognify` WebSocket — P3 / P3-pre.
- `cognee_lib::api::recall::recall(...)` — the HTTP layer must not call it; the library function stays as-is for SDK consumers.
- Streaming search results, ETag caching for visualize, per-user LLM cost quotas — open-question follow-ups, see [routers/llm.md §6](../routers/llm.md#6-open-questions) and [routers/visualize.md §6](../routers/visualize.md#6-open-questions).
- Cross-SDK HTTP parity tests (`e2e-cross-sdk/harness/test_http_*.py`) — those land per phase in P8 alongside the parity harness; per-router specs already enumerate them but the harness wiring itself is P8.
