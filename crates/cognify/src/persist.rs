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
//! A [`PersistGroup`] is one batch of artifacts. Today the pipeline persists
//! the whole run as **one** group ([`crate::tasks::add_data_points`]); the
//! layer is shaped so a caller can build one group per document and call
//! [`persist_group`] once per group with one [`PersistState`]. The state
//! remembers what earlier groups wrote — ledger row ids, graph node ids, graph
//! edge keys, vector points, edge-evidence ids — so a node shared by several
//! groups is written and embedded once, and no ledger row is written twice.
//! Within a group nothing is filtered, so one group writes exactly what the
//! pre-O4a stages wrote.
//!
//! # Write order within a group
//!
//! 1. **Ledger rows, all of them, first** ("ledger before artifact", I1): the
//!    extraction rows (entities, Timestamps, semantic and claimed edges), the
//!    web-page rows, the custom-graph rows, the provenance rows (chunks,
//!    entities, semantic edges, summaries, entity types, documents, structural
//!    edges), then the DLT rows.
//! 2. **Graph writes, in the order the stages used to make them**: entity and
//!    Timestamp nodes, semantic edges; WebPage / WebSite nodes and the edges
//!    the graph does not hold yet; chunk, summary, entity-type and document
//!    nodes, custom nodes and edges, structural edges; DLT nodes and edges.
//!    The order is kept on purpose: a backend that drops an edge whose
//!    endpoint does not exist yet (Ladybug's `MATCH … MERGE`) stores the same
//!    edges it did before.
//! 3. **Vector points**: chunks, entities, entity types, summaries, edge
//!    types, documents, then custom nodes and DLT schema nodes.
//! 4. **Edge evidence**, only after the graph and vector writes succeeded.
//!
//! Any failure is returned at once and fails the run, as every persistence
//! failure did before.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};

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
    AttributedDynamicGraph, DltArtifacts, ExtractedGraphData, LedgerIdentity, ProvenanceStamp,
    WebPage,
};

/// One batch of artifacts [`persist_group`] writes.
///
/// Borrowed, so the pipeline's single group costs no copy; a caller that
/// builds one group per document owns that document's artifacts and lends
/// them. `producers` must name the producing chunks of `entities`, `edges`,
/// `timestamps` and `claimed_existing_edges`; `chunks` are what attributes
/// them to data items, so a group carries the chunks its artifacts came from.
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
    /// The provenance stamp the graph node of an entity carries when it
    /// differs from the entity's own (see
    /// [`crate::tasks::PersistHandoff`]); every other entity is written as is.
    pub(crate) entity_graph_stamps: &'a HashMap<Uuid, ProvenanceStamp>,
    pub(crate) dlt: Option<&'a DltArtifacts>,
}

static NO_PRODUCERS: LazyLock<ArtifactProducers> = LazyLock::new(ArtifactProducers::default);
static NO_STAMPS: LazyLock<HashMap<Uuid, ProvenanceStamp>> = LazyLock::new(HashMap::new);

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
            entity_graph_stamps: &NO_STAMPS,
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

/// What has been written, by identity.
#[derive(Debug, Default)]
struct Written {
    ledger_nodes: HashSet<Uuid>,
    ledger_edges: HashSet<Uuid>,
    graph_nodes: HashSet<String>,
    graph_edges: HashSet<(String, String, String)>,
    vectors: HashSet<(&'static str, Uuid)>,
    evidence: HashSet<Uuid>,
}

impl Written {
    fn absorb(&mut self, other: Written) {
        self.ledger_nodes.extend(other.ledger_nodes);
        self.ledger_edges.extend(other.ledger_edges);
        self.graph_nodes.extend(other.graph_nodes);
        self.graph_edges.extend(other.graph_edges);
        self.vectors.extend(other.vectors);
        self.evidence.extend(other.evidence);
    }
}

/// What the groups of one run have written so far, and the run-level outputs
/// they add up to.
///
/// A group filters out only what an **earlier** group wrote; what it writes
/// itself is folded in once it has been written in full, so one group is
/// never filtered against itself. A group that fails leaves the state as it
/// was before it.
#[derive(Debug, Default)]
pub(crate) struct PersistState {
    written: Written,
    edge_types: Vec<EdgeType>,
    embeddings: Vec<Embedding>,
    embedding_count: usize,
    indexed_fields: IndexedFieldsStats,
}

/// The run-level outputs of a [`PersistState`], for the `CognifyResult`.
pub(crate) struct PersistOutputs {
    /// Every `EdgeType` point embedded, in the order the groups embedded them.
    pub(crate) edge_types: Vec<EdgeType>,
    /// The vectors kept when `retain_embeddings` is on.
    pub(crate) embeddings: Vec<Embedding>,
    /// One per chunk, entity and summary, plus one per custom-node field.
    pub(crate) embedding_count: usize,
    pub(crate) indexed_fields: IndexedFieldsStats,
}

impl PersistState {
    pub(crate) fn into_outputs(self) -> PersistOutputs {
        PersistOutputs {
            edge_types: self.edge_types,
            embeddings: self.embeddings,
            embedding_count: self.embedding_count,
            indexed_fields: self.indexed_fields,
        }
    }
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
        !self
            .earlier
            .graph_edges
            .contains(&(edge.0.clone(), edge.1.clone(), edge.2.clone()))
    }

