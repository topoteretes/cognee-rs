//! Task 1 of the `from_entities` method: read what a dataset contains, in one
//! graph call (Python's `load_dataset_content.py`).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use cognee_graph::{EdgeData, GraphNode, NodeData};
use cognee_models::{Data, marks_internal};
use serde_json::Value;

use super::persist::IndexStore;
use super::{DEFAULT_ENTITY_TYPE_NAME, DOCUMENT_INDEX_NODE_TYPES, ENTITY_NAME_MAX_CHARS};
use crate::error::CognifyError;

/// A dataset's documents, their chunks, the entities those chunks mention,
/// and its index as stored now. Every chunk and entity id in it resolves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatasetContent {
    pub dataset_id: String,
    /// Document id → name, for documents that have one.
    pub document_names: HashMap<String, String>,
    /// Chunk id → document id.
    pub document_by_chunk: HashMap<String, String>,
    /// Only entities some chunk mentions.
    pub entities: HashMap<String, DatasetEntity>,
    /// (source id, target id) → edge texts, in edge order.
    pub relations: HashMap<(String, String), Vec<String>>,
    /// Index node id → text; only the write reads it.
    pub existing_summary_texts: BTreeMap<String, String>,
}

impl DatasetContent {
    /// A document with no name renders as its id.
    pub fn document_name<'a>(&'a self, document_id: &'a str) -> &'a str {
        self.document_names
            .get(document_id)
            .map_or(document_id, String::as_str)
    }
}

/// One entity as the dataset states it.
#[derive(Debug, Clone, PartialEq)]
pub struct DatasetEntity {
    pub name: String,
    pub type_name: String,
    /// The dataset's chunks that mention it.
    pub chunk_ids: BTreeSet<String>,
}

/// Read everything the `from_entities` index needs (Python's
/// `load_dataset_content`).
///
/// The documents are the dataset's data items. Python names a document by
/// the `document_name` its chunks carry, which is the document's name or
/// its file's basename; Rust chunks carry no name, so the data item gives it.
pub async fn load_dataset_content(store: &IndexStore<'_>) -> Result<DatasetContent, CognifyError> {
    let data = store
        .db
        .get_dataset_data(store.dataset_id)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?;
    let documents: HashMap<String, Option<String>> = data
        .iter()
        .map(|item| (item.id.to_string(), data_item_name(item)))
        .collect();
    let filters = HashMap::from([(
        Cow::Borrowed("type"),
        DOCUMENT_INDEX_NODE_TYPES
            .iter()
            .map(|t| Value::from(*t))
            .collect(),
    )]);
    let (nodes, edges) = store.graph_db.get_filtered_graph_data(&filters).await?;
    Ok(dataset_content_from_rows(
        &nodes,
        &edges,
        &store.dataset_key(),
        &documents,
    ))
}

/// `name or basename(raw_data_location)`, as Python's chunkers name a document.
fn data_item_name(item: &Data) -> Option<String> {
    let basename = item
        .raw_data_location
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let name = if item.name.is_empty() {
        basename
    } else {
        item.name.as_str()
    };
    normalized_text(Some(&Value::from(name)), None)
}

/// Reshape raw graph rows (`_dataset_content_from_rows`). `documents` maps
/// the dataset's document ids to their names.
pub fn dataset_content_from_rows(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    dataset_id: &str,
    documents: &HashMap<String, Option<String>>,
) -> DatasetContent {
    let graph_nodes = parse_nodes(nodes, dataset_id, documents);
    let graph_edges = parse_edges(edges, &graph_nodes);
    // Only entities a chunk mentions: entity ids derive from names, so in a
    // shared database one entity node serves every dataset that names it.
    let mentioned = graph_edges.chunks_by_entity;
    let relations = graph_edges
        .relations
        .into_iter()
        .filter(|((source, target), _)| {
            mentioned.contains_key(source) && mentioned.contains_key(target)
        })
        .collect();
    let entities = mentioned
        .into_iter()
        .filter_map(|(entity_id, chunk_ids)| {
            let name = graph_nodes.entity_names.get(&entity_id)?.clone();
            let type_name = graph_edges
                .type_by_entity
                .get(&entity_id)
                .cloned()
                .unwrap_or_else(|| DEFAULT_ENTITY_TYPE_NAME.to_string());
            Some((
                entity_id,
                DatasetEntity {
                    name,
                    type_name,
                    chunk_ids,
                },
            ))
        })
        .collect();
    DatasetContent {
        dataset_id: dataset_id.to_string(),
        document_names: graph_nodes.document_names,
        document_by_chunk: graph_nodes.document_by_chunk,
        entities,
        relations,
        existing_summary_texts: graph_nodes.existing_summary_texts,
    }
}

