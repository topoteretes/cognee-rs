#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! SDK-501 follow-on — `GET /api/v1/activity/pipeline-runs` surfaces the failure summary
//! a tolerantly-completed cognify run persisted.
//!
//! `cognee_cognify::rollback::run_info_with_failures` writes
//! `run_info = {"data": [...], "cognify_failures": {…}}` on the `COMPLETED` row
//! of a run that finished with documents outstanding. Nothing outside
//! `crates/cognify` could read it: the activity listing dropped `run_info` on
//! the floor. These cases pin both directions of the new `cognify_failures`
//! field — present with the real numbers when the row carries the key, and
//! **absent from the JSON entirely** when it does not, which is what keeps the
//! response byte-identical to the Python-mirrored shape for every run Python
//! could also produce.

mod support;

use cognee_database::{PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository};
use cognee_http_server::build_router;
use support::{body_json, build_p4_state, oneshot_get};
use uuid::Uuid;

const FAILED_A: &str = "11111111-1111-4111-8111-111111111111";
const FAILED_B: &str = "22222222-2222-4222-8222-222222222222";
const UNREACHED: &str = "33333333-3333-4333-8333-333333333333";

/// The exact payload `run_info_with_failures` produces for a run that failed
/// two documents and never reached a third.
fn failure_run_info() -> serde_json::Value {
    serde_json::json!({
        "data": [],
        "cognify_failures": {
            "failed_data_ids": [FAILED_A, FAILED_B],
            "unreached_data_ids": [UNREACHED],
            "failure_count": 5,
            "chunk_failure_ratio": 0.25,
        }
    })
}

/// Seed one `pipeline_runs` row and return the assembled router.
async fn app_with_run(
    dataset_id: Uuid,
    run_info: Option<serde_json::Value>,
) -> (axum::Router, Uuid) {
    let state = build_p4_state(None, None, None).await;
    let db = state
        .components()
        .expect("components wired")
        .database
        .clone();

    let repo = SeaOrmPipelineRunRepository::new(db);
    let pipeline_run_id = Uuid::new_v4();
    repo.log_pipeline_run(
        pipeline_run_id,
        Uuid::new_v4(),
        "cognify_pipeline",
        Some(dataset_id),
        PipelineRunStatus::Completed,
        run_info,
    )
    .await
    .expect("seed pipeline run");

    let app = build_router(state).await.expect("router");
    (app, pipeline_run_id)
}

#[tokio::test]
async fn pipeline_runs_expose_the_persisted_cognify_failure_payload() {
    let dataset_id = Uuid::new_v4();
    let (app, pipeline_run_id) = app_with_run(dataset_id, Some(failure_run_info())).await;

    let resp = oneshot_get(
        app,
        &format!("/api/v1/activity/pipeline-runs?dataset_id={dataset_id}"),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let rows = body.as_array().expect("a JSON array of runs");
    let row = rows
        .iter()
        .find(|r| r["pipeline_run_id"] == serde_json::json!(pipeline_run_id.to_string()))
        .unwrap_or_else(|| panic!("seeded run must be listed; got {body}"));

    let failures = row
        .get("cognify_failures")
        .unwrap_or_else(|| panic!("`cognify_failures` must be present on this row; got {row}"));

    // Specific values from the seeded payload — not merely "the key exists".
    assert_eq!(
        failures["failed_data_ids"],
        serde_json::json!([FAILED_A, FAILED_B]),
        "failed ids must survive verbatim: {failures}"
    );
    assert_eq!(
        failures["unreached_data_ids"],
        serde_json::json!([UNREACHED]),
        "unreached ids must survive verbatim: {failures}"
    );
    assert_eq!(failures["failure_count"], serde_json::json!(5));
    assert_eq!(failures["chunk_failure_ratio"], serde_json::json!(0.25));
}

#[tokio::test]
async fn a_clean_run_row_omits_the_cognify_failures_key_entirely() {
    let dataset_id = Uuid::new_v4();
    // What a clean run writes: `run_info` with only the `data` key the Python
    // shape requires. `run_info_with_failures` is never called for such a run.
    let clean = serde_json::json!({"data": []});
    let (app, pipeline_run_id) = app_with_run(dataset_id, Some(clean)).await;

    let resp = oneshot_get(
        app,
        &format!("/api/v1/activity/pipeline-runs?dataset_id={dataset_id}"),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let row = body
        .as_array()
        .expect("array")
        .iter()
        .find(|r| r["pipeline_run_id"] == serde_json::json!(pipeline_run_id.to_string()))
        .unwrap_or_else(|| panic!("seeded run must be listed; got {body}"))
        .clone();

    assert!(
        row.get("cognify_failures").is_none(),
        "a clean run's row must not carry the key at all (Python-mirrored shape): {row}"
    );
    // The distinctive string must be absent from the serialized row, not just
    // null-valued — `skip_serializing_if` is the whole contract here.
    let serialized = row.to_string();
    assert!(
        !serialized.contains("cognify_failures"),
        "clean row leaked the key into the wire shape: {serialized}"
    );
    // The row is otherwise a normal listing entry.
    assert_eq!(row["pipeline_name"], serde_json::json!("cognify_pipeline"));
}

/// `run_info` is untyped JSON owned by whoever wrote the row, so the route
/// must treat an unreadable `cognify_failures` payload as absent rather than
/// as an error. Two shapes that a stricter reader would blow up on: the key
/// explicitly `null`, and an object whose fields have the wrong types.
#[tokio::test]
async fn a_malformed_cognify_failures_payload_is_omitted_not_fatal() {
    for malformed in [
        serde_json::json!({"data": [], "cognify_failures": null}),
        serde_json::json!({"data": [], "cognify_failures": "not an object"}),
        serde_json::json!({"data": [], "cognify_failures": {
            "failed_data_ids": "not-a-uuid-list",
            "unreached_data_ids": [],
            "failure_count": -1,
            "chunk_failure_ratio": "NaN",
        }}),
    ] {
        let dataset_id = Uuid::new_v4();
        let (app, pipeline_run_id) = app_with_run(dataset_id, Some(malformed.clone())).await;

        let resp = oneshot_get(
            app,
            &format!("/api/v1/activity/pipeline-runs?dataset_id={dataset_id}"),
        )
        .await;
        // Not a 500: one unreadable historical row must not take down the
        // listing.
        assert_eq!(resp.status(), 200, "malformed payload {malformed} 500'd");

        let body = body_json(resp).await;
        let row = body
            .as_array()
            .expect("array")
            .iter()
            .find(|r| r["pipeline_run_id"] == serde_json::json!(pipeline_run_id.to_string()))
            .unwrap_or_else(|| panic!("seeded run must still be listed; got {body}"))
            .clone();
        assert!(
            !row.to_string().contains("cognify_failures"),
            "an unreadable payload must be dropped, not forwarded: {row}"
        );
        // The row is otherwise intact.
        assert_eq!(row["pipeline_name"], serde_json::json!("cognify_pipeline"));
    }
}

#[tokio::test]
async fn a_row_with_null_run_info_omits_the_key() {
    let dataset_id = Uuid::new_v4();
    let (app, _) = app_with_run(dataset_id, None).await;

    let resp = oneshot_get(
        app,
        &format!("/api/v1/activity/pipeline-runs?dataset_id={dataset_id}"),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let serialized = body_json(resp).await.to_string();
    assert!(
        !serialized.contains("cognify_failures"),
        "a NULL run_info must not synthesize the key: {serialized}"
    );
}
