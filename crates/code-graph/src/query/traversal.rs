//! The walking CODE operations over a [`CodeGraphSnapshot`]: `explore`,
//! `traverse`, `find_path` and `impact_analysis`, Python-shaped (same keys,
//! order, defaults, bounds and messages).
//!
//! Ports of `CodeRetriever._explore`, `_traverse`, `_find_path`,
//! `_impact_analysis`, `_resolve_seeds`, `_traverse_graph`,
//! `_reachable_count`, `_impact_seeds` and `_impact_summary`
//! (`code_retriever.py:1778-2172`).
//!
//! Python evaluates an operation's arguments in a fixed order (the seeds
//! first, then the walk's bounds and filters as its call lists them) and
//! raises on the first bad one; each function here reads them in the same
//! order, so a call with several mistakes reports the one Python reports.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde_json::{Map, Value, json};

use super::args::{
    CodeConfig, Direction, as_strings, bounded_int, direction, first_truthy, first_truthy_text,
    flag_or, node_types, relation_types, truthy_text,
};
use super::error::{CodeQueryError, CodeQueryResult};
use super::snapshot::{CodeGraphSnapshot, SnapshotEdge, file_of};
use crate::pyjson::{is_truthy, py_str, py_str_truthy_or, py_strip};

/// `_TYPE_SYMBOL_KINDS`: the symbol kinds whose members and constructor
/// stand in for a type in an impact or reverse walk.
const TYPE_SYMBOL_KINDS: [&str; 4] = ["struct", "class", "interface", "type"];

/// The bounds and filters of one breadth-first walk (`_traverse_graph`'s
/// keyword arguments).
struct Walk<'a> {
    direction: Direction,
    relation_types: Option<&'a HashSet<String>>,
    node_types: Option<&'a HashSet<String>>,
    max_depth: usize,
    max_nodes: usize,
}

/// `_explore`: the neighbourhood of one seed, both ways by default.
///
/// The seed comes from `name`/`focus` (else the query text) or
/// `id`/`node_id`, within `repo`; `direction` (both), `relation_types`,
/// `node_types`, `max_depth` (1, max 2) and `max_nodes` (100, max 500) shape
/// the walk. `focus` is the seed with its outgoing relations.
///
/// # Errors
///
/// `not_found` / `ambiguous` for a seed that does not resolve,
/// `invalid_argument` for no seed or a malformed argument.
pub fn explore(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let name = first_truthy_text(config, &["name", "focus"]).unwrap_or_else(|| query.to_string());
    let node_id = first_truthy_text(config, &["id", "node_id"]);
    let seed = snapshot.resolve(
        Some(&name),
        node_id.as_deref(),
        config.get("repo"),
        "explore seed",
    )?;
    let direction = direction(config.get("direction"), "both")?;
    let relation_types = relation_types(config)?;
    let node_types = node_types(config.get("node_types"), "node_types")?;
    let max_depth = bounded_int(config.get("max_depth"), "max_depth", 1, 2)?;
    let max_nodes = bounded_int(config.get("max_nodes"), "max_nodes", 100, 500)?;
    let traversal = traverse_graph(
        snapshot,
        std::slice::from_ref(&seed),
        &Walk {
            direction,
            relation_types: relation_types.as_ref(),
            node_types: node_types.as_ref(),
            max_depth,
            max_nodes,
        },
    );
    let mut result = Map::new();
    result.insert("operation".into(), json!("explore"));
    result.insert("focus".into(), snapshot.fact(&seed, None, true));
    result.extend(traversal);
    Ok(Value::Object(result))
}

