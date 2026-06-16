//! Ontology file management (CRUD operations with per-user storage).
//!
//! Provides [`OntologyManager`] for uploading, listing, retrieving, and deleting
//! ontology files, with per-user isolation and JSON metadata sidecars.
//!
//! Matches the Python `OntologyService` API from
//! `cognee/api/v1/ontologies/ontologies.py`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{OntologyError, OntologyResult};
use crate::noop::NoOpOntologyResolver;
use crate::rdflib::RdfLibOntologyResolver;
use crate::traits::OntologyResolver;

/// Accepted RDF file extensions for ontology uploads.
const VALID_EXTENSIONS: &[&str] = &["owl", "ttl", "rdf", "xml", "nt", "jsonld"];

/// Validate that an ontology key is safe for use as a filename component.
///
/// Rejects keys containing path separators, traversal sequences, or control
/// characters that could escape the per-user storage directory.
fn validate_ontology_key(key: &str) -> OntologyResult<()> {
    if key.is_empty() {
        return Err(OntologyError::InvalidFormat(
            "Ontology key must not be empty".into(),
        ));
    }
    if key.contains('/')
        || key.contains('\\')
        || key.contains('\0')
        || key.contains("..")
        || key == "."
    {
        return Err(OntologyError::InvalidFormat(
            "Ontology key must not contain path separators, '..', or null bytes".into(),
        ));
    }
    Ok(())
}

/// Metadata for a single uploaded ontology file.
///
/// Matches Python's `OntologyMetadata` dataclass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyMetadata {
    /// User-defined key identifying this ontology.
    pub ontology_key: String,
    /// Original filename as provided at upload time.
    pub filename: String,
    /// Size of the file in bytes.
    pub size_bytes: u64,
    /// Timestamp when the file was uploaded (UTC, ISO 8601).
    pub uploaded_at: DateTime<Utc>,
    /// Optional human-readable description.
    pub description: Option<String>,
}

/// File management layer for per-user ontology CRUD operations.
///
/// Storage layout mirrors the Python `OntologyService`:
/// ```text
/// <system_root>/ontologies/<user_id>/
///   metadata.json           # { "key1": { ... }, "key2": { ... } }
///   key1.owl
///   key2.ttl
/// ```
///
/// The manager is intentionally separate from [`OntologyResolver`] (which is
/// synchronous, in-memory, and stateless after initialization).
pub struct OntologyManager {
    base_dir: PathBuf,
}

