#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Stage 1 integration tests — `apply_feedback_weights_pipeline`.
#![cfg(feature = "testing")]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_cognify::memify::feedback_weights::{
    EDGE_ID_DELIMITER, FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY, FEEDBACK_WEIGHTS_APPLIED_KEY,
    FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY, FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY,
    FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY, FEEDBACK_WEIGHTS_ATTEMPTS_KEY,
    FEEDBACK_WEIGHTS_MAX_ATTEMPTS, FEEDBACK_WEIGHTS_PRUNED_IDS_KEY, FeedbackApplyResult,
    apply_feedback_weights_pipeline,
};
use cognee_graph::{
    EdgeData, EdgeKey, GraphDBResult, GraphDBTrait, GraphNode, MockGraphDB, NodeData,
};
use cognee_session::{
    FsSessionStore, SessionManager, SessionQAUpdate, SessionStore, UsedGraphElementIds,
};
use serde_json::{Value, json};
use uuid::Uuid;

async fn add_node_with_weight(db: &MockGraphDB, id: &str, weight: f64) {
    db.add_node_raw(json!({
        "id": id,
        "name": id,
        "type": "Entity",
        "feedback_weight": weight,
    }))
    .await
    .unwrap();
}

fn make_store_and_manager() -> (
    tempfile::TempDir,
    Arc<dyn SessionStore>,
    Arc<SessionManager>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(dir.path()));
    let manager = Arc::new(SessionManager::new(Arc::clone(&store)));
    (dir, store, manager)
}

async fn create_qa_with_feedback(
    store: &dyn SessionStore,
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    node_ids: Vec<String>,
    edge_ids: Vec<String>,
    feedback_score: i32,
) -> String {
    let qa_id = store
        .create_qa_entry(session_id, Some(user_id), "Q", "A", None)
        .await
        .unwrap();
    manager
        .update_qa(
            Some(session_id),
            Some(user_id),
            &qa_id,
            SessionQAUpdate {
                feedback_score: Some(Some(feedback_score)),
                used_graph_element_ids: Some(Some(UsedGraphElementIds { node_ids, edge_ids })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    qa_id
}

#[tokio::test]
async fn feedback_high_score_pushes_weights_up() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-high";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;
    add_node_with_weight(&graph, "n2", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string(), "n2".to_string()],
        vec![],
        5,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 1);
    assert_eq!(res.applied, 1);
    assert_eq!(res.skipped, 0);

    // Node weights should have moved towards 1.0. stream_update_weight(0.5, 1.0, 0.1) = 0.55
    let w1 = graph
        .get_node_feedback_weights(&["n1".to_string(), "n2".to_string()])
        .await
        .unwrap();
    assert_eq!(w1.len(), 2);
    for (_, v) in w1 {
        assert!((v - 0.55).abs() < 1e-9, "expected 0.55, got {v}");
    }
}

#[tokio::test]
async fn feedback_low_score_pushes_weights_down() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-low";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![],
        1,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.applied, 1);

    let w = graph
        .get_node_feedback_weights(&["n1".to_string()])
        .await
        .unwrap();
    let v = *w.get("n1").unwrap();
    // stream_update_weight(0.5, 0.0, 0.1) = 0.45
    assert!((v - 0.45).abs() < 1e-9, "expected 0.45, got {v}");
}

#[tokio::test]
async fn feedback_idempotent_second_run_skips() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-idem";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![],
        5,
    )
    .await;

    let first = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(first.processed, 1);

    // Verify the memify_metadata was set.
    let entries = store
        .get_all_qa_entries(session_id, Some(&user_id))
        .await
        .unwrap();
    let meta = entries[0].memify_metadata.as_ref().expect("metadata set");
    assert_eq!(
        meta.get(FEEDBACK_WEIGHTS_APPLIED_KEY),
        Some(&serde_json::Value::Bool(true))
    );

    // Second run should skip.
    let second = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(second.processed, 0);
    assert_eq!(second.skipped, 1);
}

