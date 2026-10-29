//! Data replacement API -- `update()`.
//!
//! Resolve the target id -> drop the document's memory (the row stays) ->
//! re-add pinned to the same id -> re-cognify.
//!
//! Equivalent to Python's `cognee.api.v1.update.update()` full-rebuild path
//! (`update.py:215-337`): the document KEEPS its `data_id`, owner and
//! pre-fork `legacy_id` by construction, because the row is never deleted.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::{AclDb, DeleteDb, PipelineRunRepository, RelationalDb};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::OntologyResolver;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use uuid::Uuid;

use super::error::ApiError;

/// Result of an `update()` operation (Python's `UpdateResult`, `data_id`
/// first: the id the document keeps).
#[derive(Debug)]
pub struct UpdateResult {
    /// The document's id: the row the requested id resolved to (the exact id,
    /// or the row whose pre-fork `legacy_id` it was). It is unchanged by the
    /// update: the row is refreshed in place, never replaced.
    pub data_id: Uuid,
    /// The memory-only delete that dropped the document's graph and vector
    /// memory (and its cognify completion marker) before the re-add.
    pub delete_result: DeleteResult,
    /// Cognify phase result (`None` only when cognify did not run).
    pub cognify_result: Option<CognifyResult>,
}

/// Replace one document of a dataset, keeping its data id: resolve ->
/// memory-only forget -> pinned add -> re-cognify.
///
/// # Order of operations
///
/// First the preconditions, none of which mutate anything: the `write`
/// check, exactly one replacement item, the node-set validation the add would
/// apply, and the target: `data_id` is resolved in `dataset_id` (the exact id,
/// else the row whose `legacy_id` it is; [`DeleteDb::resolve_data_id`]) and an
/// id that resolves to nothing is [`ApiError::UpdateTargetNotFound`] — a stale
/// or mistyped id never silently creates a second document. A dry run of the
/// step-1 delete also checks that the dataset exists and holds the row. Then:
///
/// 1. The document's memory (graph nodes, edges, vectors, its cognify
///    completion marker) is dropped; the row and its stored files stay.
/// 2. The replacement is added **pinned** to the resolved id
///    ([`AddParams::pinned_id`]): the content-hash lookup is skipped and the
///    row is refreshed in place with the new content, name and metadata (its
///    cognify marker cleared if the content changed).
/// 3. The document is cognified.
///
/// This is Python's order. A re-add that fails after step 1 leaves a document
/// with no memory that the next cognify rebuilds from its stored content, not
/// a document that is gone.
///
/// # Arguments
/// * `data_id` - ID of the data item to replace (the id the caller holds: the
///   current one or a pre-fork `legacy_id`).
/// * `new_data` - The replacement: exactly one input.
/// * `dataset_id` - Explicit dataset UUID (no re-derivation from name).
/// * `dataset_name` - Dataset name: only the add pipeline's name/lock key; both
///   the delete and the add target `dataset_id`.
/// * `owner_id` / `tenant_id` - Ownership context.
/// * `node_set` - Optional graph node identifiers for access-control grouping.
/// * `preferred_loaders` - Optional MIME-type-to-loader-name overrides.
/// * `incremental_loading` - Applied to the cognify phase.
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
/// names, or a replacement that is not exactly one item, is
/// `ApiError::Ingestion` (Python's `InvalidNodeSetError` / `IngestionError`).
/// A `data_id` unknown in the dataset is `ApiError::UpdateTargetNotFound`
/// (404). A dataset that does not exist is the delete service's error. All of
/// these are raised **before** anything is touched. Propagates errors from the
/// forget, add and cognify phases.
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

    // ── Refuse what the add would refuse, before anything is touched ────────
    // `update()` replaces exactly one document (`update.py:201-206`).
    if new_data.len() != 1 {
        return Err(ApiError::Ingestion(format!(
            "update() replaces exactly one document; got a list of {} items.",
            new_data.len()
        )));
    }
    // The document is resolved, and cognify run, through the relational
    // database; refuse now rather than after the memory is dropped.
    let database = db
        .clone()
        .ok_or_else(|| ApiError::Cognify("update requires a relational database".to_string()))?;

    // `add_with_params` validates the call's `node_set` and every item's
    // `external_metadata["node_set"]` itself; checking here as well refuses a
    // bad name before the memory is dropped.
    cognee_ingestion::validate_add_inputs(&new_data, node_set.as_deref())
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Resolve the target ───────────────────────────────────────────────────
    // The document KEEPS its id through updates. The incoming id (exact, then
    // the pre-fork `legacy_id`) is resolved once, up front. An id that
    // resolves to nothing is a caller error, not a create: ids are random
    // uuid4s, so a stale or mistyped id can never match, and silently creating
    // a second document would hide the mistake as duplication.
    let pinned_id = DeleteDb::resolve_data_id(database.as_ref(), dataset_id, data_id)
        .await
        .map_err(|e| ApiError::InvalidArgument(format!("failed to resolve data {data_id}: {e}")))?
        .ok_or(ApiError::UpdateTargetNotFound {
            data_id,
            dataset_id,
        })?;

    // A dry run of the step-1 delete (target resolution only, no mutation) by
    // the authorized `dataset_id`, not by name: a by-name scope is re-resolved
    // without a tenant predicate and could reach a same-named dataset in
    // another tenant. A missing dataset, or an item the dataset does not hold,
    // fails here, before the memory is dropped.
    delete_service
        .preview(&forget_memory_request(dataset_id, pinned_id))
        .await?;

    // ── Step 1: drop the document's memory; the row and its files stay ───────
    let delete_result = delete_service
        .execute(&forget_memory_request(dataset_id, pinned_id))
        .await?;

    // ── Step 2: re-add, pinned to the document's own id ──────────────────────
    let params = AddParams {
        node_set,
        preferred_loaders,
        dataset_id: Some(dataset_id),
        importance_weight: None,
        incremental_loading,
        pinned_id: Some(pinned_id),
    };
    let data_items = add_pipeline
        .add_with_params(new_data, dataset_name, owner_id, tenant_id, &params)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    // ── Step 3: Re-cognify ───────────────────────────────────────────────────
    let cognify_result = if !data_items.is_empty() {
        // OSS build has no DB-backed user lookup (there is no `users` table
        // in the OSS schema), so we always fall back to `None`.
        // `cognify()` then uses `user_id.to_string()` as the provenance
        // stamp.
        let user_email: Option<String> = None;

        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads()
                .map_err(|e| ApiError::Cognify(format!("failed to construct thread pool: {e}")))?,
        );

        // Gap 08-07: persist the `pipeline_runs` trail.
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
        data_id: pinned_id,
        delete_result,
        cognify_result,
    })
}

