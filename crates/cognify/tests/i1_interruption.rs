#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! **Interrupting** invariant I1: an artifact never exists in the graph or the
//! vector store without an ownership row naming the run that created it.
//!
//! Everything else on this branch exercises I1 on success paths, or with a
//! single injected failure at the *LLM* layer — which is upstream of every
//! store write and therefore cannot interrupt the ledger→artifact window at
//! all. This file interrupts the window itself: it wraps the mock stores in
//! decorators that fail the *n*-th write of each kind, sweeps *n* over every
//! write a cognify run actually performs, and re-checks I1 after each.
//!
//! Three things make this evidence rather than argument:
//!
//! 1. The census test enumerates the writes, so the sweep is over the real
//!    call sites rather than a hand-picked list that silently shrinks when a
//!    stage is added.
//! 2. [`I1Audit::violations`] is the single checker, and
//!    `proof_of_detection_*` seeds the exact defect it exists to catch — a
//!    graph node with no ledger row — and asserts it is caught.
//! 3. The run is driven with [`RollbackScope::Nothing`] as well as the default
//!    `WholeRun`, so a pass cannot be credited to the sweep repairing a
//!    violation the ordering allowed. Under `Nothing` nothing is ever removed,
//!    so I1 there is a statement about write *ordering* alone.
//!
//! Fully offline: `MockLlm` / `MockStorage` / `MockGraphDB` / `MockVectorDB` /
//! `MockEmbeddingEngine` over in-memory SQLite. No network, no LLM key, no
//! skip path.

// ── Floor guard ────────────────────────────────────────────────────────────
// Deliberately NOT behind `#![cfg(feature = "testing")]`, unlike its siblings
// in this directory. `cognee-cognify/testing` is not a default feature and
// nothing outside `cognee/testing` turns it on, so a file-level gate compiles
// the whole target away under `cargo test -p cognee-cognify`: libtest then
// prints `running 0 tests … ok` and the suite reports green while proving
// nothing — the silent no-op `scripts/ci/assert_pg_suite_ran.sh` exists to make
// impossible. The gate buys nothing here either way: `testing` only forwards to
// `cognee-storage/testing`, `cognee-graph/testing` and `cognee-vector/testing`,
// and this crate's `[dev-dependencies]` already enable all three for every test
// build. Leaving the file ungated means it cannot compile to zero cases, under
// any feature set. Do not "restore" the attribute.
mod rollback_harness;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_cognify::{CognifyConfig, CognifyError, CognifyResult, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
use cognee_database::{
    DatabaseConnection, GraphEdge, GraphNode, PipelineRunRepository, SeaOrmPipelineRunRepository,
    connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, NodeData};
use cognee_models::{Data, Dataset, EdgeType, Triplet};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockVectorDB};
use cognee_vector::{SearchResult, VectorDB, VectorDBError, VectorDBResult, VectorPoint};
use serde_json::Value;
use uuid::Uuid;

use rollback_harness::SummarizationFailingLlm;

// ---------------------------------------------------------------------------
// Interruption policy
// ---------------------------------------------------------------------------

/// Which write an [`InterruptingGraphDb`] / [`InterruptingVectorDb`] fails.
///
/// Ordinal rather than by-argument on purpose: the point is to interrupt
/// *every* write site the pipeline reaches, and an ordinal sweep enumerates
/// them without a hand-maintained list of which stage writes what.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Trip {
    /// Nothing fails — used for the census run.
    Never,
    /// Fail the n-th (1-based) node write (`add_node_raw` / `add_nodes_raw`,
    /// which every `add_nodes` call funnels through).
    NodeWrite(usize),
    /// Fail the n-th (1-based) `add_edges` / `add_edge` call.
    EdgeWrite(usize),
    /// Fail the n-th (1-based) `index_points` call.
    IndexPoints(usize),
}

/// Names the writes a run performed, in order, so a test can say what the
/// n-th one was when an assertion about it fails.
#[derive(Default)]
struct WriteLog {
    node_writes: Mutex<Vec<String>>,
    edge_writes: Mutex<Vec<String>>,
    index_writes: Mutex<Vec<String>>,
}

impl WriteLog {
    fn node_writes(&self) -> Vec<String> {
        self.node_writes.lock().unwrap().clone() // lock poison is unrecoverable
    }
    fn edge_writes(&self) -> Vec<String> {
        self.edge_writes.lock().unwrap().clone() // lock poison is unrecoverable
    }
    fn index_writes(&self) -> Vec<String> {
        self.index_writes.lock().unwrap().clone() // lock poison is unrecoverable
    }
}

/// A [`MockGraphDB`] that fails one nominated write and delegates everything
/// else — including the deletes the run sweep issues, so a swept run really
/// does lose its artifacts.
struct InterruptingGraphDb {
    inner: MockGraphDB,
    trip: Trip,
    node_calls: AtomicUsize,
    edge_calls: AtomicUsize,
    log: Arc<WriteLog>,
    /// The edges that would exist in a *production* graph store.
    ///
    /// `MockGraphDB::delete_nodes` removes the node and leaves its edges
    /// behind; both real adapters cascade — ladybug issues `DETACH DELETE`
    /// (`ladybug.rs:903`) and the Postgres adapter declares
    /// `REFERENCES graph_node(id) ON DELETE CASCADE`
    /// (`pg_graph_adapter.rs:1642-1643`). The run sweep depends on that
    /// cascade: it deletes nodes and never edges ("graph edges are never
    /// deleted directly — they cascade with their endpoints",
    /// `sweep.rs:171-173`). Auditing the mock's raw edge list would therefore
    /// report a mock artifact as an I1 violation, so this shadow applies the
    /// production semantics.
    edges: Mutex<BTreeSet<(String, String, String)>>,
}

impl InterruptingGraphDb {
    fn new(trip: Trip, log: Arc<WriteLog>) -> Self {
        Self {
            inner: MockGraphDB::new(),
            trip,
            node_calls: AtomicUsize::new(0),
            edge_calls: AtomicUsize::new(0),
            log,
            edges: Mutex::new(BTreeSet::new()),
        }
    }

    /// The edges a production store would still hold.
    fn surviving_edges(&self) -> BTreeSet<(String, String, String)> {
        self.edges.lock().unwrap().clone() // lock poison is unrecoverable
    }

