use std::process::ExitCode as StdExitCode;
use std::sync::Arc;

use clap::Parser;
use cognee::{ComponentManager, ConfigManager};
use cognee_cli::cli::{Cli, Commands};
use cognee_cli::commands;
use cognee_cli::config_store::{Settings, load_settings};
use cognee_cli::error::{CliError, ExitCode};
#[cfg(feature = "bench")]
use commands::bench;
#[cfg(feature = "visualization")]
use commands::visualize;
use commands::{
    add, add_and_cognify, cognify, config, delete, forget, improve, memify, recall, remember,
    run_sequence, search,
};
use tracing::error;

/// How long to wait for the relational pool to close on exit. See
/// [`close_components`].
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Close the relational connection pool before the process exits.
///
/// Exiting without closing leaves a SQLite database's `-wal`/`-shm` sidecars
/// behind: dropping the pool only flags it closed and lets its connections tear
/// down concurrently, and SQLite unlinks the sidecars only when the *last*
/// connection closes (issue #132). The next `cognee` invocation recovers them, so
/// nothing is corrupt, but the files linger next to the database and a caller
/// that wraps the CLI in a temporary directory cannot clean up after it.
///
/// Each command owns (and drops) its own runtime, so this builds a small
/// current-thread one purely to drive the close — a pool drain, not I/O.
///
/// Bounded by [`CLOSE_TIMEOUT`] because the close waits for connections to come
/// back: if a command's runtime was dropped while one was still checked out, its
/// pool permit is never released and the wait would never finish. Timing out
/// costs only the sidecars we were trying to remove, whereas hanging would cost
/// the caller their exit code, so this is deliberately best-effort.
fn close_components(cm: &ComponentManager) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            tracing::debug!(%error, "could not build a runtime to close the database");
            return;
        }
    };
    rt.block_on(async {
        if tokio::time::timeout(CLOSE_TIMEOUT, cm.close())
            .await
            .is_err()
        {
            tracing::debug!(
                "closing the relational database timed out after {CLOSE_TIMEOUT:?}; \
                 its WAL sidecars may be left for the next run to recover"
            );
        }
    });
}

fn run(settings: Settings) -> Result<(), CliError> {
    let cli = Cli::parse();

    // Priority: defaults < JSON config < env vars (settings already overlaid in main).
    let config = ConfigManager::new(settings);
    let cm = Arc::new(ComponentManager::new(config));

    let result = dispatch(cli.command, &cm);

    // Release the database before returning, whether the command succeeded or
    // not — a failed run has usually opened it too.
    close_components(&cm);

    result
}

fn dispatch(command: Commands, cm: &Arc<ComponentManager>) -> Result<(), CliError> {
    match command {
        Commands::Add(args) => add::run(args, Arc::clone(cm)),
        Commands::Cognify(args) => cognify::run(args, Arc::clone(cm)),
        Commands::AddAndCognify(args) => add_and_cognify::run(args, Arc::clone(cm)),
        Commands::Memify(args) => memify::run(args, Arc::clone(cm)),
        Commands::Search(args) => search::run(args, Arc::clone(cm)),
        Commands::Remember(args) => remember::run(args, Arc::clone(cm)),
        Commands::Recall(args) => recall::run(args, Arc::clone(cm)),
        Commands::Forget(args) => forget::run(args, Arc::clone(cm)),
        Commands::Improve(args) => improve::run(args, Arc::clone(cm)),
        Commands::Delete(args) => delete::run(args, Arc::clone(cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(args) => run_sequence::run(args, Arc::clone(cm)),
        #[cfg(feature = "visualization")]
        Commands::Visualize(args) => visualize::run(args, Arc::clone(cm)),
        #[cfg(feature = "bench")]
        Commands::Bench(args) => bench::run(args, Arc::clone(cm)),
    }
}

fn main() -> StdExitCode {
    // Settings load runs before subscriber install so init_telemetry sees the
    // correct configuration on the first span (decision 11). No subscriber is
    // installed yet, so failures must go to stderr directly.
    let settings = match load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("Error: {error}");
            return StdExitCode::from(error.exit_code() as u8);
        }
    };

    // Decision 6 (default filter via init_logging) + decision 8
    // (env-var-only — no new CLI flags). The env-var surface lives in
    // `cognee-logging::LoggingConfig`; if parsing fails we keep startup
    // alive by falling back to the documented defaults instead of
    // aborting before any log line could surface the problem.
    let logging_cfg = match cognee_logging::LoggingConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("warning: invalid logging env var: {err}; falling back to defaults");
            cognee_logging::LoggingConfig::defaults()
        }
    };

    // Extra tracing layers installed alongside the stdout logger. The
    // `profiling` build adds an offline per-stage span-timing layer that the
    // `bench` subcommand arms per phase (see `commands::bench_telemetry`).
    #[cfg(feature = "profiling")]
    let profiling_layers = std::iter::once(commands::bench_telemetry::layer());
    #[cfg(not(feature = "profiling"))]
    let profiling_layers = std::iter::empty::<cognee_logging::BoxedLayer>();

    #[cfg(not(feature = "telemetry"))]
    let _log_guards = cognee_logging::init_logging(logging_cfg, profiling_layers);

    #[cfg(feature = "telemetry")]
    let (_log_guards, _telemetry_guard) = {
        use cognee::telemetry::{TelemetryGuard, init_telemetry};
        use tracing_subscriber::{Layer, Registry, layer::Identity};

        // Telemetry init failure must not abort the user's CLI command —
        // fall back to a noop layer + noop guard.
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

        let extra = std::iter::once(telemetry_layer).chain(profiling_layers);
        let guards = cognee_logging::init_logging(logging_cfg, extra);
        (guards, telemetry_guard)
    };

    // Returning ExitCode (rather than calling process::exit) lets locals —
    // including _telemetry_guard and _log_guards — drop, flushing the
    // final span batch and any buffered log lines.
    match run(settings) {
        Ok(()) => StdExitCode::from(ExitCode::Success as u8),
        Err(error) => {
            error!("Error: {error}");
            StdExitCode::from(error.exit_code() as u8)
        }
    }
}
