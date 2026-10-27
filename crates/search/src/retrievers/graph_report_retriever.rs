//! `GRAPH_REPORT`: a Markdown insight report over the whole knowledge graph.
//!
//! Port of Python's `GraphReportRetriever` (`graph_report_retriever.py`),
//! computed from `get_graph_data()` alone:
//!
//! 1. Hub nodes: the top-N knowledge nodes by normalised degree plus
//!    normalised PageRank (no LLM call).
//! 2. Surprising connections: entity-to-entity edges whose endpoints belong to
//!    different node sets.
//! 3. Edge provenance: EXTRACTED (entity-to-entity) vs DERIVED (structural)
//!    edges.
//! 4. Suggested questions: one structured LLM call over the report.
//!
//! The query is ignored. The analysis graph mirrors Python's `nx.DiGraph`:
//! `NodeSet` containers and `belongs_to_set` edges are left out, and parallel
//! edges between the same ordered pair collapse into one (the last
//! relationship wins), which is what degree and provenance count.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, NodeData};
use cognee_llm::{Llm, LlmExt};
use cognee_session::SessionContext;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};

/// Organizational container nodes, left out of the analysis graph. Their
/// `belongs_to_set` edges are the source of node-set membership, not content.
const CONTAINER_TYPE: &str = "NodeSet";
/// The membership edge that ties any node to its `NodeSet` container.
const MEMBERSHIP_RELATIONSHIP: &str = "belongs_to_set";
/// Node types surfaced as hubs (the knowledge layer). Structural nodes stay in
/// the topology but are not surfaced unless nothing else exists.
const HUB_TYPES: [&str; 2] = ["Entity", "EntityType"];
/// The node type whose edges count as extracted knowledge.
const ENTITY_TYPE: &str = "Entity";

/// Hub nodes and surprising connections surfaced when the request sets no
/// `top_k` (Python's `GraphReportRetriever(top_n=10)`).
pub const DEFAULT_GRAPH_REPORT_TOP_N: usize = 10;

/// The report returned for a graph with no nodes.
pub const EMPTY_GRAPH_REPORT: &str =
    "# Graph Insight Report\n\n_Graph is empty — run `cognify` first._\n";

/// The context item's payload key holding the deterministic report sections.
pub const REPORT_PAYLOAD_KEY: &str = "report";

/// networkx `pagerank` defaults.
const PAGERANK_ALPHA: f64 = 0.85;
const PAGERANK_MAX_ITER: usize = 100;
const PAGERANK_TOL: f64 = 1.0e-6;

/// The suggested-questions call reads at most this many characters of the
/// report.
const SUGGESTION_CONTEXT_CHARS: usize = 2000;
const SUGGESTION_SYSTEM_PROMPT: &str = "You are a knowledge graph analyst. Suggest short, specific questions answerable by querying the described knowledge graph. Return only the questions list.";
const FALLBACK_QUESTION: &str = "What are the main topics in this knowledge graph?";

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SuggestedQuestions {
    questions: Vec<String>,
}

/// The directed analysis graph: content nodes in first-seen order and one
/// edge per ordered pair, iterated as networkx iterates a `DiGraph` (by source
/// node order, then by each source's first-added target).
struct AnalysisGraph {
    nodes: Vec<String>,
    /// Per source node: target index -> relationship, in first-added order.
    successors: Vec<IndexMap<usize, String>>,
}

impl AnalysisGraph {
    fn build(node_type: &IndexMap<String, Option<String>>, edges: &[EdgeData]) -> Self {
        let nodes: Vec<String> = node_type
            .iter()
            .filter(|(_, kind)| kind.as_deref() != Some(CONTAINER_TYPE))
            .map(|(id, _)| id.clone())
            .collect();
        let index: HashMap<String, usize> = nodes
            .iter()
            .enumerate()
            .map(|(at, id)| (id.clone(), at))
            .collect();
        let mut successors = vec![IndexMap::new(); nodes.len()];
        for (source, target, relationship, _) in edges {
            if relationship == MEMBERSHIP_RELATIONSHIP {
                continue;
            }
            if let (Some(&from), Some(&to)) = (index.get(source), index.get(target)) {
                successors[from].insert(to, relationship.clone());
            }
        }
        Self { nodes, successors }
    }

