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
    add, add_and_cognify, cognify, config, delete, export, forget, improve, memify, recall,
    remember, run_sequence, search,
};
use tracing::error;

fn run(settings: Settings) -> Result<(), CliError> {
    let cli = Cli::parse();

    // Priority: defaults < JSON config < env vars (settings already overlaid in main).
    let config = ConfigManager::new(settings);
    let cm = Arc::new(ComponentManager::new(config));

    let result = dispatch(cli.command, &cm);

    // Fallback release. A command that ran through `teardown::run_command` has
    // already released on its own runtime — the one that owns the connections, and
    // therefore the only one that can settle their in-flight returns — so this is
    // then a cheap no-op. It exists for the paths that have not: a command with no
    // async work (`config`), and `run-sequence`, which defers the teardown so its
    // steps share one warm manager.
    cognee_cli::teardown::release_blocking(&cm);

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
        Commands::Export(args) => export::run(args, Arc::clone(cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(args) => run_sequence::run(args, Arc::clone(cm)),
        #[cfg(feature = "visualization")]
        Commands::Visualize(args) => visualize::run(args, Arc::clone(cm)),
        #[cfg(feature = "bench")]
        Commands::Bench(args) => bench::run(args, Arc::clone(cm)),
    }
}

/// Answer `--version`/`--help` before any config or logging I/O.
///
/// `run()` parses argv only after `load_settings()` and the subscriber install,
/// so without this both flags depend on a readable `config.json` and print a
/// `Logging initialized` line (plus create a log file) ahead of their own
/// output. That defeats their main use — a cheap probe of an installed binary.
///
/// Returns `Some(exit)` only for the two display kinds, which clap prints to
/// stdout. Every other outcome yields `None` so the real parse in `run()`
/// produces the diagnostics unchanged — a bare `cognee-cli` still reports a
/// missing subcommand exactly as before. Decision 11's ordering is untouched:
/// this returns before either step rather than reordering them.
fn short_circuit_version_or_help() -> Option<StdExitCode> {
    use clap::CommandFactory as _;
    use clap::error::ErrorKind;

    match Cli::command().try_get_matches_from(std::env::args_os()) {
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayVersion | ErrorKind::DisplayHelp
            ) =>
        {
            // Ignoring the write error is deliberate: a closed stdout leaves
            // nothing to report it on, and the exit code still carries.
            let _ = err.print();
            Some(StdExitCode::from(ExitCode::Success as u8))
        }
        _ => None,
    }
}

fn main() -> StdExitCode {
    if let Some(exit) = short_circuit_version_or_help() {
        return exit;
    }

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
