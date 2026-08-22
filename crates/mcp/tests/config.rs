use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cognee_mcp::config::{AgentConfig, EnvSource};
use cognee_mcp::embedding_generation::EmbeddingGeneration;
use cognee_mcp::layout::StateLayout;
use cognee_mcp::limits::ResourceLimits;

#[derive(Default)]
struct FakeEnv {
    values: BTreeMap<String, String>,
    reads: RefCell<Vec<String>>,
}

impl EnvSource for FakeEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.reads.borrow_mut().push(key.to_owned());
        self.values.get(key).cloned()
    }
}

fn fake_env<const N: usize>(values: [(&str, &str); N]) -> FakeEnv {
    FakeEnv {
        values: values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        reads: RefCell::default(),
    }
}

#[test]
fn cognee_key_alias_wins_without_leaking() {
    let env = fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_PROXY_KEY", "alias-secret"),
        ("APEX_LLM_PROXY_KEY", "parent-secret"),
        ("APEX_COGNEE_LLM_MODEL", "gpt-5.4-nano"),
    ]);

    let cfg = AgentConfig::from_env(&env).expect("config");

    assert_eq!(cfg.proxy_key().expose(), "alias-secret");
    let debug = format!("{cfg:?}");
    let json = serde_json::to_string(&cfg).expect("serialize config");
    for secret in ["alias-secret", "parent-secret"] {
        assert!(!debug.contains(secret));
        assert!(!json.contains(secret));
    }
    assert_eq!(cfg.layout.root, PathBuf::from("/home/alice/.apex/cognee"));
    assert_eq!(cfg.dataset, "agent_sessions");
    assert!(cfg.embedding.is_none());
}

#[test]
fn parent_key_is_the_only_fallback() {
    let env = fake_env([
        ("HOME", "/home/alice"),
        ("APEX_LLM_PROXY_KEY", "parent-secret"),
        ("OPENAI_API_KEY", "generic-secret"),
        ("LLM_API_KEY", "generic-secret"),
    ]);

    let cfg = AgentConfig::from_env(&env).expect("config");

    assert_eq!(cfg.proxy_key().expose(), "parent-secret");
    let reads = env.reads.borrow();
    assert!(!reads.iter().any(|key| key == "OPENAI_API_KEY"));
    assert!(!reads.iter().any(|key| key == "LLM_API_KEY"));
    assert!(reads.iter().all(|key| {
        key == "HOME"
            || key == "APEX_HOME"
            || key == "APEX_LLM_PROXY_KEY"
            || key.starts_with("APEX_COGNEE_")
    }));
}

#[test]
fn both_apex_profiles_share_the_default_state_root() {
    for apex_home in ["/home/alice/.apex", "/home/alice/.apex-copilot"] {
        let env = fake_env([("HOME", "/home/alice"), ("APEX_HOME", apex_home)]);
        let cfg = AgentConfig::from_env(&env).expect("config");
        assert_eq!(cfg.layout.root, PathBuf::from("/home/alice/.apex/cognee"));
    }

    let env = fake_env([
        ("HOME", "/home/alice"),
        ("APEX_HOME", "/home/alice/.apex-copilot"),
        ("APEX_COGNEE_ROOT", "/private/cognee"),
        ("APEX_COGNEE_DATASET", "team_memory"),
    ]);
    let cfg = AgentConfig::from_env(&env).expect("config");
    assert_eq!(cfg.layout.root, PathBuf::from("/private/cognee"));
    assert_eq!(cfg.dataset, "team_memory");
}

#[test]
fn partial_embedding_configuration_is_capture_safe_but_has_no_default() {
    let absent =
        AgentConfig::from_env(&fake_env([("HOME", "/home/alice")])).expect("capture config");
    assert!(absent.embedding.is_none());

    let partial = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_EMBEDDING_MODEL", "operator-model"),
    ]))
    .expect("partial capture config");
    let embedding = partial
        .embedding
        .as_ref()
        .expect("partial embedding retained");
    assert_eq!(embedding.model, "operator-model");
    assert_eq!(embedding.dimensions, 0);
}

#[test]
fn resource_defaults_match_the_fleet_policy() {
    let cfg = AgentConfig::from_env(&fake_env([("HOME", "/home/alice")])).expect("config");

    assert_eq!(
        cfg.limits,
        ResourceLimits {
            engine_owners: 1,
            llm_lanes: 1,
            drain_timeout_seconds: 120,
            llm_timeout_seconds: 45,
            embedding_timeout_seconds: 30,
            max_llm_calls: 8,
            max_input_tokens: 48_000,
            max_output_tokens: 8_000,
            embedding_batch_size: 64,
            max_events_per_drain: 50,
            improve_every: 20,
            lease_stale_seconds: 180,
            max_attempts: 5,
            spool_high_water_bytes: 512 * 1024 * 1024,
        }
    );
}