    fn new_point(&self, class: &'static str, id: Uuid) -> bool {
        !self.earlier.vectors.contains(&(class, id))
    }
}

/// Persist one group: ledger rows, then graph writes, then vector points, then
/// edge evidence (see the module docs for the exact order).
pub(crate) async fn persist_group(
    group: PersistGroup<'_>,
    state: &mut PersistState,
    context: &PersistContext<'_>,
) -> Result<(), CognifyError> {
    let mut tracker = Tracker::new(&state.written);

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
    );
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
    write_ledger(context.db, &mut tracker, nodes, edges).await?;
    let (nodes, edges) = crate::tasks::provenance_rows(
        context.ledger,
        group.chunks,
        group.entities,
        group.edges,
        group.summaries,
        group.documents,
        &structural_edges,
        group.producers,
    );
    let (node_rows, edge_rows) = write_ledger(context.db, &mut tracker, nodes, edges).await?;
    if node_rows > 0 {
        info!("Upserted {node_rows} provenance node records");
    }
    if edge_rows > 0 {
        info!("Upserted {edge_rows} provenance edge records");
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

    // ── 2. Graph, in the order the stages wrote it before O4a ───────────────
    let graph_db = context.graph_db.as_ref();
    write_extraction_graph(
        graph_db,
        &mut tracker,
        group.entities,
        group.entity_graph_stamps,
        group.timestamps,
        group.edges,
    )
    .await?;
    write_web_page_graph(graph_db, &mut tracker, group.web_pages).await?;

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
    write_edges(graph_db, &mut tracker, &dynamic_edges).await?;
    // The structural edges `get_graph_from_model` discovered above.
    let written = write_edges(graph_db, &mut tracker, &structural_edges).await?;
    if written > 0 {
        info!("Upserted {written} structural edges");
    }
    if let Some(dlt) = group.dlt {
        let written =
            write_raw_nodes(graph_db, &mut tracker, dlt.schema_nodes.iter().cloned()).await?;
        if written > 0 {
            info!("Added {written} DLT schema nodes to graph");
        }
        let written = write_edges(graph_db, &mut tracker, &dlt.edges).await?;
        if written > 0 {
            info!("Added {written} DLT FK edges to graph");
        }
    }

    // ── 3. Vector points ────────────────────────────────────────────────────
    let outputs = write_vectors(
        group,
        context,
        &mut tracker,
        &structural_edges,
        &dynamic_edges,
        &dynamic_nodes,
        &entity_types,
    )
    .await?;

    // ── 4. Edge evidence ────────────────────────────────────────────────────
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
    state.written.absorb(now);
    state.edge_types.extend(outputs.edge_types);
    state.embeddings.extend(outputs.embeddings);
    state.embedding_count += outputs.embedding_count;
    for (collection, count) in outputs.indexed_fields.field_counts {
        *state
            .indexed_fields
            .field_counts
            .entry(collection)
            .or_insert(0) += count;
    }
    Ok(())
}

