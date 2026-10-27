//! Stage 2d of `improve()` — build/refresh centroid-slot truth coordinates for
//! a dataset's session learnings, then project every `DocumentChunk` node onto
//! those slots and persist the per-node alignment.
//!
//! Ported from Python `cognee/modules/truth_subspace/build.py`. This is the
//! orchestration layer that *drives* the pure math
//! (`cognee-truth-subspace::align`/`centroids`) and I/O wiring
//! (`load_centroids`/`upsert_centroids`, `set_node_truth_state`).
//!
//! # Write order: the centroids are the commit point
//!
//! The hybrid reranker only trusts a chunk's coordinates when the chunk's
//! `truth_epoch` equals the live centroids' epoch. A build that moves the epoch
//! from N to N+1 therefore holds the N+1 centroids in memory, scores and
//! persists the chunk coordinates at N+1 *first*, and upserts the N+1 centroids
//! *last* (`build.py:7-17`). A failure anywhere before that final write leaves
//! epoch N live: chunks already rewritten at N+1 no longer match it, so the
//! reranker ignores them until a later build succeeds — degraded reranking,
//! never wrong reranking. Chunks whose embedding batch failed are skipped
//! outright rather than stored as all-zero "neutral" coordinates, and the
//! backend's truth-state support is probed before the first embedding call.
//!
//! # Infallible by construction
//!
//! Like the Python original (`-> dict`), [`build_truth_subspace`] never returns
//! a `Result` and never aborts `improve()`: every internal fallible step fails
//! open to a [`TruthSubspaceResult`] whose [`status`](TruthSubspaceResult::status)
//! says what happened. The stage is default-off end-to-end, so when the caller
//! does not opt in this code never runs.

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

/// Graph node type of a chunk (`DocumentChunk.__name__`).
const DOCUMENT_CHUNK_TYPE: &str = "DocumentChunk";
/// Graph node type of a node set (`NodeSet.__name__`).
const NODE_SET_TYPE: &str = "NodeSet";

/// Node-text embedding batch size — keeps memory bounded on large subgraphs
/// (`build.py:55`). Applies only to chunk-node embedding; learning-statement
/// embedding is always a single call.
pub const NODE_EMBED_BATCH_SIZE: usize = 64;

/// Skip reason when the graph backend has no truth-state support
/// (`build.py:63`, `REASON_BACKEND_UNSUPPORTED`).
pub const REASON_BACKEND_UNSUPPORTED: &str = "backend_unsupported";

/// Outcome of a [`build_truth_subspace`] run (`build.py:57-60`): Python's
/// `PipelineRunInfo` vocabulary plus `skipped`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TruthBuildStatus {
    /// The build ran to the end (possibly with nothing to do).
    #[default]
    Completed,
    /// The build did not run; [`TruthSubspaceResult::reason`] says why.
    Skipped,
    /// A step failed and the previous epoch was kept live;
    /// [`TruthSubspaceResult::error`] says what failed.
    Errored,
}

impl TruthBuildStatus {
    /// The wire name Python uses (`"completed"` / `"skipped"` / `"errored"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Errored => "errored",
        }
    }
}

/// Summary of a [`build_truth_subspace`] run.
///
/// The Rust analog of Python's return `dict` (`build.py:66-90`);
/// [`TruthSubspaceResult::default`] is Python's `empty_result`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TruthSubspaceResult {
    /// Number of centroid slots that are live after the call.
    pub anchors: usize,
    /// Number of `DocumentChunk` nodes whose truth state was persisted.
    pub nodes_scored: usize,
    /// Number of `DocumentChunk` nodes left unscored because their embedding
    /// batch failed. They keep whatever state they had; they are never
    /// written as all-zero coordinates.
    pub nodes_skipped: usize,
    /// Stable sha256 signature of the ordered learning ids.
    pub signature: String,
    /// The epoch that is *live* after the call: a build that failed before its
    /// final centroid write reports the previous epoch.
    pub truth_epoch: i64,
    /// What happened.
    pub status: TruthBuildStatus,
    /// Why the build was skipped (set only when `status` is `Skipped`).
    pub reason: Option<String>,
    /// What failed (set only when `status` is `Errored`).
    pub error: Option<String>,
}

