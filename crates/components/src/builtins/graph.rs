//! Built-in graph database factories: ladybug/kuzu (embedded) and Postgres.
//!
//! Every item here lives behind a graph-backend feature, so the imports are
//! gated too — otherwise a build with neither `ladybug` nor `pggraph` (e.g. a
//! consumer depending on this crate with `default-features = false`) would warn
//! on unused imports and fail under `-D warnings`.

#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use std::path::Path;
#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use std::sync::Arc;

#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use async_trait::async_trait;
#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use cognee_graph::GraphDBTrait;

#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use crate::context::BackendBuildContext;
#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use crate::error::ComponentError;
#[cfg(any(feature = "ladybug", feature = "pggraph"))]
use crate::traits::GraphDbFactory;

/// Embedded ladybug/kuzu graph backend, stored at a local file path.
#[cfg(feature = "ladybug")]
pub struct LadybugGraphFactory {
    provider: &'static str,
}

#[cfg(feature = "ladybug")]
impl LadybugGraphFactory {
    /// Construct a factory registered under `provider` (`"ladybug"` or `"kuzu"`).
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[cfg(feature = "ladybug")]
#[async_trait]
impl GraphDbFactory for LadybugGraphFactory {
    fn provider(&self) -> &str {
        self.provider
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let graph_path = if ctx.graph_file_path.is_empty() {
            format!("{}/graph", ctx.system_root_directory.display())
        } else {
            ctx.graph_file_path.clone()
        };

        if let Some(parent) = Path::new(&graph_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ComponentError::GraphDb(format!("create_dir_all({}): {e}", parent.display()))
            })?;
        }

        let graph_db = cognee_graph::LadybugAdapter::new(&graph_path)
            .await
            .map_err(|e| ComponentError::GraphDb(format!("initialization failed: {e}")))?;
        graph_db
            .initialize()
            .await
            .map_err(|e| ComponentError::GraphDb(format!("schema initialization failed: {e}")))?;
        Ok(Arc::new(graph_db))
    }
}

/// Postgres-backed graph store. Consumes the caller-resolved
/// [`BackendBuildContext::graph_postgres_url`].
#[cfg(feature = "pggraph")]
pub struct PgGraphFactory {
    provider: &'static str,
}

#[cfg(feature = "pggraph")]
impl PgGraphFactory {
    /// Construct a factory registered under `provider` (`"postgres"` or
    /// `"postgresql"`).
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[cfg(feature = "pggraph")]
#[async_trait]
impl GraphDbFactory for PgGraphFactory {
    fn provider(&self) -> &str {
        self.provider
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let url = ctx.graph_postgres_url.as_ref().ok_or_else(|| {
            ComponentError::Config(
                "graph_database_provider=postgres requires a resolved Postgres URL".into(),
            )
        })?;
        let adapter = cognee_graph::PgGraphAdapter::new(url)
            .await
            .map_err(|e| ComponentError::GraphDb(format!("pggraph init failed: {e}")))?;
        Ok(Arc::new(adapter))
    }
}
