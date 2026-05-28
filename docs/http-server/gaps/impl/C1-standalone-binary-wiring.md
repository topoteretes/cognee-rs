# C1 — Wire default backends in the standalone binary

## 1. Source / current state

The standalone binary lives at [crates/http-server/src/main.rs](../../../../crates/http-server/src/main.rs). The relevant slice (lines 109-140) is:

```rust
let args = Args::parse();

let mut cfg = HttpServerConfig::from_env().context("failed to load config from environment")?;
cfg.host = args.host;
cfg.port = args.port;
// ...cors and env overrides...

let mut state = AppState::build(cfg.clone())
    .await
    .context("failed to build AppState")?;
state.spans = spans;
#[cfg(feature = "telemetry")]
{
    state.telemetry_guard = telemetry_guard;
}

let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
cognee_http_server::run(addr, state).await.context("server error")?;
```

`AppState::build` (in [crates/http-server/src/state.rs](../../../../crates/http-server/src/state.rs)) constructs only:
- `Arc<HttpServerConfig>` (the config)
- `pipelines: Arc<dyn PipelineRunRegistry>` backed by `NoopPipelineRunRepository`
- `mailer: Arc<LoggingMailer>`
- `spans: Arc<SpanBuffer>`
- `sync: Arc<SyncRegistry>`

…and leaves every backend-bearing slot empty:

```rust
lib: None,
auth: None,
health: None,
```

The `lib: None` field is `Option<Arc<ComponentHandles>>` — the struct in [crates/http-server/src/components.rs](../../../../crates/http-server/src/components.rs) carrying every production backend. The binary never builds a `ComponentHandles`, never calls `state.install_real_health_checker()`, and never wires `state.auth`. The `lifecycle::on_startup` invoked from `build_router` does not paper over this either; it only seeds permission rows when the DB is already wired.

The only construction path that fills `ComponentHandles` today is hand-rolled test fixtures: `tests/support/mod.rs::build_component_handles`, `tests/test_health_real.rs::build_real_handles`, and the env-gated end-to-end fixtures in `tests/test_cognify_blocking.rs` / `tests/test_memify.rs` / `tests/test_remember.rs`.

`AppState::install_real_health_checker` already exists (state.rs lines 142-147) and short-circuits when `lib` is `None`:

```rust
pub fn install_real_health_checker(&mut self) {
    if let Some(handles) = &self.lib {
        let checker = crate::health::RealHealthChecker::new(Arc::clone(handles), &self.config);
        self.health = Some(Arc::new(checker));
    }
}
```

So even if backends *were* wired, the binary today still wouldn't get a real health checker because nobody calls this method.

## 2. Impact table — current vs desired binary behaviour

Reproduced from [docs/http-server/gaps/README.md §C1](../README.md) (verified against the source on 2026-05-28):

| Endpoint | Behavior in tests (state.lib wired) | Behavior in standalone binary today | Desired binary behavior |
|---|---|---|---|
| `POST /api/v1/memify` | Runs real memify, populates `("Triplet","text")` collection | Returns `PipelineRunCompleted` with no work | Same as tests |
| `POST /api/v1/remember` | Full add → cognify → memify | Returns `PipelineRunCompleted` with no work | Same as tests |
| `GET /api/v1/datasets/{id}/graph` | Returns populated snapshot via `formatted_graph_data` | Returns `{"nodes": [], "edges": []}` (graph_db is `None`) | Same as tests |
| `GET /health`, `GET /health/detailed` | Real probes (`RealHealthChecker`) | Synthetic `MockHealthChecker` entries (provider="mock") | Real probes |
| `PATCH /api/v1/update` | Runs delete + add + cognify | 500 — `vector_db` / `embedding_engine` missing | Same as tests |
| `POST /api/v1/responses` | Routes through `OpenAIResponsesClient` | 500 — "responses client is not wired" | Routes via OpenAI when token present, honest 500 otherwise |
| `POST /api/v1/cognify` (blocking) | Real pipeline | 500 — backends missing | Same as tests |
| `POST /api/v1/notebooks/{id}/{cell}/run` | Runs `python3` subprocess | 501 (notebook_runner is `None`) | 200 when `COGNEE_NOTEBOOK_RUNNER_ENABLED=1`, else preserve the explicit 501 |
| `POST /api/v1/recall` (trace / graph_context / session sources) | Real session store / manager | Empty arrays (`session_store`/`session_manager` `None`) | Same as tests |
| `POST /api/v1/search` | Real `SearchOrchestrator` | 500 — `search_orchestrator` `None` | Same as tests |

