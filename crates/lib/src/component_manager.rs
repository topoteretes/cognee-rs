//! ComponentManager: lazy-initializing, shared component store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock as TokioRwLock;

use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine, EmbeddingProvider};
use cognee_graph::GraphDBTrait;
#[cfg(feature = "ladybug")]
use cognee_graph::LadybugAdapter;
#[cfg(feature = "pggraph")]
use cognee_graph::PgGraphAdapter;
use cognee_llm::{Llm, OpenAIAdapter, Transcriber};
use cognee_storage::{LocalStorage, StorageTrait};
#[cfg(feature = "pgvector")]
use cognee_vector::PgVectorAdapter;
use cognee_vector::{BruteForceVectorDB, VectorDB};

use crate::config::{ConfigManager, Settings};
use crate::context::PipelineContext;
use crate::error::ComponentError;

/// Assemble a `postgres://user:pass@host:port/dbname` URL with percent-encoded
/// credentials. Shared by the vector and graph URL resolvers.
#[cfg(any(feature = "pgvector", feature = "pggraph"))]
fn build_postgres_url(
    host: &str,
    port: u16,
    name: &str,
    user: &str,
    pass: &str,
) -> Result<String, String> {
    #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
    let mut parsed = url::Url::parse("postgres://localhost").expect("static URL is always valid");
    parsed
        .set_host(Some(host))
        .map_err(|e| format!("invalid host '{host}': {e}"))?;
    parsed
        .set_port(Some(port))
        .map_err(|_| format!("invalid port {port}"))?;
    parsed.set_path(&format!("/{name}"));
    parsed
        .set_username(user)
        .map_err(|_| format!("invalid username '{user}'"))?;
    parsed
        .set_password(Some(pass))
        .map_err(|_| "invalid password".to_string())?;
    Ok(parsed.to_string())
}

/// Manages shared, lazily-initialized pipeline components.
///
/// Each component is created on first access and cached for subsequent calls.
/// When the underlying [`ConfigManager`]'s version advances (due to a setter
/// call), cached components are lazily re-created on the next access.
///
/// Constructed from [`ConfigManager`] — typically loaded once via
/// `ConfigManager::from_env()` or `ConfigManager::new(settings)`.
pub struct ComponentManager {
    config: ConfigManager,
    // Each cached component stores (version_at_creation, component_arc).
    // When the config version advances past the cached version, the
    // component is lazily re-created on next access.
    storage: TokioRwLock<Option<(u64, Arc<dyn StorageTrait>)>>,
    database: TokioRwLock<Option<(u64, Arc<DatabaseConnection>)>>,
    graph_db: TokioRwLock<Option<(u64, Arc<dyn GraphDBTrait>)>>,
    vector_db: TokioRwLock<Option<(u64, Arc<dyn VectorDB>)>>,
    embedding_engine: TokioRwLock<Option<(u64, Arc<dyn EmbeddingEngine>)>>,
    llm: TokioRwLock<Option<(u64, Arc<dyn Llm>)>>,
    // Stores Option<Arc<dyn Transcriber>>: None when the provider does not
    // support transcription (e.g. litert). The outer Option<(ver, ...)> is
    // the version-keyed cache envelope.
    #[allow(clippy::type_complexity)]
    transcriber: TokioRwLock<Option<(u64, Option<Arc<dyn Transcriber>>)>>,
}

impl ComponentManager {
    pub fn new(config: ConfigManager) -> Self {
        Self {
            config,
            storage: TokioRwLock::new(None),
            database: TokioRwLock::new(None),
            graph_db: TokioRwLock::new(None),
            vector_db: TokioRwLock::new(None),
            embedding_engine: TokioRwLock::new(None),
            llm: TokioRwLock::new(None),
            transcriber: TokioRwLock::new(None),
        }
    }

