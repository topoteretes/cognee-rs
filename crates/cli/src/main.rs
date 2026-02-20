mod cli;
mod commands;
mod config_store;
mod error;

use clap::Parser;
use cli::{Cli, Commands};
use commands::{add, cognify, config, delete, search};
use error::{CliError, ExitCode};

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
    match run() {
        Ok(()) => std::process::exit(ExitCode::Success as i32),
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(error.exit_code() as i32);
        }
    }
}
