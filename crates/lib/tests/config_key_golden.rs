//! Golden tests for the `Settings` key surface: the env binding
//! (`Settings::overlay_from`) and the generic setter (`ConfigManager::set`).
//!
//! Each test computes, for a fixed matrix of inputs, the fields that end up
//! different from `Settings::default()`, and compares the result with a JSON
//! snapshot under `tests/golden/`. Recording the *difference from the
//! default*, never the default itself, keeps the snapshots independent of
//! target- and feature-dependent defaults (Android embedding, graph backend).
//!
//! The snapshots pin behaviour across refactors of the config code. A change
//! to them is a behaviour change; review the diff. Regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cognee --test config_key_golden`.
//!
//! Config sections that only a cargo feature compiles in (see
//! `config_key_golden_support::FEATURE_SECTIONS`) are left out of these
//! snapshots, so the snapshots stay valid whatever is enabled. When the registry
//! does register one, `registry_binds_exactly_the_pinned_env_vars` expects its
//! env vars too. Their own snapshots are pinned by
//! `config_key_golden_session_stores`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]

mod config_key_golden_support;

use std::collections::BTreeMap;

use cognee::config::{ConfigManager, MapEnv, Settings};
use config_key_golden_support::{
    FEATURE_SECTIONS, check_golden, diff_from_default, env_probe_cases, overlay, section_set_keys,
    set_probe_cases,
};
use serde_json::{Value, json};