    /// Read-only snapshot of current settings.
    ///
    /// Returns a `RwLockReadGuard` that auto-derefs to `&Settings`.
    /// Most call sites that use `cm.settings().field_name` work unchanged.
    pub fn settings(&self) -> std::sync::RwLockReadGuard<'_, Settings> {
        self.config.read()
    }

    /// Access the underlying [`ConfigManager`] for runtime mutation.
    pub fn config(&self) -> &ConfigManager {
        &self.config
    }

    async fn init_storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let data_root = self.config.read().data_root_directory.clone();
        let storage = LocalStorage::new(PathBuf::from(&data_root));
        storage
            .initialize()
            .await
            .map_err(|e| ComponentError::Storage(format!("initialization failed: {e}")))?;
        Ok(Arc::new(storage))
    }

    async fn init_database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        let url = self.config.read().resolved_relational_db_url();

        // For SQLite file-backed databases, ensure the parent directory exists
        // before handing the URL to sea-orm.  sea-orm's `?mode=rwc` creates the
        // *file* but not missing ancestor directories, so without this step any
        // settings override that redirects the DB to a new path (e.g. per-test
        // isolation) would fail with "unable to open database file".
        //
        // URL shapes we handle:
        //   sqlite:./rel/path/db       (relative, 1-slash)
        //   sqlite:///abs/path/db      (absolute, 3-slash)
        //   sqlite://localhost/abs/db  (host form)
        // All others (postgres, in-memory `sqlite::memory:`) are left alone.
        if url.starts_with("sqlite:") && !url.contains(":memory:") {
            // Strip the sqlite: scheme and any leading host ("//localhost") or
            // extra slashes to get the raw filesystem path (before '?').
            let after_scheme = url.trim_start_matches("sqlite:");
            let path_part = if after_scheme.starts_with("//localhost/") {
                Some(&after_scheme["//localhost".len()..])
            } else if after_scheme.starts_with("///") {
                // sqlite:///abs/path — empty authority, absolute path.
                Some(&after_scheme[2..])
            } else if after_scheme.starts_with("//") {
                // sqlite://somehost/... — genuine host form; leave entirely to
                // the driver instead of attempting create_dir_all("//somehost").
                None
            } else {
                Some(after_scheme)
            };
            // Drop query string (e.g. ?mode=rwc).
            if let Some(path_part) = path_part {
                let path_no_query = path_part.split('?').next().unwrap_or(path_part);
                let db_path = Path::new(path_no_query);
                if let Some(parent) = db_path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    // Non-fatal: an unusual-but-driver-valid URL must still
                    // reach sea-orm and surface the driver's own error.
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        tracing::warn!(
                            "could not create SQLite parent directory '{}': {e}",
                            parent.display()
                        );
                    }
                }
            }
        }

        let db = connect(&url)
            .await
            .map_err(|e| ComponentError::Database(format!("initialization failed: {e}")))?;
        initialize(&db)
            .await
            .map_err(|e| ComponentError::Database(format!("schema initialization failed: {e}")))?;
        Ok(Arc::new(db))
    }

    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let provider = self.config.read().graph_database_provider.to_lowercase();

        match provider.as_str() {
            "ladybug" | "kuzu" => self.init_ladybug_graph_db().await,

            #[cfg(feature = "pggraph")]
            "postgres" | "postgresql" => {
                let url = {
                    let s = self.config.read();
                    self.resolved_graph_db_url(&s)?
                };
                let adapter = PgGraphAdapter::new(&url)
                    .await
                    .map_err(|e| ComponentError::GraphDb(format!("pggraph init failed: {e}")))?;
                Ok(Arc::new(adapter))
            }

            #[cfg(not(feature = "pggraph"))]
            "postgres" | "postgresql" => Err(ComponentError::Config(
                "graph_database_provider=postgres requires the `pggraph` crate feature".into(),
            )),

            other => Err(ComponentError::Config(format!(
                "Unsupported graph_database_provider '{other}'. Supported: ladybug, kuzu, postgres.",
            ))),
        }
    }

    async fn init_ladybug_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let graph_path = {
            let s = self.config.read();
            if !s.graph_file_path.is_empty() {
                s.graph_file_path.clone()
            } else {
                format!("{}/graph", s.system_root_directory)
            }
        };

        if let Some(parent) = Path::new(&graph_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        #[cfg(feature = "ladybug")]
        {
            let graph_db = LadybugAdapter::new(&graph_path)
                .await
                .map_err(|e| ComponentError::GraphDb(format!("initialization failed: {e}")))?;
            graph_db.initialize().await.map_err(|e| {
                ComponentError::GraphDb(format!("schema initialization failed: {e}"))
            })?;
            Ok(Arc::new(graph_db))
        }

        #[cfg(not(feature = "ladybug"))]
        Err(ComponentError::Config(
            "graph_database_provider=ladybug requires the `ladybug` crate feature".to_string(),
        ))
    }

    /// Build a Postgres connection URL from the graph_database_* settings,
    /// falling back to the relational db_* fields when graph-specific creds
    /// are not fully configured (Python `get_graph_engine.py:332-367` parity).
    ///
    /// Precedence:
    /// 1. `graph_database_url` already looks like a full `postgres://` URL → return as-is.
    /// 2. All graph-specific fields are set (username, password, host, port, name) → build from those.
    /// 3. Fall back to the relational `db_*` fields with a warning.
    /// 4. Neither complete → error.
    #[cfg(feature = "pggraph")]
    fn resolved_graph_db_url(&self, s: &Settings) -> Result<String, ComponentError> {
        if s.graph_database_url.starts_with("postgres://")
            || s.graph_database_url.starts_with("postgresql://")
        {
            return Ok(s.graph_database_url.clone());
        }

        let graph_host = if s.graph_database_host.is_empty() {
            None
        } else {
            Some(s.graph_database_host.as_str())
        };

        let graph_creds_complete = graph_host.is_some()
            && !s.graph_database_username.is_empty()
            && !s.graph_database_name.is_empty();

        let (host, port, name, user, pass) = if graph_creds_complete {
            (
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                graph_host.expect("checked above"),
                s.graph_database_port,
                s.graph_database_name.as_str(),
                s.graph_database_username.as_str(),
                s.graph_database_password.as_str(),
            )
        } else {
            tracing::warn!(
                "Postgres graph credentials not fully configured; falling back to the \
                 relational database configuration. Set GRAPH_DATABASE_* explicitly to avoid this."
            );
            if s.db_host.is_empty() || s.db_name.is_empty() || s.db_username.is_empty() {
                return Err(ComponentError::Config(
                    "Missing required Postgres graph credentials".into(),
                ));
            }
            (
                s.db_host.as_str(),
                s.db_port,
                s.db_name.as_str(),
                s.db_username.as_str(),
                s.db_password.as_str(),
            )
        };

        build_postgres_url(host, port, name, user, pass)
            .map_err(|e| ComponentError::Config(format!("failed to build graph DB URL: {e}")))
    }

    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        // Clone all needed fields out of the read guard before any await.
        let (provider, _dim) = {
            let s = self.config.read();
            (
                s.vector_db_provider.to_lowercase(),
                s.embedding_dimensions as usize,
            )
        };

        match provider.as_str() {
            "pgvector" => {
                #[cfg(feature = "pgvector")]
                {
                    let url = {
                        let s = self.config.read();
                        self.resolved_vector_db_url(&s)?
                    };
                    let adapter = PgVectorAdapter::new(&url, _dim).await.map_err(|e| {
                        ComponentError::VectorDb(format!("pgvector init failed: {e}"))
                    })?;
                    Ok(Arc::new(adapter))
                }

                #[cfg(not(feature = "pgvector"))]
                Err(ComponentError::Config(
                    "vector_db_provider=pgvector requires the `pgvector` crate feature".to_string(),
                ))
            }
            // Pure-Rust in-memory brute-force backend (OSS edge/Android default).
            "brute-force" | "brute_force" | "bruteforce" => Ok(Arc::new(BruteForceVectorDB::new())),
            // T4-move removed the Qdrant + LanceDB adapters from OSS. Rather
            // than hard-error, fall back to the in-memory brute-force backend
            // so existing configs keep booting. Operators get a `warn!` line
            // telling them what happened and how to silence it.
            "qdrant" | "lancedb" => {
                tracing::warn!(
                    provider = %provider,
                    "vector_db_provider='{provider}' is no longer available in OSS; \
                     falling back to in-memory brute-force. Set vector_db_provider='pgvector' \
                     for production, or 'brute-force' to silence this warning.",
                );
                Ok(Arc::new(BruteForceVectorDB::new()))
            }
            #[cfg(feature = "testing")]
            "mock" => Ok(Arc::new(cognee_vector::MockVectorDB::new())),
            other => Err(ComponentError::Config(format!(
                "Unsupported vector_db_provider '{other}'. \
                 Supported: pgvector, brute-force, mock (testing feature only).",
            ))),
        }
    }

    /// Build a Postgres connection URL from the vector_db_* settings.
    ///
    /// If `vector_db_url` already looks like a full `postgres://` URL it is
    /// returned as-is. Otherwise the URL is assembled from the individual
    /// `vector_db_*` / `db_*` fields using the `url` crate so that special
    /// characters in passwords are percent-encoded correctly.
    #[cfg(feature = "pgvector")]
    fn resolved_vector_db_url(&self, settings: &Settings) -> Result<String, ComponentError> {
        if settings.vector_db_url.starts_with("postgres://")
            || settings.vector_db_url.starts_with("postgresql://")
        {
            return Ok(settings.vector_db_url.clone());
        }

        let host = if settings.vector_db_url.is_empty() {
            "localhost"
        } else {
            &settings.vector_db_url
        };
        let port = settings.vector_db_port;
        let name = if settings.vector_db_name.is_empty() {
            "cognee_vectors"
        } else {
            &settings.vector_db_name
        };
        let user = if settings.db_username.is_empty() {
            "postgres"
        } else {
            &settings.db_username
        };
        let pass = &settings.db_password;

        build_postgres_url(host, port, name, user, pass)
            .map_err(|e| ComponentError::Config(format!("failed to build vector DB URL: {e}")))
    }

    /// Initialize the embedding engine from Settings fields instead of
    /// calling `EmbeddingConfig::from_env()` directly.
    ///
    /// This ensures that runtime config changes via `ConfigManager` flow
    /// through to the embedding engine.
    async fn init_embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        // Build EmbeddingConfig from Settings inside a block so the guard
        // is dropped before any .await point.
        let mut config = {
            let settings = self.config.read();

            // Map Settings.embedding_provider string to EmbeddingProvider enum.
            let provider_str = settings.embedding_provider.trim().to_lowercase();
            let provider = match provider_str.as_str() {
                "onnx" => EmbeddingProvider::Onnx,
                "fastembed" => EmbeddingProvider::Fastembed,
                "openai" => EmbeddingProvider::OpenAi,
                "openai_compatible" => EmbeddingProvider::OpenAiCompatible,
                "ollama" => EmbeddingProvider::Ollama,
                "mock" => EmbeddingProvider::Mock,
                _ => EmbeddingProvider::Onnx,
            };

            // Endpoint/key fall back to the LLM provider's when no embedding-
            // specific values are set. The default embedding provider is OpenAI
            // (off-Android), and cognee typically shares one OpenAI-compatible
            // account for chat + embeddings, so this makes the default work with
            // just OPENAI_URL/OPENAI_TOKEN (→ llm_endpoint/llm_api_key) — no
            // separate EMBEDDING_ENDPOINT/EMBEDDING_API_KEY required.
            let endpoint = [&settings.embedding_endpoint, &settings.llm_endpoint]
                .into_iter()
                .find(|v| !v.is_empty())
                .cloned();

            let api_key = [&settings.embedding_api_key, &settings.llm_api_key]
                .into_iter()
                .find(|v| !v.is_empty())
                .cloned();

            // Check MOCK_EMBEDDING env var as a fallback (preserves backward compat).
            // `deterministic`/`hash` selects SHA-256-derived vectors; other truthy
            // values keep the legacy zero-vector mode.
            let mock_mode = std::env::var("MOCK_EMBEDDING")
                .ok()
                .map(|v| v.trim().to_lowercase());
            let mock_deterministic =
                matches!(mock_mode.as_deref(), Some("deterministic") | Some("hash"));
            let mock = mock_deterministic
                || matches!(mock_mode.as_deref(), Some("true") | Some("1") | Some("yes"));
            let mock_mode = if mock_deterministic {
                cognee_embedding::MockVectorMode::Deterministic
            } else {
                cognee_embedding::MockVectorMode::Zero
            };

            EmbeddingConfig {
                provider: if mock {
                    EmbeddingProvider::Mock
                } else {
                    provider
                },
                model: settings.embedding_model_name.clone(),
                dimensions: settings.embedding_dimensions as usize,
                endpoint,
                api_key,
                api_version: None,
                max_completion_tokens: 8191,
                batch_size: settings.embedding_batch_size as usize,
                mock,
                mock_mode,
                #[cfg(feature = "onnx")]
                onnx: cognee_embedding::OnnxEmbeddingConfig {
                    model_path: PathBuf::from(&settings.embedding_model_path),
                    tokenizer_path: PathBuf::from(&settings.embedding_tokenizer_path),
                    model_name: settings.embedding_model_name.clone(),
                    dimensions: settings.embedding_dimensions as usize,
                    max_sequence_length: settings.embedding_max_sequence_length as usize,
                    batch_size: settings.embedding_batch_size as usize,
                },
                huggingface_tokenizer: None,
            }
        };
        // settings guard is now dropped — safe to await.

        // Still check env vars for fields not yet in Settings (api_version,
        // huggingface_tokenizer, max_completion_tokens) — forward compatibility.
        if let Ok(val) = std::env::var("EMBEDDING_API_VERSION") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.api_version = Some(val);
            }
        }
        if let Ok(val) = std::env::var("HUGGINGFACE_TOKENIZER") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.huggingface_tokenizer = Some(val);
            }
        }
        if let Ok(val) = std::env::var("EMBEDDING_MAX_COMPLETION_TOKENS")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            config.max_completion_tokens = n;
        }

        config.create_engine().await.map_err(|e| {
            ComponentError::EmbeddingEngine(format!("embedding engine init failed: {e}"))
        })
    }

    async fn init_llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        // Clone all needed fields out of the read guard before any await.
        let (
            provider,
            llm_model,
            llm_api_key,
            llm_endpoint,
            llm_max_retries,
            llm_mock,
            llm_cassette,
            llm_record_path,
        ) = {
            let s = self.config.read();
            (
                s.llm_provider.to_lowercase(),
                s.llm_model.clone(),
                s.llm_api_key.clone(),
                s.llm_endpoint.clone(),
                s.llm_max_retries,
                s.llm_mock,
                s.llm_cassette.clone(),
                s.llm_record_path.clone(),
            )
        };

        // `llm_cassette` is only consumed on the mock path; silence the
        // unused-variable lint in builds without the `mock` feature.
        #[cfg(not(feature = "mock-llm"))]
        let _ = &llm_cassette;

        // Mock first — like MOCK_EMBEDDING, this overrides the configured
        // provider. Selected by `MOCK_LLM` (llm_mock) or `llm_provider=mock`.
        if llm_mock || provider == "mock" {
            #[cfg(feature = "mock-llm")]
            {
                let cassette = llm_cassette.trim();
                if cassette.is_empty() {
                    return Err(ComponentError::Config(
                        "MOCK_LLM is set but MOCK_LLM_CASSETTE is empty; set it to a cassette path"
                            .to_string(),
                    ));
                }
                let replay = cognee_llm::mock::ReplayLlm::from_path(cassette)
                    .map_err(|e| ComponentError::Llm(format!("mock cassette load failed: {e}")))?;
                return Ok(Arc::new(replay));
            }
            #[cfg(not(feature = "mock-llm"))]
            {
                return Err(ComponentError::Config(
                    "MOCK_LLM was requested but the mock LLM is unavailable; \
                     rebuild with the `mock-llm` feature"
                        .to_string(),
                ));
            }
        }

        // Build the real adapter exactly as before.
        let adapter: Arc<dyn Llm> = match provider.as_str() {
            "openai" => {
                if llm_api_key.is_empty() {
                    return Err(ComponentError::Config(
                        "llm_api_key must be configured".to_string(),
                    ));
                }

                let endpoint = if llm_endpoint.is_empty() {
                    None
                } else {
                    Some(llm_endpoint)
                };

                let retries = llm_max_retries.max(1);

                let adapter = OpenAIAdapter::new(llm_model, llm_api_key, endpoint)
                    .map_err(|e| ComponentError::Llm(format!("initialization failed: {e}")))?
                    .with_structured_output_retries(retries)
                    .with_network_retries(retries);

                Arc::new(adapter)
            }
            "litert" => {
                return Err(ComponentError::Config(
                    "llm_provider=litert is not available in this build. \
                     The LiteRT adapter has been extracted to the closed cognee-llm-litert crate."
                        .to_string(),
                ));
            }
            _ => {
                return Err(ComponentError::Config(format!(
                    "Unsupported llm_provider '{provider}'. Supported: openai, mock.",
                )));
            }
        };

        // Optional recording wrap (`COGNEE_RECORD_LLM`). Only the real adapter is
        // worth recording — replaying a recording of a mock is pointless.
        if !llm_record_path.trim().is_empty() {
            #[cfg(feature = "mock-llm")]
            {
                let recorder = cognee_llm::mock::RecordingLlm::new(
                    adapter,
                    llm_record_path.trim().to_string(),
                );
                return Ok(Arc::new(recorder));
            }
            #[cfg(not(feature = "mock-llm"))]
            {
                return Err(ComponentError::Config(
                    "COGNEE_RECORD_LLM was set but LLM recording is unavailable; \
                     rebuild with the `mock-llm` feature"
                        .to_string(),
                ));
            }
        }

        Ok(adapter)
    }

    async fn init_transcriber(&self) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        let (provider, llm_model, llm_api_key, llm_endpoint, llm_max_retries) = {
            let s = self.config.read();
            (
                s.llm_provider.to_lowercase(),
                s.llm_model.clone(),
                s.llm_api_key.clone(),
                s.llm_endpoint.clone(),
                s.llm_max_retries,
            )
        };

        match provider.as_str() {
            "openai" => {
                if llm_api_key.is_empty() {
                    return Err(ComponentError::Config(
                        "llm_api_key must be configured".to_string(),
                    ));
                }

                let endpoint = if llm_endpoint.is_empty() {
                    None
                } else {
                    Some(llm_endpoint)
                };

                let retries = llm_max_retries.max(1);

                let adapter = OpenAIAdapter::new(llm_model, llm_api_key, endpoint)
                    .map_err(|e| ComponentError::Llm(format!("initialization failed: {e}")))?
                    .with_structured_output_retries(retries)
                    .with_network_retries(retries);

                Ok(Some(Arc::new(adapter) as Arc<dyn Transcriber>))
            }
            // litert and any future providers that do not implement Transcriber
            // return None — audio stays gracefully unsupported (D5).
            _ => Ok(None),
        }
    }

    /// Return the [`Transcriber`] for the configured LLM provider, if supported.
    ///
    /// Returns `Ok(Some(_))` for OpenAI (Whisper). Returns `Ok(None)` for
    /// providers that do not support audio transcription (e.g. `litert`), so
    /// callers can skip registering the `AudioLoader` rather than failing.
    pub async fn transcriber(&self) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        let current_ver = self.config.version();
        // Fast path: read lock
        {
            let guard = self.transcriber.read().await;
            if let Some((ver, ref opt)) = *guard
                && ver == current_ver
            {
                return Ok(opt.clone());
            }
        }
        // Slow path: write lock with double-check
        let mut guard = self.transcriber.write().await;
        if let Some((ver, ref opt)) = *guard
            && ver == current_ver
        {
            return Ok(opt.clone());
        }
        let new = self.init_transcriber().await?;
        *guard = Some((current_ver, new.clone()));
        Ok(new)
    }
}

