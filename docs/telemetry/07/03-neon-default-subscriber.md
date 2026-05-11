# Task 07-03 — Neon default stderr subscriber on module init

**Status**: implemented in commit 422d874.
**Owner**: _unassigned_
**Depends on**: [Task 07-01 — Workspace deps](01-workspace-deps.md).
**Blocks**:
- [Task 07-05 — Binding OTLP setup](05-binding-otlp-setup.md) (the OTLP layer composes on top of whatever subscriber is already installed).
- [Task 07-07 — Tests](07-tests.md) (`js/__tests__/default_subscriber.test.ts` exercises this).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 1 (hybrid auto-init, Option A for JS), 7 (JS callback bridge deferred).

---

## 1. Goal

Install a `tracing-subscriber::fmt` layer writing to stderr the first
time the Neon `cdylib` is loaded by Node. Concretely:

1. From `#[neon::main] fn main` (in
   [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs)),
   call a new `default_subscriber::install()` helper **before** any
   `cx.export_function` calls.
2. The helper:
   - Short-circuits when `COGNEE_BINDING_SUPPRESS_LOGS` is set to a
     non-empty value.
   - Otherwise builds a Registry with `EnvFilter` (RUST_LOG /
     `default_filter()`) and a `tracing_subscriber::fmt::layer()`
     writing to `std::io::stderr()` with `with_ansi(true)`.
   - Calls `.try_init()`; swallows `Err` (another subscriber
     already installed wins — matches PyO3 semantics from
     07-02).
3. Idempotent via `std::sync::Once`.

Behaviourally: a Node host that runs `require("cognee-neon")` sees
the same `info,ort=warn,reqwest=warn,…` baseline as the CLI binary,
written to `process.stderr`. `setupLogging()` (gap 06) remains
separately callable and adds the rotating file appender on top.

## 2. Rationale

- Decision 1 chose Option A from the parent doc: an env-gated stderr
  fmt subscriber. No bridging into a JS-side logger — Node hosts
  routinely capture stderr (`pino`, `winston`, Docker, journald all
  do) and the alternative (a `Channel`-based callback) was deferred
  (decision 7).
- Decision 7 explicitly defers the JS callback bridge. The default
  install must therefore stand alone.
- Matching `cognee-cli`'s baseline filter (and `cognee_logging::default_filter()`)
  keeps the noise level consistent with the binary the Node host's
  developer probably already used.

## 3. Pre-conditions

- Task 07-01 committed; `tracing-subscriber` is a direct dep of
  `cognee-neon` (verify with `grep tracing-subscriber js/cognee-neon/Cargo.toml`).
- [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs)
  currently registers `setupLogging` from gap 06 task 08 — line 37.
  The new install call sits *before* it.

## 4. Step-by-step

### 4.1 Create `js/cognee-neon/src/default_subscriber.rs`

```rust
//! Default stderr `tracing` subscriber for the Neon binding (gap 07).
//!
//! Installed automatically when the `cdylib` is first loaded by
//! Node, before any exported function is registered. Honours
//! `COGNEE_BINDING_SUPPRESS_LOGS=<any non-empty>` as opt-out.
//!
//! Composes with the explicit `setupLogging()` (gap 06) which adds
//! the rotating file appender. The default subscriber is the
//! "events are never silently dropped" baseline.

use std::sync::Once;

use tracing_subscriber::{EnvFilter, fmt};

static INIT: Once = Once::new();

pub(crate) fn install() {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        // Mirror cognee-logging's default filter so Node hosts see
        // the same noise baseline as the CLI binary. We don't link
        // cognee-logging::default_filter() here because doing so
        // would force cognee-logging into the Neon dep graph; the
        // string literal is short and stable.
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(
                "info,\
                 ort=warn,\
                 reqwest=warn,\
                 hyper=warn,\
                 h2=warn,\
                 rustls=warn,\
                 sqlx=warn,\
                 sea_orm=warn,\
                 sea_orm_migration=warn,\
                 tower_http=warn,\
                 qdrant_segment=warn,\
                 qdrant_shard=warn"
            ));

        let _ = fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .try_init();
    });
}
```