/// Every env var `Settings::overlay_from` binds, in the order the loader has
/// always read them.
const ENV_VARS: &[&str] = &[
    "LLM_PROVIDER",
    "LLM_MODEL",
    "OPENAI_MODEL",
    "LLM_API_KEY",
    "OPENAI_TOKEN",
    "LLM_ENDPOINT",
    "OPENAI_URL",
    "LLM_API_VERSION",
    "LLM_REASONING",
    "LLM_STRUCTURED_OUTPUT_MODE",
    "LLM_TEMPERATURE",
    "LLM_MAX_COMPLETION_TOKENS",
    "LLM_MAX_TOKENS",
    "LLM_ARGS",
    "LLM_STREAMING",
    "LLM_MAX_RETRIES",
    "LLM_NETWORK_RETRIES",
    "LLM_MIN_RETRY_SECONDS",
    "LLM_MAX_PARALLEL_REQUESTS",
    "LLM_REQUEST_TIMEOUT_SECONDS",
    "LLM_CONNECT_TIMEOUT_SECONDS",
    "LLM_REQUEST_DEADLINE_SECONDS",
    "COGNEE_CHUNK_SIZE",
    "CHUNK_SIZE",
    "COGNEE_CHUNK_OVERLAP",
    "CHUNK_OVERLAP",
    "MOCK_LLM",
    "MOCK_LLM_CASSETTE",
    "COGNEE_RECORD_LLM",
    "GRAPH_DATABASE_PROVIDER",
    "GRAPH_DATABASE_URL",
    "GRAPH_DATABASE_NAME",
    "GRAPH_DATABASE_USERNAME",
    "GRAPH_DATABASE_PASSWORD",
    "GRAPH_DATABASE_PORT",
    "GRAPH_DATABASE_HOST",
    "GRAPH_DATABASE_KEY",
    "GRAPH_FILE_PATH",
    "VECTOR_DB_PROVIDER",
    "VECTOR_DB_URL",
    "VECTOR_DB_PORT",
    "VECTOR_DB_NAME",
    "VECTOR_DB_KEY",
    "VECTOR_DB_USERNAME",
    "VECTOR_DB_PASSWORD",
    "VECTOR_DB_HOST",
    "DB_PROVIDER",
    "DB_HOST",
    "DB_PORT",
    "DB_NAME",
    "DB_USERNAME",
    "DB_PASSWORD",
    "DB_SSLMODE",
    "DATABASE_CONNECT_ARGS",
    "DATABASE_URL",
    "EMBEDDING_PROVIDER",
    "EMBEDDING_ENDPOINT",
    "EMBEDDING_API_KEY",
    "EMBEDDING_MODEL",
    "EMBEDDING_DIMENSIONS",
    "EMBEDDING_BATCH_SIZE",
    "EMBEDDING_ONNX_BATCH_SIZE",
    "EMBEDDING_MAX_SEQUENCE_LENGTH",
    "EMBEDDING_MODEL_PATH",
    "COGNEE_E2E_EMBED_MODEL_PATH",
    "EMBEDDING_TOKENIZER_PATH",
    "COGNEE_E2E_TOKENIZER_PATH",
    "COGNEE_SYSTEM_ROOT_DIRECTORY",
    "COGNEE_DATA_ROOT_DIRECTORY",
    "COGNEE_DEFAULT_DATASET_NAME",
    "COGNEE_DEFAULT_USER_ID",
    "ONTOLOGY_FILE_PATH",
    "ONTOLOGY_RESOLVER",
    "ONTOLOGY_MATCHING_STRATEGY",
    "CACHE_BACKEND",
    "SESSION_TTL_SECONDS",
    "CACHING",
    "AUTO_FEEDBACK",
    "DEFAULT_USER_EMAIL",
    "DEFAULT_USER_PASSWORD",
    "ENABLE_BACKEND_ACCESS_CONTROL",
    "LOG_LEVEL",
    "COGNEE_LOGS_DIR",
    "CACHE_ROOT_DIRECTORY",
    "AUTO_RATE_LIMIT",
    "LLM_RATE_LIMIT_ENABLED",
    "LLM_RATE_LIMIT_REQUESTS",
    "LLM_RATE_LIMIT_INTERVAL",
    "EMBEDDING_RATE_LIMIT_ENABLED",
    "EMBEDDING_RATE_LIMIT_REQUESTS",
    "EMBEDDING_RATE_LIMIT_INTERVAL",
    "STORAGE_BACKEND",
    "STORAGE_BUCKET_NAME",
    "GRAPH_EXTRACTOR",
    "COGNEE_SESSION_STORE",
    "ALLOW_CYPHER_QUERY",
    "COGNEE_COGNIFY_FAILURE_REPORT_CAP",
    "COGNEE_COGNIFY_MAX_CHUNK_FAILURE_RATIO",
    "COGNEE_COGNIFY_ROLLBACK_SCOPE",
    "COGNEE_COGNIFY_TOLERATE_SUMMARIZATION_FAILURES",
    "COGNEE_FEEDBACK_LLM_TIMEOUT_MS",
    "COGNEE_HYBRID_MAX_CONTEXT_TOKENS",
    "COGNEE_HYBRID_OVERFLOW_SUMMARY_FILE",
    "COGNEE_HYBRID_SUMMARIZE_BATCH",
    "COGNEE_HYBRID_SUMMARIZE_OVERFLOW",
    "COGNEE_LEGACY_PER_WORD_TOKEN_COUNT",
    "COGNEE_RAISE_INCREMENTAL_LOADING_ERRORS",
    "COGNEE_TOKEN_COUNTER",
    "COGNEE_TRIPLET_NEIGHBOR_CAP",
    "RAISE_INCREMENTAL_LOADING_ERRORS",
    "SESSION_ABANDON_AFTER_SECONDS",
    "COGNEE_TRACING_ENABLED",
    "OTEL_SERVICE_NAME",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_SPAN_PROCESSOR",
    "OTEL_TRACES_SAMPLER",
    "OTEL_TRACES_SAMPLER_ARG",
    "ENV",
    "ENABLE_LAST_ACCESSED",
    "COGNEE_SINGLE_PROCESS",
    "COGNEE_STRICT_CONFIG",
    // Component config sections of a default build.
    "GRAPH_MAX_DB_SIZE",
    "COGNEE_DEBUG_LLM_REQUEST",
    "LLM_VISION_MODEL",
    "TRANSCRIPTION_MODEL",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_BASE",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE_NAME",
    "AWS_ROLE_NAME",
    "AWS_SESSION_NAME",
    "AWS_WEB_IDENTITY_TOKEN",
    "AWS_STS_ENDPOINT",
    "AWS_EXTERNAL_ID",
    "AWS_BEDROCK_RUNTIME_ENDPOINT",
    "AWS_BEARER_TOKEN_BEDROCK",
    "MOCK_EMBEDDING",
    "EMBEDDING_API_VERSION",
    "EMBEDDING_MAX_COMPLETION_TOKENS",
    "HUGGINGFACE_TOKENIZER",
    "COGNEE_PGVECTOR_HNSW_M",
    "COGNEE_PGVECTOR_HNSW_EF_CONSTRUCTION",
    "COGNEE_PGVECTOR_MAINTENANCE_WORKERS",
    "COGNEE_PGVECTOR_MAINTENANCE_WORK_MEM_MB",
];

/// Every env var this build is expected to bind: the default build's, plus
/// those of each feature-gated section the registry actually registers.
fn pinned_env_vars() -> Vec<&'static str> {
    let mut vars = ENV_VARS.to_vec();
    for fs in FEATURE_SECTIONS.iter().filter(|fs| fs.is_registered()) {
        vars.extend_from_slice(fs.env_vars);
    }
    vars
}