## 3. Design

Add a new module `crates/http-server/src/wiring.rs` (a sibling of `state.rs`). It exposes one async constructor:

```rust
pub async fn wire_default_backends(
    cfg: &HttpServerConfig,
) -> Result<ComponentHandles, ServerError>;
```

This function:

1. Materialises the per-backend config from `HttpServerConfig` env-fed fields (see §5).
2. Sequentially constructs every backend that has a sane default — `LocalStorage`, SeaORM SQLite `DatabaseConnection`, `LadybugAdapter`, `QdrantAdapter`, `OnnxEmbeddingEngine`, `OpenAIAdapter`, `RayonThreadPool`, `OntologyManager`, `DeleteService`, `SeaOrmPermissionsRepository`, `SeaOrmSyncOperationRepository`, `FsSessionStore`+`SessionManager`, `OpenAIResponsesClient`, `SubprocessRunner` (notebook).
3. Returns a fully populated `ComponentHandles`.
4. For **optional** backends (LLM, responses client, embedding engine, notebook runner, session manager) — leaves the slot `None` and logs a `tracing::warn!` when the relevant credentials/config are absent or the constructor fails, *without* aborting startup.
5. For **critical** backends (storage, relational DB, graph DB, vector DB) — propagates the error out of the function as `ServerError`; `main.rs` will print the error and exit with a non-zero status.

The binary in `main.rs` then performs:

```rust
let cfg = HttpServerConfig::from_env()?;
let mut state = AppState::build(cfg.clone()).await?;

// New wiring step (gated by COGNEE_DISABLE_DEFAULT_BACKENDS=1 for test deployments)
if !cfg.disable_default_backends {
    let handles = cognee_http_server::wiring::wire_default_backends(&cfg).await?;

    // Upgrade the pipeline registry to use the real DB-backed repository
    // before publishing the state — keeps PipelineRunRegistry and the rest of
    // ComponentHandles pointing at the same DatabaseConnection.
    let mut state = AppState::build_with_db(cfg.clone(), Arc::clone(&handles.database)).await?;
    state.lib = Some(Arc::new(handles));
    state.install_real_health_checker();
    // ...spans + telemetry_guard assignments unchanged...
}
```

`AppState::build_with_db` already exists (state.rs lines 158-194) and is the right path for using the real DB with the pipeline registry.

Auth wiring (`state.auth`) is **out of scope** for C1 — the existing `AuthContext::from_env` requires its own follow-up plan because `require_authentication=true` (current default) demands an additional code path on the binary. C1 only wires `state.lib` + `state.health`. The pre-existing `state.auth = None` behaviour is preserved; when auth is required but unconfigured the JWT extractor today refuses requests, which is intentional.

### Why not call `cognee_lib::ComponentManager`?

`cognee-http-server`'s `Cargo.toml` carries the comment:
> NOTE: cognee-lib is intentionally NOT a direct dep here — adding it would create a cycle when cognee-lib's `server` feature pulls in cognee-http-server. Concrete cognee-lib types are wired via AppState in P1+.

So we cannot import `cognee_lib::ComponentManager`. Instead we replicate the construction recipe inline in `wiring.rs`, leaning on the same low-level crates (`cognee-database`, `cognee-storage`, `cognee-graph`, `cognee-vector`, `cognee-embedding`, `cognee-llm`, `cognee-session`, `cognee-core`, `cognee-ontology`, `cognee-delete`) that `cognee-http-server` already depends on. `ComponentManager` itself acts only as a reference implementation we transcribe.

