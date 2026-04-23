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

/// What to forget.
#[derive(Debug, Clone)]
pub enum ForgetTarget {
    /// Delete a single data item from a specific dataset.
    Item { data_id: Uuid, dataset_name: String },
    /// Delete an entire dataset (resolved by name).
    Dataset { dataset_name: String },
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
/// * `db` - Database connection for name-to-ID resolution (only needed for
///   `ForgetTarget::Dataset` by name).
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
    let (scope, label) = match target {
        ForgetTarget::Item {
            data_id,
            dataset_name,
        } => {
            let scope = DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some(dataset_name.clone()),
                delete_dataset_if_empty: false,
            };
            (scope, format!("item:{data_id}"))
        }
        ForgetTarget::Dataset { dataset_name } => {
            // Validate dataset exists if we have a DB connection.
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

    let request = DeleteRequest {
        scope,
        mode: DeleteMode::Hard,
    };

    let delete_result = delete_service.execute(&request).await?;

    Ok(ForgetResult {
        target: label,
        delete_result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            dataset_name: "test_ds".to_string(),
        };
        match target {
            ForgetTarget::Item {
                data_id,
                dataset_name,
            } => {
                assert_eq!(data_id, id);
                assert_eq!(dataset_name, "test_ds");
            }
            _ => panic!("expected Item variant"),
        }
    }
}
