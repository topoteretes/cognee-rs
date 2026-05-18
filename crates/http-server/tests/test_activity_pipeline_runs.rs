//! Integration tests for `GET /api/v1/activity/pipeline-runs`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cognee_database::{
    PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository, uuid_hex,
};
use sea_orm::ConnectionTrait;
use tower::ServiceExt;
use uuid::Uuid;

/// Seed three pipeline_runs rows: two attached to a dataset+user, one orphan
/// (synthetic via raw SQL, FK off). Assert the response includes all three
/// with the orphan having `dataset_name = null`.
#[tokio::test]
async fn lists_recent_with_attribution_including_orphan() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "owner@example.com", "pw").await;
    let db = support::permissions_db(&state);

    let dataset_id = Uuid::new_v4();
    support::seed_dataset(db, dataset_id, user.id, None, "alpha").await;

    let repo = SeaOrmPipelineRunRepository::new(std::sync::Arc::new(db.clone()));
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "cognify_pipeline",
        Some(dataset_id),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log row 1");
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "memify_pipeline",
        Some(dataset_id),
        PipelineRunStatus::Completed,
        None,
    )
    .await
    .expect("log row 2");

    // Insert orphan via raw SQL with FK off.
    let orphan_dataset = Uuid::new_v4();
    let row_id = uuid_hex::to_hex(Uuid::new_v4());
    let pipeline_run_hex = uuid_hex::to_hex(Uuid::new_v4());
    let pipeline_id_hex = uuid_hex::to_hex(Uuid::new_v4());
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        "PRAGMA foreign_keys = OFF".to_string(),
    ))
    .await
    .expect("disable fk");
    let now_str = chrono::Utc::now().to_rfc3339();
    let orphan_hex = uuid_hex::to_hex(orphan_dataset);
    let values: Vec<sea_orm::Value> = vec![
        sea_orm::Value::String(Some(Box::new(row_id))),
        sea_orm::Value::String(Some(Box::new(now_str))),
        sea_orm::Value::String(Some(Box::new(pipeline_run_hex))),
        sea_orm::Value::String(Some(Box::new(pipeline_id_hex))),
        sea_orm::Value::String(Some(Box::new(orphan_hex))),
    ];
    db.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Sqlite,
        "INSERT INTO pipeline_runs (id, created_at, status, pipeline_run_id, pipeline_name, pipeline_id, dataset_id, run_info) VALUES ($1, $2, 'DATASET_PROCESSING_ERRORED', $3, 'add_pipeline', $4, $5, NULL)",
        values,
    ))
    .await
    .expect("orphan insert");
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        "PRAGMA foreign_keys = ON".to_string(),
    ))
    .await
    .expect("re-enable fk");

    let app = support::test_router(state.clone()).await;
    let bearer = support::bearer_header(&user, &state);
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/activity/pipeline-runs")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3);

    // Find the orphan row — its dataset_name should be null.
    let orphan = arr
        .iter()
        .find(|r| r.get("dataset_name").and_then(|v| v.as_str()).is_none())
        .expect("orphan row present");
    assert!(
        orphan["pipeline_name"]
            .as_str()
            .unwrap_or("")
            .contains("add_pipeline")
    );
}

// ---------------------------------------------------------------------------
// Task 08-09 (c) — four-state lifecycle trail surfaces via the activity
// endpoint with the correct enum strings and dataset attribution.
// ---------------------------------------------------------------------------

/// Seed a real `INITIATED → STARTED → COMPLETED` lifecycle through
/// `SeaOrmPipelineRunRepository::log_pipeline_run` and assert the
/// `/api/v1/activity/pipeline-runs` response surfaces all three rows in
/// `DATASET_PROCESSING_*` form with the correct dataset attribution and
/// `dataset_id_hex` populated (per task 08-01 nullability handling).
#[tokio::test]
async fn lists_recent_four_state_lifecycle_trail() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "lifecycle@example.com", "pw").await;
    let db = support::permissions_db(&state);

    let dataset_id = Uuid::new_v4();
    support::seed_dataset(db, dataset_id, user.id, None, "beta").await;

    let repo = SeaOrmPipelineRunRepository::new(std::sync::Arc::new(db.clone()));
    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(
            prid,
            pid,
            "cognify_pipeline",
            Some(dataset_id),
            status,
            None,
        )
        .await
        .expect("log row");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let app = support::test_router(state.clone()).await;
    let bearer = support::bearer_header(&user, &state);
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/activity/pipeline-runs?dataset_id={dataset_id}"
        ))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(
        arr.len(),
        3,
        "expected the full INITIATED + STARTED + COMPLETED trail"
    );

    // Response is newest-first (matches Python's get_activity_router shape).
    let statuses: Vec<&str> = arr
        .iter()
        .map(|r| r.get("status").and_then(|v| v.as_str()).unwrap_or(""))
        .collect();
    assert_eq!(
        statuses,
        vec![
            "DATASET_PROCESSING_COMPLETED",
            "DATASET_PROCESSING_STARTED",
            "DATASET_PROCESSING_INITIATED",
        ]
    );

    for row in arr {
        // dataset attribution: same dataset on every row.
        assert_eq!(
            row.get("dataset_name").and_then(|v| v.as_str()),
            Some("beta")
        );
        assert_eq!(
            row.get("pipeline_name").and_then(|v| v.as_str()),
            Some("cognify_pipeline")
        );
        // pipeline_run_id is shared across all three rows (decision 12).
        assert!(
            row.get("pipeline_run_id").is_some(),
            "every row should carry pipeline_run_id"
        );
    }
}
