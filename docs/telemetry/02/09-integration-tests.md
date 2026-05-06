# Task 02-09 — Integration tests with `mockito`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md) (the dispatcher must be wired).
- [Task 02-07 — Callsite migration](07-callsite-migration.md) (we exercise a real call site, not just the public function).

**Blocks**:
- [Task 02-10 — Cross-SDK parity](10-cross-sdk-parity.md) (uses similar mock patterns inside Docker).
- [Task 02-12 — CI updates](12-ci-updates.md) (CI runs the new lane).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Add three end-to-end integration tests under
`crates/telemetry/tests/` that exercise the full dispatch path
against a `mockito` server bound to `127.0.0.1:0`:

1. **Schema parity** — assert the captured payload byte-equals a
   Python-generated reference, except for the volatile fields
   (`anonymous_id`, `persistent_id`, `time` — the test sets
   `TRACKING_ID` and pins time to remove this volatility).
2. **Opt-out** — set `TELEMETRY_DISABLED=1` and assert mockito
   receives **zero** requests over a 1-second window.
3. **Fire-and-forget timeout** — proxy stalls 30s; the dispatcher
   returns in < 100 ms (the request is detached, the timeout fires
   in the background).

Per **decision 10**, we use `mockito` (already a workspace dev-dep
in `cognee-cli` and `cognee-cloud`) — **not** `wiremock`. The
integration tests bind to `127.0.0.1` only; the live proxy
`https://test.prometh.ai` is **never** exercised from CI.

## 2. Rationale

### Why three tests, not one mega-test

Each test has a distinct failure signature:

- A schema-parity failure says "Rust produces wrong JSON".
- An opt-out failure says "the env-check did not short-circuit".
- A timeout failure says "the dispatcher blocks the caller".

Bundling them into one test makes diagnosis harder.

### Why a Python-generated reference is overkill here

For *byte* parity, [task 02-08](08-unit-tests.md) already covers
PBKDF2 against fixtures. For *schema* parity, an in-repo Rust
reference is enough — the cross-SDK byte assertion lives in
[task 02-10](10-cross-sdk-parity.md). This task just verifies that
the wire format the Rust side produces matches the contract
(`anonymous_id` / `event_name` / `user_properties` / `properties`
nesting; `api_key_hash` mirrors `api_key_tracking_id`; `time`
matches `MM/DD/YYYY`).

### Why the test-only env override is gated

[Task 02-05](05-client-dispatch-and-optout.md) added
`COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` plus
`COGNEE_TELEMETRY_INTEGRATION_TEST` flags so production builds
ignore the override. Tests set both before running.

## 3. Pre-conditions

- Tasks 02-05, 02-06, 02-07 merged.
- `mockito = "1"` is a dev-dep of `cognee-telemetry` (added in
  [task 02-02](02-telemetry-crate-scaffold.md)).
- Rust callsite (`forget.rs`) is wired (from [task 02-07](07-callsite-migration.md)).

## 4. Step-by-step

### 4.1 Create `crates/telemetry/tests/dispatch_with_mockito.rs`

```rust
//! Integration tests exercising the full `send_telemetry` dispatch
//! path against a mockito server. All HTTP traffic stays on
//! 127.0.0.1; the live proxy `https://test.prometh.ai` is NEVER
//! contacted from these tests.

#![cfg(feature = "telemetry")]

use mockito::Server;
use serde_json::Value;
use serial_test::serial;
use std::time::Duration;
use tempfile::TempDir;

use cognee_telemetry::send_telemetry;

/// Set up an isolated env: a temp HOME, a fixed TRACKING_ID, a fresh
/// LLM_API_KEY, and the mockito URL injected via the test override.
struct IsolatedEnv {
    _home: TempDir,
}

impl IsolatedEnv {
    fn install(server_url: &str) -> Self {
        let home = TempDir::new().expect("tempdir");
        std::env::set_var("HOME", home.path());
        std::env::set_var("TRACKING_ID", "fixed-anon-12345");
        std::env::set_var("LLM_API_KEY", "sk-test-fixture");
        std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        std::env::remove_var("TELEMETRY_DISABLED");
        std::env::remove_var("ENV");
        std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
        std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", server_url);
        // Wipe the persistent-id / anon-id caches so the new HOME
        // takes effect.
        cognee_telemetry::ids::__test_only_reset_caches();
        Self { _home: home }
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        for k in [
            "HOME",
            "TRACKING_ID",
            "LLM_API_KEY",
            "TELEMETRY_API_KEY_TRACKING_SALT",
            "TELEMETRY_DISABLED",
            "ENV",
            "COGNEE_TELEMETRY_INTEGRATION_TEST",
            "COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS",
        ] {
            std::env::remove_var(k);
        }
    }
}