## 4. Config additions

Extend `HttpServerConfig` (in [crates/http-server/src/config.rs](../../../../crates/http-server/src/config.rs)) with the fields below. Default values mirror `cognee_lib::Settings::default()` so a fresh checkout works without any env vars (apart from optional OpenAI credentials). All fields are parsed from env in `HttpServerConfig::from_env()`. The `Default` impl must initialise them too.

| Field | Env var | Default | Critical? | Notes |
|---|---|---|---|---|
| `data_root_directory: PathBuf` | `DATA_ROOT_DIRECTORY` | `$XDG_CACHE_HOME/cognee/data` or `~/.cache/cognee/data` | yes | Created if missing. |
| `system_root_directory: PathBuf` | `SYSTEM_ROOT_DIRECTORY` | `$XDG_CACHE_HOME/cognee/system` or `~/.cache/cognee/system` | yes | Holds `graph/`, `vectors/`. Created if missing. |
| `relational_db_url: String` | `DATABASE_URL` / `RELATIONAL_DB_URL` | `sqlite://<system_root>/cognee.db` | yes | A `sqlite::memory:` URL is allowed. |
| `graph_provider: String` | `GRAPH_DATABASE_PROVIDER` | `"ladybug"` | yes | Only `"ladybug"` supported today. |
| `graph_file_path: PathBuf` | `GRAPH_FILE_PATH` | `<system_root>/graph` | yes | Parent dir auto-created. |
| `vector_provider: String` | `VECTOR_DB_PROVIDER` | `"qdrant"` | yes | `qdrant` (embedded) only — pgvector requires the `pgvector` feature. |
| `vector_db_url: String` | `VECTOR_DB_URL` | `<system_root>/vectors` | yes | When embedded qdrant, treated as a directory path. |
| `embedding_provider: String` | `EMBEDDING_PROVIDER` | `"onnx"` | opt-in | One of `onnx`, `openai`, `openai_compatible`, `ollama`, `mock`. |
| `embedding_dimensions: u32` | `EMBEDDING_DIMENSIONS` | `384` | yes (linked to vector) | Must match the vector DB collection dim. |
| `embedding_model_name: String` | `EMBEDDING_MODEL_NAME` | `"bge-small-en-v1.5"` | – | – |
| `embedding_model_path: PathBuf` | `EMBEDDING_MODEL_PATH` | unset → engine returns error → slot left `None` | optional | When onnx engine cannot load, the slot is dropped to `None`. |
| `embedding_tokenizer_path: PathBuf` | `EMBEDDING_TOKENIZER_PATH` | unset → same as above | optional | – |
| `embedding_endpoint: String` | `EMBEDDING_ENDPOINT` | empty | optional | Used by openai/ollama providers. |
| `embedding_api_key: SecretString` | `EMBEDDING_API_KEY` (fallback `LLM_API_KEY`, `OPENAI_TOKEN`) | empty | optional | Never logged. |
| `llm_provider: String` | `LLM_PROVIDER` | `"openai"` | optional | |
| `llm_model: String` | `LLM_MODEL` (fallback `OPENAI_MODEL`) | `"gpt-4o-mini"` | optional | |
| `llm_api_key: SecretString` | `LLM_API_KEY` (fallback `OPENAI_TOKEN`) | empty | optional | Never logged. Missing key → slot stays `None`. |
| `llm_endpoint: String` | `LLM_ENDPOINT` (fallback `OPENAI_URL`) | empty | optional | |
| `llm_max_retries: u32` | `LLM_MAX_RETRIES` | `3` | – | – |
| `session_store_backend: String` | `COGNEE_SESSION_STORE` | `"fs"` | optional | Currently `fs` only. |
| `session_root_directory: PathBuf` | `COGNEE_SESSION_DIR` | `<system_root>/sessions` | – | – |
| `notebook_runner_enabled: bool` | `COGNEE_NOTEBOOK_RUNNER_ENABLED` | `false` | opt-in | Off by default; embedders opt in. |
| `responses_client_enabled: bool` | `COGNEE_RESPONSES_CLIENT_ENABLED` | `true` when `llm_api_key` non-empty | – | – |
| `disable_default_backends: bool` | `COGNEE_DISABLE_DEFAULT_BACKENDS` | `false` | – | Test/dev escape hatch. |

