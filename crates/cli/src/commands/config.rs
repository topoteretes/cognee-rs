use serde_json::Value;
use tracing::{info, warn};

use crate::cli::{ConfigAction, ConfigArgs};
use crate::config_store::{
    ConfigDocument, effective_strict, get_value, is_known_key, known_keys, load_config,
    reject_unknown, save_config, set_value, unset_key,
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

    if let Some(key) = key {
        let value = get_value(&config.settings, key)?;
        info!("{key}: {value}");
        return Ok(());
    }

    info!(
        "{}",
        serde_json::to_string_pretty(&config).map_err(|error| CliError::Runtime(format!(
            "Failed to format config output: {error}"
        )))?
    );

    Ok(())
}

fn handle_set(key: &str, raw_value: &str) -> Result<(), CliError> {
    let mut config = load_config()?;
    if !is_known_key(key) {
        reject_unknown(key, effective_strict(&config.settings))?;
        warn_unchanged(key);
        return Ok(());
    }
    let parsed = serde_json::from_str::<Value>(raw_value)
        .unwrap_or_else(|_| Value::String(raw_value.to_string()));

    set_value(&mut config.settings, key, parsed)?;
    save_config(&config)?;

    info!("Success: Set {key}");
    Ok(())
}

fn handle_list() -> Result<(), CliError> {
    info!("Available configuration keys:");
    for key in known_keys() {
        info!("  {key}");
    }

    Ok(())
}

fn handle_unset(key: &str, force: bool) -> Result<(), CliError> {
    if !force && !confirm(&format!("Unset configuration key '{key}'?"))? {
        info!("Unset cancelled.");
        return Ok(());
    }

    let mut config = load_config()?;
    // A stale entry no component owns (e.g. written by a build with more
    // features) is removed even in strict mode: that is how a config is
    // repaired.
    if !is_known_key(key) && !config.settings.sections.contains_key(key) {
        reject_unknown(key, effective_strict(&config.settings))?;
        warn_unchanged(key);
        return Ok(());
    }
    unset_key(&mut config.settings, key)?;
    save_config(&config)?;

    info!("Success: Unset {key}");
    Ok(())
}

fn handle_reset(force: bool) -> Result<(), CliError> {
    if !force && !confirm("Reset all configuration to defaults?")? {
        info!("Reset cancelled.");
        return Ok(());
    }

    let config = ConfigDocument::default();
    save_config(&config)?;

    info!("Success: Configuration reset to defaults");
    Ok(())
}

/// An unknown key in lenient mode: say so, and leave the file untouched.
fn warn_unchanged(key: &str) {
    warn!(
        "Unknown config key '{key}': nothing was changed. Use 'cognee-cli config list' to \
         see supported keys (set strict_config / COGNEE_STRICT_CONFIG=true to make this an error)."
    );
}

fn confirm(prompt: &str) -> Result<bool, CliError> {
    use std::io;

    info!("{prompt} [y/N]: ");

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| CliError::Runtime(format!("Failed to read user input: {error}")))?;

    let normalized = input.trim().to_lowercase();
    Ok(normalized == "y" || normalized == "yes")
}
