mod local_storage;
mod storage_trait;

#[cfg(feature = "testing")]
mod mock_storage;

pub use local_storage::LocalStorage;
pub use storage_trait::{StorageError, StorageTrait, StorageWriter};

#[cfg(feature = "testing")]
pub use mock_storage::MockStorage;
