# Router: users — get-user-id (by email)

A single-endpoint router that resolves an email address to a user's UUID. Used by admin tooling, the permissions UI, and the cross-SDK e2e harness when one SDK needs to look up a user provisioned by the other. Mounted at `/api/v1/users` alongside the fastapi-users CRUD ([users.md](users.md)).

The endpoint has its own dedicated router file in Python because it is a cognee-specific addition — fastapi-users does not provide a "get id by email" lookup.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [users.md](users.md), [auth.md](auth.md).

## 1. Mount & file

- Mount prefix: `/api/v1/users` (path: `/get-user-id`).
- Router file: `crates/http-server/src/routers/users_by_email.rs`.
- Python source: [`cognee/api/v1/users/routers/get_user_id_by_email_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py).
- Mounted in: [`cognee/api/client.py:264-268`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L264-L268) — `app.include_router(get_user_id_by_email_router(), prefix="/api/v1/users", tags=["users"])`.
- Lookup helper: [`cognee/modules/users/methods/get_user_id_by_email.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_user_id_by_email.py) — selects `User.id` where `User.email == email`, returns `None` on miss.
- DTO base class: [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py) — `InDTO` is the cognee-internal Pydantic base for input bodies; equivalent to a plain `BaseModel` for our purposes.

## 2. Endpoints

### 2.1 `POST /get-user-id` — resolve email to UUID

- **Auth**: `required` (`AuthenticatedUser`). Source: [`get_user_id_by_email_router.py:18`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L18). Note: any active user can call this — there is no superuser gate. This is intentional; the endpoint is used by the frontend to look up tenant members by email.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `UserEmailRequestDTO`. Pydantic source: [`get_user_id_by_email_router.py:10-11`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L10-L11):

  ```python
  class UserEmailRequest(InDTO):
      email: EmailStr
  ```

  | JSON field | Type | Required | Notes |
  |---|---|---|---|
  | `email` | `String` (RFC 5322) | yes | Validated by Pydantic `EmailStr`. |

