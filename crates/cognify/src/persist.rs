//! One persistence layer for every write a cognify run makes (SDK-1694 O4a).
//!
//! Every graph, vector, ownership-ledger and edge-evidence write of the
//! cognify pipeline goes through [`persist_group`]: the extracted entities,
//! Timestamps and semantic edges, the claimed existing edges' ownership rows,
//! the WebPage / WebSite artifacts, the chunks, summaries, entity types and
//! documents, the runtime custom graphs, the structural edges, every vector
//! point, the edge evidence, and — in a group of its own, after the pipeline —
//! the DLT schema nodes and foreign-key edges.
//!
//! # A group, and the run-level state
//!
//! A [`PersistGroup`] is one batch of artifacts. The cognify pipeline saves
//! one group per document, from a single writer, in input order
//! ([`crate::document_stage`], SDK-1694 O4b); the whole-run
//! [`crate::tasks::add_data_points`] saves a run as one group. Every group of
//! a run goes through one [`PersistState`], which remembers what earlier
//! groups wrote — ledger row ids, graph node ids, graph edge keys, vector
//! points, edge-evidence ids — so an artifact shared by several groups is
//! written and embedded once, and no ledger row is written twice. Within a
//! group nothing is filtered, so one group writes exactly what the pre-O4a
//! stages wrote.
//!
//! # Rewrite on change (per-document state)
//!
//! With [`PersistState::per_document`], an artifact a **later** group changes
//! is written again rather than skipped:
//!
//! - an **entity** whose merged form changed — a new type, a new NodeSet
//!   membership, or the first real extraction replacing an ontology
//!   placeholder — gets its graph node rewritten, its vector point rewritten
//!   when the payload's inputs (name, NodeSets, importance weight) changed, and
//!   the ownership rows earlier groups wrote for it rewritten, so every row's
//!   `attributes` carry the entity as it is now. The point is rewritten with
//!   the vector it was embedded with only under `retain_embeddings`, which
//!   holds every vector of the run anyway, and only while its name — the
//!   embedded text — is unchanged; otherwise the index pass embeds the name
//!   (again), once per actual change. The vector store cannot hand a vector
//!   back (`VectorDB::retrieve` returns payloads) nor update a payload alone,
//!   so without the retained vector there is nothing to reuse;
//! - a **WebPage** two documents of one URL share is rewritten with the later
//!   document's copy, so the last document in input order wins, as it did
//!   when the run was saved as one group;
//! - an **EdgeType** point whose id a later group reaches with a
//!   lexicographically smaller spelling is rewritten with it, so the
//!   surviving spelling is the run's smallest, as in one group.
//!
//! The rest is first-write-wins, as before: EntityTypes, Timestamps, edges and
//! custom nodes come to every group as the run's first copy already, and
//! chunks, summaries and documents belong to one group each.
//!
//! # Write order within a group
//!
//! 1. **Ledger rows, all of them, first** ("ledger before artifact", I1): the
//!    extraction rows (entities, Timestamps, semantic and claimed edges), the
//!    web-page rows, the custom-graph rows, the provenance rows (chunks,
//!    entities, semantic edges, summaries, entity types, documents, structural
//!    edges), the rewritten rows of changed entities, then the DLT rows.
//! 2. **Graph nodes, all of them**: entities, Timestamps, WebPage / WebSite,
//!    chunks, summaries, entity types, documents, custom nodes, DLT schema
//!    nodes.
//! 3. **Graph edges, only then**: semantic (extracted and ontology) edges, the
//!    web edges the graph does not hold yet, custom edges, structural edges.
//!    Every endpoint a group's edges name is a node of the same group (or was
//!    written by an earlier group), so it exists by now. This matters:
//!    Ladybug's `add_edges` is `UNWIND … MATCH … MERGE` and silently drops an
//!    edge whose endpoint does not exist, and Postgres' `graph_edge` has
//!    foreign keys to `graph_node`, so the write fails. A DLT group indexes
//!    its schema nodes' vectors before its edges, as the DLT step did before
//!    O4a.
//! 4. **Vector points**: chunks, entities, entity types, summaries, edge
//!    types, documents, then custom nodes.
//! 5. **Edge evidence**, only after the graph and vector writes succeeded.
//!
//! # Across groups
//!
//! The order holds within a group. An edge whose endpoint only a **later**
//! group writes would still reach the graph first; a caller that splits a run
//! into groups must put every endpoint of a group's edges in that group or an
//! earlier one. Per-document groups do by construction: an entity, type or
//! Timestamp is in every group whose document produced it, and a node already
//! written by an earlier group is skipped (or rewritten), not missing.
//!
//! # EdgeType counts
//!
//! `number_of_edges` counts every edge the run wrote once, whichever group
//! wrote it: an edge key an earlier group already counted is not counted
//! again. The run's `EdgeType`s are built from those counts when the state is
//! turned into its outputs ([`PersistState::into_outputs`]), so they match a
//! run saved as one group.
//!
//! # The first group costs no copy
//!
//! A group is filtered against what earlier groups wrote. With no earlier
//! group (a run saved as one group) there is nothing to filter, so the
//! borrowed slices go to the stores as they are (SDK-507).
//!
//! Any failure is returned at once and fails the run, as every persistence
//! failure did before.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use chrono::Utc;
use cognee_database::{GraphEdge, GraphNode, RelationalDb};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, GraphDBTraitExt};
use cognee_models::{Document, DocumentChunk, EdgeType, Embedding, EntityType};
use cognee_vector::VectorDB;
use serde_json::json;
use std::borrow::Cow;
use tracing::info;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::dynamic_graph::DynamicDataPoint;
use crate::error::CognifyError;
use crate::graph_extraction::GraphExtractable;
use crate::graph_integration::{ArtifactProducers, GraphEdgePair, GraphNodePair, TimestampNode};
use crate::pipeline::IndexedFieldsStats;
use crate::summarization::TextSummary;
use crate::tasks::{
    AttributedDynamicGraph, DltArtifacts, ExtractedGraphData, LedgerIdentity, WebPage,
};

/// One batch of artifacts [`persist_group`] writes.
///
/// Borrowed, so a run saved as one group costs no copy; a caller that builds
/// one group per document owns that document's artifacts and lends them.
/// `producers` must name the producing chunks of `entities`, `edges`,
/// `timestamps` and `claimed_existing_edges`, of each entity's types and of
/// the ontology output (SDK-1694 O5a); `chunks` are what attributes them to
/// data items, so a group carries the chunks its artifacts came from. A
/// group's ledger rows name only the documents of its own `chunks`: an
/// artifact the group carries but another document produced (an earlier
/// document's type on a shared entity) gets no row from it.
#[derive(Clone, Copy)]
pub(crate) struct PersistGroup<'a> {
    pub(crate) chunks: &'a [DocumentChunk],
    pub(crate) documents: &'a [Document],
    pub(crate) summaries: &'a [TextSummary],
    /// Deduplicated entities, each with every type it was declared with.
    pub(crate) entities: &'a [GraphNodePair],
    pub(crate) timestamps: &'a [TimestampNode],
    /// Semantic (extracted and ontology) edges, `edge_text` already stamped.
    pub(crate) edges: &'a [GraphEdgePair],
    /// Edges the graph already holds: ownership rows only.
    pub(crate) claimed_existing_edges: &'a [GraphEdgePair],
    pub(crate) producers: &'a ArtifactProducers,
    pub(crate) dynamic_graphs: &'a [AttributedDynamicGraph],
    pub(crate) web_pages: &'a [WebPage],
    pub(crate) dlt: Option<&'a DltArtifacts>,
}

static NO_PRODUCERS: std::sync::LazyLock<ArtifactProducers> =
    std::sync::LazyLock::new(ArtifactProducers::default);

impl PersistGroup<'static> {
    /// A group with nothing in it, to fill with struct-update syntax.
    pub(crate) fn empty() -> Self {
        Self {
            chunks: &[],
            documents: &[],
            summaries: &[],
            entities: &[],
            timestamps: &[],
            edges: &[],
            claimed_existing_edges: &[],
            producers: &NO_PRODUCERS,
            dynamic_graphs: &[],
            web_pages: &[],
            dlt: None,
        }
    }
}

/// Where and as whom a group is written.
pub(crate) struct PersistContext<'a> {
    pub(crate) graph_db: &'a Arc<dyn GraphDBTrait>,
    pub(crate) vector_db: &'a Arc<dyn VectorDB>,
    pub(crate) embedding_engine: &'a Arc<dyn EmbeddingEngine>,
    pub(crate) db: &'a dyn RelationalDb,
    /// Who and which run the ledger rows name.
    pub(crate) ledger: LedgerIdentity,
    /// Who and which run the edge evidence names; `None` records none.
    pub(crate) evidence: Option<LedgerIdentity>,
    /// The caller's user, unresolved: the label pre-stamped on `EdgeType`s.
    pub(crate) user_id: Option<Uuid>,
    /// Read for the custom-graph walk (`chunks_per_batch`,
    /// `chunk_attachment`) and `retain_embeddings`.
    pub(crate) config: &'a CognifyConfig,
    /// The run's chunk → data-item map when the run is saved one document per
    /// group: where the ownership fallbacks look an entity's `chunk_id` stamp
    /// up, so a group gets the rows one group would. `None` for one group.
    pub(crate) run_chunk_documents: Option<&'a HashMap<Uuid, Uuid>>,
}

/// The vector-point classes [`Written::vectors`] tracks. A point's identity is
/// its class and id: every class but the custom ones is one collection (or,
/// for chunks and documents, a fixed collection per point), and a custom or
/// DLT node is embedded into all its fields at once.
const CHUNK_POINT: &str = "chunk";
const ENTITY_POINT: &str = "entity";
const ENTITY_TYPE_POINT: &str = "entity_type";
const SUMMARY_POINT: &str = "summary";
const EDGE_TYPE_POINT: &str = "edge_type";
const DOCUMENT_POINT: &str = "document";
const DYNAMIC_POINT: &str = "dynamic";
const DLT_POINT: &str = "dlt";

/// The edge classes counted towards `number_of_edges`, each on its own: one
/// group counts an `is_a` that is both an ontology and a structural edge once
/// per class, as the pre-O4a stages did.
const SEMANTIC_EDGES: usize = 0;
const STRUCTURAL_EDGES: usize = 1;
const DYNAMIC_EDGES: usize = 2;

