//! Entity neighborhood → edge-bullet rendering for the hybrid retriever.
//!
//! Port of `cognee/modules/retrieval/hybrid/entities.py`. Turns already-fetched
//! `Entity_name` vector hits plus their one-hop graph neighborhood into the
//! per-entity edge-bullet blocks the hybrid retriever renders into the LLM
//! prompt. Only [`build_entities`]'s `get_neighborhood` call is I/O; the
//! dedupe / sort / truncate / formatting logic is pure.
//!
//! # Known limitation — `belongs_to_set` is not inherited by entities
//!
//! Rust's `expand_with_nodes_and_edges` port constructs entities with
//! `Entity::new(&node.name, None, &node.name, Some(dataset_id))`
//! (`crates/cognify/src/graph_integration/expansion.rs:471`), which never
//! propagates the source chunk's NodeSet names into `belongs_to_set`; that
//! field only carries the dataset-UUID default from the `DataPoint` base
//! constructor (`crates/models/src/data_point.rs:135`). A dataset-UUID string
//! never matches a `node_name` filter of NodeSet names, so on this workspace a
//! `node_name`-scoped hybrid search's `Entity_name` query structurally returns
//! zero entity hits — a strictly narrower miss than Python, whose entities
//! inherit `data_chunk.belongs_to_set` and therefore survive a scoped search
//! (only facts are hidden). This is out of scope here and tracked as a
//! follow-up backfill task, not fixed in this module.

use std::collections::{HashMap, HashSet};

use cognee_graph::{EdgeData, GraphDBTrait, GraphNode};
use serde_json::{Map, Value};

use super::facts::{CONTAINS_FACT_PREFIX, EdgeLite, connection_edge_type_id};
use super::results::{
    display_value, first_display_value, payload, payload_matches_node_filter, result_id,
};
use crate::types::SearchItem;

/// A node in a rebuilt connection triple: a JSON object carrying at least an
/// `"id"` (Python's `nodes_by_id.get(id, {"id": id})` dict).
type NodeLite = Value;

/// A resolved entity plus its ranked edge bullets.
///
/// Port of the Python entity dict (`_entity_from_result`, `entities.py:165-177`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntityResult {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub entity_type: Option<String>,
    pub edges: Vec<EdgeBullet>,
    /// `EdgeType` ids of connections the block expresses without a bullet of
    /// their own — a chunk-mention edge repeating the description line. The
    /// facts lane excludes them like the bullets' ids
    /// ([`super::facts::select_facts_for_entities`]). Rust-only.
    pub covered_edge_type_ids: Vec<String>,
}

/// A single rendered edge bullet for an entity.
///
/// Port of the Python edge dict (`_edge_bullet`, `entities.py:262-281`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeBullet {
    pub text: String,
    pub source: Option<String>,
    pub target: Option<String>,
    pub source_id: Option<String>,
    pub relationship: Option<String>,
    pub target_id: Option<String>,
    pub edge_type_id: Option<String>,
}

