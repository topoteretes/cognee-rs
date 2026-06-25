#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
use std::sync::Arc;

use cognee_cognify::create_web_page_nodes;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::{DataPoint, Document, DocumentChunk};
use serde_json::json;
use uuid::Uuid;

fn web_page_id(url: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("WebPage:{url}").as_bytes()).to_string()
}

fn web_site_id(domain: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("WebSite:{domain}").as_bytes()).to_string()
}

fn document(id: Uuid, metadata: serde_json::Value) -> Document {
    let mut base = DataPoint::new("TextDocument", None);
    base.id = id;
    Document {
        base,
        document_type: "text".to_string(),
        name: "page.txt".to_string(),
        raw_data_location: "file:///tmp/page.txt".to_string(),
        mime_type: "text/plain".to_string(),
        extension: "txt".to_string(),
        data_id: id,
        external_metadata: Some(metadata.to_string()),
    }
}

#[tokio::test]
async fn url_provenance_creates_page_site_and_chunk_edges_offline() {
    let graph = Arc::new(MockGraphDB::new());
    let doc_id = Uuid::parse_str("00000000-0000-0000-0000-00000000f001").unwrap();
    let chunk_id = Uuid::parse_str("00000000-0000-0000-0000-00000000c001").unwrap();
    let final_url = "https://example.test/docs/page";
    let documents = vec![document(
        doc_id,
        json!({
            "source": "url",
            "url": "https://example.test/start",
            "final_url": final_url,
            "content_type": "text/html",
            "title": "Offline fixture",
        }),
    )];
    let chunks = vec![DocumentChunk::new(
        chunk_id,
        "Offline URL fixture content".to_string(),
        4,
        0,
        "paragraph_end".to_string(),
        doc_id,
    )];

    create_web_page_nodes(&documents, &chunks, graph.clone())
        .await
        .unwrap();

    let page_id = web_page_id(final_url);
    let site_id = web_site_id("example.test");
    let page = graph.get_node(&page_id).await.unwrap().unwrap();
    assert_eq!(page.get("type").and_then(|v| v.as_str()), Some("WebPage"));
    assert_eq!(page.get("url").and_then(|v| v.as_str()), Some(final_url));

    let site = graph.get_node(&site_id).await.unwrap().unwrap();
    assert_eq!(site.get("type").and_then(|v| v.as_str()), Some("WebSite"));
    assert_eq!(
        site.get("domain").and_then(|v| v.as_str()),
        Some("example.test")
    );

    let (_, edges) = graph.get_graph_data().await.unwrap();
    assert!(edges.iter().any(|(source, target, relationship, _)| {
        source == &page_id && target == &site_id && relationship == "PART_OF"
    }));
    assert!(edges.iter().any(|(source, target, relationship, _)| {
        source == &chunk_id.to_string() && target == &page_id && relationship == "SOURCED_FROM"
    }));
}