/// `_traverse`: a walk from several seeds, forward by default.
///
/// Seeds come from `names`/`name`/`start`/`starts` and
/// `node_ids`/`id`/`start_id`/`start_ids` (else the query text). A reverse
/// walk from a type also starts from its methods and constructor
/// ([`impact_seeds`]) unless `type_rollup` is false. `max_depth` (5, max
/// 20) and `max_nodes` (100, max 500) bound it.
///
/// # Errors
///
/// As [`resolve_seeds`], and `invalid_argument` for a malformed argument.
pub fn traverse(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let mut seeds = resolve_seeds(
        snapshot,
        config,
        query,
        "traverse seed",
        &["start", "starts"],
        &["start_id", "start_ids"],
    )?;
    let direction = direction(config.get("direction"), "forward")?;
    if direction == Direction::Reverse && flag_or(config, "type_rollup", true) {
        seeds = impact_seeds(snapshot, &seeds);
    }
    let relation_types = relation_types(config)?;
    let node_types = node_types(config.get("node_types"), "node_types")?;
    let max_depth = bounded_int(config.get("max_depth"), "max_depth", 5, 20)?;
    let max_nodes = bounded_int(config.get("max_nodes"), "max_nodes", 100, 500)?;
    let traversal = traverse_graph(
        snapshot,
        &seeds,
        &Walk {
            direction,
            relation_types: relation_types.as_ref(),
            node_types: node_types.as_ref(),
            max_depth,
            max_nodes,
        },
    );
    let mut result = Map::new();
    result.insert("operation".into(), json!("traverse"));
    result.extend(traversal);
    Ok(Value::Object(result))
}

/// `_find_path`: the shortest forward path from a source to a target.
///
/// The source comes from `source`/`from`/`name` (else the query text) or
/// `source_id`/`from_id`/`id`, within `source_repo` (else `repo`); the target
/// from `target`/`to` or `target_id`/`to_id`, within `target_repo`. A type
/// target is also reached through its methods or constructor
/// (`target_rollup`, on by default), reported as `matched_to`.
/// `max_depth` (10, max 20) and `relation_types` bound the walk; neighbours
/// are visited in the stable node order, so the path is deterministic.
///
/// # Errors
///
/// `not_found` / `ambiguous` for an end that does not resolve,
/// `invalid_argument` for a missing end or a malformed argument.
pub fn find_path(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let source_name =
        first_truthy_text(config, &["source", "from", "name"]).unwrap_or_else(|| query.to_string());
    let source_id = first_truthy_text(config, &["source_id", "from_id", "id"]);
    let source = snapshot.resolve(
        Some(&source_name),
        source_id.as_deref(),
        first_truthy(config, &["source_repo", "repo"]),
        "path source",
    )?;
    let target_name = first_truthy_text(config, &["target", "to"]);
    let target_id = first_truthy_text(config, &["target_id", "to_id"]);
    let target = snapshot.resolve(
        target_name.as_deref(),
        target_id.as_deref(),
        config.get("target_repo"),
        "path target",
    )?;
    let max_depth = bounded_int(config.get("max_depth"), "max_depth", 10, 20)?;
    let relation_types = relation_types(config)?;
    let target_candidates: HashSet<String> = if flag_or(config, "target_rollup", true) {
        impact_seeds(snapshot, std::slice::from_ref(&target))
    } else {
        vec![target.clone()]
    }
    .into_iter()
    .collect();

    let fact = |node_id: &str| snapshot.fact(node_id, None, false);
    let mut result = Map::new();
    result.insert("operation".into(), json!("find_path"));
    result.insert("from".into(), fact(&source));
    result.insert("to".into(), fact(&target));

    if target_candidates.contains(&source) {
        result.insert("found".into(), json!(true));
        result.insert(
            "path".into(),
            json!([snapshot.fact(&source, Some(0), false)]),
        );
        result.insert("edges".into(), json!([]));
        if source != target {
            result.insert("matched_to".into(), fact(&source));
        }
        return Ok(Value::Object(result));
    }

    let mut parent: HashMap<&String, (&String, &SnapshotEdge)> = HashMap::new();
    let mut visited: HashSet<&String> = HashSet::from([&source]);
    let mut queue: VecDeque<(&String, usize)> = VecDeque::from([(&source, 0)]);
    let mut matched: Option<&String> = None;
    while matched.is_none() {
        let Some((node_id, depth)) = queue.pop_front() else {
            break;
        };
        if depth >= max_depth {
            continue;
        }
        for (neighbor_id, edge) in
            snapshot.adjacent(node_id, Direction::Forward, relation_types.as_ref())
        {
            if !visited.insert(neighbor_id) {
                continue;
            }
            parent.insert(neighbor_id, (node_id, edge));
            if target_candidates.contains(neighbor_id) {
                matched = Some(neighbor_id);
                break;
            }
            queue.push_back((neighbor_id, depth + 1));
        }
    }

    let mut path_ids: Vec<&String> = Vec::new();
    let mut path_edges: Vec<&SnapshotEdge> = Vec::new();
    if let Some(matched) = matched {
        let mut current = matched;
        while *current != source {
            path_ids.push(current);
            let (previous, edge) = parent[current];
            path_edges.push(edge);
            current = previous;
        }
        path_ids.push(&source);
        path_ids.reverse();
        path_edges.reverse();
    }
    result.insert("found".into(), json!(matched.is_some()));
    result.insert(
        "path".into(),
        Value::Array(
            path_ids
                .iter()
                .enumerate()
                .map(|(depth, node_id)| snapshot.fact(node_id, Some(depth), false))
                .collect(),
        ),
    );
    result.insert(
        "edges".into(),
        Value::Array(
            path_edges
                .iter()
                .map(|edge| snapshot.edge_payload(edge))
                .collect(),
        ),
    );
    if let Some(matched) = matched
        && *matched != target
    {
        result.insert("matched_to".into(), fact(matched));
    }
    Ok(Value::Object(result))
}

