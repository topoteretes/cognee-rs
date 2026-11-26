//! Deterministic queries over a dataset's code graph: the engine behind the
//! `CODE` search type.
//!
//! Port of Python cognee's `cognee/modules/retrieval/code_retriever.py`
//! (with `code_search_index.py`), parity pin `a59433e6b`. A CODE call names
//! one exact graph operation and its arguments (the request's `code_query`,
//! merged into `retriever_specific_config`); no LLM, embedding or vector
//! search is involved. The dataset's code subgraph is read through the graph
//! adapter ([`load_code_graph_snapshot`]), indexed once
//! ([`CodeGraphSnapshot`]) and kept in a process-wide cache
//! ([`code_graph_snapshot_cache`]) that the code-graph writer and deletes
//! invalidate.
//!
//! [`CodeRequest::parse`] validates the operation and the `diagram` argument
//! exactly as Python's `CodeRetriever.__init__` does, and
//! [`run_code_operation`] runs any of the fifteen: `query_facts`, `explore`,
//! `traverse`, `find_path`, `impact_analysis`, `insights`, `architecture`,
//! `delta`, `capabilities`, `neighbors`, `subgraph`, and the text and flow
//! operations `search`, `flows`, `hubs` and `ask` (BM25 over the
//! [`CodeSearchIndex`], Python's `code_search_index.py`). Any result with a
//! graph can be drawn as a Mermaid or DOT diagram (Python's
//! `code_graph_diagram.py`, see [`render_result_diagram`]).

mod args;
mod cache;
mod diagram;
mod error;
mod flows;
mod navigation;
mod normalize;
mod ops;
mod overview;
mod snapshot;
mod text_index;
mod traversal;

use serde_json::{Value, json};

pub use args::{CodeConfig, Direction};
pub use cache::{
    CodeGraphSnapshotCache, DEFAULT_CACHE_MAX_ENTRIES, DEFAULT_CACHE_TTL, SNAPSHOT_FORMAT_VERSION,
    SnapshotCacheKey, SnapshotCacheLimits, clear_code_graph_snapshot_cache,
    code_graph_snapshot_cache, configure_code_graph_snapshot_cache,
    invalidate_code_graph_snapshots,
};
pub use diagram::{
    DiagramGraph, DiagramItem, collect_diagram_graph, dot_text, mermaid_text, render_dot,
    render_mermaid, render_result_diagram,
};
pub use error::{
    CODE_ERROR_STATUS, CodeErrorClass, CodeErrorCode, CodeQueryError, CodeQueryResult,
};
pub use flows::{CallsReach, FLOW_WALK_MAX_DEPTH, FLOW_WALK_MAX_VISITED, FlowReach, FlowVia};
pub use navigation::{NO_CODE_GRAPH_MESSAGE, NO_FLOWS_MESSAGE};
pub use normalize::{json_key, normalize_properties, normalize_value, scalar_text};
pub use snapshot::{
    CODE_NODE_TYPES, CodeGraphSnapshot, CodeTextIndex, FACT_TYPE_BY_KIND, FLOW_TYPE, FlowMembers,
    FlowRole, NodeProperties, NodeSortKey, REPOSITORY_TYPE, SnapshotEdge, TextMatch, doc_of,
    file_of, is_test_path, kind_of_type, load_code_graph_snapshot, match_tier, properties_of,
    short_names,
};
pub use text_index::{CodeSearchIndex, DocumentFields, SearchMatch, query_terms, tokenize};

use crate::pyjson::{is_truthy, py_repr, py_str, py_strip};

/// `_CAPABILITIES_VERSION`.
pub const CAPABILITIES_VERSION: u32 = 1;

/// The diagram formats a result can be drawn in (`DIAGRAM_FORMATS`).
pub const DIAGRAM_FORMATS: [&str; 2] = ["mermaid", "dot"];

/// `_LIMITS`: the defaults and caps of the navigation operations, reported
/// verbatim by `capabilities`.
pub fn limits_json() -> Value {
    json!({
        "search": {"limit": {"default": 10, "max": 50}},
        "neighbors": {"limit": {"default": 50, "max": 200}},
        "subgraph": {
            "seeds": 20,
            "depth": {"default": 1, "max": 3},
            "max_nodes": {"default": 100, "max": 500},
            "max_edges": {"default": 300, "max": 1000},
        },
        "flows": {"limit": {"default": 20, "max": 50}},
        "hubs": {"limit": {"default": 20, "max": 100}},
        "ask": {
            "max_seeds": {"default": 5, "max": 10},
            "max_nodes": {"default": 60, "max": 200},
            "max_citations": {"default": 25, "max": 60},
        },
    })
}

