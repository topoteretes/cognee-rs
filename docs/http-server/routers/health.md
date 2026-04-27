# Router: health

Liveness/readiness probes for the cognee HTTP server. Two unauthenticated GET endpoints — one cheap (`GET /health`) intended for k8s/load-balancer probes, and one expensive (`GET /health/detailed`) that fans out to every backend (relational DB, vector DB, graph DB, file storage, optionally LLM and embedding service) and returns a structured per-component report. The router is the only public, no-auth endpoint other than `/` and the auth-login routes; everything else in the `/api/v1` tree gates on the `AuthenticatedUser` extractor.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/health` (NOT `/api/v1/health` — see [`client.py:282-286`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L282-L286)).
- Router file: `crates/http-server/src/routers/health.rs`.
- Python source: [`cognee/api/v1/health/routers/get_health_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py) and the helper module [`cognee/api/v1/health/health.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py).
- Mounted in `build_router` via `.nest("/health", health::router())` per [../architecture.md §7](../architecture.md#7-router-composition).

## 2. Endpoints

### 2.1 `GET /health` — shallow liveness probe

- **Auth**: `none`. Public endpoint — load balancers, k8s `livenessProbe`, and Docker `HEALTHCHECK` directives must not need credentials. Cross-cutting auth conventions in [README §3.2](README.md#32-authentication-declaration).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `application/json`, DTO `HealthShallowDTO`. Status `200` when the system is HEALTHY or DEGRADED, status `503` when UNHEALTHY.
  - `status`: `"ready"` when status code is 200, `"not ready"` when 503.
  - `health`: `HealthStatus` enum string (`"healthy"` | `"degraded"` | `"unhealthy"`).
  - `version`: cognee version string (e.g. `"1.0.0-local"` or whatever the binary is built at).
  - **Note**: the shallow probe still runs the four critical component checks under the hood — it just returns the *aggregated* status without the per-component breakdown. This matches Python's [`get_health_router.py:11-26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L11-L26) calling `get_health_status(detailed=False)`.
- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | 503 | `HealthShallowFailureDTO { status: "not ready", reason: "health check failed: <err>" }` | The health-check machinery itself raised — i.e. `HealthChecker::get_health_status` returned `Err`, not just an UNHEALTHY status. Mirrors Python's `except Exception` branch at [`get_health_router.py:27-31`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L27-L31). |
  | 503 | `HealthShallowDTO { status: "not ready", health: "unhealthy", version }` | At least one *critical* component (relational DB, vector DB, graph DB, or file storage) is UNHEALTHY. |

  Note: this endpoint never returns the canonical `ApiError` JSON envelope from [../architecture.md §9](../architecture.md#9-error-handling). It returns its own `{status, reason}` shape for the failure path because Python does, and existing probes parse that shape. The handler must short-circuit `IntoResponse for ApiError` and produce the literal JSON.

- **Side effects**: none (pure reads). The component checks themselves perform: a `SELECT 1` on the relational pool, a list-tables call on the vector DB, a `MATCH () RETURN count(*) LIMIT 1` on the graph DB, and a write+delete probe on file storage. None of these mutate persistent state.
- **Delegation target**: `cognee_lib::health::HealthChecker::get_health_status(detailed: false)` — a new module in `cognee-lib` that ports [`health.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py). The HTTP handler is purely shape translation; all backend-poking lives in the library.
- **Validation rules**: Not applicable — no input.
- **Rate / size limits**: respect the global 100 MiB body limit (see [../architecture.md §8](../architecture.md#8-middleware-stack)) by virtue of being GET. Not separately rate-limited; deployments that need rate-limiting should put a load balancer in front.
- **OpenAPI**: tags `["health"]` matching [`client.py:285`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L285). `security = []` override to declare it as public, otherwise `utoipa` will inherit the global `BearerAuth`/`ApiKeyAuth` from [../architecture.md §13](../architecture.md#13-openapi-generation--utoipa). Document both 200 and 503 response schemas.
- **Telemetry**: span name `cognee.api.health.shallow`. Per [../observability.md §7](../observability.md#7-access-logging) the access-log filter drops sub-`warn` lines for `/health` so probe traffic doesn't drown the log; errors and slow checks still log normally. Record attribute `cognee.health.aggregate_status` (one of `healthy|degraded|unhealthy`) on the span so trace viewers can spot probe failures.
- **Python parity notes**:
  - The Python handler returns status `503` for both UNHEALTHY *and* DEGRADED in `/health/detailed` ([`get_health_router.py:42-44`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L42-L44)) but the shallow `/health` only flips to 503 for UNHEALTHY ([`get_health_router.py:17`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L17)). DEGRADED stays at 200 on the shallow probe so transient LLM/embedding outages don't kick a pod out of rotation. **Do not "fix" this asymmetry** — replicate verbatim.
  - The shallow shape uses key `health` (not `status`) for the enum value; `status` is the human-readable `"ready"`/`"not ready"` string. The detailed shape uses `status` for the enum. Confusing, but matches Python.

### 2.2 `GET /health/detailed` — comprehensive component report

- **Auth**: `none`. Same rationale as `/health`. Note: this endpoint is **expensive** (writes a probe file, runs DB queries on every backend, optionally hits the LLM/embedding APIs). Operators who expose the server on a public network should reverse-proxy this path behind auth or fence it with an allowlist; document this in deployment guides.
- **Path params**: none.
- **Query params**: none. (Python does not accept a `detailed` query param — the distinction is purely path-based.)
- **Request body**: none.
- **Response body**: `application/json`, DTO `HealthDetailedDTO` (the full `HealthResponse` from [`health.py:31-36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L31-L36)). Status `200` when HEALTHY, `503` when DEGRADED or UNHEALTHY (see parity note in §2.1).
  - `status`: `HealthStatus` enum.
  - `timestamp`: RFC 3339 UTC timestamp string.
  - `version`: cognee version string.
  - `uptime`: seconds since process start (integer).
  - `components`: `HashMap<String, ComponentHealthDTO>` — keyed by `relational_db`, `vector_db`, `graph_db`, `file_storage` always; plus `llm_provider` and `embedding_service` only when detailed=true. Each value:
    - `status`: `HealthStatus` enum (HEALTHY | DEGRADED | UNHEALTHY).
    - `provider`: backend name (e.g. `"sqlite"`, `"qdrant"`, `"ladybug"`, `"local"`, `"openai"`) or `"unknown"` if the check failed before identifying it.
    - `response_time_ms`: integer milliseconds.
    - `details`: human-readable string (e.g. `"Connection successful"`, `"Connection failed: <err>"`).
- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | 503 | `HealthDetailedFailureDTO { status: "unhealthy", error: "Health check system failure: <err>" }` | The health-check machinery itself raised. Note key is `error` (not `reason` like the shallow path) and the value is `"unhealthy"` (not `"not ready"`). Matches Python's [`get_health_router.py:47-51`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L47-L51). |
  | 503 | `HealthDetailedDTO` (full body) | At least one critical component is UNHEALTHY *or* any component is DEGRADED. Body still has the per-component breakdown so callers can see *what* failed. |