/// What has been written, by identity.
#[derive(Debug, Default)]
struct Written {
    ledger_nodes: HashSet<Uuid>,
    ledger_edges: HashSet<Uuid>,
    graph_nodes: HashSet<String>,
    /// `(source, target, relationship)` as [`edge_key`]: 16 bytes per edge
    /// instead of three strings.
    graph_edges: HashSet<Uuid>,
    vectors: HashSet<(&'static str, Uuid)>,
    evidence: HashSet<Uuid>,
}

impl Written {
    fn is_empty(&self) -> bool {
        self.ledger_nodes.is_empty()
            && self.ledger_edges.is_empty()
            && self.graph_nodes.is_empty()
            && self.graph_edges.is_empty()
            && self.vectors.is_empty()
            && self.evidence.is_empty()
    }

    fn absorb(&mut self, other: Written) {
        self.ledger_nodes.extend(other.ledger_nodes);
        self.ledger_edges.extend(other.ledger_edges);
        self.graph_nodes.extend(other.graph_nodes);
        self.graph_edges.extend(other.graph_edges);
        self.vectors.extend(other.vectors);
        self.evidence.extend(other.evidence);
    }
}

/// The version of an entity the stores hold, for rewrite on change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntityVersion {
    /// The graph node (and ledger `attributes`): the whole entity.
    graph: u64,
    /// The embedded text: the name. A vector is reused only while it holds.
    name: u64,
    /// The rest of the vector payload's inputs: NodeSets, importance weight.
    payload: u64,
}

impl EntityVersion {
    fn of(entity: &cognee_models::Entity) -> Self {
        let graph = fingerprint(&serde_json::to_vec(entity).unwrap_or_default());
        let name = fingerprint(entity.name.as_bytes());
        let mut hasher = std::hash::DefaultHasher::new();
        serde_json::to_vec(&entity.base.belongs_to_set)
            .unwrap_or_default()
            .hash(&mut hasher);
        entity
            .base
            .importance_weight
            .map(f64::to_bits)
            .hash(&mut hasher);
        Self {
            graph,
            name,
            payload: hasher.finish(),
        }
    }

    /// Whether the vector point (text or payload) differs from `other`'s.
    fn vector_differs(&self, other: &Self) -> bool {
        self.name != other.name || self.payload != other.payload
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// What a per-document state keeps to rewrite what a later group changes.
#[derive(Debug, Default)]
struct Versions {
    entities: HashMap<Uuid, EntityVersion>,
    /// The vector each entity was embedded with, reused when only its payload
    /// is rewritten. Kept only with `retain_embeddings`, which holds every
    /// vector of the run anyway; without it a rewritten point is embedded
    /// again by the index pass, a window at a time (SDK-711).
    entity_vectors: HashMap<Uuid, Vec<f32>>,
    /// The data items whose ownership row of each entity has been written.
    entity_rows: HashMap<Uuid, Vec<Uuid>>,
    /// Raw (WebPage / WebSite) nodes, by id: their JSON's fingerprint.
    raw_nodes: HashMap<String, u64>,
    /// The spelling each `EdgeType` point was embedded with.
    edge_type_names: HashMap<Uuid, String>,
}

/// What the groups of one run have written so far, and the run-level outputs
/// they add up to.
///
/// A group filters out only what an **earlier** group wrote; what it writes
/// itself is folded in once it has been written in full, so one group is
/// never filtered against itself. A group that fails leaves the state as it
/// was before it, apart from the rewrite bookkeeping of what it did write.
#[derive(Debug, Default)]
pub(crate) struct PersistState {
    written: Written,
    /// `Some` for [`Self::per_document`]: rewrite what a later group changes.
    versions: Option<Versions>,
    /// Edge keys counted towards `number_of_edges`, per edge class.
    counted_edges: [HashSet<Uuid>; 3],
    /// Every counted edge's retrieval text, with how often it was counted.
    edge_texts: BTreeMap<String, usize>,
    /// The dataset and user label the run's `EdgeType`s are built with.
    edge_type_owner: Option<(Uuid, Option<String>)>,
    embeddings: Vec<Embedding>,
    embedding_count: usize,
    indexed_fields: IndexedFieldsStats,
}

/// The run-level outputs of a [`PersistState`], for the `CognifyResult`.
pub(crate) struct PersistOutputs {
    /// One `EdgeType` per point id, counting every edge the run wrote once.
    pub(crate) edge_types: Vec<EdgeType>,
    /// The vectors kept when `retain_embeddings` is on.
    pub(crate) embeddings: Vec<Embedding>,
    /// One per chunk, entity and summary, plus one per custom-node field.
    pub(crate) embedding_count: usize,
    pub(crate) indexed_fields: IndexedFieldsStats,
}

impl PersistState {
    /// The state of a run saved as one group per document: what a later
    /// group changes is rewritten (see the module docs).
    pub(crate) fn per_document() -> Self {
        Self {
            versions: Some(Versions::default()),
            ..Self::default()
        }
    }

    pub(crate) fn into_outputs(self) -> PersistOutputs {
        let edge_types = match self.edge_type_owner {
            Some((dataset_id, user_label)) => run_edge_types(
                self.edge_texts
                    .into_iter()
                    .flat_map(|(text, count)| std::iter::repeat_n(text, count)),
                dataset_id,
                user_label.as_deref(),
            ),
            None => Vec::new(),
        };
        PersistOutputs {
            edge_types,
            embeddings: self.embeddings,
            embedding_count: self.embedding_count,
            indexed_fields: self.indexed_fields,
        }
    }
}

/// One `EdgeType` per derived point id over `texts`, pre-stamped as the run's
/// `EdgeType`s are.
fn run_edge_types(
    texts: impl Iterator<Item = String>,
    dataset_id: Uuid,
    user_label: Option<&str>,
) -> Vec<EdgeType> {
    let mut edge_types = crate::tasks::build_edge_types(texts, dataset_id);
    // Pre-stamp the `source_*` provenance keys before they are indexed. The
    // LLM-derived edge-type names trace back to the entity-extraction task, so
    // the pipeline / task literals match. `task_rank: None`: Python's
    // `create_edge_type_datapoints` (`index_graph_edges.py:50`) builds these
    // objects locally and never returns them from `add_data_points`, so no
    // stamper ever assigns them a rank; the vector payload carries none either
    // way (`cognee_models::index_schema_payload`).
    let mut local_visited: HashSet<Uuid> = HashSet::new();
    for edge_type in &mut edge_types {
        crate::graph_integration::expansion::pre_stamp_extraction(
            edge_type,
            user_label,
            None,
            &mut local_visited,
        );
    }
    edge_types
}

/// What earlier groups wrote, and what the current one has written so far.
struct Tracker<'s> {
    earlier: &'s Written,
    now: Written,
}

impl<'s> Tracker<'s> {
    fn new(earlier: &'s Written) -> Self {
        Self {
            earlier,
            now: Written::default(),
        }
    }

    fn new_node(&self, id: &str) -> bool {
        !self.earlier.graph_nodes.contains(id)
    }

    fn new_edge(&self, edge: &EdgeData) -> bool {
        !self.earlier.graph_edges.contains(&edge_key(edge))
    }

    /// Whether an earlier group wrote anything. When none did, nothing can be
    /// filtered out, and the group's slices are written as they are.
    fn first_group(&self) -> bool {
        self.earlier.is_empty()
    }

    fn new_point(&self, class: &'static str, id: Uuid) -> bool {
        !self.earlier.vectors.contains(&(class, id))
    }
}

/// An edge's identity in [`Written::graph_edges`]: a v5 UUID of its exact
/// `(source, target, relationship)` strings, separated so no two keys meet.
fn edge_key(edge: &EdgeData) -> Uuid {
    edge_key_of(&edge.0, &edge.1, &edge.2)
}

fn edge_key_of(source: &str, target: &str, relationship: &str) -> Uuid {
    let key = format!("{source}\u{1f}{target}\u{1f}{relationship}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes())
}

/// `items` without the ones `keep` rejects — borrowed as they are when
/// nothing can be rejected (the first group), so that group copies nothing.
fn fresh<'a, T: Clone>(
    items: &'a [T],
    first_group: bool,
    keep: impl Fn(&T) -> bool,
) -> Cow<'a, [T]> {
    if first_group {
        Cow::Borrowed(items)
    } else {
        Cow::Owned(items.iter().filter(|item| keep(item)).cloned().collect())
    }
}

/// How each of a group's entities is written.
struct EntityPlan {
    /// Positions (into the group's entities) whose graph node is written.
    graph: Vec<usize>,
    /// Positions whose vector point is written for the first time.
    new_vectors: Vec<usize>,
    /// Positions whose vector payload is rewritten with the cached vector.
    rewritten_vectors: Vec<usize>,
    /// Positions an earlier group wrote and this one changed: their earlier
    /// ownership rows are rewritten.
    changed: Vec<usize>,
    /// The version each entity is written at, when the state tracks them.
    versions: Vec<Option<EntityVersion>>,
}

impl EntityPlan {
    fn new(entities: &[GraphNodePair], versions: Option<&Versions>, tracker: &Tracker<'_>) -> Self {
        let mut plan = Self {
            graph: Vec::new(),
            new_vectors: Vec::new(),
            rewritten_vectors: Vec::new(),
            changed: Vec::new(),
            versions: Vec::with_capacity(entities.len()),
        };
        for (position, pair) in entities.iter().enumerate() {
            let id = pair.entity.base.id;
            let new_node = tracker.new_node(&id.to_string());
            let new_point = tracker.new_point(ENTITY_POINT, id);
            let Some(versions) = versions else {
                if new_node {
                    plan.graph.push(position);
                }
                if new_point {
                    plan.new_vectors.push(position);
                }
                plan.versions.push(None);
                continue;
            };
            let now = EntityVersion::of(&pair.entity);
            let earlier = versions.entities.get(&id);
            if new_node || earlier.is_none_or(|v| v.graph != now.graph) {
                plan.graph.push(position);
                if !new_node {
                    plan.changed.push(position);
                }
            }
            if new_point {
                plan.new_vectors.push(position);
            } else if earlier.is_none_or(|v| v.vector_differs(&now)) {
                plan.rewritten_vectors.push(position);
            }
            plan.versions.push(Some(now));
        }
        plan
    }
}

