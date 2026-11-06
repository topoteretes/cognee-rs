//! `improve(run_in_background=True)`: resolve and validate in the caller,
//! then run the stages as a process-wide background task.
//!
//! Python's `improve()` resolves its inputs (`_resolve_inputs`,
//! `cognee/api/v1/improve/improve.py`) before it detaches the run: the
//! dataset goes through the write-level resolver `remember`/`memify` use — a
//! name resolves to the caller's dataset or creates it, a UUID naming no
//! dataset or one the caller may not write raises — and the configuration
//! and `feedback_alpha` are validated. Only then is the run handed to the
//! background, so every error a caller can fix reaches the caller.
//! [`improve_in_background`] does the same and returns the `running`
//! [`ImproveResult`]; callers wait for the run with
//! [`wait_for_background_tasks`](crate::background_tasks::wait_for_background_tasks).

use std::future::Future;
use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_core::DatasetLocks;
use cognee_database::{AclDb, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::Dataset;
use cognee_ontology::OntologyResolver;
use cognee_session::{SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};
use uuid::Uuid;

use super::{ImproveConfig, ImproveParams, ImproveResult, improve, validate_feedback_alpha};
use crate::api::error::ApiError;

/// The owned counterpart of [`ImproveParams`], for [`improve_in_background`]:
/// the spawned run outlives the caller, so it owns what [`ImproveParams`]
/// borrows (the add pipeline and the cognify configuration). The fields mean
/// what they mean on [`ImproveParams`]; there is no `run_in_background`, the
/// run is always reported as a background one.
pub struct OwnedImproveParams {
    /// Dataset name; resolved, or created for the caller, when `dataset_id`
    /// is `None`.
    pub dataset_name: String,
    /// A dataset id to target. It must exist and the caller must be allowed
    /// to write it.
    pub dataset_id: Option<Uuid>,
    pub session_ids: Option<Vec<String>>,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub feedback_alpha: Option<f64>,
    pub config: ImproveConfig,
    pub personalization: cognee_search::PreferenceConfig,
    pub auto_feedback: bool,
    pub build_global_context_index: bool,
    pub build_truth_subspace: bool,
    pub llm: Arc<dyn Llm>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    pub ontology_resolver: Arc<dyn OntologyResolver>,
    /// Metadata DB connection. Required: the dataset is resolved before the
    /// run is dispatched.
    pub db: Option<Arc<dyn RelationalDb>>,
    pub session_store: Option<Arc<dyn SessionStore>>,
    pub session_manager: Option<Arc<SessionManager>>,
    pub add_pipeline: Option<Arc<AddPipeline>>,
    pub dataset_locks: Option<Arc<DatasetLocks>>,
    /// The ACL store the dataset is authorized (`write`) and a created one
    /// granted through. `None` uses the add pipeline's
    /// ([`AddPipeline::acl_db`]); with neither, ownership stands in for the
    /// grant: a dataset id must name one of the owner's datasets in
    /// `tenant_id`.
    pub acl_db: Option<Arc<dyn AclDb>>,
    pub cognify_config: CognifyConfig,
}

impl OwnedImproveParams {
    /// The borrowed [`ImproveParams`] of this run.
    pub fn as_params(&self, run_in_background: bool) -> ImproveParams<'_> {
        ImproveParams {
            dataset_name: self.dataset_name.clone(),
            dataset_id: self.dataset_id,
            session_ids: self.session_ids.clone(),
            owner_id: self.owner_id,
            tenant_id: self.tenant_id,
            feedback_alpha: self.feedback_alpha,
            config: self.config.clone(),
            personalization: self.personalization,
            auto_feedback: self.auto_feedback,
            build_global_context_index: self.build_global_context_index,
            build_truth_subspace: self.build_truth_subspace,
            run_in_background,
            llm: Arc::clone(&self.llm),
            storage: Arc::clone(&self.storage),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            embedding_engine: Arc::clone(&self.embedding_engine),
            ontology_resolver: Arc::clone(&self.ontology_resolver),
            db: self.db.clone(),
            session_store: self.session_store.clone(),
            session_manager: self.session_manager.clone(),
            add_pipeline: self.add_pipeline.as_deref(),
            dataset_locks: self.dataset_locks.clone(),
            cognify_config: &self.cognify_config,
        }
    }

    fn acl(&self) -> Option<Arc<dyn AclDb>> {
        self.acl_db.clone().or_else(|| {
            self.add_pipeline
                .as_ref()
                .and_then(|pipeline| pipeline.acl_db().cloned())
        })
    }

    fn locks(&self) -> Arc<DatasetLocks> {
        self.dataset_locks
            .clone()
            .or_else(|| {
                self.add_pipeline
                    .as_ref()
                    .and_then(|pipeline| pipeline.dataset_locks().cloned())
            })
            .unwrap_or_else(DatasetLocks::global)
    }
}

/// Start an improve run in the background (Python's
/// `improve(run_in_background=True)`) and return its `running` result.
///
/// Before anything is spawned the configuration and `feedback_alpha` are
/// validated ([`ApiError::InvalidArgument`]) and the dataset is resolved: a
/// `dataset_id` that names no dataset is [`ApiError::InvalidArgument`], one
/// the caller may not write [`ApiError::PermissionDenied`]; a name resolves
/// to the caller's dataset or creates it (granting the owner, with an ACL).
/// The returned result carries the resolved dataset id and name. The run's
/// own outcome, a fatal-stage abort included, is logged; wait for it with
/// [`wait_for_background_tasks`](crate::background_tasks::wait_for_background_tasks).
pub async fn improve_in_background(params: OwnedImproveParams) -> Result<ImproveResult, ApiError> {
    improve_in_background_then(params, |_| async {}).await
}

