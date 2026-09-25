//! Streaming file response builders for `GET /{dataset_id}/data/{data_id}/raw`.
//!
//! Two builders:
//!  - `serve_local_file` — reads from the local filesystem via `tokio::fs::File`.
//!  - `serve_bytes` — wraps an already-buffered `Vec<u8>` (used for small files
//!    or when the caller has already read the data).
//!
//! S3 support is intentionally left as a 501 — `cognee-storage` does not yet
//! expose an async-read S3 backend.

use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use tokio_util::io::ReaderStream;

use crate::error::ApiError;

/// Build a streaming `200 OK` response that reads `path` from disk.
///
/// Sets `Content-Type`, `Content-Disposition: attachment; filename="<name>"`,
/// and `Content-Length` from `fs::metadata().len()`.
pub async fn serve_local_file(
    path: &std::path::Path,
    download_name: &str,
    mime: &str,
) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ApiError::NotFound(format!("raw file not found on disk: {e}")))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("metadata read error: {e}")))?;
    let content_length = metadata.len();

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let content_disposition = format!("attachment; filename=\"{download_name}\"");

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        )
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&content_disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .header(header::CONTENT_LENGTH, content_length.to_string())
        .body(body)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;

    Ok(response)
}

/// Build a `200 OK` response from a `Vec<u8>` buffer.
///
/// Omits `Content-Length` because it can be computed from the bytes, but
/// axum will set it automatically via its body chunking.
#[allow(clippy::result_large_err)] // ApiError is inherently large; boxing at this level adds noise
pub fn serve_bytes(data: Vec<u8>, download_name: &str, mime: &str) -> Result<Response, ApiError> {
    let content_disposition = format!("attachment; filename=\"{download_name}\"");

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        )
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&content_disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .header(header::CONTENT_LENGTH, data.len().to_string())
        .body(Body::from(data))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))?;

    Ok(response)
}