/// Persist one group: ledger rows, then graph writes, then vector points, then
/// edge evidence (see the module docs for the exact order).
pub(crate) async fn persist_group(
    group: PersistGroup<'_>,
    state: &mut PersistState,
    context: &PersistContext<'_>,
) -> Result<(), CognifyError> {
    // Split the borrow: the tracker reads what earlier groups wrote while the
    // rewrite bookkeeping is updated as this group writes.
    let PersistState {
        written: run_written,
        versions,
        counted_edges,
        edge_texts,
        edge_type_owner,
        embeddings: run_embeddings,
        embedding_count: run_embedding_count,
        indexed_fields: run_indexed_fields,
    } = state;
    let mut tracker = Tracker::new(run_written);
    let plan = EntityPlan::new(group.entities, versions.as_ref(), &tracker);

    // ── Prepare: pure computation over the group, no I/O ────────────────────
    // Discovering the structural edges (`get_graph_from_model`) is what lets
    // the ledger name every artifact before the first of them exists.
    let mut extractable: Vec<&dyn GraphExtractable> = Vec::new();
    extractable.extend(group.chunks.iter().map(|c| c as &dyn GraphExtractable));
    extractable.extend(group.summaries.iter().map(|s| s as &dyn GraphExtractable));
    for pair in group.entities {
        extractable.push(&pair.entity as &dyn GraphExtractable);
        for entity_type in pair.entity_types() {
            extractable.push(entity_type as &dyn GraphExtractable);
        }
    }
    let mut structural_edges = crate::graph_extraction::get_graph_from_model(&extractable);

    // Runtime custom graphs: one storage walk per Python `add_data_points`
    // batch, shared by the ledger, the graph writes and the index pass. Raw
    // node writes are needed because their concrete Rust types do not exist;
    // within a batch a node id is claimed by its first occurrence, and nodes
    // are deduplicated across batches by their Python-compatible identity UUID.
    let stored_dynamic =
        crate::tasks::store_dynamic_graphs(group.dynamic_graphs, group.chunks, context.config);
    let mut dynamic_nodes: BTreeMap<Uuid, &DynamicDataPoint> = BTreeMap::new();
    let mut seen_relationships = HashSet::new();
    let mut dynamic_edges: Vec<EdgeData> = Vec::new();
    let now = Utc::now().to_rfc3339();
    for stored in &stored_dynamic {
        for node in &stored.nodes {
            dynamic_nodes.entry(node.base.id).or_insert(node);
        }
        for relationship in &stored.relationships {
            if seen_relationships.insert(relationship) {
                dynamic_edges.push((
                    relationship.source.to_string(),
                    relationship.target.to_string(),
                    relationship.field_name.clone(),
                    HashMap::from([(Cow::from("updated_at"), json!(now))]),
                ));
            }
        }
    }

    // Before the ledger rows are built, so they carry the same `edge_text`
    // the graph edges and their `EdgeType` points are keyed on.
    crate::tasks::stamp_structural_edge_text(
        &mut structural_edges,
        &mut dynamic_edges,
        &group,
        &dynamic_nodes,
    );

    // ── 1. Ledger: every row before any artifact (I1) ───────────────────────
    let (nodes, edges) = crate::tasks::extraction_ownership_rows(
        context.ledger,
        group.chunks,
        group.entities,
        group.edges,
        group.claimed_existing_edges,
        group.timestamps,
        group.producers,
        context.run_chunk_documents,
    );
    if let Some(versions) = versions.as_mut() {
        let entity_ids: HashSet<Uuid> = group.entities.iter().map(|p| p.entity.base.id).collect();
        for row in nodes.iter().filter(|row| entity_ids.contains(&row.slug)) {
            let data_ids = versions.entity_rows.entry(row.slug).or_default();
            if !data_ids.contains(&row.data_id) {
                data_ids.push(row.data_id);
            }
        }
    }
    // The rows this group writes itself, already carrying the entity as it is
    // now; every other row of a changed entity is rewritten below.
    let group_entity_rows: HashSet<Uuid> = nodes
        .iter()
        .map(|row| row.id)
        .filter(|id| !tracker.earlier.ledger_nodes.contains(id))
        .collect();
    let (node_rows, edge_rows) = write_ledger(context.db, &mut tracker, nodes, edges).await?;
    if node_rows + edge_rows > 0 {
        info!(
            "Recorded ownership of {node_rows} entities and Timestamps and {edge_rows} semantic \
             edges before writing them"
        );
    }
    let (nodes, edges) = crate::tasks::web_page_ownership_rows(context.ledger, group.web_pages);
    write_ledger(context.db, &mut tracker, nodes, edges).await?;
    let (nodes, edges) = crate::tasks::dynamic_graph_ownership_rows(
        context.ledger,
        group.dynamic_graphs,
        &stored_dynamic,
        group.chunks,
        &structural_edges,
        &dynamic_edges,
    );
    // A runtime custom graph's `contains` row and the per-document structural
    // row of the same edge share an id (both name the chunk's document, since
    // SDK-1694 O5a); the custom row keeps its own, as it did before.
    let custom_rows: HashSet<Uuid> = edges.iter().map(|row| row.id).collect();
    write_ledger(context.db, &mut tracker, nodes, edges).await?;
    let (nodes, mut edges) = crate::tasks::provenance_rows(
        context.ledger,
        group.chunks,
        group.entities,
        group.edges,
        group.summaries,
        group.documents,
        &structural_edges,
        group.producers,
        context.run_chunk_documents,
    );
    edges.retain(|row| row.data_id.is_nil() || !custom_rows.contains(&row.id));
    let (node_rows, edge_rows) = write_ledger(context.db, &mut tracker, nodes, edges).await?;
    if node_rows > 0 {
        info!("Upserted {node_rows} provenance node records");
    }
    if edge_rows > 0 {
        info!("Upserted {edge_rows} provenance edge records");
    }
    // An entity this group changed: the rows earlier groups wrote for it
    // carry its old form in `attributes`, so they are written again.
    if let Some(versions) = versions.as_ref()
        && !plan.changed.is_empty()
    {
        let rows: Vec<GraphNode> = plan
            .changed
            .iter()
            .map(|position| &group.entities[*position].entity)
            .flat_map(|entity| {
                versions
                    .entity_rows
                    .get(&entity.base.id)
                    .into_iter()
                    .flatten()
                    .map(|data_id| {
                        crate::tasks::entity_ledger_row(context.ledger, entity, *data_id)
                    })
                    .filter(|row| !group_entity_rows.contains(&row.id))
            })
            .collect();
        if !rows.is_empty() {
            cognee_database::ProvenanceDb::upsert_provenance_graph(context.db, &rows, &[]).await?;
            info!(
                "Rewrote {} ownership rows of entities this document changed",
                rows.len()
            );
        }
    }
    if let Some(dlt) = group.dlt {
        let (nodes, edges) = crate::tasks::dlt_provenance_rows(
            context.ledger,
            &dlt.schema_nodes,
            &dlt.edges,
            &dlt.row_chunk_docs,
            &dlt.column_value_owners,
        );
        write_ledger(context.db, &mut tracker, nodes, edges).await?;
    }

    // ── 2. Graph nodes: every node of the group, before any edge ────────────
    let graph_db = context.graph_db.as_ref();
    let entity_refs: Vec<&cognee_models::Entity> = plan
        .graph
        .iter()
        .map(|position| &group.entities[*position].entity)
        .collect();
    write_typed_nodes_as_given(graph_db, &mut tracker, &entity_refs, |e| e.base.id).await?;
    if let Some(versions) = versions.as_mut() {
        for position in &plan.graph {
            if let Some(version) = plan.versions[*position] {
                versions
                    .entities
                    .entry(group.entities[*position].entity.base.id)
                    .and_modify(|v| v.graph = version.graph)
                    .or_insert(version);
            }
        }
    }
    // Timestamp nodes: not embedded, no EntityType; written before the edges
    // that point at them.
    let timestamp_refs: Vec<&cognee_models::Timestamp> = group
        .timestamps
        .iter()
        .map(|node| &node.timestamp)
        .collect();
    let written_timestamps =
        write_typed_nodes(graph_db, &mut tracker, &timestamp_refs, |t| t.base.id).await?;
    if written_timestamps > 0 {
        info!("Upserted {written_timestamps} Timestamp nodes");
    }
    let (web_nodes, web_edges) = crate::tasks::web_page_graph(group.web_pages);
    write_web_nodes(graph_db, &mut tracker, versions.as_mut(), web_nodes).await?;

    // Python stores DocumentChunks, TextSummaries and EntityTypes as graph
    // nodes in `add_data_points`.
    let chunks: Vec<&DocumentChunk> = group.chunks.iter().collect();
    let written = write_typed_nodes(graph_db, &mut tracker, &chunks, |c| c.base.id).await?;
    if written > 0 {
        info!("Stored {written} document chunks as graph nodes");
    }
    let summaries: Vec<&TextSummary> = group.summaries.iter().collect();
    let written = write_typed_nodes(graph_db, &mut tracker, &summaries, |s| s.base.id).await?;
    if written > 0 {
        info!("Stored {written} text summaries as graph nodes");
    }
    // One write per type id: many entities share a type.
    let entity_types = unique_entity_types(group.entities);
    if !group.entities.is_empty() {
        let written =
            write_typed_nodes(graph_db, &mut tracker, &entity_types, |t| t.base.id).await?;
        info!("Stored {written} entity types as graph nodes");
    }
    // Documents. Python reaches them by recursively walking each chunk's
    // `is_part_of` (a full Document DataPoint) in `get_graph_from_model`.
    // Rust's `is_part_of` is just a `Uuid`, so they are stored explicitly. The
    // node `id` equals the source Data item's id (content-addressed,
    // Python-identical) and its `type` is the concrete subclass name
    // (TextDocument, PdfDocument, …), so the `is_part_of` edge target
    // resolves to a stored Document node.
    let documents: Vec<&Document> = group.documents.iter().collect();
    let written = write_typed_nodes(graph_db, &mut tracker, &documents, |d| d.base.id).await?;
    if written > 0 {
        info!("Stored {written} documents as graph nodes");
    }
    write_raw_nodes(
        graph_db,
        &mut tracker,
        dynamic_nodes.values().map(|node| node.to_graph_value()),
    )
    .await?;
    if let Some(dlt) = group.dlt {
        let written =
            write_raw_nodes(graph_db, &mut tracker, dlt.schema_nodes.iter().cloned()).await?;
        if written > 0 {
            info!("Added {written} DLT schema nodes to graph");
        }
    }

    // ── 3. Graph edges, now that every endpoint exists ──────────────────────
    write_semantic_edges(graph_db, &mut tracker, group.edges).await?;
    write_missing_edges(graph_db, &mut tracker, web_edges).await?;
    write_edges(graph_db, &mut tracker, &dynamic_edges).await?;
    // The structural edges `get_graph_from_model` discovered above.
    let written = write_edges(graph_db, &mut tracker, &structural_edges).await?;
    if written > 0 {
        info!("Upserted {written} structural edges");
    }
    if let Some(dlt) = group.dlt {
        // The schema nodes' vectors before the FK edges, as the DLT step
        // wrote them before O4a.
        let fresh: Vec<serde_json::Value> = dlt
            .schema_nodes
            .iter()
            .filter(|node| schema_node_id(node).is_none_or(|id| tracker.new_point(DLT_POINT, id)))
            .cloned()
            .collect();
        if !fresh.is_empty() {
            crate::tasks::index_dlt_schema_nodes(
                &fresh,
                context.ledger,
                context.embedding_engine,
                context.vector_db,
            )
            .await?;
        }
        tracker.now.vectors.extend(
            fresh
                .iter()
                .filter_map(schema_node_id)
                .map(|id| (DLT_POINT, id)),
        );
        let written = write_edges(graph_db, &mut tracker, &dlt.edges).await?;
        if written > 0 {
            info!("Added {written} DLT FK edges to graph");
        }
    }

    // ── 4. Vector points ────────────────────────────────────────────────────
    // `number_of_edges`: every edge of the group whose key no earlier group
    // counted, in its class.
    let semantic_texts: Vec<(Uuid, String)> = group
        .edges
        .iter()
        .map(|edge| {
            let data = semantic_edge_data(edge);
            (edge_key(&data), crate::tasks::edge_retrieval_text(edge))
        })
        .collect();
    let class_texts = |edges: &[EdgeData]| -> Vec<(Uuid, String)> {
        edges
            .iter()
            .map(|edge| {
                (
                    edge_key(edge),
                    EdgeType::retrieval_text(
                        edge.3.get("edge_text").and_then(serde_json::Value::as_str),
                        &edge.2,
                    ),
                )
            })
            .collect()
    };
    let classes = [
        (SEMANTIC_EDGES, semantic_texts),
        (STRUCTURAL_EDGES, class_texts(&structural_edges)),
        (DYNAMIC_EDGES, class_texts(&dynamic_edges)),
    ];
    let group_texts: Vec<String> = classes
        .iter()
        .flat_map(|(_, texts)| texts.iter().map(|(_, text)| text.clone()))
        .collect();
    let user_label = context.user_id.as_ref().map(|id| id.to_string());
    edge_type_owner.get_or_insert_with(|| (context.ledger.dataset_id, user_label.clone()));
    let mut edge_types = run_edge_types(
        group_texts.into_iter(),
        context.ledger.dataset_id,
        user_label.as_deref(),
    );
    edge_types.retain(|edge_type| match versions.as_ref() {
        // Per document: a point is written when it is new, or when this group
        // reaches it with a smaller spelling than the one embedded, which is
        // the spelling one group would have kept.
        Some(versions) => versions
            .edge_type_names
            .get(&edge_type.base.id)
            .is_none_or(|name| edge_type.relationship_name < *name),
        None => tracker.new_point(EDGE_TYPE_POINT, edge_type.base.id),
    });

    let outputs = write_vectors(
        group,
        context,
        &mut tracker,
        versions.as_mut(),
        &plan,
        &edge_types,
        &dynamic_nodes,
        &entity_types,
    )
    .await?;
    for (class, texts) in classes {
        let counted = &mut counted_edges[class];
        let mut keys = Vec::with_capacity(texts.len());
        for (key, text) in texts {
            if !counted.contains(&key) {
                *edge_texts.entry(text).or_insert(0) += 1;
                keys.push(key);
            }
        }
        counted.extend(keys);
    }
    if let Some(versions) = versions.as_mut() {
        for edge_type in &edge_types {
            versions
                .edge_type_names
                .insert(edge_type.base.id, edge_type.relationship_name.clone());
        }
    }

    // ── 5. Edge evidence ────────────────────────────────────────────────────
    // Only after the graph and vector writes succeeded (Python captures at the
    // end of `add_data_points`, `add_data_points.py:449`): the sidecar then
    // never describes an edge the graph does not hold. Python captures over
    // the batch's `edges` after `custom_edges` were appended to them
    // (`add_data_points.py:403`), so runtime custom edges are structural
    // candidates too.
    if let Some(identity) = context.evidence {
        let rows: Vec<_> = crate::tasks::edge_evidence_rows(
            identity,
            group.chunks,
            structural_edges.iter().chain(dynamic_edges.iter()),
            group.producers,
        )
        .into_iter()
        .filter(|row| !tracker.earlier.evidence.contains(&row.id))
        .collect();
        if !rows.is_empty() {
            cognee_database::ProvenanceDb::insert_edge_evidence(context.db, &rows).await?;
            info!("Recorded {} edge evidence rows", rows.len());
            tracker.now.evidence.extend(rows.iter().map(|row| row.id));
        }
    }

    let Tracker { now, .. } = tracker;
    run_written.absorb(now);
    for embedding in outputs.replaced {
        match run_embeddings.iter_mut().find(|earlier| {
            earlier.data_point_id == embedding.data_point_id && earlier.data_type == "Entity"
        }) {
            Some(earlier) => *earlier = embedding,
            None => run_embeddings.push(embedding),
        }
    }
    run_embeddings.extend(outputs.embeddings);
    *run_embedding_count += outputs.embedding_count;
    for (collection, count) in outputs.indexed_fields.field_counts {
        *run_indexed_fields
            .field_counts
            .entry(collection)
            .or_insert(0) += count;
    }
    Ok(())
}

