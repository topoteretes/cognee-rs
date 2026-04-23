//! Data replacement API -- `update()`.
//!
//! Three-step pipeline: delete old data -> re-add new data -> re-cognify.
//!
//! Equivalent to Python's `cognee.api.v1.update.update()`.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::DatabaseConnection;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::{Data, DataInput};
use cognee_ontology::OntologyResolver;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use uuid::Uuid;

use super::error::ApiError;

/// Result of an `update()` operation.
#[derive(Debug)]
pub struct UpdateResult {
    /// ID of the data item that was deleted.
    pub deleted_data_id: Uuid,
    /// Delete phase summary.
    pub delete_result: DeleteResult,
    /// Newly added data items.
    pub new_data: Vec<Data>,
    /// Cognify phase result (optional -- only present when cognify was run).
    pub cognify_result: Option<CognifyResult>,
}

/// Replace data in a dataset: delete old -> re-add new -> re-cognify.
///
/// # Arguments
/// * `data_id` - ID of the data item to replace.
/// * `new_data` - Replacement data inputs.
/// * `dataset_name` - Dataset to operate within.
/// * `owner_id` / `tenant_id` - Ownership context.
/// * `delete_service` - Pre-configured [`DeleteService`].
/// * `add_pipeline` - Ingestion pipeline.
/// * `llm` .. `cognify_config` - Components for the cognify phase.
///
/// # Errors
/// Propagates errors from delete, add, or cognify phases.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    data_id: Uuid,
    new_data: Vec<DataInput>,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    delete_service: &DeleteService,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<UpdateResult, ApiError> {
    // Step 1: Delete old data.
    let delete_request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id,
            data_id,
            dataset_name: Some(dataset_name.to_string()),
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Hard,
    };
    let delete_result = delete_service.execute(&delete_request).await?;

    // Step 2: Re-add new data.
    let data_items = add_pipeline
        .add(new_data, dataset_name, owner_id, tenant_id)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // Step 3: Re-cognify (if data was added).
    let cognify_result = if !data_items.is_empty() {
        // Generate a dataset ID from the dataset name (deterministic).
        let dataset_id = cognee_ingestion::generate_dataset_id(dataset_name, owner_id, tenant_id);

        let result = cognify(
            data_items.clone(),
            dataset_id,
            Some(owner_id),
            tenant_id,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            db,
            ontology_resolver,
            cognify_config,
        )
        .await
        .map_err(|e| ApiError::Cognify(e.to_string()))?;
        Some(result)
    } else {
        None
    };

    Ok(UpdateResult {
        deleted_data_id: data_id,
        delete_result,
        new_data: data_items,
        cognify_result,
    })
}
