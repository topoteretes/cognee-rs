//! Abstract file-storage layer for Cognee.
//!
//! Provides a trait-based interface for blob/file persistence so storage
//! backends can be swapped without touching the ingestion pipeline.
//!
//! - [`StorageTrait`] (+ [`StorageExt`], [`StorageWriter`]) — async storage operations
//! - [`LocalStorage`] — local-filesystem implementation using `file://` URIs
//! - `MockStorage` (feature `testing`) — in-memory implementation for tests

mod local_storage;
mod storage_trait;
mod stored_file_claims;
mod stored_files;

#[cfg(feature = "testing")]
mod mock_storage;

pub use local_storage::LocalStorage;
pub use storage_trait::{StorageError, StorageExt, StorageTrait, StorageWriter};
pub use stored_file_claims::{ClaimScope, RemovalGuard, removing_stored_files};
pub use stored_files::{is_storage_managed, remove_stored_file};

#[cfg(feature = "testing")]
pub use mock_storage::MockStorage;
