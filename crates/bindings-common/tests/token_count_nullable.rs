#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! SDK-951: bindings report `token_count` as `null` until cognify counts it.
//!
//! The stored column keeps Python's `-1` sentinel (cross-SDK database parity);
//! only the JSON a binding returns maps it to `null`. Pinned end-to-end through
//! the shared ops every binding calls:
//!
//! - `add` returns `null` (cognify has not run);
//! - `listData` returns `null` before cognify;
//! - `addAndCognify` returns the count cognify stored, not the pre-cognify
//!   snapshot.
//!
//! No LLM is needed: the graph is built by a registered stub extractor that
//! returns an empty graph per chunk, summaries are off, and embeddings use the
//! mock provider.

use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;

use cognee::cognify::{
    ChunkGraphExtractor, ChunkGraphResult, ChunkRef, ExtractionContext, GraphBackendError,
    KnowledgeGraph,
};
use cognee::config::Settings;
use cognee_bindings_common::HandleState;
use cognee_bindings_common::graph_backend::{GraphBackendFuture, register_graph_backend};
use cognee_bindings_common::ops::{datasets, pipeline};

/// Kind name unique to this test binary: the registry is process-wide.
const KIND: &str = "sdk951-empty-graph";

/// Returns an empty graph for every chunk, so cognify needs no LLM.
struct EmptyGraphExtractor;

#[async_trait::async_trait]
impl ChunkGraphExtractor for EmptyGraphExtractor {
    fn name(&self) -> &str {
        KIND
    }

    async fn extract_graphs<'c, 'x>(
        &self,
        chunks: &[ChunkRef<'c>],
        _ctx: &ExtractionContext<'x>,
    ) -> Result<Vec<ChunkGraphResult>, GraphBackendError> {
        Ok(chunks
            .iter()
            .map(|_| {
                Ok(KnowledgeGraph {
                    nodes: Vec::new(),
                    edges: Vec::new(),
                })
            })
            .collect())
    }
}

fn empty_graph_factory(_spec: Value) -> GraphBackendFuture {
    Box::pin(async { Ok(Arc::new(EmptyGraphExtractor) as Arc<dyn ChunkGraphExtractor>) })
}

/// Hermetic settings: every store under a temp dir, a dummy key so the strict
/// LLM resolve succeeds without network I/O, and the mock embedding provider.
fn settings_under(dir: &TempDir) -> Settings {
    let root = dir.path();
    Settings {
        sections: [
            (
                "relational.url".to_string(),
                json!(format!(
                    "sqlite://{}?mode=rwc",
                    root.join("cognee.db").display()
                )),
            ),
            ("embedding.provider".to_string(), json!("mock")),
            ("llm.api_key".to_string(), json!("sk-test")),
        ]
        .into(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

fn text(s: &str) -> Value {
    json!({ "type": "text", "text": s })
}

/// The single row of `rows`, asserting there is exactly one.
fn only(rows: &Value) -> &Value {
    let rows = rows.as_array().expect("rows are an array");
    assert_eq!(rows.len(), 1, "expected one row, got {rows:?}");
    &rows[0]
}

#[tokio::test(flavor = "multi_thread")]
async fn token_count_is_null_until_cognify_counts_it() {
    register_graph_backend(KIND, empty_graph_factory).expect("first registration");

    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));

    // `add`: cognify has not run, so the count is `null`, not `-1`.
    let added = pipeline::add(
        &state,
        text("Alice met Bob in Paris to talk about graphs."),
        "sdk951",
        &Value::Null,
    )
    .await
    .expect("add");
    let row = only(&added["added"]);
    assert!(
        row["token_count"].is_null(),
        "add must report an uncomputed count as null, got {row}"
    );

    // `listData` before cognify: also `null`.
    let dataset_id = datasets::list_datasets(&state)
        .await
        .expect("list datasets")[0]["id"]
        .as_str()
        .expect("dataset id")
        .to_owned();
    let listed = datasets::list_data(&state, &dataset_id)
        .await
        .expect("list data");
    assert!(
        only(&listed)["token_count"].is_null(),
        "listData before cognify must report null, got {listed}"
    );

    // `addAndCognify`: cognify has run, so the returned row carries the count
    // it stored.
    let opts = json!({ "graphBackend": KIND, "summarization": false });
    let both = pipeline::add_and_cognify(
        &state,
        text("Carol wrote a paper on knowledge graphs in Berlin."),
        "sdk951-cognified",
        &opts,
    )
    .await
    .expect("add and cognify");
    let row = only(&both["add"]["added"]);
    let count = row["token_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("addAndCognify must report the post-cognify count, got {row}"));
    assert!(
        count > 0,
        "a non-empty text has a positive count, got {count}"
    );
}
