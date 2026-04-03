/// Shared SeaORM ↔ domain-type conversions and error helpers used across ops modules.
use chrono::Utc;
use cognee_models::{Data, Dataset};
use sea_orm::ActiveValue::Set;

use crate::entities::{
    artifact_reference, data, dataset, dataset_data, edge, graph_metrics, node, pipeline_run,
    query_log, result_log, task_run,
};
use crate::types::{
    ArtifactReference, DatabaseError, GraphEdge, GraphMetrics, GraphNode, PipelineRun,
    PipelineRunStatus, SearchHistoryEntry, SearchHistoryEntryType, TaskRun,
};

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

pub(crate) fn map_sea_err(e: sea_orm::DbErr) -> DatabaseError {
    match &e {
        sea_orm::DbErr::RecordNotFound(_) => DatabaseError::NotFound(e.to_string()),
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
            id: m.id,
            name: m.name,
            owner_id: m.owner_id,
            tenant_id: m.tenant_id,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<&Dataset> for dataset::ActiveModel {
    fn from(d: &Dataset) -> Self {
        Self {
            id: Set(d.id),
            name: Set(d.name.clone()),
            owner_id: Set(d.owner_id),
            tenant_id: Set(d.tenant_id),
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
            id: m.id,
            name: m.name,
            raw_data_location: m.raw_data_location,
            original_data_location: m.original_data_location,
            extension: m.extension,
            mime_type: m.mime_type,
            content_hash: m.content_hash,
            owner_id: m.owner_id,
            created_at: m.created_at,
            updated_at: m.updated_at,
            label: m.label,
            original_extension: m.original_extension,
            original_mime_type: m.original_mime_type,
            loader_engine: m.loader_engine,
            raw_content_hash: m.raw_content_hash,
            tenant_id: m.tenant_id,
            external_metadata: m.external_metadata,
            node_set: m.node_set,
            pipeline_status: m.pipeline_status,
            token_count: m.token_count,
            data_size: m.data_size,
            last_accessed: m.last_accessed,
        }
    }
}

impl From<&Data> for data::ActiveModel {
    fn from(d: &Data) -> Self {
        Self {
            id: Set(d.id),
            name: Set(d.name.clone()),
            raw_data_location: Set(d.raw_data_location.clone()),
            original_data_location: Set(d.original_data_location.clone()),
            extension: Set(d.extension.clone()),
            mime_type: Set(d.mime_type.clone()),
            content_hash: Set(d.content_hash.clone()),
            owner_id: Set(d.owner_id),
            created_at: Set(d.created_at),
            updated_at: Set(d.updated_at),
            label: Set(d.label.clone()),
            original_extension: Set(d.original_extension.clone()),
            original_mime_type: Set(d.original_mime_type.clone()),
            loader_engine: Set(d.loader_engine.clone()),
            raw_content_hash: Set(d.raw_content_hash.clone()),
            tenant_id: Set(d.tenant_id),
            external_metadata: Set(d.external_metadata.clone()),
            node_set: Set(d.node_set.clone()),
            pipeline_status: Set(d.pipeline_status.clone()),
            token_count: Set(d.token_count),
            data_size: Set(d.data_size),
            last_accessed: Set(d.last_accessed),
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
        dataset_id: Set(dataset_id),
        data_id: Set(data_id),
        created_at: Set(Utc::now()),
    }
}

// ---------------------------------------------------------------------------
// Search history conversions
// ---------------------------------------------------------------------------

pub(crate) fn query_model_to_history(m: query_log::Model) -> SearchHistoryEntry {
    SearchHistoryEntry {
        entry_id: m.id,
        query_id: m.id,
        entry_type: SearchHistoryEntryType::Query,
        content: m.query_text,
        query_type: Some(m.query_type),
        user_id: m.user_id,
        created_at: m.created_at,
    }
}

pub(crate) fn result_model_to_history(m: result_log::Model) -> SearchHistoryEntry {
    SearchHistoryEntry {
        entry_id: m.id,
        query_id: m.query_id,
        entry_type: SearchHistoryEntryType::Result,
        content: m.serialized_result,
        query_type: None,
        user_id: m.user_id,
        created_at: m.created_at,
    }
}

// ---------------------------------------------------------------------------
// Artifact reference conversions
// ---------------------------------------------------------------------------

impl From<artifact_reference::Model> for ArtifactReference {
    fn from(m: artifact_reference::Model) -> Self {
        Self {
            id: m.id,
            owner_id: m.owner_id,
            dataset_id: m.dataset_id,
            data_id: m.data_id,
            artifact_kind: m.artifact_kind,
            artifact_id: m.artifact_id,
            collection_name: m.collection_name,
            created_at: m.created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Graph node/edge conversions
// ---------------------------------------------------------------------------

impl From<node::Model> for GraphNode {
    fn from(m: node::Model) -> Self {
        Self {
            id: m.id,
            slug: m.slug,
            user_id: m.user_id,
            data_id: m.data_id,
            dataset_id: m.dataset_id,
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
        Self {
            id: Set(n.id),
            slug: Set(n.slug),
            user_id: Set(n.user_id),
            data_id: Set(n.data_id),
            dataset_id: Set(n.dataset_id),
            label: Set(n.label.clone()),
            node_type: Set(n.node_type.clone()),
            indexed_fields: Set(n.indexed_fields.clone()),
            attributes: Set(n.attributes.clone()),
            created_at: Set(n.created_at),
        }
    }
}

impl From<edge::Model> for GraphEdge {
    fn from(m: edge::Model) -> Self {
        Self {
            id: m.id,
            slug: m.slug,
            user_id: m.user_id,
            data_id: m.data_id,
            dataset_id: m.dataset_id,
            source_node_id: m.source_node_id,
            destination_node_id: m.destination_node_id,
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
            id: Set(e.id),
            slug: Set(e.slug),
            user_id: Set(e.user_id),
            data_id: Set(e.data_id),
            dataset_id: Set(e.dataset_id),
            source_node_id: Set(e.source_node_id),
            destination_node_id: Set(e.destination_node_id),
            relationship_name: Set(e.relationship_name.clone()),
            label: Set(e.label.clone()),
            attributes: Set(e.attributes.clone()),
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
            id: m.id,
            created_at: m.created_at,
            status: entity_status_to_domain(m.status),
            pipeline_run_id: m.pipeline_run_id,
            pipeline_name: m.pipeline_name,
            pipeline_id: m.pipeline_id,
            dataset_id: m.dataset_id,
            run_info: m.run_info,
        }
    }
}

impl From<&PipelineRun> for pipeline_run::ActiveModel {
    fn from(r: &PipelineRun) -> Self {
        Self {
            id: Set(r.id),
            created_at: Set(r.created_at),
            status: Set(domain_status_to_entity(r.status.clone())),
            pipeline_run_id: Set(r.pipeline_run_id),
            pipeline_name: Set(r.pipeline_name.clone()),
            pipeline_id: Set(r.pipeline_id),
            dataset_id: Set(r.dataset_id),
            run_info: Set(r.run_info.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Task run conversions
// ---------------------------------------------------------------------------

impl From<task_run::Model> for TaskRun {
    fn from(m: task_run::Model) -> Self {
        Self {
            id: m.id,
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
            id: Set(r.id),
            task_name: Set(r.task_name.clone()),
            created_at: Set(r.created_at),
            status: Set(r.status.clone()),
            run_info: Set(r.run_info.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Graph metrics conversions
// ---------------------------------------------------------------------------

impl From<graph_metrics::Model> for GraphMetrics {
    fn from(m: graph_metrics::Model) -> Self {
        Self {
            id: m.id,
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
            id: Set(gm.id),
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