/// Wait up to `timeout` for `mock` to be hit at least once. Polls at
/// 25 ms intervals to keep flake low.
async fn wait_for_hit(mock: &mockito::Mock, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if mock.matched() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
#[serial]
async fn schema_parity_against_reference() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .match_header("content-type", "application/json")
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());

    send_telemetry(
        "cognee.forget",
        "user-id-string",
        Some(serde_json::json!({
            "target": "everything",
            "dataset": "",
            "data_id": "",
            "cognee_version": "0.1.0-test",
            "url": "https://example.com/private",
        })),
    );

    assert!(wait_for_hit(&mock, Duration::from_secs(5)).await);

    // Inspect the captured body.
    let body_bytes = mock
        .last_request()
        .expect("at least one request")
        .body()
        .to_owned();
    let body: Value = serde_json::from_slice(&body_bytes).expect("json");

    // Top-level shape.
    assert_eq!(body["event_name"], "cognee.forget");
    assert_eq!(body["anonymous_id"], "fixed-anon-12345");

    // user_properties tuple.
    let up = &body["user_properties"];
    assert!(up["api_key_tracking_id"].as_str().unwrap().starts_with("ak_"));
    assert_eq!(up["api_key_tracking_id"], up["api_key_hash"]);
    assert!(up["persistent_id"].as_str().unwrap().len() >= 32);

    // properties tuple, including the additional flatten.
    let p = &body["properties"];
    assert_eq!(p["sdk_runtime"], "rust");
    assert_eq!(p["target"], "everything");
    assert_eq!(p["cognee_version"], "0.1.0-test");

    // URL was sanitized via uuid5.
    let sanitized_url = p["url"].as_str().expect("url is string");
    assert!(uuid::Uuid::parse_str(sanitized_url).is_ok(),
            "expected uuid5, got {sanitized_url}");
    assert_ne!(sanitized_url, "https://example.com/private");

    // time matches MM/DD/YYYY.
    let time_re = regex::Regex::new(r"^\d{2}/\d{2}/\d{4}$").unwrap();
    assert!(
        time_re.is_match(p["time"].as_str().unwrap()),
        "unexpected time format: {}", p["time"]
    );

    mock.assert_async().await;
}

#[tokio::test]
#[serial]
async fn opt_out_via_telemetry_disabled() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .expect(0)
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());
    std::env::set_var("TELEMETRY_DISABLED", "1");

    send_telemetry("cognee.forget", "user", None);

    // Wait a generous window to ensure no late dispatch sneaks
    // through.
    tokio::time::sleep(Duration::from_millis(500)).await;
    mock.assert_async().await;
}