/// Build/refresh centroid slots and chunk coordinates for `dataset_id`.
///
/// Ports `build.py:180-458`. Infallible — see the module docs. `session_ids`
/// empty is valid input (mirrors Python's `Optional[...] = None`) and takes the
/// from-scratch rebuild branch; `improve()` always passes non-empty (gated by
/// `has_sessions`).
///
/// `dataset_id` replaces Python's `_resolve_dataset` (`build.py:174-177,200`):
/// the caller (Stage 2d) resolves the dataset exactly as Stage 4 does, so this
/// function skips Python's per-call ACL re-check (Rust's owner/tenant scoping
/// has no such layer here, matching the sibling stage functions).
///
/// # Outcomes (parity-exact with `build.py`)
///
/// * backend without truth-state support → `Skipped` / `backend_unsupported`,
///   before any read or embedding call (`build.py:209-225`); a graph engine
///   that cannot be reached at all → `Errored`, as Python's raise from
///   `get_graph_engine()` ends up.
/// * no learning statements, or no centroid slots from them → `default()`
///   (`Completed`, all zero). The latter drops the computed `signature` — a
///   preserved Python quirk (`build.py:296-298`).
/// * learning-embed / node-load failure, no chunk embedded, chunk-write
///   failure, no chunk write accepted, centroid-commit failure → `Errored`,
///   with the *previous* epoch and its centroid count reported live
///   (`build.py:252-264`).
/// * no scoreable chunks → the new centroids are committed on their own
///   (`build.py:362-374`).
/// * success → `Completed` with the new epoch (`build.py:452-458`).
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
            NODE_EMBED_BATCH_SIZE,
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
    batch_size: usize,
) -> TruthSubspaceResult {
    let dataset_id_str = dataset_id.to_string();

    // Step 0: capability gate, before any embedding call (`build.py:209-225`).
    // Without it the build would embed every chunk and only then find the
    // backend cannot store the result. A probe error means the dataset's
    // engine is unreachable — Python's `get_graph_engine()` raising — so the
    // build errors instead of mistaking it for an empty graph further down.
    match graph_db.supports_truth_state().await {
        Ok(true) => {}
        Ok(false) => {
            info!("truth_subspace: graph backend has no truth-state support, skipping");
            return TruthSubspaceResult {
                status: TruthBuildStatus::Skipped,
                reason: Some(REASON_BACKEND_UNSUPPORTED.to_string()),
                ..TruthSubspaceResult::default()
            };
        }
        Err(error) => {
            warn!("truth_subspace: graph engine unavailable: {error}");
            return TruthSubspaceResult {
                status: TruthBuildStatus::Errored,
                error: Some(format!("graph engine unavailable: {error}")),
                ..TruthSubspaceResult::default()
            };
        }
    }

    // Step 1: accepted learning statements from the session_learnings node set.
    let statements = fetch_learning_statements(graph_db.as_ref(), session_ids).await;
    if statements.is_empty() {
        info!("truth_subspace: no learnings found, nothing to build");
        return TruthSubspaceResult::default();
    }

    // Existing centroids (fail-open to empty; `build.py:233-237`).
    let existing_centroids = load_centroids(vector_db.as_ref(), &dataset_id_str, k)
        .await
        .unwrap_or_default();
    let previous_epoch = existing_centroids
        .iter()
        .map(|c| c.truth_epoch)
        .max()
        .unwrap_or(0);

    // Dedupe (LAST-wins) + sort by learning id via a BTreeMap — matches Python's
    // dict comprehension + `sorted(key=item[0])` (`build.py:240-247`). This is
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

    // Report the state that is live: epoch N's centroids, untouched
    // (`build.py:252-264`). Every caller has an error to report.
    let live_epoch_result =
        |nodes_scored: usize, nodes_skipped: usize, error: String| TruthSubspaceResult {
            anchors: existing_centroids.len(),
            nodes_scored,
            nodes_skipped,
            signature: signature.clone(),
            truth_epoch: previous_epoch,
            status: TruthBuildStatus::Errored,
            reason: None,
            error: Some(error),
        };

    // Embed learning texts in ONE call, not batched (`build.py:266-273`).
    let learning_refs: Vec<&str> = learning_texts.iter().map(String::as_str).collect();
    let learning_vecs = match embedding_engine.embed(&learning_refs).await {
        Ok(vecs) => vecs,
        Err(error) => {
            warn!("truth_subspace: learning embedding failed open: {error}");
            return live_epoch_result(0, 0, format!("learning embedding failed: {error}"));
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
    // rebuild from scratch (`build.py:278-294`). Pure/sync closure.
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

    // Rebuilt empty -> `default()`, DROPPING the computed signature (faithful
    // Python quirk; `build.py:296-298`).
    let rebuilt_centroids = build_for_epoch(previous_epoch);
    if rebuilt_centroids.is_empty() {
        return TruthSubspaceResult::default();
    }

    // Step 2: decide the epoch. New centroids are held in memory until the
    // chunks have been scored against them; nothing is written yet
    // (`build.py:300-319`).
    //
    // Known limit (as in Python): chunk validity at read time is the integer
    // epoch alone, not the learning basis. A run that stamped chunks at N+1 and
    // crashed before the commit, followed by a run over DIFFERENT learnings
    // that also computes N+1 but skips some chunks, leaves those chunks passing
    // the epoch guard with abandoned-basis coordinates. Two independent
    // failures, damage bounded by the [0.75, 1.25] factor.
    let (current_epoch, centroids, centroids_pending_write) =
        if centroids_changed(&existing_centroids, &rebuilt_centroids, 1e-6) {
            let current_epoch = previous_epoch + 1;
            (current_epoch, build_for_epoch(current_epoch), true)
        } else {
            (previous_epoch, existing_centroids.clone(), false)
        };

    let centroid_vecs: Vec<Vec<f64>> = centroids.iter().map(|c| c.centroid.clone()).collect();

    // Write the N+1 centroids: the last step of an epoch move. `Err` carries
    // the error to report (`build.py:323-334`).
    let commit_centroids = || async {
        if !centroids_pending_write {
            return Ok(());
        }
        upsert_centroids(vector_db.as_ref(), &centroids)
            .await
            .map_err(|error| {
                warn!("truth_subspace: centroid upsert failed open: {error}");
                format!("centroid upsert failed: {error}")
            })
    };

    // Step 3: load ALL DocumentChunk nodes — the chunk lane the hybrid
    // retriever reranks (`build.py:336-360`). Intentional Rust divergence:
    // Python uses `get_graph_data()` + in-memory filtering to sidestep a Kuzu
    // `asyncio.gather` single-connection deadlock; Rust has no such
    // constraint, so we use the server-side filtered query directly (same node
    // set, same shape as `lexical_retriever::load_document_chunks`).
    let filters = HashMap::from([(Cow::Borrowed("type"), vec![json!(DOCUMENT_CHUNK_TYPE)])]);
    let nodes = match graph_db.get_filtered_graph_data(&filters).await {
        Ok((nodes, _edges)) => nodes,
        Err(error) => {
            warn!("truth_subspace: node load failed open: {error}");
            return live_epoch_result(0, 0, format!("node load failed: {error}"));
        }
    };

    let mut node_ids: Vec<String> = Vec::new();
    let mut node_texts: Vec<String> = Vec::new();
    for (node_id, node_data) in nodes {
        if node_data.get("type").and_then(|v| v.as_str()) != Some(DOCUMENT_CHUNK_TYPE) {
            continue;
        }
        // `_node_index_text`: DocumentChunk `text`, falling back to `name`
        // (`build.py:93-98`) — distinct from the learning-statement fetch,
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

    if node_texts.is_empty() {
        // Nothing to score, so nothing can be left inconsistent: the new
        // centroids can go live on their own (`build.py:362-374`).
        info!(
            "truth_subspace: {} centroids, no scoreable nodes",
            centroids.len()
        );
        if let Err(error) = commit_centroids().await {
            return live_epoch_result(0, 0, error);
        }
        return TruthSubspaceResult {
            anchors: centroids.len(),
            signature,
            truth_epoch: current_epoch,
            ..TruthSubspaceResult::default()
        };
    }

    // Step 4: embed node texts (batched) and compute coords per node at the
    // target epoch. Nodes whose batch failed are skipped, never zero-filled
    // (`build.py:376-396`).
    let node_vecs = embed_in_batches(embedding_engine.as_ref(), &node_texts, batch_size).await;
    let mut scored: HashMap<String, NodeTruthState> = HashMap::new();
    let mut nodes_skipped = 0;
    for (node_id, node_vec) in node_ids.iter().zip(node_vecs) {
        let Some(node_vec) = node_vec else {
            nodes_skipped += 1;
            continue;
        };
        // `node_coords`/`pad_coords` are total, so Python's per-node
        // try/except has no Rust counterpart.
        let coords = pad_coords(&align::node_coords(&node_vec, &centroid_vecs), k);
        scored.insert(
            node_id.clone(),
            NodeTruthState {
                truth_alignment: coords,
                truth_epoch: current_epoch,
            },
        );
    }

    if scored.is_empty() {
        // Every chunk failed to embed. Moving the epoch now would leave the
        // whole corpus stale against the live centroids, so keep epoch N
        // (`build.py:398-408`).
        warn!(
            "truth_subspace: no node could be scored ({nodes_skipped} skipped), \
             keeping epoch {previous_epoch} live"
        );
        return live_epoch_result(0, nodes_skipped, "no node embeddings available".into());
    }

    // Step 5: persist per-node coordinate vectors at epoch N+1 first ...
    // (`build.py:410-433`).
    let nodes_scored = match graph_db.set_node_truth_state(&scored).await {
        Ok(write_result) => write_result.values().filter(|ok| **ok).count(),
        Err(error) => {
            warn!("truth_subspace: persisting alignments failed open: {error}");
            return live_epoch_result(
                0,
                nodes_skipped,
                format!("persisting alignments failed: {error}"),
            );
        }
    };
    if nodes_scored == 0 {
        // Every write was refused (the chunks were deleted meanwhile, say).
        // Committing N+1 centroids over zero live-epoch chunks would silently
        // turn truth reranking off — keep epoch N.
        warn!(
            "truth_subspace: no node accepted its truth state ({nodes_skipped} skipped), \
             keeping epoch {previous_epoch} live"
        );
        return live_epoch_result(0, nodes_skipped, "no truth state written".into());
    }

    // Step 6: ... and only then move the centroids to N+1. Until this write
    // lands, epoch N is what the reranker sees (`build.py:435-441`).
    if let Err(error) = commit_centroids().await {
        return live_epoch_result(nodes_scored, nodes_skipped, error);
    }

    info!(
        centroids = centroids.len(),
        nodes_scored = nodes_scored,
        nodes_skipped = nodes_skipped,
        epoch = current_epoch,
        signature = %signature,
        "truth_subspace: built subspace"
    );
    TruthSubspaceResult {
        anchors: centroids.len(),
        nodes_scored,
        nodes_skipped,
        signature,
        truth_epoch: current_epoch,
        ..TruthSubspaceResult::default()
    }
}

/// Embed `texts` in batches of `batch_size` ([`NODE_EMBED_BATCH_SIZE`] outside
/// tests), preserving order (`build.py:139-171`).
///
/// A failed batch — an error, or a vector count that does not match the batch
/// — yields `None` for each of its texts so the caller skips those nodes. It
/// never yields a placeholder vector: an empty or all-zero vector would project
/// to all-zero "neutral" coordinates and be persisted as if it were a real
/// score, and a short batch would shift every later vector onto the wrong node.
async fn embed_in_batches(
    embedding_engine: &dyn EmbeddingEngine,
    texts: &[String],
    batch_size: usize,
) -> Vec<Option<Vec<f64>>> {
    let mut vectors: Vec<Option<Vec<f64>>> = Vec::with_capacity(texts.len());
    for batch in texts.chunks(batch_size.max(1)) {
        let refs: Vec<&str> = batch.iter().map(String::as_str).collect();
        match embedding_engine.embed(&refs).await {
            Ok(batch_vectors) if batch_vectors.len() == batch.len() => {
                vectors.extend(
                    batch_vectors
                        .into_iter()
                        .map(|vec| Some(vec.into_iter().map(f64::from).collect())),
                );
            }
            Ok(batch_vectors) => {
                warn!(
                    "truth_subspace: node embedding batch returned {} vectors for {} texts, \
                     skipping those nodes",
                    batch_vectors.len(),
                    batch.len()
                );
                vectors.extend(batch.iter().map(|_| None));
            }
            Err(error) => {
                warn!(
                    "truth_subspace: node embedding batch of {} failed, skipping those nodes: \
                     {error}",
                    batch.len()
                );
                vectors.extend(batch.iter().map(|_| None));
            }
        }
    }
    vectors
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
/// Ports `build.py:107-136`. Traverses the node set(s) to their member
/// `DocumentChunk` nodes and returns their de-duplicated `text` (only `text` —
/// NO `name` fallback here, unlike chunk-node scoring). Dedupe key is
/// `.to_lowercase()` (accepted casefold divergence, same as `learning_id`),
/// first-seen order preserved. Fail-open -> `[]`.
async fn fetch_learning_statements(
    graph_db: &dyn GraphDBTrait,
    session_ids: &[String],
) -> Vec<String> {
    // Node-set names: TRUTH_NODE_SET when no sessions, else one per session id
    // (`build.py:101-104`).
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
    //! Unit tests, including a port of Python's
    //! `cognee/tests/unit/truth_subspace/test_build_write_order_and_gates.py`:
    //! the backend probe runs before any embedding call, a failed embedding
    //! batch is skipped rather than zero-filled, and chunk coordinates land
    //! before the centroids so any earlier failure keeps epoch N live.

    use super::*;
    use async_trait::async_trait;
    use cognee_embedding::{EmbeddingError, EmbeddingResult, MockEmbeddingEngine};
    use cognee_graph::{EdgeData, GraphDBError, GraphDBResult, GraphNode, MockGraphDB, NodeData};
    use cognee_vector::{MockVectorDB, SearchResult, VectorDBError, VectorDBResult, VectorPoint};
    use serde_json::Value;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const DIM: usize = 8;
    const SESSION: &str = "s1";

    /// Shared, ordered log of the two writes whose order is the commit
    /// protocol: `"set_node_truth_state"` and `"upsert_centroids"`.
    type WriteLog = Arc<Mutex<Vec<&'static str>>>;

    /// What [`ScriptedGraph::get_filtered_graph_data`] does.
    #[derive(Clone, Copy, PartialEq)]
    enum NodeLoad {
        Inner,
        Fail,
        /// Return no nodes, standing in for a graph with no scoreable chunks.
        Empty,
    }

    /// What [`ScriptedGraph::set_node_truth_state`] does.
    #[derive(Clone, Copy, PartialEq)]
    enum TruthWrite {
        Inner,
        Fail,
        /// Accept the call but report every write as refused.
        RefuseAll,
    }

    /// A graph double over an inner [`MockGraphDB`] (whose call log records the
    /// reads) with scripted capability, node-load and truth-write behaviour.
    /// Only the trait's required methods are delegated; the rest use the
    /// trait defaults.
    struct ScriptedGraph {
        inner: MockGraphDB,
        supports: AtomicBool,
        /// The probe errors, as a router whose dataset engine cannot be opened.
        unreachable: AtomicBool,
        node_load: Mutex<NodeLoad>,
        truth_write: Mutex<TruthWrite>,
        writes: WriteLog,
    }

    #[async_trait]
    impl GraphDBTrait for ScriptedGraph {
        async fn supports_truth_state(&self) -> GraphDBResult<bool> {
            if self.unreachable.load(Ordering::SeqCst) {
                return Err(GraphDBError::ConnectionError("engine unreachable".into()));
            }
            Ok(self.supports.load(Ordering::SeqCst))
        }
        async fn set_node_truth_state(
            &self,
            updates: &HashMap<String, NodeTruthState>,
        ) -> GraphDBResult<HashMap<String, bool>> {
            self.writes.lock().unwrap().push("set_node_truth_state");
            let mode = *self.truth_write.lock().unwrap();
            match mode {
                TruthWrite::Inner => self.inner.set_node_truth_state(updates).await,
                TruthWrite::Fail => Err(GraphDBError::QueryError("graph write failed".into())),
                TruthWrite::RefuseAll => Ok(updates.keys().map(|id| (id.clone(), false)).collect()),
            }
        }
        async fn get_filtered_graph_data(
            &self,
            filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            let mode = *self.node_load.lock().unwrap();
            match mode {
                NodeLoad::Inner => self.inner.get_filtered_graph_data(filters).await,
                NodeLoad::Fail => Err(GraphDBError::QueryError("graph down".into())),
                NodeLoad::Empty => Ok((Vec::new(), Vec::new())),
            }
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

    /// A vector-db double over an inner [`MockVectorDB`] that logs every
    /// `upsert_raw_vectors` (the centroid commit) and can make it fail.
    struct RecordingVector {
        inner: MockVectorDB,
        fail_upsert: AtomicBool,
        upserts: AtomicUsize,
        writes: WriteLog,
    }

    #[async_trait]
    impl VectorDB for RecordingVector {
        async fn upsert_raw_vectors(
            &self,
            data_type: &str,
            field_name: &str,
            points: &[VectorPoint],
        ) -> VectorDBResult<()> {
            self.writes.lock().unwrap().push("upsert_centroids");
            self.upserts.fetch_add(1, Ordering::SeqCst);
            if self.fail_upsert.load(Ordering::SeqCst) {
                return Err(VectorDBError::StorageError("vector store down".into()));
            }
            self.inner
                .upsert_raw_vectors(data_type, field_name, points)
                .await
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

    /// Deterministic embeddings (via [`MockEmbeddingEngine`]) that record every
    /// batch. A batch containing a text in `fail_on` errors; one containing a
    /// text in `short_on` comes back one vector short.
    struct RecordingEmbedding {
        inner: MockEmbeddingEngine,
        fail_on: Mutex<HashSet<String>>,
        short_on: Mutex<HashSet<String>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl RecordingEmbedding {
        fn batch_hits(set: &Mutex<HashSet<String>>, texts: &[&str]) -> bool {
            let set = set.lock().unwrap();
            texts.iter().any(|text| set.contains(*text))
        }
    }

    #[async_trait]
    impl EmbeddingEngine for RecordingEmbedding {
        async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            self.calls
                .lock()
                .unwrap()
                .push(texts.iter().map(|t| (*t).to_string()).collect());
            if Self::batch_hits(&self.fail_on, texts) {
                return Err(EmbeddingError::InferenceError(
                    "embedding provider down".into(),
                ));
            }
            let mut vectors = self.inner.embed(texts).await?;
            if Self::batch_hits(&self.short_on, texts) {
                vectors.pop();
            }
            Ok(vectors)
        }
        fn dimension(&self) -> usize {
            self.inner.dimension()
        }
        fn batch_size(&self) -> usize {
            self.inner.batch_size()
        }
        fn max_sequence_length(&self) -> usize {
            self.inner.max_sequence_length()
        }
    }

    /// One dataset's stores, wired through the doubles above.
    struct Fixture {
        dataset: Uuid,
        /// Clone of the graph double's inner mock (shared state), for seeding
        /// and inspecting what was read and stored.
        graph: MockGraphDB,
        scripted: Arc<ScriptedGraph>,
        vector: Arc<RecordingVector>,
        embed: Arc<RecordingEmbedding>,
        writes: WriteLog,
    }

    impl Fixture {
        /// Lessons in session [`SESSION`]'s learnings node set, plus corpus
        /// chunks outside it.
        async fn new(lessons: &[&str], corpus: &[&str]) -> Self {
            let graph = MockGraphDB::new();
            seed_graph(&graph, SESSION, lessons, corpus).await;
            Self::with_graph(graph)
        }

        fn with_graph(graph: MockGraphDB) -> Self {
            let writes: WriteLog = Arc::new(Mutex::new(Vec::new()));
            Self {
                dataset: Uuid::new_v4(),
                scripted: Arc::new(ScriptedGraph {
                    inner: graph.clone(),
                    supports: AtomicBool::new(true),
                    unreachable: AtomicBool::new(false),
                    node_load: Mutex::new(NodeLoad::Inner),
                    truth_write: Mutex::new(TruthWrite::Inner),
                    writes: Arc::clone(&writes),
                }),
                graph,
                vector: Arc::new(RecordingVector {
                    inner: MockVectorDB::new(),
                    fail_upsert: AtomicBool::new(false),
                    upserts: AtomicUsize::new(0),
                    writes: Arc::clone(&writes),
                }),
                embed: Arc::new(RecordingEmbedding {
                    inner: MockEmbeddingEngine::deterministic(DIM),
                    fail_on: Mutex::new(HashSet::new()),
                    short_on: Mutex::new(HashSet::new()),
                    calls: Mutex::new(Vec::new()),
                }),
                writes,
            }
        }

        async fn build(&self, k: usize) -> TruthSubspaceResult {
            self.build_with(&[SESSION.to_string()], k, NODE_EMBED_BATCH_SIZE)
                .await
        }

        async fn build_with(
            &self,
            session_ids: &[String],
            k: usize,
            batch_size: usize,
        ) -> TruthSubspaceResult {
            let graph: Arc<dyn GraphDBTrait> = self.scripted.clone();
            let vector: Arc<dyn VectorDB> = self.vector.clone();
            let embed: Arc<dyn EmbeddingEngine> = self.embed.clone();
            cognee_models::dataset_scope::with_dataset(
                self.dataset,
                build_truth_subspace_in_scope(
                    self.dataset,
                    session_ids,
                    graph,
                    vector,
                    embed,
                    k,
                    batch_size,
                ),
            )
            .await
        }

        fn set_node_load(&self, mode: NodeLoad) {
            *self.scripted.node_load.lock().unwrap() = mode;
        }

        fn set_truth_write(&self, mode: TruthWrite) {
            *self.scripted.truth_write.lock().unwrap() = mode;
        }

        fn fail_embedding_on(&self, text: &str) {
            self.embed.fail_on.lock().unwrap().insert(text.to_string());
        }

        fn writes(&self) -> Vec<&'static str> {
            self.writes.lock().unwrap().clone()
        }

        fn upserts(&self) -> usize {
            self.vector.upserts.load(Ordering::SeqCst)
        }

        fn embed_calls(&self) -> Vec<Vec<String>> {
            self.embed.calls.lock().unwrap().clone()
        }

        /// The epochs of the centroids that are live in the vector store.
        async fn live_centroid_epochs(&self, k: usize) -> Vec<i64> {
            load_centroids(&self.vector.inner, &self.dataset.to_string(), k)
                .await
                .unwrap()
                .iter()
                .map(|c| c.truth_epoch)
                .collect()
        }

        async fn truth_state(&self, id: &str) -> Option<NodeTruthState> {
            self.graph
                .get_node_truth_state(&[id.to_string()])
                .await
                .unwrap()
                .remove(id)
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
            add_lesson(graph, session_id, &format!("lesson-{session_id}-{i}"), text).await;
        }

        for (i, text) in corpus_texts.iter().enumerate() {
            graph
                .add_node_raw(json!({
                    "id": format!("corpus-{i}"),
                    "type": DOCUMENT_CHUNK_TYPE,
                    "text": text,
                }))
                .await
                .unwrap();
        }
    }

    /// Add one lesson chunk to `session_id`'s learnings node set.
    async fn add_lesson(graph: &MockGraphDB, session_id: &str, chunk_id: &str, text: &str) {
        graph
            .add_node_raw(json!({
                "id": chunk_id,
                "type": DOCUMENT_CHUNK_TYPE,
                "text": text,
            }))
            .await
            .unwrap();
        graph
            .add_edge(&format!("nodeset-{session_id}"), chunk_id, "contains", None)
            .await
            .unwrap();
    }

    fn assert_errored_containing(out: &TruthSubspaceResult, needle: &str) {
        assert_eq!(out.status, TruthBuildStatus::Errored, "{out:?}");
        let error = out.error.as_deref().expect("errored carries an error");
        assert!(error.contains(needle), "error {error:?} lacks {needle:?}");
        assert_eq!(out.reason, None);
    }

    #[tokio::test]
    async fn no_statements_returns_default() {
        let fx = Fixture::with_graph(MockGraphDB::new()); // empty graph -> no learnings
        let out = fx.build(3).await;
        assert_eq!(out, TruthSubspaceResult::default());
        assert_eq!(out.status, TruthBuildStatus::Completed);
    }

    #[tokio::test]
    async fn the_public_entry_point_runs_the_build() {
        let graph = MockGraphDB::new();
        seed_graph(&graph, SESSION, &["Lesson one"], &["Corpus A"]).await;
        let out = build_truth_subspace(
            Uuid::new_v4(),
            &[SESSION.to_string()],
            Arc::new(graph),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::deterministic(DIM)),
            3,
        )
        .await;
        assert_eq!(out.status, TruthBuildStatus::Completed);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(out.nodes_scored, 2);
    }

    // py: test_unsupported_backend_is_skipped_before_any_embedding_call
    #[tokio::test]
    async fn unsupported_backend_is_skipped_before_any_embedding_call() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.scripted.supports.store(false, Ordering::SeqCst);

        let out = fx.build(3).await;

        assert_eq!(out.status, TruthBuildStatus::Skipped);
        assert_eq!(out.reason.as_deref(), Some(REASON_BACKEND_UNSUPPORTED));
        assert_eq!(out.error, None);
        assert_eq!((out.anchors, out.nodes_scored, out.truth_epoch), (0, 0, 0));
        // Zero embedding calls, zero reads of the learnings, zero writes.
        assert!(fx.embed_calls().is_empty());
        assert!(fx.graph.get_call_log().is_empty(), "no graph read");
        assert!(fx.writes().is_empty(), "no write");
    }

    // A probe error is an unreachable engine, not an unsupported one and not an
    // empty graph: the build errors before reading or embedding anything.
    #[tokio::test]
    async fn unreachable_graph_engine_errors_before_any_read() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.scripted.unreachable.store(true, Ordering::SeqCst);

        let out = fx.build(3).await;

        assert_errored_containing(&out, "graph engine unavailable");
        assert_eq!((out.anchors, out.truth_epoch), (0, 0));
        assert!(fx.embed_calls().is_empty());
        assert!(fx.graph.get_call_log().is_empty(), "no graph read");
        assert!(fx.writes().is_empty());
    }

    #[tokio::test]
    async fn learning_embed_failure_keeps_epoch_n_live() {
        let fx = Fixture::new(&["Coffee matters", "Tea matters"], &[]).await;
        fx.fail_embedding_on("Coffee matters");

        let out = fx.build(3).await;

        assert_errored_containing(&out, "learning embedding failed");
        assert_eq!(out.anchors, 0, "no existing centroids");
        assert_eq!(out.nodes_scored, 0);
        assert!(!out.signature.is_empty(), "signature computed before embed");
        assert_eq!(out.truth_epoch, 0, "previous_epoch");
        assert!(fx.writes().is_empty());
    }

    // Also py: test_unchanged_learnings_keep_the_existing_epoch_and_write_no_centroids
    #[tokio::test]
    async fn fresh_build_bumps_epoch_then_idempotent() {
        let k = 3;
        let fx = Fixture::new(
            &[
                "Alpha lesson",
                "Beta lesson",
                "Gamma lesson",
                "Delta lesson",
            ],
            &["Corpus A"],
        )
        .await;

        let first = fx.build(k).await;
        assert_eq!(first.status, TruthBuildStatus::Completed);
        assert!(first.anchors <= k && first.anchors > 0);
        assert_eq!(first.truth_epoch, 1, "bumped from 0");
        assert!(!first.signature.is_empty());
        assert_eq!(fx.upserts(), 1);

        // Second identical run: centroids unchanged -> epoch 1 stays live, the
        // chunks are re-scored at it, and no centroid write happens. Reset a
        // chunk first so its epoch can only come from the second run.
        fx.graph
            .set_node_truth_state(&HashMap::from([(
                "corpus-0".to_string(),
                NodeTruthState {
                    truth_alignment: vec![],
                    truth_epoch: 0,
                },
            )]))
            .await
            .unwrap();
        let writes_before = fx.writes();
        let second = fx.build(k).await;
        assert_eq!(second.status, TruthBuildStatus::Completed);
        assert_eq!(second.truth_epoch, 1, "epoch unchanged on no-op");
        assert_eq!(second.anchors, first.anchors);
        assert_eq!(second.signature, first.signature);
        assert_eq!(second.nodes_scored, 5);
        assert_eq!(fx.upserts(), 1, "no centroid write when unchanged");
        assert_eq!(
            fx.writes()[writes_before.len()..],
            ["set_node_truth_state"],
            "the second run re-scores the chunks and writes nothing else"
        );
        let rescored = fx.truth_state("corpus-0").await.unwrap();
        assert_eq!(rescored.truth_epoch, 1, "re-scored at the existing epoch");
        assert!(!rescored.truth_alignment.is_empty());
    }

    // DocumentChunk corpus nodes get a length-k truth_alignment with the
    // current epoch.
    #[tokio::test]
    async fn corpus_nodes_get_truth_alignment() {
        let k = 4;
        let fx = Fixture::new(
            &["Lesson one", "Lesson two"],
            &["Corpus chunk A", "Corpus chunk B", "Corpus chunk C"],
        )
        .await;

        let out = fx.build(k).await;
        // 5 DocumentChunk nodes total (2 lessons + 3 corpus) are all scoreable.
        assert_eq!(out.nodes_scored, 5);
        assert_eq!(out.nodes_skipped, 0);
        assert_eq!(out.truth_epoch, 1);

        let ts = fx.truth_state("corpus-0").await.expect("corpus-0 scored");
        assert_eq!(ts.truth_alignment.len(), k);
        assert_eq!(ts.truth_epoch, 1);
    }

    // A dataset whose ONLY DocumentChunks are the lessons still scores them
    // (they ARE DocumentChunks).
    #[tokio::test]
    async fn lessons_only_are_scored() {
        let fx = Fixture::new(&["Only lesson text here"], &[]).await;
        let out = fx.build(3).await;
        assert!(out.anchors > 0);
        assert_eq!(out.nodes_scored, 1, "the single lesson chunk is scored");
    }

    // py: test_chunk_coordinates_are_written_before_the_centroids
    #[tokio::test]
    async fn chunk_coordinates_are_written_before_the_centroids() {
        let k = 3;
        let fx = Fixture::new(&["Lesson one", "Lesson two"], &["Corpus A", "Corpus B"]).await;

        let out = fx.build(k).await;

        assert_eq!(fx.writes(), ["set_node_truth_state", "upsert_centroids"]);
        assert_eq!(out.status, TruthBuildStatus::Completed);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(out.anchors, 2);
        assert_eq!(out.nodes_scored, 4);
        assert_eq!(out.nodes_skipped, 0);
        assert_eq!(fx.truth_state("corpus-1").await.unwrap().truth_epoch, 1);
        assert_eq!(fx.live_centroid_epochs(k).await, [1, 1]);
    }

    // py: test_failed_embedding_batch_skips_those_chunks_instead_of_zero_filling
    #[tokio::test]
    async fn failed_embedding_batch_skips_those_chunks_instead_of_zero_filling() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A", "unembeddable", "Corpus B"]).await;
        fx.fail_embedding_on("unembeddable");

        // One text per batch so a single failing batch maps to a single chunk.
        let out = fx.build_with(&[SESSION.to_string()], 3, 1).await;

        assert_eq!(out.status, TruthBuildStatus::Completed);
        assert_eq!(out.nodes_scored, 3);
        assert_eq!(out.nodes_skipped, 1);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(fx.upserts(), 1);
        assert!(
            fx.truth_state("corpus-1")
                .await
                .unwrap()
                .truth_alignment
                .is_empty(),
            "the failed chunk was never written"
        );
        for survivor in ["corpus-0", "corpus-2"] {
            let state = fx.truth_state(survivor).await.unwrap();
            assert_eq!(state.truth_epoch, 1);
            // Real coordinates for the survivors — never an all-zero row.
            assert!(
                state.truth_alignment.iter().any(|&c| c != 0.0),
                "{survivor}"
            );
        }
    }

    // Rust-only: Python guards the same case at `build.py:161-169`. A batch
    // that comes back short must be skipped whole — zipping it would shift
    // every later vector onto the wrong chunk.
    #[tokio::test]
    async fn short_embedding_batch_is_skipped_not_misaligned() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A", "Corpus B"]).await;
        fx.embed.short_on.lock().unwrap().insert("Corpus A".into());

        let batch = NODE_EMBED_BATCH_SIZE; // all three chunks in one batch
        let out = fx.build_with(&[SESSION.to_string()], 3, batch).await;

        assert_errored_containing(&out, "no node embeddings available");
        assert_eq!(out.nodes_skipped, 3);
        assert_eq!(out.truth_epoch, 0);
        assert!(fx.writes().is_empty(), "nothing written, epoch 0 live");
    }

    // py: test_all_embeddings_failing_keeps_epoch_n_live
    #[tokio::test]
    async fn all_embeddings_failing_keeps_epoch_n_live() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.fail_embedding_on("Corpus A");

        let out = fx.build(3).await;

        assert_errored_containing(&out, "no node embeddings available");
        assert_eq!(out.truth_epoch, 0);
        assert_eq!(out.anchors, 0);
        assert_eq!(out.nodes_scored, 0);
        assert_eq!(
            out.nodes_skipped, 2,
            "lesson and corpus chunk share the batch"
        );
        assert!(fx.writes().is_empty());
    }

    // py: test_failed_chunk_write_leaves_epoch_n_live
    #[tokio::test]
    async fn failed_chunk_write_leaves_epoch_n_live() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.set_truth_write(TruthWrite::Fail);

        let out = fx.build(3).await;

        assert_errored_containing(&out, "graph write failed");
        assert_eq!(out.truth_epoch, 0);
        assert_eq!(out.anchors, 0);
        assert_eq!(out.nodes_scored, 0);
        assert_eq!(fx.writes(), ["set_node_truth_state"], "no centroid commit");
    }

    // py: test_failed_node_load_leaves_epoch_n_live
    #[tokio::test]
    async fn failed_node_load_leaves_epoch_n_live() {
        let fx = Fixture::new(&["Lesson one", "Lesson two"], &[]).await;
        fx.set_node_load(NodeLoad::Fail);

        let out = fx.build(3).await;

        assert_errored_containing(&out, "node load failed");
        assert_eq!(out.truth_epoch, 0);
        assert!(!out.signature.is_empty());
        assert!(fx.writes().is_empty());
        // Only the learnings were embedded; no chunk embedding ran.
        let calls = fx.embed_calls();
        assert_eq!(calls.len(), 1);
        let mut learned = calls[0].clone();
        learned.sort();
        assert_eq!(learned, ["Lesson one", "Lesson two"]);
    }

    // py: test_all_truth_state_writes_refused_skips_the_commit
    #[tokio::test]
    async fn all_truth_state_writes_refused_skips_the_commit() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.set_truth_write(TruthWrite::RefuseAll);

        let out = fx.build(3).await;

        assert_errored_containing(&out, "no truth state written");
        assert_eq!(out.truth_epoch, 0);
        assert_eq!(out.nodes_scored, 0);
        assert_eq!(fx.upserts(), 0);
    }

    // py: test_no_scoreable_chunks_still_commits_the_new_centroids
    #[tokio::test]
    async fn no_scoreable_chunks_still_commits_the_new_centroids() {
        let k = 3;
        let fx = Fixture::new(&["Lesson one", "Lesson two"], &[]).await;
        fx.set_node_load(NodeLoad::Empty);

        let out = fx.build(k).await;

        assert_eq!(out.status, TruthBuildStatus::Completed);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(out.anchors, 2);
        assert_eq!(out.nodes_scored, 0);
        assert_eq!(fx.writes(), ["upsert_centroids"]);
        assert_eq!(fx.live_centroid_epochs(k).await, [1, 1]);
    }

    #[tokio::test]
    async fn no_scoreable_chunks_with_a_failed_commit_keeps_epoch_n_live() {
        let fx = Fixture::new(&["Lesson one"], &[]).await;
        fx.set_node_load(NodeLoad::Empty);
        fx.vector.fail_upsert.store(true, Ordering::SeqCst);

        let out = fx.build(3).await;

        assert_errored_containing(&out, "centroid upsert failed");
        assert_eq!(out.truth_epoch, 0);
        assert_eq!(out.anchors, 0);
    }

    // py: test_failed_centroid_commit_reports_the_previous_epoch
    #[tokio::test]
    async fn failed_centroid_commit_reports_the_previous_epoch() {
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        fx.vector.fail_upsert.store(true, Ordering::SeqCst);

        let out = fx.build(3).await;

        // Chunks were scored at N+1, but the commit failed, so N (here:
        // nothing) stays live.
        assert_eq!(fx.writes(), ["set_node_truth_state", "upsert_centroids"]);
        assert_errored_containing(&out, "vector store down");
        assert_eq!(out.truth_epoch, 0);
        assert_eq!(out.anchors, 0);
        assert_eq!(out.nodes_scored, 2);
    }

    // py: test_failed_chunk_write_keeps_a_nonzero_previous_epoch_live
    #[tokio::test]
    async fn failed_chunk_write_keeps_a_nonzero_previous_epoch_live() {
        let k = 3;
        let fx = Fixture::new(&["Lesson one"], &["Corpus A"]).await;
        let first = fx.build(k).await;
        assert_eq!(
            (first.status, first.truth_epoch),
            (TruthBuildStatus::Completed, 1)
        );
        let live_anchors = first.anchors;

        // A different learning basis, so the build computes changed centroids
        // at epoch 2 — and the chunk write then fails.
        add_lesson(&fx.graph, SESSION, "lesson-new", "A brand new lesson").await;
        fx.set_truth_write(TruthWrite::Fail);
        let out = fx.build(k).await;

        assert_errored_containing(&out, "graph write failed");
        assert_eq!(out.truth_epoch, 1, "epoch 1 — not 0, not 2");
        assert_eq!(out.anchors, live_anchors);
        assert_eq!(fx.upserts(), 1, "only the first run committed");
        assert!(fx.live_centroid_epochs(k).await.iter().all(|&e| e == 1));
    }

    // session_ids == &[] exercises build_centroids_from_learning_vectors (the
    // rebuild branch), not extend. With no sessions the fetch falls back to
    // the TRUTH_NODE_SET ("session_learnings") node set name.
    #[tokio::test]
    async fn empty_session_ids_uses_rebuild_branch() {
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
        let fx = Fixture::with_graph(graph);

        // Empty session_ids -> rebuild branch; still builds centroids + scores.
        let out = fx.build_with(&[], 3, NODE_EMBED_BATCH_SIZE).await;
        assert!(out.anchors > 0);
        assert_eq!(out.truth_epoch, 1);
        assert_eq!(out.nodes_scored, 2, "both default-set lessons scored");
    }

    // Signature-drop quirk: when rebuilt centroids are empty the result is
    // default() with an EMPTY signature. Rebuilt is empty only when learning
    // vectors produce no slots, which `extend`/`build` never do for a
    // non-empty list, so the quirk is verified in truth-subspace; here we
    // assert the positive complement — a successful build keeps the signature.
    #[tokio::test]
    async fn successful_build_keeps_signature() {
        let fx = Fixture::new(&["Kept lesson"], &[]).await;
        let out = fx.build(3).await;
        assert!(!out.signature.is_empty());
    }

    // Node-scoring text/name truthiness fallback (`build.py:93-98`). A
    // DocumentChunk whose `text` is empty-string or null but whose `name` is
    // non-empty is scored via `name`; a chunk whose text AND name are both
    // falsy is skipped. Mirrors Python's `text or name or ""`.
    #[tokio::test]
    async fn falsy_text_falls_back_to_name_for_scoring() {
        // One real lesson so learnings/centroids build.
        let fx = Fixture::new(&["A lesson"], &[]).await;
        for node in [
            json!({"id": "empty-text-named", "type": DOCUMENT_CHUNK_TYPE,
                   "text": "", "name": "Named via name field"}),
            json!({"id": "null-text-named", "type": DOCUMENT_CHUNK_TYPE,
                   "text": Value::Null, "name": "Also named"}),
            json!({"id": "both-empty", "type": DOCUMENT_CHUNK_TYPE,
                   "text": "", "name": ""}),
        ] {
            fx.graph.add_node_raw(node).await.unwrap();
        }

        let out = fx.build(3).await;
        // 1 lesson + 2 name-fallback chunks scored; the both-empty chunk
        // skipped (and not counted as an embedding skip).
        assert_eq!(out.nodes_scored, 3);
        assert_eq!(out.nodes_skipped, 0);

        assert_eq!(
            fx.truth_state("empty-text-named")
                .await
                .unwrap()
                .truth_epoch,
            1
        );
        assert_eq!(
            fx.truth_state("null-text-named").await.unwrap().truth_epoch,
            1
        );
        assert_eq!(
            fx.truth_state("both-empty").await.unwrap().truth_epoch,
            -1,
            "both text and name falsy -> never scored"
        );
    }

    #[test]
    fn status_names_match_python() {
        assert_eq!(TruthBuildStatus::Completed.as_str(), "completed");
        assert_eq!(TruthBuildStatus::Skipped.as_str(), "skipped");
        assert_eq!(TruthBuildStatus::Errored.as_str(), "errored");
    }
}