/// The extraction part of the layer on its own, for
/// [`crate::tasks::extract_graph_from_data`] driven outside the pipeline: the
/// extraction ownership rows, then the entity and Timestamp nodes and the
/// semantic edges — exactly the steps [`persist_group`] takes for them.
pub(crate) async fn persist_extraction(
    db: &dyn RelationalDb,
    graph_db: &dyn GraphDBTrait,
    ledger: LedgerIdentity,
    graph: &ExtractedGraphData,
) -> Result<(), CognifyError> {
    let earlier = Written::default();
    let mut tracker = Tracker::new(&earlier);
    let (nodes, edges) = crate::tasks::extraction_ownership_rows(
        ledger,
        &graph.chunks,
        &graph.entities,
        &graph.edges,
        &graph.claimed_existing_edges,
        &graph.timestamps,
        &graph.producers,
    );
    let (node_rows, edge_rows) = write_ledger(db, &mut tracker, nodes, edges).await?;
    if node_rows + edge_rows > 0 {
        info!(
            "Recorded ownership of {node_rows} entities and Timestamps and {edge_rows} semantic \
             edges before writing them"
        );
    }
    write_extraction_graph(
        graph_db,
        &mut tracker,
        &graph.entities,
        &graph.handoff.entity_graph_stamps,
        &graph.timestamps,
        &graph.edges,
    )
    .await
}

