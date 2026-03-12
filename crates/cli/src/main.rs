mod cli;
mod commands;
mod config_store;
mod error;

use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use cognee_lib::ComponentManager;
use commands::{add, cognify, config, delete, run_sequence, search};
use config_store::load_config;
use error::{CliError, ExitCode};
use tracing::error;
use tracing_subscriber::EnvFilter;

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    let config = load_config()?;
    let cm = Arc::new(ComponentManager::new(config.settings));

    match cli.command {
        Commands::Add(args) => add::run(args, Arc::clone(&cm)),
        Commands::Cognify(args) => cognify::run(args, Arc::clone(&cm)),
        Commands::Search(args) => search::run(args, Arc::clone(&cm)),
        Commands::Delete(args) => delete::run(args, Arc::clone(&cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(args) => run_sequence::run(args, Arc::clone(&cm)),
    }
}

fn main() {
    // Suppress verbose ONNX Runtime graph-optimizer logs (ort crate) by default.
    // Users can re-enable them with RUST_LOG="info,ort=info".
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();

    match run() {
        Ok(()) => std::process::exit(ExitCode::Success as i32),
        Err(error) => {
            error!("Error: {error}");
            std::process::exit(error.exit_code() as i32);
        }
    }
}
