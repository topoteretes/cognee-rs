use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, NeighborhoodScope};
use cognee_models::{INTERNAL_PROPERTY, marks_internal};
use cognee_vector::{SearchResult, VectorDB, VectorDBError};
use serde_json::Value;
use tracing::debug;
use uuid::Uuid;

use crate::graph_retrieval::triplet_ranking::element_weight;
use crate::graph_retrieval::{DEFAULT_ELEMENT_WEIGHT, TripletElement, rank_edge_score};
use crate::types::SearchError;
use crate::utils::edge_type_point_id;

const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

/// Hop radius of the default graph load around the vector-search seed set,
/// used when [`GraphRetrievalConfig::neighborhood_depth`] is unset.
///
/// Must stay at 1 for the scoped load to be **result-identical** to loading the
/// whole graph. The edge ranking below keeps an edge when *at least one*
/// endpoint is a seed. `get_neighborhood(seeds, 1)` returns every edge whose
/// endpoints both lie in `seeds ∪ N(seeds)`, which is a superset of that set:
/// if `src` is a seed then `tgt ∈ N(seeds)` by adjacency, so the edge is
/// returned; extra edges between two non-seed neighbors are then dropped by the
/// same filter that already ran against the full graph. Node property lookups
/// are unaffected because every endpoint of a kept edge is in the resolved set.
///
/// Raising this would pull in edges the filter discards anyway — more IO for
/// identical output. Lowering it to 0 is backend-dependent (the trait's default
/// BFS returns no edges at depth 0, while the Postgres and Ladybug overrides
/// return seed-to-seed edges) and would drop edges with one non-seed endpoint.
/// A caller-set `neighborhood_depth` takes a different path altogether (see
/// that field).
const NEIGHBORHOOD_DEPTH: usize = 1;

/// Default cosine distance assigned to graph elements (nodes or edges) that have no
/// vector match for the current query. Matches Python's `triplet_distance_penalty`
/// default of 6.5 in
/// `cognee/modules/retrieval/utils/brute_force_triplet_search.py:56,227`.
///
/// Like every element distance it is scaled by `2 - importance_weight` before
/// it enters a triplet's score, so an unmatched element contributes 9.75 at the
/// default importance of 0.5 (see [`rank_edge_score`]).
pub const DEFAULT_TRIPLET_DISTANCE_PENALTY: f32 = 6.5;

/// Collections searched to find candidate graph nodes and edge-type distances.
/// Each entry is (data_type, field_name).
///
/// Mirrors Python's dynamic enumeration of all DataPoint subclass index collections
/// (graph_completion_retriever.py:88-99). Python reflects over every DataPoint
/// subclass at query time and includes `Triplet_text` when memify has populated it.
/// Rust uses a static list that covers the same set; the per-collection
/// `has_collection` guard below ensures collections absent from the store are skipped.
///
/// `Triplet_text` is included so that after `memify` runs, triplet vectors influence
/// graph search ranking exactly as they do in Python.
///
/// Note on Triplet point IDs: Triplet vector points are identified by
/// `generate_node_id(start_id + relationship_name + end_id)`, which is NOT the same
/// as any graph node ID. Python's `map_vector_distances_to_graph_nodes` silently skips
/// hits whose IDs don't match a known graph node (CogneeGraph.py:428-429). Rust does
/// the same: the candidate_node_ids set collects point IDs from all collections, and
/// only IDs that match an actual graph node endpoint end up contributing to edge scores.
/// Triplet hits that don't match a graph node ID are therefore harmless — they add the
/// point ID to candidate_node_ids but never match any graph edge endpoint, so no
/// spurious edges are surfaced.
const SEARCH_COLLECTIONS: [(&str, &str); 6] = [
    ("Entity", "name"),
    ("TextSummary", "text"),
    ("EntityType", "name"),
    ("DocumentChunk", "text"),
    ("EdgeType", "relationship_name"),
    ("Triplet", "text"),
];

