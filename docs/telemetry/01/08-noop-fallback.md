# Task 08 — No-deps fallback for `cognee-observability`

**Status**: Implemented in commit 5b925c7
**Owner:** _unassigned_
**Depends on:**
- [Task 02 — Scaffold the `cognee-observability` workspace crate](./02-observability-crate-scaffold.md) (provides the crate skeleton, `TelemetryGuard`, `SettingsView`, `EnvSettingsView`).
- [Task 04 — Implement `init_telemetry` and `TelemetryGuard`](./04-init-telemetry-implementation.md) (already shipped the unified `init.rs`, the `BoxedTelemetryLayer<S>` alias, the `TelemetryInitError` enum, and the runtime noop branch).

**Blocks:**
- [Task 05 — `cognee-lib` re-exports & subscriber composition helper](./05-cognee-lib-reexports.md)
- [Task 06 — Refactor CLI subscriber](./06-cli-subscriber-refactor.md)
- [Task 07 — Refactor HTTP server subscriber](./07-http-server-subscriber-refactor.md)

**Parent doc:** [01 — OpenTelemetry SDK + OTLP Export Wiring](../01-otel-otlp-export.md)

---

## 0. What changed since this sub-doc was first drafted

The bulk of what task 08 originally described — a separate `noop.rs` mirroring a `real.rs` module, a non-generic `Box<dyn Layer<Registry>>` alias, and a rename of `OtelInitError` → `TelemetryInitError` — was absorbed into **task 04 (commit `9b99576`)**. That commit landed a single `crates/observability/src/init.rs` with inline `#[cfg(feature = "telemetry")]` branching instead of two parallel modules, and improved the layer alias to `BoxedTelemetryLayer<S>` (generic over the subscriber `S`) so call sites are not pinned to `tracing_subscriber::Registry`. The error enum was renamed to `TelemetryInitError` in commit `27c2bb2`.

The runtime contract this task was originally chartered to deliver is therefore **already in place**:

