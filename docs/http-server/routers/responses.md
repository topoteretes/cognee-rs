# Router: responses

The `responses` router exposes an **OpenAI-compatible Responses API** in front of the cognee tool surface (`add`, `cognify`, `search`, `prune`). A client posts a natural-language `input` plus a tools schema; the server forwards the request to OpenAI's [`/v1/responses` endpoint](https://platform.openai.com/docs/api-reference/responses/create), then dispatches any returned `function_call` items into the matching cognee Python function and folds the results back into an OpenAI-shaped `ResponseBody`. This lets ChatGPT-flavored clients (and tools that already speak the Responses API) drive cognee operations through tool calls without bespoke integration code.

This router is **deferred entirely to Stage B** in the Rust port. Stage A ships a stub that returns `501 Not Implemented` so OpenAPI consumers see the route exist and so SDKs that probe it during boot don't 404. The future-design portion of this doc captures the wire shape and dispatch behavior in enough detail to drop the real implementation in without re-design.

> **Stage A deliverable**: route registered, DTOs defined, `POST /` returns `501` with the documented body. No OpenAI client code shipped.
> **Stage B deliverable**: real OpenAI Responses API integration, function-call dispatch into `cognee_lib::*`, full response envelope.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../observability.md](../observability.md).

## 1. Mount & file

- Mount prefix: `/api/v1/responses`
- Router file: `crates/http-server/src/routers/responses.rs`
- Python source: [`cognee/api/v1/responses/routers/get_responses_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py)
- Python supporting modules:
  - [`cognee/api/v1/responses/models.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/models.py) — Pydantic DTOs.
  - [`cognee/api/v1/responses/dispatch_function.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/dispatch_function.py) — function-call dispatcher.
  - [`cognee/api/v1/responses/routers/default_tools.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/default_tools.py) — default tools schema.

Mount registration: [`cognee/api/client.py:252`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L252) with `tags=["responses"]`.

## 2. Endpoints

One endpoint. Stage A ships only the stub.

### 2.1 `POST /` — create a response (OpenAI-compatible)

#### 2.1.1 Wire contract (target shape, both phases honor envelope)

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `ResponseRequestDTO` — see §4.
- **Response body**:
  - **Stage B**: `200 OK`, `application/json`, `ResponseBodyDTO` — see §4. The `tool_calls` array contains every dispatched cognee function call with its inline result.
  - **Stage A**: `501 Not Implemented`, `application/json`, `{"detail": "OpenAI Responses API surface is not implemented in this build", "code": "RESPONSES_NOT_IMPLEMENTED"}`.

- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `400` | `{"detail": [...], "body": ...}` | Validation error on `ResponseRequestDTO` (e.g. missing `input`, malformed tool definition). |
  | `401` | `{"detail": "Unauthorized"}` | No credential; `REQUIRE_AUTHENTICATION=true`. |
  | `429` (Stage B) | `{"detail": "Upstream OpenAI rate limit exceeded"}` | OpenAI returned 429; we forward as a structured `ApiError::TooManyRequests`. |
  | `502` (Stage B) | `{"detail": "Upstream OpenAI error: <message>"}` | OpenAI returned a non-2xx other than 429, or the upstream connection failed. |
  | `500` (Stage B) | `{"detail": "<message>"}` | Function-dispatch error after the upstream call succeeded. **Note**: per Python's current behavior, dispatch errors are folded *into the response body* (`tool_calls[i].output.status = "error"`) and the HTTP status remains `200`. We replicate that — `500` is reserved for *router-level* failures (DB, panics, framework). |
  | `501` (Stage A only) | `{"detail": "OpenAI Responses API surface is not implemented in this build", "code": "RESPONSES_NOT_IMPLEMENTED"}` | Stub. |

- **Side effects** (Stage B):
  - One outgoing HTTP request to `https://api.openai.com/v1/responses` (or the configured base URL).
  - For each `function_call` in the upstream response, a synchronous in-process call to the matching `cognee_lib::*` function (`add`, `cognify`, `search`, `prune`). These can in turn write to the relational DB, graph DB, vector DB, file storage, embedding engine, and LLM.
  - Costs OpenAI tokens; the response includes a `usage` block.

