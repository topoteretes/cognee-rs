#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Shared test helpers for the cognee-http-server integration tests.
//!
//! The auth/permissions helpers (`setup_auth_db`, `build_auth_test_state`,
//! `seed_user`, `bearer_header`, `cookie_header`, `build_permissions_state`,
//! the closed seed helpers) moved closed alongside the auth router
//! family in T3-pre. What remains is the OSS-side P0/P4/P7 surface.
#![allow(dead_code)]

use axum::{Router, body::Body, http::Request};
use tower::ServiceExt;

use cognee_http_server::{AppState, HttpServerConfig, build_router};

// ─── Basic helpers ───────────────────────────────────────────────────────────

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

// ─── P4 / P7 helpers (read path + components) ────────────────────────────────

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
use std::sync::Arc;

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
        acl_db: None,
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator,
        llm,
        graph_db,
        vector_db: None,
        thread_pool: None,
        embedding_engine: None,
        ontology_resolver: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
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
