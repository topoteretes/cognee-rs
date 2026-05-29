//! DTOs for `POST /api/v1/improve`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/improve`.
///
/// Mirrors Python's `ImprovePayloadDTO` (an `InDTO`). Wire is camelCase per
/// Decision 10; snake_case is accepted as input via per-field aliases.
///
/// The v2 additions — `extraction_tasks`, `enrichment_tasks`, `data`,
/// `node_name`, `session_ids` — match Python
/// `cognee/api/v1/improve/routers/get_improve_router.py:21-37` field-for-field.
/// The HTTP handler now wires these fields into the real improve-stage
/// execution path (feedback weights, session persistence, memify, and optional
/// graph-to-session sync).
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImprovePayloadDTO {
    /// Optional list of extraction-task identifiers (informational; reserved
    /// for future power-user overrides).
    #[serde(default, alias = "extraction_tasks")]
    pub extraction_tasks: Option<Vec<String>>,

    /// Optional list of enrichment-task identifiers (informational; reserved
    /// for future power-user overrides).
    #[serde(default, alias = "enrichment_tasks")]
    pub enrichment_tasks: Option<Vec<String>>,

    /// Optional inline text payload (Python parity: `data: Optional[str]`).
    #[serde(default)]
    pub data: Option<String>,

    /// Dataset name. Either `dataset_id` or `dataset_name` is required.
    #[serde(default, alias = "dataset_name")]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Empty string is treated as absent.
    #[serde(default, alias = "dataset_id")]
    pub dataset_id: super::util::DatasetIdRef,

    /// Optional graph node-name filter (Python parity: `node_name: Optional[List[str]]`).
    #[serde(default, alias = "node_name")]
    pub node_name: Option<Vec<String>>,

    /// When `true`, dispatch to the background and return immediately.
    #[serde(default, alias = "run_in_background")]
    pub run_in_background: Option<bool>,

    /// Session IDs that, when present and non-empty, trigger the v2 four-stage
    /// session-bridge path (Stages 1, 2, 4 in `crates/cognify/src/memify/`).
    #[serde(default, alias = "session_ids")]
    pub session_ids: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn improve_dto_accepts_camelcase_input() {
        let json = r#"{
            "extractionTasks": ["t1"],
            "enrichmentTasks": ["e1"],
            "data": "hello",
            "datasetName": "ds",
            "datasetId": "00000000-0000-0000-0000-000000000001",
            "nodeName": ["n1", "n2"],
            "runInBackground": true,
            "sessionIds": ["s1", "s2"]
        }"#;
        let parsed: ImprovePayloadDTO = serde_json::from_str(json).expect("parse camelCase");
        assert_eq!(
            parsed.extraction_tasks.as_deref(),
            Some(&["t1".to_string()][..])
        );
        assert_eq!(
            parsed.enrichment_tasks.as_deref(),
            Some(&["e1".to_string()][..])
        );
        assert_eq!(parsed.data.as_deref(), Some("hello"));
        assert_eq!(parsed.dataset_name.as_deref(), Some("ds"));
        assert!(parsed.dataset_id.as_option().is_some());
        assert_eq!(
            parsed.node_name.as_deref(),
            Some(&["n1".to_string(), "n2".to_string()][..])
        );
        assert_eq!(parsed.run_in_background, Some(true));
        assert_eq!(
            parsed.session_ids.as_deref(),
            Some(&["s1".to_string(), "s2".to_string()][..])
        );
    }

    #[test]
    fn improve_dto_accepts_snake_case_input_via_alias() {
        let json = r#"{
            "extraction_tasks": ["t1"],
            "enrichment_tasks": ["e1"],
            "data": "hello",
            "dataset_name": "ds",
            "dataset_id": "00000000-0000-0000-0000-000000000001",
            "node_name": ["n1"],
            "run_in_background": true,
            "session_ids": ["s1"]
        }"#;
        let parsed: ImprovePayloadDTO = serde_json::from_str(json).expect("parse snake_case");
        assert_eq!(
            parsed.extraction_tasks.as_deref(),
            Some(&["t1".to_string()][..])
        );
        assert_eq!(
            parsed.enrichment_tasks.as_deref(),
            Some(&["e1".to_string()][..])
        );
        assert_eq!(parsed.data.as_deref(), Some("hello"));
        assert_eq!(parsed.dataset_name.as_deref(), Some("ds"));
        assert!(parsed.dataset_id.as_option().is_some());
        assert_eq!(parsed.node_name.as_deref(), Some(&["n1".to_string()][..]));
        assert_eq!(parsed.run_in_background, Some(true));
        assert_eq!(parsed.session_ids.as_deref(), Some(&["s1".to_string()][..]));
    }

    #[test]
    fn improve_dto_serializes_camelcase_only() {
        let dto = ImprovePayloadDTO {
            extraction_tasks: Some(vec!["t1".into()]),
            enrichment_tasks: Some(vec!["e1".into()]),
            data: Some("hello".into()),
            dataset_name: Some("ds".into()),
            dataset_id: super::super::util::DatasetIdRef(Some(Uuid::nil())),
            node_name: Some(vec!["n1".into()]),
            run_in_background: Some(false),
            session_ids: Some(vec!["s1".into()]),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        for required in [
            "\"extractionTasks\"",
            "\"enrichmentTasks\"",
            "\"data\"",
            "\"datasetName\"",
            "\"datasetId\"",
            "\"nodeName\"",
            "\"runInBackground\"",
            "\"sessionIds\"",
        ] {
            assert!(s.contains(required), "missing {required}: {s}");
        }
        for forbidden in [
            "\"extraction_tasks\"",
            "\"enrichment_tasks\"",
            "\"dataset_name\"",
            "\"dataset_id\"",
            "\"node_name\"",
            "\"run_in_background\"",
            "\"session_ids\"",
        ] {
            assert!(
                !s.contains(forbidden),
                "snake_case key {forbidden} leaked: {s}"
            );
        }
    }

    #[test]
    fn improve_dto_session_ids_only_round_trip() {
        // A minimal payload that exercises only the new session_ids field
        // (the headline addition for the v2 four-stage path).
        let json = r#"{ "sessionIds": ["s1"], "datasetName": "ds" }"#;
        let parsed: ImprovePayloadDTO = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed.session_ids.as_deref(), Some(&["s1".to_string()][..]));
        assert_eq!(parsed.dataset_name.as_deref(), Some("ds"));
        // Defaults for all other fields are None.
        assert!(parsed.extraction_tasks.is_none());
        assert!(parsed.enrichment_tasks.is_none());
        assert!(parsed.data.is_none());
        assert!(parsed.node_name.is_none());
        assert!(parsed.run_in_background.is_none());
    }
}
