//! Stage 4 of `improve()` — incrementally sync recent graph edges into a
//! session's `graph_context`.
//!
//! Ported from `/tmp/cognee-python/cognee/tasks/memify/sync_graph_to_session.py`.
//!
//! Behavior:
//! 1. Load the checkpoint timestamp for
//!    `graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}`.
//! 2. Paginate through edges in the relational DB with `created_at > since`,
//!    in batches of [`BATCH_SIZE`]. Stop when a partial batch is returned.
//! 3. Resolve edge endpoints to node records so each line can include
//!    `label`/`type`/`description`.
//! 4. Emit each edge as a JSON-line
//!    `{"source": ..., "relationship": ..., "target": ...}`.
//! 5. Merge with existing `graph_context` and cap at [`DEFAULT_MAX_LINES`]
//!    lines (drop oldest when over).
//! 6. Persist the merged context and advance the checkpoint.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use cognee_database::ops::graph_storage::{get_edges_since, get_nodes_by_ids};
use cognee_database::uuid_hex;
use cognee_database::{CheckpointStore, DatabaseConnection, DatabaseError, GraphEdge, GraphNode};
use cognee_session::SessionManager;
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

/// Pagination batch size. Matches Python `sync_graph_to_session.py:34`.
pub const BATCH_SIZE: u64 = 500;

/// Default cap on the number of JSON-lines stored in a session's graph
/// context. Matches Python `DEFAULT_MAX_LINES = 500`.
pub const DEFAULT_MAX_LINES: usize = 500;

/// Error type for Stage 4 (`sync_graph_to_session`).
#[derive(Debug, Error)]
pub enum SyncError {
    #[error("Database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),
}

/// Summary of a Stage 4 run.
#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    /// Number of newly synced edges.
    pub synced: usize,
    /// Total number of JSON-lines in the session graph_context after merge.
    pub total: usize,
}

/// Build the checkpoint key. Matches Python `_checkpoint_key()`.
pub fn checkpoint_key(user_id: &str, dataset_id: Uuid, session_id: &str) -> String {
    format!("graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}")
}

/// Render an edge as a JSON-line using node metadata from `node_map`.
///
/// Returns `None` if either endpoint is missing from `node_map`. Matches
/// Python `_edge_to_text()` semantics (keys: `label`, `type`, `description`).
pub fn edge_to_json_line(edge: &GraphEdge, node_map: &HashMap<Uuid, GraphNode>) -> Option<String> {
    let src = node_map.get(&edge.source_node_id)?;
    let dst = node_map.get(&edge.destination_node_id)?;

    let mut src_obj = serde_json::Map::new();
    src_obj.insert(
        "label".to_string(),
        serde_json::Value::String(src.label.clone().unwrap_or_else(|| {
            if !src.node_type.is_empty() {
                src.node_type.clone()
            } else {
                src.id.to_string()
            }
        })),
    );
    if !src.node_type.is_empty() {
        src_obj.insert(
            "type".to_string(),
            serde_json::Value::String(src.node_type.clone()),
        );
    }
    if let Some(attrs) = src
        .attributes
        .as_ref()
        .and_then(|v| v.get("description"))
        .and_then(|v| v.as_str())
    {
        src_obj.insert(
            "description".to_string(),
            serde_json::Value::String(attrs.to_string()),
        );
    }

    let mut dst_obj = serde_json::Map::new();
    dst_obj.insert(
        "label".to_string(),
        serde_json::Value::String(dst.label.clone().unwrap_or_else(|| {
            if !dst.node_type.is_empty() {
                dst.node_type.clone()
            } else {
                dst.id.to_string()
            }
        })),
    );
    if !dst.node_type.is_empty() {
        dst_obj.insert(
            "type".to_string(),
            serde_json::Value::String(dst.node_type.clone()),
        );
    }
    if let Some(attrs) = dst
        .attributes
        .as_ref()
        .and_then(|v| v.get("description"))
        .and_then(|v| v.as_str())
    {
        dst_obj.insert(
            "description".to_string(),
            serde_json::Value::String(attrs.to_string()),
        );
    }

    let relationship = if edge.relationship_name.is_empty() {
        "related_to".to_string()
    } else {
        edge.relationship_name.clone()
    };

    let mut line = serde_json::Map::new();
    line.insert("source".to_string(), serde_json::Value::Object(src_obj));
    line.insert(
        "relationship".to_string(),
        serde_json::Value::String(relationship),
    );
    line.insert("target".to_string(), serde_json::Value::Object(dst_obj));
    Some(serde_json::Value::Object(line).to_string())
}