/// `_impact_analysis`: what depends on the targets, by distance.
///
/// Targets come from `names`/`name`/`target`/`targets` and
/// `node_ids`/`id`/`target_id`/`target_ids` (else the query text); a type
/// target also stands for its methods and constructor ([`impact_seeds`]).
/// The reverse walk (`max_depth` 3, max 10; `max_nodes` 200, max 500;
/// `relation_types`, `node_types`) lists the dependents `by_depth`;
/// `total_dependents` counts every reachable one, past the display cap.
/// `cross_repo_impact` names the other repositories reached, and
/// `include_forward` / `include_dependencies` adds the targets' own forward
/// walk.
///
/// # Errors
///
/// As [`resolve_seeds`], and `invalid_argument` for a malformed argument.
pub fn impact_analysis(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let targets = resolve_seeds(
        snapshot,
        config,
        query,
        "impact target",
        &["target", "targets"],
        &["target_id", "target_ids"],
    )?;
    let seeds = impact_seeds(snapshot, &targets);
    let max_depth = bounded_int(config.get("max_depth"), "max_depth", 3, 10)?;
    let max_nodes = bounded_int(config.get("max_nodes"), "max_nodes", 200, 500)?;
    let relation_types = relation_types(config)?;
    let node_types = node_types(config.get("node_types"), "node_types")?;
    let reverse_walk = Walk {
        direction: Direction::Reverse,
        relation_types: relation_types.as_ref(),
        node_types: node_types.as_ref(),
        max_depth,
        max_nodes,
    };
    let mut reverse = traverse_graph(snapshot, &seeds, &reverse_walk);

    let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut grouped: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    if let Some(Value::Array(nodes)) = reverse.get("nodes") {
        for node in nodes {
            let node_id = node.get("id").and_then(Value::as_str).unwrap_or_default();
            let depth = node.get("depth").and_then(Value::as_u64).unwrap_or(0);
            if !seed_set.contains(node_id) && depth > 0 {
                grouped.entry(depth).or_default().push(node.clone());
            }
        }
    }
    let by_depth: Map<String, Value> = grouped
        .into_iter()
        .map(|(depth, nodes)| (depth.to_string(), Value::Array(nodes)))
        .collect();
    let total_dependents = reachable_count(
        snapshot,
        &seeds,
        Direction::Reverse,
        relation_types.as_ref(),
        max_depth,
    );

    let target_repos: HashSet<String> = targets
        .iter()
        .map(|node_id| py_str_truthy_or(snapshot.nodes[node_id].get("repo"), ""))
        .collect();
    let cross_repo: BTreeSet<String> = by_depth
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|node| node.get("repo").filter(|repo| is_truthy(repo)))
        .map(py_str)
        .filter(|repo| !target_repos.contains(repo))
        .collect();

    let fact = |node_id: &String, depth: Option<usize>| snapshot.fact(node_id, depth, false);
    let summary = impact_summary(&by_depth, total_dependents);
    let mut result = Map::new();
    result.insert("operation".into(), json!("impact_analysis"));
    result.insert(
        "targets".into(),
        Value::Array(targets.iter().map(|node_id| fact(node_id, None)).collect()),
    );
    result.insert(
        "impact_seeds".into(),
        Value::Array(seeds.iter().map(|node_id| fact(node_id, Some(0))).collect()),
    );
    result.insert("by_depth".into(), Value::Object(by_depth));
    result.insert(
        "edges".into(),
        reverse.remove("edges").unwrap_or_else(|| json!([])),
    );
    result.insert("total_dependents".into(), json!(total_dependents));
    result.insert("cross_repo_impact".into(), json!(cross_repo));
    result.insert("summary".into(), json!(summary));
    result.insert(
        "stats".into(),
        reverse.remove("stats").unwrap_or(Value::Null),
    );
    if flag_or(config, "include_dependencies", false) || flag_or(config, "include_forward", false) {
        let forward = traverse_graph(
            snapshot,
            &targets,
            &Walk {
                direction: Direction::Forward,
                ..reverse_walk
            },
        );
        result.insert("forward_dependencies".into(), Value::Object(forward));
    }
    Ok(Value::Object(result))
}

