# HTTP Server — WebSocket Protocol

Specification for the live pipeline-progress WebSocket at **`/api/v1/cognify/subscribe/{pipeline_run_id}`**. This is the only WebSocket endpoint Python exposes ([`get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py) — the `subscribe_to_cognify_info` block). The Rust server replicates it byte-for-byte so existing frontend / SDK clients work unchanged.

Companion docs: [plan.md](plan.md), [architecture.md](architecture.md), [pipelines.md](pipelines.md) (defines the `RunEvent` channel this endpoint consumes), [auth.md](auth.md) (the JWT semantics used for the auth handshake).

## 1. Goals & non-goals

### Goals

- **Wire compatibility with Python**: same path, same auth handshake (cookie-based JWT), same JSON frame shape, same close codes, same termination conditions.
- **Subscriber-first**: multiple concurrent subscribers to the same `pipeline_run_id` see the same event sequence; the server fans out from the in-memory broadcast channel exposed by `cognee_core::PipelineRunRegistry::subscribe` ([pipelines.md §6.2](pipelines.md#62-public-types)).
- **Bounded resource use per connection**: a slow client cannot stall the producer; if it lags past the broadcast capacity the server closes its socket with `WS_1011_INTERNAL_ERROR`.

### Non-goals

- **Generic pub/sub**: no other endpoints. Other long-running operations (`/sync`, `/memify`, `/improve`, `/remember`) currently expose status only through the durable `/datasets/status` endpoint, not WebSockets. Python is the same.
- **Bidirectional messaging**: the server only pushes; client messages are accepted but ignored. There is no protocol for the client to acknowledge, request replay, or filter.
- **Cross-process fan-out**: one process, one channel. Multi-replica deployments need sticky WS routing — see [pipelines.md §15](pipelines.md#15-open-questions).

## 2. Endpoint

| Property | Value |
|---|---|
| Path | `/api/v1/cognify/subscribe/{pipeline_run_id}` |
| Path parameter | `pipeline_run_id: Uuid` (string in path, parsed as UUID v5) |
| Method | `GET` upgrade to WebSocket (RFC 6455) |
| Auth | Cookie-based JWT only (see §4) |
| OpenAPI | Documented as `[utoipa::path(get, ...)]` with a `WebSocketUpgrade` extractor; appears in OpenAPI as a regular GET that returns `101 Switching Protocols`. |

## 3. Connection lifecycle

```
client ──HTTP GET /api/v1/cognify/subscribe/<run_id>──►  server
       (Upgrade: websocket, Connection: Upgrade,
        Sec-WebSocket-Version: 13, …,
        Cookie: auth_token=<jwt>)

       ◄──HTTP 101 Switching Protocols──             server (auth ok)

       ◄──TEXT frame {pipeline_run_id, status, payload}── server
       ◄──TEXT frame {pipeline_run_id, status, payload}── server
       …
       ◄──Close frame WS_1000_NORMAL_CLOSURE on PipelineRunCompleted only
                       or WS_1011_INTERNAL_ERROR on Lagged
                       or WS_1008_POLICY_VIOLATION on Unauthorized
                       (errored / already-completed runs do NOT close;
                        match Python's [line 342](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L342) behavior)
```

Flow:

1. Client opens the WebSocket. The HTTP upgrade request includes the auth cookie.
2. Server accepts the upgrade unconditionally. Auth is enforced *after* the upgrade (matches Python's [`websocket.accept()` → cookie read → close on failure](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L295-L312) flow).
3. Server reads the cookie, verifies the JWT, looks up the user. On failure it sends a Close frame with code `1008` (Policy Violation) and reason `"Unauthorized"`, then disconnects.
4. Server registers a subscription against `cognee_core::PipelineRunRegistry::subscribe(pipeline_run_id)`. If the run id is unknown, the registry returns an empty-but-attached `Stream` (matching Python's [`initialize_queue` call](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L319)) — clients commonly subscribe before the producer's first event lands.
5. Server forwards every `RunEvent` from the channel as a TEXT frame. Frames are JSON, no compression, UTF-8.
6. On a `PipelineRunCompleted` event, the server sends one final TEXT frame with that status, then sends a Close frame with code `1000` (Normal Closure) and tears down the subscription.
7. **On `PipelineRunErrored` or `PipelineRunAlreadyCompleted`**, the server forwards the frame and **continues looping** — it does NOT close the connection. This matches Python's [`isinstance(pipeline_run_info, PipelineRunCompleted)` check](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L342) which only triggers the close path on `Completed`. The connection stays open until the client disconnects, the channel closes, or the registry's TTL sweeps the run. This is a known Python quirk; we replicate it for strict wire parity.
8. If the client closes early, the server detects `WebSocketDisconnect` on the next send and tears down the subscription; the producer continues unaffected.

## 4. Authentication

**Mechanism**: cookie-based JWT only. Python's [WS handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L295-L312) reads `websocket.cookies.get(AUTH_TOKEN_COOKIE_NAME)` and rejects with `WS_1008_POLICY_VIOLATION` if absent or invalid. We do the same.

**Why not bearer / X-Api-Key**: browsers cannot set custom headers on the WebSocket upgrade — only cookies and the `Sec-WebSocket-Protocol` header are reliably forwarded. Python doesn't bother with the protocol-header trick; we don't either.

**Why not query-string token**: putting the JWT in the URL is the common alternative when cookies aren't available, but it leaks into access logs and browser history. Python's frontend uses cookies; we keep the same surface.

**Future addition**: when we ship SDK/MCP clients that need server-push without browser cookies, we can add a `?token=…` query-string fallback behind a feature flag. Out of scope for phase 1.

### Verification details (matches [auth.md](auth.md))

```
1. Read cookie named AUTH_TOKEN_COOKIE_NAME (default "auth_token") from upgrade headers.
2. If missing → close 1008 "Unauthorized".
3. Decode HS256 JWT with FASTAPI_USERS_JWT_SECRET; require aud == ["fastapi-users:auth"];
   require exp > now; reject otherwise → close 1008 "Unauthorized".
4. Look up user by sub claim. If user is missing or inactive → close 1008 "Unauthorized".
5. (Optional, future) Authorize: confirm the user owns the dataset for this pipeline_run_id.
   Python doesn't currently enforce this — any authenticated user can subscribe to any
   pipeline_run_id. Document the gap; do not fix in phase 1.
```

## 5. Frame format

### 5.1 Server → client (TEXT)

```json
{
  "pipeline_run_id": "0193b0f1-ea2c-7000-8000-000000000001",
  "status": "PipelineRunStarted",
  "payload": { "nodes": [], "edges": [] }
}
```

Fields:

| Field | Type | Notes |
|---|---|---|
| `pipeline_run_id` | string (UUID) | Always equals the path parameter. |
| `status` | string | One of: `PipelineRunStarted`, `PipelineRunYield`, `PipelineRunCompleted`, `PipelineRunErrored`, `PipelineRunAlreadyCompleted`. See [pipelines.md §3.3](pipelines.md#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame). |
| `payload` | object | Formatted graph snapshot for the run's dataset. Shape matches `GraphDTO`: `{nodes: [...], edges: [...]}`. May be empty `{}` if the dataset has no graph yet. |

Encoding: UTF-8 JSON. No streaming, no chunking — each event is one TEXT frame.

### 5.2 Client → server

Ignored. The Python handler does not call `websocket.receive_*`; it only sends. We do the same. A client that sends a frame gets no response; the framework silently drops it.

### 5.3 Payload computation

For every event, the server calls `state.lib.formatted_graph_data(dataset_id, user)` to populate `payload`. Python does the same on every yield. This is wasteful for `PipelineRunYield` events but **matches Python behavior** and is what the existing frontend expects; do not optimize away.

The graph fetch is an `await` — if it fails (DB error, dataset deleted mid-run), we substitute `payload: {}` and emit anyway rather than dropping the event.

## 6. Status semantics & terminal close

| Received `status` | Server action |
|---|---|
| `PipelineRunStarted` | Forward. Continue subscribing. |
| `PipelineRunYield` | Forward. Continue subscribing. |
| `PipelineRunCompleted` | Forward. Send Close frame `1000`. Tear down. |
| `PipelineRunErrored` | **Forward. Continue subscribing.** Python only closes on `Completed` (see [`get_cognify_router.py:342`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L342)), leaving the WebSocket open and the in-memory queue alive after errors. Rust matches verbatim. The error is conveyed via the JSON `status` field; it is the client's responsibility to disconnect after observing it. |
| `PipelineRunAlreadyCompleted` | **Forward. Continue subscribing.** Same Python quirk as `Errored` — only `Completed` closes the WS. |

The `PipelineRunCompleted` frame is sent before the Close frame so the client never has to infer state from the close code alone. For non-`Completed` terminals, the client must read the `status` field of the forwarded frame to detect termination.

**Why match the bug**: closing on errored / already-completed events is a clean fix, but it changes the wire contract. SDKs and frontends written against Python's behavior will not have a disconnect handler for those cases — they'll treat an unexpected close as a transient network issue and reconnect. To preserve client compatibility, Rust replicates Python's behavior. Document the quirk in the SDK changelogs; do not silently fix.

## 7. Error close codes

| Code | Reason | When |
|---|---|---|
| `1000` Normal Closure | (no reason text) | `PipelineRunCompleted` received and forwarded. Errored / AlreadyCompleted do NOT trigger a close. |
| `1008` Policy Violation | `"Unauthorized"` | Auth fails (no cookie / bad signature / expired / unknown user). |
| `1011` Internal Error | `"channel lagged"` | Subscriber fell behind by more than `channel_capacity` events (default 64; configurable via [`cognee_core::pipeline_run_registry::RegistryConfig::channel_capacity`](pipelines.md#62-public-types)). Indicates a stalled or slow client. |
| `1011` Internal Error | `"<error message>"` | Unhandled exception while computing the payload or sending. |
| `4404` Application: Not Found | `"unknown pipeline_run_id"` | (Future) The run id is unknown and the registry refuses to wait. Currently we always wait, matching Python. |

The custom `4xxx` codes are reserved for future use; phase 1 does not emit any custom code.

## 8. Heartbeat / keepalive

- Server does not send Ping frames proactively. Tokio's WebSocket implementation handles connection-level pongs automatically when peers send Pings.
- We rely on **TCP keepalive** + the natural cadence of pipeline yields to keep proxies happy.
- For pipelines that go silent for extended periods (e.g. a stuck stage), the connection may be killed by intermediate proxies (nginx default timeout: 60s). Mitigation: emit `PipelineRunYield` heartbeats every 30s during idle stages. Producer-side change in `cognee_core::PipelineRunRegistry`, not the WS handler. **Deferred to phase 2.**

## 9. Server-side implementation

### 9.1 Handler skeleton

```rust
// crates/http-server/src/routers/cognify.rs
use axum::extract::ws::{WebSocketUpgrade, WebSocket, Message, CloseFrame, CloseCode};
use axum::extract::{Path, State};

pub async fn ws_subscribe(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
    cookies: CookieJar,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state, run_id, cookies))
}

async fn ws_loop(mut socket: WebSocket, state: AppState, run_id: Uuid, cookies: CookieJar) {
    // 1. Auth (matches §4)
    let user = match auth_from_cookie(&state, &cookies).await {
        Ok(u) => u,
        Err(_) => return close(&mut socket, 1008, "Unauthorized").await,
    };

    // 2. Subscribe (always succeeds; missing run_id is treated as empty)
    let mut events = state.pipelines.subscribe(run_id);   // cognee_core::PipelineRunRegistry::subscribe

    // 3. Forward
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return close(&mut socket, 1011, "channel lagged").await;
            }
        };

        let payload = state.lib.formatted_graph_data(event.dataset_id, &user)
            .await
            .unwrap_or_else(|_| serde_json::json!({}));

        let frame = serde_json::json!({
            "pipeline_run_id": event.run_id,
            "status":          event.status_str(),
            "payload":         payload,
        });

        if socket.send(Message::Text(frame.to_string())).await.is_err() {
            // Client disconnected
            return;
        }

        // Match Python: only PipelineRunCompleted closes the socket.
        // Errored and AlreadyCompleted are forwarded but the loop continues.
        if event.is_completed() {
            close(&mut socket, 1000, "").await;
            return;
        }
    }
}
```

### 9.2 Why we accept the upgrade *before* auth

It matches Python: [`await websocket.accept()`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L294) runs unconditionally; cookie lookup and rejection happen on the established connection. This costs us a TCP+TLS handshake on bad-auth attempts, but the alternative (rejecting at the HTTP layer) is harder in axum and would diverge from Python behavior.

### 9.3 Subscription before producer

`cognee_core::PipelineRunRegistry::subscribe(run_id)` matches Python's [`initialize_queue` semantics](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L319) — if the run id is unknown, the registry returns an empty `Stream` attached to a placeholder slot. This lets clients connect *before* the producer starts (race-free); the channel is bounded by `RegistryConfig::channel_capacity` (default 64). Empty placeholders are evicted by the registry's TTL sweep if no producer ever attaches (see [pipelines.md §11](pipelines.md#11-eviction--resource-budget)).

## 10. Client expectations

Documented behavior for SDK / frontend implementers:

- **Reconnect on socket close 1011**: indicates server-side lag or transient error; reconnect with exponential backoff.
- **Do not reconnect on 1000**: the run is terminal; refetch via `/datasets/status` or `/datasets/{id}/graph`.
- **Do not reconnect on 1008**: auth is broken; redirect to login.
- **Treat status=`PipelineRunErrored` as success on the WebSocket level**: the run failed but the WebSocket delivered the failure correctly.
- **Idempotent connect**: subscribing twice to the same run_id is fine — both clients see the same event stream.

## 11. Testing strategy

| Layer | Tests |
|---|---|
| Unit | Frame serialization (TEXT, UTF-8, JSON shape); close-code mapping. |
| Auth | Connect with no cookie → close 1008; with expired JWT → close 1008; with cookie audience mismatch → close 1008. |
| Subscription | Two clients subscribe to the same run; producer emits 5 events including a terminal; both clients see all 5 + close 1000. |
| Lag | Slow consumer that doesn't read for 100 events; assert close 1011 and the producer is unaffected. |
| Race | Subscribe *before* the producer starts; assert no events are lost. |
| Disconnect | Client closes early; assert the producer continues and the registry tears down the subscriber slot. |
| Cross-SDK | Drive the same `pipeline_run_id` in Python and Rust, capture the WS frame stream from both, diff JSON shapes. |

Test harness: `tokio-tungstenite` for the client, `axum::serve` on `127.0.0.1:0` for the server. Test fixtures in `crates/http-server/tests/ws/`.

## 12. Open questions

1. **Authorization (not authentication)**: Python lets any authenticated user subscribe to any `pipeline_run_id`. Rust matches verbatim — no ownership verification, no dataset-level gate. Strict wire parity; the gap is documented for SDK consumers.
2. **Heartbeat frames**: Python doesn't emit them; long-stalled stages can be killed by proxies. See §8.
3. **Bearer/API-key alternative**: needed only when SDKs (not browsers) drive WebSockets. Phase-2 with a `?token=…` query parameter behind a `ws-token-query` feature flag.
4. **Replay from durable status table**: clients that connect *after* a run completed see no events (the channel is gone). Should we synthesize a single `PipelineRunCompleted` frame from the latest `pipeline_runs` row? Useful but inconsistent with Python. Defer.
5. **Frame encoding**: TEXT vs BINARY (msgpack). TEXT is what Python uses; BINARY would be smaller for large graph payloads. Profile first; defer.

## 13. References

- Python WS handler: [`cognee/api/v1/cognify/routers/get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py) (the `subscribe_to_cognify_info` block at the bottom of the file).
- Python in-memory queues: [`cognee/modules/pipelines/queues/pipeline_run_info_queues.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/queues/pipeline_run_info_queues.py).
- Python event types: [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
- Cookie name + JWT secret env vars: [auth.md §3, §4](auth.md).
- Channel + registry that this endpoint consumes: [pipelines.md §5, §9](pipelines.md).
- WebSocket close codes (RFC 6455): [https://datatracker.ietf.org/doc/html/rfc6455#section-7.4](https://datatracker.ietf.org/doc/html/rfc6455#section-7.4).