Two helper functions belong in `config.rs`:

```rust
fn default_cache_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("cognee")
    } else if let Some(home) = dirs::home_dir() {
        home.join(".cache").join("cognee")
    } else {
        PathBuf::from("./.cognee")
    }
}

fn ensure_dir(p: &Path) -> Result<(), ServerError> {
    std::fs::create_dir_all(p).map_err(|e| ServerError::Other(anyhow::anyhow!("create_dir_all({:?}): {e}", p)))
}
```

The `dirs` crate is already pulled in transitively; if it isn't a workspace dep we fall back to `std::env::var("HOME")`.

## 5. Implementation steps

### Step (a) — extend `HttpServerConfig` ([crates/http-server/src/config.rs](../../../../crates/http-server/src/config.rs))

1. Add every field listed in §4 to the struct.
2. Update `Default` to populate them (cache-root defaults; non-required fields stay empty).
3. Extend `HttpServerConfig::from_env()` to parse each new env var. Each `*_API_KEY` field uses `secrecy::SecretString` to prevent accidental `Debug`/log leakage.
4. Add fall-through alias support for `LLM_API_KEY`/`OPENAI_TOKEN`, `LLM_ENDPOINT`/`OPENAI_URL`, `LLM_MODEL`/`OPENAI_MODEL` to match the test convention already used by `tests/test_cognify_blocking.rs::maybe_env`.
5. Add `#[derive(Debug)]` audit: do **not** derive `Debug` printing for the secret fields directly — `SecretString` already redacts.

### Step (b) — create `crates/http-server/src/wiring.rs`

```rust
//! Construct production backends from `HttpServerConfig`.
pub async fn wire_default_backends(cfg: &HttpServerConfig) -> Result<ComponentHandles, ServerError> {
    // 1. Filesystem prep
    ensure_dir(&cfg.data_root_directory)?;
    ensure_dir(&cfg.system_root_directory)?;

    // 2. Storage
    let storage = wire_storage(cfg).await?;

    // 3. Relational DB
    let database = wire_database(cfg).await?;

    // 4. Graph DB
    let graph_db = wire_graph_db(cfg).await?;

    // 5. Vector DB
    let vector_db = wire_vector_db(cfg).await?;

    // 6. Embedding engine (optional — log + None on failure)
    let embedding_engine = wire_embedding_engine(cfg).await;

    // 7. LLM (optional)
    let llm = wire_llm(cfg);

    // 8. Thread pool
    let thread_pool = Some(Arc::new(RayonThreadPool::with_default_threads()
        .map_err(|e| ServerError::Other(anyhow::anyhow!("rayon pool: {e}")))?)
        as Arc<dyn CpuPool>);

    // 9. Ontology manager + resolver
    let ontology_manager = Arc::new(OntologyManager::new(cfg.data_root_directory.join("ontology")));
    let ontology_resolver: Option<Arc<dyn OntologyResolver>> = None; // NoOp default at handler level

    // 10. Derived services
    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        database.clone() as Arc<dyn DeleteDb>,
    ));
    let permissions = Some(Arc::new(SeaOrmPermissionsRepository::new(database.clone()))
        as Arc<dyn PermissionsRepository>);
    let sync_ops = Some(Arc::new(SeaOrmSyncOperationRepository::new(database.clone()))
        as Arc<dyn SyncOperationRepository>);

    // 11. Session store + manager
    let (session_store, session_manager) = wire_session(cfg).await;

    // 12. Search orchestrator (uses all the above)
    let search_orchestrator = wire_search_orchestrator(
        Arc::clone(&database), llm.clone(), graph_db.clone(),
        vector_db.clone(), embedding_engine.clone(),
    ).await;

    // 13. Responses client (optional)
    let responses_client = wire_responses_client(cfg);

    // 14. Notebook runner (opt-in)
    let notebook_runner = if cfg.notebook_runner_enabled {
        Some(SubprocessRunner::new().into_dyn())
    } else {
        None
    };

    Ok(ComponentHandles {
        database,
        storage,
        delete_service,
        ontology_manager,
        search_orchestrator,
        llm,
        graph_db: Some(graph_db),
        vector_db: Some(vector_db),
        thread_pool,
        embedding_engine,
        ontology_resolver,
        permissions,
        sync_ops,
        session_store,
        session_manager,
        responses_client,
        notebook_runner,
    })
}
```

