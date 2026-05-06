# Action item 3 — Wire the `telemetry` cargo feature in `cognee-lib`, `cognee-cli`, and `cognee-http-server`

- **Status**: Implemented in commit ef813b9
- **Owner / dependencies:**
  - **Depends on:** task [`02` — bootstrap the `cognee-observability` crate](./02-cognee-observability-crate.md) (this task assumes `cognee-observability` exists with its own `telemetry` feature that activates the OTEL stack).
  - **Blocks:** task [`05` — `cognee-lib` `init_telemetry` re-exports & subscriber helper](./05-cognee-lib-public-api.md), task [`06` — CLI subscriber refactor](./06-cli-subscriber-refactor.md), task [`07` — HTTP-server subscriber refactor](./07-http-server-subscriber-refactor.md).
- **Anchor in parent:** action item 3 of [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md#action-items) (this task supersedes the original "add `telemetry` to default" wording with the locked decisions 1, 7, and 8).

## Rationale

The parent design (decisions 1, 6, 7, 8) settled on:

1. A single, workspace-level cargo feature called `telemetry` that turns on the OpenTelemetry stack everywhere.
2. The OTEL implementation lives in a sibling crate `cognee-observability` (decision 6), not directly inside `cognee-lib`.
3. The feature is **opt-in** — it must NOT appear in any `default = [...]` list (decision 1), nor in `android-default` (decision 8).
4. Both binary front-ends (`cognee-cli`, `cognee-http-server`) must expose `--features telemetry` that forwards through `cognee-lib` (decision 7).

A single forwarding feature is preferable to per-crate flags because:

- **One toggle.** Operators flip `--features telemetry` once instead of `--features cognee-lib/telemetry,cognee-observability/telemetry,cognee-core/telemetry`.
- **Consistent semantics.** Whenever telemetry is "on", *both* the high-level event emission (`tracing::info!(target: "cognee.telemetry", ...)` call sites already gated on `feature = "telemetry"` in [`crates/lib/src/api/forget.rs:105`](../../../crates/lib/src/api/forget.rs#L105) and the four sites in [`crates/core/src/pipeline.rs`](../../../crates/core/src/pipeline.rs) at lines 970/993/1039/1068) **and** the OTLP exporter pipeline (from `cognee-observability`) are activated together.
- **Separation of concern intact.** `cognee-core/telemetry` is still a free-standing feature (it just gates `tracing::event!` calls inside the pipeline runner). `cognee-lib/telemetry` *forwards* to it rather than replacing it, so any other consumer of `cognee-core` (e.g. `cognee-http-server` already depends on `cognee-core` directly) can still enable it independently if needed.
- **Off by default.** Decision 1 makes `cargo build` produce a binary with zero OTEL dependencies. Users who need OTEL run `cargo build --features telemetry` (or `cargo install cognee-cli --features telemetry`).

The existing `telemetry = []` feature on `cognee-lib` (line 41 of `crates/lib/Cargo.toml`) is empty today and only acts as a marker for the `cfg(feature = "telemetry")` guards in the high-level API. Replacing it with a forwarding feature keeps those guards working **and** pulls in the new OTEL pipeline, with no source code changes inside `crates/lib/src/api/`.

## Pre-conditions

- Task [`02`](./02-cognee-observability-crate.md) merged: the workspace contains `crates/observability/` with a manifest exposing `telemetry = [...]` that activates the OTEL stack (workspace deps from action item 1).
- Workspace-wide OTEL deps from action item 1 already declared in the root [`Cargo.toml`](../../../Cargo.toml)'s `[workspace.dependencies]`.

## Step-by-step

### 1. Edit [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml)

a. Replace the standalone `telemetry = []` declaration (currently line 41, with the comment block at lines 37–40) with a forwarding feature that keeps the explanatory comment intact:

```toml
# External telemetry event export (opt-in). When enabled, the high-level API
# functions emit `tracing` events on the `cognee.telemetry` target so
# downstream subscribers (OTEL log exporter, tracing_subscriber::Layer, etc.)
# can capture them. Mirrors Python's `send_telemetry()` calls. This feature
# also pulls in the `cognee-observability` crate which installs the
# OpenTelemetry SDK + OTLP exporter (see docs/telemetry/01-otel-otlp-export.md)
# and forwards to `cognee-core/telemetry` so pipeline-runner event emission
# is enabled in lockstep.
telemetry = [
    "dep:cognee-observability",
    "cognee-observability/telemetry",
    "cognee-core/telemetry",
]
```

b. Add the optional dependency to `[dependencies]` (alphabetically near the other `cognee-*` rows, after `cognee-models`):

```toml
cognee-observability = { path = "../observability", optional = true }
```

c. **Do NOT** add `"telemetry"` to the `default = [...]` list at line 7 (decision 1) and **do NOT** add it to `android-default` at line 57 (decision 8).

### 2. Edit [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml)

Add a forwarding feature in `[features]` (next to the other forwarding features at lines 28–38). Place it after `tiktoken` for alphabetical-ish grouping:

```toml
telemetry    = ["cognee-lib/telemetry"]
```

**Do NOT** add `"telemetry"` to `default` (line 12) — per decision 1, plain `cargo install cognee-cli` ships without OTEL.

### 3. Edit [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)

The crate currently has `default = []` (line 16) and a `bin` feature only. Add a forwarding feature plus the optional dep:

```toml
[features]
default = []
bin = [ ... ]
telemetry = ["dep:cognee-observability", "cognee-observability/telemetry", "cognee-core/telemetry"]
```

…and under `[dependencies]`:

```toml
cognee-observability = { path = "../observability", optional = true }
```

The HTTP server intentionally does **not** depend on `cognee-lib` (see the comment at lines 36–38 of its manifest about the cycle through `cognee-lib`'s `server` feature), so it must declare the forwarding feature directly against `cognee-observability` rather than going through `cognee-lib`.

### 4. Confirm `android-default` does NOT inherit `telemetry`

Re-read `crates/lib/Cargo.toml:57-70` and `crates/cli/Cargo.toml:54`. Verify that neither composite includes the new `telemetry` entry. (Decision 8.)

### 5. Verify the default-off build

```bash
cargo check --all-targets
```

Expected: succeeds with no `cognee-observability` symbols pulled in. The existing `#[cfg(feature = "telemetry")]` guards in `crates/lib/src/api/forget.rs:105` and `crates/core/src/pipeline.rs` lines {970, 993, 1039, 1068} compile out.

### 6. Verify the telemetry-on build

```bash
cargo check --all-targets --features cognee-lib/telemetry
cargo check --all-targets -p cognee-cli --features cognee-cli/telemetry
cargo check --all-targets -p cognee-http-server --features cognee-http-server/telemetry
```

Expected: the `cognee-observability` crate is built, OTEL deps are linked, and the pipeline-runner event-emission `#[cfg]`s in `cognee-core` are now active.

### 7. Verify the no-default-features fallback

```bash
cargo check --all-targets -p cognee-lib --no-default-features
cargo check --all-targets -p cognee-cli --no-default-features
cargo check --all-targets -p cognee-http-server --no-default-features
```

Expected: every crate compiles to its noop subset (no OTEL, no embedding backend, etc.). This guards the per-decision-1 contract that telemetry is never silently activated by an unrelated feature.

### 8. Run the full check suite

```bash
scripts/check_all.sh
```

(Per project rule — fmt, clippy, capi/python/js binding checks. None of those crates are affected by this change, but the umbrella check is required after Cargo.toml edits.)

## Resulting diffs

### `crates/lib/Cargo.toml`

```diff
@@ line ~41 (the `telemetry = []` entry)
-telemetry = []
+telemetry = [
+    "dep:cognee-observability",
+    "cognee-observability/telemetry",
+    "cognee-core/telemetry",
+]
@@ in [dependencies], near other cognee-* entries
+cognee-observability = { path = "../observability", optional = true }
```

### `crates/cli/Cargo.toml`

```diff
@@ in [features], after `tiktoken      = ["cognee-lib/tiktoken"]`
+telemetry    = ["cognee-lib/telemetry"]
```

### `crates/http-server/Cargo.toml`

```diff
@@ in [features]
 default = []
 bin = [ ... ]
+telemetry = [
+    "dep:cognee-observability",
+    "cognee-observability/telemetry",
+    "cognee-core/telemetry",
+]
@@ in [dependencies]
+cognee-observability = { path = "../observability", optional = true }
```

No `default = [...]` line changes anywhere.

## Verification matrix

| Build | Command | Expected outcome |
|---|---|---|
| Default off | `cargo check --all-targets` | No `cognee-observability`, no OTEL deps, telemetry `cfg`s inactive. |
| Telemetry on (lib) | `cargo check --all-targets -p cognee-lib --features cognee-lib/telemetry` | `cognee-observability` compiled, telemetry `cfg`s active. |
| Telemetry on (cli) | `cargo check --all-targets -p cognee-cli --features cognee-cli/telemetry` | Forwarding through `cognee-lib` works. |
| Telemetry on (http-server) | `cargo check --all-targets -p cognee-http-server --features cognee-http-server/telemetry` | Direct `cognee-observability` dep wired (no `cognee-lib` cycle). |
| Noop fallback | `cargo check --all-targets -p cognee-lib --no-default-features` | Compiles; no OTEL deps. |
| Full suite | `scripts/check_all.sh` | All gates green. |

## Files modified

- [`crates/lib/Cargo.toml`](../../../crates/lib/Cargo.toml)
- [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml)
- [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)

No source files (`.rs`) are touched in this task. The bridge layer wiring lives in tasks [`05`](./05-cognee-lib-public-api.md) (re-exports), [`06`](./06-cli-subscriber-refactor.md) (CLI), and [`07`](./07-http-server-subscriber-refactor.md) (HTTP server).

## Risks

1. **Feature unification changes `cognee-core/telemetry` semantics.** Today, only `cognee-lib`'s own gated event sites turn on when `cognee-lib/telemetry` is set; the four `cfg(feature = "telemetry")` blocks in `crates/core/src/pipeline.rs` (lines 970, 993, 1039, 1068) only activate when `cognee-core/telemetry` is **separately** enabled (currently never, because no consumer turns it on). After this task they activate together. **Mitigation:** the events are pure `tracing::info!(target: "cognee.telemetry", ...)` calls — they cost a filter check when no subscriber matches the target. Confirm by grepping `crates/core/src/pipeline.rs` and reviewing the four sites; if any of them are on a hot inner loop, mark them `#[inline(never)]` or audit the formatting cost. Initial inspection shows they fire at pipeline-task boundaries, so cost is negligible.
2. **`cargo install cognee-cli` users lose implicit telemetry.** Some operators may have relied on the default — but per design decision 1, telemetry is intentionally opt-in. Call this out in the release notes and in the documentation update bundled with task [`11`](./11-documentation.md). The migration path is `cargo install cognee-cli --features telemetry`.
3. **CI matrix grows.** Default builds must still pass (they will, since `telemetry` stays out of `default`). A new `--features telemetry` lane is required to keep the OTEL path green; that lane is added by task [`12` — CI](./12-ci.md), not here. If task 12 lags, regressions in the OTEL path could go unnoticed.
4. **Three forwarding sites must stay in sync.** If a future PR adds a fourth crate that owns its own `telemetry = []` (e.g. a new pipeline crate), the maintainer must remember to forward it from `cognee-lib/telemetry`. **Mitigation:** add a brief HOWTO line in `cognee-lib/Cargo.toml`'s comment block above the feature definition pointing at this sub-doc.
5. **`cognee-http-server` cannot forward through `cognee-lib`.** Because `cognee-lib`'s `server` feature pulls in `cognee-http-server` (creating a cycle if reversed), the HTTP server lists `cognee-observability` as its own optional dep. This means there are two source-of-truth feature definitions for the OTEL forwarding (in `cognee-lib` and `cognee-http-server`). They must stay textually aligned. A clippy lint isn't available; rely on the verification matrix above.

## Cross-cutting note

The existing `tracing::info!(target: "cognee.telemetry", ...)` and `tracing::event!(target: "cognee.telemetry", ...)` call sites — e.g. [`crates/lib/src/api/forget.rs:103-115`](../../../crates/lib/src/api/forget.rs#L103) and the four sites in [`crates/core/src/pipeline.rs`](../../../crates/core/src/pipeline.rs) at lines 970/993/1039/1068 — keep working unmodified. The `#[cfg(feature = "telemetry")]` guard on each of those sites resolves against the same workspace-level feature, regardless of whether the activation arrived via:

- `cargo build -p cognee-lib --features telemetry` (forwards to `cognee-core/telemetry`), or
- `cargo build -p cognee-core --features telemetry` (direct), or
- `cargo build -p cognee-http-server --features telemetry` (forwards independently).

In all three cases the same `cfg`-gated code paths emit the same events; the OTEL subscriber installed by `cognee-observability::init_telemetry` (task [`05`](./05-cognee-lib-public-api.md)) will pick them up.

## References

- Parent doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md) — see the "Design decisions (locked)" table for decisions 1, 6, 7, 8, 10.
- Sibling: [`02` — bootstrap `cognee-observability`](./02-cognee-observability-crate.md) (must land first).
- Sibling: [`05` — `init_telemetry` public API & re-exports](./05-cognee-lib-public-api.md).
- Sibling: [`06` — CLI subscriber refactor](./06-cli-subscriber-refactor.md).
- Sibling: [`07` — HTTP-server subscriber refactor](./07-http-server-subscriber-refactor.md).
- Sibling: [`12` — CI lane for `--features telemetry`](./12-ci.md).
- Code anchors:
  - [`crates/lib/Cargo.toml#L41`](../../../crates/lib/Cargo.toml#L41) (existing `telemetry = []`)
  - [`crates/lib/Cargo.toml#L7`](../../../crates/lib/Cargo.toml#L7) (default features — must NOT change)
  - [`crates/lib/Cargo.toml#L57`](../../../crates/lib/Cargo.toml#L57) (`android-default` — must NOT change)
  - [`crates/cli/Cargo.toml#L12`](../../../crates/cli/Cargo.toml#L12) (default features — must NOT change)
  - [`crates/http-server/Cargo.toml#L16`](../../../crates/http-server/Cargo.toml#L16) (default features — must NOT change)
  - [`crates/core/Cargo.toml#L7`](../../../crates/core/Cargo.toml#L7) (existing `cognee-core/telemetry`)
  - Existing event sites: [`crates/lib/src/api/forget.rs:105`](../../../crates/lib/src/api/forget.rs#L105), [`crates/core/src/pipeline.rs:970`](../../../crates/core/src/pipeline.rs#L970), [`:993`](../../../crates/core/src/pipeline.rs#L993), [`:1039`](../../../crates/core/src/pipeline.rs#L1039), [`:1068`](../../../crates/core/src/pipeline.rs#L1068).
