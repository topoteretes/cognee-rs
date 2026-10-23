//! Per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`): every dataset gets
//! its own graph and vector database, provisioned by a
//! [`DatasetDatabaseHandler`] and recorded in the `dataset_database` table.
//!
//! [`DatasetStorage`] is the resolver behind the routing engines
//! ([`cognee_graph::DatasetRoutedGraphDB`], [`cognee_vector::DatasetRoutedVectorDB`]):
//! given a dataset it provisions the databases on first use, builds their
//! engines with the configured provider factories, and caches them. It is the
//! Rust counterpart of Python's `set_database_global_context_variables`,
//! `get_or_create_dataset_database` and the handlers' `delete_dataset`. See
//! `docs/design/per-dataset-storage.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

use cognee_database::{DatasetDatabaseRecord, RelationalDb};
use cognee_graph::{
    DatasetGraphResolver, DatasetRoutedGraphDB, GraphDBError, GraphDBResult, GraphDBTrait,
};
use cognee_vector::{
    DatasetRoutedVectorDB, DatasetVectorResolver, VectorDB, VectorDBError, VectorDBResult,
};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::{GraphDbFactory, VectorDbFactory};

/// The connection-info key recording the embedding model a dataset was built
/// with (Python `ensure_embedding_model_matches.MODEL_KEY`).
pub const EMBEDDING_MODEL_KEY: &str = "embedding_model";
/// The connection-info key recording the embedding width a dataset was built
/// with (Python `ensure_embedding_model_matches.DIMENSIONS_KEY`).
pub const EMBEDDING_DIMENSIONS_KEY: &str = "embedding_dimensions";

/// Which store a [`DatasetDatabaseHandler`] provisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerKind {
    Graph,
    Vector,
}

impl HandlerKind {
    fn label(self) -> &'static str {
        match self {
            HandlerKind::Graph => "graph",
            HandlerKind::Vector => "vector",
        }
    }
}

/// One handler's half of a `dataset_database` row: where its database lives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HandlerRecord {
    pub provider: String,
    pub database_name: String,
    pub database_url: Option<String>,
    pub database_key: Option<String>,
    /// Host, port, schema — never a plaintext credential: the handler fills
    /// those in from the live configuration in
    /// [`DatasetDatabaseHandler::dataset_context`].
    pub connection_info: Map<String, Value>,
}

impl HandlerRecord {
    /// A string field of the connection info.
    pub fn info_str(&self, key: &str) -> Option<&str> {
        self.connection_info.get(key).and_then(Value::as_str)
    }

    fn graph_half(row: &DatasetDatabaseRecord) -> Self {
        Self {
            provider: row.graph_database_provider.clone(),
            database_name: row.graph_database_name.clone(),
            database_url: row.graph_database_url.clone(),
            database_key: row.graph_database_key.clone(),
            connection_info: object(&row.graph_database_connection_info),
        }
    }

