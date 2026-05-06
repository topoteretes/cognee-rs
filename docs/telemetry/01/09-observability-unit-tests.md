# Task 09 — Unit tests for `cognee-observability`

**Status:** Not started
**Owner:** _unassigned_
**Depends on:** [Task 02 — Scaffold the `cognee-observability` workspace crate](./02-observability-crate-scaffold.md), [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-implement-init-otel-and-guard.md), [Task 08 — Noop fallback (and `Settings` field overlay)](./08-noop-fallback-and-tests.md)
**Blocks:** Nothing (terminal task within the test pyramid; integration tests against a fake OTLP collector are tracked separately as action item 10 in the parent doc).
**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md)

---

## 1. Goal

Add the focused unit-test suite for the new `cognee-observability` crate plus the small overlay-test additions in `cognee-lib::config`. Together they cover:

- `parse_otlp_headers` happy paths and one error / skip case.
- `is_tracing_enabled` Python-parity logic (locked decision 2: settings flag **OR** non-empty endpoint).
- `init_telemetry` returning a noop guard when both inputs are empty.
- `init_telemetry` building and globally installing a real provider when an endpoint is set.
- `already_instrumented()` on a fresh process (returns `false`) and after a provider has been installed (returns `true`).
- `Settings` field defaults and env-var overlay for the three new keys introduced by decisions 4 and 5: `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_SPAN_PROCESSOR`, `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG`.
- `TelemetryGuard::drop` invokes `force_flush` + `shutdown` on the installed provider.

The integration test against a real (or stub) OTLP collector is **not** in this task — it lives in action item 10 of the parent doc and gets its own per-task sub-doc when it is ready to be split out.

## 2. Rationale

### What each test guards against

