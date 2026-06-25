//! Shared JSON marshalling helpers for the bindings facade.
//!
//! This module contains the **neon-free** subset of what was previously in
//! `ts/cognee-ts-neon/src/json.rs`. Helpers that require `neon::Context` /
//! `neon::Handle` (`stringify_js`, `parse_js`, `js_to_serde`, `js_to_value`,
//! `read_opts`) stay in `cognee-ts-neon`; they are the JS↔serde bridge halves.
//!
//! ## Shared result-building helpers
//!
//! `cognify_result_json` and `marshal_inputs` / `marshal_one` were
//! previously copy-pasted across three modules; they live here now, consumed by
//! both the JS and C bindings.

use base64::Engine as _;
use serde_json::json;

use cognee_lib::models::DataInput;

use crate::SdkError;

// ---------------------------------------------------------------------------
// Shared result helpers.
// ---------------------------------------------------------------------------

/// Hand-build the `CognifyResult` JSON from its `.len()` counts.
///
/// `CognifyResult` is NOT `Serialize` (it carries non-serialisable internal
/// fields), so its JSON is hand-built here from the same `.len()` counts the
/// CLI prints. Shared between all binding surfaces.
pub fn cognify_result_json(result: &cognee_lib::cognify::CognifyResult) -> serde_json::Value {
    json!({
        "chunks": result.chunks.len(),
        "entities": result.entities.len(),
        "edges": result.edges.len(),
        "summaries": result.summaries.len(),
        "embeddings": result.embeddings.len(),
        "alreadyCompleted": result.already_completed,
        "priorPipelineRunId": result.prior_pipeline_run_id.map(|id| id.to_string()),
    })
}

// ---------------------------------------------------------------------------
// DataInput marshalling helpers.
// ---------------------------------------------------------------------------

/// Marshal a single `{ type, … }` JSON object into a [`DataInput`].
///
/// Handles: `text`, `file`, `url`, `binary` (base64 string, plain byte array,
/// or Node `Buffer` JSON projection `{ type:"Buffer", data:[..] }`), `s3`
/// (unsupported stub), `dataItem` (out of scope). Any other `type` returns a
/// `Validation` error.
pub fn marshal_one(value: &serde_json::Value) -> Result<DataInput, SdkError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SdkError::Validation("each data input must be an object".to_string()))?;
    let ty = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SdkError::Validation("data input is missing a string `type`".to_string()))?;

    match ty {
        "text" => {
            let text = obj.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("text input requires a `text` string".into())
            })?;
            Ok(DataInput::Text(text.to_string()))
        }
        "file" => {
            let path = obj.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("file input requires a `path` string".into())
            })?;
            Ok(DataInput::FilePath(path.to_string()))
        }
        "url" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SdkError::Validation("url input requires a `url` string".into()))?;
            Ok(DataInput::Url(url.to_string()))
        }
        "binary" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    SdkError::Validation(
                        "binary input requires a `name` string (used for MIME detection)".into(),
                    )
                })?
                .to_string();
            let data = marshal_bytes(obj.get("bytes"))?;
            Ok(DataInput::Binary { data, name })
        }
        "s3" => Err(SdkError::Unsupported(
            "s3 inputs are not yet supported (DataInput::S3Path is a stub)".into(),
        )),
        "dataItem" => Err(SdkError::Unsupported(
            "the recursive `dataItem` input variant is out of scope for v1".into(),
        )),
        other => Err(SdkError::Validation(format!(
            "unknown data input type `{other}`"
        ))),
    }
}

/// Decode `bytes` for a binary input: a base64 string, a plain JSON array of
/// byte values, or a Node `Buffer`/`Uint8Array` JSON projection
/// (`{ type: "Buffer", data: [..] }`).
pub fn marshal_bytes(bytes: Option<&serde_json::Value>) -> Result<Vec<u8>, SdkError> {
    let bytes =
        bytes.ok_or_else(|| SdkError::Validation("binary input requires `bytes`".to_string()))?;

    match bytes {
        serde_json::Value::String(s) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|e| SdkError::Validation(format!("invalid base64 `bytes`: {e}"))),
        serde_json::Value::Array(arr) => decode_byte_array(arr),
        serde_json::Value::Object(obj) => {
            // Node Buffer/Uint8Array stringifies to { type: "Buffer", data: [..] }.
            let data = obj.get("data").and_then(|v| v.as_array()).ok_or_else(|| {
                SdkError::Validation(
                    "binary `bytes` object must carry a numeric `data` array".to_string(),
                )
            })?;
            decode_byte_array(data)
        }
        _ => Err(SdkError::Validation(
            "binary `bytes` must be a base64 string, a byte array, or a Buffer".to_string(),
        )),
    }
}

/// Convert a JSON array of integers in `0..=255` into `Vec<u8>`.
fn decode_byte_array(arr: &[serde_json::Value]) -> Result<Vec<u8>, SdkError> {
    arr.iter()
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .ok_or_else(|| {
                    SdkError::Validation("binary `bytes` array must contain bytes 0..=255".into())
                })
        })
        .collect()
}

/// Marshal the `dataInput` argument — a single item **or** an array of items —
/// into `Vec<DataInput>`.
pub fn marshal_inputs(value: &serde_json::Value) -> Result<Vec<DataInput>, SdkError> {
    match value {
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return Err(SdkError::Validation(
                    "dataInput array must not be empty".to_string(),
                ));
            }
            items.iter().map(marshal_one).collect()
        }
        other => marshal_one(other).map(|input| vec![input]),
    }
}
