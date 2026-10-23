//! Stage 2d of `improve()` — build/refresh centroid-slot truth coordinates for
//! a dataset's session learnings, then project every `DocumentChunk` node onto
//! those slots and persist the per-node alignment.
//!
//! Ported from Python `cognee/modules/truth_subspace/build.py:108-299`
//! (`build.py:1-6`). This is the orchestration layer that *drives* the pure
//! math (`cognee-truth-subspace::align`/`centroids`) and I/O wiring
//! (`load_centroids`/`upsert_centroids`, `set_node_truth_state`) landed by
//! earlier Phase-2 tasks.
//!
//! # Infallible by construction
//!
//! Like the Python original (`-> dict`), [`build_truth_subspace`] never returns
//! a `Result` and never aborts `improve()`: every internal fallible step fails
//! open to a partial [`TruthSubspaceResult`] (see the six documented return
//! shapes below). The stage is default-off end-to-end, so when the caller does
//! not opt in this code never runs.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, NodeTruthState};
use cognee_truth_subspace::centroids::pad_coords;
use cognee_truth_subspace::{
    TRUTH_NODE_SET, TruthCentroidPayload, align, build_centroids_from_learning_vectors,
    centroids_changed, extend_centroids_with_learning_vectors, learning_id, load_centroids,
    truth_session_node_set, upsert_centroids,
};
use cognee_vector::VectorDB;
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

/// Graph node type of a chunk (`DocumentChunk.__name__`, `build.py:78,230`).
const DOCUMENT_CHUNK_TYPE: &str = "DocumentChunk";
/// Graph node type of a node set (`NodeSet.__name__`, `build.py:66-67`).
const NODE_SET_TYPE: &str = "NodeSet";

/// Node-text embedding batch size — keeps memory bounded on large subgraphs
/// (`build.py:39`). Applies only to chunk-node embedding; learning-statement
/// embedding is always a single call (`build.py:157`).
pub const NODE_EMBED_BATCH_SIZE: usize = 64;

/// Summary of a [`build_truth_subspace`] run.
///
/// This is the Rust analog of Python's return `dict` (`build.py:119,294-299`);
/// [`TruthSubspaceResult::default`] is Python's `empty_result` (`build.py:119`).
/// It is log-only — `improve()` never merges it into its own return value,
/// matching Python (`improve.py:213-219`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TruthSubspaceResult {
    /// Number of centroid slots (`anchors`, `build.py:295`).
    pub anchors: usize,
    /// Number of `DocumentChunk` nodes whose truth state was persisted.
    pub nodes_scored: usize,
    /// Stable sha256 signature of the ordered learning ids (`build.py:153`).
    pub signature: String,
    /// Centroid epoch the alignments were computed against.
    pub truth_epoch: i64,
}

/// Build/refresh centroid slots and chunk coordinates for `dataset_id`.
///
/// Ports `build.py:108-299`. Infallible — see the module docs and the six
/// return shapes below. `session_ids` empty is valid input (mirrors Python's
/// `Optional[...] = None`) and takes the from-scratch rebuild branch; this
/// task's only call site always passes non-empty (gated by `has_sessions`).
///
/// `dataset_id` replaces Python's `_resolve_dataset` (`build.py:102-105,121`):
/// the caller (Stage 2d) resolves the dataset exactly as Stage 4 does, so this
/// function skips Python's per-call ACL re-check (Rust's owner/tenant scoping
/// has no such layer here, matching the sibling stage functions).
///
/// # Return shapes (parity-exact with `build.py`)
///
/// 1. dataset not found → `default()` — handled by the CALLER (this fn takes a
///    resolved `dataset_id`).
/// 2. no learning statements → `default()` (`build.py:132-134`).
/// 3. learning-embed failure → `{existing.len(), 0, signature, previous_epoch}`
///    (`build.py:158-165`).
/// 4. rebuilt centroids empty → `default()`, **dropping the already-computed
///    `signature`** — a preserved Python quirk (`build.py:188-190`).
/// 5. centroid-upsert / node-load / no-node-texts / no-scored / persist failure
///    → `{centroids.len(), 0, signature, current_epoch}` (five call sites, one
///    shape; `build.py:196-283`).
/// 6. full success → `{centroids.len(), nodes_scored, signature, current_epoch}`
///    (`build.py:294-299`).
///
/// Runs in `dataset_id`'s [`cognee_models::dataset_scope`] (per-dataset
/// storage).
pub async fn build_truth_subspace(
    dataset_id: Uuid,
    session_ids: &[String],
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    k: usize,
) -> TruthSubspaceResult {
    cognee_models::dataset_scope::with_dataset(
        dataset_id,
        build_truth_subspace_in_scope(
            dataset_id,
            session_ids,
            graph_db,
            vector_db,
            embedding_engine,
            k,
        ),
    )
    .await
}

