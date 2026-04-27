//! `GET /api/v1/ontologies` and `POST /api/v1/ontologies` — list and upload ontologies.
//!
//! Python parity: `cognee/api/v1/ontologies/routers/get_ontologies_router.py`.
//! Rust delegation: `cognee_ontology::OntologyManager` (via `state.components()`).

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use cognee_ontology::OntologyError;

use crate::auth::AuthenticatedUser;
use crate::dto::ontologies::{
    OntologyListEntryDTO, OntologyListResponseDTO, OntologyMetadataDTO, OntologyUploadResponseDTO,
};
use crate::error::ApiError;
use crate::multipart::{MultipartOpts, UploadGuard, parse_multipart};
use crate::state::AppState;

use axum::extract::Multipart;

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Convert a `chrono::DateTime<Utc>` to an ISO-8601 string.
fn format_uploaded_at(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

// ─── get_list ─────────────────────────────────────────────────────────────────

/// `GET /api/v1/ontologies` — list all ontologies for the authenticated user.
///
/// Empty result → `{}` (200, not 404).
pub async fn get_list(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<OntologyListResponseDTO>, ApiError> {
    let manager = state
        .components()
        .ok_or_else(|| {
            ApiError::OntologyEnvelope(
                "components not initialized".into(),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?
        .ontology_manager
        .clone();

    let map = manager.list(user.id).await.map_err(|e| {
        ApiError::OntologyEnvelope(e.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
    })?;

    let result: OntologyListResponseDTO = map
        .into_iter()
        .map(|(key, meta)| {
            (
                key,
                OntologyListEntryDTO {
                    filename: meta.filename,
                    size_bytes: meta.size_bytes,
                    uploaded_at: format_uploaded_at(meta.uploaded_at),
                    description: meta.description,
                },
            )
        })
        .collect();

    Ok(Json(result))
}

// ─── post_upload ──────────────────────────────────────────────────────────────

/// `POST /api/v1/ontologies` — upload one ontology file.
///
/// Validation rules (Python parity):
/// - Exactly one `ontology_file` part (>1 → 400).
/// - `ontology_key.trim()` must not start with `[` or `{`.
/// - Filename must end in `.owl` (case-insensitive) for Python parity.
/// - Buffer the file fully into memory before writing (Python parity).
pub async fn post_upload(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<OntologyUploadResponseDTO>, ApiError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let opts = MultipartOpts::default();
    let mut parsed = parse_multipart(multipart, &opts, &request_id).await?;
    let _guard = UploadGuard::new(parsed.spool_dir.clone());

    // ── Extract fields ────────────────────────────────────────────────────
    let ontology_key = parsed
        .fields
        .get("ontology_key")
        .and_then(|v| v.first())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();

    if ontology_key.starts_with('[') || ontology_key.starts_with('{') {
        return Err(ApiError::OntologyEnvelope(
            "ontology_key must not start with '[' or '{'".into(),
            StatusCode::BAD_REQUEST,
        ));
    }
    if ontology_key.is_empty() {
        return Err(ApiError::OntologyEnvelope(
            "ontology_key is required".into(),
            StatusCode::BAD_REQUEST,
        ));
    }

    let description: Option<String> = parsed
        .fields
        .get("description")
        .and_then(|v| v.first())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('[') || s.starts_with('{') {
                Err(ApiError::OntologyEnvelope(
                    "description must not start with '[' or '{'".into(),
                    StatusCode::BAD_REQUEST,
                ))
            } else {
                Ok(s)
            }
        })
        .transpose()?;

    // ── File parts ────────────────────────────────────────────────────────
    let spooled_files = parsed.files.remove("ontology_file").unwrap_or_default();

    if spooled_files.len() > 1 {
        return Err(ApiError::OntologyEnvelope(
            "Only one ontology_file is allowed".into(),
            StatusCode::BAD_REQUEST,
        ));
    }
    let spooled = spooled_files.into_iter().next().ok_or_else(|| {
        ApiError::OntologyEnvelope(
            "ontology_file part is required".into(),
            StatusCode::BAD_REQUEST,
        )
    })?;

    // Validate filename ends in .owl (Python parity — stricter than manager).
    let filename = spooled
        .filename
        .clone()
        .unwrap_or_else(|| format!("{}.owl", ontology_key));
    if !filename.to_lowercase().ends_with(".owl") {
        return Err(ApiError::OntologyEnvelope(
            "File must be in .owl format".into(),
            StatusCode::BAD_REQUEST,
        ));
    }

    // Buffer the full file into memory (Python parity per spec §3.4).
    let content = tokio::fs::read(&spooled.path)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("spool read error: {e}")))?;

    // ── Upload via OntologyManager ────────────────────────────────────────
    let manager = state
        .components()
        .ok_or_else(|| {
            ApiError::OntologyEnvelope(
                "components not initialized".into(),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?
        .ontology_manager
        .clone();

    let meta = manager
        .upload(
            user.id,
            &ontology_key,
            &filename,
            &content,
            description.as_deref(),
        )
        .await
        .map_err(|e| match e {
            OntologyError::DuplicateKey(ref key) => ApiError::OntologyEnvelope(
                format!("Ontology key '{}' already exists", key),
                StatusCode::BAD_REQUEST,
            ),
            OntologyError::InvalidFormat(ref msg) => {
                ApiError::OntologyEnvelope(msg.clone(), StatusCode::BAD_REQUEST)
            }
            other => {
                ApiError::OntologyEnvelope(other.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
            }
        })?;

    let dto = OntologyMetadataDTO {
        ontology_key: meta.ontology_key,
        filename: meta.filename,
        size_bytes: meta.size_bytes,
        uploaded_at: format_uploaded_at(meta.uploaded_at),
        description: meta.description,
    };

    Ok(Json(OntologyUploadResponseDTO {
        uploaded_ontologies: vec![dto],
    }))
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_list))
        .route("/", post(post_upload))
}
