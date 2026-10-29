use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_models::{Data, Dataset};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{data, datasets, pipeline_runs};
use crate::types::{DatabaseError, PipelineRunStatus};

#[async_trait]
pub trait IngestDb: Send + Sync {
    /// Look up a dataset by its UUID.
    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError>;

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError>;

    /// List all datasets owned by the given user.
    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError>;

    /// The dataset named `name` owned by `owner_id` in `tenant_id`, with
    /// **strict** tenant equality (`None` matches only a tenant-less row) —
    /// Python's `get_dataset_ids` rule
    /// (`cognee/modules/data/methods/get_dataset_ids.py:30-34@37f788bd2`).
    /// This is the only by-name dataset lookup: the tenant-loose variant was
    /// removed (SDK-936), so a name never resolves across tenants.
    ///
    /// The default filters [`Self::list_datasets_by_owner`] and is correct for
    /// any implementation; override it with a point query.
    async fn get_dataset_by_name_in_tenant(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        Ok(self
            .list_datasets_by_owner(owner_id)
            .await?
            .into_iter()
            .find(|ds| ds.name == name && ds.tenant_id == tenant_id))
    }

    /// The datasets among `ids` that exist, in `ids` order (duplicates and
    /// missing ids dropped). The default looks each id up with
    /// [`Self::get_dataset`]; override it with a batch query.
    async fn get_datasets_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Dataset>, DatabaseError> {
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if seen.insert(*id)
                && let Some(ds) = self.get_dataset(*id).await?
            {
                out.push(ds);
            }
        }
        Ok(out)
    }

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError>;

    /// The row holding `content_hash` in `dataset_id` for `owner_id` and
    /// `tenant_id` (strict tenant equality), if any. This is the ingest
    /// dedup lookup — Python's `identify_many`: rows are dataset-scoped, so
    /// the same content in another dataset, or added by another owner, is
    /// not a match.
    async fn find_data_in_dataset(
        &self,
        dataset_id: Uuid,
        content_hash: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Data>, DatabaseError>;

    /// Resolve a caller-supplied data id to the canonical row id in
    /// `dataset_id`: the exact id, else the row whose `legacy_id` it is
    /// (Python's `resolve_data_id`). `None` when the id is unknown there.
    async fn resolve_data_id(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<Option<Uuid>, DatabaseError>;

    async fn create_data(&self, d: Data) -> Result<Data, DatabaseError>;

    /// Write the whole row `d` (matched by `d.id`) and stamp `updated_at`.
    /// Used by a pinned add (`update()`), which refreshes the row in place the
    /// way Python's `ingest_data` merges it. A missing row is
    /// [`DatabaseError::NotFound`].
    async fn update_data(&self, d: Data) -> Result<Data, DatabaseError>;

    /// Set the `node_set` and `external_metadata` columns of an existing Data
    /// record (and stamp `updated_at`), leaving every other column as it is.
    ///
    /// The ingest pipeline uses it on a re-add of content that is already
    /// stored, where Python's `ingest_data` update branch refreshes those two
    /// fields instead of returning the row unchanged. It replaced a full-row
    /// `update_data(Data)` (SDK-1129): a full-row write from the pipeline's
    /// earlier read rolled back any `token_count`, `last_accessed` or
    /// `pipeline_status` a concurrent cognify or search wrote in between.
    /// Implementations must not touch other columns. A missing row is
    /// [`DatabaseError::NotFound`].
    ///
    /// Returns the `updated_at` value written, exactly as a later read would
    /// return it, so the caller can patch the row it already holds instead of
    /// reading it back.
    async fn update_data_node_set(
        &self,
        data_id: Uuid,
        node_set: Option<&str>,
        external_metadata: Option<&str>,
    ) -> Result<DateTime<Utc>, DatabaseError>;

    /// Make `dataset_id` the owning dataset of the existing row `data_id`
    /// (`data.dataset_id`). Rows are dataset-scoped: a row of another dataset
    /// is refused. The ingest pipeline does not need it (it creates rows with
    /// their dataset); it serves callers that build `Data` rows themselves.
    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Update the `last_accessed` timestamp on the given Data records.
    ///
    /// Implementations should perform a bulk `UPDATE data SET last_accessed = ?
    /// WHERE id IN (...)` query. An empty `data_ids` slice is a no-op.
    async fn update_last_accessed(
        &self,
        data_ids: &[Uuid],
        timestamp: DateTime<Utc>,
    ) -> Result<(), DatabaseError>;

    /// Get the latest pipeline run status for a (pipeline_name, dataset_id) pair.
    ///
    /// Returns `None` if no matching run exists.
    async fn get_latest_pipeline_status(
        &self,
        pipeline_name: &str,
        dataset_id: Uuid,
    ) -> Result<Option<PipelineRunStatus>, DatabaseError>;
}

#[async_trait]
impl IngestDb for DatabaseConnection {
    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset(self, id).await
    }

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError> {
        datasets::create_dataset(self, dataset).await
    }

    async fn get_dataset_by_name_in_tenant(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset_by_name_in_tenant(self, name, owner_id, tenant_id).await
    }

    async fn get_datasets_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Dataset>, DatabaseError> {
        datasets::get_datasets_by_ids(self, ids).await
    }

    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        datasets::list_datasets_by_owner(self, owner_id).await
    }

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        data::get_data(self, id).await
    }

    async fn find_data_in_dataset(
        &self,
        dataset_id: Uuid,
        content_hash: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Data>, DatabaseError> {
        data::find_data_in_dataset(self, dataset_id, content_hash, owner_id, tenant_id).await
    }

    async fn resolve_data_id(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<Option<Uuid>, DatabaseError> {
        data::resolve_data_id(self, dataset_id, data_id).await
    }

    async fn create_data(&self, d: Data) -> Result<Data, DatabaseError> {
        data::create_data(self, d).await
    }

    async fn update_data(&self, d: Data) -> Result<Data, DatabaseError> {
        data::update_data(self, d).await
    }

    async fn update_data_node_set(
        &self,
        data_id: Uuid,
        node_set: Option<&str>,
        external_metadata: Option<&str>,
    ) -> Result<DateTime<Utc>, DatabaseError> {
        data::update_data_node_set(self, data_id, node_set, external_metadata).await
    }

    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        datasets::attach_data_to_dataset(self, dataset_id, data_id).await
    }

    async fn update_last_accessed(
        &self,
        data_ids: &[Uuid],
        timestamp: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        data::update_last_accessed(self, data_ids, timestamp).await
    }

    async fn get_latest_pipeline_status(
        &self,
        pipeline_name: &str,
        dataset_id: Uuid,
    ) -> Result<Option<PipelineRunStatus>, DatabaseError> {
        pipeline_runs::get_latest_pipeline_status(self, pipeline_name, dataset_id).await
    }
}