- `init_telemetry` is feature-agnostic: with `--features telemetry` off it returns `Ok((noop_layer, TelemetryGuard::noop()))`; with the feature on it still returns the same shape when `is_tracing_enabled(settings)` is `false`.
- `is_tracing_enabled(&dyn SettingsView)` returns `tracing_enabled || !otlp_endpoint.is_empty()` per parent [decision 6](../01-otel-otlp-export.md#design-decisions-locked) (implicit activation when an endpoint is set). This corrects the original sub-doc, which mistakenly proposed "noop always returns false".
- `TelemetryGuard::noop()` is publicly constructible and `has_provider()` is exposed in `cfg(test)` / `debug_assertions` builds for inspection.
- `parse_otlp_headers` lives in `crates/observability/src/headers.rs` with its own unit tests and is exported unconditionally.
- A stub `TelemetryInitError::FeatureDisabled` is defined on the no-feature path so that downstream code can name the type without cfg-gating.

What remains for this task is **only the two small gaps** below; gap (b) (CI tree-grep proving no OTEL deps under `--no-default-features`) is **deferred to [task 12](./12-ci-updates.md)**.

- **Gap (a) — Unit test pinning the noop contract.** The repo currently has no test that locks the "tracing disabled → Ok with noop guard" behaviour. We add one to `crates/observability/src/init.rs` and run it under both `--no-default-features` and `--features telemetry`.
- **Gap (c) — Feature-state contract paragraph in the crate-level rustdoc.** The current `lib.rs` rustdoc describes the feature flag in prose but does not explicitly nail down the two-pronged noop guarantee (compile-off OR runtime-disabled). We add a short, normative paragraph.

## 1. Goal

Make the noop semantics that already exist in code observable in two places they currently are not:

1. A unit test, so the contract cannot regress silently.
2. The crate-level rustdoc, so embedders reading `cargo doc` see the contract spelled out.

That is the entire scope of task 08 as it stands today.

## 2. Rationale (kept brief, see prior context for the long form)

### 2.1 Uniform API across feature states

[Tasks 06](./06-cli-subscriber-refactor.md) and [07](./07-http-server-subscriber-refactor.md) need to call `init_telemetry` once and unconditionally compose the returned layer. If the disabled-feature path either failed to compile or returned a structurally different value, every call site would need a `#[cfg(feature = "telemetry")]` fork. Task 04 already enforced the uniform shape; the unit test in step 1 below is what keeps it that way.

### 2.2 Why the noop is `tracing_subscriber::layer::Identity`

`Identity` is a unit struct that implements `Layer<S>` for any `S: Subscriber`, with all methods left as the trait defaults — it observes nothing and forwards nothing. `init.rs` boxes it into `BoxedTelemetryLayer<S>` for both the disabled and the runtime-noop paths. No custom `NoopLayer` is needed.

### 2.3 Two paths to "noop"

There are two distinct ways callers can land on the noop branch, and both must behave identically:

- **Compile-time off**: built without the `telemetry` feature. No OTEL crates are linked; the function body falls through to `Box::new(Identity::new())`.
- **Runtime off**: built *with* `telemetry` but `is_tracing_enabled(settings)` returns `false` (no `COGNEE_TRACING_ENABLED=true` and an empty `OTEL_EXPORTER_OTLP_ENDPOINT`). Same return value.

The unit test in step 1 covers both states; the rustdoc in step 2 names them.

## 3. Pre-conditions

- [Task 02](./02-observability-crate-scaffold.md) merged.
- [Task 04](./04-init-telemetry-implementation.md) merged (commit `9b99576` and follow-up `27c2bb2`).
- A clean `cargo check --workspace` on `main`.

## 4. Step-by-step

### Step 1 — Pin the noop contract with a unit test

**File:** `crates/observability/src/init.rs`. Extend the existing `#[cfg(test)] mod tests` block (or add one if absent — at the bottom of the file).

**Test name:** `init_telemetry_noop_when_tracing_disabled`.

**Body sketch (≤ 30 lines):**

- Construct an `EnvSettingsView::default()` (or any `SettingsView` impl with `tracing_enabled() == false` and an empty `otlp_endpoint()`).
- Call `init_telemetry::<tracing_subscriber::Registry>(&settings)` and assert the result is `Ok`.
- Assert `guard.has_provider() == false` — the inspector is gated on `cfg(any(test, debug_assertions))` per `crates/observability/src/guard.rs` so it is callable from this test under both feature states.
- Wire the returned layer into a `tracing_subscriber::Registry::default().with(layer)` chain to confirm it composes; this is the lightest possible "the layer is the identity layer" check that does not depend on internal OTEL types.

The test must compile and pass under **both** `cargo test -p cognee-observability` (default features, `telemetry` off) and `cargo test -p cognee-observability --features telemetry`. On the `telemetry`-on path the test exercises the runtime noop branch (`is_tracing_enabled` returns `false` for the default settings); on the off path it exercises the compile-time branch.

The test is intentionally tiny — it locks the contract, it does **not** test the OTEL plumbing. Provider-build, sampler, and exporter behaviour live in [task 09](./09-observability-unit-tests.md) and the integration suite in [task 10](./10-otel-export-integration-test.md).

### Step 2 — Add a "Feature-state contract" rustdoc paragraph

**File:** `crates/observability/src/lib.rs`. Insertion point: just below the existing `## Feature flags` subsection of the crate-level `//!` rustdoc, as a new `## Feature-state contract` subsection.

**Content (5–10 lines, prose):**

> [`init_telemetry`] returns `Ok((noop_layer, TelemetryGuard::noop()))` whenever the process is *not* configured to export spans — specifically when **either** (1) the `telemetry` cargo feature is **off** at compile time, **or** (2) [`is_tracing_enabled`] returns `false` at runtime (i.e. `COGNEE_TRACING_ENABLED` is not truthy and `OTEL_EXPORTER_OTLP_ENDPOINT` is empty). On both paths the returned layer is a boxed [`tracing_subscriber::layer::Identity`] that observes nothing, and the guard's `Drop` runs no code. [`TelemetryGuard::noop`] is publicly constructible for tests and embedders that want the same shape without going through `init_telemetry`. This mirrors parent [decision 6](../../docs/telemetry/01/01-otel-otlp-export.md#design-decisions-locked) (implicit activation: an endpoint alone is enough to opt in).

The paragraph is normative: any future change that breaks either branch of the contract must update this paragraph, the test in step 1, and the parent decision table together.

## 5. Verification

```bash
# Step 1 unit test — default features (telemetry feature off, compile-time noop branch).
cargo test -p cognee-observability init_telemetry_noop_when_tracing_disabled

# Step 1 unit test — telemetry feature on (runtime noop branch).
cargo test -p cognee-observability --features telemetry init_telemetry_noop_when_tracing_disabled

# Step 2 rustdoc — confirm the new "Feature-state contract" subsection renders.
cargo doc -p cognee-observability --no-deps
# Open target/doc/cognee_observability/index.html and verify the section is
# visible under the crate-level docs.

# Project gate.
scripts/check_all.sh
```

Expected:

- Both `cargo test` runs pass; the new test exits 0 in each.
- `cargo doc` produces HTML that includes the new `## Feature-state contract` subsection.
- `scripts/check_all.sh` exits 0.

The `cargo tree --no-default-features | grep -E 'opentelemetry|tracing-opentelemetry'` check that proves no OTEL crates leak into the default build is **deferred to [task 12](./12-ci-updates.md)**, which owns the workspace CI lanes.

## 6. Files modified

| File | Change |
|---|---|
| [`crates/observability/src/init.rs`](../../../crates/observability/src/init.rs) | Add `init_telemetry_noop_when_tracing_disabled` unit test in the `#[cfg(test)] mod tests` block. |
| [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs) | Add a `## Feature-state contract` subsection to the crate-level `//!` rustdoc. |

No source outside `crates/observability/` is changed in this task.

## 7. Risks

1. **Type signature drift between feature states.** If a future change to `init.rs` evolves the return type but only updates one cfg branch, `cargo check --no-default-features` breaks at every call site. **Mitigation:** the unit test in step 1 runs in *both* feature states (the verification block runs `cargo test` twice) and exercises the same call. CI in [task 12](./12-ci-updates.md) re-runs both lanes.
2. **`has_provider()` visibility regression.** The test calls `guard.has_provider()`, which is gated on `cfg(any(test, debug_assertions))`. If a future cleanup tightens that gate, the test breaks. **Mitigation:** if `has_provider()` is ever removed or further restricted, replace the assertion with a structural `matches!`-style check on a private accessor added for testing, or with the looser "the returned layer composes onto a registry without panic" assertion already in the test body.
3. **Doc paragraph drift from parent decisions.** The rustdoc paragraph references parent decision 6. If decision 6 is renumbered or renegotiated, the paragraph and the parent table can fall out of sync. **Mitigation:** any change to decision 6 must update this paragraph in the same PR.

## 8. References

- Parent gap doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md), in particular the
  ["When is OTEL enabled?"](../01-otel-otlp-export.md#when-is-otel-enabled) subsection,
  [Action items](../01-otel-otlp-export.md#action-items) #8 (this task) and #12 (the deferred
  CI lane), and [Design decisions table](../01-otel-otlp-export.md#design-decisions-locked)
  decisions 1, 6, 10.
- Sibling sub-docs:
  - [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md) — pre-condition
    (crate skeleton, `SettingsView`, `EnvSettingsView`).
  - [`04-init-telemetry-implementation.md`](./04-init-telemetry-implementation.md) — pre-condition
    and the source of the unified `init.rs` plus `BoxedTelemetryLayer<S>` (commit `9b99576`).
  - [`05-cognee-lib-reexports.md`](./05-cognee-lib-reexports.md) — first downstream consumer of
    the uniform API.
  - [`06-cli-subscriber-refactor.md`](./06-cli-subscriber-refactor.md),
    [`07-http-server-subscriber-refactor.md`](./07-http-server-subscriber-refactor.md) — both
    rely on `with(layer)` compiling regardless of feature state.
  - [`09-observability-unit-tests.md`](./09-observability-unit-tests.md) — owns the broader unit
    test suite (sampler, protocol, exporter); this task only adds the single noop-contract test.
  - [`12-ci-updates.md`](./12-ci-updates.md) — owns the deferred `cargo tree` no-OTEL-deps check.
- External:
  - [`tracing_subscriber::layer::Identity`](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/layer/struct.Identity.html)
    — canonical noop layer used by `init.rs::noop_layer`.
  - [`tracing_subscriber::Layer`](https://docs.rs/tracing-subscriber/0.3/tracing_subscriber/layer/trait.Layer.html)
    — trait whose default methods are no-ops.
- Project conventions: [`../../../.claude/CLAUDE.md`](../../../.claude/CLAUDE.md), specifically the
  [Coding conventions](../../../.claude/CLAUDE.md#coding-conventions) section (no `unwrap()` in
  non-test code; `expect("...")` only with a justifying message).