#[derive(Debug, Clone)]
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    /// Default cosine distance used for nodes/edges not found in vector search.
    /// Matches Python's `triplet_distance_penalty` semantics (default 6.5).
    pub triplet_distance_penalty: f32,
    /// How much per-node `feedback_weight` values influence triplet ranking.
    /// Must be in [0.0, 1.0]. 0.0 (default) means pure similarity-based ranking.
    pub feedback_influence: f32,
    /// Filter graph to nodes of this type before scoring.
    /// When combined with `node_name`, calls `get_nodeset_subgraph` instead of
    /// the seed-scoped `get_neighborhood` load.
    pub node_type: Option<String>,
    /// Filter graph to nodes with these names (paired with `node_type`).
    pub node_name: Option<Vec<String>>,
    /// "OR" (default): include neighbors of ANY named node.
    /// "AND": include only neighbors connected to ALL named nodes.
    pub node_name_filter_operator: String,
    /// Opt-in hub-expansion cap (`search.triplet_neighbor_cap`,
    /// `COGNEE_TRIPLET_NEIGHBOR_CAP`): `Some(n)` keeps, per
    /// vector-search seed, every neighbour that is itself a seed plus at most `n`
    /// other neighbours (see [`NeighborhoodScope::max_neighbors_per_seed`]).
    /// Unset (the default) keeps the full depth-1 neighbourhood, which is
    /// result-identical to a full-graph load. A cap truncates hub fan-out and so
    /// CHANGES results: edges from a hub seed to its dropped neighbours never
    /// reach the ranking (they carry the non-seed penalty, so they rank below
    /// seed-to-seed edges, but they can still make the top k).
    /// `None` and `Some(0)` both mean uncapped. Only applies at depth 1, so
    /// it has no effect when `neighborhood_depth` is above 1.
    pub neighbor_cap: Option<usize>,
    /// k-hop radius of Python's neighborhood projection
    /// (`neighborhood_depth`, `brute_force_triplet_search.py`
    /// `get_memory_fragment`). `None` (the default) keeps the depth-1 load
    /// described on [`NEIGHBORHOOD_DEPTH`]. `Some(depth)` projects the
    /// `depth`-hop neighborhood of the nearest
    /// [`neighborhood_seed_top_k`](Self::neighborhood_seed_top_k) seeds,
    /// gives the nodes that expansion discovered a real distance through
    /// [`VectorDB::score_by_ids`], and ranks **every** projected edge — not
    /// only those touching a seed — exactly as Python ranks every edge of its
    /// fragment. Must be at least 1.
    pub neighborhood_depth: Option<usize>,
    /// How many seeds, nearest first, the neighborhood projection expands
    /// from. `None` means all of them: Python's public `search()` passes
    /// `None` through, and `relevant_ids[:None]` is the whole list. Ignored
    /// without `neighborhood_depth`. Must be at least 1.
    pub neighborhood_seed_top_k: Option<usize>,
    /// The requesting user's effective `prefers` weights by node id (Python's
    /// `personal_weights`, from [`crate::user_preferences`]). Empty — the
    /// default — ranks exactly as an un-personalized search.
    pub personal_weights: Arc<HashMap<String, f32>>,
    /// The most a personal weight may move an element's distance
    /// (`personalization_influence`); see [`rank_edge_score`].
    pub personal_influence: f32,
}

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: DEFAULT_TRIPLET_DISTANCE_PENALTY,
            feedback_influence: 0.0,
            node_type: None,
            node_name: None,
            node_name_filter_operator: "OR".to_string(),
            neighbor_cap: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            personal_weights: Arc::default(),
            personal_influence: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    /// Total triplet distance (lower = better match): the sum of the source
    /// node's, target node's and edge's cosine distances, each scaled by
    /// `2 - importance_weight` and blended with its `feedback_weight` (see
    /// [`rank_edge_score`]).
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
    /// Text content of the source node (present on DocumentChunk nodes).
    pub source_text: Option<String>,
    /// Text content of the target node (present on DocumentChunk nodes).
    pub target_text: Option<String>,
    /// Description of the source node (present on Entity nodes).
    pub source_description: Option<String>,
    /// Description of the target node (present on Entity nodes).
    pub target_description: Option<String>,
    /// The edge's nonblank `edge_text` property — its natural-language
    /// description, rendered after the relationship in the graph context.
    pub edge_text: Option<String>,
}

#[tracing::instrument(
    name = "cognee.retrieval.graph_search",
    skip(graph_db, vector_db, embedding_engine, config),
    fields(
        cognee.result.count = tracing::field::Empty,
    )
)]
pub async fn brute_force_triplet_search(
    query: &str,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    graph_db: &dyn GraphDBTrait,
    config: &GraphRetrievalConfig,
) -> Result<Vec<RankedGraphEdge>, SearchError> {
    validate_config(config)?;

    let load = GraphLoad::resolve(config);
    if load == GraphLoad::NoSeeds {
        // Decided by the config alone, so before paying for the embedding and
        // the vector searches.
        tracing::warn!(
            target: "cognee::search",
            "neighborhood_depth requires seed node ids from vector search, which a \
             node_name filter disables — returning no triplets"
        );
        tracing::Span::current().record("cognee.result.count", 0u64);
        return Ok(vec![]);
    }

    let query_vector = embed_query(embedding_engine, query).await?;

    let mut hits = QueryHits::default();
    let mut node_dataset_ids = HashMap::<String, String>::new();

    let searched = search_collections(
        vector_db,
        &SEARCH_COLLECTIONS,
        |data_type, field_name| async {
            Ok(Some(
                vector_db
                    .search_similar(
                        data_type,
                        field_name,
                        &query_vector,
                        config.wide_search_top_k,
                    )
                    .await?,
            ))
        },
    )
    .await?;
    // The node collections that exist, in search order: the neighborhood
    // projection re-scores its expansion against exactly these (Python
    // iterates `vector_search.node_distances`).
    let mut node_collections: Vec<(&str, &str)> = Vec::new();
    for (data_type, field_name, results) in searched {
        if !is_edge_type_collection(data_type, field_name) {
            node_collections.push((data_type, field_name));
        }
        hits.absorb(data_type, field_name, results, &mut node_dataset_ids);
    }

    // The vector-search hits, before any expansion adds distances.
    let candidate_node_ids: HashSet<String> = hits.node_distances.keys().cloned().collect();
    if candidate_node_ids.is_empty() {
        debug!("no candidate nodes found from vector search — returning empty");
        tracing::Span::current().record("cognee.result.count", 0u64);
        return Ok(vec![]);
    }

    tracing::debug!(
        target: "cognee::search",
        wide_search_results = candidate_node_ids.len(),
        "Vector search complete"
    );

    let (graph_nodes, graph_edges) = if let GraphLoad::NodeSet {
        node_type,
        node_names,
    } = load
    {
        graph_db
            .get_nodeset_subgraph(node_type, node_names, &config.node_name_filter_operator)
            .await?
    } else {
        // Scope the load to the seed set's neighborhood instead of pulling the
        // whole graph. This is result-identical to `get_graph_data()`, not merely
        // narrower — see `NEIGHBORHOOD_DEPTH` for the argument.
        //
        // Python scopes here unconditionally: its `relevant_node_ids` is only
        // `None` when `wide_search_limit is None`, but `wide_search_top_k`
        // defaults to 100, so the id filter is always populated and
        // `CogneeGraph._get_full_or_id_filtered_graph` always takes the scoped
        // branch (brute_force_triplet_search.py:156-160, CogneeGraph.py:123-151).
        // The full-graph branch is reached only by batch search, which is
        // [`brute_force_triplet_search_batch`].
        //
        // Deliberately NOT `get_id_filtered_graph_data`, despite the name match
        // with Python's method: this trait's version keeps an edge only when
        // *both* endpoints are in the id set (traits.rs), which would silently
        // drop every edge with one non-seed endpoint and change ranking. Python's
        // Neo4j implementation uses OR semantics, which `get_neighborhood`
        // reproduces.
        // Sorted, not just collected: `candidate_node_ids` is a `HashSet`, so its
        // iteration order varies per process, and the trait's default BFS walks
        // the frontier in seed order — so an unsorted seed list makes that
        // backend's edge order vary run to run. Cheap insurance; the ranking
        // sort's tie-break is what actually guarantees a stable top-k, since
        // neither real adapter's row order tracks the seed order (Ladybug
        // re-collects into its own `HashSet` and returns storage order; the
        // Postgres CTE has no ORDER BY).
        let (seed_ids, depth) = match load {
            // Python's neighborhood projection: the nearest `seed_top_k` seeds.
            GraphLoad::Projection { depth, seed_top_k } => {
                (nearest_seeds(&hits.node_distances, seed_top_k), depth)
            }
            _ => {
                let mut seed_ids: Vec<String> = candidate_node_ids.iter().cloned().collect();
                seed_ids.sort_unstable();
                (seed_ids, NEIGHBORHOOD_DEPTH)
            }
        };
        match config.neighbor_cap.filter(|n| *n > 0) {
            None => graph_db.get_neighborhood(&seed_ids, depth).await?,
            Some(cap) => {
                let scope = NeighborhoodScope {
                    max_neighbors_per_seed: Some(cap),
                    ..NeighborhoodScope::default()
                };
                graph_db
                    .get_neighborhood_scoped(&seed_ids, depth, &scope)
                    .await?
            }
        }
    };

    if load.ranks_every_loaded_edge() {
        score_neighborhood_expansion(
            vector_db,
            &query_vector,
            &node_collections,
            &graph_nodes,
            &candidate_node_ids,
            &mut hits.node_distances,
            &mut node_dataset_ids,
        )
        .await?;
    }

    let graph = ProjectedGraph::new(graph_nodes, graph_edges);
    // Only edges with at least one endpoint found in vector search are ranked;
    // the neighborhood projection ranks every edge it loaded instead, as Python
    // ranks every edge of its fragment — otherwise edges past the first hop
    // could never surface.
    let seed_filter = (!load.ranks_every_loaded_edge()).then_some(&candidate_node_ids);
    let ranked_edges = graph.rank(&hits, seed_filter, &node_dataset_ids, config);
    tracing::Span::current().record("cognee.result.count", ranked_edges.len() as u64);
    Ok(ranked_edges)
}

