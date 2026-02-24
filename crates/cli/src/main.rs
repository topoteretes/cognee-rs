mod cli;
mod commands;
mod config_store;
mod error;

use clap::Parser;
use cli::{Cli, Commands};
use commands::{add, cognify, config, delete, search};
use error::{CliError, ExitCode};
use tracing::error;
use tracing_subscriber::EnvFilter;

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Add(args) => add::run(args),
        Commands::Cognify(args) => cognify::run(args),
        Commands::Search(args) => search::run(args),
        Commands::Delete(args) => delete::run(args),
        Commands::Config(args) => config::run(args),
    }
}

fn main() {
    // Suppress verbose ONNX Runtime graph-optimizer logs (ort crate) by default.
    // Users can re-enable them with RUST_LOG="info,ort=info".
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
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