    fn edges(&self) -> impl Iterator<Item = (usize, usize, &str)> {
        self.successors
            .iter()
            .enumerate()
            .flat_map(|(from, targets)| {
                targets
                    .iter()
                    .map(move |(&to, relationship)| (from, to, relationship.as_str()))
            })
    }

    fn edge_count(&self) -> usize {
        self.successors.iter().map(IndexMap::len).sum()
    }

    /// In-degree plus out-degree (a self-loop counts twice), as `DiGraph.degree`.
    fn degrees(&self) -> Vec<usize> {
        let mut degree: Vec<usize> = self.successors.iter().map(IndexMap::len).collect();
        for (_, to, _) in self.edges() {
            degree[to] += 1;
        }
        degree
    }

    /// networkx's `pagerank` with its defaults (alpha 0.85, 100 iterations,
    /// tolerance 1e-6, uniform personalization, dangling mass spread
    /// uniformly). `None` without edges or when the power iteration does not
    /// converge — Python then ranks hubs by degree alone.
    fn pagerank(&self) -> Option<Vec<f64>> {
        let n = self.nodes.len();
        if self.edge_count() == 0 {
            return None;
        }
        let uniform = 1.0 / n as f64;
        let out_degree: Vec<usize> = self.successors.iter().map(IndexMap::len).collect();
        let mut rank = vec![uniform; n];
        for _ in 0..PAGERANK_MAX_ITER {
            let dangling_mass: f64 = (0..n)
                .filter(|&node| out_degree[node] == 0)
                .map(|node| rank[node])
                .sum();
            let mut next = vec![0.0; n];
            for (from, to, _) in self.edges() {
                next[to] += rank[from] / out_degree[from] as f64;
            }
            for value in &mut next {
                *value = PAGERANK_ALPHA * (*value + dangling_mass * uniform)
                    + (1.0 - PAGERANK_ALPHA) * uniform;
            }
            let error: f64 = next
                .iter()
                .zip(&rank)
                .map(|(new, old)| (new - old).abs())
                .sum();
            rank = next;
            if error < n as f64 * PAGERANK_TOL {
                return Some(rank);
            }
        }
        warn!("PageRank did not converge, ranking hubs by degree only");
        None
    }
}

/// Min-max normalisation to `[0, 1]`; every value is `1.0` when they are all
/// equal (Python's `_normalize`).
fn normalize(values: &[f64]) -> Vec<f64> {
    let Some(low) = values.iter().copied().reduce(f64::min) else {
        return Vec::new();
    };
    let high = values.iter().copied().fold(low, f64::max);
    let span = high - low;
    if span == 0.0 {
        return vec![1.0; values.len()];
    }
    values.iter().map(|value| (value - low) / span).collect()
}

/// The deterministic part of the report.
struct GraphReport {
    markdown: String,
    payload: Value,
}

