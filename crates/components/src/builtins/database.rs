//! Relational database construction. No provider choice (sea-orm dispatches on
//! the URL scheme), so this is a free function rather than a registered factory.

use std::path::Path;
use std::sync::Arc;

use cognee_database::{DatabaseConnection, connect, initialize};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;

/// Connect to and initialize the relational database from the resolved URL.
///
/// For SQLite file-backed databases, the parent directory is created first:
/// sea-orm's `?mode=rwc` creates the *file* but not missing ancestor
/// directories, so a settings override that redirects the DB to a new path
/// (e.g. per-test isolation) would otherwise fail with "unable to open
/// database file".
///
/// URL shapes handled:
///   `sqlite:./rel/path/db`       (relative, 1-slash)
///   `sqlite:///abs/path/db`      (absolute, 3-slash)
///   `sqlite://localhost/abs/db`  (host form)
/// All others (postgres, in-memory `sqlite::memory:`) are left alone.
pub async fn build_database(
    ctx: &BackendBuildContext,
) -> Result<Arc<DatabaseConnection>, ComponentError> {
    let url = &ctx.relational_db_url;

    if url.starts_with("sqlite:") && !url.contains(":memory:") {
        // Strip the sqlite: scheme and any leading host ("//localhost") or
        // extra slashes to get the raw filesystem path (before '?').
        let after_scheme = url.trim_start_matches("sqlite:");
        let path_part = if after_scheme.starts_with("//localhost/") {
            Some(&after_scheme["//localhost".len()..])
        } else if after_scheme.starts_with("///") {
            // sqlite:///abs/path — empty authority, absolute path.
            Some(&after_scheme[2..])
        } else if after_scheme.starts_with("//") {
            // sqlite://somehost/... — genuine host form; leave entirely to the
            // driver instead of attempting create_dir_all("//somehost").
            None
        } else {
            Some(after_scheme)
        };
        // Drop query string (e.g. ?mode=rwc).
        if let Some(path_part) = path_part {
            let path_no_query = path_part.split('?').next().unwrap_or(path_part);
            let db_path = Path::new(path_no_query);
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
            // Create the DB file when missing. `cognee-lib`'s default URL carries
            // `?mode=rwc` (the driver would create it), but the HTTP server's
            // `sqlite://{path}` form has no such query, so without this an
            // absolute path to a not-yet-existing file fails to open. Creating
            // an empty file is a valid empty SQLite DB and is idempotent for the
            // `mode=rwc` form. Non-fatal — let the driver surface real errors.
            if !db_path.as_os_str().is_empty()
                && !db_path.exists()
                && let Err(e) = std::fs::File::create(db_path)
            {
                tracing::warn!(
                    "could not create SQLite database file '{}': {e}",
                    db_path.display()
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
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
            },
        }
    }

    // Both default URL shapes used by the two callers must create missing parent
    // directories and connect: `cognee-lib`'s single-slash relative form and the
    // absolute three-slash form the HTTP server produces.
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
        // Absolute path → three-slash form (`sqlite://` + `/abs/...`).
        let url = format!("sqlite://{}", nested.display());
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
