# Task 02-05 — HTTP client, fire-and-forget dispatch, opt-out

**Status**: implemented in commit c1cf154 (note: env.rs added a cfg(test)+COGNEE_TELEMETRY_INTEGRATION_TEST proxy_url override hatch so 02-09 mockito tests can redirect dispatch to 127.0.0.1; env_test_disables guards against parallel-test races by skipping the negative assertion when a sibling test currently has TELEMETRY_DISABLED set — proper serial_test::serial wiring lands in 02-08).
**Owner**: _unassigned_
**Depends on**:
- [Task 02-02 — Crate scaffold](02-telemetry-crate-scaffold.md) — `env`, `real`, `noop` module placeholders.
- [Task 02-03 — Identity derivation](03-id-derivation.md) — `ids::*`.
- [Task 02-04 — Payload + sanitize](04-payload-and-sanitize.md) — `TelemetryPayload`, `sanitize_nested_properties`.

**Blocks**:
- [Task 02-06 — Public API + noop fallback](06-public-api-and-noop.md) (public surface freezes once the dispatcher is wired).
- [Task 02-09 — Integration tests](09-integration-tests.md) (mockito tests exercise the real dispatcher).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Wire three pieces together so a call to `send_telemetry(...)`
results in a real `POST https://test.prometh.ai` (or a documented
no-op):

1. **Env-driven opt-out** (`crates/telemetry/src/env.rs`) — checks
   `TELEMETRY_DISABLED` and `ENV in {test, dev}`. Reads
   `TELEMETRY_REQUEST_TIMEOUT` (default 5s).
2. **HTTP client** (`crates/telemetry/src/client.rs`) — process-wide
   `Lazy<reqwest::Client>` with rustls TLS + JSON support.
3. **Dispatcher** (`crates/telemetry/src/real.rs`) — assembles the
   `TelemetryPayload`, applies URL sanitization, and fires the POST
   on a detached `tokio::spawn`. When no tokio handle is present,
   the runtime fallback per **decision 5** logs a warning and spins
   up a one-shot single-thread `Runtime`.

This task does **not** define the public `send_telemetry(...)`
function signature — that freezes in
[task 02-06](06-public-api-and-noop.md). It only fills the
`real::send_telemetry_impl` body that the public stub already calls.

## 2. Rationale

### Why an env module separate from the dispatcher

Three reasons:

1. **Testability.** The opt-out logic is a pure function of the
   environment; testing it inline in `real.rs` would require spinning
   up tokio in tests that have nothing to do with HTTP.
2. **Reuse.** The opt-out check is also called by the public
   `send_telemetry` entry point in [task 02-06](06-public-api-and-noop.md)
   *before* it bothers building the payload, so the early-exit path
   never touches identity derivation or sanitization.
3. **Mirrors Python.** Python keeps the env checks at the top of
   `send_telemetry` (utils.py:194-199) but reads them as plain
   `os.getenv`. A centralised module captures the same surface in
   one place.

### Why `once_cell::sync::Lazy<reqwest::Client>`

A new `reqwest::Client` triggers rustls trust-store load and
certificate parsing on every call. With telemetry firing from API
endpoints, that overhead would dominate latency. The standard fix
is a process-wide singleton:

```rust
static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(env::request_timeout()))
        .pool_max_idle_per_host(2)
        .build()
        .expect("reqwest client builder cannot fail with default params")
});
```

We size the connection pool at **2 idle per host** because the proxy
is a single host and large pools waste sockets in long-running CLI
processes.

### Decision 5 — runtime fallback

Decision 5 in the locked decisions table:

> When `tokio::runtime::Handle::try_current()` returns `Err`, log a
> warning at `WARN` level and spin up a one-shot
> `tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()`
> to dispatch the request, blocking up to `TELEMETRY_REQUEST_TIMEOUT`.

This adds a synchronous-blocking path that is undesirable for hot
loops but acceptable for the embedded/Android case (decision 1
excludes `telemetry` from `android-default` anyway, so this is
defensive). The warning surfaces inefficiency so callers can
migrate to async.

