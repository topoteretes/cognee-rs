//! Widen a temporal rerank's candidate pool with what the window itself
//! points at.
//!
//! Port of `cognee/modules/retrieval/temporal_hybrid/expansion.py`. Hybrid's
//! candidates are chosen by vector similarity, and embeddings barely encode
//! dates: of the rows dated on the day a question names, few land among the
//! similarity candidates. The graph knows them anyway: every one has an edge
//! into that day's `Timestamp` node. So once the window is known, the nodes
//! attached to the in-window timestamps are added to the candidates (after
//! hybrid's own, which keep their relevance order) before the anchors are
//! computed; the rerank then moves them to the front.
//!
//! Only chunks are added: what counts as one is decided by the vector store,
//! not by type names, so an attached id found in a chunk collection joins and
//! anything else (entities, other node types) is ignored. Entities stay with
//! the entity lane: an entity attached to a window is usually a hub with edges
//! into many times ("the space race"), and promoting those displaces the
//! entities the question is about.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_graph::{EdgeData, WindowTimestamp};
use cognee_vector::VectorDB;
use uuid::Uuid;

use crate::retrievers::context_items::search_results_to_context;
use crate::retrievers::hybrid::{payload_matches_node_filter, result_id};
use crate::types::{SearchError, SearchItem};

/// The collections whose rows count as chunks: `(data_type, field)`.
///
/// Python's `chunk_collections` adds `DltRow_text` when DLT rows are indexed;
/// Rust has no DLT ingestion, so document chunks are the only chunk rows.
pub(crate) const CHUNK_COLLECTIONS: &[(&str, &str)] = &[("DocumentChunk", "text")];

/// Ids of the nodes with an edge into one of `timestamps`, in timestamp order.
///
/// `edges` is `get_neighborhood(timestamp ids, depth = 1)`'s edge half; only
/// edges whose target is one of the timestamps count, so a timestamp's other
/// neighbours (and the timestamps themselves) are never candidates. A node
/// attached to several timestamps takes the rank of the earliest; ties break
/// by id. Port of `attached_node_ids`.
pub(crate) fn attached_node_ids(timestamps: &[WindowTimestamp], edges: &[EdgeData]) -> Vec<String> {
    let order: HashMap<&str, usize> = timestamps
        .iter()
        .enumerate()
        .map(|(index, timestamp)| (timestamp.id.as_str(), index))
        .collect();
    let mut attached: HashMap<&str, usize> = HashMap::new();
    for (source, target, _, _) in edges {
        let Some(&rank) = order.get(target.as_str()) else {
            continue;
        };
        if order.contains_key(source.as_str()) {
            continue;
        }
        attached
            .entry(source.as_str())
            .and_modify(|current| *current = (*current).min(rank))
            .or_insert(rank);
    }
    let mut ids: Vec<(&str, usize)> = attached.into_iter().collect();
    ids.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    ids.into_iter().map(|(id, _)| id.to_string()).collect()
}