#[tokio::test]
#[serial]
async fn fire_and_forget_does_not_block_caller() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_chunked_body(|w| {
            // Stall by sleeping in the body writer. mockito 1.x
            // exposes `with_chunked_body` for streaming callbacks.
            std::thread::sleep(Duration::from_millis(2_000));
            w.write_all(b"{}")
        })
        .create_async()
        .await;
    let _env = IsolatedEnv::install(&server.url());

    let start = tokio::time::Instant::now();
    send_telemetry("cognee.forget", "user", None);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "send_telemetry blocked the caller for {elapsed:?}"
    );
    // We don't care whether the request eventually completes — it's
    // fire-and-forget. The mockito assertion is intentionally not
    // checked here.
    let _ = mock;
}
```

`cognee_telemetry::ids::__test_only_reset_caches()` is a small
public-but-hidden helper that wipes the `Lazy<Mutex<...>>` caches
for `ANON_ID` and `PERSISTENT_ID`. Add it to
`crates/telemetry/src/ids.rs`:

```rust
/// Wipe the cached anonymous and persistent IDs so the next call
/// re-reads from disk. Test-only: gated by `cfg(any(test, debug_assertions))`
/// to avoid leaking into release builds.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn __test_only_reset_caches() {
    if let Ok(mut g) = ANON_ID.lock() { *g = None; }
    if let Ok(mut g) = PERSISTENT_ID.lock() { *g = None; }
}
```

Gating it behind `debug_assertions` keeps it accessible from
integration tests (which are debug builds) without exposing it in
released `cargo build --release` artefacts.

### 4.2 Add `regex` and `mockito` to dev-deps

If `regex` is not already a dev-dep of `cognee-telemetry`, add to
[`crates/telemetry/Cargo.toml`](../../../crates/telemetry/Cargo.toml):

```toml
[dev-dependencies]
mockito = "1"
regex = "1"
serial_test = { workspace = true }
tempfile.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util"] }
```

`regex` is not in the current `[workspace.dependencies]` block; it
is small enough to declare locally. If a workspace dep exists by
task implementation time, prefer `regex.workspace = true`.

### 4.3 Verify

```bash
cargo test -p cognee-telemetry --features telemetry --tests
cargo clippy -p cognee-telemetry --features telemetry --tests -- -D warnings
```

The three integration tests must pass on the developer machine and
in CI. Network policy: `mockito` binds to `127.0.0.1:0` (a random
unused port). No outbound DNS or routing to `test.prometh.ai`.

### 4.4 Add a CI-side network-isolation check (optional, recommended)

In [task 02-12](12-ci-updates.md), the CI lane that runs this test
should also assert that no DNS lookup for `test.prometh.ai` occurs.
A simple way: wrap the cargo test in a `unshare -n` (Linux) and
verify it still passes. If `unshare` is unavailable on the CI
runner, skip — the in-test assertion that `mockito.expect(0)`
catches accidental egress is sufficient.

## 5. Verification

```bash
# 1. All three tests pass.
cargo test -p cognee-telemetry --features telemetry --test dispatch_with_mockito

# 2. No clippy warnings.
cargo clippy -p cognee-telemetry --features telemetry --tests -- -D warnings

# 3. Verify mockito binds to 127.0.0.1.
cargo test -p cognee-telemetry --features telemetry --test dispatch_with_mockito \
  -- --nocapture 2>&1 | grep '127.0.0.1'

# 4. Confirm no live-proxy egress (manual sanity check).
sudo tcpdump -i any host test.prometh.ai &
TCPDUMP=$!
cargo test -p cognee-telemetry --features telemetry --test dispatch_with_mockito
kill $TCPDUMP
# Expected: zero captured packets.
```

## 6. Files modified

- `crates/telemetry/tests/dispatch_with_mockito.rs` — new file.
- `crates/telemetry/Cargo.toml` — add `regex` to dev-dependencies if
  not already present.
- `crates/telemetry/src/ids.rs` — add `__test_only_reset_caches()`
  helper.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `mockito::Mock::last_request()` API drift between versions | mockito 1.x is stable; we pin to `mockito = "1"` | Pin to a specific minor (`mockito = "1.5"`) if drift surfaces. |
| Test thread sleeps inside `with_chunked_body` block the mockito server thread, breaking other parallel tests | Tests are `#[serial]`, so only one runs at a time | If the chunked-body callback is too restrictive, switch to `with_body` + `with_response_delay` (mockito 1.x API). |
| `127.0.0.1` is firewalled in some CI runners | None — GitHub Actions allows loopback | Document fallback (use `localhost` if `127.0.0.1` is rejected). |
| `wait_for_hit` polling races with mockito's internal queue | 25ms tick + 5s timeout is generous | Bump timeout to 10s if flakes appear. |
| Tests leak env vars if a panic skips `Drop` | Acceptable — `#[serial]` ensures the next test resets in `IsolatedEnv::install` | Document in the module preamble. |
| Live proxy reached accidentally because the override env var leaked from another test | Mitigated by `IsolatedEnv::install` resetting *both* override flags before each test | Sub-agent C verifies no test omits `IsolatedEnv`. |

## 8. Out of scope

- Cross-SDK byte parity in Docker (covered by [task 02-10](10-cross-sdk-parity.md)).
- Smoke-test against the live `test.prometh.ai` proxy (intentionally
  not automated; see [task 02-11](11-user-docs.md) for the manual
  recipe).
- Failure-path event emissions (`... Errored`) — see
  [task 02-07](07-callsite-migration.md) "Out of scope".