fn type_of(props: &NodeData) -> Option<String> {
    match props.get("type")? {
        Value::String(kind) => Some(kind.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Python truthiness of a JSON value, for `props.get(..) or ..` chains.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python's `_display_name`: the node's name or label, else `type (short-id)`.
fn display_name(node_id: &str, props: Option<&NodeData>) -> String {
    let name = props.and_then(|props| {
        props
            .get("name")
            .filter(|value| truthy(value))
            .or_else(|| props.get("label"))
    });
    if let Some(Value::String(name)) = name
        && !name.trim().is_empty()
    {
        return name.clone();
    }
    let kind = props
        .and_then(|props| props.get("type"))
        .filter(|value| truthy(value))
        .map_or_else(
            || "Node".to_string(),
            |value| match value {
                Value::String(kind) => kind.clone(),
                other => other.to_string(),
            },
        );
    let short_id: String = node_id.chars().take(8).collect();
    format!("{kind} ({short_id})")
}

/// Python's `_resolve_node_sets`: `belongs_to_set` edges to named `NodeSet`
/// containers, plus the comma-separated `source_node_set` property.
fn resolve_node_sets(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    node_type: &IndexMap<String, Option<String>>,
) -> HashMap<String, BTreeSet<String>> {
    let nodeset_name: HashMap<&str, &str> = nodes
        .iter()
        .filter(|(id, _)| node_type.get(id).and_then(Option::as_deref) == Some(CONTAINER_TYPE))
        .filter_map(|(id, props)| match props.get("name") {
            Some(Value::String(name)) if !name.is_empty() => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();

    let mut node_sets: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (source, target, relationship, _) in edges {
        if relationship == MEMBERSHIP_RELATIONSHIP
            && let Some(name) = nodeset_name.get(target.as_str())
        {
            node_sets
                .entry(source.clone())
                .or_default()
                .insert((*name).to_string());
        }
    }
    for (id, props) in nodes {
        if let Some(Value::String(source_node_set)) = props.get("source_node_set") {
            let names: BTreeSet<String> = source_node_set
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect();
            if !names.is_empty() {
                node_sets.entry(id.clone()).or_default().extend(names);
            }
        }
    }
    node_sets
}

/// Python's `get_context_from_objects`: the three deterministic sections.
fn build_report(nodes: &[GraphNode], edges: &[EdgeData], top_n: usize) -> GraphReport {
    if nodes.is_empty() {
        return GraphReport {
            markdown: EMPTY_GRAPH_REPORT.to_string(),
            payload: json!({
                "node_count": 0,
                "edge_count": 0,
                "hubs": [],
                "cross_set_connections": [],
                "edge_provenance": {"EXTRACTED": 0, "DERIVED": 0},
            }),
        };
    }

    // Later duplicates of a node id overwrite its properties but keep its
    // first position, as Python's dicts do.
    let mut props_by_id: HashMap<&str, &NodeData> = HashMap::new();
    let mut node_type: IndexMap<String, Option<String>> = IndexMap::new();
    for (id, props) in nodes {
        props_by_id.insert(id.as_str(), props);
        node_type.insert(id.clone(), type_of(props));
    }
    let node_sets = resolve_node_sets(nodes, edges, &node_type);
    let graph = AnalysisGraph::build(&node_type, edges);
    let type_at = |at: usize| node_type.get(&graph.nodes[at]).and_then(Option::as_deref);

    let degree = graph.degrees();
    let pagerank = graph.pagerank();
    let pagerank_at = |at: usize| pagerank.as_ref().map_or(0.0, |ranks| ranks[at]);

    // Hubs: normalised degree + normalised PageRank, best first, ties in node
    // order; knowledge-layer nodes preferred.
    let degree_score = normalize(&degree.iter().map(|&d| d as f64).collect::<Vec<_>>());
    let pagerank_score = pagerank.as_deref().map(normalize).unwrap_or_default();
    let score: Vec<f64> = (0..graph.nodes.len())
        .map(|at| degree_score[at] + pagerank_score.get(at).copied().unwrap_or(0.0))
        .collect();
    let mut ranked: Vec<usize> = (0..graph.nodes.len()).collect();
    ranked.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let knowledge: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|&at| type_at(at).is_some_and(|kind| HUB_TYPES.contains(&kind)))
        .collect();
    let hubs: Vec<usize> = if knowledge.is_empty() {
        ranked
    } else {
        knowledge
    }
    .into_iter()
    .take(top_n)
    .collect();

    // Surprising connections: entity-to-entity edges whose endpoints carry
    // different node sets, fully disjoint sets first, then by endpoint PageRank.
    let mut surprising: Vec<(bool, f64, usize, usize, &str)> = Vec::new();
    for (from, to, relationship) in graph.edges() {
        if type_at(from) != Some(ENTITY_TYPE) || type_at(to) != Some(ENTITY_TYPE) {
            continue;
        }
        let (Some(from_sets), Some(to_sets)) = (
            node_sets
                .get(&graph.nodes[from])
                .filter(|sets| !sets.is_empty()),
            node_sets
                .get(&graph.nodes[to])
                .filter(|sets| !sets.is_empty()),
        ) else {
            continue;
        };
        if from_sets != to_sets {
            let disjoint = from_sets.is_disjoint(to_sets);
            let novelty = pagerank_at(from) + pagerank_at(to);
            surprising.push((disjoint, novelty, from, to, relationship));
        }
    }
    surprising.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.total_cmp(&a.1)));
    surprising.truncate(top_n);

    // Provenance: EXTRACTED entity-to-entity edges, DERIVED everything else.
    let (mut extracted, mut derived) = (0usize, 0usize);
    for (from, to, _) in graph.edges() {
        if type_at(from) == Some(ENTITY_TYPE) && type_at(to) == Some(ENTITY_TYPE) {
            extracted += 1;
        } else {
            derived += 1;
        }
    }

    let name_of = |at: usize| {
        let id = &graph.nodes[at];
        display_name(id, props_by_id.get(id.as_str()).copied())
    };
    let sets_of = |at: usize| -> Vec<String> {
        node_sets
            .get(&graph.nodes[at])
            .map(|sets| sets.iter().cloned().collect())
            .unwrap_or_default()
    };
    let sets_label = |at: usize| {
        let sets = sets_of(at);
        if sets.is_empty() {
            "—".to_string()
        } else {
            sets.join(", ")
        }
    };

    let mut lines = vec![
        "# Graph Insight Report\n".to_string(),
        format!(
            "**Nodes:** {} | **Edges:** {}\n",
            graph.nodes.len(),
            graph.edge_count()
        ),
        "\n## 🏆 Hub Nodes\n".to_string(),
        "Top nodes by degree + PageRank (most connected / influential):\n".to_string(),
    ];
    for (rank, &at) in hubs.iter().enumerate() {
        lines.push(format!(
            "{}. **{}** (set: `{}`, degree: {}, PageRank: {:.4})",
            rank + 1,
            name_of(at),
            sets_label(at),
            degree[at],
            pagerank_at(at)
        ));
    }

    lines.push("\n\n## 🔗 Surprising Cross-Set Connections\n".to_string());
    if surprising.is_empty() {
        lines.push(
            "_No cross-node_set connections found (add data under multiple node_sets)._"
                .to_string(),
        );
    } else {
        lines.push("Entity pairs linked across different node sets:\n".to_string());
        for &(_, _, from, to, relationship) in &surprising {
            lines.push(format!(
                "- **{}** (`{}`) —[{relationship}]→ **{}** (`{}`)",
                name_of(from),
                sets_label(from),
                name_of(to),
                sets_label(to)
            ));
        }
    }

    lines.push("\n\n## 🏷️ Edge Provenance\n".to_string());
    lines.push("How edges entered the graph:\n".to_string());
    let total = (extracted + derived).max(1) as f64;
    for (label, count) in [
        ("EXTRACTED (entity-to-entity, LLM-derived)", extracted),
        ("DERIVED (structural / chunk-linked)", derived),
    ] {
        lines.push(format!(
            "- `{label}`: {count} edges ({:.1}%)",
            100.0 * count as f64 / total
        ));
    }
    lines.push(String::new());

    let payload = json!({
        "node_count": graph.nodes.len(),
        "edge_count": graph.edge_count(),
        "hubs": hubs.iter().map(|&at| json!({
            "id": graph.nodes[at],
            "name": name_of(at),
            "node_sets": sets_of(at),
            "degree": degree[at],
            "pagerank": pagerank_at(at),
        })).collect::<Vec<_>>(),
        "cross_set_connections": surprising.iter().map(|&(_, _, from, to, relationship)| json!({
            "source_id": graph.nodes[from],
            "target_id": graph.nodes[to],
            "relationship": relationship,
        })).collect::<Vec<_>>(),
        "edge_provenance": {"EXTRACTED": extracted, "DERIVED": derived},
    });

    GraphReport {
        markdown: lines.join("\n"),
        payload,
    }
}

pub struct GraphReportRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    top_n: usize,
}

impl GraphReportRetriever {
    /// `top_n`: hub nodes and surprising connections to surface when the
    /// request sets no `top_k` (default [`DEFAULT_GRAPH_REPORT_TOP_N`]).
    pub fn new(graph_db: Arc<dyn GraphDBTrait>, llm: Arc<dyn Llm>, top_n: Option<usize>) -> Self {
        Self {
            graph_db,
            llm,
            top_n: top_n.unwrap_or(DEFAULT_GRAPH_REPORT_TOP_N),
        }
    }

    /// Python's `_suggest_questions`: one structured call over the start of
    /// the report, a fixed question when it fails.
    async fn suggest_questions(&self, report: &str) -> String {
        let excerpt: String = report.chars().take(SUGGESTION_CONTEXT_CHARS).collect();
        let text_input = format!(
            "Knowledge graph report:\n{excerpt}\n\nBased on the hub nodes and cross-set \
             connections, suggest 4–5 specific, concise questions a user could ask to explore \
             this graph."
        );
        let questions = match self
            .llm
            .create_structured_output::<SuggestedQuestions>(
                &text_input,
                SUGGESTION_SYSTEM_PROMPT,
                None,
            )
            .await
        {
            Ok(suggested) => suggested.questions,
            Err(error) => {
                warn!(%error, "GraphReportRetriever: suggested-questions call failed");
                vec![FALLBACK_QUESTION.to_string()]
            }
        };

        let mut lines = vec![
            "\n## 💡 Suggested Questions\n".to_string(),
            "Questions ready to pipe into `cognee.search()`:\n".to_string(),
        ];
        lines.extend(questions.iter().map(|question| format!("- {question}")));
        lines.join("\n")
    }
}

#[async_trait]
impl SearchRetriever for GraphReportRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphReport
    }

    /// One item: the deterministic report as Markdown under
    /// [`REPORT_PAYLOAD_KEY`], with the hubs, cross-set connections and
    /// provenance counts it was built from. The query is ignored.
    #[tracing::instrument(
        name = "cognee.retrieval.get_context",
        skip(self, _query, params),
        fields(cognee.retrieval.retriever = "GraphReportRetriever")
    )]
    async fn get_context(
        &self,
        _query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let (nodes, edges) = self.graph_db.get_graph_data().await?;
        info!(
            nodes = nodes.len(),
            edges = edges.len(),
            "GraphReportRetriever fetched the graph"
        );
        let report = build_report(&nodes, &edges, params.top_k_or(self.top_n));
        let mut payload = report.payload;
        if let Value::Object(map) = &mut payload {
            map.insert(
                REPORT_PAYLOAD_KEY.to_string(),
                Value::String(report.markdown),
            );
        }
        Ok(vec![SearchItem {
            id: None,
            score: None,
            payload,
        }])
    }

    /// The report with the suggested questions appended. Python makes the
    /// suggestions call even for the empty-graph report.
    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };
        let report = context
            .iter()
            .find_map(|item| item.payload.get(REPORT_PAYLOAD_KEY)?.as_str())
            .unwrap_or_default();
        let questions = self.suggest_questions(report).await;
        Ok(SearchOutput::Text(format!("{report}\n{questions}")))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Mutex;

    use cognee_graph::MockGraphDB;
    use cognee_llm::{GenerationOptions, GenerationResponse, LlmError, LlmResult, Message};

    use super::*;

    /// Python's own report for [`fixture`] with `top_n = 3`, produced by
    /// `GraphReportRetriever.get_context_from_objects` at cognee `origin/dev`
    /// (bcab7e8) with networkx 3.6.1's PageRank.
    const PYTHON_REPORT_TOP_3: &str = "# Graph Insight Report\n\n**Nodes:** 7 | **Edges:** 11\n\n\n## 🏆 Hub Nodes\n\nTop nodes by degree + PageRank (most connected / influential):\n\n1. **Dave** (set: `beta`, degree: 5, PageRank: 0.2623)\n2. **Person** (set: `—`, degree: 4, PageRank: 0.2997)\n3. **Bob** (set: `beta`, degree: 3, PageRank: 0.1058)\n\n\n## 🔗 Surprising Cross-Set Connections\n\nEntity pairs linked across different node sets:\n\n- **Alice** (`alpha`) —[likes]→ **Bob** (`beta`)\n- **Carol** (`alpha, beta`) —[knows]→ **Dave** (`beta`)\n\n\n## 🏷️ Edge Provenance\n\nHow edges entered the graph:\n\n- `EXTRACTED (entity-to-entity, LLM-derived)`: 4 edges (36.4%)\n- `DERIVED (structural / chunk-linked)`: 7 edges (63.6%)\n";

    /// The same with `top_n = 10`: every knowledge node, ties in node order.
    const PYTHON_REPORT_TOP_10: &str = "# Graph Insight Report\n\n**Nodes:** 7 | **Edges:** 11\n\n\n## 🏆 Hub Nodes\n\nTop nodes by degree + PageRank (most connected / influential):\n\n1. **Dave** (set: `beta`, degree: 5, PageRank: 0.2623)\n2. **Person** (set: `—`, degree: 4, PageRank: 0.2997)\n3. **Bob** (set: `beta`, degree: 3, PageRank: 0.1058)\n4. **Alice** (set: `alpha`, degree: 3, PageRank: 0.0879)\n5. **Carol** (set: `alpha, beta`, degree: 3, PageRank: 0.0879)\n\n\n## 🔗 Surprising Cross-Set Connections\n\nEntity pairs linked across different node sets:\n\n- **Alice** (`alpha`) —[likes]→ **Bob** (`beta`)\n- **Carol** (`alpha, beta`) —[knows]→ **Dave** (`beta`)\n\n\n## 🏷️ Edge Provenance\n\nHow edges entered the graph:\n\n- `EXTRACTED (entity-to-entity, LLM-derived)`: 4 edges (36.4%)\n- `DERIVED (structural / chunk-linked)`: 7 edges (63.6%)\n";

    fn node(id: &str, props: Value) -> GraphNode {
        let Value::Object(map) = props else {
            panic!("node properties must be an object");
        };
        (
            id.to_string(),
            map.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        )
    }

    fn edge(source: &str, target: &str, relationship: &str) -> EdgeData {
        (
            source.to_string(),
            target.to_string(),
            relationship.to_string(),
            HashMap::new(),
        )
    }

    /// NodeSet containers, `belongs_to_set` and `source_node_set` membership,
    /// a parallel edge (Alice -> Bob, `knows` then `likes`), a self-loop, a
    /// dangling node and structural nodes.
    fn fixture() -> (Vec<GraphNode>, Vec<EdgeData>) {
        let nodes = vec![
            node("ns-alpha", json!({"type": "NodeSet", "name": "alpha"})),
            node("ns-beta", json!({"type": "NodeSet", "name": "beta"})),
            node("e1", json!({"type": "Entity", "name": "Alice"})),
            node("e2", json!({"type": "Entity", "name": "Bob"})),
            node("e3", json!({"type": "Entity", "name": "Carol"})),
            node("e4", json!({"type": "Entity", "name": "Dave"})),
            node("t1", json!({"type": "EntityType", "name": "Person"})),
            node(
                "c1-0123456789",
                json!({"type": "DocumentChunk", "source_node_set": "alpha, gamma"}),
            ),
            node("d1", json!({"type": "TextDocument", "name": "doc.txt"})),
        ];
        let edges = vec![
            edge("e1", "ns-alpha", "belongs_to_set"),
            edge("e2", "ns-beta", "belongs_to_set"),
            edge("e3", "ns-alpha", "belongs_to_set"),
            edge("e3", "ns-beta", "belongs_to_set"),
            edge("e4", "ns-beta", "belongs_to_set"),
            edge("e1", "e2", "knows"),
            edge("e1", "e2", "likes"),
            edge("e3", "e4", "knows"),
            edge("e2", "e4", "knows"),
            edge("e1", "t1", "is_a"),
            edge("e2", "t1", "is_a"),
            edge("e3", "t1", "is_a"),
            edge("e4", "t1", "is_a"),
            edge("c1-0123456789", "e1", "contains"),
            edge("c1-0123456789", "e3", "contains"),
            edge("c1-0123456789", "d1", "is_part_of"),
            edge("e4", "e4", "knows"),
        ];
        (nodes, edges)
    }

    #[test]
    fn report_matches_python_byte_for_byte() {
        let (nodes, edges) = fixture();
        assert_eq!(
            build_report(&nodes, &edges, 3).markdown,
            PYTHON_REPORT_TOP_3
        );
        assert_eq!(
            build_report(&nodes, &edges, 10).markdown,
            PYTHON_REPORT_TOP_10
        );
    }

    #[test]
    fn report_payload_carries_hubs_connections_and_provenance() {
        let (nodes, edges) = fixture();
        let payload = build_report(&nodes, &edges, 3).payload;

        let hub_ids: Vec<&str> = payload["hubs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hub| hub["id"].as_str().unwrap())
            .collect();
        assert_eq!(hub_ids, ["e4", "t1", "e2"]);
        assert_eq!(payload["hubs"][0]["degree"], 5);
        assert_eq!(payload["hubs"][0]["node_sets"], json!(["beta"]));
        assert_eq!(
            payload["cross_set_connections"],
            json!([
                {"source_id": "e1", "target_id": "e2", "relationship": "likes"},
                {"source_id": "e3", "target_id": "e4", "relationship": "knows"},
            ])
        );
        assert_eq!(
            payload["edge_provenance"],
            json!({"EXTRACTED": 4, "DERIVED": 7})
        );
        assert_eq!(payload["node_count"], 7);
        assert_eq!(payload["edge_count"], 11);
    }

    /// networkx's PageRank: every rank in (0, 1), summing to 1.
    #[test]
    fn pagerank_is_a_distribution_and_absent_without_edges() {
        let (nodes, edges) = fixture();
        let node_type: IndexMap<String, Option<String>> = nodes
            .iter()
            .map(|(id, props)| (id.clone(), type_of(props)))
            .collect();
        let ranks = AnalysisGraph::build(&node_type, &edges).pagerank().unwrap();
        assert_eq!(ranks.len(), 7);
        assert!((ranks.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{ranks:?}");

        assert!(AnalysisGraph::build(&node_type, &[]).pagerank().is_none());
    }

    /// Without edges there is no PageRank, so hubs rank by degree alone and
    /// non-knowledge nodes surface when no entity exists.
    #[test]
    fn hubs_fall_back_to_every_content_node() {
        let nodes = vec![
            node("c1", json!({"type": "DocumentChunk"})),
            node("c2", json!({"type": "DocumentChunk", "label": "Second"})),
        ];
        let report = build_report(&nodes, &[], 5);
        assert!(
            report.markdown.contains(
                "1. **DocumentChunk (c1)** (set: `—`, degree: 0, PageRank: 0.0000)\n\
                 2. **Second** (set: `—`, degree: 0, PageRank: 0.0000)"
            ),
            "{}",
            report.markdown
        );
        assert!(
            report
                .markdown
                .contains("_No cross-node_set connections found")
        );
        assert!(report.markdown.contains(": 0 edges (0.0%)"));
    }

    #[test]
    fn normalize_maps_to_unit_range_and_flat_values_to_one() {
        assert_eq!(normalize(&[1.0, 3.0, 2.0]), [0.0, 1.0, 0.5]);
        assert_eq!(normalize(&[2.0, 2.0]), [1.0, 1.0]);
        assert!(normalize(&[]).is_empty());
    }

    #[test]
    fn empty_graph_is_the_fixed_report() {
        assert_eq!(build_report(&[], &[], 10).markdown, EMPTY_GRAPH_REPORT);
    }

    struct QuestionLlm {
        questions: Option<Vec<&'static str>>,
        prompts: Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait]
    impl Llm for QuestionLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            panic!("the report makes no free-text call");
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            self.prompts.lock().unwrap().push(messages);
            match &self.questions {
                Some(questions) => Ok(json!({ "questions": questions })),
                None => Err(LlmError::ConfigError("offline".to_string())),
            }
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    async fn retriever(
        nodes: &[GraphNode],
        edges: &[EdgeData],
        questions: Option<Vec<&'static str>>,
    ) -> (GraphReportRetriever, Arc<QuestionLlm>) {
        let graph = MockGraphDB::new();
        for (id, props) in nodes {
            let mut value = json!(props);
            value["id"] = json!(id);
            graph.add_node_raw(value).await.unwrap();
        }
        graph.add_edges(edges).await.unwrap();
        let llm = Arc::new(QuestionLlm {
            questions,
            prompts: Mutex::new(Vec::new()),
        });
        (
            GraphReportRetriever::new(Arc::new(graph), Arc::clone(&llm) as Arc<dyn Llm>, None),
            llm,
        )
    }

    #[tokio::test]
    async fn completion_appends_suggested_questions_to_the_report() {
        let (nodes, edges) = fixture();
        let (retriever, llm) = retriever(
            &nodes,
            &edges,
            Some(vec!["Who is Dave?", "How is Bob linked?"]),
        )
        .await;
        assert_eq!(retriever.search_type(), SearchType::GraphReport);

        let params = SearchParams {
            top_k: Some(2),
            ..SearchParams::default()
        };
        let context = retriever.get_context("ignored", &params).await.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].payload["hubs"].as_array().unwrap().len(), 2);
        let report = context[0].payload[REPORT_PAYLOAD_KEY]
            .as_str()
            .unwrap()
            .to_string();

        let output = retriever
            .get_completion(
                "ignored",
                Some(context),
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap();
        let SearchOutput::Text(text) = output else {
            panic!("expected text output");
        };
        assert_eq!(
            text,
            format!(
                "{report}\n\n## 💡 Suggested Questions\n\nQuestions ready to pipe into \
                 `cognee.search()`:\n\n- Who is Dave?\n- How is Bob linked?"
            )
        );

        let prompts = llm.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0][0].content, SUGGESTION_SYSTEM_PROMPT);
        assert!(
            prompts[0][1]
                .content
                .starts_with(&format!("Knowledge graph report:\n{report}")),
            "{}",
            prompts[0][1].content
        );
    }

    /// A failed suggestions call falls back to one fixed question, and the
    /// empty graph still gets its report and the call.
    #[tokio::test]
    async fn empty_graph_and_failed_suggestions_still_report() {
        let (retriever, llm) = retriever(&[], &[], None).await;
        let output = retriever
            .get_completion(
                "",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();
        let SearchOutput::Text(text) = output else {
            panic!("expected text output");
        };
        assert!(text.starts_with(EMPTY_GRAPH_REPORT), "{text}");
        assert!(text.ends_with(&format!("- {FALLBACK_QUESTION}")), "{text}");
        assert_eq!(llm.prompts.lock().unwrap().len(), 1);
    }
}
