//! The whole-graph CODE operations over a [`CodeGraphSnapshot`]: `insights`
//! (the explainer findings) and `architecture` (a module-level overview),
//! Python-shaped (same keys, order, defaults, bounds and messages).
//!
//! Ports of `CodeRetriever._architecture` and `_insights`
//! (`code_retriever.py:1451-1657`).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Value, json};

use super::args::{
    CodeConfig, as_strings, bounded_int, confidence, node_types, offset, optional_bool,
    relation_types, truthy_text,
};
use super::error::CodeQueryResult;
use super::snapshot::{CodeGraphSnapshot, NodeProperties, file_of, properties_of};
use crate::pyjson::{casefold, is_truthy, py_str_truthy_or, py_strip};

/// `_ARCHITECTURE_DEFAULT_TYPES`: what an overview draws unless `node_types`
/// says otherwise.
const ARCHITECTURE_DEFAULT_TYPES: [&str; 4] = [
    "CodeModule",
    "ApiEndpoint",
    "StorageResource",
    "CodeService",
];

/// `_ARCHITECTURE_SKIPPED_EDGE_TYPES`: containment, never rolled up.
const ARCHITECTURE_SKIPPED_EDGE_TYPES: [&str; 3] = ["part_of", "has_method", "evidences"];

/// The module node type.
const MODULE_TYPE: &str = "CodeModule";

/// `node.get("type")` as text (empty when missing).
fn type_of(node: &NodeProperties) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// `_insights`: the explainer findings (`CodeInsight` facts) with the facts
/// they cite.
///
/// Filters: `source`/`sources` (explainer names such as `cycles` or
/// `hotspots`), `min_confidence` (0-1), `informational` (true keeps only
/// describe-the-graph findings, false drops them), `repo`, and a title
/// substring `name` — the query text only for a call that configures
/// nothing but the operation. Each insight carries `evidence`: the facts its
/// `evidences` edges point at. Ordered by confidence (highest first, none
/// last), then the stable node order; paged by `limit` (50, max 500) and
/// `offset`; `by_source` counts every match per explainer.
///
/// # Errors
///
/// `invalid_argument` for a malformed filter or page.
pub fn insights(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let mut sources: HashSet<String> = as_strings(config.get("sources"), "sources")?
        .into_iter()
        .collect();
    if let Some(source) = truthy_text(config, "source") {
        sources.insert(source);
    }
    let min_confidence = confidence(config.get("min_confidence"), "min_confidence")?;
    let informational = optional_bool(config.get("informational"), "informational")?;
    let repo = config.get("repo").filter(|repo| !repo.is_null());
    let query_name = if config.keys().all(|key| key == "operation") {
        query
    } else {
        ""
    };
    let substring = casefold(py_strip(
        &truthy_text(config, "name").unwrap_or_else(|| query_name.to_string()),
    ));

    let confidence_of = |node_id: &str| -> Option<f64> {
        match properties_of(&snapshot.nodes[node_id]).get("confidence") {
            Some(Value::Number(number)) => number.as_f64(),
            _ => None,
        }
    };

    let empty = HashSet::new();
    let mut matched: Vec<&String> = Vec::new();
    for node_id in snapshot.by_type.get("CodeInsight").unwrap_or(&empty) {
        let node = &snapshot.nodes[node_id];
        let properties = properties_of(node);
        if !sources.is_empty()
            && !properties
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|source| sources.contains(source))
        {
            continue;
        }
        if let Some(min_confidence) = min_confidence
            && !confidence_of(node_id).is_some_and(|confidence| confidence >= min_confidence)
        {
            continue;
        }
        if let Some(informational) = informational
            && properties.get("informational").is_some_and(is_truthy) != informational
        {
            continue;
        }
        if let Some(repo) = repo
            && node.get("repo").unwrap_or(&Value::Null) != repo
        {
            continue;
        }
        if !substring.is_empty()
            && !casefold(&py_str_truthy_or(node.get("name"), "")).contains(&substring)
        {
            continue;
        }
        matched.push(node_id);
    }
    matched.sort_by(|left, right| {
        let score = |node_id: &str| confidence_of(node_id).unwrap_or(-1.0);
        score(right)
            .partial_cmp(&score(left))
            .unwrap_or(Ordering::Equal)
            .then_with(|| snapshot.node_order(left, right))
    });
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    for node_id in &matched {
        let source = properties_of(&snapshot.nodes[*node_id]).get("source");
        *by_source
            .entry(py_str_truthy_or(source, "unknown"))
            .or_default() += 1;
    }

    let total = matched.len();
    let offset = offset(config.get("offset"))?;
    let limit = bounded_int(config.get("limit"), "limit", 50, 500)?;
    let page: Vec<&String> = matched.iter().skip(offset).take(limit).copied().collect();
    let insights: Vec<Value> = page
        .iter()
        .map(|node_id| {
            let mut insight = snapshot.fact(node_id, None, false);
            let evidence: Vec<Value> = snapshot
                .forward(node_id)
                .filter(|edge| edge.relation == "evidences")
                .map(|edge| snapshot.fact(&edge.target_id, None, false))
                .collect();
            if let Value::Object(insight) = &mut insight {
                insight.insert("evidence".into(), Value::Array(evidence));
            }
            insight
        })
        .collect();
    Ok(json!({
        "operation": "insights",
        "insights": insights,
        "total": total,
        "offset": offset,
        "limit": limit,
        "has_more": offset.saturating_add(page.len()) < total,
        "by_source": by_source,
    }))
}