- **Delegation target**: TBD — Stage B.
  - Stage B introduces `cognee_lib::responses::dispatch(input, tools, tool_choice, temperature, user) -> ResponseBodyDTO`.
  - Internally it calls a thin OpenAI client (reuse the `reqwest`+`rustls` stack already set up for `cognee-llm`) and a `dispatch_function` analogue that pattern-matches function names to cognee facade calls.

- **Validation rules**:
  - `input` is required and non-empty (Pydantic `str` field with no default; matched in Rust by `String` non-`Option`).
  - `model` defaults to `"cognee-v1"` (the only enum variant Python accepts; see §4.1). Other values are rejected at validation time.
  - `tools` is `Option<Vec<ToolFunctionDTO>>`; `None` means "use server default tools" (`DEFAULT_TOOLS`, see §3.2).
  - `tool_choice` is `"auto"` by default; accepts `"auto" | "none" | "required" | {"type": "function", "function": {"name": "..."}}` per OpenAI's schema.
  - `temperature` defaults to `1.0`. Range is not validated by Python; we accept any `f32` and let the upstream reject out-of-range values.
  - `max_completion_tokens` is `Option<u32>`. Forwarded verbatim to the upstream call.
  - `user` is `Option<String>`. Forwarded as the OpenAI `user` field for abuse-tracking.

