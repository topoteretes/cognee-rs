use crate::error::CliError;
pub use cognee::Settings;
use cognee::config::{ConfigError, ConfigSchema, builtin_schema, key_spec, keys};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bumped to 2 when `Settings::chunk_size` became `Option<u32>`.
///
/// Version-1 documents encode "auto-calculate" as the literal `1500` (the old
/// sentinel), and `save_config` writes the whole `Settings` struct, so every
/// config.json written by a prior version carries `"chunk_size": 1500` — even
/// for a user who only ever set an API key. Deserialising that as `Some(1500)`
/// would turn "auto" into an explicit override on upgrade: 8191 -> 1500 tokens
/// per chunk on OpenAI-compatible embeddings (~5x the extraction calls), or
/// 512 -> 1500 on the local ONNX/BGE engine, where every chunk then exceeds the
/// model's sequence limit. `migrate_document` rewrites it on load.
/// Version 3 exists for the identical reason, one field over: `llm_temperature`
/// moved from `f64` (default `0.0`) to `Option<f64>` (default "send nothing").
/// A config.json written by any prior version carries `"llm_temperature": 0.0`
/// — the whole `Settings` struct is serialised, so merely running
/// `cognee-cli config set <anything>` persists it. Deserialising that as
/// `Some(0.0)` would put an explicit `temperature: 0.0` on every Bedrock call
/// for a user who never chose one, which is the extreme of the very setting
/// this release stopped sending by default.
/// Version 4: settings that stopped being `Settings` fields and became
/// component config sections (first the LLM ones: `llm_model`, `llm_api_key`,
/// `llm_max_retries`, `transcription_model`, ...) have namespaced canonical
/// keys (`llm.model`, `llm.api_key`, ...) and keep the old flat names as
/// aliases. The step canonicalizes every alias of the built-in schema, so it
/// covers each setting that moves into a section. Every pre-v4
/// file carries those flat names (again: the whole struct was serialised), so
/// `migrate_document` rewrites them to the canonical keys. Left as aliases,
/// they would sit beside the canonical keys that env vars, CLI flags and
/// `config set` write, and `config unset` would leave them behind.
const CURRENT_VERSION: u32 = 4;

/// The version-1 sentinel for "auto-calculate the chunk size".
const LEGACY_AUTO_CHUNK_SIZE: u32 = 1500;

/// The pre-v3 default for `llm_temperature`, persisted as a real value.
const LEGACY_DEFAULT_TEMPERATURE: f64 = 0.0;

pub use cognee::config::DEFAULT_SYSTEM_PROMPT_PATH;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            settings: Settings::default(),
        }
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "env-only by design: COGNEE_CONFIG_HOME locates the config file itself"
)]
pub fn config_file_path() -> Result<PathBuf, CliError> {
    // Production always resolves the config dir via `dirs::config_dir()` —
    // unchanged on every platform. Tests set `COGNEE_CONFIG_HOME` to an isolated
    // absolute temp dir so parallel CLI test processes don't collide on one
    // shared config file (and race the atomic `config.json[.tmp]` replace).
    //
    // We deliberately do NOT key off `XDG_CONFIG_HOME`: honoring it on macOS
    // would relocate a real user's existing config (which holds durable
    // credentials with no regeneration path) the moment they have XDG set, and
    // `dirs` already consults XDG on Linux. Only an *absolute* override is
    // accepted — a relative one would resolve against the CWD, making config
    // silently per-directory.
    let base_dir = match std::env::var_os("COGNEE_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => dirs::config_dir().ok_or_else(|| {
            CliError::Runtime("Could not resolve user config directory for cognee-cli".to_string())
        })?,
    };

    Ok(base_dir.join("cognee-rust").join("config.json"))
}

/// Load the persisted CLI config (JSON), then overlay environment variables on top.
///
/// Priority: `defaults < JSON config < env vars`.
/// The `.env` file is loaded automatically inside [`Settings::overlay_from_env`].
pub fn load_settings() -> Result<Settings, CliError> {
    let config = load_config()?;
    let mut settings = config.settings;
    settings.overlay_from_env();
    Ok(settings)
}

/// [`load_settings`] for a CLI built on a custom component registry: `schema`
/// is `cognee::config::schema_with(&registry.config_schema()?)`, so the env
/// vars of the registry's own config sections are read too.
pub fn load_settings_with_schema(schema: &ConfigSchema) -> Result<Settings, CliError> {
    let config = load_config()?;
    let mut settings = config.settings;
    settings.overlay_from_env_with_schema(schema);
    Ok(settings)
}

