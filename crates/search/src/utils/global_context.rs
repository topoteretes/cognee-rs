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
use uuid::Uuid;

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

/// Whether a summary's `dataset_id` (a node attribute or vector payload
/// field) is one of `dataset_ids`; `None` is an unscoped read.
fn in_scope(value: Option<&Value>, dataset_ids: Option<&[Uuid]>) -> bool {
    let Some(dataset_ids) = dataset_ids else {
        return true;
    };
    value
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .is_some_and(|id| dataset_ids.contains(&id))
}

/// The root summary's text, or `None` when no root exists in scope.
///
/// Python relies on backend access control to read one dataset's graph. Here
/// datasets may share one graph, so a scoped read (`dataset_ids`, the
/// search's `SearchParams::dataset_ids`) keeps only the roots of those
/// datasets, by their `dataset_id`; several scoped roots are joined in
/// `dataset_ids` order. An unscoped read takes the first root found, as
/// Python does within its scope.
pub async fn load_root_text(
    graph_db: &dyn GraphDBTrait,
    dataset_ids: Option<&[Uuid]>,
) -> Result<Option<String>, SearchError> {
    let filters = HashMap::from([(
        Cow::Borrowed("type"),
        vec![Value::from(GlobalContextSummary::TYPE)],
    )]);
    let (nodes, _) = graph_db
        .get_filtered_graph_data(&filters)
        .await
        .map_err(|e| SearchError::GraphError(e.to_string()))?;
    let roots = nodes.iter().filter(|(_, attributes)| {
        attributes.get("type").and_then(Value::as_str) == Some(GlobalContextSummary::TYPE)
            && is_root(attributes.get("is_root"))
    });
    let text = |attributes: &HashMap<Cow<'_, str>, Value>| {
        attributes
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let Some(scope) = dataset_ids else {
        return Ok(roots.into_iter().next().and_then(|(_, a)| text(a)));
    };
    let roots: Vec<_> = roots.collect();
    let texts: Vec<String> = scope
        .iter()
        .filter_map(|dataset_id| {
            roots
                .iter()
                .find(|(_, attributes)| {
                    in_scope(
                        attributes.get("dataset_id"),
                        Some(std::slice::from_ref(dataset_id)),
                    )
                })
                .and_then(|(_, attributes)| text(attributes))
        })
        .collect();
    Ok((!texts.is_empty()).then(|| texts.join("\n\n")))
}

/// How many nearest summaries a scoped search reads before keeping the
/// scope's: the vector store has no `dataset_id` predicate, so the scope is
/// applied after the search (exact for a collection up to this size).
const SCOPED_SUMMARY_FETCH: usize = 4096;

/// Up to `top_k` non-root summary texts nearest `query_vector`; empty when
/// `top_k` is 0 or the collection does not exist. A scoped search
/// (`dataset_ids`) keeps only those datasets' summaries, by their payload's
/// `dataset_id`.
pub async fn search_top_global_context_summaries(
    vector_db: &dyn VectorDB,
    query_vector: &[f32],
    top_k: usize,
    dataset_ids: Option<&[Uuid]>,
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
    // +1 in case the root is among the results; a scoped search reads wide
    // enough to find its datasets' summaries among everyone's.
    let limit = if dataset_ids.is_some() {
        SCOPED_SUMMARY_FETCH.max(top_k + 1)
    } else {
        top_k + 1
    };
    let results = vector_db
        .search_similar(GlobalContextSummary::TYPE, "text", query_vector, limit)
        .await
        .map_err(vector_error)?;
    Ok(results
        .into_iter()
        .filter(|result| !is_root(result.metadata.get("is_root")))
        .filter(|result| in_scope(result.metadata.get("dataset_id"), dataset_ids))
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
/// Empty for an empty query. `dataset_ids` scopes both reads to the search's
/// datasets (`SearchParams::dataset_ids`; `None` unscoped).
pub async fn build_global_context_prelude(
    query: &str,
    top_k: usize,
    dataset_ids: Option<&[Uuid]>,
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
) -> Result<String, SearchError> {
    if query.is_empty() {
        return Ok(String::new());
    }
    let root_text = load_root_text(graph_db, dataset_ids).await?;
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
        search_top_global_context_summaries(vector_db, &vector, top_k, dataset_ids).await?
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
            search_top_global_context_summaries(&db, &[1.0, 0.0], 3, None)
                .await
                .unwrap()
                .is_empty()
        );
        let db = store(&[(1.0, json!({"text": "A"}))]).await;
        assert!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 0, None)
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
            search_top_global_context_summaries(&db, &[1.0, 0.0], 2, None)
                .await
                .unwrap(),
            ["A"]
        );
        assert_eq!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 4, None)
                .await
                .unwrap(),
            ["A", "B", "C"]
        );
    }

    // Datasets may share one graph and one vector store: a scoped search
    // reads only its datasets' summaries, never another dataset's.
    #[tokio::test]
    async fn a_scoped_search_keeps_only_its_datasets_summaries() {
        let (mine, other) = (Uuid::new_v4(), Uuid::new_v4());
        let db = store(&[
            (
                1.0,
                json!({"text": "theirs", "dataset_id": other.to_string()}),
            ),
            (0.9, json!({"text": "mine", "dataset_id": mine.to_string()})),
            (0.8, json!({"text": "unknown"})),
        ])
        .await;
        assert_eq!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 1, Some(&[mine]))
                .await
                .unwrap(),
            ["mine"]
        );
        assert_eq!(
            search_top_global_context_summaries(&db, &[1.0, 0.0], 1, None)
                .await
                .unwrap(),
            ["theirs"]
        );
    }

    #[tokio::test]
    async fn a_scoped_root_is_its_datasets_root() {
        use cognee_graph::MockGraphDB;
        let (mine, other) = (Uuid::new_v4(), Uuid::new_v4());
        let graph = MockGraphDB::new();
        for (dataset_id, text) in [(other, "their world"), (mine, "my world")] {
            let summary = GlobalContextSummary::new(
                Uuid::new_v4(),
                text,
                dataset_id.to_string(),
                1,
                true,
                None,
            );
            graph
                .add_node_raw(serde_json::to_value(&summary).unwrap())
                .await
                .unwrap();
        }
        assert_eq!(
            load_root_text(&graph, Some(&[mine]))
                .await
                .unwrap()
                .as_deref(),
            Some("my world")
        );
        assert_eq!(
            load_root_text(&graph, Some(&[Uuid::new_v4()]))
                .await
                .unwrap(),
            None
        );
        assert!(load_root_text(&graph, None).await.unwrap().is_some());
    }
}
