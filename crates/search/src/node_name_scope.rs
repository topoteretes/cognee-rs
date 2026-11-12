//! What `node_name` means for each search type, and which types can honour it.
//!
//! Port of Python's `cognee/modules/search/methods/node_name_scope.py`.
//! `node_name` is a filter whose meaning is set by `node_type`:
//!
//! - `NodeSet` (the default): the node-set tags the data was ingested with.
//! - Any other graph node type: the named nodes of that type and their
//!   one-hop neighbours.
//!
//! A search type handed a filter it cannot apply must refuse it: dropping the
//! filter returns unscoped results, and matching it against the wrong field
//! returns none, and neither tells the caller what happened.
//!
//! Python's `node_type` is a class, and `None` with names is refused. The Rust
//! API carries the class name as an `Option<String>` whose `None` is the
//! `NodeSet` default (Python's default argument), so it cannot express
//! Python's explicit `None`. An explicit empty string stands in for it and is
//! refused with Python's message.

use crate::types::{SearchError, SearchType};

/// The class name of the default `node_type`: `node_name` names node-set tags.
pub const NODE_SET_TYPE: &str = "NodeSet";

/// The scope a `node_name` filter asks for (Python `NodeNameScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeNameScope {
    /// Node-set tags (`node_type` is `NodeSet`).
    NodeSet,
    /// The named nodes of a graph node type and their one-hop neighbours.
    Graph,
}

impl NodeNameScope {
    /// Python's enum value: `"node_set"` or `"graph"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NodeSet => "node_set",
            Self::Graph => "graph",
        }
    }
}

const EVERY_SCOPE: &[NodeNameScope] = &[NodeNameScope::NodeSet, NodeNameScope::Graph];
const NODE_SET_ONLY: &[NodeNameScope] = &[NodeNameScope::NodeSet];

/// The scopes each search type honours (Python `SUPPORTED_SCOPES`,
/// `node_name_scope.py:36-49`). A type missing here ignores `node_name` and is
/// refused when one is given. `HYBRID_COMPLETION` lists every scope because a
/// graph scope is served by deferring to `GRAPH_COMPLETION`. Python's
/// `HYBRID_COMPLETION_DECOMPOSITION` and `AGENTIC_COMPLETION` have no Rust
/// [`SearchType`] and are left out.
pub const SUPPORTED_SCOPES: [(SearchType, &[NodeNameScope]); 10] = [
    (SearchType::Chunks, EVERY_SCOPE),
    (SearchType::RagCompletion, EVERY_SCOPE),
    (SearchType::HybridCompletion, EVERY_SCOPE),
    (SearchType::Temporal, NODE_SET_ONLY),
    (SearchType::CodingRules, NODE_SET_ONLY),
    (SearchType::GraphCompletion, EVERY_SCOPE),
    (SearchType::GraphCompletionCot, EVERY_SCOPE),
    (SearchType::GraphCompletionContextExtension, EVERY_SCOPE),
    (SearchType::GraphCompletionDecomposition, EVERY_SCOPE),
    (SearchType::GraphSummaryCompletion, EVERY_SCOPE),
];

/// Whether `search_type` can apply a `node_name` filter of `scope`.
pub fn honours(search_type: SearchType, scope: NodeNameScope) -> bool {
    SUPPORTED_SCOPES
        .iter()
        .any(|(supported, scopes)| *supported == search_type && scopes.contains(&scope))
}

/// The scope `node_name` asks for, or `None` when no filter is set (Python
/// `node_name_scope`, `node_name_scope.py:52-69`). `node_type: None` is the
/// `NodeSet` default; an empty `node_type` with names is refused, because the
/// graph projection needs the type.
pub fn node_name_scope(
    node_type: Option<&str>,
    node_name: Option<&[String]>,
) -> Result<Option<NodeNameScope>, SearchError> {
    if node_name.is_none_or(<[String]>::is_empty) {
        return Ok(None);
    }
    match node_type {
        None | Some(NODE_SET_TYPE) => Ok(Some(NodeNameScope::NodeSet)),
        Some("") => Err(SearchError::QueryValidation(
            "node_name needs a node_type: NodeSet for node-set tags, or the graph node type to \
             scope to (a Document class, Entity, ...)."
                .to_string(),
        )),
        Some(_) => Ok(Some(NodeNameScope::Graph)),
    }
}

/// The search types that honour `scope`, in [`SUPPORTED_SCOPES`] order.
pub fn search_types_honouring(scope: NodeNameScope) -> Vec<SearchType> {
    SUPPORTED_SCOPES
        .iter()
        .filter(|(_, scopes)| scopes.contains(&scope))
        .map(|(search_type, _)| *search_type)
        .collect()
}