#[test]
fn resource_values_reject_zero_and_overflow() {
    for (key, value) in [
        ("APEX_COGNEE_MAX_LLM_CALLS", "0"),
        ("APEX_COGNEE_MAX_INPUT_TOKENS", "not-a-number"),
        ("APEX_COGNEE_EMBEDDING_BATCH_SIZE", "4294967296"),
        ("APEX_COGNEE_SPOOL_HIGH_WATER_BYTES", "18446744073709551616"),
    ] {
        let env = fake_env([("HOME", "/home/alice"), (key, value)]);
        let error = AgentConfig::from_env(&env).expect_err("invalid resource value");
        let message = error.to_string();
        assert!(message.contains(key), "{message}");
        assert!(!message.contains(value), "invalid value leaked: {message}");
    }
}

#[test]
fn model_endpoint_credentials_are_safe_in_debug_and_serialization() {
    let env = fake_env([
        ("HOME", "/home/alice"),
        (
            "APEX_COGNEE_LLM_ENDPOINT",
            "https://alice:url-secret@proxy.example/v1?token=query-secret",
        ),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT",
            "https://bob:embedding-secret@proxy.example/v1/embeddings",
        ),
        ("APEX_COGNEE_EMBEDDING_MODEL", "embedding-model"),
    ]);
    let cfg = AgentConfig::from_env(&env).expect("config");

    let debug = format!("{cfg:?}");
    let json = serde_json::to_string(&cfg).expect("serialize config");
    for secret in ["url-secret", "query-secret", "embedding-secret"] {
        assert!(!debug.contains(secret));
        assert!(!json.contains(secret));
    }
}

#[test]
#[cfg(unix)]
fn state_tree_and_module_written_files_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = StateLayout::under(temp.path().join("state"));
    layout.ensure_private().expect("private state tree");
    let status_file = layout.status.join("config-status.json");
    layout
        .write_private_file(&status_file, br#"{"ready":true}"#)
        .expect("private status file");

    visit_paths(&layout.root, &mut |path| {
        let metadata = std::fs::symlink_metadata(path).expect("metadata");
        let mode = metadata.permissions().mode() & 0o777;
        if metadata.is_dir() {
            assert_eq!(mode, 0o700, "directory {}", path.display());
        } else if metadata.is_file() {
            assert_eq!(mode, 0o600, "file {}", path.display());
        }
    });
}

#[test]
#[cfg(all(unix, feature = "runtime"))]
fn state_creation_establishes_process_umask_077() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--ignored",
            "--exact",
            "state_creation_umask_probe_helper",
            "--nocapture",
        ])
        .env("COGNEE_UMASK_PROBE_ROOT", temp.path())
        .output()
        .expect("run isolated umask probe");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "isolated subprocess helper"]
#[cfg(all(unix, feature = "runtime"))]
fn state_creation_umask_probe_helper() {
    let Some(root) = std::env::var_os("COGNEE_UMASK_PROBE_ROOT") else {
        return;
    };
    // SAFETY: This helper runs in a dedicated subprocess with no other test threads.
    unsafe { libc::umask(0) };
    StateLayout::under(PathBuf::from(root))
        .ensure_private()
        .expect("private state tree");
    // SAFETY: The helper owns the subprocess-wide umask.
    let previous = unsafe { libc::umask(0o077) };
    assert_eq!(previous & 0o777, 0o077);
}

#[test]
#[cfg(all(unix, not(feature = "runtime")))]
fn no_default_feature_private_filesystem_apis_remain_available() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = StateLayout::under(temp.path().join("state"));
    layout.ensure_private().expect("private state tree");
    let status_file = layout.status.join("no-default-status.json");
    layout
        .write_private_file(&status_file, br#"{"ready":true}"#)
        .expect("private no-default status file");

    assert_eq!(
        std::fs::metadata(&layout.root)
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(status_file)
            .expect("status metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn private_file_rejects_parent_directory_components() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = StateLayout::under(temp.path().join("state"));
    layout.ensure_private().expect("private state tree");
    let outside = temp.path().join("escaped-status.json");
    let traversal = layout.root.join("status/../../escaped-status.json");

    assert!(layout.write_private_file(&traversal, b"escape").is_err());
    assert!(!outside.exists());
}

#[test]
#[cfg(unix)]
fn private_file_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = StateLayout::under(temp.path().join("state"));
    layout.ensure_private().expect("private state tree");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).expect("outside fixture");
    let escape = layout.status.join("escape");
    symlink(&outside, &escape).expect("symlink fixture");

    assert!(
        layout
            .write_private_file(&escape.join("leaked.json"), b"escape")
            .is_err()
    );
    assert!(!outside.join("leaked.json").exists());
}