/// Graph triplet search for several queries over **one** graph load: one ranked
/// list per query, in query order. Port of Python's batch mode
/// (`brute_force_triplet_search(query_batch=...)`), which the decomposition
/// retriever uses for its sub-questions.
///
/// Python's batch mode differs from single-query search, and so does this:
///
/// * every vector collection is searched **without a limit** (Python's
///   `batch_search(limit=None)` reads the whole collection), so every node
///   and edge type gets its real distance to every query;
/// * the **whole graph** is loaded (`get_graph_data`) — or the NodeSet
///   subgraph with `node_type` + `node_name` — since there is no seed set to
///   scope it by, and every loaded edge is ranked;
/// * `wide_search_top_k` and `node_name` play no part in the vector search,
///   and `neighborhood_depth` yields no triplets: the projection needs seed
///   ids, which batch mode never has (Python's `get_memory_fragment` swallows
///   the resulting `ValueError`);
/// * `neighbor_cap` (`search.triplet_neighbor_cap`) does not apply either:
///   it bounds a seed's neighbourhood, and batch mode loads the whole graph
///   (Python has no such cap);
/// * a collection that disappears while the batch runs counts as empty, as
///   Python's `_search_batch_collection` treats `CollectionNotFoundError`.
///
/// One deliberate saving over Python: `Triplet_text` is not searched. Its point
/// ids are never graph node ids, so its rows cannot change any ranking, and an
/// unlimited read of it — about one row per edge once memify has run — per
/// query would be pure I/O.
///
/// That makes it far more expensive than a single search on a large store.
/// Callers with one query should use [`brute_force_triplet_search`], as
/// Python's `get_triplets_batch` does.
///
/// # Errors
/// [`SearchError::InvalidInput`] for an empty batch or a blank query (Python's
/// `validate_queries`) and for the configuration errors
/// [`brute_force_triplet_search`] rejects.
#[tracing::instrument(
    name = "cognee.retrieval.graph_search_batch",
    skip(queries, graph_db, vector_db, embedding_engine, config),
    fields(
        cognee.retrieval.batch_size = queries.len(),
        cognee.result.count = tracing::field::Empty,
    )
)]
pub async fn brute_force_triplet_search_batch(
    queries: &[&str],
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    graph_db: &dyn GraphDBTrait,
    config: &GraphRetrievalConfig,
) -> Result<Vec<Vec<RankedGraphEdge>>, SearchError> {
    validate_query_batch(queries)?;
    validate_config(config)?;
    let empty = || vec![Vec::new(); queries.len()];

    if config.neighborhood_depth.is_some() {
        tracing::warn!(
            target: "cognee::search",
            "neighborhood_depth requires seed node ids from vector search, which batch \
             search never has — returning no triplets"
        );
        tracing::Span::current().record("cognee.result.count", 0u64);
        return Ok(empty());
    }

    // One request for the whole batch (Python embeds its `query_texts` at
    // once), each query shaped as `embed_query` shapes it.
    let query_vectors = embedding_engine.embed_queries(queries).await?;
    if query_vectors.len() != queries.len() {
        return Err(SearchError::EmbeddingError(format!(
            "embedding engine returned {} vectors for {} queries",
            query_vectors.len(),
            queries.len()
        )));
    }

    let collections: Vec<(&'static str, &'static str)> = SEARCH_COLLECTIONS
        .into_iter()
        .filter(|(data_type, _)| *data_type != "Triplet")
        .collect();
    let searched = search_collections(vector_db, &collections, |data_type, field_name| {
        let query_vectors = &query_vectors;
        async move {
            let results = async {
                let size = vector_db.collection_size(data_type, field_name).await?;
                if size == 0 {
                    return Ok(Vec::new());
                }
                vector_db
                    .batch_search_similar(data_type, field_name, query_vectors, size)
                    .await
            }
            .await;
            match results {
                // Dropped since `has_collection` saw it: empty, as Python's
                // `_search_batch_collection` answers a `CollectionNotFoundError`.
                Err(VectorDBError::CollectionNotFound(_)) => Ok(None),
                Err(error) => Err(error.into()),
                Ok(results) if results.is_empty() => Ok(None),
                // One list per query, or the distances would land on the wrong
                // query — `batch_search_similar` is an overridable method.
                Ok(results) if results.len() != query_vectors.len() => {
                    Err(SearchError::VectorError(format!(
                        "batch_search_similar on {data_type}/{field_name} returned {} result \
                         lists for {} queries",
                        results.len(),
                        query_vectors.len()
                    )))
                }
                Ok(results) => Ok(Some(results)),
            }
        }
    })
    .await?;

    let mut hits: Vec<QueryHits> = (0..queries.len()).map(|_| QueryHits::default()).collect();
    let mut node_dataset_ids = HashMap::<String, String>::new();
    for (data_type, field_name, per_query) in searched {
        for (query_hits, results) in hits.iter_mut().zip(per_query) {
            query_hits.absorb(data_type, field_name, results, &mut node_dataset_ids);
        }
    }

    // Python's `has_results`: any node or edge hit for any query.
    if hits.iter().all(QueryHits::is_empty) {
        debug!("no vector results for any query in the batch — returning empty");
        tracing::Span::current().record("cognee.result.count", 0u64);
        return Ok(empty());
    }

    let (graph_nodes, graph_edges) = match GraphLoad::resolve(config) {
        GraphLoad::NodeSet {
            node_type,
            node_names,
        } => {
            graph_db
                .get_nodeset_subgraph(node_type, node_names, &config.node_name_filter_operator)
                .await?
        }
        _ => graph_db.get_graph_data().await?,
    };
    let graph = ProjectedGraph::new(graph_nodes, graph_edges);

    let ranked: Vec<Vec<RankedGraphEdge>> = hits
        .iter()
        .map(|query_hits| graph.rank(query_hits, None, &node_dataset_ids, config))
        .collect();
    tracing::Span::current().record(
        "cognee.result.count",
        ranked.iter().map(Vec::len).sum::<usize>() as u64,
    );
    Ok(ranked)
}