/// Refuses unless `search_type` can apply the filter `node_type` /
/// `node_name` ask for, with Python's message (`reject_unsupported_node_name`,
/// `node_name_scope.py:86-105`). Returns the scope, `None` without a filter.
pub fn reject_unsupported_node_name(
    search_type: SearchType,
    node_type: Option<&str>,
    node_name: Option<&[String]>,
) -> Result<Option<NodeNameScope>, SearchError> {
    let Some(scope) = node_name_scope(node_type, node_name)? else {
        return Ok(None);
    };
    if honours(search_type, scope) {
        return Ok(Some(scope));
    }
    let mut honouring: Vec<String> = search_types_honouring(scope)
        .into_iter()
        .map(SearchType::wire_name)
        .collect();
    honouring.sort();
    Err(SearchError::QueryValidation(format!(
        "{} does not apply node_name with node_type={}. Search types that do: {}.",
        search_type.wire_name(),
        node_type.unwrap_or(NODE_SET_TYPE),
        honouring.join(", "),
    )))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["alpha".to_string()]
    }

    #[test]
    fn no_names_is_no_scope_whatever_the_type() {
        let empty: Vec<String> = Vec::new();
        for node_type in [None, Some("NodeSet"), Some("Entity"), Some("")] {
            assert_eq!(node_name_scope(node_type, None).unwrap(), None);
            assert_eq!(node_name_scope(node_type, Some(&empty)).unwrap(), None);
        }
    }

    #[test]
    fn the_default_and_node_set_name_node_sets_and_any_other_type_the_graph() {
        let names = names();
        assert_eq!(
            node_name_scope(None, Some(&names)).unwrap(),
            Some(NodeNameScope::NodeSet)
        );
        assert_eq!(
            node_name_scope(Some("NodeSet"), Some(&names)).unwrap(),
            Some(NodeNameScope::NodeSet)
        );
        assert_eq!(
            node_name_scope(Some("Entity"), Some(&names)).unwrap(),
            Some(NodeNameScope::Graph)
        );
    }

    #[test]
    fn an_empty_node_type_with_names_is_refused_with_pythons_message() {
        let err = node_name_scope(Some(""), Some(&names())).unwrap_err();
        assert!(matches!(err, SearchError::QueryValidation(_)));
        assert_eq!(
            err.to_string(),
            "node_name needs a node_type: NodeSet for node-set tags, or the graph node type to \
             scope to (a Document class, Entity, ...)."
        );
    }

    #[test]
    fn the_table_matches_python_for_the_shared_types() {
        use SearchType::*;
        let graph = search_types_honouring(NodeNameScope::Graph);
        assert_eq!(
            graph,
            vec![
                Chunks,
                RagCompletion,
                HybridCompletion,
                GraphCompletion,
                GraphCompletionCot,
                GraphCompletionContextExtension,
                GraphCompletionDecomposition,
                GraphSummaryCompletion,
            ]
        );
        let node_set = search_types_honouring(NodeNameScope::NodeSet);
        assert_eq!(node_set.len(), SUPPORTED_SCOPES.len());
        for search_type in [Temporal, CodingRules] {
            assert!(honours(search_type, NodeNameScope::NodeSet));
            assert!(!honours(search_type, NodeNameScope::Graph));
        }
        for search_type in [
            Summaries,
            Cypher,
            NaturalLanguage,
            FeelingLucky,
            Feedback,
            ChunksLexical,
            GraphReport,
        ] {
            assert!(!honours(search_type, NodeNameScope::NodeSet));
            assert!(!honours(search_type, NodeNameScope::Graph));
        }
    }

    #[test]
    fn a_type_that_honours_the_scope_is_accepted() {
        let names = names();
        assert_eq!(
            reject_unsupported_node_name(SearchType::Temporal, None, Some(&names)).unwrap(),
            Some(NodeNameScope::NodeSet)
        );
        assert_eq!(
            reject_unsupported_node_name(SearchType::GraphCompletion, Some("Entity"), Some(&names))
                .unwrap(),
            Some(NodeNameScope::Graph)
        );
        assert_eq!(
            reject_unsupported_node_name(SearchType::ChunksLexical, Some("Entity"), None).unwrap(),
            None
        );
    }

    #[test]
    fn a_type_that_cannot_apply_a_graph_scope_is_refused_with_pythons_message() {
        let err =
            reject_unsupported_node_name(SearchType::Temporal, Some("Entity"), Some(&names()))
                .unwrap_err();
        assert!(matches!(err, SearchError::QueryValidation(_)));
        assert_eq!(
            err.to_string(),
            "TEMPORAL does not apply node_name with node_type=Entity. Search types that do: \
             CHUNKS, GRAPH_COMPLETION, GRAPH_COMPLETION_CONTEXT_EXTENSION, GRAPH_COMPLETION_COT, \
             GRAPH_COMPLETION_DECOMPOSITION, GRAPH_SUMMARY_COMPLETION, HYBRID_COMPLETION, \
             RAG_COMPLETION."
        );
    }

    #[test]
    fn a_type_that_ignores_node_name_is_refused_under_the_default_node_type() {
        let err = reject_unsupported_node_name(SearchType::ChunksLexical, None, Some(&names()))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "CHUNKS_LEXICAL does not apply node_name with node_type=NodeSet. Search types that \
             do: CHUNKS, CODING_RULES, GRAPH_COMPLETION, GRAPH_COMPLETION_CONTEXT_EXTENSION, \
             GRAPH_COMPLETION_COT, GRAPH_COMPLETION_DECOMPOSITION, GRAPH_SUMMARY_COMPLETION, \
             HYBRID_COMPLETION, RAG_COMPLETION, TEMPORAL."
        );
    }
}
