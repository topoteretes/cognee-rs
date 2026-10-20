//! Data replacement API -- `update()`.
//!
//! Three-step pipeline: add the new data -> remove the old data -> re-cognify.
//! The old data is removed only once the new data is stored, so an add that
//! fails leaves the old data intact.
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
    /// ID of the data item that was replaced. When the new data has the same
    /// content (and so the same content-addressed id) the row is kept and
    /// only its graph and vector memory is dropped and rebuilt.
    pub deleted_data_id: Uuid,
    /// Remove phase summary: a soft delete of the old item from the dataset,
    /// or — when the new data re-added the same item — a memory-only delete.
    pub delete_result: DeleteResult,
    /// Newly added data items.
    pub new_data: Vec<Data>,
    /// Cognify phase result (optional -- only present when cognify was run).
    pub cognify_result: Option<CognifyResult>,
}

/// Replace data in a dataset: add new -> remove old -> re-cognify.
///
/// # Order of operations
///
/// First the preconditions, none of which mutate anything: the `write`
/// check, the node-set validation the add would apply, and a dry run of the
/// step-2 delete (the dataset exists and holds `data_id`). Then:
///
/// 1. The new data is added. Any add failure — a missing backend, an
///    unreadable file, a loader or storage error, the dataset vanishing —
///    returns here with the old data untouched.
/// 2. The old data is removed. If the add produced `data_id` itself (the
///    replacement's content equals the old content), the row is kept and only
///    its memory is dropped (a memory-only delete, which also clears its
///    cognify completion marker so step 3 rebuilds it under the new node set);
///    otherwise the old item is soft-deleted from the dataset as before.
/// 3. The added items are cognified.
///
/// This follows Python's ordering principle — the v1.6.4 `update()` never
/// deletes the document row; it drops the document's memory, re-adds pinned
/// to the same id, and re-cognifies, so "a re-add that fails leaves a document
/// to re-cognify rather than a document that is gone". Rust's ids are
/// content-addressed rather than pinned, so new content is a new row and the
/// old row has to go; doing that after the add keeps the same guarantee.
///
/// Remaining windows: if step 2 fails, both the new and the old item are in
/// the dataset (nothing is lost; the call errors and can be retried, or the
/// old item deleted). If step 3 fails, the new item is stored but not
/// cognified, as with any failed cognify. A replacement with the same content
/// keeps the stored row, so only its `node_set` is refreshed by the re-add —
/// a new label or other `external_metadata` keys on an identical-content
/// replacement are not written (the plain re-add refresh's existing scope).
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
/// names is `ApiError::Ingestion` (Python's `InvalidNodeSetError`). A
/// dataset that does not exist, or does not hold `data_id`, is the delete
/// service's error. All of these, and every add failure, are raised **before**
/// the old data is touched. Propagates errors from the remove and cognify
/// phases.
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

    // ── Refuse what the add would refuse, before anything is stored ─────────
    // `add_with_params` validates the call's `node_set` and every item's
    // `external_metadata["node_set"]` itself; checking here as well refuses a
    // bad name before the delete dry run below does any work.
    let params = AddParams {
        node_set,
        preferred_loaders,
        dataset_id: Some(dataset_id),
        importance_weight: None,
        incremental_loading,
    };
    cognee_ingestion::validate_add_inputs(&new_data, params.node_set.as_deref())
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;
    // Step 3 needs the relational database; refuse now rather than after the
    // old data is removed.
    if !new_data.is_empty() && db.is_none() {
        return Err(ApiError::Cognify(
            "cognify requires a relational database".to_string(),
        ));
    }

    // ── The old item is there to replace ─────────────────────────────────────
    // By the authorized `dataset_id`, not by name: a by-name scope is
    // re-resolved without a tenant predicate and could detach the item from a
    // same-named dataset in another tenant — not the one the write check just
    // authorized. Python's `update()` is driven by `dataset_id` throughout
    // (`cognee/api/v1/update/update.py:36@37f788bd2`); the add also targets it
    // (`AddParams::dataset_id`).
    //
    // A dry run of the step-2 delete (target resolution only, no mutation):
    // a missing dataset or an item the dataset does not hold fails here, as it
    // did when the delete ran first, rather than after the new data is added.
    delete_service
        .preview(&remove_old_data_request(dataset_id, data_id, false))
        .await?;

    // ── Step 1: Add the new data ─────────────────────────────────────────────
    // Before anything is removed: an add failure returns with the old data
    // intact. Python's `update()` gets the same guarantee by never deleting
    // the row (`forget(memory_only=True)` + a pinned re-add, update.py@f154b32).
    let data_items = add_pipeline
        .add_with_params(new_data, dataset_name, owner_id, tenant_id, &params)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Step 2: Remove the old data ──────────────────────────────────────────
    // When the replacement has the old content, the add re-attached the same
    // content-addressed row (refreshing its node set): keep it, and drop only
    // its memory — which also clears its cognify completion marker, so step 3
    // rebuilds it under the new node set rather than skipping it.
    let same_item = data_items.iter().any(|item| item.id == data_id);
    let delete_result = remove_old_data(delete_service, dataset_id, data_id, same_item).await?;

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