/// Python's `validate_queries` for a batch: non-empty, and no blank query.
pub(crate) fn validate_query_batch(queries: &[&str]) -> Result<(), SearchError> {
    if queries.is_empty() {
        return Err(SearchError::InvalidInput(
            "query_batch must be a non-empty list of strings.".to_string(),
        ));
    }
    if queries.iter().any(|query| query.trim().is_empty()) {
        return Err(SearchError::InvalidInput(
            "All items in query_batch must be non-empty strings.".to_string(),
        ));
    }
    Ok(())
}

/// Search every collection of `collections` that exists, concurrently (one
/// pooled connection each), and return what `search` found in collection
/// order — so merging the results is exactly the sequential outcome,
/// including first-wins dataset ids. A collection that does not exist, or for
/// which `search` answers `None`, is left out.
async fn search_collections<T, F, Fut>(
    vector_db: &dyn VectorDB,
    collections: &[(&'static str, &'static str)],
    search: F,
) -> Result<Vec<(&'static str, &'static str, T)>, SearchError>
where
    F: Fn(&'static str, &'static str) -> Fut,
    Fut: Future<Output = Result<Option<T>, SearchError>>,
{
    let searches = collections.iter().map(|&(data_type, field_name)| {
        let search = &search;
        async move {
            if !vector_db.has_collection(data_type, field_name).await? {
                debug!("vector collection {data_type}/{field_name} does not exist — skipping");
                return Ok(None);
            }
            search(data_type, field_name).await
        }
    });
    let mut found = Vec::with_capacity(collections.len());
    for (&(data_type, field_name), result) in collections
        .iter()
        .zip(futures::future::join_all(searches).await)
    {
        if let Some(results) = result? {
            found.push((data_type, field_name, results));
        }
    }
    Ok(found)
}

/// The configuration checks both search modes share.
fn validate_config(config: &GraphRetrievalConfig) -> Result<(), SearchError> {
    if config.feedback_influence < 0.0 || config.feedback_influence > 1.0 {
        return Err(SearchError::InvalidInput(
            "feedback_influence must be in range [0.0, 1.0]".to_string(),
        ));
    }

    validate_neighborhood(config.neighborhood_depth, config.neighborhood_seed_top_k)?;

    let op = config.node_name_filter_operator.to_uppercase();
    if op != "AND" && op != "OR" {
        return Err(SearchError::InvalidInput(format!(
            "Invalid node_name_filter_operator: {:?}. Must be AND or OR.",
            config.node_name_filter_operator
        )));
    }
    Ok(())
}

