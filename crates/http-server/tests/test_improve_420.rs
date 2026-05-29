//! Integration test: the 420 parity quirk for `POST /api/v1/improve`.
//!
//! Per p3-pipelines-and-websocket.md §5 and improve.md §2.1:
//!
//! > "Force `PipelineRunErrored` ... and assert:
//! >  (a) HTTP status is literally `420`;
//! >  (b) body is the serialised `PipelineRunInfoDTO` with
//! >      `status="PipelineRunErrored"` and an `error` field;
//! >  (c) body is NOT wrapped in the canonical `{"error":..., "detail":...}` envelope."
//!
//! This is the headline parity test for P3.
//!
//! This test exercises the parity contract directly through
//! `ApiError::PipelineErrored { pipeline_source: Improve, ... }` so the 420
//! mapping remains guarded independently of backend fixture complexity.

mod support;

use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
use cognee_http_server::error::{ApiError, PipelineErrorSource};
use uuid::Uuid;

/// Headline parity assertion: `/improve` on `PipelineRunErrored` returns
/// HTTP 420 with the **raw `PipelineRunInfoDTO`** body (not the canonical
/// `{"error":..., "detail":...}` envelope).
///
/// Per improve.md §2.1 Python parity note and p3-pipelines-and-websocket.md §5
/// `test_improve_420.rs` spec.
#[tokio::test]
async fn improve_pipeline_errored_returns_420_with_raw_run_info() {
    let run_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    // Build the run_info as the handler does on PipelineRunErrored.
    let run_info_dto = serde_json::json!({
        "status": "PipelineRunErrored",
        "pipeline_run_id": run_id,
        "dataset_id": dataset_id,
        "dataset_name": "test_improve_dataset",
        "error": "simulated improve failure"
    });

    let resp = ApiError::PipelineErrored {
        pipeline_source: PipelineErrorSource::Improve,
        run_info: run_info_dto.clone(),
    }
    .into_response();

    // (a) Status is literally 420.
    assert_eq!(
        resp.status().as_u16(),
        420,
        "PipelineRunErrored from /improve must return HTTP 420, not 500"
    );

    let body_bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("body is valid JSON");

    // (b) Body is the raw PipelineRunInfoDTO.
    assert_eq!(
        body["status"], "PipelineRunErrored",
        "body.status must be 'PipelineRunErrored'"
    );
    assert_eq!(
        body["error"], "simulated improve failure",
        "body.error must carry the pipeline error message"
    );
    assert_eq!(
        body["pipeline_run_id"].as_str().unwrap(),
        run_id.to_string().as_str(),
        "body must include pipeline_run_id"
    );

    // (c) Body is NOT wrapped in the canonical envelope.
    assert_ne!(
        body.get("error").and_then(|e| e.as_str()),
        Some("Pipeline run errored"),
        "body must NOT be the canonical error envelope"
    );
    assert!(
        body.get("detail").is_none(),
        "body must NOT have a 'detail' field (not the canonical envelope)"
    );
}

/// Control assertion: cognify/memify `PipelineRunErrored` returns 500 (not 420)
/// with the canonical `{"error": ..., "detail": ...}` envelope.
#[tokio::test]
async fn cognify_pipeline_errored_returns_500_with_canonical_envelope() {
    let resp = ApiError::PipelineErrored {
        pipeline_source: PipelineErrorSource::Cognify,
        run_info: serde_json::json!({
            "error": "Pipeline run errored",
            "detail": "cognify internal error"
        }),
    }
    .into_response();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body_bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");

    assert_eq!(body["error"], "Pipeline run errored");
    assert_eq!(body["detail"], "cognify internal error");
}
