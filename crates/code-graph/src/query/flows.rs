//! Enola's flows as the CODE results show them, and the reverse `calls` walk
//! that finds the flows through a symbol: Python's `_CodeGraphSnapshot`
//! flow helpers (`code_retriever.py:911-1039`).
//!
//! A flow (a `CodeFlow` node written from Enola's `flows.json`) names its
//! entry point, stages and routes as Enola refs; the snapshot resolved each
//! ref to a fact node once ([`FlowMembers`](super::FlowMembers)). These
//! helpers render a flow as a list item ([`CodeGraphSnapshot::flow_summary`])
//! or in full ([`CodeGraphSnapshot::flow_detail`]), and answer which flows
//! reach a symbol ([`CodeGraphSnapshot::flows_reaching`]).

use std::collections::{HashMap, VecDeque};

use serde_json::{Map, Value, json};

use super::normalize::normalize_value;
use super::snapshot::{
    CodeGraphSnapshot, doc_of, file_of, int_or_none, is_none_or_empty, mappings, properties_of,
};
use crate::pyjson::is_truthy;

/// `_FLOW_WALK_MAX_DEPTH`: Enola walks a flow at most eight calls deep; the
/// reverse walk that finds the flows through a symbol keeps the bound.
pub const FLOW_WALK_MAX_DEPTH: usize = 8;
/// `_FLOW_WALK_MAX_VISITED`: ... and stops at 5000 symbols.
pub const FLOW_WALK_MAX_VISITED: usize = 5000;

/// How a flow goes through a symbol ([`CodeGraphSnapshot::flows_reaching`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowVia {
    /// The symbol is the flow's entry point.
    Entry,
    /// Enola lists the symbol as one of the flow's stages.
    Stage,
    /// The flow's entry calls down to the symbol.
    Reach,
    /// The symbol is one of the flow's routes.
    Route,
}

impl FlowVia {
    /// The wire form: `entry`, `stage`, `reach` or `route`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Stage => "stage",
            Self::Reach => "reach",
            Self::Route => "route",
        }
    }
}

/// One flow that goes through a symbol: its id, how, and how many `calls`
/// hops from its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowReach {
    /// The flow node id.
    pub flow_id: String,
    /// How the flow goes through the symbol.
    pub via: FlowVia,
    /// The `calls` hops from the flow's entry to the symbol (a stage the
    /// walk missed counts at its own depth, a route at 0).
    pub distance: i64,
}

/// The callers of a symbol, transitively, and whether the walk was cut
/// ([`CodeGraphSnapshot::calls_reach`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallsReach {
    /// Node id -> `calls` hops to the symbol (the symbol itself at 0).
    pub distances: HashMap<String, i64>,
    /// Whether the walk stopped at [`FLOW_WALK_MAX_VISITED`] symbols.
    pub truncated: bool,
}

impl CodeGraphSnapshot {
    /// `calls_reach`: the callers of `node_id`, transitively — a reverse
    /// breadth-first walk over `calls` edges, bounded like Enola's own flow
    /// walk (eight hops, 5000 symbols).
    pub fn calls_reach(&self, node_id: &str) -> CallsReach {
        let mut distances = HashMap::from([(node_id.to_string(), 0_i64)]);
        let mut queue = VecDeque::from([node_id.to_string()]);
        while let Some(current) = queue.pop_front() {
            let depth = distances[&current];
            if depth >= FLOW_WALK_MAX_DEPTH as i64 {
                continue;
            }
            for edge in self.reverse(&current) {
                if edge.relation != "calls" || distances.contains_key(&edge.source_id) {
                    continue;
                }
                if distances.len() >= FLOW_WALK_MAX_VISITED {
                    return CallsReach {
                        distances,
                        truncated: true,
                    };
                }
                distances.insert(edge.source_id.clone(), depth + 1);
                queue.push_back(edge.source_id.clone());
            }
        }
        CallsReach {
            distances,
            truncated: false,
        }
    }