### Step (c) — per-backend construction recipes

Each helper below mirrors an existing reference; I cite the line that proves the pattern compiles.

**`wire_storage`** — reference: `tests/test_cognify_blocking.rs:91-93`
```rust
let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(cfg.data_root_directory.clone()));
storage.initialize().await.map_err(...)?;
```

**`wire_database`** — reference: `tests/test_cognify_blocking.rs:95-100` + `lib::component_manager.rs:86-95`
```rust
let url = cfg.relational_db_url.clone();
if url.starts_with("sqlite://") {
    let path = url.trim_start_matches("sqlite://");
    if !path.starts_with(':') {           // not sqlite::memory:
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !Path::new(path).exists() { std::fs::File::create(path)?; }
    }
}
let db = connect(&url).await?;
initialize(&db).await?;
Arc::new(db)
```

**`wire_graph_db`** — reference: `tests/test_cognify_blocking.rs:102-108` + `lib::component_manager.rs:97-136`
```rust
if let Some(parent) = cfg.graph_file_path.parent() { std::fs::create_dir_all(parent)?; }
let g = LadybugAdapter::new(cfg.graph_file_path.to_string_lossy().as_ref()).await?;
g.initialize().await?;
Arc::new(g) as Arc<dyn GraphDBTrait>
```
Guard with a clear error when `cfg.graph_provider != "ladybug"` since the http-server crate does not pull in `kuzu` or other backends.

**`wire_vector_db`** — reference: `tests/test_cognify_blocking.rs:110-111` + `lib::component_manager.rs:138-194`
```rust
let dir = if cfg.vector_db_url.is_empty() {
    cfg.system_root_directory.join("vectors")
} else {
    PathBuf::from(&cfg.vector_db_url)
};
std::fs::create_dir_all(&dir)?;
Arc::new(QdrantAdapter::new(dir, cfg.embedding_dimensions as usize)) as Arc<dyn VectorDB>
```

**`wire_embedding_engine`** — reference: `tests/test_cognify_blocking.rs:113-124`
```rust
let onnx_cfg = OnnxEmbeddingConfig {
    model_path: cfg.embedding_model_path.clone(),
    tokenizer_path: cfg.embedding_tokenizer_path.clone(),
    model_name: cfg.embedding_model_name.clone(),
    dimensions: cfg.embedding_dimensions as usize,
    max_sequence_length: 512,
    batch_size: 32,
};
match OnnxEmbeddingEngine::new(onnx_cfg) {
    Ok(e) => Some(Arc::new(e) as Arc<dyn EmbeddingEngine>),
    Err(err) => {
        tracing::warn!("embedding engine unavailable, /cognify will return 500: {err}");
        None
    }
}
```
Return type is `Option<Arc<dyn EmbeddingEngine>>` so failure is non-fatal.

