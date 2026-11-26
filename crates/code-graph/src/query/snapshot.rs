//! The in-memory indexes the CODE operations read: Python's
//! `_CodeGraphSnapshot` (`code_retriever.py:445-1039`).
//!
//! One snapshot is built from the dataset-scoped code subgraph the graph
//! adapter returns ([`load_code_graph_snapshot`]): every node of the fourteen
//! fact types plus `CodeRepository`, and the edges among them. Repository
//! nodes are held aside for `delta` and `capabilities`, flow nodes (Enola's
//! `flows.json`) for the flow operations; neither is a fact, so no fact index
//! or traversal sees them. Everything a later operation needs is indexed here
//! once, flows included, and the snapshot is immutable afterwards (the text
//! index is built on first use and kept), so it can be shared between
//! concurrent searches through the [cache](super::cache).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use cognee_graph::{EdgeData, GraphDBTrait, GraphNode};
use serde_json::{Map, Value, json};

use super::args::Direction;
use super::error::{CodeQueryError, CodeQueryResult};
use super::normalize::{json_key, normalize_properties, normalize_value, py_str_or, scalar_text};
use super::text_index::{CodeSearchIndex, DocumentFields, query_terms};
use crate::error::CodeGraphResult;
use crate::pyjson::{casefold, is_truthy, py_repr_str, py_round, py_str, py_strip};

/// One graph node's properties, as plain JSON.
pub type NodeProperties = Map<String, Value>;

/// The node types a CODE snapshot loads, besides `CodeRepository`
/// (`CODE_NODE_TYPES`).
pub const CODE_NODE_TYPES: [&str; 14] = [
    "ApiEndpoint",
    "CodeAssociation",
    "CodeExtractionAccount",
    "CodeInsight",
    "CodeIntent",
    "CodeLintFinding",
    "CodeModule",
    "CodeService",
    "CodeSymbol",
    "CodeTestReference",
    "CodeFileReference",
    "CodeFlow",
    "ExternalDependency",
    "StorageResource",
];

/// The repository node type, held aside from the facts.
pub const REPOSITORY_TYPE: &str = "CodeRepository";
/// The flow node type, held aside from the facts.
pub const FLOW_TYPE: &str = "CodeFlow";

/// `_KIND_BY_TYPE`: node type -> fact kind, for every type
/// [`CODE_NODE_TYPES`] lists.
pub fn kind_of_type(node_type: &str) -> Option<&'static str> {
    Some(match node_type {
        "ApiEndpoint" => "route",
        "CodeAssociation" => "association",
        "CodeExtractionAccount" => "extraction",
        "CodeInsight" => "insight",
        "CodeIntent" => "intent",
        "CodeLintFinding" => "lint",
        "CodeModule" => "module",
        "CodeService" => "service",
        "CodeSymbol" => "symbol",
        "CodeTestReference" => "test_ref",
        "CodeFileReference" => "file_ref",
        "CodeFlow" => "flow",
        "ExternalDependency" => "dependency",
        "StorageResource" => "storage",
        _ => return None,
    })
}

/// `_FACT_TYPE_BY_KIND`, sorted by kind: the kinds a `kinds` / `node_types`
/// filter accepts. Flows are not facts, so no fact-shaped operation selects
/// them.
pub const FACT_TYPE_BY_KIND: [(&str, &str); 13] = [
    ("association", "CodeAssociation"),
    ("dependency", "ExternalDependency"),
    ("extraction", "CodeExtractionAccount"),
    ("file_ref", "CodeFileReference"),
    ("insight", "CodeInsight"),
    ("intent", "CodeIntent"),
    ("lint", "CodeLintFinding"),
    ("module", "CodeModule"),
    ("route", "ApiEndpoint"),
    ("service", "CodeService"),
    ("storage", "StorageResource"),
    ("symbol", "CodeSymbol"),
    ("test_ref", "CodeTestReference"),
];

/// `_DECLARATION_VIEW_TYPES`: the facts a framework extractor derives from a
/// declaration and names after it (an ORM model's storage, a relation
/// method's association).
const DECLARATION_VIEW_TYPES: [&str; 2] = ["StorageResource", "CodeAssociation"];
/// `_INTERNAL_EDGE_PROPERTIES`: edge properties the adapter keeps for itself.
const INTERNAL_EDGE_PROPERTIES: [&str; 7] = [
    "source_node_id",
    "target_node_id",
    "relationship_name",
    "relationship_type",
    "edge_text",
    "created_at",
    "updated_at",
];
/// `_TEST_PATH_SEGMENTS`.
const TEST_PATH_SEGMENTS: [&str; 5] = ["test", "tests", "__tests__", "spec", "testdata"];
/// `_CODE_EXTENSIONS`: a short name drops these file extensions.
const CODE_EXTENSIONS: [&str; 23] = [
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx", "kt", "kts", "m", "mjs", "mm",
    "php", "py", "rb", "rs", "scala", "swift", "ts", "tsx",
];
/// `_EXACT_NAME_BONUS`: the score a text match gains when the whole text is
/// the fact's name (tier 3) or its short name or qualified suffix (tier 2).
fn exact_name_bonus(tier: i32) -> Option<f64> {
    match tier {
        3 => Some(5.0),
        2 => Some(2.5),
        _ => None,
    }
}

/// `_is_test_path`: whether a path looks like test code — a test directory
/// segment, or a test file name (`_TEST_FILE_NAME`, matched in full).
pub fn is_test_path(file_path: &str) -> bool {
    let normalized = file_path.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let Some(last) = parts.last() else {
        return false;
    };
    if parts
        .iter()
        .any(|part| TEST_PATH_SEGMENTS.contains(&part.to_lowercase().as_str()))
    {
        return true;
    }
    is_test_file_name(last)
}