async fn embed_query(
    embedding_engine: &dyn EmbeddingEngine,
    query: &str,
) -> Result<Vec<f32>, SearchError> {
    embedding_engine
        .embed_query(query)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            SearchError::InvalidInput("embedding engine returned no vectors".to_string())
        })
}

/// One query's vector-search distances, keyed the way the ranking looks them up.
#[derive(Debug, Default)]
struct QueryHits {
    /// node id -> cosine distance (lower = better), the nearest across
    /// collections.
    node_distances: HashMap<String, f32>,
    /// EdgeType vector-row point id -> cosine distance (lower = better).
    ///
    /// NOT keyed by the bare `relationship_name`. cognify builds one EdgeType
    /// row per edge *retrieval text* — the nonblank `edge_text` property
    /// falling back to the relationship name (`EdgeType::retrieval_text`) —
    /// and both the row's point id and its `relationship_name` metadata carry
    /// that retrieval text, not the relation label. Since `fact_extraction`
    /// asks the LLM for a description on every edge, keying on the label made
    /// the lookup miss for essentially every edge, so every edge silently took
    /// the `triplet_distance_penalty` and edge vectors never influenced
    /// ranking at all.
    ///
    /// Python matches by point id for the same reason: `CogneeGraph.add_edge`
    /// stamps `edge_type_id = EdgeType.id_for(edge_text or relationship_type)`
    /// on each edge (`CogneeGraph.py:58-61`) and
    /// `map_vector_distances_to_graph_edges` looks the hit up by
    /// `str(result.id)` (`CogneeGraph.py:392`). Rust's hybrid lane already did
    /// the same via `connection_edge_type_id`. The ranking recomputes the id
    /// with the shared `edge_type_point_id` helper.
    edge_type_distances: HashMap<String, f32>,
}

