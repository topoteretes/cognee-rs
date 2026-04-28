//! Integration tests for `GET /api/v1/activity/export/{dataset_id}`.
//!
//! `ComponentHandles::formatted_graph_data` is a stub returning an empty
//! graph (`{"nodes": [], "edges": []}`) until the underlying Python port
//! lands; these tests exercise the wire-level contract that does not depend
//! on graph contents — Markdown header, document section, Content-Type,
//! Content-Disposition, the 404 plain-text body — per the task doc note in
//! `p6-observability.md` step 12.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cognee_database::IngestDb;
use cognee_models::Dataset;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn export_unknown_dataset_returns_404_plain_text() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "exporter@example.com", "pw").await;
    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;

    let bogus = Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/activity/export/{bogus}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("text/plain"),
        "404 must be text/plain; got {ct}"
    );
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = std::str::from_utf8(&body_bytes).unwrap_or("");
    assert_eq!(body, "Dataset not found");
}

#[tokio::test]
async fn export_existing_dataset_returns_markdown_with_headers() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "exporter2@example.com", "pw").await;

    // Insert a dataset directly via IngestDb so the handler's lookup hits.
    let db = state.components().expect("components").database.clone();
    let dataset_id = Uuid::new_v4();
    let ds = Dataset::new("alpha-export".to_string(), user.id, None, dataset_id);
    db.create_dataset(ds).await.expect("create dataset");

    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/activity/export/{dataset_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    // Content-Type must be markdown.
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(ct, "text/markdown; charset=utf-8");

    // Content-Disposition: attachment; filename="<name>-memory-export.md"
    let cd = resp
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        cd.contains("attachment"),
        "Content-Disposition missing attachment: {cd}"
    );
    assert!(
        cd.contains("alpha-export-memory-export.md"),
        "Content-Disposition filename wrong: {cd}"
    );

    // Body shape: starts with the dataset header, contains the "Exported:"
    // metadata line. Graph sections are gated on emptiness — with the stub
    // graph the body holds only header + meta.
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = std::str::from_utf8(&body_bytes).unwrap_or("");
    assert!(
        body.starts_with("# Dataset: alpha-export"),
        "body must start with header, got: {}",
        body.chars().take(80).collect::<String>()
    );
    assert!(
        body.contains("Exported:"),
        "body must contain Exported metadata"
    );
    // Empty graph → no `## Entities` / `## Relationships` headers.
    assert!(!body.contains("## Entities"));
    assert!(!body.contains("## Relationships"));
}