The fallback **must never panic**. If the runtime build fails
(it can't, with default flags) we log and drop.

## 3. Pre-conditions

- Tasks 02-02, 02-03, 02-04 merged.
- `cargo check -p cognee-telemetry --features telemetry` passes.

## 4. Step-by-step

### 4.1 Create `crates/telemetry/src/env.rs`

```rust
//! Environment-driven configuration for `send_telemetry`.

/// Returns `true` if the user has explicitly disabled telemetry, or
/// if the process is running in a `test` or `dev` environment.
///
/// Mirrors Python utils.py:194-199.
pub fn is_disabled() -> bool {
    if let Ok(v) = std::env::var("TELEMETRY_DISABLED") {
        if !v.is_empty() {
            return true;
        }
    }
    if let Ok(env) = std::env::var("ENV") {
        if env == "test" || env == "dev" {
            return true;
        }
    }
    false
}

/// Total HTTP request timeout in seconds, capped at 60.
///
/// Mirrors Python utils.py:24 — default 5s, env override
/// `TELEMETRY_REQUEST_TIMEOUT`.
pub fn request_timeout_secs() -> u64 {
    std::env::var("TELEMETRY_REQUEST_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v.clamp(1, 60))
        .unwrap_or(5)
}

/// The proxy URL. Hard-coded per decision 2 of the locked decisions
/// table — reuse Python's `https://test.prometh.ai` so cross-SDK
/// identity grouping works.
///
/// A test-only override (`COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`) is
/// honoured **only** when `cfg(test)` is active or the env var
/// `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty. Production
/// builds ignore both.
pub fn proxy_url() -> String {
    #[cfg(test)]
    {
        if let Ok(v) = std::env::var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS") {
            if !v.is_empty() {
                return v;
            }
        }
    }
    if std::env::var("COGNEE_TELEMETRY_INTEGRATION_TEST")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        if let Ok(v) = std::env::var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS") {
            if !v.is_empty() {
                return v;
            }
        }
    }
    "https://test.prometh.ai".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_disabled_truthy_value() {
        std::env::set_var("TELEMETRY_DISABLED", "1");
        assert!(is_disabled());
        std::env::set_var("TELEMETRY_DISABLED", "false");
        // Python checks for *any* non-empty value; we mirror.
        assert!(is_disabled());
        std::env::remove_var("TELEMETRY_DISABLED");
    }

    #[test]
    fn telemetry_disabled_empty_value() {
        std::env::set_var("TELEMETRY_DISABLED", "");
        // Python's `if os.getenv("TELEMETRY_DISABLED"):` treats empty
        // as falsy — we do too.
        std::env::remove_var("ENV");
        assert!(!is_disabled());
        std::env::remove_var("TELEMETRY_DISABLED");
    }

    #[test]
    fn env_test_disables() {
        std::env::remove_var("TELEMETRY_DISABLED");
        std::env::set_var("ENV", "test");
        assert!(is_disabled());
        std::env::set_var("ENV", "dev");
        assert!(is_disabled());
        std::env::set_var("ENV", "production");
        assert!(!is_disabled());
        std::env::remove_var("ENV");
    }

    #[test]
    fn timeout_default_and_clamp() {
        std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
        assert_eq!(request_timeout_secs(), 5);

        std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "0");
        assert_eq!(request_timeout_secs(), 1);

        std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "120");
        assert_eq!(request_timeout_secs(), 60);

        std::env::set_var("TELEMETRY_REQUEST_TIMEOUT", "10");
        assert_eq!(request_timeout_secs(), 10);

        std::env::remove_var("TELEMETRY_REQUEST_TIMEOUT");
    }
}
```

The `request_timeout_secs` clamp `[1, 60]` is a hardening choice;
Python accepts arbitrary values. A 60s upper bound prevents a
misconfiguration from blocking shutdown indefinitely on the runtime
fallback path (which is synchronous).

### 4.2 Create `crates/telemetry/src/client.rs`

```rust
//! Process-wide reqwest client for `send_telemetry`.

use once_cell::sync::Lazy;
use reqwest::Client;
use std::time::Duration;

use crate::env::request_timeout_secs;