- **Response body** (`200 OK`, `application/json`):

  ```json
  {"user_id": "<uuid>"}
  ```

  Source: [`get_user_id_by_email_router.py:24`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L24). The UUID is serialized as a lowercase, hyphenated string (Python's `str(UUID)`); Rust must emit the same format. `serde_json` of `uuid::Uuid` already produces this shape.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `404` | `{"detail": "User not found"}` | No user with that email. The literal string `"User not found"` is part of the wire contract. Source: [`get_user_id_by_email_router.py:21-22`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L21-L22). |
  | `400` | `{"detail": [{"loc":[...],"msg":"...","type":"..."}], "body": {...}}` | Missing email field, malformed JSON, or email syntax invalid. From the global `RequestValidationError` handler in [`client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176). |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**: pure read.
- **Delegation target**: `state.lib.user_repo().find_id_by_email(email).await`. Returns `Result<Option<Uuid>, DbError>`. Handler maps:
  - `Ok(Some(id))` → `200 {"user_id": id}`
  - `Ok(None)` → `404 {"detail": "User not found"}`
  - `Err(e)` → `500 {"detail": "..."}`
- **Validation rules**:
  - `email`: parses as `EmailStr` (we use the same loose RFC 5322 interpretation as Pydantic; `email_address::EmailAddress` or a thin regex matching).
  - **Case sensitivity**: Python's SQL `WHERE email = :email` is case-sensitive on most engines (PostgreSQL exact match, SQLite collates by default). If the DB stores `Alice@Example.com` and the request sends `alice@example.com`, no row is returned. fastapi-users does not normalize the case. We replicate exactly — no `LOWER()` in the query. Track as an open question; many production deployments expect case-insensitive lookup.
- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack). Phase-1: no per-IP rate limit. This endpoint is a *small* enumeration vector (an attacker with a valid auth credential can probe whether emails exist); documented as an open question.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:get_user_id_by_email`. Response schema: `GetUserIdResponseDTO`. 404 example with the exact "User not found" string.
- **Telemetry**: span `cognee.api.users.get_user_id_by_email`. Attributes:
  - `result = "found" | "not_found"`
  - `user.id` (caller)
  - **Never log the requested email** — even though the caller has it, putting it in spans pollutes the buffer with PII. The `result` boolean is enough to debug.
- **Python parity notes**:
  - The 404 detail is the exact string `"User not found"` (capital U, two words). Do not use the canonical `"Not Found"` shape that comes from `HTTPException(status_code=404)` without a `detail`.
  - The `str(user_id)` cast at [`get_user_id_by_email_router.py:24`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L24) emits a lowercase hyphenated UUID; serde's default for `uuid::Uuid` matches.
  - The `body.email` access at [`get_user_id_by_email_router.py:19`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py#L19) calls `str(body.email)` — Pydantic's `EmailStr` is itself a `str` subclass, so the cast is a no-op. We treat `email` as a `String` directly.

## 3. Cross-cutting behavior

- **Authentication**: `AuthenticatedUser` only. No tenant gate. This is a read-only lookup; any authenticated user can probe for any other user's UUID via their email, which is consistent with the email being treated as a non-secret identifier.
- **No tenant scoping**: a user in tenant A can find a user in tenant B if they know their email. Document — Python behaves the same. Some deployments may want a tenant filter; defer to a follow-up.
- **Error envelope**: standard string `detail`. No structured shapes.
- **`UserEmailRequestDTO` reuse**: this DTO appears nowhere else in the cognee API. Keep it local.
- **`Uuid` serialization**: `serde_json` of `uuid::Uuid` yields a lowercase hyphenated string (e.g. `"0193b0f1-ea2c-7000-8000-000000000001"`). Matches Python's `str(uuid)`. Confirmed; no custom serializer needed.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/users_by_email.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/users/get-user-id`.
/// Pydantic source: `UserEmailRequest(InDTO)` in
/// `get_user_id_by_email_router.py:10-11`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UserEmailRequestDTO {
    pub email: String,
}

/// Response body for `POST /api/v1/users/get-user-id`.
/// Wire shape: `{"user_id": "<uuid>"}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetUserIdResponseDTO {
    pub user_id: Uuid,
}
```

No `#[serde(rename_all = "snake_case")]` needed — both fields are already snake_case.

## 5. Implementation tasks

1. Add `UserEmailRequestDTO` and `GetUserIdResponseDTO` in `crates/http-server/src/dto/users_by_email.rs`.
2. Extend the `UserRepository` trait in `cognee-database` with `find_id_by_email(email: &str) -> Result<Option<Uuid>, DbError>` if it does not already exist (it likely does — the auth login path needs the same query). Reuse if present.
3. Add the handler in `crates/http-server/src/routers/users_by_email.rs`:
   ```rust
   async fn post_get_user_id(
       State(state): State<AppState>,
       _user: AuthenticatedUser,
       Json(body): Json<UserEmailRequestDTO>,
   ) -> Result<Json<GetUserIdResponseDTO>, ApiError> {
       match state.lib.user_repo().find_id_by_email(&body.email).await? {
           Some(user_id) => Ok(Json(GetUserIdResponseDTO { user_id })),
           None => Err(ApiError::NotFound("User not found".to_owned())),
       }
   }
   ```
4. Wire under `/api/v1/users` in `build_router` alongside the fastapi-users CRUD router. Mount path: `.route("/get-user-id", post(post_get_user_id))` on the users router (or a small dedicated subrouter combined with the CRUD one — see [../architecture.md §7](../architecture.md#7-router-composition)).
5. Add OpenAPI annotations:
   - `tags = ["users"]`, `security = [...]` (default global auth).
   - 200 response example: `{"user_id": "0193b0f1-ea2c-7000-8000-000000000001"}`.
   - 404 response example: `{"detail": "User not found"}`.
6. Add unit tests:
   - Happy path: insert a user with email `alice@example.com`, hit the endpoint, assert 200 and matching UUID.
   - Missing user: hit with an unknown email, assert 404 + exact `"User not found"` body.
   - Malformed email: hit with `"not-an-email"`, assert 400 with structured `detail` array (Pydantic-style).
   - Missing field: hit with `{}`, assert 400.
   - Unauthenticated: hit with no credential when `REQUIRE_AUTHENTICATION=true`, assert 401.
   - Case-mismatch: insert `Alice@Example.com`, look up `alice@example.com`, assert **404** (case-sensitive parity with Python).
7. Add integration tests in `crates/http-server/tests/test_users_by_email.rs` covering the same matrix against a SQLite-backed `AppState`.
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_users_by_email.py`: Python provisions a user, Rust resolves their UUID; vice versa.

## 6. Open questions

1. **Case sensitivity**. Python's exact-match query is case-sensitive on most relational engines. Real-world deployments often want case-insensitive lookup so `Alice@Example.com` and `alice@example.com` resolve to the same user. Phase-1: match Python (case-sensitive). Phase-2: normalize emails to lowercase at storage time and at lookup; coordinated change with Python.
2. **Enumeration vector**. An authenticated user can probe arbitrary emails. Within a single-tenant deployment this is a non-issue (everyone trusts everyone). In a multi-tenant SaaS-shaped deployment, this leaks "user X exists in this system" across tenants. Defer; mitigate with rate limiting (e.g. 100 lookups / 5 min per caller) when multi-tenant SaaS lands.
3. **Tenant filter**. Should the endpoint constrain results to users visible in the caller's current tenant? Python does not; we match. Phase-2: optional `?tenant_scope=current` query param.
4. **GET vs POST**. Using POST for an idempotent lookup is unidiomatic — GET with a query param (or a path param) is more REST-shaped. Python chose POST to avoid putting emails in URLs / logs. Replicate.
5. **Suppress 404 vs return null**. An alternative wire shape would be `200 {"user_id": null}` on miss, avoiding the need for clients to handle 404s differently. Python uses 404; we replicate. Tracked for future consistency cleanup.
6. **`InDTO` semantics**. cognee's `InDTO` adds `model_config = ConfigDict(extra="forbid")` (rejects unknown fields). Our serde `Deserialize` with `#[serde(default)]` (only on the optional field, which there isn't here) accepts unknown fields silently. To match `extra="forbid"`, we must add `#[serde(deny_unknown_fields)]` to the struct. Confirm `InDTO` actually does this — code lives at [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py).

## 7. References

- Python router: [`cognee/api/v1/users/routers/get_user_id_by_email_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_user_id_by_email_router.py)
- Python lookup helper: [`cognee/modules/users/methods/get_user_id_by_email.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_user_id_by_email.py)
- Mount: [`cognee/api/client.py:264-268`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L264-L268)
- DTO base class: [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py)
- Companion auth doc: [../auth.md §8.12](../auth.md#812-apiv1usersget-user-id--post)
- Sibling routers: [users.md](users.md), [auth.md](auth.md)
- Tenants schema: [../tenants.md §3.2](../tenants.md#32-users)
- Validation handler: [../architecture.md §10](../architecture.md#10-request-validation)