/// `_TEST_FILE_NAME.fullmatch(name)`: `test_*.py`, `*_test.py`, `*_test.go`,
/// `*.test|spec.js|jsx|ts|tsx`, `*Test.java`, `*Tests.cs`, `*_spec.rb`. The
/// regex's `.` matches no newline, so a name holding one never matches.
fn is_test_file_name(name: &str) -> bool {
    if name.contains('\n') {
        return false;
    }
    if name.len() >= "test_.py".len() && name.starts_with("test_") && name.ends_with(".py") {
        return true;
    }
    const SUFFIXES: [&str; 13] = [
        "_test.py",
        "_test.go",
        ".test.js",
        ".test.jsx",
        ".test.ts",
        ".test.tsx",
        ".spec.js",
        ".spec.jsx",
        ".spec.ts",
        ".spec.tsx",
        "Test.java",
        "Tests.cs",
        "_spec.rb",
    ];
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// `_short_names`: the shorter forms a qualified name is also known by,
/// casefolded — the part after the last `.`, the part after the last `/`,
/// and that basename without a code file extension.
pub fn short_names(name: &str) -> BTreeSet<String> {
    let lowered = casefold(name);
    let mut forms = BTreeSet::new();
    if let Some((_, last)) = lowered.rsplit_once('.') {
        forms.insert(last.to_string());
    }
    let basename = lowered.rsplit('/').next().unwrap_or(&lowered).to_string();
    if basename != lowered {
        forms.insert(basename.clone());
    }
    if let Some((stem, extension)) = basename.rsplit_once('.')
        && CODE_EXTENSIONS.contains(&extension)
    {
        forms.insert(stem.to_string());
    }
    forms
}

/// `_match_tier`: how well `name` matches the casefolded `lowered_term`. 3 is
/// the exact name, 2 a short name or a qualified suffix (after `.` or `/`), 1
/// a substring, -1 no match.
pub fn match_tier(name: &str, lowered_term: &str) -> i32 {
    let lowered_name = casefold(name);
    if !lowered_name.contains(lowered_term) {
        return -1;
    }
    if lowered_name == lowered_term {
        return 3;
    }
    if short_names(name).contains(lowered_term) {
        return 2;
    }
    if let Some(prefix) = lowered_name.strip_suffix(lowered_term)
        && let Some(boundary) = prefix.chars().last()
        && matches!(boundary, '.' | '/')
    {
        return 2;
    }
    1
}

/// The stable order of fact nodes (`node_sort_key`): repo, type, casefolded
/// name, file, line, id — each as Python's `str()` of the field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeSortKey([String; 6]);

/// One normalized, deduplicated edge between two fact nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotEdge {
    /// The source node id.
    pub source_id: String,
    /// The target node id.
    pub target_id: String,
    /// The relationship name.
    pub relation: String,
    /// The normalized edge properties.
    pub properties: NodeProperties,
}

/// The fact nodes a flow's entry, stages and routes resolve to, aligned with
/// the refs in its properties; `None` where a ref names no node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowMembers {
    /// The entry point.
    pub entry: Option<String>,
    /// The stages, in the flow's order.
    pub stages: Vec<Option<String>>,
    /// The routes.
    pub routes: Vec<Option<String>>,
}

/// The part a node plays in a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowRole {
    /// The flow's entry point.
    Entry,
    /// One of its stages.
    Stage,
    /// One of its routes.
    Route,
}

impl FlowRole {
    /// The wire form: `entry`, `stage` or `route`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Stage => "stage",
            Self::Route => "route",
        }
    }
}

/// The text-search structures, built on first use (`_CodeTextIndex`).
#[derive(Debug, Clone)]
pub struct CodeTextIndex {
    /// BM25 over names, docstrings, paths and flow summaries.
    pub bm25: CodeSearchIndex,
    /// Casefolded full names and their short forms -> node and flow ids, so
    /// an exact or short-name match is found even when every query token is
    /// a stopword.
    pub name_forms: HashMap<String, Vec<String>>,
}

