# Implementation Plan: Ontology Management (Gap 7)

This document provides the step-by-step implementation plan for adding ontology file management capabilities to the Rust SDK, matching the Python `OntologyService` API.

---

## Goal

Implement file upload, listing, retrieval, and deletion for ontology files, with per-user storage and metadata tracking. This bridges the gap between the Python `OntologyService` (in `cognee/api/v1/ontologies/ontologies.py`) and the Rust crate, which currently only supports ontology *resolution* (loading + matching + subgraph extraction) but not *management* (CRUD operations on ontology files).

---

## Design Overview

### OntologyManager Struct

A new `OntologyManager` struct owns the file storage directory and provides async methods for CRUD operations on ontology files. It is intentionally separate from `OntologyResolver` (which is synchronous, in-memory, and stateless after init).

```rust
use std::path::PathBuf;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyMetadata {
    pub ontology_key: String,
    pub filename: String,
    pub size_bytes: u64,
    pub uploaded_at: DateTime<Utc>,
    pub description: Option<String>,
}

pub struct OntologyManager {
    base_dir: PathBuf,
}
```

### File Storage Layout

Mirrors the Python layout: `<base_dir>/ontologies/<user_id>/` with a `metadata.json` sidecar.

```
<system_root>/ontologies/
  <user_id>/
    metadata.json           # { "key1": { filename, size_bytes, uploaded_at, description }, ... }
    key1.owl
    key2.ttl
    ...
```

### Key Differences from Python

| Aspect | Python | Rust (planned) |
|--------|--------|----------------|
| Base directory | `tempfile.gettempdir()` | Configurable `system_root` (from `Settings`) |
| Accepted formats | `.owl` only | `.owl`, `.ttl`, `.rdf`, `.xml`, `.nt`, `.jsonld` (matches loader) |
| Stored filename | `{ontology_key}.owl` | `{ontology_key}.{ext}` (preserves original extension) |
| API surface | `OntologyService` class | `OntologyManager` struct |
| get_ontology_contents | Returns `List[str]` (multiple keys) | Returns `Vec<u8>` for single key (binary-safe) |

---

## Step-by-Step Plan

### Step 1: Add OntologyManager to the ontology crate

**File:** `crates/ontology/src/manager.rs` (new)

Create the `OntologyManager` struct with directory helpers:

```rust
impl OntologyManager {
    pub fn new(system_root: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: system_root.into().join("ontologies"),
        }
    }

    fn user_dir(&self, user_id: Uuid) -> PathBuf {
        self.base_dir.join(user_id.to_string())
    }

    fn metadata_path(&self, user_id: Uuid) -> PathBuf {
        self.user_dir(user_id).join("metadata.json")
    }
}
```

**Rationale:** Placing the manager in the `cognee-ontology` crate keeps all ontology-related code together. The manager only depends on `std`, `tokio`, `serde`, `serde_json`, `chrono`, and `uuid` -- all already available in the workspace.

### Step 2: Implement metadata persistence

Private helper methods for loading/saving the `metadata.json` sidecar:

```rust
impl OntologyManager {
    async fn load_metadata(&self, user_id: Uuid) -> OntologyResult<HashMap<String, OntologyMetadata>> {
        let path = self.metadata_path(user_id);
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = tokio::fs::read_to_string(&path).await
            .map_err(|e| OntologyError::Io(e.to_string()))?;
        serde_json::from_str(&content)
            .map_err(|e| OntologyError::ParseError(format!("metadata.json: {}", e)))
    }

    async fn save_metadata(
        &self,
        user_id: Uuid,
        metadata: &HashMap<String, OntologyMetadata>,
    ) -> OntologyResult<()> {
        let path = self.metadata_path(user_id);
        let content = serde_json::to_string_pretty(metadata)
            .map_err(|e| OntologyError::ParseError(format!("metadata.json: {}", e)))?;
        tokio::fs::write(&path, content).await
            .map_err(|e| OntologyError::Io(e.to_string()))?;
        Ok(())
    }
}
```

### Step 3: Implement upload

