//! Shared test helpers for the cognee-http-server integration tests.
#![allow(dead_code)]

use axum::{Router, body::Body, http::Request};
use tower::ServiceExt;

use cognee_http_server::{AppState, HttpServerConfig, build_router};

// ─── Basic helpers (P0) ──────────────────────────────────────────────────────

/// Build an `AppState` suitable for tests.
///
/// Uses default config (localhost:0, MockHealthChecker, no auth).
pub async fn build_test_state() -> AppState {
    AppState::build(HttpServerConfig::default())
        .await
        .expect("AppState::build")
}

/// Build a test state with explicit CORS origins.
pub async fn build_test_state_with_cors(origins: Vec<String>) -> AppState {
    let cfg = HttpServerConfig {
        cors_allowed_origins: origins,
        ..HttpServerConfig::default()
    };
    AppState::build(cfg).await.expect("AppState::build")
}

/// Build the full router from a test state.
pub async fn test_router(state: AppState) -> Router {
    build_router(state).await.expect("build_router")
}

/// Fire a single GET request at the given path against the router.
pub async fn oneshot_get(app: Router, path: &str) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("request");
    app.oneshot(req).await.expect("response")
}

/// Fire a single request with explicit headers.
pub async fn oneshot_request(app: Router, req: Request<Body>) -> axum::response::Response {
    app.oneshot(req).await.expect("response")
}

/// Read the response body as parsed JSON.
pub async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    use axum::body::to_bytes;
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

// ─── P1 helpers (auth) ───────────────────────────────────────────────────────

use cognee_database::{
    AuthUser, SeaOrmApiKeyRepository, SeaOrmUserAuthRepository, UpdateUserPayload,
};
use cognee_http_server::auth::AuthContext;
use cognee_http_server::auth::mailer::{ConsoleMailer, MailEvent};
use sea_orm::{ConnectionTrait, Database, EntityTrait, Statement};
use std::sync::Arc;

/// Build an in-memory SQLite DB with the auth tables.
pub async fn setup_auth_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite::memory:");

    let ddl = [
        "CREATE TABLE IF NOT EXISTS principals (
            id TEXT PRIMARY KEY NOT NULL,
            type TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT
        )",
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY NOT NULL,
            email TEXT NOT NULL UNIQUE,
            hashed_password TEXT NOT NULL DEFAULT '',
            is_active BOOLEAN NOT NULL DEFAULT 1,
            is_superuser BOOLEAN NOT NULL DEFAULT 0,
            is_verified BOOLEAN NOT NULL DEFAULT 1,
            tenant_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT
        )",
        "CREATE TABLE IF NOT EXISTS user_api_key (
            id TEXT PRIMARY KEY NOT NULL,
            user_id TEXT NOT NULL,
            api_key TEXT NOT NULL,
            label TEXT,
            name TEXT,
            created_at TEXT,
            expires_at TEXT
        )",
    ];

    for sql in ddl {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            sql.to_owned(),
        ))
        .await
        .expect("create table");
    }

    db
}

/// Insert a `principals` row (required FK parent for `users`).
pub async fn insert_principal(db: &sea_orm::DatabaseConnection, id: &str) {
    use cognee_database::entities::principal;
    use sea_orm::{ActiveValue::Set, InsertResult};
    let am = principal::ActiveModel {
        id: Set(id.to_owned()),
        principal_type: Set("user".to_owned()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    };
    let _: InsertResult<principal::ActiveModel> = principal::Entity::insert(am)
        .exec(db)
        .await
        .expect("insert principal");
}

/// Build a test `AppState` backed by an in-memory SQLite DB with auth enabled.
/// Returns `(state, mail_events)` — `mail_events` captures emails sent during the test.
pub async fn build_auth_test_state() -> (AppState, Arc<std::sync::Mutex<Vec<MailEvent>>>) {
    let db = setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });

    use cognee_http_server::config::Environment;
    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev, // accept default secret without env vars
        ..HttpServerConfig::default()
    };

    let (mailer, events) = ConsoleMailer::new();

    // Build auth context — no env vars needed in Dev mode (super_secret accepted)
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    };

    (state, events)
}

/// Seed a user into the state's user repo.
/// Returns the created `AuthUser`.
pub async fn seed_user(state: &AppState, email: &str, password: &str) -> AuthUser {
    use cognee_http_server::auth::register::create_user;
    let auth = state.auth.as_ref().expect("auth ctx");
    let mailer = state.mailer.as_ref();
    create_user(email, password, mailer, auth)
        .await
        .expect("seed_user")
}