/// The memory-only delete [`update`] runs as step 1, also dry-run before it:
/// the document's graph and vector memory and its cognify completion marker go,
/// the row and its files stay (Python's `forget(memory_only=True)`).
fn forget_memory_request(dataset_id: Uuid, data_id: Uuid) -> DeleteRequest {
    DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Soft,
        memory_only: true,
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
    use cognee_models::Data;
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
    /// first) and to the tenant-less `"x"`. Deleting it in the tenant-less
    /// dataset (by id, as `update()`'s forget does) must detach it there only.
    /// A by-name delete re-resolved `"x"` with no tenant predicate and could
    /// hit the T1 row.
    #[tokio::test]
    async fn delete_targets_the_authorized_dataset_id_not_a_same_named_row() {
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
        ingest
            .attach_data_to_dataset(untenanted.id, data.id)
            .await
            .unwrap();
        // Rows are dataset-scoped: the same content in the same-named T1
        // dataset is a row of its own.
        let t1_data = Data {
            id: Uuid::new_v4(),
            ..data.clone()
        };
        ingest.create_data(t1_data.clone()).await.unwrap();
        ingest
            .attach_data_to_dataset(tenanted.id, t1_data.id)
            .await
            .unwrap();
        let delete_service = DeleteService::new(
            Arc::new(cognee_storage::MockStorage::new()) as Arc<dyn StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        );

        delete_service
            .execute(&DeleteRequest {
                scope: DeleteScope::DataInDataset {
                    dataset_id: untenanted.id,
                    data_id: data.id,
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
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
            vec![t1_data.id],
            "the same-named T1 dataset keeps its row"
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

    /// Python's order (`update.py:309-337`): the memory is dropped, then the
    /// pinned re-add runs, so an add that fails — here a file path that does
    /// not exist, and a pipeline missing its graph backend — leaves the
    /// document's row in the dataset (to be re-cognified), never a document
    /// that is gone.
    #[tokio::test]
    async fn a_failing_add_leaves_the_row_in_the_dataset() {
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

    /// An id the dataset does not hold (an existing row attached to no
    /// dataset) resolves to nothing: `UpdateTargetNotFound` (Python's 404),
    /// before anything is dropped or added.
    #[tokio::test]
    async fn an_id_the_dataset_does_not_hold_is_not_found() {
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
        assert!(
            matches!(&err, ApiError::UpdateTargetNotFound { data_id, .. } if *data_id == unattached.id),
            "got {err:?}"
        );
        assert_eq!(fx.linked().await, vec![old.id], "nothing was added");
    }

    /// A normal update keeps the document's id: the row is refreshed in
    /// place with the new content, and still the dataset's only row.
    #[tokio::test]
    async fn an_update_keeps_the_data_id() {
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
        assert_eq!(result.data_id, old.id);
        assert_eq!(
            result.delete_result.deleted_data, 0,
            "memory-only: no row deleted"
        );
        assert!(result.cognify_result.is_some());

        assert_eq!(fx.linked().await, vec![old.id], "one row, same id");
        let row = fx.row(old.id).await.expect("the row is kept");
        assert_ne!(row.content_hash, old.content_hash, "the content changed");
        assert_eq!(row.dataset_id, Some(fx.dataset_id));
        assert_eq!(row.owner_id, fx.owner);
    }

    /// A pre-fork id resolves to the row it forked into: the update acts on
    /// it, and reports the row's id.
    #[tokio::test]
    async fn a_legacy_id_resolves_to_the_forked_row() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;
        let legacy = Uuid::new_v4();
        let mut forked = fx.row(old.id).await.expect("row");
        forked.legacy_id = Some(legacy);
        cognee_database::IngestDb::update_data(fx.db.as_ref(), forked)
            .await
            .unwrap();

        let result = fx
            .update_with(
                &fx.add_pipeline,
                legacy,
                vec![DataInput::Text("the new content".into())],
                None,
            )
            .await
            .expect("update by the legacy id succeeds");
        assert_eq!(
            result.data_id, old.id,
            "the resolved id, not the legacy one"
        );
        assert_eq!(fx.linked().await, vec![old.id]);
        let row = fx.row(old.id).await.expect("row");
        assert_eq!(row.legacy_id, Some(legacy), "the legacy id is kept");
    }

    /// An id that is neither a row id nor a legacy id of the dataset is a
    /// caller error (404), not a create.
    #[tokio::test]
    async fn an_unknown_id_is_not_found_and_creates_nothing() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;
        let err = fx
            .update_with(
                &fx.add_pipeline,
                Uuid::new_v4(),
                vec![DataInput::Text("new".into())],
                None,
            )
            .await
            .expect_err("unknown id");
        assert!(
            matches!(err, ApiError::UpdateTargetNotFound { .. }),
            "got {err:?}"
        );
        assert_eq!(fx.linked().await, vec![old.id]);
    }

    /// `update()` replaces exactly one document (`update.py:201-206`).
    #[tokio::test]
    async fn update_takes_exactly_one_replacement() {
        let fx = Fixture::new().await;
        let old = fx.add("the old content").await;
        for new_data in [
            Vec::new(),
            vec![DataInput::Text("a".into()), DataInput::Text("b".into())],
        ] {
            let err = fx
                .update_with(&fx.add_pipeline, old.id, new_data, None)
                .await
                .expect_err("not exactly one item");
            assert!(
                matches!(&err, ApiError::Ingestion(msg) if msg.contains("exactly one")),
                "got {err:?}"
            );
        }
        assert_eq!(fx.linked().await, vec![old.id]);
    }

    /// Replacement content equal to ANOTHER row's content in the dataset: a
    /// pinned add skips the content-hash lookup, so the dataset ends with two
    /// rows holding that content (Python: `data_dataset_content_lookup` is
    /// non-unique), and the target keeps its id.
    #[tokio::test]
    async fn a_replacement_equal_to_another_row_keeps_two_rows() {
        let fx = Fixture::new().await;
        let target = fx.add("the target content").await;
        let other = fx.add("the other content").await;

        let result = fx
            .update_with(
                &fx.add_pipeline,
                target.id,
                vec![DataInput::Text("the other content".into())],
                None,
            )
            .await
            .expect("update succeeds");
        assert_eq!(result.data_id, target.id);
        let mut linked = fx.linked().await;
        linked.sort();
        let mut expected = vec![target.id, other.id];
        expected.sort();
        assert_eq!(linked, expected, "two rows, no dedup onto the other row");
        assert_eq!(
            fx.row(target.id).await.expect("row").content_hash,
            other.content_hash
        );
    }

    /// A replacement with the old content: the row is kept (as always), its
    /// node set is re-tagged, and its memory is rebuilt under it.
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
        assert_eq!(result.data_id, old.id);
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