**`wire_llm`** — reference: `tests/test_cognify_blocking.rs:126-129` + `lib::component_manager.rs:344-378`
```rust
use secrecy::ExposeSecret;
let key = cfg.llm_api_key.expose_secret().to_string();
if key.is_empty() { return None; }                            // log a warn at info level
let endpoint = if cfg.llm_endpoint.is_empty() { None } else { Some(cfg.llm_endpoint.clone()) };
let adapter = OpenAIAdapter::new(cfg.llm_model.clone(), key, endpoint).ok()?
    .with_structured_output_retries(cfg.llm_max_retries.max(1))
    .with_network_retries(cfg.llm_max_retries.max(1));
Some(Arc::new(adapter) as Arc<dyn Llm>)
```
**Never log `key`.** Log only `tracing::info!("LLM provider configured: openai, endpoint={}, model={}", endpoint.unwrap_or("<default>"), cfg.llm_model)`.

**`wire_session`** — reference: `crates/session/src/session_manager.rs:26`, `crates/session/src/fs_store.rs:53`
```rust
let dir = cfg.session_root_directory.clone();
std::fs::create_dir_all(&dir).ok();
let store = Arc::new(FsSessionStore::new(dir)) as Arc<dyn SessionStore>;
let manager = Arc::new(SessionManager::new(Arc::clone(&store)));
(Some(store), Some(manager))
```

**`wire_search_orchestrator`** — reference: `crates/search/src/orchestration/search_execution_builder.rs:29-46` (`SearchBuilder::new(vector_db, embedding_engine, graph_db, llm)`), then `.build()` into a `SearchOrchestrator`, then `.with_database(db as Arc<dyn SearchHistoryDb>)`. When any of the four is `None`, return `None` and log a warn — the search router already surfaces a clean 500.

**`wire_responses_client`** — reference: `crates/llm/src/responses_client.rs:153` (`OpenAIResponsesClient::new(api_key, base_url)`)
```rust
let key = cfg.llm_api_key.expose_secret().to_string();
if key.is_empty() || !cfg.responses_client_enabled { return None; }
let base = if cfg.llm_endpoint.is_empty() { None } else { Some(cfg.llm_endpoint.clone()) };
OpenAIResponsesClient::new(key, base).ok()
    .map(|c| Arc::new(c) as Arc<dyn ResponsesClient>)
```

**`wire_notebook_runner`** — opt-in only. Default `false` keeps the 501 envelope, which preserves wire compatibility for embedders that do not want to expose code execution.

### Step (d) — update `main.rs`

```rust
let mut cfg = HttpServerConfig::from_env()?;
// ...flag overrides...

let handles = if cfg.disable_default_backends {
    tracing::warn!("COGNEE_DISABLE_DEFAULT_BACKENDS=1 — backends not wired; endpoints will return placeholders");
    None
} else {
    Some(cognee_http_server::wiring::wire_default_backends(&cfg).await?)
};

let mut state = match handles.as_ref() {
    Some(h) => {
        let mut s = AppState::build_with_db(cfg.clone(), Arc::clone(&h.database)).await?;
        s.lib = Some(Arc::new(h.clone()));
        s.install_real_health_checker();
        s
    }
    None => AppState::build(cfg.clone()).await?,
};
state.spans = spans;
#[cfg(feature = "telemetry")] { state.telemetry_guard = telemetry_guard; }
```

Three notes on this rewrite:
- `ComponentHandles: Clone` is `#[derive(Clone)]`, so `h.clone()` is cheap (every field is `Arc<…>` or `Option<Arc<…>>`).
- We must call `AppState::build_with_db` instead of `AppState::build` so the pipeline registry uses the real `SeaOrmPipelineRunRepository` rather than the no-op repo, otherwise `pipeline-runs` queries via the activity router still return empty.
- `state.install_real_health_checker()` is called before `run()` — it's a no-op if `lib` is `None`, so the test-only branch above stays safe.

### Step (e) — smoke test

Add `crates/http-server/tests/test_main_wiring.rs`:

