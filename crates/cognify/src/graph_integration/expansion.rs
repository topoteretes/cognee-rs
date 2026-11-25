//! Graph expansion logic.
//!
//! Mirrors Python's `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`
//! Converts LLM-layer KnowledgeGraph objects to storage-layer Entity/EntityType pairs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use uuid::Uuid;

use cognee_core::{CoreError, CpuPool, CpuPoolExt, HasDataPoint, ProvenanceContext, stamp_tree};
use cognee_models::{Entity, EntityType, Timestamp};
use cognee_ontology::traits::{OntologyEdge, OntologySubgraph};
use cognee_ontology::{AttachedOntologyNode, NodeCategory, OntologyResolver};
use cognee_utils::{generate_edge_name, generate_node_name, normalize_identifier};
use tracing::{debug, warn};

use crate::fact_extraction::{KnowledgeGraph, Node};
use crate::graph_integration::types::{
    ArtifactProducers, GraphEdgePair, GraphNodePair, ONTOLOGY_INDIVIDUAL_TYPE, TimestampNode,
    is_ontology_individual_placeholder,
};

/// How many distinct unresolved endpoint references to name in the summary log.
const UNRESOLVED_SAMPLE_LIMIT: usize = 10;

/// Tally of how edge endpoints resolved during one expansion pass.
///
/// Endpoint resolution is lossy by design — an LLM routinely emits an edge
/// referencing a node id it never declared — but until now the only evidence
/// was one `warn!` line per dropped edge, which at corpus scale is tens of
/// thousands of lines and no total. These counters make the drop rate a single
/// number that can be attributed and tracked between runs.
///
/// The `_cross_chunk` counters split the two resolved tallies by *where the
/// matched key came from*: the chunk that referenced it, or an earlier chunk of
/// the same unit (one data item, or one block of it). That split prices
/// resolving within the edge's own chunk only, as Python does — see
/// [`EdgeResolutionStats::resolved_cross_chunk`].
///
/// Counting note, inherited: `attempted` is per **edge**, while every
/// `resolved_*` counter is per **endpoint**, so the resolved tallies run to
/// roughly twice `attempted` on a pass that drops nothing.
///
/// # Stability
///
/// `#[non_exhaustive]`: this is a diagnostic tally the pipeline **produces** and
/// callers **read**, so constructing one outside this crate is not a supported
/// use. It has already gained counters twice, and each addition would otherwise
/// be a breaking change for any downstream struct literal or exhaustive
/// destructuring. Taking that break once, here, alongside an addition that is
/// breaking anyway, is cheaper than taking it again on the next counter.
/// Reading fields, `Default`, `Clone` and `Copy` are all unaffected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EdgeResolutionStats {
    /// Edges considered (before database-existence filtering).
    pub attempted: usize,
    /// Endpoints resolved by their declared node id — the common path.
    pub resolved_by_id: usize,
    /// Endpoints resolved only via the node-name fallback alias.
    pub resolved_by_name: usize,
    /// Of [`Self::resolved_by_id`], those whose id key was last declared by an
    /// *earlier* chunk than the one whose edge referenced it.
    pub resolved_by_id_cross_chunk: usize,
    /// Of [`Self::resolved_by_name`], the same split.
    pub resolved_by_name_cross_chunk: usize,
    /// Largest gap, in chunk positions, between the most recent declaration of
    /// a matched key and the chunk whose edge referenced it. Zero when every
    /// resolution was chunk-local.
    pub max_cross_chunk_distance: usize,
    /// Edges dropped because the source endpoint matched nothing.
    pub dropped_source_missing: usize,
    /// Edges dropped because the target endpoint matched nothing.
    pub dropped_target_missing: usize,
    /// Edges dropped because an endpoint matched a name shared by two or more
    /// distinct entities, so resolving it would have been a coin flip.
    pub dropped_ambiguous_name: usize,
}

impl EdgeResolutionStats {
    /// Total edges dropped for any endpoint-resolution reason.
    pub fn dropped(&self) -> usize {
        self.dropped_source_missing + self.dropped_target_missing + self.dropped_ambiguous_name
    }

    /// Endpoints that resolved only because the registry spans the whole unit.
    ///
    /// Endpoints never resolve across units, so this is the population that
    /// [`EndpointScope::Chunk`] — Python's rule, an edge resolves only against
    /// its own chunk's nodes — would drop.
    ///
    /// Attribution follows the **most recent** declaration of a key at or
    /// before the referencing chunk. Extraction re-declares a recurring entity
    /// in every chunk that mentions it, so attributing to the *first*
    /// declaration would count a resolution the referencing chunk could satisfy
    /// on its own. [`Self::max_cross_chunk_distance`] says how far back, within
    /// a unit, the furthest such match reached.
    pub fn resolved_cross_chunk(&self) -> usize {
        self.resolved_by_id_cross_chunk + self.resolved_by_name_cross_chunk
    }

    /// Add `other`'s counters to these: every count is summed, the distance is
    /// the larger of the two.
    pub(crate) fn absorb(&mut self, other: &EdgeResolutionStats) {
        self.attempted += other.attempted;
        self.resolved_by_id += other.resolved_by_id;
        self.resolved_by_name += other.resolved_by_name;
        self.resolved_by_id_cross_chunk += other.resolved_by_id_cross_chunk;
        self.resolved_by_name_cross_chunk += other.resolved_by_name_cross_chunk;
        self.max_cross_chunk_distance = self
            .max_cross_chunk_distance
            .max(other.max_cross_chunk_distance);
        self.dropped_source_missing += other.dropped_source_missing;
        self.dropped_target_missing += other.dropped_target_missing;
        self.dropped_ambiguous_name += other.dropped_ambiguous_name;
    }

    /// Record one endpoint that matched the id registry.
    fn record_resolved_by_id(&mut self, origin: ChunkPosition, position: ChunkPosition) {
        self.resolved_by_id += 1;
        if origin != position {
            self.resolved_by_id_cross_chunk += 1;
            self.widen_cross_chunk_distance(origin, position);
        }
    }

    /// Record one endpoint that matched the name-alias registry.
    fn record_resolved_by_name(&mut self, origin: ChunkPosition, position: ChunkPosition) {
        self.resolved_by_name += 1;
        if origin != position {
            self.resolved_by_name_cross_chunk += 1;
            self.widen_cross_chunk_distance(origin, position);
        }
    }

    /// `saturating_sub` rather than `-`: the single forward pass over `graphs`
    /// means a key is always registered at or before the position that reads
    /// it, but a wrapped `usize` in release would report a nonsense distance if
    /// that ever stopped holding.
    fn widen_cross_chunk_distance(&mut self, origin: ChunkPosition, position: ChunkPosition) {
        self.max_cross_chunk_distance = self
            .max_cross_chunk_distance
            .max(position.saturating_sub(origin));
    }
}

/// Zero-based position of a chunk in its unit's `graphs` sequence.
///
/// Used to attribute a resolved endpoint to the chunk that most recently
/// declared its key. It is a position within the unit, not a chunk id: the
/// comparison that matters is ordinal.
type ChunkPosition = usize;

/// How one edge endpoint resolved to an entity.
enum EndpointResolution {
    /// Matched a declared node id, last declared at `origin`.
    ById { entity: Uuid, origin: ChunkPosition },
    /// Matched a node's human-readable name via the fallback alias, last
    /// declared at `origin`.
    ByName { entity: Uuid, origin: ChunkPosition },
    /// The name is shared by two or more distinct entities.
    Ambiguous,
    /// Nothing matched.
    Missing,
}

/// Register a node's human-readable name as a fallback alias for its entity.
///
/// The extraction prompt asks the model for an `id` *and* a separate "most
/// complete human-readable name", so a model that emits
/// `{id: "einstein", name: "Albert Einstein"}` and then an edge referencing
/// `"Albert Einstein"` produces an endpoint that the id-keyed registry cannot
/// resolve. Aliasing the name closes that gap.
///
/// A name claimed by two distinct entities is poisoned to `None` rather than
/// resolved arbitrarily — dropping such an edge is correct, silently attaching
/// it to whichever entity happened to be seen first is not.
///
/// Call this **only where a node id is declared for the first time**, next to
/// the `node_id_to_entity_id` insert. Because it poisons, it is not idempotent
/// across re-declarations: a later chunk that re-declares an already-known node
/// id under a name some *other* entity already owns would turn that name from
/// resolvable into `Ambiguous` and drop edges that resolve today. A
/// re-declaration must go through [`refresh_name_alias_origin`] instead, which
/// moves an existing alias's origin forward without minting a new mapping.
fn register_name_alias(
    aliases: &mut HashMap<String, Option<(Uuid, ChunkPosition)>>,
    raw_name: &str,
    entity_id: Uuid,
    id_key: &str,
    position: ChunkPosition,
) {
    let key = normalize_identifier(raw_name);
    if key.is_empty() || key == id_key {
        return;
    }
    aliases
        .entry(key)
        .and_modify(|slot| match slot {
            // The same entity re-registering from a later chunk must not poison
            // its own alias — and it *does* move the origin forward, because the
            // re-declaring chunk could now resolve it on its own.
            Some((id, origin)) if *id == entity_id => *origin = position,
            // A different entity claiming the same name: unresolvable, and it
            // stays that way.
            Some(_) => *slot = None,
            None => {}
        })
        .or_insert(Some((entity_id, position)));
}

/// Move an *already registered* alias's origin forward to `position`.
///
/// The sibling of the `node_id_to_entity_id` refresh: both maps record the most
/// recent declaration of a key at or before the chunk reading it, and both do
/// that by `get_mut`-ing an existing entry — never by inserting one. Extraction
/// re-declares a recurring entity in every chunk that mentions it, and without
/// this the cross-chunk counters would attribute such a resolution to the first
/// declaration and over-report what chunk-local resolution would drop.
///
/// Narrower than [`register_name_alias`] in exactly two ways, both deliberate:
/// an unknown name is left unknown rather than created, and a name owned by a
/// different entity is left alone rather than poisoned. Only the entity that
/// already holds the alias can move it.
fn refresh_name_alias_origin(
    aliases: &mut HashMap<String, Option<(Uuid, ChunkPosition)>>,
    raw_name: &str,
    entity_id: Uuid,
    position: ChunkPosition,
) {
    let key = normalize_identifier(raw_name);
    if let Some(Some((id, origin))) = aliases.get_mut(&key)
        && *id == entity_id
    {
        *origin = position;
    }
}

/// The id-registry key of a node id, or of an edge endpoint naming one.
///
/// Under [`EndpointScope::Unit`] it is normalised (case and spacing), so an
/// edge that spells an id differently still resolves. Under
/// [`EndpointScope::Chunk`] it is the id verbatim, as Python's
/// `_add_extracted_edges` looks endpoints up: `A` and `a` are two nodes, and
/// an endpoint `Steve Jobs` does not reach the id `steve_jobs`.
fn registry_key(node_id: &str, scope: EndpointScope) -> String {
    match scope {
        EndpointScope::Unit => normalize_identifier(node_id),
        EndpointScope::Chunk => node_id.to_string(),
    }
}

/// Resolve one edge endpoint, preferring a node-id match and falling back to
/// the node-name alias only when the id map misses — and only under
/// [`EndpointScope::Unit`]: Python matches by node id alone.
fn resolve_endpoint(
    by_id: &HashMap<String, (Uuid, ChunkPosition)>,
    by_name: &HashMap<String, Option<(Uuid, ChunkPosition)>>,
    raw: &str,
    scope: EndpointScope,
) -> EndpointResolution {
    if let Some((entity, origin)) = by_id.get(&registry_key(raw, scope)) {
        return EndpointResolution::ById {
            entity: *entity,
            origin: *origin,
        };
    }
    if scope == EndpointScope::Chunk {
        return EndpointResolution::Missing;
    }
    let key = normalize_identifier(raw);
    match by_name.get(&key) {
        Some(Some((entity, origin))) => EndpointResolution::ByName {
            entity: *entity,
            origin: *origin,
        },
        Some(None) => EndpointResolution::Ambiguous,
        None => EndpointResolution::Missing,
    }
}

/// Stamp a freshly-constructed `Entity` / `EntityType` / `EdgeType` at
/// emission time so the pipeline executor's recursion finds
/// `source_pipeline` and `source_task` already set.
///
/// Mirrors Python's `_stamp_provenance_deep` in
/// `cognee/tasks/graph/extract_graph_from_data.py`.
///
/// `user_label` is the resolved provenance label
/// (see [`cognee_core::PipelineContext::user_label`] for the canonical
/// shape). Pass `None` if the user is not known at construction time —
/// the executor walk fills the field in later.
///
/// Per locked decision 6 of `docs/telemetry/05-datapoint-provenance.md`,
/// this pre-stamp coexists with cognify's local `stamp_provenance`
/// helper at `crates/cognify/src/tasks.rs`; the `if dp.source_X.is_none()`
/// guards inside [`stamp_tree`] make double-stamping a no-op.
///
/// `task_rank` is the 1-based pipeline position written to
/// `topological_rank`, or `None` to leave the field at its `Some(0)`
/// sentinel. It is a **parameter, not a constant**, because this helper has
/// two unrelated callers with different correct answers:
///
/// - the graph-expansion path below, reached from the extract-graph task,
///   which passes that task's rank (the standalone `extract_graph_from_data`
///   persists entities before any task body could stamp them, so the rank has
///   to be right here);
/// - the persistence layer (`crate::persist`), which pre-stamps
///   freshly-counted `EdgeType` DataPoints and passes `None` — Python's
///   `index_graph_edges.py:50` builds those `EdgeType` objects without ever
///   handing them to a provenance stamper, so their rank stays at the `0`
///   sentinel and matching that is the parity-correct outcome.
pub(crate) fn pre_stamp_extraction(
    target: &mut dyn HasDataPoint,
    user_label: Option<&str>,
    task_rank: Option<i32>,
    visited: &mut HashSet<Uuid>,
) {
    let ctx = ProvenanceContext {
        // Locked Decision 14 (LIB-06): the pipeline name carried on every
        // stamped DataPoint. Must byte-match the `.with_name(...)` set on
        // `build_cognify_pipeline` — hence the shared constant rather than a
        // second literal.
        pipeline_name: crate::tasks::COGNIFY_PIPELINE_STAMP_NAME,
        task_name: crate::tasks::EXTRACT_GRAPH_TASK_NAME,
        user_label,
        node_set: None,
        content_hash: None,
        task_rank,
    };
    stamp_tree(target, &ctx, visited);
}

/// Core graph integration function. Converts LLM-layer KnowledgeGraph objects
/// to storage-layer Entity/EntityType pairs.
///
/// This mirrors the Python `expand_with_nodes_and_edges()` function from
/// `cognee/modules/graph/utils/expand_with_nodes_and_edges.py`.
///
/// # Process
/// 1. Create EntityType for each unique node type
/// 2. Create Entity for each node
/// 3. Create Edge for each relationship
/// 4. Deduplicate in-memory using HashMaps
///
/// # Deduplication Keys
/// - **Node**: `{node_id}_{category}` where category = "entity" or "type"
/// - **Edge**: `{source_entity_id}_{target_entity_id}_{relationship_name}`
///
/// # Arguments
/// * `graphs` - Vector of (chunk_id, KnowledgeGraph) pairs. Each graph is
///   paired with the UUID of the chunk it was extracted from, so entities
///   are tagged with the correct source chunk.
/// * `dataset_id` - UUID of the dataset
/// * `chunk_node_sets` - Map from source chunk UUID to that chunk's
///   `belongs_to_set` NodeSet entries. Each created Entity inherits the NodeSet
///   entries of the chunk it was extracted from, mirroring Python's
///   `Entity(belongs_to_set=data_chunk.belongs_to_set)` in
///   `expand_with_nodes_and_edges.py:227`. This is what keeps a node_name-scoped
///   (NodeSet-filtered) search from dropping every extracted entity. Chunks with
///   no NodeSet metadata are simply absent from the map.
/// * `chunk_importance_weights` - Map from source chunk UUID to that chunk's
///   `importance_weight`. Every created EntityType, Entity, and ontology-derived
///   node inherits the `importance_weight` of the chunk it was extracted from,
///   mirroring Python's `importance_weight=data_chunk.importance_weight` on the
///   type node, entity node, and ontology nodes in
///   `expand_with_nodes_and_edges.py:66,79,163,229`. Chunks absent from the map
///   default to `0.5` (Python's `DataPoint.importance_weight` default).
/// * `existing_edges` - The (source, target, label) keys, on the resolved
///   entity ids, of the edges already in the graph database: what
///   [`crate::graph_integration::retrieve_existing_edges`] returns for this
///   expansion's edges. See [`Expansion::split_existing_edges`].
/// * `ontology_resolver` - Ontology resolver for entity validation and enrichment.
///   When loaded, validates entity types against "classes" and entities against
///   "individuals". A [`NoOpOntologyResolver`] leaves everything unvalidated.
/// * `task_rank` - 1-based position of the calling task in its pipeline,
///   written to `topological_rank` on every created Entity / EntityType (see
///   [`pre_stamp_extraction`]). Pass
///   `Some(`[`crate::tasks::EXTRACT_GRAPH_TASK_RANK`]`)` for the default
///   cognify pipeline, the task's real position for a custom pipeline, or
///   `None` to leave the rank unstamped.
///
/// # Returns
/// Tuple of (graph_nodes, graph_edges, claimed_existing_edges, producers) for
/// storage. The [`ArtifactProducers`] set names every chunk that produced each
/// merged entity and edge, not just the one that created it — merging keeps
/// only the first, and one ownership row per producing file needs all of them.
/// It also names, per `(entity, type)` and per type, the chunks that declared
/// them, and the chunk whose ontology lookup brought in each ontology-only
/// entity and ontology edge (SDK-1694 O5a).
///
/// `claimed_existing_edges` holds the edges this run produced that
/// `existing_edges` says are already in the graph. They must NOT be written
/// to the graph again — that is what the filter is for — but they do need
/// ownership rows, because otherwise the only claim on such an edge belongs to
/// the run that first created it, and deleting *that* run's data item finds the
/// edge exclusively owned and deletes it from under every later file that
/// references it. A caller that drops this list re-creates, across runs, the
/// cross-file damage per-producer ownership rows exist to prevent.
///
/// The row is dataset-scoped, like every other ownership row, but it protects
/// the edge across datasets too: in one shared store a delete counts rows of
/// any other dataset as claimants (`ClaimantScope::AnyDataset`, SDK-948), so
/// an edge two datasets produced survives deleting either. Under per-dataset
/// storage each dataset has its own graph and only same-dataset rows count.
#[allow(clippy::too_many_arguments)]
pub async fn expand_with_nodes_and_edges(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    dataset_id: Uuid,
    chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: &HashMap<Uuid, f64>,
    existing_edges: &HashSet<(Uuid, Uuid, String)>,
    ontology_resolver: &dyn OntologyResolver,
    user_label: Option<&str>,
    task_rank: Option<i32>,
) -> (
    Vec<GraphNodePair>,
    Vec<GraphEdgePair>,
    Vec<GraphEdgePair>,
    ArtifactProducers,
) {
    let (nodes, edges, claimed_existing_edges, producers, _stats) =
        expand_with_nodes_and_edges_with_stats(
            graphs,
            dataset_id,
            chunk_node_sets,
            chunk_importance_weights,
            existing_edges,
            ontology_resolver,
            user_label,
            task_rank,
        )
        .await;
    (nodes, edges, claimed_existing_edges, producers)
}

/// [`expand_with_nodes_and_edges`], additionally returning the endpoint-resolution
/// tally so a caller can report how many edges the pass dropped and why.
///
/// Prefer this from the pipeline; the four-tuple form above exists so the many
/// call sites that do not care about the tally stay unchanged.
///
/// The tally covers **endpoint resolution only**. An edge routed into
/// `claimed_existing_edges` because the database already holds it is not a
/// drop: both its endpoints resolved, and the edge is still returned — to the
/// claim bucket rather than the write bucket. `dropped()` therefore stays a
/// count of edges this pass could not build at all, and `attempted -
/// dropped()` stays an upper bound on `graph_edges.len()`.
///
/// # Timestamps
///
/// This view predates Timestamp nodes and has no slot for them: the
/// Timestamp nodes [`expand_extracted_graphs`] builds are left out, and so are
/// the edges that point at them, so the result stays self-consistent. The
/// pipeline uses [`expand_extracted_graphs`].
#[allow(clippy::too_many_arguments)]
pub async fn expand_with_nodes_and_edges_with_stats(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    dataset_id: Uuid,
    chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: &HashMap<Uuid, f64>,
    existing_edges: &HashSet<(Uuid, Uuid, String)>,
    ontology_resolver: &dyn OntologyResolver,
    user_label: Option<&str>,
    task_rank: Option<i32>,
) -> (
    Vec<GraphNodePair>,
    Vec<GraphEdgePair>,
    Vec<GraphEdgePair>,
    ArtifactProducers,
    EdgeResolutionStats,
) {
    let expansion = expand_extracted_graphs(
        graphs,
        dataset_id,
        chunk_node_sets,
        chunk_importance_weights,
        existing_edges,
        ontology_resolver,
        user_label,
        task_rank,
    )
    .await;
    let timestamp_ids: HashSet<Uuid> = expansion
        .timestamps
        .iter()
        .map(|node| node.timestamp.base.id)
        .collect();
    let untimed = |edge: &GraphEdgePair| {
        !timestamp_ids.contains(&edge.source_entity_id)
            && !timestamp_ids.contains(&edge.target_entity_id)
    };
    (
        expansion.nodes,
        expansion.edges.into_iter().filter(untimed).collect(),
        expansion
            .claimed_existing_edges
            .into_iter()
            .filter(untimed)
            .collect(),
        expansion.producers,
        expansion.stats,
    )
}

/// What one expansion pass produced. See [`expand_with_nodes_and_edges`] for
/// the meaning of every field but `timestamps`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Expansion {
    pub nodes: Vec<GraphNodePair>,
    /// The edges to write. Straight out of [`expand_extracted_graphs_by_unit`]
    /// this is every edge the units built, extracted and ontology alike;
    /// [`Self::split_existing_edges`] then moves out the ones the graph
    /// already holds.
    pub edges: Vec<GraphEdgePair>,
    /// Edges this run produced that the graph already holds: ownership rows
    /// only, no graph write. Filled by [`Self::split_existing_edges`].
    pub claimed_existing_edges: Vec<GraphEdgePair>,
    /// Every chunk that produced each entity, Timestamp and edge.
    pub producers: ArtifactProducers,
    pub stats: EdgeResolutionStats,
    /// The Timestamp nodes: one per normalized time string, whichever chunks
    /// named it. Extracted nodes the LLM typed "Timestamp" whose name parses
    /// ([`Timestamp::from_text`]) become these instead of entities, so the
    /// `*_at` edges pointing at them anchor facts to real times. A
    /// "Timestamp" whose name does not parse stays an ordinary entity.
    pub timestamps: Vec<TimestampNode>,
}

impl Expansion {
    /// Move the edges the graph already holds out of [`Self::edges`].
    ///
    /// `existing` is the set of (source, target, label) keys, on the resolved
    /// ids, that the graph database already has: what
    /// [`crate::graph_integration::retrieve_existing_edges`] returns for
    /// [`Self::edges`]. This is the second half of Python's existing-edge
    /// check (`find_existing_edge_identities`, then
    /// `attach_new_edges_to_data_points`, which leaves every existing edge
    /// out of the write).
    ///
    /// An existing edge some chunk produced moves to
    /// [`Self::claimed_existing_edges`]: it is not written again, but this
    /// run gets an ownership row for it (see [`expand_with_nodes_and_edges`]).
    /// That is a deliberate superset of Python on a ledger graph, which
    /// records nothing for any existing edge: its chunk ownership
    /// (`_produced_edge_identities`, read by `collect_chunk_ownership`) is
    /// only used when the graph stores its provenance in the graph
    /// (`add_data_points.py:255-258`). Without the claim, deleting the data
    /// item that first wrote the edge would take it from under every later
    /// one that also produced it.
    ///
    /// An existing ontology edge is claimed the same way, by every document
    /// whose ontology lookup brought it in (SDK-1694 O5a): Python's in-graph
    /// provenance attaches a document-scoped source ref to it, and Rust's
    /// ledger rows are the counterpart. Python's ledger path records nothing
    /// for any existing edge, extracted or ontology. An existing edge nothing
    /// in this run produced (no producers recorded, a hand-built input) is
    /// dropped.
    ///
    /// Run once the units are merged: the check needs the final ids, and the
    /// writer may run it per document.
    pub fn split_existing_edges(&mut self, existing: &HashSet<(Uuid, Uuid, String)>) {
        if existing.is_empty() {
            return;
        }
        let mut claimed_keys: HashSet<(Uuid, Uuid, String)> = self
            .claimed_existing_edges
            .iter()
            .map(GraphEdgePair::dedup_key)
            .collect();
        let edges = std::mem::take(&mut self.edges);
        for edge in edges {
            let key = edge.dedup_key();
            if !existing.contains(&key) {
                self.edges.push(edge);
            } else if self.producers.produces_edge(&key) {
                // One claim per key: an extracted edge and an ontology edge
                // can share one, and both are on the list until the in-memory
                // deduplication.
                if claimed_keys.insert(key) {
                    self.claimed_existing_edges.push(edge);
                }
            }
        }
    }
}