async fn build_truth_subspace_in_scope(
    dataset_id: Uuid,
    session_ids: &[String],
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    k: usize,
) -> TruthSubspaceResult {
    let dataset_id_str = dataset_id.to_string();

    // Step 1: accepted learning statements from the session_learnings node set.
    let statements = fetch_learning_statements(graph_db.as_ref(), session_ids).await;

    // Shape 2: no learnings -> nothing to build.
    if statements.is_empty() {
        info!("truth_subspace: no learnings found, nothing to build");
        return TruthSubspaceResult::default();
    }

    // Existing centroids (fail-open to empty; `build.py:136-140`).
    let existing_centroids = load_centroids(vector_db.as_ref(), &dataset_id_str, k)
        .await
        .unwrap_or_default();
    let previous_epoch = existing_centroids
        .iter()
        .map(|c| c.truth_epoch)
        .max()
        .unwrap_or(0);

    // Dedupe (LAST-wins) + sort by learning id via a BTreeMap — matches Python's
    // dict comprehension + `sorted(key=item[0])` (`build.py:143-152`). This is
    // intentionally NOT `unique_learning_vectors` (first-wins).
    let mut learning_map: BTreeMap<String, String> = BTreeMap::new();
    for statement in &statements {
        if statement.trim().is_empty() {
            continue;
        }
        learning_map.insert(learning_id(statement), statement.clone());
    }
    let learning_ids: Vec<String> = learning_map.keys().cloned().collect();
    let learning_texts: Vec<String> = learning_map.values().cloned().collect();
    let signature = align::stable_signature(&learning_ids);

    // Embed learning texts in ONE call, not batched (`build.py:155-165`).
    let learning_refs: Vec<&str> = learning_texts.iter().map(String::as_str).collect();
    let learning_vecs = match embedding_engine.embed(&learning_refs).await {
        Ok(vecs) => vecs,
        Err(error) => {
            warn!("truth_subspace: learning embedding failed open: {error}");
            // Shape 3.
            return TruthSubspaceResult {
                anchors: existing_centroids.len(),
                nodes_scored: 0,
                signature,
                truth_epoch: previous_epoch,
            };
        }
    };
    let updated_at = Utc::now().timestamp_millis();
    // Widen f32 -> f64 (EmbeddingEngine yields f32; truth-subspace is f64).
    let learning_vectors: Vec<(String, Vec<f64>)> = learning_ids
        .iter()
        .zip(learning_vecs)
        .map(|(id, vec)| (id.clone(), vec.into_iter().map(f64::from).collect()))
        .collect();

    // `build_for_epoch` — extend existing slots when scoped to sessions, else
    // rebuild from scratch (`build.py:170-186`). Pure/sync closure.
    let build_for_epoch = |truth_epoch: i64| -> Vec<TruthCentroidPayload> {
        if session_ids.is_empty() {
            build_centroids_from_learning_vectors(
                &dataset_id_str,
                &learning_vectors,
                truth_epoch,
                Some(updated_at),
                k,
            )
        } else {
            extend_centroids_with_learning_vectors(
                &dataset_id_str,
                &existing_centroids,
                &learning_vectors,
                truth_epoch,
                Some(updated_at),
                k,
            )
        }
    };

    // Shape 4: rebuilt empty -> `default()`, DROPPING the computed signature
    // (faithful Python quirk; `build.py:188-190`).
    let rebuilt_centroids = build_for_epoch(previous_epoch);
    if rebuilt_centroids.is_empty() {
        return TruthSubspaceResult::default();
    }

    // Change detection + upsert (`build.py:192-207`).
    let (current_epoch, centroids) =
        if centroids_changed(&existing_centroids, &rebuilt_centroids, 1e-6) {
            let current_epoch = previous_epoch + 1;
            let centroids = build_for_epoch(current_epoch);
            if let Err(error) = upsert_centroids(vector_db.as_ref(), &centroids).await {
                warn!("truth_subspace: centroid upsert failed open: {error}");
                // Shape 5 (upsert failure).
                return TruthSubspaceResult {
                    anchors: centroids.len(),
                    nodes_scored: 0,
                    signature,
                    truth_epoch: current_epoch,
                };
            }
            (current_epoch, centroids)
        } else {
            // No upsert call at all when unchanged.
            (previous_epoch, existing_centroids.clone())
        };

    let centroid_vecs: Vec<Vec<f64>> = centroids.iter().map(|c| c.centroid.clone()).collect();

    // Load ALL DocumentChunk nodes — the chunk lane the hybrid retriever reranks
    // (`build.py:211-241`). Intentional Rust divergence: Python uses
    // `get_graph_data()` + in-memory filtering to sidestep a Kuzu
    // `asyncio.gather` single-connection deadlock (`build.py:216-218`); Rust has
    // no such constraint, so we use the server-side filtered query directly
    // (same node set, same shape as `lexical_retriever::load_document_chunks`).
    let filters = HashMap::from([(Cow::Borrowed("type"), vec![json!(DOCUMENT_CHUNK_TYPE)])]);
    let nodes = match graph_db.get_filtered_graph_data(&filters).await {
        Ok((nodes, _edges)) => nodes,
        Err(error) => {
            warn!("truth_subspace: node load failed open: {error}");
            // Shape 5 (node-load failure).
            return TruthSubspaceResult {
                anchors: centroids.len(),
                nodes_scored: 0,
                signature,
                truth_epoch: current_epoch,
            };
        }
    };

    let mut node_ids: Vec<String> = Vec::new();
    let mut node_texts: Vec<String> = Vec::new();
    for (node_id, node_data) in nodes {
        if node_data.get("type").and_then(|v| v.as_str()) != Some(DOCUMENT_CHUNK_TYPE) {
            continue;
        }
        // `_node_index_text`: DocumentChunk `text`, falling back to `name`
        // (`build.py:42-47,237`) — distinct from the learning-statement fetch,
        // which reads only `text`. Python uses `text or name or ""`, so the
        // fallback fires on ANY falsy `text` (missing, null, OR empty string),
        // not just an absent key.
        let text = node_field_str(&node_data, "text");
        let text = if text.is_empty() {
            node_field_str(&node_data, "name")
        } else {
            text
        };
        if node_id.is_empty() || text.is_empty() {
            continue;
        }
        node_ids.push(node_id);
        node_texts.push(text);
    }

    // Shape 5 (no scoreable nodes; `build.py:243-250`).
    if node_texts.is_empty() {
        info!(
            "truth_subspace: {} centroids, no scoreable nodes",
            centroids.len()
        );
        return TruthSubspaceResult {
            anchors: centroids.len(),
            nodes_scored: 0,
            signature,
            truth_epoch: current_epoch,
        };
    }

    // Embed node texts in bounded batches, preserving order (`build.py:88-99`).
    // A failed batch pads with EMPTY vectors (NEUTRAL coords), never dropping
    // nodes. Widen f32 -> f64.
    let mut node_vecs: Vec<Vec<f64>> = Vec::with_capacity(node_texts.len());
    for batch in node_texts.chunks(NODE_EMBED_BATCH_SIZE) {
        let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
        match embedding_engine.embed(&refs).await {
            Ok(vecs) => {
                for vec in vecs {
                    node_vecs.push(vec.into_iter().map(f64::from).collect());
                }
            }
            Err(error) => {
                warn!("truth_subspace: node embedding batch failed open: {error}");
                for _ in batch {
                    node_vecs.push(Vec::new());
                }
            }
        }
    }

    // Per-node coords. `zip` truncates to the shorter of ids/vecs (Python
    // `zip(node_ids, node_vecs)` semantics). `node_coords`/`pad_coords` are
    // total — an empty node vec still yields valid all-zero coords — so no
    // per-node fail-open is needed (Python's per-node try/except is dead code).
    let mut scored: HashMap<String, NodeTruthState> = HashMap::new();
    for (node_id, node_vec) in node_ids.iter().zip(node_vecs.iter()) {
        let coords = pad_coords(&align::node_coords(node_vec, &centroid_vecs), k);
        scored.insert(
            node_id.clone(),
            NodeTruthState {
                truth_alignment: coords,
                truth_epoch: current_epoch,
            },
        );
    }

    // Shape 5 (no scored nodes; `build.py:265-271`).
    if scored.is_empty() {
        return TruthSubspaceResult {
            anchors: centroids.len(),
            nodes_scored: 0,
            signature,
            truth_epoch: current_epoch,
        };
    }

    // Persist per-node coordinate vectors (`build.py:273-285`).
    let nodes_scored = match graph_db.set_node_truth_state(&scored).await {
        Ok(write_result) => write_result.values().filter(|ok| **ok).count(),
        Err(error) => {
            warn!("truth_subspace: persisting alignments failed open: {error}");
            // Shape 5 (persist failure).
            return TruthSubspaceResult {
                anchors: centroids.len(),
                nodes_scored: 0,
                signature,
                truth_epoch: current_epoch,
            };
        }
    };

    // Shape 6 (success; `build.py:287-299`).
    info!(
        centroids = centroids.len(),
        nodes_scored = nodes_scored,
        epoch = current_epoch,
        signature = %signature,
        "truth_subspace: built subspace"
    );
    TruthSubspaceResult {
        anchors: centroids.len(),
        nodes_scored,
        signature,
        truth_epoch: current_epoch,
    }
}