#[tokio::test]
async fn feedback_skips_entries_without_graph_ids() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-noids";

    let graph = MockGraphDB::new();

    // Entry without used_graph_element_ids (not eligible)
    let _qa_id = store
        .create_qa_entry(session_id, Some(&user_id), "Q", "A", None)
        .await
        .unwrap();
    // Note: we left feedback_score and used_graph_element_ids as None
    // via create_qa_entry.

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 0);
    assert_eq!(res.skipped, 1);
}

#[tokio::test]
async fn feedback_skips_entries_with_invalid_score() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-invalid";

    let graph = MockGraphDB::new();

    // Score 0 is invalid (must be 1-5)
    let qa_id = store
        .create_qa_entry(session_id, Some(&user_id), "Q", "A", None)
        .await
        .unwrap();
    manager
        .update_qa(
            Some(session_id),
            Some(&user_id),
            &qa_id,
            SessionQAUpdate {
                feedback_score: Some(Some(0)),
                used_graph_element_ids: Some(Some(UsedGraphElementIds {
                    node_ids: vec!["n1".into()],
                    edge_ids: vec![],
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 0);
    assert_eq!(res.skipped, 1);
}

#[tokio::test]
async fn feedback_rejects_bad_alpha() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let graph = MockGraphDB::new();

    let r1 = apply_feedback_weights_pipeline(
        &["s".to_string()],
        owner,
        0.0,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await;
    assert!(r1.is_err());

    let r2 = apply_feedback_weights_pipeline(
        &["s".to_string()],
        owner,
        1.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await;
    assert!(r2.is_err());
}

#[tokio::test]
async fn feedback_handles_edge_ids_on_mock() {
    // MockGraphDB's default `get_edge_feedback_weights` returns an empty
    // map and the default `update_edge_property` is a warning no-op — so
    // the pipeline should report the entry as processed but not applied
    // when there are edge-only ids (node_success true, edge_success false
    // because the in-map reading returns nothing).
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-edge";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    let edge_id = format!("src{EDGE_ID_DELIMITER}tgt{EDGE_ID_DELIMITER}rel");
    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![edge_id],
        5,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 1);
    // Edge update returns empty weights from MockGraphDB default, so
    // applied=0.
    assert_eq!(res.applied, 0);
}

/// A graph double that delegates every required `GraphDBTrait` method to an
/// inner [`MockGraphDB`] but adds real in-memory edge `feedback_weight`
/// storage by overriding `get_edge_feedback_weights` /
/// `set_edge_feedback_weights`. This lets us exercise the *success* edge path
/// that the plain mock cannot (its default edge-weight read returns an empty
/// map, forcing `applied == 0`). Delegation shape mirrors the
/// `ScriptedGraph` double in `build_truth_subspace.rs`.
struct EdgeStoreGraph {
    inner: MockGraphDB,
    edge_weights: Mutex<HashMap<EdgeKey, f64>>,
    /// Edge writes to these keys report `false` (the graph has the edge but
    /// the write did not succeed).
    failing_edge_writes: Mutex<HashSet<EdgeKey>>,
    /// Every edge read raises.
    edge_reads_raise: AtomicBool,
    /// Every node write batch, in call order.
    node_write_log: Mutex<Vec<HashMap<String, f64>>>,
    /// Every edge write batch, in call order.
    edge_write_log: Mutex<Vec<HashMap<EdgeKey, f64>>>,
}

impl EdgeStoreGraph {
    fn new() -> Self {
        Self {
            inner: MockGraphDB::new(),
            edge_weights: Mutex::new(HashMap::new()),
            failing_edge_writes: Mutex::new(HashSet::new()),
            edge_reads_raise: AtomicBool::new(false),
            node_write_log: Mutex::new(Vec::new()),
            edge_write_log: Mutex::new(Vec::new()),
        }
    }

    fn seed_edge(&self, key: EdgeKey, weight: f64) {
        // lock poison is unrecoverable
        self.edge_weights.lock().unwrap().insert(key, weight);
    }

    fn edge_weight(&self, key: &EdgeKey) -> Option<f64> {
        // lock poison is unrecoverable
        self.edge_weights.lock().unwrap().get(key).copied()
    }
}

#[async_trait]
impl GraphDBTrait for EdgeStoreGraph {
    async fn get_edge_feedback_weights(
        &self,
        edge_keys: &[EdgeKey],
    ) -> GraphDBResult<HashMap<EdgeKey, f64>> {
        if self.edge_reads_raise.load(Ordering::SeqCst) {
            return Err(cognee_graph::GraphDBError::QueryError(
                "edge read failed".to_string(),
            ));
        }
        // lock poison is unrecoverable
        let store = self.edge_weights.lock().unwrap();
        let mut out = HashMap::new();
        for k in edge_keys {
            if let Some(w) = store.get(k) {
                out.insert(k.clone(), *w);
            }
        }
        Ok(out)
    }

    async fn set_edge_feedback_weights(
        &self,
        updates: &HashMap<EdgeKey, f64>,
    ) -> GraphDBResult<HashMap<EdgeKey, bool>> {
        // lock poison is unrecoverable
        self.edge_write_log.lock().unwrap().push(updates.clone());
        // lock poison is unrecoverable
        let failing = self.failing_edge_writes.lock().unwrap();
        // lock poison is unrecoverable
        let mut store = self.edge_weights.lock().unwrap();
        let mut out = HashMap::new();
        for (k, w) in updates {
            if failing.contains(k) {
                out.insert(k.clone(), false);
                continue;
            }
            store.insert(k.clone(), *w);
            out.insert(k.clone(), true);
        }
        Ok(out)
    }

    async fn set_node_feedback_weights(
        &self,
        updates: &HashMap<String, f64>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        // lock poison is unrecoverable
        self.node_write_log.lock().unwrap().push(updates.clone());
        self.inner.set_node_feedback_weights(updates).await
    }

    // --- everything below just delegates to the inner MockGraphDB ---
    async fn get_filtered_graph_data(
        &self,
        filters: &HashMap<Cow<'static, str>, Vec<Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_filtered_graph_data(filters).await
    }
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
        self.inner.add_node_raw(node).await
    }
    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        self.inner.add_nodes_raw(nodes).await
    }
    async fn delete_node(&self, id: &str) -> GraphDBResult<()> {
        self.inner.delete_node(id).await
    }
    async fn delete_nodes(&self, ids: &[String]) -> GraphDBResult<()> {
        self.inner.delete_nodes(ids).await
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
        self.inner.add_edge(s, t, r, p).await
    }
    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        self.inner.add_edges(edges).await
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
    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_graph_data().await
    }
    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
        self.inner.get_graph_metrics(include_optional).await
    }
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        op: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner
            .get_nodeset_subgraph(node_type, node_names, op)
            .await
    }
    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_neighborhood(node_ids, depth).await
    }
}

