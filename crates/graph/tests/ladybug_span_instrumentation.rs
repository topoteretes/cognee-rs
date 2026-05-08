//! Span attribute integration tests for the Ladybug adapter.
#![cfg(feature = "ladybug")]

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_test_utils::SpanCapture;
use serial_test::serial;
use tempfile::TempDir;

async fn make_adapter() -> (LadybugAdapter, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("graph.lbug");
    let adapter = LadybugAdapter::new(db_path.to_str().expect("utf-8 path"))
        .await
        .expect("open ladybug");
    adapter.initialize().await.expect("initialize");
    (adapter, dir)
}

#[tokio::test]
#[serial]
async fn query_emits_cognee_db_graph_query_span() {
    let capture = SpanCapture::install();
    let (adapter, _dir) = make_adapter().await;
    // A query that returns zero rows is the cleanest assertion.
    adapter
        .query("MATCH (n:Node) WHERE n.id = 'no-such-id' RETURN n", None)
        .await
        .expect("query");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(0));
    let recorded_query = s.field_str("cognee.db.query").expect("query attr present");
    assert!(
        recorded_query.contains("MATCH (n:Node)"),
        "expected query text in span, got {recorded_query}"
    );
}

#[tokio::test]
#[serial]
async fn query_redacts_secret_in_recorded_attribute() {
    let capture = SpanCapture::install();
    let (adapter, _dir) = make_adapter().await;

    // OpenAI-style key embedded in a Cypher literal. The query is
    // intentionally invalid; we just need the span to fire on the
    // path before the engine errors.
    let q = "MATCH (n) WHERE n.token = 'sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12345' RETURN n";
    let _ = adapter.query(q, None).await; // ignore result

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    let recorded = s.field_str("cognee.db.query").expect("query attr present");
    assert!(
        recorded.contains("***REDACTED***"),
        "expected redaction marker in: {recorded}"
    );
    assert!(
        !recorded.contains("DEFGHIJKLMNOP"),
        "secret leaked through redaction: {recorded}"
    );
}

#[tokio::test]
#[serial]
async fn long_query_truncated_to_500_chars_before_redaction() {
    let capture = SpanCapture::install();
    let (adapter, _dir) = make_adapter().await;
    let long_query = format!("MATCH (n) WHERE n.x = '{}' RETURN n", "a".repeat(800));
    let _ = adapter.query(&long_query, None).await;

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    let recorded = s.field_str("cognee.db.query").expect("query attr present");
    // Truncation is byte-len <= 500. (Char-boundary walk may shave a few.)
    assert!(
        recorded.len() <= 500,
        "recorded len {} exceeded 500",
        recorded.len()
    );
}