```rust
//! Boots `wire_default_backends` against an in-memory SQLite + tempdir,
//! asserts `/health/detailed` returns *real* probe entries (not the
//! MockHealthChecker placeholder), and asserts `lib.is_some()`.

#[tokio::test]
async fn wire_default_backends_produces_real_health_response() {
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: test-only.
    unsafe {
        std::env::set_var("DATA_ROOT_DIRECTORY", tmp.path().join("data"));
        std::env::set_var("SYSTEM_ROOT_DIRECTORY", tmp.path().join("system"));
        std::env::set_var("RELATIONAL_DB_URL", "sqlite::memory:");
        std::env::set_var("EMBEDDING_PROVIDER", "mock");
        // No OPENAI_TOKEN → llm slot stays None.
    }

    let cfg = HttpServerConfig::from_env().unwrap();
    let handles = cognee_http_server::wiring::wire_default_backends(&cfg).await.unwrap();
    assert!(handles.graph_db.is_some());
    assert!(handles.vector_db.is_some());

    let mut state = AppState::build_with_db(cfg.clone(), Arc::clone(&handles.database)).await.unwrap();
    state.lib = Some(Arc::new(handles));
    state.install_real_health_checker();

    let app = build_router(state).await.unwrap();
    let resp = support::oneshot_get(app, "/health/detailed").await;
    assert_eq!(resp.status(), 200);
    let body = support::body_json(resp).await;
    for key in &["relational_db", "graph_db", "file_storage"] {
        assert_ne!(body["components"][key]["provider"].as_str().unwrap_or(""), "mock");
    }
}
```

The test must `serial_test::serial` because it mutates process-wide env vars; the existing test suite already uses `serial_test`.

### Step (f) — error policy

Adopt this contract inside `wire_default_backends`:

| Failure mode | Action |
|---|---|
| `LocalStorage::initialize` returns Err | Return `ServerError` — process exits. |
| `connect()` / `initialize()` (SQL) fails | Return `ServerError`. |
| `LadybugAdapter::new` fails | Return `ServerError`. |
| `QdrantAdapter::new` panics (it does not return Result today) | Documented expected behaviour; wrap in `tokio::task::spawn_blocking` if necessary. |
| `OnnxEmbeddingEngine::new` returns Err (missing model file) | `tracing::warn!`, slot left `None`. |
| `OpenAIAdapter::new` returns Err | `tracing::warn!`, slot left `None`. |
| `OpenAIResponsesClient::new` returns Err | `tracing::warn!`, slot left `None`. |
| `RayonThreadPool::with_default_threads` returns Err | Return `ServerError` — pipelines hard-depend on it. |

The boundary between critical and optional matches what Python parity requires: a server with no `OPENAI_TOKEN` should still boot, serve `/health`, list datasets, and *correctly* surface 500s for endpoints that need an LLM. The current binary fails this expectation by hiding the missing backend behind a fake 200.

## 6. Tests

**A — unit tests for `wire_default_backends`** (in `crates/http-server/src/wiring.rs`, `#[cfg(test)] mod tests`):
1. With `RELATIONAL_DB_URL=sqlite::memory:`, `EMBEDDING_PROVIDER=mock`, no LLM key → returns Ok, `llm.is_none()`, `embedding_engine.is_some()` (mock), `database`/`graph_db`/`vector_db`/`storage` all populated.
2. With `EMBEDDING_PROVIDER=onnx` and a non-existent `EMBEDDING_MODEL_PATH` → returns Ok with `embedding_engine = None` and a warn log captured via `tracing_subscriber::fmt::test_writer`.
3. With `LLM_API_KEY` set (any non-empty value, no network call required for construction) → `llm.is_some()`, `responses_client.is_some()`.
4. With `COGNEE_NOTEBOOK_RUNNER_ENABLED=1` → `notebook_runner.is_some()`.
5. With an invalid `GRAPH_DATABASE_PROVIDER=kuzu` → returns Err.

**B — integration smoke test** (`crates/http-server/tests/test_main_wiring.rs`): boots a router from the wiring helper, asserts `/health/detailed` reports `provider != "mock"` for every critical component (see step e).

