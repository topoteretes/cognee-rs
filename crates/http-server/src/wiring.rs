//! Construct default standalone backend handles for the HTTP server binary.
//!
//! Backend construction is delegated to the shared `cognee-components`
//! registry; this module owns the eager `ComponentHandles` assembly and the
//! server-specific policies (required-vs-optional downgrade, the pgvector
//! coherence guard, session / search / responses wiring).

use std::path::Path;
use std::sync::Arc;

use anyhow::anyhow;
use cognee_components::{ComponentRegistry, build_database, build_storage};
use cognee_core::{CpuPool, RayonThreadPool};
use cognee_database::{
    CheckpointStore, DatabaseConnection, DeleteDb, IngestDb, SeaOrmCheckpointStore, SearchHistoryDb,
};
use cognee_delete::DeleteService;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, OpenAIResponsesClient, ResponsesClient, Transcriber};
use cognee_ontology::{OntologyManager, OntologyResolver};
use cognee_search::{
    SeaOrmSessionStore, SearchBuilder, SearchOrchestrator, SessionManager, SessionStore,
};
use cognee_vector::VectorDB;
use secrecy::ExposeSecret;

use crate::components::ComponentHandles;
use crate::config::HttpServerConfig;
use crate::error::ServerError;
use crate::notebook_runner::SubprocessRunner;

fn ensure_dir(path: &Path) -> Result<(), ServerError> {
    std::fs::create_dir_all(path)
        .map_err(|e| ServerError::Other(anyhow!("create_dir_all({}): {e}", path.display())))
}

/// Wire the default standalone backends using the OSS built-in registry.
pub async fn wire_default_backends(
    cfg: &HttpServerConfig,
) -> Result<ComponentHandles, ServerError> {
    wire_default_backends_with(cfg, &ComponentRegistry::with_builtins()).await
}

/// Wire the default standalone backends using a caller-supplied registry.
///
/// Closed/embedding entry points call this with a registry that has external
/// adapter factories registered (e.g. qdrant / litert) so a configured
/// `vector_provider="qdrant"` resolves without editing OSS.
pub async fn wire_default_backends_with(
    cfg: &HttpServerConfig,
    registry: &ComponentRegistry,
) -> Result<ComponentHandles, ServerError> {
    ensure_dir(&cfg.data_root_directory)?;
    ensure_dir(&cfg.system_root_directory)?;

    let ctx = cfg.backend_context();

    // Required backends — a failure here aborts startup.
    let storage = build_storage(&ctx).await?;
    let database = build_database(&ctx).await?;
    let graph_db = wire_graph_db(cfg, registry, &ctx).await?;
    let vector_db = wire_vector_db(cfg, registry, &ctx).await?;

    // Optional backends — a failure downgrades to `None` (handlers surface a
    // 500-level envelope at runtime), preserving the historical behavior.
    let embedding_engine = wire_embedding_engine(registry, &ctx).await;
    let llm = wire_llm(registry, &ctx).await;
    let transcriber = wire_transcriber(registry, &ctx).await;

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

async fn wire_graph_db(
    cfg: &HttpServerConfig,
    registry: &ComponentRegistry,
    ctx: &cognee_components::BackendBuildContext,
) -> Result<Arc<dyn GraphDBTrait>, ServerError> {
    // The standalone server ships only the embedded ladybug graph; guard early
    // with an actionable message rather than the registry's generic
    // "unregistered provider" error.
    if !cfg.graph_provider.eq_ignore_ascii_case("ladybug") {
        return Err(ServerError::Other(anyhow!(
            "unsupported graph provider '{}'; only 'ladybug' is supported",
            cfg.graph_provider
        )));
    }
    Ok(registry.build_graph(ctx).await?)
}

/// Validate the pgvector configuration. Kept in the server wrapper (not the
/// shared factory) so `cognee-lib`'s empty-URL→localhost synthesis is
/// unaffected.
fn validate_vector_config(cfg: &HttpServerConfig) -> Result<(), ServerError> {
    let provider = cfg.vector_provider.to_ascii_lowercase();
    if provider != "pgvector" {
        return Ok(());
    }
    let url = cfg.vector_db_url.trim();
    if url.is_empty() {
        return Err(ServerError::Other(anyhow!(
            "VECTOR_DB_URL (postgres connection string) is required when \
             VECTOR_DB_PROVIDER=pgvector"
        )));
    }
    // Guard against the incoherent default (VECTOR_DB_PROVIDER unset → pgvector,
    // VECTOR_DB_URL unset → derived from SYSTEM_ROOT_DIRECTORY, i.e. a
    // filesystem path). Without this, pgvector reports a cryptic "connection
    // string '…/vectors' cannot be parsed". Point the operator at the actual
    // misconfiguration instead.
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Err(ServerError::Other(anyhow!(
            "VECTOR_DB_PROVIDER=pgvector requires a postgres connection string in \
             VECTOR_DB_URL (postgres://… or postgresql://…), but got '{url}'. If you \
             did not intend to use pgvector, set VECTOR_DB_PROVIDER explicitly (e.g. \
             'mock' in a dev-mock build); the default derives this value from \
             SYSTEM_ROOT_DIRECTORY, which is not a valid Postgres URL."
        )));
    }
    Ok(())
}