    fn vector_half(row: &DatasetDatabaseRecord) -> Self {
        Self {
            provider: row.vector_database_provider.clone(),
            database_name: row.vector_database_name.clone(),
            database_url: row.vector_database_url.clone(),
            database_key: row.vector_database_key.clone(),
            connection_info: object(&row.vector_database_connection_info),
        }
    }
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// Provisions, locates and drops one kind of per-dataset database (Python
/// `DatasetDatabaseHandlerInterface`).
///
/// A handler never builds an engine itself: [`dataset_context`](Self::dataset_context)
/// derives the build context of the dataset's database from the base one, and
/// the configured provider's factory builds the engine from it.
///
/// Every name a handler derives must come from the dataset id (and owner), so
/// two concurrent first uses of one dataset provision the same database and
/// the loser of the row insert can use the winner's row.
#[async_trait]
pub trait DatasetDatabaseHandler: Send + Sync {
    /// The handler id (`ladybug`, `pgvector_shared`, …), stored in the row.
    fn name(&self) -> &str;
    /// Graph or vector.
    fn kind(&self) -> HandlerKind;
    /// The provider ids this handler works with.
    fn providers(&self) -> &[&'static str];
    /// Create the dataset's database if needed (idempotent) and describe it.
    async fn create_dataset(
        &self,
        base: &BackendBuildContext,
        dataset_id: Uuid,
        owner_id: Uuid,
    ) -> Result<HandlerRecord, ComponentError>;
    /// The build context of the dataset's database, credentials resolved from
    /// `base` (Python's `resolve_dataset_connection_info` plus the
    /// context-variable binding).
    fn dataset_context(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<BackendBuildContext, ComponentError>;
    /// Drop the dataset's database. Its engines are closed before this runs.
    async fn delete_dataset(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        owner_id: Uuid,
    ) -> Result<(), ComponentError>;
}

/// The handler a provider uses when `graph.dataset_database_handler` is
/// unset, following Python's `graph/config.py:77-90`. A provider without a
/// built-in handler defaults to a handler of its own name, which an adapter
/// crate can register.
pub fn default_graph_handler(provider: &str) -> String {
    match provider {
        "ladybug" => "ladybug".to_string(),
        "kuzu" => "kuzu".to_string(),
        "postgres" | "postgresql" | "postgres_demo" => "postgres_graph".to_string(),
        "mock" => MEMORY_HANDLER.to_string(),
        other => other.to_string(),
    }
}

/// The handler a provider uses when `vector.dataset_database_handler` is
/// unset, following Python's `vector/config.py:49-54`.
pub fn default_vector_handler(provider: &str) -> String {
    match provider {
        "lancedb" => "lancedb".to_string(),
        "pgvector" => "pgvector".to_string(),
        "mock" | "brute-force" => MEMORY_HANDLER.to_string(),
        other => other.to_string(),
    }
}

/// The in-memory handler: every dataset gets a fresh engine from the
/// configured (in-memory) factory, nothing persists. For tests and the
/// `mock` / `brute-force` providers.
pub const MEMORY_HANDLER: &str = "memory";

/// A dataset's bound engines.
struct Bound {
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
}

type BindCell = Arc<OnceCell<Arc<Bound>>>;

/// Registered handlers by `(kind, name)`.
pub(crate) type HandlerMap = HashMap<(HandlerKind, String), Arc<dyn DatasetDatabaseHandler>>;

/// Per-dataset storage: provisions each dataset's databases on first use,
/// builds and caches their engines, and drops them with the dataset.
pub struct DatasetStorage {
    base: BackendBuildContext,
    database: Arc<dyn RelationalDb>,
    graph_factory: Arc<dyn GraphDbFactory>,
    vector_factory: Arc<dyn VectorDbFactory>,
    graph_handler: Arc<dyn DatasetDatabaseHandler>,
    vector_handler: Arc<dyn DatasetDatabaseHandler>,
    /// Every registered handler by kind and name: a row keeps the handler
    /// that provisioned it, which a later configuration may no longer select.
    handlers: HandlerMap,
    /// A poisoned lock is recovered: every critical section is a single map
    /// operation, which a panic cannot leave half done.
    bound: Mutex<HashMap<Uuid, BindCell>>,
}

impl DatasetStorage {
    /// Assemble per-dataset storage. Fails, like Python's
    /// `multi_user_support_possible`, when a selected handler is not
    /// registered or does not work with the configured provider.
    pub(crate) fn new(
        base: BackendBuildContext,
        database: Arc<dyn RelationalDb>,
        graph_factory: Arc<dyn GraphDbFactory>,
        vector_factory: Arc<dyn VectorDbFactory>,
        graph_handler_name: &str,
        vector_handler_name: &str,
        handlers: HandlerMap,
    ) -> Result<Self, ComponentError> {
        let graph_handler = select_handler(
            &handlers,
            HandlerKind::Graph,
            graph_handler_name,
            &base.graph_provider,
        )?;
        let vector_handler = select_handler(
            &handlers,
            HandlerKind::Vector,
            vector_handler_name,
            &base.vector_provider,
        )?;
        Ok(Self {
            base,
            database,
            graph_factory,
            vector_factory,
            graph_handler,
            vector_handler,
            handlers,
            bound: Mutex::new(HashMap::new()),
        })
    }

    /// The graph engine to hand out: routes every call to the current
    /// dataset's engine.
    pub fn graph_db(self: &Arc<Self>) -> Arc<dyn GraphDBTrait> {
        Arc::new(DatasetRoutedGraphDB::new(
            Arc::clone(self) as Arc<dyn DatasetGraphResolver>
        ))
    }

    /// The vector engine to hand out: routes every call to the current
    /// dataset's engine.
    pub fn vector_db(self: &Arc<Self>) -> Arc<dyn VectorDB> {
        Arc::new(DatasetRoutedVectorDB::new(
            Arc::clone(self) as Arc<dyn DatasetVectorResolver>
        ))
    }

    /// The names of the selected graph and vector handlers.
    pub fn handler_names(&self) -> (&str, &str) {
        (self.graph_handler.name(), self.vector_handler.name())
    }

    /// The dataset's graph engine, provisioning its databases on first use.
    pub async fn graph_for(
        &self,
        dataset_id: Uuid,
    ) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        Ok(Arc::clone(&self.bind(dataset_id).await?.graph))
    }

    /// The dataset's vector engine, provisioning its databases on first use.
    pub async fn vector_for(&self, dataset_id: Uuid) -> Result<Arc<dyn VectorDB>, ComponentError> {
        Ok(Arc::clone(&self.bind(dataset_id).await?.vector))
    }

    /// Bind a dataset: its row (provisioned on first use), then its engines.
    /// Concurrent binds of one dataset share one build.
    async fn bind(&self, dataset_id: Uuid) -> Result<Arc<Bound>, ComponentError> {
        let cell = {
            let mut bound = self.bound.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(bound.entry(dataset_id).or_default())
        };
        cell.get_or_try_init(|| async {
            let row = self.get_or_create_row(dataset_id).await?;
            self.check_embedding_model(&row)?;
            let owner_id = row.owner_id.ok_or_else(|| {
                ComponentError::Config(format!(
                    "dataset_database row of dataset {dataset_id} has no owner_id"
                ))
            })?;
            let graph_handler =
                self.handler(HandlerKind::Graph, &row.graph_dataset_database_handler)?;
            let vector_handler =
                self.handler(HandlerKind::Vector, &row.vector_dataset_database_handler)?;
            let graph_ctx = graph_handler.dataset_context(
                &self.base,
                &HandlerRecord::graph_half(&row),
                owner_id,
            )?;
            let vector_ctx = vector_handler.dataset_context(
                &self.base,
                &HandlerRecord::vector_half(&row),
                owner_id,
            )?;
            let graph = self.graph_factory.build(&graph_ctx).await?;
            let vector = self.vector_factory.build(&vector_ctx).await?;
            tracing::debug!(%dataset_id, "bound per-dataset graph and vector engines");
            Ok::<_, ComponentError>(Arc::new(Bound { graph, vector }))
        })
        .await
        .cloned()
    }

    /// The dataset's row, provisioned on first use (Python
    /// `get_or_create_dataset_database`).
    async fn get_or_create_row(
        &self,
        dataset_id: Uuid,
    ) -> Result<DatasetDatabaseRecord, ComponentError> {
        if let Some(row) = self
            .database
            .get_dataset_database(dataset_id)
            .await
            .map_err(db_error)?
        {
            return Ok(row);
        }
        // Storage is namespaced by the dataset's owner, never by a caller: a
        // grantee's first use provisions the owner's databases (Python #4829).
        let dataset = cognee_database::IngestDb::get_dataset(&*self.database, dataset_id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| {
                ComponentError::Config(format!("Dataset {dataset_id} does not exist."))
            })?;
        let owner_id = dataset.owner_id;

        let graph = self
            .graph_handler
            .create_dataset(&self.base, dataset_id, owner_id)
            .await?;
        let vector = self
            .vector_handler
            .create_dataset(&self.base, dataset_id, owner_id)
            .await?;
        let mut vector_info = vector.connection_info;
        vector_info.insert(
            EMBEDDING_MODEL_KEY.to_string(),
            Value::String(self.base.embedding.model.clone()),
        );
        vector_info.insert(
            EMBEDDING_DIMENSIONS_KEY.to_string(),
            Value::from(self.base.embedding_dimensions),
        );
        let row = DatasetDatabaseRecord {
            dataset_id,
            owner_id: Some(owner_id),
            graph_database_name: graph.database_name,
            vector_database_name: vector.database_name,
            graph_database_provider: graph.provider,
            vector_database_provider: vector.provider,
            graph_dataset_database_handler: self.graph_handler.name().to_string(),
            vector_dataset_database_handler: self.vector_handler.name().to_string(),
            graph_database_url: graph.database_url,
            vector_database_url: vector.database_url,
            graph_database_key: graph.database_key,
            vector_database_key: vector.database_key,
            graph_database_connection_info: Value::Object(graph.connection_info),
            vector_database_connection_info: Value::Object(vector_info),
            cognee_version: Some(format!("cognee-rs {}", env!("CARGO_PKG_VERSION"))),
            migration_revision: None,
            migration_last_error: None,
            created_at: Some(Utc::now()),
            updated_at: None,
        };
        if self
            .database
            .insert_dataset_database(&row)
            .await
            .map_err(db_error)?
        {
            tracing::info!(
                %dataset_id,
                %owner_id,
                graph_handler = self.graph_handler.name(),
                vector_handler = self.vector_handler.name(),
                "provisioned per-dataset storage"
            );
            return Ok(row);
        }
        // Another caller provisioned this dataset between the read and the
        // insert. Names derive from the dataset id, so both created the same
        // databases; the winner's row is the one to use.
        self.database
            .get_dataset_database(dataset_id)
            .await.map_err(db_error)?
            .ok_or_else(|| {
                ComponentError::Config(format!(
                    "dataset_database row of dataset {dataset_id} vanished after a conflicting insert"
                ))
            })
    }

    /// Refuse a dataset built with an embedding width other than the
    /// configured one (Python `ensure_embedding_model_matches`), instead of
    /// failing inside the vector store. A row without a record (written by an
    /// older release) is not checked.
    fn check_embedding_model(&self, row: &DatasetDatabaseRecord) -> Result<(), ComponentError> {
        let info = &row.vector_database_connection_info;
        let Some(stored) = info.get(EMBEDDING_DIMENSIONS_KEY).and_then(Value::as_u64) else {
            return Ok(());
        };
        let configured = self.base.embedding_dimensions as u64;
        if stored == configured {
            return Ok(());
        }
        let stored_model = info
            .get(EMBEDDING_MODEL_KEY)
            .and_then(Value::as_str)
            .unwrap_or("an unknown model");
        Err(ComponentError::Config(format!(
            "dataset {} was embedded with {stored_model} ({stored} dimensions), but the \
             configured embedding model {} produces {configured}; use the original model or \
             delete and rebuild the dataset",
            row.dataset_id, self.base.embedding.model
        )))
    }

    fn handler(
        &self,
        kind: HandlerKind,
        name: &str,
    ) -> Result<Arc<dyn DatasetDatabaseHandler>, ComponentError> {
        self.handlers
            .get(&(kind, name.to_string()))
            .cloned()
            .ok_or_else(|| {
                ComponentError::Config(format!(
                    "no {} dataset database handler named {name:?} is registered",
                    kind.label()
                ))
            })
    }

    /// Take the dataset's engines out of the cache and close them, so its
    /// databases can be dropped (the embedded ones hold file locks).
    async fn evict(&self, dataset_id: Uuid) {
        let cell = {
            self.bound
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&dataset_id)
        };
        if let Some(bound) = cell.and_then(|cell| cell.get().cloned()) {
            if let Err(error) = bound.graph.close().await {
                tracing::warn!(%dataset_id, %error, "closing a dataset's graph engine failed");
            }
            if let Err(error) = bound.vector.close().await {
                tracing::warn!(%dataset_id, %error, "closing a dataset's vector engine failed");
            }
        }
    }