/// `_resolve_seeds`: the seeds of a multi-seed walk, as node ids in the
/// stable node order without duplicates.
///
/// Ids come from `node_ids`, `id` and the `id_aliases` fields, names from
/// `names`, `name` and the `name_aliases` fields (all validated first);
/// with neither, the stripped query text is the one name. Every seed must
/// resolve (within `repo`): the first that does not fails the call.
///
/// # Errors
///
/// `invalid_argument` for a malformed list or no seed at all, `not_found`
/// or `ambiguous` for a seed that does not resolve.
fn resolve_seeds(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
    role: &str,
    name_aliases: &[&str],
    id_aliases: &[&str],
) -> CodeQueryResult<Vec<String>> {
    let mut ids = as_strings(config.get("node_ids"), "node_ids")?;
    if let Some(id) = truthy_text(config, "id") {
        ids.push(id);
    }
    for field in id_aliases {
        ids.extend(as_strings(config.get(*field), field)?);
    }
    let mut names = as_strings(config.get("names"), "names")?;
    if let Some(name) = truthy_text(config, "name") {
        names.push(name);
    }
    for field in name_aliases {
        names.extend(as_strings(config.get(*field), field)?);
    }
    let query = py_strip(query);
    if ids.is_empty() && names.is_empty() && !query.is_empty() {
        names.push(query.to_string());
    }
    let repo = config.get("repo");
    let mut resolved = Vec::with_capacity(ids.len() + names.len());
    for node_id in &ids {
        resolved.push(snapshot.resolve(None, Some(node_id), repo, role)?);
    }
    for name in &names {
        resolved.push(snapshot.resolve(Some(name), None, repo, role)?);
    }
    if resolved.is_empty() {
        return Err(CodeQueryError::invalid(
            format!("CODE {role} requires query_text, names, or node_ids."),
            "names",
        ));
    }
    Ok(sorted_unique(snapshot, resolved))
}

/// Node ids in the stable node order, each once (`sorted(set(ids),
/// key=node_sort_key)`).
fn sorted_unique(snapshot: &CodeGraphSnapshot, mut ids: Vec<String>) -> Vec<String> {
    ids.sort_by(|left, right| snapshot.node_order(left, right));
    // The sort key ends with the id, so equal ids are adjacent.
    ids.dedup();
    ids
}