#[tokio::test]
async fn feedback_edge_success_updates_weight_and_marks_applied() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-edge-success";

    // Graph double with real edge-weight storage, seeded at 0.5.
    let graph = EdgeStoreGraph::new();
    let edge_key = ("src".to_string(), "tgt".to_string(), "rel".to_string());
    graph.seed_edge(edge_key.clone(), 0.5);

    let edge_id = format!("src{EDGE_ID_DELIMITER}tgt{EDGE_ID_DELIMITER}rel");
    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec![],
        vec![edge_id],
        5,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();

    assert_eq!(res.processed, 1);
    assert_eq!(res.applied, 1);
    assert_eq!(res.skipped, 0);

    // Score 5 -> normalized 1.0; stream_update_weight(0.5, 1.0, 0.1) = 0.55.
    let w = graph.edge_weight(&edge_key).expect("edge weight present");
    assert!((w - 0.55).abs() < 1e-9, "expected 0.55, got {w}");

    // The QA entry is marked applied (success=true) so a second run skips it.
    let entries = store
        .get_all_qa_entries(session_id, Some(&user_id))
        .await
        .unwrap();
    let meta = entries[0].memify_metadata.as_ref().expect("metadata set");
    assert_eq!(
        meta.get(FEEDBACK_WEIGHTS_APPLIED_KEY),
        Some(&serde_json::Value::Bool(true))
    );
}