    /// Drop the dataset's databases and its row (Python `delete_dataset`):
    /// the graph database, then the vector database. `Ok(false)` when the
    /// dataset never had per-dataset storage.
    pub async fn delete_dataset_storage(&self, dataset_id: Uuid) -> Result<bool, ComponentError> {
        self.evict(dataset_id).await;
        let Some(row) = self
            .database
            .get_dataset_database(dataset_id)
            .await
            .map_err(db_error)?
        else {
            return Ok(false);
        };
        self.drop_databases(&row).await?;
        self.database
            .delete_dataset_database(dataset_id)
            .await
            .map_err(db_error)?;
        Ok(true)
    }

    /// Drop every dataset's databases and rows (Python `prune_system`'s
    /// `prune_graph_databases` / `prune_vector_databases`). Returns how many
    /// datasets were dropped.
    pub async fn prune(&self) -> Result<usize, ComponentError> {
        let rows = self
            .database
            .list_dataset_databases()
            .await
            .map_err(db_error)?;
        for row in &rows {
            self.evict(row.dataset_id).await;
            self.drop_databases(row).await?;
            self.database
                .delete_dataset_database(row.dataset_id)
                .await
                .map_err(db_error)?;
        }
        Ok(rows.len())
    }

    async fn drop_databases(&self, row: &DatasetDatabaseRecord) -> Result<(), ComponentError> {
        let owner_id = row.owner_id.unwrap_or_else(Uuid::nil);
        self.handler(HandlerKind::Graph, &row.graph_dataset_database_handler)?
            .delete_dataset(&self.base, &HandlerRecord::graph_half(row), owner_id)
            .await?;
        self.handler(HandlerKind::Vector, &row.vector_dataset_database_handler)?
            .delete_dataset(&self.base, &HandlerRecord::vector_half(row), owner_id)
            .await?;
        Ok(())
    }

