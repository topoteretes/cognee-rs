# Task 06-07 — Refactor `crates/http-server/src/main.rs` to use `init_logging`

**Status**: implemented in commit 5d9eb3c
**Owner**: _unassigned_
**Depends on**: [Task 06-05 — init_logging](05-init-logging.md).
**Blocks**:
- [Task 06-10 — Tests](10-tests.md) (HTTP server E2E test depends on the new subscriber being live).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 6 (default filter), 13 (`SpanBufferLayer` stays independent — composed via `extra_layers`).

---

## 1. Goal

Replace the duplicated `init_tracing` functions at
[`crates/http-server/src/main.rs:108-164`](../../../crates/http-server/src/main.rs#L108-L164)
with a single delegate to
`cognee_logging::init_logging(LoggingConfig::from_env(), extra_layers)`.
Pass `SpanBufferLayer` (always) and `telemetry_layer` (only when
`--features telemetry`) via `extra_layers`. Keep the `Arc<SpanBuffer>`
wiring through `AppState` intact. Hold the returned `LogGuards` in
`main()`'s scope.

## 2. Rationale

- Decision 13: `SpanBufferLayer` is for the `/spans` HTTP endpoint
  and must not be mirrored to the file sink. Passing it via
  `extra_layers` keeps that independence explicit — the file sink
  only receives the stdout-and-file `fmt` layer plus the
  `EnvFilter`; the buffer is a sibling.
- Decision 6: the default filter is now broader. The server inherits
  it for free via `LoggingConfig::default_filter()`.
- Removing the two near-duplicate `init_tracing` functions is a
  drop in maintenance load.

## 3. Pre-conditions

- Task 06-05 committed; `cognee-logging` exports `init_logging`,
  `LoggingConfig`, `LogGuards`, `BoxedLayer`.
- `crates/http-server/src/main.rs` still uses the
  `Registry::default().with(env_filter).with(fmt_layer).with(span_buffer_layer)
  .try_init()` pattern in both cfg branches.

### Current state — `crates/http-server/src/main.rs` (lines 108–164)

Two `init_tracing` functions, one per `#[cfg]`. Both end with
`Registry::default().with(...).try_init()`. The telemetry-on variant
boxes the OTEL layer; the telemetry-off variant has no OTEL. Both
construct their own `EnvFilter` with `"info,ort=warn"` fallback.

## 4. Step-by-step

### 4.1 Update `crates/http-server/Cargo.toml`

Add `cognee-logging = { path = "../logging" }` under
`[dependencies]`. Alphabetical near other `cognee-*` deps.

### 4.2 Replace both `init_tracing` functions

In [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs),
delete the two `init_tracing` functions (lines ~108–164) and replace
the call sites (lines ~56–62) with inline logic in `main()`:

```rust
#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = dotenv::dotenv();

    let spans = Arc::new(SpanBuffer::new(BufferConfig::from_env()));

    let logging_cfg = cognee_logging::LoggingConfig::from_env()
        .unwrap_or_else(|err| {
            eprintln!("warning: invalid logging env var: {err}; falling back to defaults");
            cognee_logging::LoggingConfig::defaults()
        });

    let span_buffer_layer: cognee_logging::BoxedLayer =
        Box::new(SpanBufferLayer::new((*spans).clone()));

    #[cfg(not(feature = "telemetry"))]
    let _log_guards = cognee_logging::init_logging(
        logging_cfg,
        std::iter::once(span_buffer_layer),
    );

    #[cfg(feature = "telemetry")]
    let (_log_guards, telemetry_guard) = {
        let settings = cognee_observability::EnvSettingsView::from_env();
        let (telemetry_layer, telemetry_guard) =
            match cognee_observability::init_telemetry::<tracing_subscriber::Registry>(&settings) {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!(?err, "telemetry init failed; continuing without OTEL");
                    (
                        Box::new(tracing_subscriber::layer::Identity::new())
                            as cognee_observability::BoxedTelemetryLayer<tracing_subscriber::Registry>,
                        cognee_observability::TelemetryGuard::noop(),
                    )
                }
            };

        let extras: Vec<cognee_logging::BoxedLayer> =
            vec![telemetry_layer, span_buffer_layer];
        let guards = cognee_logging::init_logging(logging_cfg, extras);
        (guards, Some(Arc::new(telemetry_guard)))
    };

    let args = Args::parse();
    // ... rest of main unchanged ...
}
```

### 4.3 Type-alias compatibility

`cognee_observability::BoxedTelemetryLayer<Registry>` is currently
defined as `Box<dyn Layer<Registry> + Send + Sync>` (verify in
`crates/observability/src/init.rs`). It should be **identical** to
`cognee_logging::BoxedLayer`. Sub-agent B verifies the equivalence
and adds a `From`/coercion path if needed (most likely they just
match — `Box<dyn Layer<Registry> + Send + Sync>` is a single type).

If they differ in `'static` bounds or `Sized` requirements:
implementor adds a trivial wrapper in `crates/logging/src/init.rs`
or — preferred — adjusts `BoxedLayer` in 06-05 (re-open 06-05 sub-doc
via the orchestrator's "needs-update" path).

### 4.4 Verify `state.spans` and `state.telemetry_guard` wiring still works

The existing flow:

```rust
let mut state = AppState::build(cfg.clone()).await.context("...")?;
state.spans = spans;
#[cfg(feature = "telemetry")]
{ state.telemetry_guard = telemetry_guard; }
```

stays as-is. `telemetry_guard` becomes `Option<Arc<TelemetryGuard>>`
to match the new tuple shape (the implementor maps from the local
binding to the existing `state.telemetry_guard` type — confirm what
`state.telemetry_guard`'s declared type is in
`crates/http-server/src/lib.rs` before assigning).

### 4.5 Remove now-dead `init_tracing` function bodies

Delete lines ~108–164 of `crates/http-server/src/main.rs` entirely
(both `#[cfg(feature = "telemetry")] fn init_tracing(...)` and the
non-telemetry variant). The use-statements inside those functions go
away with them.

## 5. Verification

```bash
# 1. HTTP server compiles in both feature configurations.
cargo check -p cognee-http-server --all-targets
cargo check -p cognee-http-server --all-targets --no-default-features
cargo check -p cognee-http-server --all-targets --features telemetry

# 2. Existing HTTP server tests pass.
cargo test -p cognee-http-server

# 3. Smoke: boot the server, hit /healthz, verify both stdout log
#    line and *.log file under COGNEE_LOGS_DIR contain the request.
COGNEE_LOGS_DIR=$(mktemp -d) cargo run -p cognee-http-server -- --port 18001 &
SERVER_PID=$!
sleep 2
curl -s http://localhost:18001/healthz
kill $SERVER_PID
ls "$COGNEE_LOGS_DIR"/*.log
grep -l "healthz\|GET" "$COGNEE_LOGS_DIR"/*.log

# 4. Clippy.
cargo clippy -p cognee-http-server --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)
  — add `cognee-logging` dep.
- [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
  — inline logging init in `main()`, delete both `init_tracing`
  functions.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `cognee_observability::BoxedTelemetryLayer<Registry>` is not type-equal to `cognee_logging::BoxedLayer` | Medium | Verify the alias definition in `crates/observability/src/init.rs`. If different, adjust `BoxedLayer` in 06-05 to match exactly. |
| `SpanBufferLayer` requires a generic parameter that `Box<dyn Layer<Registry>>` cannot satisfy | Low | Today's code already boxes it implicitly via `.with(span_buffer_layer)` on the `Registry`. The same coercion works through `Vec<BoxedLayer>`. |
| Server tests that captured the literal `"info,ort=warn"` filter (e.g. asserting noisy log lines) now see different output | Low | No HTTP server test today asserts on log filter contents; if found, fix to use the new default. |
| OTEL init ordering — telemetry must initialize before the first `tracing::*` call to capture the request that triggered it | Maintained | The new order is identical to the old: parse env → init telemetry → init logging (which composes both layers atomically via `try_init`). |

## 8. Out of scope

- Mirroring `SpanBufferLayer` content into the file sink (decision
  13 forbids).
- Adding a `/logs` HTTP endpoint that streams the file. Out of
  scope for gap 06.
- Per-request `tower_http::trace` rework. The decision-6 default
  filter already sets `tower_http=warn`, so per-request logs are
  silenced; users opt back in via `RUST_LOG=tower_http=info`.
