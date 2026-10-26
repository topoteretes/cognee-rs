//! Built-in dataset database handlers for the Postgres backends: `pgvector` /
//! `postgres_graph` (a database per dataset) and `pgvector_shared` /
//! `postgres_graph_shared` (a schema per dataset in the relational database).
//!
//! Python parity: `vector/pgvector/PGVectorDatasetDatabaseHandler.py`,
//! `PGVectorSharedDatasetDatabaseHandler.py`,
//! `graph/postgres_demo/PostgresGraphDatasetDatabaseHandler.py` and
//! `PostgresGraphSharedDatasetDatabaseHandler.py`. The names (`"{dataset_id}"`
//! databases, `ds_{hex}` schemas) and the connection-info keys are the same, so
//! a `dataset_database` row and the server it describes are interchangeable
//! between the SDKs.
//!
//! No row ever holds a credential: it records host, port, database and schema,
//! and [`DatasetDatabaseHandler::dataset_context`] takes the username and
//! password from the live configuration on every bind — the vector or graph
//! server's resolved URL for the dedicated handlers, the relational URL for
//! the shared ones.

use async_trait::async_trait;
use serde_json::{Map, Value};
use url::Url;
use uuid::Uuid;

use crate::context::BackendBuildContext;
use crate::dataset_storage::{DatasetDatabaseHandler, HandlerKind, HandlerRecord};
use crate::error::ComponentError;
use crate::pg_admin;

/// Where a Postgres handler keeps a dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `CREATE DATABASE "{dataset_id}"` on the graph or vector server. Needs
    /// `CREATEDB`.
    Database,
    /// Schema `ds_{hex}` in the relational database, the adapter pinned to it
    /// through `search_path`. Needs `CREATE` on that database.
    Schema,
}

/// A dataset's graph or vector store in Postgres: its own database, or its
/// own schema of the relational database. See the module docs.
pub struct PostgresDatasetHandler {
    kind: HandlerKind,
    layout: Layout,
}

impl PostgresDatasetHandler {
    /// `pgvector`: a database per dataset on the vector server.
    pub fn pgvector() -> Self {
        Self {
            kind: HandlerKind::Vector,
            layout: Layout::Database,
        }
    }

    /// `pgvector_shared`: a schema per dataset in the relational database.
    pub fn pgvector_shared() -> Self {
        Self {
            kind: HandlerKind::Vector,
            layout: Layout::Schema,
        }
    }

    /// `postgres_graph`: a database per dataset on the graph server.
    pub fn postgres_graph() -> Self {
        Self {
            kind: HandlerKind::Graph,
            layout: Layout::Database,
        }
    }

    /// `postgres_graph_shared`: a schema per dataset in the relational
    /// database.
    pub fn postgres_graph_shared() -> Self {
        Self {
            kind: HandlerKind::Graph,
            layout: Layout::Schema,
        }
    }

    /// The connection-info keys of host, port and schema. Python names them
    /// `host` / `port` / `schema` for vector and prefixes them with
    /// `graph_database_` for graph.
    fn keys(&self) -> [&'static str; 3] {
        match self.kind {
            HandlerKind::Vector => ["host", "port", "schema"],
            HandlerKind::Graph => [
                "graph_database_host",
                "graph_database_port",
                "graph_database_schema",
            ],
        }
    }

    /// The live server URL, credentials included: the graph or vector one for
    /// a database per dataset, the relational one for a schema per dataset.
    fn server(&self, base: &BackendBuildContext) -> Result<Url, ComponentError> {
        match self.layout {
            Layout::Schema => pg_admin::parse_postgres_url(
                &base.relational_db_url,
                &format!(
                    "the relational database URL (the {} handler keeps each dataset in a schema \
                     of the relational database, so it must be Postgres)",
                    self.name()
                ),
            ),
            Layout::Database => {
                let (resolved, setting) = match self.kind {
                    HandlerKind::Vector => (&base.vector_postgres_url, "the vector database URL"),
                    HandlerKind::Graph => (&base.graph_postgres_url, "the graph database URL"),
                };
                match resolved {
                    Some(Ok(url)) => pg_admin::parse_postgres_url(url, setting),
                    Some(Err(cause)) => Err(ComponentError::Config(cause.clone())),
                    None => Err(ComponentError::Config(format!(
                        "the {} dataset database handler requires a resolved Postgres URL for {setting}",
                        self.name()
                    ))),
                }
            }
        }
    }