pub fn load_config() -> Result<ConfigDocument, CliError> {
    let path = config_file_path()?;

    if !path.exists() {
        return Ok(ConfigDocument::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|error| CliError::Runtime(format!("Failed to read config file: {error}")))?;

    let mut document = serde_json::from_str::<ConfigDocument>(&content)
        .map_err(|error| CliError::Runtime(format!("Failed to parse config file: {error}")))?;
    migrate_document(&mut document);
    Ok(document)
}

/// Bring a config document loaded from disk up to [`CURRENT_VERSION`].
///
/// In-memory only — the upgraded document is written back on the next
/// `save_config`, so a read-only command never rewrites the user's file.
fn migrate_document(document: &mut ConfigDocument) {
    if document.version < 2 {
        // v1 -> v2: `chunk_size` moved from a `1500`-means-auto sentinel to
        // `Option<u32>`. Map the sentinel back to "auto" so an upgrade preserves
        // the chunk size the user was actually getting. A user who explicitly
        // chose 1500 under v1 was already being auto-resolved, so this loses
        // nothing they had.
        if document.settings.chunk_size == Some(LEGACY_AUTO_CHUNK_SIZE) {
            document.settings.chunk_size = None;
        }
        document.version = 2;
    }
    if document.version < 3 {
        // v2 -> v3: `llm_temperature` moved from `f64` (default 0.0) to
        // `Option<f64>` (default: send no temperature at all). Map a persisted
        // 0.0 back to "unset", so an upgrade does not silently start pinning
        // every request to the lowest possible temperature.
        //
        // This does lose the setting for a user who deliberately chose 0.0 under
        // v2 — but under v2 that value was never reaching the provider on the
        // path that matters, so what they had was the provider default anyway.
        // Preserving the appearance of a setting that did nothing, at the cost
        // of silently changing every request, is the worse trade.
        if document
            .settings
            .llm()
            .temperature
            .is_some_and(|t| (t - LEGACY_DEFAULT_TEMPERATURE).abs() < f64::EPSILON)
        {
            // Pre-v3 files spell it `llm_temperature`; it lives in the `llm`
            // section now, under `llm.temperature`.
            document.settings.sections.remove("llm_temperature");
            document.settings.sections.remove("llm.temperature");
        }
        document.version = 3;
    }
    if document.version < 4 {
        // v3 -> v4: canonicalize every flat key that is now an alias of a
        // section key. When a file somehow holds both spellings, the
        // canonical one is the newer write and is kept.
        canonicalize_section_aliases(&mut document.settings.sections);
        document.version = 4;
    }
}

/// Rewrite every key of `sections` that is an alias of a built-in section
/// setting to its canonical key; an alias whose canonical key is also
/// present is dropped.
fn canonicalize_section_aliases(sections: &mut BTreeMap<String, Value>) {
    let schema = builtin_schema();
    let aliases: Vec<(String, &'static str)> = sections
        .keys()
        .filter_map(|key| {
            schema
                .key_def(key)
                .filter(|def| def.key != key)
                .map(|def| (key.clone(), def.key))
        })
        .collect();
    for (alias, canonical) in aliases {
        let Some(value) = sections.remove(&alias) else {
            continue;
        };
        sections.entry(canonical.to_string()).or_insert(value);
    }
}

pub fn save_config(config: &ConfigDocument) -> Result<(), CliError> {
    let path = config_file_path()?;
    let directory = path.parent().ok_or_else(|| {
        CliError::Runtime("Could not resolve parent directory for config file".to_string())
    })?;

    fs::create_dir_all(directory).map_err(|error| {
        CliError::Runtime(format!("Failed to create config directory: {error}"))
    })?;

    let serialized = serde_json::to_string_pretty(config)
        .map_err(|error| CliError::Runtime(format!("Failed to serialize config: {error}")))?;

    let temp_path = path.with_extension("json.tmp");
    let mut temp_file = fs::File::create(&temp_path).map_err(|error| {
        CliError::Runtime(format!("Failed to create temp config file: {error}"))
    })?;
    temp_file
        .write_all(serialized.as_bytes())
        .map_err(|error| CliError::Runtime(format!("Failed to write temp config file: {error}")))?;

    fs::rename(&temp_path, &path)
        .map_err(|error| CliError::Runtime(format!("Failed to replace config file: {error}")))
}

/// The settings `cognee-cli config` can address: every `Settings` key whose
/// value is saved in `config.json` (`llm_args` and `summarization_schema` are
/// not serialized, so a `config set` of them would be silently lost), then the
/// keys of the component config sections compiled into this build.
pub fn known_keys() -> Vec<&'static str> {
    keys()
        .iter()
        .filter(|spec| spec.persisted)
        .map(|spec| spec.key)
        .chain(builtin_schema().key_defs().map(|def| def.key))
        .collect()
}

/// A key `config set` / `config unset` must refuse even though it exists: a
/// `Settings` field that is not saved in `config.json`.
fn reject_unpersisted(key: &str) -> Result<(), CliError> {
    match key_spec(key) {
        Some(spec) if !spec.persisted => Err(CliError::Validation(format!(
            "Config key '{key}' is not saved in the config file; set it through the \
             environment or the SDK instead."
        ))),
        _ => Ok(()),
    }
}

fn cli_error(error: ConfigError) -> CliError {
    match error {
        ConfigError::TypeMismatch { key, reason } => {
            CliError::Validation(format!("Config key '{key}': {reason}"))
        }
        ConfigError::UnknownKey(key) => CliError::Validation(format!(
            "Unknown config key '{key}'. Use 'cognee-cli config list' to see supported keys."
        )),
    }
}

/// Whether `key` (or an alias) names a setting of this build: a `Settings`
/// key or a key of a compiled-in component config section.
pub fn is_known_key(key: &str) -> bool {
    key_spec(key).is_some() || builtin_schema().key_def(key).is_some()
}

/// Whether strict mode is on for this invocation: the saved `strict_config`
/// with the environment (`COGNEE_STRICT_CONFIG`) overlaid, as every other
/// command sees it. The env value is only read, never saved.
pub fn effective_strict(settings: &Settings) -> bool {
    let mut effective = settings.clone();
    effective.overlay_from_env();
    effective.strict_config
}

/// The unknown-key policy of `config set` / `config unset` for a `key` that
/// [`is_known_key`] rejected: an error under strict mode, else `Ok`, and the
/// caller changes nothing.
pub fn reject_unknown(key: &str, strict: bool) -> Result<(), CliError> {
    if strict {
        return Err(cli_error(ConfigError::UnknownKey(key.to_string())));
    }
    Ok(())
}

/// `config get <key>`: the value of a setting, addressed by its key or an
/// alias.
pub fn get_value(settings: &Settings, key: &str) -> Result<Value, CliError> {
    reject_unpersisted(key)?;
    settings
        .get_key(key)
        .ok_or_else(|| cli_error(ConfigError::UnknownKey(key.to_string())))
}

pub fn as_flat_map(settings: &Settings) -> BTreeMap<&'static str, Value> {
    known_keys()
        .into_iter()
        .map(|key| (key, settings.get_key(key).unwrap_or(Value::Null)))
        .collect()
}

