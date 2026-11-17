use super::storage_trait::{StorageError, StorageTrait, StorageWriter, TempFile};
use async_trait::async_trait;
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tracing::{debug, instrument};
use uuid::Uuid;

pub struct LocalStorage {
    base_path: PathBuf,
}

impl LocalStorage {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// The full path `file_name` is written to, with its parent folders
    /// created. Refuses a path that leaves the storage root, by `..` or
    /// through a symlink (Python `LocalFileStorage._resolve_storage_path`).
    async fn prepare_target(&self, file_name: &str) -> Result<PathBuf, StorageError> {
        let full_path = self.base_path.join(checked_relative_path(file_name)?);
        self.ensure_inside_root(&full_path, file_name).await?;
        let parent = full_path
            .parent()
            .ok_or_else(|| StorageError::InvalidPath(file_name.to_string()))?;
        fs::create_dir_all(parent)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create directory: {e}")))?;
        Ok(full_path)
    }

    /// Refuses `full_path` when its nearest existing ancestor, with symlinks
    /// followed, lies outside the storage root.
    async fn ensure_inside_root(
        &self,
        full_path: &Path,
        file_name: &str,
    ) -> Result<(), StorageError> {
        let root = self.canonical_root().await?;
        let resolved = canonicalize_existing_prefix(full_path).await?;
        if resolved.starts_with(&root) {
            return Ok(());
        }
        Err(StorageError::InvalidPath(format!(
            "{file_name:?} resolves outside the storage root"
        )))
    }

    /// The storage root with symlinks resolved, created when missing.
    async fn canonical_root(&self) -> Result<PathBuf, StorageError> {
        fs::create_dir_all(&self.base_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create base directory: {e}")))?;
        fs::canonicalize(&self.base_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to resolve base directory: {e}")))
    }

    /// A relative location names a file this storage wrote inside its root,
    /// so it gets the same root check as a write: no `..`, and no symlink
    /// leading out. A `file://` URI or absolute path, which a row may hold
    /// for the user's own file, is left as given.
    async fn check_stored_location(&self, location: &str) -> Result<(), StorageError> {
        if location.starts_with("file://") || Path::new(location).is_absolute() {
            return Ok(());
        }
        let full_path = self.base_path.join(checked_relative_path(location)?);
        self.ensure_inside_root(&full_path, location).await
    }

    /// Resolve a location string into a filesystem path.
    ///
    /// Mirrors Python's `get_data_file_path()` + `open_data_file()` which
    /// strips the `file://` scheme and uses the resulting absolute path
    /// directly.
    ///
    /// Accepted inputs:
    /// - plain relative path: `ab/cd/file.txt`  → `base_path/ab/cd/file.txt`
    /// - absolute `file://` URI: `file:///data/ab/cd/file.txt` → `/data/ab/cd/file.txt`
    fn resolve_location(&self, location: &str) -> PathBuf {
        let path_str = location.strip_prefix("file://").unwrap_or(location);
        let path = Path::new(path_str);

        if path.is_absolute() {
            // Absolute path (from a file:// URI) — use directly, just like
            // Python's `open_data_file` does after `get_data_file_path()`.
            path.to_path_buf()
        } else {
            // Relative path (plain storage location) — join with base.
            self.base_path.join(path)
        }
    }
}

#[async_trait]
impl StorageTrait for LocalStorage {
    async fn initialize(&self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.base_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to create base directory: {e}")))
    }

    #[instrument(name = "storage.store", skip(self, data), fields(file_name, bytes = data.len()))]
    async fn store(&self, data: &[u8], file_name: &str) -> Result<String, StorageError> {
        // `file_name` is the path under the storage root, used as given, so
        // the same content always lands in the same file (Python
        // `LocalFileStorage.store(..., overwrite=True)`).
        let full_path = self.prepare_target(file_name).await?;
        write_atomically(&full_path, &mut &data[..]).await?;
        Ok(file_name.to_string())
    }

    async fn store_if_absent(&self, data: &[u8], file_name: &str) -> Result<String, StorageError> {
        // The path is checked before anything is probed; like Python, two
        // first writes racing may both write, the later one winning.
        let full_path = self.prepare_target(file_name).await?;
        let exists = fs::try_exists(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to check file existence: {e}")))?;
        if !exists {
            write_atomically(&full_path, &mut &data[..]).await?;
        }
        Ok(file_name.to_string())
    }

    #[instrument(name = "storage.store_stream", skip(self, reader), fields(file_name))]
    async fn store_stream_dyn(
        &self,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        file_name: &str,
    ) -> Result<String, StorageError> {
        let full_path = self.prepare_target(file_name).await?;
        write_atomically(&full_path, reader).await?;
        Ok(file_name.to_string())
    }

    #[instrument(name = "storage.create_writer", skip(self), fields(file_name))]
    async fn create_writer(&self, file_name: &str) -> Result<StorageWriter, StorageError> {
        let full_path = self.prepare_target(file_name).await?;
        let (temp, file) = TempFile::create(temp_path_for(&full_path)?)
            .map_err(|e| StorageError::IoError(format!("Failed to create file: {e}")))?;
        Ok(StorageWriter::atomic(
            file,
            file_name.to_string(),
            temp,
            full_path,
        ))
    }

    #[instrument(name = "storage.retrieve", skip(self), fields(location))]
    async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError> {
        self.check_stored_location(location).await?;
        let full_path = self.resolve_location(location);

        let bytes = fs::read(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {location}"))
            } else {
                StorageError::IoError(format!("Failed to read file: {e}"))
            }
        })?;
        debug!(bytes = bytes.len(), "file retrieved");
        Ok(bytes)
    }

    async fn exists(&self, location: &str) -> Result<bool, StorageError> {
        let full_path = self.resolve_location(location);

        fs::try_exists(&full_path)
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to check file existence: {e}")))
    }

    #[instrument(name = "storage.delete", skip(self), fields(location))]
    async fn delete(&self, location: &str) -> Result<(), StorageError> {
        let full_path = self.resolve_location(location);

        fs::remove_file(&full_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("File not found: {location}"))
            } else {
                StorageError::IoError(format!("Failed to delete file: {e}"))
            }
        })
    }

    fn get_full_path(&self, location: &str) -> PathBuf {
        self.resolve_location(location)
    }

    fn base_path(&self) -> &str {
        self.base_path.to_str().unwrap_or("")
    }

    async fn remove_all(&self) -> Result<(), StorageError> {
        let mut entries = fs::read_dir(&self.base_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                // Directory doesn't exist — nothing to remove.
                return StorageError::NotFound(format!(
                    "Base directory not found: {}",
                    self.base_path.display()
                ));
            }
            StorageError::IoError(format!("Failed to read directory: {e}"))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| StorageError::IoError(format!("Failed to iterate directory entry: {e}")))?
        {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| StorageError::IoError(format!("Failed to get file type: {e}")))?;
            if file_type.is_dir() {
                fs::remove_dir_all(&path).await.map_err(|e| {
                    StorageError::IoError(format!(
                        "Failed to remove directory {}: {}",
                        path.display(),
                        e
                    ))
                })?;
            } else {
                fs::remove_file(&path).await.map_err(|e| {
                    StorageError::IoError(format!(
                        "Failed to remove file {}: {}",
                        path.display(),
                        e
                    ))
                })?;
            }
        }
        Ok(())
    }
}

