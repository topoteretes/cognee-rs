mod cli;
mod commands;
mod config_store;
mod error;

use std::process::ExitCode as StdExitCode;
use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use cognee_lib::{ComponentManager, ConfigManager};
#[cfg(feature = "bench")]
use commands::bench;
#[cfg(feature = "cloud")]
use commands::disconnect;
#[cfg(feature = "cloud")]
use commands::serve;
#[cfg(feature = "visualization")]
use commands::visualize;
use commands::{
    add, add_and_cognify, cognify, config, delete, forget, improve, memify, recall, remember,
    run_sequence, search,
};
use config_store::{Settings, load_settings};
use error::{CliError, ExitCode};
use tracing::error;

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
        Commands::Remember(args) => remember::run(args, Arc::clone(&cm)),
        Commands::Recall(args) => recall::run(args, Arc::clone(&cm)),
        Commands::Forget(args) => forget::run(args, Arc::clone(&cm)),
        Commands::Improve(args) => improve::run(args, Arc::clone(&cm)),
        Commands::Delete(args) => delete::run(args, Arc::clone(&cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(args) => run_sequence::run(args, Arc::clone(&cm)),
        #[cfg(feature = "visualization")]
        Commands::Visualize(args) => visualize::run(args, Arc::clone(&cm)),
        #[cfg(feature = "cloud")]
        Commands::Serve(args) => serve::run(args, Arc::clone(&cm)),
        #[cfg(feature = "cloud")]
        Commands::Disconnect(args) => disconnect::run(args, Arc::clone(&cm)),
        #[cfg(feature = "bench")]
        Commands::Bench(args) => bench::run(args, Arc::clone(&cm)),
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

    #[cfg(not(feature = "telemetry"))]
    let _log_guards = cognee_logging::init_logging(
        logging_cfg,
        std::iter::empty::<cognee_logging::BoxedLayer>(),
    );

    #[cfg(feature = "telemetry")]
    let (_log_guards, _telemetry_guard) = {
        use cognee_lib::telemetry::{TelemetryGuard, init_telemetry};
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

        let guards = cognee_logging::init_logging(logging_cfg, std::iter::once(telemetry_layer));
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