/// The extracted graph with only the first node for each graph-local id.
///
/// Port of Python's `_remove_duplicate_extracted_nodes_by_id`: a model that
/// declares one id twice gets the first declaration kept and the rest dropped
/// with a warning — lossy, but an id must name one node for its edges to mean
/// anything.
fn without_duplicate_node_ids(mut graph: KnowledgeGraph) -> KnowledgeGraph {
    let mut seen: HashSet<String> = HashSet::new();
    graph.nodes.retain(|node| {
        if seen.insert(node.id.clone()) {
            true
        } else {
            warn!("Ignoring duplicate extracted node ID: {}", node.id);
            false
        }
    });
    graph
}

/// The final entity id for every graph-local node id of one chunk's graph.
///
/// Port of Python's `_calculate_entity_ids_by_extracted_node_id`. An entity's
/// id derives from its **name** (`Entity.id_for(name)`), so the same name in
/// two chunks or two documents is one entity. Several nodes sharing a name
/// within one graph stay distinct: ordered by (type, description, id), each
/// gets a deterministic graph-scoped id `Entity.id_for(name, chunk_id,
/// ordinal)`, ordinals from 1. Expects node ids already made unique
/// ([`without_duplicate_node_ids`]).
fn entity_ids_by_node_id(graph: &KnowledgeGraph, chunk_id: Uuid) -> HashMap<String, Uuid> {
    let mut by_name: HashMap<Uuid, Vec<&Node>> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for node in &graph.nodes {
        let name_based = Entity::id_for(&node.name);
        let group = by_name.entry(name_based).or_default();
        if group.is_empty() {
            order.push(name_based);
        }
        group.push(node);
    }

    let chunk = chunk_id.to_string();
    let mut ids = HashMap::new();
    for name_based in order {
        let mut group = by_name.remove(&name_based).unwrap_or_default();
        if let [only] = group.as_slice() {
            ids.insert(only.id.clone(), name_based);
            continue;
        }
        group.sort_by_cached_key(|node| {
            (
                generate_node_name(&node.node_type),
                generate_node_name(&node.description),
                generate_node_name(&node.id),
            )
        });
        for (ordinal, node) in group.into_iter().enumerate() {
            let ordinal = (ordinal + 1).to_string();
            ids.insert(
                node.id.clone(),
                cognee_utils::data_point_id_for("Entity", &[&node.name, &chunk, &ordinal]),
            );
        }
    }
    ids
}

/// Whether an extracted node's type is "Timestamp" (normalized as Python's
/// `generate_node_name(type) == "timestamp"`).
fn is_timestamp_type(node_type: &str) -> bool {
    generate_node_name(node_type) == "timestamp"
}

/// Which chunks of a unit an edge endpoint may resolve against.
///
/// A unit is what one graph is built from: one data item, or one block of
/// `chunks_per_batch` chunks of it (see
/// [`crate::graph_integration::extraction_units`]). Endpoints never resolve
/// across units, so an edge of one document never reaches a node only
/// another document declared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EndpointScope {
    /// Any chunk of the unit, up to and including the edge's own: an edge can
    /// reach a node an earlier chunk of the same block declared, by node id
    /// or, failing that, by the node's name. Rust's
    /// default; it keeps edges Python drops.
    #[default]
    Unit,
    /// Only the edge's own chunk, and only by the verbatim node id, as
    /// Python's `_add_extracted_edges` resolves them
    /// (`expand_with_nodes_and_edges.py`): the node-name fallback is off too,
    /// and ids that differ only in case or spacing are different nodes.
    Chunk,
}

/// [`expand_with_nodes_and_edges_with_stats`], additionally returning the
/// Timestamp nodes. All of `graphs` is treated as one unit, so an endpoint
/// may resolve against any earlier chunk of it. The cognify pipeline uses
/// [`expand_extracted_graphs_by_unit`].
#[allow(clippy::too_many_arguments)]
pub async fn expand_extracted_graphs(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    dataset_id: Uuid,
    chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: &HashMap<Uuid, f64>,
    existing_edges: &HashSet<(Uuid, Uuid, String)>,
    ontology_resolver: &dyn OntologyResolver,
    user_label: Option<&str>,
    task_rank: Option<i32>,
) -> Expansion {
    let ontology = OntologyLookups::new(ontology_resolver);
    let inputs = UnitInputs {
        dataset_id,
        chunk_node_sets,
        chunk_importance_weights,
        ontology: &ontology,
        user_label,
        task_rank,
        endpoint_scope: EndpointScope::Unit,
    };
    let (mut expansion, unresolved_sample) = expand_unit(graphs, &inputs);
    log_resolution_summary(&expansion.stats, &unresolved_sample);
    expansion.split_existing_edges(existing_edges);
    expansion
}

/// Build one graph per unit, as Python does, and merge them.
///
/// Python runs one pipeline per data item and builds the graph of each block
/// of `chunks_per_batch` chunks with fresh maps
/// (`construct_data_points_and_edges`), so within a unit the first
/// declaration of an entity decides its description and type, and across
/// units the types are combined. Each of `units` (from
/// [`crate::graph_integration::extraction_units`]) is expanded with fresh
/// maps and the results merged by [`merge_expansions`] in input order.
///
/// The units are expanded off the async runtime. With `cpu_pool` — the
/// pipeline passes the run's [`cognee_core::TaskContext::thread_pool`], the
/// pool the host sized — each unit is one task on that pool, so they run
/// concurrently on its threads and no other pool is started. Without one,
/// they run one after another on a single `spawn_blocking` thread. Either
/// way the merge then runs in input order, so the result equals expanding
/// the units one after another. Ontology lookups are cached for the whole
/// run.
///
/// `endpoint_scope` says which chunks of a unit an edge endpoint may match;
/// endpoints never match across units.
///
/// The result's `edges` are every edge built, whether or not the graph
/// already holds it, and `claimed_existing_edges` is empty: look the edges up
/// with [`crate::graph_integration::retrieve_existing_edges`] and split them
/// with [`Expansion::split_existing_edges`], as Python checks the final
/// edges (`find_existing_edge_identities`).
///
/// # Errors
///
/// [`CoreError::TaskAborted`] when a unit's expansion panicked, or the pool
/// shut down before it finished.
#[allow(clippy::too_many_arguments)]
pub async fn expand_extracted_graphs_by_unit(
    units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
    dataset_id: Uuid,
    chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: &HashMap<Uuid, f64>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    user_label: Option<&str>,
    task_rank: Option<i32>,
    endpoint_scope: EndpointScope,
    cpu_pool: Option<Arc<dyn CpuPool>>,
) -> Result<Expansion, CoreError> {
    let expander = UnitExpander::new(
        dataset_id,
        chunk_node_sets.clone(),
        chunk_importance_weights.clone(),
        ontology_resolver,
        user_label,
        task_rank,
        endpoint_scope,
        cpu_pool,
    );
    let mut merged = ExpansionMerger::default();
    let mut log = ResolutionLog::default();
    expander
        .expand_each(units, |expansion, sample| {
            log.add(&expansion.stats, sample);
            merged.push(expansion);
        })
        .await?;
    log.emit();
    Ok(merged.finish())
}

/// Expands units for one run: the per-chunk lookups, the ontology cache, the
/// stamp and the CPU pool are set up **once** and shared by every call, so a
/// caller that expands one document at a time (SDK-1694 O4b) clones none of
/// them per document and asks the ontology about a name once per run.
///
/// The units are expanded off the async runtime. With a CPU pool — the
/// pipeline passes the run's [`cognee_core::TaskContext::thread_pool`], the
/// pool the host sized — each unit is one task on that pool, so they run
/// concurrently on its threads and no other pool is started. Without one,
/// they run one after another on a single `spawn_blocking` thread.
#[derive(Clone)]
pub(crate) struct UnitExpander {
    shared: Arc<SharedUnitInputs>,
    cpu_pool: Option<Arc<dyn CpuPool>>,
}

impl UnitExpander {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        dataset_id: Uuid,
        chunk_node_sets: HashMap<Uuid, Vec<serde_json::Value>>,
        chunk_importance_weights: HashMap<Uuid, f64>,
        ontology_resolver: Arc<dyn OntologyResolver>,
        user_label: Option<&str>,
        task_rank: Option<i32>,
        endpoint_scope: EndpointScope,
        cpu_pool: Option<Arc<dyn CpuPool>>,
    ) -> Self {
        Self {
            shared: Arc::new(SharedUnitInputs {
                dataset_id,
                chunk_node_sets,
                chunk_importance_weights,
                ontology: OntologyLookups::shared(ontology_resolver),
                user_label: user_label.map(str::to_owned),
                task_rank,
                endpoint_scope,
            }),
            cpu_pool,
        }
    }

    /// Expand each of `units` with fresh maps and hand each result, **in
    /// input order**, to `each` together with its sample of unresolved
    /// endpoint references. A unit is handed over as soon as it and every
    /// unit before it are done, so finished expansions are not all held until
    /// the last one is.
    ///
    /// None of this work is cancellable: dropping the returned future stops
    /// the wait, not the expansion already handed to a worker, which runs to
    /// completion and is then discarded.
    ///
    /// # Errors
    ///
    /// [`CoreError::TaskAborted`] when a unit's expansion panicked, or the
    /// pool shut down before it finished.
    pub(crate) async fn expand_each(
        &self,
        units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
        mut each: impl FnMut(Expansion, Vec<String>),
    ) -> Result<(), CoreError> {
        use futures::StreamExt;

        // Each unit enters the caller's span, so its logs stay under cognify's.
        let span = tracing::Span::current();
        match &self.cpu_pool {
            Some(pool) => {
                let mut tasks: futures::stream::FuturesOrdered<_> = units
                    .into_iter()
                    .map(|graphs| {
                        let shared = Arc::clone(&self.shared);
                        let span = span.clone();
                        pool.spawn(move || {
                            let _entered = span.enter();
                            expand_unit(graphs, &shared.inputs())
                        })
                    })
                    .collect();
                while let Some(expanded) = tasks.next().await {
                    let (expansion, sample) = expanded?;
                    each(expansion, sample);
                }
            }
            None => {
                let shared = Arc::clone(&self.shared);
                let expanded: Vec<(Expansion, Vec<String>)> =
                    tokio::task::spawn_blocking(move || {
                        let _entered = span.enter();
                        let inputs = shared.inputs();
                        units
                            .into_iter()
                            .map(|graphs| expand_unit(graphs, &inputs))
                            .collect()
                    })
                    .await
                    .map_err(|error| CoreError::TaskAborted {
                        reason: format!("graph expansion did not finish: {error}"),
                    })?;
                for (expansion, sample) in expanded {
                    each(expansion, sample);
                }
            }
        }
        Ok(())
    }

    /// Expand `units` and return the expansions in input order.
    pub(crate) async fn expand(
        &self,
        units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
    ) -> Result<Vec<(Expansion, Vec<String>)>, CoreError> {
        let mut expanded = Vec::with_capacity(units.len());
        self.expand_each(units, |expansion, sample| {
            expanded.push((expansion, sample))
        })
        .await?;
        Ok(expanded)
    }
}

/// The endpoint-resolution counters and the unresolved sample of a run,
/// summed over its units and logged once.
#[derive(Debug, Default)]
pub(crate) struct ResolutionLog {
    stats: EdgeResolutionStats,
    sample: Vec<String>,
}

impl ResolutionLog {
    pub(crate) fn add(&mut self, stats: &EdgeResolutionStats, sample: Vec<String>) {
        self.stats.absorb(stats);
        let room = UNRESOLVED_SAMPLE_LIMIT.saturating_sub(self.sample.len());
        self.sample.extend(sample.into_iter().take(room));
    }

    /// Log the run's summary (see [`log_resolution_summary`]).
    pub(crate) fn emit(&self) {
        log_resolution_summary(&self.stats, &self.sample);
    }
}

/// What every unit's expansion reads, besides its own graphs.
#[derive(Clone, Copy)]
struct UnitInputs<'a> {
    dataset_id: Uuid,
    chunk_node_sets: &'a HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: &'a HashMap<Uuid, f64>,
    ontology: &'a OntologyLookups<'a>,
    user_label: Option<&'a str>,
    task_rank: Option<i32>,
    endpoint_scope: EndpointScope,
}

/// [`UnitInputs`], owned, for workers that outlive the caller's borrows.
struct SharedUnitInputs {
    dataset_id: Uuid,
    chunk_node_sets: HashMap<Uuid, Vec<serde_json::Value>>,
    chunk_importance_weights: HashMap<Uuid, f64>,
    ontology: OntologyLookups<'static>,
    user_label: Option<String>,
    task_rank: Option<i32>,
    endpoint_scope: EndpointScope,
}

impl SharedUnitInputs {
    fn inputs(&self) -> UnitInputs<'_> {
        UnitInputs {
            dataset_id: self.dataset_id,
            chunk_node_sets: &self.chunk_node_sets,
            chunk_importance_weights: &self.chunk_importance_weights,
            ontology: &self.ontology,
            user_label: self.user_label.as_deref(),
            task_rank: self.task_rank,
            endpoint_scope: self.endpoint_scope,
        }
    }
}

/// The resolver behind [`OntologyLookups`]: borrowed by a caller that runs
/// on its own stack, shared by one that hands units to worker threads.
enum ResolverRef<'a> {
    Borrowed(&'a dyn OntologyResolver),
    Shared(Arc<dyn OntologyResolver>),
}

impl ResolverRef<'_> {
    fn get(&self) -> &dyn OntologyResolver {
        match self {
            Self::Borrowed(resolver) => *resolver,
            Self::Shared(resolver) => resolver.as_ref(),
        }
    }
}

/// The ontology resolver with its successful `get_subgraph` answers cached
/// for a run.
///
/// Every unit starts from fresh maps, so without the cache a type or an
/// individual named in many documents is looked up once per document.
///
/// The cache assumes `get_subgraph` is pure for the duration of a run: the
/// answer depends only on its arguments and the ontology loaded when the run
/// started. Every resolver in this repository answers from the loaded
/// ontology alone. An error is not cached; the next lookup of that name asks
/// the resolver again.
struct OntologyLookups<'a> {
    resolver: ResolverRef<'a>,
    loaded: bool,
    /// One slot per (name, category). A unit holds the slot's lock while it
    /// asks the resolver, so concurrent units naming the same thing wait for
    /// that one lookup instead of repeating it; different names do not wait
    /// for each other.
    cache: Mutex<HashMap<(String, &'static str), SubgraphSlot>>,
}

/// One (name, category) entry of the ontology cache: empty until a lookup
/// succeeds.
type SubgraphSlot = Arc<Mutex<Option<Arc<OntologySubgraph>>>>;

impl<'a> OntologyLookups<'a> {
    fn new(resolver: &'a dyn OntologyResolver) -> Self {
        Self::with(ResolverRef::Borrowed(resolver))
    }

    fn with(resolver: ResolverRef<'a>) -> Self {
        Self {
            loaded: resolver.get().is_loaded(),
            resolver,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// `resolver.get_subgraph(name, category, directed = true)`, from the
    /// cache when an earlier lookup succeeded. An error is returned as its
    /// message, which is all the caller logs.
    fn get_subgraph(
        &self,
        name: &str,
        category: &'static str,
    ) -> Result<Arc<OntologySubgraph>, String> {
        // A poisoned lock only means another unit panicked; what the map and
        // the slots hold is still valid.
        let slot = Arc::clone(
            self.cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry((name.to_string(), category))
                .or_default(),
        );
        let mut answer = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(subgraph) = answer.as_ref() {
            return Ok(Arc::clone(subgraph));
        }
        let subgraph = Arc::new(
            self.resolver
                .get()
                .get_subgraph(name, category, true)
                .map_err(|error| error.to_string())?,
        );
        *answer = Some(Arc::clone(&subgraph));
        Ok(subgraph)
    }
}

impl OntologyLookups<'static> {
    fn shared(resolver: Arc<dyn OntologyResolver>) -> Self {
        Self::with(ResolverRef::Shared(resolver))
    }
}

/// Merge the expansions of several units into one, in input order.
///
/// - An entity's name, description, importance weight, metadata and other
///   fields come from the **first unit in input order** that declared it
///   with a real type — or, when none did, the first unit that declared it.
///   An ontology individual that only another document's ontology lookup
///   brought in (typed with the [`ONTOLOGY_INDIVIDUAL_TYPE`] placeholder)
///   gives way to the first real extraction of it. First-unit-wins is a
///   deliberate divergence from Python, where every data item writes its own
///   copy of the entity and the last write wins — with data items running
///   concurrently, whichever finishes last. Only `belongs_to_set` is
///   combined: the entity keeps the NodeSet membership of every unit, as
///   Python keeps a `belongs_to_set` edge per NodeSet.
/// - Its types are combined by [`GraphNodePair::merge_types_from`].
/// - Every EntityType id ends up as one copy, the first one in input order,
///   so the graph write, the index and the ledger all see the same node.
/// - Edges and claimed edges are combined by (source, target, label), the
///   first unit's edge kept.
/// - Producers are combined, so every chunk that declared an artifact still
///   gets an ownership row.
/// - A Timestamp node comes from its first declaration.
/// - Endpoint-resolution counters are summed; the cross-chunk distance is the
///   largest of any unit.
pub fn merge_expansions(expansions: impl IntoIterator<Item = Expansion>) -> Expansion {
    let mut merged = ExpansionMerger::default();
    for expansion in expansions {
        merged.push(expansion);
    }
    merged.finish()
}

/// The accumulator behind [`merge_expansions`]. It keeps everything in the
/// order first seen, and each unit's output is in declaration order, so the
/// merged output is deterministic for a given input.
#[derive(Default)]
struct ExpansionMerger {
    nodes: Vec<GraphNodePair>,
    node_index: HashMap<Uuid, usize>,
    /// The first copy of each EntityType id, in input order: recorded as the
    /// units arrive, not read off the merged nodes, whose order is the order
    /// the *entities* were first seen. Every node pushed gets these copies
    /// before it is merged, so no pass over the merged nodes is needed after.
    first_entity_types: HashMap<Uuid, EntityType>,
    edges: Vec<GraphEdgePair>,
    edge_index: HashMap<(Uuid, Uuid, String), usize>,
    claimed_existing_edges: Vec<GraphEdgePair>,
    claimed_keys: HashSet<(Uuid, Uuid, String)>,
    producers: ArtifactProducers,
    stats: EdgeResolutionStats,
    timestamps: Vec<TimestampNode>,
    timestamp_index: HashMap<Uuid, usize>,
    /// The entities whose first unit brought them in through the ontology
    /// alone (see [`ArtifactProducers::is_ontology_first`]).
    ontology_first: HashSet<Uuid>,
}

impl ExpansionMerger {
    fn push(&mut self, expansion: Expansion) {
        self.producers.merge(expansion.producers.clone());
        self.push_without_producers(expansion);
    }

    /// [`Self::push`], leaving the producers to the caller.
    fn push_without_producers(&mut self, expansion: Expansion) {
        for mut node in expansion.nodes {
            // Each unit built its own copy of a shared EntityType, stamped
            // with that unit's chunk importance weight. Every node gets the
            // first one.
            for entity_type in node.entity_types_mut() {
                let first = self
                    .first_entity_types
                    .entry(entity_type.base.id)
                    .or_insert_with(|| entity_type.clone());
                if *entity_type != *first {
                    *entity_type = first.clone();
                }
            }
            match self.node_index.get(&node.entity.base.id) {
                Some(&index) => {
                    let merged = &mut self.nodes[index];
                    if is_placeholder_only(merged) && has_real_type(&node) {
                        // The first real extraction supplies the fields; the
                        // placeholder copy keeps only its NodeSet membership.
                        let placeholder = std::mem::replace(merged, node);
                        union_node_sets(&mut merged.entity, &placeholder.entity);
                    } else {
                        merged.merge_types_from(&node);
                        union_node_sets(&mut merged.entity, &node.entity);
                    }
                }
                None => {
                    let id = node.entity.base.id;
                    // First seen through the ontology alone: its first unit
                    // has an ontology producer and no extracting one.
                    if expansion.producers.entity_chunks(id).is_empty()
                        && !expansion.producers.ontology_entity_chunks(id).is_empty()
                    {
                        self.ontology_first.insert(id);
                    }
                    self.node_index.insert(id, self.nodes.len());
                    self.nodes.push(node);
                }
            }
        }
        for edge in expansion.edges {
            let key = edge.dedup_key();
            if !self.edge_index.contains_key(&key) {
                self.edge_index.insert(key, self.edges.len());
                self.edges.push(edge);
            }
        }
        for edge in expansion.claimed_existing_edges {
            if self.claimed_keys.insert(edge.dedup_key()) {
                self.claimed_existing_edges.push(edge);
            }
        }
        for node in expansion.timestamps {
            let id = node.timestamp.base.id;
            if !self.timestamp_index.contains_key(&id) {
                self.timestamp_index.insert(id, self.timestamps.len());
                self.timestamps.push(node);
            }
        }
        self.stats.absorb(&expansion.stats);
    }

