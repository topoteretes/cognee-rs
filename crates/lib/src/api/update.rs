//! Data replacement API -- `update()`.
//!
//! Three-step pipeline: delete old data -> re-add new data -> re-cognify.
//!
//! Equivalent to Python's `cognee.api.v1.update.update()`.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::{AclDb, PipelineRunRepository, RelationalDb};
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
/// * `dataset_name` - Dataset name: only the add pipeline's name/lock key; both
///   the delete and the add target `dataset_id`.
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
/// Returns `ApiError::PermissionDenied` when `acl_db` is set and the caller
/// lacks `write` on the dataset **in `tenant_id`** — no grant and a grant only
/// on another tenant's dataset answer the same, like Python's single
/// `PermissionDeniedError` (403,
/// `cognee/modules/users/exceptions/exceptions.py:42-51@37f788bd2`). A failing
/// ACL backend is `ApiError::InvalidArgument`. A `node_set` (the call's, or
/// one in an item's `external_metadata`) that is not a list of non-empty
/// names is `ApiError::Ingestion` (Python's `InvalidNodeSetError`), raised
/// **before** the old data is touched. Propagates errors from delete, add, or
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
    db: Option<Arc<dyn RelationalDb>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<UpdateResult, ApiError> {
    // ── Permission gate ───────────────────────────────────────────────────────
    if let Some(acl) = acl_db {
        require_write_permission(acl, owner_id, tenant_id, dataset_id).await?;
    }

    // ── Refuse what the add would refuse, before anything is deleted ─────────
    // Step 2's `add_with_params` validates the call's `node_set` and every
    // item's `external_metadata["node_set"]` before it stores anything — but
    // by then step 1 has already detached (and, with no other dataset holding
    // it, deleted) the old document, so a bad name would destroy the old data
    // and store nothing. Python's `update()` cannot lose data this way: its
    // `forget(memory_only=True)` keeps the row, and `ingest_data` validates
    // before storing. Validate here, where the old data is still intact.
    let params = AddParams {
        node_set,
        preferred_loaders,
        dataset_id: Some(dataset_id),
        importance_weight: None,
        incremental_loading,
    };
    cognee_ingestion::validate_add_inputs(&new_data, params.node_set.as_deref())
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Step 1: Delete old data ───────────────────────────────────────────────
    // By the authorized `dataset_id`, not by name: a by-name scope is
    // re-resolved without a tenant predicate and could detach the item from a
    // same-named dataset in another tenant — not the one the write check just
    // authorized. Python's `update()` is driven by `dataset_id` throughout
    // (`cognee/api/v1/update/update.py:36@37f788bd2`); step 2 already adds by
    // id (`AddParams::dataset_id`).
    let delete_result = delete_old_data(delete_service, dataset_id, data_id).await?;

    // ── Step 2: Re-add new data ───────────────────────────────────────────────
    let data_items = add_pipeline
        .add_with_params(new_data, dataset_name, owner_id, tenant_id, &params)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Step 3: Re-cognify (if data was added) ───────────────────────────────
    let cognify_result = if !data_items.is_empty() {
        // OSS build has no DB-backed user lookup (there is no `users` table
        // in the OSS schema), so we always fall back to `None`.
        // `cognify()` then uses `user_id.to_string()` as the provenance
        // stamp.
        let user_email: Option<String> = None;

        let database = db.clone().ok_or_else(|| {
            ApiError::Cognify("cognify requires a relational database".to_string())
        })?;
        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads()
                .map_err(|e| ApiError::Cognify(format!("failed to construct thread pool: {e}")))?,
        );

        // Gap 08-07: persist the four-state `pipeline_runs` trail.
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::clone(&database).pipeline_runs();

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

/// Step 1 of [`update`]: detach/delete `data_id` from the dataset
/// `dataset_id` — by id (see the comment at the call site).
async fn delete_old_data(
    delete_service: &DeleteService,
    dataset_id: Uuid,
    data_id: Uuid,
) -> Result<DeleteResult, ApiError> {
    let delete_request = DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        // Python update() → datasets.delete_data defaults mode="soft" (datasets.py:147).
        mode: DeleteMode::Soft,
        memory_only: false,
    };
    Ok(delete_service.execute(&delete_request).await?)
}

