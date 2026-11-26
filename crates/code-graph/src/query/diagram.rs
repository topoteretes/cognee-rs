//! Render a CODE result as an architecture diagram: Mermaid or Graphviz
//! DOT source text.
//!
//! Port of Python cognee's `cognee/modules/retrieval/code_graph_diagram.py`
//! (parity pin `a59433e6b`). When a CODE call asks for one (`diagram`:
//! `true`, `"mermaid"` or `"dot"`; `architecture` draws Mermaid unless told
//! `false`), the result's facts and edges are also rendered as diagram
//! source, so a UI, a README or a chat client can show the graph. Rendering
//! is deterministic and purely textual: the same result always gives
//! byte-identical source, and the source is byte-identical to Python's.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::DiagramFormat;
use crate::pyjson::{is_truthy, py_repr, py_str, py_str_truthy_or};

/// One node or edge of a diagram: a fact or an edge as a result lists it.
pub type DiagramItem = Map<String, Value>;

/// `_MERMAID_SHAPES`: fact kind -> Mermaid node shape (open, close); any
/// other kind is a plain box.
fn mermaid_shape(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "module" | "repository" => ("[[", "]]"),
        "symbol" => ("([", "])"),
        "route" => (">", "]"),
        "storage" => ("[(", ")]"),
        "dependency" => ("{{", "}}"),
        "service" => ("[/", "/]"),
        "insight" => ("{", "}"),
        "intent" => ("[/", "\\]"),
        _ => ("[", "]"),
    }
}

/// `_DOT_SHAPES`: fact kind -> Graphviz shape; any other kind is a box.
fn dot_shape(kind: &str) -> &'static str {
    match kind {
        "module" => "component",
        "symbol" => "ellipse",
        "route" => "cds",
        "storage" => "cylinder",
        "dependency" => "hexagon",
        "service" => "box3d",
        "insight" => "diamond",
        "intent" => "parallelogram",
        "repository" => "folder",
        _ => "box",
    }
}

/// `_ROOT_MODULE_LABEL`: how the repository's root module (named `.`) is
/// drawn.
const ROOT_MODULE_LABEL: &str = "(root)";

/// The highlight colour of the focus, seeds or path.
const HIGHLIGHT_COLOR: &str = "#d64545";

/// Python's `str(value)` of an optional value: `None` when missing.
fn text(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_string(), py_str)
}

/// Python's `str(a or b or "")` over optional values.
fn first_truthy_text(values: &[Option<&Value>]) -> String {
    let first = values
        .iter()
        .flatten()
        .copied()
        .find(|value| is_truthy(value));
    py_str_truthy_or(first, "")
}

/// `_label`: a node's name (else its id); the root module is `(root)`.
fn label(node: &DiagramItem) -> String {
    let name = first_truthy_text(&[node.get("name"), node.get("id")]);
    if node.get("kind").and_then(Value::as_str) == Some("module") && name == "." {
        return ROOT_MODULE_LABEL.to_string();
    }
    name
}

/// `_mermaid_text`: a label escaped with Mermaid's entity codes. `#` goes
/// first, or the codes would be escaped again; `&` too, since Mermaid
/// renders labels as HTML.
pub fn mermaid_text(text: &str) -> String {
    text.replace('#', "#35;")
        .replace('&', "#38;")
        .replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
        .replace('\n', " ")
}

/// `_yaml_string`: a double-quoted YAML scalar for the Mermaid front-matter
/// title. Mermaid would show `<`/`>` escaped, so they become the
/// single-guillemet look-alikes `‹`/`›`.
fn yaml_string(text: &str) -> String {
    let text = text.replace('<', "\u{2039}").replace('>', "\u{203a}");
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

/// `_dot_text`: a label escaped for a DOT double-quoted string.
pub fn dot_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

/// `_edge_label`: the relation, with ` x<count>` when an architecture edge
/// rolls up more than one.
fn edge_label(edge: &DiagramItem) -> String {
    let label = first_truthy_text(&[edge.get("type")]);
    match edge.get("count") {
        Some(Value::Number(count)) if !count.is_f64() && count.as_u64().is_some_and(|n| n > 1) => {
            format!("{label} x{count}")
        }
        _ => label,
    }
}

/// The nodes grouped by repository (`str(repo or "")`), in sorted repository
/// order, each group in node order.
fn by_repo(nodes: &[DiagramItem]) -> BTreeMap<String, Vec<&DiagramItem>> {
    let mut groups: BTreeMap<String, Vec<&DiagramItem>> = BTreeMap::new();
    for node in nodes {
        groups
            .entry(first_truthy_text(&[node.get("repo")]))
            .or_default()
            .push(node);
    }
    groups
}

/// `{str(node["id"]): f"n{index}"}`: each node's short alias (a repeated id
/// takes its last position, as Python's dict comprehension does).
fn aliases(nodes: &[DiagramItem]) -> HashMap<String, String> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (text(node.get("id")), format!("n{index}")))
        .collect()
}