    /// `flow_ref`: an Enola ref (entry, stage, route) with the cognee node it
    /// resolves to. `node_id` is `None` when no fact node carries the ref's
    /// id or name. The ref's own file, line and doc come first; the node
    /// fills what it lacks, and empty fields are left out.
    pub fn flow_ref(&self, reference: Option<&Value>, node_id: Option<&str>) -> Map<String, Value> {
        let empty = Map::new();
        let reference = match reference {
            Some(Value::Object(reference)) => reference,
            _ => &empty,
        };
        let node = node_id.and_then(|node_id| self.nodes.get(node_id));
        // Python's `ref.get(key) or <node fallback>`.
        let truthy = |key: &str| reference.get(key).filter(|value| is_truthy(value)).cloned();
        let mut result = Map::new();
        result.insert(
            "name".into(),
            truthy("name")
                .or_else(|| node.and_then(|node| node.get("name").cloned()))
                .unwrap_or(Value::Null),
        );
        result.insert(
            "enola_id".into(),
            reference.get("id").cloned().unwrap_or(Value::Null),
        );
        result.insert(
            "node_id".into(),
            node_id.map_or(Value::Null, |node_id| json!(node_id)),
        );
        let line = match reference.get("line") {
            Some(line) if !line.is_null() => Some(line.clone()),
            _ => node.and_then(|node| node.get("line").cloned()),
        };
        let optional = [
            (
                "file",
                truthy("file").or_else(|| node.map(|node| Value::String(file_of(node)))),
            ),
            ("line", line),
            (
                "doc",
                truthy("doc").or_else(|| node.and_then(doc_of).map(Value::String)),
            ),
            ("method", reference.get("method").cloned()),
        ];
        for (key, value) in optional {
            if let Some(value) = value
                && !is_none_or_empty(&value)
            {
                result.insert(key.into(), value);
            }
        }
        result
    }

    /// `flow_summary`: one flow as a list item — identity, ranking, entry,
    /// routes, summary and metrics.
    ///
    /// # Panics
    ///
    /// Never for an id of [`Self::flows`]; another id is a caller bug.
    pub fn flow_summary(&self, flow_id: &str) -> Map<String, Value> {
        let flow = &self.flows[flow_id];
        let properties = properties_of(flow);
        let members = &self.flow_members[flow_id];
        let field =
            |map: &Map<String, Value>, key: &str| map.get(key).cloned().unwrap_or(Value::Null);
        let mut result = Map::new();
        result.insert("id".into(), json!(flow_id));
        result.insert("enola_id".into(), field(flow, "enola_id"));
        result.insert("name".into(), field(flow, "name"));
        result.insert("rank".into(), field(properties, "rank"));
        result.insert("repo".into(), field(flow, "repo"));
        result.insert("mechanism".into(), field(properties, "mechanism"));
        if properties.get("side") == Some(&Value::Bool(true)) {
            result.insert("side".into(), Value::Bool(true));
        }
        result.insert(
            "entry".into(),
            Value::Object(self.flow_ref(properties.get("entry"), members.entry.as_deref())),
        );
        let routes: Vec<Value> = mappings(properties.get("routes"))
            .into_iter()
            .zip(&members.routes)
            .map(|(route, node_id)| Value::Object(self.flow_ref(Some(route), node_id.as_deref())))
            .collect();
        if !routes.is_empty() {
            result.insert("routes".into(), Value::Array(routes));
        }
        result.insert(
            "summary".into(),
            self.flow_summary_text(flow_id)
                .map_or(Value::Null, Value::String),
        );
        let metrics = match properties.get("metrics") {
            Some(metrics @ Value::Object(_)) => normalize_value(metrics),
            _ => Value::Object(Map::new()),
        };
        result.insert("metrics".into(), metrics);
        result
    }

    /// `flow_detail`: [`Self::flow_summary`] plus the stages (with Enola's
    /// order, depth, parent and reach), the modules and the flows it
    /// overlaps.
    ///
    /// # Panics
    ///
    /// Never for an id of [`Self::flows`]; another id is a caller bug.
    pub fn flow_detail(&self, flow_id: &str) -> Map<String, Value> {
        let mut result = self.flow_summary(flow_id);
        let properties = properties_of(&self.flows[flow_id]);
        let stages: Vec<Value> = mappings(properties.get("stages"))
            .into_iter()
            .zip(&self.flow_members[flow_id].stages)
            .map(|(stage, node_id)| {
                let mut item = self.flow_ref(Some(stage), node_id.as_deref());
                for key in ["order", "depth", "parent", "reach"] {
                    if let Some(value) = stage.get(key)
                        && !value.is_null()
                    {
                        item.insert(key.into(), value.clone());
                    }
                }
                Value::Object(item)
            })
            .collect();
        result.insert("stages".into(), Value::Array(stages));
        let modules: Vec<Value> = mappings(properties.get("modules"))
            .into_iter()
            .cloned()
            .collect();
        result.insert("modules".into(), normalize_value(&Value::Array(modules)));
        for key in ["same_reach_as", "nested_in"] {
            if let Some(value) = properties.get(key)
                && is_truthy(value)
            {
                result.insert(key.into(), normalize_value(value));
            }
        }
        result
    }