/// Deny the update unless `owner_id` holds `write` on `dataset_id` in
/// `tenant_id`.
///
/// Uses the roles-aware, tenant-filtered check: Python authorizes `update()`
/// through the dataset ACL (`ingest_data` →
/// `get_specific_user_permission_datasets(user.id, "write", [dataset_id])`,
/// `cognee/tasks/ingestion/ingest_data.py:212`,
/// `cognee/modules/users/permissions/methods/get_specific_user_permission_datasets.py:27-38`
/// @37f788bd2), which walks the caller's tenant and role grants as well as
/// direct ones and then keeps only datasets with `tenant_id == user.tenant_id`
/// (`get_all_user_permission_datasets.py:49-53@37f788bd2`). A collaborator who
/// holds `write` only via a role is admitted; a grant on another tenant's
/// dataset is not.
async fn require_write_permission(
    acl: &dyn AclDb,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    let permitted = acl
        .has_permission_in_tenant(owner_id, dataset_id, "write", tenant_id)
        .await
        .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
    if permitted {
        Ok(())
    } else {
        Err(ApiError::PermissionDenied(
            "write permission denied on dataset".to_string(),
        ))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
            require_write_permission(&acl, user, None, dataset)
                .await
                .is_ok(),
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

        assert!(
            require_write_permission(&acl, user, None, dataset)
                .await
                .is_ok()
        );
    }

    /// Scenario: the caller holds `read` (directly and via a role) but not
    /// `write`. Expected: denied with `PermissionDenied` (Python's
    /// `PermissionDeniedError`, 403).
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

        let err = require_write_permission(&acl, user, None, dataset).await;
        assert!(
            matches!(err, Err(ApiError::PermissionDenied(_))),
            "got {err:?}"
        );
    }

    /// Scenario: `write` granted on a dataset in tenant T2; the caller acts in
    /// T1 (or tenant-less). Expected: the same `PermissionDenied` as no grant
    /// — Python intersects with the tenant-filtered set
    /// (`get_all_user_permission_datasets.py:49-53@37f788bd2`). In T2 the same
    /// grant is admitted.
    #[tokio::test]
    async fn write_grant_in_another_tenant_is_denied() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        acl.set_dataset_tenant(dataset, t2);
        assert!(acl.grant_permission(user, dataset, "write").await.is_ok());

        for tenant in [Some(t1), None] {
            let err = require_write_permission(&acl, user, tenant, dataset).await;
            assert!(
                matches!(err, Err(ApiError::PermissionDenied(_))),
                "{tenant:?}: got {err:?}"
            );
        }
        assert!(
            require_write_permission(&acl, user, Some(t2), dataset)
                .await
                .is_ok()
        );
    }

    /// Review loop 2 #1. One data item is attached to `"x"` in T1 (seeded
    /// first) and to the tenant-less `"x"`. Updating it in the tenant-less
    /// dataset must detach it there only. The old by-name delete step
    /// re-resolved `"x"` with no tenant predicate and could hit the T1 row.
    #[tokio::test]
    async fn delete_step_targets_the_authorized_dataset_id_not_a_same_named_row() {
        use cognee_database::{DeleteDb, IngestDb, connect, initialize};
        use cognee_models::Dataset;
        let db = Arc::new(connect("sqlite::memory:").await.unwrap());
        initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let ingest: &dyn IngestDb = db.as_ref();
        let tenanted = ingest
            .create_dataset(Dataset::new("x".into(), owner, Some(t1), Uuid::new_v4()))
            .await
            .unwrap();
        let untenanted = ingest
            .create_dataset(Dataset::new("x".into(), owner, None, Uuid::new_v4()))
            .await
            .unwrap();
        let loc = format!("file:///tmp/{}.txt", Uuid::new_v4());
        let data = Data::builder(
            Uuid::new_v4(),
            "d.txt",
            loc.as_str(),
            loc.as_str(),
            "txt",
            "text/plain",
            "hash",
            owner,
        )
        .build();
        ingest.create_data(data.clone()).await.unwrap();
        for ds in [tenanted.id, untenanted.id] {
            ingest.attach_data_to_dataset(ds, data.id).await.unwrap();
        }
        let delete_service = DeleteService::new(
            Arc::new(cognee_storage::MockStorage::new()) as Arc<dyn StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        );

        delete_old_data(&delete_service, untenanted.id, data.id)
            .await
            .unwrap();

        let ids = |v: Vec<Data>| v.into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert!(
            ids(DeleteDb::get_dataset_data(db.as_ref(), untenanted.id)
                .await
                .unwrap())
            .is_empty(),
            "detached from the authorized (tenant-less) dataset"
        );
        assert_eq!(
            ids(DeleteDb::get_dataset_data(db.as_ref(), tenanted.id)
                .await
                .unwrap()),
            vec![data.id],
            "the same-named T1 dataset keeps its link"
        );
    }

    /// SDK-1129 follow-up. The add step refuses a bad `node_set`, but the
    /// delete step ran first, so an invalid name used to destroy the old
    /// document and store nothing. Python's `update()` never loses data that
    /// way. The name is now refused before the delete, with the old data
    /// — its row, its dataset link — intact.
    #[tokio::test]
    async fn an_invalid_node_set_is_refused_before_the_old_data_is_deleted() {
        use cognee_database::{DeleteDb, IngestDb, connect, initialize};
        use cognee_models::Dataset;
        let db = Arc::new(connect("sqlite::memory:").await.unwrap());
        initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let ingest: &dyn IngestDb = db.as_ref();
        let dataset = ingest
            .create_dataset(Dataset::new("x".into(), owner, None, Uuid::new_v4()))
            .await
            .unwrap();
        let loc = format!("file:///tmp/{}.txt", Uuid::new_v4());
        let old = Data::builder(
            Uuid::new_v4(),
            "old.txt",
            loc.as_str(),
            loc.as_str(),
            "txt",
            "text/plain",
            "old-hash",
            owner,
        )
        .build();
        ingest.create_data(old.clone()).await.unwrap();
        ingest
            .attach_data_to_dataset(dataset.id, old.id)
            .await
            .unwrap();

        let storage: Arc<dyn StorageTrait> = Arc::new(cognee_storage::MockStorage::new());
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let vector_db: Arc<dyn VectorDB> = Arc::new(cognee_vector::MockVectorDB::new());
        let delete_service =
            DeleteService::new(Arc::clone(&storage), db.clone() as Arc<dyn DeleteDb>);
        let add_pipeline = AddPipeline::new(Arc::clone(&storage), db.clone() as Arc<dyn IngestDb>)
            .with_thread_pool(Arc::new(
                cognee_core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(db.clone());
        let llm: Arc<dyn Llm> = Arc::new(cognee_test_utils::MockLlm::empty());
        let embedding: Arc<dyn EmbeddingEngine> =
            Arc::new(cognee_embedding::MockEmbeddingEngine::new(8));
        let ontology: Arc<dyn OntologyResolver> = Arc::new(cognee_ontology::NoOpOntologyResolver);
        let cognify_config = CognifyConfig::default();

        // The call's own node set, and an item-level one in `external_metadata`.
        let bad_calls: Vec<(Vec<DataInput>, Option<Vec<String>>)> = vec![
            (
                vec![DataInput::Text("replacement".into())],
                Some(vec!["alpha".into(), " ".into()]),
            ),
            (
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("replacement".into())),
                    label: "item".into(),
                    external_metadata: Some(r#"{"node_set": "alpha"}"#.into()),
                }],
                None,
            ),
        ];
        for (new_data, node_set) in bad_calls {
            let err = update(
                old.id,
                new_data,
                dataset.id,
                "x",
                owner,
                None,
                node_set,
                None,
                true,
                None,
                &delete_service,
                &add_pipeline,
                Arc::clone(&llm),
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding),
                Some(db.clone() as Arc<dyn RelationalDb>),
                Arc::clone(&ontology),
                &cognify_config,
            )
            .await
            .expect_err("a bad node set is refused");
            assert!(
                matches!(&err, ApiError::Ingestion(msg) if msg.contains("Invalid node_set")),
                "got {err:?}"
            );

            // The old document is untouched: still a row, still in the dataset.
            assert!(
                IngestDb::get_data(db.as_ref(), old.id)
                    .await
                    .unwrap()
                    .is_some(),
                "the old row survives"
            );
            let linked: Vec<Uuid> = DeleteDb::get_dataset_data(db.as_ref(), dataset.id)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.id)
                .collect();
            assert_eq!(linked, vec![old.id], "the old row is still in the dataset");
        }
    }
}
