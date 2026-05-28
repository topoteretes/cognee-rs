# Gap 08 — POST /api/v1/forget cloud proxy

## Source / current state

- Handler: [crates/http-server/src/routers/forget.rs](../../../../crates/http-server/src/routers/forget.rs).
- Live marker, L57:
  ```rust
  // TODO(cloud): proxy via cloud client when state.lib.cloud_client.is_some()
  ```
- Above that marker (L36-L55) the handler validates the payload (`resolve_mode`), pulls `ComponentHandles` from `state.components()`, clones `database` + `delete_service`, and falls straight into the three local-only `match mode { … }` arms (L59-L202). All three arms call `delete_service.execute(…)` from `cognee_delete::DeleteService`.
- `state.lib` is `Option<Arc<ComponentHandles>>` (see `state.rs` L52). `ComponentHandles` (`components.rs` L32-L120+) currently has **no** `cloud_client` slot — the TODO mentions a field that does not yet exist.
- The Python reference (`/tmp/cognee-python/cognee/api/v1/forget/forget.py` L95-L106) implements the same branching: it asks `get_remote_client()` for an `Optional[CogneeCloudClient]`, and when present it forwards the call (`await client.forget(data_id=…, dataset=…, everything=…, memory_only=…)`) and returns the upstream JSON unchanged. Otherwise it falls through to the local path. The cloud client itself is in `/tmp/cognee-python/cognee/api/v1/serve/cloud_client.py` L297-L317 — a thin `aiohttp` `POST {service_url}/api/v1/forget` wrapper that re-raises `RuntimeError(f"Remote forget failed ({resp.status}): {body}")` on `>=400`. Notice Python *does* leak the upstream status and body, which we explicitly **do not** want to copy.
- Existing tests ([crates/http-server/tests/test_forget.rs](../../../../crates/http-server/tests/test_forget.rs)) cover the local-only path: auth guard (401), cross-field validation (422), and the three mode-resolution smoke tests. No test exercises a `cloud_client = Some(…)` configuration because no slot exists yet.

**Net assessment.** The local path is correct and exercised. The TODO is dead-code: nothing in the codebase ever sets `cloud_client` because no such field exists. This gap is *adding a new feature*, not closing a regression.

## Severity classification

**Low priority — future feature, not a regression.**

- Local delete works end-to-end (covered by `test_forget.rs` + `crates/delete/src/lib.rs` integration tests).
- No Rust-side deployment currently runs as a "thin client in front of a cloud cognee" — this is a Python-only deployment topology today.
- Closing this gap with no concrete cloud target risks a trait whose shape is later wrong.
- Recommendation: ship Stage A (trait + slot + handler branch, with no real impl) only when at least one embedder asks for it. Stage B (real HTTP impl) is a separate, opt-in follow-up.

If we decide to defer entirely, the only required change is renaming/clarifying the L57 TODO so it doesn't mislead readers into thinking a `cloud_client` field already exists. That is a one-line doc fix and orthogonal to the rest of this plan.

## Strategy

We mirror Python's "remote-or-local" runtime switch with a single trait object slot on `ComponentHandles`:

- A new trait `CloudDeleteClient` (`Send + Sync + 'static`) defining one async method: `forward_forget`.
- A new optional slot `cloud_client: Option<Arc<dyn CloudDeleteClient>>` on `ComponentHandles` (alongside `delete_service`).
- The handler checks the slot **after** `resolve_mode()` succeeds (so payload validation stays consistent across both paths) but **before** any DB / permission work. When set, it forwards and returns the upstream response. When unset, the existing local path runs unchanged.

### Where the trait lives

Host the trait in **`crates/http-server/src/cloud_client.rs`**, not a new crate.

Rationale:
- Only one consumer today (`forget.rs`). Other cloud-proxy gaps (`/remember`, `/recall`, `/improve`) could land later, but until then a dedicated `cognee-cloud-client` crate is overhead with no payoff.
- The trait's request / response types are *exactly* the handler-level DTOs (`ForgetPayloadDTO`, `ForgetResponseDTO`) plus `AuthenticatedUser`. Putting the trait next to its DTOs avoids re-exporting them through a new crate or duplicating their shapes.
- The actual `HttpCloudDeleteClient` impl (Stage B) *can* live in a new crate later without breaking the trait — moves are cheap once the surface is stable.

### Trait shape

```rust
// crates/http-server/src/cloud_client.rs (NEW)
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

use crate::auth::AuthenticatedUser;
use crate::dto::forget::{ForgetPayloadDTO, ForgetResponseDTO};

#[derive(Debug, Error)]
pub enum CloudClientError {
    /// Upstream returned a 4xx/5xx. The contained message is already
    /// scrubbed (no URLs, no auth headers, no upstream response bodies).
    #[error("cloud forget upstream returned an error")]
    Upstream { status: u16 },

    /// Transport/connection/timeout failure. No URL or token leakage.
    #[error("cloud forget upstream is unreachable")]
    Unreachable,

    /// Upstream response could not be parsed into ForgetResponseDTO.
    #[error("cloud forget upstream returned a malformed response")]
    MalformedResponse,
}

#[async_trait]
pub trait CloudDeleteClient: Send + Sync + 'static {
    async fn forward_forget(
        &self,
        payload: &ForgetPayloadDTO,
        user: &AuthenticatedUser,
    ) -> Result<ForgetResponseDTO, CloudClientError>;
}
```