- **Side effects**: writes and immediately deletes a probe file `<data_root_directory>/health_check_test` (local file storage path) or `health_check_test` (S3 path). Mirrors Python's [`health.py:160-172`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L160-L172). Idempotent — survives concurrent invocations because each call uses the same fixed name and overwrites/removes; the very tight race where two checks delete each other's file is acceptable because the resulting transient `OSError` flips that single check to UNHEALTHY for one cycle, and the operator-visible signal is correct.
- **Delegation target**: `cognee_lib::health::HealthChecker::get_health_status(detailed: true)`. Same library entry point, different argument; the implementation runs the four critical checks unconditionally and adds the two non-critical checks (`llm_provider`, `embedding_service`) only when `detailed=true`. Cross-reference Python's [`health.py:243-291`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L243-L291).
- **Validation rules**: Not applicable — no input.
- **Rate / size limits**: same as §2.1. The handler must internally enforce a per-component timeout (see Open Questions §6) so a stuck backend doesn't make the endpoint hang indefinitely.
- **OpenAPI**: tags `["health"]`. `security = []`. Document both 200 and 503 responses; 503 has two possible bodies (full-component or system-failure) — declare a `oneOf` in the response schema.
- **Telemetry**: span name `cognee.api.health.detailed`. Each component check emits a child span `cognee.health.check.<component>` with attributes `cognee.health.component`, `cognee.health.provider`, `cognee.health.response_time_ms`, `cognee.health.aggregate_status`. The library code does the instrumentation; the HTTP handler is just `#[tracing::instrument(name = "cognee.api.health.detailed", skip_all)]`. Span attribute keys for the component-level spans should be added to the shared list in [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions) (proposed: `cognee.health.component`, `cognee.health.provider`).
- **Python parity notes**:
  - Detailed flips to 503 on **DEGRADED** ([`get_health_router.py:43-44`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py#L43-L44)); shallow does not. Verbatim port — do not unify.
  - LLM and embedding service health checks return DEGRADED (not UNHEALTHY) on failure — see [`health.py:212-217`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L212-L217) and [`health.py:236-241`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L236-L241). They are non-critical: they degrade the status but never make it UNHEALTHY by themselves. Critical checks (DB / storage) return UNHEALTHY on failure.
  - The four critical checks run via `asyncio.gather(..., return_exceptions=True)` — failures are caught and turned into UNHEALTHY components, never propagated. Rust port uses `tokio::join!` (heterogeneous types) or `futures::future::join_all` of boxed futures, with each future returning `Result<ComponentHealth, ComponentHealth>` so a panic inside one check doesn't poison the whole probe. Catching panics requires `tokio::task::JoinHandle::is_panic` semantics — easier to wrap each check in its own `tokio::spawn` and turn a `JoinError` into a UNHEALTHY component.

## 3. Cross-cutting behavior

- **No auth**: both endpoints are explicitly public. The handler functions take no `AuthenticatedUser` extractor. Document the choice via a Rust comment because reviewers will instinctively expect every handler to gate on auth.
- **Bypass `ApiError`**: this is the only router in the whole server that uses bespoke success and failure JSON shapes. **Do not** wrap responses in the generic `ApiError` envelope from [../architecture.md §9](../architecture.md#9-error-handling) even on the failure path. The handler returns `axum::response::Response` directly, built via `(StatusCode, Json<HealthShallowDTO>).into_response()`.
- **Library decoupling**: all knowledge of cognee internals (SeaORM connection, vector engine, graph engine, file storage path) lives in the new `cognee_lib::health` module. The HTTP crate must not import from `cognee-database`, `cognee-vector`, `cognee-graph`, or `cognee-storage` directly — that would defeat the dependency hygiene established in [../architecture.md §3](../architecture.md#3-crate-topology).
- **Access-log filtering**: `/health` is in the noisy-endpoint allowlist that gets sub-`warn` lines dropped (see [../observability.md §7](../observability.md#7-access-logging)). `/health/detailed` is **not** filtered — it's expected to be called by humans and dashboards, not load balancers, so we want the access-log line.

## 4. DTO definitions

Located in `crates/http-server/src/dto/health.rs`. Names match Python's Pydantic class names where they exist (`HealthStatus`, `HealthResponse`, `ComponentHealth`); the failure-path shapes have no Python class so we name them `HealthShallowFailureDTO` / `HealthDetailedFailureDTO` for clarity.

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Mirrors `HealthStatus` enum from [`health.py:18-21`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L18-L21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Success/UNHEALTHY-aggregate body for `GET /health`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthShallowDTO {
    /// "ready" when status code 200, "not ready" when 503.
    pub status: &'static str,
    /// Aggregate status of the four critical components.
    pub health: HealthStatus,
    pub version: String,
}

/// Failure body for `GET /health` when the health-check machinery itself errored.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthShallowFailureDTO {
    pub status: &'static str, // always "not ready"
    pub reason: String,       // "health check failed: <err>"
}

/// Mirrors `ComponentHealth` from [`health.py:24-28`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L24-L28).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComponentHealthDTO {
    pub status: HealthStatus,
    pub provider: String,        // "sqlite" | "qdrant" | "ladybug" | "local" | "s3" | "openai" | "unknown"
    pub response_time_ms: u64,
    pub details: String,
}

/// Mirrors `HealthResponse` from [`health.py:31-36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L31-L36).
/// Body for `GET /health/detailed`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthDetailedDTO {
    pub status: HealthStatus,
    pub timestamp: String,                                  // RFC 3339 UTC
    pub version: String,
    pub uptime: u64,                                        // seconds
    pub components: HashMap<String, ComponentHealthDTO>,    // keys: "relational_db" | "vector_db" | "graph_db" | "file_storage" | "llm_provider"? | "embedding_service"?
}

/// Failure body for `GET /health/detailed` when the health-check machinery itself errored.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthDetailedFailureDTO {
    pub status: &'static str, // always "unhealthy"
    pub error: String,        // "Health check system failure: <err>"
}
```

Library-side types (in `crates/lib/src/health.rs`, behind no feature gate so the SDK can also expose health checks for embedded uses):

```rust
pub struct HealthCheckReport {
    pub status: HealthStatus,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub version: String,
    pub uptime: std::time::Duration,
    pub components: std::collections::HashMap<String, ComponentHealth>,
}

pub struct ComponentHealth {
    pub status: HealthStatus,
    pub provider: String,
    pub response_time: std::time::Duration,
    pub details: String,
}

#[async_trait::async_trait]
pub trait HealthChecker: Send + Sync {
    async fn get_health_status(&self, detailed: bool) -> Result<HealthCheckReport, HealthError>;
}
```

The HTTP DTOs are produced by `From<&HealthCheckReport>` / `From<&ComponentHealth>` impls — we don't reuse the library types directly because the HTTP layer needs `serde` field names and `utoipa::ToSchema`, while the library types use `Duration` and `DateTime` for ergonomics.

## 5. Implementation tasks

1. Add a new module `crates/lib/src/health.rs` with the `HealthChecker` trait and a default impl `DefaultHealthChecker { start_time: Instant, components: ComponentManager }`. Port the six check methods from [`health.py:43-241`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L43-L241), each returning `ComponentHealth`. Each check wraps in `tokio::spawn` so panics turn into UNHEALTHY components.
2. Add unit tests in the same file covering: HEALTHY aggregate (all critical components healthy), UNHEALTHY aggregate (one critical UNHEALTHY), DEGRADED aggregate (no critical UNHEALTHY but at least one DEGRADED), system-failure path (panic in checker), `MockHealthChecker` for use in HTTP handler tests.
3. Wire `Arc<dyn HealthChecker>` into `AppState` in `crates/http-server/src/state.rs` (new field `health: Arc<dyn cognee_lib::health::HealthChecker>`).
4. Add DTO structs in `crates/http-server/src/dto/health.rs` (per §4) with `From<&HealthCheckReport>` impl that populates `timestamp` via `chrono::DateTime::to_rfc3339`, `uptime` via `Duration::as_secs`, and `response_time_ms` via `Duration::as_millis() as u64`.
5. Add handler functions in `crates/http-server/src/routers/health.rs`:
   - `pub fn router() -> Router<AppState>` registering `GET /` → `get_shallow` and `GET /detailed` → `get_detailed`. Note: the inner path is `/`, not `/health`, because the `/health` prefix is supplied by `.nest()` in `build_router`.
   - `async fn get_shallow(State(state): State<AppState>) -> Response` — calls `state.health.get_health_status(false).await`, maps to `HealthShallowDTO` or `HealthShallowFailureDTO`, returns the right `(StatusCode, Json<…>).into_response()`.
   - `async fn get_detailed(State(state): State<AppState>) -> Response` — same shape, with the detailed flag and the detailed DTO.
6. Add OpenAPI annotations: `#[utoipa::path(get, path = "/health", responses(...))]` and `#[utoipa::path(get, path = "/health/detailed", responses(...))]` with `security(())` to override the global security requirement. Register both in the `OpenApi` derive in `src/openapi.rs`.
7. Add unit tests in `crates/http-server/src/routers/health.rs` (`#[cfg(test)] mod tests`) using `tower::ServiceExt::oneshot` and a `MockHealthChecker`:
   - HEALTHY state → 200 + `{status: "ready", health: "healthy", version}`.
   - UNHEALTHY state → 503 + `{status: "not ready", health: "unhealthy", ...}`.
   - DEGRADED state on shallow → **200** + `{status: "ready", health: "degraded", ...}` (parity guard).
   - DEGRADED state on detailed → **503** + full body (parity guard).
   - Panicking checker → 503 + `{status: "not ready", reason: "..."}` (shallow) or `{status: "unhealthy", error: "..."}` (detailed). The two key names differ on purpose; assert exact JSON.
8. Add integration tests in `crates/http-server/tests/test_health.rs`:
   - Boot real `AppState` with in-memory SQLite, embedded Qdrant, Ladybug, and a `tempfile::tempdir()` data root. Probe `/health` → 200. Probe `/health/detailed` → 200 with all four critical components present.
   - Boot with a deliberately broken graph DB (e.g. no Ladybug binary) and assert `/health` returns 503 + `health: "unhealthy"`.
   - Probe is hit 100 times concurrently — assert no `health_check_test` artifact is left behind in the temp dir at end (probe-file leak check).
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_health.py`: spin up Python uvicorn and Rust binary with identical config, hit `/health` and `/health/detailed` on both, assert response body shapes match (modulo `version`, `timestamp`, `uptime`, `response_time_ms`). Keys, status codes, and `status`/`health` enum values must match exactly.

## 6. Open questions

1. **Where does `version` come from?** Python reads it from `pyproject.toml` falling back to `importlib.metadata` ([`version.py:7-25`](https://github.com/topoteretes/cognee/blob/main/cognee/version.py)). Rust equivalent: `env!("CARGO_PKG_VERSION")` of `cognee-lib`, captured at build time. The version string is **the bare version**, with no `-rust` suffix — strict wire parity. Operators who need to distinguish the backend serving a probe should rely on the `Server` header or other infrastructure-level signal.
2. **Per-component timeout.** Python has no timeout — a stuck DB makes `/health/detailed` hang. Rust matches: no `tokio::time::timeout`, no `COGNEE_HEALTH_CHECK_TIMEOUT_SECONDS` env var. Operators wanting a timeout should configure it at the reverse-proxy layer.
3. **`provider` value when the check fails before configuring.** Python returns `"unknown"` ([`health.py:73-77`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L73-L77)) when the config lookup itself raises. Rust matches: `"unknown"` in the same branch, even though the configured backend name is available at compile time. Strict wire parity — no enrichment.
4. **File-storage probe in S3 mode.** The Python S3 branch calls `storage.store("health_check_test", BytesIO(b"test"))` then `storage.remove("health_check_test")` ([`health.py:168-172`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py#L168-L172)) — that hits the real bucket on every probe. Rust replicates verbatim (no in-process caching). Operators wanting to throttle S3 probe traffic should configure a less-frequent k8s `periodSeconds`.
5. **Auth bypass surface area.** The router is publicly accessible — Python does not gate it. Rust matches: no `REQUIRE_AUTH_FOR_HEALTH_DETAILED` flag, no path-level auth. Operators wanting auth must apply it at a reverse-proxy / WAF layer.

## 7. References

- Python router: [`cognee/api/v1/health/routers/get_health_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/get_health_router.py).
- Python checker module: [`cognee/api/v1/health/health.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/health.py).
- Python init export: [`cognee/api/v1/health/__init__.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/__init__.py) and [`cognee/api/v1/health/routers/__init__.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/health/routers/__init__.py).
- Mounting in FastAPI: [`cognee/api/client.py:282-286`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L282-L286).
- Version helper: [`cognee/version.py`](https://github.com/topoteretes/cognee/blob/main/cognee/version.py).
- Cross-cutting conventions: [README §3](README.md#3-cross-router-conventions).
- Span-name conventions: [../observability.md §3.4](../observability.md#34-span-name-conventions).
- Access-log filtering for noisy probes: [../observability.md §7](../observability.md#7-access-logging).
- Error model that this router intentionally bypasses: [../architecture.md §9](../architecture.md#9-error-handling).
- Implementation phase that lands this router: [../plan.md §4 (P0)](../plan.md#4-implementation-phases).
