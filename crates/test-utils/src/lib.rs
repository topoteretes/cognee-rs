//! Test utilities for cognee-rust crates.
//!
//! Re-exports mock implementations and provides helpers for constructing
//! [`TaskContext`] in tests without requiring real database backends.
//!
//! Also exposes [`pg_test_url`] for building a PostgreSQL connection URL from
//! the `DB_*` environment variables (mirroring the Python `DB_PROVIDER` /
//! `DB_HOST` / … convention).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test infrastructure — panics are acceptable"
)]

pub mod mock_acl_db;
pub mod mock_llm;
pub mod mock_transcriber;
pub mod span_capture;

// `mock_user_db`, `mock_role_db`, `mock_tenant_db` moved to the closed
// `cognee-access-control::test_utils` module
//.

use std::{path::PathBuf, sync::Arc};

use cognee_core::{CancellationHandle, RayonThreadPool, TaskContext, TaskContextBuilder};
use cognee_database::DatabaseConnection;
use cognee_embedding::{EmbeddingEngine, config::EmbeddingConfig};
use cognee_llm::OpenAIAdapter;

pub use cognee_graph::MockGraphDB;
pub use cognee_storage::MockStorage;
pub use cognee_vector::MockVectorDB;
pub use mock_acl_db::MockAclDb;
pub use mock_llm::MockLlm;
pub use mock_transcriber::MockTranscriber;
pub use span_capture::{CapturedSpan, SpanCapture, SpanCaptureGuard};

/// Resolve the directory used for local ONNX embedding artifacts in E2E tests.
///
/// Precedence:
/// 1. Parent directory of `COGNEE_E2E_EMBED_MODEL_PATH`
/// 2. Parent directory of `EMBEDDING_MODEL_PATH`
/// 3. `COGNEE_TEST_MODEL_DIR`
/// 4. Workspace-local `target/models`
pub fn e2e_embedding_model_dir() -> PathBuf {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_path_buf();
    }
    if let Ok(model_path) = std::env::var("EMBEDDING_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_path_buf();
    }
    if let Ok(model_dir) = std::env::var("COGNEE_TEST_MODEL_DIR") {
        return model_dir.into();
    }

    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate should live under workspace/crates")
        .join("target/models")
}

/// Build an embedding engine from environment configuration for use in integration tests.
///
/// Calls [`EmbeddingConfig::from_env`] which defaults to OpenAI `text-embedding-3-small`
/// (reading `EMBEDDING_API_KEY`, falling back to `LLM_API_KEY`) on non-Android platforms.
/// Returns `(engine, dimensions)` on success, or `None` with a printed skip message on
/// failure so tests can gracefully skip:
///
/// ```rust,ignore
/// let Some((embedding_engine, embedding_dims)) =
///     cognee_test_utils::create_test_embedding_engine().await
/// else { return };
/// ```
pub async fn create_test_embedding_engine() -> Option<(Arc<dyn EmbeddingEngine>, usize)> {
    let _ = dotenv::dotenv();
    let config = EmbeddingConfig::from_env();
    let dimensions = config.dimensions;
    match config.create_engine().await {
        Ok(engine) => Some((engine, dimensions)),
        Err(e) => {
            eprintln!("⚠️  Skipping: could not create embedding engine: {e}");
            None
        }
    }
}

/// Read a required LLM environment variable, loading `.env` first (idempotent).
///
/// Accepts the Python-compatible canonical names (`LLM_API_KEY`, `LLM_ENDPOINT`,
/// `LLM_MODEL`) as fallbacks for the legacy Rust test aliases (`OPENAI_TOKEN`,
/// `OPENAI_URL`, `OPENAI_MODEL`), so a single `.env` with the canonical names
/// works for both the CLI and the integration tests. This is the single source
/// of truth the per-crate `tests/test_utils.rs` shims re-export — keeping the
/// alias fallback consistent everywhere is what makes [`llm_env_available`] a
/// reliable guard (a guard that passes on `LLM_*`-only creds must not then panic
/// in here).
///
/// Panics if neither the requested variable nor its canonical fallback is set.
pub fn require_env(var_name: &str) -> String {
    let _ = dotenv::dotenv();

    let canonical_fallback = match var_name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };

    if let Ok(v) = std::env::var(var_name)
        && !v.is_empty()
    {
        return v;
    }
    if let Some(canonical) = canonical_fallback
        && let Ok(v) = std::env::var(canonical)
        && !v.is_empty()
    {
        return v;
    }
    panic!("❌ Required environment variable '{var_name}' is not set")
}

