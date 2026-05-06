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
use tracing_subscriber::EnvFilter;
#[cfg(feature = "telemetry")]
use tracing_subscriber::{Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};

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

    // Suppress verbose ONNX Runtime graph-optimizer logs (ort crate) by default.
    // Users can re-enable them with RUST_LOG="info,ort=info".
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

        let fmt_layer = fmt::layer().with_target(false);

        let _ = Registry::default()
            .with(telemetry_layer)
            .with(env_filter)
            .with(fmt_layer)
            .try_init();

        telemetry_guard
    };

    // Returning ExitCode (rather than calling process::exit) lets locals —
    // including _telemetry_guard — drop, flushing the final span batch.
    match run(settings) {
        Ok(()) => StdExitCode::from(ExitCode::Success as u8),
        Err(error) => {
            error!("Error: {error}");
            StdExitCode::from(error.exit_code() as u8)
        }
    }
}
