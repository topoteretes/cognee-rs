# Task 03-07 — `cognee.api.improve` OTEL span (Python parity)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**: —
**Blocks**: —

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decision**: #4 — bundle the missing OTEL span into this gap.

---

## 1. Goal

Wrap the body of [`cognee_lib::api::improve::improve()`](../../crates/lib/src/api/improve.rs#L125)
in a `tracing::info_span!("cognee.api.improve", ...)` to bring OTEL
parity with Python. Python emits both an analytics event and an OTEL
span around the function; gap 02-07 added the analytics event but
the span was never wired.

This is **independent** of all other gap-03 tasks. It can land in any
PR. Total diff: ~10 lines.

## 2. Rationale

### Why now

Python wraps `improve()` in a `cognee.api.improve` OTEL span. The
Rust port has the analytics event but no span. The result: in a
distributed-tracing dashboard (Tempo, Honeycomb), an `improve()` call
shows up as either:

- A bare run of pipeline-level / sub-step spans with no enclosing
  `cognee.api.improve` parent — readable, but inconsistent with
  `recall`, `forget`, etc. which all have parents.
- Or worse, no span at all if the caller doesn't have a tracer
  configured.

Adding the span is mechanical and gives a small but real operator
benefit. Bundling it here keeps the gap-03 PR train tidy.

### Span attributes to set

Match the Python equivalent and align with the existing
`cognee.api.recall` span at [`recall.rs:128`](../../crates/lib/src/api/recall.rs#L128):

| Attribute | Source |
|---|---|
| `dataset` | `dataset_name` (cloned from `params.dataset_name`) |
| `session_count` | `session_ids.as_ref().map(\|v\| v.len()).unwrap_or(0)` |
| `run_in_background` | `false` (locked literal — matches the analytics event payload from gap 02-07) |
| `cognee.tenant_id` | `params.tenant_id` formatted via `tenant_id_for_telemetry` |

Use existing semantic-attribute constants from
[`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
where one fits, or hard-code matching Python's verbatim attribute
names (Python prefers verbose `dataset_name`, `session_count`).

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- No outstanding edits to
  [`crates/lib/src/api/improve.rs`](../../crates/lib/src/api/improve.rs).

## 4. Step-by-step

### 4.1 Wrap the function body in a span

Edit [`improve.rs`](../../crates/lib/src/api/improve.rs#L125). The
current shape (after gap 02-07's analytics emit at line 161) is:

```rust
pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let ImproveParams { /* destructure */ } = params;

    let mut result = ImproveResult::default();
    let has_sessions = session_ids.as_ref().is_some_and(|ids| !ids.is_empty());

    // Mirrors Python `send_telemetry("cognee.improve", ...)` from
    // cognee/api/v1/improve/improve.py:91.
    #[cfg(feature = "telemetry")]
    {
        // existing emit
    }

    // ---- Stage 1: Apply Feedback Weights ----
    // ...
}
```

Refactor to wrap the body in a span. Two patterns are workable:

#### Pattern A — `#[tracing::instrument]` attribute

Cleanest, but the function takes a borrowed `ImproveParams<'_>` with
many fields. The attribute would need `skip_all` and explicit
`fields(...)`:

```rust
#[tracing::instrument(
    name = "cognee.api.improve",
    skip_all,
    fields(
        dataset = tracing::field::Empty,
        session_count = tracing::field::Empty,
        run_in_background = false,
    )
)]
pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let ImproveParams { dataset_name, session_ids, /* … */ } = params;
    let span = tracing::Span::current();
    span.record("dataset", dataset_name.as_str());
    span.record(
        "session_count",
        session_ids.as_ref().map(|v| v.len()).unwrap_or(0) as i64,
    );

    // ... rest unchanged ...
}
```

#### Pattern B — manual `info_span!().enter()`

Mirrors how `recall.rs:128` does it today. Less idiomatic but keeps
the function signature unchanged:

```rust
pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let ImproveParams { dataset_name, session_ids, /* … */ } = params;

    let span = tracing::info_span!(
        "cognee.api.improve",
        dataset = %dataset_name,
        session_count = session_ids.as_ref().map(|v| v.len()).unwrap_or(0),
        run_in_background = false,
    );
    let _enter = span.enter();

    // ... rest of body unchanged, including the existing analytics emit ...
}
```

> **Recommend Pattern B** for symmetry with `recall.rs`. Sub-agent
> B may pick A if it's cleaner once the body is fully read.

### 4.2 Don't break the analytics emit

The existing `#[cfg(feature = "telemetry")]` block in `improve.rs`
that fires `cognee.improve` (gap 02-07) must remain inside the new
span so the event ends up linked to the trace. Place the
`info_span!` *before* the analytics emit, not after.

### 4.3 (Optional) Tenant ID attribute

If the surrounding tracing infra includes a `cognee.tenant_id`
semantic-attribute constant, set it on the span:

```rust
let span = tracing::info_span!(
    "cognee.api.improve",
    dataset = %dataset_name,
    session_count = session_ids.as_ref().map(|v| v.len()).unwrap_or(0),
    run_in_background = false,
    "cognee.tenant_id" = tracing::field::Empty,
);
let _enter = span.enter();
if let Some(tid) = tenant_id {
    span.record("cognee.tenant_id", tid.to_string().as_str());
}
```

If no such constant exists today (search
[`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
and [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs)),
omit the attribute rather than introduce one in this small task.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Run the existing improve tests.
cargo test -p cognee-lib --test improve_tests   # adjust path if naming differs

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh

# 5. (Smoke) Run an example or a small in-process improve() call with
#    `RUST_LOG=info` and confirm `cognee.api.improve` appears in
#    structured-log output.
```

## 6. Files modified

- [`crates/lib/src/api/improve.rs`](../../crates/lib/src/api/improve.rs)
  — wrap function body in `tracing::info_span!("cognee.api.improve", …)`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Span dropped by an early-return path | Span lifetime tied to `_enter` guard — drops at function end regardless of return path. | Pattern B's `let _enter = span.enter();` is `Drop`-driven. |
| Attribute names diverge from Python | Verify Python's `improve.py:91` attribute set before hard-coding names. | Sub-agent A double-checks against `https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py#L91`. |
| Span suppressed when subscriber is not configured | Expected behaviour — span is a no-op when no subscriber is registered. | Document in commit body. |

## 8. Out of scope

- Adding richer span events (per-stage `info!` events already exist
  inside `improve()`).
- Wiring this span to the OTEL exporter — gap 01 (closed) already
  bridges `tracing` to OTLP via `tracing-opentelemetry`. The span
  shows up automatically.
- Migrating other API endpoints' missing OTEL spans (track separately
  if any are still missing — they aren't, as of 2026-05-07).

**Status**: implemented in commit ca4224e