    fn finish(self) -> Expansion {
        let mut producers = self.producers;
        for id in self.ontology_first {
            producers.mark_ontology_first(id);
        }
        Expansion {
            nodes: self.nodes,
            edges: self.edges,
            claimed_existing_edges: self.claimed_existing_edges,
            producers,
            stats: self.stats,
            timestamps: self.timestamps,
        }
    }
}

/// Whether `node`'s only type is the ontology-individual placeholder.
fn is_placeholder_only(node: &GraphNodePair) -> bool {
    node.entity_type
        .as_ref()
        .is_some_and(is_ontology_individual_placeholder)
        && node.extra_entity_types.is_empty()
}

/// Whether `node` has a type somebody declared.
fn has_real_type(node: &GraphNodePair) -> bool {
    node.entity_types()
        .any(|entity_type| !is_ontology_individual_placeholder(entity_type))
}

/// `nodes` with one node per entity id: the first, with the NodeSet entries
/// of the later ones added.
fn first_node_per_id(nodes: Vec<GraphNodePair>) -> Vec<GraphNodePair> {
    let mut kept: Vec<GraphNodePair> = Vec::with_capacity(nodes.len());
    let mut index: HashMap<Uuid, usize> = HashMap::new();
    for node in nodes {
        match index.get(&node.entity.base.id) {
            Some(&at) => union_node_sets(&mut kept[at].entity, &node.entity),
            None => {
                index.insert(node.entity.base.id, kept.len());
                kept.push(node);
            }
        }
    }
    kept
}

/// Add the NodeSet entries of `other` that `entity` does not have yet.
fn union_node_sets(entity: &mut Entity, other: &Entity) {
    let Some(other_sets) = other.base.belongs_to_set.as_ref() else {
        return;
    };
    let sets = entity.base.belongs_to_set.get_or_insert_with(Vec::new);
    for node_set in other_sets {
        if !sets.contains(node_set) {
            sets.push(node_set.clone());
        }
    }
}

/// One unit's expansion, plus a bounded sample of the endpoint references it
/// could not resolve (for the summary line, logged once by the caller).
fn expand_unit(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    inputs: &UnitInputs<'_>,
) -> (Expansion, Vec<String>) {
    let UnitInputs {
        dataset_id,
        chunk_node_sets,
        chunk_importance_weights,
        ontology,
        user_label,
        task_rank,
        endpoint_scope,
    } = *inputs;
    // Function-local visited set for the pre-stamp pass. The executor's
    // per-run set sees the same DataPoints during its own walk and
    // short-circuits via the `if dp.source_pipeline.is_none()` guard
    // (locked decision 2). The two visited sets do not need to share
    // state.
    let mut local_visited: HashSet<Uuid> = HashSet::new();

    // Maps for deduplication
    //
    // `IndexMap`s, so a unit's output is in declaration order rather than
    // hash order and a run is reproducible.
    let mut node_map: IndexMap<String, GraphNodePair> = IndexMap::new();
    let mut edge_map: IndexMap<(Uuid, Uuid, String), GraphEdgePair> = IndexMap::new();
    let mut type_map = HashMap::new();

    // Every chunk that produced each merged artifact, not only the one that
    // created it (see the `# Returns` note above).
    let mut producers = ArtifactProducers::default();

    // Map from node_id to entity_id for edge resolution. The second element is
    // the position of the chunk that **most recently** declared the key, at or
    // before the chunk currently being read — refreshed on every re-declaration,
    // not just the first, because that is the declaration the referencing
    // chunk could see on its own. It is carried here rather than in a parallel
    // map so attribution costs 8 bytes an entry instead of a second copy of
    // every key (SDK-507 is an allocation ticket). Under
    // `EndpointScope::Chunk` both registries are cleared at every chunk.
    let mut node_id_to_entity_id: HashMap<String, (Uuid, ChunkPosition)> = HashMap::new();

    // Fallback map from a node's human-readable *name* to its entity id, used
    // only when the id map misses. `None` marks a name claimed by two or more
    // distinct entities, which must stay unresolvable. The position follows the
    // same most-recent-declaration rule as the id map above.
    let mut name_to_entity_id: HashMap<String, Option<(Uuid, ChunkPosition)>> = HashMap::new();

    // Timestamp nodes by id, in first-declaration order.
    let mut timestamp_map: HashMap<Uuid, TimestampNode> = HashMap::new();
    let mut timestamp_order: Vec<Uuid> = Vec::new();

    let mut stats = EdgeResolutionStats::default();
    // A bounded sample of unresolved endpoint references, for the summary line.
    let mut unresolved_sample: Vec<String> = Vec::new();

    // Ontology-specific collections (populated by get_subgraph expansion)
    let mut key_mapping: HashMap<String, String> = HashMap::new();
    let mut ontology_types_map: IndexMap<String, EntityType> = IndexMap::new();
    let mut ontology_entities_map: IndexMap<String, GraphNodePair> = IndexMap::new();
    let mut ontology_edge_keys: HashSet<String> = HashSet::new();
    let mut ontology_edges_out: Vec<GraphEdgePair> = Vec::new();
    // The chunk whose ontology lookup first brought in each ontology node and
    // edge of this unit (SDK-1694 O5a): which document the ledger rows of
    // ontology output name. Filled from the entries each lookup appends.
    let mut ontology_node_chunks: HashMap<Uuid, Uuid> = HashMap::new();
    let mut ontology_edge_chunks: Vec<Uuid> = Vec::new();
    let record_ontology_output = |chunk_id: Uuid,
                                  (types_before, entities_before): (usize, usize),
                                  types: &IndexMap<String, EntityType>,
                                  entities: &IndexMap<String, GraphNodePair>,
                                  edges: &[GraphEdgePair],
                                  node_chunks: &mut HashMap<Uuid, Uuid>,
                                  edge_chunks: &mut Vec<Uuid>| {
        for (_, entity_type) in types.iter().skip(types_before) {
            node_chunks.entry(entity_type.base.id).or_insert(chunk_id);
        }
        for (_, pair) in entities.iter().skip(entities_before) {
            node_chunks.entry(pair.entity.base.id).or_insert(chunk_id);
        }
        edge_chunks.resize(edges.len(), chunk_id);
    };

    // Process all graphs — each graph carries its source chunk_id
    // `position` is the chunk's ordinal in this unit. Registry entries record
    // the position that created them so a resolved endpoint can be attributed
    // to its own chunk or to an earlier one — the split that prices
    // chunk-local resolution (`EndpointScope::Chunk`).
    for (position, (chunk_id, graph)) in graphs.into_iter().enumerate() {
        // Python resolves an edge's endpoints only against its own chunk's
        // nodes. Forgetting the earlier chunks' registrations gives the same
        // scope; the entities themselves stay merged, so a re-declaring chunk
        // registers its node ids again below.
        if endpoint_scope == EndpointScope::Chunk {
            node_id_to_entity_id.clear();
            name_to_entity_id.clear();
        }

        // The importance_weight every EntityType / Entity / ontology node
        // extracted from this chunk inherits, mirroring Python's
        // `importance_weight=data_chunk.importance_weight`
        // (expand_with_nodes_and_edges.py:66,79,163,229). Default 0.5 for chunks
        // absent from the map (Python's `DataPoint.importance_weight` default).
        let chunk_importance_weight = chunk_importance_weights
            .get(&chunk_id)
            .copied()
            .unwrap_or(0.5);

        let graph = without_duplicate_node_ids(graph);
        let entity_ids = entity_ids_by_node_id(&graph, chunk_id);

        for node in graph.nodes {
            // A node the LLM typed "Timestamp" whose name parses becomes a
            // shared Timestamp node (Python `_get_or_create_timestamp`); one
            // that does not parse stays an ordinary entity below.
            if is_timestamp_type(&node.node_type) {
                if let Some(timestamp) = Timestamp::from_text(&node.name) {
                    let timestamp_id = timestamp.base.id;
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        timestamp_map.entry(timestamp_id)
                    {
                        let mut timestamp = timestamp;
                        timestamp.base.importance_weight = Some(chunk_importance_weight);
                        pre_stamp_extraction(
                            &mut timestamp,
                            user_label,
                            task_rank,
                            &mut local_visited,
                        );
                        timestamp_order.push(timestamp_id);
                        e.insert(TimestampNode {
                            timestamp,
                            description: node.description.clone(),
                        });
                    }

                    // Register the node id (and its names) for edge resolution
                    // the way an entity's first declaration does, or move the
                    // origin forward on a re-declaration.
                    let id_key = registry_key(&node.id, endpoint_scope);
                    match node_id_to_entity_id.get_mut(&id_key) {
                        Some((entity, origin)) if *entity == timestamp_id => {
                            *origin = position;
                            refresh_name_alias_origin(
                                &mut name_to_entity_id,
                                &node.name,
                                timestamp_id,
                                position,
                            );
                        }
                        _ => {
                            register_name_alias(
                                &mut name_to_entity_id,
                                &node.name,
                                timestamp_id,
                                &id_key,
                                position,
                            );
                            if let Some(registered) = timestamp_map.get(&timestamp_id) {
                                register_name_alias(
                                    &mut name_to_entity_id,
                                    &registered.timestamp.timestamp_str,
                                    timestamp_id,
                                    &id_key,
                                    position,
                                );
                            }
                            node_id_to_entity_id.insert(id_key, (timestamp_id, position));
                        }
                    }
                    producers.record_entity(timestamp_id, chunk_id);
                    continue;
                }
                warn!(
                    "Timestamp {:?} is not a normalized time string; kept as an entity",
                    node.name
                );
            }

            // Step 1: Create or get EntityType (with ontology subgraph expansion)
            //
            // The *stored* type name is normalized (lowercase, apostrophes
            // stripped) exactly as Python does at
            // `expand_with_nodes_and_edges.py:30`
            // (`normalized_type_name = generate_node_name(extracted_type)`).
            // Without this Rust persists the model's raw title-case, so the two
            // SDKs write `Organization` and `organization` for the same type.
            // The id is unaffected — `EntityType::id_for` already normalizes —
            // but the name is an `index_fields` value, so the raw form is what
            // reaches the vector index and every rendered context string.
            //
            // The dedup key is normalized for the same reason Python keys on
            // `str(EntityType.id_for(extracted_type))`: two chunks spelling one
            // type differently must land on one map entry, otherwise they
            // produce two entries carrying the same (normalization-insensitive)
            // id.
            let normalized_type_name = generate_node_name(&node.node_type);
            // An empty or punctuation-only type would become an EntityType
            // named "", which every reader shows as a type with no label;
            // Python's `_get_or_create_entity_type` leaves the entity untyped
            // instead (no EntityType node, no `is_a` edge).
            let entity_type: Option<EntityType> = if normalized_type_name.trim().is_empty() {
                None
            } else {
                let type_key = format!("{normalized_type_name}_type");

                // Check if this key was already remapped to a canonical form
                let effective_key = key_mapping
                    .get(&type_key)
                    .cloned()
                    .unwrap_or_else(|| type_key.clone());

                if !type_map.contains_key(&effective_key) {
                    let mut et =
                        EntityType::from_node_type(&normalized_type_name, Some(dataset_id));
                    // Python: `importance_weight=data_chunk.importance_weight`
                    // (expand_with_nodes_and_edges.py:163).
                    et.base.importance_weight = Some(chunk_importance_weight);
                    pre_stamp_extraction(&mut et, user_label, task_rank, &mut local_visited);

                    if ontology.is_loaded() {
                        match ontology.get_subgraph(&node.node_type, "classes").as_deref() {
                            Ok((onto_nodes, onto_edges, Some(root_node))) => {
                                // Python normalizes the ontology-supplied name too
                                // before storing it
                                // (`construct_data_points_and_edges_with_ontology.py:223`),
                                // while deriving the id from the raw name — the two
                                // agree because `id_for` normalizes anyway.
                                let canonical_name = generate_node_name(&root_node.name);

                                // Canonicalize: rename + regenerate deterministic ID
                                et.mark_ontology_valid(Some(canonical_name.clone()));
                                et.base.id = EntityType::id_for(&canonical_name);

                                // Record key mapping if canonical differs
                                let new_type_key = format!("{canonical_name}_type");
                                if new_type_key != type_key {
                                    key_mapping.insert(type_key.clone(), new_type_key.clone());
                                }

                                // Process ontology subgraph nodes and edges
                                let before =
                                    (ontology_types_map.len(), ontology_entities_map.len());
                                process_ontology_nodes(
                                    onto_nodes,
                                    dataset_id,
                                    chunk_importance_weight,
                                    &node_map,
                                    &type_map,
                                    &mut ontology_types_map,
                                    &mut ontology_entities_map,
                                    user_label,
                                    task_rank,
                                    &mut local_visited,
                                );
                                // The resolver returns the matched root class
                                // *separately* from `onto_nodes` (it is not in the
                                // node list). An ontology `is_a` edge whose endpoint
                                // names that root must still resolve to the root's
                                // class id, so include it in the category lookup for
                                // edge resolution only — NOT in `process_ontology_nodes`,
                                // which would double-create it (the main loop already
                                // created the canonical type node).
                                let mut edge_category_nodes = onto_nodes.clone();
                                edge_category_nodes.push(root_node.clone());
                                process_ontology_edges(
                                    &edge_category_nodes,
                                    onto_edges,
                                    &mut ontology_edge_keys,
                                    &mut ontology_edges_out,
                                );
                                record_ontology_output(
                                    chunk_id,
                                    before,
                                    &ontology_types_map,
                                    &ontology_entities_map,
                                    &ontology_edges_out,
                                    &mut ontology_node_chunks,
                                    &mut ontology_edge_chunks,
                                );

                                // Insert under canonical key
                                type_map.insert(
                                    if new_type_key != type_key {
                                        new_type_key
                                    } else {
                                        effective_key.clone()
                                    },
                                    et,
                                );
                            }
                            Ok((_, _, None)) => {
                                // No match in ontology
                                type_map.insert(effective_key.clone(), et);
                            }
                            Err(e) => {
                                warn!(
                                    "Ontology subgraph extraction failed for '{}': {}",
                                    node.node_type, e
                                );
                                type_map.insert(effective_key.clone(), et);
                            }
                        }
                    } else {
                        type_map.insert(effective_key.clone(), et);
                    }
                }

                // Re-resolve the effective key (may have been remapped above)
                let resolved_key = key_mapping
                    .get(&type_key)
                    .cloned()
                    .unwrap_or_else(|| type_key.clone());
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                let entity_type = type_map
                    .get(&resolved_key)
                    .expect("entity type was just inserted or already existed");
                Some(entity_type.clone())
            };

            // Step 2: Create Entity
            // Cloned into the `entry` call below so the key survives for the
            // canonical-name lookup after the match.
            //
            // Keyed on the final entity id, which derives from the node's
            // name (see `entity_ids_by_node_id`), so the same name declared by
            // two chunks lands on one entity — the first declaration's
            // description wins, as in Python.
            #[allow(clippy::expect_used, reason = "every node of this graph has an id")]
            let entity_id = *entity_ids
                .get(&node.id)
                .expect("entity_ids_by_node_id covers every node of the graph");
            let entity_key = format!("{entity_id}_entity");

            // Validate entity against ontology "individuals" with subgraph expansion.
            // Collect subgraph data for deferred processing (after insert releases borrow).
            let mut deferred_individual_data = None;

            if let indexmap::map::Entry::Vacant(e) = node_map.entry(entity_key.clone()) {
                let mut entity_pair = create_entity_node(
                    &node,
                    entity_id,
                    entity_type.clone(), // Pass the shared entity_type
                    dataset_id,
                    chunk_id,
                    chunk_node_sets.get(&chunk_id),
                    chunk_importance_weight,
                );
                pre_stamp_extraction(
                    &mut entity_pair.entity,
                    user_label,
                    task_rank,
                    &mut local_visited,
                );

                if ontology.is_loaded() {
                    let individual = ontology.get_subgraph(&node.name, "individuals");
                    match individual.as_deref() {
                        Ok((_, _, Some(root_individual))) => {
                            // Normalized for the same reason as the class
                            // canonical name above
                            // (`construct_data_points_and_edges_with_ontology.py:223`).
                            let canonical_name = generate_node_name(&root_individual.name);

                            // Store original name in metadata
                            entity_pair.entity.base.set_metadata(
                                "original_name",
                                serde_json::json!(entity_pair.entity.name.clone()),
                            );

                            // Replace name and ID with canonical form
                            entity_pair.entity.name = canonical_name.clone();
                            entity_pair.entity.base.id = Entity::id_for(&canonical_name);
                            entity_pair.entity.base.set_ontology_valid(true);

                            // Defer subgraph processing until after insert
                            deferred_individual_data = individual.as_ref().ok().cloned();
                        }
                        Ok((_, _, None)) => {}
                        Err(err) => {
                            warn!(
                                "Ontology individual lookup failed for '{}': {}",
                                node.name, err
                            );
                        }
                    }
                }

                // Track node_id -> entity_id mapping for edge resolution.
                // Key on the *normalized* node id so an edge that references the
                // same entity with different casing/spacing still resolves — the
                // way Python's `Entity.id_for` hashing is normalization-insensitive
                // (expand_with_nodes_and_edges.py:300-304).
                let entity_id = entity_pair.entity.base.id;
                let id_key = registry_key(&node.id, endpoint_scope);

                // Alias the name the LLM gave this node, and — when the ontology
                // canonicalised it — the canonical name too, so an edge that
                // references either spelling still resolves.
                //
                // Registration lives here, in the first-declaration branch,
                // alongside the `node_id_to_entity_id` insert below: creating a
                // name→entity mapping is a first-declaration act. The post-match
                // block runs on re-declarations too and only moves origins.
                register_name_alias(
                    &mut name_to_entity_id,
                    &node.name,
                    entity_id,
                    &id_key,
                    position,
                );
                register_name_alias(
                    &mut name_to_entity_id,
                    &entity_pair.entity.name,
                    entity_id,
                    &id_key,
                    position,
                );

                // The origin refresh happens after the match, so a chunk that
                // *re*-declares this node id runs it too — see that block.
                node_id_to_entity_id.insert(id_key, (entity_id, position));

                e.insert(entity_pair);
            }

            // Process deferred ontology individual subgraph (outside the Vacant borrow)
            if let Some(subgraph) = deferred_individual_data {
                let (ont_nodes, ont_edges, _) = &*subgraph;
                let before = (ontology_types_map.len(), ontology_entities_map.len());
                process_ontology_nodes(
                    ont_nodes,
                    dataset_id,
                    chunk_importance_weight,
                    &node_map,
                    &type_map,
                    &mut ontology_types_map,
                    &mut ontology_entities_map,
                    user_label,
                    task_rank,
                    &mut local_visited,
                );
                process_ontology_edges(
                    ont_nodes,
                    ont_edges,
                    &mut ontology_edge_keys,
                    &mut ontology_edges_out,
                );
                record_ontology_output(
                    chunk_id,
                    before,
                    &ontology_types_map,
                    &ontology_entities_map,
                    &ontology_edges_out,
                    &mut ontology_node_chunks,
                    &mut ontology_edge_chunks,
                );
            }

            // Record this chunk as a producer whether or not it was the chunk
            // that created the entity, register this declaration's node id
            // against it, and refresh the registry origins to this chunk.
            //
            // The entity is read back out of `node_map`, not from the vacant
            // branch: it may be one an earlier chunk created under the same
            // name (ids derive from the name now), and after an ontology
            // individual rewrote `entity.base.id` that map holds the canonical
            // id. So the node id is registered here, on every declaration —
            // a later chunk that names an existing entity under a node id of
            // its own ("a" for an earlier "alice_1") must still resolve its
            // edges, own the entity and link it from its chunk.
            //
            // The origin follows the **most recent** declaration at or before
            // this chunk, not the first: extraction re-declares a recurring
            // entity in every chunk that mentions it, and attributing to the
            // first declaration would count a chunk-local resolution as
            // cross-chunk and drag `max_cross_chunk_distance` toward the corpus
            // length — the opposite of the signal that counter exists to give.
            #[allow(
                clippy::expect_used,
                reason = "the entry was just filled or already existed"
            )]
            let entity_id = node_map
                .get(&entity_key)
                .map(|pair| pair.entity.base.id)
                .expect("node_map holds this node's entity after the vacant check");
            let id_key = registry_key(&node.id, endpoint_scope);
            match node_id_to_entity_id.get_mut(&id_key) {
                Some((entity, origin)) if *entity == entity_id => *origin = position,
                _ => {
                    // First declaration of this node id, or the model reusing an
                    // id an earlier chunk gave a different entity: this chunk's
                    // edges mean this entity, as Python resolves them per chunk.
                    register_name_alias(
                        &mut name_to_entity_id,
                        &node.name,
                        entity_id,
                        &id_key,
                        position,
                    );
                    node_id_to_entity_id.insert(id_key, (entity_id, position));
                }
            }

            // Keep the alias origins in step with the id origin above — and
            // *only* the origins: a re-declaration's name may differ from the
            // one that registered the entity, and registering it as a new alias
            // could poison a name another entity already owns. The `node_map`
            // lookup picks up an ontology-canonicalised name, whose alias the
            // declaring chunk registered under that same name.
            refresh_name_alias_origin(&mut name_to_entity_id, &node.name, entity_id, position);
            if let Some(pair) = node_map.get(&entity_key) {
                refresh_name_alias_origin(
                    &mut name_to_entity_id,
                    &pair.entity.name,
                    entity_id,
                    position,
                );
            }

            producers.record_entity(entity_id, chunk_id);
        }

        // Step 3: Create Edges. Whether the graph already holds one is decided
        // after the merge (`Expansion::split_existing_edges`).
        for edge in graph.edges {
            // Look up entity IDs from node IDs; skip edges the LLM produced with
            // node IDs that don't match any extracted node (common with local models).
            stats.attempted += 1;

            let source_entity_id = match resolve_endpoint(
                &node_id_to_entity_id,
                &name_to_entity_id,
                &edge.source_node_id,
                endpoint_scope,
            ) {
                EndpointResolution::ById { entity, origin } => {
                    stats.record_resolved_by_id(origin, position);
                    entity
                }
                EndpointResolution::ByName { entity, origin } => {
                    stats.record_resolved_by_name(origin, position);
                    entity
                }
                EndpointResolution::Ambiguous => {
                    stats.dropped_ambiguous_name += 1;
                    debug!(
                        "Skipping edge: source '{}' matches a name shared by several entities",
                        edge.source_node_id
                    );
                    continue;
                }
                EndpointResolution::Missing => {
                    stats.dropped_source_missing += 1;
                    if unresolved_sample.len() < UNRESOLVED_SAMPLE_LIMIT {
                        unresolved_sample.push(edge.source_node_id.clone());
                    }
                    debug!(
                        "Skipping edge: source node '{}' not found in extracted nodes",
                        edge.source_node_id
                    );
                    continue;
                }
            };

            let target_entity_id = match resolve_endpoint(
                &node_id_to_entity_id,
                &name_to_entity_id,
                &edge.target_node_id,
                endpoint_scope,
            ) {
                EndpointResolution::ById { entity, origin } => {
                    stats.record_resolved_by_id(origin, position);
                    entity
                }
                EndpointResolution::ByName { entity, origin } => {
                    stats.record_resolved_by_name(origin, position);
                    entity
                }
                EndpointResolution::Ambiguous => {
                    stats.dropped_ambiguous_name += 1;
                    debug!(
                        "Skipping edge: target '{}' matches a name shared by several entities",
                        edge.target_node_id
                    );
                    continue;
                }
                EndpointResolution::Missing => {
                    stats.dropped_target_missing += 1;
                    if unresolved_sample.len() < UNRESOLVED_SAMPLE_LIMIT {
                        unresolved_sample.push(edge.target_node_id.clone());
                    }
                    debug!(
                        "Skipping edge: target node '{}' not found in extracted nodes",
                        edge.target_node_id
                    );
                    continue;
                }
            };

            // Timestamps are leaves (Python `_with_timestamps_as_leaves`). The
            // prompt asks for `marie_curie -born_at-> 1867`; when the model
            // draws it the other way (`1898 -discovery_of-> radium`) the fact is
            // still that radium is tied to 1898, so the edge is reversed and
            // keeps its name. An edge between two timestamps has no entity to
            // live on and is dropped. Decided on the *resolved* endpoints, so a
            // timestamp declared by an earlier chunk, or reached through its
            // name alias, counts too.
            let (source_entity_id, target_entity_id) = match (
                timestamp_map.contains_key(&source_entity_id),
                timestamp_map.contains_key(&target_entity_id),
            ) {
                (true, true) => {
                    warn!(
                        "Dropping edge {:?} between timestamps {:?} and {:?}",
                        edge.relationship_name, edge.source_node_id, edge.target_node_id
                    );
                    continue;
                }
                (true, false) => {
                    debug!(
                        "Reversing edge {:?} drawn from timestamp {:?} to {:?}",
                        edge.relationship_name, edge.source_node_id, edge.target_node_id
                    );
                    (target_entity_id, source_entity_id)
                }
                _ => (source_entity_id, target_entity_id),
            };

            // The stored label is normalised as Python's `_add_extracted_edges`
            // does (`generate_edge_name`: lowercase, spaces to underscores,
            // apostrophes dropped) before the edge key is built, so `Born In`
            // and `born_in` are one edge. The reversal above keeps the label,
            // so the order of the two does not matter.
            let relationship_name = generate_edge_name(&edge.relationship_name);
            let edge_key = (source_entity_id, target_entity_id, relationship_name);

            // Same reasoning as for entities: every chunk that yielded this
            // edge is a producer, not only the one that created it — and that
            // includes a chunk whose edge an earlier run already put in the
            // graph. Such an edge is not written again, but this run produced
            // it and must own it (see `claimed_existing_edges`), so it is
            // recorded whether or not the graph holds it.
            producers.record_edge(edge_key.clone(), chunk_id);

            if let indexmap::map::Entry::Vacant(e) = edge_map.entry(edge_key) {
                // Mirror Python's `_process_graph_edges` edge property map
                // (expand_with_nodes_and_edges.py:296-309): persist
                // relationship_name / source_node_id / target_node_id /
                // ontology_valid / edge_text. `edge_text` is the trimmed
                // `Edge.description` (Python `_strip_nonblank_text`), feeding
                // EdgeType + Triplet embeddings; empty string when absent/blank
                // so downstream readers fall back to relationship_name.
                let edge_text = edge
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string();

                let relationship_name = e.key().2.clone();
                let mut edge_pair =
                    GraphEdgePair::new(source_entity_id, target_entity_id, &relationship_name);
                edge_pair.add_property("relationship_name", relationship_name);
                edge_pair.add_property("source_node_id", source_entity_id.to_string());
                edge_pair.add_property("target_node_id", target_entity_id.to_string());
                edge_pair.add_property("ontology_valid", "false");
                edge_pair.add_property("edge_text", edge_text);

                e.insert(edge_pair);
            }
        }
    }

    // Merge LLM-extracted nodes with ontology-derived nodes
    let mut graph_nodes: Vec<GraphNodePair> = node_map.into_values().collect();

    // Convert ontology-derived class types into GraphNodePairs (as "type nodes")
    for et in ontology_types_map.into_values() {
        let entity = Entity::from_node(
            &et.name,
            &et.name,
            format!("Ontology-derived type: {}", et.name),
            et.base.id,
            Some(dataset_id),
        );
        // The class's lookup chunk (recorded against the type id) owns the
        // node made for it too.
        if let Some(chunk_id) = ontology_node_chunks.get(&et.base.id).copied() {
            ontology_node_chunks
                .entry(entity.base.id)
                .or_insert(chunk_id);
        }
        graph_nodes.push(GraphNodePair::new(entity, Some(et)));
    }

    // Add ontology-derived individual nodes
    graph_nodes.extend(ontology_entities_map.into_values());

    // Two names can end on one entity: `node_map` is keyed on the name-based
    // id, and an ontology individual match then rewrites the entity's id to
    // the canonical one ("Alice" and "alice_canonical" both become
    // `alice_canonical`). Python's `_get_or_create_entity` returns the entity
    // already made for that id, so here too the first one wins — its type and
    // fields — and only NodeSet membership is combined. Producers are already
    // keyed on the final id. The merger then only combines across units.
    let graph_nodes = first_node_per_id(graph_nodes);

    // Type and ontology producers (SDK-1694 O5a). An entity the unit
    // extracted is owned by its extracting chunks; one only the ontology
    // brought in, by the chunk whose lookup did. Either way its owners own its
    // type — in a unit an entity has one — and so the `is_a` edge to it.
    for pair in &graph_nodes {
        let entity_id = pair.entity.base.id;
        let mut owners = producers.entity_chunks(entity_id).to_vec();
        if owners.is_empty()
            && let Some(chunk_id) = ontology_node_chunks.get(&entity_id).copied()
        {
            producers.record_ontology_entity(entity_id, chunk_id);
            owners.push(chunk_id);
        }
        for entity_type in pair.entity_types() {
            for chunk_id in &owners {
                producers.record_entity_type(entity_id, entity_type.base.id, *chunk_id);
            }
        }
    }
    for (edge, chunk_id) in ontology_edges_out.iter().zip(&ontology_edge_chunks) {
        producers.record_ontology_edge(edge.dedup_key(), *chunk_id);
    }

    // Merge LLM-extracted edges with ontology-derived edges
    let mut graph_edges: Vec<GraphEdgePair> = edge_map.into_values().collect();
    graph_edges.extend(ontology_edges_out);

    let timestamps = timestamp_order
        .into_iter()
        .filter_map(|id| timestamp_map.remove(&id))
        .collect();

    (
        Expansion {
            nodes: graph_nodes,
            edges: graph_edges,
            claimed_existing_edges: Vec::new(),
            producers,
            stats,
            timestamps,
        },
        unresolved_sample,
    )
}