/// `file_name` as a relative path made only of plain names: no `..`, no root
/// and no drive prefix, so joining it to the storage root stays inside it.
/// The last part must name a file, so `.` or `./` (the root itself) is
/// refused too.
fn checked_relative_path(file_name: &str) -> Result<&Path, StorageError> {
    let path = Path::new(file_name);
    let plain = path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let names_a_file = matches!(path.components().next_back(), Some(Component::Normal(_)));
    if !plain || !names_a_file {
        return Err(StorageError::InvalidPath(format!(
            "{file_name:?} is not a path inside the storage root"
        )));
    }
    Ok(path)
}

/// The nearest ancestor of `path` (itself included) that exists, with
/// symlinks followed. An entry that exists but does not resolve, such as a
/// dangling symlink, is an error rather than skipped.
async fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, StorageError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(StorageError::IoError(format!(
                    "Failed to inspect path: {e}"
                )));
            }
            Ok(_) => {}
        }
        return fs::canonicalize(ancestor).await.map_err(|e| {
            StorageError::InvalidPath(format!("{} does not resolve: {e}", ancestor.display()))
        });
    }
    Err(StorageError::InvalidPath(format!(
        "{} has no existing ancestor",
        path.display()
    )))
}

/// Write what `reader` yields to `full_path` through a temp file renamed
/// over it, so a reader never sees a half-written file (Python's
/// write-then-rename).
async fn write_atomically(
    full_path: &Path,
    reader: &mut (dyn AsyncRead + Unpin + Send),
) -> Result<(), StorageError> {
    // Removed on drop unless committed, even if this future is cancelled.
    let to_io_error = |e| StorageError::IoError(format!("Failed to write file: {e}"));
    let (temp, mut file) = TempFile::create(temp_path_for(full_path)?).map_err(to_io_error)?;
    tokio::io::copy(reader, &mut file)
        .await
        .map_err(to_io_error)?;
    file.flush().await.map_err(to_io_error)?;
    temp.commit(full_path).await.map_err(to_io_error)
}