/// The web-page part of the layer on its own, for
/// [`crate::tasks::create_web_page_nodes`] driven outside the pipeline: the
/// ownership rows, then the nodes and the edges the graph does not hold yet.
pub(crate) async fn persist_web_pages(
    db: &dyn RelationalDb,
    graph_db: &dyn GraphDBTrait,
    ledger: LedgerIdentity,
    pages: &[WebPage],
) -> Result<(), CognifyError> {
    let earlier = Written::default();
    let mut tracker = Tracker::new(&earlier);
    let (nodes, edges) = crate::tasks::web_page_ownership_rows(ledger, pages);
    write_ledger(db, &mut tracker, nodes, edges).await?;
    write_web_page_graph(graph_db, &mut tracker, pages).await
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

/// The entity and Timestamp nodes, then the semantic edges.
async fn write_extraction_graph(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    entities: &[GraphNodePair],
    entity_graph_stamps: &HashMap<Uuid, ProvenanceStamp>,
    timestamps: &[TimestampNode],
    edges: &[GraphEdgePair],
) -> Result<(), CognifyError> {
    // An entity whose graph node keeps an earlier stamp is written as a copy
    // carrying it; every other one is written as is.
    let restamped: Vec<cognee_models::Entity> = entities
        .iter()
        .filter_map(|pair| {
            let stamp = entity_graph_stamps.get(&pair.entity.base.id)?;
            let mut entity = pair.entity.clone();
            stamp.apply_to(&mut entity.base);
            Some(entity)
        })
        .collect();
    let restamped_by_id: HashMap<Uuid, &cognee_models::Entity> = restamped
        .iter()
        .map(|entity| (entity.base.id, entity))
        .collect();
    let entity_refs: Vec<&cognee_models::Entity> = entities
        .iter()
        .map(|pair| {
            restamped_by_id
                .get(&pair.entity.base.id)
                .copied()
                .unwrap_or(&pair.entity)
        })
        .collect();
    write_typed_nodes(graph_db, tracker, &entity_refs, |e| e.base.id).await?;
    // Timestamp nodes: not embedded, no EntityType; written before the edges
    // that point at them.
    let timestamp_refs: Vec<&cognee_models::Timestamp> =
        timestamps.iter().map(|node| &node.timestamp).collect();
    let written = write_typed_nodes(graph_db, tracker, &timestamp_refs, |t| t.base.id).await?;
    if written > 0 {
        info!("Upserted {written} Timestamp nodes");
    }
    let edge_data: Vec<EdgeData> = edges.iter().map(semantic_edge_data).collect();
    let written = write_edges(graph_db, tracker, &edge_data).await?;
    if written > 0 {
        info!("Upserted {written} extracted graph edges (LLM and ontology)");
    }
    Ok(())
}

/// The WebPage / WebSite nodes, then the edges the graph does not hold yet.
async fn write_web_page_graph(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    pages: &[WebPage],
) -> Result<(), CognifyError> {
    let (nodes, candidate_edges) = crate::tasks::web_page_graph(pages);
    write_raw_nodes(graph_db, tracker, nodes.into_iter()).await?;
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
    if fresh.is_empty() {
        return Ok(0);
    }
    graph_db
        .add_nodes(&fresh)
        .await
        .map_err(CognifyError::from)?;
    tracker
        .now
        .graph_nodes
        .extend(fresh.iter().map(|node| id(node).to_string()));
    Ok(fresh.len())
}

/// Write the raw JSON nodes no earlier group wrote; return how many.
async fn write_raw_nodes(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    nodes: impl Iterator<Item = serde_json::Value>,
) -> Result<usize, CognifyError> {
    let fresh: Vec<serde_json::Value> = nodes
        .filter(|node| {
            node.get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| tracker.new_node(id))
        })
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    let ids: Vec<String> = fresh
        .iter()
        .filter_map(|node| node.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();
    let count = fresh.len();
    graph_db
        .add_nodes_raw(fresh)
        .await
        .map_err(CognifyError::from)?;
    tracker.now.graph_nodes.extend(ids);
    Ok(count)
}

/// Write the edges no earlier group wrote; return how many.
async fn write_edges(
    graph_db: &dyn GraphDBTrait,
    tracker: &mut Tracker<'_>,
    edges: &[EdgeData],
) -> Result<usize, CognifyError> {
    let fresh: Vec<EdgeData> = edges
        .iter()
        .filter(|edge| tracker.new_edge(edge))
        .cloned()
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    graph_db
        .add_edges(&fresh)
        .await
        .map_err(CognifyError::from)?;
    tracker
        .now
        .graph_edges
        .extend(fresh.iter().map(|(source, target, relationship, _)| {
            (source.clone(), target.clone(), relationship.clone())
        }));
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

/// Embed and index the group's points that no earlier group embedded, and
/// return the group's share of the run-level outputs.
#[allow(clippy::too_many_arguments)]
async fn write_vectors(
    group: PersistGroup<'_>,
    context: &PersistContext<'_>,
    tracker: &mut Tracker<'_>,
    structural_edges: &[EdgeData],
    dynamic_edges: &[EdgeData],
    dynamic_nodes: &BTreeMap<Uuid, &DynamicDataPoint>,
    entity_types: &[&EntityType],
) -> Result<PersistOutputs, CognifyError> {
    let dataset_id = context.ledger.dataset_id;

    // Build EdgeTypes keyed on each edge's retrieval text (port of Python's
    // `create_edge_type_datapoints` + `index_graph_edges`). They are only
    // vector-indexed, never added to the graph: Python's `index_graph_edges`
    // touches the vector engine only (`index_graph_edges.py:86-88`), so the
    // graph node set matches Python's.
    //
    // Python keys EdgeType ids and the embedded `relationship_name` on the
    // edge's retrieval text — `get_edge_retrieval_text(edge_text,
    // relationship_name)` (`index_graph_edges.py:33-53`): the nonblank
    // `edge_text`, else the nonblank relationship name, else dropped. It counts
    // every edge `add_data_points` writes, structural and custom field edges
    // included (`add_data_points.py:346`), which is what lets a `"Document
    // chunk mentions …"` edge be query-ranked and surface as a fact.
    let structural_texts =
        structural_edges
            .iter()
            .chain(dynamic_edges)
            .map(|(_, _, relationship, props)| {
                EdgeType::retrieval_text(
                    props.get("edge_text").and_then(serde_json::Value::as_str),
                    relationship,
                )
            });
    let mut edge_types = crate::tasks::build_edge_types(
        group
            .edges
            .iter()
            .map(crate::tasks::edge_retrieval_text)
            .chain(structural_texts),
        dataset_id,
    );
    // Pre-stamp the `source_*` provenance keys before they are indexed. The
    // LLM-derived edge-type names trace back to the entity-extraction task, so
    // the pipeline / task literals match. `task_rank: None`: Python's
    // `create_edge_type_datapoints` (`index_graph_edges.py:50`) builds these
    // objects locally and never returns them from `add_data_points`, so no
    // stamper ever assigns them a rank; the vector payload carries none either
    // way (`cognee_models::index_schema_payload`).
    {
        let user_label = context.user_id.as_ref().map(|id| id.to_string());
        let mut local_visited: HashSet<Uuid> = HashSet::new();
        for edge_type in &mut edge_types {
            crate::graph_integration::expansion::pre_stamp_extraction(
                edge_type,
                user_label.as_deref(),
                None,
                &mut local_visited,
            );
        }
    }
    edge_types.retain(|edge_type| tracker.new_point(EDGE_TYPE_POINT, edge_type.base.id));

    let chunks: Vec<DocumentChunk> = group
        .chunks
        .iter()
        .filter(|chunk| tracker.new_point(CHUNK_POINT, chunk.base.id))
        .cloned()
        .collect();
    let entities: Vec<GraphNodePair> = group
        .entities
        .iter()
        .filter(|pair| tracker.new_point(ENTITY_POINT, pair.entity.base.id))
        .cloned()
        .collect();
    let summaries: Vec<TextSummary> = group
        .summaries
        .iter()
        .filter(|summary| tracker.new_point(SUMMARY_POINT, summary.base.id))
        .cloned()
        .collect();
    let documents: Vec<Document> = group
        .documents
        .iter()
        .filter(|document| tracker.new_point(DOCUMENT_POINT, document.base.id))
        .cloned()
        .collect();
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

    let mut indexed_fields = crate::tasks::index_data_points_except(
        &chunks,
        &entities,
        &summaries,
        &documents,
        &edge_types,
        dataset_id,
        Arc::clone(context.embedding_engine),
        Arc::clone(context.vector_db),
        &embeddings,
        &embedded_types,
    )
    .await?;
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
    let mut dlt_nodes: Vec<Uuid> = Vec::new();
    if let Some(dlt) = group.dlt {
        let fresh: Vec<serde_json::Value> = dlt
            .schema_nodes
            .iter()
            .filter(|node| schema_node_id(node).is_none_or(|id| tracker.new_point(DLT_POINT, id)))
            .cloned()
            .collect();
        crate::tasks::index_dlt_schema_nodes(
            &fresh,
            context.ledger,
            context.embedding_engine,
            context.vector_db,
        )
        .await?;
        dlt_nodes.extend(fresh.iter().filter_map(schema_node_id));
    }

    let vectors = &mut tracker.now.vectors;
    vectors.extend(chunks.iter().map(|c| (CHUNK_POINT, c.base.id)));
    vectors.extend(entities.iter().map(|e| (ENTITY_POINT, e.entity.base.id)));
    vectors.extend(new_types.into_iter().map(|id| (ENTITY_TYPE_POINT, id)));
    vectors.extend(summaries.iter().map(|s| (SUMMARY_POINT, s.base.id)));
    vectors.extend(edge_types.iter().map(|e| (EDGE_TYPE_POINT, e.base.id)));
    vectors.extend(documents.iter().map(|d| (DOCUMENT_POINT, d.base.id)));
    vectors.extend(dynamic.iter().map(|n| (DYNAMIC_POINT, n.base.id)));
    vectors.extend(dlt_nodes.into_iter().map(|id| (DLT_POINT, id)));

    Ok(PersistOutputs {
        edge_types,
        embeddings,
        embedding_count,
        indexed_fields,
    })
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
mod tests {
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

    const DATASET: Uuid = Uuid::from_u128(0x0a4a_5001);
    const USER: Uuid = Uuid::from_u128(0x0a4a_5002);
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
            let groups = groups_of(data.producers.edge_chunks(&edge.dedup_key()));
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

    struct Stores {
        graph: Arc<MockGraphDB>,
        vector: Arc<MockVectorDB>,
        engine: Arc<MockEmbeddingEngine>,
        db: DatabaseConnection,
    }

    impl Stores {
        async fn new() -> Self {
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
            };
            let mut state = PersistState::default();
            for group in groups {
                persist_group(*group, &mut state, &context).await.unwrap();
            }
            state.into_outputs()
        }

        /// What the stores hold, as sets of canonical JSON text, with
        /// wall-clock values blanked.
        async fn snapshot(&self) -> BTreeMap<String, BTreeSet<String>> {
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
            entity_graph_stamps: &data.handoff.entity_graph_stamps,
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
                entity_graph_stamps: &data.handoff.entity_graph_stamps,
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
}
