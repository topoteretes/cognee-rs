//! Relational database construction. No provider choice (sea-orm dispatches on
//! the URL scheme), so this is a free function rather than a registered factory.

use std::path::Path;
use std::sync::Arc;

use cognee_database::{DatabaseConnection, connect, initialize};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;

/// Extract the on-disk filesystem path from a SQLite URL, or `None` for
/// non-file URLs (`postgres://…`, in-memory `sqlite::memory:`).
///
/// SQLite has no meaningful remote "host", so any `//<authority>` is treated as
/// part of the path — matching the HTTP server's historical
/// `strip_prefix("sqlite://")`, which is why a two-slash *relative* form like
/// `sqlite://data/cognee.db` must still yield `data/cognee.db` (not be skipped
/// as a host form). Shapes handled:
///
///   `sqlite:./rel/db`            → `./rel/db`        (1-slash relative)
///   `sqlite:/abs/db`             → `/abs/db`         (1-slash absolute)
///   `sqlite://rel/db`            → `rel/db`          (2-slash relative)
///   `sqlite:///abs/db`           → `/abs/db`         (3-slash absolute)
///   `sqlite://localhost/abs/db`  → `/abs/db`         (host form)
///
/// Any `?query` (e.g. `?mode=rwc`) is stripped.
fn sqlite_fs_path(url: &str) -> Option<String> {
    if !url.starts_with("sqlite:") || url.contains(":memory:") {
        return None;
    }
    let after = url.trim_start_matches("sqlite:");
    // Collapse any run of leading slashes on the absolute forms to a single
    // one, so an extra slash (e.g. `sqlite:////abs`) can't yield a
    // double-leading-slash path that some platforms read as a UNC/network path.
    let path = if let Some(rest) = after.strip_prefix("//localhost/") {
        format!("/{}", rest.trim_start_matches('/'))
    } else if let Some(rest) = after.strip_prefix("//") {
        // `//<rest>`: absolute when `rest` starts with `/` (i.e. `sqlite:///…`),
        // otherwise a relative path (`sqlite://data/…`).
        match rest.strip_prefix('/') {
            Some(abs) => format!("/{}", abs.trim_start_matches('/')),
            None => rest.to_string(),
        }
    } else {
        after.to_string()
    };
    let path = path.split('?').next().unwrap_or(&path).to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Connect to and initialize the relational database from the resolved URL.
///
/// For SQLite file-backed databases, the parent directory is created first
/// (non-fatal): sea-orm's `?mode=rwc` creates the *file* but not missing
/// ancestor directories, so a settings override that redirects the DB to a new
/// path (e.g. per-test isolation) would otherwise fail with "unable to open
/// database file". File creation itself is left to the driver via `?mode=rwc`
/// (callers that want auto-create put `mode=rwc` in the URL); this function
/// never creates the DB file, so a mistyped path still fails loudly at connect
/// instead of silently producing an empty database at the wrong location.
pub async fn build_database(
    ctx: &BackendBuildContext,
) -> Result<Arc<DatabaseConnection>, ComponentError> {
    let url = &ctx.relational_db_url;

    if let Some(path) = sqlite_fs_path(url) {
        let db_path = Path::new(&path);
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            // Non-fatal: an unusual-but-driver-valid URL must still reach
            // sea-orm and surface the driver's own error.
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "could not create SQLite parent directory '{}': {e}",
                    parent.display()
                );
            }
        }
    }

    let db = connect(url)
        .await
        .map_err(|e| ComponentError::Database(format!("initialization failed: {e}")))?;
    initialize(&db)
        .await
        .map_err(|e| ComponentError::Database(format!("schema initialization failed: {e}")))?;
    Ok(Arc::new(db))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn ctx_with_db_url(url: String) -> BackendBuildContext {
        BackendBuildContext {
            data_root_directory: std::path::PathBuf::from("/tmp"),
            system_root_directory: std::path::PathBuf::from("/tmp"),
            relational_db_url: url,
            graph_provider: "ladybug".to_string(),
            graph_file_path: String::new(),
            graph_postgres_url: None,
            vector_provider: "brute-force".to_string(),
            vector_db_url: String::new(),
            vector_postgres_url: None,
            embedding_dimensions: 384,
            embedding: crate::context::EmbeddingInputs {
                provider: "mock".to_string(),
                model: String::new(),
                dimensions: 384,
                endpoint: None,
                api_key: None,
                batch_size: 36,
                mock: true,
                mock_deterministic: false,
                api_version: None,
                huggingface_tokenizer: None,
                max_completion_tokens: 8191,
                onnx_model_path: std::path::PathBuf::new(),
                onnx_tokenizer_path: std::path::PathBuf::new(),
                onnx_model_name: String::new(),
                onnx_dimensions: 384,
                onnx_max_sequence_length: 512,
                onnx_batch_size: 32,
            },
            llm: crate::context::LlmInputs {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: "sk-test".to_string(),
                endpoint: String::new(),
                max_retries: 3,
                llm_args: serde_json::Map::new(),
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
            },
        }
    }

    // ── sqlite_fs_path: every URL shape, incl. the two-slash *relative* form
    //    that a relative SYSTEM_ROOT_DIRECTORY produces on the HTTP server ──
    #[test]
    fn sqlite_fs_path_covers_all_shapes() {
        assert_eq!(sqlite_fs_path("sqlite:./rel/db"), Some("./rel/db".into()));
        assert_eq!(sqlite_fs_path("sqlite:/abs/db"), Some("/abs/db".into()));
        // Two-slash relative — the regression case: must NOT be dropped as a host form.
        assert_eq!(
            sqlite_fs_path("sqlite://data/cognee.db"),
            Some("data/cognee.db".into())
        );
        assert_eq!(sqlite_fs_path("sqlite:///abs/db"), Some("/abs/db".into()));
        assert_eq!(
            sqlite_fs_path("sqlite://localhost/abs/db"),
            Some("/abs/db".into())
        );
        // Extra leading slashes collapse to one (no double-leading-slash /
        // accidental UNC path).
        assert_eq!(sqlite_fs_path("sqlite:////abs/db"), Some("/abs/db".into()));
        // Query strings are stripped.
        assert_eq!(
            sqlite_fs_path("sqlite://data/db?mode=rwc"),
            Some("data/db".into())
        );
        // Non-file URLs yield None.
        assert_eq!(sqlite_fs_path("sqlite::memory:"), None);
        assert_eq!(sqlite_fs_path("postgres://h/db"), None);
    }

    // The two-slash forms both callers can produce must create missing parent
    // directories and connect (file creation delegated to the driver via
    // `?mode=rwc`): the relative form (relative SYSTEM_ROOT) and the absolute
    // three-slash form.
    #[tokio::test]
    async fn build_database_creates_parent_dirs_for_relative_sqlite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("cognee.db");
        let url = format!("sqlite:{}?mode=rwc", nested.display());
        build_database(&ctx_with_db_url(url))
            .await
            .expect("relative sqlite URL should connect after dir creation");
        assert!(nested.parent().expect("parent").exists());
    }

    #[tokio::test]
    async fn build_database_creates_parent_dirs_for_absolute_sqlite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("x").join("y").join("cognee.db");
        // Absolute path → three-slash form; `?mode=rwc` lets the driver create
        // the file (build_database no longer creates it itself).
        let url = format!("sqlite://{}?mode=rwc", nested.display());
        build_database(&ctx_with_db_url(url))
            .await
            .expect("absolute sqlite URL should connect after dir creation");
        assert!(nested.parent().expect("parent").exists());
    }

    #[tokio::test]
    async fn build_database_handles_in_memory() {
        build_database(&ctx_with_db_url("sqlite::memory:".to_string()))
            .await
            .expect("in-memory sqlite should connect");
    }
}