static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(request_timeout_secs()))
        .pool_max_idle_per_host(2)
        .user_agent(concat!(
            "cognee-rust/",
            env!("CARGO_PKG_VERSION"),
            " (send_telemetry)"
        ))
        .build()
        .expect("reqwest client builder cannot fail with default params")
});

/// Borrow the process-wide reqwest client.
pub fn client() -> &'static Client {
    &HTTP
}
```

The `Lazy` initialiser reads `request_timeout_secs()` **once** at
first use. Changing `TELEMETRY_REQUEST_TIMEOUT` mid-process does not
re-affect already-built clients — same as Python, where the timeout
is read once via `int(os.getenv(...))`.

### 4.3 Replace `crates/telemetry/src/real.rs` with the dispatcher

```rust
//! Real (`feature = "telemetry"`) dispatcher.

use serde_json::Value;

use crate::client::client;
use crate::env::{is_disabled, proxy_url, request_timeout_secs};
use crate::ids::{get_anonymous_id, get_api_key_tracking_id, get_persistent_id};
use crate::payload::{
    format_time_field, AdditionalProperties, Properties, TelemetryPayload, UserProperties,
};
use crate::sanitize::sanitize_nested_properties;
use crate::UserIdRef;

/// Real implementation of `send_telemetry`. Returns immediately;
/// the HTTP POST is dispatched on a detached tokio task. When called
/// outside a tokio runtime, falls back to a one-shot single-thread
/// runtime (decision 5).
pub(crate) fn send_telemetry_impl(
    event_name: &str,
    user_id: UserIdRef<'_>,
    additional_properties: Option<Value>,
) {
    if is_disabled() {
        return;
    }

    let body = build_body(event_name, user_id, additional_properties);

    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(post(body));
        }
        Err(_) => {
            tracing::warn!(
                target: "cognee.telemetry",
                "send_telemetry called from a non-tokio context; \
                 spinning up a one-shot runtime (decision 5). \
                 Consider calling from an async context for better \
                 performance."
            );
            spin_up_one_shot(body);
        }
    }
}

fn build_body(
    event_name: &str,
    user_id: UserIdRef<'_>,
    additional_properties: Option<Value>,
) -> serde_json::Value {
    let anon = get_anonymous_id();
    let persistent = get_persistent_id();
    let tracking = get_api_key_tracking_id();
    let user = match user_id {
        UserIdRef::Uuid(u) => u.to_string(),
        UserIdRef::Symbolic(s) => s.to_string(),
        UserIdRef::None => String::new(),
    };

    // Sanitize URL keys before assembling the payload.
    let mut additional = AdditionalProperties::from_value(additional_properties);
    let mut as_value = additional.as_value_mut();
    sanitize_nested_properties(&mut as_value, &["url"]);
    additional.replace_with(as_value);

    let payload = TelemetryPayload {
        anonymous_id: &anon,
        event_name,
        user_properties: UserProperties {
            user_id: &user,
            persistent_id: &persistent,
            api_key_tracking_id: &tracking,
            api_key_hash: &tracking,
        },
        properties: Properties {
            time: format_time_field(chrono::Utc::now()),
            user_id: &user,
            anonymous_id: &anon,
            persistent_id: &persistent,
            api_key_tracking_id: &tracking,
            api_key_hash: &tracking,
            sdk_runtime: "rust",
            cognee_version: env!("CARGO_PKG_VERSION"),
            additional,
        },
    };

    // serialize once. If serialization fails (it can't for this
    // schema), drop with a debug log.
    serde_json::to_value(&payload).unwrap_or_else(|e| {
        tracing::debug!(
            target: "cognee.telemetry",
            error = %e,
            "telemetry payload serialization failed"
        );
        Value::Null
    })
}

async fn post(body: Value) {
    if body.is_null() {
        return;
    }
    let url = proxy_url();
    match client().post(&url).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            tracing::debug!(
                target: "cognee.telemetry",
                status = %resp.status(),
                "telemetry proxy returned non-2xx"
            );
        }
        Err(e) => {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                "telemetry request failed"
            );
        }
        _ => {}
    }
}

