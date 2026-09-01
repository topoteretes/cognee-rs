// UUID hex conversions from database bytes always produce valid UUIDs — any
// failure here indicates database corruption, not a recoverable condition.
#![allow(
    clippy::expect_used,
    reason = "UUID hex round-trips from DB are guaranteed by schema; failure indicates corruption"
)]

use crate::entities::{
    data, dataset, dataset_data, edge, graph_metrics, node, pipeline_run, query, result_log,
    task_run,
};
use crate::types::{
    DatabaseError, GraphEdge, GraphMetrics, GraphNode, PipelineRun, PipelineRunStatus,
    SearchHistoryEntry, SearchHistoryEntryType, TaskRun,
};
use crate::uuid_hex;
/// Shared SeaORM ↔ domain-type conversions and error helpers used across ops modules.
use chrono::Utc;
use cognee_models::{Data, Dataset};
use cognee_utils::sanitize::{sanitize_json, sanitize_str, sanitize_string};
use sea_orm::ActiveValue::Set;

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

pub(crate) fn map_sea_err(e: sea_orm::DbErr) -> DatabaseError {
    match &e {
        sea_orm::DbErr::RecordNotFound(_) => DatabaseError::NotFound(e.to_string()),
        #[cfg(any(feature = "sqlite", feature = "postgres"))]
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(sqlx_err)) => {
            let s = sqlx_err.to_string();
            if s.contains("UNIQUE constraint failed") || s.contains("unique constraint") {
                DatabaseError::UniqueViolation(s)
            } else {
                DatabaseError::QueryError(s)
            }
        }
        _ => DatabaseError::QueryError(e.to_string()),
    }
}

/// SeaORM raises an error when on_conflict do_nothing finds a duplicate.
/// This helper treats that as a no-op success.
pub(crate) fn ignore_do_nothing(result: Result<(), DatabaseError>) -> Result<(), DatabaseError> {
    match result {
        Err(DatabaseError::QueryError(ref msg))
            if msg.contains("None of the records are inserted") =>
        {
            Ok(())
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Dataset conversions
// ---------------------------------------------------------------------------

impl From<dataset::Model> for Dataset {
    fn from(m: dataset::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            name: m.name,
            owner_id: uuid_hex::from_hex(&m.owner_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            tenant_id: uuid_hex::from_hex_opt(m.tenant_id.as_deref()).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<&Dataset> for dataset::ActiveModel {
    fn from(d: &Dataset) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(d.id)),
            name: Set(d.name.clone()),
            owner_id: Set(uuid_hex::to_hex(d.owner_id)),
            tenant_id: Set(uuid_hex::to_hex_opt(d.tenant_id)),
            created_at: Set(d.created_at),
            updated_at: Set(d.updated_at),
        }
    }
}

// ---------------------------------------------------------------------------
// Data conversions
// ---------------------------------------------------------------------------

impl From<data::Model> for Data {
    fn from(m: data::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            name: m.name,
            raw_data_location: m.raw_data_location,
            original_data_location: m.original_data_location,
            extension: m.extension,
            mime_type: m.mime_type,
            content_hash: m.content_hash,
            owner_id: uuid_hex::from_hex(&m.owner_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            created_at: m.created_at,
            updated_at: m.updated_at,
            label: m.label,
            original_extension: m.original_extension,
            original_mime_type: m.original_mime_type,
            loader_engine: m.loader_engine,
            raw_content_hash: m.raw_content_hash,
            tenant_id: uuid_hex::from_hex_opt(m.tenant_id.as_deref()).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            external_metadata: m.external_metadata,
            node_set: m.node_set,
            pipeline_status: m.pipeline_status,
            token_count: m.token_count,
            data_size: m.data_size,
            last_accessed: m.last_accessed,
            importance_weight: m.importance_weight,
        }
    }
}

impl From<&Data> for data::ActiveModel {
    fn from(d: &Data) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(d.id)),
            name: Set(d.name.clone()),
            raw_data_location: Set(d.raw_data_location.clone()),
            original_data_location: Set(d.original_data_location.clone()),
            extension: Set(d.extension.clone()),
            mime_type: Set(d.mime_type.clone()),
            content_hash: Set(d.content_hash.clone()),
            owner_id: Set(uuid_hex::to_hex(d.owner_id)),
            created_at: Set(d.created_at),
            updated_at: Set(d.updated_at),
            label: Set(d.label.clone()),
            original_extension: Set(d.original_extension.clone()),
            original_mime_type: Set(d.original_mime_type.clone()),
            loader_engine: Set(d.loader_engine.clone()),
            raw_content_hash: Set(d.raw_content_hash.clone()),
            tenant_id: Set(uuid_hex::to_hex_opt(d.tenant_id)),
            external_metadata: Set(d.external_metadata.clone()),
            node_set: Set(d.node_set.clone()),
            pipeline_status: Set(d.pipeline_status.clone()),
            token_count: Set(d.token_count),
            data_size: Set(d.data_size),
            last_accessed: Set(d.last_accessed),
            importance_weight: Set(d.importance_weight),
        }
    }
}