/// The module each fact rolls up to (`_architecture`'s `module_of`),
/// memoized for one call.
struct ModuleIndex<'a> {
    snapshot: &'a CodeGraphSnapshot,
    cache: HashMap<&'a str, Option<&'a String>>,
}

impl<'a> ModuleIndex<'a> {
    /// The module that declares a fact: itself for a module, else the
    /// target of its first `declares` edge to a module, else the module
    /// (same repository, lowest id) named after its file's directory or the
    /// nearest ancestor directory, up to `.`.
    fn module_of(&mut self, node_id: &'a str) -> Option<&'a String> {
        if let Some(found) = self.cache.get(node_id) {
            return *found;
        }
        let snapshot = self.snapshot;
        let (key, node) = snapshot.nodes.get_key_value(node_id)?;
        let found = if type_of(node) == MODULE_TYPE {
            Some(key)
        } else {
            snapshot
                .forward(node_id)
                .find(|edge| {
                    edge.relation == "declares"
                        && type_of(&snapshot.nodes[&edge.target_id]) == MODULE_TYPE
                })
                .map(|edge| &edge.target_id)
                .or_else(|| Self::module_by_directory(snapshot, node))
        };
        self.cache.insert(key, found);
        found
    }

    /// The directory walk of `module_of`.
    fn module_by_directory(
        snapshot: &'a CodeGraphSnapshot,
        node: &NodeProperties,
    ) -> Option<&'a String> {
        let file = file_of(node);
        let mut directory = file.rsplit_once('/').map_or(".", |(head, _)| head);
        let repo = node.get("repo").unwrap_or(&Value::Null);
        loop {
            let found = snapshot
                .by_name
                .get(directory)
                .into_iter()
                .flatten()
                .filter(|candidate| {
                    let candidate = &snapshot.nodes[*candidate];
                    type_of(candidate) == MODULE_TYPE
                        && candidate.get("repo").unwrap_or(&Value::Null) == repo
                })
                .min();
            if found.is_some() {
                return found;
            }
            if directory == "." || directory.is_empty() {
                return None;
            }
            directory = directory.rsplit_once('/').map_or(".", |(head, _)| head);
        }
    }
}