    /// Record the write and report whether it is the one to fail.
    fn note_node_write(&self, what: String) -> bool {
        let nth = self.node_calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.log
            .node_writes
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(format!("#{nth} {what}"));
        self.trip == Trip::NodeWrite(nth)
    }

    fn note_edge_write(&self, what: String) -> bool {
        let nth = self.edge_calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.log
            .edge_writes
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(format!("#{nth} {what}"));
        self.trip == Trip::EdgeWrite(nth)
    }
}

fn node_error() -> GraphDBError {
    GraphDBError::NodeError("interrupted: simulated graph node write failure".into())
}

fn edge_error() -> GraphDBError {
    GraphDBError::EdgeError("interrupted: simulated graph edge write failure".into())
}

#[async_trait]
impl GraphDBTrait for InterruptingGraphDb {
    async fn initialize(&self) -> GraphDBResult<()> {
        self.inner.initialize().await
    }
    async fn is_empty(&self) -> GraphDBResult<bool> {
        self.inner.is_empty().await
    }
    async fn query(
        &self,
        q: &str,
        params: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<Vec<Vec<Value>>> {
        self.inner.query(q, params).await
    }
    async fn delete_graph(&self) -> GraphDBResult<()> {
        self.inner.delete_graph().await
    }
    async fn has_node(&self, id: &str) -> GraphDBResult<bool> {
        self.inner.has_node(id).await
    }
    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        if self.note_node_write("add_node_raw(1)".into()) {
            return Err(node_error());
        }
        self.inner.add_node_raw(node).await
    }
    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        if self.note_node_write(format!("add_nodes_raw({})", nodes.len())) {
            return Err(node_error());
        }
        self.inner.add_nodes_raw(nodes).await
    }
    async fn delete_node(&self, id: &str) -> GraphDBResult<()> {
        self.inner.delete_node(id).await?;
        let mut edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        edges.retain(|(source, target, _)| source != id && target != id);
        Ok(())
    }
    async fn delete_nodes(&self, ids: &[String]) -> GraphDBResult<()> {
        self.inner.delete_nodes(ids).await?;
        let gone: HashSet<&String> = ids.iter().collect();
        let mut edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        edges.retain(|(source, target, _)| !gone.contains(source) && !gone.contains(target));
        Ok(())
    }
    async fn get_node(&self, id: &str) -> GraphDBResult<Option<NodeData>> {
        self.inner.get_node(id).await
    }
    async fn get_nodes(&self, ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        self.inner.get_nodes(ids).await
    }
    async fn has_edge(&self, s: &str, t: &str, r: &str) -> GraphDBResult<bool> {
        self.inner.has_edge(s, t, r).await
    }
    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        self.inner.has_edges(edges).await
    }
    async fn add_edge(
        &self,
        s: &str,
        t: &str,
        r: &str,
        p: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<()> {
        if self.note_edge_write(format!("add_edge({r})")) {
            return Err(edge_error());
        }
        self.inner.add_edge(s, t, r, p).await?;
        self.edges
            .lock()
            .unwrap() // lock poison is unrecoverable
            .insert((s.to_string(), t.to_string(), r.to_string()));
        Ok(())
    }
    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        if self.note_edge_write(format!("add_edges({})", edges.len())) {
            return Err(edge_error());
        }
        self.inner.add_edges(edges).await?;
        let mut shadow = self.edges.lock().unwrap(); // lock poison is unrecoverable
        for (source, target, relationship, _) in edges {
            shadow.insert((source.clone(), target.clone(), relationship.clone()));
        }
        Ok(())
    }
    async fn get_edges(&self, id: &str) -> GraphDBResult<Vec<EdgeData>> {
        self.inner.get_edges(id).await
    }
    async fn get_neighbors(&self, id: &str) -> GraphDBResult<Vec<NodeData>> {
        self.inner.get_neighbors(id).await
    }
    async fn get_connections(
        &self,
        id: &str,
    ) -> GraphDBResult<Vec<(NodeData, HashMap<Cow<'static, str>, Value>, NodeData)>> {
        self.inner.get_connections(id).await
    }
    async fn get_graph_data(&self) -> GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<EdgeData>)> {
        self.inner.get_graph_data().await
    }
    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
        self.inner.get_graph_metrics(include_optional).await
    }
    async fn get_filtered_graph_data(
        &self,
        f: &HashMap<Cow<'static, str>, Vec<Value>>,
    ) -> GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<EdgeData>)> {
        self.inner.get_filtered_graph_data(f).await
    }
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        op: &str,
    ) -> GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<EdgeData>)> {
        self.inner
            .get_nodeset_subgraph(node_type, node_names, op)
            .await
    }
}

/// A [`MockVectorDB`] that fails one nominated `index_points` call and
/// remembers every point id that was actually written, so the audit knows
/// which ids to look for.
struct InterruptingVectorDb {
    inner: MockVectorDB,
    trip: Trip,
    index_calls: AtomicUsize,
    /// `(data_type, field_name, point_id)` for every point a *successful*
    /// write put in the store.
    written: Mutex<BTreeSet<(String, String, Uuid)>>,
    log: Arc<WriteLog>,
}

impl InterruptingVectorDb {
    fn new(trip: Trip, log: Arc<WriteLog>) -> Self {
        Self {
            inner: MockVectorDB::new(),
            trip,
            index_calls: AtomicUsize::new(0),
            written: Mutex::new(BTreeSet::new()),
            log,
        }
    }

    fn record(&self, data_type: &str, field_name: &str, points: &[VectorPoint]) {
        let mut written = self.written.lock().unwrap(); // lock poison is unrecoverable
        for point in points {
            written.insert((data_type.to_string(), field_name.to_string(), point.id));
        }
    }

    /// Every point id this run wrote and that is still in the store.
    async fn surviving_points(&self) -> BTreeSet<(String, String, Uuid)> {
        let candidates = self.written.lock().unwrap().clone(); // lock poison is unrecoverable
        let mut alive = BTreeSet::new();
        for (data_type, field_name, id) in candidates {
            let found = self
                .inner
                .retrieve(&data_type, &field_name, &[id])
                .await
                .expect("retrieve from the mock vector store");
            if !found.is_empty() {
                alive.insert((data_type, field_name, id));
            }
        }
        alive
    }
}