    fn cached(&self) -> Vec<Arc<Bound>> {
        self.bound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter_map(|cell| cell.get().cloned())
            .collect()
    }

    fn cached_for(&self, dataset_id: Uuid) -> Option<Arc<Bound>> {
        self.bound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&dataset_id)
            .and_then(|cell| cell.get().cloned())
    }
}

fn select_handler(
    handlers: &HandlerMap,
    kind: HandlerKind,
    name: &str,
    provider: &str,
) -> Result<Arc<dyn DatasetDatabaseHandler>, ComponentError> {
    let mut supported: Vec<&str> = handlers
        .keys()
        .filter(|(k, _)| *k == kind)
        .map(|(_, name)| name.as_str())
        .collect();
    supported.sort_unstable();
    let switch_off = "or set ENABLE_BACKEND_ACCESS_CONTROL=false to switch off per-dataset storage";
    let handler = handlers.get(&(kind, name.to_string())).ok_or_else(|| {
        ComponentError::Config(format!(
            "unsupported {kind} dataset database handler {name:?}: per-dataset storage \
                 (ENABLE_BACKEND_ACCESS_CONTROL) needs one for {kind} provider {provider:?}. \
                 Supported {kind} handlers: {supported}. Select one with \
                 {env}_DATASET_DATABASE_HANDLER, {switch_off}",
            kind = kind.label(),
            supported = supported.join(", "),
            env = kind.label().to_ascii_uppercase(),
        ))
    })?;
    if !handler.providers().contains(&provider) {
        return Err(ComponentError::Config(format!(
            "the {kind} dataset database handler {name:?} does not work with {kind} provider \
             {provider:?} (it supports {providers}), {switch_off}",
            kind = kind.label(),
            providers = handler.providers().join(", "),
        )));
    }
    Ok(Arc::clone(handler))
}

