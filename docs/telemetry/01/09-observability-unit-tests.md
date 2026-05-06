# Task 09 — Unit tests for `cognee-observability`

**Status**: Implemented in commit 52c2be7

## Implementation notes

Deviations from the as-written task plan recorded for downstream reviewers:

1. **Production fix to `already_instrumented()`**: replaced the broken Debug-string heuristic (which always returned `true` against OTEL 0.31, silently disabling our telemetry path) with a `static OUR_PROVIDER_INSTALLED: OnceLock<()>` set after `set_tracer_provider()`. The function's semantics shifted from "host has installed any OTEL provider" detection to "we have installed it" idempotency, and it is no longer feature-gated. This change was outside the original tests-only task scope but was required because the integration tests would otherwise have pinned a broken behavior. Discovered while writing tests 7/8/10/11.
2. **Tests 7, 8, 10, and 11 were collapsed into a single `init_telemetry_full_activation_lifecycle` test** in `crates/observability/tests/init.rs`. Rationale: `OnceLock` is per-process; `#[serial]` enforces non-overlap but not deterministic ordering across separate `#[test]` functions. A single linear test walks the full lifecycle (default-noop → endpoint activation → flag activation → idempotent re-entry).
3. **Empty-endpoint and unknown-protocol error cases are not covered in this binary** because the `OnceLock` shortcut takes the bridge branch on every call after first install. This is documented in test comments. Covering these paths would require a separate test binary.
4. **Defaults extension**: `default_values_are_correct` in `crates/lib/src/config.rs` now pins all 8 OTEL fields (it previously pinned only `cognee_tracing_enabled` and `otel_service_name`).

**Original status:** Largely-already-implemented by tasks 04/07/08 — only the residual gap items below remain.
**Owner:** _unassigned_
**Depends on:** [Task 02 — Scaffold the `cognee-observability` workspace crate](./02-observability-crate-scaffold.md), [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-init-telemetry-implementation.md), [Task 08 — Noop fallback (and `Settings` field overlay)](./08-noop-fallback.md)
**Blocks:** Nothing (terminal task within the test pyramid; integration tests against a fake OTLP collector are tracked separately as action item 10 in the parent doc).
**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md)

---

## 1. Goal

Add the focused unit-test suite for the new `cognee-observability` crate plus the small overlay-test additions in `cognee-lib::config`. Together they cover:

- `parse_otlp_headers` happy paths and one error / skip case. **DONE — shipped in task 04 (commit 9b99576).** 6 tests live in `crates/observability/src/headers.rs` (`empty_input`, `single_pair`, `multiple_pairs_with_whitespace`, `malformed_pairs_skipped`, `empty_value_kept`, `trailing_comma`).
- `is_tracing_enabled` Python-parity logic (locked decision 2: settings flag **OR** non-empty endpoint). **REMAINING — see test 9 in §6.**
- `init_telemetry` returning a noop guard when both inputs are empty. **DONE — shipped in task 08 (commit 5b925c7) as `init_telemetry_noop_when_tracing_disabled` in `crates/observability/src/init.rs`.** Asserts via `guard.has_provider() == false`.
- `init_telemetry` building and globally installing a real provider when an endpoint is set. **REMAINING — see tests 7, 8 in §6.**
- `already_instrumented()` on a fresh process (returns `false`) and after a provider has been installed (returns `true`). **REMAINING — see tests 10, 11 in §6.**
- `Settings` field defaults and env-var overlay for the new OTEL keys. **PARTIALLY DONE.** `overlay_picks_up_otel_service_name` already exists (currently around `crates/lib/src/config.rs:1480`); defaults for `cognee_tracing_enabled` and `otel_service_name` already pinned (currently around `crates/lib/src/config.rs:1538-1539`) inside `default_values_are_correct`. Overlay tests for `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_SPAN_PROCESSOR`, `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG`, plus default-pin extension for those six fields, **REMAIN** (see tests 12–15 in §6).
- `EnvSettingsView::from_env()` parsing of the eight OTEL env vars including truthy/falsy `COGNEE_TRACING_ENABLED`. **DONE — shipped in task 07 (commit 56433e5).** 4 tests in `crates/observability/src/settings.rs` (`from_env_empty_matches_defaults`, `tracing_enabled_truthy_values`, `tracing_enabled_falsy_values`, `from_env_reads_all_fields`).
- `TelemetryGuard::drop` invokes `force_flush` + `shutdown` on the installed provider. **REMAINING but blocked.** See test 16 in §6 — the proposed `RecordingProcessor` helper does not exist and the current guard holds `SdkTracerProvider` directly (not a custom processor); a different test strategy is needed.

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

