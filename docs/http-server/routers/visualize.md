# Router: visualize

The `/api/v1/visualize` router serves single-file HTML knowledge-graph visualizations rendered by the existing `cognee-visualization` crate (a force-directed d3.js graph viewer). `GET /` renders the graph for one of the caller's datasets; `POST /multi` aggregates several users' datasets into a combined visualization (superuser-only). Both endpoints return `text/html` directly — they do **not** return JSON. This is the only router in the API that returns HTML as its happy-path response.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/visualize`
- Router file: `crates/http-server/src/routers/visualize.rs`
- DTO file: `crates/http-server/src/dto/visualize.rs`
- Python source: [`cognee/api/v1/users/routers/get_visualize_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py) (note: lives under `users/routers/` even though it mounts at `/visualize`).
- Rust visualization crate (already implemented): [`crates/visualization/`](../../../crates/visualization/) with public API `cognee_visualization::render(&dyn GraphDBTrait) -> Result<String, VisualizationError>`.

## 2. Endpoints

### 2.1 `GET /api/v1/visualize` — render a single-dataset HTML visualization

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**:
  - `dataset_id` (required, `Uuid`): the dataset whose graph to render. Python: `dataset_id: UUID` extracted via FastAPI's automatic `Query` parser ([`get_visualize_router.py:28`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L28)). Missing or malformed → 422 (Python's default validation error). Rust: `Query<VisualizeQueryDTO>` extractor produces the same shape.
- **Request body**: none.
- **Response body** (`200 OK`, `text/html; charset=utf-8`): the HTML document produced by `cognee_visualization::render(graph_db).await?`. Single-file (CSS + JS inline + d3 v7 from the bundled asset directory). Typical size 50 KiB – 5 MiB depending on graph density. No `Content-Disposition` header (rendered inline).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. JSON, not HTML. |
  | 422 | `{"detail": [...], "body": ...}` | Missing or malformed `dataset_id` query param. From the custom `Json`/`Query` extractor. |
  | 409 | `{"error": "<msg>"}` | Catch-all. Python: [`get_visualize_router.py:74-75`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L74-L75) — any exception (dataset not found, permission denied, graph DB read error, visualization render error) collapses to `409 {"error": str(exc)}`. Rust matches exactly: a single error envelope for everything except auth/validation failures. **404 (dataset not found) and 403 (permission denied) both surface as 409** — this is intentional Python behavior that Rust replicates verbatim, and there is no plan to surface a more specific code. |

  All non-200 responses use `application/json`; only the 200 path is `text/html`.
- **Side effects**: none (read-only). The `set_database_global_context_variables(dataset.id, dataset.owner_id)` call in Python ([`get_visualize_router.py:69`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L69)) is multi-tenant context-routing (only active when `ENABLE_BACKEND_ACCESS_CONTROL=true`). The Rust port wires the same context via the `AppState::lib`'s tenant-aware DB layer; see [../tenants.md §7](../tenants.md#7-tenant_id-on-related-tables).
- **Delegation target**:
  1. Resolve and authorize the dataset: `state.lib.datasets().get_authorized([dataset_id], "read", &user).await` → returns the matching `Dataset` row(s) or `Err(PermissionDenied)`. This is the same call used by `/api/v1/datasets/{id}/graph`.
  2. Set tenant context (when `ENABLE_BACKEND_ACCESS_CONTROL=true`): `state.lib.set_tenant_context(dataset.id, dataset.owner_id).await`.
  3. Render: `cognee_visualization::render(state.lib.graph_db()).await?` returns `String`.
  4. Wrap in `axum::response::Html(html)` → emits `Content-Type: text/html; charset=utf-8` automatically.
