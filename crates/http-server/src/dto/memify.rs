//! DTOs for `POST /api/v1/memify`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/memify`.
///
/// Mirrors Python's `MemifyPayloadDTO` (an `InDTO`). Wire is camelCase per
/// Decision 10; snake_case is accepted as input via per-field aliases.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemifyPayloadDTO {
    /// Dataset name. Either `dataset_id` or `dataset_name` is required.
    #[serde(default, alias = "dataset_name")]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Empty string is treated as absent.
    /// Either `dataset_id` or `dataset_name` is required.
    #[serde(default, alias = "dataset_id")]
    pub dataset_id: super::util::DatasetIdRef,

    /// When `true`, dispatch to the background and return immediately.
    #[serde(default, alias = "run_in_background")]
    pub run_in_background: Option<bool>,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn memify_dto_accepts_camelcase_input() {
        let json = r#"{
            "datasetName": "ds",
            "datasetId": "00000000-0000-0000-0000-000000000001",
            "runInBackground": true
        }"#;
        let parsed: MemifyPayloadDTO = serde_json::from_str(json).expect("parse camelCase");
        assert_eq!(parsed.dataset_name.as_deref(), Some("ds"));
        assert!(parsed.dataset_id.as_option().is_some());
        assert_eq!(parsed.run_in_background, Some(true));
    }

    #[test]
    fn memify_dto_accepts_snake_case_input_via_alias() {
        let json = r#"{
            "dataset_name": "ds",
            "dataset_id": "00000000-0000-0000-0000-000000000001",
            "run_in_background": true
        }"#;
        let parsed: MemifyPayloadDTO = serde_json::from_str(json).expect("parse snake_case");
        assert_eq!(parsed.dataset_name.as_deref(), Some("ds"));
        assert!(parsed.dataset_id.as_option().is_some());
        assert_eq!(parsed.run_in_background, Some(true));
    }

    #[test]
    fn memify_dto_serializes_camelcase_only() {
        let dto = MemifyPayloadDTO {
            dataset_name: Some("ds".into()),
            dataset_id: super::super::util::DatasetIdRef(Some(Uuid::nil())),
            run_in_background: Some(false),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"datasetName\""), "missing datasetName: {s}");
        assert!(s.contains("\"datasetId\""), "missing datasetId: {s}");
        assert!(
            s.contains("\"runInBackground\""),
            "missing runInBackground: {s}"
        );
        for forbidden in [
            "\"dataset_name\"",
            "\"dataset_id\"",
            "\"run_in_background\"",
        ] {
            assert!(
                !s.contains(forbidden),
                "snake_case key {forbidden} leaked: {s}"
            );
        }
    }
}
