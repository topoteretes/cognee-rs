//! `ComponentHandles` — pre-built component instances passed to P2 handlers.
//!
//! This struct is a lightweight alternative to `cognee_lib::ComponentManager`
//! that avoids a dependency cycle: `cognee-lib` may eventually import
//! `cognee-http-server`, so `cognee-http-server` must not import `cognee-lib`.
//!
//! All components are eagerly initialized in `AppState::build`; there is no
//! lazy caching here (unlike `ComponentManager`'s `RwLock` slots).

use std::sync::Arc;

use cognee_core::CpuPool;
use cognee_database::AclDb;
use cognee_database::{CheckpointStore, DatabaseConnection};
use cognee_delete::DeleteService;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_llm::ResponsesClient;
use cognee_llm::Transcriber;
use cognee_ontology::{OntologyManager, OntologyResolver};
use cognee_search::{SearchOrchestrator, SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

use crate::cloud_client::CloudDeleteClient;
use crate::notebook_runner::NotebookRunner;

/// Pre-initialized pipeline component handles shared across all P2 handlers.
///
/// Obtained from `state.components()`.
#[derive(Clone)]
pub struct ComponentHandles {
    /// SeaORM database connection (implements `IngestDb`, `DeleteDb`).
    /// The `AclDb` implementation is provided by the closed
    /// `cognee-access-control` crate and wired through [`Self::acl_db`].
    pub database: Arc<DatabaseConnection>,

    /// Optional ACL backend. `None` in pure-OSS builds (which do not enforce
    /// permissions). Wired by the closed cloud assembly to a newtype that
    /// implements `AclDb` over the shared `DatabaseConnection`.
    pub acl_db: Option<Arc<dyn AclDb>>,

    /// File storage backend.
    pub storage: Arc<dyn StorageTrait>,

    /// Fully configured `DeleteService` (with storage + DB wired).
    pub delete_service: Arc<DeleteService>,

    /// Optional cloud delete proxy used by `POST /api/v1/forget`.
    pub cloud_client: Option<Arc<dyn CloudDeleteClient>>,

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

    /// Transcriber for audio document processing (Whisper). `None` when the
    /// configured LLM provider does not support audio transcription.
    pub transcriber: Option<Arc<dyn Transcriber>>,

    /// Knowledge-graph DB used by the visualize router.
    pub graph_db: Option<Arc<dyn GraphDBTrait>>,

    /// Vector DB handle required by [`cognee_core::TaskContext`] when the
    /// add / cognify / memify convenience functions route through
    /// `pipeline::execute` (LIB-06). `None` means the corresponding
    /// pipeline handlers surface a 500 / 409 envelope at runtime.
    pub vector_db: Option<Arc<dyn VectorDB>>,

    /// CPU pool used by [`cognee_core::TaskContext`]. Same routing notes
    /// as [`vector_db`](Self::vector_db).
    pub thread_pool: Option<Arc<dyn CpuPool>>,

    /// Text embedding engine used by the cognify pipeline (chunks, entities,
    /// summaries). `None` means the cognify / update handlers surface a 500
    /// envelope at runtime.
    pub embedding_engine: Option<Arc<dyn EmbeddingEngine>>,

    /// Ontology resolver passed into the cognify pipeline. `None` means
    /// the cognify / update handlers fall back to a pass-through
    /// `NoOpOntologyResolver`, matching the CLI default when no
    /// `ontology_file_path` is configured.
    pub ontology_resolver: Option<Arc<dyn OntologyResolver>>,

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

    /// Checkpoint store used by improve Stage 4 (`sync_graph_to_session`) to
    /// persist per-session high-water marks and avoid re-syncing old edges.
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,

    /// OpenAI Responses API client — wires `POST /api/v1/responses`
    /// (Python `get_responses_router.py`). `None` means the handler
    /// returns `500` "responses client is not wired" until embedders
    /// populate it.
    pub responses_client: Option<Arc<dyn ResponsesClient>>,

    /// Notebook cell execution backend used by
    /// `POST /api/v1/notebooks/{notebook_id}/{cell_id}/run`. `None` means
    /// the handler returns 501 — the same envelope it returned in Stage A
    /// before Stage B landed — preserving wire compatibility for embedders
    /// that don't want to expose code execution.
    pub notebook_runner: Option<Arc<dyn NotebookRunner>>,
}

impl ComponentHandles {
    /// Return the formatted knowledge-graph data for a dataset as the JSON
    /// shape `{"nodes": [...], "edges": [...]}`.
    ///
    /// Wires to `cognee_graph::get_formatted_graph_data` when both a
    /// `graph_db` handle and a `dataset_id` are available. When either is
    /// missing — e.g. the server is running in test mode without backends —
    /// returns the empty-graph fallback `{"nodes": [], "edges": []}` so that
    /// the WS frame still has a valid shape.
    ///
    /// Python parity: `cognee.modules.graph.methods.get_formatted_graph_data`.
    pub async fn formatted_graph_data(
        &self,
        dataset_id: Option<uuid::Uuid>,
        user_id: uuid::Uuid,
    ) -> Result<serde_json::Value, anyhow::Error> {
        let Some(graph_db) = self.graph_db.as_ref() else {
            return Ok(serde_json::json!({"nodes": [], "edges": []}));
        };
        let Some(did) = dataset_id else {
            return Ok(serde_json::json!({"nodes": [], "edges": []}));
        };
        cognee_graph::get_formatted_graph_data(graph_db.as_ref(), did, user_id)
            .await
            .map_err(|e| anyhow::anyhow!("get_formatted_graph_data failed: {e}"))
    }
}