/// Seed a superuser.
pub async fn seed_superuser(state: &AppState, email: &str, password: &str) -> AuthUser {
    let user = seed_user(state, email, password).await;
    let auth = state.auth.as_ref().expect("auth ctx");
    auth.user_repo
        .update(
            user.id,
            UpdateUserPayload {
                is_superuser: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("make superuser")
}

/// Build a test `AppState` with authentication **required** (401 for unauthenticated requests).
///
/// Use this variant when testing the 401 auth guard — `build_auth_test_state` uses
/// `require_authentication: false` (anonymous default user), which never returns 401.
pub async fn build_auth_required_test_state() -> (AppState, Arc<std::sync::Mutex<Vec<MailEvent>>>) {
    let db = setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });

    use cognee_http_server::config::Environment;
    let cfg = HttpServerConfig {
        require_authentication: true,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };

    let (mailer, events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    };

    (state, events)
}

/// Build a `Bearer <jwt>` header value for the given user.
pub fn bearer_header(user: &AuthUser, state: &AppState) -> String {
    use cognee_http_server::auth::jwt::encode_login_jwt;
    let auth = state.auth.as_ref().expect("auth ctx");
    let token = encode_login_jwt(user.id, auth).expect("encode jwt");
    format!("Bearer {token}")
}

/// Build a cookie header value for the given user.
pub fn cookie_header(user: &AuthUser, state: &AppState) -> String {
    use cognee_http_server::auth::jwt::encode_login_jwt;
    let auth = state.auth.as_ref().expect("auth ctx");
    let token = encode_login_jwt(user.id, auth).expect("encode jwt");
    format!("{}={}", auth.cookie_name, token)
}

// ─── P4 helpers (read path) ───────────────────────────────────────────────────

use async_trait::async_trait;
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_graph::GraphDBTrait;
use cognee_http_server::components::ComponentHandles;
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, LlmError, LlmResult};
use cognee_search::orchestration::{SearchOrchestrator, SearchTypeRegistry};
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};
use cognee_session::SessionContext;

/// Stub retriever for P4 integration tests — returns canned text/items.
pub struct StubRetriever {
    pub kind: SearchType,
    pub text: Option<String>,
    pub items: Option<Vec<cognee_search::types::SearchItem>>,
    pub error: Option<String>,
}

impl StubRetriever {
    pub fn text_for(kind: SearchType, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: Some(text.into()),
            items: None,
            error: None,
        }
    }

    pub fn items_for(kind: SearchType, items: Vec<cognee_search::types::SearchItem>) -> Self {
        Self {
            kind,
            text: None,
            items: Some(items),
            error: None,
        }
    }

    pub fn error_for(kind: SearchType, message: impl Into<String>) -> Self {
        Self {
            kind,
            text: None,
            items: None,
            error: Some(message.into()),
        }
    }
}

#[async_trait]
impl SearchRetriever for StubRetriever {
    fn search_type(&self) -> SearchType {
        self.kind
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if let Some(msg) = &self.error {
            return Err(SearchError::InvalidInput(msg.clone()));
        }
        Ok(self.items.clone().unwrap_or_default())
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        if let Some(msg) = &self.error {
            return Err(SearchError::InvalidInput(msg.clone()));
        }
        if let Some(text) = &self.text {
            return Ok(SearchOutput::Text(text.clone()));
        }
        Ok(SearchOutput::Items(self.items.clone().unwrap_or_default()))
    }
}

/// Build a fresh in-memory `DatabaseConnection` with all migrations applied.
pub async fn build_search_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("in-memory sqlite");
    initialize(&db).await.expect("init schema");
    Arc::new(db)
}

/// Build a `SearchOrchestrator` with one stub retriever and a wired DB.
pub async fn build_orchestrator(
    db: Arc<DatabaseConnection>,
    retriever: Arc<dyn SearchRetriever>,
) -> Arc<SearchOrchestrator> {
    let mut registry = SearchTypeRegistry::new();
    registry.register(retriever);
    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(db.clone() as Arc<dyn cognee_database::SearchHistoryDb>);
    Arc::new(orchestrator)
}

/// Mock LLM adapter used by P4 LLM-router tests.
pub struct MockLlm {
    response: String,
    /// Records the most recent system+user prompts so tests can assert that
    /// `safe_params` filtering preserved/dropped the right keys.
    pub last_options: Arc<std::sync::Mutex<Option<GenerationOptions>>>,
}

