//! Built-in storage factory: the local filesystem.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_storage::{LocalStorage, StorageTrait};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::StorageFactory;

/// Storage on the local filesystem, under `data_root_directory`.
pub struct LocalStorageFactory;

#[async_trait]
impl StorageFactory for LocalStorageFactory {
    fn provider(&self) -> &str {
        "local"
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        build_storage(ctx).await
    }
}

/// Build and initialize the local-filesystem storage backend.
///
/// Kept for callers that build storage without a registry;
/// [`crate::ComponentRegistry::build_storage`] selects by `storage.backend`.
pub async fn build_storage(
    ctx: &BackendBuildContext,
) -> Result<Arc<dyn StorageTrait>, ComponentError> {
    let storage = LocalStorage::new(ctx.data_root_directory.clone());
    storage
        .initialize()
        .await
        .map_err(|e| ComponentError::Storage(format!("initialization failed: {e}")))?;
    Ok(Arc::new(storage))
}
