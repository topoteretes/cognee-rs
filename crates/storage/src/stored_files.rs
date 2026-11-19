//! Removing the files cognee stored, shared by delete and by an add that
//! moves a row on to new files.

use std::path::Path;

use crate::{RemovalGuard, StorageError, StorageTrait};

/// Whether `location` lives under the storage root `base_path`, so cognee
/// may remove it. A user's own local file, an external URL (`s3://`,
/// `http://`) or an inline marker (`text://`, `binary://`) is never removed
/// (Python `is_managed_data_file`).
pub fn is_storage_managed(base_path: &str, location: &str) -> bool {
    let path_str = location.strip_prefix("file://").unwrap_or(location);
    if path_str.contains("://") {
        return false;
    }
    if base_path.is_empty() {
        // A backend without a filesystem root (mock, S3-backed) keys its
        // files by bare location.
        return true;
    }
    let path = Path::new(path_str);
    if !path.is_absolute() {
        // A plain relative storage key.
        return true;
    }
    let base = Path::new(base_path);
    let base = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(base)
    };
    path.starts_with(base)
}

/// Delete the stored file at `location`, then the folder it was in when that
/// is left empty (an upload's `<content_hash>/`). The folder stays while an
/// add claims a file in it: the add may have created it and not yet written
/// there. Taken under `removal`, so no claim lands between the check and the
/// delete.
pub async fn remove_stored_file(
    storage: &dyn StorageTrait,
    removal: &RemovalGuard,
    location: &str,
) -> Result<(), StorageError> {
    storage.delete(location).await?;
    let full_path = storage.get_full_path(location);
    let folder_in_use = full_path
        .parent()
        .is_some_and(|folder| removal.is_claimed_under(folder));
    if !folder_in_use {
        storage.remove_empty_parent(location).await?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::{LocalStorage, removing_stored_files};

    /// Removing the last file of an upload folder removes the folder.
    #[tokio::test]
    async fn removing_the_last_file_removes_its_folder() {
        let root = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(root.path().to_path_buf());
        storage.store(b"x", "hash1/a.txt").await.unwrap();

        let removal = removing_stored_files().await;
        remove_stored_file(&storage, &removal, "hash1/a.txt")
            .await
            .unwrap();

        assert!(!root.path().join("hash1").exists());
        assert!(root.path().exists(), "the root stays");
    }

    /// The folder stays while an add claims a file in it: the add may have
    /// created the folder and not yet written its file there.
    #[tokio::test]
    async fn a_claim_in_the_folder_keeps_it() {
        let root = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(root.path().to_path_buf());
        storage.store(b"x", "hash2/old.txt").await.unwrap();
        let add = crate::ClaimScope::default();
        add.claim(&storage.get_full_path("hash2/new.txt")).await;

        let removal = removing_stored_files().await;
        remove_stored_file(&storage, &removal, "hash2/old.txt")
            .await
            .unwrap();

        assert!(!root.path().join("hash2/old.txt").exists());
        assert!(
            root.path().join("hash2").exists(),
            "the claimed folder stays"
        );
    }
}
