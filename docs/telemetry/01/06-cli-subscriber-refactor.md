# Task 01-06: Refactor the CLI subscriber to compose the OTEL bridge layer

## Status

Not started.

## Owner / dependencies

- **Depends on**:
  - [Task 01-02 — `cognee-observability` crate scaffold](02-cognee-observability-crate.md)
    (the new crate that hosts `init_telemetry` and `TelemetryGuard`).
  - [Task 01-04 — `init_telemetry` implementation](04-init-telemetry.md)
    (provides the function this task calls; defines the layer/guard pair
    returned to the subscriber).
  - [Task 01-05 — `cognee-lib` re-exports](05-cognee-lib-reexports.md)
    (this task imports the symbols via
    `cognee_lib::observability::{init_telemetry, TelemetryGuard}` so the
    CLI does not depend on `cognee-observability` directly).
  - [Task 01-03 — `telemetry` cargo feature wiring](03-telemetry-feature-wiring.md)
    (specifically the `telemetry = ["cognee-lib/telemetry"]` forwarding
    feature on `crates/cli/Cargo.toml`, which is **off** in
    `default` per [decision 1](../01-otel-otlp-export.md#design-decisions-locked) and
    [decision 7](../01-otel-otlp-export.md#design-decisions-locked)).
- **Blocks**:
  - [Task 01-09 — Unit tests](09-unit-tests.md)
    (the CLI smoke tests reference the post-refactor structure of
    `main()`).
- **Siblings (parallel work)**:
  - [Task 01-07 — HTTP server subscriber refactor](07-http-server-subscriber-refactor.md)
    performs the analogous change in `crates/http-server/src/main.rs`.
- **Owner**: TBD.

## Rationale

Three independent changes are required, all in
[`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs):

1. **Move `load_settings()` from `run()` into `main()`, before subscriber
   init.** This is the binding resolution recorded in
   [decision 11](../01-otel-otlp-export.md#design-decisions-locked) (option
   `(a)`). Today the subscriber is installed first using only the
   `RUST_LOG` env var, and `load_settings()` runs *after*, inside
   `run()` (line 28 of the current file). That ordering means the
   subscriber cannot consult `Settings.cognee_tracing_enabled`,
   `Settings.otel_exporter_otlp_endpoint`, or
   `Settings.otel_exporter_otlp_headers` when it decides whether to
   compose the OTEL layer — so the very first spans of `main()` would
   bypass OTEL even if the user has configured it. Loading settings
   first lets `init_telemetry(&settings)` see the correct configuration
   on the first span.

2. **Compose the OTEL bridge layer with the existing `fmt` layer.** The
   current implementation uses the `tracing_subscriber::fmt()`
   builder shortcut (lines 55–58), which installs a single-layer
   subscriber. Once we need two layers (`fmt` + OTEL bridge) we must
   switch to the explicit `Registry::default().with(filter).with(fmt).with(otel)`
   form, matching the shape used by the HTTP server today and described
   in [§ "Subscriber composition"](../01-otel-otlp-export.md#subscriber-composition).
   The CLI does **not** add a `SpanBufferLayer` — that layer only lives
   on the HTTP server (per the project guide and
   [§ "Existing `SpanBufferLayer`"](../01-otel-otlp-export.md#existing-spanbufferlayer)).

3. **Hold `TelemetryGuard` for the lifetime of `main()`.** The guard
   returned by `init_telemetry` carries the
   `Drop`-based flush-and-shutdown behaviour described in
   [§ "Shutdown handling"](../01-otel-otlp-export.md#shutdown-handling).
   Dropping it before `main()` returns would cause spans to be lost on
   process exit. Per
   [decision 10](../01-otel-otlp-export.md#design-decisions-locked) the
   type is named `TelemetryGuard` (not `OtelGuard`).

## Pre-conditions

- Tasks [01-02](02-cognee-observability-crate.md),
  [01-03](03-telemetry-feature-wiring.md),
  [01-04](04-init-telemetry.md) and
  [01-05](05-cognee-lib-reexports.md) are merged.
- `cognee_lib::observability::init_telemetry` exists with the signature
  agreed in task 01-04, returning a `(Layer, TelemetryGuard)` pair (or
  `Result<...>` — see [§ "init_telemetry error handling"](#init_telemetry-error-handling)
  below). Both the `cfg(feature = "telemetry")` and
  `cfg(not(feature = "telemetry"))` paths compile per
  [decision 1](../01-otel-otlp-export.md#design-decisions-locked).

## Step-by-step

### 1. Refactor `run()` to take `Settings` as a parameter

Change the signature of `run()` so that settings are passed in rather
than loaded inside the function. The existing function constructs
`ConfigManager::new(settings)` directly, so passing the owned `Settings`
through is a one-line change.

**Before** (current `run()` signature, lines 23–29):

```rust
fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let settings = load_settings()?;
    let config = ConfigManager::new(settings);
    let cm = Arc::new(ComponentManager::new(config));
    ...
}
```

**After**:

```rust
fn run(settings: Settings) -> Result<(), CliError> {
    let cli = Cli::parse();

    let config = ConfigManager::new(settings);
    let cm = Arc::new(ComponentManager::new(config));
    ...
}
```

Notes:

- `Settings` is re-exported by `config_store` today
  ([`crates/cli/src/config_store.rs:2`](../../../crates/cli/src/config_store.rs#L2):
  `pub use cognee_lib::Settings;`), so the new import in `main.rs` is
  trivial: `use config_store::{Settings, load_settings};`.
- `Cli::parse()` stays inside `run()`. `clap` already exits with a
  formatted error and a non-zero status on parse failure via its
  built-in handling, so it is fine that the subscriber is not yet
  installed by the time `Cli::parse()` would print a usage message —
  parse errors go directly to stderr and do not flow through
  `tracing`.
- The return type stays `Result<(), CliError>` — `From<ComponentError>
  for CliError` ([`crates/cli/src/error.rs:29`](../../../crates/cli/src/error.rs#L29))
  handles the `?` propagation.

### 2. Refactor `main()` to load settings, then build the subscriber

Reorder the body of `main()` so that the new sequence is:

1. Load settings (no tracing macros yet — failures must use `eprintln!`
   and `process::exit`).
2. Build the subscriber (filter + fmt layer + telemetry layer).
3. Bind the `TelemetryGuard` to a long-lived `let _guard = ...;`.
4. Call `run(settings)`.
5. Translate `Result` into an `ExitCode`. **See
   [§ "Drop order vs `process::exit`"](#drop-order-vs-processexit) below
   — naïvely calling `std::process::exit` after a `match` skips the
   guard's `Drop`, dropping the last span batch. This sub-doc
   recommends switching `main` to return `std::process::ExitCode`.**

#### `init_telemetry` error handling

`init_telemetry(&settings)` may legitimately fail (e.g. the OTLP
endpoint URL is malformed, or the gRPC channel cannot be constructed
synchronously). Per the design intent of "telemetry failure should not
break the user's CLI command", **a failure here must not abort the
process**. The recommended pattern:

```rust
let (telemetry_layer, telemetry_guard) =
    match cognee_lib::observability::init_telemetry(&settings) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("warning: failed to initialise OTEL telemetry: {err}");
            cognee_lib::observability::noop_telemetry()
        }
    };
```

Where `noop_telemetry()` returns the same `(Layer, TelemetryGuard)`
shape but with a `Layer::default()` (i.e. `tracing_subscriber::layer::Identity`)
and a noop guard. Task [01-04](04-init-telemetry.md) is responsible for
exposing this helper alongside `init_telemetry`. If task 01-04 instead
chose to make `init_telemetry` infallible (returning the noop pair on
failure internally and emitting the warning itself), this match
collapses to a single line:

```rust
let (telemetry_layer, telemetry_guard) =
    cognee_lib::observability::init_telemetry(&settings);
```

Either shape is acceptable; this task should match whichever signature
01-04 settled on. The new `main.rs` shown below uses the `Result` form
because it is the more conservative assumption.

#### Settings load failure path

`load_settings()` returns `Result<Settings, CliError>`
([`crates/cli/src/config_store.rs:42`](../../../crates/cli/src/config_store.rs#L42)).
Today its failure modes are: cannot resolve the user config dir, cannot
read the JSON file, or cannot parse it (lines 31–47, 57–60). Because
the subscriber is not yet installed when this runs in the new ordering,
we cannot use `tracing::error!` — falling back to the same pattern the
runtime error path uses today, but via `eprintln!`:

```rust
let settings = match load_settings() {
    Ok(settings) => settings,
    Err(error) => {
        eprintln!("Error: {error}");
        return error.exit_code();
    }
};
```

(Returning the `ExitCode` from `main` rather than calling
`process::exit` ensures any later destructors run — although none exist
on the failure path, keeping the pattern uniform with the success path
avoids a footgun.)

### 3. Compose the subscriber explicitly

Switch from the `tracing_subscriber::fmt()` shortcut (which installs a
single-layer subscriber) to an explicit `Registry`-based composition:

```rust
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt, Registry};