/// Build the entity blocks for the given `Entity_name` hits.
///
/// Port of `build_entities` (`entities.py:43-81`). Returns `([], {})` for no
/// hits. Builds one entity per hit; if no hit has a nonempty id, returns the
/// entities unedited (no neighborhood call). Otherwise fetches the one-hop
/// neighborhood, drops neighbours outside the requested node set (see
/// [`keep_scoped_connections`]) and attaches ranked edge bullets.
///
/// The second value is the set of `EdgeType` row ids expressed by the kept
/// connections — taken before the per-entity `max_edges` cut — which is what a
/// node-scoped search may draw facts from
/// ([`super::facts::select_facts_for_entities`]).
///
/// **Fail-open:** a `get_neighborhood` error is logged and the entities are
/// returned with empty edges — the error is never propagated (mirrors Python's
/// `try/except` returning bare entities).
pub(crate) async fn build_entities(
    graph: &dyn GraphDBTrait,
    entity_hits: &[SearchItem],
    max_edges_per_entity: usize,
    edge_ranks: &HashMap<String, usize>,
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> (Vec<EntityResult>, HashSet<String>) {
    if entity_hits.is_empty() {
        return (vec![], HashSet::new());
    }

    let mut entities: Vec<EntityResult> = entity_hits.iter().map(entity_from_result).collect();
    let entity_ids: Vec<String> = entities
        .iter()
        .filter(|entity| !entity.id.is_empty())
        .map(|entity| entity.id.clone())
        .collect();
    if entity_ids.is_empty() {
        return (entities, HashSet::new());
    }

    let (nodes, edges) = match graph.get_neighborhood(&entity_ids, 1).await {
        Ok(neighborhood) => neighborhood,
        Err(error) => {
            tracing::warn!(
                %error,
                "Graph neighborhood retrieval failed; returning entities without edges"
            );
            return (entities, HashSet::new());
        }
    };

    let connections_by_entity_id = keep_scoped_connections(
        partition_neighborhood(&entity_ids, nodes, edges),
        node_name,
        node_name_filter_operator,
    );
    let reachable_edge_type_ids = reachable_edge_type_ids(&connections_by_entity_id);
    for entity in &mut entities {
        let connections = connections_by_entity_id
            .get(&entity.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        (entity.edges, entity.covered_edge_type_ids) = edge_bullets_from_connections(
            connections,
            max_edges_per_entity,
            edge_ranks,
            entity.description.as_deref(),
        );
    }
    (entities, reachable_edge_type_ids)
}

type Connections = HashMap<String, Vec<(NodeLite, EdgeLite, NodeLite)>>;

/// Drop 1-hop neighbours that are not in the requested node set.
///
/// Port of `_keep_scoped_connections` (`entities.py:112-131`). `is a` type
/// edges stay: EntityType nodes usually have no `belongs_to_set`. Unscoped
/// searches are unchanged.
fn keep_scoped_connections(
    connections_by_entity_id: Connections,
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Connections {
    let Some(names) = node_name.filter(|names| !names.is_empty()) else {
        return connections_by_entity_id;
    };
    connections_by_entity_id
        .into_iter()
        .map(|(entity_id, triples)| {
            let kept = triples
                .into_iter()
                .filter(|triple| {
                    connection_in_scope(&entity_id, triple, names, node_name_filter_operator)
                })
                .collect();
            (entity_id, kept)
        })
        .collect()
}

/// Port of `_connection_in_scope` (`entities.py:134-144`): a type edge is always
/// in scope; otherwise the neighbour — the endpoint that is not `entity_id` —
/// must pass the node filter.
fn connection_in_scope(
    entity_id: &str,
    (source, edge, target): &(NodeLite, EdgeLite, NodeLite),
    node_name: &[String],
    node_name_filter_operator: &str,
) -> bool {
    let relationship = edge.relationship_name.as_ref().and_then(display_value);
    if is_type_relationship(relationship.as_deref()) {
        return true;
    }
    let source_id = source.get("id").and_then(display_value);
    let neighbor = if source_id.as_deref() == Some(entity_id) {
        target
    } else {
        source
    };
    payload_matches_node_filter(neighbor, Some(node_name), node_name_filter_operator)
}

/// Port of `_reachable_edge_type_ids` (`entities.py:147-155`).
fn reachable_edge_type_ids(connections_by_entity_id: &Connections) -> HashSet<String> {
    connections_by_entity_id
        .values()
        .flatten()
        .filter_map(|(_, edge, _)| connection_edge_type_id(edge))
        .collect()
}

/// Resolve a single entity hit into an [`EntityResult`] with empty edges.
///
/// Port of `_entity_from_result` (`entities.py:165-177`). `name` falls back
/// `name` → `text` → `id`, never empty. `text` is where the indexer puts the
/// entity's name, as Python's `IndexSchema` does; a row written before the
/// indexer carried it resolves to its id. `description` is read as Python
/// reads it, but only Rust's indexer writes it (Python's `IndexSchema` row has
/// no such key), so on a Python-written store it is always empty.
fn entity_from_result(item: &SearchItem) -> EntityResult {
    let result_payload = payload(item);
    let entity_id = result_id(item).unwrap_or_default();

    let id_value = Value::String(entity_id.clone());
    let mut name_candidates: Vec<&Value> = Vec::new();
    if let Some(value) = result_payload.get("name") {
        name_candidates.push(value);
    }
    if let Some(value) = result_payload.get("text") {
        name_candidates.push(value);
    }
    name_candidates.push(&id_value);
    let name = first_display_value(&name_candidates).unwrap_or_default();

    EntityResult {
        id: entity_id,
        name,
        description: result_payload.get("description").and_then(display_value),
        entity_type: entity_type(result_payload),
        edges: vec![],
        covered_edge_type_ids: vec![],
    }
}

/// Resolve an entity's domain type from a payload/entity object.
///
/// Port of `_entity_type` (`entities.py:201-206`): the first nonblank
/// `display_value` of `is_a` then `type`, suppressing the payload's structural
/// type; else `None`.
///
/// Python suppresses `"IndexSchema"`, the class every one of its vector rows
/// is written as. A Rust row's `type` is the indexed DataPoint's class instead
/// — `"Entity"` on `Entity_name` — so that is suppressed too. Neither side's
/// payload carries a domain type under these keys, so the header is
/// `### {name}` on both.
fn entity_type(result_payload: &Value) -> Option<String> {
    for key in ["is_a", "type"] {
        if let Some(value) = result_payload.get(key)
            && let Some(entity_type) = display_value(value)
            && !is_structural_type(&entity_type)
        {
            return Some(entity_type);
        }
    }
    None
}

/// Whether `value` names the vector row's schema class rather than a domain
/// type — see [`entity_type`].
fn is_structural_type(value: &str) -> bool {
    matches!(value, "IndexSchema" | "Entity")
}

/// Rebuild per-entity `(source, edge, target)` connection triples from the flat
/// one-hop subgraph.
///
/// Port of `_partition_neighborhood` (`entities.py:80-105`). Each seed id gets an
/// (initially empty) connection list. A triple is pushed onto its `source_id`'s
/// list (when that id is a seed) and onto its `target_id`'s list (when that id
/// is a seed **and** differs from `source_id`, deduping self-loops). Edges with
/// neither endpoint among the seeds (neighbor-to-neighbor) are silently dropped.
fn partition_neighborhood(
    entity_ids: &[String],
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
) -> HashMap<String, Vec<(NodeLite, EdgeLite, NodeLite)>> {
    let mut nodes_by_id: HashMap<String, Value> = HashMap::new();
    for (id, data) in nodes {
        let mut object = Map::new();
        object.insert("id".to_string(), Value::String(id.clone()));
        for (key, value) in data {
            object.insert(key.into_owned(), value);
        }
        nodes_by_id.insert(id, Value::Object(object));
    }

    let mut connections: HashMap<String, Vec<(NodeLite, EdgeLite, NodeLite)>> = entity_ids
        .iter()
        .map(|id| (id.clone(), Vec::new()))
        .collect();

    for (source_id, target_id, relationship_name, properties) in edges {
        let source = nodes_by_id
            .get(&source_id)
            .cloned()
            .unwrap_or_else(|| Value::Object(id_only_object(&source_id)));
        let target = nodes_by_id
            .get(&target_id)
            .cloned()
            .unwrap_or_else(|| Value::Object(id_only_object(&target_id)));

        let mut property_object = Map::new();
        for (key, value) in properties {
            property_object.insert(key.into_owned(), value);
        }
        let edge = EdgeLite {
            relationship_name: Some(Value::String(relationship_name)),
            edge_text: None,
            properties: Some(Value::Object(property_object)),
        };
        let triple = (source, edge, target);

        let push_source = connections.contains_key(&source_id);
        let push_target = connections.contains_key(&target_id) && target_id != source_id;
        match (push_source, push_target) {
            (true, true) => {
                if let Some(list) = connections.get_mut(&source_id) {
                    list.push(triple.clone());
                }
                if let Some(list) = connections.get_mut(&target_id) {
                    list.push(triple);
                }
            }
            (true, false) => {
                if let Some(list) = connections.get_mut(&source_id) {
                    list.push(triple);
                }
            }
            (false, true) => {
                if let Some(list) = connections.get_mut(&target_id) {
                    list.push(triple);
                }
            }
            (false, false) => {}
        }
    }
    connections
}

/// A `{"id": id}` JSON object for a node absent from the neighborhood.
fn id_only_object(id: &str) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(id.to_string()));
    object
}

/// Build ranked, deduped, capped edge bullets for one entity's connections.
///
/// Port of `_edge_bullets_from_connections` (`entities.py:209-240`). `max_edges
/// == 0` yields `[]`. Empty-text bullets are skipped. Dedupe runs on two
/// **independent** tracks: a keyed `(source_id, relationship, target_id)` set
/// and a text-only set — a keyed bullet is never checked against the text set
/// or vice versa. A **stable** sort by [`edge_sort_key`] preserves connection
/// order among equal keys, then the list is truncated to `max_edges`.
///
/// Diverging from Python, chunk-mention bullets (see [`is_chunk_mention`]) are
/// thinned before the cut: one repeating `description` — the line the block
/// already prints under its header — is dropped, its `EdgeType` id returned
/// as the second value so the facts lane does not print it a third time; and
/// of several with the same text, one is kept. Rust cognify gives every
/// chunk's edge to an entity the same text, so without this a much-mentioned
/// entity's block is the same sentence once per chunk, filling the cut.
fn edge_bullets_from_connections(
    connections: &[(NodeLite, EdgeLite, NodeLite)],
    max_edges: usize,
    edge_ranks: &HashMap<String, usize>,
    description: Option<&str>,
) -> (Vec<EdgeBullet>, Vec<String>) {
    if max_edges == 0 {
        return (vec![], vec![]);
    }

    let mut edges: Vec<EdgeBullet> = Vec::new();
    let mut covered_edge_type_ids: Vec<String> = Vec::new();
    let mut seen_keys: HashSet<(String, String, String)> = HashSet::new();
    let mut seen_texts: HashSet<String> = HashSet::new();
    let mut seen_mentions: HashSet<String> = HashSet::new();

    for (source, edge, target) in connections {
        let Some(bullet) = edge_bullet(source, edge, target) else {
            continue;
        };
        if is_chunk_mention(&bullet) {
            if description.is_some_and(|description| mention_repeats(&bullet.text, description)) {
                covered_edge_type_ids.extend(bullet.edge_type_id.clone());
                continue;
            }
            if !seen_mentions.insert(bullet.text.clone()) {
                continue;
            }
        }

        let dedupe_key = edge_dedupe_key(&bullet);
        match &dedupe_key {
            Some(key) if seen_keys.contains(key) => continue,
            None if seen_texts.contains(&bullet.text) => continue,
            _ => {}
        }
        match dedupe_key {
            Some(key) => {
                seen_keys.insert(key);
            }
            None => {
                seen_texts.insert(bullet.text.clone());
            }
        }
        edges.push(bullet);
    }

    edges.sort_by_key(|edge| edge_sort_key(edge, edge_ranks));
    edges.truncate(max_edges);
    (edges, covered_edge_type_ids)
}

/// Whether a bullet is a chunk→entity `contains` edge carrying the
/// `"Document chunk mentions {name}: {description}"` text.
fn is_chunk_mention(bullet: &EdgeBullet) -> bool {
    bullet
        .relationship
        .as_deref()
        .is_some_and(|relationship| relationship.trim().eq_ignore_ascii_case("contains"))
        && bullet.text.starts_with(CONTAINS_FACT_PREFIX)
}

/// Whether a chunk-mention text's description part is `description`.
fn mention_repeats(text: &str, description: &str) -> bool {
    let description = description.trim();
    !description.is_empty()
        && text
            .trim_end()
            .strip_suffix(description)
            .is_some_and(|head| head.ends_with(": "))
}

/// Sort key: type edges first, then query-ranked edges, then legacy order.
///
/// Port of `_edge_sort_key` (`entities.py:243-250`): `(0, 0)` for a type edge;
/// `(1, rank)` when the edge's `edge_type_id` is in `edge_ranks`; else `(2, 0)`.
fn edge_sort_key(edge: &EdgeBullet, edge_ranks: &HashMap<String, usize>) -> (u8, usize) {
    if is_type_edge(edge) {
        return (0, 0);
    }
    match edge.edge_type_id.as_ref().and_then(|id| edge_ranks.get(id)) {
        Some(rank) => (1, *rank),
        None => (2, 0),
    }
}

/// Render a single connection triple into an [`EdgeBullet`], or `None` to drop.
///
/// Port of `_edge_bullet` (`entities.py:262-281`). Text prefers the top-level
/// `edge_text` (absent from graph triples in practice, kept for fidelity),
/// then the nested `properties.edge_text`. Both SDKs' cognify now stamp an
/// `edge_text` on every edge they write (`ensure_default_edge_properties`,
/// `crates/cognify/src/graph_extraction/edge_text.rs`), so what follows only
/// runs for edges written without one — rows from before that stamping, or
/// from a writer that skips it. For those this diverges deliberately from
/// Python, in rendering only, not in the byte-identical prompt *template*:
///
/// 1. A structural chunk→entity `contains` edge whose source carries no name
///    is dropped outright — see [`is_unnamed_contains_edge`].
/// 2. The synthesized fallback reads as a sentence, e.g. `"Alice is a
///    person."`, via [`render_edge_sentence`], instead of Python's
///    `"{source} -- {relationship} -- {target}"` triple — the same shape the
///    write-time stamp gives the edges that have text.
///
/// If nothing is renderable the bullet is dropped. `edge_type_id` is
/// recomputed via [`connection_edge_type_id`] (edge-text-first), never from
/// the raw relationship name.
fn edge_bullet(source: &NodeLite, edge: &EdgeLite, target: &NodeLite) -> Option<EdgeBullet> {
    let relationship = edge.relationship_name.as_ref().and_then(display_value);
    let source_label = node_label(source);
    let target_label = node_label(target);

    let mut text = edge
        .edge_text
        .as_ref()
        .and_then(display_value)
        .or_else(|| nested_edge_text(edge));
    if text.is_none() && is_unnamed_contains_edge(source, relationship.as_deref()) {
        return None;
    }
    if text.is_none()
        && let (Some(source_label), Some(relationship), Some(target_label)) =
            (&source_label, &relationship, &target_label)
    {
        text = Some(render_edge_sentence(
            source_label,
            relationship,
            target_label,
        ));
    }
    let text = text?;

    Some(EdgeBullet {
        text,
        source: source_label,
        target: target_label,
        source_id: source.get("id").and_then(display_value),
        relationship,
        target_id: target.get("id").and_then(display_value),
        edge_type_id: connection_edge_type_id(edge),
    })
}

/// Whether `(source, relationship)` is a structural chunk→entity `contains`
/// edge whose source carries no name. [`edge_bullet`] drops such an edge
/// only when it also has no `edge_text` — noise for the prompt.
///
/// `DocumentChunk::relationships()` (`crates/cognify/src/graph_extraction/extractable.rs`)
/// emits a `contains` edge from every chunk to every entity it mentions.
/// `DocumentChunk` has no `name` field (`crates/models/src/document_chunk.rs`),
/// so [`node_label`] falls back to the chunk's id. Before cognify stamped
/// `edge_text` on these edges, the synthesized bullet asserted that one UUID
/// "contains" an entity: `ecb1240e-1b45-54cf-bdca-0cfca21971e9 contains
/// Alice.` Relabelling the chunk would not rescue it either — a chunk preview
/// followed by "contains Alice" is not a fact the model can answer from — so
/// the bullet is dropped.
///
/// An edge that *does* carry `edge_text` is kept. Python writes
/// `"Document chunk mentions {name}: {description}"` there, and so does Rust
/// cognify now; for a Python-written store (the cross-read case) that line
/// may be the only one carrying the entity's description.
///
/// cognee-rs#245 first made [`node_label`] refuse any bare-UUID label, then
/// deliberately removed that guard before merging, keeping UUID-labelled
/// bullets on the grounds that Python rendered them the same way. It does
/// not: Python stamps an `edge_text` on every edge at write time, so its
/// `contains` bullets carry a sentence, never a chunk's UUID.
fn is_unnamed_contains_edge(source: &NodeLite, relationship: Option<&str>) -> bool {
    let is_contains = relationship
        .is_some_and(|relationship| relationship.trim().eq_ignore_ascii_case("contains"));
    is_contains && source.get("name").and_then(display_value).is_none()
}

/// Render a synthesized `(source, relationship, target)` triple as a
/// period-terminated sentence, e.g. `"Alice is a person."`.
///
/// The read-time counterpart of the sentence cognify stamps as `edge_text`
/// at write time (Python `_build_fallback_edge_text`,
/// `prepare_edges_for_storage.py:85-98`), used for edges written without
/// one — prose is easier for a small on-device model to read than the
/// `"{source} -- {relationship} -- {target}"` triple Python renders here.
/// The relationship label is normalized the same way [`is_type_relationship`]
/// does (lowercase, `_`/`-` → space, whitespace-collapsed); Python's
/// write-time sentence only maps `_` and keeps case, which is identical for
/// cognify's snake_case names. Labels are rendered as-is.
fn render_edge_sentence(source: &str, relationship: &str, target: &str) -> String {
    let relationship_words = relationship
        .to_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{source} {relationship_words} {target}.")
}

/// The dedupe key for a bullet, or `None` when any component is blank.
///
/// Port of `_edge_dedupe_key` (`entities.py:284-290`).
fn edge_dedupe_key(edge: &EdgeBullet) -> Option<(String, String, String)> {
    match (&edge.source_id, &edge.relationship, &edge.target_id) {
        (Some(source_id), Some(relationship), Some(target_id)) => {
            Some((source_id.clone(), relationship.clone(), target_id.clone()))
        }
        _ => None,
    }
}

/// Whether a bullet is an `is a` / type edge.
///
/// Port of `_is_type_edge` (`entities.py:300-304`), minus its text fallback:
/// only the relationship decides, via [`is_type_relationship`] — the same
/// test [`connection_in_scope`] applies. Python also pins any bullet whose
/// text contains `" is a "`. Every real type edge is already caught by its
/// `is_a` relationship (`Entity::relationships`, the ontology expansion), so
/// the text test only ever adds false positives: `is_a_member_of` edges,
/// whose stamped sentence reads "Alice is a member of Club.", or
/// LLM-written text such as "Acme is a subsidiary of Globex." on a
/// `subsidiary_of` edge. Pinned ahead of the query-ranked bullets, those
/// crowd them out of the `max_edges` cut on exactly the hub entities most
/// often retrieved.
fn is_type_edge(edge: &EdgeBullet) -> bool {
    is_type_relationship(edge.relationship.as_deref())
}

/// Whether a relationship label names an `is a` / type edge.
///
/// Port of `_is_type_relationship` (`entities.py:297-301`): lowercase, `_`/`-`
/// → space, trimmed, compared to `"is a"`.
fn is_type_relationship(relationship: Option<&str>) -> bool {
    relationship.is_some_and(|relationship| {
        relationship.to_lowercase().replace(['_', '-'], " ").trim() == "is a"
    })
}

/// The nested `properties.edge_text` of an edge, or `None`.
///
/// Port of `_nested_edge_text` (`entities.py:307-311`).
fn nested_edge_text(edge: &EdgeLite) -> Option<String> {
    edge.properties
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|map| map.get("edge_text"))
        .and_then(display_value)
}

