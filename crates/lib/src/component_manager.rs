//! ComponentManager: lazy-initializing, shared component store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock as TokioRwLock;
use tracing::warn;

use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine, EmbeddingProvider};
use cognee_graph::GraphDBTrait;
#[cfg(feature = "ladybug")]
use cognee_graph::LadybugAdapter;
#[cfg(all(feature = "android-litert", target_os = "android"))]
use cognee_llm::LiteRtAdapter;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_storage::{LocalStorage, StorageTrait};
#[cfg(feature = "pgvector")]
use cognee_vector::PgVectorAdapter;
#[cfg(feature = "qdrant")]
use cognee_vector::QdrantAdapter;
use cognee_vector::VectorDB;

use crate::config::{ConfigManager, Settings};
use crate::context::PipelineContext;
use crate::error::ComponentError;

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
        let db = connect(&url)
            .await
            .map_err(|e| ComponentError::Database(format!("initialization failed: {e}")))?;
        initialize(&db)
            .await
            .map_err(|e| ComponentError::Database(format!("schema initialization failed: {e}")))?;
        Ok(Arc::new(db))
    }

    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let (provider, graph_path) = {
            let s = self.config.read();
            let provider = s.graph_database_provider.to_lowercase();
            if provider != "ladybug" && provider != "kuzu" {
                return Err(ComponentError::Config(format!(
                    "Unsupported graph_database_provider '{}'. Supported: ladybug, kuzu.",
                    s.graph_database_provider
                )));
            }
            let graph_path = if !s.graph_file_path.is_empty() {
                s.graph_file_path.clone()
            } else {
                format!("{}/graph", s.system_root_directory)
            };
            (provider, graph_path)
        };
        // settings guard is now dropped — safe to await.
        let _ = provider; // suppress unused-variable warning when ladybug is not enabled

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

    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        // Clone all needed fields out of the read guard before any await.
        let (provider, dim, vector_db_url, system_root_dir) = {
            let s = self.config.read();
            (
                s.vector_db_provider.to_lowercase(),
                s.embedding_dimensions as usize,
                s.vector_db_url.clone(),
                s.system_root_directory.clone(),
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
                    let adapter = PgVectorAdapter::new(&url, dim).await.map_err(|e| {
                        ComponentError::VectorDb(format!("pgvector init failed: {e}"))
                    })?;
                    Ok(Arc::new(adapter))
                }

                #[cfg(not(feature = "pgvector"))]
                Err(ComponentError::Config(
                    "vector_db_provider=pgvector requires the `pgvector` crate feature".to_string(),
                ))
            }
            "qdrant" | "lancedb" => {
                if provider == "lancedb" {
                    warn!("vector_db_provider=lancedb is mapped to embedded qdrant adapter.");
                }

                let vector_data_dir = if !vector_db_url.is_empty() {
                    PathBuf::from(&vector_db_url)
                } else {
                    Path::new(&system_root_dir).join("vectors")
                };

                std::fs::create_dir_all(&vector_data_dir)?;

                #[cfg(feature = "qdrant")]
                return Ok(Arc::new(QdrantAdapter::new(vector_data_dir, dim)));

                #[cfg(not(feature = "qdrant"))]
                Err(ComponentError::Config(
                    "vector_db_provider=qdrant requires the `qdrant` crate feature".to_string(),
                ))
            }
            other => Err(ComponentError::Config(format!(
                "Unsupported vector_db_provider '{other}'. Supported: qdrant, lancedb, pgvector.",
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

        let mut parsed =
            url::Url::parse("postgres://localhost").expect("static URL is always valid");
        parsed
            .set_host(Some(host))
            .map_err(|e| ComponentError::Config(format!("invalid vector_db host: {e}")))?;
        parsed
            .set_port(Some(port))
            .map_err(|_| ComponentError::Config("invalid vector_db port".into()))?;
        parsed.set_path(&format!("/{name}"));
        parsed
            .set_username(user)
            .map_err(|_| ComponentError::Config("invalid vector_db username".into()))?;
        parsed
            .set_password(Some(pass))
            .map_err(|_| ComponentError::Config("invalid vector_db password".into()))?;

        Ok(parsed.to_string())
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

            let endpoint = if settings.embedding_endpoint.is_empty() {
                None
            } else {
                Some(settings.embedding_endpoint.clone())
            };

            let api_key = if settings.embedding_api_key.is_empty() {
                None
            } else {
                Some(settings.embedding_api_key.clone())
            };

            // Check MOCK_EMBEDDING env var as a fallback (preserves backward compat)
            let mock = std::env::var("MOCK_EMBEDDING")
                .ok()
                .map(|v| {
                    let v = v.trim().to_lowercase();
                    v == "true" || v == "1" || v == "yes"
                })
                .unwrap_or(false);

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

                Ok(Arc::new(adapter))
            }
            "litert" => {
                #[cfg(all(feature = "android-litert", target_os = "android"))]
                {
                    let model_path = llm_model.trim();
                    if model_path.is_empty() {
                        return Err(ComponentError::Config(
                            "llm_model must point to a local LiteRT model path when llm_provider=litert"
                                .to_string(),
                        ));
                    }

                    let backend = if llm_endpoint.trim().is_empty() {
                        None
                    } else {
                        Some(llm_endpoint)
                    };

                    let adapter = LiteRtAdapter::new(model_path.to_string(), backend)
                        .map_err(|e| ComponentError::Llm(format!("initialization failed: {e}")))?;

                    Ok(Arc::new(adapter))
                }

                #[cfg(not(all(feature = "android-litert", target_os = "android")))]
                {
                    Err(ComponentError::Config(
                        "llm_provider=litert requires Android target and the `android-litert` crate feature"
                            .to_string(),
                    ))
                }
            }
            _ => Err(ComponentError::Config(format!(
                "Unsupported llm_provider '{provider}'. Supported: openai, litert.",
            ))),
        }
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