let env_filter =
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
let fmt_layer = fmt::layer().with_target(false);

let subscriber = Registry::default()
    .with(env_filter)
    .with(fmt_layer)
    .with(telemetry_layer);

let _ = subscriber.try_init();
```

`try_init()` (rather than `init()`) preserves the existing CLI's
resilience: if a host process already installed a global subscriber
(e.g. an integration-test harness), we silently skip re-installation
instead of panicking. This matches today's `let _ = tracing_subscriber::fmt()...try_init();`
pattern at line 55.

### 4. Hold the guard for the lifetime of `main`

Bind the guard to an underscore-prefixed local that lives until the end
of `main()`:

```rust
let _telemetry_guard = telemetry_guard;
```

Or — if you find that more readable — leave the destructured binding
from step 2 in place; what matters is that the variable is *not*
shadowed and *not* moved into a shorter-lived scope.

### 5. Translate the `Result` into an `ExitCode`

See the next subsection. Recommendation: change the signature of
`main()` to `fn main() -> ExitCode`, where `ExitCode` is
`std::process::ExitCode`. This lets `main` return normally so the
guard's `Drop` runs. The existing CLI `ExitCode` enum
([`crates/cli/src/error.rs:5`](../../../crates/cli/src/error.rs#L5)) is
internal — it is `#[repr(u8)]` and converted via `as i32` to feed
`std::process::exit`. Wrap with `From<ExitCode> for std::process::ExitCode`
or call `ExitCode::from(value as u8)` at the boundary.