#[test]
fn embedding_generation_is_immutable_stable_and_below_the_private_root() {
    let layout = StateLayout::under(PathBuf::from("/private/cognee"));
    let cfg = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_EMBEDDING_PROVIDER", "openai"),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT",
            "https://proxy.example/v1/embeddings?token=secret",
        ),
        ("APEX_COGNEE_EMBEDDING_MODEL", "embed-v1"),
        ("APEX_COGNEE_EMBEDDING_DIMENSIONS", "1536"),
    ]))
    .expect("config");
    let embedding = cfg.embedding.as_ref().expect("embedding");

    let first = EmbeddingGeneration::new(&layout, "generation-001", embedding).expect("generation");
    let second =
        EmbeddingGeneration::new(&layout, "generation-001", embedding).expect("generation");

    assert_eq!(first, second);
    assert_eq!(first.id(), "generation-001");
    assert_eq!(first.fingerprint().provider, "openai");
    assert_eq!(first.fingerprint().endpoint_class, "https://proxy.example");
    assert_eq!(first.fingerprint().model, "embed-v1");
    assert_eq!(first.fingerprint().dimensions, 1536);
    assert_eq!(first.fingerprint().stable_id().len(), 64);
    for path in [first.data(), first.vector(), first.graph()] {
        assert!(path.starts_with(&layout.root), "{}", path.display());
    }
    assert!(first.sqlite_url().starts_with("sqlite:///private/cognee/"));
    let json = serde_json::to_string(&first).expect("generation json");
    assert!(!json.contains("secret"));
}

#[test]
fn embedding_generation_exposes_only_read_access() {
    let source = include_str!("../src/embedding_generation.rs");
    assert!(!source.contains("pub data: PathBuf"));
    assert!(!source.contains("pub vector: PathBuf"));
    assert!(!source.contains("pub graph: PathBuf"));

    let before_struct = source
        .split_once("pub struct EmbeddingGeneration")
        .expect("generation declaration")
        .0;
    let derive = before_struct.rsplit_once("#[derive").expect("derive").1;
    assert!(!derive.contains("Deserialize"));
}

#[test]
fn generation_id_rejects_path_traversal() {
    let layout = StateLayout::under(PathBuf::from("/private/cognee"));
    let cfg = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_EMBEDDING_PROVIDER", "openai"),
        ("APEX_COGNEE_EMBEDDING_ENDPOINT", "https://proxy.example"),
        ("APEX_COGNEE_EMBEDDING_MODEL", "embed-v1"),
        ("APEX_COGNEE_EMBEDDING_DIMENSIONS", "1536"),
    ]))
    .expect("config");
    let embedding = cfg.embedding.as_ref().expect("embedding");

    assert!(EmbeddingGeneration::new(&layout, "../escape", embedding).is_err());
}