// ---------------------------------------------------------------------------
// Row bookkeeping (Python `test_apply_feedback_weights.py`): implicit ratings,
// retries, the attempt cap, re-rating, pruned ids and graph errors.
// ---------------------------------------------------------------------------

const SESSION: &str = "sess-rows";

fn edge_key() -> EdgeKey {
    ("src".to_string(), "tgt".to_string(), "rel".to_string())
}

fn edge_id() -> String {
    format!("src{EDGE_ID_DELIMITER}tgt{EDGE_ID_DELIMITER}rel")
}

struct Rows {
    _dir: tempfile::TempDir,
    store: Arc<dyn SessionStore>,
    manager: Arc<SessionManager>,
    owner: Uuid,
    user_id: String,
}

impl Rows {
    fn new() -> Self {
        let (dir, store, manager) = make_store_and_manager();
        let owner = Uuid::new_v4();
        Self {
            _dir: dir,
            store,
            manager,
            owner,
            user_id: owner.to_string(),
        }
    }

    /// A QA entry using `node_ids` and `edge_ids`, with no rating yet.
    async fn qa(&self, node_ids: &[&str], edge_ids: &[String]) -> String {
        let qa_id = self
            .store
            .create_qa_entry(SESSION, Some(&self.user_id), "Q", "A", None)
            .await
            .unwrap();
        self.manager
            .update_qa(
                Some(SESSION),
                Some(&self.user_id),
                &qa_id,
                SessionQAUpdate {
                    used_graph_element_ids: Some(Some(UsedGraphElementIds {
                        node_ids: node_ids.iter().map(|id| id.to_string()).collect(),
                        edge_ids: edge_ids.to_vec(),
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        qa_id
    }

    /// The user's own rating, resetting the done flag as `add_feedback` does.
    async fn rate(&self, qa_id: &str, score: i32) {
        self.manager
            .add_feedback(
                Some(SESSION),
                Some(&self.user_id),
                qa_id,
                Some("text"),
                Some(score),
            )
            .await
            .unwrap();
    }

    /// The feedback row the next turn's analysis stores for `qa_id`.
    async fn implicit_rating(&self, qa_id: &str, rating: i32) {
        let row = json!({
            "id": Uuid::new_v4().to_string(),
            "kind": "feedback",
            "raw_text": "thanks",
            "referenced_qa_ids": [qa_id],
            "referenced_qa_rating": rating,
        });
        self.manager
            .create_session_context_entry(
                Some(SESSION),
                Some(&self.user_id),
                row.as_object().unwrap().clone(),
            )
            .await
            .unwrap();
    }

    async fn run(&self, graph: &dyn GraphDBTrait, alpha: f64) -> FeedbackApplyResult {
        apply_feedback_weights_pipeline(
            &[SESSION.to_string()],
            self.owner,
            alpha,
            graph,
            Arc::clone(&self.store),
            Arc::clone(&self.manager),
        )
        .await
        .unwrap()
    }

    async fn meta(&self, qa_id: &str) -> HashMap<String, Value> {
        self.store
            .get_all_qa_entries(SESSION, Some(&self.user_id))
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.id.to_string() == qa_id)
            .and_then(|entry| entry.memify_metadata)
            .unwrap_or_default()
    }
}

async fn node_weight(graph: &dyn GraphDBTrait, id: &str) -> f64 {
    graph
        .get_node_feedback_weights(&[id.to_string()])
        .await
        .unwrap()[id]
}

/// Nodes `n1` and edge `src|||tgt|||rel` at 0.5.
async fn graph_with_n1_and_e1() -> EdgeStoreGraph {
    let graph = EdgeStoreGraph::new();
    add_node_with_weight(&graph.inner, "n1", 0.5).await;
    graph.seed_edge(edge_key(), 0.5);
    graph
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
}

#[tokio::test]
async fn implicit_rating_moves_weights_at_half_alpha() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.implicit_rating(&qa, 5).await;

    let result = rows.run(&graph, 0.1).await;

    assert_eq!((result.processed, result.applied), (1, 1));
    approx(node_weight(&graph, "n1").await, 0.525);
    approx(graph.edge_weight(&edge_key()).unwrap(), 0.525);
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY], json!("implicit"));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY], json!(5));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
}