impl OntologyManager {
    /// Create a new manager rooted at `system_root`.
    ///
    /// Ontology files are stored under `<system_root>/ontologies/<user_id>/`.
    pub fn new(system_root: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: system_root.into().join("ontologies"),
        }
    }

    /// Directory for a specific user's ontology files.
    fn user_dir(&self, user_id: Uuid) -> PathBuf {
        self.base_dir.join(user_id.to_string())
    }

    /// Path to the per-user metadata sidecar file.
    fn metadata_path(&self, user_id: Uuid) -> PathBuf {
        self.user_dir(user_id).join("metadata.json")
    }

    // ── Metadata persistence helpers ────────────────────────────────────

    /// Load the metadata map for a user, returning an empty map if none exists.
    async fn load_metadata(
        &self,
        user_id: Uuid,
    ) -> OntologyResult<HashMap<String, OntologyMetadata>> {
        let path = self.metadata_path(user_id);
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))?;
        serde_json::from_str(&content)
            .map_err(|e| OntologyError::ParseError(format!("metadata.json: {e}")))
    }

    /// Persist the metadata map for a user (atomic: write temp file + rename).
    async fn save_metadata(
        &self,
        user_id: Uuid,
        metadata: &HashMap<String, OntologyMetadata>,
    ) -> OntologyResult<()> {
        let path = self.metadata_path(user_id);
        let content = serde_json::to_string_pretty(metadata)
            .map_err(|e| OntologyError::ParseError(format!("metadata.json: {e}")))?;
        let tmp_path = path.with_extension("json.tmp");
        tokio::fs::write(&tmp_path, content)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))?;
        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))?;
        Ok(())
    }

    // ── Public API ──────────────────────────────────────────────────────

    /// Upload a single ontology file.
    ///
    /// Validates the file extension against the set of supported RDF formats,
    /// stores the file on disk, and records metadata in the per-user sidecar.
    ///
    /// Matches Python's `OntologyService.upload_ontology()`.
    ///
    /// # Errors
    ///
    /// - [`OntologyError::InvalidFormat`] if the extension is not recognized
    /// - [`OntologyError::DuplicateKey`] if `ontology_key` already exists for this user
    /// - [`OntologyError::Io`] on file system errors
    pub async fn upload(
        &self,
        user_id: Uuid,
        ontology_key: &str,
        filename: &str,
        content: &[u8],
        description: Option<&str>,
    ) -> OntologyResult<OntologyMetadata> {
        // 0. Validate ontology key is safe for use as a path component
        validate_ontology_key(ontology_key)?;

        // 1. Validate file extension
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| {
                OntologyError::InvalidFormat("File must have a recognized RDF extension".into())
            })?;

        let ext_lower = ext.to_lowercase();
        if !VALID_EXTENSIONS.contains(&ext_lower.as_str()) {
            return Err(OntologyError::InvalidFormat(format!(
                "Unsupported extension '.{}'. Accepted: {}",
                ext,
                VALID_EXTENSIONS.join(", ")
            )));
        }

        // 2. Check for duplicate key
        let mut metadata = self.load_metadata(user_id).await?;
        if metadata.contains_key(ontology_key) {
            return Err(OntologyError::DuplicateKey(ontology_key.to_string()));
        }

        // 3. Ensure user directory exists and write the file
        let dir = self.user_dir(user_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))?;

        let stored_filename = format!("{ontology_key}.{ext_lower}");
        let file_path = dir.join(&stored_filename);
        tokio::fs::write(&file_path, content)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))?;

        // 4. Update and persist metadata
        let meta = OntologyMetadata {
            ontology_key: ontology_key.to_string(),
            filename: filename.to_string(),
            size_bytes: content.len() as u64,
            uploaded_at: Utc::now(),
            description: description.map(|s| s.to_string()),
        };
        metadata.insert(ontology_key.to_string(), meta.clone());
        self.save_metadata(user_id, &metadata).await?;

        Ok(meta)
    }

    /// Batch upload multiple ontology files.
    ///
    /// Validates that there are no duplicate keys within the batch, then uploads
    /// each file sequentially. If any upload fails, previously-uploaded files in
    /// the batch remain on disk (no rollback).
    ///
    /// Matches Python's `OntologyService.upload_ontologies()`.
    pub async fn upload_batch(
        &self,
        user_id: Uuid,
        items: Vec<(String, String, Vec<u8>, Option<String>)>,
    ) -> OntologyResult<Vec<OntologyMetadata>> {
        // Validate no duplicate keys within the batch
        let keys: Vec<&str> = items.iter().map(|(k, _, _, _)| k.as_str()).collect();
        let unique: HashSet<&str> = keys.iter().copied().collect();
        if unique.len() != keys.len() {
            return Err(OntologyError::DuplicateKey(
                "Duplicate keys in batch".into(),
            ));
        }

        let mut results = Vec::with_capacity(items.len());
        for (key, filename, content, description) in &items {
            results.push(
                self.upload(user_id, key, filename, content, description.as_deref())
                    .await?,
            );
        }
        Ok(results)
    }

    /// List all ontologies for a user.
    ///
    /// Returns a map from ontology key to metadata. Returns an empty map if the
    /// user has no uploads.
    ///
    /// Matches Python's `OntologyService.list_ontologies()`.
    pub async fn list(&self, user_id: Uuid) -> OntologyResult<HashMap<String, OntologyMetadata>> {
        self.load_metadata(user_id).await
    }

    /// Get file contents for a single ontology key.
    ///
    /// Returns the raw bytes of the stored ontology file.
    ///
    /// Matches Python's `OntologyService.get_ontology_contents()` (single key).
    ///
    /// # Errors
    ///
    /// - [`OntologyError::NotFound`] if the key does not exist in metadata or the
    ///   file is missing from disk.
    pub async fn get_contents(&self, user_id: Uuid, ontology_key: &str) -> OntologyResult<Vec<u8>> {
        validate_ontology_key(ontology_key)?;
        let metadata = self.load_metadata(user_id).await?;
        let meta = metadata.get(ontology_key).ok_or_else(|| {
            OntologyError::NotFound(format!("Ontology key '{ontology_key}' not found"))
        })?;

        // Determine stored file extension from original filename
        let ext = Path::new(&meta.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("owl");
        let stored_filename = format!("{}.{}", ontology_key, ext.to_lowercase());
        let path = self.user_dir(user_id).join(stored_filename);

        if !path.exists() {
            return Err(OntologyError::NotFound(format!(
                "Ontology file for key '{ontology_key}' not found on disk"
            )));
        }

        tokio::fs::read(&path)
            .await
            .map_err(|e| OntologyError::Io(e.to_string()))
    }

    /// Get file contents for multiple ontology keys.
    ///
    /// Matches Python's `OntologyService.get_ontology_contents()` (list variant).
    pub async fn get_contents_batch(
        &self,
        user_id: Uuid,
        ontology_keys: &[&str],
    ) -> OntologyResult<Vec<Vec<u8>>> {
        let mut results = Vec::with_capacity(ontology_keys.len());
        for key in ontology_keys {
            results.push(self.get_contents(user_id, key).await?);
        }
        Ok(results)
    }

    /// Delete an ontology file by key.
    ///
    /// Removes both the file from disk and its metadata entry. No Python
    /// equivalent (Rust-only addition).
    ///
    /// # Errors
    ///
    /// - [`OntologyError::NotFound`] if the key does not exist
    /// - [`OntologyError::Io`] on file system errors
    pub async fn delete(&self, user_id: Uuid, ontology_key: &str) -> OntologyResult<()> {
        validate_ontology_key(ontology_key)?;
        let mut metadata = self.load_metadata(user_id).await?;
        let meta = metadata.remove(ontology_key).ok_or_else(|| {
            OntologyError::NotFound(format!("Ontology key '{ontology_key}' not found"))
        })?;

        // Remove the file from disk
        let ext = Path::new(&meta.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("owl");
        let stored_filename = format!("{}.{}", ontology_key, ext.to_lowercase());
        let path = self.user_dir(user_id).join(stored_filename);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| OntologyError::Io(e.to_string()))?;
        }

        // Persist updated metadata
        self.save_metadata(user_id, &metadata).await?;
        Ok(())
    }

    /// Build an [`OntologyResolver`] from all uploaded ontology files for a user.
    ///
    /// Returns [`NoOpOntologyResolver`] if the user has no uploaded files.
    ///
    /// This is a synchronous operation because `RdfLibOntologyResolver::new()`
    /// loads and parses files synchronously using sophia.
    pub fn build_resolver(&self, user_id: Uuid) -> OntologyResult<Box<dyn OntologyResolver>> {
        let dir = self.user_dir(user_id);
        if !dir.exists() {
            return Ok(Box::new(NoOpOntologyResolver::new()));
        }

        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| OntologyError::Io(e.to_string()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| VALID_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
            })
            .collect();

        if files.is_empty() {
            return Ok(Box::new(NoOpOntologyResolver::new()));
        }

        Ok(Box::new(RdfLibOntologyResolver::new(files)?))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn temp_manager() -> (tempfile::TempDir, OntologyManager) {
        let dir = tempfile::tempdir().expect("tempdir should be creatable");
        let manager = OntologyManager::new(dir.path());
        (dir, manager)
    }

    fn sample_turtle() -> Vec<u8> {
        br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/Vehicle> a owl:Class ;
    rdfs:label "Vehicle" .
<http://example.org/Car> a owl:Class ;
    rdfs:subClassOf <http://example.org/Vehicle> ;
    rdfs:label "Car" .
"#
        .to_vec()
    }

    #[tokio::test]
    async fn test_upload_and_list() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let meta = mgr
            .upload(
                user,
                "my-ontology",
                "ontology.ttl",
                &sample_turtle(),
                Some("Test"),
            )
            .await
            .unwrap();

        assert_eq!(meta.ontology_key, "my-ontology");
        assert_eq!(meta.filename, "ontology.ttl");
        assert_eq!(meta.size_bytes, sample_turtle().len() as u64);
        assert_eq!(meta.description.as_deref(), Some("Test"));

        let list = mgr.list(user).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.contains_key("my-ontology"));
    }

    #[tokio::test]
    async fn test_duplicate_key_rejected() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        mgr.upload(user, "dup-key", "a.ttl", b"data", None)
            .await
            .unwrap();

        let result = mgr.upload(user, "dup-key", "b.ttl", b"other", None).await;
        assert!(matches!(result, Err(OntologyError::DuplicateKey(_))));
    }

    #[tokio::test]
    async fn test_get_contents_roundtrip() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();
        let content = sample_turtle();

        mgr.upload(user, "rt", "schema.owl", &content, None)
            .await
            .unwrap();

        let retrieved = mgr.get_contents(user, "rt").await.unwrap();
        assert_eq!(retrieved, content);
    }

    #[tokio::test]
    async fn test_delete_removes_file_and_metadata() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        mgr.upload(user, "to-delete", "schema.ttl", b"data", None)
            .await
            .unwrap();

        mgr.delete(user, "to-delete").await.unwrap();

        let list = mgr.list(user).await.unwrap();
        assert!(list.is_empty());

        let result = mgr.get_contents(user, "to-delete").await;
        assert!(matches!(result, Err(OntologyError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_invalid_format_rejected() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let result = mgr.upload(user, "bad", "data.txt", b"hello", None).await;
        assert!(matches!(result, Err(OntologyError::InvalidFormat(_))));

        let result = mgr.upload(user, "bad", "noext", b"hello", None).await;
        assert!(matches!(result, Err(OntologyError::InvalidFormat(_))));
    }

    #[tokio::test]
    async fn test_valid_extensions_accepted() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        for (key, filename) in [
            ("a", "a.owl"),
            ("b", "b.ttl"),
            ("c", "c.rdf"),
            ("d", "d.xml"),
            ("e", "e.nt"),
            ("f", "f.jsonld"),
        ] {
            mgr.upload(user, key, filename, b"data", None)
                .await
                .unwrap();
        }
        assert_eq!(mgr.list(user).await.unwrap().len(), 6);
    }

    #[tokio::test]
    async fn test_empty_state() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let list = mgr.list(user).await.unwrap();
        assert!(list.is_empty());

        let result = mgr.get_contents(user, "nonexistent").await;
        assert!(matches!(result, Err(OntologyError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_batch_upload() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let items = vec![
            (
                "k1".into(),
                "a.ttl".into(),
                b"aaa".to_vec(),
                Some("first".into()),
            ),
            ("k2".into(), "b.owl".into(), b"bbb".to_vec(), None),
        ];
        let results = mgr.upload_batch(user, items).await.unwrap();
        assert_eq!(results.len(), 2);

        let list = mgr.list(user).await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.contains_key("k1"));
        assert!(list.contains_key("k2"));
    }

    #[tokio::test]
    async fn test_batch_upload_duplicate_keys_in_batch() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let items = vec![
            ("same".into(), "a.ttl".into(), b"aaa".to_vec(), None),
            ("same".into(), "b.owl".into(), b"bbb".to_vec(), None),
        ];
        let result = mgr.upload_batch(user, items).await;
        assert!(matches!(result, Err(OntologyError::DuplicateKey(_))));
    }

    #[tokio::test]
    async fn test_build_resolver_no_files() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let resolver = mgr.build_resolver(user).unwrap();
        assert!(!resolver.is_loaded());
    }

    #[tokio::test]
    async fn test_build_resolver_with_turtle_file() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        mgr.upload(user, "vehicles", "vehicles.ttl", &sample_turtle(), None)
            .await
            .unwrap();

        let resolver = mgr.build_resolver(user).unwrap();
        assert!(resolver.is_loaded());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_key() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        let result = mgr.delete(user, "nope").await;
        assert!(matches!(result, Err(OntologyError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_path_traversal_rejected() {
        let (_dir, mgr) = temp_manager();
        let user = Uuid::new_v4();

        for bad_key in ["../escape", "foo/bar", "foo\\bar", "..", ".", "a\0b", ""] {
            let result = mgr.upload(user, bad_key, "a.ttl", b"data", None).await;
            assert!(
                result.is_err(),
                "Expected error for key '{}', got Ok",
                bad_key.escape_debug()
            );
        }
    }

    #[tokio::test]
    async fn test_user_isolation() {
        let (_dir, mgr) = temp_manager();
        let user1 = Uuid::new_v4();
        let user2 = Uuid::new_v4();

        mgr.upload(user1, "shared-key", "a.ttl", b"user1-data", None)
            .await
            .unwrap();
        mgr.upload(user2, "shared-key", "b.owl", b"user2-data", None)
            .await
            .unwrap();

        let c1 = mgr.get_contents(user1, "shared-key").await.unwrap();
        let c2 = mgr.get_contents(user2, "shared-key").await.unwrap();
        assert_eq!(c1, b"user1-data");
        assert_eq!(c2, b"user2-data");
    }
}