#[async_trait]
impl VectorDB for InterruptingVectorDb {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        self.inner
            .create_collection(data_type, field_name, dimension)
            .await
    }
    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        self.inner.has_collection(data_type, field_name).await
    }
    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        let nth = self.index_calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.log
            .index_writes
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push(format!(
                "#{nth} index_points({data_type}/{field_name}, {})",
                points.len()
            ));
        if self.trip == Trip::IndexPoints(nth) {
            return Err(VectorDBError::StorageError(
                "interrupted: simulated vector index failure".into(),
            ));
        }
        self.inner
            .index_points(data_type, field_name, points)
            .await?;
        self.record(data_type, field_name, points);
        Ok(())
    }
    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        self.inner
            .upsert_raw_vectors(data_type, field_name, points)
            .await?;
        self.record(data_type, field_name, points);
        Ok(())
    }
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.inner
            .search_similar(data_type, field_name, query_vector, top_k)
            .await
    }
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        self.inner.delete_collection(data_type, field_name).await
    }
    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        self.inner
            .delete_points(data_type, field_name, point_ids)
            .await
    }
    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.inner.retrieve(data_type, field_name, ids).await
    }
    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        self.inner.collection_size(data_type, field_name).await
    }
    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        self.inner.list_collections().await
    }
}

// ---------------------------------------------------------------------------
// The I1 audit
// ---------------------------------------------------------------------------

/// One artifact that exists with no ownership row naming a run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Violation(String);

/// Everything I1 is about: the artifacts that exist, and the ownership rows
/// that claim them.
struct I1Audit {
    ledger_nodes: Vec<GraphNode>,
    ledger_edges: Vec<GraphEdge>,
    graph_nodes: BTreeSet<String>,
    graph_edges: BTreeSet<(String, String, String)>,
    vector_points: BTreeSet<(String, String, Uuid)>,
}

impl I1Audit {
    async fn collect(
        db: &DatabaseConnection,
        dataset_id: Uuid,
        graph_db: &InterruptingGraphDb,
        vector_db: &InterruptingVectorDb,
    ) -> Self {
        let (nodes, _mock_edges) = graph_db
            .get_graph_data()
            .await
            .expect("read the mock graph store");
        Self {
            ledger_nodes: get_nodes_by_dataset(db, dataset_id)
                .await
                .expect("read the node ledger"),
            ledger_edges: get_edges_by_dataset(db, dataset_id)
                .await
                .expect("read the edge ledger"),
            graph_nodes: nodes.into_iter().map(|(id, _)| id).collect(),
            graph_edges: graph_db.surviving_edges(),
            vector_points: vector_db.surviving_points().await,
        }
    }

    /// The `EdgeType_relationship_name` point id an ownership edge row
    /// licenses, recomputed exactly as `cognee-delete` does.
    fn edge_type_point_id(edge: &GraphEdge) -> Option<Uuid> {
        let edge_text = edge
            .attributes
            .as_ref()
            .and_then(|a| a.get("edge_text"))
            .and_then(Value::as_str);
        let text = EdgeType::retrieval_text(edge_text, &edge.relationship_name);
        (!text.is_empty()).then(|| EdgeType::deterministic_id(&text))
    }

    /// The `Triplet_text` point id an ownership edge row licenses.
    fn triplet_point_id(edge: &GraphEdge) -> Uuid {
        Triplet::new(
            edge.source_node_id,
            edge.destination_node_id,
            edge.relationship_name.clone(),
            String::new(),
        )
        .id
    }

    /// Every artifact that exists without an ownership row naming a run.
    ///
    /// The vector arm asks the question the sweep asks, not a weaker one: a
    /// point is claimed only if a row exists that `delete_vector_artifacts`
    /// would actually turn into a delete for that `(collection, id)` pair. A
    /// row that names the slug but not the collection would leave the point
    /// behind forever, so it does not count as ownership.
    fn violations(&self) -> Vec<Violation> {
        let mut out = Vec::new();

        // Nodes: slug ↔ graph node id, and the row must name a run.
        let owned_nodes: HashMap<Uuid, &GraphNode> = self
            .ledger_nodes
            .iter()
            .map(|row| (row.slug, row))
            .collect();
        for id in &self.graph_nodes {
            match Uuid::parse_str(id).ok().and_then(|u| owned_nodes.get(&u)) {
                None => out.push(Violation(format!("graph node {id} has no ownership row"))),
                Some(row) if row.pipeline_run_id.is_none() => out.push(Violation(format!(
                    "graph node {id} has an ownership row that names no run"
                ))),
                Some(_) => {}
            }
        }

        // Edges: (source, target, relationship) ↔ ownership edge row.
        let owned_edges: HashSet<(String, String, String)> = self
            .ledger_edges
            .iter()
            .filter(|row| row.pipeline_run_id.is_some())
            .map(|row| {
                (
                    row.source_node_id.to_string(),
                    row.destination_node_id.to_string(),
                    row.relationship_name.clone(),
                )
            })
            .collect();
        for edge in &self.graph_edges {
            if !owned_edges.contains(edge) {
                out.push(Violation(format!(
                    "graph edge {} -[{}]-> {} has no ownership row naming a run",
                    edge.0, edge.2, edge.1
                )));
            }
        }

        // Vector points: the set the sweep could reach from the rows.
        let mut reachable: HashSet<(String, String, Uuid)> = HashSet::new();
        for row in self
            .ledger_nodes
            .iter()
            .filter(|r| r.pipeline_run_id.is_some())
        {
            if let Value::Array(fields) = &row.indexed_fields {
                for field in fields.iter().filter_map(Value::as_str) {
                    reachable.insert((row.node_type.clone(), field.to_string(), row.slug));
                }
            }
        }
        for row in self
            .ledger_edges
            .iter()
            .filter(|r| r.pipeline_run_id.is_some())
        {
            reachable.insert(("Triplet".into(), "text".into(), Self::triplet_point_id(row)));
            if let Some(id) = Self::edge_type_point_id(row) {
                reachable.insert(("EdgeType".into(), "relationship_name".into(), id));
            }
        }
        for point in &self.vector_points {
            if !reachable.contains(point) {
                out.push(Violation(format!(
                    "vector point {} in {}_{} has no ownership row naming a run",
                    point.2, point.0, point.1
                )));
            }
        }

        out.sort();
        out
    }

