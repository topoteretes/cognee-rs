# Task 10 — End-to-end OTLP export integration test against an in-process fake collector

**Status:** Not started
**Owner:** _unassigned_
**Depends on:**
- [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-implement-init-otel-and-guard.md) — provides the real OTEL bring-up that this test exercises.
- [Task 06 — Refactor CLI subscriber](./06-refactor-cli-subscriber.md) — locks the public composition shape (`init_telemetry(&OtelSettings)` returning a guard plus a layer) that the test mirrors. (If task 06 is renumbered, follow whichever sub-doc owns the call-site refactor — the test only depends on the public API shape.)
- [Task 09 — Unit tests in `cognee-observability`](./09-unit-tests-observability.md) — covers the small helpers (`parse_otlp_headers`, `already_instrumented`, noop fallback). This task is the orthogonal *integration* counterpart that proves spans actually leave the process.

**Soft pre-conditions (manifest-level):** [Task 01 — workspace OTEL deps](./01-workspace-otel-deps.md), [Task 02 — `cognee-observability` crate scaffold](./02-observability-crate-scaffold.md), [Task 03 — `cognee-lib` feature wiring](./03-cognee-lib-feature-wiring.md). The integration test lives inside `cognee-observability` itself, so only tasks 01/02/04 are strictly required at runtime; 03 is needed for the parent doc's downstream wiring but does not affect this test.