    fn provider<'a>(&self, base: &'a BackendBuildContext) -> &'a str {
        match self.kind {
            HandlerKind::Vector => &base.vector_provider,
            HandlerKind::Graph => &base.graph_provider,
        }
    }

    /// The URL of the dataset's database: the live server and credentials,
    /// the host and port the row recorded (Python connects to the recorded
    /// ones too), the row's database, and for a schema the `search_path` pin.
    fn dataset_url(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
    ) -> Result<Url, ComponentError> {
        let [host_key, port_key, _] = self.keys();
        let database = pg_admin::validate_database_name(&record.database_name)?;
        let url = pg_admin::database_url(
            &self.server(base)?,
            database,
            record.info_str(host_key),
            info_port(&record.connection_info, port_key),
        )?;
        match self.schema(record)? {
            // `public` stays on the path for pgvector: the extension's types
            // and operators live there (Python `PGVectorAdapter` pins
            // `"<schema>, public"`). The graph tables need nothing outside the
            // schema, so a graph adapter sees only its own (Python
            // `PostgresDemoAdapter` pins the schema alone).
            Some(schema) => match self.kind {
                HandlerKind::Vector => pg_admin::with_search_path(&url, &[schema, "public"]),
                HandlerKind::Graph => pg_admin::with_search_path(&url, &[schema]),
            },
            None => Ok(url),
        }
    }

    /// The row's schema, required in the schema layout.
    fn schema<'a>(&self, record: &'a HandlerRecord) -> Result<Option<&'a str>, ComponentError> {
        if self.layout == Layout::Database {
            return Ok(None);
        }
        let [_, _, schema_key] = self.keys();
        let schema = record.info_str(schema_key).ok_or_else(|| {
            ComponentError::Config(format!(
                "the {} row of dataset database {:?} records no {schema_key:?}",
                self.name(),
                record.database_name
            ))
        })?;
        pg_admin::validate_schema_name(schema).map(Some)
    }
}

