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
use cognee_database::SyncOperationRepository;
use cognee_database::permissions::PermissionsRepository;
use cognee_delete::DeleteService;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_ontology::OntologyManager;
use cognee_search::{SearchOrchestrator, SessionManager, SessionStore};
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

    // ── P4 read-path slots ────────────────────────────────────────────────
    //
    // Optional handles wired by embedders that want the full read-path
    // surface. Each slot is `None` by default; the relevant routers
    // surface a 500-level error when the corresponding handle is missing.
    /// Pre-built search orchestrator. `None` means HTTP search is unwired
    /// — handlers return `SearchError {500, "Internal server error"}`.
    pub search_orchestrator: Option<Arc<SearchOrchestrator>>,

    /// Configured LLM adapter for `/api/v1/llm/*` handlers.
    pub llm: Option<Arc<dyn Llm>>,

    /// Knowledge-graph DB used by the visualize router.
    pub graph_db: Option<Arc<dyn GraphDBTrait>>,

    /// SeaORM-backed `PermissionsRepository` for the 8-step `user_can`
    /// resolution per `tenants.md §5.1`.
    ///
    /// Kept optional so test fixtures that build a bare `ComponentHandles`
    /// without the new repo continue to compile; the
    /// [`crate::permissions::check_permission`] helper falls back to
    /// `AclDb::has_permission_with_roles` when this slot is `None`.
    pub permissions: Option<Arc<dyn PermissionsRepository>>,

    /// `sync_operations` repository — wires `POST /api/v1/sync` and
    /// `GET /api/v1/sync/status`. Optional so test fixtures that don't
    /// exercise the cloud sync flow can leave it unset.
    pub sync_ops: Option<Arc<dyn SyncOperationRepository>>,

    /// Backing store for session Q&A history — wires the `session` source
    /// of `POST /api/v1/recall` (Python `_search_session`,
    /// `recall.py:146-208`). `None` means session-source recall returns
    /// empty (matches Python's `is_available` short-circuit at
    /// `recall.py:170-171`). Reuses the `SessionStore` re-exported by
    /// `cognee-search` to avoid pulling `cognee-session` into the crate's
    /// non-dev dependency graph.
    pub session_store: Option<Arc<dyn SessionStore>>,

    /// Manager for agent-trace sessions and the per-session graph context
    /// snapshot — wires the `trace` and `graph_context` sources of
    /// `POST /api/v1/recall` (Python `_search_trace` /
    /// `_fetch_graph_context`). `None` means both sources return empty.
    pub session_manager: Option<Arc<SessionManager>>,
}

impl ComponentHandles {
    /// Return the formatted knowledge-graph data for a dataset as the JSON
    /// shape `{"nodes": [...], "edges": [...]}`.
    ///
    /// **Blocking gap**: the underlying `get_formatted_graph_data` function
    /// has not yet been ported from Python. Returns an empty graph
    /// `{"nodes": [], "edges": []}` until the implementation lands.
    ///
    /// The WebSocket handler calls this on every event and substitutes `{}`
    /// on any error, so a stub return is correct wire-parity for now.
    ///
    /// TODO: wire to `cognee_graph::get_formatted_graph_data(dataset_id, user)`
    /// once that function is ported.
    pub async fn formatted_graph_data(
        &self,
        _dataset_id: Option<uuid::Uuid>,
        _user_id: uuid::Uuid,
    ) -> Result<serde_json::Value, anyhow::Error> {
        Ok(serde_json::json!({"nodes": [], "edges": []}))
    }
}