/// Log, once per expansion, how many edges endpoint resolution dropped and
/// how many endpoints matched a node from an earlier chunk of their unit.
fn log_resolution_summary(stats: &EdgeResolutionStats, unresolved_sample: &[String]) {
    if stats.dropped() > 0 {
        // The sample lists unmatched references only; an ambiguous endpoint did
        // match, just not to a single entity, so naming it would not help.
        let sample = if unresolved_sample.is_empty() {
            String::new()
        } else {
            format!(" (unmatched sample: {})", unresolved_sample.join(", "))
        };
        warn!(
            attempted = stats.attempted,
            dropped = stats.dropped(),
            source_missing = stats.dropped_source_missing,
            target_missing = stats.dropped_target_missing,
            ambiguous_name = stats.dropped_ambiguous_name,
            recovered_by_name = stats.resolved_by_name,
            "Dropped {} of {} extracted edges in endpoint resolution: {} unmatched source, \
             {} unmatched target, {} ambiguous name{}",
            stats.dropped(),
            stats.attempted,
            stats.dropped_source_missing,
            stats.dropped_target_missing,
            stats.dropped_ambiguous_name,
            sample
        );
    } else if stats.resolved_by_name > 0 {
        debug!(
            recovered_by_name = stats.resolved_by_name,
            "All extracted edges resolved; {} endpoint(s) needed the node-name fallback",
            stats.resolved_by_name
        );
    }

    // Report the cross-chunk share separately from the drop rate: it is not a
    // loss, it is what resolving only within the edge's own chunk (Python's
    // rule, `EndpointScope::Chunk`) would drop. Resolution never crosses a
    // unit (one data item or block of it), so these all stay inside one.
    if stats.resolved_cross_chunk() > 0 {
        debug!(
            cross_chunk = stats.resolved_cross_chunk(),
            cross_chunk_by_id = stats.resolved_by_id_cross_chunk,
            cross_chunk_by_name = stats.resolved_by_name_cross_chunk,
            resolved_by_id = stats.resolved_by_id,
            resolved_by_name = stats.resolved_by_name,
            max_distance = stats.max_cross_chunk_distance,
            "{} endpoint(s) resolved against a node from an earlier chunk of the same unit \
             (max {} chunks back); chunk-local resolution would drop these",
            stats.resolved_cross_chunk(),
            stats.max_cross_chunk_distance
        );
    }
}

/// Helper: Create Entity from Node.
///
/// Mirrors Python's `_create_entity_node()` function.
///
/// `chunk_belongs_to_set` carries the source chunk's `belongs_to_set` NodeSet
/// entries (or `None` when the chunk has no NodeSet metadata). Python assigns
/// them wholesale (`Entity(belongs_to_set=data_chunk.belongs_to_set)`,
/// expand_with_nodes_and_edges.py:227); Rust instead seeds `belongs_to_set` with
/// the dataset-id entry in `Entity::new`, so we UNION the chunk's NodeSet entries
/// in rather than overwriting — preserving dataset scoping while making the
/// entity discoverable by a node_name-scoped (NodeSet-filtered) search.
///
/// `entity_type` is `None` for an untyped entity (blank extracted type): the
/// entity then has no `is_a`.
///
/// `importance_weight` is the source chunk's `importance_weight`, stamped onto
/// the entity to mirror Python's `Entity(importance_weight=data_chunk.importance_weight)`
/// (expand_with_nodes_and_edges.py:229).
fn create_entity_node(
    node: &Node,
    entity_id: Uuid,
    entity_type: Option<EntityType>,
    dataset_id: Uuid,
    chunk_id: Uuid,
    chunk_belongs_to_set: Option<&Vec<serde_json::Value>>,
    importance_weight: f64,
) -> GraphNodePair {
    // The *stored* entity name is normalized (lowercase, apostrophes stripped),
    // mirroring Python's `name=generate_node_name(extracted_node.name)`
    // (expand_with_nodes_and_edges.py:55). The lowercase form is Python's
    // canonical stored value, not LLM noise, and `name` is an `index_fields`
    // value — so leaving the model's raw title-case here is what made the two
    // SDKs embed and render `Machine Learning` against `machine learning`.
    //
    // The description is deliberately left untouched: Python passes
    // `description=extracted_node.description` through unnormalized (line 57).
    //
    // `entity_id` is the caller's name-based (or graph-scoped ordinal) id —
    // see `entity_ids_by_node_id`. The LLM's own node id is kept in metadata.
    let mut entity = Entity::new(
        generate_node_name(&node.name),
        entity_type.as_ref().map(|et| et.base.id),
        &node.description,
        Some(dataset_id),
    );
    entity.base.id = entity_id;
    entity
        .base
        .set_metadata("original_node_id", serde_json::json!(node.id));

    // Store chunk_id reference in metadata
    let mut entity_with_chunk = entity;
    entity_with_chunk
        .base
        .set_metadata("chunk_id", serde_json::json!(chunk_id.to_string()));

    // Inherit the source chunk's importance_weight (Python line 229).
    entity_with_chunk.base.importance_weight = Some(importance_weight);

    // Inherit the source chunk's NodeSet membership (union, not replace).
    if let Some(chunk_sets) = chunk_belongs_to_set {
        let belongs_to_set = entity_with_chunk
            .base
            .belongs_to_set
            .get_or_insert_with(Vec::new);
        for node_set in chunk_sets {
            if !belongs_to_set.contains(node_set) {
                belongs_to_set.push(node_set.clone());
            }
        }
    }

    GraphNodePair::new(entity_with_chunk, entity_type)
}

/// Convert ontology subgraph nodes into graph integration types.
///
/// For each [`AttachedOntologyNode`]:
/// - **Classes** become [`EntityType`] entries in `ontology_types_map`
/// - **Individuals** become [`GraphNodePair`] entries in `ontology_entities_map`
///
/// All produced items receive deterministic UUID5 IDs and `ontology_valid = true`.
/// Duplicates are skipped when a matching key already exists in the LLM-produced
/// maps (`node_map`, `type_map`) or in the ontology output maps.
#[allow(clippy::too_many_arguments)]
fn process_ontology_nodes(
    ontology_nodes: &[AttachedOntologyNode],
    dataset_id: Uuid,
    importance_weight: f64,
    node_map: &IndexMap<String, GraphNodePair>,
    type_map: &HashMap<String, EntityType>,
    ontology_types_map: &mut IndexMap<String, EntityType>,
    ontology_entities_map: &mut IndexMap<String, GraphNodePair>,
    user_label: Option<&str>,
    task_rank: Option<i32>,
    visited: &mut HashSet<Uuid>,
) {
    for node in ontology_nodes {
        match node.category {
            NodeCategory::Classes => {
                // Python: `EntityType.id_for(name)` for ontology class nodes —
                // derived from the raw name, which is equivalent because
                // `id_for` normalizes its input.
                let node_id = EntityType::id_for(&node.name);
                // Python stores the *normalized* name and uses it as the
                // description as well
                // (`construct_data_points_and_edges_with_ontology.py:223-231`).
                let normalized_name = generate_node_name(&node.name);
                let dedup_key = format!("{node_id}_type");
                // Skip if the LLM already extracted this type (check by
                // name-based key — normalized, matching how `type_map` is keyed
                // in the main loop).
                let llm_type_key = format!("{normalized_name}_type");
                if type_map.contains_key(&llm_type_key)
                    || ontology_types_map.contains_key(&dedup_key)
                {
                    continue;
                }
                // Also skip if there is already a node_map entry for this node id
                let node_entity_key = format!("{node_id}_entity");
                if node_map.contains_key(&node_entity_key) {
                    continue;
                }

                let mut et = EntityType::new(&normalized_name, &normalized_name, Some(dataset_id));
                et.base.id = node_id;
                et.base.set_ontology_valid(true);
                // Python: `importance_weight=data_chunk.importance_weight`
                // (expand_with_nodes_and_edges.py:66).
                et.base.importance_weight = Some(importance_weight);
                pre_stamp_extraction(&mut et, user_label, task_rank, visited);
                ontology_types_map.insert(dedup_key, et);
            }
            NodeCategory::Individuals => {
                // Python: `Entity.id_for(name)` for ontology individual nodes —
                // raw name in, normalized inside `id_for`.
                let node_id = Entity::id_for(&node.name);
                // Python stores the normalized name as both name and
                // description
                // (`construct_data_points_and_edges_with_ontology.py:232-240`).
                let normalized_name = generate_node_name(&node.name);
                let dedup_key = format!("{node_id}_entity");
                // Skip if already present in either map
                if node_map.contains_key(&dedup_key)
                    || ontology_entities_map.contains_key(&dedup_key)
                {
                    continue;
                }

                let mut entity =
                    Entity::new(&normalized_name, None, &normalized_name, Some(dataset_id));
                entity.base.id = node_id;
                entity.base.set_ontology_valid(true);
                // Python: `importance_weight=data_chunk.importance_weight`
                // (expand_with_nodes_and_edges.py:79).
                entity.base.importance_weight = Some(importance_weight);
                pre_stamp_extraction(&mut entity, user_label, task_rank, visited);

                // Placeholder EntityType for the GraphNodePair (Rust-only; the
                // Python `Entity(is_a=...)` field is optional). Its id is stable
                // but has no Python counterpart.
                let mut placeholder_et =
                    EntityType::new(ONTOLOGY_INDIVIDUAL_TYPE, "", Some(dataset_id));
                placeholder_et.base.id = EntityType::id_for(ONTOLOGY_INDIVIDUAL_TYPE);
                pre_stamp_extraction(&mut placeholder_et, user_label, task_rank, visited);

                let pair = GraphNodePair::new(entity, Some(placeholder_et));
                ontology_entities_map.insert(dedup_key, pair);
            }
        }
    }
}

/// Convert ontology edge tuples into [`GraphEdgePair`] objects.
///
/// Each `(source, relation, target)` tuple is mapped to a [`GraphEdgePair`] with
/// deterministic UUID5 source/target IDs and normalized relationship names. An
/// edge already emitted for this unit (in `ontology_edge_keys`) is skipped;
/// whether the graph already holds one is decided after the merge
/// ([`Expansion::split_existing_edges`]).
fn process_ontology_edges(
    ontology_nodes: &[AttachedOntologyNode],
    ontology_edges: &[OntologyEdge],
    ontology_edge_keys: &mut HashSet<String>,
    ontology_edges_out: &mut Vec<GraphEdgePair>,
) {
    // Mirror Python's `node_category = {node.name: node.category ...}`: an edge
    // endpoint that names a class resolves via `EntityType::id_for`, otherwise
    // `Entity::id_for` (expand_with_nodes_and_edges.py:84-89). Endpoints not in
    // the node list default to Entity, as in Python.
    //
    // Keyed on `generate_edge_name(node.name)` on both sides, as Python does
    // (`construct_data_points_and_edges_with_ontology.py:251-255`), so an edge
    // endpoint whose spelling differs from the node declaration only in case or
    // spacing still finds its category instead of silently defaulting to
    // Entity.
    let is_class: HashMap<String, bool> = ontology_nodes
        .iter()
        .map(|n| {
            (
                generate_edge_name(&n.name),
                matches!(n.category, NodeCategory::Classes),
            )
        })
        .collect();
    let endpoint_id = |name: &str| -> Uuid {
        if is_class
            .get(&generate_edge_name(name))
            .copied()
            .unwrap_or(false)
        {
            EntityType::id_for(name)
        } else {
            Entity::id_for(name)
        }
    };

    for (source, relation, target) in ontology_edges {
        let source_id = endpoint_id(source);
        let target_id = endpoint_id(target);
        let rel_name = generate_edge_name(relation);
        let edge_key = format!("{source_id}_{target_id}_{rel_name}");

        if ontology_edge_keys.contains(&edge_key) {
            continue;
        }

        let mut edge = GraphEdgePair::new(source_id, target_id, &rel_name);
        edge.add_property("ontology_valid", "true");
        edge.add_property("relationship_name", &rel_name);
        edge.add_property("source_node_id", source_id.to_string());
        edge.add_property("target_node_id", target_id.to_string());

        ontology_edge_keys.insert(edge_key);
        ontology_edges_out.push(edge);
    }
}

/// The graph of a run, merged one document at a time (SDK-1694 O4b).
///
/// The incremental form of [`merge_expansions`]: each document's units are
/// pushed in order, so after the last document the merged nodes, edges and
/// Timestamps are exactly what merging every unit of the run at once gives —
/// the first block in input order sets an entity's fields, types are
/// combined, NodeSet memberships are unioned, an EntityType id keeps its first
/// copy and an edge key its first edge.
///
/// [`Self::add_document`] returns the document's **view**: the current merged
/// copy of every entity, edge and Timestamp the document produced, and the
/// document's own producers. An entity an earlier document already declared
/// comes back as merged so far, so it carries this document's new types and
/// NodeSets as well as the earlier ones, and the earlier document's fields.
#[derive(Default)]
pub(crate) struct RunGraph {
    merged: ExpansionMerger,
}

/// One document's part of a [`RunGraph`].
#[derive(Debug, Default)]
pub(crate) struct DocumentGraph {
    /// The merged copies of the entities the document produced, in run order.
    pub(crate) nodes: Vec<GraphNodePair>,
    /// The first copies of the edges the document produced, in run order.
    pub(crate) edges: Vec<GraphEdgePair>,
    /// The Timestamps the document named, in run order.
    pub(crate) timestamps: Vec<TimestampNode>,
    /// The document's own producers.
    pub(crate) producers: ArtifactProducers,
}

impl RunGraph {
    /// Merge one document's unit expansions, in unit order, and return its
    /// view. `prepare` runs on the merged copy of each of the document's
    /// entities before the view is taken (the extraction stamp), and must be
    /// idempotent: an entity several documents produced passes through it once
    /// per document.
    pub(crate) fn add_document(
        &mut self,
        expansions: Vec<Expansion>,
        mut prepare: impl FnMut(&mut GraphNodePair),
    ) -> DocumentGraph {
        let mut node_ids: Vec<Uuid> = Vec::new();
        let mut edge_keys: Vec<(Uuid, Uuid, String)> = Vec::new();
        let mut timestamp_ids: Vec<Uuid> = Vec::new();
        let mut producers = ArtifactProducers::default();
        for expansion in expansions {
            node_ids.extend(expansion.nodes.iter().map(|node| node.entity.base.id));
            edge_keys.extend(expansion.edges.iter().map(GraphEdgePair::dedup_key));
            timestamp_ids.extend(expansion.timestamps.iter().map(|t| t.timestamp.base.id));
            producers.merge(expansion.producers.clone());
            self.merged.push_without_producers(expansion);
        }

        let merged = &mut self.merged;
        let mut node_positions: Vec<usize> = node_ids
            .iter()
            .filter_map(|id| merged.node_index.get(id).copied())
            .collect();
        node_positions.sort_unstable();
        node_positions.dedup();
        let mut edge_positions: Vec<usize> = edge_keys
            .iter()
            .filter_map(|key| merged.edge_index.get(key).copied())
            .collect();
        edge_positions.sort_unstable();
        edge_positions.dedup();
        let mut timestamp_positions: Vec<usize> = timestamp_ids
            .iter()
            .filter_map(|id| merged.timestamp_index.get(id).copied())
            .collect();
        timestamp_positions.sort_unstable();
        timestamp_positions.dedup();

        let nodes: Vec<GraphNodePair> = node_positions
            .into_iter()
            .map(|position| {
                let node = &mut merged.nodes[position];
                prepare(node);
                node.clone()
            })
            .collect();
        // A run-level fact the document's own producers cannot know: whether
        // the entity's first appearance in the run was ontology-only.
        for node in &nodes {
            if merged.ontology_first.contains(&node.entity.base.id) {
                producers.mark_ontology_first(node.entity.base.id);
            }
        }
        DocumentGraph {
            nodes,
            edges: edge_positions
                .into_iter()
                .map(|position| merged.edges[position].clone())
                .collect(),
            timestamps: timestamp_positions
                .into_iter()
                .map(|position| merged.timestamps[position].clone())
                .collect(),
            producers,
        }
    }

    /// The merged copy of the edge with `key`, to stamp its `edge_text` once
    /// it is known to be new.
    pub(crate) fn edge_mut(&mut self, key: &(Uuid, Uuid, String)) -> Option<&mut GraphEdgePair> {
        let position = *self.merged.edge_index.get(key)?;
        self.merged.edges.get_mut(position)
    }