/// The node half of a dataset's content.
struct GraphNodes {
    document_names: HashMap<String, String>,
    document_by_chunk: HashMap<String, String>,
    entity_names: HashMap<String, String>,
    entity_type_names: HashMap<String, String>,
    existing_summary_texts: BTreeMap<String, String>,
}

fn property<'a>(attributes: &'a NodeData, key: &str) -> Option<&'a Value> {
    attributes.get(key)
}

/// The rows this index can name, by node type. A row it cannot use is skipped.
fn parse_nodes(
    nodes: &[GraphNode],
    dataset_id: &str,
    documents: &HashMap<String, Option<String>>,
) -> GraphNodes {
    let mut out = GraphNodes {
        document_names: HashMap::new(),
        document_by_chunk: HashMap::new(),
        entity_names: HashMap::new(),
        entity_type_names: HashMap::new(),
        existing_summary_texts: BTreeMap::new(),
    };
    for (node_id, attributes) in nodes {
        if marks_internal(property(attributes, "is_internal")) {
            continue;
        }
        match property(attributes, "type").and_then(Value::as_str) {
            Some("DocumentChunk") => {
                // A chunk with no document, or with another dataset's, counts nowhere.
                let Some(document_id) = normalized_text(property(attributes, "document_id"), None)
                else {
                    continue;
                };
                let Some(name) = documents.get(&document_id) else {
                    continue;
                };
                if let Some(name) = name {
                    out.document_names
                        .entry(document_id.clone())
                        .or_insert_with(|| name.clone());
                }
                out.document_by_chunk.insert(node_id.clone(), document_id);
            }
            Some("Entity") => {
                if let Some(name) =
                    normalized_text(property(attributes, "name"), Some(ENTITY_NAME_MAX_CHARS))
                {
                    out.entity_names.insert(node_id.clone(), name);
                }
            }
            Some("EntityType") => {
                if let Some(name) =
                    normalized_text(property(attributes, "name"), Some(ENTITY_NAME_MAX_CHARS))
                {
                    out.entity_type_names.insert(node_id.clone(), name);
                }
            }
            Some("GlobalContextSummary") => {
                // Only this dataset's index nodes, so a shared database never
                // deletes another dataset's.
                if property(attributes, "dataset_id").is_some_and(|v| value_str(v) == dataset_id) {
                    let text = property(attributes, "text")
                        .map(value_str)
                        .unwrap_or_default();
                    out.existing_summary_texts.insert(node_id.clone(), text);
                }
            }
            _ => {}
        }
    }
    out
}

/// Python's `str(value)` for the scalars a property holds; null is empty.
fn value_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The edge half: what the node rows alone cannot say.
struct GraphEdges {
    chunks_by_entity: HashMap<String, BTreeSet<String>>,
    type_by_entity: HashMap<String, String>,
    relations: HashMap<(String, String), Vec<String>>,
}

