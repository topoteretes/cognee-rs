//! Construct default standalone backend handles for the HTTP server binary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use cognee_core::{CpuPool, RayonThreadPool};
use cognee_database::{
    CheckpointStore, DatabaseConnection, DeleteDb, IngestDb, PoolConfig, SeaOrmCheckpointStore,
    SearchHistoryDb, connect_with_pool, initialize,
};
use cognee_delete::DeleteService;
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine, EmbeddingProvider};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::{
    Llm, OpenAIResponsesClient, ResponsesClient, Transcriber, build_openai_compatible_adapter,
};
use cognee_ontology::{OntologyManager, OntologyResolver};
use cognee_search::{
    SeaOrmSessionStore, SearchBuilder, SearchOrchestrator, SessionManager, SessionStore,
};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{PgVectorAdapter, VectorDB};
use secrecy::ExposeSecret;

use crate::components::ComponentHandles;
use crate::config::HttpServerConfig;
use crate::error::ServerError;
use crate::notebook_runner::SubprocessRunner;

fn ensure_dir(path: &Path) -> Result<(), ServerError> {
    std::fs::create_dir_all(path)
        .map_err(|e| ServerError::Other(anyhow!("create_dir_all({}): {e}", path.display())))
}

pub async fn wire_default_backends(
    cfg: &HttpServerConfig,
) -> Result<ComponentHandles, ServerError> {
    ensure_dir(&cfg.data_root_directory)?;
    ensure_dir(&cfg.system_root_directory)?;

    let storage = wire_storage(cfg).await?;
    let database = wire_database(cfg).await?;
    let graph_db = wire_graph_db(cfg).await?;
    let vector_db = wire_vector_db(cfg).await?;

    let embedding_engine = wire_embedding_engine(cfg).await;
    let llm = wire_llm(cfg);
    let transcriber = wire_transcriber(cfg);

    let thread_pool: Option<Arc<dyn CpuPool>> = Some(Arc::new(
        RayonThreadPool::with_default_threads()
            .map_err(|e| ServerError::Other(anyhow!("rayon thread pool init failed: {e}")))?,
    ));

    let ontology_manager = Arc::new(OntologyManager::new(
        cfg.data_root_directory.join("ontology"),
    ));
    let ontology_resolver: Option<Arc<dyn OntologyResolver>> = None;

    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        Arc::clone(&database) as Arc<dyn DeleteDb>,
    ));

    let checkpoint_store = Some(
        Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&database))) as Arc<dyn CheckpointStore>
    );

    let (session_store, session_manager) = wire_session(cfg, Arc::clone(&database)).await;

    let search_orchestrator = wire_search_orchestrator(
        Arc::clone(&database),
        llm.clone(),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        embedding_engine.clone(),
        session_manager.clone(),
    );

    let responses_client = wire_responses_client(cfg);

    let notebook_runner = if cfg.notebook_runner_enabled {
        Some(SubprocessRunner::new().into_dyn())
    } else {
        None
    };

    Ok(ComponentHandles {
        database,
        acl_db: None,
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator,
        llm,
        transcriber,
        graph_db: Some(graph_db),
        vector_db: Some(vector_db),
        thread_pool,
        embedding_engine,
        ontology_resolver,
        session_store,
        session_manager,
        checkpoint_store,
        responses_client,
        notebook_runner,
    })
}

async fn wire_storage(cfg: &HttpServerConfig) -> Result<Arc<dyn StorageTrait>, ServerError> {
    let storage =
        Arc::new(LocalStorage::new(cfg.data_root_directory.clone())) as Arc<dyn StorageTrait>;
    storage
        .initialize()
        .await
        .map_err(|e| ServerError::Other(anyhow!("storage init failed: {e}")))?;
    Ok(storage)
}