    /// Every merged entity, in run order.
    pub(crate) fn into_parts(self) -> (Vec<GraphNodePair>, Vec<GraphEdgePair>) {
        (self.merged.nodes, self.merged.edges)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::fact_extraction::Edge;
    use cognee_ontology::{NoOpOntologyResolver, OntologyResult, traits::OntologySubgraph};
    use std::collections::BTreeSet;

    /// Helper to get the default no-op resolver used by most tests.
    fn noop() -> NoOpOntologyResolver {
        NoOpOntologyResolver::new()
    }

    fn create_test_graph() -> KnowledgeGraph {
        KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A technology company".to_string(),
                },
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A software engineer".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "techcorp_1".to_string(),
                relationship_name: "works_at".to_string(),
                description: None,
            }],
        }
    }

    // ----- Timestamp nodes (temporal-memify-10) -----

    fn node(id: &str, name: &str, node_type: &str, description: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            node_type: node_type.to_string(),
            description: description.to_string(),
        }
    }

    fn edge(source: &str, target: &str, relationship: &str) -> Edge {
        Edge {
            source_node_id: source.to_string(),
            target_node_id: target.to_string(),
            relationship_name: relationship.to_string(),
            description: None,
        }
    }

    async fn expand(graphs: Vec<(Uuid, KnowledgeGraph)>) -> Expansion {
        expand_extracted_graphs(
            graphs,
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn a_parseable_timestamp_node_becomes_a_shared_timestamp() {
        let first = KnowledgeGraph {
            nodes: vec![
                node("marie_curie", "Marie Curie", "Person", "A physicist"),
                node("1867", "1867", "Timestamp", "Her birth year"),
            ],
            edges: vec![edge("marie_curie", "1867", "born_at")],
        };
        let second = KnowledgeGraph {
            nodes: vec![
                node("pierre", "Pierre Curie", "Person", "A physicist"),
                node("year_1867", "1867", "timestamp", ""),
            ],
            edges: vec![edge("pierre", "year_1867", "mentioned_at")],
        };
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let out = expand(vec![(chunk_a, first), (chunk_b, second)]).await;

        // One Timestamp, no Entity or EntityType for it.
        assert_eq!(out.timestamps.len(), 1);
        let ts = &out.timestamps[0];
        assert_eq!(ts.timestamp.timestamp_str, "1867");
        assert_eq!(ts.timestamp.base.id, Timestamp::id_for("1867"));
        assert_eq!(ts.description, "Her birth year");
        assert_eq!(out.nodes.len(), 2);
        assert!(
            out.nodes
                .iter()
                .all(|n| n.entity_type.as_ref().unwrap().name == "person")
        );

        // Both `*_at` edges resolve to the Timestamp, and both chunks produced it.
        assert_eq!(out.edges.len(), 2);
        assert!(
            out.edges
                .iter()
                .all(|e| e.target_entity_id == ts.timestamp.base.id)
        );
        assert_eq!(
            out.producers.entity_chunks(ts.timestamp.base.id),
            &[chunk_a, chunk_b]
        );
        assert_eq!(out.stats.dropped(), 0);
    }

    #[tokio::test]
    async fn edges_from_a_timestamp_are_reversed_and_between_timestamps_dropped() {
        let graph = KnowledgeGraph {
            nodes: vec![
                node("radium", "Radium", "Element", ""),
                node("1898", "1898", "Timestamp", ""),
                node("1902", "1902", "Timestamp", ""),
            ],
            edges: vec![
                edge("1898", "radium", "discovery_of"),
                edge("1898", "1902", "precedes"),
            ],
        };
        let out = expand(vec![(Uuid::new_v4(), graph)]).await;

        assert_eq!(out.timestamps.len(), 2);
        assert_eq!(out.edges.len(), 1);
        let reversed = &out.edges[0];
        assert_eq!(reversed.relationship_name, "discovery_of");
        assert_eq!(reversed.source_entity_id, Entity::id_for("radium"));
        assert_eq!(reversed.target_entity_id, Timestamp::id_for("1898"));
    }

    #[tokio::test]
    async fn an_unparseable_timestamp_stays_an_entity() {
        let graph = KnowledgeGraph {
            nodes: vec![node("spring", "that spring", "Timestamp", "")],
            edges: vec![],
        };
        let out = expand(vec![(Uuid::new_v4(), graph)]).await;
        assert!(out.timestamps.is_empty());
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].entity_type.as_ref().unwrap().name, "timestamp");
    }

    #[tokio::test]
    async fn prose_and_span_timestamps_are_normalized() {
        let graph = KnowledgeGraph {
            nodes: vec![
                node("war", "Napoleonic Wars", "Event", ""),
                node("d", "23 March 1947", "Timestamp", ""),
                node("s", "from 1803 to 1815", "Timestamp", ""),
            ],
            edges: vec![edge("war", "s", "lasted_during")],
        };
        let out = expand(vec![(Uuid::new_v4(), graph)]).await;
        let strs: Vec<&str> = out
            .timestamps
            .iter()
            .map(|t| t.timestamp.timestamp_str.as_str())
            .collect();
        assert_eq!(strs, vec!["1947-03-23", "1803/1815"]);
        assert_eq!(
            out.edges[0].target_entity_id,
            Timestamp::id_for("1803/1815")
        );
    }

    // ----- Identity rules (extraction-04) -----

    #[tokio::test]
    async fn entity_ids_derive_from_the_name_not_the_llm_node_id() {
        let first = KnowledgeGraph {
            nodes: vec![node("alice_1", "Alice", "Person", "First")],
            edges: vec![],
        };
        let second = KnowledgeGraph {
            nodes: vec![node("a", "Alice", "Person", "Second")],
            edges: vec![],
        };
        let out = expand(vec![(Uuid::new_v4(), first), (Uuid::new_v4(), second)]).await;
        // Python: `Entity.id_for(node.name)` — one entity across chunks, the
        // first declaration's description kept.
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].entity.base.id, Entity::id_for("Alice"));
        assert_eq!(out.nodes[0].entity.description, "First");
        assert_eq!(
            out.nodes[0].entity.base.get_metadata("original_node_id"),
            Some(&serde_json::json!("alice_1"))
        );
    }

    #[tokio::test]
    async fn a_later_chunk_naming_an_entity_under_a_new_node_id_still_resolves_and_owns_it() {
        let first = KnowledgeGraph {
            nodes: vec![node("alice_1", "Alice", "Person", "")],
            edges: vec![],
        };
        let second = KnowledgeGraph {
            nodes: vec![
                node("a", "Alice", "Person", ""),
                node("acme", "Acme", "Organization", ""),
            ],
            edges: vec![edge("a", "acme", "works_at")],
        };
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let out = expand(vec![(chunk_a, first), (chunk_b, second)]).await;

        let alice = Entity::id_for("Alice");
        assert_eq!(out.stats.dropped(), 0, "{:?}", out.stats);
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0].source_entity_id, alice);
        assert_eq!(out.edges[0].target_entity_id, Entity::id_for("Acme"));
        // Both chunks own Alice, so deleting the first file keeps her.
        assert_eq!(out.producers.entity_chunks(alice), &[chunk_a, chunk_b]);
    }

    #[tokio::test]
    async fn a_node_id_reused_for_another_entity_resolves_to_that_chunks_entity() {
        let first = KnowledgeGraph {
            nodes: vec![
                node("1", "Carol", "Person", ""),
                node("alice", "Alice", "Person", ""),
            ],
            edges: vec![],
        };
        let second = KnowledgeGraph {
            nodes: vec![
                node("1", "Alice", "Person", ""),
                node("acme", "Acme", "Organization", ""),
            ],
            edges: vec![edge("1", "acme", "works_at")],
        };
        let out = expand(vec![(Uuid::new_v4(), first), (Uuid::new_v4(), second)]).await;
        assert_eq!(out.edges.len(), 1);
        assert_eq!(
            out.edges[0].source_entity_id,
            Entity::id_for("Alice"),
            "in the second chunk, node id 1 is Alice, not the first chunk's Carol"
        );
    }

    #[tokio::test]
    async fn an_edge_from_a_timestamp_declared_in_an_earlier_chunk_is_reversed() {
        let first = KnowledgeGraph {
            nodes: vec![node("1898", "1898", "Timestamp", "")],
            edges: vec![],
        };
        let second = KnowledgeGraph {
            nodes: vec![node("radium", "Radium", "Element", "")],
            edges: vec![edge("1898", "radium", "discovery_of")],
        };
        let out = expand(vec![(Uuid::new_v4(), first), (Uuid::new_v4(), second)]).await;
        assert_eq!(out.edges.len(), 1);
        assert_eq!(out.edges[0].source_entity_id, Entity::id_for("radium"));
        assert_eq!(out.edges[0].target_entity_id, Timestamp::id_for("1898"));
    }

    #[tokio::test]
    async fn same_name_nodes_in_one_graph_get_ordinal_ids() {
        let chunk_id = Uuid::new_v4();
        let graph = KnowledgeGraph {
            nodes: vec![
                node("mercury_planet", "Mercury", "Planet", "Closest to the sun"),
                node("mercury_element", "Mercury", "Element", "A liquid metal"),
                node("venus", "Venus", "Planet", ""),
            ],
            edges: vec![
                edge("mercury_planet", "venus", "neighbours"),
                edge("mercury_element", "venus", "named_alongside"),
            ],
        };
        let out = expand(vec![(chunk_id, graph)]).await;

        // Ordered by (type, description, id): element before planet.
        let chunk = chunk_id.to_string();
        let element = cognee_utils::data_point_id_for("Entity", &["Mercury", &chunk, "1"]);
        let planet = cognee_utils::data_point_id_for("Entity", &["Mercury", &chunk, "2"]);
        let ids: HashSet<Uuid> = out.nodes.iter().map(|n| n.entity.base.id).collect();
        assert_eq!(
            ids,
            HashSet::from([element, planet, Entity::id_for("Venus")])
        );
        // Python: uuid5(NAMESPACE_OID, f"Entity:mercury|{chunk_id}|1").
        assert_eq!(
            element,
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("Entity:mercury|{chunk}|1").as_bytes()
            )
        );
        // Each edge still resolves to its own Mercury.
        let source_of = |rel: &str| {
            out.edges
                .iter()
                .find(|e| e.relationship_name == rel)
                .map(|e| e.source_entity_id)
        };
        assert_eq!(source_of("neighbours"), Some(planet));
        assert_eq!(source_of("named_alongside"), Some(element));
    }

    #[tokio::test]
    async fn a_duplicate_node_id_keeps_the_first_declaration() {
        let graph = KnowledgeGraph {
            nodes: vec![
                node("x", "Alice", "Person", ""),
                node("x", "Bob", "Person", ""),
            ],
            edges: vec![],
        };
        let out = expand(vec![(Uuid::new_v4(), graph)]).await;
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].entity.name, "alice");
    }

    #[tokio::test]
    async fn a_blank_type_leaves_the_entity_untyped() {
        let graph = KnowledgeGraph {
            nodes: vec![node("thing", "Thing", " ", ""), node("x", "X", "'", "")],
            edges: vec![],
        };
        let out = expand(vec![(Uuid::new_v4(), graph)]).await;
        assert_eq!(out.nodes.len(), 2);
        for pair in &out.nodes {
            assert!(pair.entity_type.is_none(), "{:?}", pair.entity.name);
            assert_eq!(pair.entity.is_a, None);
        }
    }

    #[tokio::test]
    async fn the_legacy_view_leaves_timestamps_and_their_edges_out() {
        let graph = KnowledgeGraph {
            nodes: vec![
                node("alice", "Alice", "Person", ""),
                node("acme", "Acme", "Organization", ""),
                node("2020", "2020", "Timestamp", ""),
            ],
            edges: vec![
                edge("alice", "2020", "joined_at"),
                edge("alice", "acme", "works_at"),
            ],
        };
        let (nodes, edges, _, _) = expand_with_nodes_and_edges(
            vec![(Uuid::new_v4(), graph)],
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;
        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relationship_name, "works_at");
    }

    #[tokio::test]
    async fn test_expand_single_graph() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Should have 2 nodes (TechCorp, Alice)
        assert_eq!(nodes.len(), 2);

        // Should have 1 edge (Alice works_at TechCorp)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relationship_name, "works_at");

        // Verify node names
        // Names are stored normalized (Python `generate_node_name`), so the
        // expected values are lowercase.
        let names: Vec<String> = nodes.iter().map(|n| n.entity.name.clone()).collect();
        assert!(names.contains(&"techcorp".to_string()));
        assert!(names.contains(&"alice".to_string()));
    }

    #[tokio::test]
    async fn test_expand_deduplicates_nodes() {
        let graph1 = create_test_graph();
        let graph2 = create_test_graph();

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph1), (chunk_id, graph2)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Should have 2 unique nodes (deduplication by node_id)
        assert_eq!(nodes.len(), 2);

        // Should have 1 unique edge (deduplication by source+target+relationship)
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn expand_records_every_producing_chunk_for_a_merged_entity() {
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let dataset_id = Uuid::new_v4();

        let (nodes, _edges, _claimed_edges, producers) = expand_with_nodes_and_edges(
            vec![
                (chunk_a, create_test_graph()),
                (chunk_b, create_test_graph()),
            ],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // The merge still yields one entity per name …
        assert_eq!(nodes.len(), 2);

        // … but both chunks are recorded as its producers, in first-seen order.
        let alice_id = Entity::id_for("Alice");
        assert_eq!(producers.entity_chunks(alice_id), [chunk_a, chunk_b]);
    }

    #[tokio::test]
    async fn expand_records_every_producing_chunk_for_a_merged_edge() {
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let dataset_id = Uuid::new_v4();

        let (_nodes, edges, _claimed_edges, producers) = expand_with_nodes_and_edges(
            vec![
                (chunk_a, create_test_graph()),
                (chunk_b, create_test_graph()),
            ],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(
            producers.edge_chunks(&edges[0].dedup_key()),
            [chunk_a, chunk_b]
        );
    }

    /// An edge an earlier run already wrote is kept out of the graph write —
    /// that is what the filter is for — but it is *not* thrown away: it comes
    /// back as a claim, with its producing chunk recorded, so the caller can
    /// give this run's data item an ownership row for it.
    #[tokio::test]
    async fn expand_returns_a_dedup_skipped_edge_as_a_claim_instead_of_dropping_it() {
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let alice_id = Entity::id_for("Alice");
        let techcorp_id = Entity::id_for("TechCorp");
        let existing = HashSet::from([(alice_id, techcorp_id, "works_at".to_string())]);

        let (_nodes, edges, claimed_edges, producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, create_test_graph())],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &existing,
            &noop(),
            None,
            None,
        )
        .await;

        assert!(
            edges.is_empty(),
            "the edge is already in the graph, so this run writes it no second time"
        );
        assert_eq!(claimed_edges.len(), 1, "…but it is still claimed");
        assert_eq!(claimed_edges[0].source_entity_id, alice_id);
        assert_eq!(claimed_edges[0].target_entity_id, techcorp_id);
        assert_eq!(claimed_edges[0].relationship_name, "works_at");
        assert_eq!(
            producers.edge_chunks(&claimed_edges[0].dedup_key()),
            [chunk_id],
            "the claim needs a producing chunk, or it resolves to no data item"
        );
    }

    #[tokio::test]
    async fn expand_records_no_producers_for_ontology_derived_entities() {
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges, _claimed_edges, producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, create_test_graph())],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // The ontology ancestor class is appended with no producing chunk, so
        // the ownership row falls back to the metadata stamp.
        let ancestor_id = Entity::id_for("legalentity");
        assert!(
            nodes.iter().any(|n| n.entity.base.id == ancestor_id),
            "expected the ontology-derived ancestor node to be present"
        );
        assert!(producers.entity_chunks(ancestor_id).is_empty());

        // An LLM entity canonicalised by the ontology is still recorded, under
        // its rewritten id — this is why recording reads `node_id_to_entity_id`
        // rather than the id the vacant branch constructed.
        let alice_id = Entity::id_for("alice_canonical");
        assert_eq!(producers.entity_chunks(alice_id), [chunk_id]);
    }

    #[tokio::test]
    async fn test_expand_creates_entity_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Check that entity types are created
        for node_pair in &nodes {
            assert!(!node_pair.entity_type.as_ref().unwrap().name.is_empty());
            assert_eq!(
                node_pair.entity_type.as_ref().unwrap().base.data_type,
                "EntityType"
            );
        }

        // Verify types
        let types: Vec<String> = nodes
            .iter()
            .map(|n| n.entity_type.as_ref().unwrap().name.clone())
            .collect();
        assert!(types.contains(&"organization".to_string()));
        assert!(types.contains(&"person".to_string()));
    }

    #[tokio::test]
    async fn test_expand_links_entities_to_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Check that entities reference their types
        for node_pair in &nodes {
            assert_eq!(
                node_pair.entity.is_a,
                Some(node_pair.entity_type.as_ref().unwrap().base.id)
            );
        }
    }

    // ----- Graph building per unit (SDK-1694) -----

    async fn expand_units(
        units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
        scope: EndpointScope,
    ) -> Expansion {
        expand_units_with(units, scope, Arc::new(noop()), &HashMap::new()).await
    }

    /// Expansion on an injected 2-thread pool, as the pipeline runs it.
    async fn expand_units_with(
        units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
        scope: EndpointScope,
        resolver: Arc<dyn OntologyResolver>,
        chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
    ) -> Expansion {
        let pool: Arc<dyn CpuPool> = Arc::new(cognee_core::RayonThreadPool::new(2).unwrap());
        expand_units_on(units, scope, resolver, chunk_node_sets, Some(pool)).await
    }

    async fn expand_units_on(
        units: Vec<Vec<(Uuid, KnowledgeGraph)>>,
        scope: EndpointScope,
        resolver: Arc<dyn OntologyResolver>,
        chunk_node_sets: &HashMap<Uuid, Vec<serde_json::Value>>,
        pool: Option<Arc<dyn CpuPool>>,
    ) -> Expansion {
        expand_extracted_graphs_by_unit(
            units,
            Uuid::new_v4(),
            chunk_node_sets,
            &HashMap::new(),
            resolver,
            None,
            None,
            scope,
            pool,
        )
        .await
        .unwrap()
    }

    fn graph(nodes: Vec<Node>, edges: Vec<Edge>) -> KnowledgeGraph {
        KnowledgeGraph { nodes, edges }
    }

    /// Every (entity name, type name) pair.
    fn entity_type_pairs(expansion: &Expansion) -> BTreeSet<(String, String)> {
        expansion
            .nodes
            .iter()
            .flat_map(|pair| {
                pair.entity_types()
                    .map(|t| (pair.entity.name.clone(), t.name.clone()))
            })
            .collect()
    }

    /// The type names of the entity named `name`, primary first.
    fn types_of(expansion: &Expansion, name: &str) -> Vec<String> {
        let pair = expansion
            .nodes
            .iter()
            .find(|pair| pair.entity.name == name)
            .unwrap();
        // The entity's own type ids — what becomes the `is_a` edges — agree
        // with the EntityTypes the node carries.
        assert_eq!(
            pair.entity.type_ids().collect::<Vec<_>>(),
            pair.entity_types().map(|t| t.base.id).collect::<Vec<_>>()
        );
        pair.entity_types().map(|t| t.name.clone()).collect()
    }

    fn edge_keys(expansion: &Expansion) -> BTreeSet<(Uuid, Uuid, String)> {
        expansion
            .edges
            .iter()
            .map(GraphEdgePair::dedup_key)
            .collect()
    }

    /// Ticket test 1: the first type per unit wins inside it, the types of
    /// every unit are combined, and the description is the first unit's.
    #[tokio::test]
    async fn an_entity_keeps_the_first_type_of_each_unit() {
        let (a1, a2, b1) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let doc_a = vec![
            (
                a1,
                graph(
                    vec![node("apple", "Apple", "Company", "from A chunk 1")],
                    vec![],
                ),
            ),
            (
                a2,
                graph(
                    vec![node("apple", "Apple", "Organization", "from A chunk 2")],
                    vec![],
                ),
            ),
        ];
        let doc_b = vec![(
            b1,
            graph(vec![node("apple", "Apple", "Fruit", "from B")], vec![]),
        )];

        let out = expand_units(vec![doc_a, doc_b], EndpointScope::Unit).await;

        assert_eq!(out.nodes.len(), 1);
        assert_eq!(types_of(&out, "apple"), ["company", "fruit"]);
        assert!(
            !out.nodes
                .iter()
                .flat_map(GraphNodePair::entity_types)
                .any(|t| t.name == "organization"),
            "a type that lost inside its unit must not be written"
        );
        assert_eq!(out.nodes[0].entity.description, "from A chunk 1");
        // Every chunk still produced the entity.
        let apple = out.nodes[0].entity.base.id;
        assert_eq!(out.producers.entity_chunks(apple), [a1, a2, b1]);
    }

    /// Ticket test 2: a `chunks_per_batch` boundary inside one document
    /// starts a new unit, so both blocks' types are kept.
    #[tokio::test]
    async fn a_block_boundary_inside_a_document_keeps_both_types() {
        let doc = Uuid::new_v4();
        let chunks: Vec<cognee_models::DocumentChunk> = (0..2)
            .map(|index| {
                cognee_models::DocumentChunk::new(
                    Uuid::new_v4(),
                    format!("chunk {index}"),
                    1,
                    index,
                    "sentence_end".to_string(),
                    doc,
                )
            })
            .collect();
        let graphs = [
            (
                chunks[0].base.id,
                graph(vec![node("apple", "Apple", "Company", "")], vec![]),
            ),
            (
                chunks[1].base.id,
                graph(vec![node("apple", "Apple", "Fruit", "")], vec![]),
            ),
        ];
        let unit = |per_batch: usize| {
            crate::graph_integration::extraction_units(
                &chunks,
                per_batch,
                graphs
                    .iter()
                    .map(|(chunk, g)| (doc, *chunk, (*chunk, g.clone()))),
            )
        };

        let split = expand_units(unit(1), EndpointScope::Unit).await;
        assert_eq!(types_of(&split, "apple"), ["company", "fruit"]);

        let whole = expand_units(unit(2000), EndpointScope::Unit).await;
        assert_eq!(types_of(&whole, "apple"), ["company"]);
    }

    fn three_documents() -> Vec<Vec<(Uuid, KnowledgeGraph)>> {
        vec![
            vec![(
                Uuid::new_v4(),
                graph(
                    vec![
                        node("apple", "Apple", "Company", ""),
                        node("jobs", "Steve Jobs", "Person", ""),
                    ],
                    vec![edge("jobs", "apple", "founded")],
                ),
            )],
            vec![(
                Uuid::new_v4(),
                graph(
                    vec![
                        node("apple", "Apple", "Fruit", ""),
                        node("tree", "Apple tree", "Plant", ""),
                    ],
                    vec![edge("tree", "apple", "bears")],
                ),
            )],
            vec![(
                Uuid::new_v4(),
                graph(
                    vec![
                        node("apple", "Apple", "Brand", ""),
                        node("jobs", "Steve Jobs", "Founder", ""),
                    ],
                    vec![edge("jobs", "apple", "founded")],
                ),
            )],
        ]
    }

    /// Ticket test 3: the order the units are merged in changes which type
    /// is primary, never the set of types, edges or producers.
    #[tokio::test]
    async fn the_merged_graph_does_not_depend_on_unit_order() {
        let units = three_documents();
        let forward = expand_units(units.clone(), EndpointScope::Unit).await;
        let mut reversed = units;
        reversed.reverse();
        let backward = expand_units(reversed, EndpointScope::Unit).await;

        assert_eq!(entity_type_pairs(&forward), entity_type_pairs(&backward));
        assert_eq!(edge_keys(&forward), edge_keys(&backward));
        for pair in &forward.nodes {
            let id = pair.entity.base.id;
            let mut ahead = forward.producers.entity_chunks(id).to_vec();
            let mut behind = backward.producers.entity_chunks(id).to_vec();
            ahead.sort_unstable();
            behind.sort_unstable();
            assert_eq!(ahead, behind);
        }
        for key in edge_keys(&forward) {
            let mut ahead = forward.producers.edge_chunks(&key).to_vec();
            let mut behind = backward.producers.edge_chunks(&key).to_vec();
            ahead.sort_unstable();
            behind.sort_unstable();
            assert_eq!(ahead, behind);
        }
        assert_eq!(forward.stats, backward.stats);
    }

    /// Ticket test 4: documents cognified together get the same
    /// (entity, type) pairs as the same documents cognified one by one.
    #[tokio::test]
    async fn the_types_do_not_depend_on_how_documents_are_split_across_runs() {
        let units = three_documents();
        let together = expand_units(units.clone(), EndpointScope::Unit).await;

        let mut one_by_one = BTreeSet::new();
        for unit in units {
            let alone = expand_units(vec![unit], EndpointScope::Unit).await;
            one_by_one.extend(entity_type_pairs(&alone));
        }

        assert_eq!(entity_type_pairs(&together), one_by_one);
        assert_eq!(
            types_of(&together, "apple"),
            ["company", "fruit", "brand"],
            "every document's type, in first-seen order"
        );
    }

    /// Ticket test 5: an edge endpoint never resolves against a node only
    /// another document declared.
    #[tokio::test]
    async fn an_endpoint_never_resolves_in_another_document() {
        let doc_a = vec![(
            Uuid::new_v4(),
            graph(vec![node("alice", "Alice", "Person", "")], vec![]),
        )];
        let doc_b = vec![(
            Uuid::new_v4(),
            graph(
                vec![node("bob", "Bob", "Person", "")],
                // `alice` is declared only by document A, by id and by name.
                vec![edge("bob", "alice", "knows"), edge("Alice", "bob", "knows")],
            ),
        )];

        let out = expand_units(vec![doc_a, doc_b], EndpointScope::Unit).await;

        assert!(out.edges.is_empty(), "{:?}", out.edges);
        assert_eq!(out.stats.attempted, 2);
        assert_eq!(out.stats.dropped_target_missing, 1);
        assert_eq!(out.stats.dropped_source_missing, 1);
    }

    /// D2: within one unit, `EndpointScope::Unit` lets an edge reach a node an
    /// earlier chunk declared; `EndpointScope::Chunk` (Python's rule) does
    /// not.
    #[tokio::test]
    async fn the_endpoint_scope_decides_whether_an_edge_reaches_an_earlier_chunk() {
        let unit = vec![
            (
                Uuid::new_v4(),
                graph(vec![node("alice", "Alice", "Person", "")], vec![]),
            ),
            (
                Uuid::new_v4(),
                graph(
                    vec![node("bob", "Bob", "Person", "")],
                    // `alice` is declared only by the first chunk.
                    vec![edge("bob", "alice", "knows")],
                ),
            ),
        ];

        let unit_scope = expand_units(vec![unit.clone()], EndpointScope::Unit).await;
        assert_eq!(unit_scope.edges.len(), 1);
        assert_eq!(unit_scope.stats.resolved_cross_chunk(), 1);

        let chunk_scope = expand_units(vec![unit], EndpointScope::Chunk).await;
        assert!(chunk_scope.edges.is_empty());
        assert_eq!(chunk_scope.stats.dropped_target_missing, 1);
        assert_eq!(chunk_scope.stats.resolved_cross_chunk(), 0);
        // The entities themselves are unaffected by the scope.
        assert_eq!(chunk_scope.nodes.len(), 2);
    }

    // ----- Review fixes on the per-unit merge (SDK-1694) -----

    /// An ontology whose class `Company` brings the individual `Alice` along.
    struct CompanyKnowsAlice {
        lookups: std::sync::atomic::AtomicUsize,
    }

    impl OntologyResolver for CompanyKnowsAlice {
        fn find_closest_match(&self, _: &str, _: &str) -> OntologyResult<Option<String>> {
            Ok(None)
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _directed: bool,
        ) -> OntologyResult<OntologySubgraph> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if (node_name, node_type) != ("Company", "classes") {
                return Ok((vec![], vec![], None));
            }
            let alice = AttachedOntologyNode {
                uri: "http://test.org#Alice".to_string(),
                name: "Alice".to_string(),
                category: NodeCategory::Individuals,
            };
            let root = AttachedOntologyNode {
                uri: "http://test.org#Company".to_string(),
                name: "company".to_string(),
                category: NodeCategory::Classes,
            };
            Ok((vec![alice], vec![], Some(root)))
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    /// Finding 1: the `OntologyIndividual` placeholder another unit gives an
    /// entity never joins a real type, whichever unit comes first.
    #[tokio::test]
    async fn the_ontology_individual_placeholder_never_joins_a_real_type() {
        let with_placeholder = vec![(
            Uuid::new_v4(),
            graph(vec![node("acme", "Acme", "Company", "")], vec![]),
        )];
        let typed = vec![(
            Uuid::new_v4(),
            graph(vec![node("alice", "Alice", "Person", "")], vec![]),
        )];

        for units in [
            vec![with_placeholder.clone(), typed.clone()],
            vec![typed.clone(), with_placeholder.clone()],
        ] {
            let resolver = Arc::new(CompanyKnowsAlice {
                lookups: Default::default(),
            });
            let out =
                expand_units_with(units, EndpointScope::Unit, resolver, &HashMap::new()).await;
            assert_eq!(types_of(&out, "alice"), ["person"]);
        }

        // Alone, the placeholder is still what an untyped individual gets.
        let alone = expand_units_with(
            vec![with_placeholder],
            EndpointScope::Unit,
            Arc::new(CompanyKnowsAlice {
                lookups: Default::default(),
            }),
            &HashMap::new(),
        )
        .await;
        let alice = alone
            .nodes
            .iter()
            .find(|pair| pair.entity.name == "alice")
            .unwrap();
        assert_eq!(
            alice.entity_type.as_ref().map(|t| t.name.as_str()),
            Some(ONTOLOGY_INDIVIDUAL_TYPE)
        );
        assert_eq!(alice.entity.is_a, None, "the placeholder is no is_a edge");
    }

    /// Finding 8: an ontology lookup is made once per run, not once per unit.
    #[tokio::test]
    async fn ontology_lookups_are_cached_across_units() {
        let unit = || {
            vec![(
                Uuid::new_v4(),
                graph(vec![node("acme", "Acme", "Company", "")], vec![]),
            )]
        };
        let resolver = Arc::new(CompanyKnowsAlice {
            lookups: Default::default(),
        });
        expand_units_with(
            vec![unit(), unit(), unit(), unit()],
            EndpointScope::Unit,
            resolver.clone(),
            &HashMap::new(),
        )
        .await;
        // One `classes` lookup for Company, one `individuals` lookup for Acme.
        assert_eq!(
            resolver.lookups.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    /// An ontology whose class `person`, also reached as `Human`, is an
    /// `agent`: either spelling brings the edge `person -is_a-> agent` along.
    struct PersonOrHumanIsAnAgent;

    impl OntologyResolver for PersonOrHumanIsAnAgent {
        fn find_closest_match(&self, _: &str, _: &str) -> OntologyResult<Option<String>> {
            Ok(None)
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _directed: bool,
        ) -> OntologyResult<OntologySubgraph> {
            if node_type != "classes" || !["Person", "Human"].contains(&node_name) {
                return Ok((vec![], vec![], None));
            }
            let class = |name: &str| AttachedOntologyNode {
                uri: format!("http://test.org#{name}"),
                name: name.to_string(),
                category: NodeCategory::Classes,
            };
            Ok((
                vec![class("agent")],
                vec![(
                    "person".to_string(),
                    "is_a".to_string(),
                    "agent".to_string(),
                )],
                Some(class("person")),
            ))
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    /// Two documents that spell one ontology class differently (`Person`,
    /// `Human`) both brought its ontology edge in, so both own it (SDK-1694
    /// O5a): the second spelling's lookup adds its chunk to the first's and
    /// does not replace it. A third document that spells it both ways, across
    /// two chunks, is one more owner, named by its first lookup.
    #[tokio::test]
    async fn every_document_owns_an_ontology_edge_whichever_spelling_brought_it_in() {
        let (a, b, c1, c2) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let one = |name: &str, node_type: &str| {
            graph(
                vec![node(&name.to_lowercase(), name, node_type, "")],
                vec![],
            )
        };
        let units = vec![
            vec![(a, one("Ada", "Person"))],
            vec![(b, one("Grace", "Human"))],
            vec![(c1, one("Alan", "Human")), (c2, one("Edsger", "Person"))],
        ];

        let out = expand_units_with(
            units,
            EndpointScope::Unit,
            Arc::new(PersonOrHumanIsAnAgent),
            &HashMap::new(),
        )
        .await;

        let person_is_a_agent = (
            EntityType::id_for("person"),
            EntityType::id_for("agent"),
            "is_a".to_string(),
        );
        assert_eq!(
            out.edges
                .iter()
                .filter(|edge| edge.dedup_key() == person_is_a_agent)
                .count(),
            1,
            "one edge, whatever the spelling"
        );
        assert_eq!(
            out.producers.ontology_edge_chunks(&person_is_a_agent),
            [a, b, c1]
        );
        assert!(
            out.producers.edge_chunks(&person_is_a_agent).is_empty(),
            "no chunk extracted it"
        );
    }

    /// Finding 2: an entity keeps the NodeSet membership of every unit.
    #[tokio::test]
    async fn an_entity_keeps_the_node_sets_of_every_unit() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let node_sets = HashMap::from([
            (a, vec![serde_json::json!("X")]),
            (b, vec![serde_json::json!("Y")]),
        ]);
        let units = vec![
            vec![(
                a,
                graph(vec![node("apple", "Apple", "Company", "")], vec![]),
            )],
            vec![(b, graph(vec![node("apple", "Apple", "Fruit", "")], vec![]))],
        ];

        let out = expand_units_with(units, EndpointScope::Unit, Arc::new(noop()), &node_sets).await;

        let sets = out.nodes[0].entity.base.belongs_to_set.as_ref().unwrap();
        assert!(sets.contains(&serde_json::json!("X")), "{sets:?}");
        assert!(sets.contains(&serde_json::json!("Y")), "{sets:?}");
        assert_eq!(
            sets.iter()
                .filter(|set| **set == serde_json::json!("X"))
                .count(),
            1
        );
    }

    /// Finding 3: one EntityType id is one copy, the first unit's, wherever it
    /// appears.
    #[tokio::test]
    async fn a_shared_entity_type_keeps_the_first_units_copy() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let unit = |chunk: Uuid, weight: f64, key: &str, name: &str| {
            let weights = HashMap::from([(chunk, weight)]);
            let graphs = vec![(chunk, graph(vec![node(key, name, "Company", "")], vec![]))];
            async move {
                expand_extracted_graphs(
                    graphs,
                    Uuid::new_v4(),
                    &HashMap::new(),
                    &weights,
                    &HashSet::new(),
                    &noop(),
                    None,
                    None,
                )
                .await
            }
        };
        let merged = merge_expansions([
            unit(a, 0.2, "acme", "Acme").await,
            unit(b, 0.9, "globex", "Globex").await,
        ]);

        let weights: Vec<Option<f64>> = merged
            .nodes
            .iter()
            .flat_map(GraphNodePair::entity_types)
            .map(|t| t.base.importance_weight)
            .collect();
        assert_eq!(weights, [Some(0.2), Some(0.2)]);
    }

    /// Finding 7: across units the first unit's fields win — a deliberate
    /// divergence from Python's last-writer-wins.
    #[tokio::test]
    async fn the_first_unit_decides_an_entitys_fields() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let units = vec![
            vec![(
                a,
                graph(vec![node("apple", "Apple", "Company", "first")], vec![]),
            )],
            vec![(
                b,
                graph(vec![node("apple", "Apple", "Fruit", "second")], vec![]),
            )],
        ];
        let out = expand_units(units, EndpointScope::Unit).await;
        let apple = &out.nodes[0].entity;
        assert_eq!(apple.description, "first");
        assert_eq!(
            apple.base.get_metadata("chunk_id"),
            Some(&serde_json::json!(a.to_string()))
        );
    }

    /// Finding 5: under `EndpointScope::Chunk` an endpoint matches by node id
    /// only, as in Python; the name fallback stays on for `Unit`.
    #[tokio::test]
    async fn chunk_scope_matches_endpoints_by_node_id_only() {
        let unit = vec![(
            Uuid::new_v4(),
            graph(
                vec![
                    node("einstein", "Albert Einstein", "Person", ""),
                    node("ulm", "Ulm", "City", ""),
                ],
                vec![edge("Albert Einstein", "ulm", "born_in")],
            ),
        )];
        let by_name = expand_units(vec![unit.clone()], EndpointScope::Unit).await;
        assert_eq!(by_name.edges.len(), 1);
        assert_eq!(by_name.stats.resolved_by_name, 1);

        let by_id = expand_units(vec![unit], EndpointScope::Chunk).await;
        assert!(by_id.edges.is_empty());
        assert_eq!(by_id.stats.dropped_source_missing, 1);
    }

    /// Finding 6: a unit's output is in declaration order, not hash order.
    #[tokio::test]
    async fn a_units_output_follows_declaration_order() {
        let names: Vec<String> = (0..40).map(|i| format!("Entity {i}")).collect();
        let nodes: Vec<Node> = names
            .iter()
            .map(|name| node(name, name, "Thing", ""))
            .collect();
        let edges: Vec<Edge> = names
            .windows(2)
            .map(|pair| edge(&pair[0], &pair[1], "next"))
            .collect();
        let out = expand(vec![(Uuid::new_v4(), graph(nodes, edges))]).await;

        let got: Vec<&str> = out.nodes.iter().map(|n| n.entity.name.as_str()).collect();
        let want: Vec<String> = names.iter().map(|n| generate_node_name(n)).collect();
        assert_eq!(got, want.iter().map(String::as_str).collect::<Vec<_>>());
        let sources: Vec<Uuid> = out.edges.iter().map(|e| e.source_entity_id).collect();
        let want_sources: Vec<Uuid> = names[..39].iter().map(|n| Entity::id_for(n)).collect();
        assert_eq!(sources, want_sources);
    }

    /// Finding 8: expanding the units concurrently gives what expanding them
    /// one after another and merging in order gives.
    #[tokio::test]
    async fn concurrent_expansion_equals_sequential() {
        let mut units = Vec::new();
        for i in 0..24 {
            units.extend(three_documents().into_iter().map(|unit| {
                unit.into_iter()
                    .map(|(chunk, mut g)| {
                        // Vary the descriptions so first-unit-wins is visible.
                        for n in &mut g.nodes {
                            n.description = format!("unit {i}");
                        }
                        (chunk, g)
                    })
                    .collect::<Vec<_>>()
            }));
        }

        let concurrent = expand_units(units.clone(), EndpointScope::Unit).await;
        let without_pool = expand_units_on(
            units.clone(),
            EndpointScope::Unit,
            Arc::new(noop()),
            &HashMap::new(),
            None,
        )
        .await;
        let mut sequential = Vec::new();
        for unit in units {
            sequential.push(expand(unit).await);
        }
        let sequential = merge_expansions(sequential);

        type NodeView = (Uuid, String, Vec<Uuid>, Vec<Uuid>);
        let view = |out: &Expansion| -> (Vec<NodeView>, Vec<(Uuid, Uuid, String)>) {
            let nodes = out
                .nodes
                .iter()
                .map(|n| {
                    let id = n.entity.base.id;
                    (
                        id,
                        n.entity.description.clone(),
                        n.entity_types().map(|t| t.base.id).collect(),
                        out.producers.entity_chunks(id).to_vec(),
                    )
                })
                .collect();
            let edges = out.edges.iter().map(GraphEdgePair::dedup_key).collect();
            (nodes, edges)
        };
        assert_eq!(view(&concurrent), view(&sequential));
        assert_eq!(view(&without_pool), view(&sequential));
        assert_eq!(concurrent.stats, sequential.stats);
    }

    // ----- Second review round (SDK-1694) -----

    /// A `CpuPool` that runs each task on a fresh thread named
    /// `injected-pool`, and counts them.
    struct NamedThreadPool {
        spawned: std::sync::atomic::AtomicUsize,
    }

    impl CpuPool for NamedThreadPool {
        fn spawn_raw(
            &self,
            task: Box<dyn FnOnce() + Send + 'static>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CoreError>> + Send>>
        {
            self.spawned
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::Builder::new()
                .name("injected-pool".to_string())
                .spawn(move || {
                    task();
                    let _ = tx.send(());
                })
                .unwrap();
            Box::pin(async move {
                rx.await.map_err(|_| CoreError::TaskAborted {
                    reason: "worker dropped".to_string(),
                })
            })
        }
    }

    /// Records the thread every ontology lookup runs on.
    #[derive(Default)]
    struct ThreadRecordingResolver {
        threads: Mutex<Vec<Option<String>>>,
    }

    impl OntologyResolver for ThreadRecordingResolver {
        fn find_closest_match(&self, _: &str, _: &str) -> OntologyResult<Option<String>> {
            Ok(None)
        }

        fn get_subgraph(&self, _: &str, _: &str, _: bool) -> OntologyResult<OntologySubgraph> {
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().name().map(str::to_string));
            Ok((vec![], vec![], None))
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    /// Finding 1: the units are expanded on the injected pool, one task each,
    /// not on a pool of the expansion's own.
    #[tokio::test]
    async fn units_are_expanded_on_the_injected_pool() {
        let units: Vec<Vec<(Uuid, KnowledgeGraph)>> = (0..3)
            .map(|i| {
                vec![(
                    Uuid::new_v4(),
                    graph(
                        vec![node(
                            &format!("n{i}"),
                            &format!("Name {i}"),
                            &format!("Type{i}"),
                            "",
                        )],
                        vec![],
                    ),
                )]
            })
            .collect();
        let pool = Arc::new(NamedThreadPool {
            spawned: Default::default(),
        });
        let resolver = Arc::new(ThreadRecordingResolver::default());

        expand_units_on(
            units,
            EndpointScope::Unit,
            resolver.clone(),
            &HashMap::new(),
            Some(pool.clone()),
        )
        .await;

        assert_eq!(pool.spawned.load(std::sync::atomic::Ordering::SeqCst), 3);
        let threads = resolver.threads.lock().unwrap();
        assert!(!threads.is_empty());
        assert!(
            threads
                .iter()
                .all(|name| name.as_deref() == Some("injected-pool")),
            "{threads:?}"
        );
    }

    /// Finding 2: a type id keeps the copy of the first unit, in input order,
    /// that declared it — not of the first merged node that has it.
    #[tokio::test]
    async fn a_shared_entity_type_keeps_the_first_copy_in_input_order() {
        let unit = |weight: f64, key: &str, name: &str, kind: &str| {
            let chunk = Uuid::new_v4();
            let weights = HashMap::from([(chunk, weight)]);
            let graphs = vec![(chunk, graph(vec![node(key, name, kind, "")], vec![]))];
            async move {
                expand_extracted_graphs(
                    graphs,
                    Uuid::new_v4(),
                    &HashMap::new(),
                    &weights,
                    &HashSet::new(),
                    &noop(),
                    None,
                    None,
                )
                .await
            }
        };
        let merged = merge_expansions([
            unit(0.5, "apple", "Apple", "Company").await,
            unit(0.9, "banana", "Banana", "Fruit").await,
            unit(0.2, "apple", "Apple", "Fruit").await,
        ]);

        for pair in &merged.nodes {
            for entity_type in pair.entity_types().filter(|t| t.name == "fruit") {
                assert_eq!(
                    entity_type.base.importance_weight,
                    Some(0.9),
                    "{}",
                    pair.entity.name
                );
            }
        }
        assert_eq!(types_of(&merged, "apple"), ["company", "fruit"]);
    }

    /// An ontology that errors on its first lookup of each name, then answers.
    #[derive(Default)]
    struct FlakyResolver {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl OntologyResolver for FlakyResolver {
        fn find_closest_match(&self, _: &str, _: &str) -> OntologyResult<Option<String>> {
            Ok(None)
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _: bool,
        ) -> OntologyResult<OntologySubgraph> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                return Err(cognee_ontology::OntologyError::MatchingError(
                    "not yet".to_string(),
                ));
            }
            CompanyKnowsAlice {
                lookups: Default::default(),
            }
            .get_subgraph(node_name, node_type, true)
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    /// Finding 4: a failed lookup is not cached, a successful one is.
    #[test]
    fn the_ontology_cache_retries_errors_and_shares_answers() {
        let resolver = FlakyResolver::default();
        let lookups = OntologyLookups::new(&resolver);

        assert!(lookups.get_subgraph("Company", "classes").is_err());
        let first = lookups.get_subgraph("Company", "classes").unwrap();
        let again = lookups.get_subgraph("Company", "classes").unwrap();

        assert!(
            Arc::ptr_eq(&first, &again),
            "a hit shares the cached answer"
        );
        assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// Finding 5: an entity known in one unit only through the ontology
    /// placeholder takes its fields from the unit that extracted it, in
    /// either order.
    #[tokio::test]
    async fn a_real_extraction_supplies_the_fields_over_a_placeholder() {
        let with_placeholder = vec![(
            Uuid::new_v4(),
            graph(vec![node("acme", "Acme", "Company", "")], vec![]),
        )];
        let typed = vec![(
            Uuid::new_v4(),
            graph(
                vec![node("alice", "Alice", "Person", "Alice, a person")],
                vec![],
            ),
        )];

        for units in [
            vec![with_placeholder.clone(), typed.clone()],
            vec![typed.clone(), with_placeholder.clone()],
        ] {
            let out = expand_units_with(
                units,
                EndpointScope::Unit,
                Arc::new(CompanyKnowsAlice {
                    lookups: Default::default(),
                }),
                &HashMap::new(),
            )
            .await;
            let alice = out
                .nodes
                .iter()
                .find(|pair| pair.entity.name == "alice")
                .unwrap();
            assert_eq!(alice.entity.description, "Alice, a person");
            assert_eq!(types_of(&out, "alice"), ["person"]);
        }
    }

    /// Within one unit, two names an ontology resolves to one individual are
    /// one entity, and the first declaration's type wins, as in Python: the
    /// merger must not see two copies and combine their types.
    #[tokio::test]
    async fn an_ontology_canonicalised_entity_keeps_its_first_type_within_a_unit() {
        let unit = vec![
            (
                Uuid::new_v4(),
                graph(vec![node("alice", "Alice", "Person", "first")], vec![]),
            ),
            (
                Uuid::new_v4(),
                graph(
                    vec![node("ac", "alice_canonical", "Robot", "second")],
                    vec![],
                ),
            ),
        ];

        let out = expand_units_with(
            vec![unit],
            EndpointScope::Unit,
            Arc::new(MockOntologyResolver),
            &HashMap::new(),
        )
        .await;

        let canonical = Entity::id_for("alice_canonical");
        let copies: Vec<&GraphNodePair> = out
            .nodes
            .iter()
            .filter(|pair| pair.entity.base.id == canonical)
            .collect();
        assert_eq!(copies.len(), 1);
        let types: Vec<&str> = copies[0].entity_types().map(|t| t.name.as_str()).collect();
        assert_eq!(types, ["person"]);
        assert_eq!(copies[0].entity.description, "first");
    }

    /// `merge_expansions` keeps the first unit's edge and claim, sums the
    /// counters and keeps the largest distance.
    #[tokio::test]
    async fn merge_expansions_combines_edges_claims_and_counters() {
        let first = expand(vec![(
            Uuid::new_v4(),
            graph(
                vec![node("a", "A", "T", ""), node("b", "B", "T", "")],
                vec![Edge {
                    description: Some("first".to_string()),
                    ..edge("a", "b", "rel")
                }],
            ),
        )])
        .await;
        let second = expand(vec![(
            Uuid::new_v4(),
            graph(
                vec![node("a", "A", "T", ""), node("b", "B", "T", "")],
                vec![Edge {
                    description: Some("second".to_string()),
                    ..edge("a", "b", "rel")
                }],
            ),
        )])
        .await;
        let attempted = first.stats.attempted + second.stats.attempted;

        let merged = merge_expansions([first, second]);

        assert_eq!(merged.nodes.len(), 2);
        assert_eq!(merged.edges.len(), 1);
        assert_eq!(merged.edges[0].properties["edge_text"], "first");
        assert_eq!(merged.stats.attempted, attempted);
        assert_eq!(
            merged
                .producers
                .edge_chunks(&merged.edges[0].dedup_key())
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn test_expand_stores_chunk_reference() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Verify chunk_id is stored in metadata
        for node_pair in &nodes {
            let chunk_ref = node_pair.entity.base.get_metadata("chunk_id");
            assert!(chunk_ref.is_some());
        }
    }

    #[tokio::test]
    async fn test_expand_entity_inherits_source_chunk_node_set() {
        // Parity with Python `Entity(belongs_to_set=data_chunk.belongs_to_set)`
        // (expand_with_nodes_and_edges.py:227): an extracted entity must carry
        // the NodeSet entries of its source chunk so a node_name-scoped
        // (NodeSet-filtered) HYBRID_COMPLETION search keeps it.
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        // Build a chunk->NodeSet map mirroring how DocumentChunk carries
        // NodeSet objects in `belongs_to_set` (models/document.rs).
        let node_set = serde_json::json!({
            "id": Uuid::new_v4().to_string(),
            "name": "my_node_set",
            "type": "NodeSet"
        });
        let mut chunk_node_sets: HashMap<Uuid, Vec<serde_json::Value>> = HashMap::new();
        chunk_node_sets.insert(chunk_id, vec![node_set.clone()]);

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &chunk_node_sets,
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert!(!nodes.is_empty(), "expected at least one node");
        for pair in &nodes {
            let belongs_to_set = pair
                .entity
                .base
                .belongs_to_set
                .as_ref()
                .expect("entity should have a belongs_to_set");

            // The source chunk's NodeSet entry is present (search discoverability).
            assert!(
                belongs_to_set.contains(&node_set),
                "entity '{}' should inherit its chunk's NodeSet entry",
                pair.entity.name
            );

            // The dataset-id entry from Entity::new is preserved (union, not replace).
            assert!(
                belongs_to_set
                    .iter()
                    .any(|v| v == &serde_json::json!(dataset_id.to_string())),
                "entity '{}' should still carry the dataset-id entry",
                pair.entity.name
            );
        }
    }

    #[tokio::test]
    async fn test_expand_entity_and_type_inherit_source_chunk_importance_weight() {
        // Parity with Python `importance_weight=data_chunk.importance_weight`
        // (expand_with_nodes_and_edges.py:163,229): every extracted EntityType
        // and Entity must carry the source chunk's importance_weight, not the
        // DataPoint default of 0.5.
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let mut chunk_importance_weights: HashMap<Uuid, f64> = HashMap::new();
        chunk_importance_weights.insert(chunk_id, 0.9);

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &chunk_importance_weights,
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert!(!nodes.is_empty(), "expected at least one node");
        for pair in &nodes {
            assert_eq!(
                pair.entity.base.importance_weight,
                Some(0.9),
                "entity '{}' should inherit its chunk's importance_weight",
                pair.entity.name
            );
            assert_eq!(
                pair.entity_type.as_ref().unwrap().base.importance_weight,
                Some(0.9),
                "entity_type '{}' should inherit its chunk's importance_weight",
                pair.entity_type.as_ref().unwrap().name
            );
        }
    }

    #[tokio::test]
    async fn test_expand_importance_weight_defaults_to_half_when_absent() {
        // Chunks absent from the importance map default to 0.5 (Python's
        // `DataPoint.importance_weight` default).
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert!(!nodes.is_empty(), "expected at least one node");
        for pair in &nodes {
            assert_eq!(pair.entity.base.importance_weight, Some(0.5));
            assert_eq!(
                pair.entity_type.as_ref().unwrap().base.importance_weight,
                Some(0.5)
            );
        }
    }

    #[tokio::test]
    async fn test_expand_ontology_nodes_inherit_source_chunk_importance_weight() {
        // The ontology-derived ancestor ("legalentity") must also carry the
        // source chunk's importance_weight (Python expand_with_nodes_and_edges.py:66).
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "techcorp_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A technology company".to_string(),
            }],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let mut chunk_importance_weights: HashMap<Uuid, f64> = HashMap::new();
        chunk_importance_weights.insert(chunk_id, 0.9);

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &chunk_importance_weights,
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        let legalentity = nodes
            .iter()
            .find(|n| {
                n.entity.name == "legalentity"
                    || n.entity_type.as_ref().unwrap().name == "legalentity"
            })
            .expect("expected ontology-derived 'legalentity' node");

        // The ontology-derived class type carries the chunk's importance_weight.
        assert_eq!(
            legalentity
                .entity_type
                .as_ref()
                .unwrap()
                .base
                .importance_weight,
            Some(0.9),
            "ontology-derived type should inherit the chunk's importance_weight"
        );
    }

    #[tokio::test]
    async fn test_expand_missing_target_node_is_skipped() {
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A person".to_string(),
            }],
            edges: vec![Edge {
                source_node_id: "alice_1".to_string(),
                target_node_id: "missing_node".to_string(), // LLM inconsistency
                relationship_name: "knows".to_string(),
                description: None,
            }],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // Node is kept; the unresolvable edge is skipped
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 0);
    }

    /// An edge that references a node by its human-readable *name* rather than
    /// the id the model declared must still resolve.
    ///
    /// The extraction prompt asks for an id and a separate "most complete
    /// human-readable name", so `{id: "einstein", name: "Albert Einstein"}`
    /// followed by an edge referencing `"Albert Einstein"` is a shape models
    /// produce routinely. Before the name alias existed, such edges were dropped.
    #[tokio::test]
    async fn test_expand_resolves_edge_endpoint_by_node_name() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "einstein".to_string(),
                    name: "Albert Einstein".to_string(),
                    node_type: "Person".to_string(),
                    description: "A physicist".to_string(),
                },
                Node {
                    id: "princeton".to_string(),
                    name: "Princeton University".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A university".to_string(),
                },
            ],
            edges: vec![Edge {
                // Both endpoints given as names, not the declared ids.
                source_node_id: "Albert Einstein".to_string(),
                target_node_id: "Princeton University".to_string(),
                relationship_name: "worked_at".to_string(),
                description: None,
            }],
        };

        let (_nodes, edges, _claimed_edges, _producers, stats) =
            expand_with_nodes_and_edges_with_stats(
                vec![(Uuid::new_v4(), graph)],
                Uuid::new_v4(),
                &HashMap::new(),
                &HashMap::new(),
                &HashSet::new(),
                &noop(),
                None,
                None,
            )
            .await;

        assert_eq!(edges.len(), 1, "name-referenced edge should resolve");
        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.dropped(), 0);
        assert_eq!(
            stats.resolved_by_name, 2,
            "both endpoints resolved via the name fallback"
        );
        assert_eq!(stats.resolved_by_id, 0);
    }

    /// A declared node id always wins over a name alias, even when another
    /// node's name normalises to the same key.
    #[tokio::test]
    async fn test_expand_node_id_wins_over_name_alias() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "mercury".to_string(),
                    name: "Mercury the planet".to_string(),
                    node_type: "Planet".to_string(),
                    description: "A planet".to_string(),
                },
                Node {
                    id: "hg".to_string(),
                    // Normalises to "mercury", colliding with the id above.
                    name: "Mercury".to_string(),
                    node_type: "Element".to_string(),
                    description: "An element".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "mercury".to_string(),
                target_node_id: "hg".to_string(),
                relationship_name: "shares_name_with".to_string(),
                description: None,
            }],
        };

        let (nodes, edges, _claimed_edges, _producers, stats) =
            expand_with_nodes_and_edges_with_stats(
                vec![(Uuid::new_v4(), graph)],
                Uuid::new_v4(),
                &HashMap::new(),
                &HashMap::new(),
                &HashSet::new(),
                &noop(),
                None,
                None,
            )
            .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(stats.resolved_by_id, 2, "both endpoints matched a real id");
        assert_eq!(stats.resolved_by_name, 0);

        // The source must be the planet (id "mercury"), not the element whose
        // name also normalises to "mercury".
        let planet_id = nodes
            .iter()
            .find(|p| p.entity.name == "mercury the planet")
            .map(|p| p.entity.base.id)
            .expect("planet node is present");
        assert_eq!(edges[0].source_entity_id, planet_id);
    }

    /// A name claimed by two distinct entities must stay unresolvable — guessing
    /// which one an edge meant would silently corrupt the graph.
    #[tokio::test]
    async fn test_expand_ambiguous_name_alias_is_not_resolved() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "paris_fr".to_string(),
                    name: "Paris".to_string(),
                    node_type: "City".to_string(),
                    description: "Capital of France".to_string(),
                },
                Node {
                    id: "paris_tx".to_string(),
                    name: "Paris".to_string(),
                    node_type: "City".to_string(),
                    description: "A city in Texas".to_string(),
                },
                Node {
                    id: "france".to_string(),
                    name: "France".to_string(),
                    node_type: "Country".to_string(),
                    description: "A country".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "Paris".to_string(), // ambiguous
                target_node_id: "france".to_string(),
                relationship_name: "capital_of".to_string(),
                description: None,
            }],
        };

        let (_nodes, edges, _claimed_edges, _producers, stats) =
            expand_with_nodes_and_edges_with_stats(
                vec![(Uuid::new_v4(), graph)],
                Uuid::new_v4(),
                &HashMap::new(),
                &HashMap::new(),
                &HashSet::new(),
                &noop(),
                None,
                None,
            )
            .await;

        assert_eq!(edges.len(), 0, "an ambiguous endpoint must not be guessed");
        assert_eq!(stats.dropped_ambiguous_name, 1);
        assert_eq!(stats.dropped_source_missing, 0);
        assert_eq!(stats.resolved_by_name, 0);
    }

    /// The tally must separate a missing source from a missing target, and count
    /// every attempted edge — that is the denominator of the drop rate.
    #[tokio::test]
    async fn test_edge_resolution_stats_counts_drops_by_reason() {
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A person".to_string(),
            }],
            edges: vec![
                Edge {
                    source_node_id: "ghost".to_string(),
                    target_node_id: "alice_1".to_string(),
                    relationship_name: "knows".to_string(),
                    description: None,
                },
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "ghost".to_string(),
                    relationship_name: "knows".to_string(),
                    description: None,
                },
            ],
        };

        let (_nodes, edges, _claimed_edges, _producers, stats) =
            expand_with_nodes_and_edges_with_stats(
                vec![(Uuid::new_v4(), graph)],
                Uuid::new_v4(),
                &HashMap::new(),
                &HashMap::new(),
                &HashSet::new(),
                &noop(),
                None,
                None,
            )
            .await;

        assert_eq!(edges.len(), 0);
        assert_eq!(stats.attempted, 2);
        assert_eq!(stats.dropped_source_missing, 1);
        assert_eq!(stats.dropped_target_missing, 1);
        assert_eq!(stats.dropped(), 2);
        // The second edge's source resolved before its target failed.
        assert_eq!(stats.resolved_by_id, 1);
    }

    /// A surface variant must drop, not bind to the only node it could mean.
    ///
    /// `apple` is a *unique* match against the one declared node, so a looser
    /// endpoint normaliser guarded only by uniqueness (SDK-505 step 3) would
    /// accept it and attach the edge to `Apple Inc.` — silently, because
    /// `resolve_endpoint` attaches rather than merges and the tally would book it
    /// as a legitimate resolution, not a drop. Pinned before any such normaliser
    /// exists (SDK-633). It pins the suffix-stripping / token-subset shape only;
    /// punctuation folding is a separate policy question this does not decide.
    ///
    /// Both endpoints are covered: they resolve in separate match arms, and a
    /// source miss `continue`s before the target is ever looked up.
    #[tokio::test]
    async fn test_surface_variant_of_sole_node_is_dropped_not_attached() {
        let (edges, stats) = expand_ordered_chunks(vec![KnowledgeGraph {
            nodes: vec![node_named("apple_inc", "Apple Inc.")],
            edges: vec![edge_between("apple", "apple_inc")],
        }])
        .await;

        assert!(
            edges.is_empty(),
            "source `apple` must not resolve to `Apple Inc.`"
        );
        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.dropped_source_missing, 1);
        assert_eq!(stats.dropped(), 1);
        // Asserted by outcome, not by path: a normaliser hooked into the id
        // lookup would show up here, not in `resolved_by_name`.
        assert_eq!(stats.resolved_by_id + stats.resolved_by_name, 0);

        let (edges, stats) = expand_ordered_chunks(vec![KnowledgeGraph {
            nodes: vec![node_named("apple_inc", "Apple Inc.")],
            edges: vec![edge_between("apple_inc", "apple")],
        }])
        .await;

        assert!(
            edges.is_empty(),
            "target `apple` must not resolve to `Apple Inc.`"
        );
        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.dropped_target_missing, 1);
        assert_eq!(stats.dropped(), 1);
        // Exactly one resolution: the source's own declared id.
        assert_eq!(stats.resolved_by_id, 1);
        assert_eq!(stats.resolved_by_name, 0);
    }

    /// A node carrying a distinct human-readable name, for the surface-variant
    /// drop test above and the cross-chunk attribution tests below.
    fn node_named(id: &str, name: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            node_type: "Person".to_string(),
            description: "irrelevant to endpoint resolution".to_string(),
        }
    }

    fn edge_between(source: &str, target: &str) -> Edge {
        Edge {
            source_node_id: source.to_string(),
            target_node_id: target.to_string(),
            relationship_name: "knows".to_string(),
            description: None,
        }
    }

    /// Run one expansion pass over chunks in the given order. Position in this
    /// vec is what the cross-chunk counters attribute against.
    async fn expand_ordered_chunks(
        graphs: Vec<KnowledgeGraph>,
    ) -> (Vec<GraphEdgePair>, EdgeResolutionStats) {
        let graphs = graphs
            .into_iter()
            .map(|graph| (Uuid::new_v4(), graph))
            .collect();
        let (_nodes, edges, _claimed, _producers, stats) = expand_with_nodes_and_edges_with_stats(
            graphs,
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;
        (edges, stats)
    }

    /// A node re-declared by a later chunk is available to that chunk on its
    /// own, so an edge referencing it loses nothing to chunk-local resolution —
    /// no matter how far back the *first* declaration was.
    ///
    /// This is the case that makes the counter worth trusting. LLM extraction
    /// re-declares a recurring entity in every chunk that mentions it, so
    /// attributing to the earliest declaration would drive
    /// `max_cross_chunk_distance` toward the unit length and over-count what
    /// chunk-local resolution would drop.
    #[tokio::test]
    async fn test_edge_resolution_stats_ignores_redeclared_node() {
        let (edges, stats) = expand_ordered_chunks(vec![
            KnowledgeGraph {
                nodes: vec![node_named("einstein", "Albert Einstein")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("planck", "Max Planck")],
                edges: vec![],
            },
            KnowledgeGraph {
                // Re-declares the chunk-0 node alongside a fresh one, then
                // references both. Everything it needs is in its own chunk.
                nodes: vec![
                    node_named("einstein", "Albert Einstein"),
                    node_named("bohr", "Niels Bohr"),
                ],
                edges: vec![edge_between("einstein", "bohr")],
            },
        ])
        .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(stats.resolved_by_id, 2);
        assert_eq!(
            stats.resolved_cross_chunk(),
            0,
            "the referencing chunk declared both endpoints itself"
        );
        assert_eq!(
            stats.max_cross_chunk_distance, 0,
            "distance must follow the most recent declaration, not the first"
        );
    }

    /// The same, through the name-alias fallback: a re-declared node's alias is
    /// equally available to the re-declaring chunk.
    #[tokio::test]
    async fn test_edge_resolution_stats_ignores_redeclared_name_alias() {
        let (edges, stats) = expand_ordered_chunks(vec![
            KnowledgeGraph {
                nodes: vec![node_named("einstein", "Albert Einstein")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("planck", "Max Planck")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![
                    node_named("einstein", "Albert Einstein"),
                    node_named("bohr", "Niels Bohr"),
                ],
                // Source by name, resolved through the alias the chunk itself
                // just re-registered.
                edges: vec![edge_between("Albert Einstein", "bohr")],
            },
        ])
        .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(
            stats.resolved_cross_chunk(),
            0,
            "the alias was re-registered by the referencing chunk"
        );
        assert_eq!(stats.max_cross_chunk_distance, 0);
    }

    /// A node id re-declared under another name follows the name.
    ///
    /// Entity ids derive from the name (Python `Entity.id_for(name)`), so when
    /// chunk 2 re-declares node id `nova_labs` as "Nova" — the name chunk 0 gave
    /// `nova_corp` — that node *is* the Nova entity, and chunk 2's node id points
    /// at it from then on. The name "Nova" keeps resolving to that one entity
    /// (never `Ambiguous`), and because chunk 2 declared it itself, the
    /// resolution is chunk-local.
    #[tokio::test]
    async fn test_a_redeclared_node_id_follows_the_name_it_is_given() {
        let graphs = vec![
            KnowledgeGraph {
                nodes: vec![node_named("nova_corp", "Nova")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("nova_labs", "Nova Labs")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![
                    node_named("nova_labs", "Nova"),
                    node_named("bohr", "Niels Bohr"),
                ],
                edges: vec![
                    edge_between("Nova", "bohr"),
                    edge_between("nova_labs", "bohr"),
                ],
            },
        ]
        .into_iter()
        .map(|graph| (Uuid::new_v4(), graph))
        .collect();

        let (nodes, edges, _claimed, _producers, stats) = expand_with_nodes_and_edges_with_stats(
            graphs,
            Uuid::new_v4(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert_eq!(stats.dropped(), 0, "{stats:?}");
        let nova_id = nodes
            .iter()
            .find(|p| p.entity.name == "nova")
            .map(|p| p.entity.base.id)
            .expect("the Nova entity is present");
        assert_eq!(nova_id, Entity::id_for("Nova"));
        // Both edges — by name and by the re-pointed node id — reach Nova.
        assert_eq!(
            edges.len(),
            1,
            "same (source, target, relationship) twice is one edge"
        );
        assert_eq!(edges[0].source_entity_id, nova_id);
        assert_eq!(stats.resolved_cross_chunk(), 0);
    }

    /// An edge referencing only its own chunk's nodes is chunk-local, so
    /// `EndpointScope::Chunk` would not touch it. Guards the counter against firing
    /// on every resolution — the pair with the three tests below is what makes
    /// the discriminator load-bearing.
    #[tokio::test]
    async fn test_edge_resolution_stats_ignores_chunk_local_resolution() {
        let (edges, stats) = expand_ordered_chunks(vec![KnowledgeGraph {
            nodes: vec![
                node_named("einstein", "Albert Einstein"),
                node_named("bohr", "Niels Bohr"),
            ],
            edges: vec![edge_between("einstein", "bohr")],
        }])
        .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(stats.resolved_by_id, 2);
        assert_eq!(
            stats.resolved_cross_chunk(),
            0,
            "both endpoints came from the referencing chunk"
        );
        assert_eq!(stats.max_cross_chunk_distance, 0);
    }

    /// An id-keyed endpoint that matches a node declared by an *earlier* chunk
    /// is exactly what `EndpointScope::Chunk` would cut off.
    #[tokio::test]
    async fn test_edge_resolution_stats_counts_cross_chunk_id_resolution() {
        let (edges, stats) = expand_ordered_chunks(vec![
            KnowledgeGraph {
                nodes: vec![node_named("einstein", "Albert Einstein")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("bohr", "Niels Bohr")],
                // Source was declared one chunk back; target is local.
                edges: vec![edge_between("einstein", "bohr")],
            },
        ])
        .await;

        assert_eq!(edges.len(), 1, "the edge still resolves and is emitted");
        assert_eq!(stats.dropped(), 0);
        assert_eq!(stats.resolved_by_id, 2);
        assert_eq!(
            stats.resolved_by_id_cross_chunk, 1,
            "only the source crossed a chunk boundary"
        );
        assert_eq!(stats.resolved_by_name_cross_chunk, 0);
        assert_eq!(stats.max_cross_chunk_distance, 1);
    }

    /// The name-alias fallback is the population the cross-chunk split exists
    /// for: it is the recovery mechanism chunk-local resolution would undo.
    #[tokio::test]
    async fn test_edge_resolution_stats_counts_cross_chunk_name_resolution() {
        let (edges, stats) = expand_ordered_chunks(vec![
            KnowledgeGraph {
                nodes: vec![node_named("einstein", "Albert Einstein")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("bohr", "Niels Bohr")],
                // Source given as the earlier chunk's *name*, not its id.
                edges: vec![edge_between("Albert Einstein", "bohr")],
            },
        ])
        .await;

        assert_eq!(edges.len(), 1);
        assert_eq!(stats.resolved_by_name, 1);
        assert_eq!(
            stats.resolved_by_name_cross_chunk, 1,
            "the name fallback reached across a chunk boundary"
        );
        assert_eq!(
            stats.resolved_by_id_cross_chunk, 0,
            "the local target resolved by id and did not cross"
        );
        assert_eq!(stats.max_cross_chunk_distance, 1);
    }

    /// `max_cross_chunk_distance` reports the widest gap, not the last one —
    /// it is how far back, within a unit, resolution reached.
    #[tokio::test]
    async fn test_edge_resolution_stats_reports_widest_cross_chunk_distance() {
        let (edges, stats) = expand_ordered_chunks(vec![
            KnowledgeGraph {
                nodes: vec![node_named("einstein", "Albert Einstein")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("bohr", "Niels Bohr")],
                edges: vec![],
            },
            KnowledgeGraph {
                nodes: vec![node_named("curie", "Marie Curie")],
                edges: vec![
                    // Two chunks back, then one chunk back.
                    edge_between("einstein", "curie"),
                    edge_between("bohr", "curie"),
                ],
            },
        ])
        .await;

        assert_eq!(edges.len(), 2);
        assert_eq!(stats.resolved_by_id, 4);
        assert_eq!(
            stats.resolved_by_id_cross_chunk, 2,
            "both sources crossed; the shared target is local to its chunk"
        );
        assert_eq!(
            stats.max_cross_chunk_distance, 2,
            "the widest gap wins, not the most recent"
        );
    }

    #[tokio::test]
    async fn test_expand_empty_graphs() {
        let dataset_id = Uuid::new_v4();

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    async fn test_expand_multiple_edges_same_entities() {
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice_1".to_string(),
                    name: "Alice".to_string(),
                    node_type: "Person".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A company".to_string(),
                },
            ],
            edges: vec![
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "works_at".to_string(),
                    description: None,
                },
                Edge {
                    source_node_id: "alice_1".to_string(),
                    target_node_id: "techcorp_1".to_string(),
                    relationship_name: "founded".to_string(),
                    description: None,
                },
            ],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert_eq!(nodes.len(), 2);
        // Should have 2 edges (different relationships)
        assert_eq!(edges.len(), 2);

        let relationships: Vec<String> =
            edges.iter().map(|e| e.relationship_name.clone()).collect();
        assert!(relationships.contains(&"works_at".to_string()));
        assert!(relationships.contains(&"founded".to_string()));
    }

    #[tokio::test]
    async fn test_expand_multiple_chunks_different_ids() {
        // Create two graphs from different chunks — each entity should get
        // the chunk_id of the chunk it was extracted from.
        let chunk_id_a = Uuid::new_v4();
        let chunk_id_b = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let graph_a = KnowledgeGraph {
            nodes: vec![Node {
                id: "alice_1".to_string(),
                name: "Alice".to_string(),
                node_type: "Person".to_string(),
                description: "A software engineer".to_string(),
            }],
            edges: vec![],
        };

        let graph_b = KnowledgeGraph {
            nodes: vec![Node {
                id: "bob_1".to_string(),
                name: "Bob".to_string(),
                node_type: "Person".to_string(),
                description: "A data scientist".to_string(),
            }],
            edges: vec![],
        };

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id_a, graph_a), (chunk_id_b, graph_b)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        assert_eq!(nodes.len(), 2);

        // Find each node and verify its chunk_id metadata
        for node_pair in &nodes {
            let chunk_ref = node_pair
                .entity
                .base
                .get_metadata("chunk_id")
                .expect("chunk_id metadata should be present");

            if node_pair.entity.name == "alice" {
                assert_eq!(
                    chunk_ref.as_str().unwrap(),
                    chunk_id_a.to_string(),
                    "Alice should be tagged with chunk_id_a"
                );
            } else if node_pair.entity.name == "bob" {
                assert_eq!(
                    chunk_ref.as_str().unwrap(),
                    chunk_id_b.to_string(),
                    "Bob should be tagged with chunk_id_b"
                );
            } else {
                panic!("Unexpected entity name: {}", node_pair.entity.name);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mock ontology resolver for testing ontology validation
    // -----------------------------------------------------------------------

    /// Mock resolver that returns canonical names and realistic subgraphs.
    ///
    /// **`get_subgraph` behavior:**
    /// - `("Organization", "classes")` → root "organisation" with ancestor "legalentity", is_a edge
    /// - `("Person", "classes")` → root "person", no ancestors
    /// - Everything else → empty
    ///
    /// **`find_closest_match` behavior:**
    /// - `("Alice", "individuals")` → `Some("Alice_Canonical")`
    /// - Everything else → `None` (classes are handled via get_subgraph)
    struct MockOntologyResolver;

    impl OntologyResolver for MockOntologyResolver {
        fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>> {
            match (name, category) {
                ("Alice", "individuals") => Ok(Some("Alice_Canonical".to_string())),
                _ => Ok(None),
            }
        }

        fn get_subgraph(
            &self,
            node_name: &str,
            node_type: &str,
            _directed: bool,
        ) -> OntologyResult<OntologySubgraph> {
            match (node_name, node_type) {
                ("Organization", "classes") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#Organisation".to_string(),
                        name: "organisation".to_string(),
                        category: NodeCategory::Classes,
                    };
                    let ancestor = AttachedOntologyNode {
                        uri: "http://test.org#LegalEntity".to_string(),
                        name: "legalentity".to_string(),
                        category: NodeCategory::Classes,
                    };
                    Ok((
                        // Real resolver returns only the traversed subgraph
                        // (ancestors); the matched root is returned separately.
                        vec![ancestor],
                        vec![(
                            "organisation".to_string(),
                            "is_a".to_string(),
                            "legalentity".to_string(),
                        )],
                        Some(root),
                    ))
                }
                ("Person", "classes") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#Person".to_string(),
                        name: "person".to_string(),
                        category: NodeCategory::Classes,
                    };
                    Ok((vec![], vec![], Some(root)))
                }
                ("Alice", "individuals") => {
                    let root = AttachedOntologyNode {
                        uri: "http://test.org#alice_canonical".to_string(),
                        name: "alice_canonical".to_string(),
                        category: NodeCategory::Individuals,
                    };
                    Ok((vec![], vec![], Some(root)))
                }
                _ => Ok((vec![], vec![], None)),
            }
        }

        fn is_loaded(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_expand_with_ontology_validates_entity_types() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // 2 LLM nodes + 1 ontology ancestor (legalentity) = 3
        assert!(
            nodes.len() >= 2,
            "Expected at least 2 nodes, got {}",
            nodes.len()
        );

        // Find LLM-extracted nodes (not ontology-derived)
        // Note: Alice's name is canonicalized to "alice_canonical" by individual matching
        let llm_nodes: Vec<_> = nodes
            .iter()
            .filter(|n| {
                n.entity.name == "techcorp"
                    || n.entity.name == "alice"
                    || n.entity.name == "alice_canonical"
            })
            .collect();
        assert_eq!(llm_nodes.len(), 2);

        for node_pair in &llm_nodes {
            // All entity types should be ontology-valid (both "Organization"
            // and "Person" are matched by MockOntologyResolver via get_subgraph)
            assert!(
                node_pair.entity_type.as_ref().unwrap().is_ontology_valid(),
                "EntityType '{}' should be ontology-valid",
                node_pair.entity_type.as_ref().unwrap().name
            );

            if node_pair.entity.name == "techcorp" {
                // "Organization" → canonical "organisation" (lowercase from uri_to_key)
                assert_eq!(node_pair.entity_type.as_ref().unwrap().name, "organisation");
            } else if node_pair.entity.name == "alice_canonical" {
                // "Person" → canonical "person" (lowercase from uri_to_key)
                assert_eq!(node_pair.entity_type.as_ref().unwrap().name, "person");
                // Alice is matched as individual and canonicalized
                assert!(
                    node_pair.entity.base.ontology_valid,
                    "Entity 'alice_canonical' should be ontology-valid"
                );
                // Original name stored in metadata. This records the name the
                // entity carried *before ontology canonicalisation*, which is
                // now the normalized extracted name rather than the model's raw
                // title-case — the normalisation happens first, in
                // `create_entity_node`.
                assert_eq!(
                    node_pair.entity.base.get_metadata("original_name"),
                    Some(&serde_json::json!("alice")),
                );
            }
        }
    }

    #[tokio::test]
    async fn test_expand_noop_resolver_leaves_entities_unvalidated() {
        let graph = create_test_graph();
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            None,
            None,
        )
        .await;

        // With NoOp resolver nothing should be ontology-validated
        for node_pair in &nodes {
            assert!(
                !node_pair.entity_type.as_ref().unwrap().is_ontology_valid(),
                "EntityType '{}' should NOT be ontology-valid with NoOp resolver",
                node_pair.entity_type.as_ref().unwrap().name
            );
            assert!(
                !node_pair.entity.base.ontology_valid,
                "Entity '{}' should NOT be ontology-valid with NoOp resolver",
                node_pair.entity.name
            );
        }
    }

    // -----------------------------------------------------------------------
    // Tests for ontology helper functions
    // -----------------------------------------------------------------------

    #[test]
    fn test_ontology_node_ids_are_class_namespaced() {
        // Ontology class nodes hash as EntityType, individuals as Entity, so a
        // class and an individual sharing a name never collide (parity with
        // Python expand_with_nodes_and_edges.py:46-49).
        assert_ne!(EntityType::id_for("Car"), Entity::id_for("Car"));
        // Normalization still holds within a class.
        assert_eq!(EntityType::id_for("Car"), EntityType::id_for("car"));
        assert_eq!(
            EntityType::id_for("Car"),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"EntityType:car"),
        );
    }

    #[test]
    fn test_generate_edge_name_normalizes_relations() {
        assert_eq!(generate_edge_name("is a"), "is_a");
        assert_eq!(generate_edge_name("Is A"), "is_a");
        assert_eq!(generate_edge_name("don't know"), "dont_know");
    }

    #[test]
    fn test_process_ontology_nodes_creates_entity_types_for_classes() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![
            AttachedOntologyNode {
                uri: "http://example.org#Vehicle".to_string(),
                name: "Vehicle".to_string(),
                category: NodeCategory::Classes,
            },
            AttachedOntologyNode {
                uri: "http://example.org#Car".to_string(),
                name: "Car".to_string(),
                category: NodeCategory::Classes,
            },
        ];

        let node_map = IndexMap::new();
        let type_map = HashMap::new();
        let mut ontology_types_map = IndexMap::new();
        let mut ontology_entities_map = IndexMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            0.5,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
            None,
            None,
            &mut HashSet::new(),
        );

        assert_eq!(ontology_types_map.len(), 2);
        assert!(ontology_entities_map.is_empty());

        // Verify each EntityType has ontology_valid=true and deterministic IDs
        for et in ontology_types_map.values() {
            assert!(et.base.ontology_valid);
        }

        // Check deterministic IDs
        let vehicle_key = format!("{}_type", EntityType::id_for("Vehicle"));
        let car_key = format!("{}_type", EntityType::id_for("Car"));
        assert!(ontology_types_map.contains_key(&vehicle_key));
        assert!(ontology_types_map.contains_key(&car_key));

        let vehicle_et = &ontology_types_map[&vehicle_key];
        assert_eq!(vehicle_et.base.id, EntityType::id_for("Vehicle"));
        assert_eq!(vehicle_et.name, "vehicle");
    }

    #[test]
    fn test_process_ontology_nodes_skips_duplicates() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![AttachedOntologyNode {
            uri: "http://example.org#Organization".to_string(),
            name: "Organization".to_string(),
            category: NodeCategory::Classes,
        }];

        let node_map = IndexMap::new();
        // Pre-populate type_map with an "Organization" entry (as if LLM already extracted it)
        let mut type_map = HashMap::new();
        // `type_map` is keyed on the *normalized* type name, matching how the
        // main expansion loop builds `type_key`.
        type_map.insert(
            "organization_type".to_string(),
            EntityType::new("organization", "A type", Some(dataset_id)),
        );

        let mut ontology_types_map = IndexMap::new();
        let mut ontology_entities_map = IndexMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            0.5,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
            None,
            None,
            &mut HashSet::new(),
        );

        // Should be skipped because it already exists in type_map
        assert!(ontology_types_map.is_empty());
    }

    #[test]
    fn test_process_ontology_nodes_creates_entities_for_individuals() {
        let dataset_id = Uuid::new_v4();
        let nodes = vec![AttachedOntologyNode {
            uri: "http://example.org#MyCar".to_string(),
            name: "MyCar".to_string(),
            category: NodeCategory::Individuals,
        }];

        let node_map = IndexMap::new();
        let type_map = HashMap::new();
        let mut ontology_types_map = IndexMap::new();
        let mut ontology_entities_map = IndexMap::new();

        process_ontology_nodes(
            &nodes,
            dataset_id,
            0.5,
            &node_map,
            &type_map,
            &mut ontology_types_map,
            &mut ontology_entities_map,
            None,
            None,
            &mut HashSet::new(),
        );

        assert_eq!(ontology_entities_map.len(), 1);
        assert!(ontology_types_map.is_empty());

        let dedup_key = format!("{}_entity", Entity::id_for("MyCar"));
        let pair = &ontology_entities_map[&dedup_key];
        assert!(pair.entity.base.ontology_valid);
        assert_eq!(pair.entity.base.id, Entity::id_for("MyCar"));
        assert_eq!(pair.entity.name, "mycar");
        // Placeholder type
        assert_eq!(
            pair.entity_type.as_ref().unwrap().name,
            "OntologyIndividual"
        );
        assert_eq!(
            pair.entity_type.as_ref().unwrap().base.id,
            EntityType::id_for("OntologyIndividual")
        );
    }

    #[test]
    fn test_process_ontology_edges_creates_edges() {
        let edges: Vec<OntologyEdge> = vec![
            ("Car".to_string(), "is a".to_string(), "Vehicle".to_string()),
            (
                "Vehicle".to_string(),
                "has part".to_string(),
                "Engine".to_string(),
            ),
        ];

        let mut ontology_edge_keys = HashSet::new();
        let mut ontology_edges_out = Vec::new();

        // No node list → endpoints default to Entity::id_for (matches Python's
        // `node_category.get(x) != "classes" → Entity`).
        process_ontology_edges(
            &[],
            &edges,
            &mut ontology_edge_keys,
            &mut ontology_edges_out,
        );

        assert_eq!(ontology_edges_out.len(), 2);
        assert_eq!(ontology_edge_keys.len(), 2);

        // Verify first edge: Car -> Vehicle via "is_a"
        let car_id = Entity::id_for("Car");
        let vehicle_id = Entity::id_for("Vehicle");
        let edge0 = &ontology_edges_out[0];
        assert_eq!(edge0.source_entity_id, car_id);
        assert_eq!(edge0.target_entity_id, vehicle_id);
        assert_eq!(edge0.relationship_name, "is_a");
        assert_eq!(
            edge0.properties.get("ontology_valid"),
            Some(&"true".to_string())
        );
        assert_eq!(
            edge0.properties.get("source_node_id"),
            Some(&car_id.to_string())
        );
        assert_eq!(
            edge0.properties.get("target_node_id"),
            Some(&vehicle_id.to_string())
        );

        // Verify second edge: Vehicle -> Engine via "has_part"
        let engine_id = Entity::id_for("Engine");
        let edge1 = &ontology_edges_out[1];
        assert_eq!(edge1.source_entity_id, vehicle_id);
        assert_eq!(edge1.target_entity_id, engine_id);
        assert_eq!(edge1.relationship_name, "has_part");
    }

    /// A unit emits an ontology edge once, however many lookups bring it.
    #[test]
    fn test_process_ontology_edges_skips_repeats() {
        let edges: Vec<OntologyEdge> = vec![
            ("Car".to_string(), "is a".to_string(), "Vehicle".to_string()),
            ("Car".to_string(), "Is A".to_string(), "Vehicle".to_string()),
        ];
        let mut ontology_edge_keys = HashSet::new();
        let mut ontology_edges_out = Vec::new();

        for _ in 0..2 {
            process_ontology_edges(
                &[],
                &edges,
                &mut ontology_edge_keys,
                &mut ontology_edges_out,
            );
        }

        assert_eq!(ontology_edges_out.len(), 1);
        assert_eq!(ontology_edges_out[0].relationship_name, "is_a");
    }

    // -----------------------------------------------------------------------
    // Tests for entity type subgraph expansion (Step 3)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_expand_ontology_adds_ancestor_type_nodes() {
        // "Organization" matches ontology class "organisation" which has ancestor "legalentity"
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "techcorp_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A technology company".to_string(),
            }],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // LLM node (TechCorp) + ontology-derived ancestor (legalentity)
        assert!(
            nodes.len() >= 2,
            "Expected at least 2 nodes (LLM + ontology ancestor), got {}",
            nodes.len()
        );

        // The ancestor "legalentity" should be present as an ontology-derived node
        let legalentity_node = nodes.iter().find(|n| {
            n.entity.name == "legalentity" || n.entity_type.as_ref().unwrap().name == "legalentity"
        });
        assert!(
            legalentity_node.is_some(),
            "Expected ontology-derived 'legalentity' node in output"
        );

        // The ancestor should be ontology-valid
        if let Some(le) = legalentity_node {
            assert!(
                le.entity_type.as_ref().unwrap().base.ontology_valid
                    || le.entity.base.ontology_valid
            );
        }
    }

    #[tokio::test]
    async fn test_expand_ontology_adds_is_a_edges() {
        // "Organization" matches ontology class "organisation" → is_a → "legalentity"
        let graph = KnowledgeGraph {
            nodes: vec![Node {
                id: "techcorp_1".to_string(),
                name: "TechCorp".to_string(),
                node_type: "Organization".to_string(),
                description: "A technology company".to_string(),
            }],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (_nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // There should be an ontology-derived "is_a" edge
        let is_a_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.relationship_name == "is_a")
            .collect();
        assert_eq!(
            is_a_edges.len(),
            1,
            "Expected exactly 1 is_a edge from ontology"
        );

        let is_a = &is_a_edges[0];

        // Source = organisation, target = legalentity — both ontology classes,
        // so they resolve via EntityType::id_for.
        assert_eq!(is_a.source_entity_id, EntityType::id_for("organisation"));
        assert_eq!(is_a.target_entity_id, EntityType::id_for("legalentity"));

        // Should be marked as ontology-derived
        assert_eq!(
            is_a.properties.get("ontology_valid"),
            Some(&"true".to_string())
        );
    }

    #[tokio::test]
    async fn test_expand_edges_connect_to_canonicalized_entities() {
        // Verify that LLM-extracted edges resolve correctly even after
        // entity names/IDs are canonicalized by the ontology resolver.
        // This confirms that name_mapping-based edge remapping is NOT needed
        // in Rust (unlike Python) because node_id_to_entity_id keys by
        // the original LLM node.id, not the entity name.
        let graph = create_test_graph(); // alice_1, techcorp_1, works_at
        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // The "works_at" edge should still connect Alice to TechCorp
        let works_at: Vec<_> = edges
            .iter()
            .filter(|e| e.relationship_name == "works_at")
            .collect();
        assert_eq!(works_at.len(), 1, "Expected exactly 1 works_at edge");

        // Find Alice (canonicalized to "alice_canonical") and TechCorp by entity name
        let alice = nodes
            .iter()
            .find(|n| n.entity.name == "alice_canonical")
            .expect("Alice should be canonicalized to 'alice_canonical'");
        let techcorp = nodes
            .iter()
            .find(|n| n.entity.name == "techcorp")
            .expect("TechCorp entity should exist");

        // Edge endpoints must match the entity UUIDs
        assert_eq!(
            works_at[0].source_entity_id, alice.entity.base.id,
            "Edge source should point to canonicalized Alice's UUID"
        );
        assert_eq!(
            works_at[0].target_entity_id, techcorp.entity.base.id,
            "Edge target should point to TechCorp's UUID"
        );
    }

    #[tokio::test]
    async fn test_expand_ontology_no_duplicate_derived_nodes() {
        // Two entities share the same type "Organization". The ancestor "legalentity"
        // should appear only once (deduplication across entities of the same type).
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A tech company".to_string(),
                },
                Node {
                    id: "acmecorp_1".to_string(),
                    name: "AcmeCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "Another company".to_string(),
                },
            ],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // Both entities should exist
        assert!(nodes.iter().any(|n| n.entity.name == "techcorp"));
        assert!(nodes.iter().any(|n| n.entity.name == "acmecorp"));

        // Both should share the same EntityType (canonicalized to "organisation")
        let tc = nodes.iter().find(|n| n.entity.name == "techcorp").unwrap();
        let ac = nodes.iter().find(|n| n.entity.name == "acmecorp").unwrap();
        assert_eq!(
            tc.entity_type.as_ref().unwrap().base.id,
            ac.entity_type.as_ref().unwrap().base.id
        );

        // There should be exactly 1 legalentity derived node (not duplicated)
        let legalentity_count = nodes
            .iter()
            .filter(|n| {
                n.entity.name == "legalentity"
                    || n.entity_type.as_ref().unwrap().name == "legalentity"
            })
            .count();
        assert_eq!(
            legalentity_count, 1,
            "legalentity ancestor should appear exactly once"
        );

        // Exactly 1 is_a edge
        let is_a_edges: Vec<_> = edges
            .iter()
            .filter(|e| e.relationship_name == "is_a")
            .collect();
        assert_eq!(is_a_edges.len(), 1, "Expected exactly 1 is_a edge");
    }

    #[tokio::test]
    async fn test_expand_ontology_mixed_validated_and_unvalidated() {
        // "Organization" matches the ontology, "Concept" does not
        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "techcorp_1".to_string(),
                    name: "TechCorp".to_string(),
                    node_type: "Organization".to_string(),
                    description: "A tech company".to_string(),
                },
                Node {
                    id: "quantum_1".to_string(),
                    name: "QuantumTheory".to_string(),
                    node_type: "Concept".to_string(),
                    description: "A scientific concept".to_string(),
                },
            ],
            edges: vec![],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = MockOntologyResolver;

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        // Both entities should exist
        let tc = nodes.iter().find(|n| n.entity.name == "techcorp").unwrap();
        let qt = nodes
            .iter()
            .find(|n| n.entity.name == "quantumtheory")
            .unwrap();

        // Organization type is canonicalized and validated
        assert!(tc.entity_type.as_ref().unwrap().is_ontology_valid());
        assert_eq!(tc.entity_type.as_ref().unwrap().name, "organisation");

        // Concept type is NOT in the ontology
        assert!(!qt.entity_type.as_ref().unwrap().is_ontology_valid());
        assert_eq!(qt.entity_type.as_ref().unwrap().name, "concept");
    }

    #[tokio::test]
    async fn pre_stamp_sets_pipeline_and_task_on_entity_types() {
        // Freshly-LLM-constructed Entity / EntityType DataPoints emerge
        // from `expand_with_nodes_and_edges` with `source_pipeline` and
        // `source_task` already set, mirroring Python's
        // `_stamp_provenance_deep` in `extract_graph_from_data.py`.
        //
        // The caller-supplied `task_rank` must land on both halves of every
        // pair: the nodes are persisted to the graph DB inside
        // `extract_graph_from_data`, so a rank applied after this call would
        // never reach the stored rows.
        const CALLER_RANK: i32 = 3;
        let dataset_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();
        let graph = create_test_graph();

        let (nodes, _edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &noop(),
            Some("alice@example.com"),
            Some(CALLER_RANK),
        )
        .await;

        assert!(!nodes.is_empty(), "expected at least one node");
        for pair in &nodes {
            assert_eq!(
                pair.entity_type
                    .as_ref()
                    .unwrap()
                    .base
                    .source_pipeline
                    .as_deref(),
                Some("cognify_pipeline"),
                "EntityType '{}' should be pre-stamped with cognify_pipeline",
                pair.entity_type.as_ref().unwrap().name
            );
            assert_eq!(
                pair.entity_type.as_ref().unwrap().base.topological_rank,
                Some(CALLER_RANK),
                "EntityType '{}' should carry the caller-supplied task_rank",
                pair.entity_type.as_ref().unwrap().name
            );
            assert_eq!(
                pair.entity_type
                    .as_ref()
                    .unwrap()
                    .base
                    .source_task
                    .as_deref(),
                Some("extract_graph_from_data"),
                "EntityType '{}' should be pre-stamped with extract_graph_from_data",
                pair.entity_type.as_ref().unwrap().name
            );
            assert_eq!(
                pair.entity_type
                    .as_ref()
                    .unwrap()
                    .base
                    .source_user
                    .as_deref(),
                Some("alice@example.com"),
                "EntityType '{}' should carry the supplied user_label",
                pair.entity_type.as_ref().unwrap().name
            );

            assert_eq!(
                pair.entity.base.source_pipeline.as_deref(),
                Some("cognify_pipeline"),
                "Entity '{}' should be pre-stamped with cognify_pipeline",
                pair.entity.name
            );
            assert_eq!(
                pair.entity.base.topological_rank,
                Some(CALLER_RANK),
                "Entity '{}' should carry the caller-supplied task_rank",
                pair.entity.name
            );
            assert_eq!(
                pair.entity.base.source_task.as_deref(),
                Some("extract_graph_from_data"),
                "Entity '{}' should be pre-stamped with extract_graph_from_data",
                pair.entity.name
            );
            assert_eq!(
                pair.entity.base.source_user.as_deref(),
                Some("alice@example.com"),
                "Entity '{}' should carry the supplied user_label",
                pair.entity.name
            );
        }
    }

    // ----- Relationship labels and the existing-edge check (SDK-1694 O2) -----

    /// Ticket test 6: labels are normalised as Python's `generate_edge_name`
    /// before the edge key is built, so `Born In`, `born_in` and `BORN IN`
    /// are one `born_in` edge.
    #[tokio::test]
    async fn relationship_labels_are_normalised_before_the_edge_key() {
        let chunk = KnowledgeGraph {
            nodes: vec![
                node("ada", "Ada Lovelace", "Person", ""),
                node("london", "London", "City", ""),
            ],
            edges: vec![
                edge("ada", "london", "Born In"),
                edge("ada", "london", "born_in"),
                edge("ada", "london", "BORN IN"),
                edge("ada", "london", "Ada's Home"),
            ],
        };
        let chunk_id = Uuid::new_v4();
        let out = expand(vec![(chunk_id, chunk)]).await;

        let labels: Vec<&str> = out
            .edges
            .iter()
            .map(|edge| edge.relationship_name.as_str())
            .collect();
        assert_eq!(labels, ["born_in", "adas_home"]);
        assert_eq!(
            out.edges[0].properties.get("relationship_name").unwrap(),
            "born_in"
        );
        // The producer is keyed on the normalised label too, so the claim
        // and the edge evidence find it.
        assert_eq!(
            out.producers.edge_chunks(&out.edges[0].dedup_key()),
            [chunk_id]
        );
    }

    /// An edge drawn from a timestamp is reversed and keeps its label, which
    /// is normalised too: the order of the two steps does not matter.
    #[tokio::test]
    async fn a_reversed_timestamp_edge_gets_the_normalised_label() {
        let chunk = KnowledgeGraph {
            nodes: vec![
                node("marie", "Marie Curie", "Person", ""),
                node("birth", "1867", "Timestamp", ""),
            ],
            edges: vec![edge("birth", "marie", "Birth Year Of")],
        };
        let out = expand(vec![(Uuid::new_v4(), chunk)]).await;

        assert_eq!(
            out.edges
                .iter()
                .map(GraphEdgePair::dedup_key)
                .collect::<Vec<_>>(),
            [(
                Entity::id_for("Marie Curie"),
                Timestamp::id_for("1867"),
                "birth_year_of".to_string()
            )]
        );
    }

    /// Under `EndpointScope::Chunk` the id registry is keyed on the verbatim
    /// node id, as Python's: `A` and `a` are two nodes, so `A -> a` is
    /// Alpha -> Beta, not a self-edge on whichever was declared last.
    #[tokio::test]
    async fn chunk_scope_tells_node_ids_apart_by_case() {
        let unit = vec![(
            Uuid::new_v4(),
            graph(
                vec![
                    node("A", "Alpha", "Letter", ""),
                    node("a", "Beta", "Letter", ""),
                ],
                vec![edge("A", "a", "precedes")],
            ),
        )];

        let out = expand_units(vec![unit], EndpointScope::Chunk).await;

        assert_eq!(
            out.edges
                .iter()
                .map(GraphEdgePair::dedup_key)
                .collect::<Vec<_>>(),
            [(
                Entity::id_for("Alpha"),
                Entity::id_for("Beta"),
                "precedes".to_string()
            )]
        );
    }

    /// Under `EndpointScope::Chunk` an endpoint that spells a node id
    /// differently does not resolve, as in Python, which drops the edge;
    /// under `EndpointScope::Unit` the normalised id still finds it.
    #[tokio::test]
    async fn chunk_scope_matches_endpoints_on_the_verbatim_id() {
        let unit = vec![(
            Uuid::new_v4(),
            graph(
                vec![
                    node("steve_jobs", "Steve Jobs", "Person", ""),
                    node("apple", "Apple", "Company", ""),
                ],
                vec![edge("Steve Jobs", "apple", "founded")],
            ),
        )];

        let chunk_scope = expand_units(vec![unit.clone()], EndpointScope::Chunk).await;
        assert!(chunk_scope.edges.is_empty());
        assert_eq!(chunk_scope.stats.dropped_source_missing, 1);

        let unit_scope = expand_units(vec![unit], EndpointScope::Unit).await;
        assert_eq!(unit_scope.edges.len(), 1);
        assert_eq!(unit_scope.stats.resolved_by_id, 2);
    }

    /// Expansion builds every edge; the split after it moves the ones the
    /// graph holds to the claims, once per key across units.
    #[tokio::test]
    async fn split_existing_edges_claims_what_the_graph_already_holds() {
        let unit = |chunk: Uuid| {
            vec![(
                chunk,
                graph(
                    vec![
                        node("alice", "Alice Smith", "Person", ""),
                        node("acme", "Acme", "Company", ""),
                        node("bob", "Bob", "Person", ""),
                    ],
                    vec![
                        edge("alice", "acme", "Works At"),
                        edge("bob", "acme", "works_at"),
                    ],
                ),
            )]
        };
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut out = expand_units(vec![unit(chunk_a), unit(chunk_b)], EndpointScope::Unit).await;
        assert_eq!(out.edges.len(), 2);
        assert!(out.claimed_existing_edges.is_empty());

        // The graph has Alice's edge, under the name-based id and the
        // normalised label.
        let alice_works_at = (
            Entity::id_for("Alice Smith"),
            Entity::id_for("Acme"),
            "works_at".to_string(),
        );
        out.split_existing_edges(&HashSet::from([alice_works_at.clone()]));

        assert_eq!(
            out.edges
                .iter()
                .map(|edge| edge.source_entity_id)
                .collect::<Vec<_>>(),
            [Entity::id_for("Bob")]
        );
        assert_eq!(
            out.producers.edge_chunks(&alice_works_at),
            [chunk_a, chunk_b]
        );
        assert_eq!(
            out.claimed_existing_edges
                .iter()
                .map(GraphEdgePair::dedup_key)
                .collect::<Vec<_>>(),
            [alice_works_at]
        );
    }

    /// An ontology edge the graph already holds is not written again (O2), and
    /// since SDK-1694 O5a it is claimed by the chunk whose ontology lookup
    /// brought it in, as Python's in-graph provenance attaches the document's
    /// ref to it. Its producer is recorded as ontology output, never as an
    /// extracting chunk (which would link the chunk to it with `contains`).
    #[tokio::test]
    async fn an_existing_ontology_edge_is_claimed_by_the_chunk_that_looked_it_up() {
        let chunk_id = Uuid::new_v4();
        let chunk = KnowledgeGraph {
            nodes: vec![node("techcorp", "TechCorp", "Organization", "")],
            edges: vec![],
        };
        let organisation_is_a_legal_entity = (
            EntityType::id_for("organisation"),
            EntityType::id_for("legalentity"),
            "is_a".to_string(),
        );
        let expand_against = |existing: HashSet<(Uuid, Uuid, String)>| {
            let chunk = chunk.clone();
            async move {
                expand_extracted_graphs(
                    vec![(chunk_id, chunk)],
                    Uuid::new_v4(),
                    &HashMap::new(),
                    &HashMap::new(),
                    &existing,
                    &MockOntologyResolver,
                    None,
                    None,
                )
                .await
            }
        };
        let keys = |edges: &[GraphEdgePair]| -> Vec<(Uuid, Uuid, String)> {
            edges.iter().map(GraphEdgePair::dedup_key).collect()
        };

        let fresh = expand_against(HashSet::new()).await;
        assert!(
            keys(&fresh.edges).contains(&organisation_is_a_legal_entity),
            "a fresh graph gets the ontology edge"
        );
        assert_eq!(
            fresh
                .producers
                .ontology_edge_chunks(&organisation_is_a_legal_entity),
            [chunk_id]
        );
        assert!(
            fresh
                .producers
                .edge_chunks(&organisation_is_a_legal_entity)
                .is_empty()
        );

        let again = expand_against(HashSet::from([organisation_is_a_legal_entity.clone()])).await;
        assert!(!keys(&again.edges).contains(&organisation_is_a_legal_entity));
        assert_eq!(
            keys(&again.claimed_existing_edges),
            [organisation_is_a_legal_entity]
        );
    }

    /// The run's graph merged one document at a time (SDK-1694 O4b) equals
    /// merging every unit at once, and each document's view carries only its
    /// own producers, with the merged copies of its entities.
    #[tokio::test]
    async fn merging_document_by_document_equals_the_whole_run() {
        let units = three_documents();
        let whole = expand_units(units.clone(), EndpointScope::Unit).await;

        let expander = UnitExpander::new(
            Uuid::new_v4(),
            HashMap::new(),
            HashMap::new(),
            Arc::new(noop()),
            None,
            None,
            EndpointScope::Unit,
            None,
        );
        let mut run = RunGraph::default();
        let mut views = Vec::new();
        for unit in units.clone() {
            let chunk = unit[0].0;
            let expansions = expander
                .expand(vec![unit])
                .await
                .unwrap()
                .into_iter()
                .map(|(expansion, _)| expansion)
                .collect();
            views.push((chunk, run.add_document(expansions, |_| {})));
        }
        for (chunk, view) in &views {
            for node in &view.nodes {
                assert_eq!(
                    view.producers.entity_chunks(node.entity.base.id),
                    &[*chunk],
                    "a document's view names only its own chunk"
                );
            }
        }
        // The third document's Apple is the merged one: all three types.
        let apple = views[2]
            .1
            .nodes
            .iter()
            .find(|node| node.entity.name.eq_ignore_ascii_case("apple"))
            .unwrap();
        assert_eq!(apple.entity_types().count(), 3);

        // The same merged entities with the same types, in the same order,
        // and the same edges (the dataset id in a NodeSet differs).
        let shape = |nodes: &[GraphNodePair]| -> Vec<(Uuid, Vec<Uuid>, String)> {
            nodes
                .iter()
                .map(|node| {
                    (
                        node.entity.base.id,
                        node.entity_types().map(|t| t.base.id).collect(),
                        node.entity.description.clone(),
                    )
                })
                .collect()
        };
        let (nodes, edges) = run.into_parts();
        assert_eq!(shape(&nodes), shape(&whole.nodes));
        let keys = |edges: &[GraphEdgePair]| -> Vec<(Uuid, Uuid, String)> {
            edges.iter().map(GraphEdgePair::dedup_key).collect()
        };
        assert_eq!(keys(&edges), keys(&whole.edges));
    }
}
