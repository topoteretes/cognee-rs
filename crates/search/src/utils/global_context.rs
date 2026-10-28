//! The global context index read side, shared by the retrievers that can
//! prepend it (Python's `cognee/modules/retrieval/utils/global_context.py`).
//!
//! The index is built by `improve(build_global_context_index=true)`: a root
//! summary of the dataset plus bucket summaries embedded in
//! `GlobalContextSummary_text`. Graph completion prepends it when
//! `include_global_context_index` is set.

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::GlobalContextSummary;
use cognee_vector::VectorDB;
use serde_json::Value;

use crate::types::SearchError;

/// Default number of bucket summaries a prelude names
/// (`global_context_index_top_k`).
pub const DEFAULT_GLOBAL_CONTEXT_INDEX_TOP_K: usize = 3;

/// Python's `_is_root`: a bool, the string `"true"`, or truthiness.
fn is_root(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The root summary's text, or `None` when no root exists in scope.
///
/// Relies on the dataset scope to read one dataset's graph; within it a
/// dataset has at most one root, and the first one found wins.
pub async fn load_root_text(graph_db: &dyn GraphDBTrait) -> Result<Option<String>, SearchError> {
    let filters = HashMap::from([(
        Cow::Borrowed("type"),
        vec![Value::from(GlobalContextSummary::TYPE)],
    )]);
    let (nodes, _) = graph_db
        .get_filtered_graph_data(&filters)
        .await
        .map_err(|e| SearchError::GraphError(e.to_string()))?;
    Ok(nodes
        .iter()
        .filter(|(_, attributes)| {
            attributes.get("type").and_then(Value::as_str) == Some(GlobalContextSummary::TYPE)
        })
        .find(|(_, attributes)| is_root(attributes.get("is_root")))
        .and_then(|(_, attributes)| {
            attributes
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        }))
}

/// Up to `top_k` non-root summary texts nearest `query_vector`; empty when
/// `top_k` is 0 or the collection does not exist.
pub async fn search_top_global_context_summaries(
    vector_db: &dyn VectorDB,
    query_vector: &[f32],
    top_k: usize,
) -> Result<Vec<String>, SearchError> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let vector_error = |e: cognee_vector::VectorDBError| SearchError::VectorError(e.to_string());
    if !vector_db
        .has_collection(GlobalContextSummary::TYPE, "text")
        .await
        .map_err(vector_error)?
    {
        return Ok(Vec::new());
    }
    // +1 in case the root is among the results.
    let results = vector_db
        .search_similar(GlobalContextSummary::TYPE, "text", query_vector, top_k + 1)
        .await
        .map_err(vector_error)?;
    Ok(results
        .into_iter()
        .filter(|result| !is_root(result.metadata.get("is_root")))
        .filter_map(|result| {
            result
                .metadata
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        })
        .take(top_k)
        .collect())
}

/// The prepend string; empty when neither input has content.
pub fn format_global_context_prelude(root_text: Option<&str>, top_summaries: &[String]) -> String {
    let mut blocks = Vec::new();
    if let Some(root) = root_text.filter(|text| !text.is_empty()) {
        blocks.push(format!("World summary:\n{root}"));
    }
    if !top_summaries.is_empty() {
        blocks.push(format!("Relevant areas:\n{}", top_summaries.join("\n\n")));
    }
    blocks.join("\n\n")
}

/// The prelude for `query`: the root, then the `top_k` nearest bucket
/// summaries (Python's `GraphCompletionRetriever._build_global_context_prelude`).
/// Empty for an empty query.
pub async fn build_global_context_prelude(
    query: &str,
    top_k: usize,
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
) -> Result<String, SearchError> {
    if query.is_empty() {
        return Ok(String::new());
    }
    let root_text = load_root_text(graph_db).await?;
    let top_summaries = if top_k == 0 {
        Vec::new()
    } else {
        let vector = embedding_engine
            .embed_queries(&[query])
            .await
            .map_err(|e| SearchError::EmbeddingError(e.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| SearchError::EmbeddingError("no query embedding".to_string()))?;
        search_top_global_context_summaries(vector_db, &vector, top_k).await?
    };
    Ok(format_global_context_prelude(
        root_text.as_deref(),
        &top_summaries,
    ))
}

/// Ports of Python's `test_global_context.py`.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_vector::{BruteForceVectorDB, VectorPoint};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn prelude_is_empty_when_both_inputs_are_empty() {
        assert_eq!(format_global_context_prelude(None, &[]), "");
    }

    #[test]
    fn prelude_omits_world_when_root_missing() {
        assert_eq!(
            format_global_context_prelude(None, &["A".to_string(), "B".to_string()]),
            "Relevant areas:\nA\n\nB"
        );
    }

    #[test]
    fn prelude_omits_areas_when_top_summaries_empty() {
        assert_eq!(
            format_global_context_prelude(Some("root"), &[]),
            "World summary:\nroot"
        );
    }

    #[test]
    fn prelude_includes_both_blocks_when_both_present() {
        assert_eq!(
            format_global_context_prelude(Some("root"), &["A".to_string(), "B".to_string()]),
            "World summary:\nroot\n\nRelevant areas:\nA\n\nB"
        );
    }

    async fn store(points: &[(f32, Value)]) -> BruteForceVectorDB {
        let db = BruteForceVectorDB::new();
        db.create_collection(GlobalContextSummary::TYPE, "text", 2)
            .await
            .unwrap();
        let points: Vec<VectorPoint> = points
            .iter()
            .map(|(x, metadata)| {
                let mut point = VectorPoint::new(Uuid::new_v4(), vec![*x, 1.0 - *x]);
                for (k, v) in metadata.as_object().unwrap() {
                    point = point.with_metadata(k.clone(), v.clone());
                }
                point
            })
            .collect();
        db.index_points(GlobalContextSummary::TYPE, "text", &points)
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn top_summaries_are_empty_when_top_k_is_zero_or_no_collection() {
        let db = BruteForceVectorDB::new();
        assert!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 3)
                .await
                .unwrap()
                .is_empty()
        );
        let db = store(&[(1.0, json!({"text": "A"}))]).await;
        assert!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn top_summaries_drop_roots_skip_missing_text_and_cap_at_top_k() {
        let db = store(&[
            (1.0, json!({"text": "root", "is_root": true})),
            (0.9, json!({"text": "A", "is_root": false})),
            (0.8, json!({"is_root": "false"})),
            (0.7, json!({"text": "B"})),
            (0.6, json!({"text": "C"})),
        ])
        .await;
        assert_eq!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 2)
                .await
                .unwrap(),
            ["A"]
        );
        assert_eq!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 4)
                .await
                .unwrap(),
            ["A", "B", "C"]
        );
    }
}