async fn wire_database(cfg: &HttpServerConfig) -> Result<Arc<DatabaseConnection>, ServerError> {
    let url = cfg.relational_db_url.clone();

    if let Some(path) = url.strip_prefix("sqlite://")
        && !path.starts_with(':')
    {
        let db_path = PathBuf::from(path);
        if let Some(parent) = db_path.parent() {
            ensure_dir(parent)?;
        }
        if !db_path.exists() {
            std::fs::File::create(&db_path).map_err(|e| {
                ServerError::Other(anyhow!("create sqlite file {}: {e}", db_path.display()))
            })?;
        }
    }

    // Pool sizing is chosen here, in the layer that selects the URL; tune via
    // `PoolConfig` rather than pushing backend guesses into `connect`.
    let db = connect_with_pool(&url, PoolConfig::default())
        .await
        .map_err(|e| ServerError::Other(anyhow!("database connect failed: {e}")))?;
    initialize(&db)
        .await
        .map_err(|e| ServerError::Other(anyhow!("database migrate failed: {e}")))?;

    Ok(Arc::new(db))
}

async fn wire_graph_db(cfg: &HttpServerConfig) -> Result<Arc<dyn GraphDBTrait>, ServerError> {
    if !cfg.graph_provider.eq_ignore_ascii_case("ladybug") {
        return Err(ServerError::Other(anyhow!(
            "unsupported graph provider '{}'; only 'ladybug' is supported",
            cfg.graph_provider
        )));
    }

    if let Some(parent) = cfg.graph_file_path.parent() {
        ensure_dir(parent)?;
    }

    let path = cfg.graph_file_path.to_string_lossy().to_string();
    let graph = LadybugAdapter::new(&path)
        .await
        .map_err(|e| ServerError::Other(anyhow!("graph init failed: {e}")))?;
    graph
        .initialize()
        .await
        .map_err(|e| ServerError::Other(anyhow!("graph schema init failed: {e}")))?;
    Ok(Arc::new(graph) as Arc<dyn GraphDBTrait>)
}

async fn wire_vector_db(cfg: &HttpServerConfig) -> Result<Arc<dyn VectorDB>, ServerError> {
    // The qdrant adapter has been extracted to the closed `cognee-vector-qdrant`
    // crate; the OSS http-server wires pgvector as the production default and
    // exposes an opt-in in-memory mock behind the `dev-mock` feature so local
    // dev / `cargo test` work without a Postgres instance.
    let provider = cfg.vector_provider.to_ascii_lowercase();
    match provider.as_str() {
        "pgvector" => {
            let url = cfg.vector_db_url.trim();
            if url.is_empty() {
                return Err(ServerError::Other(anyhow!(
                    "VECTOR_DB_URL (postgres connection string) is required when \
                     VECTOR_DB_PROVIDER=pgvector"
                )));
            }
            let adapter = PgVectorAdapter::new(url, cfg.embedding_dimensions as usize)
                .await
                .map_err(|e| ServerError::Other(anyhow!("pgvector adapter init: {e}")))?;
            Ok(Arc::new(adapter) as Arc<dyn VectorDB>)
        }
        #[cfg(feature = "dev-mock")]
        "mock" => {
            // OSS single-user dev path — keeps `cargo test` + local dev working
            // without a Postgres instance. Off in production builds.
            // The `dev-mock` feature enables `cognee-vector/testing`, which
            // is where `MockVectorDB` actually lives.
            Ok(Arc::new(cognee_vector::MockVectorDB::new()) as Arc<dyn VectorDB>)
        }
        other => Err(ServerError::Other(anyhow!(
            "vector_db_provider='{other}' not supported in the OSS http-server. \
             Supported: 'pgvector' (and 'mock' when built with the `dev-mock` \
             feature). The Qdrant adapter has been extracted to the closed \
             cognee-vector-qdrant crate."
        ))),
    }
}

