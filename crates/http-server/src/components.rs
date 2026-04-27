//! `ComponentHandles` — pre-built component instances passed to P2 handlers.
//!
//! This struct is a lightweight alternative to `cognee_lib::ComponentManager`
//! that avoids a dependency cycle: `cognee-lib` may eventually import
//! `cognee-http-server`, so `cognee-http-server` must not import `cognee-lib`.
//!
//! All components are eagerly initialized in `AppState::build`; there is no
//! lazy caching here (unlike `ComponentManager`'s `RwLock` slots).

use std::sync::Arc;

use cognee_database::DatabaseConnection;
use cognee_delete::DeleteService;
use cognee_ontology::OntologyManager;
use cognee_storage::StorageTrait;

/// Pre-initialized pipeline component handles shared across all P2 handlers.
///
/// Obtained from `state.components()`.
#[derive(Clone)]
pub struct ComponentHandles {
    /// SeaORM database connection (implements `IngestDb`, `DeleteDb`, `AclDb`).
    pub database: Arc<DatabaseConnection>,

    /// File storage backend.
    pub storage: Arc<dyn StorageTrait>,

    /// Fully configured `DeleteService` (with storage + DB wired).
    pub delete_service: Arc<DeleteService>,

    /// Ontology manager (per-user file storage).
    pub ontology_manager: Arc<OntologyManager>,
}