**Note on filter duplication:** the literal above is intentionally
identical to `cognee_logging::default_filter()`
([`crates/logging/src/init.rs`](../../../crates/logging/src/init.rs)).
The Neon binding already depends on `cognee-logging` (gap 06 task
08), so the implementor MAY choose to call
`cognee_logging::default_filter()` directly instead of duplicating
the literal. The duplication shown above is a fallback if the
implementor judges the dep weight worth saving. Either choice is
acceptable; document the decision in the commit body.

### 4.2 Wire into `#[neon::main]`

Edit [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs):

```rust
mod default_subscriber;
// ... existing module declarations ...

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    // gap 07 decision 1: install the default stderr subscriber
    // before any function is registered so events emitted during
    // export setup are captured.
    default_subscriber::install();

    // Runtime
    cx.export_function("init", runtime::init)?;
    // ... existing exports ...
}
```

### 4.3 TypeScript-side env-var documentation

Add the env var to the TypeScript declarations / README. The TS
facade lives in [`js/src/index.ts`](../../../js/src/index.ts) (or
`js/lib/index.ts` — verify before editing). Add to the file's
JSDoc-style header:

```typescript
/**
 * Environment variables consumed by the Rust core on import:
 *
 *   COGNEE_BINDING_SUPPRESS_LOGS=1  — suppress the default
 *     tracing-subscriber stderr install. Set before `require`ing
 *     this module if your host owns its logger.
 *
 * After import, call `setupLogging()` to add file logging,
 * `setupTelemetry()` to add OTLP export, and
 * `setupTelemetryAnalytics()` to enable product-analytics emission.
 */
```

(No code change; just a doc comment.)

## 5. Verification

```bash
# 1. Neon binding compiles.
cd js/cognee-neon && cargo check --all-targets && cd -

# 2. Build the .node addon.
cd js/cognee-neon && npm run build && cd -
# (or `cargo build --release` then symlink; check existing build
# script in js/cognee-neon/package.json)

# 3. Smoke test (manual; full test in 07-07).
node - <<'NODE'
process.env.RUST_LOG = 'debug';
const cog = require('./js');
// any cog.* call should emit a debug line to stderr.
NODE

# 4. Suppression works.
COGNEE_BINDING_SUPPRESS_LOGS=1 node - <<'NODE'
const cog = require('./js');
// no stderr output expected.
NODE

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`js/cognee-neon/src/lib.rs`](../../../js/cognee-neon/src/lib.rs) —
  module declaration + one-line `install()` call.
- `js/cognee-neon/src/default_subscriber.rs` — NEW.
- [`js/src/index.ts`](../../../js/src/index.ts) (or
  `js/lib/index.ts` — verify) — JSDoc env-var documentation.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Some Node hosts (e.g. PM2 with stdout-only capture) miss stderr | Low | Document; users who care set `COGNEE_BINDING_SUPPRESS_LOGS=1` and configure their own subscriber. JS callback bridge (decision 7) is the long-term fix. |
| Filter literal drifts from `cognee_logging::default_filter()` over time | Medium — string duplication is the price of decoupling. | Sub-agent A's task review must check that the two strings stay byte-identical when 07-03 is touched; alternatively the implementor picks the "import from `cognee_logging`" variant. |
| `tracing-subscriber` version mismatch between Neon binding (its own Cargo.lock) and the workspace | Low — pinned to `0.3` in both manifests after 07-01. | `cargo tree` check in verification step. |
| `try_init` succeeds in the binding then `setupLogging()` is called and silently no-ops — host expected file logging | Medium — `setupLogging()` already documents idempotence (gap 06). | Document in 07-08 README: order matters — call `setupLogging()` first if you want file output, otherwise the default subscriber claims the global slot. **OR** revise gap-06 task 08's `setupLogging` to *replace* a default install — but that's a gap-06 change, out of scope here. Recommended path: 07-08 docs only. |

## 8. Out of scope

- The JS callback bridge (decision 7 deferred).
- Migrating the filter literal to `cognee_logging::default_filter()`
  if the implementor judges the duplication acceptable.
- Capturing stderr into a JS-side ring buffer (gap 04 / decision 13
  of gap 06 covers the Rust-side ring buffer; the JS side is a
  future gap).
- Touching the TypeScript build pipeline. The `default_subscriber`
  module is internal Rust; no `.d.ts` changes needed.
