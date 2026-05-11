# Task 06-06 — Refactor `crates/cli/src/main.rs` to use `init_logging`

**Status**: implemented in commit 0c22fc1 (note: also added `LoggingConfig::defaults()` to `crates/logging/src/config.rs` as a minor cross-task fixup so the env-parse-failure fallback in `main()` is copy-free)
**Owner**: _unassigned_
**Depends on**: [Task 06-05 — init_logging](05-init-logging.md).
**Blocks**:
- [Task 06-10 — Tests](10-tests.md) (CLI E2E test depends on the new subscriber being live).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 6 (default filter), 8 (env-var-only — no new CLI flags).

---

## 1. Goal

Replace both subscriber-install branches in
[`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs) (lines
~65–103) with a single call to
`cognee_logging::init_logging(LoggingConfig::from_env()?, extra_layers)`.
Hold the returned `LogGuards` in `main()`'s scope so file-writer
worker threads flush at exit. The `#[cfg(feature = "telemetry")]`
branch keeps composing the OTEL layer — it just provides it via
`extra_layers` now instead of building its own subscriber.

## 2. Rationale

- Decision 8 locked env-var-only — no new CLI flags.
- The current setup at
  [`crates/cli/src/main.rs:65-103`](../../../crates/cli/src/main.rs#L65-L103)
  duplicates the `EnvFilter` directive (`"info,ort=warn"`) and the
  `fmt::layer()` builder in two cfg branches. Centralising via
  `init_logging` removes the duplication and gives every binary the
  same file-logging behaviour.
- Holding `LogGuards` in `main`'s scope is the documented
  `tracing-appender` pattern. The existing `_telemetry_guard` is
  already there for the OTEL exporter — we just add a sibling
  `_log_guards`.

## 3. Pre-conditions

- Task 06-05 committed; `cognee-logging` exports `init_logging`,
  `LoggingConfig`, `LogGuards`, `BoxedLayer`.
- `crates/cli/src/main.rs` matches the structure in §"Current state"
  below. If line numbers have shifted, sub-agent A updates this
  sub-doc before sub-agent B implements.

### Current state — `crates/cli/src/main.rs` (lines 22–24, 65–103)

```rust
use tracing::error;
use tracing_subscriber::EnvFilter;
#[cfg(feature = "telemetry")]
use tracing_subscriber::{Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};
```

```rust
let env_filter =
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));

#[cfg(not(feature = "telemetry"))]
{
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();
}

#[cfg(feature = "telemetry")]
let _telemetry_guard = {
    use cognee_lib::telemetry::{TelemetryGuard, init_telemetry};
    use tracing_subscriber::{Layer, layer::Identity};

    let (telemetry_layer, telemetry_guard): (
        Box<dyn Layer<Registry> + Send + Sync>,
        TelemetryGuard,
    ) = match init_telemetry::<Registry>(&settings) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("warning: failed to initialise OTEL telemetry: {err}");
            (Box::new(Identity::new()), TelemetryGuard::noop())
        }
    };

    let fmt_layer = fmt::layer().with_target(false);

    let _ = Registry::default()
        .with(telemetry_layer)
        .with(env_filter)
        .with(fmt_layer)
        .try_init();

    telemetry_guard
};
```

## 4. Step-by-step

### 4.1 Update `crates/cli/Cargo.toml`

Add `cognee-logging = { path = "../logging" }` under
`[dependencies]`. Alphabetical near the other `cognee-*` deps.

### 4.2 Replace the subscriber-install block

In [`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs) `main()`:

```rust
fn main() -> StdExitCode {
    let settings = match load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("Error: {error}");
            return StdExitCode::from(error.exit_code() as u8);
        }
    };

    // === REPLACE everything from `let env_filter = ...` through the
    // end of the `#[cfg(feature = "telemetry")]` block with: ===

    let logging_cfg = match cognee_logging::LoggingConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("warning: invalid logging env var: {err}; falling back to defaults");
            cognee_logging::LoggingConfig::defaults()  // implementor: add a defaults() ctor in 06-02 if not already present
        }
    };

    #[cfg(not(feature = "telemetry"))]
    let _log_guards = cognee_logging::init_logging(
        logging_cfg,
        std::iter::empty::<cognee_logging::BoxedLayer>(),
    );

    #[cfg(feature = "telemetry")]
    let (_log_guards, _telemetry_guard) = {
        use cognee_lib::telemetry::{TelemetryGuard, init_telemetry};
        use tracing_subscriber::{Layer, Registry, layer::Identity};

        let (telemetry_layer, telemetry_guard): (
            Box<dyn Layer<Registry> + Send + Sync>,
            TelemetryGuard,
        ) = match init_telemetry::<Registry>(&settings) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("warning: failed to initialise OTEL telemetry: {err}");
                (Box::new(Identity::new()), TelemetryGuard::noop())
            }
        };

        let guards = cognee_logging::init_logging(
            logging_cfg,
            std::iter::once(telemetry_layer),
        );
        (guards, telemetry_guard)
    };

    // === END replacement ===

    match run(settings) {
        Ok(()) => StdExitCode::from(ExitCode::Success as u8),
        Err(error) => {
            error!("Error: {error}");
            StdExitCode::from(error.exit_code() as u8)
        }
    }
}
```

### 4.3 Remove the now-dead imports

In the file's top imports (lines 22–24):

```rust
// Remove:
use tracing_subscriber::EnvFilter;
#[cfg(feature = "telemetry")]
use tracing_subscriber::{Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};
```

Keep `use tracing::error;` (used by the bottom error handler).

The `#[cfg(feature = "telemetry")]` block now imports
`tracing_subscriber::{Layer, Registry, layer::Identity}` *inside* the
block — that's the only place it's still needed.

