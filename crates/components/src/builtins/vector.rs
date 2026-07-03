//! Built-in vector database factories: pgvector, lancedb, brute-force, mock.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_vector::{BruteForceVectorDB, VectorDB};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::VectorDbFactory;

/// Postgres + pgvector backend. Consumes the caller-resolved
/// [`BackendBuildContext::vector_postgres_url`].
#[cfg(feature = "pgvector")]
pub struct PgVectorFactory;

#[cfg(feature = "pgvector")]
#[async_trait]
impl VectorDbFactory for PgVectorFactory {
    fn provider(&self) -> &str {
        "pgvector"
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError> {
        let url = match ctx.vector_postgres_url.as_ref() {
            Some(Ok(url)) => url,
            Some(Err(cause)) => return Err(ComponentError::Config(cause.clone())),
            None => {
                return Err(ComponentError::Config(
                    "vector_db_provider=pgvector requires a resolved Postgres URL".into(),
                ));
            }
        };
        let adapter = cognee_vector::PgVectorAdapter::new(url, ctx.embedding_dimensions)
            .await
            .map_err(|e| ComponentError::VectorDb(format!("pgvector init failed: {e}")))?;
        Ok(Arc::new(adapter))
    }
}

/// Embedded LanceDB backend on non-Android targets.
///
/// The provider id is target-invariant: on Android (where the LanceDB + Arrow
/// native stack does not cross-compile) `build` transparently falls back to the
/// in-memory brute-force backend. `vector_db_url = ":memory:"` is honored as an
/// explicit brute-force opt-in for ephemeral / test workloads on all targets.
pub struct LanceDbFactory;

#[async_trait]
impl VectorDbFactory for LanceDbFactory {
    fn provider(&self) -> &str {
        "lancedb"
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError> {
        if ctx.vector_db_url == ":memory:" {
            return Ok(Arc::new(BruteForceVectorDB::new()));
        }

        #[cfg(not(target_os = "android"))]
        {
            let path = if ctx.vector_db_url.is_empty() {
                ctx.system_root_directory
                    .join("databases")
                    .join("cognee.lancedb")
            } else {
                std::path::PathBuf::from(&ctx.vector_db_url)
            };
            let adapter = cognee_vector::LanceDbAdapter::new(path)
                .await
                .map_err(|e| ComponentError::VectorDb(format!("lancedb init failed: {e}")))?;
            Ok(Arc::new(adapter))
        }
        #[cfg(target_os = "android")]
        {
            tracing::warn!(
                "vector_db_provider='lancedb' is not available on Android; falling back to \
                 in-memory brute-force. Set vector_db_provider='pgvector' for production \
                 durable storage."
            );
            Ok(Arc::new(BruteForceVectorDB::new()))
        }
    }
}

/// Pure-Rust in-memory brute-force backend (Android default + `:memory:`
/// escape hatch).
pub struct BruteForceFactory;

#[async_trait]
impl VectorDbFactory for BruteForceFactory {
    fn provider(&self) -> &str {
        "brute-force"
    }

    async fn build(&self, _ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError> {
        Ok(Arc::new(BruteForceVectorDB::new()))
    }
}

/// In-memory mock backend for tests / Postgres-free local dev.
#[cfg(feature = "testing")]
pub struct MockVectorFactory;

#[cfg(feature = "testing")]
#[async_trait]
impl VectorDbFactory for MockVectorFactory {
    fn provider(&self) -> &str {
        "mock"
    }

    async fn build(&self, _ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError> {
        Ok(Arc::new(cognee_vector::MockVectorDB::new()))
    }
}