/// Primary/alias pairs, probed for precedence.
const ENV_ALIASES: &[(&str, &str)] = &[
    ("LLM_MODEL", "OPENAI_MODEL"),
    ("LLM_API_KEY", "OPENAI_TOKEN"),
    ("LLM_ENDPOINT", "OPENAI_URL"),
    ("LLM_MAX_COMPLETION_TOKENS", "LLM_MAX_TOKENS"),
    ("COGNEE_CHUNK_SIZE", "CHUNK_SIZE"),
    ("COGNEE_CHUNK_OVERLAP", "CHUNK_OVERLAP"),
    ("EMBEDDING_API_KEY", "LLM_API_KEY"),
    ("EMBEDDING_MODEL_PATH", "COGNEE_E2E_EMBED_MODEL_PATH"),
    ("EMBEDDING_TOKENIZER_PATH", "COGNEE_E2E_TOKENIZER_PATH"),
    ("DB_SSLMODE", "DATABASE_CONNECT_ARGS"),
    ("ANTHROPIC_BASE_URL", "ANTHROPIC_API_BASE"),
];

/// Every key `ConfigManager::set` is asked about: all serialized `Settings`
/// fields, the two `#[serde(skip)]` fields, the `embedding_model` alias, and
/// a key that does not exist. Feature-gated sections are left out;
/// `config_key_golden_session_stores` covers them.
fn set_keys() -> Vec<String> {
    let mut keys: Vec<String> = serde_json::to_value(Settings::default())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.extend(
        [
            "llm_args",
            "summarization_schema",
            "embedding_model",
            "no_such_key",
        ]
        .into_iter()
        .map(String::from),
    );
    keys.extend(section_set_keys(None));
    keys.sort();
    keys.dedup();
    keys
}

#[test]
fn env_overlay_matches_golden() {
    let mut cases = BTreeMap::new();
    cases.insert("<empty env>".to_string(), overlay(&MapEnv::new()));
    env_probe_cases(ENV_VARS, &mut cases);
    for (primary, alias) in ENV_ALIASES {
        let both = MapEnv::new().with(*primary, "1").with(*alias, "2");
        cases.insert(format!("{primary}=1 + {alias}=2"), overlay(&both));
        let empty_primary = MapEnv::new().with(*primary, "").with(*alias, "2");
        cases.insert(
            format!("{primary}=\"\" + {alias}=2"),
            overlay(&empty_primary),
        );
        let bad_primary = MapEnv::new().with(*primary, "abc").with(*alias, "2");
        cases.insert(format!("{primary}=abc + {alias}=2"), overlay(&bad_primary));
    }
    check_golden(
        "env_overlay.json",
        &Value::Object(cases.into_iter().collect()),
    );
}

#[test]
fn generic_set_matches_golden() {
    let mut cases = BTreeMap::new();
    set_probe_cases(&set_keys(), &mut cases);
    // `system_root_directory` cascades into the paths derived from it.
    let cm = ConfigManager::new(Settings::default());
    cm.set("graph_file_path", json!("./.cognee_system/custom_graph"))
        .unwrap();
    cm.set("system_root_directory", json!("/srv/cognee"))
        .unwrap();
    cases.insert(
        "system_root_directory cascade".to_string(),
        diff_from_default(&cm.read()),
    );
    // Strict mode turns an unknown key into an error.
    let cm = ConfigManager::new(Settings {
        strict_config: true,
        ..Settings::default()
    });
    cases.insert(
        "strict: no_such_key <- 1".to_string(),
        match cm.set("no_such_key", json!(1)) {
            Ok(()) => json!({ "ok": diff_from_default(&cm.read()) }),
            Err(e) => json!({ "err": format!("{e}") }),
        },
    );
    check_golden(
        "generic_set.json",
        &Value::Object(cases.into_iter().collect()),
    );
}

/// The registry binds exactly the env vars pinned above: none lost, none
/// added without updating this list (and the docs).
#[test]
fn registry_binds_exactly_the_pinned_env_vars() {
    let mut bound: Vec<&str> = cognee::config::keys()
        .iter()
        .flat_map(|spec| spec.env.iter().copied())
        .chain(
            cognee::config::builtin_schema()
                .key_defs()
                .flat_map(|def| def.env.iter().copied()),
        )
        .collect();
    bound.sort_unstable();
    bound.dedup();
    let mut pinned = pinned_env_vars();
    pinned.sort_unstable();
    assert_eq!(bound, pinned);
}