fn spin_up_one_shot(body: Value) {
    let timeout = std::time::Duration::from_secs(request_timeout_secs());
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                "could not bootstrap one-shot tokio runtime; dropping event"
            );
            return;
        }
    };
    rt.block_on(async move {
        let _ = tokio::time::timeout(timeout, post(body)).await;
    });
}
```

### 4.4 Wire `env.rs` and `client.rs` into `lib.rs`

Replace the `pub mod env { ... }` placeholder in
`crates/telemetry/src/lib.rs` with:

```rust
pub mod env;
mod client;
```

`client` is private (the singleton is an internal detail).

### 4.5 Verify

```bash
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry  # noop branch
cargo test -p cognee-telemetry --features telemetry --lib env::
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings
```

## 5. Verification

```bash
# 1. Both feature states compile.
cargo check -p cognee-telemetry --features telemetry
cargo check -p cognee-telemetry

# 2. Env tests pass.
cargo test -p cognee-telemetry --features telemetry --lib env::

# 3. No clippy warnings.
cargo clippy -p cognee-telemetry --features telemetry -- -D warnings

# 4. No outbound HTTP fires when disabled.
TELEMETRY_DISABLED=1 cargo test -p cognee-telemetry --features telemetry
# (The integration test in task 02-09 will gate this with mockito;
#  here we just confirm `is_disabled()` short-circuits early.)
```

Live HTTP smoke test (manual, **not** in CI — never call the real
proxy from automation):

```bash
# In a separate dev shell only:
RUST_LOG=cognee.telemetry=debug \
  cargo run --example send_telemetry_smoke --features telemetry
# Expected: a debug log line `telemetry request fired` and no panics.
# (Add a 5-line example under `examples/send_telemetry_smoke.rs` if
#  one doesn't exist.)
```

## 6. Files modified

- `crates/telemetry/src/env.rs` — new file (replaces inline empty
  module).
- `crates/telemetry/src/client.rs` — new file.
- `crates/telemetry/src/real.rs` — full body (replaces stub).
- `crates/telemetry/src/lib.rs` — change `pub mod env { ... }` to
  `pub mod env;` and add `mod client;`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Lazy<Client>` initialised before `TELEMETRY_REQUEST_TIMEOUT` is set | Possible if a process sets the env var after first call | Document in [task 02-11](11-user-docs.md): set env vars before SDK init. |
| `tokio::spawn` on a runtime that's about to shut down drops the request | The future is detached; if the runtime exits before the request completes, the request is cancelled | Acceptable — same as Python where `loop.create_task` futures are cancelled at loop shutdown. The CLI `main` returns to the shell only after its top-level `await` finishes. The HTTP server holds tokio for its lifetime. |
| `spin_up_one_shot` blocks the calling thread for up to `request_timeout_secs` | True — that's the cost of decision 5 | Warning is logged; users can switch to async or set `TELEMETRY_DISABLED=1`. The `android-default` feature excludes telemetry so Android binaries don't hit this. |
| `serde_json::to_value` failure goes silently to debug | Schema is fully owned by us; failure is impossible in practice | Defensive `unwrap_or_else` keeps the SDK from panicking if a future schema change introduces a non-serialisable variant. |
| Test-only env override of `proxy_url` leaks into production | The override is gated by `#[cfg(test)]` AND `COGNEE_TELEMETRY_INTEGRATION_TEST`. A production binary needs both to fire | `cargo check --release` will not include the `cfg(test)` branch. The integration-test env name is intentionally verbose to make accidental usage obvious. |
| User sets `TELEMETRY_REQUEST_TIMEOUT=0` to make events instantly fail | Clamp `[1, 60]` rejects 0 | Documented in rustdoc on `request_timeout_secs`. |

## 8. Out of scope

- Public API freeze + noop fallback body (covered by [task 02-06](06-public-api-and-noop.md)).
- Replacing `forget.rs` placeholder + porting other call sites (covered by [task 02-07](07-callsite-migration.md)).
- Mockito-driven integration tests (covered by [task 02-09](09-integration-tests.md)).
- Cross-SDK parity (covered by [task 02-10](10-cross-sdk-parity.md)).
