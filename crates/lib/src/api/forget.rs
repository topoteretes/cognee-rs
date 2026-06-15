//! Unified deletion API -- `forget()`.
//!
//! Thin wrapper over [`DeleteService`] that maps a user-friendly
//! [`ForgetTarget`] enum to the underlying [`DeleteScope`].
//!
//! Equivalent to Python's `cognee.api.v1.forget.forget()`.

use cognee_database::IngestDb;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use uuid::Uuid;

use super::error::ApiError;

/// A reference to a dataset, either by human-readable name or by UUID.
///
/// Matches Python's `dataset: Union[str, UUID]` API semantics in
/// `cognee.api.v1.forget.forget()`.
#[derive(Debug, Clone)]
pub enum DatasetRef {
    /// Dataset identified by name (scoped to the `owner_id` passed to
    /// [`forget`]).
    Name(String),
    /// Dataset identified by UUID. Resolving a UUID back to a dataset name
    /// requires an [`IngestDb`] connection.
    Id(Uuid),
}

impl DatasetRef {
    /// Resolve this reference to a dataset name, performing a reverse lookup
    /// via [`IngestDb::get_dataset`] when the reference is a UUID.
    ///
    /// # Errors
    /// * Returns [`ApiError::InvalidArgument`] if `self` is [`DatasetRef::Id`]
    ///   and `db` is `None` (we cannot resolve a UUID without a DB connection).
    /// * Returns [`ApiError::InvalidArgument`] if the dataset cannot be found
    ///   in the database, or if the dataset is owned by a different user.
    pub async fn to_name(
        &self,
        owner_id: Uuid,
        db: Option<&dyn IngestDb>,
    ) -> Result<String, ApiError> {
        match self {
            DatasetRef::Name(name) => Ok(name.clone()),
            DatasetRef::Id(id) => {
                let db = db.ok_or_else(|| {
                    ApiError::InvalidArgument(
                        "db connection required to resolve dataset UUID".to_string(),
                    )
                })?;
                let dataset = db.get_dataset(*id).await.map_err(|e| {
                    ApiError::InvalidArgument(format!("Dataset {id} lookup failed: {e}"))
                })?;
                let dataset = dataset
                    .ok_or_else(|| ApiError::InvalidArgument(format!("Dataset {id} not found")))?;
                if dataset.owner_id != owner_id {
                    return Err(ApiError::InvalidArgument(format!(
                        "Dataset {id} not owned by the requesting user"
                    )));
                }
                Ok(dataset.name)
            }
        }
    }
}

/// What to forget.
#[derive(Debug, Clone)]
pub enum ForgetTarget {
    /// Delete a single data item from a specific dataset.
    Item { data_id: Uuid, dataset: DatasetRef },
    /// Delete an entire dataset (by name or UUID).
    Dataset { dataset: DatasetRef },
    /// Delete all data for the given owner.
    All,
}

/// Summary of a forget operation.
#[derive(Debug, Clone)]
pub struct ForgetResult {
    pub target: String,
    pub delete_result: DeleteResult,
}