The trait takes the *parsed* DTO (not raw JSON) so we never proxy a malformed body. It takes `&AuthenticatedUser` so an impl can attach a tenant-scoped service token when calling the upstream, but the trait does not expose any auth/url types in its return type — that's what makes scrubbing trivial.

## Implementation steps

### Stage A — Trait + slot + handler branch (closes the gap)

1. **Add the new module.**
   - File: `crates/http-server/src/cloud_client.rs` containing `CloudDeleteClient`, `CloudClientError`.
   - Re-export from `crates/http-server/src/lib.rs`: `pub mod cloud_client; pub use cloud_client::{CloudDeleteClient, CloudClientError};`.
   - Add `async-trait` and `thiserror` dependencies if not already in `Cargo.toml` (both are already used elsewhere in the crate per the `DeleteError` definition in `cognee_delete`).

2. **Add the slot on `ComponentHandles`** ([crates/http-server/src/components.rs](../../../../crates/http-server/src/components.rs)):
   ```rust
   /// Optional cloud proxy for `POST /api/v1/forget`. When `Some`, the
   /// forget router forwards the validated payload upstream and returns
   /// the response verbatim. When `None`, the router runs the local
   /// `DeleteService`. Mirrors Python's `get_remote_client()` switch.
   pub cloud_client: Option<Arc<dyn crate::cloud_client::CloudDeleteClient>>,
   ```
   - Default to `None` in every `ComponentHandles` constructor / test fixture (look for all sites that build `ComponentHandles { … }` — the compiler will find them).
   - Document the slot is *additive*: no public-API caller of `ComponentHandles { … }` is forced to change behavior, only to add `cloud_client: None`.

3. **Branch in `forget.rs`** (immediately after `resolve_mode()` and `state.components()` resolution, before the `mode` match):
   ```rust
   // L57 replacement
   if let Some(cloud) = &components.cloud_client {
       return match cloud.forward_forget(&payload, &user).await {
           Ok(resp) => Ok(Json(resp)),
           Err(e) => Err(map_cloud_error(e)),
       };
   }
   ```
   - Keep the existing local path untouched below this block.
   - `map_cloud_error` converts `CloudClientError` to `ApiError::OntologyEnvelope(msg, status)` with a **fixed, scrubbed message**. Suggested mapping:
     | `CloudClientError`     | HTTP status | Body                                              |
     | ---------------------- | ----------- | -------------------------------------------------- |
     | `Upstream { status }`  | `502`       | `{"error": "An error occurred during deletion."}` |
     | `Unreachable`          | `503`       | `{"error": "Deletion service is unavailable."}`   |
     | `MalformedResponse`    | `502`       | `{"error": "An error occurred during deletion."}` |
   - The local path already uses the `"An error occurred during deletion."` envelope for 500s, so callers see the same message regardless of routing (matching Python parity behavior, minus the leaked upstream body).
   - **Log** the discriminant + upstream status server-side via `tracing::error!(error = %e, status = ?…, "forget cloud proxy failed");` so operators can debug without leaking through the API. Never include URLs or auth headers in those logs either — the trait error type doesn't carry them, so this is enforced by construction.

4. **Cover the standalone-binary path.** `crates/http-server/src/main.rs` and `state.rs` build `AppState` without ever touching `cloud_client`. Verify with a `cargo check` after step 2 that `cloud_client: None` is the default everywhere `ComponentHandles` is constructed. No code change needed here beyond the struct-init updates from step 2.

5. **Error scrubbing audit.** The trait does *not* expose `String` payloads from the upstream. `CloudClientError::Upstream { status }` only carries the bare numeric status. A real impl (Stage B) must therefore:
   - Read and *discard* the upstream response body for error responses (or log it server-side only).
   - Never put the upstream URL, the user's bearer token, or any `Authorization` header value into a `Display`-able field.
   - Use a fresh `reqwest::Client` configured with `redirect::Policy::none()` so we never blindly follow a `Location` to a foreign host.
   This audit is documented here so that Stage B reviewers know what to check.

6. **No `.unwrap()` in non-test code.** All `Result` handling in `forget.rs` and `cloud_client.rs` must use `?` / `match` / `map_err`. The trait surface uses `Result<…, CloudClientError>`. The error enum derives `thiserror::Error`. (The existing `forget.rs` already uses `.expect(…)` for `resolve_mode`-guaranteed invariants, which is a separate gap and not touched here.)

### Stage B — Real `HttpCloudDeleteClient` (optional, deferred)

