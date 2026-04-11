//! ComponentManager: lazy-initializing, shared component store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::OnceCell;
use tracing::warn;

use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine};
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

use crate::config::Settings;
use crate::context::PipelineContext;
use crate::error::ComponentError;

/// Manages shared, lazily-initialized pipeline components.
///
/// Each component is created on first access and cached for subsequent calls.
/// Constructed from [`Settings`] — typically loaded once from the CLI config file.
pub struct ComponentManager {
    settings: Settings,
    storage: OnceCell<Arc<dyn StorageTrait>>,
    database: OnceCell<Arc<DatabaseConnection>>,
    graph_db: OnceCell<Arc<dyn GraphDBTrait>>,
    vector_db: OnceCell<Arc<dyn VectorDB>>,
    embedding_engine: OnceCell<Arc<dyn EmbeddingEngine>>,
    llm: OnceCell<Arc<dyn Llm>>,
}

impl ComponentManager {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            storage: OnceCell::new(),
            database: OnceCell::new(),
            graph_db: OnceCell::new(),
            vector_db: OnceCell::new(),
            embedding_engine: OnceCell::new(),
            llm: OnceCell::new(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    async fn init_storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let storage = LocalStorage::new(PathBuf::from(&self.settings.data_root_directory));
        storage
            .initialize()
            .await
            .map_err(|e| ComponentError::Storage(format!("initialization failed: {e}")))?;
        Ok(Arc::new(storage))
    }

    async fn init_database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        let db = connect(&self.settings.resolved_relational_db_url())
            .await
            .map_err(|e| ComponentError::Database(format!("initialization failed: {e}")))?;
        initialize(&db)
            .await
            .map_err(|e| ComponentError::Database(format!("schema initialization failed: {e}")))?;
        Ok(Arc::new(db))
    }

    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let provider = self.settings.graph_database_provider.to_lowercase();
        if provider != "ladybug" && provider != "kuzu" {
            return Err(ComponentError::Config(format!(
                "Unsupported graph_database_provider '{}'. Supported: ladybug, kuzu.",
                self.settings.graph_database_provider
            )));
        }

        let graph_path = if !self.settings.graph_file_path.is_empty() {
            self.settings.graph_file_path.clone()
        } else {
            format!("{}/graph", self.settings.system_root_directory)
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

    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        let provider = self.settings.vector_db_provider.to_lowercase();
        let dim = self.settings.embedding_dimensions as usize;

        match provider.as_str() {
            "pgvector" => {
                #[cfg(feature = "pgvector")]
                {
                    let url = self.resolved_vector_db_url()?;
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

                let vector_data_dir = if !self.settings.vector_db_url.is_empty() {
                    PathBuf::from(&self.settings.vector_db_url)
                } else {
                    Path::new(&self.settings.system_root_directory).join("vectors")
                };

                std::fs::create_dir_all(&vector_data_dir)?;

                #[cfg(feature = "qdrant")]
                return Ok(Arc::new(QdrantAdapter::new(vector_data_dir, dim)));

                #[cfg(not(feature = "qdrant"))]
                Err(ComponentError::Config(
                    "vector_db_provider=qdrant requires the `qdrant` crate feature".to_string(),
                ))
            }
            _ => Err(ComponentError::Config(format!(
                "Unsupported vector_db_provider '{}'. Supported: qdrant, lancedb, pgvector.",
                self.settings.vector_db_provider
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
    fn resolved_vector_db_url(&self) -> Result<String, ComponentError> {
        if self.settings.vector_db_url.starts_with("postgres://")
            || self.settings.vector_db_url.starts_with("postgresql://")
        {
            return Ok(self.settings.vector_db_url.clone());
        }

        let host = if self.settings.vector_db_url.is_empty() {
            "localhost"
        } else {
            &self.settings.vector_db_url
        };
        let port = self.settings.vector_db_port;
        let name = if self.settings.vector_db_name.is_empty() {
            "cognee_vectors"
        } else {
            &self.settings.vector_db_name
        };
        let user = if self.settings.db_username.is_empty() {
            "postgres"
        } else {
            &self.settings.db_username
        };
        let pass = &self.settings.db_password;

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

    fn init_embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        let config = EmbeddingConfig::from_env();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|e| ComponentError::EmbeddingEngine(format!("no tokio runtime: {e}")))?;
        let engine = handle.block_on(config.create_engine()).map_err(|e| {
            ComponentError::EmbeddingEngine(format!("embedding engine init failed: {e}"))
        })?;
        Ok(engine)
    }

    fn init_llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        let provider = self.settings.llm_provider.to_lowercase();
        match provider.as_str() {
            "openai" => {
                if self.settings.llm_api_key.is_empty() {
                    return Err(ComponentError::Config(
                        "llm_api_key must be configured".to_string(),
                    ));
                }

                let endpoint = if self.settings.llm_endpoint.is_empty() {
                    None
                } else {
                    Some(self.settings.llm_endpoint.clone())
                };

                let retries = self.settings.llm_max_retries.max(1);

                let adapter = OpenAIAdapter::new(
                    self.settings.llm_model.clone(),
                    self.settings.llm_api_key.clone(),
                    endpoint,
                )
                .map_err(|e| ComponentError::Llm(format!("initialization failed: {e}")))?
                .with_structured_output_retries(retries)
                .with_network_retries(retries);

                Ok(Arc::new(adapter))
            }
            "litert" => {
                #[cfg(all(feature = "android-litert", target_os = "android"))]
                {
                    let model_path = self.settings.llm_model.trim();
                    if model_path.is_empty() {
                        return Err(ComponentError::Config(
                            "llm_model must point to a local LiteRT model path when llm_provider=litert"
                                .to_string(),
                        ));
                    }

                    let backend = if self.settings.llm_endpoint.trim().is_empty() {
                        None
                    } else {
                        Some(self.settings.llm_endpoint.clone())
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
                "Unsupported llm_provider '{}'. Supported: openai, litert.",
                self.settings.llm_provider
            ))),
        }
    }
}

#[async_trait]
impl PipelineContext for ComponentManager {
    async fn storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        self.storage
            .get_or_try_init(|| self.init_storage())
            .await
            .cloned()
    }

    async fn database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        self.database
            .get_or_try_init(|| self.init_database())
            .await
            .cloned()
    }

    async fn graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        self.graph_db
            .get_or_try_init(|| self.init_graph_db())
            .await
            .cloned()
    }

    async fn vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        self.vector_db
            .get_or_try_init(|| self.init_vector_db())
            .await
            .cloned()
    }

    async fn embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        self.embedding_engine
            .get_or_try_init(|| async { self.init_embedding_engine() })
            .await
            .cloned()
    }

    async fn llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        self.llm
            .get_or_try_init(|| async { self.init_llm() })
            .await
            .cloned()
    }
}