    fn assert_holds(&self, context: &str) {
        let violations = self.violations();
        assert!(
            violations.is_empty(),
            "I1 violated {context}: {} artifact(s) exist with no ownership row\n{}",
            violations.len(),
            violations
                .iter()
                .map(|v| format!("  - {}", v.0))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Whether any ownership row names an artifact that is *not* in the
    /// stores — the tolerated direction of the window.
    fn has_unrealized_rows(&self) -> bool {
        self.ledger_nodes
            .iter()
            .any(|row| !self.graph_nodes.contains(&row.slug.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// One dataset, one file, one run — with the stores wrapped so a nominated
/// write fails.
struct Fixture {
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    owner_id: Uuid,
    graph_db: Arc<InterruptingGraphDb>,
    vector_db: Arc<InterruptingVectorDb>,
    log: Arc<WriteLog>,
    dataset_id: Uuid,
    items: Vec<Data>,
}

impl Fixture {
    async fn new(trip: Trip, texts: &[&str]) -> Self {
        Self::with_metadata(trip, texts, None).await
    }

    /// Same, but every item carries `external_metadata` — which is what makes
    /// `classify_documents` produce a `dlt_row` document and route the run
    /// through `extract_dlt_fk_edges`, a whole write site the prose fixture
    /// never reaches.
    async fn with_metadata(trip: Trip, texts: &[&str], metadata: Option<&str>) -> Self {
        let dataset_id = Uuid::new_v4();
        let owner_id = Uuid::new_v4();
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(&conn, Dataset::new("i1".into(), owner_id, None, dataset_id))
            .await
            .expect("seed dataset");
        let db = Arc::new(conn);

        let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
        let mut items = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            let data_id = Uuid::new_v4();
            let location = storage
                .store(text.as_bytes(), &format!("i1-{data_id}"))
                .await
                .expect("MockStorage::store");
            let mut builder = Data::builder(
                data_id,
                format!("i1-{index}.txt"),
                location,
                format!("i1-{index}.txt"),
                "txt",
                "text/plain",
                format!("test-hash-{data_id}"),
                owner_id,
            );
            if let Some(metadata) = metadata {
                builder = builder.external_metadata(metadata);
            }
            let item = builder.build();
            create_data(&db, item.clone()).await.expect("persist Data");
            attach_data_to_dataset(&db, dataset_id, data_id)
                .await
                .expect("attach to dataset");
            items.push(item);
        }

        let log = Arc::new(WriteLog::default());
        Self {
            storage,
            owner_id,
            graph_db: Arc::new(InterruptingGraphDb::new(trip.clone(), Arc::clone(&log))),
            vector_db: Arc::new(InterruptingVectorDb::new(trip, Arc::clone(&log))),
            log,
            db,
            dataset_id,
            items,
        }
    }

    async fn run(&self, config: &CognifyConfig) -> Result<CognifyResult, CognifyError> {
        // Answers extraction from the canned knowledge graph and
        // summarization from a canned summary, dispatching on the schema.
        // Nothing in this fixture's text trips its failure marker, so the LLM
        // layer never fails: every failure in this file comes from a store.
        self.run_with_llm(config, Arc::new(SummarizationFailingLlm))
            .await
    }

    async fn run_with_llm(
        &self,
        config: &CognifyConfig,
        llm: Arc<dyn cognee_llm::Llm>,
    ) -> Result<CognifyResult, CognifyError> {
        let repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&self.db)));
        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"),
        );
        cognify(
            self.items.clone(),
            self.dataset_id,
            Some(self.owner_id),
            None,
            None,
            llm,
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>,
            Arc::clone(&self.vector_db) as Arc<dyn VectorDB>,
            Arc::new(MockEmbeddingEngine::new(8)),
            Arc::clone(&self.db),
            repo,
            thread_pool,
            Arc::new(NoOpOntologyResolver::new()),
            config,
        )
        .await
    }

    async fn audit(&self) -> I1Audit {
        I1Audit::collect(&self.db, self.dataset_id, &self.graph_db, &self.vector_db).await
    }
}

/// The config the sweep runs under. Summarization on, so the `TextSummary`
/// node and `TextSummary_text` collection are exercised too; triplet indexing
/// on, so `Triplet_text` is.
fn full_config() -> CognifyConfig {
    CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_summarization(true)
        .with_triplet_embeddings(true)
}

const FIXTURE_TEXT: &str = "Alice works at Acme. Acme builds tools.";

// ---------------------------------------------------------------------------
// Census — what a run actually writes
// ---------------------------------------------------------------------------

/// Enumerate the store writes one cognify run performs, so the interruption
/// sweeps below cover every call site rather than a hand-picked list.
///
/// This is not decoration: if a stage is added that writes to a store, the
/// counts here change and the sweeps automatically widen to cover it.
#[tokio::test]
async fn census_of_the_writes_one_run_performs() {
    let fixture = Fixture::new(Trip::Never, &[FIXTURE_TEXT]).await;
    fixture
        .run(&full_config())
        .await
        .expect("uninterrupted run");

    let nodes = fixture.log.node_writes();
    let edges = fixture.log.edge_writes();
    let index = fixture.log.index_writes();

    println!("node writes ({}):", nodes.len());
    for w in &nodes {
        println!("    {w}");
    }
    println!("edge writes ({}):", edges.len());
    for w in &edges {
        println!("    {w}");
    }
    println!("vector index writes ({}):", index.len());
    for w in &index {
        println!("    {w}");
    }

    assert!(
        !nodes.is_empty() && !edges.is_empty() && !index.is_empty(),
        "the fixture must reach all three kinds of store write, or the \
         interruption sweeps below are vacuous"
    );

    fixture
        .audit()
        .await
        .assert_holds("after an uninterrupted run");
}

/// The number of writes of each kind the census above observed. Asserted, not
/// assumed: if a stage starts writing more, the sweeps widen and this constant
/// is what tells the reader they did.
const NODE_WRITES: usize = 5;
const EDGE_WRITES: usize = 2;
const INDEX_WRITES: usize = 7;

/// `RollbackScope::Nothing` — the sweep is disabled, so I1 can only be
/// upheld by the order the stages write in.
fn no_sweep_config() -> CognifyConfig {
    full_config().with_rollback_scope(cognee_cognify::RollbackScope::Nothing)
}

/// Interrupt the `nth` write of a kind and return the audit plus how many
/// writes of that kind were reached.
async fn interrupt(trip: Trip, config: &CognifyConfig) -> (I1Audit, usize, Vec<String>) {
    let fixture = Fixture::new(trip.clone(), &[FIXTURE_TEXT]).await;
    // The result is deliberately ignored: a run whose store write failed may
    // return `Err`, or may collect the failure and return `Ok` with the item
    // marked outstanding. I1 is a claim about the stores either way.
    let _ = fixture.run(config).await;
    let (reached, log) = match trip {
        Trip::NodeWrite(_) => (fixture.log.node_writes().len(), fixture.log.node_writes()),
        Trip::EdgeWrite(_) => (fixture.log.edge_writes().len(), fixture.log.edge_writes()),
        Trip::IndexPoints(_) => (fixture.log.index_writes().len(), fixture.log.index_writes()),
        Trip::Never => (0, Vec::new()),
    };
    (fixture.audit().await, reached, log)
}

// ---------------------------------------------------------------------------
// The interruption sweeps
// ---------------------------------------------------------------------------

/// Fail each graph *node* write in turn; I1 must hold after every one.
///
/// Run with the sweep disabled, so nothing can be credited to rollback
/// repairing a violation that the write ordering allowed.
#[tokio::test]
async fn interrupting_any_graph_node_write_preserves_i1_without_a_sweep() {
    let config = no_sweep_config();
    for nth in 1..=NODE_WRITES {
        let (audit, reached, log) = interrupt(Trip::NodeWrite(nth), &config).await;
        assert!(
            reached >= nth,
            "node write #{nth} was never reached, so the interruption was a no-op \
             (log: {log:?}) — the sweep bound {NODE_WRITES} is stale"
        );
        audit.assert_holds(&format!(
            "with graph node write #{nth} failing and no sweep (writes: {log:?})"
        ));
    }
}

/// Fail each graph *edge* write in turn.
#[tokio::test]
async fn interrupting_any_graph_edge_write_preserves_i1_without_a_sweep() {
    let config = no_sweep_config();
    for nth in 1..=EDGE_WRITES {
        let (audit, reached, log) = interrupt(Trip::EdgeWrite(nth), &config).await;
        assert!(
            reached >= nth,
            "edge write #{nth} was never reached (log: {log:?})"
        );
        audit.assert_holds(&format!(
            "with graph edge write #{nth} failing and no sweep (writes: {log:?})"
        ));
    }
}

/// Fail each vector `index_points` call in turn.
///
/// This is the arm that matters most: a vector point is reachable from the
/// ledger only through its row's `node_type` *and* `indexed_fields`, so a
/// collection the ledger does not describe is unsweepable even when a row for
/// the same slug exists.
#[tokio::test]
async fn interrupting_any_vector_index_write_preserves_i1_without_a_sweep() {
    let config = no_sweep_config();
    for nth in 1..=INDEX_WRITES {
        let (audit, reached, log) = interrupt(Trip::IndexPoints(nth), &config).await;
        assert!(
            reached >= nth,
            "vector index write #{nth} was never reached (log: {log:?})"
        );
        audit.assert_holds(&format!(
            "with vector index write #{nth} failing and no sweep (writes: {log:?})"
        ));
    }
}

/// The same sweeps under the default `RollbackScope::WholeRun`, where the
/// rollback machinery also runs. I1 must survive the sweep too — a sweep that
/// deletes ownership rows while leaving artifacts behind would manufacture
/// exactly the orphan it exists to remove.
#[tokio::test]
async fn interrupting_any_write_preserves_i1_through_the_run_sweep() {
    let config = full_config();
    for nth in 1..=NODE_WRITES {
        let (audit, _, log) = interrupt(Trip::NodeWrite(nth), &config).await;
        audit.assert_holds(&format!(
            "with graph node write #{nth} failing and the run sweep active (writes: {log:?})"
        ));
    }
    for nth in 1..=EDGE_WRITES {
        let (audit, _, log) = interrupt(Trip::EdgeWrite(nth), &config).await;
        audit.assert_holds(&format!(
            "with graph edge write #{nth} failing and the run sweep active (writes: {log:?})"
        ));
    }
    for nth in 1..=INDEX_WRITES {
        let (audit, _, log) = interrupt(Trip::IndexPoints(nth), &config).await;
        audit.assert_holds(&format!(
            "with vector index write #{nth} failing and the run sweep active (writes: {log:?})"
        ));
    }
}

/// The window between the ledger write and the artifact write is *allowed* to
/// leave rows that name artifacts which do not exist. This asserts that the
/// tolerated direction actually occurs — without it the sweeps above could
/// pass vacuously, by the ledger and the stores simply always agreeing.
#[tokio::test]
async fn the_tolerated_direction_is_the_one_that_occurs() {
    let config = no_sweep_config();
    // Failing the first node write leaves the extraction stage's ownership
    // rows written and its entities unwritten.
    let (audit, _, log) = interrupt(Trip::NodeWrite(1), &config).await;
    assert!(
        audit.has_unrealized_rows(),
        "interrupting the first node write must leave ownership rows naming \
         artifacts that were never written — the tolerated direction. If this \
         fails the interruption is not landing in the window at all \
         (writes: {log:?})"
    );
    audit.assert_holds("in the tolerated direction");
}

// ---------------------------------------------------------------------------
// Proof of detection
// ---------------------------------------------------------------------------

/// The audit is only evidence if it catches the defect it exists to catch.
///
/// Seeds each of the three shapes of I1 violation directly into the stores of
/// a run that had *already* passed the audit — a graph node, a graph edge and
/// a vector point that no ownership row names — and asserts the audit turns
/// from clean to reporting exactly those three.
#[tokio::test]
async fn proof_of_detection_the_audit_catches_an_unledgered_write() {
    let fixture = Fixture::new(Trip::Never, &[FIXTURE_TEXT]).await;
    fixture
        .run(&full_config())
        .await
        .expect("uninterrupted run");
    fixture
        .audit()
        .await
        .assert_holds("before the defect is seeded");

    let rogue_node = Uuid::new_v4();
    let rogue_target = Uuid::new_v4();
    let rogue_point = Uuid::new_v4();

    // A graph node written with no preceding ledger write — the exact defect
    // the ledger-before-artifacts ordering exists to prevent.
    fixture
        .graph_db
        .add_node_raw(serde_json::json!({ "id": rogue_node.to_string(), "name": "rogue" }))
        .await
        .expect("seed the rogue node");
    fixture
        .graph_db
        .add_node_raw(serde_json::json!({ "id": rogue_target.to_string(), "name": "rogue" }))
        .await
        .expect("seed the rogue endpoint");
    fixture
        .graph_db
        .add_edges(&[(
            rogue_node.to_string(),
            rogue_target.to_string(),
            "rogue_rel".to_string(),
            HashMap::new(),
        )])
        .await
        .expect("seed the rogue edge");
    fixture
        .vector_db
        .create_collection("Rogue", "text", 8)
        .await
        .expect("seed the rogue collection");
    fixture
        .vector_db
        .index_points(
            "Rogue",
            "text",
            &[VectorPoint::new(rogue_point, vec![0.0_f32; 8])],
        )
        .await
        .expect("seed the rogue point");

    let violations = fixture.audit().await.violations();
    let rendered: Vec<&str> = violations.iter().map(|v| v.0.as_str()).collect();

    assert_eq!(
        violations.len(),
        4,
        "the audit must report the two rogue nodes, the rogue edge and the \
         rogue point, and nothing else: {rendered:#?}"
    );
    assert!(
        rendered
            .iter()
            .any(|v| v.contains(&rogue_node.to_string()) && v.contains("graph node")),
        "the unledgered graph node must be reported: {rendered:#?}"
    );
    assert!(
        rendered.iter().any(|v| v.contains("rogue_rel")),
        "the unledgered graph edge must be reported: {rendered:#?}"
    );
    assert!(
        rendered
            .iter()
            .any(|v| v.contains(&rogue_point.to_string()) && v.contains("Rogue_text")),
        "the unledgered vector point must be reported: {rendered:#?}"
    );
}

/// The audit must not be satisfied by an ownership row that names no run.
///
/// A row with a `NULL` `pipeline_run_id` is invisible to every run-scoped
/// sweep query, so an artifact it "claims" is exactly as unsweepable as one
/// with no row at all. Without this the audit would pass on a regression that
/// simply stopped stamping the run id — which is the defect the branch's
/// ledger commit exists to prevent.
#[tokio::test]
async fn proof_of_detection_a_row_naming_no_run_is_not_ownership() {
    let fixture = Fixture::new(Trip::Never, &[FIXTURE_TEXT]).await;
    fixture
        .run(&full_config())
        .await
        .expect("uninterrupted run");

    let mut audit = fixture.audit().await;
    assert!(
        audit.violations().is_empty(),
        "the run itself must be clean first"
    );
    assert!(
        !audit.ledger_nodes.is_empty(),
        "the run must have written ownership rows"
    );

    for row in &mut audit.ledger_nodes {
        row.pipeline_run_id = None;
    }
    for row in &mut audit.ledger_edges {
        row.pipeline_run_id = None;
    }

    assert!(
        !audit.violations().is_empty(),
        "stripping the run id from every ownership row must make every artifact \
         unowned — an unsweepable row is not ownership"
    );
}

// ---------------------------------------------------------------------------
// The temporal branch
// ---------------------------------------------------------------------------

const TEMPORAL_TEXT: &str = "Alice joined Acme in 2020.";

/// Interrupt a temporal write and return the audit plus the write log.
async fn interrupt_temporal(trip: Trip, config: &CognifyConfig) -> (I1Audit, usize, Vec<String>) {
    let fixture = Fixture::new(trip.clone(), &[TEMPORAL_TEXT]).await;
    let _ = fixture
        .run_with_llm(
            config,
            Arc::new(rollback_harness::TemporalFixtureLlm::new()),
        )
        .await;
    let (reached, log) = match trip {
        Trip::NodeWrite(_) => (fixture.log.node_writes().len(), fixture.log.node_writes()),
        Trip::EdgeWrite(_) => (fixture.log.edge_writes().len(), fixture.log.edge_writes()),
        Trip::IndexPoints(_) => (fixture.log.index_writes().len(), fixture.log.index_writes()),
        Trip::Never => (0, Vec::new()),
    };
    (fixture.audit().await, reached, log)
}

/// The temporal pipeline writes through its own persistence stage, with its
/// own ledger call. Census it, then interrupt every one of its writes.
#[tokio::test]
async fn interrupting_any_temporal_write_preserves_i1() {
    let config = rollback_harness::temporal_config()
        .with_rollback_scope(cognee_cognify::RollbackScope::Nothing);

    let (audit, _, _) = interrupt_temporal(Trip::Never, &config).await;
    audit.assert_holds("after an uninterrupted temporal run");

    let census = Fixture::new(Trip::Never, &[TEMPORAL_TEXT]).await;
    census
        .run_with_llm(
            &config,
            Arc::new(rollback_harness::TemporalFixtureLlm::new()),
        )
        .await
        .expect("uninterrupted temporal run");
    let nodes = census.log.node_writes();
    let edges = census.log.edge_writes();
    let index = census.log.index_writes();
    println!("temporal node writes: {nodes:#?}");
    println!("temporal edge writes: {edges:#?}");
    println!("temporal index writes: {index:#?}");
    assert!(
        !nodes.is_empty() && !edges.is_empty() && !index.is_empty(),
        "the temporal fixture must reach all three kinds of store write"
    );

    for nth in 1..=nodes.len() {
        let (audit, reached, log) = interrupt_temporal(Trip::NodeWrite(nth), &config).await;
        assert!(
            reached >= nth,
            "temporal node write #{nth} unreached: {log:?}"
        );
        audit.assert_holds(&format!(
            "with temporal node write #{nth} failing ({log:?})"
        ));
    }
    for nth in 1..=edges.len() {
        let (audit, reached, log) = interrupt_temporal(Trip::EdgeWrite(nth), &config).await;
        assert!(
            reached >= nth,
            "temporal edge write #{nth} unreached: {log:?}"
        );
        audit.assert_holds(&format!(
            "with temporal edge write #{nth} failing ({log:?})"
        ));
    }
    for nth in 1..=index.len() {
        let (audit, reached, log) = interrupt_temporal(Trip::IndexPoints(nth), &config).await;
        assert!(
            reached >= nth,
            "temporal index write #{nth} unreached: {log:?}"
        );
        audit.assert_holds(&format!(
            "with temporal index write #{nth} failing ({log:?})"
        ));
    }
}

// ---------------------------------------------------------------------------
// The other pipelines that write to the stores
// ---------------------------------------------------------------------------

/// `memify()` writes `Triplet_text` vector points straight from the graph, and
/// `improve()`'s truth-subspace stage writes `TruthCentroid_vector` points —
/// neither writes an ownership row of its own.
///
/// This runs memify over the stores a real cognify run left behind and audits
/// the result. Whether memify's points are *incidentally* claimed depends
/// entirely on whether every edge it read has a cognify ownership row that
/// happens to yield the same `Triplet` id; nothing structural connects the two.
/// The assertion below therefore records what is actually true today, with the
/// list of anything unclaimed spelled out, so a change either way is visible.
#[tokio::test]
async fn memify_writes_are_only_incidentally_covered_by_the_cognify_ledger() {
    use cognee_cognify::memify::{MemifyConfig, memify};

    let fixture = Fixture::new(Trip::Never, &[FIXTURE_TEXT]).await;
    fixture.run(&full_config()).await.expect("cognify run");
    fixture
        .audit()
        .await
        .assert_holds("after the cognify run that seeds the graph");

    let before = fixture.audit().await.vector_points.len();

    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"));
    memify(
        Arc::clone(&fixture.graph_db) as Arc<dyn GraphDBTrait>,
        Arc::clone(&fixture.vector_db) as Arc<dyn VectorDB>,
        Arc::new(MockEmbeddingEngine::new(8)),
        thread_pool,
        Arc::clone(&fixture.db),
        Arc::new(cognee_database::NoopPipelineRunRepository::new()),
        Some(fixture.dataset_id),
        Some(fixture.owner_id),
        None,
        &MemifyConfig::default(),
    )
    .await
    .expect("memify over the cognify graph");

    let audit = fixture.audit().await;
    assert!(
        audit.vector_points.len() > before,
        "memify must have written vector points for this test to say anything"
    );

    let violations = audit.violations();
    println!(
        "memify left {} artifact(s) with no ownership row:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!("  - {}", v.0))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // memify writes no ownership rows at all: the `nodes`/`edges` ledger is
    // exactly what the cognify run left. Pinning that is the point — it is the
    // difference between "structurally enforced" and "happens to hold".
    let ledger_runs: HashSet<Option<Uuid>> = audit
        .ledger_nodes
        .iter()
        .map(|row| row.pipeline_run_id)
        .chain(audit.ledger_edges.iter().map(|row| row.pipeline_run_id))
        .collect();
    assert_eq!(
        ledger_runs.len(),
        1,
        "memify must not have added ownership rows under a run of its own; if \
         it now does, this test is the place to say so: {ledger_runs:?}"
    );
}

/// Write sites that the ownership ledger does **not** cover at all.
///
/// I1 is enforced structurally only inside cognify's own stages. Two other
/// places in the workspace write to the graph or the vector store on a live
/// dataset and write no ownership row of any kind:
///
/// * `FeedbackRetriever::get_completion`
///   (`crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs:376,386`)
///   — a `Feedback` node keyed on a fresh v4 uuid, plus its edges, written
///   during *search*.
/// * `upsert_centroids` (`crates/truth-subspace/src/centroids.rs:392`), reached
///   from `improve()`'s `build_truth_subspace` stage — `TruthCentroid_vector`
///   points keyed on `uuid5("TruthCentroid:{dataset}:{slot}")`.
///
/// Neither has a row, so no run-scoped sweep and no dataset delete can ever
/// reach them: `delete_vector_artifacts` and `delete_graph_artifacts` both
/// select exclusively from the `nodes`/`edges` ledger. This test asserts the
/// gap rather than asserting it away — when either site starts writing a row,
/// the audit stops reporting it and this test fails, which is the signal to
/// move it up into the sweeps above.
#[tokio::test]
async fn feedback_and_truth_centroid_writes_have_no_ownership_row() {
    use cognee_search::retrievers::FeedbackRetriever;
    use cognee_search::{SearchParams, SearchRetriever, SessionContext};
    use cognee_truth_subspace::{TruthCentroidPayload, upsert_centroids};

    let fixture = Fixture::new(Trip::Never, &[FIXTURE_TEXT]).await;
    fixture.run(&full_config()).await.expect("cognify run");
    fixture
        .audit()
        .await
        .assert_holds("before the extra writes");

    // 1. Search-time feedback: a graph node and no ledger row.
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(cognee_test_utils::MockLlm::new(vec![
        serde_json::json!({"sentiment": "positive", "score": 0.9}).to_string(),
    ]));
    let retriever = FeedbackRetriever::new(
        Arc::clone(&fixture.graph_db) as Arc<dyn GraphDBTrait>,
        llm,
        None,
        None,
    );
    retriever
        .get_completion(
            "that answer was great",
            None,
            &SessionContext::default(),
            &SearchParams::default(),
        )
        .await
        .expect("store feedback");

    // 2. improve()'s truth-subspace stage: a vector point and no ledger row.
    upsert_centroids(
        &*(Arc::clone(&fixture.vector_db) as Arc<dyn VectorDB>),
        &[TruthCentroidPayload {
            dataset_id: fixture.dataset_id.to_string(),
            slot: 0,
            count: 1,
            truth_epoch: 1,
            updated_at: 1,
            centroid: vec![0.0; 8],
            learning_ids: Vec::new(),
        }],
    )
    .await
    .expect("upsert centroids");

    let violations = fixture.audit().await.violations();
    let rendered: Vec<&str> = violations.iter().map(|v| v.0.as_str()).collect();
    println!("uncovered write sites produced:\n{rendered:#?}");

    assert!(
        rendered
            .iter()
            .any(|v| v.starts_with("graph node") && v.contains("has no ownership row")),
        "the search-time Feedback node must be reported unowned; if it is not, \
         the feedback path now writes a ledger row and belongs in the sweeps \
         above: {rendered:#?}"
    );
    assert!(
        rendered.iter().any(|v| v.contains("TruthCentroid_vector")),
        "the truth-subspace centroid point must be reported unowned: {rendered:#?}"
    );
}

// ---------------------------------------------------------------------------
// The DLT branch
// ---------------------------------------------------------------------------

/// A DLT-sourced item: `classify_documents` turns it into a `dlt_row`
/// document, which routes the run through `extract_dlt_fk_edges` — the
/// `SchemaTable` / `SchemaRelationship` nodes and the `is_row_of` / FK edges,
/// a write site the prose fixture never touches.
fn dlt_metadata() -> String {
    serde_json::json!({
        "source": "dlt",
        "table_name": "orders",
        "fk_references": [{
            "target_data_id": Uuid::new_v4().to_string(),
            "relationship_name": "belongs_to_customer",
            "target_table": "customers",
            "column": "customer_id"
        }]
    })
    .to_string()
}

/// Interrupt every write the DLT branch performs.
#[tokio::test]
async fn interrupting_any_dlt_write_preserves_i1() {
    let config = full_config().with_rollback_scope(cognee_cognify::RollbackScope::Nothing);
    let metadata = dlt_metadata();

    let census = Fixture::with_metadata(Trip::Never, &[FIXTURE_TEXT], Some(&metadata)).await;
    let _ = census.run(&config).await;
    let nodes = census.log.node_writes();
    let edges = census.log.edge_writes();
    let index = census.log.index_writes();
    println!("dlt node writes: {nodes:#?}");
    println!("dlt edge writes: {edges:#?}");
    println!("dlt index writes: {index:#?}");
    let census_audit = census.audit().await;
    census_audit.assert_holds("after an uninterrupted DLT run");
    let node_types: BTreeSet<&str> = census_audit
        .ledger_nodes
        .iter()
        .map(|row| row.node_type.as_str())
        .collect();
    println!("dlt ledger node types: {node_types:?}");
    assert!(
        node_types.contains("SchemaTable"),
        "the DLT fixture must reach extract_dlt_fk_edges — no SchemaTable row \
         means the run took the prose path and this sweep is vacuous: \
         {node_types:?}"
    );

    for nth in 1..=nodes.len() {
        let fixture =
            Fixture::with_metadata(Trip::NodeWrite(nth), &[FIXTURE_TEXT], Some(&metadata)).await;
        let _ = fixture.run(&config).await;
        let log = fixture.log.node_writes();
        assert!(log.len() >= nth, "dlt node write #{nth} unreached: {log:?}");
        fixture
            .audit()
            .await
            .assert_holds(&format!("with DLT node write #{nth} failing ({log:?})"));
    }
    for nth in 1..=edges.len() {
        let fixture =
            Fixture::with_metadata(Trip::EdgeWrite(nth), &[FIXTURE_TEXT], Some(&metadata)).await;
        let _ = fixture.run(&config).await;
        let log = fixture.log.edge_writes();
        assert!(log.len() >= nth, "dlt edge write #{nth} unreached: {log:?}");
        fixture
            .audit()
            .await
            .assert_holds(&format!("with DLT edge write #{nth} failing ({log:?})"));
    }
    for nth in 1..=index.len() {
        let fixture =
            Fixture::with_metadata(Trip::IndexPoints(nth), &[FIXTURE_TEXT], Some(&metadata)).await;
        let _ = fixture.run(&config).await;
        let log = fixture.log.index_writes();
        assert!(
            log.len() >= nth,
            "dlt index write #{nth} unreached: {log:?}"
        );
        fixture
            .audit()
            .await
            .assert_holds(&format!("with DLT index write #{nth} failing ({log:?})"));
    }
}

// ---------------------------------------------------------------------------
// The URL / WebPage branch
// ---------------------------------------------------------------------------

/// A URL-sourced item, which routes the run through `create_web_page_nodes` —
/// `WebPage` / `WebSite` nodes plus `PART_OF` and `SOURCED_FROM` edges. These
/// are `add_nodes_raw` JSON blobs rather than DataPoints, so the ordinary
/// `upsert_provenance` pass never sees them; they have a ledger call of their
/// own, and this is what interrupts it.
fn url_metadata() -> String {
    serde_json::json!({
        "source": "url",
        "final_url": "https://example.com/alice",
        "title": "Alice at Acme"
    })
    .to_string()
}

#[tokio::test]
async fn interrupting_any_web_page_write_preserves_i1() {
    let config = full_config()
        .with_web_page_nodes(true)
        .with_rollback_scope(cognee_cognify::RollbackScope::Nothing);
    let metadata = url_metadata();

    let census = Fixture::with_metadata(Trip::Never, &[FIXTURE_TEXT], Some(&metadata)).await;
    let _ = census.run(&config).await;
    let nodes = census.log.node_writes();
    let edges = census.log.edge_writes();
    println!("web node writes: {nodes:#?}");
    println!("web edge writes: {edges:#?}");
    let census_audit = census.audit().await;
    census_audit.assert_holds("after an uninterrupted URL run");
    let node_types: BTreeSet<&str> = census_audit
        .ledger_nodes
        .iter()
        .map(|row| row.node_type.as_str())
        .collect();
    assert!(
        node_types.contains("WebPage") && node_types.contains("WebSite"),
        "the URL fixture must reach create_web_page_nodes: {node_types:?}"
    );

    for nth in 1..=nodes.len() {
        let fixture =
            Fixture::with_metadata(Trip::NodeWrite(nth), &[FIXTURE_TEXT], Some(&metadata)).await;
        let _ = fixture.run(&config).await;
        let log = fixture.log.node_writes();
        assert!(log.len() >= nth, "web node write #{nth} unreached: {log:?}");
        fixture
            .audit()
            .await
            .assert_holds(&format!("with WebPage node write #{nth} failing ({log:?})"));
    }
    for nth in 1..=edges.len() {
        let fixture =
            Fixture::with_metadata(Trip::EdgeWrite(nth), &[FIXTURE_TEXT], Some(&metadata)).await;
        let _ = fixture.run(&config).await;
        let log = fixture.log.edge_writes();
        assert!(log.len() >= nth, "web edge write #{nth} unreached: {log:?}");
        fixture
            .audit()
            .await
            .assert_holds(&format!("with WebPage edge write #{nth} failing ({log:?})"));
    }
}