// ---------------------------------------------------------------------------
// DatasetData conversions
// ---------------------------------------------------------------------------

pub(crate) fn make_dataset_data_active(
    dataset_id: uuid::Uuid,
    data_id: uuid::Uuid,
) -> dataset_data::ActiveModel {
    dataset_data::ActiveModel {
        dataset_id: Set(uuid_hex::to_hex(dataset_id)),
        data_id: Set(uuid_hex::to_hex(data_id)),
        created_at: Set(Utc::now()),
    }
}

// ---------------------------------------------------------------------------
// Search history conversions
// ---------------------------------------------------------------------------

pub(crate) fn query_model_to_history(m: query::Model) -> SearchHistoryEntry {
    let id = uuid_hex::from_hex(&m.id).expect(
        "DB stores only valid UUID hex strings; corruption indicates data integrity failure",
    );
    SearchHistoryEntry {
        entry_id: id,
        query_id: id,
        entry_type: SearchHistoryEntryType::Query,
        content: m.query_text,
        query_type: Some(m.query_type),
        user_id: uuid_hex::from_hex_opt(m.user_id.as_deref()).expect(
            "DB stores only valid UUID hex strings; corruption indicates data integrity failure",
        ),
        created_at: m.created_at,
    }
}

pub(crate) fn result_model_to_history(m: result_log::Model) -> SearchHistoryEntry {
    SearchHistoryEntry {
        entry_id: uuid_hex::from_hex(&m.id).expect(
            "DB stores only valid UUID hex strings; corruption indicates data integrity failure",
        ),
        query_id: uuid_hex::from_hex(&m.query_id).expect(
            "DB stores only valid UUID hex strings; corruption indicates data integrity failure",
        ),
        entry_type: SearchHistoryEntryType::Result,
        content: m.serialized_result,
        query_type: None,
        user_id: uuid_hex::from_hex_opt(m.user_id.as_deref()).expect(
            "DB stores only valid UUID hex strings; corruption indicates data integrity failure",
        ),
        created_at: m.created_at,
    }
}

// ---------------------------------------------------------------------------
// Graph node/edge conversions
// ---------------------------------------------------------------------------