**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md), specifically the [Testing strategy → Integration test](../01-otel-otlp-export.md#integration-test) section, which prescribes:

> Spawn a tonic gRPC server implementing `opentelemetry-proto`'s `TraceService` on `127.0.0.1:0`. Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<port>`. Build a `Settings` with `cognee_tracing_enabled = true`. Call `init_otel(&settings)`, attach the bridge to a fresh `Registry`, install via `tracing::subscriber::with_default`. Inside, call a function decorated with `#[tracing::instrument(name = "test.span", fields(foo = "bar"))]`. Drop the guard; assert the server received exactly one batch with one span named `"test.span"`, attribute `foo == "bar"`, resource attribute `service.name == "cognee"`.

This sub-doc turns that paragraph into runnable code.

---

## 1. Goal

Add a single integration test, `crates/observability/tests/otel_export.rs`, that:

1. Stands up a fake OTLP/gRPC `TraceService` on `127.0.0.1:0` inside the test process.
2. Points cognee's OTEL bring-up at it via `OTEL_EXPORTER_OTLP_ENDPOINT`.
3. Calls `init_telemetry(&OtelSettings { tracing_enabled: true, exporter_otlp_endpoint: …, … })`.
4. Emits one `#[tracing::instrument]`-annotated function call inside a `tracing::subscriber::with_default` scope.
5. Drops the `TelemetryGuard` (forcing flush + shutdown).
6. Asserts the fake collector received at least one `ExportTraceServiceRequest` containing a span named `"test.span"`, attribute `foo == "bar"`, and resource attribute `service.name == "cognee"`.

The test is gated `#![cfg(feature = "telemetry")]` so the noop branch (no exporter, no spans on the wire) cannot accidentally pass it.

## 2. Rationale

### Why an in-process fake collector beats Docker / a real `otel-collector`

- **No external infrastructure.** The `lib-tests.yml` GitHub Actions workflow already runs without Docker for the workspace tests; adding a docker-compose dependency just for one assertion would force CI to either spin up an `otel-collector` sidecar (slow, flaky) or skip the test on CI (defeats the purpose). A tonic stub binds in <10 ms on `127.0.0.1`.
- **Deterministic assertions.** A real collector batches, transforms, and forwards. We want to assert the **exact protobuf** that left our process. Implementing `TraceService::export(..)` ourselves gives us the unmodified `ExportTraceServiceRequest` to inspect.
- **Hermetic.** No port-already-bound flakes (we use `127.0.0.1:0` and read back the port), no DNS, no shared state between test runs.
- **Small dep delta.** The crate already takes `opentelemetry-otlp` (with `grpc-tonic`) when `telemetry` is on, which transitively brings in `tonic`. The only test-only addition is `opentelemetry-proto` with the `gen-tonic` feature (server bindings) plus the `serial_test` and `tokio` macros we already use elsewhere.

### Why depend on `opentelemetry-proto` rather than vendoring `.proto` files

The OTEL `opentelemetry-proto` crate publishes pre-generated tonic server + client bindings behind the `gen-tonic` cargo feature. Going through `tonic_build` ourselves would mean (a) committing the `.proto` files into the repo, (b) running protoc at build time (extra system dep), and (c) re-deriving types we'd have to keep in sync with the upstream OTEL spec on every minor bump. The crate exists precisely to avoid this. We pin it to the same `=0.31` minor track as `opentelemetry-otlp` so the ABI on the wire matches what the exporter sends.

### Why gRPC only in this task — HTTP/protobuf deferred

[Decision 3](../01-otel-otlp-export.md#design-decisions-locked) ships both transports, with **gRPC as the default**. Testing the default path first is the highest-leverage assertion: every user who does not set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` exercises this path. An HTTP/protobuf counterpart test (axum or hyper handler decoding the protobuf body) is a natural follow-up — recorded under [Risks → Follow-ups](#7-risks-and-follow-ups). Splitting these keeps the PR small enough to review in one sitting.

### Why `#![cfg(feature = "telemetry")]`

When the `telemetry` feature is off, `init_telemetry` returns a noop guard ([task 08](./08-noop-fallback-and-tests.md)), no exporter is built, and no bytes ever leave the process. Compiling and running this test under `--no-default-features` would therefore vacuously pass (collector receives nothing, assertion fails) or vacuously skip (depending on how we coded it). Gating the *whole file* via `#![cfg(feature = "telemetry")]` makes the intent explicit: this test exists to validate the real export path and is meaningless without it.

## 3. Pre-conditions

- Tasks 01, 02, 04 are merged. The crate exists, the manifest pulls OTEL deps under the `telemetry` feature, and `init_telemetry` actually builds an `SdkTracerProvider` with an OTLP exporter.
- `cargo check -p cognee-observability --features telemetry` succeeds on `main`.
- The workspace's [`[patch.crates-io]` block](../../../Cargo.toml) keeps overriding `tonic` to the qdrant fork; confirm in advance (during task 04) that `opentelemetry-otlp = "=0.31"` with `grpc-tonic` resolves successfully against that patched tonic. If task 04 had to relax the patch or split exporters into a sub-crate, this test inherits that decision unchanged — it just uses whichever `tonic` ends up in the lockfile.

## 4. Step-by-step

### 4.1 Add dev-dependencies to `crates/observability/Cargo.toml`

Append the following under the existing `[dev-dependencies]` table introduced in [task 02 §4.2](./02-observability-crate-scaffold.md#42-create-cratesobservabilitycargotoml):

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync", "net"] }
opentelemetry-proto = { version = "=0.31", default-features = false, features = ["gen-tonic", "trace"] }
tonic = { workspace = true }
serial_test = { workspace = true }
```

Notes:

- `tokio` already exists in `[dev-dependencies]` from task 02 with `["macros", "rt-multi-thread"]`; this patch widens the feature set to add `time` (for `sleep`/`timeout`), `sync` (for `oneshot` shutdown channel), and `net` (so we can call `TcpListener::bind` with the tokio runtime). Cargo unifies feature sets, so the final dev build has the union.
- `tonic` is **not** currently a `[workspace.dependencies]` entry. Two options:
  1. Reference it through `opentelemetry-proto`'s re-export (it re-exports `tonic` via `opentelemetry_proto::tonic::collector::trace::v1::trace_service_server`). Then we don't need a direct `tonic` dev-dep at all. **Preferred** — fewer manifest changes.
  2. Add `tonic = "..."` to `[workspace.dependencies]` and reference here as `tonic = { workspace = true }`. This requires picking a version compatible with the patched fork the workspace already uses (`qdrant/tonic v0.11.0-qdrant`). The patch substitutes regardless of the requested version, so a lenient `tonic = "0.12"` should work; verify with `cargo tree -p cognee-observability --features telemetry -i tonic`.

  This sub-doc uses **option (1)**: import `tonic` types via the `opentelemetry_proto::tonic` re-export. The dev-deps block therefore omits the explicit `tonic` line. If task 04 finds it needs `tonic` at the workspace level for a different reason, switch to option (2) and update §5.1 below accordingly.
- `opentelemetry-proto` is gated to `dev-dependencies` only — never a runtime dep. The `gen-tonic` feature compiles the tonic server/client bindings; without it only the prost types are exposed and we can't `impl TraceService for …`. The `trace` feature scopes the generated code to just the trace pillar (we don't need metrics or logs here).
- `serial_test` is added because the OTEL global `TracerProvider` is process-wide (set via `opentelemetry::global::set_tracer_provider`). Two parallel telemetry tests would race on that global. We mark this test with `#[serial_test::serial]` to be safe even if future tests are added.

Final dev-deps shape after this change:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync", "net"] }
opentelemetry-proto = { version = "=0.31", default-features = false, features = ["gen-tonic", "trace"] }
serial_test = { workspace = true }
```

### 4.2 Create `crates/observability/tests/otel_export.rs`

Full test file — see [§5 Resulting code](#5-resulting-code) for the verbatim contents. Key structural choices:

1. **`#![cfg(feature = "telemetry")]` at the top** — the whole file is excluded under default features.
2. **`MockTraceService`** — a struct holding `Arc<Mutex<Vec<ExportTraceServiceRequest>>>` plus an `Arc<Notify>` so the test can `await` arrival of the first request rather than `sleep`-and-hope.
3. **gRPC server lifecycle** — bind to `127.0.0.1:0`, read back the port via `TcpListener::local_addr`, hand the listener to `tonic::transport::Server::serve_with_incoming_shutdown`, drive shutdown via a `tokio::sync::oneshot` channel.
4. **Exporter wiring** — set `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<port>` *before* calling `init_telemetry`. The OTEL SDK reads it directly. `OtelSettings.exporter_otlp_endpoint` is also populated for symmetry — task 04 decides which wins; the env var is the canonical OTEL knob.
5. **Subscriber composition** — fresh `Registry` + `tracing_opentelemetry::layer().with_tracer(tracer)`. We do **not** install globally with `subscriber.try_init()` because that would persist between test runs in the same process. Use `tracing::subscriber::with_default(subscriber, || { … })` instead.
6. **Span emission** — call a function annotated `#[tracing::instrument(name = "test.span", fields(foo = "bar"))]`. Inside, do nothing (no I/O). The instrumentation alone produces the span on entry/exit.
7. **Flush** — drop the `TelemetryGuard` *inside* the `with_default` scope so the bridge layer is still installed when the SDK calls back to record exit. The `Drop` impl (task 04) calls `force_flush()` then `shutdown()`. The batch processor must finish writing to the gRPC channel before `shutdown()` returns.
8. **Wait** — `tokio::time::timeout(Duration::from_secs(5), notify.notified())` to await the first request. 5 s is generous for a localhost gRPC call; on a healthy CI box it returns in <100 ms.
9. **Assertions** — lock the shared `Vec`, expect ≥1 entry, walk the proto structure to find the expected span name, span attribute `foo`, and resource attribute `service.name`.

### 4.3 Run the test

From the workspace root:

```bash
cargo test -p cognee-observability --features telemetry --test otel_export -- --nocapture --test-threads=1
```

`--test-threads=1` is belt-and-braces alongside `#[serial_test::serial]` — multiple tests in this file (added later) would still be serialised globally because `OTEL_EXPORTER_OTLP_ENDPOINT` and the OTEL global provider are both process-wide.

`--nocapture` is helpful while developing so panic messages from the assertion include the full pretty-printed `ExportTraceServiceRequest` for diagnosis.

For the project gate that `[CLAUDE.md](../../../.claude/CLAUDE.md#build--development)` mandates after any change:

```bash
scripts/check_all.sh
```

This will exercise `cargo fmt --check`, `cargo check --all-targets`, `cargo clippy -- -D warnings`, and the wrapper-binding checks. Note that `check_all.sh` does not currently pass `--features telemetry`, so it will not run the new test by default; that is fine — `lib-tests.yml` (or a new lane added in [parent doc action 12](../01-otel-otlp-export.md#action-items)) is responsible for executing it under the right feature flags.

## 5. Resulting code

### 5.1 `crates/observability/Cargo.toml` — `[dev-dependencies]` after the patch

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync", "net"] }
opentelemetry-proto = { version = "=0.31", default-features = false, features = ["gen-tonic", "trace"] }
serial_test = { workspace = true }
```

(The rest of the manifest is unchanged from [task 02 §5.1](./02-observability-crate-scaffold.md#51-cratesobservabilitycargotoml).)

### 5.2 `crates/observability/tests/otel_export.rs` — full text

```rust
//! End-to-end integration test: spans emitted through cognee's OTEL bring-up
//! must reach an OTLP/gRPC collector. We stand up an in-process tonic server
//! implementing `opentelemetry_proto::collector::trace::v1::TraceService`,
//! point `OTEL_EXPORTER_OTLP_ENDPOINT` at it, run a small instrumented
//! function, drop the guard, and assert the collector received the span we
//! expected.
//!
//! Gated on the `telemetry` feature — without it `init_telemetry` is a
//! noop and there is nothing to test.
//!
//! Run:
//! ```bash
//! cargo test -p cognee-observability --features telemetry \
//!     --test otel_export -- --nocapture --test-threads=1
//! ```

#![cfg(feature = "telemetry")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cognee_observability::{init_telemetry, OtelSettings};

use opentelemetry_proto::tonic::collector::trace::v1::{
    trace_service_server::{TraceService, TraceServiceServer},
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex, Notify};
use tonic::{transport::Server, Request, Response, Status};
use tracing_subscriber::{layer::SubscriberExt, Registry};

/// Captures every `ExportTraceServiceRequest` that arrives over the wire.
#[derive(Default, Clone)]
struct CapturedExports {
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    arrived: Arc<Notify>,
}

/// Mock implementation of the OTLP TraceService.
struct MockTraceService {
    captured: CapturedExports,
}

#[tonic::async_trait]
impl TraceService for MockTraceService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let req = request.into_inner();
        self.captured.requests.lock().await.push(req);
        self.captured.arrived.notify_waiters();
        // OTLP allows an empty success response.
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

/// Bind a tonic server on 127.0.0.1:0, returning the captured-exports handle,
/// the bound socket address, and a shutdown trigger.
async fn spawn_mock_collector(
) -> (CapturedExports, SocketAddr, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0 — port 0 must always be available on loopback");
    let addr = listener
        .local_addr()
        .expect("the listener was just bound, so local_addr must exist");

    let captured = CapturedExports::default();
    let svc = MockTraceService {
        captured: captured.clone(),
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let handle = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(TraceServiceServer::new(svc))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    (captured, addr, shutdown_tx, handle)
}

#[tracing::instrument(name = "test.span", fields(foo = "bar"))]
fn emit_span() {
    // Body intentionally empty: the `#[instrument]` macro alone produces a
    // span on entry/exit. We don't want any extra noise spans inside.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn spans_flow_to_otlp_collector() {
    let (captured, addr, shutdown_tx, server_task) = spawn_mock_collector().await;

    // Point cognee at our fake collector. The OTEL SDK reads
    // OTEL_EXPORTER_OTLP_ENDPOINT directly; OtelSettings is populated for
    // symmetry / explicitness.
    let endpoint = format!("http://{addr}");
    // SAFETY: tests in this file are serialised by `#[serial_test::serial]`
    // and `--test-threads=1`, so this env-var write does not race with other
    // telemetry tests.
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint);

    let settings = OtelSettings {
        tracing_enabled: true,
        service_name: "cognee".to_string(),
        exporter_otlp_endpoint: endpoint.clone(),
        exporter_otlp_headers: String::new(),
    };

    // Real OTEL bring-up. Returns a guard that flushes + shuts down on drop,
    // and (per task 04) also exposes a tracing_subscriber::Layer that bridges
    // tracing → OTEL. The exact accessor name is decided in task 04; here we
    // assume `guard.tracer_for_test()` returns the `opentelemetry::trace::Tracer`
    // we need to attach the bridge to a fresh Registry. If task 04 names it
    // differently, update this call site.
    let guard = init_telemetry(&settings).expect("init_telemetry must succeed when telemetry feature is on and endpoint is reachable");

    let tracer = guard.tracer_for_test();
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = Registry::default().with(otel_layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_span();
    });

    // Force the batch processor to flush before we assert. Dropping the guard
    // calls force_flush() + shutdown() per task 04.
    drop(guard);

    // Wait up to 5s for the first ExportTraceServiceRequest to arrive.
    tokio::time::timeout(Duration::from_secs(5), captured.arrived.notified())
        .await
        .expect("collector did not receive any spans within 5s — flush/shutdown likely failed");

    // Tear the server down cleanly so the runtime doesn't hold the port.
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;

    // --- Assertions on the captured protobuf -------------------------------

    let exports = captured.requests.lock().await;
    assert!(
        !exports.is_empty(),
        "collector received zero ExportTraceServiceRequests"
    );

    let mut found_span = false;
    let mut found_service_name = false;

    for export in exports.iter() {
        for resource_spans in &export.resource_spans {
            // Resource attributes -> service.name
            if let Some(resource) = &resource_spans.resource {
                for kv in &resource.attributes {
                    if kv.key == "service.name" {
                        if let Some(any_value) = &kv.value {
                            if let Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s)) =
                                &any_value.value
                            {
                                assert_eq!(
                                    s, "cognee",
                                    "service.name resource attribute must equal 'cognee', got '{s}'"
                                );
                                found_service_name = true;
                            }
                        }
                    }
                }
            }

            for scope_spans in &resource_spans.scope_spans {
                for span in &scope_spans.spans {
                    if span.name == "test.span" {
                        found_span = true;
                        // Span attribute foo == "bar"
                        let foo = span.attributes.iter().find(|kv| kv.key == "foo");
                        let foo_kv = foo.unwrap_or_else(|| {
                            panic!(
                                "span 'test.span' has no 'foo' attribute; attributes were: {:?}",
                                span.attributes
                            )
                        });
                        let foo_value = foo_kv
                            .value
                            .as_ref()
                            .and_then(|v| v.value.as_ref())
                            .expect("foo attribute has no value");
                        match foo_value {
                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => {
                                assert_eq!(s, "bar", "span attribute 'foo' must equal 'bar'");
                            }
                            other => panic!("span attribute 'foo' must be a string, got {other:?}"),
                        }
                    }
                }
            }
        }
    }

    assert!(
        found_span,
        "no span named 'test.span' found in captured exports: {exports:#?}"
    );
    assert!(
        found_service_name,
        "no resource attribute 'service.name' found in captured exports: {exports:#?}"
    );
}
```

#### Notes on the test file

- The `tokio_stream` import (`tokio_stream::wrappers::TcpListenerStream`) requires `tokio-stream` in `[dev-dependencies]`. It is already in `[workspace.dependencies]` (with `features = ["sync"]`). Add `tokio-stream = { workspace = true, features = ["net"] }` to `crates/observability/Cargo.toml`'s `[dev-dependencies]` if `cargo check` complains about the missing `net` feature; the wrapper for `TcpListener` lives behind it. (If task 04 already added `tokio-stream` to runtime deps, the dev-dep is unnecessary.)
- The `guard.tracer_for_test()` accessor referenced above is **not** defined in this task — it lives in task 04's `TelemetryGuard` impl. If task 04 prefers a different surface (e.g. `opentelemetry::global::tracer("cognee")` after `init_telemetry` has installed the provider globally), replace the `let tracer = guard.tracer_for_test();` line with `let tracer = opentelemetry::global::tracer("cognee");`. The rest of the test is unaffected. This sub-doc explicitly leaves the choice to task 04 and documents both forms.
- The OTLP exporter inside `init_telemetry` will, by default, use the **batch** span processor (locked by [decision 4](../01-otel-otlp-export.md#design-decisions-locked)). The batch processor flushes asynchronously; `force_flush()` synchronously waits for the in-flight batch to drain (with a configurable timeout). The 5 s `tokio::time::timeout` is the upper bound on `force_flush + network round-trip` for localhost gRPC; if the test ever flakes, the SDK's flush timeout is the first thing to inspect.

## 6. Verification

Once all three of the assertions pass:

- [ ] `cargo test -p cognee-observability --features telemetry --test otel_export -- --nocapture --test-threads=1` exits 0 in <10 s on a typical dev machine.
- [ ] Running with `--features telemetry` removed: `cargo test -p cognee-observability --test otel_export` reports `running 0 tests` (the `#![cfg(feature = "telemetry")]` excluded the entire file, as designed).
- [ ] Forcing a regression — temporarily comment out the `force_flush` call inside `TelemetryGuard::drop` (task 04) — must cause this test to fail with the message `collector did not receive any spans within 5s — flush/shutdown likely failed`. This verifies the test actually depends on the flush path and isn't a tautology.
- [ ] `scripts/check_all.sh` passes (fmt, clippy, capi/python/js binding checks).

## 7. Risks and follow-ups

1. **Async race between `force_flush` and the batch processor.** The OTEL `BatchSpanProcessor` runs on the tokio runtime; `force_flush()` blocks (via a channel) until the in-flight batch drains. If task 04 configures a flush timeout shorter than the gRPC round-trip on localhost, `force_flush` returns early and the `notified()` await times out. Mitigation: task 04's flush timeout default should be ≥ 1 s (Python defaults to 30 s; we can match). The 5 s outer `tokio::time::timeout` is a safety net.
2. **tonic server lifecycle.** The server is spawned inside the test runtime and must be torn down or it leaks across tests. We use `oneshot` for clean shutdown and `await` the join handle with a 2 s timeout. If the join hangs (e.g. an exporter task is still mid-RPC), the timeout drops the future and the runtime aborts on test exit.
3. **Port flakiness.** Binding to `127.0.0.1:0` and reading `local_addr()` is the standard way to avoid hard-coded port collisions. Do **not** parameterise the test with an env var port — that would re-introduce flakes on parallel CI shards.
4. **`tracing::subscriber::with_default` thread-locality.** `with_default` installs the subscriber on the **current** thread only. The `#[instrument]`-decorated function is called synchronously inside the closure, so the subscriber sees its enter/exit. If task 04 ever switches to per-task subscribers via `WithSubscriber` (e.g. `Future::with_subscriber`), update the test to match. We use `flavor = "multi_thread"` for the tokio runtime so the gRPC server can run on a worker while the closure executes on the main task — but the instrumented function itself is sync and stays on the calling thread.
5. **Process-wide OTEL global state.** `opentelemetry::global::set_tracer_provider` installs a process-wide singleton. Two parallel tests writing to it would clobber each other. We mitigate with `#[serial_test::serial]` (within the binary) and `--test-threads=1` (across binaries within `cargo test -p cognee-observability`, where cargo by default runs each integration test file as a separate binary). Future tests in this file inherit the same `#[serial]` attribute.
6. **Env var leakage.** `std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", …)` mutates the process environment. Other tests in the same process would see it. Because integration tests in `tests/*.rs` run in **separate binaries**, this leak is bounded to one test invocation. We do not `remove_var` afterwards because the next test in the same binary (none today) would set its own value anyway.
7. **`tonic` patch compatibility.** The workspace [`[patch.crates-io]`](../../../Cargo.toml) substitutes `tonic` with the qdrant fork at `v0.11.0-qdrant`. `opentelemetry-otlp = "=0.31"` with `grpc-tonic` requests a newer tonic; the patch will override regardless. If the fork's API has diverged enough to break `opentelemetry-otlp`'s codegen, task 04 will surface that — this test does **not** introduce the constraint, it just consumes whatever task 04 delivers.
8. **HTTP/protobuf path NOT exercised here.** Follow-up: add a sibling test `tests/otel_export_http.rs` that spawns an axum/hyper handler decoding the prost bytes from the request body. It can reuse `MockTraceService`'s capture struct verbatim and only differ in the transport adapter and the env var (`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`). Tracking note: when this gap doc's [Action items](../01-otel-otlp-export.md#action-items) are updated post-merge, file the HTTP test as item 13 or as a continuation under item 10.
9. **OTEL semconv naming drift.** We assert `service.name` as a literal string. The `opentelemetry-semantic-conventions` crate provides a constant for this (`SERVICE_NAME`); the assertion stays a string because the *wire* representation of the attribute is always the spec key, regardless of which constant the producer used. If the SDK ever renames the wire key (semver break), this test catches it.
10. **CI feature matrix.** [Action item 12](../01-otel-otlp-export.md#action-items) in the parent doc plans a `cargo check -p cognee-lib --no-default-features` lane. To exercise this test, CI also needs a `cargo test -p cognee-observability --features telemetry --test otel_export` lane. File a follow-up to wire that into `lib-tests.yml` after this task lands.

## 8. Files modified / created

| File | Change |
|---|---|
| [`crates/observability/Cargo.toml`](../../../crates/observability/Cargo.toml) | **Modify.** Add `opentelemetry-proto`, `serial_test`, and the additional `tokio` features (`time`, `sync`, `net`) under `[dev-dependencies]`. Optionally add `tokio-stream = { workspace = true, features = ["net"] }` if `TcpListenerStream` is not already reachable. |
| `crates/observability/tests/otel_export.rs` | **New.** The integration test in §5.2 above. |

No production source files are modified by this task; no other crate's manifest is touched.

## 9. References

- Parent gap doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md).
  - [Testing strategy → Integration test](../01-otel-otlp-export.md#integration-test) — paragraph this task realises.
  - [Design decisions](../01-otel-otlp-export.md#design-decisions-locked) — decisions 3 (gRPC default), 4 (batch processor default), 10 (`TelemetryGuard` name).
  - [Action items](../01-otel-otlp-export.md#action-items) item 10.
- Sibling sub-docs:
  - [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md) — owns the `Cargo.toml` we extend in §4.1.
  - [`04-implement-init-otel-and-guard.md`](./04-implement-init-otel-and-guard.md) — owns `init_telemetry`, `TelemetryGuard`, and the `tracer_for_test()` (or equivalent) accessor.
  - [`08-noop-fallback-and-tests.md`](./08-noop-fallback-and-tests.md) — explains why this test is `#![cfg(feature = "telemetry")]`-gated.
  - [`09-unit-tests-observability.md`](./09-unit-tests-observability.md) — the orthogonal unit-test sub-doc (parsers, helpers); this is the integration counterpart.
- External documentation:
  - [`opentelemetry-proto` crate](https://docs.rs/opentelemetry-proto/0.31.0/opentelemetry_proto/) — `tonic::collector::trace::v1::trace_service_server::{TraceService, TraceServiceServer}`, `ExportTraceServiceRequest`, `ExportTraceServiceResponse`, `tonic::common::v1::any_value::Value`.
  - [tonic guide — implementing a server](https://github.com/hyperium/tonic) — `Server::builder().add_service(...).serve_with_incoming_shutdown(...)`.
  - [tokio `TcpListener::local_addr`](https://docs.rs/tokio/latest/tokio/net/struct.TcpListener.html#method.local_addr) and [`tokio_stream::wrappers::TcpListenerStream`](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.TcpListenerStream.html).
  - [OTLP / gRPC export specification](https://opentelemetry.io/docs/specs/otlp/#otlpgrpc) — wire format we are asserting on.
  - [`tracing::subscriber::with_default`](https://docs.rs/tracing/latest/tracing/subscriber/fn.with_default.html) — thread-local installation semantics.