#[tokio::test]
async fn explicit_rating_moves_weights_at_full_alpha_and_beats_implicit() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[]).await;
    rows.implicit_rating(&qa, 5).await;
    rows.rate(&qa, 1).await;

    rows.run(&graph, 0.1).await;

    // Explicit score 1 at full alpha: 0.5 + 0.1 * (0 - 0.5).
    approx(node_weight(&graph, "n1").await, 0.45);
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY], json!("explicit"));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY], json!(1));
}

#[tokio::test]
async fn explicit_rerating_after_an_implicit_one_reapplies_at_full_alpha() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[]).await;
    rows.implicit_rating(&qa, 5).await;

    rows.run(&graph, 0.1).await;
    approx(node_weight(&graph, "n1").await, 0.525); // half alpha

    // Same score, now explicit: new information, so it re-applies.
    rows.rate(&qa, 5).await;
    let second = rows.run(&graph, 0.1).await;

    assert_eq!((second.processed, second.applied), (1, 1));
    approx(node_weight(&graph, "n1").await, 0.5725); // full alpha from 0.525
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY], json!("explicit"));
    // Restarted: this is the first attempt of the new rating.
    assert_eq!(meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY], json!(1));
}

#[tokio::test]
async fn rerating_with_another_score_starts_the_row_over() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;
    rows.run(&graph, 0.1).await;
    approx(node_weight(&graph, "n1").await, 0.55);

    rows.rate(&qa, 1).await;
    let second = rows.run(&graph, 0.1).await;

    assert_eq!(second.applied, 1);
    approx(node_weight(&graph, "n1").await, 0.495);
    approx(graph.edge_weight(&edge_key()).unwrap(), 0.495);
    assert_eq!(
        rows.meta(&qa).await[FEEDBACK_WEIGHTS_ATTEMPTS_KEY],
        json!(1)
    );
}

#[tokio::test]
async fn the_same_rating_with_a_reset_flag_moves_nothing_again() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;
    rows.run(&graph, 0.1).await;

    rows.rate(&qa, 5).await; // resets the done flag, same score and source
    let second = rows.run(&graph, 0.1).await;

    assert_eq!((second.processed, second.applied), (1, 1));
    assert_eq!(second.elements_moved, 0);
    approx(node_weight(&graph, "n1").await, 0.55);
    assert_eq!(graph.node_write_log.lock().unwrap().len(), 1);
    assert_eq!(
        rows.meta(&qa).await[FEEDBACK_WEIGHTS_APPLIED_KEY],
        json!(true)
    );
}