**C — config validation tests** (added to `crates/http-server/src/config.rs::tests`):
- `data_root_directory` defaults to `$XDG_CACHE_HOME/cognee/data` when that env var is set.
- `data_root_directory` falls back to `~/.cache/cognee/data` when `XDG_CACHE_HOME` is unset.
- `embedding_dimensions` parses as `u32` and rejects negative / non-numeric values with a clear `ServerError`.
- Secrets (`llm_api_key`, `embedding_api_key`) round-trip through `from_env()` without appearing in any `Debug`/`Display` output: assertion via `format!("{:?}", cfg)` not containing the secret literal.

All three classes are CI-safe — none require network, OpenAI tokens, or ONNX model files.

## 7. Acceptance criteria

- [ ] `crates/http-server/src/wiring.rs` exists and exposes `pub async fn wire_default_backends`.
- [ ] `crates/http-server/src/lib.rs` re-exports `pub mod wiring;`.
- [ ] `crates/http-server/src/main.rs` calls `wire_default_backends` (gated by `cfg.disable_default_backends`), populates `state.lib`, and calls `state.install_real_health_checker()`.
- [ ] `HttpServerConfig` gains every field listed in §4, each parsed by `from_env()`.
- [ ] All sensitive fields use `secrecy::SecretString`; no test prints them via `Debug`.
- [ ] `cargo build -p cognee-http-server --features bin` succeeds without `cognee-lib` in the dependency graph.
- [ ] `crates/http-server/tests/test_main_wiring.rs` passes against in-memory backends.
- [ ] `cargo test --workspace` passes (modulo pre-existing unrelated failures).
- [ ] `tests/test_health_real.rs` continues to pass — the existing tests must remain green because the new wiring is additive.
- [ ] Manual `curl` against a binary launched with only `RELATIONAL_DB_URL=sqlite::memory:` returns real (non-mock) `/health/detailed` JSON.
- [ ] Manual `curl POST /api/v1/cognify` against the binary (with embedding model + OPENAI_TOKEN set) completes a real pipeline run, mirroring `tests/test_cognify_blocking.rs`.

## 8. Constraints / risks

- **Workspace cycle**: `wiring.rs` cannot `use cognee_lib::*`. All backend construction must reach down through `cognee-database`, `cognee-storage`, `cognee-graph`, `cognee-vector`, `cognee-embedding`, `cognee-llm`, `cognee-session`, `cognee-core`, `cognee-ontology`, `cognee-delete`, `cognee-search`. These are already direct deps in [crates/http-server/Cargo.toml](../../../../crates/http-server/Cargo.toml).
- **Sequential dependencies**: `OntologyManager` needs storage; `DeleteService` needs storage + DB; `SearchOrchestrator` needs vector + graph + llm + embedding; `RealHealthChecker` needs all critical handles. Construction order in §5(b) reflects this graph. `tokio::join!` is *not* worth it for binary startup — the total wall-clock time is dominated by ONNX model load (when present), and parallelism complicates error logging.
- **Logging policy**: `tracing::info!` messages must include the *configured endpoint and model* (when non-empty) but never the secret. Use `?key` or `%key` only where `key` is a `SecretString` whose `Debug` impl is redacted.
- **Default cache dir**: `XDG_CACHE_HOME` takes precedence over `~/.cache`. On macOS the convention is the same. On Windows we fall back to `%LOCALAPPDATA%\cognee` (use `dirs::cache_dir()` if available; otherwise current-dir fallback).
- **Idempotency**: each helper must tolerate being re-run against an existing data dir (Ladybug + Qdrant must reopen, not error).
- **Notebook runner is opt-in** — defaults to off, preserving the explicit `501` envelope and existing regression guard `run_cell_with_runner_does_not_return_501` in `routers/notebooks.rs`.
- **Smoke test cannot require ONNX or OpenAI**. Use `EMBEDDING_PROVIDER=mock` and leave `LLM_API_KEY` unset; cover the success case for those backends in existing env-gated tests instead.

## 9. Status

**not-started**
