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

The switch is **process-wide** (one dispatcher per process, whichever SDK handle
emits the event) and is configured by
[`TelemetryConfig`](../../crates/telemetry/src/config.rs): defaults (on) <
environment < code. The environment is read when an event is sent (so a `.env`
loaded after startup is honoured); an explicit call wins over it in both directions
until `reset_to_env`.

| How | Effect |
|---|---|
| `cognee::telemetry::set_enabled(false)` (Rust), `set_telemetry_enabled(False)` (Python), `setTelemetryEnabled(false)` (TS), `Cognee.setTelemetryEnabled(false)` (Java), `cognee_set_telemetry_enabled(0)` (C) | Disables for the whole process, from code. Also drops events queued but not yet posted. `is_enabled()` / the binding equivalents report the state. |
| `cognee-cli --no-telemetry …` | Disables for one CLI invocation. |
| `TELEMETRY_DISABLED=1` (any non-empty value) | Disables, as the environment default. |
| `ENV=test` or `ENV=dev` | Disables, as the environment default. |
| Build with `--no-default-features` | Disables at compile time. `send_telemetry` / `try_send_telemetry` remain in the public surface but compile to no-op bodies — no `reqwest`, no tokio fallback, no PBKDF2 cost — and `is_enabled()` is always `false`. |

The check happens before any identity derivation or HTTP work, so a disabled
process pays nothing per event. See
[configuration.md § Product analytics](../configuration.md#product-analytics) for the
per-binding API.

## Identity

Events carry several non-PII identifiers:

| Field | Source | Storage |
|---|---|---|
| `anonymous_id` | random uuid4 (override with `TRACKING_ID` / `TelemetryConfig::tracking_id`) | `<project_root>/.anon_id` |
| `persistent_id` | random uuid4 | `~/.cognee/.persistent_id` (machine-local; survives `forget(everything=True)`) |
| `api_key_tracking_id` / `api_key_hash` | deterministic PBKDF2-HMAC-SHA256 of the resolved LLM API key (`llm.api_key` of the live `ComponentManager`s; empty if they disagree) | not stored (cached in memory per key and salt) |
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

Read by `TelemetryConfig::from_env()` when an event is sent, unless code has pinned
the configuration. Each has a `TelemetryConfig` field settable from code
(`cognee::telemetry::configure`; `set_enabled` pins only `enabled`).

| Variable | Field | Default | Effect |
|---|---|---|---|
| `TELEMETRY_DISABLED` | `enabled` | _(unset)_ | Any non-empty value disables. |
| `ENV` | `enabled` | _(unset)_ | `test` or `dev` disables. |
| `COGNEE_HOST_SDK` | `host_sdk` | _(unset)_ | Suppresses a language binding's events when a host SDK sends its own. |
| `TRACKING_ID` | `tracking_id` | _(unset)_ | Overrides `anonymous_id`. |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `api_key_tracking_salt` | `cognee.telemetry.api-key-tracking.v1` | PBKDF2 salt override (rotate to invalidate prior api-key ids). |
| `TELEMETRY_REQUEST_TIMEOUT` | `request_timeout_secs` | `5` | HTTP timeout in seconds, clamped to `[1, 60]`. |
| `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` | `endpoint_override` | _(unset)_ | Test-harness collector; honoured only while `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty. |

The `api_key_tracking_id` in each event is a salted hash of the **resolved** LLM key
(`llm.api_key`, from env, a config file or code). Every `ComponentManager` holds a
`cognee::telemetry::LlmKeyRegistration` for its key, updated when its configuration
changes and dropped with the manager; embedders without a manager call
`cognee::telemetry::set_llm_api_key`. The `LLM_API_KEY` variable is not read by the
analytics client itself. When every live registration uses the same key, its hash is
reported; when live handles use *different* keys, process-wide events cannot be
attributed to one of them and the id is reported empty rather than mislabelled.

*Divergence from Python:* Python hashes `os.getenv("LLM_API_KEY")` only. Rust hashes
the key it actually uses, which can also come from its `OPENAI_TOKEN` alias, a config
file or code. The derivation is identical (PBKDF2-HMAC-SHA256, same salt), so the same
key gives the same id in both SDKs, which is what cross-SDK grouping needs; a Rust
process configured only through `OPENAI_TOKEN` reports an id where a Python process
without `LLM_API_KEY` (and so without a working LLM) reports none.

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