/// `config set`. An unknown key is a warning and changes nothing, or an error
/// when `strict_config` is on.
pub fn set_value(settings: &mut Settings, key: &str, value: Value) -> Result<(), CliError> {
    reject_unpersisted(key)?;
    settings.set_key(key, value).map_err(cli_error)
}

/// `config unset`: restore the default. An unknown key is handled as in
/// [`set_value`].
pub fn unset_key(settings: &mut Settings, key: &str) -> Result<(), CliError> {
    reject_unpersisted(key)?;
    settings.reset_key(key).map_err(cli_error)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// TOP-8 upgrade hazard: every config.json written before `chunk_size`
    /// became an `Option` carries the literal `1500`, which meant "auto" under
    /// the old sentinel encoding. Reading it as an explicit override would drop
    /// a user from 8191 (or 512) to 1500 tokens per chunk on upgrade.
    #[test]
    fn v1_document_maps_the_legacy_sentinel_back_to_auto() {
        let mut document = ConfigDocument {
            version: 1,
            settings: Settings {
                chunk_size: Some(LEGACY_AUTO_CHUNK_SIZE),
                ..Settings::default()
            },
        };
        migrate_document(&mut document);
        assert_eq!(
            document.settings.chunk_size, None,
            "the v1 sentinel must restore auto-calculation"
        );
        assert_eq!(document.version, CURRENT_VERSION);
    }

    /// A v1 user who chose a real size keeps it — only the sentinel is remapped.
    #[test]
    fn v1_document_keeps_a_genuinely_explicit_chunk_size() {
        let mut document = ConfigDocument {
            version: 1,
            settings: Settings {
                chunk_size: Some(4096),
                ..Settings::default()
            },
        };
        migrate_document(&mut document);
        assert_eq!(document.settings.chunk_size, Some(4096));
        assert_eq!(document.version, CURRENT_VERSION);
    }

    /// At the current version `1500` is a legitimate explicit choice and must
    /// survive — that is the whole point of dropping the sentinel.
    #[test]
    fn current_version_document_is_left_alone() {
        let mut document = ConfigDocument {
            version: CURRENT_VERSION,
            settings: Settings {
                chunk_size: Some(LEGACY_AUTO_CHUNK_SIZE),
                ..Settings::default()
            },
        };
        migrate_document(&mut document);
        assert_eq!(
            document.settings.chunk_size,
            Some(LEGACY_AUTO_CHUNK_SIZE),
            "an explicit 1500 chosen under v2 must not be reinterpreted as auto"
        );
    }

    /// A config.json written before v3 carries `"llm_temperature": 0.0` even for
    /// a user who only ever set an API key — the whole `Settings` struct is
    /// serialised. Reading that back as `Some(0.0)` would pin every Bedrock call
    /// to the lowest possible temperature on upgrade, which is the opposite of
    /// the release's intent.
    #[test]
    fn migrating_v2_maps_a_persisted_default_temperature_back_to_unset() {
        let mut document = ConfigDocument {
            version: 2,
            settings: Settings {
                sections: [("llm_temperature".to_string(), serde_json::json!(0.0))].into(),
                ..Settings::default()
            },
        };
        migrate_document(&mut document);
        assert_eq!(document.version, CURRENT_VERSION);
        assert_eq!(
            document.settings.llm().temperature,
            None,
            "a persisted 0.0 must become 'unset', not an explicit zero",
        );
    }

    /// A temperature the user actually chose survives the migration.
    #[test]
    fn migrating_v2_keeps_a_deliberately_set_temperature() {
        let mut document = ConfigDocument {
            version: 2,
            settings: Settings {
                sections: [("llm_temperature".to_string(), serde_json::json!(0.7))].into(),
                ..Settings::default()
            },
        };
        migrate_document(&mut document);
        assert_eq!(document.settings.llm().temperature, Some(0.7));
    }

    /// A config.json written before v4 (by any earlier cognee-cli)
    /// serialised the flat LLM fields: `"llm_api_key": ""`,
    /// `"llm_max_retries": 3`, ... Parse one exactly as `load_config` does.
    fn legacy_v3_document() -> ConfigDocument {
        let mut document: ConfigDocument = serde_json::from_value(serde_json::json!({
            "version": 3,
            "settings": {
                "llm_api_key": "",
                "llm_model": "old-model",
                "llm_max_retries": 3,
                "transcription_model": "",
            }
        }))
        .expect("legacy document parses");
        migrate_document(&mut document);
        document
    }

    #[test]
    fn migrating_v3_rewrites_flat_llm_keys_to_canonical_keys() {
        let document = legacy_v3_document();
        assert_eq!(document.version, CURRENT_VERSION);
        let sections = &document.settings.sections;
        for alias in [
            "llm_api_key",
            "llm_model",
            "llm_max_retries",
            "transcription_model",
        ] {
            assert!(!sections.contains_key(alias), "alias {alias} must be gone");
        }
        assert_eq!(
            sections.get("llm.model"),
            Some(&serde_json::json!("old-model"))
        );
        assert_eq!(sections.get("llm.max_retries"), Some(&serde_json::json!(3)));
    }

    /// The upgrade hazard itself: the file's `"llm_api_key": ""` must not
    /// overwrite `LLM_API_KEY` from the env, nor its `"llm_max_retries": 3`
    /// the `--llm-max-retries` flag, once `ConfigManager` normalizes them.
    #[test]
    fn a_legacy_file_does_not_shadow_env_or_cli_overrides() {
        let mut settings = legacy_v3_document().settings;
        settings.overlay_from(&cognee::config::MapEnv::new().with("LLM_API_KEY", "sk-x"));
        settings
            .sections
            .insert("llm.max_retries".to_string(), serde_json::json!(10));
        let cm = cognee::config::ConfigManager::new(settings);
        let llm = cm.read().llm();
        assert_eq!(llm.api_key, "sk-x");
        assert_eq!(llm.max_retries, 10);
        assert_eq!(llm.model, "old-model", "the file's own value still applies");
    }

    /// `config set` / `config unset` of a flat name act on the one value the
    /// runtime reads, even if a stale alias spelling is still around.
    #[test]
    fn set_and_unset_by_alias_leave_no_stale_spelling() {
        let mut settings = Settings::default();
        settings
            .sections
            .insert("llm_model".to_string(), serde_json::json!("stale"));
        set_value(&mut settings, "llm_model", Value::String("fresh".into())).expect("set");
        assert_eq!(settings.llm().model, "fresh");
        assert_eq!(get_value(&settings, "llm_model").expect("get"), "fresh");

        settings
            .sections
            .insert("llm_model".to_string(), serde_json::json!("stale"));
        unset_key(&mut settings, "llm_model").expect("unset");
        assert!(!settings.sections.contains_key("llm_model"));
        assert!(!settings.sections.contains_key("llm.model"));
    }

    /// `single_process` must be a first-class CLI key on every surface the
    /// others use: listable, settable, readable back, and unsettable.
    ///
    /// It shipped in review reachable only through `COGNEE_SINGLE_PROCESS`,
    /// which left `cognee config set single_process true` answering "unknown
    /// key" while the docs told people to set it.
    #[test]
    fn single_process_is_a_first_class_cli_key() {
        assert!(
            known_keys().contains(&"single_process"),
            "`cognee config list` must show the key"
        );

        let mut settings = Settings::default();
        assert_eq!(settings.single_process, None);

        set_value(&mut settings, "single_process", Value::Bool(true)).expect("set true");
        assert_eq!(settings.single_process, Some(true));
        assert_eq!(
            as_flat_map(&settings).get("single_process"),
            Some(&Value::Bool(true)),
            "`cognee config get` must read back what was set"
        );

        // A CLI argument arrives as a string, and must mean what the same text
        // means in the env var — including blank meaning "derive".
        set_value(&mut settings, "single_process", Value::String("0".into())).expect("set \"0\"");
        assert_eq!(settings.single_process, Some(false));
        set_value(&mut settings, "single_process", Value::String("1".into())).expect("set \"1\"");
        assert_eq!(settings.single_process, Some(true));
        set_value(&mut settings, "single_process", Value::String("  ".into())).expect("blank");
        assert_eq!(
            settings.single_process, None,
            "a blank value means derive, not an explicit false"
        );

        set_value(&mut settings, "single_process", Value::Bool(true)).expect("set true again");
        unset_key(&mut settings, "single_process").expect("unset");
        assert_eq!(
            settings.single_process, None,
            "`cognee config unset` must restore the derived answer"
        );
    }

    /// `cognee config set single_process 1` must work, because the
    /// documentation lists `1` as an accepted value.
    ///
    /// The bug this pins: `commands::config::handle_set` JSON-parses the raw
    /// argument before calling [`set_value`], so the bare `1` an operator
    /// types never arrives as `Value::String("1")` — it arrives as
    /// `Value::Number(1)`, which fell through to `expect_bool` and was
    /// rejected with "expects true/false". The existing coverage above only
    /// used the string form, which the CLI can never actually produce for a
    /// bare digit, so the suite was green while the documented invocation
    /// failed.
    #[test]
    fn a_json_number_sets_single_process_the_way_the_docs_promise() {
        // Exactly what `handle_set` hands to `set_value` for these arguments.
        let parse = |raw: &str| {
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        };
        assert!(
            parse("1").is_number(),
            "the premise: the CLI's own parse turns `1` into a number, not a string"
        );

        let mut settings = Settings::default();

        set_value(&mut settings, "single_process", parse("1")).expect("`config set … 1`");
        assert_eq!(settings.single_process, Some(true));

        set_value(&mut settings, "single_process", parse("0")).expect("`config set … 0`");
        assert_eq!(settings.single_process, Some(false));

        // And the spellings that already worked must keep working, through the
        // same parser, so no two of them can disagree.
        for (raw, expected) in [
            ("true", Some(true)),
            ("false", Some(false)),
            ("yes", Some(true)),
            ("no", Some(false)),
            ("on", Some(true)),
            ("null", None),
        ] {
            set_value(&mut settings, "single_process", parse(raw)).expect(raw);
            assert_eq!(settings.single_process, expected, "`config set … {raw}`");
        }
    }

    /// The flat map is what `cognee config get` prints, and `null` there means
    /// "derived" — reporting `false` would say the sweep was refused when
    /// nobody had answered.
    #[test]
    fn unset_single_process_prints_as_null_not_false() {
        let settings = Settings::default();
        assert_eq!(
            as_flat_map(&settings).get("single_process"),
            Some(&Value::Null)
        );
    }
}