### 4.4 Add `LoggingConfig::defaults()` if absent

Implementor cross-check: if `LoggingConfig::defaults()` was not
defined in 06-02, add a simple `pub fn defaults() -> Self` that
returns the same values `from_env()` would produce on an empty env.
This makes the error fallback at step 4.2 cheap and copy-free.

## 5. Verification

```bash
# 1. CLI binary compiles in both feature configurations.
cargo check -p cognee-cli --all-targets
cargo check -p cognee-cli --all-targets --no-default-features
cargo check -p cognee-cli --all-targets --features telemetry

# 2. CLI tests (none affected directly here; CLI E2E test lands in 06-10).
cargo test -p cognee-cli

# 3. Smoke: run a CLI command with COGNEE_LOGS_DIR pointing at /tmp/<X>
#    and assert a *.log file appears.
COGNEE_LOGS_DIR=$(mktemp -d) cargo run -p cognee-cli -- --help
ls "$COGNEE_LOGS_DIR"/*.log

# 4. Clippy.
cargo clippy -p cognee-cli --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml) — add
  `cognee-logging` dep.
- [`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs) —
  replace the subscriber-install block; remove dead imports.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `cognee-cli` regresses when `--features telemetry` is on because the OTEL layer composes differently | Medium | The `extra_layers` shape mirrors today's `Registry::default().with(telemetry_layer).with(env_filter).with(fmt_layer)`. Test step 1 covers both feature configs. |
| Existing scripts depending on the literal `"info,ort=warn"` filter string see different default behavior | Low — no script greps for that string, but `RUST_LOG` users get the broader default filter. Documented in 06-11. | The decision-6 baseline is strictly a superset of `info,ort=warn`. Users who set `RUST_LOG` are unaffected. |
| `_log_guards` getting dropped earlier than `_telemetry_guard` flushes log lines that mention OTEL errors before they reach the file | Very low | Both guards live to end-of-`main`. Drop order is reverse-declaration; `_telemetry_guard` drops first (declared second-to-last in the tuple destructure), flushing OTEL spans; then `_log_guards` drops, flushing pending file writes. |

## 8. Out of scope

- Adding `--log-level`, `--log-file`, `--log-format` CLI flags
  (decision 8 locked env-var-only).
- Removing the `--features telemetry` cfg branch. Gap 06 keeps the
  feature gate intact.
- Refactoring the `error!("Error: {error}")` exit path.