/// The (source, target) aliases of an edge whose two ends are drawn.
fn edge_ends<'a>(
    alias: &'a HashMap<String, String>,
    edge: &DiagramItem,
) -> Option<(&'a String, &'a String)> {
    Some((
        alias.get(&text(edge.get("source_id")))?,
        alias.get(&text(edge.get("target_id")))?,
    ))
}

/// `render_mermaid`: a Mermaid `flowchart LR` of the given facts and edges.
///
/// Nodes are grouped into one subgraph per repository when more than one is
/// present; shapes follow the fact kind; the highlighted nodes (the seeds,
/// focus or path of the operation) get a distinct class. An edge with an end
/// that is not drawn is left out. A non-empty `title` becomes YAML front
/// matter.
pub fn render_mermaid(
    nodes: &[DiagramItem],
    edges: &[DiagramItem],
    highlight_ids: &[String],
    title: Option<&str>,
) -> String {
    let alias = aliases(nodes);
    let highlighted: Vec<&String> = highlight_ids
        .iter()
        .filter_map(|node_id| alias.get(node_id))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        // The front matter is YAML: quoted, or a '#' starts a comment and a
        // ':' breaks the mapping.
        lines.push("---".to_string());
        lines.push(format!("title: {}", yaml_string(title)));
        lines.push("---".to_string());
    }
    lines.push("flowchart LR".to_string());

    let node_line = |node: &DiagramItem, indent: &str| {
        let (open, close) = mermaid_shape(&text(node.get("kind")));
        format!(
            "{indent}{}{open}\"{}\"{close}",
            alias[&text(node.get("id"))],
            mermaid_text(&label(node))
        )
    };
    let groups = by_repo(nodes);
    if groups.len() > 1 {
        for (index, (repo, members)) in groups.iter().enumerate() {
            let repo = if repo.is_empty() { "(no repo)" } else { repo };
            lines.push(format!(
                "    subgraph repo{index}[\"{}\"]",
                mermaid_text(repo)
            ));
            lines.extend(members.iter().map(|node| node_line(node, "        ")));
            lines.push("    end".to_string());
        }
    } else {
        lines.extend(nodes.iter().map(|node| node_line(node, "    ")));
    }

    for edge in edges {
        let Some((source, target)) = edge_ends(&alias, edge) else {
            continue;
        };
        let label = mermaid_text(&edge_label(edge));
        if label.is_empty() {
            lines.push(format!("    {source} --> {target}"));
        } else {
            lines.push(format!("    {source} -- \"{label}\" --> {target}"));
        }
    }

    if !highlighted.is_empty() {
        lines.push(format!(
            "    classDef highlight stroke:{HIGHLIGHT_COLOR},stroke-width:3px"
        ));
        let classes: Vec<&str> = highlighted.iter().map(|alias| alias.as_str()).collect();
        lines.push(format!("    class {} highlight", classes.join(",")));
    }
    lines.join("\n") + "\n"
}

/// `render_dot`: a Graphviz `digraph` of the given facts and edges, grouped
/// and shaped as [`render_mermaid`] does. A non-empty `title` labels the
/// graph.
pub fn render_dot(
    nodes: &[DiagramItem],
    edges: &[DiagramItem],
    highlight_ids: &[String],
    title: Option<&str>,
) -> String {
    let alias = aliases(nodes);
    let highlight: HashSet<&String> = highlight_ids
        .iter()
        .filter(|node_id| alias.contains_key(*node_id))
        .collect();

    let mut lines: Vec<String> = vec![
        "digraph code_graph {".to_string(),
        "    rankdir=LR;".to_string(),
        "    node [fontname=\"Helvetica\"];".to_string(),
    ];
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        lines.push(format!("    label=\"{}\"; labelloc=t;", dot_text(title)));
    }

    let node_line = |node: &DiagramItem, indent: &str| {
        let node_id = text(node.get("id"));
        let mut attributes = vec![
            format!("label=\"{}\"", dot_text(&label(node))),
            format!("shape={}", dot_shape(&text(node.get("kind")))),
        ];
        if highlight.contains(&node_id) {
            attributes.push(format!("color=\"{HIGHLIGHT_COLOR}\" penwidth=3"));
        }
        format!("{indent}{} [{}];", alias[&node_id], attributes.join(" "))
    };
    let groups = by_repo(nodes);
    if groups.len() > 1 {
        for (index, (repo, members)) in groups.iter().enumerate() {
            let repo = if repo.is_empty() { "(no repo)" } else { repo };
            lines.push(format!("    subgraph cluster_{index} {{"));
            lines.push(format!("        label=\"{}\";", dot_text(repo)));
            lines.extend(members.iter().map(|node| node_line(node, "        ")));
            lines.push("    }".to_string());
        }
    } else {
        lines.extend(nodes.iter().map(|node| node_line(node, "    ")));
    }

    for edge in edges {
        let Some((source, target)) = edge_ends(&alias, edge) else {
            continue;
        };
        let label = dot_text(&edge_label(edge));
        if label.is_empty() {
            lines.push(format!("    {source} -> {target};"));
        } else {
            lines.push(format!("    {source} -> {target} [label=\"{label}\"];"));
        }
    }
    lines.push("}".to_string());
    lines.join("\n") + "\n"
}