- **Validation rules**: `dataset_id` must be a valid UUID v4/v5; that's enforced at the extractor level.
- **Rate / size limits**: response size is unbounded in principle. Large graphs (>100 K nodes) produce HTML that exceeds reasonable browser memory; the visualization crate's `serialize_graph` truncates at the data-model level (currently no cap — flag in §6).
- **Permission gate**: `read` on the requested dataset, resolved via `PermissionsRepository::user_can(user.id, dataset_id, "read")` (see [../tenants.md §5](../tenants.md#5-permission-resolution)). Failure surfaces as a `PermissionDeniedError` which Python folds into the 409 catch-all; we match.
- **OpenAPI**: tag `["v1", "visualize"]`. The `200` response is declared with `content: { "text/html": { schema: { type: "string", format: "html" } } }` so OpenAPI clients render it correctly. Per Python, `response_model=None` ([`get_visualize_router.py:27`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L27)) — the returned content is intentionally non-JSON.
- **Telemetry**: span name `cognee.api.visualize`. Attributes:
  - `cognee.dataset.id` — the requested dataset UUID.
  - `cognee.visualize.node_count` — set after `graph_db.get_graph_data()` returns.
  - `cognee.visualize.edge_count` — same.
  - `cognee.visualize.html.bytes` — size of the rendered HTML (post-render).
  - Python emits `send_telemetry("Visualize API Endpoint Invoked", ...)` ([`get_visualize_router.py:52-60`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L52-L60)) with `dataset_id` only; we extend with the three count/size attributes for diagnostics.
- **Python parity notes**:
  - Python's `visualize_graph()` ([`cognee/api/v1/visualize/visualize.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/visualize.py)) writes the HTML to `~/graph_visualization.html` *and* returns it. The Rust port already separates these concerns: `cognee_visualization::render()` returns the HTML string without writing to disk; `cognee_visualization::visualize()` writes to a path. The HTTP handler uses `render()` only — no filesystem side effect.
  - Python's `dataset = await get_authorized_existing_datasets([dataset_id], "read", user)` ([`get_visualize_router.py:67`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L67)) raises `PermissionDeniedError` when access is denied; that error class has its own status code (typically 403), but the broad `except Exception` clause in the handler ([line 74](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L74)) converts everything to 409. **Rust must replicate the swallow** — return 409, not 403, for permission denied here. Document loudly.

### 2.2 `POST /api/v1/visualize/multi` — render a combined multi-user visualization

- **Auth**: `required` (`AuthenticatedUser`) **plus superuser check**. Per Python: `if not user.is_superuser: return JSONResponse(403, ...)` ([`get_visualize_router.py:114-118`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L114-L118)). Rust uses a dedicated `SuperuserOnly` extractor wrapping `AuthenticatedUser` with the same check; rejection produces `403 {"error": "Superuser privileges required for multi-user visualization"}` (note: `{error}` envelope, not `{detail}`).
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `Vec<UserDatasetPairDTO>` — a JSON array of `{user_id, dataset_id}` pairs. Python: [`get_visualize_router.py:79`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L79). Empty array is allowed and produces an empty visualization (HTML with zero nodes).
- **Response body** (`200 OK`, `text/html; charset=utf-8`): the combined visualization. Each pair's nodes are tagged with the owner's user-id so the d3 renderer can color-code them. The Rust visualization crate must support a multi-user mode — see §3 for the extension required.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. |
  | 403 | `{"error": "Superuser privileges required for multi-user visualization"}` | Caller is not a superuser. Python: [`get_visualize_router.py:114-118`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L114-L118). |
  | 422 | `{"detail": [...], "body": ...}` | Body does not parse as `Vec<UserDatasetPairDTO>`. |
  | 409 | `{"error": "<msg>"}` | Catch-all (any of the iteratively-resolved users / datasets failed, or rendering failed). Python: [`get_visualize_router.py:131-132`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L131-L132). |

- **Side effects**: none beyond auth/permission resolution reads.
- **Delegation target**:
  1. Verify `user.is_superuser`.
  2. For each `(user_id, dataset_id)` pair:
     - `target_user = state.lib.users().get(pair.user_id).await?`
     - `dataset = state.lib.datasets().get_authorized([pair.dataset_id], "read", &target_user).await?` — resolve permission against the *target* user, not the caller (matches Python's [`get_visualize_router.py:122-125`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L122-L125)).
     - Collect `(target_user, dataset)` into a vector.
  3. Render: `cognee_visualization::render_multi_user(&pairs).await?` — returns combined HTML. **This function does not yet exist in the Rust visualization crate**; see §3 for the implementation task.
  4. Wrap in `axum::response::Html`.
- **Validation rules**: each pair's `user_id` and `dataset_id` must be valid UUIDs (extractor-level). No length cap on the array, but practical limit ~50 pairs (above that the d3 layout becomes unusable — flag in §6).
- **Rate / size limits**: default body limit. Output HTML scales linearly with total node count.
- **Permission gate**: superuser globally; per-target-user dataset read permission per pair. Crucially, the Rust impl must check permission against the *target user*, not the caller — so the caller (a superuser) doesn't accidentally elevate access by being able to read every dataset.
- **OpenAPI**: tag `["v1", "visualize"]`. Request body schema from `Vec<UserDatasetPairDTO>`; `200` response declared as `text/html`. Mark `security = [{BearerAuth: []}, {ApiKeyAuth: []}]` and add a `403` response declaring the superuser requirement in the description.
- **Telemetry**: span name `cognee.api.visualize.multi`. Attributes:
  - `cognee.visualize.pair_count` — `pairs.len()`.
  - `cognee.visualize.node_count`, `cognee.visualize.edge_count`, `cognee.visualize.html.bytes` — as in the GET endpoint.
  - `cognee.user.is_superuser` — always `true` if we got past the gate.
- **Python parity notes**:
  - Python iterates pairs with `await get_user(pair.user_id)` and `await get_authorized_existing_datasets(...)` ([`get_visualize_router.py:122-125`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L122-L125)). On any failure for any pair, the broad `except Exception` ([line 131](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L131)) returns 409 *for the whole request* — partial success is not supported. Match exactly.
  - The aggregation pattern (color-by-user) is implemented in `visualize_multi_user_graph()` ([`cognee/api/v1/visualize/visualize.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/visualize.py)). Rust must port this to `cognee-visualization` — see §3.

## 3. Cross-cutting behavior

### 3.1 The `cognee-visualization` crate — what exists and what's missing

The crate is implemented at [`crates/visualization/`](../../../crates/visualization/) and exposes:

| Symbol | Status | Use |
|---|---|---|
| `pub async fn visualize(graph_db, output_path)` | Implemented | Writes HTML to a path; returns the path. Used by the CLI. |
| `pub async fn render(graph_db)` | Implemented | Returns the HTML string without filesystem side effects. **This is what the GET handler uses.** |
| `pub async fn render_multi_user(pairs)` | **Not yet implemented** | Aggregates multiple `(User, Dataset)` pairs into one visualization with color-by-user. **Must be added before the POST handler can land.** |

The HTML pipeline (`html.rs`, `serialize.rs`, `paths.rs`) is otherwise complete — the only missing piece is the multi-user aggregation. Implementation outline:

```rust
// crates/visualization/src/lib.rs (new)
pub async fn render_multi_user(
    pairs: &[(User, Arc<dyn GraphDBTrait>)],
) -> Result<String, VisualizationError> {
    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();
    for (user, gdb) in pairs {
        let (nodes, edges) = gdb.get_graph_data().await?;
        // Tag each node with the owner's user-id so the d3 renderer can
        // color-code by user (the existing template supports a `user_id`
        // node attribute).
        for mut n in nodes {
            n.attributes.insert("user_id".into(), user.id.to_string().into());
            all_nodes.push(n);
        }
        all_edges.extend(edges);
    }
    let serialized = serialize::serialize_graph(all_nodes, all_edges);
    html::build_html(&serialized, /* color_by_user = */ Some(true))
}
```

The exact signature is up to the implementer; the HTTP handler imports it as opaque.

### 3.2 Tenant context

When `ENABLE_BACKEND_ACCESS_CONTROL=true`, the Python handler calls `set_database_global_context_variables(dataset.id, dataset.owner_id)` before reading the graph DB. This ensures multi-tenant DB queries are scoped correctly. The Rust port must do the equivalent via the existing tenant-context infrastructure (see [../tenants.md §7](../tenants.md#7-tenant_id-on-related-tables)). Skip when `ENABLE_BACKEND_ACCESS_CONTROL=false`.

### 3.3 Content-type handling

Both endpoints emit `text/html; charset=utf-8` on success and `application/json` on error. The `IntoResponse` impl for `ApiError` always emits JSON (canonical), so emitting JSON on error is automatic. The handler returns `Result<Html<String>, ApiError>`; axum's `Html` wrapper sets the right content-type.

### 3.4 Caching

Visualizations are not cached server-side. Every request re-fetches the graph and re-renders. For large graphs this is slow (~1–10s); a future enhancement could ETag-cache the rendered HTML by graph-content-hash. Out of scope for phase 4 — flag in §6.

## 4. DTO definitions

```rust
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Query parameters for `GET /api/v1/visualize`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct VisualizeQueryDTO {
    pub dataset_id: Uuid,
}

/// Mirrors Python `UserDatasetPair` (`get_visualize_router.py:19-21`).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserDatasetPairDTO {
    pub user_id: Uuid,
    pub dataset_id: Uuid,
}
```

(There is no response DTO — the response is raw HTML wrapped in `axum::response::Html<String>`.)

## 5. Implementation tasks

1. Implement `cognee_visualization::render_multi_user()` in `crates/visualization/src/lib.rs`. Add a corresponding test in `crates/visualization/tests/`.
2. Add `crates/http-server/src/dto/visualize.rs` with `VisualizeQueryDTO` and `UserDatasetPairDTO`.
3. Add `crates/http-server/src/routers/visualize.rs` with `get_visualize`, `post_visualize_multi`, and `pub fn router()`.
4. Wire `nest("/visualize", visualize::router())` in `build_router()`.
5. Implement a `SuperuserOnly` extractor in `crates/http-server/src/auth/` that wraps `AuthenticatedUser` and returns `ApiError::Forbidden("Superuser privileges required ...")` (with the `{error}` envelope, not `{detail}`) when `is_superuser=false`.
6. Ensure `ApiError::VisualizeError(StatusCode, String)` returns the `{error}` envelope used by the catch-all 409 path.
7. Unit tests: `VisualizeQueryDTO` parses `?dataset_id=<uuid>`; `UserDatasetPairDTO` deserializes; `SuperuserOnly` extractor rejects non-superusers.
8. Integration tests in `crates/http-server/tests/test_visualize.rs`:
   - GET with valid dataset → 200, `Content-Type: text/html`, body contains `<svg` (or whatever the canonical d3 root element is).
   - GET with no `dataset_id` → 422.
   - GET against a dataset the user doesn't own → 409 (NOT 403 — match Python's swallow).
   - POST `/multi` as a non-superuser → 403 with `{error}` envelope.
   - POST `/multi` as a superuser with a valid pair → 200 HTML.
   - POST `/multi` as a superuser with an empty array → 200 HTML (empty graph).
9. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_visualize.py`: byte-for-byte HTML equality is hard (d3 layout isn't deterministic), so assert structural properties: same number of `<g class="node">` elements, same number of `<line>` edge elements, identical `Content-Type`.

## 6. Open questions

1. **Permission-denied = 409, not 403** (matching Python's broad `except`). Is this a wire-compat constraint or a fixable Python bug? Strict wire parity says keep 409. Recommend keeping 409 for phase 4; revisit when a v2 of the API breaks compat.
2. **Node-count cap**: large graphs (>100 K nodes) produce unusable HTML and may OOM the browser. Add a server-side cap (e.g. 10,000 nodes) and return 409 with a clear message? Discuss with frontend team.
3. **HTML caching**: ETag by graph-content-hash. Out of scope for phase 4; track separately.
4. **d3 asset bundling**: the rendered HTML inlines d3 from `crates/visualization/assets/`. Confirm the bundled d3 version matches Python's (`d3.min.js` v7) for consistent layout behavior.
5. **`render_multi_user` color palette**: needs N distinguishable colors for N users. The current single-user renderer uses a fixed scheme. For multi-user, propose `d3.schemeCategory10` (Python's choice) and document.
6. **Multi-tenant context vars**: when `ENABLE_BACKEND_ACCESS_CONTROL=true`, the GET endpoint switches DB context per request. Should the POST `/multi` do this per pair? Python does NOT — flag whether that's intentional or a bug.

## 7. References

- Python router (note path — under `users/`): [`cognee/api/v1/users/routers/get_visualize_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py).
- Python visualization functions: [`cognee/api/v1/visualize/visualize.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/visualize.py).
- Python authorized-dataset lookup: [`cognee/modules/data/methods/get_authorized_existing_datasets.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/data/methods/get_authorized_existing_datasets.py).
- Rust visualization crate: [`crates/visualization/src/lib.rs`](../../../crates/visualization/src/lib.rs).
- Rust graph DB trait: [`crates/graph/src/lib.rs`](../../../crates/graph/src/lib.rs).
- [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution) for authentication resolution.
- [../tenants.md §5](../tenants.md#5-permission-resolution) for `read` permission resolution against datasets.
- [../tenants.md §7](../tenants.md#7-tenant_id-on-related-tables) for the `set_database_global_context_variables` analog.
- [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions) for the tracing-attribute keys cited above.