/// The vector rows for `node_ids` across `collections`, in `node_ids` order.
///
/// The first collection holding an id wins, and a row outside the requested
/// node set is dropped. A collection the store does not have contributes
/// nothing (the lanes treat a missing collection as an empty channel too).
/// Port of `retrieve_in_collections`.
pub(crate) async fn retrieve_in_collections(
    vector_db: &Arc<dyn VectorDB>,
    collections: &[(&str, &str)],
    node_ids: &[String],
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Result<Vec<SearchItem>, SearchError> {
    if node_ids.is_empty() {
        return Ok(vec![]);
    }
    // Vector rows are keyed by UUID; an id that is not one has no row.
    let uuids: Vec<Uuid> = node_ids
        .iter()
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();
    let mut by_id: HashMap<String, SearchItem> = HashMap::new();
    for (data_type, field) in collections {
        if uuids.is_empty() || !vector_db.has_collection(data_type, field).await? {
            continue;
        }
        let hits = search_results_to_context(vector_db.retrieve(data_type, field, &uuids).await?)?;
        for hit in hits {
            let Some(hit_id) = result_id(&hit) else {
                continue;
            };
            if by_id.contains_key(&hit_id)
                || !payload_matches_node_filter(&hit.payload, node_name, node_name_filter_operator)
            {
                continue;
            }
            by_id.insert(hit_id, hit);
        }
    }
    Ok(node_ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::HashMap;

    use cognee_vector::{MockVectorDB, VectorPoint};
    use serde_json::json;

    use super::*;

    fn ts(id: &str) -> WindowTimestamp {
        WindowTimestamp {
            id: id.to_string(),
            timestamp_str: None,
            time_at: 0,
            time_until: 1,
        }
    }

    fn edge(source: &str, target: &str) -> EdgeData {
        (
            source.to_string(),
            target.to_string(),
            "rel".to_string(),
            HashMap::new(),
        )
    }

    #[test]
    fn attached_node_ids_orders_by_timestamp_and_skips_timestamps() {
        let edges = vec![
            edge("r2", "ts_1950_06"),
            edge("r1", "ts_1950"),
            edge("r3", "ts_1950"),
            edge("c1", "ts_1950"),
            edge("atlas", "ts_1950"),
            edge("tbl", "ts_1950"),
            edge("atlas", "helios"),
            // Timestamp to timestamp: never a candidate.
            edge("ts_1950_06", "ts_1950"),
            // An edge *from* a timestamp does not attach its target.
            edge("ts_1950", "elsewhere"),
        ];
        assert_eq!(
            attached_node_ids(&[ts("ts_1950"), ts("ts_1950_06")], &edges),
            ["atlas", "c1", "r1", "r3", "tbl", "r2"]
        );
    }

    async fn index(db: &MockVectorDB, data_type: &str, id: Uuid, text: &str, sets: &[&str]) {
        let mut point = VectorPoint::new(id, vec![1.0, 0.0])
            .with_metadata("id", json!(id.to_string()))
            .with_metadata("text", json!(text));
        if !sets.is_empty() {
            point = point.with_metadata("belongs_to_set", json!(sets));
        }
        db.index_points(data_type, "text", &[point]).await.unwrap();
    }

    #[tokio::test]
    async fn retrieve_in_collections_keeps_id_order_and_first_collection_wins() {
        let db = MockVectorDB::new();
        db.create_collection("A", "text", 2).await.unwrap();
        db.create_collection("B", "text", 2).await.unwrap();
        let (x, y, z) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        index(&db, "A", x, "from A", &[]).await;
        index(&db, "A", y, "y", &[]).await;
        index(&db, "B", x, "from B", &[]).await;
        index(&db, "B", z, "z", &["other"]).await;
        let db: Arc<dyn VectorDB> = Arc::new(db);

        let ids = [z, x, Uuid::new_v4(), y].map(|id| id.to_string()).to_vec();
        let hits = retrieve_in_collections(
            &db,
            &[("A", "text"), ("B", "text"), ("C", "text")],
            &ids,
            None,
            "OR",
        )
        .await
        .unwrap();
        let got: Vec<(String, String)> = hits
            .iter()
            .map(|hit| {
                (
                    result_id(hit).unwrap(),
                    hit.payload["text"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (z.to_string(), "z".to_string()),
                (x.to_string(), "from A".to_string()),
                (y.to_string(), "y".to_string()),
            ]
        );

        // The node-set filter applies: only z is in "other".
        let scoped = retrieve_in_collections(
            &db,
            &[("A", "text"), ("B", "text")],
            &ids,
            Some(&["other".to_string()]),
            "OR",
        )
        .await
        .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(result_id(&scoped[0]), Some(z.to_string()));

        assert!(
            retrieve_in_collections(&db, &[("A", "text")], &[], None, "OR")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