/// `_traverse_graph`: a breadth-first walk from `seeds`, as the `nodes`,
/// `edges` and `stats` entries of a result.
///
/// The seeds (depth 0) are always listed, even past `max_nodes`. A
/// neighbour of a type outside `node_types` is walked through but not
/// listed; past `max_nodes` a new neighbour is neither listed nor walked
/// from, and `truncated` is set. Every edge followed counts in
/// `edges_traversed`; the edges listed are those between two listed nodes,
/// in edge order.
fn traverse_graph(
    snapshot: &CodeGraphSnapshot,
    seeds: &[String],
    walk: &Walk,
) -> Map<String, Value> {
    let seeds = sorted_unique(snapshot, seeds.to_vec());
    let mut visited: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut included: HashSet<&str> = visited.clone();
    let mut nodes: Vec<Value> = seeds
        .iter()
        .map(|node_id| snapshot.fact(node_id, Some(0), false))
        .collect();
    let mut queue: VecDeque<(&str, usize)> =
        seeds.iter().map(|node_id| (node_id.as_str(), 0)).collect();
    let mut traversed: HashMap<(&str, &str, &str), &SnapshotEdge> = HashMap::new();
    let mut max_depth_reached = 0usize;
    let mut truncated = nodes.len() > walk.max_nodes;

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= walk.max_depth {
            continue;
        }
        for (neighbor_id, edge) in snapshot.adjacent(node_id, walk.direction, walk.relation_types) {
            traversed
                .entry((&edge.source_id, &edge.target_id, &edge.relation))
                .or_insert(edge);
            if !visited.insert(neighbor_id) {
                continue;
            }
            let neighbor_depth = depth + 1;
            max_depth_reached = max_depth_reached.max(neighbor_depth);
            if let Some(node_types) = walk.node_types {
                let neighbor_type = snapshot.nodes[neighbor_id]
                    .get("type")
                    .map(py_str)
                    .unwrap_or_default();
                if !node_types.contains(&neighbor_type) {
                    queue.push_back((neighbor_id, neighbor_depth));
                    continue;
                }
            }
            if nodes.len() >= walk.max_nodes {
                truncated = true;
                continue;
            }
            included.insert(neighbor_id);
            nodes.push(snapshot.fact(neighbor_id, Some(neighbor_depth), false));
            queue.push_back((neighbor_id, neighbor_depth));
        }
    }

    let edges_traversed = traversed.len();
    let mut edges: Vec<&SnapshotEdge> = traversed
        .into_values()
        .filter(|edge| {
            included.contains(edge.source_id.as_str()) && included.contains(edge.target_id.as_str())
        })
        .collect();
    edges.sort_by(|left, right| snapshot.edge_order(left, right));
    let mut result = Map::new();
    result.insert("nodes".into(), Value::Array(nodes));
    result.insert(
        "edges".into(),
        Value::Array(
            edges
                .iter()
                .map(|edge| snapshot.edge_payload(edge))
                .collect(),
        ),
    );
    result.insert(
        "stats".into(),
        json!({
            "nodes_visited": visited.len(),
            "edges_traversed": edges_traversed,
            "max_depth_reached": max_depth_reached,
            "truncated": truncated,
        }),
    );
    result
}

/// `_reachable_count`: how many nodes (seeds excluded) a walk of
/// `max_depth` reaches, with no display cap.
fn reachable_count(
    snapshot: &CodeGraphSnapshot,
    seeds: &[String],
    direction: Direction,
    relation_types: Option<&HashSet<String>>,
    max_depth: usize,
) -> usize {
    let mut visited: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let seed_count = visited.len();
    let mut queue: VecDeque<(&str, usize)> =
        seeds.iter().map(|node_id| (node_id.as_str(), 0)).collect();
    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for (neighbor_id, _edge) in snapshot.adjacent(node_id, direction, relation_types) {
            if visited.insert(neighbor_id) {
                queue.push_back((neighbor_id, depth + 1));
            }
        }
    }
    visited.len() - seed_count
}

/// Python's `text.rsplit(separator, 1)[0]`: everything before the last
/// `separator`, or the whole text when there is none.
fn before_last(text: &str, separator: char) -> &str {
    text.rsplit_once(separator).map_or(text, |(head, _)| head)
}