#[tokio::test]
async fn a_retry_moves_only_the_ids_that_did_not_move() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    graph.failing_edge_writes.lock().unwrap().insert(edge_key());
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;

    let first = rows.run(&graph, 0.1).await;
    assert_eq!(
        (first.processed, first.applied, first.elements_failed),
        (1, 0, 1)
    );
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(false));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY], json!(["n1"]));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY], json!([]));
    assert_eq!(meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY], json!(1));

    graph.failing_edge_writes.lock().unwrap().clear();
    let second = rows.run(&graph, 0.1).await;
    assert_eq!((second.processed, second.applied), (1, 1));
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
    assert_eq!(
        meta[FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY],
        json!([edge_id()])
    );
    assert_eq!(meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY], json!(2));
    // n1 moved once across both runs; the edge moved once, on the retry.
    approx(node_weight(&graph, "n1").await, 0.55);
    approx(graph.edge_weight(&edge_key()).unwrap(), 0.55);
    assert_eq!(graph.node_write_log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_row_is_done_after_the_attempt_cap() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    graph.failing_edge_writes.lock().unwrap().insert(edge_key());
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;

    let mut processed = Vec::new();
    for _ in 0..=FEEDBACK_WEIGHTS_MAX_ATTEMPTS {
        let result = rows.run(&graph, 0.1).await;
        assert_eq!(result.applied, 0);
        processed.push(result.processed);
    }

    assert_eq!(processed, vec![1, 1, 1, 0], "the capped row is not retried");
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true)); // sealed by the cap
    assert_eq!(
        meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY],
        json!(FEEDBACK_WEIGHTS_MAX_ATTEMPTS)
    );
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY], json!([]));
    approx(node_weight(&graph, "n1").await, 0.55);
    assert_eq!(graph.node_write_log.lock().unwrap().len(), 1);
    assert_eq!(
        graph.edge_write_log.lock().unwrap().len(),
        FEEDBACK_WEIGHTS_MAX_ATTEMPTS as usize
    );
}

#[tokio::test]
async fn pruned_ids_keep_the_row_pending_until_the_cap() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1", "n_deleted"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;

    let mut results = Vec::new();
    for _ in 0..=FEEDBACK_WEIGHTS_MAX_ATTEMPTS {
        results.push(rows.run(&graph, 0.1).await);
    }

    // Each surviving element moved exactly once.
    approx(node_weight(&graph, "n1").await, 0.55);
    approx(graph.edge_weight(&edge_key()).unwrap(), 0.55);
    assert_eq!(graph.node_write_log.lock().unwrap().len(), 1);
    assert_eq!(graph.edge_write_log.lock().unwrap().len(), 1);
    assert!(results.iter().all(|r| r.applied == 0));
    let processed: Vec<usize> = results.iter().map(|r| r.processed).collect();
    let skipped: Vec<usize> = results.iter().map(|r| r.skipped).collect();
    assert_eq!(processed, vec![1, 1, 1, 0]);
    assert_eq!(skipped, vec![0, 0, 0, 1]);
    assert_eq!(results[0].elements_pruned, 1);
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY], json!(["n1"]));
    assert_eq!(meta[FEEDBACK_WEIGHTS_PRUNED_IDS_KEY], json!(["n_deleted"]));
}