    /// `flows_reaching`: every flow that goes through `node_id`, in Enola's
    /// ranking. `reached` is [`Self::calls_reach`] of the node.
    ///
    /// A flow goes through the node when its entry is the node or calls down
    /// to it (via `entry`; `stage` when Enola listed the node as a stage;
    /// else `reach`; the distance in `calls` hops). A stage the walk missed
    /// still counts, at the stage's depth (or 1), and a route at 0.
    pub fn flows_reaching(&self, node_id: &str, reached: &CallsReach) -> Vec<FlowReach> {
        let mut found = Vec::new();
        for flow_id in &self.flow_order {
            let members = &self.flow_members[flow_id];
            let stage_position = members
                .stages
                .iter()
                .position(|stage| stage.as_deref() == Some(node_id));
            let entry_distance = members.entry.as_ref().and_then(|entry| {
                reached
                    .distances
                    .get(entry)
                    .map(|distance| (entry, *distance))
            });
            let (via, distance) = if let Some((entry, distance)) = entry_distance {
                let via = if entry == node_id {
                    FlowVia::Entry
                } else if stage_position.is_some() {
                    FlowVia::Stage
                } else {
                    FlowVia::Reach
                };
                (via, distance)
            } else if let Some(position) = stage_position {
                let stages = mappings(properties_of(&self.flows[flow_id]).get("stages"));
                let depth = stages
                    .get(position)
                    .and_then(|stage| int_or_none(stage.get("depth")));
                // Python's `depth or 1`.
                (
                    FlowVia::Stage,
                    depth.filter(|depth| *depth != 0).unwrap_or(1),
                )
            } else if members
                .routes
                .iter()
                .any(|route| route.as_deref() == Some(node_id))
            {
                (FlowVia::Route, 0)
            } else {
                continue;
            };
            found.push(FlowReach {
                flow_id: flow_id.clone(),
                via,
                distance,
            });
        }
        found
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;

    use cognee_graph::{EdgeData, GraphNode};

    use super::*;

    fn symbol(id: &str) -> GraphNode {
        (
            id.to_string(),
            HashMap::from([
                (Cow::Borrowed("type"), json!("CodeSymbol")),
                (Cow::Borrowed("name"), json!(id)),
                (Cow::Borrowed("file_path"), json!(format!("{id}.py"))),
            ]),
        )
    }

    fn calls(source: &str, target: &str) -> EdgeData {
        (
            source.to_string(),
            target.to_string(),
            "calls".to_string(),
            HashMap::new(),
        )
    }

    /// A chain of ten callers: the walk stops eight hops up, as Enola's.
    #[test]
    fn calls_reach_stops_at_eight_hops() {
        let names: Vec<String> = (0..11).map(|index| format!("s{index:02}")).collect();
        let nodes = names.iter().map(|name| symbol(name)).collect();
        let edges = names
            .windows(2)
            .map(|pair| calls(&pair[1], &pair[0]))
            .collect();
        let snapshot = CodeGraphSnapshot::new(nodes, edges);
        let reach = snapshot.calls_reach("s00");
        assert!(!reach.truncated);
        assert_eq!(reach.distances.len(), 9);
        assert_eq!(reach.distances["s08"], 8);
        assert!(!reach.distances.contains_key("s09"));
    }

    /// More callers than the walk may visit: it stops at 5000 symbols (the
    /// start included) and says so. Other relations are not walked.
    #[test]
    fn calls_reach_stops_at_five_thousand_symbols() {
        let mut nodes = vec![symbol("target"), symbol("importer")];
        let mut edges = vec![(
            "importer".to_string(),
            "target".to_string(),
            "imports".to_string(),
            HashMap::new(),
        )];
        for index in 0..FLOW_WALK_MAX_VISITED + 10 {
            let caller = format!("caller{index:05}");
            nodes.push(symbol(&caller));
            edges.push(calls(&caller, "target"));
        }
        let snapshot = CodeGraphSnapshot::new(nodes, edges);
        let reach = snapshot.calls_reach("target");
        assert!(reach.truncated);
        assert_eq!(reach.distances.len(), FLOW_WALK_MAX_VISITED);
        assert!(!reach.distances.contains_key("importer"));
        assert!(reach.distances.values().all(|distance| *distance <= 1));
    }

    /// A flow `f` entered at `entry` (which calls `mid`), with Enola stages
    /// `staged` (depth 0) and `deep` (depth 3) its calls do not reach, and
    /// route `route`. Refs resolve by name through the flow's own edges.
    fn flow_snapshot() -> CodeGraphSnapshot {
        let mut nodes: Vec<GraphNode> = ["entry", "mid", "staged", "deep", "route", "other"]
            .into_iter()
            .map(symbol)
            .collect();
        nodes.push((
            "f".to_string(),
            HashMap::from([
                (Cow::Borrowed("type"), json!("CodeFlow")),
                (Cow::Borrowed("name"), json!("main")),
                (
                    Cow::Borrowed("fact_properties"),
                    json!({
                        "entry": {"name": "entry", "line": 0, "file": ""},
                        "stages": [
                            {"name": "staged", "depth": 0, "order": 1},
                            "not a mapping",
                            {"name": "deep", "depth": 3, "order": null},
                        ],
                        "routes": [{"name": "route", "method": "POST"}],
                        "mechanism": "main",
                    }),
                ),
            ]),
        ));
        let member = |target: &str, relation: &str| {
            (
                "f".to_string(),
                target.to_string(),
                relation.to_string(),
                HashMap::new(),
            )
        };
        let edges = vec![
            member("entry", "flow_entry"),
            member("staged", "flow_stage"),
            member("deep", "flow_stage"),
            member("route", "flow_route"),
            calls("entry", "mid"),
        ];
        CodeGraphSnapshot::new(nodes, edges)
    }

    /// Python's `flows_reaching`: the entry and what it calls down to carry
    /// their `calls` distance; a stage the walk missed counts at its depth
    /// (`depth or 1`, so depth 0 is 1), a route at 0, anything else not at
    /// all.
    #[test]
    fn flows_reaching_entry_reach_stage_and_route() {
        let snapshot = flow_snapshot();
        let reaching = |node_id: &str| {
            let reached = snapshot.calls_reach(node_id);
            snapshot
                .flows_reaching(node_id, &reached)
                .into_iter()
                .map(|reach| (reach.flow_id, reach.via, reach.distance))
                .collect::<Vec<_>>()
        };
        let flow = |via, distance| vec![("f".to_string(), via, distance)];
        assert_eq!(reaching("entry"), flow(FlowVia::Entry, 0));
        assert_eq!(reaching("mid"), flow(FlowVia::Reach, 1));
        assert_eq!(reaching("staged"), flow(FlowVia::Stage, 1));
        assert_eq!(reaching("deep"), flow(FlowVia::Stage, 3));
        assert_eq!(reaching("route"), flow(FlowVia::Route, 0));
        assert!(reaching("other").is_empty());
    }

    /// Python's `flow_ref`: the ref's own fields first, the node filling a
    /// falsy name or file; a line of 0 is the ref's (`is not None`), and
    /// empty fields are left out. `flow_detail` keeps a stage's non-null
    /// `order`/`depth` and skips list items that are not mappings. The
    /// expected values are what Python at the pin returns for this graph.
    #[test]
    fn flow_ref_and_detail_follow_python_precedence() {
        let snapshot = flow_snapshot();
        let detail = snapshot.flow_detail("f");
        assert_eq!(
            detail["entry"],
            json!({"name": "entry", "enola_id": null, "node_id": "entry",
                   "file": "entry.py", "line": 0})
        );
        assert_eq!(
            detail["routes"],
            json!([{"name": "route", "enola_id": null, "node_id": "route",
                    "file": "route.py", "method": "POST"}])
        );
        assert_eq!(
            detail["stages"],
            json!([
                {"name": "staged", "enola_id": null, "node_id": "staged",
                 "file": "staged.py", "order": 1, "depth": 0},
                {"name": "deep", "enola_id": null, "node_id": "deep",
                 "file": "deep.py", "depth": 3},
            ])
        );
        assert_eq!(detail["modules"], json!([]));
        assert_eq!(detail["metrics"], json!({}));
        assert_eq!(detail["summary"], Value::Null);
        // A ref no node resolves keeps only what it carries.
        assert_eq!(
            snapshot.flow_ref(Some(&json!({"id": "E", "name": "ghost", "doc": ""})), None),
            json!({"name": "ghost", "enola_id": "E", "node_id": null})
                .as_object()
                .unwrap()
                .clone()
        );
    }

    #[test]
    fn flow_via_wire_names() {
        assert_eq!(
            [
                FlowVia::Entry,
                FlowVia::Stage,
                FlowVia::Reach,
                FlowVia::Route
            ]
            .map(FlowVia::as_str),
            ["entry", "stage", "reach", "route"]
        );
    }
}