Both `init_telemetry` (when activated) and `already_instrumented_after_set_true` mutate **process-global** OTEL state via `opentelemetry::global::set_tracer_provider`. The OTEL Rust SDK's global provider is install-once-and-cached: after one test installs a provider, subsequent parallel tests see it. We use `#[serial_test::serial]` (already a workspace dev-dependency, see [`Cargo.toml:83`](../../../Cargo.toml#L83)) on every test that touches global state. Note that `serial_test` is already wired into `cognee-lib`'s `[dev-dependencies]` ([`crates/lib/Cargo.toml:127`](../../../crates/lib/Cargo.toml#L127)) but **not** yet into `cognee-observability` — §4.1 adds it. The same applies to env-var tests — `cargo test` shares a single process, and `std::env::set_var` is process-global.

### Why we test settings-overlay logic in `cognee-lib`, not here

Per [task 02 §4.3](./02-observability-crate-scaffold.md), the OTEL settings input lives in `cognee-observability` as the `SettingsView` trait (with `EnvSettingsView` as the env-driven impl), but the **env-var overlay** for `cognee-lib::Settings` is implemented inside `Settings::overlay_from_env` (which task 08 extends with the new fields). Tests for the overlay therefore belong in [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs)'s existing `#[cfg(test)] mod tests`, not in the observability crate. This keeps each test next to the code it exercises and matches the convention of the existing overlay tests (see the `overlay_picks_up_*` block starting around [`crates/lib/src/config.rs:1129`](../../../crates/lib/src/config.rs#L1129)).

## 3. Pre-conditions

- [Task 02](./02-observability-crate-scaffold.md): the `cognee-observability` crate exists with the `SettingsView` trait, `EnvSettingsView`, `TelemetryGuard`, `TelemetryInitError`, and the `init_telemetry` entry point.
- [Task 04](./04-init-telemetry-implementation.md): the real `init_telemetry` body builds a real `SdkTracerProvider`, installs it globally, returns `(BoxedTelemetryLayer<S>, TelemetryGuard)` whose `Drop` calls `force_flush` + `shutdown_with_timeout`. `parse_otlp_headers`, `is_tracing_enabled`, `already_instrumented` are all public functions on the crate root.
- [Task 08](./08-noop-fallback.md): `Settings` has the new fields `otel_exporter_otlp_protocol`, `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`, with defaults `"grpc"`, `"batch"`, `""`, `""` respectively, and the corresponding env-var overlay branches in `Settings::overlay_from_env`.

If any of those land later than expected, mark the dependent tests `#[ignore]` with a TODO referencing the task that gates them — do **not** weaken the assertions to make them pass.

## 4. Step-by-step

### 4.1 Add dev-dependencies to `crates/observability/Cargo.toml`

The crate currently has only `tokio` under `[dev-dependencies]` (see [`crates/observability/Cargo.toml:35-36`](../../../crates/observability/Cargo.toml#L35)); it does NOT yet pull in `serial_test`. This task must add `serial_test = { workspace = true }` so tests 7, 8, 10, 11 (and 16 if unblocked) compile. `temp-env` is optional — recommended only if a workspace-level dep is acceptable; if not, fall back to the `unsafe { std::env::set_var(...) }` + `serial_test::serial` pattern used in [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs#L1129).

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

Match the existing pattern (see the `overlay_picks_up_*` block starting at [`config.rs:1129`](../../../crates/lib/src/config.rs#L1129) and continuing through [`:1518`](../../../crates/lib/src/config.rs#L1518)): `#[serial_test::serial]`, `unsafe { std::env::set_var(...) }`, `Settings::default().overlay_from_env()`, then `unsafe { std::env::remove_var(...) }`. Add tests for the remaining new env vars (the `OTEL_TRACES_SAMPLER_ARG` variant is covered in the same test as `OTEL_TRACES_SAMPLER` to keep the file size in check).

## 5. Resulting code

### 5.1 `crates/observability/src/headers.rs` — `#[cfg(test)] mod tests`

The function under test is added by task 04 with this signature (verbatim from [`01/04-init-telemetry-implementation.md`](./04-init-telemetry-implementation.md), reproduced here only so the tests are self-explanatory):

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

Append next to the existing `overlay_picks_up_*` tests (the block runs from around [`config.rs:1129`](../../../crates/lib/src/config.rs#L1129) through [`:1518`](../../../crates/lib/src/config.rs#L1518)). The pattern (`unsafe { std::env::set_var(...) }` + `serial_test::serial` + cleanup) is copied verbatim from the surrounding tests for consistency:

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

Status legend: **DONE** (already shipped), **TODO** (still to add), **BLOCKED** (design question must be resolved first).

| # | Name | File | Status | Notes |
|---|---|---|---|---|
| 1 | `empty_input` | `crates/observability/src/headers.rs` | **DONE** (task 04) | Pure. Originally proposed as `parse_otlp_headers_empty`. |
| 2 | `single_pair` | `crates/observability/src/headers.rs` | **DONE** (task 04) | Pure. Covers what was proposed as `parse_otlp_headers_single`. |
| 3 | `multiple_pairs_with_whitespace` | `crates/observability/src/headers.rs` | **DONE** (task 04) | Pure. Combined `parse_otlp_headers_multi` + `..._whitespace`. |
| 4 | `malformed_pairs_skipped` | `crates/observability/src/headers.rs` | **DONE** (task 04) | Pure. Covers `parse_otlp_headers_invalid_pair_is_skipped`. |
| 5 | `empty_value_kept` + `trailing_comma` | `crates/observability/src/headers.rs` | **DONE** (task 04) | Two extra tests beyond the original proposal. |
| 6 | `init_telemetry_noop_when_tracing_disabled` | `crates/observability/src/init.rs` | **DONE** (task 08) | Asserts via `guard.has_provider() == false`. Inline `#[cfg(test)] mod tests`, not a separate `tests/init.rs` binary. |
| 7 | `init_telemetry_endpoint_only_activates` | `crates/observability/tests/init.rs` (new) | **TODO** | `#[serial]`. Implicit activation — decision 2. Build with `--features telemetry`. |
| 8 | `init_telemetry_flag_only_no_endpoint` | `crates/observability/tests/init.rs` (new) | **TODO** | `#[serial]`. Pins Python parity — current `build_exporter` will likely error on empty endpoint, so this test may need to assert an `Err(TelemetryInitError::ExporterBuild)` instead, or task 04 needs an extra branch. **Confirm against `crates/observability/src/init.rs::build_exporter` before writing.** |
| 9 | `is_tracing_enabled_python_parity` | `crates/observability/src/init.rs` `#[cfg(test)] mod tests` | **TODO** | Pure — no env, no global state. 2×2 truth table over `(tracing_enabled, otlp_endpoint)`. Build a small in-test impl of `SettingsView` (or reuse `EnvSettingsView` with explicit field overrides). No `#[serial]` needed. |
| 10 | `already_instrumented_default_false` | `crates/observability/tests/init.rs` (new) | **TODO** | `#[serial]`. Must be first in alphabetical order in this binary. Feature-gated. |
| 11 | `already_instrumented_after_set_true` | `crates/observability/tests/init.rs` (new) | **TODO** | `#[serial]`. Installs its own provider to be order-independent. Feature-gated. |
| 12 | `overlay_picks_up_otel_service_name` | `crates/lib/src/config.rs` (`mod tests`, currently around line 1480) | **DONE** (pre-task-09, see commit history of `config.rs`) | The protocol/headers/endpoint variants below are the actual gap. |
| 12b | `overlay_picks_up_otel_exporter_otlp_endpoint` | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | `#[serial]` + `set_var`/`remove_var`. |
| 12c | `overlay_picks_up_otel_exporter_otlp_headers` | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | `#[serial]`. |
| 12d | `overlay_picks_up_otel_exporter_otlp_protocol` | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | `#[serial]`. |
| 13 | `overlay_picks_up_otel_span_processor` | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | `#[serial]`. |
| 14 | `overlay_picks_up_otel_traces_sampler` | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | `#[serial]`. Covers `OTEL_TRACES_SAMPLER` + `OTEL_TRACES_SAMPLER_ARG`. |
| 15 | extend `default_values_are_correct` (currently around line 1521) with the 6 new OTEL fields | `crates/lib/src/config.rs` (`mod tests`) | **TODO** | Pure. Currently pins only `cognee_tracing_enabled` and `otel_service_name`. Extend to cover `otel_exporter_otlp_endpoint=""`, `otel_exporter_otlp_headers=""`, `otel_exporter_otlp_protocol="grpc"`, `otel_span_processor="batch"`, `otel_traces_sampler=""`, `otel_traces_sampler_arg=""`. Avoid creating a new test — append asserts to the existing one. |
| 16 | `telemetry_guard_drop_calls_shutdown` | `crates/observability/tests/init.rs` (new) | **BLOCKED** | The proposed `RecordingProcessor` helper does **not** exist and the current `TelemetryGuard` holds `SdkTracerProvider` directly (`crates/observability/src/guard.rs:23-26`). To add this test, either (a) add a `pub(crate)` test-only constructor that swaps the provider for a recording fake, (b) write the test as an integration-style assertion that drives a real OTLP exporter and observes the side effects through it, or (c) drop the test and rely on action item 10's collector-based integration test. Resolve as a §11 design question before writing. |

API-name corrections vs original §1 list (apply when porting the §5.2 sketch into real test code):
- `init_telemetry` returns `Result<(BoxedTelemetryLayer<S>, TelemetryGuard), TelemetryInitError>`, not just a guard — see [`crates/observability/src/init.rs:79-86`](../../../crates/observability/src/init.rs#L79). It is generic over the subscriber type, so call sites must spell `init_telemetry::<Registry>(&settings)` or rely on inference.
- The settings input is `&dyn SettingsView` ([`crates/observability/src/settings.rs:13-30`](../../../crates/observability/src/settings.rs#L13)), not a struct literal `TelemetrySettings`. There is no `TelemetrySettings` type in the current crate. The proposed test bodies in §5.2 must be rewritten to use either `EnvSettingsView` (which exposes `Default` and `from_env`, but no public field setters — wrap it in a thin in-test `struct StaticSettings { ... }` impl of `SettingsView`), or build that small in-test impl directly.
- Headers function is `pub fn parse_otlp_headers` (not `pub(crate)`); see [`crates/observability/src/lib.rs:45`](../../../crates/observability/src/lib.rs#L45).
- Test 6's noop assertion uses `TelemetryGuard::has_provider()` ([`guard.rs:53-62`](../../../crates/observability/src/guard.rs#L53), cfg-gated for `test/debug_assertions`), not `already_instrumented()`.

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
| `crates/observability/Cargo.toml` | Add `serial_test = { workspace = true }` under `[dev-dependencies]` (currently only `tokio` is there — see [`crates/observability/Cargo.toml:35-36`](../../../crates/observability/Cargo.toml#L35)). |
| `crates/observability/src/headers.rs` | **DONE** — task 04 already shipped 6 unit tests; nothing further required. |
| `crates/observability/src/init.rs` | Add `is_tracing_enabled_python_parity` (test 9) inside the existing `#[cfg(test)] mod tests` block (currently around lines 275-291). Pure, no `#[serial]`. |
| `crates/observability/tests/init.rs` | **New file** for tests 7, 8, 10, 11 (test 16 stays deferred — see §8b). All `#[serial]`, all gated `#[cfg(feature = "telemetry")]`. |
| [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs) | Add overlay tests 12b, 12c, 12d, 13, 14 (5 new tests). Extend the existing `default_values_are_correct` (currently around line 1521) with assertions for the 6 OTEL fields not yet pinned. |

No production code is added or modified by this task — only tests and (optionally) a `[dev-dependencies]` line.

## 8b. Open design question (must resolve before writing test 16)

How should `telemetry_guard_drop_calls_shutdown` (test 16) observe the `force_flush` + `shutdown` side effects?

The current `TelemetryGuard` holds `SdkTracerProvider` directly (`crates/observability/src/guard.rs:22-26`) and `Drop` calls `provider.force_flush()` + `provider.shutdown_with_timeout(...)` on it. There is no recording processor injection point. Options:

1. **Add a test-only constructor** to `TelemetryGuard` that builds an `SdkTracerProvider` wired with a custom `SpanProcessor` impl that records flush/shutdown calls. Pros: small surface; cons: requires hand-rolling a `SpanProcessor` mock and exposing a `pub(crate)` or `#[cfg(test)] pub` constructor.
2. **Drop test 16** and rely on action item 10's collector-based integration test (which already exercises the flush-on-drop path end-to-end via spans actually arriving at a fake collector). Pros: zero new code; cons: leaves the unit-level RAII assertion uncovered until 10 lands.
3. **Inspect the provider's internal state** after drop. Not viable — the SDK doesn't expose a stable "is shutdown" predicate.

Recommended: **option 2** (drop test 16 from this task, document it as covered by action item 10) unless the team explicitly wants the unit-level assertion. If option 1 is preferred, the test helper must live in `crates/observability/src/guard.rs` behind `#[cfg(any(test, feature = "testing"))]` and be added in a follow-up to task 04, not in this task.

## 9. Risks

1. **Global state survives within a test binary.** `opentelemetry::global::set_tracer_provider` is install-once; once a test installs a real provider, every subsequent test in the same `tests/init.rs` binary sees it. Mitigations:
   - `#[serial_test::serial]` on every test that touches the global.
   - Test names ordered so `already_instrumented_default_false` runs first (Cargo sorts alphabetically; `default_false` < `init_telemetry_*` < `after_set_true`).
   - `already_instrumented_after_set_true` installs its own provider so it does not depend on what ran before it.
   - The test binary is a fresh process per `cargo test` invocation, so re-running the suite always starts from a clean global.
   - **Residual risk:** if `cargo test -p cognee-observability --features telemetry -- --test-threads=2` is ever run, `serial_test` still serialises but the binary is the same — assertions remain valid because they each install before asserting.

2. **Env-var leak between tests.** `cargo test` shares one process per test binary. The `unsafe { set_var }` + cleanup pattern from `cognee-lib`'s existing tests works only because `serial_test::serial` enforces ordering. If a future test forgets `#[serial_test::serial]` while setting env vars, it can silently see another test's leftover env. Reviewers should fail any new env-mutating test that omits the annotation.

3. **OTEL crate may require a non-empty endpoint when an exporter is requested.** Test `init_telemetry_flag_only_no_endpoint` assumes that with `flag=true, endpoint=""`, task 04 will skip the OTLP exporter and still build the provider (Python parity). If the OTEL Rust SDK panics during `with_endpoint("")` (some versions do), task 04 must guard against that path, and this test will catch it. The `expect(...)` message tells a future debugger exactly where to look.

4. **`#[deny(missing_docs)]` on the crate root** ([task 02 §4.3](./02-observability-crate-scaffold.md), confirmed at [`crates/observability/src/lib.rs:34`](../../../crates/observability/src/lib.rs#L34)). The `RecordingProcessor` test helper that the §5.2 sketch of test 16 assumes does **not** exist in the current crate; if a future revision chooses to land that helper rather than deferring test 16 to action item 10, it must be exposed under a `#[doc(hidden)]` `pub mod testing` or under `#[cfg(any(test, feature = "testing"))]` with a doc string. This task as-is keeps test 16 deferred (see §8b).

5. **`temp-env` is optional in this task.** The tests as written use `std::env::set_var` directly to avoid needing a workspace-level dep change. If reviewers prefer `temp-env::with_var(... || { ... })`, the test bodies become safer against panic-mid-test (they auto-restore env on unwind) and the `unsafe` blocks vanish — but adding the dep is a separate decision out of scope here.

6. **Test 7 vs 8 wording in `--no-default-features` mode.** When `cognee-observability` is built without the `telemetry` feature, `init_telemetry` always returns the noop guard regardless of inputs, so tests 7/8/11/16 must be feature-gated (`#[cfg(feature = "telemetry")]`). Test 6 (disabled-returns-noop), test 9 (`is_tracing_enabled` is a pure predicate), and test 10 (default global is noop) work under both feature shapes. Test 16 strictly requires the real path's `RecordingProcessor` helper.

## 10. References

- Parent doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md), in particular the [Testing strategy → Unit tests](../01-otel-otlp-export.md#unit-tests) subsection (the canonical list this task expands), the [Design decisions](../01-otel-otlp-export.md#design-decisions-locked) table (decisions 2, 4, 5, 10), and [Action items](../01-otel-otlp-export.md#action-items) #9.
- [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md) — defines the `SettingsView` trait, `EnvSettingsView`, `TelemetryGuard`, `TelemetryInitError`, the `tests/` directory location, and `[dev-dependencies]` already in place.
- [`04-init-telemetry-implementation.md`](./04-init-telemetry-implementation.md) — provides `parse_otlp_headers`, `is_tracing_enabled`, `already_instrumented`, and the real `TelemetryGuard::Drop` body. The `RecordingProcessor` test helper that test 16 needs does **not** exist; see §8b for the deferral plan.
- [`08-noop-fallback.md`](./08-noop-fallback.md) — adds the new `Settings` fields (`otel_exporter_otlp_protocol`, `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`) and their `overlay_from_env` branches, which tests 12–15 cover.
- Existing overlay-test patterns to mirror: the `overlay_picks_up_*` block starting at [`crates/lib/src/config.rs:1129`](../../../crates/lib/src/config.rs#L1129).
- Workspace dev-deps already available: [`Cargo.toml:83`](../../../Cargo.toml#L83) (`serial_test = "3.2"`).
- Project conventions: [`../../../.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) — section "Test Patterns" (serial tests, `#[tokio::test]`, co-located inline tests vs `tests/`).
- OpenTelemetry Rust SDK install-once global semantics: [`opentelemetry_sdk` 0.31 docs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/) → `global::set_tracer_provider`.