/// Trimmed string value of a node field, with Python truthiness semantics.
///
/// Returns `""` when the key is missing, JSON `null`, or a non-string value;
/// a present string is trimmed. This lets the caller reproduce Python's
/// `node_data.get("text") or node_data.get("name") or ""` (`build.py:46`),
/// where the `name` fallback fires on ANY falsy `text`, not just an absent key.
fn node_field_str(node_data: &cognee_graph::NodeData, key: &str) -> String {
    node_data
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Read accepted lesson statements from the `session_learnings` node set.
///
/// Ports `build.py:56-85`. Traverses the node set(s) to their member
/// `DocumentChunk` nodes and returns their de-duplicated `text` (only `text` —
/// NO `name` fallback here, unlike chunk-node scoring). Dedupe key is
/// `.to_lowercase()` (accepted casefold divergence, same as `learning_id`),
/// first-seen order preserved. Fail-open -> `[]`.
async fn fetch_learning_statements(
    graph_db: &dyn GraphDBTrait,
    session_ids: &[String],
) -> Vec<String> {
    // Node-set names: TRUTH_NODE_SET when no sessions, else one per session id
    // (`build.py:50-53`).
    let names: Vec<String> = if session_ids.is_empty() {
        TRUTH_NODE_SET.iter().map(|s| (*s).to_string()).collect()
    } else {
        session_ids
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| truth_session_node_set(s))
            .collect()
    };

    // `"OR"` is Python's unstated `get_nodeset_subgraph` default.
    let nodes = match graph_db
        .get_nodeset_subgraph(NODE_SET_TYPE, &names, "OR")
        .await
    {
        Ok((nodes, _edges)) => nodes,
        Err(error) => {
            warn!("truth_subspace: learning lookup failed open: {error}");
            return Vec::new();
        }
    };

    let mut statements: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (_node_id, node_data) in nodes {
        if node_data.get("type").and_then(|v| v.as_str()) != Some(DOCUMENT_CHUNK_TYPE) {
            continue;
        }
        let text = node_data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !text.is_empty() && seen.insert(text.to_lowercase()) {
            statements.push(text);
        }
    }
    statements
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cognee_embedding::MockEmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBError, GraphDBResult, GraphNode, MockGraphDB, NodeData};
    use cognee_vector::{MockVectorDB, SearchResult, VectorDBError, VectorDBResult, VectorPoint};
    use serde_json::Value;

    const DIM: usize = 8;

    /// A graph double that delegates everything to an inner [`MockGraphDB`] but
    /// makes `get_filtered_graph_data` error — exercising the shape-5 node-load
    /// failure path. Only the trait's required (non-default) methods are
    /// implemented; the rest use the trait defaults.
    struct NodeLoadFailGraph {
        inner: MockGraphDB,
    }

    #[async_trait]
    impl GraphDBTrait for NodeLoadFailGraph {
        async fn get_filtered_graph_data(
            &self,
            _filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Err(GraphDBError::QueryError("graph unavailable".into()))
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

    /// A graph double that delegates everything to an inner [`MockGraphDB`]
    /// (including `get_filtered_graph_data`, so chunk nodes DO load) but makes
    /// the terminal `set_node_truth_state` write error — exercising the shape-5
    /// persist-failure path. Mirrors [`NodeLoadFailGraph`]'s delegation shape.
    struct PersistFailGraph {
        inner: MockGraphDB,
    }

    #[async_trait]
    impl GraphDBTrait for PersistFailGraph {
        async fn set_node_truth_state(
            &self,
            _updates: &HashMap<String, NodeTruthState>,
        ) -> GraphDBResult<HashMap<String, bool>> {
            Err(GraphDBError::QueryError(
                "truth-state write unavailable".into(),
            ))
        }
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

    /// A vector-db double that delegates the read path (`retrieve`, used by
    /// `load_centroids`) to an inner [`MockVectorDB`] but makes
    /// `upsert_raw_vectors` (used by `upsert_centroids`) error — exercising the
    /// shape-5 centroid-upsert-failure path without a shared mock hook.
    struct CentroidUpsertFailVector {
        inner: MockVectorDB,
    }

    #[async_trait]
    impl VectorDB for CentroidUpsertFailVector {
        async fn upsert_raw_vectors(
            &self,
            _data_type: &str,
            _field_name: &str,
            _points: &[VectorPoint],
        ) -> VectorDBResult<()> {
            Err(VectorDBError::StorageError(
                "centroid upsert unavailable".into(),
            ))
        }
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
            self.inner.index_points(data_type, field_name, points).await
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
        async fn retrieve(
            &self,
            data_type: &str,
            field_name: &str,
            ids: &[Uuid],
        ) -> VectorDBResult<Vec<SearchResult>> {
            self.inner.retrieve(data_type, field_name, ids).await
        }
        async fn collection_size(
            &self,
            data_type: &str,
            field_name: &str,
        ) -> VectorDBResult<usize> {
            self.inner.collection_size(data_type, field_name).await
        }
    }

    /// Seed a `session_learnings:{sid}` NodeSet plus lesson `DocumentChunk`
    /// members (connected by edges so `get_nodeset_subgraph` returns them), and
    /// optionally extra corpus `DocumentChunk` nodes (NOT in the node set).
    async fn seed_graph(
        graph: &MockGraphDB,
        session_id: &str,
        lesson_texts: &[&str],
        corpus_texts: &[&str],
    ) {
        let set_id = format!("nodeset-{session_id}");
        graph
            .add_node_raw(json!({
                "id": set_id,
                "type": NODE_SET_TYPE,
                "name": truth_session_node_set(session_id),
            }))
            .await
            .unwrap();

        for (i, text) in lesson_texts.iter().enumerate() {
            let chunk_id = format!("lesson-{session_id}-{i}");
            graph
                .add_node_raw(json!({
                    "id": chunk_id,
                    "type": DOCUMENT_CHUNK_TYPE,
                    "text": text,
                }))
                .await
                .unwrap();
            graph
                .add_edge(&set_id, &chunk_id, "contains", None)
                .await
                .unwrap();
        }

        for (i, text) in corpus_texts.iter().enumerate() {
            let chunk_id = format!("corpus-{i}");
            graph
                .add_node_raw(json!({
                    "id": chunk_id,
                    "type": DOCUMENT_CHUNK_TYPE,
                    "text": text,
                }))
                .await
                .unwrap();
        }
    }

    fn handles(
        graph: MockGraphDB,
        vector: MockVectorDB,
        embed: MockEmbeddingEngine,
    ) -> (
        Arc<dyn GraphDBTrait>,
        Arc<dyn VectorDB>,
        Arc<dyn EmbeddingEngine>,
    ) {
        (Arc::new(graph), Arc::new(vector), Arc::new(embed))
    }

    // Test 1 — no learning statements -> default().
    #[tokio::test]
    async fn no_statements_returns_default() {
        let graph = MockGraphDB::new(); // empty graph -> no learnings
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let (g, v, e) = handles(graph, vector, embed);

        let out = build_truth_subspace(Uuid::new_v4(), &["s1".to_string()], g, v, e, 3).await;
        assert_eq!(out, TruthSubspaceResult::default());
    }

    // Test 2 — learning-embed failure -> shape 3.
    #[tokio::test]
    async fn learning_embed_failure_returns_shape3() {
        let graph = MockGraphDB::new();
        seed_graph(&graph, "s1", &["Coffee matters", "Tea matters"], &[]).await;
        let vector = MockVectorDB::new(); // fresh -> no existing centroids
        let embed = MockEmbeddingEngine::deterministic(DIM);
        embed.set_failure_after(0); // very first embed call (learnings) fails
        let (g, v, e) = handles(graph, vector, embed);

        let out = build_truth_subspace(Uuid::new_v4(), &["s1".to_string()], g, v, e, 3).await;
        assert_eq!(out.anchors, 0, "no existing centroids");
        assert_eq!(out.nodes_scored, 0);
        assert!(!out.signature.is_empty(), "signature computed before embed");
        assert_eq!(out.truth_epoch, 0, "previous_epoch");
    }

    // Test 3 — fresh dataset -> epoch bumped to 1, centroids <= k; a second
    // identical run is a no-op (unchanged, epoch stays 1).
    #[tokio::test]
    async fn fresh_build_bumps_epoch_then_idempotent() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let graph = MockGraphDB::new();
        seed_graph(
            &graph,
            "s1",
            &[
                "Alpha lesson",
                "Beta lesson",
                "Gamma lesson",
                "Delta lesson",
            ],
            &[],
        )
        .await;
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let (g, v, e) = handles(graph, vector, embed);

        let first = build_truth_subspace(
            dataset,
            &["s1".to_string()],
            Arc::clone(&g),
            Arc::clone(&v),
            Arc::clone(&e),
            k,
        )
        .await;
        assert!(first.anchors <= k && first.anchors > 0);
        assert_eq!(first.truth_epoch, 1, "bumped from 0");
        assert!(!first.signature.is_empty());

        // Second identical run: centroids unchanged -> no upsert, epoch unchanged.
        let second = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, k).await;
        assert_eq!(second.truth_epoch, 1, "epoch unchanged on no-op");
        assert_eq!(second.anchors, first.anchors);
        assert_eq!(second.signature, first.signature);
    }

    // Test 4 — DocumentChunk corpus nodes get a length-k truth_alignment with
    // the current epoch; assert via MockGraphDB's recorded set_node_truth_state.
    #[tokio::test]
    async fn corpus_nodes_get_truth_alignment() {
        let dataset = Uuid::new_v4();
        let k = 4;
        let graph = MockGraphDB::new();
        seed_graph(
            &graph,
            "s1",
            &["Lesson one", "Lesson two"],
            &["Corpus chunk A", "Corpus chunk B", "Corpus chunk C"],
        )
        .await;
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let graph_arc: Arc<dyn GraphDBTrait> = Arc::new(graph.clone());
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], graph_arc, v, e, k).await;
        // 5 DocumentChunk nodes total (2 lessons + 3 corpus) are all scoreable.
        assert_eq!(out.nodes_scored, 5);
        assert_eq!(out.truth_epoch, 1);
        assert!(
            graph
                .get_call_log()
                .contains(&"set_node_truth_state".to_string())
        );

        // Every scored node carries a length-k alignment with the epoch.
        let state = graph
            .get_node_truth_state(&["corpus-0".to_string()])
            .await
            .unwrap();
        let ts = state.get("corpus-0").expect("corpus-0 scored");
        assert_eq!(ts.truth_alignment.len(), k);
        assert_eq!(ts.truth_epoch, 1);
    }

    // Test 5 — a dataset whose ONLY DocumentChunks are the lessons still scores
    // them (they ARE DocumentChunks): the positive complement showing every
    // scoreable chunk is picked up by the corpus scan.
    #[tokio::test]
    async fn lessons_only_are_scored() {
        let dataset = Uuid::new_v4();
        let graph = MockGraphDB::new();
        seed_graph(&graph, "s1", &["Only lesson text here"], &[]).await;
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let (g, v, e) = handles(graph, vector, embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, 3).await;
        assert!(out.anchors > 0);
        assert_eq!(out.nodes_scored, 1, "the single lesson chunk is scored");
    }

    // Test 5b — shape 5: node-load failure. Learnings fetch + centroid build
    // succeed (anchors > 0, signature computed), then the DocumentChunk load
    // errors -> {anchors: centroids.len(), 0, signature, current_epoch}.
    #[tokio::test]
    async fn node_load_failure_returns_shape5() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let inner = MockGraphDB::new();
        seed_graph(&inner, "s1", &["Lesson one", "Lesson two"], &[]).await;
        let graph = NodeLoadFailGraph { inner };
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let g: Arc<dyn GraphDBTrait> = Arc::new(graph);
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, k).await;
        assert!(out.anchors > 0, "centroids were built before the node load");
        assert_eq!(out.nodes_scored, 0);
        assert!(!out.signature.is_empty());
        assert_eq!(out.truth_epoch, 1, "current_epoch after the upsert");
    }

    // Test 6 — node-embedding batch fails (learnings already embedded) -> nodes
    // still scored with NEUTRAL (all-zero) coords, nodes_scored == node count.
    #[tokio::test]
    async fn node_embed_batch_failure_gives_neutral_coords() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let graph = MockGraphDB::new();
        seed_graph(&graph, "s1", &["Lesson one"], &["Corpus A", "Corpus B"]).await;
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        // First embed call = learnings (succeeds); second = node batch (fails).
        embed.set_failure_after(1);
        let graph_arc: Arc<dyn GraphDBTrait> = Arc::new(graph.clone());
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], graph_arc, v, e, k).await;
        // 3 DocumentChunks (1 lesson + 2 corpus) all still scored, none dropped.
        assert_eq!(out.nodes_scored, 3);
        assert_eq!(out.truth_epoch, 1);

        // The neutral (empty node vec) -> node_coords all-zero -> padded to k.
        let state = graph
            .get_node_truth_state(&["corpus-0".to_string()])
            .await
            .unwrap();
        let ts = state.get("corpus-0").expect("corpus-0 scored");
        assert_eq!(ts.truth_alignment.len(), k);
        assert!(
            ts.truth_alignment.iter().all(|&c| c == 0.0),
            "neutral coords are all zero"
        );
    }

    // Test 7 — session_ids == &[] exercises build_centroids_from_learning_vectors
    // (the rebuild branch), not extend. With no sessions the fetch falls back to
    // the TRUTH_NODE_SET ("session_learnings") node set name.
    #[tokio::test]
    async fn empty_session_ids_uses_rebuild_branch() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let graph = MockGraphDB::new();
        // NodeSet named exactly "session_learnings" (the TRUTH_NODE_SET default).
        let set_id = "nodeset-default";
        graph
            .add_node_raw(json!({
                "id": set_id,
                "type": NODE_SET_TYPE,
                "name": "session_learnings",
            }))
            .await
            .unwrap();
        for (i, text) in ["Rebuild lesson A", "Rebuild lesson B"].iter().enumerate() {
            let chunk_id = format!("dl-{i}");
            graph
                .add_node_raw(json!({
                    "id": chunk_id,
                    "type": DOCUMENT_CHUNK_TYPE,
                    "text": text,
                }))
                .await
                .unwrap();
            graph
                .add_edge(set_id, &chunk_id, "contains", None)
                .await
                .unwrap();
        }
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let (g, v, e) = handles(graph, vector, embed);

        // Empty session_ids -> rebuild branch; still builds centroids + scores.
        let out = build_truth_subspace(dataset, &[], g, v, e, k).await;
        assert!(out.anchors > 0);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(out.nodes_scored, 2, "both default-set lessons scored");
    }

    // Test 8 — signature-drop quirk: when rebuilt centroids are empty the result
    // is default() with an EMPTY signature, even though a signature was computed.
    //
    // Rebuilt is empty only when there are learning statements but the resulting
    // learning_vectors produce no slots, which `extend`/`build` never do for a
    // non-empty vector list. The quirk is therefore verified at the unit level
    // in truth-subspace; here we assert the contract that a successful build
    // NEVER drops the signature (the positive complement).
    #[tokio::test]
    async fn successful_build_keeps_signature() {
        let dataset = Uuid::new_v4();
        let graph = MockGraphDB::new();
        seed_graph(&graph, "s1", &["Kept lesson"], &[]).await;
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let (g, v, e) = handles(graph, vector, embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, 3).await;
        assert!(!out.signature.is_empty());
    }

    // Test 9 — node-scoring text/name truthiness fallback (`build.py:46`).
    // A DocumentChunk whose `text` is empty-string or null but whose `name` is
    // non-empty is scored via `name`; a chunk whose text AND name are both
    // falsy is skipped. Mirrors Python's `text or name or ""`.
    #[tokio::test]
    async fn falsy_text_falls_back_to_name_for_scoring() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let graph = MockGraphDB::new();
        // One real lesson so learnings/centroids build.
        seed_graph(&graph, "s1", &["A lesson"], &[]).await;

        // Corpus chunk with empty-string text but a real name -> scored via name.
        graph
            .add_node_raw(json!({
                "id": "empty-text-named",
                "type": DOCUMENT_CHUNK_TYPE,
                "text": "",
                "name": "Named via name field",
            }))
            .await
            .unwrap();
        // Corpus chunk with null text but a real name -> scored via name.
        graph
            .add_node_raw(json!({
                "id": "null-text-named",
                "type": DOCUMENT_CHUNK_TYPE,
                "text": Value::Null,
                "name": "Also named",
            }))
            .await
            .unwrap();
        // Corpus chunk with BOTH text and name empty -> skipped.
        graph
            .add_node_raw(json!({
                "id": "both-empty",
                "type": DOCUMENT_CHUNK_TYPE,
                "text": "",
                "name": "",
            }))
            .await
            .unwrap();

        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let graph_arc: Arc<dyn GraphDBTrait> = Arc::new(graph.clone());
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], graph_arc, v, e, k).await;
        // 1 lesson + 2 name-fallback chunks scored; the both-empty chunk skipped.
        assert_eq!(out.nodes_scored, 3);

        // The name-fallback chunks got the current epoch persisted; the
        // both-empty chunk was never scored (no truth_epoch field -> sentinel -1).
        let scored = graph
            .get_node_truth_state(&[
                "empty-text-named".to_string(),
                "null-text-named".to_string(),
                "both-empty".to_string(),
            ])
            .await
            .unwrap();
        assert_eq!(
            scored.get("empty-text-named").unwrap().truth_epoch,
            1,
            "empty text -> scored via name"
        );
        assert_eq!(
            scored.get("null-text-named").unwrap().truth_epoch,
            1,
            "null text -> scored via name"
        );
        assert_eq!(
            scored.get("both-empty").unwrap().truth_epoch,
            -1,
            "both text and name falsy -> never scored"
        );
    }

    // Test 10 — shape 5: persist failure. Learnings fetch + centroid build +
    // node load + scoring all succeed, then the terminal set_node_truth_state
    // write errors -> {anchors: centroids.len(), 0, signature, current_epoch}.
    // The failure never propagates (build_truth_subspace is infallible).
    #[tokio::test]
    async fn persist_failure_returns_shape5() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let inner = MockGraphDB::new();
        // Learnings so centroids build, plus a corpus chunk so there IS a
        // scoreable node reaching the persist call (otherwise it short-circuits
        // at the no-scoreable-nodes branch instead of the persist branch).
        seed_graph(&inner, "s1", &["Lesson one", "Lesson two"], &["Corpus A"]).await;
        let graph = PersistFailGraph { inner };
        let vector = MockVectorDB::new();
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let g: Arc<dyn GraphDBTrait> = Arc::new(graph);
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, k).await;
        assert!(out.anchors > 0, "centroids were built before the persist");
        assert_eq!(out.nodes_scored, 0, "persist failed -> nothing counted");
        assert!(!out.signature.is_empty());
        assert_eq!(out.truth_epoch, 1, "current_epoch after the upsert");
    }

    // Test 11 — shape 5: centroid-upsert failure. Learnings fetch + centroid
    // build succeed and the change-detected upsert_raw_vectors errors ->
    // {anchors: centroids.len(), 0, signature, current_epoch}. Fresh vector db
    // (retrieve delegates -> no existing centroids), so centroids_changed is
    // true and the epoch bumps to 1 before the failing upsert.
    #[tokio::test]
    async fn centroid_upsert_failure_returns_shape5() {
        let dataset = Uuid::new_v4();
        let k = 3;
        let graph = MockGraphDB::new();
        seed_graph(&graph, "s1", &["Lesson one", "Lesson two"], &[]).await;
        let vector = CentroidUpsertFailVector {
            inner: MockVectorDB::new(),
        };
        let embed = MockEmbeddingEngine::deterministic(DIM);
        let g: Arc<dyn GraphDBTrait> = Arc::new(graph);
        let v: Arc<dyn VectorDB> = Arc::new(vector);
        let e: Arc<dyn EmbeddingEngine> = Arc::new(embed);

        let out = build_truth_subspace(dataset, &["s1".to_string()], g, v, e, k).await;
        // anchors == centroids.len() > 0 (built, then upsert failed).
        assert!(out.anchors > 0, "centroids were built before the upsert");
        assert_eq!(out.nodes_scored, 0, "upsert failed before any scoring");
        assert!(!out.signature.is_empty());
        assert_eq!(out.truth_epoch, 1, "current_epoch: previous(0) + 1");
    }
}