/// `_impact_seeds`: the targets plus, for a type (a `CodeSymbol` whose
/// `symbol_kind` is struct, class, interface or type), its `has_method`
/// members and its constructor — `pkg.NewName` for `pkg.Name`, `NewName`
/// in the same directory for a bare `Name` — in the same repository and
/// package. In the stable node order.
pub(super) fn impact_seeds(snapshot: &CodeGraphSnapshot, targets: &[String]) -> Vec<String> {
    let mut seeds: HashSet<String> = targets.iter().cloned().collect();
    for target_id in targets {
        let node = &snapshot.nodes[target_id];
        let is_type = node.get("type").and_then(Value::as_str) == Some("CodeSymbol")
            && node
                .get("symbol_kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| TYPE_SYMBOL_KINDS.contains(&kind));
        if !is_type {
            continue;
        }
        seeds.extend(
            snapshot
                .forward(target_id)
                .filter(|edge| edge.relation == "has_method")
                .map(|edge| edge.target_id.clone()),
        );

        let name = py_str_truthy_or(node.get("name"), "");
        let file = file_of(node);
        let (package, constructor) = match name.rsplit_once('.') {
            Some((package, short_name)) => (package, format!("{package}.New{short_name}")),
            None => (before_last(&file, '/'), format!("New{name}")),
        };
        let repo = node.get("repo").unwrap_or(&Value::Null);
        for candidate_id in snapshot.by_name.get(&constructor).into_iter().flatten() {
            let candidate = &snapshot.nodes[candidate_id];
            let candidate_name = py_str_truthy_or(candidate.get("name"), "");
            let candidate_file = file_of(candidate);
            let candidate_package = if candidate_name.contains('.') {
                before_last(&candidate_name, '.')
            } else {
                before_last(&candidate_file, '/')
            };
            if candidate.get("repo").unwrap_or(&Value::Null) == repo && candidate_package == package
            {
                seeds.insert(candidate_id.clone());
            }
        }
    }
    sorted_unique(snapshot, seeds.into_iter().collect())
}

/// `_impact_summary`: one line on the dependents, e.g. `3 total dependents
/// (showing 2) — depth 1: 1 route, 1 symbol; depth 2: 1 symbol`.
fn impact_summary(by_depth: &Map<String, Value>, total: usize) -> String {
    if total == 0 {
        return "No dependents found.".to_string();
    }
    let mut shown = 0usize;
    let mut groups = Vec::new();
    for (depth, nodes) in by_depth {
        let nodes = nodes.as_array().map(Vec::as_slice).unwrap_or_default();
        shown += nodes.len();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for node in nodes {
            *counts
                .entry(py_str_truthy_or(node.get("kind"), "unknown"))
                .or_default() += 1;
        }
        let detail: Vec<String> = counts
            .iter()
            .map(|(kind, count)| {
                let plural = if *count == 1 { "" } else { "s" };
                format!("{count} {kind}{plural}")
            })
            .collect();
        groups.push(format!("depth {depth}: {}", detail.join(", ")));
    }
    let mut summary = format!("{total} total dependents");
    if shown < total {
        summary.push_str(&format!(" (showing {shown})"));
    }
    if !groups.is_empty() {
        summary.push_str(" — ");
        summary.push_str(&groups.join("; "));
    }
    summary
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn before_last_is_python_rsplit_head() {
        assert_eq!(before_last("pkg/widget.py", '/'), "pkg");
        assert_eq!(before_last("a/b/c.py", '/'), "a/b");
        assert_eq!(before_last("widget.py", '/'), "widget.py");
        assert_eq!(before_last("", '/'), "");
        assert_eq!(before_last("pkg.Widget", '.'), "pkg");
    }

    #[test]
    fn impact_summary_matches_python() {
        assert_eq!(impact_summary(&Map::new(), 0), "No dependents found.");
        let by_depth: Map<String, Value> = serde_json::from_value(json!({
            "1": [{"kind": "symbol"}, {"kind": "route"}, {"kind": "symbol"}],
            "2": [{"kind": ""}],
        }))
        .unwrap();
        assert_eq!(
            impact_summary(&by_depth, 6),
            "6 total dependents (showing 4) — depth 1: 1 route, 2 symbols; depth 2: 1 unknown"
        );
        // Everything shown: no "(showing n)".
        assert_eq!(
            impact_summary(&by_depth, 4),
            "4 total dependents — depth 1: 1 route, 2 symbols; depth 2: 1 unknown"
        );
        // Dependents counted past the walk's node types: no groups at all.
        assert_eq!(
            impact_summary(&Map::new(), 2),
            "2 total dependents (showing 0)"
        );
    }
}