### Drop order vs `process::exit`

`std::process::exit` **does not run destructors**. The current `main`
(lines 60–66):

```rust
match run() {
    Ok(()) => std::process::exit(ExitCode::Success as i32),
    Err(error) => {
        error!("Error: {error}");
        std::process::exit(error.exit_code() as i32);
    }
}
```

is fine today because the `tracing_subscriber::fmt` global subscriber
holds no buffer that needs flushing. The OTEL bridge **does** — the
`BatchSpanProcessor` queues spans on a background thread and only
flushes them on `provider.shutdown()` (or `force_flush()`), both of
which the `TelemetryGuard::Drop` impl calls.

Two options:

| Option | Description | Recommended? |
|---|---|---|
| **(A) `main` returns `std::process::ExitCode`** | Convert the existing internal `ExitCode` enum to a `std::process::ExitCode` and return it from `main`. Destructors of all locals (including `_telemetry_guard`) run before the runtime hands the code to the OS. | **Yes** — this is the idiomatic Rust pattern since 1.61 and the change is local to `main`. |
| **(B) Explicit `drop(_telemetry_guard)` before each `process::exit` call** | Keep `main()` returning `()` and the existing `process::exit` calls; manually `drop(_telemetry_guard)` immediately before each `exit()` call. | Only if option (A) turns out to be incompatible with some other CLI behaviour (e.g. a panic handler that calls `exit` itself). Brittle: any new exit path must remember to drop. |

This sub-doc recommends option **(A)**. The full code shown below
implements it.

## Resulting code