#[test]
#[cfg(feature = "engine")]
fn graph_settings_require_complete_models_and_project_explicitly() {
    let incomplete = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_PROXY_KEY", "settings-secret"),
        ("APEX_COGNEE_LLM_MODEL", "gpt-5.4-nano"),
    ]))
    .expect("capture-safe config");
    let placeholder_embedding = cognee_mcp::config::EmbeddingConfig {
        provider: "openai".into(),
        endpoint: "https://proxy.example/v1/embeddings".into(),
        model: "embed-v1".into(),
        dimensions: 1536,
    };
    let placeholder_generation =
        EmbeddingGeneration::new(&incomplete.layout, "generation-001", &placeholder_embedding)
            .expect("generation");
    let error = incomplete
        .cognee_settings(&placeholder_generation)
        .expect_err("incomplete LLM rejected before graph write");
    assert!(error.to_string().contains("APEX_COGNEE_LLM_PROVIDER"));

    let config = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_ROOT", "/private/cognee"),
        ("APEX_COGNEE_PROXY_KEY", "settings-secret"),
        ("APEX_COGNEE_LLM_PROVIDER", "openai"),
        ("APEX_COGNEE_LLM_ENDPOINT", "https://proxy.example/v1"),
        ("APEX_COGNEE_LLM_MODEL", "gpt-5.4-nano"),
        ("APEX_COGNEE_EMBEDDING_PROVIDER", "openai"),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT",
            "https://proxy.example/v1/embeddings",
        ),
        ("APEX_COGNEE_EMBEDDING_MODEL", "embed-v1"),
        ("APEX_COGNEE_EMBEDDING_DIMENSIONS", "1536"),
    ]))
    .expect("complete config");
    let generation = EmbeddingGeneration::new(
        &config.layout,
        "generation-001",
        config.embedding.as_ref().expect("embedding"),
    )
    .expect("generation");

    let settings = config.cognee_settings(&generation).expect("settings");

    assert_eq!(settings.system_root_directory, "/private/cognee/system");
    assert_eq!(
        settings.data_root_directory,
        generation.data().display().to_string()
    );
    assert_eq!(settings.cache_root_directory, "/private/cognee/cache");
    assert_eq!(settings.logs_root_directory, "/private/cognee/status/logs");
    assert_eq!(settings.db_provider, "sqlite");
    assert_eq!(settings.relational_db_url, generation.sqlite_url());
    assert_eq!(settings.vector_db_provider, "lancedb");
    assert_eq!(
        settings.vector_db_url,
        generation.vector().display().to_string()
    );
    assert_eq!(settings.graph_database_provider, "ladybug");
    assert_eq!(
        settings.graph_file_path,
        generation.graph().display().to_string()
    );
    assert_eq!(settings.cache_backend, "seaorm");
    assert_eq!(settings.default_dataset_name, "agent_sessions");
    assert_eq!(settings.llm_provider, "openai");
    assert_eq!(settings.llm_model, "gpt-5.4-nano");
    assert_eq!(settings.llm_endpoint, "https://proxy.example/v1");
    assert_eq!(settings.llm_api_key, "settings-secret");
    assert!(settings.user_agent.starts_with("Apex/"));
    assert_eq!(settings.llm_max_parallel_requests, 1);
    assert_eq!(settings.llm_max_retries, 0);
    assert_eq!(settings.embedding_provider, "openai");
    assert_eq!(settings.embedding_model_name, "embed-v1");
    assert_eq!(settings.embedding_dimensions, 1536);
    assert_eq!(
        settings.embedding_endpoint,
        "https://proxy.example/v1/embeddings"
    );
    assert_eq!(settings.embedding_api_key, "settings-secret");
    assert_eq!(settings.embedding_batch_size, 64);
    let backend = settings.backend_context();
    assert_eq!(
        backend.llm.user_agent.as_deref(),
        Some(settings.user_agent.as_str())
    );
    assert_eq!(
        backend.embedding.user_agent.as_deref(),
        Some(settings.user_agent.as_str())
    );
}

#[test]
#[cfg(feature = "engine")]
fn graph_settings_reject_a_generation_from_another_private_root() {
    let config = AgentConfig::from_env(&fake_env([
        ("HOME", "/home/alice"),
        ("APEX_COGNEE_ROOT", "/private/cognee"),
        ("APEX_COGNEE_PROXY_KEY", "settings-secret"),
        ("APEX_COGNEE_LLM_PROVIDER", "openai"),
        ("APEX_COGNEE_LLM_ENDPOINT", "https://proxy.example/v1"),
        ("APEX_COGNEE_LLM_MODEL", "gpt-5.4-nano"),
        ("APEX_COGNEE_EMBEDDING_PROVIDER", "openai"),
        (
            "APEX_COGNEE_EMBEDDING_ENDPOINT",
            "https://proxy.example/v1/embeddings",
        ),
        ("APEX_COGNEE_EMBEDDING_MODEL", "embed-v1"),
        ("APEX_COGNEE_EMBEDDING_DIMENSIONS", "1536"),
    ]))
    .expect("complete config");
    let other_layout = StateLayout::under(PathBuf::from("/private/other-cognee"));
    let generation = EmbeddingGeneration::new(
        &other_layout,
        "generation-001",
        config.embedding.as_ref().expect("embedding"),
    )
    .expect("generation");

    let error = match config.cognee_settings(&generation) {
        Err(error) => error,
        Ok(_) => panic!("cross-layout generation must be rejected"),
    };

    assert!(error.to_string().contains("private root"));
}

fn visit_paths(path: &Path, visitor: &mut impl FnMut(&Path)) {
    visitor(path);
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read directory") {
            visit_paths(&entry.expect("directory entry").path(), visitor);
        }
    }
}
