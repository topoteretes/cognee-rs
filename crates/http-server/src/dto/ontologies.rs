//! DTOs for `/api/v1/ontologies`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Multipart form for `POST /api/v1/ontologies`. OpenAPI-only DTO; the
/// handler reads parts manually.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]
pub struct OntologyUploadMultipart {
    /// User-defined identifier; must not start with `[` or `{`.
    pub ontology_key: String,

    /// The OWL file. Filename must end in `.owl` (case-insensitive).
    /// Exactly one allowed.
    #[schema(format = "binary")]
    pub ontology_file: Vec<u8>,

    /// Optional human-readable description; must not start with `[` or `{`.
    pub description: Option<String>,
}

/// Wire shape of a single uploaded-ontology entry. Snake_case (raw dict in
/// Python, not `OutDTO`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OntologyMetadataDTO {
    pub ontology_key: String,
    pub filename: String,
    pub size_bytes: u64,
    /// ISO-8601 UTC timestamp with microsecond precision (Python's
    /// `datetime.now(timezone.utc).isoformat()` shape).
    pub uploaded_at: String,
    pub description: Option<String>,
}

/// Response body for `POST /api/v1/ontologies` — always one entry in `uploaded_ontologies`.
#[derive(Debug, Serialize, ToSchema)]
pub struct OntologyUploadResponseDTO {
    pub uploaded_ontologies: Vec<OntologyMetadataDTO>,
}

/// Response body for `GET /api/v1/ontologies` — map of key → metadata.
/// We use `BTreeMap` for deterministic ordering in tests.
pub type OntologyListResponseDTO = BTreeMap<String, OntologyListEntryDTO>;

/// Per-entry metadata as written into `metadata.json`. Distinct from
/// `OntologyMetadataDTO` because the listing entry omits `ontology_key`
/// (it's the map key) — Python writes a 4-field dict.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OntologyListEntryDTO {
    pub filename: String,
    pub size_bytes: u64,
    pub uploaded_at: String,
    pub description: Option<String>,
}

/// `{error: String}` envelope. Distinct from the canonical `ApiError`
/// `{detail: ...}` and from the `add`/`update` `{error, detail}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct OntologyErrorResponseDTO {
    pub error: String,
}