- **Permission gate**: implicit. The dispatcher uses `get_default_user()` in Python ([`dispatch_function.py:35`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/dispatch_function.py#L35)) — *not* the authenticated user. This is a **known parity quirk**: `tool_calls.search` runs as the default user even when the caller is logged in as someone else. The Rust port mirrors this for compat in Stage B but flags it as an open question (see §6).

- **OpenAPI**: tag `responses`. Stage A: mark with `x-cognee-stub: true`. Stage B: full request/response schemas; security scheme inherited from global config.

- **Telemetry**: span name `cognee.api.responses.create`. Attributes:
  - `cognee.llm.provider = "openai"`
  - `cognee.llm.model` (the actual upstream model used; Python overrides to `"gpt-4o"` regardless of request input — see §3.3)
  - `cognee.responses.tool_call_count` (number of function calls dispatched)
  - `cognee.responses.dispatched_functions` (comma-joined function names)
  - `cognee.llm.usage.prompt_tokens`, `cognee.llm.usage.completion_tokens`, `cognee.llm.usage.total_tokens`

- **Python parity notes**:
  - Python **hard-codes the upstream model to `"gpt-4o"`** ([line 57](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py#L57)) regardless of `request.model`. The `request.model` value is reflected in the response (`response.model = request.model`) but is otherwise ignored. We replicate this in Stage B — same hard-coded model, same value-passthrough in the response.
  - The upstream call uses `client.responses.create(...)`, which is the **new** OpenAI Responses API (not Chat Completions). The shape of `response.output` is a list of items, each with a `type` discriminator (`"message"`, `"function_call"`, etc.). We only inspect `function_call` items.
  - Function dispatch errors are caught and stuffed into the response body — they do not raise. The HTTP status stays `200` even when every tool call errors.
  - The `dispatch_function` resolves the user via `get_default_user()`, **not** via the request's `AuthenticatedUser`. This means tool calls execute under the default user's identity regardless of who initiated the response. Rust matches Python verbatim — Stage B implementation must call `cognee_lib::default_user()` for the dispatch context, not `AuthenticatedUser`.

#### 2.1.2 Stage A stub — exact behavior

```rust
async fn create_response(
    State(_state): State<AppState>,
    user: AuthenticatedUser,            // still enforced — auth is not deferred
    Json(_req): Json<ResponseRequestDTO>,// still validated — bad payloads see 400, not 501
) -> Result<Response, ApiError> {
    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "detail": "OpenAI Responses API surface is not implemented in this build",
            "code":   "RESPONSES_NOT_IMPLEMENTED",
        })),
    ).into_response())
}
```

Auth and request-body validation **are** enforced in Stage A — only the upstream OpenAI call and dispatch are absent. This means a client can integration-test its request shape today and start receiving real responses the day Stage B ships, without changing client code paths.

#### 2.1.3 Stage B — full implementation flow

The handler: (1) validates `ResponseRequestDTO`; (2) builds an upstream request with `model="gpt-4o"` override (see §3.3), `input`, `tools or DEFAULT_TOOLS`, `tool_choice`, `temperature`, `user`, `max_completion_tokens`; (3) `POST`s to `<base>/v1/responses`; (4) iterates the upstream `output` array, dispatching each `type == "function_call"` item per the table in §3.4; (5) folds each result into a `ResponseToolCallDTO` (`output.status = "success"` or `"error"`); (6) assembles `ResponseBodyDTO { id, created, model = request.model, object = "response", status = "completed", tool_calls, usage }` and returns `200 OK`.

## 3. Cross-cutting behavior

### 3.1 Authentication mode

`required`. There is no public surface. The auth mechanism is the standard tri-modal extractor (api-key → bearer → cookie) per [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).

### 3.2 Default tools schema

When the request omits `tools`, the server fills in a built-in `DEFAULT_TOOLS` list with two functions: `search` (params: `search_query` required, `search_type` enum of `CODE|GRAPH_COMPLETION|NATURAL_LANGUAGE`, `top_k`, `datasets[]`) and `cognify` (params: `text` required, `ontology_file_path`, `custom_prompt`). A `prune` tool exists in the file but is commented out as "dangerous". The Rust port serves the same JSON via a `static DEFAULT_TOOLS: Lazy<Value>` built via `serde_json::json!`; a snapshot test asserts byte-equality with Python's serialized form.

Note the **two divergent source files** in the Python tree: [`default_tools.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/default_tools.py) (top-level, 3 tools incl. `prune`) and [`routers/default_tools.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/default_tools.py) (router-local, 2 tools, the one actually imported). The router-local file is the source of truth.

### 3.3 Upstream model override

Python hard-codes the upstream model to `"gpt-4o"` regardless of `request.model` ([line 57](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py#L57)). The request's `CogneeModel` enum has only one variant (`"cognee-v1"`), so the field is a routing key, not a model selector. Stage B keeps the override; an env var to lift the default is open question §6.3.

### 3.4 Function-call dispatch

Stage B implements a single `match` on the function name from the upstream output:

| Function name | Cognee facade call | Result string |
|---|---|---|
| `search` | `cognee_lib::search::SearchOrchestrator::search(...)` (instance method on `SearchOrchestrator`/`SearchBuilder`; no top-level free `search()` function exists). | List of search results, JSON-encoded. |
| `cognify` | optional `cognee_ingestion::AddPipeline::run(text, user)` (re-exported as `cognee_lib::add::AddPipeline`) then `cognee_lib::cognify::cognify(user, ontology_file_path, custom_prompt)`. | Hard-coded success string. |
| `prune` | `cognee_lib::api::prune::prune_data()` (and/or `prune_system()` per Python's `cognee.prune` semantics). | `"Memory has been pruned successfully."` |
| anything else | — | `"Error: Unknown function <name>"` (HTTP 200, in-band). |

The `SearchOrchestrator`, `AddPipeline`, `cognify`, and `prune_*` facades all exist on the Rust side today; Stage B work is wiring, not new functionality. Note that `add` is a *module* in `cognee_lib`, not a function — the corresponding entry point is `AddPipeline::run`.

### 3.5 OpenAI client, sync-only execution, telemetry

- **Client**: reuse the existing `reqwest` (rustls) stack from `cognee-llm`. Do not pull in a third-party "openai" Rust crate — the Responses API surface is small and dependency hygiene matters. Stage B adds `crates/http-server/src/openai/responses_client.rs` with `OpenAiResponsesRequest` and `OpenAiResponsesResponse` types deserialized from OpenAI's wire shape.
- **No background-job mode**: this endpoint has no `run_in_background` flag. Long-running tool calls (e.g. `cognify`) block until completion; timeout-sensitive clients should call `/api/v1/cognify` directly with `run_in_background=true`.
- **Telemetry**: per [../observability.md §5](../observability.md#5-secret-redaction), the redaction layer must scrub the OpenAI bearer token from captured attributes. Concretely: never record `Authorization` headers, and ensure `OPENAI_API_KEY` is not logged when emitting upstream-error spans.

## 4. DTO definitions

### 4.1 Request

```rust
// crates/http-server/src/dto/responses.rs

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use utoipa::ToSchema;

/// Mirrors `cognee.api.v1.responses.models.CogneeModel`.
/// Single-variant enum today; left as an enum so adding variants later is non-breaking.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
pub enum CogneeModelDTO {
    #[serde(rename = "cognee-v1")]
    CogneeV1,
}

impl Default for CogneeModelDTO {
    fn default() -> Self { Self::CogneeV1 }
}

/// Mirrors `cognee.api.v1.responses.models.ResponseRequest`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResponseRequestDTO {
    /// Model selector. Currently only `"cognee-v1"` is accepted.
    #[serde(default)]
    pub model: CogneeModelDTO,
    /// Natural-language input forwarded to the upstream model.
    pub input: String,
    /// Optional tools schema. When `None`, server falls back to `DEFAULT_TOOLS`.
    pub tools: Option<Vec<ToolFunctionDTO>>,
    /// Tool selection policy. Either a string (`"auto" | "none" | "required"`)
    /// or a JSON object specifying a forced tool. Stored as `Value` to match
    /// Python's `Union[str, Dict[str, Any]]`.
    #[serde(default = "ResponseRequestDTO::default_tool_choice")]
    pub tool_choice: Value,
    /// Optional end-user identifier forwarded to OpenAI for abuse-tracking.
    pub user: Option<String>,
    /// Sampling temperature. Forwarded verbatim.
    #[serde(default = "ResponseRequestDTO::default_temperature")]
    pub temperature: f32,
    /// Optional cap on completion tokens. Forwarded verbatim.
    pub max_completion_tokens: Option<u32>,
}

impl ResponseRequestDTO {
    fn default_tool_choice() -> Value { Value::String("auto".into()) }
    fn default_temperature() -> f32 { 1.0 }
}

/// Mirrors `cognee.api.v1.responses.models.ToolFunction`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ToolFunctionDTO {
    /// Always `"function"` per OpenAI's schema.
    #[serde(default = "ToolFunctionDTO::default_kind", rename = "type")]
    pub kind: String,
    pub function: FunctionDTO,
}
impl ToolFunctionDTO {
    fn default_kind() -> String { "function".into() }
}

/// Mirrors `cognee.api.v1.responses.models.Function`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct FunctionDTO {
    pub name: String,
    pub description: String,
    pub parameters: FunctionParametersDTO,
}

/// Mirrors `cognee.api.v1.responses.models.FunctionParameters`. JSON-Schema-shaped.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct FunctionParametersDTO {
    /// Always `"object"` per JSON Schema convention.
    #[serde(default = "FunctionParametersDTO::default_type", rename = "type")]
    pub kind: String,
    pub properties: HashMap<String, Value>,
    pub required: Option<Vec<String>>,
}
impl FunctionParametersDTO {
    fn default_type() -> String { "object".into() }
}
```

### 4.2 Response

```rust
/// Mirrors `cognee.api.v1.responses.models.ResponseBody`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResponseBodyDTO {
    /// Server-generated id; format `resp_<hex>`.
    pub id: String,
    /// Unix epoch seconds at response assembly time.
    pub created: i64,
    /// Echoes the request's `model`. Note Python sends `request.model` here even
    /// though the upstream call hard-codes `"gpt-4o"` — see §3.3.
    pub model: String,
    /// Always `"response"`.
    pub object: String,
    /// Always `"completed"` in Stage B (Python does not differentiate states).
    pub status: String,
    /// One entry per dispatched `function_call` from the upstream output.
    pub tool_calls: Vec<ResponseToolCallDTO>,
    /// Token usage from the upstream call. May be `None` if upstream omits it.
    pub usage: Option<ChatUsageDTO>,
    /// Reserved free-form metadata. Always emitted as `null` today (Python sets
    /// it to `None`); kept on the wire for forward compat.
    pub metadata: Option<HashMap<String, Value>>,
}

/// Mirrors `cognee.api.v1.responses.models.ResponseToolCall`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResponseToolCallDTO {
    pub id: String,
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCallDTO,
    pub output: Option<ToolCallOutputDTO>,
}

/// Mirrors `cognee.api.v1.responses.models.FunctionCall`.
#[derive(Debug, Serialize, ToSchema)]
pub struct FunctionCallDTO {
    pub name: String,
    /// JSON-encoded string. Note: this is a *string of JSON*, not a JSON object.
    /// OpenAI emits it as a string and we forward it as-is so the wire shape
    /// matches.
    pub arguments: String,
}

/// Mirrors `cognee.api.v1.responses.models.ToolCallOutput`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolCallOutputDTO {
    /// `"success"` or `"error"`. Free-form string for forward compat.
    pub status: String,
    /// Free-form payload. Python wraps results as `{"result": <value>}`; we
    /// follow.
    pub data: Option<HashMap<String, Value>>,
}

/// Mirrors `cognee.api.v1.responses.models.ChatUsage`.
#[derive(Debug, Serialize, Deserialize, ToSchema, Default)]
pub struct ChatUsageDTO {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
```

### 4.3 Field-mapping notes

The DTO docstrings in §4.1–4.2 describe each field. A handful of mappings deserve extra callouts because they are wire-load-bearing:

- `type` (Python) → `kind` (Rust) on `ToolFunctionDTO`, `FunctionParametersDTO`, `ResponseToolCallDTO`. Renamed via `#[serde(rename = "type")]` because `type` is a Rust keyword; the wire shape stays `"type": "function" | "object"`.
- `tool_choice` (`Union[str, Dict[str, Any]]`) → `serde_json::Value`. Don't try to model the union as a Rust enum — OpenAI keeps adding shapes, and a `Value` round-trips them safely.
- `FunctionCall.arguments` is a **string of JSON**, not a JSON object. OpenAI emits it that way and clients parse it themselves; we forward bytes-for-bytes.
- `ChatUsage` field names (`prompt_tokens`, `completion_tokens`, `total_tokens`) are the **renamed** versions. OpenAI's Responses API returns `input_tokens` / `output_tokens`; Python's handler ([line 165–168](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py#L165-L168)) maps them. Stage B keeps the rename for client compat — see open question §6.2.
- `ResponseBody.id` falls back to a server-generated `resp_<hex>` only when the upstream omits an id; normally the upstream id is forwarded.
- `ResponseBody.metadata` is `Dict[str, Any] = None` in Python — semantically optional. Use `Option<HashMap<String, Value>>`, emit as `null` when unset.
- All `*_<hex>` factories use `uuid::Uuid::new_v4()` rendered as `format!("{}_{}", prefix, uuid.simple())` to match Python's `uuid.uuid4().hex`.

## 5. Implementation tasks

**Stage A — stub** (lands with the rest of phase 7):

1. Add `crates/http-server/src/dto/responses.rs` with the DTOs in §4 (`#[derive(ToSchema)]`).
2. Add `crates/http-server/src/routers/responses.rs` with a single `POST /` handler that returns `501` per §2.1.2. Auth is enforced and request validation still runs — bad payloads see `400`, not `501`.
3. Embed `DEFAULT_TOOLS` as a `static Lazy<Value>` (snapshot-tested against Python's serialized form).
4. Add OpenAPI annotations (`tag = "responses"`, `x-cognee-stub: true`).
5. Tests in `crates/http-server/tests/test_responses.rs`: DTO round-trip; stub returns 501 with exact body; auth still 401; bad body still 400.

**Stage B — real implementation**:

6. Add `crates/http-server/src/openai/responses_client.rs` with `OpenAiResponsesClient` (`reqwest` + `rustls`). Base URL configurable via `OPENAI_API_BASE` (default `https://api.openai.com/v1`). Define internal wire types separately from the public DTOs.
7. Add `crates/http-server/src/dispatch/responses_dispatch.rs` that matches function names per §3.4 and folds outcomes into `ResponseToolCallDTO`. Resolve `user` via `default_user` for parity (open question §6.4).
8. Wire `cognee_lib::responses::create_response(...)` through the facade so other consumers (MCP, embedders) can reuse the dispatcher.
9. Replace the 501 stub. Upstream error mapping: 429 → `TooManyRequests`; other non-2xx and connect/timeout errors → `BadGateway`. Function-dispatch errors fold into `tool_calls[i].output.status = "error"`; HTTP stays 200.
10. Tests: `wiremock`/`httpmock` mock OpenAI; assert request shape, dispatch, response assembly, error folding, unknown-function fallback string. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_responses.py` against a shared mock upstream.

## 6. Open questions

1. **Stage B timing** — when does the OpenAI Responses integration justify the dependency surface? Stage A stub buys us time, but the frontend roadmap may demand it sooner than the rest of Stage B ships. Decide before merging this doc whether Stage B is a single milestone or splits into "stub + DTOs landed" (now) and "real upstream" (later).
2. **`ChatUsage` field renaming** — Python receives `input_tokens` / `output_tokens` from OpenAI's Responses API and renames them to `prompt_tokens` / `completion_tokens` on output. This is a hold-over from Chat Completions. Should Stage B keep the rename for client-compat, or pass through the new names? **Proposed**: keep rename; document the divergence from raw OpenAI output.
3. **Hard-coded `gpt-4o`** — Python hard-codes the upstream model. Rust matches: hard-coded `"gpt-4o"`, no `COGNEE_RESPONSES_UPSTREAM_MODEL` env var. Operators wanting to change it must rebuild from source — same constraint Python deployments have.
4. **Default tools schema source of truth** — Python has *two* `default_tools.py` files (top-level and router-local) with diverging contents; the router-local one wins at runtime. Rust matches that runtime behavior by porting the router-local file's contents into a single Rust constant. The duplicate Python file's contents are not used at runtime by Python either, so the wire behavior is identical. (Internal consolidation, not a wire divergence.)
5. **Dispatcher `user` resolution** — Python uses `get_default_user()`, ignoring the authenticated request user. Rust matches verbatim: tool calls execute under the default user's identity. No warning span attribute, no behavior change. Strict wire and side-effect parity.
6. **OpenAI bearer token leakage** — the upstream client must not log the `Authorization: Bearer sk-...` header at any level. The redaction layer in [observability.md §5](../observability.md#5-secret-redaction) handles attribute redaction, but stdout `reqwest` debug logs (when `RUST_LOG=trace`) bypass it. Should Stage B disable `reqwest` trace logging unconditionally, or rely on operator discipline?

## 7. References

- Python router: [`cognee/api/v1/responses/routers/get_responses_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py)
- Pydantic models: [`cognee/api/v1/responses/models.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/models.py)
- Function-call dispatcher: [`cognee/api/v1/responses/dispatch_function.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/dispatch_function.py)
- Default tools (router-local — the file actually imported by the router): [`cognee/api/v1/responses/routers/default_tools.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/default_tools.py)
- Default tools (top-level — *not* used at runtime, kept for reference): [`cognee/api/v1/responses/default_tools.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/default_tools.py)
- Mount registration: [`cognee/api/client.py:252`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L252)
- OpenAI Responses API reference: [https://platform.openai.com/docs/api-reference/responses/create](https://platform.openai.com/docs/api-reference/responses/create)
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions)
- Plan reference (phase 7 / advanced): [../plan.md §7](../plan.md#7-implementation-phases) and [../plan.md §10 Q6](../plan.md#10-risks--open-questions)
- Auth extractor: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution)
- Telemetry conventions: [../observability.md §3.3-3.4](../observability.md#33-span-instrumentation-conventions)