fn build_embedding_config(cfg: &HttpServerConfig) -> Option<EmbeddingConfig> {
    let provider = match cfg.embedding_provider.trim().to_ascii_lowercase().as_str() {
        "onnx" => EmbeddingProvider::Onnx,
        "fastembed" => EmbeddingProvider::Fastembed,
        "openai" => EmbeddingProvider::OpenAi,
        "openai_compatible" => EmbeddingProvider::OpenAiCompatible,
        "ollama" => EmbeddingProvider::Ollama,
        "mock" => EmbeddingProvider::Mock,
        other => {
            tracing::warn!("unknown embedding provider '{other}', embedding engine not wired");
            return None;
        }
    };
    let mut embedding_cfg = EmbeddingConfig {
        provider,
        model: cfg.embedding_model_name.clone(),
        dimensions: cfg.embedding_dimensions as usize,
        ..Default::default()
    };
    if !cfg.embedding_endpoint.trim().is_empty() {
        embedding_cfg.endpoint = Some(cfg.embedding_endpoint.clone());
    }
    if !cfg.embedding_api_key.expose_secret().is_empty() {
        embedding_cfg.api_key = Some(cfg.embedding_api_key.expose_secret().to_string());
    }
    embedding_cfg.onnx.model_name = cfg.embedding_model_name.clone();
    embedding_cfg.onnx.dimensions = cfg.embedding_dimensions as usize;
    if let Some(model_path) = &cfg.embedding_model_path {
        embedding_cfg.onnx.model_path = model_path.clone();
    }
    if let Some(tokenizer_path) = &cfg.embedding_tokenizer_path {
        embedding_cfg.onnx.tokenizer_path = tokenizer_path.clone();
    }

    Some(embedding_cfg)
}

async fn wire_embedding_engine(cfg: &HttpServerConfig) -> Option<Arc<dyn EmbeddingEngine>> {
    let embedding_cfg = build_embedding_config(cfg)?;

    match embedding_cfg.create_engine().await {
        Ok(engine) => Some(engine),
        Err(err) => {
            tracing::warn!("embedding engine unavailable, wiring as None: {err}");
            None
        }
    }
}

fn wire_llm(cfg: &HttpServerConfig) -> Option<Arc<dyn Llm>> {
    // Provider routing (and the required-key / required-endpoint validation) lives
    // in the shared factory; an unsupported provider or missing credential errors
    // there and we wire None.
    match build_openai_compatible_adapter(
        &cfg.llm_provider,
        &cfg.llm_model,
        cfg.llm_api_key.expose_secret(),
        &cfg.llm_endpoint,
        cfg.llm_max_retries,
    ) {
        Ok(adapter) => Some(Arc::new(adapter) as Arc<dyn Llm>),
        Err(err) => {
            tracing::warn!("llm not wired: {err}");
            None
        }
    }
}

fn wire_transcriber(cfg: &HttpServerConfig) -> Option<Arc<dyn Transcriber>> {
    // Whisper-style transcription only works against OpenAI and user-pointed
    // OpenAI-compatible servers that expose /audio/transcriptions; other providers
    // get graceful no-audio (None).
    let provider = cfg.llm_provider.to_ascii_lowercase();
    if !matches!(provider.as_str(), "openai" | "custom" | "openai_compatible") {
        return None;
    }

    match build_openai_compatible_adapter(
        &cfg.llm_provider,
        &cfg.llm_model,
        cfg.llm_api_key.expose_secret(),
        &cfg.llm_endpoint,
        cfg.llm_max_retries,
    ) {
        Ok(adapter) => Some(Arc::new(adapter) as Arc<dyn Transcriber>),
        Err(err) => {
            tracing::warn!("transcriber not wired: {err}");
            None
        }
    }
}

async fn wire_session(
    cfg: &HttpServerConfig,
    database: Arc<DatabaseConnection>,
) -> (Option<Arc<dyn SessionStore>>, Option<Arc<SessionManager>>) {
    if !cfg.session_store_backend.eq_ignore_ascii_case("seaorm") {
        tracing::warn!(
            "session store backend '{}' unsupported in standalone wiring; session disabled",
            cfg.session_store_backend
        );
        return (None, None);
    }

    match SeaOrmSessionStore::new(database).await {
        Ok(store_impl) => {
            let store: Arc<dyn SessionStore> = Arc::new(store_impl);
            let manager = Arc::new(SessionManager::new(Arc::clone(&store)));
            (Some(store), Some(manager))
        }
        Err(err) => {
            tracing::warn!("session store wiring failed, wiring as None: {err}");
            (None, None)
        }
    }
}

