# Router: checks

Cloud connection validation. The frontend's "is my Cognee Cloud API key still good?" check. Forwards a caller-supplied `X-Api-Key` header to the cloud control-plane's `/api/api-keys/check` endpoint and surfaces success or failure.

This is the smallest router in the server (one endpoint) and exists primarily so the UI can light up a "connected to cloud" indicator without asking the user to attempt a real cloud operation.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md).

## 1. Mount & file
- Mount prefix: `/api/v1/checks`
- Router file: `crates/http-server/src/routers/checks.rs`
- Python source: [`cognee/api/v1/cloud/routers/get_checks_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cloud/routers/get_checks_router.py).
- Underlying cloud probe (Python): [`cognee/modules/cloud/operations/check_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py).
- Underlying cloud client (Rust): [`crates/cloud/`](../../crates/cloud/) — extended for this router.

## 2. Endpoints

### 2.1 `POST /api/v1/checks/connection` — validate a cloud API key

Pings the cloud control-plane with the API key supplied in the `X-Api-Key` request header. If the cloud responds 200, the local server returns 200 with no body. Any other response (including non-success status from the cloud, network failure, TLS error) becomes a `503 Service Unavailable` carrying the underlying message.

- **Auth**: `required` (`AuthenticatedUser`). Note: this is the *local* server's auth — every endpoint is gated. The `X-Api-Key` header that the handler reads is **separate** from any local `X-Api-Key` auth backend; it carries the *cloud* API key, not a local one. See Python parity notes.
- **Path params**: none.
- **Query params**: none.
- **Request body**: none. Caller must supply the `X-Api-Key: <cloud-key>` HTTP header. Empty body is intentional — Python uses `Request` directly to extract the header ([Python L13–L15](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cloud/routers/get_checks_router.py#L13-L15)).

  Headers consumed:
  | Header | Required | Notes |
  |---|---|---|
  | `X-Api-Key` | yes | The cloud API key being validated. Forwarded verbatim to the cloud probe. |

- **Response body**:
  - **Success (`200 OK`)**: empty body. Python returns `await check_api_key(api_token)` which evaluates to `None` ([`check_api_key.py:19`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py#L19) — early `return` with no value). FastAPI serializes `None` as a `null` JSON literal. We match: response body is the literal `null` (4 bytes), `Content-Type: application/json`. Open Question: do we instead use `204 No Content` for a cleaner shape? Lean: **no, match Python's `200 null`**.
  - There is no JSON envelope on success.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "Failed to connect to the cloud service. Please add your API key to local instance.", "name": "CloudApiKeyMissingError"}` | `X-Api-Key` header missing or empty. **Python parity**: `CloudApiKeyMissingError` ([Python L18–L19](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cloud/routers/get_checks_router.py#L18-L19), [`CloudApiKeyMissingError.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudApiKeyMissingError.py)) — status 400, name baked into the body. |
  | `401` | `ApiError::Unauthorized` | The *local* server's auth failed — the caller's `Authorization`/cookie/local API key is missing or invalid. Distinct from the cloud-side check. |
  | `503` | `{"detail": "Failed to connect to the cloud service. Please check your cloud API key in local instance.", "name": "CloudConnectionError"}` | Cloud responded non-200 OR network/TLS error. **Python parity**: `CloudConnectionError` ([`CloudConnectionError.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudConnectionError.py)) — status `HTTP_503_SERVICE_UNAVAILABLE`. The default `message` is the static string above; Python's `check_api_key` overrides it with `f"Failed to connect to cloud instance: {response.status} - {error_text}"` ([check_api_key.py L23–L25](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py#L23-L25)). The wire-level `detail` therefore contains the underlying status and body. We replicate this concatenation. |

  Both `CloudApiKeyMissingError` and `CloudConnectionError` derive from `CogneeConfigurationError`, which is a `CogneeApiError`. FastAPI's exception handler emits them as `{"detail": "<message>", "name": "<class name>"}` ([`client.py:179-195`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L179-L195)) — note the inclusion of `name`, which most other endpoints do not surface. The Rust side reproduces this shape via a new `ApiError` variant or by returning a tuple directly with the matching JSON body.

- **Side effects**: **none on local state**. The handler:
  1. Reads `X-Api-Key` from the request.
  2. Issues an outbound `POST <COGNEE_CLOUD_URL>/api/api-keys/check` with the same `X-Api-Key` header. Python hardcodes the URL to `http://localhost:8001` ([check_api_key.py L8](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py#L8)) — we read it from the existing `COGNEE_CLOUD_URL` env (see [`crates/cloud/src/config.rs`](../../crates/cloud/src/config.rs) and [`cognee_cloud::config::cloud_url`](../../crates/cloud/src/lib.rs)).
  3. Returns success or `CloudConnectionError`. No DB writes, no file writes, no graph writes.
- **Delegation target**: `cognee_cloud::operations::check_api_key(api_key: &str) -> CloudResult<()>` — a new free function inside the existing `cognee-cloud` crate. Mirrors Python's [`cognee/modules/cloud/operations/check_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py). Reuses the existing TLS context (`reqwest::Client::builder().use_rustls_tls()`) already set up for `cognee_cloud::management_api`. The handler is a one-liner over this function plus header parsing.
- **Validation rules**: `X-Api-Key` non-empty; otherwise `CloudApiKeyMissingError`. Python does **not** validate the *format* of the key (no regex, no length check) — neither do we. The cloud side decides whether the key is valid.
- **Rate / size limits**: empty request body; default body limit is irrelevant. No outbound rate limit; one upstream request per call. The cloud-side `/api/api-keys/check` is the rate-limited surface; we don't add a local layer.
- **Permission gate**: **none beyond local auth**. Any authenticated user can probe an arbitrary cloud key. This is intentional — the endpoint's purpose is "is the key the user *just typed in* valid?". The user supplies the key in the request, so there's no privilege-escalation surface.
- **OpenAPI**: tag `["Cloud Checks"]`. Request: no body; declares header parameter `X-Api-Key: string` as required. Response: `200` with `application/json` `null`. Errors: `400`, `401`, `503` per the table. Security: `[BearerAuth, ApiKeyAuth, CookieAuth]` for the *local* auth (the cloud `X-Api-Key` is documented as a header parameter, not a security scheme — to avoid OpenAPI confusing the local and cloud key shapes).
- **Telemetry**: span `cognee.api.checks.connection`. Attributes:
  - `cognee.cloud.url` — the cloud control-plane URL probed (e.g. `https://cloud.cognee.ai`).
  - `cognee.cloud.status` — the upstream HTTP status (set on both success and failure paths).
  - `cognee.user.id` — the *local* caller's id.
  - **Not set**: `cognee.cloud.api_key` — the redaction layer would catch it ([../observability.md §5](../observability.md#5-secret-redaction)) but we never put it on a span in the first place. The span builder explicitly skips it.

  The `X-Api-Key` value is excluded from access logs by `tower_http::trace::TraceLayer`'s default header filter — but since we control the layer config, we add `X-Api-Key` to the redacted-headers list for belt-and-braces. See [../observability.md §7](../observability.md#7-access-logging).
- **Python parity notes**:
  - **Two `X-Api-Key` semantics**: Python's [`api_key_backend`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_fastapi_users.py) uses the `X-Api-Key` header as a *local* auth backend, but this endpoint reads `request.headers.get("X-Api-Key")` directly to grab the *cloud* key ([Python L16](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cloud/routers/get_checks_router.py#L16)). The same header is therefore overloaded:
    - If the caller is authenticating to the *local* server with an `X-Api-Key`, that same header value is also forwarded to the cloud probe.
    - If the caller is authenticating with bearer or cookie auth, the `X-Api-Key` header carries only the cloud key.

    This is fine in practice (a local API key happens to be a valid cloud API key in cognee Cloud deployments where the local instance *is* the cloud instance, which is the test setup). The Rust extractor `AuthenticatedUser` accepts the `X-Api-Key` header as a local credential; the handler then re-reads the same header for the cloud probe. Behavior matches Python byte-for-byte. **Open Question**: should we differentiate via a separate `X-Cloud-Api-Key` header to avoid the overload? Defer; not in scope.

  - The 503 body's `detail` field is a concatenation: `"Failed to connect to cloud instance: 401 - <error_text>"`. The `name` field is `CloudConnnectionError` — note the **typo** (three "n"s in "Connnection") in [`CloudConnectionError.py:11`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudConnectionError.py#L11). For wire-format byte-for-byte parity we replicate the typo as `"CloudConnnectionError"` (sic). We document the typo in the Rust code with a `// sic — Python typo replicated for wire parity` comment.

  - Python's `check_api_key` swallows all exceptions in a single `except Exception as e` and re-raises as `CloudConnectionError` ([check_api_key.py L27–L28](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py#L27-L28)). Network-level errors (TLS, DNS, connection refused) all surface the same way: 503 with `detail = "Failed to connect to cloud instance: <repr of the exception>"`. Rust replicates: `reqwest::Error → CloudError::Http(...)` → wrapped into the same shape.

  - Python's hardcoded `cloud_base_url = "http://localhost:8001"` ([check_api_key.py L8](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py#L8)) is **not** parity-load-bearing; production deployments rely on env-driven cloud URLs (which is what the `cognee-cloud` crate already does). We use `cognee_cloud::config::cloud_url()` here. Cross-SDK parity tests run with the same `COGNEE_CLOUD_URL` set on both stacks.

## 3. Cross-cutting behavior

This is a single-endpoint router. The shared concerns are:

- **Header redaction**: `X-Api-Key` is added to the access-log redacted-headers list ([../observability.md §7](../observability.md#7-access-logging)) so the cloud key never reaches stdout logs.
- **Outbound TLS**: reuses `cognee-cloud`'s rustls-backed `reqwest::Client`. No new TLS configuration is introduced for this endpoint. CA bundle and proxy settings come from the same env vars `cognee-cloud` consumes.
- **Cloud URL resolution**: `cognee_cloud::config::cloud_url()` — env (`COGNEE_CLOUD_URL`), defaulting to the package default (today `https://cloud.cognee.ai`). The single Python hardcoded URL is **not** preserved.
- **No write side effects**: this router never touches the relational DB, graph DB, vector DB, file storage, broadcast channels, or registries.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/checks.rs

use serde::Serialize;
use utoipa::ToSchema;

/// 400 / 503 body shape — mirrors `CogneeApiError` JSON
/// (https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L179-L195).
///
/// Differs from the canonical `ApiError` envelope because Cognee's
/// configuration errors include the exception class name as a top-level
/// `name` field. We expose this DTO instead of synthesizing it inside
/// `ApiError::IntoResponse` to keep the parity contract explicit.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CloudConfigErrorDTO {
    /// Human-readable message. For `CloudConnectionError` this is the
    /// "Failed to connect to cloud instance: <status> - <body>"
    /// concatenation produced by `check_api_key`.
    pub detail: String,
    /// Exception class name. Values:
    ///   - "CloudApiKeyMissingError" (400)
    ///   - "CloudConnnectionError"   (503)  ← sic, Python typo replicated
    pub name: String,
}
```

There is no request DTO (the body is empty). There is no success DTO (the body is `null`).

## 5. Implementation tasks

1. Add `cognee_cloud::operations::check_api_key(api_key: &str) -> CloudResult<()>` in the existing `cognee-cloud` crate. Internally calls `POST {cloud_url}/api/api-keys/check` with `X-Api-Key: <api_key>`; maps non-200 to `CloudError::Unauthorized` or `CloudError::ManagementApi { status, body }` depending on the upstream status; maps `reqwest` errors to `CloudError::Http`.
2. Add `CloudConfigErrorDTO` in `crates/http-server/src/dto/checks.rs` with the field-by-field shape above (including the typo'd `CloudConnnectionError` value).
3. Add `crates/http-server/src/routers/checks.rs` with one `post_connection` handler:
   - Extract `X-Api-Key` via `axum::http::HeaderMap` or a custom `OptionalHeader<&'static str>` extractor.
   - On missing/empty: return `(StatusCode::BAD_REQUEST, Json(CloudConfigErrorDTO { detail: "Failed to connect to the cloud service. Please add your API key to local instance.".into(), name: "CloudApiKeyMissingError".into() }))`.
   - Otherwise call `cognee_cloud::operations::check_api_key(api_key).await`. Map `Ok(())` to `Json(serde_json::Value::Null)`; map `Err(e)` to a `503` with the concatenated `detail` string and `"CloudConnnectionError"` (sic) as `name`.
4. Register the router in `crates/http-server/src/lib.rs`'s `build_router` under `.nest("/checks", checks::router())`. Already in [../architecture.md §7](../architecture.md#7-router-composition).
5. Add `#[utoipa::path(...)]` annotation. Document the `X-Api-Key` header parameter as required.
6. Add `X-Api-Key` to the access-log redacted-headers list in `crates/http-server/src/middleware/trace.rs`.
7. Unit tests:
   - Missing header → 400 with `CloudApiKeyMissingError`.
   - Empty header → 400 with `CloudApiKeyMissingError`.
   - Mock cloud returns 200 → handler returns 200 with `null` body.
   - Mock cloud returns 401 → handler returns 503 with `detail` containing `"401"` and the upstream error text; `name = "CloudConnnectionError"` (typo verified).
   - Mock cloud refuses connection → handler returns 503 with `detail` mentioning the underlying error.
8. Integration test in `crates/http-server/tests/test_checks.rs` driving the full router via `tower::ServiceExt::oneshot`. Use `mockito::Server` for the cloud upstream (already a dev-dependency in `cognee-cloud`).
9. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_checks.py`: stand up a single `mockito` cloud and POST against both Python and Rust local servers; assert equal status codes and JSON bodies (modulo whitespace).

## 6. Open questions

1. **`200 null` vs `204 No Content`**: Python returns `None` which FastAPI serializes as `null`. `204` would be cleaner but breaks parity. Lean: **keep `200 null`** for parity; document.
2. **Replicate Python's typo `CloudConnnectionError`?**: yes for byte-for-byte parity. Lean: **replicate**, mark `// sic` in code, lock down via cross-SDK test. If Python ever fixes the typo (likely a single-character PR upstream), we'll mirror.
3. **`X-Api-Key` overload**: same header used for local API-key auth *and* for forwarding the cloud key. Should we add `X-Cloud-Api-Key` as a clearer variant (with `X-Api-Key` still accepted)? Lean: **defer** — out of scope for parity work; revisit alongside the broader cloud-auth refactor.
4. **Cloud URL source**: Python hardcodes `http://localhost:8001`; we use `cognee_cloud::config::cloud_url()` (env-driven). Should the parity tests configure both Python and Rust with the same env, or should we replicate Python's hardcode for tests? Lean: **configure both via env** (`COGNEE_CLOUD_URL=http://mock`); the hardcode is a Python bug, not a contract.
5. **Should the router live under `/api/v1/cloud/checks` instead of `/api/v1/checks`?**: Python's source path is `cognee/api/v1/cloud/routers/get_checks_router.py` (note `cloud/` segment in the source tree) but the mount in `client.py` is `/api/v1/checks` (no `cloud/`). Mount path matches Python; we keep `/api/v1/checks`. No action needed.

## 7. References

- Python router: [`cognee/api/v1/cloud/routers/get_checks_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cloud/routers/get_checks_router.py).
- Python cloud probe: [`cognee/modules/cloud/operations/check_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/operations/check_api_key.py).
- Python exceptions: [`cognee/modules/cloud/exceptions/CloudConnectionError.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudConnectionError.py), [`CloudApiKeyMissingError.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudApiKeyMissingError.py).
- Existing Rust cloud crate: [`crates/cloud/src/lib.rs`](../../crates/cloud/src/lib.rs).
- Cloud URL resolver: [`crates/cloud/src/config.rs`](../../crates/cloud/src/config.rs).
- Auth extractor (the local-side gate): [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Per-router README and template: [README.md](README.md).