impl MockLlm {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            last_options: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

#[async_trait]
impl Llm for MockLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        // lock poison is unrecoverable
        *self.last_options.lock().unwrap() = options.clone();
        Ok(GenerationResponse {
            content: self.response.clone(),
            model: "mock".into(),
            usage: None,
            finish_reason: None,
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        // lock poison is unrecoverable
        *self.last_options.lock().unwrap() = options.clone();
        Ok(serde_json::Value::String(self.response.clone()))
    }

    fn model(&self) -> &str {
        "mock"
    }
}

/// LLM that always errors — used to verify the recall/llm error envelope.
pub struct FailingLlm;

#[async_trait]
impl Llm for FailingLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::NetworkError("simulated".into()))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        Err(LlmError::NetworkError("simulated".into()))
    }

    fn model(&self) -> &str {
        "failing"
    }
}

// ─── P7 helpers (notebooks) ──────────────────────────────────────────────────

/// Build a fresh `AppState` backed by a `DatabaseConnection` with all migrations
/// (including the P7 `notebooks` table).  Auth is `require_authentication: false`
/// so tests can supply explicit bearer tokens without seeding JWT secrets.
///
/// Returns `(state, mail_events)`.
pub async fn build_notebooks_state() -> (AppState, Arc<std::sync::Mutex<Vec<MailEvent>>>) {
    let db = build_search_db().await;

    let db_for_auth: sea_orm::DatabaseConnection = (*db).clone();
    let user_repo = Arc::new(SeaOrmUserAuthRepository {
        db: db_for_auth.clone(),
    });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository {
        db: db_for_auth.clone(),
    });

    use cognee_http_server::config::Environment;
    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };

    let (mailer, events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let handles = build_component_handles(db.clone(), None, None, None);

    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: Some(handles),
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    };

    (state, events)
}

/// Build a `ComponentHandles` plumbing the supplied DB, optional search
/// orchestrator, optional LLM, and optional graph DB. File storage and
/// delete service use minimal-on-disk defaults rooted at a tempdir.
pub fn build_component_handles(
    db: Arc<DatabaseConnection>,
    search_orchestrator: Option<Arc<SearchOrchestrator>>,
    llm: Option<Arc<dyn Llm>>,
    graph_db: Option<Arc<dyn GraphDBTrait>>,
) -> Arc<ComponentHandles> {
    use cognee_delete::DeleteService;
    use cognee_ontology::OntologyManager;
    use cognee_storage::LocalStorage;

    let storage_dir = tempfile::tempdir().expect("tmp dir");
    let storage = Arc::new(LocalStorage::new(storage_dir.path().to_path_buf()))
        as Arc<dyn cognee_storage::StorageTrait>;
    // Leak the tempdir so it remains valid for the lifetime of the test.
    Box::leak(Box::new(storage_dir));
    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        db.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));
    let ontology_dir = tempfile::tempdir().expect("tmp dir");
    let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
    Box::leak(Box::new(ontology_dir));
    let permissions: Option<Arc<dyn cognee_database::permissions::PermissionsRepository>> = Some(
        Arc::new(cognee_database::permissions::SeaOrmPermissionsRepository::new(db.clone())),
    );
    let sync_ops: Option<Arc<dyn cognee_database::SyncOperationRepository>> = Some(Arc::new(
        cognee_database::SeaOrmSyncOperationRepository::new(db.clone()),
    ));
    Arc::new(ComponentHandles {
        database: db,
        storage,
        delete_service,
        ontology_manager,
        search_orchestrator,
        llm,
        graph_db,
        permissions,
        sync_ops,
        session_store: None,
        session_manager: None,
    })
}

/// Build an `AppState` wired for the P4 read-path tests.
pub async fn build_p4_state(
    search_orchestrator: Option<Arc<SearchOrchestrator>>,
    llm: Option<Arc<dyn Llm>>,
    graph_db: Option<Arc<dyn GraphDBTrait>>,
) -> AppState {
    let db = build_search_db().await;
    let handles = build_component_handles(db, search_orchestrator, llm, graph_db);
    let cfg = HttpServerConfig::default();
    let mut state = AppState::build(cfg).await.expect("build state");
    state.lib = Some(handles);
    state
}

// ─── P5 helpers (permissions HTTP tests) ─────────────────────────────────────
//
// `build_permissions_state` shares one `DatabaseConnection` between
// `auth.user_repo` (used by the `AuthenticatedUser` extractor) and the
// `ComponentHandles.permissions` repository. This is what lets HTTP-level
// permission tests authenticate as a specific user via bearer JWT *and* have
// that user be visible to `PermissionsRepository`.

