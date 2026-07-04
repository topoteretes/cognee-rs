//! ComponentManager: lazy-initializing, shared component store.
//!
//! Construction logic lives in `cognee-components`; this type owns the
//! version-keyed cache and delegates each backend build to a
//! [`ComponentRegistry`]. Supply a custom registry via [`ComponentManager::with_registry`]
//! to plug in external adapters (e.g. the closed qdrant / litert factories);
//! [`ComponentManager::new`] uses [`ComponentRegistry::with_builtins`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock as TokioRwLock;

use cognee_components::ComponentRegistry;
use cognee_database::DatabaseConnection;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, Transcriber};
use cognee_storage::StorageTrait;
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
/// Backend construction is delegated to a [`ComponentRegistry`]; the cache
/// policy and the transcriber's bespoke `Option<Arc>` slot live here.
pub struct ComponentManager {
    config: ConfigManager,
    registry: ComponentRegistry,
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
    // Version-keyed cache of the lowered build context, so `Settings::backend_context`
    // (env reads + the Postgres credential-fallback warning) runs once per config
    // version instead of once per component (7×).
    context: TokioRwLock<Option<(u64, cognee_components::BackendBuildContext)>>,
}

impl ComponentManager {
    /// Construct with the OSS built-in registry
    /// ([`ComponentRegistry::with_builtins`]).
    pub fn new(config: ConfigManager) -> Self {
        Self::with_registry(config, ComponentRegistry::with_builtins())
    }

    /// Construct with an explicit registry. Use this to inject external adapter
    /// factories (register them on the registry before passing it in).
    pub fn with_registry(config: ConfigManager, registry: ComponentRegistry) -> Self {
        Self {
            config,
            registry,
            storage: TokioRwLock::new(None),
            database: TokioRwLock::new(None),
            graph_db: TokioRwLock::new(None),
            vector_db: TokioRwLock::new(None),
            embedding_engine: TokioRwLock::new(None),
            llm: TokioRwLock::new(None),
            transcriber: TokioRwLock::new(None),
            context: TokioRwLock::new(None),
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

    /// Access the component registry (e.g. to inspect registered providers).
    pub fn registry(&self) -> &ComponentRegistry {
        &self.registry
    }

    /// Return the lowered build context for the current config version.
    ///
    /// Cached per config version: `Settings::backend_context` reads several env
    /// vars and may emit the Postgres credential-fallback warning, so building it
    /// once per version (rather than once per component) avoids duplicated work
    /// and duplicated warnings. The returned owned context is `Send`, so binding
    /// it to a local before an `.await` keeps the delegating futures `Send`.
    async fn build_context(&self) -> cognee_components::BackendBuildContext {
        let current_ver = self.config.version();
        {
            let guard = self.context.read().await;
            if let Some((ver, ctx)) = &*guard
                && *ver == current_ver
            {
                return ctx.clone();
            }
        }
        let mut guard = self.context.write().await;
        if let Some((ver, ctx)) = &*guard
            && *ver == current_ver
        {
            return ctx.clone();
        }
        // No `.await` between the config read and storing the result, so the
        // (non-`Send`) settings guard never crosses an await point.
        let ctx = self.config.read().backend_context();
        *guard = Some((current_ver, ctx.clone()));
        ctx
    }

    async fn init_storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let ctx = self.build_context().await;
        cognee_components::build_storage(&ctx).await
    }

    async fn init_database(&self) -> Result<Arc<DatabaseConnection>, ComponentError> {
        let ctx = self.build_context().await;
        cognee_components::build_database(&ctx).await
    }

    async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_graph(&ctx).await
    }

    async fn init_vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_vector(&ctx).await
    }

    async fn init_embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_embedding(&ctx).await
    }

    async fn init_llm(&self) -> Result<Arc<dyn Llm>, ComponentError> {
        let ctx = self.build_context().await;
        self.registry.build_llm(&ctx).await
    }

    /// Return the [`Transcriber`] for the configured LLM provider, if supported.
    ///
    /// Returns `Ok(Some(_))` for OpenAI-compatible providers that expose audio
    /// transcription; `Ok(None)` for providers that do not (e.g. `litert`), so
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
        let ctx = self.build_context().await;
        let new = self.registry.build_transcriber(&ctx).await?;
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

    // -- resolved graph/vector Postgres URL / PgGraph provider dispatch -------

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_returns_explicit_url_as_is() {
        let settings = Settings {
            graph_database_url: "postgres://user:pw@myhost:5432/graphs".to_string(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
            .expect("should succeed with full URL");
        assert_eq!(url, "postgres://user:pw@myhost:5432/graphs");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_builds_from_graph_creds() {
        let settings = Settings {
            graph_database_host: "graphhost".to_string(),
            graph_database_port: 5432,
            graph_database_name: "mygraph".to_string(),
            graph_database_username: "guser".to_string(),
            graph_database_password: "gpass".to_string(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
            .expect("should build from graph creds");
        assert!(url.contains("guser"), "URL should contain username");
        assert!(url.contains("graphhost"), "URL should contain host");
        assert!(url.contains("mygraph"), "URL should contain db name");
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn resolved_graph_url_falls_back_to_relational_creds() {
        let settings = Settings {
            db_host: "relhost".to_string(),
            db_port: 5432,
            db_name: "reldb".to_string(),
            db_username: "reluser".to_string(),
            db_password: "relpass".to_string(),
            ..Settings::default()
        };
        let url = settings
            .resolved_graph_postgres_url()
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
    fn resolved_graph_url_errors_when_no_creds() {
        let settings = Settings {
            db_host: String::new(),
            db_name: String::new(),
            db_username: String::new(),
            ..Settings::default()
        };
        let result = settings.resolved_graph_postgres_url();
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
            err_msg.contains("neo4j"),
            "error message should name the unsupported provider: {err_msg}"
        );
    }

    // -- Mock LLM factory wiring (MOCK_LLM / COGNEE_RECORD_LLM) ----------------

    /// Write a minimal valid cassette to a temp file and return (dir, path).
    #[cfg(feature = "mock-llm")]
    fn write_cassette() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cassette.json");
        let body = r#"{"version":1,"model":"mock-model","entries":{}}"#;
        std::fs::write(&path, body).expect("write cassette");
        (dir, path)
    }

    #[cfg(feature = "mock-llm")]
    #[tokio::test]
    async fn init_llm_uses_replay_mock_when_llm_mock_set_without_api_key() {
        let (_dir, cassette) = write_cassette();
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
        let settings = Settings {
            llm_provider: "openai".to_string(),
            llm_api_key: "sk-test".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
            llm_record_path: record_path.to_string_lossy().into_owned(),
            ..Settings::default()
        };
        let cm = ComponentManager::new(ConfigManager::new(settings));
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
