//! Selective backend cleanup -- `prune_data()` and `prune_system()`.
//!
//! Equivalent to Python's `cognee.api.v1.prune.prune`.

use cognee_graph::GraphDBTrait;
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::info;

use super::error::ApiError;

/// Granular flags controlling which backends `prune_system()` wipes.
#[derive(Debug, Clone)]
pub struct PruneTarget {
    /// Wipe all graph nodes and edges.
    pub graph: bool,
    /// Wipe all vector collections.
    pub vector: bool,
    /// Drop the relational metadata database (not yet implemented).
    pub metadata: bool,
    /// Wipe session cache.
    pub cache: bool,
}

impl PruneTarget {
    /// Everything except metadata (matches Python defaults).
    pub fn default_system() -> Self {
        Self {
            graph: true,
            vector: true,
            metadata: false,
            cache: true,
        }
    }

    /// All backends.
    pub fn all() -> Self {
        Self {
            graph: true,
            vector: true,
            metadata: true,
            cache: true,
        }
    }
}

impl Default for PruneTarget {
    fn default() -> Self {
        Self::default_system()
    }
}

/// Summary of a prune operation.
#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub data_pruned: bool,
    pub graph_pruned: bool,
    pub vector_pruned: bool,
    pub metadata_pruned: bool,
    pub cache_pruned: bool,
}

/// Remove all files from data storage.
///
/// Equivalent to Python's `prune.prune_data()`.
pub async fn prune_data(storage: &dyn StorageTrait) -> Result<(), ApiError> {
    storage.remove_all().await?;
    info!("prune_data: all storage files removed");
    Ok(())
}

/// Selective backend cleanup.
///
/// Equivalent to Python's `prune.prune_system(graph, vector, metadata, cache)`.
///
/// # Arguments
/// * `target` - Which backends to wipe.
/// * `graph_db` - Graph database (required if `target.graph` is true).
/// * `vector_db` - Vector database (required if `target.vector` is true).
/// * `session_store` - Session store (required if `target.cache` is true).
///
/// # Notes
/// - `target.metadata` is accepted but currently a no-op (logged as a
///   warning). Dropping and recreating the relational database is deferred.
pub async fn prune_system(
    target: &PruneTarget,
    graph_db: Option<&dyn GraphDBTrait>,
    vector_db: Option<&dyn VectorDB>,
    session_store: Option<&dyn SessionStore>,
) -> Result<PruneResult, ApiError> {
    let mut result = PruneResult::default();

    if target.graph {
        if let Some(gdb) = graph_db {
            gdb.delete_graph().await?;
            result.graph_pruned = true;
            info!("prune_system: graph wiped");
        } else {
            tracing::warn!("prune_system: graph=true but no graph_db provided; skipping");
        }
    }

    if target.vector {
        if let Some(vdb) = vector_db {
            vdb.prune().await?;
            result.vector_pruned = true;
            info!("prune_system: vector collections wiped");
        } else {
            tracing::warn!("prune_system: vector=true but no vector_db provided; skipping");
        }
    }

    if target.metadata {
        // Deferred -- dropping and recreating the DB is complex and rarely
        // needed (Python also defaults metadata=False).
        tracing::warn!(
            "prune_system: metadata pruning is not yet implemented; \
             the relational database was NOT dropped"
        );
    }

    if target.cache {
        if let Some(store) = session_store {
            store.prune().await?;
            result.cache_pruned = true;
            info!("prune_system: session cache wiped");
        } else {
            tracing::warn!("prune_system: cache=true but no session_store provided; skipping");
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_target_defaults_match_python() {
        let target = PruneTarget::default();
        assert!(target.graph);
        assert!(target.vector);
        assert!(!target.metadata, "metadata defaults to false like Python");
        assert!(target.cache);
    }

    #[test]
    fn prune_target_all_enables_everything() {
        let target = PruneTarget::all();
        assert!(target.graph);
        assert!(target.vector);
        assert!(target.metadata);
        assert!(target.cache);
    }
}
