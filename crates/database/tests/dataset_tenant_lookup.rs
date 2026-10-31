#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The tenant-strict by-name lookup and the batch by-id lookup.
//!
//! `get_dataset_by_name_in_tenant` is Python's `get_dataset_ids` rule
//! (`cognee/modules/data/methods/get_dataset_ids.py:30-34@37f788bd2`): owner,
//! name, then `dataset.tenant_id == user.tenant_id` with `None` ⇔ NULL. Each
//! case is checked through the SeaORM point query **and** the trait default
//! (an impl that does not override it), so both answer the same.
#![cfg(feature = "sqlite")]

use cognee_database::ops::datasets::{
    create_dataset, get_dataset_by_name_in_tenant, get_datasets_by_ids,
};
use cognee_database::{DatabaseConnection, DatabaseError, IngestDb, connect, initialize};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

async fn fresh() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    db
}

async fn seed(db: &DatabaseConnection, name: &str, owner: Uuid, tenant: Option<Uuid>) -> Dataset {
    create_dataset(
        db,
        Dataset::new(name.to_string(), owner, tenant, Uuid::new_v4()),
    )
    .await
    .unwrap()
}

/// An `IngestDb` that only forwards the required methods, so the provided
/// `get_dataset_by_name_in_tenant` / `get_datasets_by_ids` defaults run.
struct DefaultsOnly<'a>(&'a DatabaseConnection);

#[async_trait::async_trait]
impl IngestDb for DefaultsOnly<'_> {
    async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        IngestDb::get_dataset(self.0, id).await
    }
    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError> {
        IngestDb::create_dataset(self.0, dataset).await
    }
    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        IngestDb::list_datasets_by_owner(self.0, owner_id).await
    }
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        IngestDb::get_data(self.0, id).await
    }
    async fn create_data(&self, d: Data) -> Result<Data, DatabaseError> {
        IngestDb::create_data(self.0, d).await
    }
    async fn update_data_node_set(
        &self,
        data_id: Uuid,
        node_set: Option<&str>,
        external_metadata: Option<&str>,
    ) -> Result<chrono::DateTime<chrono::Utc>, DatabaseError> {
        IngestDb::update_data_node_set(self.0, data_id, node_set, external_metadata).await
    }
    async fn update_data_stored_text(
        &self,
        data_id: Uuid,
        raw_data_location: &str,
        raw_content_hash: &str,
        extension: &str,
        mime_type: &str,
        loader_engine: &str,
    ) -> Result<chrono::DateTime<chrono::Utc>, DatabaseError> {
        IngestDb::update_data_stored_text(
            self.0,
            data_id,
            raw_data_location,
            raw_content_hash,
            extension,
            mime_type,
            loader_engine,
        )
        .await
    }
    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        IngestDb::attach_data_to_dataset(self.0, dataset_id, data_id).await
    }
    async fn update_last_accessed(
        &self,
        data_ids: &[Uuid],
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DatabaseError> {
        IngestDb::update_last_accessed(self.0, data_ids, timestamp).await
    }
    async fn get_latest_pipeline_status(
        &self,
        pipeline_name: &str,
        dataset_id: Uuid,
    ) -> Result<Option<cognee_database::PipelineRunStatus>, DatabaseError> {
        IngestDb::get_latest_pipeline_status(self.0, pipeline_name, dataset_id).await
    }
}

#[tokio::test]
async fn by_name_lookup_is_tenant_strict_with_none_matching_only_null() {
    let db = fresh().await;
    let owner = Uuid::new_v4();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    // Tenanted row first, so an unordered "no tenant filter" lookup would
    // tend to return it for `None`.
    let tenanted = seed(&db, "x", owner, Some(t1)).await;
    let untenanted = seed(&db, "x", owner, None).await;
    let only_t = seed(&db, "only_t", owner, Some(t1)).await;
    let defaults = DefaultsOnly(&db);

    for (tenant, name, expected) in [
        (None, "x", Some(untenanted.id)),
        (Some(t1), "x", Some(tenanted.id)),
        (Some(t2), "x", None),
        (None, "only_t", None),
        (Some(t1), "only_t", Some(only_t.id)),
    ] {
        let point = get_dataset_by_name_in_tenant(&db, name, owner, tenant)
            .await
            .unwrap()
            .map(|d| d.id);
        let default = defaults
            .get_dataset_by_name_in_tenant(name, owner, tenant)
            .await
            .unwrap()
            .map(|d| d.id);
        assert_eq!(point, expected, "point query: {name} in {tenant:?}");
        assert_eq!(default, expected, "trait default: {name} in {tenant:?}");
    }

    // Owner-scoped: another owner never resolves the name.
    assert!(
        get_dataset_by_name_in_tenant(&db, "x", Uuid::new_v4(), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn batch_by_ids_keeps_order_and_skips_missing_and_duplicates() {
    let db = fresh().await;
    let owner = Uuid::new_v4();
    let a = seed(&db, "a", owner, None).await;
    let b = seed(&db, "b", owner, Some(Uuid::new_v4())).await;
    let missing = Uuid::new_v4();
    let ids = [b.id, missing, a.id, b.id];

    let got: Vec<Uuid> = get_datasets_by_ids(&db, &ids)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(got, vec![b.id, a.id]);
    let via_default: Vec<Uuid> = DefaultsOnly(&db)
        .get_datasets_by_ids(&ids)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(via_default, got);
    assert!(get_datasets_by_ids(&db, &[]).await.unwrap().is_empty());
}