fn db_error(error: cognee_database::DatabaseError) -> ComponentError {
    ComponentError::Database(error.to_string())
}

fn graph_error(error: ComponentError) -> GraphDBError {
    GraphDBError::ConnectionError(format!("per-dataset storage: {error}"))
}

fn vector_error(error: ComponentError) -> VectorDBError {
    VectorDBError::StorageError(format!("per-dataset storage: {error}"))
}

#[async_trait]
impl DatasetGraphResolver for DatasetStorage {
    async fn graph_for_dataset(&self, dataset_id: Uuid) -> GraphDBResult<Arc<dyn GraphDBTrait>> {
        self.graph_for(dataset_id).await.map_err(graph_error)
    }

    fn cached_graph(&self, dataset_id: Uuid) -> Option<Arc<dyn GraphDBTrait>> {
        self.cached_for(dataset_id)
            .map(|bound| Arc::clone(&bound.graph))
    }

    fn cached_graphs(&self) -> Vec<Arc<dyn GraphDBTrait>> {
        self.cached()
            .into_iter()
            .map(|bound| Arc::clone(&bound.graph))
            .collect()
    }
}

#[async_trait]
impl DatasetVectorResolver for DatasetStorage {
    async fn vector_for_dataset(&self, dataset_id: Uuid) -> VectorDBResult<Arc<dyn VectorDB>> {
        self.vector_for(dataset_id).await.map_err(vector_error)
    }