/// Step 2 of [`update`]: remove `data_id` from the dataset `dataset_id` — by
/// id (see the comment at the call site). `memory_only` keeps the row and
/// drops its graph and vector memory (the replacement re-added the same item).
async fn remove_old_data(
    delete_service: &DeleteService,
    dataset_id: Uuid,
    data_id: Uuid,
    memory_only: bool,
) -> Result<DeleteResult, ApiError> {
    Ok(delete_service
        .execute(&remove_old_data_request(dataset_id, data_id, memory_only))
        .await?)
}

/// The delete [`remove_old_data`] runs, also dry-run before the add.
fn remove_old_data_request(dataset_id: Uuid, data_id: Uuid, memory_only: bool) -> DeleteRequest {
    DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        // Python update() → datasets.delete_data defaults mode="soft" (datasets.py:147).
        mode: DeleteMode::Soft,
        memory_only,
    }
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

        remove_old_data(&delete_service, untenanted.id, data.id, false)
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

    /// The components [`update`] takes, over one in-memory SQLite database
    /// and mock stores.
    struct Fixture {
        db: Arc<cognee_database::DatabaseConnection>,
        storage: Arc<dyn StorageTrait>,
        graph_db: Arc<dyn GraphDBTrait>,
        vector_db: Arc<dyn VectorDB>,
        delete_service: DeleteService,
        add_pipeline: AddPipeline,
        llm: Arc<dyn Llm>,
        embedding: Arc<dyn EmbeddingEngine>,
        ontology: Arc<dyn OntologyResolver>,
        cognify_config: CognifyConfig,
        owner: Uuid,
        dataset_id: Uuid,
    }

    impl Fixture {
        async fn new() -> Self {
            use cognee_database::{DeleteDb, IngestDb, connect, initialize};
            use cognee_models::Dataset;
            let db = Arc::new(connect("sqlite::memory:").await.unwrap());
            initialize(&db).await.unwrap();
            let owner = Uuid::new_v4();
            let dataset = IngestDb::create_dataset(
                db.as_ref(),
                Dataset::new(
                    "x".into(),
                    owner,
                    None,
                    cognee_ingestion::generate_dataset_id("x", owner, None),
                ),
            )
            .await
            .unwrap();
            let storage: Arc<dyn StorageTrait> = Arc::new(cognee_storage::MockStorage::new());
            let graph_db: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
            let vector_db: Arc<dyn VectorDB> = Arc::new(cognee_vector::MockVectorDB::new());
            let delete_service =
                DeleteService::new(Arc::clone(&storage), db.clone() as Arc<dyn DeleteDb>);
            let add_pipeline = Self::pipeline(&db, &storage, &graph_db, &vector_db, true);
            Self {
                llm: Arc::new(
                    cognee_test_utils::MockLlm::empty()
                        .with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
                ),
                embedding: Arc::new(cognee_embedding::MockEmbeddingEngine::new(8)),
                ontology: Arc::new(cognee_ontology::NoOpOntologyResolver),
                cognify_config: CognifyConfig::default(),
                dataset_id: dataset.id,
                db,
                storage,
                graph_db,
                vector_db,
                delete_service,
                add_pipeline,
                owner,
            }
        }

        /// An `AddPipeline` over the fixture's stores; `with_graph_db: false`
        /// leaves the graph backend off, which `add_with_params` refuses with
        /// `MissingBackend`.
        fn pipeline(
            db: &Arc<cognee_database::DatabaseConnection>,
            storage: &Arc<dyn StorageTrait>,
            graph_db: &Arc<dyn GraphDBTrait>,
            vector_db: &Arc<dyn VectorDB>,
            with_graph_db: bool,
        ) -> AddPipeline {
            use cognee_database::IngestDb;
            let pipeline = AddPipeline::new(Arc::clone(storage), db.clone() as Arc<dyn IngestDb>)
                .with_thread_pool(Arc::new(
                    cognee_core::RayonThreadPool::with_default_threads().unwrap(),
                ))
                .with_vector_db(Arc::clone(vector_db))
                .with_database(db.clone());
            if with_graph_db {
                pipeline.with_graph_db(Arc::clone(graph_db))
            } else {
                pipeline
            }
        }

        /// Add `text` to the dataset, returning its row.
        async fn add(&self, text: &str) -> Data {
            let params = AddParams {
                dataset_id: Some(self.dataset_id),
                ..AddParams::default()
            };
            let mut added = self
                .add_pipeline
                .add_with_params(
                    vec![DataInput::Text(text.into())],
                    "x",
                    self.owner,
                    None,
                    &params,
                )
                .await
                .unwrap();
            assert_eq!(added.len(), 1);
            added.remove(0)
        }

        async fn update_with(
            &self,
            add_pipeline: &AddPipeline,
            data_id: Uuid,
            new_data: Vec<DataInput>,
            node_set: Option<Vec<String>>,
        ) -> Result<UpdateResult, ApiError> {
            update(
                data_id,
                new_data,
                self.dataset_id,
                "x",
                self.owner,
                None,
                node_set,
                None,
                true,
                None,
                &self.delete_service,
                add_pipeline,
                Arc::clone(&self.llm),
                Arc::clone(&self.storage),
                Arc::clone(&self.graph_db),
                Arc::clone(&self.vector_db),
                Arc::clone(&self.embedding),
                Some(self.db.clone() as Arc<dyn RelationalDb>),
                Arc::clone(&self.ontology),
                &self.cognify_config,
            )
            .await
        }

        /// The ids the dataset holds.
        async fn linked(&self) -> Vec<Uuid> {
            use cognee_database::DeleteDb;
            DeleteDb::get_dataset_data(self.db.as_ref(), self.dataset_id)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.id)
                .collect()
        }

        async fn row(&self, id: Uuid) -> Option<Data> {
            cognee_database::IngestDb::get_data(self.db.as_ref(), id)
                .await
                .unwrap()
        }
    }

    /// Review finding (#306): `update()` used to delete the old document
    /// before an add that could still fail, so any add failure lost the old
    /// data and stored nothing. The add now runs first; a failing add — here
    /// a file path that does not exist, and a pipeline missing its graph
    /// backend — leaves the old row in the dataset.
    #[tokio::test]
    async fn a_failing_add_leaves_the_old_data_intact() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;

        let missing = std::env::temp_dir()
            .join(format!("cognee-update-missing-{}.txt", Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        let err = fx
            .update_with(
                &fx.add_pipeline,
                old.id,
                vec![DataInput::FilePath(missing)],
                None,
            )
            .await
            .expect_err("a nonexistent file cannot be added");
        assert!(matches!(err, ApiError::Ingestion(_)), "got {err:?}");
        assert!(fx.row(old.id).await.is_some(), "the old row survives");
        assert_eq!(fx.linked().await, vec![old.id]);

        let no_graph = Fixture::pipeline(&fx.db, &fx.storage, &fx.graph_db, &fx.vector_db, false);
        let err = fx
            .update_with(
                &no_graph,
                old.id,
                vec![DataInput::Text("the new content".into())],
                None,
            )
            .await
            .expect_err("a pipeline with no graph backend refuses the add");
        assert!(
            matches!(&err, ApiError::Ingestion(msg) if msg.contains("graph_db")),
            "got {err:?}"
        );
        assert!(fx.row(old.id).await.is_some(), "the old row survives");
        assert_eq!(fx.linked().await, vec![old.id]);
    }

    /// An existing item the dataset does not hold is refused before the new
    /// data is added — as it was when the delete ran first.
    #[tokio::test]
    async fn an_item_the_dataset_does_not_hold_is_refused_before_the_add() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;
        let loc = format!("file:///tmp/{}.txt", Uuid::new_v4());
        let unattached = Data::builder(
            Uuid::new_v4(),
            "elsewhere.txt",
            loc.as_str(),
            loc.as_str(),
            "txt",
            "text/plain",
            "elsewhere-hash",
            fx.owner,
        )
        .build();
        cognee_database::IngestDb::create_data(fx.db.as_ref(), unattached.clone())
            .await
            .unwrap();

        let err = fx
            .update_with(
                &fx.add_pipeline,
                unattached.id,
                vec![DataInput::Text("never added".into())],
                None,
            )
            .await
            .expect_err("the dataset does not hold that item");
        assert!(matches!(err, ApiError::DeleteErr(_)), "got {err:?}");
        assert_eq!(fx.linked().await, vec![old.id], "nothing was added");
    }

    /// A normal update still replaces the data: the new item is added and
    /// cognified, the old one is gone from the dataset (and, held by no other
    /// dataset, deleted).
    #[tokio::test]
    async fn an_update_replaces_the_old_data() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;

        let result = fx
            .update_with(
                &fx.add_pipeline,
                old.id,
                vec![DataInput::Text("the new content".into())],
                None,
            )
            .await
            .expect("update succeeds");
        assert_eq!(result.deleted_data_id, old.id);
        assert_eq!(result.new_data.len(), 1);
        let new_id = result.new_data[0].id;
        assert_ne!(new_id, old.id);
        assert_eq!(result.delete_result.deleted_data, 1);
        assert!(result.cognify_result.is_some());

        assert_eq!(fx.linked().await, vec![new_id]);
        assert!(fx.row(old.id).await.is_none(), "the old row is deleted");
        assert!(fx.row(new_id).await.is_some());
    }

    /// A replacement with the old content has the old content-addressed id.
    /// The cleanup must not delete the row the add just re-attached: it is
    /// kept, re-tagged with the new node set, and its memory is rebuilt.
    #[tokio::test]
    async fn an_update_with_the_same_content_keeps_the_row() {
        let fx = Fixture::new().await;
        let old = fx.add("the same content").await;

        let result = fx
            .update_with(
                &fx.add_pipeline,
                old.id,
                vec![DataInput::Text("the same content".into())],
                Some(vec!["retagged".into()]),
            )
            .await
            .expect("update succeeds");
        assert_eq!(result.new_data.len(), 1);
        assert_eq!(result.new_data[0].id, old.id);
        assert_eq!(result.delete_result.deleted_data, 0, "memory-only");
        assert_eq!(result.delete_result.deleted_dataset_links, 0);

        assert_eq!(fx.linked().await, vec![old.id]);
        let row = fx.row(old.id).await.expect("the row is kept");
        assert_eq!(row.node_set.as_deref(), Some(r#"["retagged"]"#));
        let cognify = result.cognify_result.expect("cognify ran");
        assert!(
            !cognify.already_completed,
            "the memory-only delete cleared the completion marker, so the item is \
             re-cognified under its new node set rather than skipped"
        );
    }
}