/// The extraction part of the layer on its own, for
/// [`crate::tasks::extract_graph_from_data`] driven outside the pipeline: the
/// extraction ownership rows and the EntityType rows, then the entity,
/// Timestamp and EntityType nodes, then the semantic edges — exactly the steps
/// [`persist_group`] takes for them, in its order (every node before any edge).
///
/// The EntityType nodes are what an ontology's edges name: an entity→class or
/// class→class edge (`Algorithm → Technology`) has a type at one end at least.
/// Python writes every EntityType a chunk's graph reaches in `add_data_points`
/// before any edge; without them here Ladybug (`MATCH … MERGE`) dropped those
/// edges while their ledger rows stayed, and Postgres failed the call on
/// `graph_edge_source_id_fkey` after the ledger was committed.
pub(crate) async fn persist_extraction(
    db: &dyn RelationalDb,
    graph_db: &dyn GraphDBTrait,
    ledger: LedgerIdentity,
    graph: &ExtractedGraphData,
) -> Result<(), CognifyError> {
    let earlier = Written::default();
    let mut tracker = Tracker::new(&earlier);
    let (mut nodes, edges) = crate::tasks::extraction_ownership_rows(
        ledger,
        &graph.chunks,
        &graph.entities,
        &graph.edges,
        &graph.claimed_existing_edges,
        &graph.timestamps,
        &graph.producers,
        None,
    );
    // The same EntityType rows `persist_group` writes (through
    // `provenance_rows`), in the same transaction as the rest.
    nodes.extend(crate::tasks::entity_type_provenance_rows(
        ledger,
        &graph.entities,
        &crate::tasks::chunk_document_map(&graph.chunks),
        &graph.producers,
    ));
    let (node_rows, edge_rows) = write_ledger(db, &mut tracker, nodes, edges).await?;
    if node_rows + edge_rows > 0 {
        info!(
            "Recorded ownership of {node_rows} entities, entity types and Timestamps and \
             {edge_rows} semantic edges before writing them"
        );
    }
    let entity_refs: Vec<&cognee_models::Entity> =
        graph.entities.iter().map(|pair| &pair.entity).collect();
    write_typed_nodes(graph_db, &mut tracker, &entity_refs, |e| e.base.id).await?;
    let timestamp_refs: Vec<&cognee_models::Timestamp> = graph
        .timestamps
        .iter()
        .map(|node| &node.timestamp)
        .collect();
    let written = write_typed_nodes(graph_db, &mut tracker, &timestamp_refs, |t| t.base.id).await?;
    if written > 0 {
        info!("Upserted {written} Timestamp nodes");
    }
    let entity_types = unique_entity_types(&graph.entities);
    let written = write_typed_nodes(graph_db, &mut tracker, &entity_types, |t| t.base.id).await?;
    if written > 0 {
        info!("Stored {written} entity types as graph nodes");
    }
    write_semantic_edges(graph_db, &mut tracker, &graph.edges).await
}

/// The web-page part of the layer on its own, for
/// [`crate::tasks::create_web_page_nodes`] driven outside the pipeline: the
/// ownership rows, then the WebPage / WebSite nodes and the sourced chunks'
/// nodes, then the edges the graph does not hold yet.
///
/// The `SOURCED_FROM` edges start at the chunks, so a sourced chunk the graph
/// does not hold yet is written here too (node and ownership row), before any
/// edge, as [`persist_group`] writes chunks in the same group as the web
/// nodes. A caller that had not stored the chunks no longer loses the edges
/// (Ladybug) or the call (Postgres, `graph_edge` foreign key).
///
/// A chunk the graph already holds is left as stored. Both graph backends
/// replace a node's whole property set on conflict, and the caller's copy may
/// be older than the stored one: the chunks handed to
/// [`crate::tasks::extract_graph_from_data`] lack the `contains` list and
/// stamps its returned chunks carry, so upserting them again would silently
/// strip what an earlier write stored.
pub(crate) async fn persist_web_pages(
    db: &dyn RelationalDb,
    graph_db: &dyn GraphDBTrait,
    ledger: LedgerIdentity,
    pages: &[WebPage],
    chunks: &[DocumentChunk],
) -> Result<(), CognifyError> {
    let earlier = Written::default();
    let mut tracker = Tracker::new(&earlier);
    // Only the chunks some page names: the ones a `SOURCED_FROM` edge starts at.
    let sourced: HashSet<Uuid> = pages
        .iter()
        .flat_map(|page| page.chunk_ids.iter().copied())
        .collect();
    let sourced_ids: Vec<String> = chunks
        .iter()
        .filter(|chunk| sourced.contains(&chunk.base.id))
        .map(|chunk| chunk.base.id.to_string())
        .collect();
    // `get_nodes` is a set lookup: key on each row's own `id`, not position.
    let stored: HashSet<String> = if sourced_ids.is_empty() {
        HashSet::new()
    } else {
        graph_db
            .get_nodes(&sourced_ids)
            .await?
            .iter()
            .filter_map(|row| row.get("id").and_then(|id| id.as_str()).map(str::to_owned))
            .collect()
    };
    let sourced_chunks: Vec<&DocumentChunk> = chunks
        .iter()
        .filter(|chunk| {
            sourced.contains(&chunk.base.id) && !stored.contains(&chunk.base.id.to_string())
        })
        .collect();
    let (mut nodes, edges) = crate::tasks::web_page_ownership_rows(ledger, pages);
    nodes.extend(crate::tasks::chunk_provenance_rows(
        ledger,
        sourced_chunks.iter().copied(),
    ));
    write_ledger(db, &mut tracker, nodes, edges).await?;
    let (web_nodes, web_edges) = crate::tasks::web_page_graph(pages);
    write_raw_nodes(graph_db, &mut tracker, web_nodes.into_iter()).await?;
    write_typed_nodes(graph_db, &mut tracker, &sourced_chunks, |c| c.base.id).await?;
    write_missing_edges(graph_db, &mut tracker, web_edges).await
}