```rust
impl OntologyManager {
    /// Upload an ontology file. Validates format and stores with metadata.
    ///
    /// Matches Python's `OntologyService.upload_ontology()`.
    pub async fn upload(
        &self,
        user_id: Uuid,
        ontology_key: &str,
        filename: &str,
        content: &[u8],
        description: Option<&str>,
    ) -> OntologyResult<OntologyMetadata> {
        // 1. Validate file extension (broader than Python's .owl-only)
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| OntologyError::InvalidFormat(
                "File must have a recognized RDF extension".into()
            ))?;

        let valid_extensions = ["owl", "ttl", "rdf", "xml", "nt", "jsonld"];
        if !valid_extensions.contains(&ext.to_lowercase().as_str()) {
            return Err(OntologyError::InvalidFormat(format!(
                "Unsupported extension '.{}'. Accepted: {}",
                ext,
                valid_extensions.join(", ")
            )));
        }

        // 2. Check for duplicate key
        let mut metadata = self.load_metadata(user_id).await?;
        if metadata.contains_key(ontology_key) {
            return Err(OntologyError::DuplicateKey(ontology_key.to_string()));
        }

        // 3. Ensure directory exists and write file
        let dir = self.user_dir(user_id);
        tokio::fs::create_dir_all(&dir).await
            .map_err(|e| OntologyError::Io(e.to_string()))?;

        let stored_filename = format!("{}.{}", ontology_key, ext.to_lowercase());
        let file_path = dir.join(&stored_filename);
        tokio::fs::write(&file_path, content).await
            .map_err(|e| OntologyError::Io(e.to_string()))?;

        // 4. Update metadata
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
    /// Matches Python's `OntologyService.upload_ontologies()`.
    pub async fn upload_batch(
        &self,
        user_id: Uuid,
        items: Vec<(String, String, Vec<u8>, Option<String>)>, // (key, filename, content, description)
    ) -> OntologyResult<Vec<OntologyMetadata>> {
        // Validate no duplicate keys in batch
        let keys: Vec<&str> = items.iter().map(|(k, _, _, _)| k.as_str()).collect();
        let unique: HashSet<&str> = keys.iter().copied().collect();
        if unique.len() != keys.len() {
            return Err(OntologyError::DuplicateKey("Duplicate keys in batch".into()));
        }

        let mut results = Vec::with_capacity(items.len());
        for (key, filename, content, description) in &items {
            results.push(
                self.upload(user_id, key, filename, content, description.as_deref()).await?
            );
        }
        Ok(results)
    }
}
```

### Step 4: Implement list and get

```rust
impl OntologyManager {
    /// List all ontologies for a user.
    ///
    /// Matches Python's `OntologyService.list_ontologies()`.
    pub async fn list(
        &self,
        user_id: Uuid,
    ) -> OntologyResult<HashMap<String, OntologyMetadata>> {
        self.load_metadata(user_id).await
    }

    /// Get file contents by ontology key.
    ///
    /// Matches Python's `OntologyService.get_ontology_contents()` (single key variant).
    pub async fn get_contents(
        &self,
        user_id: Uuid,
        ontology_key: &str,
    ) -> OntologyResult<Vec<u8>> {
        let metadata = self.load_metadata(user_id).await?;
        let meta = metadata.get(ontology_key)
            .ok_or_else(|| OntologyError::NotFound(
                format!("Ontology key '{}' not found", ontology_key)
            ))?;

        // Determine stored file extension from original filename
        let ext = Path::new(&meta.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("owl");
        let stored_filename = format!("{}.{}", ontology_key, ext.to_lowercase());
        let path = self.user_dir(user_id).join(stored_filename);

        if !path.exists() {
            return Err(OntologyError::NotFound(
                format!("Ontology file for key '{}' not found on disk", ontology_key)
            ));
        }

        tokio::fs::read(&path).await
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
}
```

### Step 5: Implement delete

```rust
impl OntologyManager {
    /// Delete an ontology file by key.
    ///
    /// No Python equivalent (not exposed in Python SDK yet).
    pub async fn delete(
        &self,
        user_id: Uuid,
        ontology_key: &str,
    ) -> OntologyResult<()> {
        let mut metadata = self.load_metadata(user_id).await?;
        let meta = metadata.remove(ontology_key)
            .ok_or_else(|| OntologyError::NotFound(
                format!("Ontology key '{}' not found", ontology_key)
            ))?;

        // Remove file
        let ext = Path::new(&meta.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("owl");
        let stored_filename = format!("{}.{}", ontology_key, ext.to_lowercase());
        let path = self.user_dir(user_id).join(stored_filename);
        if path.exists() {
            tokio::fs::remove_file(&path).await
                .map_err(|e| OntologyError::Io(e.to_string()))?;
        }

        // Update metadata
        self.save_metadata(user_id, &metadata).await?;
        Ok(())
    }
}
```

### Step 6: Integrate with OntologyResolver