A reference implementation only added when an embedder requests it:

- New struct `HttpCloudDeleteClient { service_url: Url, api_key: SecretString, client: reqwest::Client }`.
- Constructor reads `service_url` + `api_key` from `HttpServerConfig` (new keys `cloud.service_url`, `cloud.api_key`, env `COGNEE_CLOUD_URL`, `COGNEE_CLOUD_API_KEY`). The `api_key` is wrapped in a `secrecy::Secret`-equivalent to avoid logging.
- `forward_forget` body:
  1. Build the request payload from `ForgetPayloadDTO` (the DTO already serializes cleanly — it's the same shape Python sends).
  2. `POST {service_url}/api/v1/forget` with `Authorization: Bearer {api_key}` and the JSON body.
  3. On `2xx`: `resp.json::<ForgetResponseDTO>().await.map_err(|_| MalformedResponse)`.
  4. On `4xx`/`5xx`: read body with size cap (e.g. 4 KiB), log it via `tracing::warn!` *internally*, return `CloudClientError::Upstream { status }`.
  5. On `reqwest::Error::is_timeout()` / `is_connect()`: `CloudClientError::Unreachable`.
- Wire opt-in: `AppState::build_with_cloud_client(…)` that takes an `Arc<dyn CloudDeleteClient>` and slots it into `ComponentHandles` after the rest of the build is done. Document this as the *only* supported way to enable cloud routing.

Flag explicitly: **Stage B is optional.** Stage A alone closes the gap because the marker at L57 is a wiring TODO, not a behavior bug.

## Tests

Add to [crates/http-server/tests/test_forget.rs](../../../../crates/http-server/tests/test_forget.rs) (Stage A only; Stage B grows its own test file when it lands):

**Test A — proxy success.** A `MockCloudDeleteClient` returns `Ok(ForgetResponseDTO::Everything(ForgetEverythingResponse { datasets_removed: 7, status: "success".into() }))`. Build an `AppState` whose `ComponentHandles.cloud_client = Some(Arc::new(mock))`. POST `/api/v1/forget` with `{"everything": true}`. Assert:
- Response status `200`.
- Response body `{"datasets_removed": 7, "status": "success"}`.
- The mock's call counter is exactly `1`.
- A second mock (`Arc<DeleteService>`) is **not** called — easiest enforcement is to use a `DeleteService` configured with mock backends that panic on `execute`. Reaching this panic fails the test.

**Test B — proxy upstream error.** Mock returns `Err(CloudClientError::Upstream { status: 503 })`. Assert:
- Response status `502` (or whatever the table picks).
- Body is the canonical envelope `{"error": "An error occurred during deletion."}`.
- Body must **not** contain the substring `503`, any URL, or any token.
- The mock for `DeleteService::execute` is not invoked.

**Test C — `cloud_client = None` keeps existing behavior.** Build the existing `build_auth_test_state()` fixture (which sets `cloud_client = None` by default after step 2). Run *all five* existing tests in `test_forget.rs` (`no_auth`, `no_fields_returns_422`, `data_id_only_returns_422`, `everything_true_ignores_extra_fields`, `everything_resolves_mode_correctly`, `dataset_only_resolves_to_mode2`) — they must continue to pass byte-for-byte.

**Test D — regression cross-check.** A single explicit test asserting `state.components().cloud_client.is_none()` for `build_auth_test_state()` — guards against an embedder accidentally enabling cloud routing in a test fixture.

**Optional Test E (parity smoke).** When Stage B lands: a `wiremock`-driven test that stands up a fake upstream and asserts the JSON body the real `HttpCloudDeleteClient` sends matches Python's payload exactly (`everything`, `dataset`, `data_id` keys, snake_case, string-stringified UUIDs per `cloud_client.py` L301-L308).

## Acceptance criteria

- [ ] `crates/http-server/src/cloud_client.rs` exists, defines `CloudDeleteClient: Send + Sync + 'static` and `CloudClientError` with `thiserror` derive.
- [ ] `ComponentHandles` gains `pub cloud_client: Option<Arc<dyn CloudDeleteClient>>` defaulting to `None` everywhere it is constructed in production and test code.
- [ ] `forget.rs` L57 TODO is removed and replaced with a runtime branch that proxies when `cloud_client.is_some()` and short-circuits before any local DB / `DeleteService` work.
- [ ] Cloud errors map to scrubbed `ApiError::OntologyEnvelope` envelopes — no URL, status body, or auth header content appears in any HTTP response.
- [ ] Tests A, B, C, D all pass.
- [ ] All pre-existing `test_forget.rs` cases pass unchanged.
- [ ] No `.unwrap()` in `forget.rs` non-test code and no `.unwrap()` in `cloud_client.rs` outside `#[cfg(test)]`.
- [ ] `cargo clippy --workspace -- -D warnings` is clean.
- [ ] Plan explicitly documents that Stage B (real HTTP impl) is deferred.

## Status

**not-started (low priority — future feature, not a regression)**