/// A node's display label: its `name`, then its `id`.
///
/// Port of `_node_label` (`entities.py:314-315`).
fn node_label(node: &NodeLite) -> Option<String> {
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(value) = node.get("name") {
        candidates.push(value);
    }
    if let Some(value) = node.get("id") {
        candidates.push(value);
    }
    first_display_value(&candidates)
}

/// Render the entity blocks as the "Relevant entities" markdown section.
///
/// Port of `format_entities` (`entities.py:158-166`). Empty if no entity yields a
/// nonempty block; otherwise a `"## Relevant entities"` header followed by the
/// blocks joined by a blank line.
pub(crate) fn format_entities(entities: &[EntityResult]) -> String {
    let blocks: Vec<String> = entities
        .iter()
        .map(format_entity)
        .filter(|block| !block.is_empty())
        .collect();
    if blocks.is_empty() {
        return String::new();
    }
    format!("## Relevant entities\n{}", blocks.join("\n\n"))
}

/// Render a single entity block, or `""` when its name is blank.
///
/// Port of `_format_entity` (`entities.py:184-202`). Header is
/// `"### {name} ({type})"` or `"### {name}"` (a structural type, see [`is_structural_type`],
/// is suppressed), followed by the description line if present and one
/// `"- {text}"` per edge with nonblank text.
pub(crate) fn format_entity(entity: &EntityResult) -> String {
    let name = entity.name.trim();
    if name.is_empty() {
        return String::new();
    }

    let entity_type = entity
        .entity_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !is_structural_type(value));
    let header = match entity_type {
        Some(entity_type) => format!("### {name} ({entity_type})"),
        None => format!("### {name}"),
    };

    let mut lines = vec![header];
    if let Some(description) = entity.description.as_deref().map(str::trim)
        && !description.is_empty()
    {
        lines.push(description.to_string());
    }
    for edge in &entity.edges {
        let edge_text = edge.text.trim();
        if !edge_text.is_empty() {
            lines.push(format!("- {edge_text}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;

    use async_trait::async_trait;
    use cognee_graph::{EdgeData, GraphDBError, GraphDBResult, GraphNode, MockGraphDB, NodeData};
    use serde_json::json;

    use super::super::facts::edge_rank_by_id;
    use super::*;

    fn entity_hit(payload: Value) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    /// An `Entity_name` row as the Rust indexer writes it: the base
    /// DataPoint's keys (structural `"type": "Entity"`, `metadata`), plus
    /// `text` holding the entity's name and the indexer's own `entity_type`.
    fn rust_entity_hit() -> SearchItem {
        SearchItem {
            id: "53105ee7-4467-5f5e-a959-5632b495e0c6".parse().ok(),
            score: None,
            payload: json!({
                "id": "53105ee7-4467-5f5e-a959-5632b495e0c6",
                "type": "Entity",
                "field": "name",
                "text": "Alice",
                "entity_type": "person",
                "metadata": { "index_fields": ["name"], "original_node_id": "person:alice" },
            }),
        }
    }

    /// Renders as Python renders its `IndexSchema` row for the same entity:
    /// the name from `text`, and no type, since neither payload holds a domain
    /// type under `is_a`/`type`.
    #[test]
    fn a_rust_written_row_renders_as_python_renders_it() {
        let entity = entity_from_result(&rust_entity_hit());
        assert_eq!(entity.name, "Alice");
        assert_eq!(entity.entity_type, None);
        assert_eq!(format_entity(&entity), "### Alice");
    }

    #[test]
    fn an_entity_with_nothing_usable_still_renders_as_its_id() {
        let entity = entity_from_result(&entity_hit(json!({
            "id": "53105ee7-4467-5f5e-a959-5632b495e0c6",
            "type": "Entity",
        })));
        assert_eq!(entity.name, "53105ee7-4467-5f5e-a959-5632b495e0c6");
        assert_eq!(entity.entity_type, None);
    }

    fn edge_hit(text: &str) -> SearchItem {
        SearchItem {
            id: Some(cognee_models::EdgeType::deterministic_id(text)),
            score: None,
            payload: json!({ "text": text }),
        }
    }

    /// Insert a `{id, name}` node into a MockGraphDB.
    async fn add_node(graph: &MockGraphDB, id: &str, name: &str) {
        graph
            .add_node_raw(json!({ "id": id, "name": name }))
            .await
            .unwrap();
    }

    /// Insert a directed edge (with optional `edge_text` property).
    async fn add_edge(
        graph: &MockGraphDB,
        src: &str,
        tgt: &str,
        rel: &str,
        edge_text: Option<&str>,
    ) {
        let mut props: HashMap<Cow<'static, str>, Value> = HashMap::new();
        if let Some(text) = edge_text {
            props.insert(Cow::from("edge_text"), json!(text));
        }
        graph.add_edge(src, tgt, rel, Some(props)).await.unwrap();
    }

    /// Build a bare [`EdgeBullet`] carrying only a relationship + text, for
    /// exercising [`is_type_edge`] in isolation.
    fn type_edge_bullet(relationship: Option<&str>, text: &str) -> EdgeBullet {
        EdgeBullet {
            text: text.to_string(),
            source: None,
            target: None,
            source_id: None,
            relationship: relationship.map(str::to_string),
            target_id: None,
            edge_type_id: None,
        }
    }

    /// A graph double whose `get_neighborhood` always errors (fail-open test).
    struct FailingGraphDB {
        inner: MockGraphDB,
    }

    impl FailingGraphDB {
        fn new() -> Self {
            Self {
                inner: MockGraphDB::new(),
            }
        }
    }

    #[async_trait]
    impl GraphDBTrait for FailingGraphDB {
        async fn get_neighborhood(
            &self,
            _node_ids: &[String],
            _depth: usize,
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
        async fn get_filtered_graph_data(
            &self,
            filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            self.inner.get_filtered_graph_data(filters).await
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
    }

    /// A graph double whose `get_neighborhood` panics if ever awaited — used to
    /// prove `build_entities` never touches the graph when no hit has an id.
    struct PanicOnNeighborhoodGraphDB {
        inner: MockGraphDB,
    }

    impl PanicOnNeighborhoodGraphDB {
        fn new() -> Self {
            Self {
                inner: MockGraphDB::new(),
            }
        }
    }

    #[async_trait]
    impl GraphDBTrait for PanicOnNeighborhoodGraphDB {
        async fn get_neighborhood(
            &self,
            _node_ids: &[String],
            _depth: usize,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            panic!("get_neighborhood must not be called when no entity has an id");
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
        async fn get_filtered_graph_data(
            &self,
            filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            self.inner.get_filtered_graph_data(filters).await
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
    }

    #[test]
    fn entity_name_falls_back_from_name_to_text_to_id() {
        let named = entity_from_result(&entity_hit(json!({"id": "entity-1", "name": "Named"})));
        assert_eq!(named.name, "Named");

        let texted = entity_from_result(&entity_hit(json!({"id": "entity-1", "text": "Texted"})));
        assert_eq!(texted.name, "Texted");

        let id_only = entity_from_result(&entity_hit(json!({"id": "entity-1"})));
        assert_eq!(id_only.name, "entity-1");
    }

    #[test]
    fn entity_type_prefers_is_a_and_suppresses_index_schema() {
        let payload = json!({"id": "e", "type": "IndexSchema", "is_a": "Office"});
        assert_eq!(entity_type(&payload), Some("Office".to_string()));

        let only_index = json!({"id": "e", "type": "IndexSchema"});
        assert_eq!(entity_type(&only_index), None);

        let domain = json!({"id": "e", "type": "Office"});
        assert_eq!(entity_type(&domain), Some("Office".to_string()));

        let only_entity = json!({"id": "e", "type": "Entity"});
        assert_eq!(entity_type(&only_entity), None);
    }

    #[tokio::test]
    async fn edge_text_explicit_and_synthesized_fallback() {
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Source").await;
        add_node(&graph, "target-1", "Target").await;
        add_edge(&graph, "entity-1", "target-1", "REL", Some("Edge text")).await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].edges[0].text, "Edge text");

        let graph2 = MockGraphDB::new();
        add_node(&graph2, "entity-1", "Source").await;
        add_node(&graph2, "target-1", "Target").await;
        add_edge(&graph2, "entity-1", "target-1", "REL", None).await;
        let (entities, _) = build_entities(&graph2, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].edges[0].text, "Source rel Target.");
    }

    #[tokio::test]
    async fn duplicate_edges_removed_and_max_edges_caps_results() {
        // Blank relationship_name -> text-only dedupe track. Two "same" collapse,
        // "other" survives; cap of 1 keeps a single bullet.
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Entity").await;
        add_node(&graph, "t1", "T1").await;
        add_node(&graph, "t2", "T2").await;
        add_node(&graph, "t3", "T3").await;
        add_edge(&graph, "entity-1", "t1", "", Some("same")).await;
        add_edge(&graph, "entity-1", "t2", "", Some("same")).await;
        add_edge(&graph, "entity-1", "t3", "", Some("other")).await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity"}))];
        let (entities, _) = build_entities(&graph, &hits, 1, &HashMap::new(), None, "OR").await;
        let texts: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["same"]);
    }

    #[tokio::test]
    async fn same_edge_text_does_not_collapse_distinct_relationships() {
        // Distinct target_ids -> distinct dedupe keys -> both survive; the literal
        // duplicate triple (entity-1, REL, t1) collapses.
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Entity").await;
        add_node(&graph, "t1", "T1").await;
        add_node(&graph, "t2", "T2").await;
        add_edge(&graph, "entity-1", "t1", "REL", Some("related")).await;
        add_edge(&graph, "entity-1", "t2", "REL", Some("related")).await;
        add_edge(&graph, "entity-1", "t1", "REL", Some("related")).await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        let target_ids: Vec<&str> = entities[0]
            .edges
            .iter()
            .filter_map(|e| e.target_id.as_deref())
            .collect();
        assert_eq!(target_ids, ["t1", "t2"]);
    }

    #[tokio::test]
    async fn is_a_edge_is_prioritized_before_edge_cap() {
        // Unranked is_a edge beats a ranked-looking owns edge for the single slot.
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Lisbon office").await;
        add_node(&graph, "project-1", "HarborLens").await;
        add_node(&graph, "type-1", "Office").await;
        add_edge(
            &graph,
            "entity-1",
            "project-1",
            "owns",
            Some("Lisbon office owns HarborLens"),
        )
        .await;
        add_edge(
            &graph,
            "entity-1",
            "type-1",
            "is_a",
            Some("Lisbon office is a Office"),
        )
        .await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity"}))];
        let (entities, _) = build_entities(&graph, &hits, 1, &HashMap::new(), None, "OR").await;
        let texts: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["Lisbon office is a Office"]);
    }

    #[tokio::test]
    async fn edge_hits_rank_entity_bullets_and_fill_facts_section() {
        let ranked_bullet = "Alice works at Acme.";
        let unranked_bullet = "Alice plays tennis.";
        let fact = "Acme acquired Initech.";
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Alice").await;
        add_node(&graph, "tennis-id", "Tennis").await;
        add_node(&graph, "acme-id", "Acme").await;
        add_node(&graph, "person-id", "Person").await;
        add_edge(
            &graph,
            "entity-1",
            "tennis-id",
            "plays",
            Some(unranked_bullet),
        )
        .await;
        add_edge(
            &graph,
            "entity-1",
            "acme-id",
            "works_at",
            Some(ranked_bullet),
        )
        .await;
        add_edge(&graph, "entity-1", "person-id", "is_a", None).await;

        let edge_hits = vec![
            edge_hit(fact),
            edge_hit(ranked_bullet),
            edge_hit("works at"),
        ];
        let edge_ranks = edge_rank_by_id(&edge_hits);
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &edge_ranks, None, "OR").await;

        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            bullets,
            ["Alice is a Person.", ranked_bullet, unranked_bullet]
        );
    }

    #[tokio::test]
    async fn a_scoped_search_drops_out_of_set_neighbours_but_keeps_type_edges() {
        // Python `_keep_scoped_connections` (entities.py:112-144).
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        graph
            .add_node_raw(json!({"id": "acme-id", "name": "Acme", "belongs_to_set": ["keep"]}))
            .await
            .unwrap();
        graph
            .add_node_raw(
                json!({"id": "umbrella-id", "name": "Umbrella", "belongs_to_set": ["drop"]}),
            )
            .await
            .unwrap();
        add_node(&graph, "person-id", "Person").await;
        add_edge(
            &graph,
            "alice-id",
            "acme-id",
            "works_at",
            Some("Alice works at Acme."),
        )
        .await;
        add_edge(
            &graph,
            "alice-id",
            "umbrella-id",
            "works_at",
            Some("Alice works at Umbrella."),
        )
        .await;
        add_edge(&graph, "alice-id", "person-id", "is_a", None).await;
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];
        let keep = vec!["keep".to_string()];

        let (entities, reachable) =
            build_entities(&graph, &hits, 5, &HashMap::new(), Some(&keep), "OR").await;
        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert!(bullets.contains(&"Alice works at Acme."), "{bullets:?}");
        assert!(bullets.contains(&"Alice is a Person."), "{bullets:?}");
        assert!(
            !bullets.contains(&"Alice works at Umbrella."),
            "{bullets:?}"
        );
        assert!(reachable.contains(
            &cognee_models::EdgeType::deterministic_id("Alice works at Acme.").to_string()
        ));
        assert!(!reachable.contains(
            &cognee_models::EdgeType::deterministic_id("Alice works at Umbrella.").to_string()
        ));

        // Unscoped, the out-of-set neighbour is a bullet like any other.
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].edges.len(), 3);
    }

    #[tokio::test]
    async fn edge_between_two_retrieved_entities_appears_under_both() {
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        add_node(&graph, "acme-id", "Acme").await;
        add_edge(
            &graph,
            "alice-id",
            "acme-id",
            "works_at",
            Some("Alice works at Acme."),
        )
        .await;
        let hits = vec![
            entity_hit(json!({"id": "alice-id", "name": "Alice"})),
            entity_hit(json!({"id": "acme-id", "name": "Acme"})),
        ];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        let bullet_texts: Vec<Vec<&str>> = entities
            .iter()
            .map(|e| e.edges.iter().map(|b| b.text.as_str()).collect())
            .collect();
        assert_eq!(
            bullet_texts,
            vec![vec!["Alice works at Acme."], vec!["Alice works at Acme."]]
        );
    }

    #[tokio::test]
    async fn malformed_row_skipped_without_dropping_entity() {
        // An edge whose target node is absent from the neighborhood: the entity
        // survives and the bullet uses the target id as its label.
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Source").await;
        add_edge(&graph, "entity-1", "target-1", "REL", None).await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].name, "Entity");
        assert_eq!(entities[0].edges[0].text, "Source rel target-1.");
    }

    #[tokio::test]
    async fn a_chunk_contains_edge_with_an_unnamed_source_is_dropped() {
        // The real-world shape: a DocumentChunk node (no `name`, only `id` and
        // `text`) structurally `contains` an entity, with no edge_text (a row
        // written before cognify stamped one) — the bullet would otherwise
        // read "<chunk-uuid> contains Alice.".
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        graph
            .add_node_raw(json!({
                "id": "ecb1240e-1b45-54cf-bdca-0cfca21971e9",
                "type": "DocumentChunk",
                "text": "Alice sat by the March Hare.",
            }))
            .await
            .unwrap();
        add_edge(
            &graph,
            "ecb1240e-1b45-54cf-bdca-0cfca21971e9",
            "alice-id",
            "contains",
            None,
        )
        .await;
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].name, "Alice");
        assert!(entities[0].edges.is_empty(), "{:?}", entities[0].edges);
    }

    #[tokio::test]
    async fn a_chunk_contains_edge_with_edge_text_is_kept() {
        // The Python-written shape (and Rust's since cognify stamps
        // edge_text): the unnamed chunk source is irrelevant once the edge
        // carries text, which here is the only line holding the description.
        let mentions = "Document chunk mentions Alice: A curious girl.";
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        graph
            .add_node_raw(json!({
                "id": "ecb1240e-1b45-54cf-bdca-0cfca21971e9",
                "type": "DocumentChunk",
                "text": "Alice sat by the March Hare.",
            }))
            .await
            .unwrap();
        add_edge(
            &graph,
            "ecb1240e-1b45-54cf-bdca-0cfca21971e9",
            "alice-id",
            "contains",
            Some(mentions),
        )
        .await;
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(bullets, [mentions]);
    }

    /// Three chunks, each `contains` Alice under the same mention text — the
    /// shape Rust cognify writes for a much-mentioned entity.
    async fn graph_with_repeated_mentions(mention: &str) -> MockGraphDB {
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        add_node(&graph, "acme-id", "Acme").await;
        for chunk in ["chunk-1", "chunk-2", "chunk-3"] {
            graph
                .add_node_raw(json!({"id": chunk, "type": "DocumentChunk"}))
                .await
                .unwrap();
            add_edge(&graph, chunk, "alice-id", "contains", Some(mention)).await;
        }
        add_edge(
            &graph,
            "alice-id",
            "acme-id",
            "works_at",
            Some("Alice works at Acme."),
        )
        .await;
        graph
    }

    #[tokio::test]
    async fn repeated_chunk_mentions_collapse_to_one_bullet() {
        let mention = "Document chunk mentions Alice: A curious girl.";
        let graph = graph_with_repeated_mentions(mention).await;
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];

        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(bullets, [mention, "Alice works at Acme."]);
        assert!(entities[0].covered_edge_type_ids.is_empty());
    }

    #[tokio::test]
    async fn a_chunk_mention_repeating_the_description_line_is_covered_not_shown() {
        let mention = "Document chunk mentions Alice: A curious girl.";
        let graph = graph_with_repeated_mentions(mention).await;
        let hits = vec![entity_hit(json!({
            "id": "alice-id",
            "name": "Alice",
            "description": "A curious girl.",
        }))];

        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(bullets, ["Alice works at Acme."]);
        // Its id is reported once per dropped edge; the facts lane only needs it
        // in the set, so repeats are harmless.
        let mention_id = cognee_models::EdgeType::deterministic_id(mention).to_string();
        assert!(
            entities[0]
                .covered_edge_type_ids
                .iter()
                .all(|id| *id == mention_id)
        );
        assert!(!entities[0].covered_edge_type_ids.is_empty());
        assert_eq!(
            format_entity(&entities[0]),
            "### Alice\nA curious girl.\n- Alice works at Acme."
        );
    }

    #[test]
    fn mention_repeats_compares_the_whole_description_after_the_colon() {
        let text = "Document chunk mentions Alice: A curious girl.";
        assert!(mention_repeats(text, " A curious girl. "));
        assert!(!mention_repeats(text, "girl."));
        assert!(!mention_repeats(text, "   "));
    }

    #[tokio::test]
    async fn a_contains_edge_with_a_named_source_still_renders() {
        // The drop guard is scoped to an unnamed source, not to the
        // "contains" relationship itself.
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        add_node(&graph, "box-id", "Toolbox").await;
        add_edge(&graph, "box-id", "alice-id", "contains", None).await;
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities[0].edges[0].text, "Toolbox contains Alice.");
    }

    #[test]
    fn render_edge_sentence_normalizes_the_relationship_into_words() {
        assert_eq!(
            render_edge_sentence("Alice", "is_a", "person"),
            "Alice is a person."
        );
        assert_eq!(
            render_edge_sentence("Alice", "WORKS-AT", "Acme"),
            "Alice works at Acme."
        );
    }

    #[tokio::test]
    async fn build_entities_fails_open_on_neighborhood_error() {
        let graph = FailingGraphDB::new();
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(
            entities,
            vec![EntityResult {
                id: "entity-1".to_string(),
                name: "Alice".to_string(),
                description: None,
                entity_type: None,
                covered_edge_type_ids: vec![],
                edges: vec![],
            }]
        );
    }

    #[tokio::test]
    async fn build_entities_returns_empty_for_no_hits() {
        let graph = MockGraphDB::new();
        let (entities, _) = build_entities(&graph, &[], 5, &HashMap::new(), None, "OR").await;
        assert!(entities.is_empty());
    }

    #[tokio::test]
    async fn ranked_edges_sorted_by_rank_value() {
        // entity-1 has four edges. Two are query-ranked (tier 1) and their
        // edge_ranks are the REVERSE of graph insertion order: "works_at" is
        // inserted first but ranks 1, "knows" is inserted second but ranks 0.
        // Two are unranked (tier 2). Expected bullet order:
        //   tier 1 by rank ascending -> knows (0), works_at (1)
        //   tier 2 in insertion order (stable sort) -> met, saw
        // A `(1, 0)`-constant sort key would leave the tier-1 pair in insertion
        // order [works_at, knows]; an unstable sort could swap met/saw.
        let works_at = "Alice works at Acme";
        let knows = "Alice knows Bob";
        let met = "Alice met Carol";
        let saw = "Alice saw Dave";

        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Alice").await;
        add_node(&graph, "acme-id", "Acme").await;
        add_node(&graph, "bob-id", "Bob").await;
        add_node(&graph, "carol-id", "Carol").await;
        add_node(&graph, "dave-id", "Dave").await;
        add_edge(&graph, "entity-1", "acme-id", "works_at", Some(works_at)).await;
        add_edge(&graph, "entity-1", "bob-id", "knows", Some(knows)).await;
        add_edge(&graph, "entity-1", "carol-id", "met", Some(met)).await;
        add_edge(&graph, "entity-1", "dave-id", "saw", Some(saw)).await;

        // Rank knows(0) ahead of works_at(1) -> reverse of insertion order.
        let edge_hits = vec![edge_hit(knows), edge_hit(works_at)];
        let edge_ranks = edge_rank_by_id(&edge_hits);
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Alice"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &edge_ranks, None, "OR").await;

        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(bullets, [knows, works_at, met, saw]);
    }

    #[test]
    fn is_type_edge_is_decided_by_the_relationship_alone() {
        // (a) Text reading " is a " no longer makes a type edge — with no
        // relationship, or with a non-type one (Python pins both).
        assert!(!is_type_edge(&type_edge_bullet(None, "Alice is a person")));
        assert!(!is_type_edge(&type_edge_bullet(
            Some("subsidiary_of"),
            "Acme is a subsidiary of Globex."
        )));
        // (b) A dash/upper "IS-A" relationship normalizes to "is a".
        assert!(is_type_edge(&type_edge_bullet(Some("IS-A"), "")));
        // (c) A non-type relationship whose text lacks " is a ".
        assert!(!is_type_edge(&type_edge_bullet(
            Some("plays"),
            "Alice plays tennis"
        )));
        // (d) An `is_a_*` relationship is not `is_a`.
        assert!(!is_type_edge(&type_edge_bullet(
            Some("is_a_member_of"),
            "Alice is a member of Club."
        )));
    }

    #[tokio::test]
    async fn an_is_a_member_of_edge_does_not_evict_a_query_ranked_edge() {
        // With no edge_text the fallback renders "Alice is a member of Club.",
        // which reads " is a ". It must stay unpinned, below the ranked edge.
        let ranked_bullet = "Alice works at Acme.";
        let graph = MockGraphDB::new();
        add_node(&graph, "alice-id", "Alice").await;
        add_node(&graph, "club-id", "Club").await;
        add_node(&graph, "acme-id", "Acme").await;
        add_edge(&graph, "alice-id", "club-id", "is_a_member_of", None).await;
        add_edge(
            &graph,
            "alice-id",
            "acme-id",
            "works_at",
            Some(ranked_bullet),
        )
        .await;
        let edge_ranks = edge_rank_by_id(&[edge_hit(ranked_bullet)]);
        let hits = vec![entity_hit(json!({"id": "alice-id", "name": "Alice"}))];

        let (entities, _) = build_entities(&graph, &hits, 1, &edge_ranks, None, "OR").await;
        let bullets: Vec<&str> = entities[0].edges.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(bullets, [ranked_bullet]);
    }

    #[tokio::test]
    async fn neighbor_to_neighbor_edge_is_dropped() {
        // entity-1 is the only seed. One edge connects two neighbors (n-a -> n-b)
        // and never touches the seed; only the seed's own edge (entity-1 -> n-a)
        // may surface as a bullet.
        let graph = MockGraphDB::new();
        add_node(&graph, "entity-1", "Entity1").await;
        add_node(&graph, "n-a", "NA").await;
        add_node(&graph, "n-b", "NB").await;
        add_edge(&graph, "n-a", "n-b", "REL", None).await;
        add_edge(&graph, "entity-1", "n-a", "REL2", None).await;
        let hits = vec![entity_hit(json!({"id": "entity-1", "name": "Entity1"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;

        assert_eq!(entities[0].edges.len(), 1);
        let bullet = &entities[0].edges[0];
        assert_eq!(bullet.text, "Entity1 rel2 NA.");
        assert_eq!(bullet.target_id.as_deref(), Some("n-a"));
        // The n-a -> n-b edge must not have leaked into the seed's bullets.
        assert!(entities[0].edges.iter().all(|e| {
            e.source_id.as_deref() != Some("n-b") && e.target_id.as_deref() != Some("n-b")
        }));
    }

    #[tokio::test]
    async fn build_entities_skips_neighborhood_when_no_ids() {
        // The only hit has no id (payload lacks "id", SearchItem.id is None), so
        // result_id is None and entity.id is empty. build_entities must return the
        // bare entity WITHOUT ever awaiting get_neighborhood (which panics here).
        let graph = PanicOnNeighborhoodGraphDB::new();
        let hits = vec![entity_hit(json!({"name": "X"}))];
        let (entities, _) = build_entities(&graph, &hits, 5, &HashMap::new(), None, "OR").await;
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "X");
        assert!(entities[0].edges.is_empty());
    }

    #[test]
    fn format_entities_renders_minimal_blocks() {
        // No optional fields -> header only.
        let minimal = EntityResult {
            id: "entity-1".to_string(),
            name: "Entity".to_string(),
            description: None,
            entity_type: None,
            covered_edge_type_ids: vec![],
            edges: vec![],
        };
        assert_eq!(
            format_entities(&[minimal]),
            "## Relevant entities\n### Entity"
        );

        // IndexSchema type is suppressed from the header.
        let index_schema = EntityResult {
            id: "entity-1".to_string(),
            name: "lisbon office logistics intelligence project".to_string(),
            description: None,
            entity_type: Some("IndexSchema".to_string()),
            covered_edge_type_ids: vec![],
            edges: vec![],
        };
        assert_eq!(
            format_entities(&[index_schema]),
            "## Relevant entities\n### lisbon office logistics intelligence project"
        );

        // Domain type + a description + an edge bullet.
        let full = EntityResult {
            id: "entity-1".to_string(),
            name: "Alice".to_string(),
            description: Some("An engineer.".to_string()),
            entity_type: Some("Person".to_string()),
            covered_edge_type_ids: vec![],
            edges: vec![EdgeBullet {
                text: "Alice works at Acme.".to_string(),
                source: Some("Alice".to_string()),
                target: Some("Acme".to_string()),
                source_id: Some("alice-id".to_string()),
                relationship: Some("works_at".to_string()),
                target_id: Some("acme-id".to_string()),
                edge_type_id: None,
            }],
        };
        assert_eq!(
            format_entities(&[full]),
            "## Relevant entities\n### Alice (Person)\nAn engineer.\n- Alice works at Acme."
        );

        // A blank-name entity yields no block, so the whole section is empty.
        let blank = EntityResult {
            id: String::new(),
            name: "   ".to_string(),
            description: None,
            entity_type: None,
            covered_edge_type_ids: vec![],
            edges: vec![],
        };
        assert_eq!(format_entities(&[blank]), "");
    }
}