/// The edges between the nodes [`parse_nodes`] kept; any other edge is skipped.
fn parse_edges(edges: &[EdgeData], nodes: &GraphNodes) -> GraphEdges {
    let mut out = GraphEdges {
        chunks_by_entity: HashMap::new(),
        type_by_entity: HashMap::new(),
        relations: HashMap::new(),
    };
    for (source, target, edge_relationship_name, attributes) in edges {
        let name = attributes
            .get("relationship_name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .unwrap_or(edge_relationship_name);
        if name == "contains"
            && nodes.document_by_chunk.contains_key(source)
            && nodes.entity_names.contains_key(target)
        {
            out.chunks_by_entity
                .entry(target.clone())
                .or_default()
                .insert(source.clone());
        } else if name == "is_a"
            && nodes.entity_names.contains_key(source)
            && let Some(type_name) = nodes.entity_type_names.get(target)
        {
            // Several types: keep the one that sorts first, so the pick is stable.
            let current = out.type_by_entity.get(source);
            if current.is_none_or(|current| type_name < current) {
                out.type_by_entity.insert(source.clone(), type_name.clone());
            }
        } else if nodes.entity_names.contains_key(source)
            && nodes.entity_names.contains_key(target)
            && let Some(edge_text) = normalized_text(attributes.get("edge_text"), None)
        {
            out.relations
                .entry((source.clone(), target.clone()))
                .or_default()
                .push(edge_text);
        }
    }
    out
}

/// Python's `str.isspace`: Unicode whitespace plus the four ASCII separators
/// (`\x1c`–`\x1f`) Rust does not count.
fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Collapsed whitespace, optionally capped to `max_chars` characters; `None`
/// for a non-string or when nothing is left (`_normalized_text`).
pub(crate) fn normalized_text(value: Option<&Value>, max_chars: Option<usize>) -> Option<String> {
    let text = value?.as_str()?;
    let mut collapsed = text
        .split(is_python_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(max) = max_chars
        && let Some((cut, _)) = collapsed.char_indices().nth(max)
    {
        collapsed.truncate(cut);
    }
    (!collapsed.is_empty()).then_some(collapsed)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, props: Value) -> GraphNode {
        let Value::Object(map) = props else {
            panic!("object")
        };
        (
            id.to_string(),
            map.into_iter().map(|(k, v)| (Cow::Owned(k), v)).collect(),
        )
    }

    fn edge(source: &str, target: &str, rel: &str, props: Value) -> EdgeData {
        let Value::Object(map) = props else {
            panic!("object")
        };
        (
            source.to_string(),
            target.to_string(),
            rel.to_string(),
            map.into_iter().map(|(k, v)| (Cow::Owned(k), v)).collect(),
        )
    }

    #[test]
    fn normalized_text_collapses_python_whitespace_and_caps_chars() {
        assert_eq!(
            normalized_text(Some(&json!("  a\t b\u{1c}c\n")), None).as_deref(),
            Some("a b c")
        );
        assert_eq!(
            normalized_text(Some(&json!("ééééé")), Some(3)).as_deref(),
            Some("ééé")
        );
        assert_eq!(normalized_text(Some(&json!("   ")), None), None);
        assert_eq!(normalized_text(Some(&json!(3)), None), None);
    }

    #[test]
    fn rows_keep_only_this_datasets_chunks_mentioned_entities_and_index_nodes() {
        let nodes = vec![
            node("c1", json!({"type": "DocumentChunk", "document_id": "d1"})),
            node(
                "c_other",
                json!({"type": "DocumentChunk", "document_id": "dx"}),
            ),
            node(
                "c_internal",
                json!({"type": "DocumentChunk", "document_id": "d1", "is_internal": true}),
            ),
            node("e1", json!({"type": "Entity", "name": " Alice  Smith "})),
            node("e2", json!({"type": "Entity", "name": "Bob"})),
            node("e3", json!({"type": "Entity", "name": "Unmentioned"})),
            node("t1", json!({"type": "EntityType", "name": "Person"})),
            node("t2", json!({"type": "EntityType", "name": "Human"})),
            node(
                "g1",
                json!({"type": "GlobalContextSummary", "dataset_id": "ds", "text": "old"}),
            ),
            node(
                "g2",
                json!({"type": "GlobalContextSummary", "dataset_id": "other", "text": "x"}),
            ),
            node(
                "g3",
                json!({"type": "GlobalContextSummary", "text": "no dataset"}),
            ),
        ];
        let edges = vec![
            edge("c1", "e1", "contains", json!({})),
            edge("c1", "e2", "contains", json!({})),
            edge("c_other", "e3", "contains", json!({})),
            edge("e1", "t1", "is_a", json!({})),
            edge("e1", "t2", "is_a", json!({})),
            edge(
                "e1",
                "e2",
                "knows",
                json!({"edge_text": "Alice  knows Bob"}),
            ),
            edge("e1", "e2", "likes", json!({})),
            edge("e1", "e3", "knows", json!({"edge_text": "Alice knows U"})),
        ];
        let documents = HashMap::from([("d1".to_string(), Some("alpha.md".to_string()))]);

        let content = dataset_content_from_rows(&nodes, &edges, "ds", &documents);

        assert_eq!(
            content.document_by_chunk,
            HashMap::from([("c1".to_string(), "d1".to_string())])
        );
        assert_eq!(content.document_name("d1"), "alpha.md");
        assert_eq!(content.entities.len(), 2);
        assert_eq!(content.entities["e1"].name, "Alice Smith");
        assert_eq!(content.entities["e1"].type_name, "Human");
        assert_eq!(content.entities["e2"].type_name, "Entity");
        assert_eq!(
            content.relations,
            HashMap::from([(
                ("e1".to_string(), "e2".to_string()),
                vec!["Alice knows Bob".to_string()]
            )])
        );
        assert_eq!(
            content.existing_summary_texts,
            BTreeMap::from([("g1".to_string(), "old".to_string())])
        );
    }
}