// Versioned accessor helper macro — avoids repeating the double-checked
// locking pattern for each component.
macro_rules! versioned_accessor {
    ($self:ident, $field:ident, $init_fn:ident) => {{
        let current_ver = $self.config.version();
        // Fast path: read lock to check cache hit
        {
            let guard = $self.$field.read().await;
            if let Some((ver, ref component)) = *guard {
                if ver == current_ver {
                    return Ok(Arc::clone(component));
                }
            }
        }
        // Slow path: write lock to reinitialize
        let mut guard = $self.$field.write().await;
        // Double-check (another task may have reinitialized while we waited)
        if let Some((ver, ref component)) = *guard {
            if ver == current_ver {
                return Ok(Arc::clone(component));
            }
        }
        let new = $self.$init_fn().await?;
        *guard = Some((current_ver, Arc::clone(&new)));
        Ok(new)
    }};
}

#[async_trait]
impl PipelineContext for ComponentManager {
    async fn storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        versioned_accessor!(self, storage, init_storage)
    }

    async fn database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        versioned_accessor!(self, database, init_database)
    }

    async fn graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        versioned_accessor!(self, graph_db, init_graph_db)
    }

    async fn vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        versioned_accessor!(self, vector_db, init_vector_db)
    }

    async fn embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        versioned_accessor!(self, embedding_engine, init_embedding_engine)
    }

    async fn llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        versioned_accessor!(self, llm, init_llm)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::config::{ConfigManager, Settings};

    fn cm_with_provider(provider: &str) -> ComponentManager {
        let settings = Settings {
            llm_provider: provider.to_string(),
            llm_api_key: "sk-test".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
            ..Settings::default()
        };
        ComponentManager::new(ConfigManager::new(settings))
    }

    #[tokio::test]
    async fn transcriber_returns_some_for_openai() {
        let cm = cm_with_provider("openai");
        let result = cm
            .transcriber()
            .await
            .expect("transcriber() should not error");
        assert!(
            result.is_some(),
            "openai provider must yield Some(transcriber)"
        );
    }

    #[tokio::test]
    async fn transcriber_returns_none_for_unknown_provider() {
        // Any non-openai provider (e.g. "mock") returns None — audio gracefully unsupported.
        let settings = Settings {
            llm_provider: "mock".to_string(),
            llm_api_key: String::new(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let result = cm
            .transcriber()
            .await
            .expect("transcriber() should not error for mock");
        assert!(result.is_none(), "non-openai provider must yield None");
    }

    #[tokio::test]
    async fn transcriber_is_cached_across_calls() {
        let cm = cm_with_provider("openai");
        let first = cm.transcriber().await.expect("first call").unwrap();
        let second = cm.transcriber().await.expect("second call").unwrap();
        // Both calls return an Arc pointing to the same allocation.
        assert!(Arc::ptr_eq(&first, &second), "transcriber should be cached");
    }

    // -- resolved_graph_db_url / PgGraph provider dispatch --------------------

    #[cfg(feature = "pggraph")]
    fn cm_with_graph_settings(settings: Settings) -> ComponentManager {
        ComponentManager::new(ConfigManager::new(settings))
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_db_url_returns_explicit_url_as_is() {
        let settings = Settings {
            graph_database_url: "postgres://user:pw@myhost:5432/graphs".to_string(),
            ..Settings::default()
        };
        let cm = cm_with_graph_settings(settings.clone());
        let url = cm
            .resolved_graph_db_url(&settings)
            .expect("should succeed with full URL");
        assert_eq!(url, "postgres://user:pw@myhost:5432/graphs");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_db_url_builds_from_graph_creds() {
        let settings = Settings {
            graph_database_host: "graphhost".to_string(),
            graph_database_port: 5432,
            graph_database_name: "mygraph".to_string(),
            graph_database_username: "guser".to_string(),
            graph_database_password: "gpass".to_string(),
            ..Settings::default()
        };
        let cm = cm_with_graph_settings(settings.clone());
        let url = cm
            .resolved_graph_db_url(&settings)
            .expect("should build from graph creds");
        assert!(url.contains("guser"), "URL should contain username");
        assert!(url.contains("graphhost"), "URL should contain host");
        assert!(url.contains("mygraph"), "URL should contain db name");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_db_url_falls_back_to_relational_creds() {
        // Graph creds not set, relational creds are set → fallback.
        let settings = Settings {
            db_host: "relhost".to_string(),
            db_port: 5432,
            db_name: "reldb".to_string(),
            db_username: "reluser".to_string(),
            db_password: "relpass".to_string(),
            ..Settings::default()
        };
        let cm = cm_with_graph_settings(settings.clone());
        let url = cm
            .resolved_graph_db_url(&settings)
            .expect("should fall back to relational creds");
        assert!(
            url.contains("reluser"),
            "URL should contain relational username"
        );
        assert!(
            url.contains("relhost"),
            "URL should contain relational host"
        );
        assert!(
            url.contains("reldb"),
            "URL should contain relational db name"
        );
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_db_url_errors_when_no_creds() {
        // Neither graph nor relational creds → config error.
        let settings = Settings {
            db_host: String::new(),
            db_name: String::new(),
            db_username: String::new(),
            ..Settings::default()
        };
        let cm = cm_with_graph_settings(settings.clone());
        let result = cm.resolved_graph_db_url(&settings);
        assert!(result.is_err(), "should error when no creds available");
    }

    #[tokio::test]
    async fn init_graph_db_rejects_unsupported_provider() {
        let settings = Settings {
            graph_database_provider: "neo4j".to_string(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let result = cm.graph_db().await;
        assert!(result.is_err());
        let err_msg = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err_msg.contains("postgres"),
            "error message should list 'postgres' as supported: {err_msg}"
        );
    }

    // -- Mock LLM factory wiring (MOCK_LLM / COGNEE_RECORD_LLM) ----------------

    /// Write a minimal valid cassette to a temp file and return (dir, path).
    #[cfg(feature = "mock-llm")]
    fn write_cassette() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cassette.json");
        // A schema-valid, empty cassette: the replay mock falls back to its
        // default EmptyGraph miss policy, so the pipeline runs with no entries.
        let body = r#"{"version":1,"model":"mock-model","entries":{}}"#;
        std::fs::write(&path, body).expect("write cassette");
        (dir, path)
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_uses_replay_mock_when_llm_mock_set_without_api_key() {
        let (_dir, cassette) = write_cassette();
        // No api_key and a non-openai-ready config — the mock must override.
        let settings = Settings {
            llm_mock: true,
            llm_cassette: cassette.to_string_lossy().into_owned(),
            llm_api_key: String::new(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm.llm().await.expect("mock llm should initialize offline");
        assert_eq!(
            llm.model(),
            "mock-model",
            "replay mock reports cassette model"
        );
        // A generate() call must succeed offline (empty response on cache miss).
        let resp = llm
            .generate(
                vec![cognee_llm::Message {
                    role: cognee_llm::MessageRole::User,
                    content: "hello".to_string(),
                }],
                None,
            )
            .await
            .expect("offline generate should succeed");
        assert_eq!(resp.model, "mock-model");
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_selects_mock_when_provider_is_mock() {
        let (_dir, cassette) = write_cassette();
        let settings = Settings {
            llm_provider: "mock".to_string(),
            llm_cassette: cassette.to_string_lossy().into_owned(),
            llm_api_key: String::new(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let llm = cm
            .llm()
            .await
            .expect("provider=mock should initialize offline");
        assert_eq!(llm.model(), "mock-model");
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_errors_when_mock_set_but_cassette_empty() {
        let settings = Settings {
            llm_mock: true,
            llm_cassette: String::new(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        let err = match cm.llm().await {
            Err(e) => e,
            Ok(_) => panic!("empty cassette must error"),
        };
        assert!(
            err.to_string().contains("MOCK_LLM_CASSETTE"),
            "error should mention the missing cassette env: {err}"
        );
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_wraps_real_adapter_in_recorder_when_record_path_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record_path = dir.path().join("recorded.json");
        // Real openai provider + a record path → wrapped in RecordingLlm.
        let settings = Settings {
            llm_provider: "openai".to_string(),
            llm_api_key: "sk-test".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
            llm_record_path: record_path.to_string_lossy().into_owned(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
        // Construction must succeed (no network call happens at init time);
        // the recorder model() delegates to the wrapped openai adapter.
        let llm = cm
            .llm()
            .await
            .expect("recording wrap should initialize without network");
        assert_eq!(
            llm.model(),
            "gpt-4o-mini",
            "recorder delegates model() to the wrapped adapter"
        );
    }
}
