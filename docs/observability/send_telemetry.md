# Product Analytics (`send_telemetry`)

Cognee-Rust includes an opt-out product-analytics client that mirrors Python's
`cognee.shared.utils.send_telemetry`. For every public API call it fires a
single fire-and-forget HTTP POST to `https://test.prometh.ai`, giving the cognee
maintainers an aggregate view of how the SDK is exercised. It is implemented in
the [`cognee-telemetry`](../../crates/telemetry/) crate.

This is **separate** from OpenTelemetry tracing — see
[`opentelemetry.md`](opentelemetry.md) for that. The two are configured
independently.

## Enabled by default

Analytics is on by default (Python parity). It is **fire-and-forget**: events
are dispatched without blocking the API call, and failures are swallowed (logged
at debug under the `cognee.telemetry` target only).

## Opting out

| How | Effect |
|---|---|
| `TELEMETRY_DISABLED=1` (any non-empty value) | Disables at runtime. Checked on every call before any identity derivation or HTTP work, so the cost is zero. |
| `ENV=test` or `ENV=dev` | Disables at runtime. |
| Build with `--no-default-features` | Disables at compile time. `send_telemetry` / `try_send_telemetry` remain in the public surface but compile to no-op bodies — no `reqwest`, no tokio fallback, no PBKDF2 cost. |

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