async fn wire_vector_db(
    cfg: &HttpServerConfig,
    registry: &ComponentRegistry,
    ctx: &cognee_components::BackendBuildContext,
) -> Result<Arc<dyn VectorDB>, ServerError> {
    validate_vector_config(cfg)?;
    Ok(registry.build_vector(ctx).await?)
}

/// Downgrade a required-backend build error to `None` with a warning — the
/// standalone server's policy for the optional (search/llm/audio) backends,
/// which surface a 500-level envelope at runtime when unwired.
fn downgrade<T>(result: Result<T, cognee_components::ComponentError>, what: &str) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(err) => {
            tracing::warn!("{what} not wired: {err}");
            None
        }
    }
}

async fn wire_embedding_engine(
    registry: &ComponentRegistry,
    ctx: &cognee_components::BackendBuildContext,
) -> Option<Arc<dyn EmbeddingEngine>> {
    downgrade(registry.build_embedding(ctx).await, "embedding engine")
}

async fn wire_llm(
    registry: &ComponentRegistry,
    ctx: &cognee_components::BackendBuildContext,
) -> Option<Arc<dyn Llm>> {
    downgrade(registry.build_llm(ctx).await, "llm")
}

async fn wire_transcriber(
    registry: &ComponentRegistry,
    ctx: &cognee_components::BackendBuildContext,
) -> Option<Arc<dyn Transcriber>> {
    // `build_transcriber` already yields `Ok(None)` for providers without audio
    // support; a hard error (bad credentials) downgrades to None as before.
    downgrade(registry.build_transcriber(ctx).await, "transcriber").flatten()
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
    use std::path::PathBuf;

    #[test]
    fn backend_context_applies_explicit_onnx_asset_paths() {
        let cfg = HttpServerConfig {
            embedding_provider: "onnx".to_string(),
            embedding_model_name: "custom-bge".to_string(),
            embedding_dimensions: 768,
            embedding_model_path: Some(PathBuf::from("/tmp/model.onnx")),
            embedding_tokenizer_path: Some(PathBuf::from("/tmp/tokenizer.json")),
            ..Default::default()
        };

        let ctx = cfg.backend_context();

        assert_eq!(ctx.embedding.provider, "onnx");
        assert_eq!(ctx.embedding.model, "custom-bge");
        assert_eq!(ctx.embedding.dimensions, 768);
        assert_eq!(ctx.embedding.onnx_model_name, "custom-bge");
        assert_eq!(ctx.embedding.onnx_dimensions, 768);
        assert_eq!(
            ctx.embedding.onnx_model_path,
            PathBuf::from("/tmp/model.onnx")
        );
        assert_eq!(
            ctx.embedding.onnx_tokenizer_path,
            PathBuf::from("/tmp/tokenizer.json")
        );
    }

    #[test]
    fn backend_context_defaults_onnx_paths_when_unset() {
        let cfg = HttpServerConfig {
            embedding_provider: "onnx".to_string(),
            embedding_model_path: None,
            embedding_tokenizer_path: None,
            ..Default::default()
        };
        let ctx = cfg.backend_context();
        assert!(
            ctx.embedding
                .onnx_model_path
                .to_string_lossy()
                .contains("target/models"),
            "unset ONNX model path must fall back to the ./target/models default"
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
        assert!(
            msg.contains("initialization failed") || msg.contains("component error"),
            "expected a database init failure, got: {msg}"
        );
    }

    #[test]
    fn validate_vector_config_pgvector_rejects_non_postgres_url() {
        // The incoherent default (VECTOR_DB_PROVIDER unset → pgvector, plus a
        // VECTOR_DB_URL derived from SYSTEM_ROOT_DIRECTORY → a filesystem path)
        // must fail with an actionable message, not the cryptic connection-
        // string parse error the pgvector driver would otherwise emit.
        let cfg = HttpServerConfig {
            vector_provider: "pgvector".to_string(),
            vector_db_url: "/srv/.cognee_system/vectors".to_string(),
            ..Default::default()
        };

        let msg = match validate_vector_config(&cfg) {
            Ok(()) => String::new(),
            Err(err) => err.to_string(),
        };
        assert!(
            msg.contains("postgres connection string") && msg.contains("VECTOR_DB_PROVIDER"),
            "expected actionable pgvector error, got: {msg}"
        );
    }
}