```rust
mod cli;
mod commands;
mod config_store;
mod error;

use std::process::ExitCode as StdExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use cognee_lib::{ComponentManager, ConfigManager};
#[cfg(feature = "cloud")]
use commands::disconnect;
#[cfg(feature = "cloud")]
use commands::serve;
#[cfg(feature = "visualization")]
use commands::visualize;
use commands::{add, add_and_cognify, cognify, config, delete, memify, run_sequence, search};
use config_store::{Settings, load_settings};
use error::{CliError, ExitCode};
use tracing::error;
use tracing_subscriber::{
    EnvFilter, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

fn run(settings: Settings) -> Result<(), CliError> {
    let cli = Cli::parse();

    // Priority: defaults < JSON config < env vars (settings already overlaid in main).
    let config = ConfigManager::new(settings);
    let cm = Arc::new(ComponentManager::new(config));

    match cli.command {
        Commands::Add(args) => add::run(args, Arc::clone(&cm)),
        Commands::Cognify(args) => cognify::run(args, Arc::clone(&cm)),
        Commands::AddAndCognify(args) => add_and_cognify::run(args, Arc::clone(&cm)),
        Commands::Memify(args) => memify::run(args, Arc::clone(&cm)),
        Commands::Search(args) => search::run(args, Arc::clone(&cm)),
        Commands::Delete(args) => delete::run(args, Arc::clone(&cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(args) => run_sequence::run(args, Arc::clone(&cm)),
        #[cfg(feature = "visualization")]
        Commands::Visualize(args) => visualize::run(args, Arc::clone(&cm)),
        #[cfg(feature = "cloud")]
        Commands::Serve(args) => serve::run(args, Arc::clone(&cm)),
        #[cfg(feature = "cloud")]
        Commands::Disconnect(args) => disconnect::run(args, Arc::clone(&cm)),
    }
}

fn main() -> StdExitCode {
    // Step 1: Load settings before installing the subscriber so the OTEL
    //         layer (built below) sees the right configuration on the
    //         first span. See docs/telemetry/01-otel-otlp-export.md
    //         decision 11.
    //
    //         No tracing subscriber is installed yet, so failures here
    //         must go to stderr directly.
    let settings = match load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("Error: {error}");
            return StdExitCode::from(error.exit_code() as u8);
        }
    };

    // Step 2: Build the OTEL bridge layer + guard. Failure must not
    //         abort the CLI — fall back to a noop pair and continue.
    let (telemetry_layer, _telemetry_guard) =
        match cognee_lib::observability::init_telemetry(&settings) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("warning: failed to initialise OTEL telemetry: {err}");
                cognee_lib::observability::noop_telemetry()
            }
        };

    // Step 3: Compose the subscriber.
    //
    //         Suppress verbose ONNX Runtime graph-optimizer logs (ort
    //         crate) by default. Users can re-enable them with
    //         RUST_LOG="info,ort=info".
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);

    let _ = Registry::default()
        .with(env_filter)
        .with(fmt_layer)
        .with(telemetry_layer)
        .try_init();

    // Step 4: Run the command. The guard is held until `main` returns.
    match run(settings) {
        Ok(()) => StdExitCode::from(ExitCode::Success as u8),
        Err(error) => {
            error!("Error: {error}");
            StdExitCode::from(error.exit_code() as u8)
        }
    }
}
```

Notes on the diff:

- The `let cli = Cli::parse();` call stays inside `run()`. Moving it
  into `main()` is also acceptable, but keeping it where it is
  minimises churn in this PR; task 01-09 may revisit if its tests need
  the parse step lifted.
- `commands::run_sequence::run(args, Arc::clone(&cm))` and the rest of
  the dispatch table are unchanged.
- `tracing::error!` is now safe to use in the success-or-error tail of
  `main` because by the time we reach it, the subscriber has been
  installed.
- `From<ComponentError>` and the `?` operator in command handlers
  continue to work — `run()`'s return type is unchanged.

## Files modified

- `crates/cli/src/main.rs` — only file touched in this task.

No other CLI source file changes; no `Cargo.toml` changes (those live
in [task 01-03](03-telemetry-feature-wiring.md)).

## Verification

1. **Default-features build (telemetry off)** — must succeed; OTEL layer
   is the identity layer and the guard is a noop:

   ```bash
   cargo check -p cognee-cli
   ```

2. **Telemetry-on build** — must succeed:

   ```bash
   cargo check -p cognee-cli --features telemetry
   ```

3. **Compile-both-ways check** for the entire workspace:

   ```bash
   cargo check --all-targets
   cargo check --all-targets --features cognee-cli/telemetry
   ```

4. **Smoke test: unreachable collector must not panic**. With telemetry
   on and an obviously-unreachable endpoint, `--help` should still
   print and the process should exit cleanly:

   ```bash
   OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:1 \
     cargo run -p cognee-cli --features telemetry -- --help
   ```

   - `--help` causes `clap` to exit before `run()` body executes, but
     after the subscriber is installed. Verifies that `init_telemetry`
     does not block on collector connectivity.
   - The eventual `Drop` of `_telemetry_guard` may try to flush; with a
     `BatchSpanProcessor` and the recommended short flush timeout this
     should fail silently and exit within a few seconds.

5. **Smoke test: malformed endpoint must downgrade gracefully**. Ensures
   the `init_telemetry` `Err` path emits a warning to stderr and
   continues:

   ```bash
   OTEL_EXPORTER_OTLP_ENDPOINT='not-a-url' \
     cargo run -p cognee-cli --features telemetry -- --help 2>&1 | \
     grep 'failed to initialise OTEL telemetry'
   ```