/// Write the ledger rows no earlier group wrote, in one transaction, and
/// return how many node and edge rows that was.
async fn write_ledger(
    db: &dyn RelationalDb,
    tracker: &mut Tracker<'_>,
    mut nodes: Vec<GraphNode>,
    mut edges: Vec<GraphEdge>,
) -> Result<(usize, usize), CognifyError> {
    nodes.retain(|row| !tracker.earlier.ledger_nodes.contains(&row.id));
    edges.retain(|row| !tracker.earlier.ledger_edges.contains(&row.id));
    if nodes.is_empty() && edges.is_empty() {
        return Ok((0, 0));
    }
    cognee_database::ProvenanceDb::upsert_provenance_graph(db, &nodes, &edges).await?;
    tracker
        .now
        .ledger_nodes
        .extend(nodes.iter().map(|row| row.id));
    tracker
        .now
        .ledger_edges
        .extend(edges.iter().map(|row| row.id));
    Ok((nodes.len(), edges.len()))
}

/// The semantic (extracted and ontology) edges.
async fn write_semantic_edges(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    edges: &[GraphEdgePair],
) -> Result<(), CognifyError> {
    let edge_data: Vec<EdgeData> = edges.iter().map(semantic_edge_data).collect();
    let written = write_edges(graph_db, tracker, &edge_data).await?;
    if written > 0 {
        info!("Upserted {written} extracted graph edges (LLM and ontology)");
    }
    Ok(())
}

/// The web-page edges the graph does not hold yet.
async fn write_missing_edges(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    candidate_edges: Vec<EdgeData>,
) -> Result<(), CognifyError> {
    if candidate_edges.is_empty() {
        return Ok(());
    }
    let existing: HashSet<(String, String, String)> = graph_db
        .has_edges(&candidate_edges)
        .await
        .map_err(CognifyError::from)?
        .into_iter()
        .map(|(source, target, relationship, _)| (source, target, relationship))
        .collect();
    let missing: Vec<EdgeData> = candidate_edges
        .into_iter()
        .filter(|(source, target, relationship, _)| {
            !existing.contains(&(source.clone(), target.clone(), relationship.clone()))
        })
        .collect();
    let written = write_edges(graph_db, tracker, &missing).await?;
    if written > 0 {
        info!("Upserted {written} web page edges");
    }
    Ok(())
}

/// Write the typed nodes no earlier group wrote; return how many.
async fn write_typed_nodes<T: serde::Serialize + Sync>(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    nodes: &[&T],
    id: impl Fn(&T) -> Uuid,
) -> Result<usize, CognifyError> {
    let fresh: Vec<&T> = nodes
        .iter()
        .copied()
        .filter(|node| tracker.new_node(&id(node).to_string()))
        .collect();
    write_typed_nodes_as_given(graph_db, tracker, &fresh, id).await
}

/// Write `nodes`, whether or not an earlier group wrote them; return how many.
async fn write_typed_nodes_as_given<T: serde::Serialize + Sync>(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    nodes: &[&T],
    id: impl Fn(&T) -> Uuid,
) -> Result<usize, CognifyError> {
    if nodes.is_empty() {
        return Ok(0);
    }
    graph_db
        .add_nodes(nodes)
        .await
        .map_err(CognifyError::from)?;
    tracker
        .now
        .graph_nodes
        .extend(nodes.iter().map(|node| id(node).to_string()));
    Ok(nodes.len())
}

/// Write the raw JSON nodes no earlier group wrote; return how many.
async fn write_raw_nodes(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    nodes: impl Iterator<Item = serde_json::Value>,
) -> Result<usize, CognifyError> {
    let fresh: Vec<serde_json::Value> = nodes
        .filter(|node| raw_node_id(node).is_none_or(|id| tracker.new_node(id)))
        .collect();
    write_raw_nodes_as_given(graph_db, tracker, fresh).await
}

/// Write `nodes`, whether or not an earlier group wrote them; return how many.
async fn write_raw_nodes_as_given(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    nodes: Vec<serde_json::Value>,
) -> Result<usize, CognifyError> {
    if nodes.is_empty() {
        return Ok(0);
    }
    let ids: Vec<String> = nodes
        .iter()
        .filter_map(raw_node_id)
        .map(str::to_string)
        .collect();
    let count = nodes.len();
    graph_db
        .add_nodes_raw(nodes)
        .await
        .map_err(CognifyError::from)?;
    tracker.now.graph_nodes.extend(ids);
    Ok(count)
}

fn raw_node_id(node: &serde_json::Value) -> Option<&str> {
    node.get("id").and_then(serde_json::Value::as_str)
}

/// The WebPage / WebSite nodes. Per document, a node an earlier group wrote
/// is written again when this group's copy differs — two documents of one
/// URL share a page, and the last one in input order wins, as when the run is
/// saved as one group ([`crate::tasks::web_page_graph`]).
async fn write_web_nodes(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    versions: Option<&mut Versions>,
    nodes: Vec<serde_json::Value>,
) -> Result<usize, CognifyError> {
    let Some(versions) = versions else {
        return write_raw_nodes(graph_db, tracker, nodes.into_iter()).await;
    };
    let mut changed = Vec::new();
    for node in nodes {
        let Some(id) = raw_node_id(&node).map(str::to_string) else {
            changed.push(node);
            continue;
        };
        let version = fingerprint(node.to_string().as_bytes());
        if tracker.new_node(&id) || versions.raw_nodes.get(&id) != Some(&version) {
            versions.raw_nodes.insert(id, version);
            changed.push(node);
        }
    }
    write_raw_nodes_as_given(graph_db, tracker, changed).await
}

/// Write the edges no earlier group wrote; return how many.
async fn write_edges(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    edges: &[EdgeData],
) -> Result<usize, CognifyError> {
    let fresh = fresh(edges, tracker.first_group(), |edge| tracker.new_edge(edge));
    if fresh.is_empty() {
        return Ok(0);
    }
    graph_db
        .add_edges(&fresh)
        .await
        .map_err(CognifyError::from)?;
    tracker.now.graph_edges.extend(fresh.iter().map(edge_key));
    Ok(fresh.len())
}

/// A semantic edge as the graph stores it: its string properties as JSON.
fn semantic_edge_data(edge_pair: &GraphEdgePair) -> EdgeData {
    let properties = edge_pair
        .properties
        .iter()
        .map(|(k, v)| (Cow::Owned(k.clone()), serde_json::Value::String(v.clone())))
        .collect();
    (
        edge_pair.source_entity_id.to_string(),
        edge_pair.target_entity_id.to_string(),
        edge_pair.relationship_name.clone(),
        properties,
    )
}

/// Every distinct EntityType of `entities`, first seen first.
fn unique_entity_types(entities: &[GraphNodePair]) -> Vec<&EntityType> {
    let mut seen = HashSet::new();
    entities
        .iter()
        .flat_map(|pair| pair.entity_types())
        .filter(|entity_type| seen.insert(entity_type.base.id))
        .collect()
}

