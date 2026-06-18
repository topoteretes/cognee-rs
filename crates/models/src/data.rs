use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a piece of data in the system, such as a file or a text.
/// Fields match the Python cognee `data` table schema for cross-SDK compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Data {
    /// Unique identifier for this data record (UUID v5, deterministic from content hash)
    pub id: Uuid,
    /// Display name derived from the source (filename, URL, or `text_<md5>.txt` for inline text)
    pub name: String,
    /// `file://` URI pointing to the stored raw content in the file storage backend
    pub raw_data_location: String,
    /// Original source location before any processing (file path, URL, or same as `raw_data_location` for inline text)
    pub original_data_location: String,
    /// File extension of the stored content (e.g. "txt", "pdf", "html")
    pub extension: String,
    /// MIME type of the stored content (e.g. "text/plain", "application/pdf")
    pub mime_type: String,
    /// MD5 hex digest of the raw content bytes (content-only, no owner mixing)
    pub content_hash: String,
    /// ID of the user or agent that owns this data record
    pub owner_id: Uuid,
    /// Timestamp when this record was first created
    pub created_at: DateTime<Utc>,
    /// Timestamp of the last update to this record, if any
    pub updated_at: Option<DateTime<Utc>>,
    /// Human-readable label for the data item (from DataItem wrapper or user-provided)
    pub label: Option<String>,
    /// Original file extension before any conversion
    pub original_extension: Option<String>,
    /// Original MIME type before any conversion
    pub original_mime_type: Option<String>,
    /// Python loader engine name (e.g. "text_loader", "pypdf_loader")
    pub loader_engine: Option<String>,
    /// MD5 hash of the **extracted-text** file stored by the loader at ADD time
    /// (Python parity, `ingest_data.py:195`). Equals [`content_hash`](Self::content_hash)
    /// only when the extracted text is byte-identical to the raw input (plain
    /// text); for inputs the loader transforms (PDF, CSV, HTML, image, audio)
    /// the two hashes differ.
    pub raw_content_hash: Option<String>,
    /// Tenant/organisation ID for multi-tenant isolation
    pub tenant_id: Option<Uuid>,
    /// Arbitrary JSON metadata blob
    pub external_metadata: Option<String>,
    /// JSON list of node IDs associated with this data item
    pub node_set: Option<String>,
    /// Pipeline processing status
    pub pipeline_status: Option<String>,
    /// Token count of the data (-1 = not yet computed)
    pub token_count: i64,
    /// Size of the data in bytes (-1 = not yet computed)
    pub data_size: i64,
    /// Last access timestamp
    pub last_accessed: Option<DateTime<Utc>>,
    /// Importance weight for ranking (0.0 to 1.0). Influences relevance scoring.
    pub importance_weight: Option<f64>,
}

impl Data {
    /// Start building a new `Data` record with the required fields.
    /// All optional fields default to `None`; `data_size` defaults to `-1`.
    #[allow(clippy::too_many_arguments)]
    pub fn builder(
        id: Uuid,
        name: impl Into<String>,
        raw_data_location: impl Into<String>,
        original_data_location: impl Into<String>,
        extension: impl Into<String>,
        mime_type: impl Into<String>,
        content_hash: impl Into<String>,
        owner_id: Uuid,
    ) -> DataBuilder {
        DataBuilder {
            id,
            name: name.into(),
            raw_data_location: raw_data_location.into(),
            original_data_location: original_data_location.into(),
            extension: extension.into(),
            mime_type: mime_type.into(),
            content_hash: content_hash.into(),
            owner_id,
            tenant_id: None,
            label: None,
            original_extension: None,
            original_mime_type: None,
            loader_engine: None,
            raw_content_hash: None,
            external_metadata: None,
            node_set: None,
            importance_weight: None,
            data_size: -1,
        }
    }
}

/// Builder for [`Data`]. Obtain via [`Data::builder`].
pub struct DataBuilder {
    id: Uuid,
    name: String,
    raw_data_location: String,
    original_data_location: String,
    extension: String,
    mime_type: String,
    content_hash: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    label: Option<String>,
    original_extension: Option<String>,
    original_mime_type: Option<String>,
    loader_engine: Option<String>,
    raw_content_hash: Option<String>,
    external_metadata: Option<String>,
    node_set: Option<String>,
    importance_weight: Option<f64>,
    data_size: i64,
}

impl DataBuilder {
    pub fn tenant_id(mut self, v: Uuid) -> Self {
        self.tenant_id = Some(v);
        self
    }
    pub fn label(mut self, v: impl Into<String>) -> Self {
        self.label = Some(v.into());
        self
    }
    pub fn original_extension(mut self, v: impl Into<String>) -> Self {
        self.original_extension = Some(v.into());
        self
    }
    pub fn original_mime_type(mut self, v: impl Into<String>) -> Self {
        self.original_mime_type = Some(v.into());
        self
    }
    pub fn loader_engine(mut self, v: impl Into<String>) -> Self {
        self.loader_engine = Some(v.into());
        self
    }
    pub fn raw_content_hash(mut self, v: impl Into<String>) -> Self {
        self.raw_content_hash = Some(v.into());
        self
    }
    pub fn external_metadata(mut self, v: impl Into<String>) -> Self {
        self.external_metadata = Some(v.into());
        self
    }
    pub fn data_size(mut self, v: i64) -> Self {
        self.data_size = v;
        self
    }
    pub fn node_set(mut self, v: impl Into<String>) -> Self {
        self.node_set = Some(v.into());
        self
    }
    pub fn importance_weight(mut self, w: f64) -> Self {
        self.importance_weight = Some(w);
        self
    }

    pub fn build(self) -> Data {
        Data {
            id: self.id,
            name: self.name,
            raw_data_location: self.raw_data_location,
            original_data_location: self.original_data_location,
            extension: self.extension,
            mime_type: self.mime_type,
            content_hash: self.content_hash,
            owner_id: self.owner_id,
            created_at: Utc::now(),
            updated_at: None,
            tenant_id: self.tenant_id,
            label: self.label,
            original_extension: self.original_extension,
            original_mime_type: self.original_mime_type,
            loader_engine: self.loader_engine,
            raw_content_hash: self.raw_content_hash,
            external_metadata: self.external_metadata,
            node_set: self.node_set,
            pipeline_status: None,
            // TODO(COG-4456): compute token_count at ingestion time using TokenCounterKind::from_env()
            // so the field is populated on add rather than remaining -1 until cognify runs.
            token_count: -1,
            data_size: self.data_size,
            last_accessed: None,
            importance_weight: self.importance_weight,
        }
    }
}