/// One ranked text match: the node or flow id, its rounded score and the
/// fields that matched.
pub type TextMatch = (String, f64, Vec<&'static str>);

/// Stable in-memory indexes and adjacency over one dataset-scoped code graph
/// (`_CodeGraphSnapshot`).
#[derive(Debug)]
pub struct CodeGraphSnapshot {
    /// Fact nodes by id.
    pub nodes: HashMap<String, NodeProperties>,
    /// Repository nodes by id: not facts, read by `delta` and
    /// `capabilities`.
    pub repositories: BTreeMap<String, NodeProperties>,
    /// Flow nodes by id: not facts, read through the flow indexes.
    pub flows: HashMap<String, NodeProperties>,
    /// Every edge between two facts, deduplicated, sorted by source, target,
    /// type and properties.
    pub edges: Vec<SnapshotEdge>,
    forward: HashMap<String, Vec<usize>>,
    reverse: HashMap<String, Vec<usize>>,
    sort_keys: HashMap<String, NodeSortKey>,
    /// Node ids by type.
    pub by_type: HashMap<String, HashSet<String>>,
    /// Node ids by name.
    pub by_name: HashMap<String, HashSet<String>>,
    /// Node ids by file.
    pub by_file: HashMap<String, HashSet<String>>,
    /// Node ids by repository.
    pub by_repo: HashMap<String, HashSet<String>>,
    /// Enola's own fact id (32 hex chars) -> node id. Several enola ids may
    /// collapse onto one node; the node keeps the first.
    pub by_enola_id: HashMap<String, String>,
    /// Property name (dotted for nested objects) -> scalar text -> node ids.
    pub by_property: HashMap<String, HashMap<String, HashSet<String>>>,
    /// Storage and association views -> the `CodeSymbol` each was derived
    /// from.
    pub declaration_of: HashMap<String, String>,
    /// Facts with a docstring.
    pub documented: HashSet<String>,
    /// Facts in test code ([`is_test_path`], judged once per file).
    pub test_nodes: HashSet<String>,
    /// Enola flow id -> flow node id.
    pub flow_by_enola_id: HashMap<String, String>,
    /// The fact nodes each flow's refs resolve to.
    pub flow_members: HashMap<String, FlowMembers>,
    /// Node id -> the (flow id, role) pairs it takes part in.
    pub flows_by_node: HashMap<String, Vec<(String, FlowRole)>>,
    /// Flow ids in Enola's ranking (rank, then name).
    pub flow_order: Vec<String>,
    text_index: OnceLock<CodeTextIndex>,
}

impl CodeGraphSnapshot {
    /// Build the snapshot from raw adapter rows: `(id, properties)` nodes and
    /// `(source, target, relationship, properties)` edges. Nodes of other
    /// types and edges touching them are ignored.
    pub fn new(raw_nodes: Vec<GraphNode>, raw_edges: Vec<EdgeData>) -> Self {
        let mut nodes = HashMap::new();
        let mut repositories = BTreeMap::new();
        let mut flows = HashMap::new();
        for (node_id, raw_properties) in raw_nodes {
            let raw: Map<String, Value> = raw_properties
                .into_iter()
                .map(|(key, value)| (key.into_owned(), value))
                .collect();
            let mut properties = normalize_properties(&Value::Object(raw));
            // Every node written since docs were read has a doc field, null
            // when undocumented. Drop it so `doc` is present exactly on
            // documented facts and the property filters match only those.
            if let Some(doc) = properties.get("doc")
                && !matches!(doc, Value::String(text) if !py_strip(text).is_empty())
            {
                properties.remove("doc");
            }
            let node_type = properties.get("type").cloned();
            let node_type = node_type.as_ref().and_then(Value::as_str);
            properties.insert("id".to_string(), Value::String(node_id.clone()));
            match node_type {
                Some(REPOSITORY_TYPE) => {
                    repositories.insert(node_id, properties);
                }
                Some(FLOW_TYPE) => {
                    flows.insert(node_id, properties);
                }
                Some(other) if CODE_NODE_TYPES.contains(&other) => {
                    nodes.insert(node_id, properties);
                }
                _ => {}
            }
        }

        let mut normalized_edges = Vec::new();
        let mut flow_edges: BTreeSet<(String, String, String)> = BTreeSet::new();
        for (source_id, target_id, relation, raw_properties) in raw_edges {
            if flows.contains_key(&source_id) && nodes.contains_key(&target_id) {
                flow_edges.insert((source_id, relation, target_id));
                continue;
            }
            if !nodes.contains_key(&source_id) || !nodes.contains_key(&target_id) {
                continue;
            }
            let raw: Map<String, Value> = raw_properties
                .into_iter()
                .map(|(key, value)| (key.into_owned(), value))
                .collect();
            let properties = normalize_properties(&Value::Object(raw));
            let sort_text = json_key(&Value::Object(properties.clone()));
            normalized_edges.push((
                sort_text,
                SnapshotEdge {
                    source_id,
                    target_id,
                    relation,
                    properties,
                },
            ));
        }
        normalized_edges.sort_by(|(left_text, left), (right_text, right)| {
            (&left.source_id, &left.target_id, &left.relation, left_text).cmp(&(
                &right.source_id,
                &right.target_id,
                &right.relation,
                right_text,
            ))
        });
        let mut edges = Vec::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (_, edge) in normalized_edges {
            let identity = (
                edge.source_id.clone(),
                edge.target_id.clone(),
                edge.relation.clone(),
            );
            if seen.insert(identity) {
                edges.push(edge);
            }
        }

        let sort_keys: HashMap<String, NodeSortKey> = nodes
            .iter()
            .map(|(node_id, node)| (node_id.clone(), node_sort_key(node_id, node)))
            .collect();

        let mut snapshot = Self {
            nodes,
            repositories,
            flows,
            edges,
            forward: HashMap::new(),
            reverse: HashMap::new(),
            sort_keys,
            by_type: HashMap::new(),
            by_name: HashMap::new(),
            by_file: HashMap::new(),
            by_repo: HashMap::new(),
            by_enola_id: HashMap::new(),
            by_property: HashMap::new(),
            declaration_of: HashMap::new(),
            documented: HashSet::new(),
            test_nodes: HashSet::new(),
            flow_by_enola_id: HashMap::new(),
            flow_members: HashMap::new(),
            flows_by_node: HashMap::new(),
            flow_order: Vec::new(),
            text_index: OnceLock::new(),
        };
        snapshot.index_adjacency();
        snapshot.index_facts();
        snapshot.declaration_of = snapshot.index_declarations();
        snapshot.documented = snapshot
            .nodes
            .iter()
            .filter(|(_, node)| doc_of(node).is_some())
            .map(|(node_id, _)| node_id.clone())
            .collect();
        let mut test_nodes: HashSet<String> = snapshot
            .by_type
            .get("CodeTestReference")
            .cloned()
            .unwrap_or_default();
        for (file_path, node_ids) in &snapshot.by_file {
            if is_test_path(file_path) {
                test_nodes.extend(node_ids.iter().cloned());
            }
        }
        snapshot.test_nodes = test_nodes;
        snapshot.index_flows(flow_edges);
        snapshot
    }

    /// An empty snapshot: a dataset without a code graph.
    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new())
    }

    fn index_adjacency(&mut self) {
        let mut forward: HashMap<String, Vec<usize>> = HashMap::new();
        let mut reverse: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, edge) in self.edges.iter().enumerate() {
            forward
                .entry(edge.source_id.clone())
                .or_default()
                .push(position);
            reverse
                .entry(edge.target_id.clone())
                .or_default()
                .push(position);
        }
        for adjacency in [&mut forward, &mut reverse] {
            for positions in adjacency.values_mut() {
                positions.sort_by(|left, right| {
                    self.edge_order(&self.edges[*left], &self.edges[*right])
                });
            }
        }
        self.forward = forward;
        self.reverse = reverse;
    }

    fn index_facts(&mut self) {
        let mut ordered: Vec<&String> = self.nodes.keys().collect();
        ordered.sort_by(|left, right| self.sort_keys[*left].cmp(&self.sort_keys[*right]));
        let mut by_type: HashMap<String, HashSet<String>> = HashMap::new();
        let mut by_name: HashMap<String, HashSet<String>> = HashMap::new();
        let mut by_file: HashMap<String, HashSet<String>> = HashMap::new();
        let mut by_repo: HashMap<String, HashSet<String>> = HashMap::new();
        let mut by_enola_id: HashMap<String, String> = HashMap::new();
        let mut by_property: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();
        for node_id in ordered {
            let node = &self.nodes[node_id];
            by_type
                .entry(py_str_or(node.get("type"), ""))
                .or_default()
                .insert(node_id.clone());
            by_name
                .entry(py_str_or(node.get("name"), ""))
                .or_default()
                .insert(node_id.clone());
            by_file
                .entry(file_of(node))
                .or_default()
                .insert(node_id.clone());
            by_repo
                .entry(py_str_or(node.get("repo"), ""))
                .or_default()
                .insert(node_id.clone());
            if let Some(Value::String(enola_id)) = node.get("enola_id")
                && !enola_id.is_empty()
            {
                by_enola_id
                    .entry(enola_id.clone())
                    .or_insert_with(|| node_id.clone());
            }
            for (key, value) in node {
                if key != "fact_properties" {
                    index_property(&mut by_property, node_id, key, value);
                }
            }
            for (key, value) in properties_of(node) {
                index_property(&mut by_property, node_id, key, value);
            }
        }
        self.by_type = by_type;
        self.by_name = by_name;
        self.by_file = by_file;
        self.by_repo = by_repo;
        self.by_enola_id = by_enola_id;
        self.by_property = by_property;
    }

    /// `_index_declarations`: storage and association views whose name, repo
    /// and file match exactly one `CodeSymbol` -> that symbol.
    fn index_declarations(&self) -> HashMap<String, String> {
        let mut declarations = HashMap::new();
        for node_type in DECLARATION_VIEW_TYPES {
            for view_id in self.by_type.get(node_type).into_iter().flatten() {
                let view = &self.nodes[view_id];
                let file_path = file_of(view);
                let name = py_str_or(view.get("name"), "");
                let symbols: Vec<&String> = self
                    .by_name
                    .get(&name)
                    .into_iter()
                    .flatten()
                    .filter(|node_id| {
                        let node = &self.nodes[*node_id];
                        node.get("type").and_then(Value::as_str) == Some("CodeSymbol")
                            && node.get("repo") == view.get("repo")
                            && file_of(node) == file_path
                    })
                    .collect();
                if !file_path.is_empty()
                    && let [symbol] = symbols.as_slice()
                {
                    declarations.insert(view_id.clone(), (*symbol).clone());
                }
            }
        }
        declarations
    }

    /// `_index_flows`: resolve each flow's entry, stages and routes to fact
    /// nodes, both ways.
    fn index_flows(&mut self, flow_edges: BTreeSet<(String, String, String)>) {
        let mut edge_targets: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
        for (source_id, relation, target_id) in flow_edges {
            edge_targets
                .entry(source_id)
                .or_default()
                .entry(relation)
                .or_default()
                .push(target_id);
        }
        let mut flow_order: Vec<String> = self.flows.keys().cloned().collect();
        flow_order.sort_by(|left, right| self.flow_rank_order(left, right));

        let empty = Vec::new();
        let mut flow_by_enola_id = HashMap::new();
        let mut flow_members = HashMap::new();
        let mut flows_by_node: HashMap<String, Vec<(String, FlowRole)>> = HashMap::new();
        for flow_id in &flow_order {
            let flow = &self.flows[flow_id];
            if let Some(Value::String(enola_id)) = flow.get("enola_id")
                && !enola_id.is_empty()
            {
                flow_by_enola_id
                    .entry(enola_id.clone())
                    .or_insert_with(|| flow_id.clone());
            }
            let properties = properties_of(flow);
            let targets = edge_targets.get(flow_id);
            let targets_of = |relation: &str| -> &Vec<String> {
                targets
                    .and_then(|targets| targets.get(relation))
                    .unwrap_or(&empty)
            };
            let entry = self.flow_ref_node(properties.get("entry"), targets_of("flow_entry"));
            let stages: Vec<Option<String>> = mappings(properties.get("stages"))
                .into_iter()
                .map(|stage| self.flow_ref_node(Some(stage), targets_of("flow_stage")))
                .collect();
            let routes: Vec<Option<String>> = mappings(properties.get("routes"))
                .into_iter()
                .map(|route| self.flow_ref_node(Some(route), targets_of("flow_route")))
                .collect();
            for (role, node_ids) in [
                (FlowRole::Entry, std::slice::from_ref(&entry)),
                (FlowRole::Stage, stages.as_slice()),
                (FlowRole::Route, routes.as_slice()),
            ] {
                let mut seen = HashSet::new();
                for node_id in node_ids.iter().flatten() {
                    if seen.insert(node_id) {
                        flows_by_node
                            .entry(node_id.clone())
                            .or_default()
                            .push((flow_id.clone(), role));
                    }
                }
            }
            flow_members.insert(
                flow_id.clone(),
                FlowMembers {
                    entry,
                    stages,
                    routes,
                },
            );
        }
        self.flow_by_enola_id = flow_by_enola_id;
        self.flow_members = flow_members;
        self.flows_by_node = flows_by_node;
        self.flow_order = flow_order;
    }

    /// `_flow_ref_node`: the fact node an Enola ref names — by its fact id,
    /// else by the flow's own edge to a node of the same name.
    fn flow_ref_node(&self, reference: Option<&Value>, edge_targets: &[String]) -> Option<String> {
        let Some(Value::Object(reference)) = reference else {
            return None;
        };
        if let Some(Value::String(enola_id)) = reference.get("id")
            && let Some(node_id) = self.by_enola_id.get(enola_id)
        {
            return Some(node_id.clone());
        }
        edge_targets
            .iter()
            .find(|node_id| {
                self.nodes
                    .get(*node_id)
                    .is_some_and(|node| node.get("name") == reference.get("name"))
            })
            .cloned()
    }

    /// The node's stable sort key ([`NodeSortKey`]).
    ///
    /// # Panics
    ///
    /// Never for an id of [`Self::nodes`]; another id is a caller bug.
    pub fn node_sort_key(&self, node_id: &str) -> &NodeSortKey {
        &self.sort_keys[node_id]
    }

    /// Compare two fact ids in the stable node order.
    pub fn node_order(&self, left: &str, right: &str) -> Ordering {
        self.sort_keys[left].cmp(&self.sort_keys[right])
    }

    /// `entity_sort_key`: [`Self::node_sort_key`] for facts and flows alike.
    pub fn entity_sort_key(&self, key: &str) -> NodeSortKey {
        if let Some(sort_key) = self.sort_keys.get(key) {
            return sort_key.clone();
        }
        let flow = self.flows.get(key);
        let field = |name: &str| py_str_or(flow.and_then(|flow| flow.get(name)), "");
        NodeSortKey([
            field("repo"),
            FLOW_TYPE.to_string(),
            casefold(&field("name")),
            flow.map(file_of).unwrap_or_default(),
            field("line"),
            key.to_string(),
        ])
    }

    /// `flow_rank_key` as an ordering: rank (none last), casefolded name,
    /// repo, id.
    pub fn flow_rank_order(&self, left: &str, right: &str) -> Ordering {
        let key = |flow_id: &str| {
            let flow = &self.flows[flow_id];
            let rank = int_or_none(properties_of(flow).get("rank"));
            let text = |name: &str| match flow.get(name) {
                Some(value) if is_truthy(value) => py_str(value),
                _ => String::new(),
            };
            (
                rank.is_none(),
                rank.unwrap_or(0),
                casefold(&text("name")),
                text("repo"),
                flow_id.to_string(),
            )
        };
        key(left).cmp(&key(right))
    }

    /// `edge_sort_key` as an ordering: type, then the source's and the
    /// target's node order.
    pub fn edge_order(&self, left: &SnapshotEdge, right: &SnapshotEdge) -> Ordering {
        left.relation
            .cmp(&right.relation)
            .then_with(|| self.node_order(&left.source_id, &right.source_id))
            .then_with(|| self.node_order(&left.target_id, &right.target_id))
    }

    /// The edges leaving `node_id`, in edge order.
    pub fn forward(&self, node_id: &str) -> impl Iterator<Item = &SnapshotEdge> {
        self.forward
            .get(node_id)
            .into_iter()
            .flatten()
            .map(|position| &self.edges[*position])
    }

    /// The edges entering `node_id`, in edge order.
    pub fn reverse(&self, node_id: &str) -> impl Iterator<Item = &SnapshotEdge> {
        self.reverse
            .get(node_id)
            .into_iter()
            .flatten()
            .map(|position| &self.edges[*position])
    }

    /// `adjacent`: the (neighbour, edge) pairs of `node_id` in `direction`,
    /// keeping only `relation_types` when given, ordered by the neighbour's
    /// node order and then the edge order (a stable sort, so an edge seen in
    /// both directions — a self loop — is listed twice).
    pub fn adjacent(
        &self,
        node_id: &str,
        direction: Direction,
        relation_types: Option<&HashSet<String>>,
    ) -> Vec<(&String, &SnapshotEdge)> {
        let mut adjacent: Vec<(&String, &SnapshotEdge)> = Vec::new();
        if direction.forward() {
            adjacent.extend(self.forward(node_id).map(|edge| (&edge.target_id, edge)));
        }
        if direction.reverse() {
            adjacent.extend(self.reverse(node_id).map(|edge| (&edge.source_id, edge)));
        }
        if let Some(relation_types) = relation_types {
            adjacent.retain(|(_, edge)| relation_types.contains(&edge.relation));
        }
        adjacent.sort_by(|left, right| {
            self.node_order(left.0, right.0)
                .then_with(|| self.edge_order(left.1, right.1))
        });
        adjacent
    }

    /// A fact node or a flow node, by id (`entity`).
    pub fn entity(&self, key: &str) -> Option<&NodeProperties> {
        self.nodes.get(key).or_else(|| self.flows.get(key))
    }

    /// Whether a fact or flow is test code (`is_test`).
    pub fn is_test(&self, key: &str) -> bool {
        if self.nodes.contains_key(key) {
            return self.test_nodes.contains(key);
        }
        self.flows
            .get(key)
            .is_some_and(|flow| is_test_path(&file_of(flow)))
    }

    /// `edge_payload`: an edge as a result lists it, with its public
    /// properties.
    pub fn edge_payload(&self, edge: &SnapshotEdge) -> Value {
        let name_of = |node_id: &str| {
            self.nodes
                .get(node_id)
                .and_then(|node| node.get("name"))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let mut result = Map::new();
        result.insert("source_id".into(), json!(edge.source_id));
        result.insert("source".into(), name_of(&edge.source_id));
        result.insert("target_id".into(), json!(edge.target_id));
        result.insert("target".into(), name_of(&edge.target_id));
        result.insert("type".into(), json!(edge.relation));
        let public: Map<String, Value> = edge
            .properties
            .iter()
            .filter(|(key, _)| !INTERNAL_EDGE_PROPERTIES.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !public.is_empty() {
            result.insert("properties".into(), normalize_value(&Value::Object(public)));
        }
        Value::Object(result)
    }

    /// `fact`: a fact node as a result lists it. `depth` is added when given;
    /// `include_relations` lists its outgoing edges.
    pub fn fact(&self, node_id: &str, depth: Option<usize>, include_relations: bool) -> Value {
        let Some(node) = self.nodes.get(node_id) else {
            return Value::Null;
        };
        let node_type = node.get("type").cloned().unwrap_or(Value::Null);
        let kind = node_type
            .as_str()
            .and_then(kind_of_type)
            .unwrap_or_default();
        let mut result = Map::new();
        result.insert("id".into(), json!(node_id));
        result.insert("kind".into(), json!(kind));
        result.insert("type".into(), node_type);
        result.insert(
            "name".into(),
            node.get("name").cloned().unwrap_or(Value::Null),
        );
        let file = Value::String(file_of(node));
        for (key, value) in [
            ("file", Some(&file)),
            ("line", node.get("line")),
            ("end_line", node.get("end_line")),
            ("repo", node.get("repo")),
            ("path", node.get("path")),
            ("description", node.get("description")),
            ("symbol_kind", node.get("symbol_kind")),
            ("enola_id", node.get("enola_id")),
        ] {
            if let Some(value) = value
                && !is_none_or_empty(value)
            {
                result.insert(key.into(), value.clone());
            }
        }
        if let Some(doc) = doc_of(node) {
            result.insert("doc".into(), json!(doc));
        }
        let properties = properties_of(node);
        if !properties.is_empty() {
            result.insert(
                "properties".into(),
                normalize_value(&Value::Object(properties.clone())),
            );
        }
        if include_relations {
            let relations: Vec<Value> = self
                .forward(node_id)
                .map(|edge| {
                    json!({
                        "type": edge.relation,
                        "target_id": edge.target_id,
                        "target": self.nodes.get(&edge.target_id)
                            .and_then(|target| target.get("name"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    })
                })
                .collect();
            if !relations.is_empty() {
                result.insert("relations".into(), Value::Array(relations));
            }
        }
        if let Some(depth) = depth {
            result.insert("depth".into(), json!(depth));
        }
        Value::Object(result)
    }

    /// `resolve`: one node id from an id (a cognee id or an enola fact id) or
    /// a name, optionally within a repository. `role` names the seed in the
    /// messages ("neighbors anchor", "subgraph seed").
    ///
    /// # Errors
    ///
    /// `not_found` for an id or name that matches nothing (or an id outside
    /// `repo`), `ambiguous` for a name that matches several nodes equally
    /// well, and `invalid_argument` when neither an id nor a non-blank name is
    /// given.
    pub fn resolve(
        &self,
        name: Option<&str>,
        node_id: Option<&str>,
        repo: Option<&Value>,
        role: &str,
    ) -> CodeQueryResult<String> {
        let repo = repo.filter(|repo| is_truthy(repo));
        if let Some(node_id) = node_id.filter(|node_id| !node_id.is_empty()) {
            let mut normalized = node_id.to_string();
            if !self.nodes.contains_key(&normalized)
                && let Some(mapped) = self.by_enola_id.get(&normalized)
            {
                normalized = mapped.clone();
            }
            let Some(node) = self.nodes.get(&normalized) else {
                return Err(CodeQueryError::not_found(
                    format!(
                        "CODE could not resolve {role} id {}.",
                        py_repr_str(&normalized)
                    ),
                    "id",
                ));
            };
            if let Some(repo) = repo
                && node.get("repo") != Some(repo)
            {
                return Err(CodeQueryError::not_found(
                    format!(
                        "CODE {role} id {} is not in repo {}.",
                        py_repr_str(&normalized),
                        crate::pyjson::py_repr(repo)
                    ),
                    "id",
                ));
            }
            return Ok(normalized);
        }

        let term = py_strip(name.unwrap_or_default());
        if term.is_empty() {
            return Err(CodeQueryError::invalid(
                format!("CODE {role} requires a non-empty name or id."),
                "name",
            ));
        }
        let best = self.name_matches(term, repo);
        match best.as_slice() {
            [] => Err(CodeQueryError::not_found(
                format!("CODE could not resolve {role} name {}.", py_repr_str(term)),
                "name",
            )),
            [only] => Ok(only.clone()),
            _ => {
                // One declaration seen twice (a class or method symbol plus
                // the storage or association view named after it) is one
                // candidate: the symbol.
                let matched: HashSet<&String> = best.iter().collect();
                let rivals: Vec<&String> = best
                    .iter()
                    .filter(|item| {
                        !self
                            .declaration_of
                            .get(*item)
                            .is_some_and(|declaration| matched.contains(declaration))
                    })
                    .collect();
                if let [only] = rivals.as_slice() {
                    return Ok((*only).clone());
                }
                let choices: Vec<String> = best
                    .iter()
                    .take(10)
                    .map(|item| {
                        let node = &self.nodes[item];
                        let repo = match node.get("repo") {
                            Some(repo) if is_truthy(repo) => py_str(repo),
                            _ => "-".to_string(),
                        };
                        format!(
                            "{} [{}, repo={repo}, id={item}]",
                            py_str_or(node.get("name"), "None"),
                            py_str_or(node.get("type"), "None"),
                        )
                    })
                    .collect();
                Err(CodeQueryError::ambiguous(
                    format!(
                        "CODE {role} name {} is ambiguous; use an exact id or repo. Candidates: {}",
                        py_repr_str(term),
                        choices.join(", ")
                    ),
                    "name",
                ))
            }
        }
    }

    /// `name_matches`: the nodes whose name matches `term` best (exact, else
    /// short name, else substring), in node order.
    pub fn name_matches(&self, term: &str, repo: Option<&Value>) -> Vec<String> {
        let repo = repo.filter(|repo| is_truthy(repo));
        let lowered = casefold(py_strip(term));
        let mut ranked: Vec<(i32, &String)> = Vec::new();
        for (candidate_id, node) in &self.nodes {
            if let Some(repo) = repo
                && node.get("repo") != Some(repo)
            {
                continue;
            }
            let name = match node.get("name") {
                Some(name) if is_truthy(name) => py_str(name),
                _ => String::new(),
            };
            let tier = match_tier(&name, &lowered);
            if tier >= 0 {
                ranked.push((tier, candidate_id));
            }
        }
        let Some(best_tier) = ranked.iter().map(|(tier, _)| *tier).max() else {
            return Vec::new();
        };
        let mut best: Vec<String> = ranked
            .into_iter()
            .filter(|(tier, _)| *tier == best_tier)
            .map(|(_, candidate_id)| candidate_id.clone())
            .collect();
        best.sort_by(|left, right| self.node_order(left, right));
        best
    }

    /// The text-search index, built on first use and kept for the snapshot's
    /// lifetime (`text_index`).
    pub fn text_index(&self) -> &CodeTextIndex {
        self.text_index.get_or_init(|| self.build_text_index())
    }

    fn build_text_index(&self) -> CodeTextIndex {
        let mut keys: Vec<&String> = self.nodes.keys().collect();
        keys.sort_by(|left, right| self.node_order(left, right));
        let mut flow_keys: Vec<&String> = self.flows.keys().collect();
        flow_keys.sort_by_cached_key(|key| self.entity_sort_key(key));
        keys.extend(flow_keys);

        let mut documents = Vec::with_capacity(keys.len());
        let mut name_forms: HashMap<String, BTreeSet<String>> = HashMap::new();
        for key in keys {
            let Some(entity) = self.entity(key) else {
                continue;
            };
            let name = match entity.get("name") {
                Some(name) if is_truthy(name) => py_str(name),
                _ => String::new(),
            };
            let summary = if self.flows.contains_key(key) {
                self.flow_summary_text(key)
            } else {
                None
            };
            documents.push((
                key.clone(),
                DocumentFields {
                    name: Some(name.clone()),
                    doc: doc_of(entity),
                    file: Some(file_of(entity)),
                    summary,
                },
            ));
            if !name.is_empty() {
                name_forms
                    .entry(casefold(&name))
                    .or_default()
                    .insert(key.clone());
                for form in short_names(&name) {
                    name_forms.entry(form).or_default().insert(key.clone());
                }
            }
        }
        CodeTextIndex {
            bm25: CodeSearchIndex::new(documents),
            name_forms: name_forms
                .into_iter()
                .map(|(form, found)| (form, found.into_iter().collect()))
                .collect(),
        }
    }

    /// `flow_summary_text`: a flow's summary prop, else its description.
    pub fn flow_summary_text(&self, flow_id: &str) -> Option<String> {
        let flow = self.flows.get(flow_id)?;
        let summary = match properties_of(flow).get("summary") {
            Some(summary) if is_truthy(summary) => Some(summary),
            _ => flow.get("description"),
        };
        match summary {
            Some(Value::String(text)) => Some(text.clone()),
            _ => None,
        }
    }

    /// `_text_matches`: rank facts and flows against `text` — BM25 over
    /// name, doc, file and flow summary, plus the exact-name bonus — ordered
    /// by score, then the stable node order. Returns the query terms and the
    /// ranked `(id, score, matched fields)`. `kinds` (node types, `None` for
    /// every type), `repo`, `file_prefix` and `include_tests` filter; `limit`
    /// keeps only the best that many (ties at the cut stay in until the
    /// stable order picks among them).
    pub fn text_matches(
        &self,
        text: &str,
        kinds: Option<&HashSet<String>>,
        repo: Option<&Value>,
        file_prefix: &str,
        include_tests: bool,
        limit: Option<usize>,
    ) -> (Vec<String>, Vec<TextMatch>) {
        let repo = repo.filter(|repo| is_truthy(repo));
        let terms = query_terms(text);
        let index = self.text_index();
        let matches = index.bm25.search(&terms);
        let lowered = casefold(py_strip(text));
        let mut candidates: BTreeSet<&String> = matches.keys().collect();
        if let Some(found) = index.name_forms.get(&lowered) {
            candidates.extend(found.iter());
        }
        let mut hits: Vec<(&String, &NodeProperties)> = Vec::new();
        for key in candidates {
            let Some(entity) = self.entity(key) else {
                continue;
            };
            let entity_type = py_str_or(entity.get("type"), "None");
            if kinds.is_some_and(|kinds| !kinds.contains(&entity_type)) {
                continue;
            }
            if let Some(repo) = repo
                && entity.get("repo") != Some(repo)
            {
                continue;
            }
            if !file_prefix.is_empty() && !file_of(entity).starts_with(file_prefix) {
                continue;
            }
            if !include_tests && self.is_test(key) {
                continue;
            }
            hits.push((key, entity));
        }
        // A storage or association view named after its declaration leaves
        // the name bonus to that declaration when both are hits.
        let hit_set: HashSet<&String> = hits.iter().map(|(key, _)| *key).collect();
        let mut ranked: Vec<TextMatch> = Vec::new();
        for (key, entity) in hits {
            let found = matches.get(key);
            let mut score = found.map_or(0.0, |found| found.score);
            let mut fields: Vec<&'static str> =
                found.map(|found| found.fields.clone()).unwrap_or_default();
            let name = match entity.get("name") {
                Some(name) if is_truthy(name) => py_str(name),
                _ => String::new(),
            };
            let tier = match_tier(&name, &lowered);
            if let Some(bonus) = exact_name_bonus(tier)
                && !self
                    .declaration_of
                    .get(key)
                    .is_some_and(|declaration| hit_set.contains(declaration))
            {
                score += bonus;
                if !fields.contains(&"name") {
                    fields.insert(0, "name");
                }
            }
            if score > 0.0 {
                ranked.push((key.clone(), py_round(score, 4), fields));
            }
        }
        if let Some(limit) = limit
            && limit > 0
            && limit < ranked.len()
        {
            let mut scores: Vec<f64> = ranked.iter().map(|(_, score, _)| *score).collect();
            scores.sort_by(|left, right| right.total_cmp(left));
            let cutoff = scores[limit - 1];
            ranked.retain(|(_, score, _)| *score >= cutoff);
        }
        ranked.sort_by(|left, right| {
            right.1.total_cmp(&left.1).then_with(|| {
                self.entity_sort_key(&left.0)
                    .cmp(&self.entity_sort_key(&right.0))
            })
        });
        if let Some(limit) = limit {
            ranked.truncate(limit);
        }
        (terms, ranked)
    }
}

/// `node_sort_key` of one fact node.
fn node_sort_key(node_id: &str, node: &NodeProperties) -> NodeSortKey {
    NodeSortKey([
        py_str_or(node.get("repo"), ""),
        py_str_or(node.get("type"), ""),
        casefold(&py_str_or(node.get("name"), "")),
        file_of(node),
        py_str_or(node.get("line"), ""),
        node_id.to_string(),
    ])
}

/// `_index_property`: index `value` under `name`, and each field of an
/// object under `name.field`, recursively.
fn index_property(
    by_property: &mut HashMap<String, HashMap<String, HashSet<String>>>,
    node_id: &str,
    name: &str,
    value: &Value,
) {
    by_property
        .entry(name.to_string())
        .or_default()
        .entry(scalar_text(value))
        .or_default()
        .insert(node_id.to_string());
    if let Value::Object(fields) = value {
        for (child, child_value) in fields {
            index_property(
                by_property,
                node_id,
                &format!("{name}.{child}"),
                child_value,
            );
        }
    }
}

/// `file_of`: a node's `file_path`, else its `file`, as text (empty when it
/// has neither).
pub fn file_of(node: &NodeProperties) -> String {
    for key in ["file_path", "file"] {
        if let Some(value) = node.get(key)
            && is_truthy(value)
        {
            return py_str(value);
        }
    }
    String::new()
}

/// `properties_of`: a node's `fact_properties` object (empty when it has
/// none).
pub fn properties_of(node: &NodeProperties) -> &NodeProperties {
    static EMPTY: OnceLock<NodeProperties> = OnceLock::new();
    match node.get("fact_properties") {
        Some(Value::Object(properties)) => properties,
        _ => EMPTY.get_or_init(Map::new),
    }
}

/// `doc_of`: a fact's docstring — its `doc` field, else the `doc` prop older
/// loads kept. An insight has none (its explainer prose is its
/// description), and blank text is no doc.
pub fn doc_of(node: &NodeProperties) -> Option<String> {
    if node.get("type").and_then(Value::as_str) == Some("CodeInsight") {
        return None;
    }
    [node.get("doc"), properties_of(node).get("doc")]
        .into_iter()
        .flatten()
        .find_map(|value| match value {
            Value::String(text) if !py_strip(text).is_empty() => Some(text.clone()),
            _ => None,
        })
}

/// `_mappings`: the object items of a list prop (Enola refs, stages);
/// anything else is empty.
pub fn mappings(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Array(items)) => items.iter().filter(|item| item.is_object()).collect(),
        _ => Vec::new(),
    }
}

/// `_int_or_none`: an integer (not a boolean) as `i64`.
pub fn int_or_none(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        _ => None,
    }
}