/// [`improve_in_background`], then `then` with the run's outcome, inside the
/// same background task (a host's post-run work, e.g. a graph checkpoint, is
/// awaited by the same drain as the run).
pub async fn improve_in_background_then<F, Fut>(
    mut params: OwnedImproveParams,
    then: F,
) -> Result<ImproveResult, ApiError>
where
    F: FnOnce(Result<ImproveResult, ApiError>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    params
        .config
        .validate()
        .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;
    validate_feedback_alpha(
        params
            .feedback_alpha
            .unwrap_or(params.config.feedback_alpha),
    )
    .map_err(|e| ApiError::InvalidArgument(e.to_string()))?;

    let dataset = resolve_dataset(&params).await?;
    params.dataset_id = Some(dataset.id);
    params.dataset_name = dataset.name;

    let session_ids: Vec<String> = params
        .session_ids
        .iter()
        .flatten()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect();
    let running = ImproveResult::running_in_background(
        params.dataset_id,
        params.dataset_name.clone(),
        session_ids,
    );

    crate::background_tasks::spawn_background_task(async move {
        let outcome = improve(params.as_params(true)).await;
        match &outcome {
            Ok(result) => info!(
                dataset = %params.dataset_name,
                status = result.status().as_str(),
                stages = %result.stage_summary(),
                "background improve finished"
            ),
            // A fatal-stage abort (`ApiError::ImproveAborted`) included.
            Err(e) => warn!(dataset = %params.dataset_name, "background improve failed: {e}"),
        }
        then(outcome).await;
    });
    Ok(running)
}

/// The write-level dataset resolution of Python's
/// `resolve_authorized_user_datasets`, for the one dataset of a run.
async fn resolve_dataset(params: &OwnedImproveParams) -> Result<Dataset, ApiError> {
    let db = params.db.as_ref().ok_or_else(|| {
        ApiError::InvalidArgument(
            "a background improve needs the relational database to resolve its dataset".to_string(),
        )
    })?;
    let acl = params.acl();
    let (owner_id, tenant_id) = (params.owner_id, params.tenant_id);
    let lookup_failed = |what: &str, e: cognee_database::DatabaseError| {
        ApiError::Improve(format!("dataset lookup failed for '{what}': {e}"))
    };

    if let Some(id) = params.dataset_id {
        // With an ACL the grant is checked first, so an unknown id and a
        // foreign one answer alike.
        if let Some(acl) = &acl {
            require_write(acl.as_ref(), owner_id, tenant_id, id).await?;
        }
        let dataset = cognee_database::IngestDb::get_dataset(&**db, id)
            .await
            .map_err(|e| lookup_failed(&id.to_string(), e))?
            .ok_or_else(|| ApiError::InvalidArgument(format!("dataset '{id}' not found")))?;
        if acl.is_none() && (dataset.owner_id != owner_id || dataset.tenant_id != tenant_id) {
            return Err(ApiError::PermissionDenied(format!(
                "write permission denied on dataset '{id}'"
            )));
        }
        return Ok(dataset);
    }

    let name = params.dataset_name.as_str();
    let find = || async {
        cognee_database::IngestDb::get_dataset_by_name_in_tenant(&**db, name, owner_id, tenant_id)
            .await
            .map_err(|e| lookup_failed(name, e))
    };
    if let Some(dataset) = find().await? {
        if let Some(acl) = &acl {
            require_write(acl.as_ref(), owner_id, tenant_id, dataset.id).await?;
        }
        return Ok(dataset);
    }

    // Create it for the caller, as `add` would: under the identity lock, so a
    // concurrent create of the same name finds the row instead of colliding.
    let locks = params.locks();
    let _guard = cognee_ingestion::lock_dataset_identity(&locks, name, owner_id, tenant_id).await;
    if let Some(dataset) = find().await? {
        return Ok(dataset);
    }
    let id = cognee_ingestion::generate_dataset_id(name, owner_id, tenant_id);
    let dataset = cognee_database::IngestDb::create_dataset(
        &**db,
        Dataset::new(name.to_string(), owner_id, tenant_id, id),
    )
    .await
    .map_err(|e| ApiError::Improve(format!("failed to create dataset '{name}': {e}")))?;
    if let Some(acl) = &acl {
        cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait(
            acl.as_ref(),
            owner_id,
            dataset.id,
        )
        .await
        .map_err(|e| ApiError::Improve(format!("failed to grant dataset '{name}': {e}")))?;
    }
    info!(dataset_id = %dataset.id, "background improve: dataset created");
    Ok(dataset)
}

/// Deny unless `owner_id` holds `write` on `dataset_id` in `tenant_id`.
async fn require_write(
    acl: &dyn AclDb,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    let permitted = acl
        .has_permission_in_tenant(owner_id, dataset_id, "write", tenant_id)
        .await
        .map_err(|e| ApiError::Improve(format!("permission check failed: {e}")))?;
    if permitted {
        Ok(())
    } else {
        Err(ApiError::PermissionDenied(format!(
            "write permission denied on dataset '{dataset_id}'"
        )))
    }
}