    fn cached_vector(&self, dataset_id: Uuid) -> Option<Arc<dyn VectorDB>> {
        self.cached_for(dataset_id)
            .map(|bound| Arc::clone(&bound.vector))
    }

    fn cached_vectors(&self) -> Vec<Arc<dyn VectorDB>> {
        self.cached()
            .into_iter()
            .map(|bound| Arc::clone(&bound.vector))
            .collect()
    }
}

#[cfg(all(test, feature = "sqlite", feature = "ladybug"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_database::IngestDb;
    use cognee_models::Dataset;
    use cognee_models::dataset_scope::with_dataset;
    use serde_json::json;

    use super::*;
    use crate::ComponentRegistry;
    use crate::registry::tests::test_ctx;

    struct Fixture {
        _root: tempfile::TempDir,
        ctx: BackendBuildContext,
        database: Arc<dyn RelationalDb>,
        registry: ComponentRegistry,
    }

    async fn fixture(vector_provider: &str) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let mut ctx = test_ctx();
        ctx.system_root_directory = root.path().join("system");
        ctx.vector_provider = vector_provider.to_string();
        let registry = ComponentRegistry::with_builtins();
        let database = registry.build_database(&ctx).await.unwrap();
        Fixture {
            _root: root,
            ctx,
            database,
            registry,
        }
    }

    async fn dataset(fx: &Fixture, owner: Uuid) -> Uuid {
        let dataset = Dataset::new("ds".to_string(), owner, None, Uuid::new_v4());
        let id = dataset.id;
        IngestDb::create_dataset(&*fx.database, dataset)
            .await
            .unwrap();
        id
    }

    fn storage(fx: &Fixture) -> Arc<DatasetStorage> {
        fx.registry
            .build_dataset_storage(&fx.ctx, Arc::clone(&fx.database))
            .unwrap()
    }

    fn node(id: &str) -> Value {
        json!({"id": id, "name": id, "type": "Entity"})
    }

    #[tokio::test]
    async fn the_first_use_provisions_under_the_owner() {
        let fx = fixture("brute-force").await;
        let owner = Uuid::new_v4();
        let id = dataset(&fx, owner).await;
        let storage = storage(&fx);
        assert_eq!(storage.handler_names(), ("ladybug", MEMORY_HANDLER));
        assert!(
            fx.database
                .get_dataset_database(id)
                .await
                .unwrap()
                .is_none()
        );

        storage.graph_for(id).await.unwrap();
        let row = fx.database.get_dataset_database(id).await.unwrap().unwrap();
        assert_eq!(row.owner_id, Some(owner));
        assert_eq!(row.graph_database_name, format!("{id}.lbug"));
        assert_eq!(row.graph_dataset_database_handler, "ladybug");
        assert_eq!(
            row.vector_database_connection_info[EMBEDDING_DIMENSIONS_KEY],
            json!(384)
        );
        assert!(
            fx.ctx
                .system_root_directory
                .join("databases")
                .join(owner.to_string())
                .join(format!("{id}.lbug"))
                .exists(),
            "the graph file lives under the owner's databases directory"
        );
    }

    #[tokio::test]
    async fn datasets_do_not_see_each_other() {
        let fx = fixture("brute-force").await;
        let owner = Uuid::new_v4();
        let (a, b) = (dataset(&fx, owner).await, dataset(&fx, owner).await);
        let storage = storage(&fx);
        let graph = storage.graph_db();

        with_dataset(a, graph.add_node_raw(node("in-a")))
            .await
            .unwrap();
        assert!(with_dataset(a, graph.has_node("in-a")).await.unwrap());
        assert!(!with_dataset(b, graph.has_node("in-a")).await.unwrap());
        assert!(
            graph.has_node("in-a").await.is_err(),
            "unscoped access fails"
        );
    }

    #[tokio::test]
    async fn concurrent_first_uses_share_one_row_and_one_engine() {
        let fx = fixture("brute-force").await;
        let id = dataset(&fx, Uuid::new_v4()).await;
        let storage = storage(&fx);
        let (first, second) = tokio::join!(storage.graph_for(id), storage.graph_for(id));
        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
        assert_eq!(fx.database.list_dataset_databases().await.unwrap().len(), 1);

        // A second storage (another process, or a rebuilt one) reuses the row.
        let other = super::tests::storage(&fx);
        other.graph_for(id).await.unwrap();
        assert_eq!(fx.database.list_dataset_databases().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_dataset_is_not_provisioned() {
        let fx = fixture("brute-force").await;
        let error = storage(&fx).graph_for(Uuid::new_v4()).await.err().unwrap();
        assert!(error.to_string().contains("does not exist"), "{error}");
        assert!(
            fx.database
                .list_dataset_databases()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn another_embedding_width_is_refused() {
        let mut fx = fixture("brute-force").await;
        let id = dataset(&fx, Uuid::new_v4()).await;
        storage(&fx).vector_for(id).await.unwrap();

        fx.ctx.embedding_dimensions = 1536;
        fx.ctx.embedding.model = "text-embedding-3-small".to_string();
        let error = storage(&fx).vector_for(id).await.err().unwrap();
        let message = error.to_string();
        assert!(message.contains("384 dimensions"), "{message}");
        assert!(message.contains("text-embedding-3-small"), "{message}");
    }

    #[tokio::test]
    async fn a_handler_for_another_provider_is_refused() {
        let mut fx = fixture("brute-force").await;
        fx.ctx.sections.insert(
            "vector.dataset_database_handler".to_string(),
            json!("lancedb"),
        );
        let error = fx
            .registry
            .build_dataset_storage(&fx.ctx, Arc::clone(&fx.database))
            .err()
            .unwrap();
        let message = error.to_string();
        assert!(
            message.contains("does not work with vector provider"),
            "{message}"
        );
        assert!(
            message.contains("ENABLE_BACKEND_ACCESS_CONTROL=false"),
            "{message}"
        );

        fx.ctx
            .sections
            .insert("vector.dataset_database_handler".to_string(), json!("nope"));
        let message = fx
            .registry
            .build_dataset_storage(&fx.ctx, Arc::clone(&fx.database))
            .err()
            .unwrap()
            .to_string();
        assert!(
            message.contains("unsupported vector dataset database handler"),
            "{message}"
        );
        assert!(message.contains("memory"), "{message}");
    }

    #[cfg(feature = "lancedb")]
    #[tokio::test]
    async fn delete_and_prune_drop_the_files_and_rows() {
        let fx = fixture("lancedb").await;
        let owner = Uuid::new_v4();
        let (a, b) = (dataset(&fx, owner).await, dataset(&fx, owner).await);
        let storage = storage(&fx);
        assert_eq!(storage.handler_names(), ("ladybug", "lancedb"));
        let vectors = storage.vector_db();
        for id in [a, b] {
            with_dataset(id, vectors.create_collection("Entity", "name", 384))
                .await
                .unwrap();
            storage.graph_for(id).await.unwrap();
        }
        let owner_dir = fx
            .ctx
            .system_root_directory
            .join("databases")
            .join(owner.to_string());
        assert!(owner_dir.join(format!("{a}.lance.db")).exists());
        assert!(owner_dir.join(format!("{a}.lbug")).exists());

        assert!(storage.delete_dataset_storage(a).await.unwrap());
        assert!(!owner_dir.join(format!("{a}.lance.db")).exists());
        assert!(!owner_dir.join(format!("{a}.lbug")).exists());
        assert!(fx.database.get_dataset_database(a).await.unwrap().is_none());
        assert!(owner_dir.join(format!("{b}.lbug")).exists());
        assert!(!storage.delete_dataset_storage(a).await.unwrap());

        // A deleted dataset's next use starts empty.
        assert!(
            !with_dataset(a, vectors.has_collection("Entity", "name"))
                .await
                .unwrap()
        );

        assert_eq!(storage.prune().await.unwrap(), 2);
        assert!(!owner_dir.join(format!("{b}.lbug")).exists());
        assert!(
            fx.database
                .list_dataset_databases()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
