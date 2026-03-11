use std::time::{Duration, Instant};

use clap::Parser;
use serde::Deserialize;
use tracing::info;

use crate::cli::{Cli, Commands, RunSequenceArgs};
use crate::error::CliError;

use super::{add, cognify, config, delete, search};

#[derive(Debug, Deserialize)]
struct SequenceStep {
    command: Vec<String>,
    #[serde(default)]
    timestamp_offset_ms: u64,
}

fn dispatch(command: Commands) -> Result<(), CliError> {
    match command {
        Commands::Add(args) => add::run(args),
        Commands::Cognify(args) => cognify::run(args),
        Commands::Search(args) => search::run(args),
        Commands::Delete(args) => delete::run(args),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(_) => Err(CliError::Validation(
            "Nested run-sequence is not allowed".to_string(),
        )),
    }
}

pub fn run(args: RunSequenceArgs) -> Result<(), CliError> {
    let content = std::fs::read_to_string(&args.sequence_file).map_err(|e| {
        CliError::Validation(format!(
            "Failed to read sequence file '{}': {}",
            args.sequence_file, e
        ))
    })?;

    let steps: Vec<SequenceStep> = serde_json::from_str(&content)
        .map_err(|e| CliError::Validation(format!("Failed to parse sequence file: {}", e)))?;

    if steps.is_empty() {
        return Err(CliError::Validation(
            "Sequence file contains no commands".to_string(),
        ));
    }

    let start = Instant::now();

    for (index, step) in steps.iter().enumerate() {
        if step.command.is_empty() {
            return Err(CliError::Validation(format!(
                "Step {}: command array is empty",
                index + 1
            )));
        }

        // Sleep until the target offset if we haven't reached it yet
        let target = Duration::from_millis(step.timestamp_offset_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            let sleep_duration = target - elapsed;
            info!(
                "Step {}/{}: waiting {:.1}s before execution",
                index + 1,
                steps.len(),
                sleep_duration.as_secs_f64()
            );
            std::thread::sleep(sleep_duration);
        }

        info!(
            "Step {}/{}: running {:?}",
            index + 1,
            steps.len(),
            step.command
        );

        // Parse the command array through Clap
        let cli_args =
            std::iter::once("cognee-cli".to_string()).chain(step.command.iter().cloned());

        let parsed = Cli::try_parse_from(cli_args).map_err(|e| {
            CliError::Validation(format!(
                "Step {}: failed to parse command {:?}: {}",
                index + 1,
                step.command,
                e
            ))
        })?;

        dispatch(parsed.command)
            .map_err(|e| CliError::Runtime(format!("Step {}: {}", index + 1, e)))?;
    }

    info!("Sequence completed: {} step(s) executed", steps.len());
    Ok(())
}
