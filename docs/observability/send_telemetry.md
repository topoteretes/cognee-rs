# Product Analytics (`send_telemetry`)

Cognee-Rust includes a product-analytics client derived from Python's
`cognee.shared.utils.send_telemetry`. When explicitly authorized, public API
calls can fire a single fire-and-forget HTTP POST to `https://test.prometh.ai`.
It is implemented in the
[`cognee-telemetry`](../../crates/telemetry/) crate.

This is **separate** from OpenTelemetry tracing — see
[`opentelemetry.md`](opentelemetry.md) for that. The two are configured
independently.

## Disabled by default

The local sovereign baseline deliberately diverges from Python parity: analytics
are off unless the operator explicitly opts in. Compiling the `telemetry`
feature supplies capability only; it does not grant runtime permission.

When authorized, events are **fire-and-forget**: they are dispatched without
blocking the API call, and failures are swallowed (logged at debug under the
`cognee.telemetry` target only).

## Opting in and suppressing

| How | Effect |
|---|---|
| `COGNEE_PRODUCT_TELEMETRY_ENABLED=1` | Explicitly enables product analytics. `true`, `yes`, and `on` are also accepted case-insensitively. Missing or any other value fails closed. |
| `TELEMETRY_DISABLED=1` (any non-empty value) | Disables at runtime. Checked on every call before any identity derivation or HTTP work, so the cost is zero. |
| `ENV=test` or `ENV=dev` | Disables at runtime. |
| Build with `--no-default-features` | Disables at compile time. `send_telemetry` / `try_send_telemetry` remain in the public surface but compile to no-op bodies — no `reqwest`, no tokio fallback, no PBKDF2 cost. |

Suppressions take precedence over opt-in.

## Identity

Events carry several non-PII identifiers:

| Field | Source | Storage |
|---|---|---|
| `anonymous_id` | random uuid4 (override with `TRACKING_ID`) | `<project_root>/.anon_id` |
| `persistent_id` | random uuid4 | `~/.cognee/.persistent_id` (machine-local; survives `forget(everything=True)`) |
| `api_key_tracking_id` / `api_key_hash` | deterministic PBKDF2-HMAC-SHA256 of `LLM_API_KEY` | not stored (re-derived per event) |
| `user_id` | passed by the caller | — |

`api_key_hash` is a backward-compatibility alias carrying the same value as
`api_key_tracking_id`.

## Payload schema

Each event is a JSON object:

| Field | Notes |
|---|---|
| `time` | event timestamp |
| `event_name` | e.g. `cognee.forget` |
| `user_id`, `anonymous_id`, `persistent_id` | identity (see above) |
| `api_key_tracking_id`, `api_key_hash` | PBKDF2 id of the LLM API key |
| `sdk_runtime` | `"rust"` — lets the backend distinguish Rust from Python events |
| `cognee_version` | crate version |
| `additional_properties` | caller-supplied `Value::Object`; reserved keys above MUST NOT appear and non-object values are dropped at sanitization time |

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `COGNEE_PRODUCT_TELEMETRY_ENABLED` | _(unset)_ | Explicit runtime permission. Recognized values: `1`, `true`, `yes`, `on`. |
| `TELEMETRY_DISABLED` | _(unset)_ | Any non-empty value disables. Read on every call. |
| `ENV` | _(unset)_ | `test` or `dev` disables. Read on every call. |
| `LLM_API_KEY` | _(unset)_ | Source of `api_key_tracking_id`; read at every event, never cached. |
| `TRACKING_ID` | _(unset)_ | Overrides `anonymous_id`. |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `cognee.telemetry.api-key-tracking.v1` | PBKDF2 salt override (rotate to invalidate prior api-key ids). |
| `TELEMETRY_REQUEST_TIMEOUT` | `5` | HTTP timeout in seconds, clamped to `[1, 60]`. Read once per process. |

## Logging / troubleshooting

All diagnostics use the `cognee.telemetry` tracing target. To see what is (or
isn't) being sent:

```bash
RUST_LOG=cognee.telemetry=debug cognee-cli search --query "..."
```

## Usage

```rust,ignore
use cognee_telemetry::send_telemetry;
use serde_json::json;

send_telemetry(
    "cognee.forget",
    "user-id-string",
    Some(json!({ "endpoint": "POST /api/v1/forget" })),
);
```

Without the explicit opt-in, this call is a no-op before identity derivation or
network-client construction.