/// Build an `AppState` with auth + permissions wired against the same
/// in-memory SQLite DB. `require_authentication: false` so the default user
/// works when no Authorization header is sent — but every HTTP test for
/// permissions sends a bearer header anyway.
pub async fn build_permissions_state() -> AppState {
    let db_conn = build_search_db().await;
    let db_for_auth: sea_orm::DatabaseConnection = (*db_conn).clone();

    let user_repo = Arc::new(SeaOrmUserAuthRepository {
        db: db_for_auth.clone(),
    });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository {
        db: db_for_auth.clone(),
    });

    use cognee_http_server::config::Environment;
    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };

    let (mailer, _events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let handles = build_component_handles(db_conn.clone(), None, None, None);

    AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: Some(handles),
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    }
}

/// Borrow the underlying `DatabaseConnection` from a state built via
/// [`build_permissions_state`]. Tests use this to seed `principals`,
/// `tenants`, `acls`, and the default-permission tables directly.
pub fn permissions_db(state: &AppState) -> &cognee_database::DatabaseConnection {
    state.components().expect("components").database.as_ref()
}

/// Borrow the `PermissionsRepository` wired into the state.
pub fn permissions_repo(
    state: &AppState,
) -> &Arc<dyn cognee_database::permissions::PermissionsRepository> {
    state
        .components()
        .expect("components")
        .permissions
        .as_ref()
        .expect("permissions repo wired")
}

/// Insert a `principals` row required as FK parent for `users`/`tenants`/`roles`.
/// The migrator does not enforce FKs in SQLite without `PRAGMA foreign_keys=ON`,
/// but seeding the row keeps the data shape consistent with the SeaORM impl
/// queries that join on `principals`.
pub async fn ensure_principal(
    db: &cognee_database::DatabaseConnection,
    id: uuid::Uuid,
    kind: &str,
) {
    use cognee_database::entities::principal;
    let hex = id.simple().to_string();
    // Existing row → no-op.
    if principal::Entity::find_by_id(hex.clone())
        .one(db)
        .await
        .expect("principal find")
        .is_some()
    {
        return;
    }
    let am = principal::ActiveModel {
        id: sea_orm::ActiveValue::Set(hex),
        principal_type: sea_orm::ActiveValue::Set(kind.into()),
        created_at: sea_orm::ActiveValue::Set(chrono::Utc::now()),
        updated_at: sea_orm::ActiveValue::Set(None),
    };
    use sea_orm::EntityTrait;
    principal::Entity::insert(am)
        .exec(db)
        .await
        .expect("insert principal");
}

/// Seed a user against the unified permissions DB. Pre-inserts the matching
/// `principals` row so the FK on `users.id → principals.id` is satisfied
/// (sqlx-sqlite enables `PRAGMA foreign_keys=ON` by default), then creates
/// the user via the low-level `UserAuthRepository::create` path so the
/// password is properly hashed.
pub async fn seed_perm_user(state: &AppState, email: &str, password: &str) -> AuthUser {
    use cognee_database::CreateUserPayload;
    use cognee_http_server::auth::password::hash_new_password;

    let id = uuid::Uuid::new_v4();
    ensure_principal(permissions_db(state), id, "user").await;

    let auth = state.auth.as_ref().expect("auth ctx");
    let hashed = hash_new_password(password).expect("hash");
    auth.user_repo
        .create(CreateUserPayload {
            id,
            email: email.into(),
            hashed_password: hashed,
            is_active: true,
            is_superuser: false,
            is_verified: true,
            tenant_id: None,
        })
        .await
        .expect("create user")
}

/// Seed a dataset row owned by `owner`, optionally tagged with `tenant_id`.
pub async fn seed_dataset(
    db: &cognee_database::DatabaseConnection,
    dataset_id: uuid::Uuid,
    owner_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
    name: &str,
) {
    use cognee_database::entities::dataset;
    use sea_orm::ActiveValue::Set;
    let hex = |u: uuid::Uuid| u.simple().to_string();
    use sea_orm::EntityTrait;
    dataset::Entity::insert(dataset::ActiveModel {
        id: Set(hex(dataset_id)),
        name: Set(name.into()),
        owner_id: Set(hex(owner_id)),
        tenant_id: Set(tenant_id.map(hex)),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    })
    .exec(db)
    .await
    .expect("insert dataset");
}