/// The graph of one CODE result: its nodes, edges and highlighted node ids.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiagramGraph {
    /// The facts, each once, in the order the result lists them.
    pub nodes: Vec<DiagramItem>,
    /// The edges, each (source, target, type) once, sorted by those three.
    pub edges: Vec<DiagramItem>,
    /// The focus, seeds, targets or path, each once, in order.
    pub highlight_ids: Vec<String>,
}

/// Collects a result's nodes (each id once) and highlights.
#[derive(Default)]
struct Collector {
    graph: DiagramGraph,
    seen: HashSet<String>,
}

impl Collector {
    /// `add_node`: a fact with an id, unless already added.
    fn add_node(&mut self, fact: Option<&Value>) {
        if let Some(Value::Object(fact)) = fact
            && let Some(node_id) = fact.get("id").filter(|id| !id.is_null())
            && self.seen.insert(py_str(node_id))
        {
            self.graph.nodes.push(fact.clone());
        }
    }

    fn highlight(&mut self, fact: &Value) {
        self.graph.highlight_ids.push(text(fact.get("id")));
    }

    /// An edge the operation implies but does not list (a neighbour entry,
    /// a fact's relation, an insight's evidence).
    fn edge(&mut self, source: String, target: String, edge_type: Value) {
        let mut edge = DiagramItem::new();
        edge.insert("source_id".into(), Value::String(source));
        edge.insert("target_id".into(), Value::String(target));
        edge.insert("type".into(), edge_type);
        self.graph.edges.push(edge);
    }

    /// `[dict(edge) for edge in result.get("edges") or []]`.
    fn copy_edges(&mut self, edges: Option<&Value>) {
        self.graph.edges.extend(
            items(edges)
                .iter()
                .filter_map(|edge| edge.as_object().cloned()),
        );
    }
}