impl From<node::Model> for GraphNode {
    fn from(m: node::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            slug: uuid_hex::from_hex(&m.slug).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            user_id: uuid_hex::from_hex(&m.user_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            data_id: uuid_hex::from_hex(&m.data_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            dataset_id: uuid_hex::from_hex(&m.dataset_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            label: m.label,
            node_type: m.node_type,
            indexed_fields: m.indexed_fields,
            attributes: m.attributes,
            created_at: m.created_at,
        }
    }
}

impl From<&GraphNode> for node::ActiveModel {
    fn from(n: &GraphNode) -> Self {
        // `attributes` carries the serialized DataPoint, chunk text included, so
        // NUL bytes must be stripped before this reaches Postgres — the whole
        // provenance batch shares one transaction, so a single bad chunk would
        // abort every row in it. See `cognee_utils::sanitize`.
        Self {
            id: Set(uuid_hex::to_hex(n.id)),
            slug: Set(uuid_hex::to_hex(n.slug)),
            user_id: Set(uuid_hex::to_hex(n.user_id)),
            data_id: Set(uuid_hex::to_hex(n.data_id)),
            dataset_id: Set(uuid_hex::to_hex(n.dataset_id)),
            label: Set(n.label.clone().map(sanitize_string)),
            node_type: Set(sanitize_str(&n.node_type).into_owned()),
            indexed_fields: Set(sanitize_json(n.indexed_fields.clone())),
            attributes: Set(n.attributes.clone().map(sanitize_json)),
            created_at: Set(n.created_at),
        }
    }
}

impl From<edge::Model> for GraphEdge {
    fn from(m: edge::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            slug: uuid_hex::from_hex(&m.slug).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            user_id: uuid_hex::from_hex(&m.user_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            data_id: uuid_hex::from_hex(&m.data_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            dataset_id: uuid_hex::from_hex(&m.dataset_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            source_node_id: uuid_hex::from_hex(&m.source_node_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            destination_node_id: uuid_hex::from_hex(&m.destination_node_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            relationship_name: m.relationship_name,
            label: m.label,
            attributes: m.attributes,
            created_at: m.created_at,
        }
    }
}

impl From<&GraphEdge> for edge::ActiveModel {
    fn from(e: &GraphEdge) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(e.id)),
            slug: Set(uuid_hex::to_hex(e.slug)),
            user_id: Set(uuid_hex::to_hex(e.user_id)),
            data_id: Set(uuid_hex::to_hex(e.data_id)),
            dataset_id: Set(uuid_hex::to_hex(e.dataset_id)),
            source_node_id: Set(uuid_hex::to_hex(e.source_node_id)),
            destination_node_id: Set(uuid_hex::to_hex(e.destination_node_id)),
            relationship_name: Set(sanitize_str(&e.relationship_name).into_owned()),
            label: Set(e.label.clone().map(sanitize_string)),
            attributes: Set(e.attributes.clone().map(sanitize_json)),
            created_at: Set(e.created_at),
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline run conversions
// ---------------------------------------------------------------------------

pub(crate) fn entity_status_to_domain(s: pipeline_run::PipelineRunStatus) -> PipelineRunStatus {
    match s {
        pipeline_run::PipelineRunStatus::Initiated => PipelineRunStatus::Initiated,
        pipeline_run::PipelineRunStatus::Started => PipelineRunStatus::Started,
        pipeline_run::PipelineRunStatus::Completed => PipelineRunStatus::Completed,
        pipeline_run::PipelineRunStatus::Errored => PipelineRunStatus::Errored,
    }
}

pub(crate) fn domain_status_to_entity(s: PipelineRunStatus) -> pipeline_run::PipelineRunStatus {
    match s {
        PipelineRunStatus::Initiated => pipeline_run::PipelineRunStatus::Initiated,
        PipelineRunStatus::Started => pipeline_run::PipelineRunStatus::Started,
        PipelineRunStatus::Completed => pipeline_run::PipelineRunStatus::Completed,
        PipelineRunStatus::Errored => pipeline_run::PipelineRunStatus::Errored,
    }
}

impl From<pipeline_run::Model> for PipelineRun {
    fn from(m: pipeline_run::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            created_at: m.created_at,
            status: entity_status_to_domain(m.status),
            pipeline_run_id: uuid_hex::from_hex(&m.pipeline_run_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            pipeline_name: m.pipeline_name,
            pipeline_id: uuid_hex::from_hex(&m.pipeline_id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            dataset_id: uuid_hex::from_hex_opt(m.dataset_id.as_deref()).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            run_info: m.run_info,
        }
    }
}

impl From<&PipelineRun> for pipeline_run::ActiveModel {
    fn from(r: &PipelineRun) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(r.id)),
            created_at: Set(r.created_at),
            status: Set(domain_status_to_entity(r.status.clone())),
            pipeline_run_id: Set(uuid_hex::to_hex(r.pipeline_run_id)),
            pipeline_name: Set(r.pipeline_name.clone()),
            pipeline_id: Set(uuid_hex::to_hex(r.pipeline_id)),
            dataset_id: Set(uuid_hex::to_hex_opt(r.dataset_id)),
            // Defensive: `run_info` is a `json` column, whose Postgres parser
            // rejects the `\u0000` escape `serde_json` emits for an embedded
            // NUL. The errored payloads that actually carry arbitrary text
            // (`run_info_for_errored`) do *not* reach this impl — the run
            // watchers call `PipelineRunRepository::log_pipeline_run`, which
            // builds the `ActiveModel` itself and sanitizes there. This impl is
            // reached only through `ops::pipeline_runs::create_pipeline_run`,
            // whose callers pass `run_info: None` today.
            run_info: Set(r.run_info.clone().map(sanitize_json)),
        }
    }
}

// ---------------------------------------------------------------------------
// Task run conversions
// ---------------------------------------------------------------------------

impl From<task_run::Model> for TaskRun {
    fn from(m: task_run::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            task_name: m.task_name,
            created_at: m.created_at,
            status: m.status,
            run_info: m.run_info,
        }
    }
}

impl From<&TaskRun> for task_run::ActiveModel {
    fn from(r: &TaskRun) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(r.id)),
            task_name: Set(r.task_name.clone()),
            created_at: Set(r.created_at),
            status: Set(r.status.clone()),
            // Defensive: `run_info` is a `json` column that Postgres will not
            // accept with a NUL inside. `task_runs::create_task_run`, the only
            // route to this impl, currently has no caller in the workspace, so
            // nothing exercises this today — it guards the public op for
            // whoever wires it up.
            run_info: Set(r.run_info.clone().map(sanitize_json)),
        }
    }
}

