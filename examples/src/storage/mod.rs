mod local_storage;
mod storage_trait;

#[cfg(any(test, feature = "testing"))]
mod mock_storage;

pub use local_storage::LocalStorage;
pub use storage_trait::{StorageError, StorageTrait, StorageWriter};

#[cfg(any(test, feature = "testing"))]
pub use mock_storage::MockStorage;