Bridge method that builds an `OntologyResolver` from all uploaded files for a given user:

```rust
impl OntologyManager {
    /// Build an OntologyResolver from all uploaded ontology files for a user.
    ///
    /// Returns `NoOpOntologyResolver` if no files are uploaded.
    pub fn build_resolver(
        &self,
        user_id: Uuid,
    ) -> OntologyResult<Box<dyn OntologyResolver>> {
        let dir = self.user_dir(user_id);
        if !dir.exists() {
            return Ok(Box::new(NoOpOntologyResolver::new()));
        }

        let valid_extensions = ["owl", "ttl", "rdf", "xml", "nt", "jsonld"];
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| OntologyError::Io(e.to_string()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| valid_extensions.contains(&ext.to_lowercase().as_str()))
            })
            .collect();

        if files.is_empty() {
            return Ok(Box::new(NoOpOntologyResolver::new()));
        }

        Ok(Box::new(RdfLibOntologyResolver::new(files)?))
    }
}
```

### Step 7: Extend OntologyError

Add new variants to the existing `OntologyError` enum in `crates/ontology/src/error.rs`:

```rust
#[derive(Error, Debug)]
pub enum OntologyError {
    // Existing variants...
    #[error("Ontology file not found: {0}")]
    FileNotFound(String),
    #[error("Ontology parsing error: {0}")]
    ParseError(String),
    #[error("Entity matching error: {0}")]
    MatchingError(String),

    // New variants for management operations:
    #[error("Invalid ontology file format: {0}")]
    InvalidFormat(String),
    #[error("Ontology not found: {0}")]
    NotFound(String),
    #[error("Duplicate ontology key: {0}")]
    DuplicateKey(String),
    #[error("IO error: {0}")]
    Io(String),
}
```

### Step 8: Wire up in crate exports

**File:** `crates/ontology/src/lib.rs`

Add the new module and re-exports:

```rust
pub mod manager;

pub use manager::{OntologyManager, OntologyMetadata};
```

### Step 9: Re-export from cognee-lib

**File:** `crates/lib/src/lib.rs`

Re-export `OntologyManager` and `OntologyMetadata` so they are accessible from the top-level API:

```rust
pub use cognee_ontology::{OntologyManager, OntologyMetadata};
```

### Step 10: Add CLI subcommands (optional, lower priority)

Extend `crates/cli/` with ontology management subcommands:

- `cognee ontology upload <key> <file> [--description <desc>]`
- `cognee ontology list`
- `cognee ontology get <key>`
- `cognee ontology delete <key>`

### Step 11: Tests

Add tests in `crates/ontology/src/manager.rs` (unit) and `crates/ontology/tests/` (integration):

1. **Upload and list** -- upload a file, verify it appears in list with correct metadata
2. **Duplicate key rejection** -- upload with same key twice, expect error
3. **Get contents** -- upload, then retrieve, verify content matches
4. **Delete** -- upload, delete, verify removed from list and disk
5. **Format validation** -- reject `.txt`, accept `.owl`, `.ttl`, etc.
6. **Build resolver** -- upload an OWL file, build resolver, verify `is_loaded() == true`
7. **Empty state** -- list/get on user with no uploads returns empty/error
8. **Batch upload** -- upload multiple files, verify all present

---

## Dependencies

No new crate dependencies are needed. All required crates are already in the workspace:

- `tokio` (async file I/O)
- `serde` + `serde_json` (metadata serialization)
- `chrono` (timestamps)
- `uuid` (user IDs)
- `thiserror` (error types)

The `cognee-ontology` crate's `Cargo.toml` will need `tokio`, `serde`, `serde_json`, `chrono`, and `uuid` added as dependencies (some may already be present).

---

## Files to Create/Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ontology/src/manager.rs` | **Create** | `OntologyManager` + `OntologyMetadata` |
| `crates/ontology/src/error.rs` | **Modify** | Add `InvalidFormat`, `NotFound`, `DuplicateKey`, `Io` variants |
| `crates/ontology/src/lib.rs` | **Modify** | Add `pub mod manager` + re-exports |
| `crates/ontology/Cargo.toml` | **Modify** | Add `tokio`, `serde`, `serde_json`, `chrono`, `uuid` deps |
| `crates/lib/src/lib.rs` | **Modify** | Re-export `OntologyManager`, `OntologyMetadata` |
| `crates/ontology/tests/manager_tests.rs` | **Create** | Integration tests |
| `crates/cli/src/main.rs` | **Modify** (optional) | Add `ontology` subcommand group |