/// Embed and index the group's points that no earlier group embedded (and,
/// per document, rewrite the payloads this group changed), and return the
/// group's share of the run-level outputs.
#[allow(clippy::too_many_arguments)]
async fn write_vectors(
    group: PersistGroup<'_>,
    context: &PersistContext<'_>,
    tracker: &mut Tracker<'_>,
    versions: Option<&mut Versions>,
    plan: &EntityPlan,
    edge_types: &[EdgeType],
    dynamic_nodes: &BTreeMap<Uuid, &DynamicDataPoint>,
    entity_types: &[&EntityType],
) -> Result<GroupOutputs, CognifyError> {
    let dataset_id = context.ledger.dataset_id;

    let first = tracker.first_group();
    let chunks = fresh(group.chunks, first, |chunk| {
        tracker.new_point(CHUNK_POINT, chunk.base.id)
    });
    let entities: Cow<'_, [GraphNodePair]> =
        if first && plan.new_vectors.len() == group.entities.len() {
            Cow::Borrowed(group.entities)
        } else {
            Cow::Owned(
                plan.new_vectors
                    .iter()
                    .map(|position| group.entities[*position].clone())
                    .collect(),
            )
        };
    let summaries = fresh(group.summaries, first, |summary| {
        tracker.new_point(SUMMARY_POINT, summary.base.id)
    });
    let documents = fresh(group.documents, first, |document| {
        tracker.new_point(DOCUMENT_POINT, document.base.id)
    });
    let embedded_types: HashSet<Uuid> = entity_types
        .iter()
        .map(|entity_type| entity_type.base.id)
        .filter(|id| !tracker.new_point(ENTITY_TYPE_POINT, *id))
        .collect();
    let new_types: Vec<Uuid> = entity_types
        .iter()
        .map(|entity_type| entity_type.base.id)
        .filter(|id| !embedded_types.contains(id))
        .collect();
    let dynamic: Vec<&DynamicDataPoint> = dynamic_nodes
        .values()
        .copied()
        .filter(|node| tracker.new_point(DYNAMIC_POINT, node.base.id))
        .collect();

    // One vector per chunk, entity and summary, whether or not they are kept.
    let dynamic_embedding_count: usize = dynamic
        .iter()
        .map(|node| node.index_field_texts().count())
        .sum();
    let embedding_count = chunks.len() + entities.len() + summaries.len() + dynamic_embedding_count;
    // Retained vectors are generated up front and reused by the index pass.
    // Otherwise the index pass embeds those texts itself, a window at a time,
    // and no corpus-sized set of vectors is ever held (SDK-711): it already
    // embeds whatever has no precomputed vector, so the work done is the same.
    let mut embeddings = if context.config.retain_embeddings {
        crate::tasks::generate_embeddings(
            &chunks,
            &entities,
            &summaries,
            Arc::clone(context.embedding_engine),
        )
        .await?
    } else {
        Vec::new()
    };
    // With `retain_embeddings` every vector the run embedded is kept anyway:
    // per document, the entities' are also cached by id, so a payload a later
    // document changes is rewritten with the vector it was embedded with.
    let retain = context.config.retain_embeddings;
    let precomputed: &[Embedding] = &embeddings;

    let anything = !chunks.is_empty()
        || !entities.is_empty()
        || !summaries.is_empty()
        || !documents.is_empty()
        || !edge_types.is_empty()
        || !new_types.is_empty();
    // A group with nothing to index (a DLT group) opens no index pass, and so
    // no bulk-load scope of its own.
    // The new types of entities whose own point is not written by this group
    // (an earlier group wrote it): indexed all the same.
    let extra_types: Vec<&EntityType> = entity_types
        .iter()
        .copied()
        .filter(|entity_type| !embedded_types.contains(&entity_type.base.id))
        .collect();
    let mut indexed_fields = if anything {
        crate::tasks::index_data_points_except(
            &chunks,
            &entities,
            &summaries,
            &documents,
            edge_types,
            dataset_id,
            Arc::clone(context.embedding_engine),
            Arc::clone(context.vector_db),
            precomputed,
            &embedded_types,
            &extra_types,
        )
        .await?
    } else {
        IndexedFieldsStats::default()
    };
    let mut replaced: Vec<Embedding> = Vec::new();
    if let Some(versions) = versions {
        // The payloads this group changed. Not counted: one point per entity,
        // as in one group. A vector is reused only when it is cached (with
        // `retain_embeddings`) and the name it embeds is unchanged; otherwise
        // the index pass embeds the name (again).
        let mut rewritten: Vec<GraphNodePair> = Vec::new();
        let mut reused: Vec<Embedding> = Vec::new();
        let mut renamed: Vec<GraphNodePair> = Vec::new();
        for position in &plan.rewritten_vectors {
            let pair = &group.entities[*position];
            let id = pair.entity.base.id;
            let same_name = match (versions.entities.get(&id), plan.versions[*position]) {
                (Some(earlier), Some(now)) => earlier.name == now.name,
                _ => false,
            };
            match versions.entity_vectors.get(&id) {
                Some(vector) if same_name => {
                    reused.push(Embedding::new(id, "Entity", "name", vector.clone()));
                }
                _ if retain => renamed.push(pair.clone()),
                _ => {}
            }
            rewritten.push(pair.clone());
        }
        if !rewritten.is_empty() {
            // A renamed entity's retained vector is replaced by its new name's.
            let fresh = if renamed.is_empty() {
                Vec::new()
            } else {
                crate::tasks::generate_embeddings(
                    &[],
                    &renamed,
                    &[],
                    Arc::clone(context.embedding_engine),
                )
                .await?
            };
            reused.extend(fresh.iter().cloned());
            let all_types: HashSet<Uuid> = entity_types.iter().map(|t| t.base.id).collect();
            crate::tasks::index_data_points_except(
                &[],
                &rewritten,
                &[],
                &[],
                &[],
                dataset_id,
                Arc::clone(context.embedding_engine),
                Arc::clone(context.vector_db),
                &reused,
                &all_types,
                &[],
            )
            .await?;
            info!(
                "Rewrote {} entity vector payloads this document changed",
                rewritten.len()
            );
            replaced = fresh;
        }
        if retain {
            for embedding in embeddings
                .iter()
                .chain(&replaced)
                .filter(|embedding| embedding.data_type == "Entity")
            {
                versions
                    .entity_vectors
                    .insert(embedding.data_point_id, embedding.vector.clone());
            }
        }
        for position in plan.new_vectors.iter().chain(&plan.rewritten_vectors) {
            if let Some(version) = plan.versions[*position] {
                versions
                    .entities
                    .entry(group.entities[*position].entity.base.id)
                    .and_modify(|v| {
                        v.name = version.name;
                        v.payload = version.payload;
                    })
                    .or_insert(version);
            }
        }
    }
    if !dynamic.is_empty() {
        let dynamic_embeddings = crate::tasks::index_dynamic_graph(
            &dynamic,
            dataset_id,
            Arc::clone(context.embedding_engine),
            Arc::clone(context.vector_db),
            context.config,
            &mut indexed_fields,
        )
        .await?;
        embeddings.extend(dynamic_embeddings);
    }

    let vectors = &mut tracker.now.vectors;
    vectors.extend(chunks.iter().map(|c| (CHUNK_POINT, c.base.id)));
    vectors.extend(entities.iter().map(|e| (ENTITY_POINT, e.entity.base.id)));
    vectors.extend(new_types.into_iter().map(|id| (ENTITY_TYPE_POINT, id)));
    vectors.extend(summaries.iter().map(|s| (SUMMARY_POINT, s.base.id)));
    vectors.extend(edge_types.iter().map(|e| (EDGE_TYPE_POINT, e.base.id)));
    vectors.extend(documents.iter().map(|d| (DOCUMENT_POINT, d.base.id)));
    vectors.extend(dynamic.iter().map(|n| (DYNAMIC_POINT, n.base.id)));

    Ok(GroupOutputs {
        embeddings,
        replaced,
        embedding_count,
        indexed_fields,
    })
}

/// One group's share of the run-level outputs.
struct GroupOutputs {
    embeddings: Vec<Embedding>,
    /// Retained entity vectors this group re-embedded after a rename: they
    /// replace the run's earlier ones.
    replaced: Vec<Embedding>,
    embedding_count: usize,
    indexed_fields: IndexedFieldsStats,
}

