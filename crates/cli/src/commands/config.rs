use serde_json::Value;

use crate::cli::{ConfigAction, ConfigArgs};
use crate::config_store::{
    ConfigDocument, as_flat_map, known_keys, load_config, save_config, set_value, unset_key,
};
use crate::error::CliError;

pub fn run(args: ConfigArgs) -> Result<(), CliError> {
    match args.action {
        ConfigAction::Get(arguments) => handle_get(arguments.key.as_deref()),
        ConfigAction::Set(arguments) => handle_set(&arguments.key, &arguments.value),
        ConfigAction::List => handle_list(),
        ConfigAction::Unset(arguments) => handle_unset(&arguments.key, arguments.force),
        ConfigAction::Reset(arguments) => handle_reset(arguments.force),
    }
}

fn handle_get(key: Option<&str>) -> Result<(), CliError> {
    let config = load_config()?;
    let flat = as_flat_map(&config.settings);

    if let Some(key) = key {
        let value = flat.get(key).ok_or_else(|| {
            CliError::Validation(format!(
                "Unknown config key '{key}'. Use 'cognee-cli config list' to see supported keys."
            ))
        })?;
        println!("{key}: {value}");
        return Ok(());
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&config).map_err(|error| CliError::Runtime(format!(
            "Failed to format config output: {error}"
        )))?
    );

    Ok(())
}

fn handle_set(key: &str, raw_value: &str) -> Result<(), CliError> {
    let mut config = load_config()?;
    let parsed = serde_json::from_str::<Value>(raw_value)
        .unwrap_or_else(|_| Value::String(raw_value.to_string()));

    set_value(&mut config.settings, key, parsed)?;
    save_config(&config)?;

    println!("Success: Set {key}");
    Ok(())
}

fn handle_list() -> Result<(), CliError> {
    println!("Available configuration keys:");
    for key in known_keys() {
        println!("  {key}");
    }

    Ok(())
}

fn handle_unset(key: &str, force: bool) -> Result<(), CliError> {
    if !force && !confirm(&format!("Unset configuration key '{key}'?"))? {
        println!("Unset cancelled.");
        return Ok(());
    }

    let mut config = load_config()?;
    unset_key(&mut config.settings, key)?;
    save_config(&config)?;

    println!("Success: Unset {key}");
    Ok(())
}

fn handle_reset(force: bool) -> Result<(), CliError> {
    if !force && !confirm("Reset all configuration to defaults?")? {
        println!("Reset cancelled.");
        return Ok(());
    }

    let config = ConfigDocument::default();
    save_config(&config)?;

    println!("Success: Configuration reset to defaults");
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool, CliError> {
    use std::io::{self, Write};

    print!("{prompt} [y/N]: ");
    io::stdout()
        .flush()
        .map_err(|error| CliError::Runtime(format!("Failed to flush stdout: {error}")))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| CliError::Runtime(format!("Failed to read user input: {error}")))?;

    let normalized = input.trim().to_lowercase();
    Ok(normalized == "y" || normalized == "yes")
}
