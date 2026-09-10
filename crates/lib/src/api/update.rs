//! Data replacement API -- `update()`.
//!
//! Three-step pipeline: delete old data -> re-add new data -> re-cognify.
//!
//! Equivalent to Python's `cognee.api.v1.update.update()`.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::{
    AclDb, DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
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
/// * `dataset_id` - Explicit dataset UUID (no re-derivation from name).
/// * `dataset_name` - Dataset name (still required for delete scope).
/// * `owner_id` / `tenant_id` - Ownership context.
/// * `node_set` - Optional graph node identifiers for access-control grouping.
/// * `preferred_loaders` - Optional MIME-type-to-loader-name overrides.
/// * `incremental_loading` - When `true`, skip re-adding content already present.
/// * `acl_db` - Optional ACL backend; when provided, a `write` permission check
///   is performed before any mutation.
/// * `delete_service` - Pre-configured [`DeleteService`].
/// * `add_pipeline` - Ingestion pipeline.
/// * `llm` .. `cognify_config` - Components for the cognify phase.
///
/// # Errors
/// Returns `ApiError::InvalidArgument` when `acl_db` is set and the caller
/// lacks `write` on the dataset — **not** `PermissionDenied`, which this
/// doc previously claimed. The variant exists (`api::error::ApiError`) and is
/// arguably the right one, but changing it is a caller-visible break, so the
/// doc is corrected to the shipped behaviour and the shape is pinned by
/// `missing_write_grant_is_denied`. Propagates errors from delete, add, or
/// cognify phases.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    data_id: Uuid,
    new_data: Vec<DataInput>,
    dataset_id: Uuid,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    node_set: Option<Vec<String>>,
    preferred_loaders: Option<HashMap<String, String>>,
    incremental_loading: bool,
    acl_db: Option<&dyn AclDb>,
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
    // ── Permission gate ───────────────────────────────────────────────────────
    if let Some(acl) = acl_db {
        require_write_permission(acl, owner_id, dataset_id).await?;
    }

    // ── Step 1: Delete old data ───────────────────────────────────────────────
    let delete_request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id,
            data_id,
            dataset_name: Some(dataset_name.to_string()),
            delete_dataset_if_empty: false,
        },
        // Python update() → datasets.delete_data defaults mode="soft" (datasets.py:147).
        mode: DeleteMode::Soft,
        memory_only: false,
    };
    let delete_result = delete_service.execute(&delete_request).await?;

    // ── Step 2: Re-add new data ───────────────────────────────────────────────
    let params = AddParams {
        node_set,
        preferred_loaders,
        dataset_id: Some(dataset_id),
        importance_weight: None,
        incremental_loading,
    };
    let data_items = add_pipeline
        .add_with_params(new_data, dataset_name, owner_id, tenant_id, &params)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Step 3: Re-cognify (if data was added) ───────────────────────────────
    let cognify_result = if !data_items.is_empty() {
        // OSS build has no DB-backed user lookup (the `users` table is owned
        // by the closed cloud build), so we always fall back to `None`.
        // `cognify()` then uses `user_id.to_string()` as the provenance
        // stamp.
        let user_email: Option<String> = None;

        let database = db.clone().ok_or_else(|| {
            ApiError::Cognify("cognify requires a DatabaseConnection".to_string())
        })?;
        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads()
                .map_err(|e| ApiError::Cognify(format!("failed to construct thread pool: {e}")))?,
        );

        // Gap 08-07: persist the four-state `pipeline_runs` trail.
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

        // Apply `incremental_loading` flag on top of the caller-provided config.
        let effective_cognify_config;
        let cognify_config_ref = if incremental_loading != cognify_config.incremental_loading {
            effective_cognify_config = cognify_config
                .clone()
                .with_incremental_loading(incremental_loading);
            &effective_cognify_config
        } else {
            cognify_config
        };

        let result = cognify(
            data_items.clone(),
            dataset_id,
            Some(owner_id),
            user_email,
            tenant_id,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            database,
            pipeline_run_repo,
            thread_pool,
            ontology_resolver,
            cognify_config_ref,
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

/// Deny the update unless `owner_id` holds `write` on `dataset_id`.
///
/// Uses the roles-aware check: Python authorizes `update()` through the
/// dataset ACL (`ingest_data` → `get_specific_user_permission_datasets(user.id,
/// "write", [dataset_id])`), which walks the caller's tenant and role grants
/// as well as direct ones. A collaborator who holds `write` only via a role
/// must therefore be admitted here too.
async fn require_write_permission(
    acl: &dyn AclDb,
    owner_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    let permitted = acl
        .has_permission_with_roles(owner_id, dataset_id, "write")
        .await
        .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
    if permitted {
        Ok(())
    } else {
        Err(ApiError::InvalidArgument(
            "write permission denied on dataset".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_test_utils::MockAclDb;

    /// Scenario: the caller holds `write` on the dataset only through a role.
    /// Expected: admitted — Python's `get_specific_user_permission_datasets`
    /// resolves role grants, so a direct-grant-only check would wrongly deny.
    #[tokio::test]
    async fn write_grant_via_role_is_admitted() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let role = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        // A role only confers grants within a tenant the user belongs to
        // (AclDb's contract, and Python nests the role walk in the tenant loop).
        acl.add_user_to_tenant(user, Uuid::new_v4());
        acl.add_user_to_role(user, role);
        assert!(acl.grant_permission(role, dataset, "write").await.is_ok());

        assert!(
            require_write_permission(&acl, user, dataset).await.is_ok(),
            "a role-held write grant must satisfy the update gate"
        );
    }

    /// Scenario: the caller holds `write` directly.
    /// Expected: admitted — behaviour unchanged by the roles-aware switch.
    #[tokio::test]
    async fn direct_write_grant_is_admitted() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        assert!(acl.grant_permission(user, dataset, "write").await.is_ok());

        assert!(require_write_permission(&acl, user, dataset).await.is_ok());
    }

    /// Scenario: the caller holds `read` (directly and via a role) but not
    /// `write`. Expected: denied with `InvalidArgument`, the pre-existing
    /// error shape for this gate.
    #[tokio::test]
    async fn missing_write_grant_is_denied() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let role = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        // A role only confers grants within a tenant the user belongs to
        // (AclDb's contract, and Python nests the role walk in the tenant loop).
        acl.add_user_to_tenant(user, Uuid::new_v4());
        acl.add_user_to_role(user, role);
        assert!(acl.grant_permission(user, dataset, "read").await.is_ok());
        assert!(acl.grant_permission(role, dataset, "read").await.is_ok());

        let err = require_write_permission(&acl, user, dataset).await;
        assert!(
            matches!(err, Err(ApiError::InvalidArgument(_))),
            "got {err:?}"
        );
    }
}
