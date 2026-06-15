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
    /// Drop the relational metadata database.
    ///
    /// NOT IMPLEMENTED (audit B6.1 / task 09 Option A) — setting this has no
    /// effect and `result.metadata_pruned` stays `false`. Python's
    /// `prune_system(metadata=True)` physically drops the DB file; that
    /// capability is deferred to Option A of task 09.
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

    /// All backends Rust currently supports wiping (graph, vector, cache).
    ///
    /// `metadata` is intentionally `false`: dropping the relational DB is not
    /// yet implemented (Python's `prune_system(metadata=True)` drops it; the
    /// Rust public `prune` default is `metadata=False`, which this matches).
    /// Tracked as audit B6.1 / task 09 Option A.
    pub fn all() -> Self {
        Self {
            graph: true,
            vector: true,
            metadata: false,
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
        // NOT IMPLEMENTED: dropping the relational DB is deferred (audit B6.1 /
        // task 09 Option A). We intentionally do NOT set result.metadata_pruned,
        // so the returned PruneResult truthfully reports metadata_pruned=false
        // even when a caller forced target.metadata=true by hand.
        tracing::warn!(
            "prune_system: metadata pruning is NOT implemented; the relational \
             database was NOT dropped (result.metadata_pruned stays false)"
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
    fn prune_target_all_does_not_advertise_metadata() {
        // Option B (audit B6.1): all() must not claim metadata will be wiped
        // because the relational-drop is not yet implemented.
        let target = PruneTarget::all();
        assert!(target.graph);
        assert!(target.vector);
        assert!(
            !target.metadata,
            "all() must not advertise metadata=true until Option A is implemented"
        );
        assert!(target.cache);
    }

    #[tokio::test]
    async fn prune_system_metadata_pruned_stays_false_when_not_implemented() {
        // Even when a caller forces target.metadata=true, the returned
        // PruneResult must report metadata_pruned=false (not yet implemented).
        let target = PruneTarget {
            graph: false,
            vector: false,
            metadata: true,
            cache: false,
        };
        let result = prune_system(&target, None, None, None)
            .await
            .expect("prune_system must not error");
        assert!(
            !result.metadata_pruned,
            "metadata_pruned must be false when metadata drop is not implemented"
        );
    }
}