/// One of the fifteen CODE operations (`_OPERATIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeOperation {
    /// Facts matching structured filters.
    QueryFacts,
    /// The neighbourhood of one seed (the default operation).
    Explore,
    /// A walk from several seeds.
    Traverse,
    /// A path between two facts.
    FindPath,
    /// What depends on a fact.
    ImpactAnalysis,
    /// The graph's synthesized insights.
    Insights,
    /// A module-level overview.
    Architecture,
    /// What the last ingestion changed.
    Delta,
    /// What the server can do and the graph holds.
    Capabilities,
    /// A text search over the facts and flows.
    Search,
    /// Every edge of one fact.
    Neighbors,
    /// The graph around up to twenty seeds.
    Subgraph,
    /// Enola's flows.
    Flows,
    /// The most connected facts.
    Hubs,
    /// A cited answer to a question.
    Ask,
}

impl CodeOperation {
    /// Every operation, sorted by name (Python's `sorted(_OPERATIONS)`).
    pub const ALL: [CodeOperation; 15] = [
        Self::Architecture,
        Self::Ask,
        Self::Capabilities,
        Self::Delta,
        Self::Explore,
        Self::FindPath,
        Self::Flows,
        Self::Hubs,
        Self::ImpactAnalysis,
        Self::Insights,
        Self::Neighbors,
        Self::QueryFacts,
        Self::Search,
        Self::Subgraph,
        Self::Traverse,
    ];

    /// The wire name: `query_facts`, `neighbors`, ...
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueryFacts => "query_facts",
            Self::Explore => "explore",
            Self::Traverse => "traverse",
            Self::FindPath => "find_path",
            Self::ImpactAnalysis => "impact_analysis",
            Self::Insights => "insights",
            Self::Architecture => "architecture",
            Self::Delta => "delta",
            Self::Capabilities => "capabilities",
            Self::Search => "search",
            Self::Neighbors => "neighbors",
            Self::Subgraph => "subgraph",
            Self::Flows => "flows",
            Self::Hubs => "hubs",
            Self::Ask => "ask",
        }
    }

    /// The operation named `name` (lowercase), if any.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.as_str() == name)
    }

    /// Whether the result has a node/edge graph to draw (`_UNDRAWABLE_OPERATIONS`
    /// holds the five that do not: capabilities, search, flows, hubs, ask).
    pub fn is_drawable(self) -> bool {
        !matches!(
            self,
            Self::Capabilities | Self::Search | Self::Flows | Self::Hubs | Self::Ask
        )
    }
}

/// A diagram format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagramFormat {
    /// Mermaid flowchart source.
    Mermaid,
    /// Graphviz DOT source.
    Dot,
}

impl DiagramFormat {
    /// The wire name: `mermaid` or `dot`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Dot => "dot",
        }
    }
}

/// A validated CODE call: the operation, the diagram asked for and the
/// configuration its arguments are read from.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeRequest {
    /// The operation to run.
    pub operation: CodeOperation,
    /// The diagram to attach to the result, if any.
    pub diagram: Option<DiagramFormat>,
    /// The operation's arguments.
    pub config: CodeConfig,
}

impl CodeRequest {
    /// Validate `config` as Python's `CodeRetriever.__init__` does:
    /// `operation` (any case; `explore` when absent) must be one of the
    /// fifteen, and `diagram` a format name, a boolean or absent — and absent
    /// for the five operations whose result has no graph.
    ///
    /// # Errors
    ///
    /// `unknown_operation`, or `invalid_argument` on `diagram`, with
    /// Python's messages.
    pub fn parse(config: CodeConfig) -> CodeQueryResult<Self> {
        let name = match config.get("operation") {
            Some(value) if is_truthy(value) => py_str(value).to_lowercase(),
            _ => "explore".to_string(),
        };
        let Some(operation) = CodeOperation::from_name(&name) else {
            let names: Vec<&str> = CodeOperation::ALL
                .iter()
                .map(|operation| operation.as_str())
                .collect();
            return Err(CodeQueryError::unknown_operation(format!(
                "Unsupported CODE operation {}; expected one of {}.",
                crate::pyjson::py_repr_str(&name),
                names.join(", ")
            )));
        };
        let diagram = diagram_format(config.get("diagram"), operation)?;
        Ok(Self {
            operation,
            diagram,
            config,
        })
    }
}