/// Sync graph→session for a single session.
#[allow(clippy::too_many_arguments)]
pub async fn sync_graph_to_session(
    user_id: &str,
    session_id: &str,
    dataset_id: Uuid,
    db: &DatabaseConnection,
    session_manager: &SessionManager,
    checkpoint_store: &dyn CheckpointStore,
    max_lines: usize,
) -> Result<SyncResult, SyncError> {
    let ck = checkpoint_key(user_id, dataset_id, session_id);
    let since: Option<DateTime<Utc>> = checkpoint_store.load(&ck).await?;

    let mut new_lines: Vec<String> = Vec::new();
    let mut latest: Option<DateTime<Utc>> = since;

    loop {
        let edges = get_edges_since(db, dataset_id, latest, BATCH_SIZE).await?;
        if edges.is_empty() {
            break;
        }

        // Collect endpoint hex ids for batch node fetch.
        let mut id_hex_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in &edges {
            id_hex_set.insert(uuid_hex::to_hex(e.source_node_id));
            id_hex_set.insert(uuid_hex::to_hex(e.destination_node_id));
        }
        let id_hex_vec: Vec<String> = id_hex_set.into_iter().collect();
        let nodes = get_nodes_by_ids(db, &id_hex_vec).await?;
        let node_map: HashMap<Uuid, GraphNode> = nodes.into_iter().map(|n| (n.id, n)).collect();

        for e in &edges {
            if let Some(line) = edge_to_json_line(e, &node_map) {
                new_lines.push(line);
            }
            if latest.map(|t| e.created_at > t).unwrap_or(true) {
                latest = Some(e.created_at);
            }
        }

        if (edges.len() as u64) < BATCH_SIZE {
            break;
        }
    }

    if new_lines.is_empty() {
        info!(
            session_id = session_id,
            "sync_graph_to_session: no new edges"
        );
        return Ok(SyncResult::default());
    }

    let existing = session_manager
        .get_graph_context(Some(session_id), Some(user_id))
        .await?;
    let mut merged: Vec<String> = existing
        .as_deref()
        .map(|s| {
            s.split('\n')
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    merged.extend(new_lines.iter().cloned());
    if merged.len() > max_lines {
        let drop = merged.len() - max_lines;
        info!(
            session_id = session_id,
            dropped = drop,
            cap = max_lines,
            "sync_graph_to_session: capping, dropping oldest"
        );
        merged.drain(0..drop);
    }

    let merged_str = merged.join("\n");
    session_manager
        .set_graph_context(Some(session_id), Some(user_id), &merged_str)
        .await?;

    if let Some(ts) = latest
        && Some(ts) != since
    {
        checkpoint_store.save(&ck, ts).await?;
    }

    info!(
        session_id = session_id,
        synced = new_lines.len(),
        total = merged.len(),
        max_lines = max_lines,
        "sync_graph_to_session: complete"
    );

    Ok(SyncResult {
        synced: new_lines.len(),
        total: merged.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_key_format() {
        let u = Uuid::nil();
        let k = checkpoint_key("user-1", u, "sess-1");
        assert_eq!(k, format!("graph_sync_checkpoint:user-1:{u}:sess-1"));
    }

    #[test]
    fn edge_to_json_line_full() {
        let src_id = Uuid::new_v4();
        let dst_id = Uuid::new_v4();
        let edge = GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            source_node_id: src_id,
            destination_node_id: dst_id,
            relationship_name: "knows".to_string(),
            label: None,
            attributes: None,
            created_at: chrono::Utc::now(),
        };
        let mut node_map = HashMap::new();
        node_map.insert(
            src_id,
            GraphNode {
                id: src_id,
                slug: Uuid::new_v4(),
                user_id: Uuid::new_v4(),
                data_id: Uuid::new_v4(),
                dataset_id: Uuid::new_v4(),
                label: Some("Alice".to_string()),
                node_type: "Person".to_string(),
                indexed_fields: serde_json::json!({}),
                attributes: Some(serde_json::json!({"description": "An engineer"})),
                created_at: chrono::Utc::now(),
            },
        );
        node_map.insert(
            dst_id,
            GraphNode {
                id: dst_id,
                slug: Uuid::new_v4(),
                user_id: Uuid::new_v4(),
                data_id: Uuid::new_v4(),
                dataset_id: Uuid::new_v4(),
                label: Some("Bob".to_string()),
                node_type: "Person".to_string(),
                indexed_fields: serde_json::json!({}),
                attributes: None,
                created_at: chrono::Utc::now(),
            },
        );
        let line = edge_to_json_line(&edge, &node_map).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["relationship"], serde_json::json!("knows"));
        assert_eq!(parsed["source"]["label"], serde_json::json!("Alice"));
        assert_eq!(parsed["source"]["type"], serde_json::json!("Person"));
        assert_eq!(
            parsed["source"]["description"],
            serde_json::json!("An engineer")
        );
        assert_eq!(parsed["target"]["label"], serde_json::json!("Bob"));
        // dst has no description
        assert!(parsed["target"].get("description").is_none());
    }

    #[test]
    fn edge_to_json_line_missing_endpoint() {
        let src_id = Uuid::new_v4();
        let dst_id = Uuid::new_v4();
        let edge = GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            source_node_id: src_id,
            destination_node_id: dst_id,
            relationship_name: "r".to_string(),
            label: None,
            attributes: None,
            created_at: chrono::Utc::now(),
        };
        let empty = HashMap::new();
        assert!(edge_to_json_line(&edge, &empty).is_none());
    }
}