/// A DLT schema node's id, as [`crate::tasks::index_dlt_schema_nodes`] reads it.
fn schema_node_id(node: &serde_json::Value) -> Option<Uuid> {
    node.get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
pub(crate) mod tests {
    //! Split equivalence (SDK-1694 O4a): persisting a run as one group per
    //! document, with one [`PersistState`], stores the same sets as persisting
    //! it as one group — and writes every node, edge and vector point once.

    use super::*;
    use std::collections::BTreeSet;

    use async_trait::async_trait;
    use cognee_database::DatabaseConnection;
    use cognee_embedding::MockEmbeddingEngine;
    use cognee_graph::MockGraphDB;
    use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
    use cognee_llm::{Llm, LlmError, LlmResult, MessageRole};
    use cognee_models::DataPoint;
    use cognee_ontology::{OntologyFileInput, OntologyResolver, RdfLibOntologyResolver};
    use cognee_vector::MockVectorDB;
    use sea_orm::{DatabaseBackend, FromQueryResult, JsonValue, Statement};
    use serde_json::Value;

    use crate::failure::FailureReport;
    use crate::tasks::{ExtractedChunks, SummarizedData};

    const ONTOLOGY: &str = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix : <http://test.cognee.ai/ontology#> .

:LegalEntity a owl:Class ; rdfs:label "LegalEntity" .
:Organisation a owl:Class ; rdfs:subClassOf :LegalEntity ; rdfs:label "Organisation" .
:Technology a owl:Class ; rdfs:label "Technology" .
:Algorithm a owl:Class ; rdfs:subClassOf :Technology ; rdfs:label "Algorithm" .
:DeepSort a :Algorithm ; rdfs:label "DeepSort" .
:TechCorp a :Organisation ; rdfs:label "TechCorp" .
"#;

    fn node(id: &str, name: &str, kind: &str, description: &str) -> Value {
        json!({"id": id, "name": name, "type": kind, "description": description})
    }

    fn edge(source: &str, target: &str, relationship: &str) -> Value {
        json!({"source_node_id": source, "target_node_id": target, "relationship_name": relationship})
    }

    /// The graph each chunk's marker is answered with: an entity typed
    /// differently in two documents and in two blocks of one, a Timestamp,
    /// ontology-resolved entities, an edge two documents produce and one the
    /// graph already holds.
    fn graph_for(marker: &str) -> Value {
        match marker {
            "MARKA1" => json!({
                "nodes": [
                    node("alice", "Alice", "Person", "A researcher."),
                    node("acme", "Acme", "Company", "A company."),
                    node("deepsort", "DeepSort", "Algorithm", "A ranking algorithm."),
                    node("apple", "Apple", "Company", "A technology company."),
                ],
                "edges": [
                    edge("alice", "acme", "works_at"),
                    edge("acme", "deepsort", "Uses Algorithm"),
                    edge("apple", "acme", "partners_with"),
                ],
            }),
            "MARKA2" => json!({
                "nodes": [
                    node("apple", "Apple", "Organization", "An old organization."),
                    node("alice", "Alice", "Person", "Born in 1867."),
                    node("1867", "1867", "Timestamp", "A year"),
                ],
                "edges": [edge("apple", "1867", "founded_in"), edge("alice", "1867", "born_in")],
            }),
            "MARKB1" => json!({
                "nodes": [
                    node("techcorp", "TechCorp", "Organisation", "A retailer."),
                    node("apple", "Apple", "Fruit", "A fruit."),
                    node("alice", "Alice", "Person", "A researcher."),
                    node("acme", "Acme", "Company", "A company."),
                    node("bob", "Bob", "Person", "A colleague."),
                ],
                "edges": [
                    edge("techcorp", "apple", "sells"),
                    edge("alice", "acme", "works_at"),
                    edge("bob", "acme", "visits"),
                ],
            }),
            "MARKC1" => json!({
                "nodes": [
                    node("bob", "Bob", "Person", "A speaker."),
                    node("carol", "Carol", "Person", "An organiser."),
                ],
                "edges": [edge("bob", "carol", "knows")],
            }),
            other => panic!("no canned graph for marker {other}"),
        }
    }

    /// Answers by schema and by the marker in the chunk text.
    struct MarkerLlm {
        custom: Option<Value>,
    }

    #[async_trait]
    impl Llm for MarkerLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Err(LlmError::FeatureNotSupported("structured only".to_string()))
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<Message>,
            json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            let user: String = messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .map(|message| message.content.as_str())
                .collect();
            let marker = ["MARKA1", "MARKA2", "MARKB1", "MARKC1"]
                .into_iter()
                .find(|marker| user.contains(marker))
                .unwrap_or_else(|| panic!("no marker in {user}"));
            let properties = json_schema.get("properties");
            if properties.and_then(|p| p.get("summary")).is_some() {
                return Ok(json!({
                    "summary": format!("Summary of {marker}."),
                    "description": "d",
                }));
            }
            if properties.and_then(|p| p.get("nodes")).is_some() {
                return Ok(graph_for(marker));
            }
            Ok(self.custom.clone().expect("a custom answer"))
        }

        fn model(&self) -> &str {
            "marker"
        }
    }

    fn document(id: Uuid, url: Option<&str>) -> Document {
        let mut base = DataPoint::new("TextDocument", None);
        base.id = id;
        Document {
            base,
            document_type: "text".to_string(),
            name: format!("doc-{id}"),
            raw_data_location: format!("file:///tmp/{id}.txt"),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: id,
            external_metadata: url.map(|url| json!({"_cognee": {"source_uri": url}}).to_string()),
        }
    }

    fn chunk(document_id: Uuid, index: usize, text: &str) -> DocumentChunk {
        DocumentChunk::new(
            Uuid::new_v5(&document_id, format!("chunk-{index}").as_bytes()),
            text.to_string(),
            text.split_whitespace().count(),
            index,
            "paragraph_end".to_string(),
            document_id,
        )
    }

    pub(crate) const DATASET: Uuid = Uuid::from_u128(0x0a4a_5001);
    pub(crate) const USER: Uuid = Uuid::from_u128(0x0a4a_5002);
    const DOC_A: Uuid = Uuid::from_u128(0xa);
    const DOC_B: Uuid = Uuid::from_u128(0xb);
    const DOC_C: Uuid = Uuid::from_u128(0xc);

    fn input() -> ExtractedChunks {
        ExtractedChunks {
            chunks: vec![
                chunk(DOC_A, 0, "MARKA1 Alice works at Acme, which uses DeepSort."),
                chunk(DOC_A, 1, "MARKA2 Apple was founded in 1867."),
                chunk(DOC_B, 0, "MARKB1 TechCorp sells apples; Bob visits Acme."),
                chunk(DOC_C, 0, "MARKC1 Bob knows Carol."),
            ],
            documents: vec![
                document(DOC_A, None),
                document(DOC_B, None),
                document(DOC_C, Some("https://example.com/talks/bob")),
            ],
            dataset_id: DATASET,
            user_id: Some(USER),
            tenant_id: None,
            failures: FailureReport::default(),
        }
    }

    /// The run's data as the persistence step receives it: the graph built
    /// over the default path (or the custom-schema one) against a graph that
    /// already holds `alice works_at acme`, so that edge is claimed; the
    /// summaries; the web pages.
    async fn run_data(config: &CognifyConfig) -> SummarizedData {
        let input = input();
        let llm: Arc<dyn Llm> = Arc::new(MarkerLlm {
            custom: Some(
                json!({"name": "Acme", "employees": [{"name": "Alice"}, {"name": "Bob"}]}),
            ),
        });
        let mut graph = if config.graph_schema.is_some() {
            let extracted =
                crate::tasks::extract_dynamic_graphs_from_data(&input, Arc::clone(&llm), config)
                    .await
                    .unwrap();
            crate::tasks::attach_dynamic_graphs(&input, extracted, config.chunk_attachment)
        } else {
            let seeded: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
            let alice = cognee_models::Entity::id_for("Alice");
            let acme = cognee_models::Entity::id_for("Acme");
            seeded
                .add_edges(&[(
                    alice.to_string(),
                    acme.to_string(),
                    "works_at".to_string(),
                    std::collections::HashMap::new(),
                )])
                .await
                .unwrap();
            let reader: Box<dyn std::io::Read> = Box::new(ONTOLOGY.as_bytes());
            let ontology: Arc<dyn OntologyResolver> =
                Arc::new(RdfLibOntologyResolver::new(OntologyFileInput::Reader(reader)).unwrap());
            let built = crate::tasks::build_extracted_graph(
                &input,
                Arc::clone(&llm),
                seeded,
                ontology,
                config,
                None,
                Some(3),
            )
            .await
            .unwrap();
            assert!(
                !built.claimed_existing_edges.is_empty() && !built.timestamps.is_empty(),
                "the fixture claims an existing edge and dates a fact"
            );
            built
        };
        graph.handoff.web_pages = crate::tasks::web_page_artifacts(&graph.documents, &graph.chunks);
        let summaries = crate::tasks::summarize_text(&input, llm, config)
            .await
            .unwrap()
            .summaries;
        SummarizedData {
            chunks: graph.chunks,
            documents: graph.documents,
            entities: graph.entities,
            timestamps: graph.timestamps,
            edges: graph.edges,
            claimed_existing_edges: graph.claimed_existing_edges,
            handoff: graph.handoff,
            producers: graph.producers,
            dynamic_graphs: graph.dynamic_graphs,
            summaries,
            dataset_id: DATASET,
            user_id: Some(USER),
            tenant_id: None,
            failures: FailureReport::default(),
        }
    }

    /// The artifacts of one document: its chunks, summaries and web page, and
    /// every entity, Timestamp and edge one of its chunks produced, so an
    /// artifact two documents produced is in both groups, as it will be when
    /// groups are built per document.
    struct DocumentGroup {
        chunks: Vec<DocumentChunk>,
        documents: Vec<Document>,
        summaries: Vec<TextSummary>,
        entities: Vec<GraphNodePair>,
        timestamps: Vec<TimestampNode>,
        edges: Vec<GraphEdgePair>,
        claimed_existing_edges: Vec<GraphEdgePair>,
        dynamic_graphs: Vec<AttributedDynamicGraph>,
        web_pages: Vec<WebPage>,
    }

    /// `data` split into one group per document (see [`DocumentGroup`]). An
    /// artifact no chunk produced — the ontology's, whose expansion ran over
    /// one document's entities — goes with what it hangs off: an entity with
    /// the document of its `chunk_id` stamp, an edge with its source entity.
    fn split_by_document(data: &SummarizedData) -> Vec<DocumentGroup> {
        let position: HashMap<Uuid, usize> = data
            .documents
            .iter()
            .enumerate()
            .map(|(index, document)| (document.base.id, index))
            .collect();
        let chunk_group: HashMap<Uuid, usize> = data
            .chunks
            .iter()
            .map(|chunk| (chunk.base.id, position[&chunk.document_id]))
            .collect();
        let groups_of = |chunks: &[Uuid]| -> BTreeSet<usize> {
            chunks
                .iter()
                .filter_map(|id| chunk_group.get(id).copied())
                .collect()
        };
        let entity_groups: HashMap<Uuid, BTreeSet<usize>> = data
            .entities
            .iter()
            .map(|pair| {
                let id = pair.entity.base.id;
                let mut groups = groups_of(data.producers.entity_chunks(id));
                // The documents whose ontology lookup brought it in (O5a).
                groups.extend(groups_of(data.producers.ontology_entity_chunks(id)));
                groups.extend(
                    data.chunks
                        .iter()
                        .filter(|chunk| chunk.contains.contains(&json!(id.to_string())))
                        .map(|chunk| chunk_group[&chunk.base.id]),
                );
                if groups.is_empty() {
                    let stamped = pair
                        .entity
                        .base
                        .get_metadata("chunk_id")
                        .and_then(Value::as_str)
                        .and_then(|chunk| Uuid::parse_str(chunk).ok())
                        .and_then(|chunk| chunk_group.get(&chunk).copied());
                    groups.insert(stamped.unwrap_or(0));
                }
                (id, groups)
            })
            .collect();
        let edge_groups = |edge: &GraphEdgePair| -> BTreeSet<usize> {
            let mut groups = groups_of(data.producers.edge_chunks(&edge.dedup_key()));
            groups.extend(groups_of(
                data.producers.ontology_edge_chunks(&edge.dedup_key()),
            ));
            if !groups.is_empty() {
                return groups;
            }
            // The source entity's first group; for an edge between types, the
            // first group of an entity of the source type; else the first.
            let source = edge.source_entity_id;
            let group = entity_groups
                .get(&source)
                .and_then(|groups| groups.first().copied())
                .or_else(|| {
                    data.entities
                        .iter()
                        .find(|pair| pair.entity_types().any(|t| t.base.id == source))
                        .and_then(|pair| entity_groups[&pair.entity.base.id].first().copied())
                })
                .unwrap_or(0);
            BTreeSet::from([group])
        };

        (0..data.documents.len())
            .map(|index| {
                let document = &data.documents[index];
                let chunks: Vec<DocumentChunk> = data
                    .chunks
                    .iter()
                    .filter(|chunk| chunk.document_id == document.base.id)
                    .cloned()
                    .collect();
                let ids: HashSet<Uuid> = chunks.iter().map(|chunk| chunk.base.id).collect();
                let edges_of = |edges: &[GraphEdgePair]| -> Vec<GraphEdgePair> {
                    edges
                        .iter()
                        .filter(|edge| edge_groups(edge).contains(&index))
                        .cloned()
                        .collect()
                };
                DocumentGroup {
                    documents: vec![document.clone()],
                    summaries: data
                        .summaries
                        .iter()
                        .filter(|summary| summary.made_from.is_some_and(|id| ids.contains(&id)))
                        .cloned()
                        .collect(),
                    entities: data
                        .entities
                        .iter()
                        .filter(|pair| entity_groups[&pair.entity.base.id].contains(&index))
                        .cloned()
                        .collect(),
                    timestamps: data
                        .timestamps
                        .iter()
                        .filter(|node| {
                            groups_of(data.producers.entity_chunks(node.timestamp.base.id))
                                .contains(&index)
                        })
                        .cloned()
                        .collect(),
                    edges: edges_of(&data.edges),
                    claimed_existing_edges: edges_of(&data.claimed_existing_edges),
                    dynamic_graphs: data
                        .dynamic_graphs
                        .iter()
                        .filter(|graph| graph.data_id == document.base.id)
                        .cloned()
                        .collect(),
                    web_pages: data
                        .handoff
                        .web_pages
                        .iter()
                        .filter(|page| page.data_id == document.base.id)
                        .cloned()
                        .collect(),
                    chunks,
                }
            })
            .collect()
    }

    pub(crate) struct Stores {
        pub(crate) graph: Arc<MockGraphDB>,
        pub(crate) vector: Arc<MockVectorDB>,
        pub(crate) engine: Arc<MockEmbeddingEngine>,
        pub(crate) db: DatabaseConnection,
    }

    impl Stores {
        pub(crate) async fn new() -> Self {
            let db = cognee_database::connect("sqlite::memory:").await.unwrap();
            cognee_database::initialize(&db).await.unwrap();
            cognee_database::ops::datasets::create_dataset(
                &db,
                cognee_models::Dataset::new("split".into(), USER, None, DATASET),
            )
            .await
            .unwrap();
            Self {
                graph: Arc::new(MockGraphDB::new()),
                vector: Arc::new(MockVectorDB::new()),
                engine: Arc::new(MockEmbeddingEngine::new(8)),
                db,
            }
        }

        /// Persist `groups` in order with one state, as `add_data_points`
        /// would with one group per document.
        async fn persist(
            &self,
            data: &SummarizedData,
            groups: &[PersistGroup<'_>],
            config: &CognifyConfig,
        ) -> PersistOutputs {
            let ledger = LedgerIdentity::new(None, data.user_id, DATASET, Some(Uuid::from_u128(7)));
            let graph_db: Arc<dyn GraphDBTrait> = self.graph.clone();
            let vector_db: Arc<dyn VectorDB> = self.vector.clone();
            let engine: Arc<dyn EmbeddingEngine> = self.engine.clone();
            let context = PersistContext {
                graph_db: &graph_db,
                vector_db: &vector_db,
                embedding_engine: &engine,
                db: &self.db,
                ledger,
                evidence: Some(ledger),
                user_id: data.user_id,
                config,
                run_chunk_documents: None,
            };
            let mut state = PersistState::default();
            for group in groups {
                persist_group(*group, &mut state, &context).await.unwrap();
            }
            state.into_outputs()
        }

        /// What the stores hold, as sets of canonical JSON text, with
        /// wall-clock values blanked.
        pub(crate) async fn snapshot(&self) -> BTreeMap<String, BTreeSet<String>> {
            fn scrub(value: &mut Value) {
                match value {
                    Value::Object(map) => {
                        for (key, entry) in map.iter_mut() {
                            if ["created_at", "updated_at", "captured_at"].contains(&key.as_str()) {
                                *entry = Value::Null;
                            } else {
                                scrub(entry);
                            }
                        }
                    }
                    Value::Array(items) => items.iter_mut().for_each(scrub),
                    Value::String(text) if text.starts_with('{') => {
                        if let Ok(mut parsed) = serde_json::from_str::<Value>(text) {
                            scrub(&mut parsed);
                            *value = parsed;
                        }
                    }
                    _ => {}
                }
            }
            fn canonical(mut value: Value) -> String {
                fn sorted(value: Value) -> Value {
                    match value {
                        Value::Object(map) => Value::Object(
                            map.into_iter()
                                .map(|(key, entry)| (key, sorted(entry)))
                                .collect::<BTreeMap<_, _>>()
                                .into_iter()
                                .collect(),
                        ),
                        Value::Array(items) => {
                            Value::Array(items.into_iter().map(sorted).collect())
                        }
                        other => other,
                    }
                }
                scrub(&mut value);
                sorted(value).to_string()
            }

            let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            let (nodes, edges) = self.graph.get_graph_data().await.unwrap();
            for (id, props) in nodes {
                let props: serde_json::Map<String, Value> = props
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v))
                    .collect();
                out.entry("graph_nodes".into())
                    .or_default()
                    .insert(canonical(json!({"id": id, "props": props})));
            }
            for (source, target, relationship, props) in edges {
                let props: serde_json::Map<String, Value> = props
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v))
                    .collect();
                out.entry("graph_edges".into())
                    .or_default()
                    .insert(canonical(json!([source, target, relationship, props])));
            }
            for (collection, points) in self.vector.all_points() {
                for point in points {
                    let payload: serde_json::Map<String, Value> =
                        point.metadata.into_iter().collect();
                    out.entry(format!("vectors {collection}"))
                        .or_default()
                        .insert(canonical(
                            json!({"id": point.id, "vector": point.vector, "payload": payload}),
                        ));
                }
            }
            for table in ["nodes", "edges", "provenance_edge_evidence"] {
                let rows = JsonValue::find_by_statement(Statement::from_string(
                    DatabaseBackend::Sqlite,
                    format!("SELECT * FROM {table}"),
                ))
                .all(&self.db)
                .await
                .unwrap();
                for row in rows {
                    out.entry(table.into()).or_default().insert(canonical(row));
                }
            }
            out
        }
    }

    /// Persist `data` once as one group and once as one group per document,
    /// and compare.
    async fn assert_split_matches_one_group(config: &CognifyConfig) {
        let data = run_data(config).await;
        let whole = PersistGroup {
            chunks: &data.chunks,
            documents: &data.documents,
            summaries: &data.summaries,
            entities: &data.entities,
            timestamps: &data.timestamps,
            edges: &data.edges,
            claimed_existing_edges: &data.claimed_existing_edges,
            producers: &data.producers,
            dynamic_graphs: &data.dynamic_graphs,
            web_pages: &data.handoff.web_pages,
            dlt: None,
        };
        let one = Stores::new().await;
        let one_outputs = one.persist(&data, &[whole], config).await;

        let documents = split_by_document(&data);
        assert_eq!(documents.len(), 3);
        let groups: Vec<PersistGroup<'_>> = documents
            .iter()
            .map(|group| PersistGroup {
                chunks: &group.chunks,
                documents: &group.documents,
                summaries: &group.summaries,
                entities: &group.entities,
                timestamps: &group.timestamps,
                edges: &group.edges,
                claimed_existing_edges: &group.claimed_existing_edges,
                producers: &data.producers,
                dynamic_graphs: &group.dynamic_graphs,
                web_pages: &group.web_pages,
                dlt: None,
            })
            .collect();
        let split = Stores::new().await;
        let split_outputs = split.persist(&data, &groups, config).await;

        let (one_sets, split_sets) = (one.snapshot().await, split.snapshot().await);
        assert!(one_sets.contains_key("graph_nodes") && one_sets.contains_key("nodes"));
        assert_eq!(
            one_sets.keys().collect::<Vec<_>>(),
            split_sets.keys().collect::<Vec<_>>(),
            "the same graph, vector collections, ledger tables and evidence"
        );
        for (section, want) in &one_sets {
            let got = &split_sets[section];
            let missing: Vec<_> = want.difference(got).collect();
            let extra: Vec<_> = got.difference(want).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{section} differs when split per document:\nmissing {missing:#?}\nextra {extra:#?}"
            );
        }

        // Written once: the mock graph appends edges and counts node writes,
        // the mock vector store counts point writes, the engine counts texts.
        assert_eq!(
            split.graph.node_write_count(),
            split.graph.node_count(),
            "every graph node written once across the groups"
        );
        // One group writes a key twice when it is both an extracted and a
        // structural edge — an ontology `is_a` the type walk rediscovers — as
        // the stages did before O4a. The split must add no repeat of its own.
        let repeated = |edges: Vec<EdgeData>| {
            let mut seen = HashSet::new();
            edges
                .into_iter()
                .filter(|(s, t, r, _)| !seen.insert((s.clone(), t.clone(), r.clone())))
                .map(|(s, t, r, _)| format!("{s} {r} {t}"))
                .collect::<BTreeSet<String>>()
        };
        assert_eq!(
            split.graph.edge_count(),
            one.graph.edge_count(),
            "no graph edge written more often than in one group"
        );
        assert_eq!(
            repeated(split.graph.get_graph_data().await.unwrap().1),
            repeated(one.graph.get_graph_data().await.unwrap().1),
        );
        let stored_points: usize = split.vector.all_points().values().map(Vec::len).sum();
        assert_eq!(
            split.vector.indexed_point_count(),
            stored_points,
            "every vector point written once across the groups"
        );
        assert_eq!(
            split.engine.embedded_text_count(),
            one.engine.embedded_text_count(),
            "no text embedded more often than in one group"
        );
        assert_eq!(split_outputs.embedding_count, one_outputs.embedding_count);
        assert_eq!(
            split_outputs.indexed_fields.field_counts,
            one_outputs.indexed_fields.field_counts
        );
        let edge_type_ids = |outputs: &PersistOutputs| -> BTreeSet<Uuid> {
            outputs.edge_types.iter().map(|e| e.base.id).collect()
        };
        assert_eq!(edge_type_ids(&split_outputs), edge_type_ids(&one_outputs));
        assert_eq!(split_outputs.edge_types.len(), one_outputs.edge_types.len());
    }

    /// The first (or only) group of a run is written through as borrowed:
    /// nothing was written before it, so nothing is filtered or copied. A
    /// later group is filtered against what the earlier ones wrote.
    #[test]
    fn the_first_group_writes_its_slices_through_uncopied() {
        let items = vec![1, 2, 3];
        let none_earlier = Written::default();
        let tracker = Tracker::new(&none_earlier);
        assert!(tracker.first_group());
        match fresh(&items, tracker.first_group(), |_| false) {
            Cow::Borrowed(slice) => assert!(std::ptr::eq(slice, items.as_slice())),
            Cow::Owned(_) => panic!("the first group must not copy its slices"),
        }

        let mut some_earlier = Written::default();
        some_earlier.graph_nodes.insert("x".to_string());
        let tracker = Tracker::new(&some_earlier);
        assert!(!tracker.first_group());
        let kept = fresh(&items, tracker.first_group(), |item| *item != 2);
        assert_eq!(kept.as_ref(), &[1, 3]);
    }

    #[tokio::test]
    async fn the_default_path_split_per_document_stores_the_same_sets() {
        let config = CognifyConfig::default().with_chunks_per_batch(1);
        assert_split_matches_one_group(&config).await;
    }

    #[tokio::test]
    async fn the_custom_schema_path_split_per_document_stores_the_same_sets() {
        let person = json!({
            "title": "Person",
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name"]
                }}
            },
            "required": ["name"]
        });
        let schema = json!({
            "$defs": {"Person": person},
            "title": "Company",
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "employees": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
                "metadata": {"type": "object", "default": {
                    "index_fields": ["name"], "identity_fields": ["name"]
                }}
            },
            "required": ["name", "employees"]
        });
        let config = CognifyConfig::default()
            .with_chunks_per_batch(1)
            .with_graph_schema(schema);
        assert_split_matches_one_group(&config).await;
    }

    /// Two spellings of one `EdgeType` id in two groups of a per-document
    /// run: the point ends up with the smaller spelling whichever group comes
    /// first, as one group keeps it, and `number_of_edges` counts both edges.
    #[tokio::test]
    async fn the_smaller_edge_type_spelling_wins_across_groups() {
        for order in [["works at", "Works At"], ["Works At", "works at"]] {
            let stores = Stores::new().await;
            let ledger = LedgerIdentity::new(None, Some(USER), DATASET, Some(Uuid::from_u128(7)));
            let graph_db: Arc<dyn GraphDBTrait> = stores.graph.clone();
            let vector_db: Arc<dyn VectorDB> = stores.vector.clone();
            let engine: Arc<dyn EmbeddingEngine> = stores.engine.clone();
            let config = CognifyConfig::default();
            let context = PersistContext {
                graph_db: &graph_db,
                vector_db: &vector_db,
                embedding_engine: &engine,
                db: &stores.db,
                ledger,
                evidence: None,
                user_id: Some(USER),
                config: &config,
                run_chunk_documents: None,
            };
            let mut state = PersistState::per_document();
            for (index, text) in order.iter().enumerate() {
                let edge = GraphEdgePair::with_properties(
                    Uuid::from_u128(100 + index as u128),
                    Uuid::from_u128(200 + index as u128),
                    "works_at",
                    HashMap::from([("edge_text".to_string(), text.to_string())]),
                );
                let edges = [edge];
                let group = PersistGroup {
                    edges: &edges,
                    ..PersistGroup::empty()
                };
                persist_group(group, &mut state, &context).await.unwrap();
            }
            let id = EdgeType::deterministic_id("works at");
            let point = stores.vector.all_points()["EdgeType_relationship_name"]
                .iter()
                .find(|point| point.id == id)
                .cloned()
                .expect("the edge type's point");
            assert_eq!(point.metadata["text"], json!("Works At"), "order {order:?}");
            let outputs = state.into_outputs();
            let edge_type = outputs
                .edge_types
                .iter()
                .find(|edge_type| edge_type.base.id == id)
                .expect("the edge type");
            assert_eq!(edge_type.relationship_name, "Works At");
            assert_eq!(edge_type.number_of_edges, 2);
        }
    }
}