/// `_architecture`: a module-level overview of the code graph, drawn as a
/// Mermaid diagram by default.
///
/// Shows modules plus the routes, storage and services they declare
/// (`node_types` overrides the set) and rolls every other edge up to the
/// modules that declare its ends: two symbols calling across modules become
/// one `calls` edge between the modules, with a `count`. Edges within one
/// module, containment edges (`part_of`, `has_method`, `evidences`) and
/// edges outside `relation_types` are dropped; a `declares` edge is drawn
/// from the module. `repo` narrows to one repository; `max_nodes` (80, max
/// 300) keeps the best-connected nodes and reports `truncated`.
///
/// # Errors
///
/// `invalid_argument` for a malformed argument.
pub fn architecture<'a>(
    snapshot: &'a CodeGraphSnapshot,
    config: &CodeConfig,
) -> CodeQueryResult<Value> {
    let repo = config.get("repo").filter(|repo| !repo.is_null());
    let node_types: HashSet<String> = node_types(config.get("node_types"), "node_types")?
        .unwrap_or_else(|| {
            ARCHITECTURE_DEFAULT_TYPES
                .iter()
                .map(|node_type| (*node_type).to_string())
                .collect()
        });
    let max_nodes = bounded_int(config.get("max_nodes"), "max_nodes", 80, 300)?;
    let relation_types = relation_types(config)?;

    let in_scope = |node_id: &str| {
        let node = &snapshot.nodes[node_id];
        node_types.contains(type_of(node))
            && repo.is_none_or(|repo| node.get("repo").unwrap_or(&Value::Null) == repo)
    };
    let mut modules = ModuleIndex {
        snapshot,
        cache: HashMap::new(),
    };

    let mut rolled: HashMap<(&String, &String, &str), usize> = HashMap::new();
    for edge in &snapshot.edges {
        let relation = edge.relation.as_str();
        if ARCHITECTURE_SKIPPED_EDGE_TYPES.contains(&relation)
            || relation_types
                .as_ref()
                .is_some_and(|types| !types.contains(relation))
        {
            continue;
        }
        // An end that is drawn stays itself; any other rolls up to its module.
        let mut endpoint = |node_id: &'a String| {
            if in_scope(node_id) {
                Some(node_id)
            } else {
                modules.module_of(node_id)
            }
        };
        let (Some(mut source), Some(mut target)) =
            (endpoint(&edge.source_id), endpoint(&edge.target_id))
        else {
            continue;
        };
        if source == target || !in_scope(source) || !in_scope(target) {
            continue;
        }
        if relation == "declares" && type_of(&snapshot.nodes[source]) != MODULE_TYPE {
            // A route or storage fact declares its module; an architecture
            // reads the other way — the module declares (exposes) the fact.
            std::mem::swap(&mut source, &mut target);
        }
        *rolled.entry((source, target, relation)).or_default() += 1;
    }

    let mut degree: HashMap<&String, usize> = HashMap::new();
    for (source, target, _) in rolled.keys() {
        *degree.entry(source).or_default() += 1;
        *degree.entry(target).or_default() += 1;
    }
    let mut candidates: Vec<&String> = snapshot
        .nodes
        .keys()
        .filter(|node_id| in_scope(node_id))
        .collect();
    let degree_of = |node_id: &String| degree.get(node_id).copied().unwrap_or(0);
    candidates.sort_by(|left, right| {
        degree_of(right)
            .cmp(&degree_of(left))
            .then_with(|| snapshot.node_order(left, right))
    });
    let mut kept: Vec<&String> = candidates.iter().take(max_nodes).copied().collect();
    let kept_set: HashSet<&String> = kept.iter().copied().collect();
    kept.sort_by(|left, right| snapshot.node_order(left, right));

    let mut rolled: Vec<((&String, &String, &str), usize)> = rolled.into_iter().collect();
    rolled.sort_by(
        |((left_source, left_target, left_type), _),
         ((right_source, right_target, right_type), _)| {
            left_type
                .cmp(right_type)
                .then_with(|| snapshot.node_order(left_source, right_source))
                .then_with(|| snapshot.node_order(left_target, right_target))
        },
    );
    let name_of = |node_id: &String| {
        snapshot.nodes[node_id]
            .get("name")
            .cloned()
            .unwrap_or(Value::Null)
    };
    let edges: Vec<Value> = rolled
        .into_iter()
        .filter(|((source, target, _), _)| kept_set.contains(source) && kept_set.contains(target))
        .map(|((source, target, edge_type), count)| {
            json!({
                "source_id": source,
                "source": name_of(source),
                "target_id": target,
                "target": name_of(target),
                "type": edge_type,
                "count": count,
            })
        })
        .collect();
    let edges_shown = edges.len();
    let repos: BTreeSet<String> = kept
        .iter()
        .map(|node_id| py_str_truthy_or(snapshot.nodes[*node_id].get("repo"), ""))
        .filter(|repo| !repo.is_empty())
        .collect();
    Ok(json!({
        "operation": "architecture",
        "repos": repos,
        "nodes": kept
            .iter()
            .map(|node_id| snapshot.fact(node_id, None, false))
            .collect::<Vec<_>>(),
        "edges": edges,
        "stats": {
            "nodes_total": candidates.len(),
            "nodes_shown": kept.len(),
            "edges_shown": edges_shown,
            "truncated": candidates.len() > kept.len(),
        },
    }))
}
