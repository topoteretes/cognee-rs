use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use cognee_lib::ComponentManager;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::cli::{Cli, Commands, RunSequenceArgs};
use crate::error::CliError;

#[cfg(feature = "cloud")]
use super::disconnect;
#[cfg(feature = "cloud")]
use super::serve;
#[cfg(feature = "visualization")]
use super::visualize;
use super::{add, add_and_cognify, cognify, config, delete, memify, search};

#[derive(Debug, Deserialize, Serialize)]
struct SequenceStep {
    command: Vec<String>,
    #[serde(default)]
    timestamp_offset_ms: u64,
}

fn dispatch(command: Commands, cm: &Arc<ComponentManager>) -> Result<(), CliError> {
    match command {
        Commands::Add(args) => add::run(args, Arc::clone(cm)),
        Commands::Cognify(args) => cognify::run(args, Arc::clone(cm)),
        Commands::AddAndCognify(args) => add_and_cognify::run(args, Arc::clone(cm)),
        Commands::Memify(args) => memify::run(args, Arc::clone(cm)),
        Commands::Search(args) => search::run(args, Arc::clone(cm)),
        Commands::Delete(args) => delete::run(args, Arc::clone(cm)),
        Commands::Config(args) => config::run(args),
        Commands::RunSequence(_) => Err(CliError::Validation(
            "Nested run-sequence is not allowed".to_string(),
        )),
        #[cfg(feature = "visualization")]
        Commands::Visualize(args) => visualize::run(args, Arc::clone(cm)),
        #[cfg(feature = "cloud")]
        Commands::Serve(args) => serve::run(args, Arc::clone(cm)),
        #[cfg(feature = "cloud")]
        Commands::Disconnect(args) => disconnect::run(args, Arc::clone(cm)),
        #[cfg(feature = "bench")]
        Commands::Bench(_) => Err(CliError::Validation(
            "bench is not allowed inside run-sequence".to_string(),
        )),
    }
}

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let millis = d.subsec_millis();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}h {:02}m {:02}.{:03}s", hours, minutes, seconds, millis)
    } else if minutes > 0 {
        format!("{}m {:02}.{:03}s", minutes, seconds, millis)
    } else {
        format!("{}.{:03}s", seconds, millis)
    }
}

fn run_single_file(file_path: &str, cm: &Arc<ComponentManager>) -> Result<(), CliError> {
    let content = std::fs::read_to_string(file_path).map_err(|e| {
        CliError::Validation(format!(
            "Failed to read sequence file '{}': {}",
            file_path, e
        ))
    })?;

    let steps: Vec<SequenceStep> = serde_json::from_str(&content).map_err(|e| {
        CliError::Validation(format!(
            "Failed to parse sequence file '{}': {}",
            file_path, e
        ))
    })?;

    if steps.is_empty() {
        return Err(CliError::Validation(format!(
            "Sequence file '{}' contains no commands",
            file_path
        )));
    }

    info!("Starting sequence file: {}", file_path);

    let start = Instant::now();

    for (index, step) in steps.iter().enumerate() {
        if step.command.is_empty() {
            return Err(CliError::Validation(format!(
                "Step {}: command array is empty in '{}'",
                index + 1,
                file_path
            )));
        }

        let step_json = serde_json::to_string(step).unwrap_or_else(|_| {
            format!(
                "{{\"command\":{:?},\"timestamp_offset_ms\":{}}}",
                step.command, step.timestamp_offset_ms
            )
        });
        info!(
            "Step {}/{}: start processing {}",
            index + 1,
            steps.len(),
            step_json
        );

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

        dispatch(parsed.command, cm).map_err(|e| {
            CliError::Runtime(format!("Step {} in '{}': {}", index + 1, file_path, e))
        })?;
    }

    let wall_time = start.elapsed();

    // Check if the last step has a non-zero offset
    let last_offset_ms = steps.last().map(|s| s.timestamp_offset_ms).unwrap_or(0);
    if last_offset_ms > 0 {
        let last_offset = Duration::from_millis(last_offset_ms);
        info!(
            "Finished sequence file '{}': {} step(s) executed in {} (last step offset: {})",
            file_path,
            steps.len(),
            format_duration(wall_time),
            format_duration(last_offset),
        );
    } else {
        info!(
            "Finished sequence file '{}': {} step(s) executed in {}",
            file_path,
            steps.len(),
            format_duration(wall_time),
        );
    }

    Ok(())
}

pub fn run(args: RunSequenceArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let file_count = args.sequence_files.len();

    for (file_index, file_path) in args.sequence_files.iter().enumerate() {
        if file_count > 1 {
            info!(
                "=== Sequence file {}/{}: {} ===",
                file_index + 1,
                file_count,
                file_path
            );
        }

        run_single_file(file_path, &cm)?;
    }

    if file_count > 1 {
        info!("All {} sequence files completed", file_count);
    }

    Ok(())
}