/// Python's `value not in (None, "")` negated: a null or an empty string.
pub fn is_none_or_empty(value: &Value) -> bool {
    matches!(value, Value::Null) || matches!(value, Value::String(text) if text.is_empty())
}

/// Load one dataset's code graph into a snapshot (Python's `CodeRetriever.
/// _snapshot` loader): every node whose type is a [`CODE_NODE_TYPES`] type
/// or `CodeRepository`, and the edges among them, through the adapter's
/// `get_filtered_graph_data`. With per-dataset storage the graph engine
/// routes the read to the current dataset's own graph.
///
/// # Errors
///
/// The graph read failed.
pub async fn load_code_graph_snapshot(
    graph: &dyn GraphDBTrait,
) -> CodeGraphResult<CodeGraphSnapshot> {
    let types: Vec<Value> = CODE_NODE_TYPES
        .iter()
        .chain(std::iter::once(&REPOSITORY_TYPE))
        .map(|node_type| Value::String((*node_type).to_string()))
        .collect();
    let filters = HashMap::from([(std::borrow::Cow::Borrowed("type"), types)]);
    let (nodes, edges) = graph.get_filtered_graph_data(&filters).await?;
    Ok(CodeGraphSnapshot::new(nodes, edges))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn node(id: &str, properties: Value) -> GraphNode {
        let Value::Object(map) = properties else {
            panic!("an object");
        };
        (
            id.to_string(),
            map.into_iter()
                .map(|(key, value)| (Cow::Owned(key), value))
                .collect(),
        )
    }

    #[test]
    fn test_paths_match_python() {
        for path in [
            "tests/a.py",
            "src/__tests__/x.ts",
            "pkg/Spec/x.rb",
            "test_a.py",
            "a/b_test.go",
            "web/x.spec.tsx",
            "FooTest.java",
            "FooTests.cs",
            "x_spec.rb",
            "test_.py",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        for path in [
            "",
            "src/a.py",
            "testing/a.py",
            "contest.py",
            "test_a.pyc",
            "a.test.py",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
    }

    #[test]
    fn short_names_and_tiers() {
        assert_eq!(
            short_names("orders/api.create_order")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["api.create_order", "create_order"]
        );
        assert_eq!(
            short_names("src/Main.py").into_iter().collect::<Vec<_>>(),
            vec!["main", "main.py", "py"]
        );
        assert_eq!(
            match_tier("orders/api.create_order", "orders/api.create_order"),
            3
        );
        assert_eq!(match_tier("orders/api.create_order", "create_order"), 2);
        assert_eq!(match_tier("orders/api.create_order", "api.create_order"), 2);
        assert_eq!(match_tier("orders/api.create_order", "order"), 1);
        assert_eq!(match_tier("orders/api.create_order", "nothing"), -1);
        assert_eq!(match_tier("geo/Straße.lookup", "geo/strasse.lookup"), 3);
    }

    #[test]
    fn nodes_are_normalized_and_partitioned() {
        let snapshot = CodeGraphSnapshot::new(
            vec![
                node(
                    "r",
                    json!({"type": "CodeRepository", "name": "shop", "last_receipt": "{\"enola_version\": \"1\"}"}),
                ),
                node("f", json!({"type": "CodeFlow", "name": "flow"})),
                node(
                    "s",
                    json!({"type": "CodeSymbol", "properties": "{\"name\": \"a.b\", \"doc\": null, \"file_path\": \"a.py\"}"}),
                ),
                node("x", json!({"type": "Entity", "name": "noise"})),
            ],
            vec![
                ("s".into(), "x".into(), "mentions".into(), HashMap::new()),
                ("f".into(), "s".into(), "flow_entry".into(), HashMap::new()),
            ],
        );
        assert_eq!(snapshot.repositories.len(), 1);
        assert_eq!(
            snapshot.repositories["r"]["last_receipt"],
            json!({"enola_version": "1"})
        );
        assert_eq!(snapshot.flows.len(), 1);
        assert_eq!(snapshot.nodes.len(), 1);
        let symbol = &snapshot.nodes["s"];
        assert_eq!(symbol["name"], json!("a.b"));
        assert!(!symbol.contains_key("doc"));
        assert_eq!(symbol["id"], json!("s"));
        assert!(snapshot.edges.is_empty());
        assert_eq!(snapshot.flow_members["f"].entry, None);
    }

    #[tokio::test]
    async fn the_loader_reads_only_code_types() {
        let graph = cognee_graph::MockGraphDB::new();
        graph
            .add_nodes_raw(vec![
                json!({"id": "s", "type": "CodeSymbol", "name": "a"}),
                json!({"id": "r", "type": "CodeRepository", "name": "repo"}),
                json!({"id": "c", "type": "DocumentChunk", "text": "x"}),
            ])
            .await
            .unwrap();
        let snapshot = load_code_graph_snapshot(&graph).await.unwrap();
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.repositories.len(), 1);
    }
}