6. **Existing CLI E2E tests** must continue to pass:

   ```bash
   cargo test -p cognee-cli
   ```

7. **`scripts/check_all.sh`** must still pass end-to-end (per the
   project rule).

## Risks

- **Settings-load failure produces no log line.** With the new ordering
  the subscriber is not installed when `load_settings()` fails, so we
  fall back to `eprintln!`. This is a behavioural regression: any
  external log-collection that scrapes `tracing` output will not see
  the message. Mitigation: the message remains visible on stderr, and
  in practice the only failure modes are "config dir not resolvable"
  or "config JSON malformed", both of which are user-visible
  immediately.

- **`process::exit` skips destructors.** Documented in
  [§ "Drop order vs `process::exit`"](#drop-order-vs-processexit).
  The recommended fix (returning `std::process::ExitCode` from `main`)
  is incorporated into the resulting code. If the team prefers
  option (B), every new exit path must remember to drop the guard.

- **`try_init()` swallows duplicate-subscriber errors.** Today's CLI
  already uses this pattern (line 55 of the current file), so this is
  not a regression — but it does mean a misconfigured embedder that
  installs its own subscriber will silently lose OTEL bridging from the
  CLI. Mitigation: document this in the user-facing observability
  guide ([task 01-11](11-documentation.md)).

- **Guard `Drop` may block on a slow collector.** If the OTLP collector
  is slow to acknowledge the final flush, `main()` will block on guard
  drop. The flush timeout is bounded by the SDK's batch-processor
  configuration ([task 01-04](04-init-telemetry.md) sets it). This is
  desirable for trace fidelity but might surprise users running short
  CLI commands. Tunable via the standard `OTEL_BSP_*` env vars.

- **`Settings` clone semantics.** `Settings` is `Clone` today (it is a
  plain `serde`-derived struct in
  [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs)). We
  pass it by value into `run()` (a single move), then forward into
  `ConfigManager::new(settings)`. No clone is required. If task 01-04
  needs `&Settings` for `init_telemetry`, the borrow happens *before*
  the move — `init_telemetry(&settings)` then `run(settings)` — which
  is the order in the resulting code above.

- **Guard ordering vs subscriber drop.** `_telemetry_guard` is declared
  *before* `Registry::default().with(...).try_init()`. Local drop
  order is reverse declaration order, so the global subscriber (which
  is `'static` once installed via `try_init` and not dropped at scope
  exit) is irrelevant; the guard drops at `main` return. Verified by
  reading `tracing_subscriber::util::SubscriberInitExt::try_init`
  source — it sets the global default and does not return a drop
  handle.

- **Re-export drift between `cognee-lib` and `cognee-observability`.**
  This task imports symbols from `cognee_lib::observability::*` and
  expects task 01-05 to keep that path stable. If 01-05 chooses a
  different path (e.g. `cognee_lib::telemetry::*`), this task's import
  block must be updated in lockstep.

## References

- Parent gap doc: [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  — especially [§ "Subscriber composition"](../01-otel-otlp-export.md#subscriber-composition),
  [§ "Shutdown handling"](../01-otel-otlp-export.md#shutdown-handling),
  and the [locked design decisions](../01-otel-otlp-export.md#design-decisions-locked).
- Sibling tasks:
  - [01-02 `cognee-observability` crate scaffold](02-cognee-observability-crate.md)
  - [01-03 `telemetry` cargo feature wiring](03-telemetry-feature-wiring.md)
  - [01-04 `init_telemetry` implementation](04-init-telemetry.md)
  - [01-05 `cognee-lib` re-exports](05-cognee-lib-reexports.md)
  - [01-07 HTTP server subscriber refactor](07-http-server-subscriber-refactor.md)
  - [01-09 Unit tests](09-unit-tests.md)
- Source files:
  - [`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs)
  - [`crates/cli/src/config_store.rs`](../../../crates/cli/src/config_store.rs)
  - [`crates/cli/src/error.rs`](../../../crates/cli/src/error.rs)
  - [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml)
- Rust stdlib: [`std::process::ExitCode`](https://doc.rust-lang.org/std/process/struct.ExitCode.html)
  (stable since 1.61) — the mechanism that lets `main` return a code
  while still running destructors.
- `tracing_subscriber` layer composition:
  [`SubscriberExt::with`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.SubscriberExt.html)
  and [`SubscriberInitExt::try_init`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/util/trait.SubscriberInitExt.html#method.try_init).