/// Seed a tenant with the given owner. Inserts the matching `principals` row.
pub async fn seed_tenant(
    db: &cognee_database::DatabaseConnection,
    tenant_id: uuid::Uuid,
    owner_id: uuid::Uuid,
    name: &str,
) {
    use cognee_database::entities::tenant;
    use sea_orm::ActiveValue::Set;
    let hex = |u: uuid::Uuid| u.simple().to_string();
    ensure_principal(db, tenant_id, "tenant").await;
    use sea_orm::EntityTrait;
    tenant::Entity::insert(tenant::ActiveModel {
        id: Set(hex(tenant_id)),
        name: Set(name.into()),
        owner_id: Set(hex(owner_id)),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    })
    .exec(db)
    .await
    .expect("insert tenant");
}

/// Add a row to `user_tenants` (M2M membership).
pub async fn seed_user_tenant_membership(
    db: &cognee_database::DatabaseConnection,
    user_id: uuid::Uuid,
    tenant_id: uuid::Uuid,
) {
    use cognee_database::entities::user_tenant;
    use sea_orm::ActiveValue::Set;
    let hex = |u: uuid::Uuid| u.simple().to_string();
    use sea_orm::EntityTrait;
    user_tenant::Entity::insert(user_tenant::ActiveModel {
        user_id: Set(hex(user_id)),
        tenant_id: Set(hex(tenant_id)),
        created_at: Set(chrono::Utc::now()),
    })
    .exec(db)
    .await
    .expect("insert user_tenant");
}

/// Set `users.tenant_id` for the given user.
pub async fn set_current_tenant(
    db: &cognee_database::DatabaseConnection,
    user_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
) {
    use cognee_database::entities::user;
    use sea_orm::ActiveValue::Set;
    use sea_orm::{EntityTrait, IntoActiveModel};
    let user_hex = user_id.simple().to_string();
    let row = user::Entity::find_by_id(user_hex)
        .one(db)
        .await
        .expect("find user")
        .expect("user row");
    let mut am = row.into_active_model();
    am.tenant_id = Set(tenant_id.map(|t| t.simple().to_string()));
    use sea_orm::ActiveModelTrait;
    am.update(db).await.expect("update user.tenant_id");
}

/// Look up a permissions row id by name.
pub async fn permission_id_by_name(db: &cognee_database::DatabaseConnection, name: &str) -> String {
    use cognee_database::entities::permission;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    permission::Entity::find()
        .filter(permission::Column::Name.eq(name))
        .one(db)
        .await
        .expect("permission lookup")
        .expect("permission seed missing")
        .id
}

/// Insert a `roles` row + matching `principals` entry (`type='role'`).
pub async fn seed_role(
    db: &cognee_database::DatabaseConnection,
    role_id: uuid::Uuid,
    tenant_id: uuid::Uuid,
    name: &str,
) {
    use cognee_database::entities::role;
    use sea_orm::ActiveValue::Set;
    let hex = |u: uuid::Uuid| u.simple().to_string();
    ensure_principal(db, role_id, "role").await;
    use sea_orm::EntityTrait;
    role::Entity::insert(role::ActiveModel {
        id: Set(hex(role_id)),
        name: Set(name.into()),
        tenant_id: Set(hex(tenant_id)),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    })
    .exec(db)
    .await
    .expect("insert role");
}

/// Add a `(user, role)` row to `user_roles`.
pub async fn seed_user_role(
    db: &cognee_database::DatabaseConnection,
    user_id: uuid::Uuid,
    role_id: uuid::Uuid,
) {
    use cognee_database::entities::user_role;
    use sea_orm::ActiveValue::Set;
    let hex = |u: uuid::Uuid| u.simple().to_string();
    use sea_orm::EntityTrait;
    user_role::Entity::insert(user_role::ActiveModel {
        user_id: Set(hex(user_id)),
        role_id: Set(hex(role_id)),
        created_at: Set(chrono::Utc::now()),
    })
    .exec(db)
    .await
    .expect("insert user_role");
}

/// Insert a row into `role_default_permissions`.
pub async fn seed_role_default_permission(
    db: &cognee_database::DatabaseConnection,
    role_id: uuid::Uuid,
    perm: &str,
) {
    use cognee_database::entities::role_default_permission;
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    let pid = permission_id_by_name(db, perm).await;
    role_default_permission::Entity::insert(role_default_permission::ActiveModel {
        role_id: Set(role_id.simple().to_string()),
        permission_id: Set(pid),
        created_at: Set(chrono::Utc::now()),
    })
    .exec(db)
    .await
    .expect("insert role_default_permission");
}