/// A unique temp file beside `full_path`, so the rename stays in one folder
/// and two writers of the same file never share a temp file.
fn temp_path_for(full_path: &Path) -> Result<PathBuf, StorageError> {
    let name = full_path
        .file_name()
        .ok_or_else(|| StorageError::InvalidPath(full_path.display().to_string()))?;
    let temp_name = format!(".{}.{}.tmp", name.to_string_lossy(), Uuid::new_v4());
    Ok(full_path.with_file_name(temp_name))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_store_and_retrieve() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"Hello, World!";
        let location = storage.store(data, "test.txt").await.unwrap();

        let retrieved = storage.retrieve(&location).await.unwrap();
        assert_eq!(data.to_vec(), retrieved);
    }

    #[tokio::test]
    async fn test_exists() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"Test data";
        let location = storage.store(data, "exists.txt").await.unwrap();

        assert!(storage.exists(&location).await.unwrap());
        assert!(!storage.exists("nonexistent.txt").await.unwrap());
    }

    #[test]
    fn resolve_plain_relative_path() {
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("ab/cd/file.txt"),
            PathBuf::from("/data/ab/cd/file.txt")
        );
    }

    #[test]
    fn resolve_absolute_file_uri() {
        // file:// URI with an absolute path — strip scheme, use path as-is
        // (mirrors Python's get_data_file_path for file:///abs/path)
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("file:///data/ab/cd/file.txt"),
            PathBuf::from("/data/ab/cd/file.txt")
        );
    }

    #[test]
    fn resolve_absolute_file_uri_different_base() {
        // URI points to a different directory than base_path — still works
        let storage = LocalStorage::new(PathBuf::from("/data"));
        assert_eq!(
            storage.resolve_location("file:///other/ab/cd/file.txt"),
            PathBuf::from("/other/ab/cd/file.txt")
        );
    }

    #[tokio::test]
    async fn test_retrieve_with_file_uri() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        storage.initialize().await.unwrap();

        let data = b"URI test data";
        let relative = storage.store(data, "uri_test.txt").await.unwrap();

        // Build a file:// URI the same way the ingestion pipeline does
        let uri = format!("file://{}", temp_dir.path().join(&relative).display());

        let retrieved = storage.retrieve(&uri).await.unwrap();
        assert_eq!(data.to_vec(), retrieved);
    }

    #[tokio::test]
    async fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        storage.initialize().await.unwrap();

        let data = b"To be deleted";
        let location = storage.store(data, "delete.txt").await.unwrap();

        assert!(storage.exists(&location).await.unwrap());

        storage.delete(&location).await.unwrap();

        assert!(!storage.exists(&location).await.unwrap());
    }

    /// A file lands at the path it is given, and storing it again replaces
    /// it in place: no copy, no temp file left (Python `overwrite=True`).
    #[tokio::test]
    async fn storing_twice_overwrites_one_file_at_the_given_path() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let first = storage.store(b"one", "hash/photo.png").await.unwrap();
        let second = storage.store(b"two", "hash/photo.png").await.unwrap();

        assert_eq!(first, "hash/photo.png");
        assert_eq!(second, first);
        let names: Vec<_> = std::fs::read_dir(temp_dir.path().join("hash"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["photo.png"]);
        assert_eq!(storage.retrieve(&first).await.unwrap(), b"two");
    }

    /// `..`, an absolute path or an empty one is refused and writes nothing.
    #[tokio::test]
    async fn a_path_leaving_the_root_is_refused() {
        let outer = TempDir::new().unwrap();
        let root = outer.path().join("root");
        let storage = LocalStorage::new(root.clone());
        let outside = outer.path().join("escaped.txt");

        for name in [
            "../escaped.txt",
            "a/../../escaped.txt",
            outside.to_str().unwrap(),
            "",
            // The root itself: its temp file would land beside the root.
            ".",
            "./",
            "./.",
        ] {
            let result = storage.store(b"x", name).await;
            assert!(
                matches!(result, Err(StorageError::InvalidPath(_))),
                "{name:?}: {result:?}"
            );
            let writer = storage.create_writer(name).await;
            assert!(
                matches!(writer, Err(StorageError::InvalidPath(_))),
                "{name:?}: create_writer"
            );
        }
        assert!(!outside.exists());
        let beside_root: Vec<_> = std::fs::read_dir(outer.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name != "root")
            .collect();
        assert!(
            beside_root.is_empty(),
            "written outside the root: {beside_root:?}"
        );
    }

    /// A folder inside the root that links outside it is refused too
    /// (Python resolves the path with `realpath` before writing).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_leaving_the_root_is_refused() {
        let root = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.path().join("link")).unwrap();
        let storage = LocalStorage::new(root.path().to_path_buf());

        let result = storage.store(b"x", "link/sub/file.txt").await;

        assert!(
            matches!(result, Err(StorageError::InvalidPath(_))),
            "{result:?}"
        );
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    /// Reading a stored name that is a symlink out of the root is refused,
    /// as writing through it is; the outside file itself is still read by
    /// its own `file://` URI, as a row for the user's own file holds it.
    #[cfg(unix)]
    #[tokio::test]
    async fn reading_a_symlink_out_of_the_root_is_refused() {
        let root = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let secret = elsewhere.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        std::os::unix::fs::symlink(&secret, root.path().join("text_h.txt")).unwrap();
        let storage = LocalStorage::new(root.path().to_path_buf());

        let result = storage.retrieve("text_h.txt").await;

        assert!(
            matches!(result, Err(StorageError::InvalidPath(_))),
            "{result:?}"
        );
        let by_uri = format!("file://{}", secret.display());
        assert_eq!(storage.retrieve(&by_uri).await.unwrap(), b"secret");
    }

    /// A writer's file appears at its path only once `finish` renames it.
    #[tokio::test]
    async fn a_writer_shows_the_file_only_after_finish() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let mut writer = storage.create_writer("streamed.txt").await.unwrap();
        writer.write_chunk(b"part one, ").await.unwrap();
        assert!(!storage.exists("streamed.txt").await.unwrap());
        writer.write_chunk(b"part two").await.unwrap();
        let location = writer.finish().await.unwrap();

        assert_eq!(location, "streamed.txt");
        assert_eq!(
            storage.retrieve(&location).await.unwrap(),
            b"part one, part two"
        );
    }

    /// Rows from earlier builds point at random `ab/cd/` folders; those
    /// files are still read and deleted through their `file://` URI.
    #[tokio::test]
    async fn a_file_from_an_older_build_is_still_read_and_deleted() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let old_file = temp_dir.path().join("ab/cd/text_old.txt");
        std::fs::create_dir_all(old_file.parent().unwrap()).unwrap();
        std::fs::write(&old_file, b"old").unwrap();
        let uri = format!("file://{}", old_file.display());

        assert_eq!(storage.retrieve(&uri).await.unwrap(), b"old");
        storage.delete(&uri).await.unwrap();
        assert!(!old_file.exists());
    }

    /// A writer abandoned before `finish` (say, on an error mid-write)
    /// removes its temp file and never creates the target.
    #[tokio::test]
    async fn a_writer_dropped_without_finish_leaves_nothing() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let mut writer = storage.create_writer("abandoned.txt").await.unwrap();
        writer.write_chunk(b"half").await.unwrap();
        drop(writer);

        assert_eq!(std::fs::read_dir(temp_dir.path()).unwrap().count(), 0);
    }

    /// A streamed store cancelled while its reader is pending removes its
    /// temp file and never creates the target.
    #[tokio::test]
    async fn a_cancelled_stream_store_leaves_nothing() {
        use tokio::io::AsyncWriteExt;
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let (mut sender, mut reader) = tokio::io::duplex(64);
        sender.write_all(b"partial").await.unwrap();

        let store = storage.store_stream_dyn(&mut reader, "stalled.txt");
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), store).await;

        assert!(result.is_err(), "the store waits on the stalled reader");
        assert_eq!(std::fs::read_dir(temp_dir.path()).unwrap().count(), 0);
    }

    /// `store_if_absent` writes a new file but keeps one already there
    /// (Python `overwrite=False`).
    #[tokio::test]
    async fn store_if_absent_keeps_the_first_file() {
        let temp_dir = TempDir::new().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let first = storage
            .store_if_absent(b"first take", "text_h.txt")
            .await
            .unwrap();
        let second = storage
            .store_if_absent(b"second take", "text_h.txt")
            .await
            .unwrap();

        assert_eq!(second, first);
        assert_eq!(storage.retrieve(&first).await.unwrap(), b"first take");
    }
}