/// `value or []` as a list: the items of an array, else none.
fn items(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

/// `collect_diagram_graph`: the nodes, edges and highlights of any CODE
/// result, or `None` for a result that carries no graph (`delta`).
///
/// Each operation shapes its result differently; this is the one place that
/// knows those shapes, so the renderers stay format-only. The edges come out
/// each once, sorted by (source, target, type) whatever order the operation
/// listed them in.
pub fn collect_diagram_graph(result: &Value) -> Option<DiagramGraph> {
    let mut collector = Collector::default();
    let operation = result.get("operation").and_then(Value::as_str);
    match operation {
        Some("explore" | "traverse" | "architecture" | "subgraph") => {
            for fact in items(result.get("nodes")) {
                collector.add_node(Some(fact));
            }
            collector.copy_edges(result.get("edges"));
            if let Some(focus) = result.get("focus").filter(|focus| focus.is_object()) {
                collector.add_node(Some(focus));
                collector.highlight(focus);
            }
            for fact in items(result.get("nodes")) {
                if fact.is_object()
                    && fact
                        .get("depth")
                        .and_then(Value::as_f64)
                        .is_some_and(|depth| depth == 0.0)
                {
                    collector.highlight(fact);
                }
            }
        }
        Some("neighbors") => {
            // Each entry is one edge of the anchor, oriented by its direction.
            if let Some(anchor) = result.get("anchor").filter(|anchor| {
                anchor.is_object() && anchor.get("id").is_some_and(|id| !id.is_null())
            }) {
                collector.add_node(Some(anchor));
                collector.highlight(anchor);
                let anchor_id = text(anchor.get("id"));
                for entry in items(result.get("neighbors")) {
                    let Some(node) = entry.get("node").filter(|node| {
                        node.is_object() && node.get("id").is_some_and(|id| !id.is_null())
                    }) else {
                        continue;
                    };
                    collector.add_node(Some(node));
                    let node_id = text(node.get("id"));
                    let outgoing = entry.get("direction").and_then(Value::as_str) == Some("out");
                    let (source, target) = if outgoing {
                        (anchor_id.clone(), node_id)
                    } else {
                        (node_id, anchor_id.clone())
                    };
                    let relation = entry.get("relation").cloned().unwrap_or(Value::Null);
                    collector.edge(source, target, relation);
                }
            }
        }
        Some("find_path") => {
            for key in ["from", "to", "matched_to"] {
                collector.add_node(result.get(key));
            }
            for fact in items(result.get("path")) {
                collector.add_node(Some(fact));
                collector.highlight(fact);
            }
            collector.copy_edges(result.get("edges"));
        }
        Some("impact_analysis") => {
            for key in ["targets", "impact_seeds"] {
                for fact in items(result.get(key)) {
                    collector.add_node(Some(fact));
                    collector.highlight(fact);
                }
            }
            if let Some(Value::Object(by_depth)) = result.get("by_depth") {
                let mut depths: Vec<(&String, &Value)> = by_depth.iter().collect();
                depths.sort_by_key(|(depth, _)| depth.parse::<i64>().unwrap_or(i64::MAX));
                for (_, facts) in depths {
                    for fact in items(Some(facts)) {
                        collector.add_node(Some(fact));
                    }
                }
            }
            collector.copy_edges(result.get("edges"));
        }
        Some("query_facts") => {
            let facts = items(result.get("facts"));
            for fact in facts {
                collector.add_node(Some(fact));
            }
            for fact in facts {
                for relation in items(fact.get("relations")) {
                    let target_id = text(relation.get("target_id"));
                    if collector.seen.contains(&target_id) {
                        let edge_type = relation.get("type").cloned().unwrap_or(Value::Null);
                        collector.edge(text(fact.get("id")), target_id, edge_type);
                    }
                }
            }
        }
        Some("insights") => {
            for insight in items(result.get("insights")) {
                collector.add_node(Some(insight));
                collector.highlight(insight);
                for fact in items(insight.get("evidence")) {
                    collector.add_node(Some(fact));
                    collector.edge(
                        text(insight.get("id")),
                        text(fact.get("id")),
                        json!("evidences"),
                    );
                }
            }
        }
        _ => return None,
    }

    let mut graph = collector.graph;
    // A deterministic, deduplicated edge order however the operation listed
    // them: the first edge of each (source, target, type) is kept.
    let mut unique: BTreeMap<(String, String, String), DiagramItem> = BTreeMap::new();
    for edge in graph.edges.drain(..) {
        let key = (
            text(edge.get("source_id")),
            text(edge.get("target_id")),
            text(edge.get("type")),
        );
        unique.entry(key).or_insert(edge);
    }
    graph.edges = unique.into_values().collect();
    let mut seen = HashSet::new();
    graph
        .highlight_ids
        .retain(|node_id| seen.insert(node_id.clone()));
    Some(graph)
}

/// `_diagram_title`: what the diagram is of, e.g. `explore: pkg.run` or
/// `path: a -> b`; otherwise the operation's name.
fn diagram_title(result: &Value) -> Option<String> {
    let operation = first_truthy_text(&[result.get("operation")]);
    let names = |key: &str| -> Vec<String> {
        items(result.get(key))
            .iter()
            .map(|fact| text(fact.get("name")))
            .collect()
    };
    let title = match operation.as_str() {
        "explore" if result.get("focus").is_some_and(Value::is_object) => {
            format!("explore: {}", text(result["focus"].get("name")))
        }
        "neighbors" if result.get("anchor").is_some_and(Value::is_object) => {
            format!("neighbors: {}", text(result["anchor"].get("name")))
        }
        "subgraph" => {
            let names = names("seeds");
            if names.is_empty() {
                "subgraph".to_string()
            } else {
                format!("subgraph: {}", names.join(", "))
            }
        }
        "find_path" => {
            let name = |key: &str| text(result.get(key).and_then(|fact| fact.get("name")));
            format!("path: {} -> {}", name("from"), name("to"))
        }
        "impact_analysis" => {
            let names = names("targets");
            if names.is_empty() {
                "impact analysis".to_string()
            } else {
                format!("impact of {}", names.join(", "))
            }
        }
        "architecture" => {
            let repos: Vec<String> = items(result.get("repos")).iter().map(py_str).collect();
            if repos.is_empty() {
                "architecture".to_string()
            } else {
                format!("architecture: {}", repos.join(", "))
            }
        }
        "" => return None,
        _ => operation,
    };
    Some(title)
}

/// `render_result_diagram`: the `diagram` block attached to a CODE result —
/// its format, source and node and edge counts. A result with no graph
/// (`delta`) gets a null source and a note.
pub fn render_result_diagram(result: &Value, format: DiagramFormat) -> Value {
    let Some(graph) = collect_diagram_graph(result) else {
        return json!({
            "format": format.as_str(),
            "source": null,
            "nodes": 0,
            "edges": 0,
            "note": format!(
                "The {} operation carries no graph to draw.",
                py_repr(result.get("operation").unwrap_or(&Value::Null))
            ),
        });
    };
    let title = diagram_title(result);
    let render = match format {
        DiagramFormat::Mermaid => render_mermaid,
        DiagramFormat::Dot => render_dot,
    };
    json!({
        "format": format.as_str(),
        "source": render(&graph.nodes, &graph.edges, &graph.highlight_ids, title.as_deref()),
        "nodes": graph.nodes.len(),
        "edges": graph.edges.len(),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn item(value: Value) -> DiagramItem {
        value.as_object().cloned().expect("an object")
    }

    #[test]
    fn labels_are_escaped_for_both_formats() {
        // Python's test_diagram_labels_are_escaped_for_both_formats.
        let nodes = vec![
            item(
                json!({"id": "a", "kind": "route", "name": "GET /items?q=\"x\"<y>#1&2", "repo": "r"}),
            ),
            item(json!({"id": "b", "kind": "module", "name": ".", "repo": "r"})),
        ];
        let edges = vec![item(
            json!({"source_id": "a", "target_id": "b", "type": "declares", "count": 3}),
        )];
        let mermaid = render_mermaid(&nodes, &edges, &[], Some("a \"quoted\" # title: Vec<T>"));
        assert!(
            mermaid.starts_with(
                "---\ntitle: \"a \\\"quoted\\\" # title: Vec\u{2039}T\u{203a}\"\n---\n"
            )
        );
        assert!(mermaid.contains("n0>\"GET /items?q=#quot;x#quot;#lt;y#gt;#35;1#38;2\"]"));
        assert!(mermaid.contains("n1[[\"(root)\"]]"));
        assert!(mermaid.contains("n0 -- \"declares x3\" --> n1"));
        assert!(!mermaid.contains("classDef"));

        let dot = render_dot(&nodes, &edges, &["b".to_string()], None);
        assert!(dot.contains("n0 [label=\"GET /items?q=\\\"x\\\"<y>#1&2\" shape=cds];"));
        assert!(
            dot.contains("n1 [label=\"(root)\" shape=component color=\"#d64545\" penwidth=3];")
        );
        assert!(dot.contains("n0 -> n1 [label=\"declares x3\"];"));
    }

    #[test]
    fn a_count_of_one_or_a_non_integer_is_not_shown() {
        for count in [json!(1), json!(true), json!(2.0), json!("3"), Value::Null] {
            let edge = item(json!({"type": "calls", "count": count}));
            assert_eq!(edge_label(&edge), "calls", "{count}");
        }
        assert_eq!(
            edge_label(&item(json!({"type": "calls", "count": 2}))),
            "calls x2"
        );
        assert_eq!(edge_label(&item(json!({"count": 5}))), " x5");
    }

    #[test]
    fn a_result_without_a_graph_gets_a_note() {
        let diagram = render_result_diagram(&json!({"operation": "delta"}), DiagramFormat::Dot);
        assert_eq!(
            diagram,
            json!({
                "format": "dot",
                "source": null,
                "nodes": 0,
                "edges": 0,
                "note": "The 'delta' operation carries no graph to draw.",
            })
        );
        let diagram = render_result_diagram(&json!({}), DiagramFormat::Mermaid);
        assert_eq!(
            diagram["note"],
            json!("The None operation carries no graph to draw.")
        );
    }

    #[test]
    fn collected_edges_are_deduplicated_and_sorted() {
        let result = json!({
            "operation": "traverse",
            "nodes": [{"id": "b", "depth": 0}, {"id": "a", "depth": 1}, {"id": "b"}, "junk"],
            "edges": [
                {"source_id": "b", "target_id": "a", "type": "calls", "first": true},
                {"source_id": "a", "target_id": "b", "type": "calls"},
                {"source_id": "b", "target_id": "a", "type": "calls", "first": false},
            ],
        });
        let graph = collect_diagram_graph(&result).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.highlight_ids, vec!["b"]);
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.edges[0]["source_id"], "a");
        assert_eq!(graph.edges[1]["first"], true);
    }
}