impl QueryHits {
    /// Merge one collection's search results for this query. A node's dataset
    /// id is first-wins across collections (and, in a batch, across queries:
    /// it is the row's metadata, the same whichever query found it).
    fn absorb(
        &mut self,
        data_type: &str,
        field_name: &str,
        results: Vec<SearchResult>,
        node_dataset_ids: &mut HashMap<String, String>,
    ) {
        for result in results {
            // Convert cosine similarity to cosine distance: distance = 1 - similarity
            let distance = 1.0 - result.score;

            if is_edge_type_collection(data_type, field_name) {
                // The row's point id IS `EdgeType::deterministic_id(retrieval_text)`
                // (cognify builds these with `EdgeType::new_deterministic`), which is
                // exactly what the ranking recomputes per graph edge. Take it
                // straight off the hit rather than re-deriving it from the
                // `relationship_name` metadata: identical in value, but it is the
                // point's actual identity and matches Python's `str(result.id)`.
                //
                // Deliberately NOT a node distance: an EdgeType id is never a graph
                // node id, and seeding the node set with it would widen the
                // neighborhood load for nothing.
                keep_nearest(
                    &mut self.edge_type_distances,
                    result.id.to_string(),
                    distance,
                );
            } else {
                // Node distances keyed by vector point ID.
                // Use min to merge across collections (lower distance = better match).
                let node_id = result.id.to_string();
                keep_nearest(&mut self.node_distances, node_id.clone(), distance);
                if let Some(dataset_id) = result.metadata.get("dataset_id").and_then(|v| v.as_str())
                {
                    node_dataset_ids
                        .entry(node_id)
                        .or_insert_with(|| dataset_id.to_string());
                }
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.node_distances.is_empty() && self.edge_type_distances.is_empty()
    }
}

/// A loaded graph, projected the way Python's `CogneeGraph` projects it, ready
/// to rank against one or more queries' distances.
struct ProjectedGraph {
    edges: Vec<EdgeData>,
    node_names: HashMap<String, String>,
    node_texts: HashMap<String, String>,
    node_descriptions: HashMap<String, String>,
    /// node id -> (importance_weight, feedback_weight)
    node_weights: HashMap<String, (f32, f32)>,
    /// Internal nodes (Python's `InternalDataPoint`, e.g. per-user preference
    /// state in a graph shared with the Python SDK) are never projected, and
    /// an edge touching one is dropped with them (`CogneeGraph.py:192-232`).
    internal_node_ids: HashSet<String>,
}

impl ProjectedGraph {
    fn new(nodes: Vec<GraphNode>, edges: Vec<EdgeData>) -> Self {
        let mut graph = Self {
            edges,
            node_names: HashMap::new(),
            node_texts: HashMap::new(),
            node_descriptions: HashMap::new(),
            node_weights: HashMap::new(),
            internal_node_ids: HashSet::new(),
        };
        for (node_id, properties) in nodes {
            if marks_internal(properties.get(INTERNAL_PROPERTY)) {
                graph.internal_node_ids.insert(node_id);
                continue;
            }
            let name = properties
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(node_id.as_str())
                .to_string();
            graph.node_names.insert(node_id.clone(), name);

            if let Some(text) = properties.get("text").and_then(|v| v.as_str()) {
                graph.node_texts.insert(node_id.clone(), text.to_string());
            }
            if let Some(desc) = properties.get("description").and_then(|v| v.as_str()) {
                graph
                    .node_descriptions
                    .insert(node_id.clone(), desc.to_string());
            }
            // Python only projects `feedback_weight` when feedback influences the
            // ranking; reading it always is equivalent, since the blend ignores
            // it at zero influence.
            graph.node_weights.insert(
                node_id,
                (
                    element_weight(properties.get("importance_weight")),
                    element_weight(properties.get("feedback_weight")),
                ),
            );
        }
        graph
    }

    fn node_element(
        &self,
        node_id: &str,
        distance: f32,
        config: &GraphRetrievalConfig,
    ) -> TripletElement {
        let (importance_weight, feedback_weight) = self
            .node_weights
            .get(node_id)
            .copied()
            .unwrap_or((DEFAULT_ELEMENT_WEIGHT, DEFAULT_ELEMENT_WEIGHT));
        TripletElement {
            distance,
            importance_weight,
            feedback_weight,
            personal_weight: config.personal_weights.get(node_id).copied(),
        }
    }

    /// The `top_k` edges for one query's `hits`, best first. With
    /// `seed_filter`, only edges with an endpoint in it are ranked.
    fn rank(
        &self,
        hits: &QueryHits,
        seed_filter: Option<&HashSet<String>>,
        node_dataset_ids: &HashMap<String, String>,
        config: &GraphRetrievalConfig,
    ) -> Vec<RankedGraphEdge> {
        let default_penalty = config.triplet_distance_penalty;

        // Score every candidate edge against borrowed data; the owned
        // `RankedGraphEdge` (names, texts, descriptions, edge text) is built only
        // for the `top_k` that survive the cut below.
        let mut scored = self
            .edges
            .iter()
            .enumerate()
            .filter_map(
                |(index, (source_id, target_id, relationship_name, properties))| {
                    if seed_filter.is_some_and(|seeds| {
                        !seeds.contains(source_id) && !seeds.contains(target_id)
                    }) {
                        return None;
                    }
                    if self.internal_node_ids.contains(source_id)
                        || self.internal_node_ids.contains(target_id)
                    {
                        return None;
                    }

                    // Unmatched nodes get the default penalty distance (not 0.0)
                    let source_dist = hits
                        .node_distances
                        .get(source_id)
                        .copied()
                        .unwrap_or(default_penalty);
                    let target_dist = hits
                        .node_distances
                        .get(target_id)
                        .copied()
                        .unwrap_or(default_penalty);

                    // Recompute the EdgeType vector-row id this edge was indexed under and
                    // look the distance up by that. `edge_type_point_id` returns `None`
                    // only when both the `edge_text` property and the relationship name
                    // are blank — cognify skips building a row for such an edge, so there
                    // is nothing to match and the penalty is correct.
                    // Unmatched edge types get the default penalty distance.
                    let edge_dist =
                        edge_type_point_id(nonblank_edge_text(properties), relationship_name)
                            .and_then(|point_id| hits.edge_type_distances.get(&point_id).copied())
                            .unwrap_or(default_penalty);

                    // Python's edge projection carries `feedback_weight` but never
                    // `importance_weight`, so an edge's importance is the default.
                    let edge = TripletElement {
                        feedback_weight: element_weight(properties.get("feedback_weight")),
                        ..TripletElement::new(edge_dist)
                    };

                    let score = rank_edge_score(
                        self.node_element(source_id, source_dist, config),
                        self.node_element(target_id, target_dist, config),
                        edge,
                        config.feedback_influence,
                        config.personal_influence,
                        default_penalty,
                    );
                    Some((index, score))
                },
            )
            .collect::<Vec<_>>();

        // Sort ascending: lowest total distance = best match (matches Python heapq.nsmallest).
        //
        // Ties break on the edge triple, not on arrival order. `sort_by` is stable, so
        // without an explicit tie-break the winners of a tie are decided by whatever
        // order the graph backend returned its rows in — which no backend guarantees
        // (the Postgres neighborhood CTE has no ORDER BY; Ladybug is storage-ordered)
        // and which `take(top_k)` below then makes user-visible. Ties are the common
        // case rather than a rarity: every endpoint absent from the vector results
        // takes the same `triplet_distance_penalty`, so an `is_a` fan-out around one
        // EntityType hub yields hundreds of exactly-equal scores. Comparing the ids
        // makes the top-k context handed to the LLM identical across runs and across
        // backends.
        //
        // A NaN score (pgvector returns a NaN similarity for a zero-norm row or
        // query) ranks last. `partial_cmp(..).unwrap_or(Equal)` made the comparator
        // intransitive in that case, which the standard sort detects and panics on.
        let rank_key = |score: f32| if score.is_nan() { f32::INFINITY } else { score };
        scored.sort_by(|&(left, left_score), &(right, right_score)| {
            let (left_source, left_target, left_relationship, _) = &self.edges[left];
            let (right_source, right_target, right_relationship, _) = &self.edges[right];
            rank_key(left_score)
                .total_cmp(&rank_key(right_score))
                .then_with(|| left_source.cmp(right_source))
                .then_with(|| left_target.cmp(right_target))
                .then_with(|| left_relationship.cmp(right_relationship))
        });
        scored.truncate(config.top_k);

        scored
            .into_iter()
            .map(|(index, score)| {
                let (source_id, target_id, relationship_name, properties) = &self.edges[index];
                RankedGraphEdge {
                    source_id: source_id.clone(),
                    target_id: target_id.clone(),
                    relationship_name: relationship_name.clone(),
                    score,
                    source_name: self
                        .node_names
                        .get(source_id)
                        .cloned()
                        .unwrap_or_else(|| source_id.clone()),
                    target_name: self
                        .node_names
                        .get(target_id)
                        .cloned()
                        .unwrap_or_else(|| target_id.clone()),
                    dataset_id: node_dataset_ids
                        .get(source_id)
                        .or_else(|| node_dataset_ids.get(target_id))
                        .cloned(),
                    source_text: self.node_texts.get(source_id).cloned(),
                    target_text: self.node_texts.get(target_id).cloned(),
                    source_description: self.node_descriptions.get(source_id).cloned(),
                    target_description: self.node_descriptions.get(target_id).cloned(),
                    edge_text: nonblank_edge_text(properties).map(str::to_string),
                }
            })
            .collect()
    }
}

/// How the graph is loaded around the vector-search hits, and so which loaded
/// edges are ranked — decided once from the config.
#[derive(Debug, PartialEq)]
enum GraphLoad<'a> {
    /// `node_type` + `node_name`: the NodeSet subgraph.
    NodeSet {
        node_type: &'a str,
        node_names: &'a [String],
    },
    /// The default: the depth-1 neighbourhood of every hit (see
    /// [`NEIGHBORHOOD_DEPTH`]).
    SeedNeighborhood,
    /// Python's neighborhood projection (`neighborhood_depth`).
    Projection {
        depth: usize,
        seed_top_k: Option<usize>,
    },
    /// `neighborhood_depth` with `node_name`. Python's projection needs seed
    /// ids, and `node_name` turns its seed search off (`wide_search_limit` is
    /// `None` whenever `node_name` is set, so `relevant_node_ids` is `None`);
    /// `get_memory_fragment` then raises, catches its own `ValueError` and
    /// returns an empty fragment — an empty result, not an error.
    NoSeeds,
}

impl<'a> GraphLoad<'a> {
    fn resolve(config: &'a GraphRetrievalConfig) -> Self {
        if let Some(depth) = config.neighborhood_depth {
            return if config.node_name.is_some() {
                Self::NoSeeds
            } else {
                Self::Projection {
                    depth,
                    seed_top_k: config.neighborhood_seed_top_k,
                }
            };
        }
        match (config.node_type.as_deref(), config.node_name.as_deref()) {
            (Some(node_type), Some(node_names)) if !node_names.is_empty() => Self::NodeSet {
                node_type,
                node_names,
            },
            _ => Self::SeedNeighborhood,
        }
    }