/// Unified deletion entry point.
///
/// # Arguments
/// * `target` - What to delete (item, dataset, or everything).
/// * `owner_id` - The owner whose data is affected.
/// * `delete_service` - Pre-configured [`DeleteService`] with all backends.
/// * `db` - Database connection for name-to-ID resolution. Required when
///   `target` references a dataset by [`DatasetRef::Id`]; optional otherwise
///   (used for dataset existence validation when resolving by name).
///
/// # Errors
/// Returns [`ApiError::InvalidArgument`] if the dataset cannot be found,
/// or [`ApiError::Delete`] if the underlying delete operation fails.
pub async fn forget(
    target: ForgetTarget,
    owner_id: Uuid,
    delete_service: &DeleteService,
    db: Option<&dyn IngestDb>,
) -> Result<ForgetResult, ApiError> {
    // Mirrors Python `send_telemetry("cognee.forget", ...)` from
    // cognee/api/v1/forget/forget.py:79.
    #[cfg(feature = "telemetry")]
    {
        let (target_label, dataset_dbg, data_id_dbg) = match &target {
            ForgetTarget::Item { data_id, dataset } => {
                ("data_item", format!("{dataset:?}"), data_id.to_string())
            }
            ForgetTarget::Dataset { dataset } => ("dataset", format!("{dataset:?}"), String::new()),
            ForgetTarget::All => ("everything", String::new(), String::new()),
        };
        cognee_telemetry::send_telemetry(
            "cognee.forget",
            owner_id,
            Some(serde_json::json!({
                "target": target_label,
                "dataset": dataset_dbg,
                "data_id": data_id_dbg,
                "cognee_version": env!("CARGO_PKG_VERSION"),
            })),
        );
    }

    let (scope, label) = match target {
        ForgetTarget::Item { data_id, dataset } => {
            let dataset_name = dataset.to_name(owner_id, db).await?;
            let scope = DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some(dataset_name),
                delete_dataset_if_empty: false,
            };
            (scope, format!("item:{data_id}"))
        }
        ForgetTarget::Dataset { dataset } => {
            let dataset_name = dataset.to_name(owner_id, db).await?;
            // Validate dataset exists if we have a DB connection and the
            // reference came in as a name (UUID path already validated above).
            if let Some(db) = db {
                let _dataset = db
                    .get_dataset_by_name(&dataset_name, owner_id, None)
                    .await
                    .map_err(|e| {
                        ApiError::InvalidArgument(format!(
                            "Dataset '{}' not found: {}",
                            dataset_name, e
                        ))
                    })?;
            }
            let scope = DeleteScope::Dataset {
                owner_id,
                dataset_name: dataset_name.clone(),
            };
            (scope, format!("dataset:{dataset_name}"))
        }
        ForgetTarget::All => {
            let scope = DeleteScope::User { owner_id };
            (scope, "all".to_string())
        }
    };

    let request = build_delete_request(scope);

    let delete_result = delete_service.execute(&request).await?;

    Ok(ForgetResult {
        target: label,
        delete_result,
    })
}

/// Build the [`DeleteRequest`] for a `forget` operation.
///
/// Extracted so the delete mode choice can be unit-tested independently of the
/// async scope-resolution logic.
fn build_delete_request(scope: DeleteScope) -> DeleteRequest {
    DeleteRequest {
        scope,
        // Python `datasets.delete_data` defaults mode="soft" and warns hard is
        // dangerous (datasets.py:147). Match the safer default.
        mode: DeleteMode::Soft,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forget_uses_soft_delete_mode() {
        // Verify that forget() constructs a Soft delete request, matching
        // Python's datasets.delete_data default (datasets.py:147).
        let scope = DeleteScope::User {
            owner_id: Uuid::nil(),
        };
        let req = build_delete_request(scope);
        assert!(
            matches!(req.mode, DeleteMode::Soft),
            "forget must use DeleteMode::Soft to match Python's default"
        );
    }

    #[test]
    fn forget_target_debug_format() {
        let target = ForgetTarget::All;
        let debug_str = format!("{:?}", target);
        assert!(debug_str.contains("All"));
    }

    #[test]
    fn forget_target_item_holds_fields() {
        let id = Uuid::new_v4();
        let target = ForgetTarget::Item {
            data_id: id,
            dataset: DatasetRef::Name("test_ds".to_string()),
        };
        match target {
            ForgetTarget::Item { data_id, dataset } => {
                assert_eq!(data_id, id);
                match dataset {
                    DatasetRef::Name(name) => assert_eq!(name, "test_ds"),
                    _ => panic!("expected Name variant"),
                }
            }
            _ => panic!("expected Item variant"),
        }
    }

    // ----- Unit tests for DatasetRef::to_name -----

    #[tokio::test]
    async fn dataset_ref_name_passthrough() {
        // DatasetRef::Name with db=None should return the name without any DB
        // lookup.
        let owner_id = Uuid::new_v4();
        let dref = DatasetRef::Name("my_ds".to_string());
        let resolved = dref.to_name(owner_id, None).await.expect("passthrough ok");
        assert_eq!(resolved, "my_ds");
    }

    #[tokio::test]
    async fn dataset_ref_id_requires_db() {
        // DatasetRef::Id with db=None must error with InvalidArgument.
        let owner_id = Uuid::new_v4();
        let dref = DatasetRef::Id(Uuid::new_v4());
        let result = dref.to_name(owner_id, None).await;
        match result {
            Err(ApiError::InvalidArgument(msg)) => {
                assert!(
                    msg.contains("db connection required"),
                    "unexpected msg: {msg}"
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn forget_target_dataset_uuid_variant_debug() {
        // Ensure the new UUID variant round-trips through Debug.
        let id = Uuid::new_v4();
        let target = ForgetTarget::Dataset {
            dataset: DatasetRef::Id(id),
        };
        let dbg = format!("{target:?}");
        assert!(dbg.contains("Dataset"), "debug: {dbg}");
        assert!(dbg.contains("Id"), "debug: {dbg}");
    }
}