| Test | Regression it catches |
|---|---|
| `parse_otlp_headers_*` | Off-by-one parsing, eager trim/strip mistakes, panics on `"key-only"` (no `=`), drift from Python's `OTEL_EXPORTER_OTLP_HEADERS` semantics. |
| `init_telemetry_disabled_returns_noop` | Accidental side effects (e.g. `set_tracer_provider` called on the empty path) or accidental dependency on environment when neither input is set. |
| `init_telemetry_endpoint_only_activates` | Decision 2 regressing — a future refactor that requires `cognee_tracing_enabled = true` to activate would silently drop spans for users who only set the endpoint env var, matching neither Python parity nor the locked design decision. |
| `init_telemetry_flag_only_no_endpoint` | Pins behaviour for the explicit-but-endpoint-less case so reviewers see what we expect (build the provider with no OTLP exporter — same shape as Python's `setup_tracing()` with no endpoint) and so any panic introduced by the OTEL crate becomes a flagged failure rather than a silent runtime crash. |
| `is_tracing_enabled_python_parity` | Truth-table regression for the `{flag} ⊕ {endpoint}` matrix — a single boolean swap gets caught immediately. |
| `already_instrumented_*` | Auto-instrumentation detection breaks if `opentelemetry::global::tracer_provider()` returns a non-noop default in a future SDK version, or if our type-sniffing logic stops matching `NoopTracerProvider`. |
| `settings_env_overlay_*` | The new fields must be picked up by `Settings::overlay_from_env`; without explicit tests it is easy to merge the field declaration without the matching overlay branch. |
| `telemetry_guard_drop_calls_shutdown` | Pin RAII semantics — the most common cause of "spans missing from the collector for short-lived processes" is a missing flush on drop. |

### Why serial execution matters

Both `init_telemetry` (when activated) and `already_instrumented_after_set_true` mutate **process-global** OTEL state via `opentelemetry::global::set_tracer_provider`. The OTEL Rust SDK's global provider is install-once-and-cached: after one test installs a provider, subsequent parallel tests see it. We use `#[serial_test::serial]` (already a workspace dev-dependency, see [`Cargo.toml:78`](../../../Cargo.toml)) on every test that touches global state. The same applies to env-var tests — `cargo test` shares a single process, and `std::env::set_var` is process-global.

### Why we test settings-overlay logic in `cognee-lib`, not here

Per [task 02 §4.3](./02-observability-crate-scaffold.md), the `TelemetrySettings` input struct lives in `cognee-observability`, but the **env-var overlay** is implemented inside `cognee-lib::config::Settings::overlay_from_env` (which task 08 extends with the new fields). Tests for the overlay therefore belong in [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs)'s existing `#[cfg(test)] mod tests`, not in the observability crate. This keeps each test next to the code it exercises and matches the convention of the existing overlay tests (see [`crates/lib/src/config.rs:1083-1207`](../../../crates/lib/src/config.rs#L1083)).

## 3. Pre-conditions

- [Task 02](./02-observability-crate-scaffold.md): the `cognee-observability` crate exists with `TelemetrySettings`, `TelemetryGuard`, `TelemetryInitError`, and the `init_telemetry` (now also called `init_telemetry`) entry point.
- [Task 04](./04-implement-init-otel-and-guard.md): the `real::init` body builds a real `SdkTracerProvider`, installs it globally, returns the guard whose `Drop` calls `force_flush` + `shutdown`. `parse_otlp_headers`, `is_tracing_enabled`, `already_instrumented` are public functions or `pub(crate)` with `#[cfg(test)]` accessors.
- [Task 08](./08-noop-fallback-and-tests.md): `Settings` has the new fields `otel_exporter_otlp_protocol`, `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`, with defaults `"grpc"`, `"batch"`, `""`, `""` respectively, and the corresponding env-var overlay branches in `Settings::overlay_from_env`.

If any of those land later than expected, mark the dependent tests `#[ignore]` with a TODO referencing the task that gates them — do **not** weaken the assertions to make them pass.

## 4. Step-by-step

### 4.1 Add dev-dependencies to `crates/observability/Cargo.toml`

The crate already has `tokio` as a dev-dep from task 02. Add `serial_test` (workspace dep) and `temp-env` for safe env-var save/restore inside helpers — this is preferable to the unsafe `std::env::set_var` pattern in `crates/lib`, but only if a workspace-level `temp-env` is acceptable; if not, fall back to the `unsafe { std::env::set_var(...) }` + `serial_test::serial` pattern used in [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs#L1083).

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
serial_test = { workspace = true }
# Optional — only needed if we want scoped env-var helpers. If task 02
# does not already pull `temp-env` into the workspace, skip this line and
# use the existing `unsafe std::env::set_var` + `serial_test::serial`
# pattern from cognee-lib.
temp-env = "0.3"
```

This task does **not** add `temp-env` to the workspace `[workspace.dependencies]` — keeping the change small. If reviewers prefer workspace-wide centralisation, lift it then; the test bodies in §5 use `std::env::set_var` directly so they compile either way.

### 4.2 Add `#[cfg(test)] mod tests` to `crates/observability/src/headers.rs`

Co-located unit tests for `parse_otlp_headers` (task 04 will create the `headers.rs` module containing the function). Tests are pure — no env, no global state — so no `serial_test` annotation is needed.

### 4.3 Add an integration-style test file `crates/observability/tests/init.rs`

Tests that exercise the full `init_telemetry` path live here rather than co-located, because:

- They install a real `SdkTracerProvider` via `opentelemetry::global::set_tracer_provider`. That call is process-global and install-once, so we want each invocation in its own binary if at all possible. Putting the tests in `tests/init.rs` (a separate test binary) provides at least one extra process boundary versus inline `#[cfg(test)] mod tests`. (Cargo still runs all tests in `tests/init.rs` in the same binary, so we still need `#[serial_test::serial]` between them — but it is one less binary sharing the global with the headers tests.)
- The OTEL SDK uses Tokio, and `tokio::test` is easier to spell at the file level.

### 4.4 Add overlay tests to `crates/lib/src/config.rs`'s existing `mod tests`

Match the existing pattern (see [`config.rs:1083-1207`](../../../crates/lib/src/config.rs#L1083)): `#[serial_test::serial]`, `unsafe { std::env::set_var(...) }`, `Settings::default().overlay_from_env()`, then `unsafe { std::env::remove_var(...) }`. Add three tests, one per new env var (the fourth, `OTEL_TRACES_SAMPLER_ARG`, is covered in the same test as `OTEL_TRACES_SAMPLER` to keep the file size in check).

## 5. Resulting code

### 5.1 `crates/observability/src/headers.rs` — `#[cfg(test)] mod tests`

The function under test is added by task 04 with this signature (verbatim from [`01/04-implement-init-otel-and-guard.md`](./04-implement-init-otel-and-guard.md), reproduced here only so the tests are self-explanatory):

```rust
/// Parse the comma-separated `key=value` form used by
/// `OTEL_EXPORTER_OTLP_HEADERS`. Returns the pairs in order; entries
/// without an `=` are skipped (lenient — matches the Python exporter's
/// behaviour of ignoring malformed pairs rather than panicking).
pub(crate) fn parse_otlp_headers(input: &str) -> Vec<(String, String)> { /* ... */ }
```

Append to the bottom of `crates/observability/src/headers.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::parse_otlp_headers;

    #[test]
    fn parse_otlp_headers_empty() {
        assert!(parse_otlp_headers("").is_empty());
        assert!(parse_otlp_headers("   ").is_empty());
    }

    #[test]
    fn parse_otlp_headers_single() {
        assert_eq!(
            parse_otlp_headers("k=v"),
            vec![("k".to_string(), "v".to_string())],
        );
    }

    #[test]
    fn parse_otlp_headers_multi() {
        assert_eq!(
            parse_otlp_headers("a=1,b=2"),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
            ],
        );
    }

    #[test]
    fn parse_otlp_headers_whitespace() {
        // Surrounding spaces around both keys and values are trimmed; the
        // delimiter `=` is preserved literally inside the value when only
        // one `=` appears (e.g. base64-encoded auth tokens).
        assert_eq!(
            parse_otlp_headers("  authorization = Bearer abc  ,  x-trace =  on "),
            vec![
                ("authorization".to_string(), "Bearer abc".to_string()),
                ("x-trace".to_string(), "on".to_string()),
            ],
        );
    }

    #[test]
    fn parse_otlp_headers_invalid_pair_is_skipped() {
        // Decision: skip malformed pairs rather than erroring. Mirrors
        // the Python exporter, which logs and ignores malformed entries.
        // If a future task tightens this to return Result<_, _>, this
        // test must be updated alongside the signature change.
        let parsed = parse_otlp_headers("good=1,bad,also-good=2");
        assert_eq!(
            parsed,
            vec![
                ("good".to_string(), "1".to_string()),
                ("also-good".to_string(), "2".to_string()),
            ],
        );
    }
}
```

### 5.2 `crates/observability/tests/init.rs` (new file)

```rust
//! Integration-style unit tests for the `init_telemetry` entry point and
//! the `TelemetryGuard` lifecycle.
//!
//! These tests mutate process-global OTEL state
//! (`opentelemetry::global::set_tracer_provider`) and the process
//! environment, so every test in this binary is annotated with
//! `#[serial_test::serial]`. Cargo runs all tests in a `tests/` file in
//! the same binary; serialising them is therefore mandatory.
//!
//! Once the OTEL global is set, it cannot be reliably reset within the
//! same process — the SDK uses an install-once `OnceCell` semantics for
//! the global provider. We mitigate this by:
//!   1. running serially, so order-dependence is observable;
//!   2. ordering tests so `already_instrumented_default_false` runs
//!      before any test that installs a provider (test names are sorted
//!      alphabetically by `cargo test`, which puts `default_false`
//!      before `after_set_true` and before `init_telemetry_*`);
//!   3. accepting that re-running the test binary (each `cargo test`
//!      invocation) is the only true reset.

use cognee_observability::{
    init_telemetry, is_tracing_enabled, already_instrumented, TelemetrySettings, TelemetryGuard,
};

/// Build an `TelemetrySettings` with the given flag and endpoint and all
/// other fields defaulted. Encapsulates the shape so a future field
/// addition only touches this helper.
fn settings(tracing_enabled: bool, endpoint: &str) -> TelemetrySettings {
    TelemetrySettings {
        tracing_enabled,
        service_name: "cognee-test".to_string(),
        exporter_otlp_endpoint: endpoint.to_string(),
        exporter_otlp_headers: String::new(),
        ..TelemetrySettings::default()
    }
}

#[test]
#[serial_test::serial]
fn already_instrumented_default_false() {
    // MUST run before any test that calls `set_tracer_provider`. Cargo
    // sorts tests alphabetically, so this name puts it first.
    assert!(
        !already_instrumented(),
        "fresh process should have a NoopTracerProvider as the global"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn init_telemetry_disabled_returns_noop() {
    // Both inputs empty/false → noop guard, no global mutation.
    let guard = init_telemetry(&settings(false, ""))
        .expect("init_telemetry must succeed on the disabled path");

    // The guard exists but did not install a provider, so the global
    // should still report "not instrumented". This is the strongest
    // observable proof that the disabled path is a true noop.
    assert!(
        !already_instrumented(),
        "disabled init_telemetry must not install a global provider"
    );

    drop(guard);
    // Dropping a noop guard is also a noop — must not panic.
}

#[tokio::test]
#[serial_test::serial]
async fn init_telemetry_endpoint_only_activates() {
    // Decision 2: a non-empty endpoint activates OTEL even without the
    // explicit `cognee_tracing_enabled = true`.
    //
    // We point at a non-routable endpoint. The OTLP exporter does NOT
    // connect synchronously at build time — the gRPC channel is
    // lazily-connected on first export, and span exports happen in a
    // background batch processor task. So `init_telemetry` returning
    // Ok here does not require a real collector.
    let guard = init_telemetry(&settings(false, "http://127.0.0.1:1"))
        .expect("init_telemetry must succeed even when the endpoint is unreachable");

    assert!(
        already_instrumented(),
        "non-empty endpoint must trigger global provider installation (decision 2)"
    );

    // Hold the guard explicitly to make the lifetime visible.
    let _guard: TelemetryGuard = guard;
}

#[tokio::test]
#[serial_test::serial]
async fn init_telemetry_flag_only_no_endpoint() {
    // Python parity: `enable_tracing()` builds a provider even with no
    // OTLP exporter (the in-memory CogneeSpanExporter is still attached).
    // For Rust, the equivalent is "build the provider, attach the
    // tracing-opentelemetry bridge, but skip the OTLP exporter".
    //
    // If task 04 chose to *require* a non-empty endpoint when the flag
    // is set (panicking or returning Err otherwise), this test must be
    // updated to assert that error shape — and the parent doc should be
    // updated to record the deviation from Python parity.
    let result = init_telemetry(&settings(true, ""));

    let guard = result.expect(
        "flag=true with empty endpoint must build a provider (Python parity); \
         if this fails, task 04 deviated from the design decision and the parent \
         doc must be updated to reflect that.",
    );

    assert!(
        already_instrumented(),
        "flag=true must install a global provider even without an endpoint"
    );

    drop(guard);
}

#[test]
#[serial_test::serial]
fn is_tracing_enabled_python_parity() {
    // Table-driven 2x2 truth table from decision 2.
    //
    //   flag | endpoint    | expected
    //  ------+-------------+----------
    //   F    | ""          | false   (disabled)
    //   F    | "http://x"  | true    (implicit activation)
    //   T    | ""          | true    (explicit)
    //   T    | "http://x"  | true    (both)
    let cases = [
        (false, "", false),
        (false, "http://example:4317", true),
        (true, "", true),
        (true, "http://example:4317", true),
    ];

    for (flag, endpoint, expected) in cases {
        let s = settings(flag, endpoint);
        assert_eq!(
            is_tracing_enabled(&s),
            expected,
            "is_tracing_enabled(flag={flag}, endpoint={endpoint:?}) should be {expected}"
        );
    }
}

#[tokio::test]
#[serial_test::serial]
async fn already_instrumented_after_set_true() {
    // After the previous test in this binary installed a provider via
    // `init_telemetry_endpoint_only_activates` or `_flag_only_no_endpoint`,
    // the global is non-noop. We can't reliably reset it (OTEL's global
    // is install-once), so the assertion is: at this point in the test
    // run, the detector returns true.
    //
    // Cargo test ordering is alphabetical, so this test name (`after_…`)
    // sorts AFTER `default_false` and AFTER both `init_telemetry_*`
    // tests. If a future Cargo version changes that ordering, the test
    // will become flaky and must be made independent (e.g. by calling
    // `init_telemetry` itself at the top of the function).
    //
    // To remove order coupling, install a provider explicitly here:
    let _guard = init_telemetry(&settings(false, "http://127.0.0.1:1"))
        .expect("install provider so already_instrumented can observe it");

    assert!(
        already_instrumented(),
        "after init_telemetry installs a provider, the detector must report true"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn telemetry_guard_drop_calls_shutdown() {
    // To verify shutdown semantics without a real collector, task 04
    // exposes a `TelemetryGuard::for_test_with_recording_processor()`
    // constructor (gated behind `#[cfg(any(test, feature = "testing"))]`)
    // that wires a tiny in-test `SpanProcessor` impl which records calls
    // to its `shutdown()` and `force_flush()` methods. If task 04 omits
    // that helper, this test should be moved to a `#[ignore]` state with
    // a TODO link to add it.
    //
    // Replace the import path below if task 04 named the helper
    // differently.
    use cognee_observability::testing::RecordingProcessor;

    let processor = RecordingProcessor::new();
    let guard = TelemetryGuard::for_test_with_recording_processor(processor.clone());

    assert_eq!(
        processor.shutdown_calls(),
        0,
        "no shutdown should have happened before the guard is dropped"
    );

    drop(guard);

    assert!(
        processor.force_flush_calls() >= 1,
        "Drop must call force_flush at least once"
    );
    assert_eq!(
        processor.shutdown_calls(),
        1,
        "Drop must call shutdown exactly once"
    );
}
```

### 5.3 `crates/lib/src/config.rs` — additions to `mod tests`

Append next to the existing `overlay_picks_up_*` tests (around [`config.rs:1207`](../../../crates/lib/src/config.rs#L1207)). The pattern (`unsafe { std::env::set_var(...) }` + `serial_test::serial` + cleanup) is copied verbatim from the surrounding tests for consistency:

```rust
#[test]
#[serial_test::serial]
fn overlay_picks_up_otel_exporter_otlp_protocol() {
    // SAFETY: test is serial — no other thread reads/writes env concurrently.
    unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf") };
    let mut s = Settings::default();
    s.overlay_from_env();
    unsafe { std::env::remove_var("OTEL_EXPORTER_OTLP_PROTOCOL") };

    assert_eq!(s.otel_exporter_otlp_protocol, "http/protobuf");
}

#[test]
#[serial_test::serial]
fn overlay_picks_up_otel_span_processor() {
    // SAFETY: test is serial — no other thread reads/writes env concurrently.
    unsafe { std::env::set_var("OTEL_SPAN_PROCESSOR", "simple") };
    let mut s = Settings::default();
    s.overlay_from_env();
    unsafe { std::env::remove_var("OTEL_SPAN_PROCESSOR") };

    assert_eq!(s.otel_span_processor, "simple");
}

#[test]
#[serial_test::serial]
fn overlay_picks_up_otel_traces_sampler() {
    // Tests both OTEL_TRACES_SAMPLER and OTEL_TRACES_SAMPLER_ARG in one
    // function — they are always read together in practice.
    // SAFETY: test is serial — no other thread reads/writes env concurrently.
    unsafe { std::env::set_var("OTEL_TRACES_SAMPLER", "parentbased_traceidratio") };
    unsafe { std::env::set_var("OTEL_TRACES_SAMPLER_ARG", "0.25") };
    let mut s = Settings::default();
    s.overlay_from_env();
    unsafe { std::env::remove_var("OTEL_TRACES_SAMPLER") };
    unsafe { std::env::remove_var("OTEL_TRACES_SAMPLER_ARG") };

    assert_eq!(s.otel_traces_sampler, "parentbased_traceidratio");
    assert_eq!(s.otel_traces_sampler_arg, "0.25");
}

#[test]
fn settings_default_otel_fields_match_decisions() {
    // Pin defaults so a typo in task 08 does not silently change them.
    let s = Settings::default();
    assert_eq!(s.cognee_tracing_enabled, false);
    assert_eq!(s.otel_service_name, "cognee");
    assert_eq!(s.otel_exporter_otlp_endpoint, "");
    assert_eq!(s.otel_exporter_otlp_headers, "");
    assert_eq!(s.otel_exporter_otlp_protocol, "grpc");
    assert_eq!(s.otel_span_processor, "batch");
    assert_eq!(s.otel_traces_sampler, "");
    assert_eq!(s.otel_traces_sampler_arg, "");
}
```

## 6. List of tests (mirrors §"Testing strategy" → "Unit tests" of the parent doc)

| # | Name | File | Notes |
|---|---|---|---|
| 1 | `parse_otlp_headers_empty` | `crates/observability/src/headers.rs` | Pure. |
| 2 | `parse_otlp_headers_single` | `crates/observability/src/headers.rs` | Pure. |
| 3 | `parse_otlp_headers_multi` | `crates/observability/src/headers.rs` | Pure. |
| 4 | `parse_otlp_headers_whitespace` | `crates/observability/src/headers.rs` | Pure. |
| 5 | `parse_otlp_headers_invalid_pair_is_skipped` | `crates/observability/src/headers.rs` | Documents the lenient skip-on-malformed contract. |
| 6 | `init_telemetry_disabled_returns_noop` | `crates/observability/tests/init.rs` | `#[serial]`. |
| 7 | `init_telemetry_endpoint_only_activates` | `crates/observability/tests/init.rs` | `#[serial]`. Implicit activation — decision 2. |
| 8 | `init_telemetry_flag_only_no_endpoint` | `crates/observability/tests/init.rs` | `#[serial]`. Pins Python parity. |
| 9 | `is_tracing_enabled_python_parity` | `crates/observability/tests/init.rs` | `#[serial]` (cheap; kept serial for env hygiene). 2×2 table. |
| 10 | `already_instrumented_default_false` | `crates/observability/tests/init.rs` | `#[serial]`. **Must run first** in this binary. |
| 11 | `already_instrumented_after_set_true` | `crates/observability/tests/init.rs` | `#[serial]`. Installs its own provider to be order-independent. |
| 12 | `overlay_picks_up_otel_exporter_otlp_protocol` | `crates/lib/src/config.rs` (`mod tests`) | `#[serial]` + `set_var`/`remove_var`. |
| 13 | `overlay_picks_up_otel_span_processor` | `crates/lib/src/config.rs` (`mod tests`) | `#[serial]`. |
| 14 | `overlay_picks_up_otel_traces_sampler` | `crates/lib/src/config.rs` (`mod tests`) | `#[serial]`. Covers `OTEL_TRACES_SAMPLER` + `OTEL_TRACES_SAMPLER_ARG`. |
| 15 | `settings_default_otel_fields_match_decisions` | `crates/lib/src/config.rs` (`mod tests`) | Pure. |
| 16 | `telemetry_guard_drop_calls_shutdown` | `crates/observability/tests/init.rs` | `#[serial]`. Uses the test-only `RecordingProcessor` helper that task 04 must expose. |

(Tests 1–11 are the parent doc's seven listed tests, expanded with the four sub-cases for header parsing and the two sides of `already_instrumented`. Tests 12–16 cover the config-overlay + defaults that the task description explicitly requested.)

## 7. Verification

```bash
# Unit tests against the noop fallback (no OTEL deps in the resolved graph).
cargo test -p cognee-observability

# Unit tests against the real path.
cargo test -p cognee-observability --features telemetry

# Settings overlay tests in the umbrella crate.
cargo test -p cognee-lib --lib config::tests

# Project-wide gate.
scripts/check_all.sh
```

Expected outcomes:

- All five `parse_otlp_headers_*` tests pass under both feature shapes.
- Under `--features telemetry`: tests 6–11 + 16 pass.
- Under default features: test 6 passes (noop path), test 7 should fall back to the noop guard and the assertion `already_instrumented()` stays `false` — **revisit test 7 in the noop fallback world**: it asserts that an endpoint activates the provider, which only holds when the `telemetry` feature is on. Mark test 7 with `#[cfg(feature = "telemetry")]` (and tests 8, 11, 16 likewise) so the default-features lane skips them cleanly. Test 6, 9, 10 are feature-agnostic.
- `cargo test -p cognee-lib --lib config::tests` includes all four new overlay/default tests (12–15) and they pass.
- `scripts/check_all.sh` is clean (fmt, check, clippy with `-D warnings`, and the binding scripts).

## 8. Files modified / created

| File | Change |
|---|---|
| `crates/observability/Cargo.toml` | Add `serial_test = { workspace = true }` (and optionally `temp-env`) under `[dev-dependencies]`. |
| `crates/observability/src/headers.rs` | **New `mod tests`** appended (5 unit tests for `parse_otlp_headers`). The module file itself is created by [task 04](./04-implement-init-otel-and-guard.md) — this task adds only the bottom `#[cfg(test)] mod tests` block. |
| `crates/observability/tests/init.rs` | **New file.** Integration-style unit tests covering `init_telemetry`, `is_tracing_enabled`, `already_instrumented`, and `TelemetryGuard::drop`. |
| [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs) | Append four tests to the existing `#[cfg(test)] mod tests` block ([line 1083](../../../crates/lib/src/config.rs#L1083)): three overlay tests + one defaults pin. |

No production code is added or modified by this task — only tests and the `[dev-dependencies]` entry.

## 9. Risks

1. **Global state survives within a test binary.** `opentelemetry::global::set_tracer_provider` is install-once; once a test installs a real provider, every subsequent test in the same `tests/init.rs` binary sees it. Mitigations:
   - `#[serial_test::serial]` on every test that touches the global.
   - Test names ordered so `already_instrumented_default_false` runs first (Cargo sorts alphabetically; `default_false` < `init_telemetry_*` < `after_set_true`).
   - `already_instrumented_after_set_true` installs its own provider so it does not depend on what ran before it.
   - The test binary is a fresh process per `cargo test` invocation, so re-running the suite always starts from a clean global.
   - **Residual risk:** if `cargo test -p cognee-observability --features telemetry -- --test-threads=2` is ever run, `serial_test` still serialises but the binary is the same — assertions remain valid because they each install before asserting.

2. **Env-var leak between tests.** `cargo test` shares one process per test binary. The `unsafe { set_var }` + cleanup pattern from `cognee-lib`'s existing tests works only because `serial_test::serial` enforces ordering. If a future test forgets `#[serial_test::serial]` while setting env vars, it can silently see another test's leftover env. Reviewers should fail any new env-mutating test that omits the annotation.

3. **OTEL crate may require a non-empty endpoint when an exporter is requested.** Test `init_telemetry_flag_only_no_endpoint` assumes that with `flag=true, endpoint=""`, task 04 will skip the OTLP exporter and still build the provider (Python parity). If the OTEL Rust SDK panics during `with_endpoint("")` (some versions do), task 04 must guard against that path, and this test will catch it. The `expect(...)` message tells a future debugger exactly where to look.

4. **`#[deny(missing_docs)]` on the crate root** ([task 02 §4.3](./02-observability-crate-scaffold.md)). The `RecordingProcessor` test helper assumed in test 16 must therefore be exposed under a `#[doc(hidden)]` `pub mod testing` inside the crate, or under `#[cfg(any(test, feature = "testing"))]` with a doc string. Reviewers of task 04 should make sure that helper exists; otherwise test 16 must be `#[ignore]`d with a TODO.

5. **`temp-env` is optional in this task.** The tests as written use `std::env::set_var` directly to avoid needing a workspace-level dep change. If reviewers prefer `temp-env::with_var(... || { ... })`, the test bodies become safer against panic-mid-test (they auto-restore env on unwind) and the `unsafe` blocks vanish — but adding the dep is a separate decision out of scope here.

6. **Test 7 vs 8 wording in `--no-default-features` mode.** When `cognee-observability` is built without the `telemetry` feature, `init_telemetry` always returns the noop guard regardless of inputs, so tests 7/8/11/16 must be feature-gated (`#[cfg(feature = "telemetry")]`). Test 6 (disabled-returns-noop), test 9 (`is_tracing_enabled` is a pure predicate), and test 10 (default global is noop) work under both feature shapes. Test 16 strictly requires the real path's `RecordingProcessor` helper.

## 10. References

- Parent doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md), in particular the [Testing strategy → Unit tests](../01-otel-otlp-export.md#unit-tests) subsection (the canonical list this task expands), the [Design decisions](../01-otel-otlp-export.md#design-decisions-locked) table (decisions 2, 4, 5, 10), and [Action items](../01-otel-otlp-export.md#action-items) #9.
- [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md) — defines `TelemetrySettings`, `TelemetryGuard`, `TelemetryInitError`, the `tests/` directory location, and `[dev-dependencies]` already in place.
- [`04-implement-init-otel-and-guard.md`](./04-implement-init-otel-and-guard.md) — provides `parse_otlp_headers`, `is_tracing_enabled`, `already_instrumented`, the real `TelemetryGuard::Drop` body, and (per §9 risk 4) the `RecordingProcessor` test helper.
- [`08-noop-fallback-and-tests.md`](./08-noop-fallback-and-tests.md) — adds the new `Settings` fields (`otel_exporter_otlp_protocol`, `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`) and their `overlay_from_env` branches, which tests 12–15 cover.
- Existing overlay-test patterns to mirror: [`crates/lib/src/config.rs:1083-1207`](../../../crates/lib/src/config.rs#L1083).
- Workspace dev-deps already available: [`Cargo.toml:78`](../../../Cargo.toml) (`serial_test = "3.2"`).
- Project conventions: [`../../../.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) — section "Test Patterns" (serial tests, `#[tokio::test]`, co-located inline tests vs `tests/`).
- OpenTelemetry Rust SDK install-once global semantics: [`opentelemetry_sdk` 0.31 docs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/) → `global::set_tracer_provider`.
