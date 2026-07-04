//! Storage backend construction. There is no provider choice, so this is a
//! free function rather than a registered factory.

use std::sync::Arc;

use cognee_storage::{LocalStorage, StorageTrait};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;

/// Build and initialize the file-storage backend (`LocalStorage`).
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