fn wire_search_orchestrator(
    database: Arc<DatabaseConnection>,
    llm: Option<Arc<dyn Llm>>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Option<Arc<dyn EmbeddingEngine>>,
    session_manager: Option<Arc<SessionManager>>,
) -> Option<Arc<SearchOrchestrator>> {
    let (Some(llm), Some(embedding_engine)) = (llm, embedding_engine) else {
        tracing::warn!(
            "search orchestrator not wired: requires llm + embedding engine, one or more missing"
        );
        return None;
    };

    let mut builder = SearchBuilder::new(
        vector_db,
        embedding_engine,
        graph_db,
        llm,
        Arc::clone(&database) as Arc<dyn SearchHistoryDb>,
    )
    .with_dataset_resolver(Arc::clone(&database) as Arc<dyn IngestDb>);

    if let Some(sm) = session_manager {
        builder = builder.with_session_manager(sm);
    }

    Some(Arc::new(builder.build()))
}

fn wire_responses_client(cfg: &HttpServerConfig) -> Option<Arc<dyn ResponsesClient>> {
    if !cfg.responses_client_enabled {
        return None;
    }

    let api_key = cfg.llm_api_key.expose_secret().to_string();
    if api_key.is_empty() {
        tracing::warn!("responses client enabled but llm api key is missing; wiring as None");
        return None;
    }

    let endpoint = if cfg.llm_endpoint.trim().is_empty() {
        None
    } else {
        Some(cfg.llm_endpoint.clone())
    };

    match OpenAIResponsesClient::new(api_key, endpoint) {
        Ok(client) => Some(Arc::new(client) as Arc<dyn ResponsesClient>),
        Err(err) => {
            tracing::warn!("responses client wiring failed, wiring as None: {err}");
            None
        }
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

    #[test]
    fn build_embedding_config_applies_explicit_onnx_asset_paths() {
        let cfg = HttpServerConfig {
            embedding_provider: "onnx".to_string(),
            embedding_model_name: "custom-bge".to_string(),
            embedding_dimensions: 768,
            embedding_model_path: Some(PathBuf::from("/tmp/model.onnx")),
            embedding_tokenizer_path: Some(PathBuf::from("/tmp/tokenizer.json")),
            ..Default::default()
        };

        let embedding_cfg = build_embedding_config(&cfg).expect("embedding config");

        assert_eq!(embedding_cfg.provider, EmbeddingProvider::Onnx);
        assert_eq!(embedding_cfg.model, "custom-bge");
        assert_eq!(embedding_cfg.dimensions, 768);
        assert_eq!(embedding_cfg.onnx.model_name, "custom-bge");
        assert_eq!(embedding_cfg.onnx.dimensions, 768);
        assert_eq!(
            embedding_cfg.onnx.model_path,
            PathBuf::from("/tmp/model.onnx")
        );
        assert_eq!(
            embedding_cfg.onnx.tokenizer_path,
            PathBuf::from("/tmp/tokenizer.json")
        );
    }

    #[tokio::test]
    async fn wire_default_backends_fails_on_invalid_database_url() {
        let mut cfg = HttpServerConfig::default();
        let temp = tempfile::tempdir().expect("tempdir");
        cfg.data_root_directory = temp.path().join("data");
        cfg.system_root_directory = temp.path().join("system");
        cfg.graph_file_path = cfg.system_root_directory.join("graph");
        cfg.vector_db_url = cfg
            .system_root_directory
            .join("vectors")
            .display()
            .to_string();
        cfg.relational_db_url = "not-a-valid-db-url".to_string();

        let result = wire_default_backends(&cfg).await;
        assert!(result.is_err());

        let msg = match result {
            Ok(_) => String::new(),
            Err(err) => err.to_string(),
        };
        assert!(msg.contains("database connect failed"));
    }
}
