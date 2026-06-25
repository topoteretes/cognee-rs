//! DTOs for `POST /api/v1/cognify` and `GET /api/v1/cognify/subscribe/{id}`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/cognify`.
///
/// Mirrors Python's `CognifyPayloadDTO` (an `InDTO`, so the wire defaults to
/// camelCase per Decision 10). Snake_case input is accepted as a fallback via
/// per-field aliases for compatibility with `populate_by_name=True` clients.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CognifyPayloadDTO {
    /// Dataset names owned by the authenticated user.
    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    /// Dataset UUIDs. When set, overrides `datasets` (Python parity:
    /// `dataset_ids if payload.dataset_ids else payload.datasets`).
    #[serde(default, alias = "dataset_ids")]
    pub dataset_ids: Option<Vec<Uuid>>,

    /// When `true`, dispatch to the background and return `PipelineRunStarted`
    /// immediately. When `false` (default), await the run to completion.
    #[serde(default, alias = "run_in_background")]
    pub run_in_background: Option<bool>,

    /// JSON Schema describing a custom Pydantic-shaped graph model.
    #[serde(default, alias = "graph_model")]
    pub graph_model: Option<serde_json::Value>,

    /// Replaces the default graph-extraction prompt for this run.
    #[serde(default, alias = "custom_prompt")]
    pub custom_prompt: Option<String>,

    /// One or more ontology keys from `POST /api/v1/ontologies/upload`.
    #[serde(default, alias = "ontology_key")]
    pub ontology_key: Option<Vec<String>>,

    /// Overrides `CognifyConfig::chunks_per_batch` for this run.
    #[serde(default, alias = "chunks_per_batch")]
    pub chunks_per_batch: Option<u32>,
}

// ─── Response aliases ─────────────────────────────────────────────────────────

/// Response body for `POST /api/v1/cognify` — Python returns the dict directly.
///
/// Key: stringified dataset UUID.
/// Value: terminal `PipelineRunInfoDTO` for that dataset.
pub type CognifyResponseDTO = std::collections::HashMap<String, PipelineRunInfoDTO>;

// ─── WebSocket frame ──────────────────────────────────────────────────────────

/// Single frame sent over the WebSocket at
/// `GET /api/v1/cognify/subscribe/{pipeline_run_id}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CognifyWsFrameDTO {
    pub pipeline_run_id: Uuid,
    /// One of the `PipelineRun*` live-event strings.
    pub status: String,
    /// Formatted graph snapshot (`{"nodes": [...], "edges": [...]}`),
    /// or `{}` on error, or `[]` for background dispatches.
    pub payload: serde_json::Value,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn cognify_dto_accepts_camelcase_input() {
        let json = r#"{
            "datasetIds": ["00000000-0000-0000-0000-000000000001"],
            "runInBackground": true,
            "graphModel": {"x": 1},
            "customPrompt": "go",
            "ontologyKey": ["k"],
            "chunksPerBatch": 4
        }"#;
        let parsed: CognifyPayloadDTO = serde_json::from_str(json).expect("parse camelCase");
        assert_eq!(parsed.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(parsed.run_in_background, Some(true));
        assert!(parsed.graph_model.is_some());
        assert_eq!(parsed.custom_prompt.as_deref(), Some("go"));
        assert_eq!(parsed.ontology_key.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(parsed.chunks_per_batch, Some(4));
    }

    #[test]
    fn cognify_dto_accepts_snake_case_input_via_alias() {
        let json = r#"{
            "dataset_ids": ["00000000-0000-0000-0000-000000000001"],
            "run_in_background": false,
            "graph_model": {"x": 1},
            "custom_prompt": "go",
            "ontology_key": ["k"],
            "chunks_per_batch": 4
        }"#;
        let parsed: CognifyPayloadDTO = serde_json::from_str(json).expect("parse snake_case");
        assert_eq!(parsed.dataset_ids.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(parsed.run_in_background, Some(false));
        assert!(parsed.graph_model.is_some());
        assert_eq!(parsed.custom_prompt.as_deref(), Some("go"));
        assert_eq!(parsed.ontology_key.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(parsed.chunks_per_batch, Some(4));
    }

    #[test]
    fn cognify_dto_serializes_camelcase_only() {
        let dto = CognifyPayloadDTO {
            datasets: Some(vec!["a".into()]),
            dataset_ids: Some(vec![Uuid::nil()]),
            run_in_background: Some(true),
            graph_model: Some(serde_json::json!({})),
            custom_prompt: Some("p".into()),
            ontology_key: Some(vec!["k".into()]),
            chunks_per_batch: Some(2),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"datasetIds\""), "missing datasetIds: {s}");
        assert!(
            s.contains("\"runInBackground\""),
            "missing runInBackground: {s}"
        );
        assert!(s.contains("\"graphModel\""), "missing graphModel: {s}");
        assert!(s.contains("\"customPrompt\""), "missing customPrompt: {s}");
        assert!(s.contains("\"ontologyKey\""), "missing ontologyKey: {s}");
        assert!(
            s.contains("\"chunksPerBatch\""),
            "missing chunksPerBatch: {s}"
        );
        // Negative — no underscores in property names.
        for forbidden in [
            "\"dataset_ids\"",
            "\"run_in_background\"",
            "\"graph_model\"",
            "\"custom_prompt\"",
            "\"ontology_key\"",
            "\"chunks_per_batch\"",
        ] {
            assert!(
                !s.contains(forbidden),
                "snake_case key {forbidden} leaked into output: {s}"
            );
        }
    }
}