// ---------------------------------------------------------------------------
// Graph metrics conversions
// ---------------------------------------------------------------------------

impl From<graph_metrics::Model> for GraphMetrics {
    fn from(m: graph_metrics::Model) -> Self {
        Self {
            id: uuid_hex::from_hex(&m.id).expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),
            num_tokens: m.num_tokens,
            num_nodes: m.num_nodes,
            num_edges: m.num_edges,
            mean_degree: m.mean_degree,
            edge_density: m.edge_density,
            num_connected_components: m.num_connected_components,
            sizes_of_connected_components: m.sizes_of_connected_components,
            num_selfloops: m.num_selfloops,
            diameter: m.diameter,
            avg_shortest_path_length: m.avg_shortest_path_length,
            avg_clustering: m.avg_clustering,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<&GraphMetrics> for graph_metrics::ActiveModel {
    fn from(gm: &GraphMetrics) -> Self {
        Self {
            id: Set(uuid_hex::to_hex(gm.id)),
            num_tokens: Set(gm.num_tokens),
            num_nodes: Set(gm.num_nodes),
            num_edges: Set(gm.num_edges),
            mean_degree: Set(gm.mean_degree),
            edge_density: Set(gm.edge_density),
            num_connected_components: Set(gm.num_connected_components),
            sizes_of_connected_components: Set(gm.sizes_of_connected_components.clone()),
            num_selfloops: Set(gm.num_selfloops),
            diameter: Set(gm.diameter),
            avg_shortest_path_length: Set(gm.avg_shortest_path_length),
            avg_clustering: Set(gm.avg_clustering),
            created_at: Set(gm.created_at),
            updated_at: Set(gm.updated_at),
        }
    }
}

// ---------------------------------------------------------------------------
// NUL-byte sanitization (no database required)
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod sanitize_tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::ActiveValue;
    use serde_json::json;
    use uuid::Uuid;

    fn unwrap_set<T>(v: ActiveValue<T>) -> T
    where
        T: Into<sea_orm::Value>,
    {
        match v {
            ActiveValue::Set(inner) => inner,
            _ => panic!("conversion must produce ActiveValue::Set"),
        }
    }

    /// Provenance rows carry the serialized DataPoint — chunk text included —
    /// and the whole batch shares one transaction, so a single NUL would abort
    /// every row in it, not just its own.
    #[test]
    fn graph_node_active_model_strips_nul_bytes() {
        let node = GraphNode {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            label: Some("Nikola\u{0} Tesla".to_string()),
            node_type: "Doc\u{0}Chunk".to_string(),
            indexed_fields: json!(["te\u{0}xt"]),
            // Mirrors Python's `test_upsert_nodes_sanitizes_strings_before_insert`
            // (`cognee/tests/unit/modules/graph/test_relational_upserts.py:65`),
            // whose `details` carries both a dirty nested key and a nested list.
            attributes: Some(json!({
                "text": "page 1\u{0}page 2",
                "details": {"summa\u{0}ry": "Nested\u{0} value", "items": ["A\u{0}", "B"]},
                "count": 7,
            })),
            created_at: Utc::now(),
        };

        let model: node::ActiveModel = (&node).into();

        assert_eq!(unwrap_set(model.label), Some("Nikola Tesla".to_string()));
        assert_eq!(unwrap_set(model.node_type), "DocChunk");
        assert_eq!(unwrap_set(model.indexed_fields), json!(["text"]));
        assert_eq!(
            unwrap_set(model.attributes),
            Some(json!({
                "text": "page 1page 2",
                "details": {"summary": "Nested value", "items": ["A", "B"]},
                "count": 7,
            }))
        );
    }

    #[test]
    fn graph_edge_active_model_strips_nul_bytes() {
        let edge = GraphEdge {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            source_node_id: Uuid::new_v4(),
            destination_node_id: Uuid::new_v4(),
            relationship_name: "is\u{0}_a".to_string(),
            label: Some("la\u{0}bel".to_string()),
            // Mirrors Python's `test_upsert_edges_sanitizes_strings_before_insert`
            // (`test_relational_upserts.py:100`), which pins that dirty keys
            // nested inside `attributes` are rewritten too, not just values.
            attributes: Some(json!({
                "edge_text": "described\u{0}relationship",
                "no\u{0}te": "nul\u{0} byte",
                "nested": {"va\u{0}lue": "still\u{0} here"},
            })),
            created_at: Utc::now(),
        };

        let model: edge::ActiveModel = (&edge).into();

        assert_eq!(unwrap_set(model.relationship_name), "is_a");
        assert_eq!(unwrap_set(model.label), Some("label".to_string()));
        assert_eq!(
            unwrap_set(model.attributes),
            Some(json!({
                "edge_text": "describedrelationship",
                "note": "nul byte",
                "nested": {"value": "still here"},
            }))
        );
    }

    #[test]
    fn clean_provenance_rows_are_unchanged() {
        let attrs = json!({"text": "no nulls — just an em dash", "n": 1});
        let node = GraphNode {
            id: Uuid::new_v4(),
            slug: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            label: Some("Ada Lovelace".to_string()),
            node_type: "Person".to_string(),
            indexed_fields: json!(["text"]),
            attributes: Some(attrs.clone()),
            created_at: Utc::now(),
        };

        let model: node::ActiveModel = (&node).into();

        assert_eq!(unwrap_set(model.label), Some("Ada Lovelace".to_string()));
        assert_eq!(unwrap_set(model.node_type), "Person");
        assert_eq!(unwrap_set(model.attributes), Some(attrs));
    }
}