/// A recorded port: a number, or the string Python may have stored.
fn info_port(info: &Map<String, Value>, key: &str) -> Option<u16> {
    match info.get(key)? {
        Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[async_trait]
impl DatasetDatabaseHandler for PostgresDatasetHandler {
    fn name(&self) -> &str {
        match (self.kind, self.layout) {
            (HandlerKind::Vector, Layout::Database) => "pgvector",
            (HandlerKind::Vector, Layout::Schema) => "pgvector_shared",
            (HandlerKind::Graph, Layout::Database) => "postgres_graph",
            (HandlerKind::Graph, Layout::Schema) => "postgres_graph_shared",
        }
    }

    fn kind(&self) -> HandlerKind {
        self.kind
    }

    fn providers(&self) -> &[&'static str] {
        match self.kind {
            HandlerKind::Vector => &["pgvector"],
            HandlerKind::Graph => &["postgres", "postgresql"],
        }
    }

    /// Idempotent, and safe to race: both creators are serialized on an
    /// advisory lock keyed by the name, and every name derives from the
    /// dataset id alone.
    async fn create_dataset(
        &self,
        base: &BackendBuildContext,
        dataset_id: Uuid,
        _owner_id: Uuid,
    ) -> Result<HandlerRecord, ComponentError> {
        let server = self.server(base)?;
        let (host, port) = pg_admin::host_port(&server);
        let [host_key, port_key, schema_key] = self.keys();
        let mut info = Map::new();
        info.insert(host_key.to_string(), Value::String(host));
        info.insert(port_key.to_string(), Value::from(port));
        let with_vector_extension = self.kind == HandlerKind::Vector;

        let database_name = match self.layout {
            Layout::Database => {
                let name = dataset_id.to_string();
                if pg_admin::create_database_if_not_exists(&server, &name).await? {
                    tracing::info!(%dataset_id, handler = self.name(), "created the dataset's Postgres database");
                }
                if with_vector_extension {
                    let database = pg_admin::database_url(&server, &name, None, None)?;
                    pg_admin::ensure_vector_extension(&database).await?;
                }
                name
            }
            Layout::Schema => {
                let schema = pg_admin::dataset_schema_name(dataset_id);
                pg_admin::create_schema_if_not_exists(&server, &schema, with_vector_extension)
                    .await?;
                info.insert(schema_key.to_string(), Value::String(schema));
                server.path().trim_start_matches('/').to_string()
            }
        };
        Ok(HandlerRecord {
            provider: self.provider(base).to_string(),
            database_name,
            connection_info: info,
            ..HandlerRecord::default()
        })
    }

    fn dataset_context(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        _owner_id: Uuid,
    ) -> Result<BackendBuildContext, ComponentError> {
        let url = self.dataset_url(base, record)?.to_string();
        let mut ctx = base.clone();
        match self.kind {
            HandlerKind::Vector => {
                ctx.vector_provider = record.provider.clone();
                ctx.vector_postgres_url = Some(Ok(url));
            }
            HandlerKind::Graph => {
                ctx.graph_provider = record.provider.clone();
                ctx.graph_postgres_url = Some(Ok(url));
            }
        }
        Ok(ctx)
    }

    /// Drops the dataset's database, or its schema with everything in it.
    async fn delete_dataset(
        &self,
        base: &BackendBuildContext,
        record: &HandlerRecord,
        _owner_id: Uuid,
    ) -> Result<(), ComponentError> {
        let [host_key, port_key, _] = self.keys();
        let host = record.info_str(host_key);
        let port = info_port(&record.connection_info, port_key);
        match self.schema(record)? {
            None => {
                let server = pg_admin::database_url(
                    &self.server(base)?,
                    pg_admin::MAINTENANCE_DATABASE,
                    host,
                    port,
                )?;
                pg_admin::drop_database_if_exists(&server, &record.database_name).await
            }
            Some(schema) => {
                let database = pg_admin::database_url(
                    &self.server(base)?,
                    pg_admin::validate_database_name(&record.database_name)?,
                    host,
                    port,
                )?;
                pg_admin::drop_schema_if_exists(&database, schema).await
            }
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
    use serde_json::json;

    use super::*;
    use crate::registry::tests::test_ctx;

    fn ctx() -> BackendBuildContext {
        let mut ctx = test_ctx();
        ctx.vector_provider = "pgvector".to_string();
        ctx.graph_provider = "postgres".to_string();
        ctx.vector_postgres_url = Some(Ok(
            "postgres://vu:vp%40ss@vec:5432/cognee?sslmode=require".into()
        ));
        ctx.graph_postgres_url = Some(Ok("postgres://gu:gp@graph:6000/cognee_graph".into()));
        ctx.relational_db_url = "postgres://ru:rp@rel:5433/cognee_db".to_string();
        ctx
    }

    fn record(name: &str, info: Value) -> HandlerRecord {
        HandlerRecord {
            provider: "pgvector".to_string(),
            database_name: name.to_string(),
            connection_info: info.as_object().cloned().unwrap(),
            ..HandlerRecord::default()
        }
    }

    #[test]
    fn handler_names_kinds_and_providers() {
        let cases = [
            (
                PostgresDatasetHandler::pgvector(),
                "pgvector",
                HandlerKind::Vector,
            ),
            (
                PostgresDatasetHandler::pgvector_shared(),
                "pgvector_shared",
                HandlerKind::Vector,
            ),
            (
                PostgresDatasetHandler::postgres_graph(),
                "postgres_graph",
                HandlerKind::Graph,
            ),
            (
                PostgresDatasetHandler::postgres_graph_shared(),
                "postgres_graph_shared",
                HandlerKind::Graph,
            ),
        ];
        for (handler, name, kind) in cases {
            assert_eq!(handler.name(), name);
            assert_eq!(handler.kind(), kind);
        }
        assert_eq!(PostgresDatasetHandler::pgvector().providers(), ["pgvector"]);
        assert_eq!(
            PostgresDatasetHandler::postgres_graph().providers(),
            ["postgres", "postgresql"]
        );
    }

    #[test]
    fn a_dedicated_vector_bind_uses_the_live_credentials_and_the_recorded_server() {
        let id = Uuid::new_v4();
        let handler = PostgresDatasetHandler::pgvector();
        let row = record(&id.to_string(), json!({"host": "vec2", "port": 5544}));
        let ctx = handler.dataset_context(&ctx(), &row, Uuid::nil()).unwrap();
        let url = ctx.vector_postgres_url.unwrap().unwrap();
        assert_eq!(
            url,
            format!("postgres://vu:vp%40ss@vec2:5544/{id}?sslmode=require")
        );
        assert_eq!(ctx.vector_provider, "pgvector");
    }

    #[test]
    fn a_dedicated_graph_bind_reads_pythons_prefixed_keys() {
        let id = Uuid::new_v4();
        let handler = PostgresDatasetHandler::postgres_graph();
        let mut row = record(
            &id.to_string(),
            json!({"graph_database_host": "graph", "graph_database_port": "6000"}),
        );
        row.provider = "postgres".to_string();
        let ctx = handler.dataset_context(&ctx(), &row, Uuid::nil()).unwrap();
        assert_eq!(
            ctx.graph_postgres_url.unwrap().unwrap(),
            format!("postgres://gu:gp@graph:6000/{id}")
        );
    }

    #[test]
    fn a_shared_bind_pins_the_schema_on_the_relational_database() {
        let id = Uuid::new_v4();
        let schema = pg_admin::dataset_schema_name(id);
        let vector_row = record(
            "cognee_db",
            json!({"host": "rel", "port": 5433, "schema": schema}),
        );
        let ctx_v = PostgresDatasetHandler::pgvector_shared()
            .dataset_context(&ctx(), &vector_row, Uuid::nil())
            .unwrap();
        let url = Url::parse(&ctx_v.vector_postgres_url.unwrap().unwrap()).unwrap();
        assert_eq!((url.username(), url.password()), ("ru", Some("rp")));
        assert_eq!(url.path(), "/cognee_db");
        let options: Vec<_> = url.query_pairs().map(|(_, v)| v.into_owned()).collect();
        assert_eq!(options, [format!("-c search_path={schema},public")]);

        let graph_row = record(
            "cognee_db",
            json!({"graph_database_host": "rel", "graph_database_port": 5433, "graph_database_schema": schema}),
        );
        let ctx_g = PostgresDatasetHandler::postgres_graph_shared()
            .dataset_context(&ctx(), &graph_row, Uuid::nil())
            .unwrap();
        let url = Url::parse(&ctx_g.graph_postgres_url.unwrap().unwrap()).unwrap();
        let options: Vec<_> = url.query_pairs().map(|(_, v)| v.into_owned()).collect();
        assert_eq!(
            options,
            [format!("-c search_path={schema}")],
            "graph sees only its schema"
        );
    }

    #[test]
    fn a_shared_row_without_a_valid_schema_is_refused() {
        let handler = PostgresDatasetHandler::pgvector_shared();
        let missing = record("cognee_db", json!({"host": "rel"}));
        assert!(
            handler
                .dataset_context(&ctx(), &missing, Uuid::nil())
                .is_err()
        );
        let evil = record("cognee_db", json!({"schema": "public,ds_x"}));
        assert!(handler.dataset_context(&ctx(), &evil, Uuid::nil()).is_err());
    }

    #[test]
    fn the_shared_handlers_need_a_postgres_relational_database() {
        let mut base = ctx();
        base.relational_db_url = "sqlite::memory:".to_string();
        let row = record("cognee_db", json!({"schema": "ds_abc"}));
        let error = PostgresDatasetHandler::pgvector_shared()
            .dataset_context(&base, &row, Uuid::nil())
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("relational database URL"), "{error}");
    }

    #[test]
    fn an_unresolved_server_url_restates_its_cause() {
        let mut base = ctx();
        base.vector_postgres_url = Some(Err("Missing required Postgres vector credentials".into()));
        let row = record(&Uuid::new_v4().to_string(), json!({}));
        let error = PostgresDatasetHandler::pgvector()
            .dataset_context(&base, &row, Uuid::nil())
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("Missing required Postgres vector credentials"),
            "{error}"
        );
    }

    #[test]
    fn a_tampered_database_name_is_refused() {
        let row = record("x\"; DROP DATABASE cognee; --", json!({}));
        assert!(
            PostgresDatasetHandler::pgvector()
                .dataset_context(&ctx(), &row, Uuid::nil())
                .is_err()
        );
    }
}

/// The four handlers against a live Postgres with pgvector, through
/// [`crate::DatasetStorage`]: isolation, delete, prune, concurrent first binds,
/// and that a shared-schema dataset never touches `public`. Needs
/// `PGVECTOR_TEST_URL=postgres://<user>:<password>@<host>:<port>/<db>` with
/// a role that may `CREATE DATABASE`; skipped when unset.
///
/// Needs both Postgres backends and both relational drivers (SQLite rows for
/// the dedicated handlers, Postgres rows for the shared ones).
// `cfg(test)` on a line of its own: the env-boundary check keys test modules
// on it, and the full list would not fit on one line.
#[cfg(test)]
#[cfg(all(
    feature = "pgvector",
    feature = "pggraph",
    feature = "postgres",
    feature = "sqlite"
))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod live_tests {

    use std::sync::Arc;

    use cognee_database::{IngestDb, RelationalDb};
    use cognee_graph::GraphDBTrait;
    use cognee_models::Dataset;
    use cognee_models::dataset_scope::with_dataset;
    use cognee_vector::{VectorDB, VectorPoint};
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use serde_json::json;

    use super::*;
    use crate::registry::tests::test_ctx;
    use crate::{ComponentRegistry, DatasetStorage};

    #[allow(
        clippy::disallowed_methods,
        reason = "the Postgres server under test is named by the process env"
    )]
    fn server_url(test: &str) -> Option<Url> {
        let Some(url) = std::env::var("PGVECTOR_TEST_URL")
            .ok()
            .filter(|u| !u.is_empty())
        else {
            eprintln!("PGVECTOR_TEST_URL not set — skipping {test}");
            return None;
        };
        Some(pg_admin::parse_postgres_url(&url, "PGVECTOR_TEST_URL").unwrap())
    }

    struct Fixture {
        ctx: BackendBuildContext,
        database: Arc<dyn RelationalDb>,
        registry: ComponentRegistry,
    }

    impl Fixture {
        fn storage(&self) -> Arc<DatasetStorage> {
            self.registry
                .build_dataset_storage(&self.ctx, Arc::clone(&self.database))
                .unwrap()
        }

        async fn dataset(&self) -> Uuid {
            let dataset = Dataset::new("ds".to_string(), Uuid::new_v4(), None, Uuid::new_v4());
            let id = dataset.id;
            IngestDb::create_dataset(&*self.database, dataset)
                .await
                .unwrap();
            id
        }
    }

    fn base_ctx(server: &Url) -> BackendBuildContext {
        let mut ctx = test_ctx();
        ctx.graph_provider = "postgres".to_string();
        ctx.vector_provider = "pgvector".to_string();
        ctx.graph_postgres_url = Some(Ok(server.to_string()));
        ctx.vector_postgres_url = Some(Ok(server.to_string()));
        ctx
    }

    /// Dedicated databases, rows in an in-memory SQLite.
    async fn dedicated(server: &Url) -> Fixture {
        let ctx = base_ctx(server);
        let registry = ComponentRegistry::with_builtins();
        let database = registry.build_database(&ctx).await.unwrap();
        Fixture {
            ctx,
            database,
            registry,
        }
    }

    /// Shared schemas in a fresh Postgres relational database of this test's
    /// own (so `public` holds exactly what this test put there).
    async fn shared(server: &Url, relational: &Url) -> Fixture {
        let mut ctx = base_ctx(server);
        ctx.relational_db_url = relational.to_string();
        for (key, value) in [
            ("relational.provider", "postgres"),
            ("graph.dataset_database_handler", "postgres_graph_shared"),
            ("vector.dataset_database_handler", "pgvector_shared"),
        ] {
            ctx.sections.insert(key.to_string(), json!(value));
        }
        let registry = ComponentRegistry::with_builtins();
        let database = registry.build_database(&ctx).await.unwrap();
        Fixture {
            ctx,
            database,
            registry,
        }
    }

    async fn connect(url: &Url) -> DatabaseConnection {
        Database::connect(url.as_str()).await.unwrap()
    }

    async fn strings(db: &DatabaseConnection, sql: &str) -> Vec<String> {
        db.query_all(Statement::from_string(db.get_database_backend(), sql))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get_by_index::<String>(0).unwrap())
            .collect()
    }

    async fn database_exists(server: &Url, name: &str) -> bool {
        let admin = connect(&pg_admin::database_url(server, "postgres", None, None).unwrap()).await;
        let found = strings(
            &admin,
            &format!("SELECT datname::text FROM pg_database WHERE datname = '{name}'"),
        )
        .await;
        admin.close().await.unwrap();
        !found.is_empty()
    }

    async fn force_drop(server: &Url, name: &str) {
        let admin = connect(&pg_admin::database_url(server, "postgres", None, None).unwrap()).await;
        admin
            .execute_unprepared(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
            .await
            .unwrap();
        admin.close().await.unwrap();
    }

    fn node(id: &str) -> Value {
        json!({"id": id, "name": id, "type": "Entity"})
    }

    fn point() -> VectorPoint {
        VectorPoint::new(Uuid::new_v4(), vec![0.5; 384])
    }

    /// Write a node and a vector into `a`, then read both back in `a` and
    /// find neither in `b`.
    async fn assert_isolated(storage: &Arc<DatasetStorage>, a: Uuid, b: Uuid) {
        let graph = storage.graph_db();
        let vector = storage.vector_db();
        with_dataset(a, graph.add_node_raw(node("in-a")))
            .await
            .unwrap();
        with_dataset(a, vector.create_collection("Entity", "name", 384))
            .await
            .unwrap();
        with_dataset(a, vector.index_points("Entity", "name", &[point()]))
            .await
            .unwrap();

        assert!(with_dataset(a, graph.has_node("in-a")).await.unwrap());
        assert_eq!(
            with_dataset(a, vector.collection_size("Entity", "name"))
                .await
                .unwrap(),
            1
        );
        assert!(!with_dataset(b, graph.has_node("in-a")).await.unwrap());
        assert!(
            !with_dataset(b, vector.has_collection("Entity", "name"))
                .await
                .unwrap()
        );
        let hits = with_dataset(b, vector.search_similar("Entity", "name", &[0.5; 384], 5)).await;
        assert!(
            hits.map(|h| h.is_empty()).unwrap_or(true),
            "b finds a's vectors"
        );
    }

    #[tokio::test]
    async fn dedicated_databases_isolate_and_drop_datasets() {
        let Some(server) = server_url("dedicated_databases_isolate_and_drop_datasets") else {
            return;
        };
        let fx = dedicated(&server).await;
        let (a, b) = (fx.dataset().await, fx.dataset().await);
        let storage = fx.storage();
        assert_eq!(storage.handler_names(), ("postgres_graph", "pgvector"));
        assert_isolated(&storage, a, b).await;

        let row = fx.database.get_dataset_database(a).await.unwrap().unwrap();
        assert_eq!(row.graph_database_name, a.to_string());
        assert_eq!(row.vector_database_name, a.to_string());
        let (host, port) = pg_admin::host_port(&server);
        assert_eq!(row.vector_database_connection_info["host"], json!(host));
        assert_eq!(row.vector_database_connection_info["port"], json!(port));
        assert_eq!(
            row.graph_database_connection_info["graph_database_host"],
            json!(host)
        );
        let stored = serde_json::to_string(&(
            &row.graph_database_connection_info,
            &row.vector_database_connection_info,
            &row.graph_database_url,
            &row.vector_database_url,
        ))
        .unwrap();
        if let Some(password) = server.password().filter(|p| !p.is_empty()) {
            assert!(
                !stored.contains(password),
                "a row stores the password: {stored}"
            );
        }
        assert!(database_exists(&server, &a.to_string()).await);

        assert!(storage.delete_dataset_storage(a).await.unwrap());
        assert!(!database_exists(&server, &a.to_string()).await);
        assert!(database_exists(&server, &b.to_string()).await);
        assert!(
            !with_dataset(a, storage.graph_db().has_node("in-a"))
                .await
                .unwrap(),
            "a deleted dataset's next use starts empty"
        );

        assert_eq!(storage.prune().await.unwrap(), 2);
        assert!(!database_exists(&server, &a.to_string()).await);
        assert!(!database_exists(&server, &b.to_string()).await);
        assert!(
            fx.database
                .list_dataset_databases()
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Two storages (two processes, as far as the server can tell) bind one
    /// fresh dataset at once: one database, one row, and each sees the other's
    /// writes.
    #[tokio::test]
    async fn concurrent_first_binds_converge_on_one_database() {
        let Some(server) = server_url("concurrent_first_binds_converge_on_one_database") else {
            return;
        };
        let fx = dedicated(&server).await;
        let id = fx.dataset().await;
        let (first, second) = (fx.storage(), fx.storage());
        let (g1, g2) = tokio::join!(first.graph_for(id), second.graph_for(id));
        let (g1, g2) = (g1.unwrap(), g2.unwrap());
        assert_eq!(fx.database.list_dataset_databases().await.unwrap().len(), 1);

        g1.add_node_raw(node("shared")).await.unwrap();
        assert!(g2.has_node("shared").await.unwrap());

        // The second storage is another process's as far as the server can
        // tell, and still holds pools to the database. Deleting the dataset
        // terminates them (`DROP DATABASE … WITH (FORCE)`), as Python does.
        assert!(first.delete_dataset_storage(id).await.unwrap());
        assert!(!database_exists(&server, &id.to_string()).await);
    }

    /// What `public` holds: its tables, and the rows of the decoy tables and
    /// the relational migration bookkeeping.
    async fn public_snapshot(db: &DatabaseConnection) -> Vec<String> {
        let mut snapshot = strings(
            db,
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' ORDER BY 1",
        )
        .await;
        for table in ["graph_node", "_vector_collections", "seaql_migrations"] {
            snapshot.extend(
                strings(db, &format!("SELECT count(*)::text FROM public.{table}"))
                    .await
                    .into_iter()
                    .map(|count| format!("{table}={count}")),
            );
        }
        snapshot
    }

    #[tokio::test]
    async fn shared_schemas_isolate_datasets_and_leave_public_alone() {
        let Some(server) = server_url("shared_schemas_isolate_datasets_and_leave_public_alone")
        else {
            return;
        };
        let scratch = format!("cognee_test_shared_{}", Uuid::new_v4().simple());
        pg_admin::create_database_if_not_exists(&server, &scratch)
            .await
            .unwrap();
        let relational = pg_admin::database_url(&server, &scratch, None, None).unwrap();
        let fx = shared(&server, &relational).await;

        // Decoys: an unpinned graph and vector store in the same database's
        // `public`, holding exactly what a dataset will look for. A pinned
        // adapter that reached past its schema would find them.
        let decoy_graph = cognee_graph::PgGraphAdapter::new(relational.as_str())
            .await
            .unwrap();
        decoy_graph.add_node_raw(node("in-a")).await.unwrap();
        decoy_graph.close().await.unwrap();
        let decoy_vector = cognee_vector::PgVectorAdapter::new(relational.as_str(), 384)
            .await
            .unwrap();
        decoy_vector
            .create_collection("Entity", "name", 384)
            .await
            .unwrap();
        decoy_vector
            .index_points("Entity", "name", &[point(), point()])
            .await
            .unwrap();
        decoy_vector.close().await.unwrap();

        let inspect = connect(&relational).await;
        let before = public_snapshot(&inspect).await;

        let (a, b) = (fx.dataset().await, fx.dataset().await);
        let storage = fx.storage();
        assert_eq!(
            storage.handler_names(),
            ("postgres_graph_shared", "pgvector_shared")
        );
        // A first bind of `b` from a second storage, racing the first one's.
        let other = fx.storage();
        let (left, right) = tokio::join!(storage.vector_for(b), other.vector_for(b));
        left.unwrap();
        right.unwrap();
        assert!(
            !with_dataset(a, storage.graph_db().has_node("in-a"))
                .await
                .unwrap(),
            "a pinned graph adapter reads public's graph_node"
        );
        assert!(
            !with_dataset(a, storage.vector_db().has_collection("Entity", "name"))
                .await
                .unwrap(),
            "a pinned vector adapter reads public's collections"
        );
        assert_isolated(&storage, a, b).await;
        assert_eq!(public_snapshot(&inspect).await, before, "public changed");

        let schema_a = pg_admin::dataset_schema_name(a);
        let schema_b = pg_admin::dataset_schema_name(b);
        let row = fx.database.get_dataset_database(a).await.unwrap().unwrap();
        assert_eq!(row.vector_database_name, scratch);
        assert_eq!(
            row.vector_database_connection_info["schema"],
            json!(schema_a)
        );
        assert_eq!(
            row.graph_database_connection_info["graph_database_schema"],
            json!(schema_a)
        );
        let tables_a = strings(
            &inspect,
            &format!(
                "SELECT table_name::text FROM information_schema.tables \
                 WHERE table_schema = '{schema_a}' ORDER BY 1"
            ),
        )
        .await;
        for table in [
            "graph_node",
            "graph_edge",
            "seaql_migrations_pggraph",
            "seaql_migrations_pgvector",
            "_vector_collections",
            "Entity_name",
        ] {
            assert!(
                tables_a.contains(&table.to_string()),
                "{table} not in {schema_a}: {tables_a:?}"
            );
        }

        let schemas = "SELECT schema_name::text FROM information_schema.schemata \
                       WHERE schema_name LIKE 'ds\\_%' ORDER BY 1";
        assert!(storage.delete_dataset_storage(a).await.unwrap());
        assert_eq!(strings(&inspect, schemas).await, [schema_b]);
        other.prune().await.unwrap();
        assert_eq!(
            storage.prune().await.unwrap(),
            0,
            "the other storage pruned b"
        );
        assert!(strings(&inspect, schemas).await.is_empty());
        assert!(
            fx.database
                .list_dataset_databases()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(public_snapshot(&inspect).await, before, "public changed");

        inspect.close().await.unwrap();
        force_drop(&server, &scratch).await;
    }
}