/// Returns `true` when live LLM credentials are configured — `OPENAI_URL`/
/// `OPENAI_TOKEN` or the canonical `LLM_ENDPOINT`/`LLM_API_KEY`. Integration/E2E
/// tests call this to skip gracefully (per the repo convention) instead of
/// panicking in [`require_env`] when run without secrets, e.g. on the
/// secret-free community CI lane.
///
/// It intentionally does **not** check the model: the model is optional (see
/// [`llm_model_from_env`]), so requiring it here would wrongly skip a runnable
/// test. Every adapter builder must therefore default the model rather than
/// `require_env` it, so that "guard passed" always implies "adapter builds".
pub fn llm_env_available() -> bool {
    let _ = dotenv::dotenv();
    let present = |names: &[&str]| {
        names
            .iter()
            .any(|n| std::env::var(n).map(|v| !v.is_empty()).unwrap_or(false))
    };
    present(&["OPENAI_URL", "LLM_ENDPOINT"]) && present(&["OPENAI_TOKEN", "LLM_API_KEY"])
}

/// The LLM model name to use in tests: `LLM_MODEL`, then `OPENAI_MODEL`, then
/// the `gpt-4o-mini` default. The model always has a sensible default, so it is
/// never a hard requirement — see [`llm_env_available`].
pub fn llm_model_from_env() -> String {
    std::env::var("LLM_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string())
}

/// Build a real [`OpenAIAdapter`] from the environment (endpoint + key via
/// [`require_env`], model via [`llm_model_from_env`]). Guard the caller with
/// [`llm_env_available`] so this never panics on the keyless lane. Returned as
/// `Arc` so it coerces directly to `Arc<dyn Llm>`.
pub fn create_openai_adapter_from_env() -> Arc<OpenAIAdapter> {
    let base_url = require_env("OPENAI_URL");
    let api_token = require_env("OPENAI_TOKEN");
    let model = llm_model_from_env();
    // Route through the production factory so litellm-style provider prefixes
    // (`openai/`, `baseten/`, …) are stripped exactly as in a real run — e.g.
    // `baseten/openai/gpt-oss-120b` → `openai/gpt-oss-120b`. Building the adapter
    // directly would send the prefixed slug verbatim and 404.
    //
    // NOTE: the default provider here is `openai`, which strips a leading
    // `openai/` segment. A custom endpoint whose *real* model slug legitimately
    // begins with `openai/` (org "openai") must therefore set `LLM_PROVIDER=custom`
    // (or `openai_compatible`) so the slug is passed through verbatim.
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    Arc::new(
        cognee_llm::build_openai_compatible_adapter(&provider, &model, &api_token, &base_url, 3)
            .unwrap_or_else(|e| panic!("❌ Failed to create OpenAI adapter: {e}")),
    )
}

/// Returns a PostgreSQL connection URL built from environment variables, or `None`
/// if `DB_PROVIDER` is not set to `"postgres"`.
///
/// Reads the following env vars (matching Python's `DB_*` convention):
/// - `DB_PROVIDER` — must equal `"postgres"` to activate
/// - `DB_HOST` — defaults to `"localhost"`
/// - `DB_PORT` — defaults to `"5432"`
/// - `DB_NAME` — defaults to `"cognee_db"`
/// - `DB_USERNAME` — defaults to `"postgres"`
/// - `DB_PASSWORD` — defaults to `""` (empty)
///
/// Tests that call this should skip gracefully when `None` is returned:
/// ```rust,ignore
/// let Some(url) = cognee_test_utils::pg_test_url() else { return };
/// ```
pub fn pg_test_url() -> Option<String> {
    let provider = std::env::var("DB_PROVIDER").unwrap_or_default();
    if provider != "postgres" {
        return None;
    }
    let host = std::env::var("DB_HOST").unwrap_or_else(|_| "localhost".to_string());
    let port = std::env::var("DB_PORT").unwrap_or_else(|_| "5432".to_string());
    let name = std::env::var("DB_NAME").unwrap_or_else(|_| "cognee_db".to_string());
    let user = std::env::var("DB_USERNAME").unwrap_or_else(|_| "postgres".to_string());
    let pass = std::env::var("DB_PASSWORD").unwrap_or_default();
    Some(format!("postgres://{user}:{pass}@{host}:{port}/{name}"))
}

/// Build a [`TaskContext`] with all-mock backends and an in-memory SQLite database.
///
/// Returns `(CancellationHandle, Arc<TaskContext>, Arc<DatabaseConnection>)`.
/// The `DatabaseConnection` is exposed so callers can perform direct DB queries
/// in assertions.
pub async fn test_task_context() -> (
    CancellationHandle,
    Arc<TaskContext>,
    Arc<DatabaseConnection>,
) {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let db = Arc::new(db);

    let (handle, ctx) = TaskContextBuilder::new()
        .thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
        .database(db.clone())
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .build()
        .unwrap();

    (handle, Arc::new(ctx), db)
}