#[tokio::test]
async fn ids_pruned_in_one_graph_are_consumed_by_the_graph_that_has_them() {
    let rows = Rows::new();
    let graph_a = EdgeStoreGraph::new(); // has n1, no edge
    add_node_with_weight(&graph_a.inner, "n1", 0.5).await;
    let graph_b = EdgeStoreGraph::new(); // has the edge, no n1
    graph_b.seed_edge(edge_key(), 0.5);
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;

    let first = rows.run(&graph_a, 0.1).await;
    assert_eq!(first.applied, 0);
    assert_eq!(
        rows.meta(&qa).await[FEEDBACK_WEIGHTS_APPLIED_KEY],
        json!(false)
    );

    let second = rows.run(&graph_b, 0.1).await;
    assert_eq!(second.applied, 1);
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
    assert_eq!(
        meta[FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY],
        json!([edge_id()])
    );
    approx(graph_b.edge_weight(&edge_key()).unwrap(), 0.55);
    // n1 moved once, in graph A; B never re-applied it.
    approx(node_weight(&graph_a, "n1").await, 0.55);
    assert!(graph_b.node_write_log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_graph_error_persists_the_bookkeeping_and_the_batch_continues() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    add_node_with_weight(&graph.inner, "n2", 0.5).await;
    graph.edge_reads_raise.store(true, Ordering::SeqCst);
    let q1 = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&q1, 5).await;
    // q2 has no edge ids, so the raising edge read never runs for it.
    let q2 = rows.qa(&["n2"], &[]).await;
    rows.rate(&q2, 5).await;

    let result = rows.run(&graph, 0.1).await;

    // q1: the node write is persisted despite the raise; attempt counted; not done.
    let meta = rows.meta(&q1).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(false));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY], json!(["n1"]));
    assert_eq!(meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY], json!(1));
    approx(node_weight(&graph, "n1").await, 0.55);
    // q2 still processed: the batch survived q1's failure.
    assert_eq!(
        rows.meta(&q2).await[FEEDBACK_WEIGHTS_APPLIED_KEY],
        json!(true)
    );
    assert_eq!((result.processed, result.applied), (1, 1));
    assert_eq!((result.skipped, result.errored), (1, 1));

    // A retry of q1 does not move n1 again; the unattempted edge keeps failing.
    rows.run(&graph, 0.1).await;
    approx(node_weight(&graph, "n1").await, 0.55);
    assert_eq!(graph.node_write_log.lock().unwrap().len(), 2); // n1 once, n2 once
    assert_eq!(
        rows.meta(&q1).await[FEEDBACK_WEIGHTS_ATTEMPTS_KEY],
        json!(2)
    );
}

#[tokio::test]
async fn repeated_graph_errors_trip_the_attempt_cap() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    graph.edge_reads_raise.store(true, Ordering::SeqCst);
    let qa = rows.qa(&["n1"], &[edge_id()]).await;
    rows.rate(&qa, 5).await;

    for _ in 0..=FEEDBACK_WEIGHTS_MAX_ATTEMPTS {
        rows.run(&graph, 0.1).await;
    }

    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true)); // sealed by the cap
    assert_eq!(
        meta[FEEDBACK_WEIGHTS_ATTEMPTS_KEY],
        json!(FEEDBACK_WEIGHTS_MAX_ATTEMPTS)
    );
}

#[tokio::test]
async fn a_rated_row_without_ids_is_marked_done_and_touches_no_weights() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&[], &[]).await;
    rows.rate(&qa, 5).await;

    let result = rows.run(&graph, 0.1).await;

    assert_eq!(
        (result.processed, result.applied, result.skipped),
        (0, 0, 1)
    );
    assert_eq!(result.done, 1);
    // Only the done marker is written, and `true`: the row is never rescanned.
    let meta = rows.meta(&qa).await;
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
    assert!(!meta.contains_key(FEEDBACK_WEIGHTS_ATTEMPTS_KEY));
    approx(node_weight(&graph, "n1").await, 0.5);
    approx(graph.edge_weight(&edge_key()).unwrap(), 0.5);
}

#[tokio::test]
async fn a_write_only_touches_this_stages_keys() {
    let rows = Rows::new();
    let graph = graph_with_n1_and_e1().await;
    let qa = rows.qa(&["n1"], &[]).await;
    rows.rate(&qa, 5).await;
    // Another stage's flag, set after the row was scanned, must survive.
    rows.manager
        .update_qa(
            Some(SESSION),
            Some(&rows.user_id),
            &qa,
            SessionQAUpdate {
                memify_metadata: Some(Some(HashMap::from([(
                    "preference_weights_applied".to_string(),
                    json!(true),
                )]))),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    rows.run(&graph, 0.1).await;

    let meta = rows.meta(&qa).await;
    assert_eq!(meta["preference_weights_applied"], json!(true));
    assert_eq!(meta[FEEDBACK_WEIGHTS_APPLIED_KEY], json!(true));
}
