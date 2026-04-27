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
        pipelines: None,
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
        pipelines: None,
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
