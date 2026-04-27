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
        spans: None,
        sync: None,
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
        spans: None,
        sync: None,
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
    Arc::new(ComponentHandles {
        database: db,
        storage,
        delete_service,
        ontology_manager,
        search_orchestrator,
        llm,
        graph_db,
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