/// `_diagram_format`: which diagram to attach to the result. `diagram` may
/// be a format name, `true` (Mermaid) or `false`/absent (none); the
/// architecture overview draws itself by default. Operations whose result
/// has no graph reject a diagram request.
fn diagram_format(
    value: Option<&Value>,
    operation: CodeOperation,
) -> CodeQueryResult<Option<DiagramFormat>> {
    let format = match value {
        None | Some(Value::Null) => {
            return Ok((operation == CodeOperation::Architecture).then_some(DiagramFormat::Mermaid));
        }
        Some(Value::Bool(false)) => return Ok(None),
        Some(Value::Bool(true)) => DiagramFormat::Mermaid,
        Some(Value::String(text)) if py_strip(text).to_lowercase() == "mermaid" => {
            DiagramFormat::Mermaid
        }
        Some(Value::String(text)) if py_strip(text).to_lowercase() == "dot" => DiagramFormat::Dot,
        Some(other) => {
            return Err(CodeQueryError::invalid(
                format!(
                    "Unsupported CODE diagram {}; expected one of {}, or a boolean.",
                    py_repr(other),
                    DIAGRAM_FORMATS.join(", ")
                ),
                "diagram",
            ));
        }
    };
    if !operation.is_drawable() {
        return Err(CodeQueryError::invalid(
            format!(
                "CODE {} results carry no graph to draw; drop diagram, or use subgraph or \
                 neighbors for a picture.",
                operation.as_str()
            ),
            "diagram",
        ));
    }
    Ok(Some(format))
}

/// Run a validated CODE call over `snapshot`, with `query` as the
/// human-readable seed (Python's `get_retrieved_objects` without the
/// snapshot load). When the request asks for a diagram, the result gets a
/// `diagram` entry last ([`render_result_diagram`]).
///
/// # Errors
///
/// The operation's own errors: `invalid_argument`, `not_found` and
/// `ambiguous`, and for `flows` and `ask` `no_code_graph` /
/// `feature_unavailable` (a dataset without a code graph, or without flows).
pub fn run_code_operation(
    snapshot: &CodeGraphSnapshot,
    request: &CodeRequest,
    query: &str,
) -> CodeQueryResult<Value> {
    let config = &request.config;
    let result = match request.operation {
        CodeOperation::QueryFacts => ops::query_facts(snapshot, config, query)?,
        CodeOperation::Explore => traversal::explore(snapshot, config, query)?,
        CodeOperation::Traverse => traversal::traverse(snapshot, config, query)?,
        CodeOperation::FindPath => traversal::find_path(snapshot, config, query)?,
        CodeOperation::ImpactAnalysis => traversal::impact_analysis(snapshot, config, query)?,
        CodeOperation::Insights => overview::insights(snapshot, config, query)?,
        CodeOperation::Architecture => overview::architecture(snapshot, config)?,
        CodeOperation::Neighbors => ops::neighbors(snapshot, config, query)?,
        CodeOperation::Subgraph => ops::subgraph(snapshot, config, query)?,
        CodeOperation::Delta => ops::delta(snapshot, config, query),
        CodeOperation::Capabilities => ops::capabilities(snapshot),
        CodeOperation::Search => navigation::search(snapshot, config, query)?,
        CodeOperation::Flows => navigation::flows(snapshot, config, query)?,
        CodeOperation::Hubs => navigation::hubs(snapshot, config)?,
        CodeOperation::Ask => navigation::ask(snapshot, config, query)?,
    };
    let Some(format) = request.diagram else {
        return Ok(result);
    };
    let diagram = render_result_diagram(&result, format);
    let mut result = result;
    if let Value::Object(fields) = &mut result {
        fields.insert("diagram".into(), diagram);
    }
    Ok(result)
}