    /// Whether every loaded edge is ranked — and the nodes the load added
    /// re-scored — rather than only edges with a vector-search hit as an
    /// endpoint. Only the projection does: Python ranks every edge of its
    /// fragment, and without it edges past the first hop could never surface.
    fn ranks_every_loaded_edge(&self) -> bool {
        matches!(self, Self::Projection { .. })
    }
}

/// Merge `distance` into `map[id]`, keeping the nearer one. A NaN (pgvector's
/// similarity for a zero-norm row) never displaces a real distance and is
/// always displaced by one, so a node scored in several collections ranks on
/// its best real match.
fn keep_nearest(map: &mut HashMap<String, f32>, id: String, distance: f32) {
    let entry = map.entry(id).or_insert(distance);
    if entry.is_nan() || distance < *entry {
        *entry = distance;
    }
}

fn is_edge_type_collection(data_type: &str, field_name: &str) -> bool {
    data_type == "EdgeType" && field_name == "relationship_name"
}

/// Reject a non-positive `neighborhood_depth` / `neighborhood_seed_top_k`,
/// with the messages Python's `cognee.search()` raises.
pub(crate) fn validate_neighborhood(
    depth: Option<usize>,
    seed_top_k: Option<usize>,
) -> Result<(), SearchError> {
    if depth == Some(0) {
        return Err(SearchError::InvalidInput(
            "neighborhood_depth must be a positive integer.".to_string(),
        ));
    }
    if seed_top_k == Some(0) {
        return Err(SearchError::InvalidInput(
            "neighborhood_seed_top_k must be a positive integer.".to_string(),
        ));
    }
    Ok(())
}

/// The seed ids for the neighborhood projection: nearest first, at most
/// `seed_top_k` (all when `None`).
///
/// Python's `extract_relevant_node_ids` ranks each node by its best distance
/// across collections and `get_memory_fragment` slices that list, so the
/// slice keeps the most similar nodes. Equal distances break on the id (Python
/// keeps insertion order there, which a `HashMap` cannot reproduce), and a NaN
/// distance ranks last, as Python's `None` score does.
fn nearest_seeds(node_distances: &HashMap<String, f32>, seed_top_k: Option<usize>) -> Vec<String> {
    let rank_key = |distance: f32| {
        if distance.is_nan() {
            f32::INFINITY
        } else {
            distance
        }
    };
    let mut seeds: Vec<(&String, f32)> = node_distances
        .iter()
        .map(|(id, distance)| (id, rank_key(*distance)))
        .collect();
    seeds.sort_by(|(left_id, left), (right_id, right)| {
        left.total_cmp(right).then_with(|| left_id.cmp(right_id))
    });
    seeds
        .into_iter()
        .take(seed_top_k.unwrap_or(usize::MAX))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Give the nodes a neighborhood projection discovered (every projected node
/// that was not a vector-search hit) their real distance to the query, so they
/// are ranked on similarity rather than all taking the penalty.
///
/// Port of the expansion re-scoring in Python's
/// `_get_top_triplet_importances`: each existing node collection is asked
/// for the expansion ids through `score_by_ids`; an adapter that cannot
/// answer stops the loop (Python `break`s on `NotImplementedError`) and the
/// remaining expansion nodes keep the penalty.
async fn score_neighborhood_expansion(
    vector_db: &dyn VectorDB,
    query_vector: &[f32],
    node_collections: &[(&str, &str)],
    graph_nodes: &[GraphNode],
    seed_ids: &HashSet<String>,
    node_distances: &mut HashMap<String, f32>,
    node_dataset_ids: &mut HashMap<String, String>,
) -> Result<(), SearchError> {
    let mut expansion_ids: Vec<Uuid> = graph_nodes
        .iter()
        .filter(|(id, properties)| {
            !seed_ids.contains(id) && !marks_internal(properties.get(INTERNAL_PROPERTY))
        })
        .filter_map(|(id, _)| Uuid::parse_str(id).ok())
        .collect();
    expansion_ids.sort_unstable();
    expansion_ids.dedup();
    if expansion_ids.is_empty() {
        return Ok(());
    }

    // Independent per collection, like the seed searches: run them
    // concurrently, then merge in collection order so the outcome is the
    // sequential one, including where an unsupporting adapter stops it.
    let expansion_ids = &expansion_ids;
    let scorings = node_collections
        .iter()
        .map(|&(data_type, field_name)| async move {
            vector_db
                .score_by_ids(data_type, field_name, expansion_ids, query_vector)
                .await
        });
    for scored in futures::future::join_all(scorings).await {
        let Some(scored) = scored? else {
            debug!("vector adapter does not support scoring by id — expansion keeps the penalty");
            break;
        };
        for result in scored {
            let node_id = result.id.to_string();
            keep_nearest(node_distances, node_id.clone(), 1.0 - result.score);
            if let Some(dataset_id) = result.metadata.get("dataset_id").and_then(|v| v.as_str()) {
                node_dataset_ids
                    .entry(node_id)
                    .or_insert_with(|| dataset_id.to_string());
            }
        }
    }
    Ok(())
}

/// An edge's `edge_text` property, unless absent or blank.
fn nonblank_edge_text<'a>(properties: &'a HashMap<Cow<'static, str>, Value>) -> Option<&'a str> {
    properties
        .get("edge_text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod penalty_default_tests {
    use super::*;

    #[test]
    fn nearest_seeds_orders_by_distance_then_id_and_truncates() {
        let distances: HashMap<String, f32> = [
            ("d".to_string(), 0.9),
            ("c".to_string(), f32::NAN),
            ("b".to_string(), 0.1),
            ("a".to_string(), 0.1),
            ("e".to_string(), 0.5),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            nearest_seeds(&distances, None),
            vec!["a", "b", "e", "d", "c"]
        );
        assert_eq!(nearest_seeds(&distances, Some(3)), vec!["a", "b", "e"]);
    }

    #[test]
    fn keep_nearest_prefers_any_real_distance_over_nan() {
        let mut map = HashMap::new();
        keep_nearest(&mut map, "a".to_string(), f32::NAN);
        keep_nearest(&mut map, "a".to_string(), 0.7);
        keep_nearest(&mut map, "a".to_string(), 0.4);
        keep_nearest(&mut map, "a".to_string(), f32::NAN);
        keep_nearest(&mut map, "a".to_string(), 0.9);
        assert_eq!(map["a"], 0.4);
    }

    #[test]
    fn graph_load_resolves_from_config() {
        let names = vec!["n".to_string()];
        let base = GraphRetrievalConfig::default();
        assert_eq!(GraphLoad::resolve(&base), GraphLoad::SeedNeighborhood);

        let node_set = GraphRetrievalConfig {
            node_type: Some("Entity".to_string()),
            node_name: Some(names.clone()),
            ..Default::default()
        };
        assert_eq!(
            GraphLoad::resolve(&node_set),
            GraphLoad::NodeSet {
                node_type: "Entity",
                node_names: &names
            }
        );

        let projection = GraphRetrievalConfig {
            neighborhood_depth: Some(2),
            neighborhood_seed_top_k: Some(3),
            ..Default::default()
        };
        let load = GraphLoad::resolve(&projection);
        assert_eq!(
            load,
            GraphLoad::Projection {
                depth: 2,
                seed_top_k: Some(3)
            }
        );
        assert!(load.ranks_every_loaded_edge());
        assert!(!GraphLoad::resolve(&base).ranks_every_loaded_edge());

        // Python: `node_name` (with or without `node_type`) disables the seeds.
        let no_seeds = GraphRetrievalConfig {
            neighborhood_depth: Some(2),
            node_name: Some(names.clone()),
            ..Default::default()
        };
        assert_eq!(GraphLoad::resolve(&no_seeds), GraphLoad::NoSeeds);
    }

    #[test]
    fn default_triplet_distance_penalty_matches_python() {
        // Python: cognee/modules/retrieval/utils/brute_force_triplet_search.py:56,227
        assert_eq!(DEFAULT_TRIPLET_DISTANCE_PENALTY, 6.5);
    }

    #[test]
    fn graph_retrieval_config_default_uses_python_penalty() {
        let cfg = GraphRetrievalConfig::default();
        assert_eq!(cfg.triplet_distance_penalty, 6.5);
    }

    /// Verifies that SEARCH_COLLECTIONS includes ("Triplet", "text"), mirroring
    /// Python's _get_vector_index_collections() which enumerates all DataPoint
    /// subclasses including Triplet (graph_completion_retriever.py:88-99).
    /// Triplet declares metadata = {"index_fields": ["text"]} (Triplet.py:9).
    #[test]
    fn search_collections_includes_triplet_text() {
        let has_triplet_text = SEARCH_COLLECTIONS
            .iter()
            .any(|&(dt, fn_)| dt == "Triplet" && fn_ == "text");
        assert!(
            has_triplet_text,
            "SEARCH_COLLECTIONS must include (\"Triplet\", \"text\") to match Python's \
             dynamic enumeration of DataPoint index collections after memify"
        );
    }

    /// Verifies that the false "intentionally excluded" comment is gone.
    /// Triplet_text is now included, not excluded.
    #[test]
    fn search_collections_has_six_entries() {
        // 5 original + Triplet_text = 6 total
        assert_eq!(
            SEARCH_COLLECTIONS.len(),
            6,
            "SEARCH_COLLECTIONS should have 6 entries after adding Triplet_text"
        );
    }
}
