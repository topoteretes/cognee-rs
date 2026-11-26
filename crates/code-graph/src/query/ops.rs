//! The CODE operations over a [`CodeGraphSnapshot`]: `query_facts`,
//! `neighbors`, `subgraph`, `delta` and `capabilities`, Python-shaped (same
//! keys, order, defaults and bounds).
//!
//! Ports of `CodeRetriever._query_facts`, `_matches_property`, `_delta`,
//! `_capabilities`, `_neighbors`, `_subgraph` and `_missing_seed`
//! (`code_retriever.py:1419-1449, 1659-1776, 2338-2366, 2408-2669`).

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Map, Value, json};

use super::args::{
    CodeConfig, as_strings, bounded_int, count, direction, identifier_arg, node_type, node_types,
    offset, relation_types, truthy_text,
};
use super::error::{CodeErrorCode, CodeQueryError, CodeQueryResult};
use super::normalize::{normalize_value, scalar_text};
use super::snapshot::{
    CodeGraphSnapshot, FACT_TYPE_BY_KIND, NodeProperties, SnapshotEdge, file_of, int_or_none,
    kind_of_type, properties_of,
};
use super::{CAPABILITIES_VERSION, CodeOperation, limits_json};
use crate::pyjson::{casefold, is_truthy, py_str, py_strip};

/// `_LIMITS["neighbors"]["limit"]`.
const NEIGHBORS_LIMIT: (usize, usize) = (50, 200);
/// `_LIMITS["subgraph"]`: seeds, depth, max_nodes and max_edges.
const SUBGRAPH_SEEDS: usize = 20;
const SUBGRAPH_DEPTH: (usize, usize) = (1, 3);
const SUBGRAPH_MAX_NODES: (usize, usize) = (100, 500);
const SUBGRAPH_MAX_EDGES: (usize, usize) = (300, 1000);

/// `_delta`: what the last ingestion changed, per repository.
///
/// Reads the `last_delta` record the code-graph writer stamps on each
/// `CodeRepository` node after a completed load and sweep, with the snapshot
/// id and the receipt projection. A repository loaded before stamping
/// existed reports `delta: null`. `repo` (else `name`, else the query text)
/// keeps the repositories whose name contains it, casefolded.
pub fn delta(snapshot: &CodeGraphSnapshot, config: &CodeConfig, query: &str) -> Value {
    let repo_filter = ["repo", "name"]
        .iter()
        .filter_map(|field| config.get(*field))
        .find(|value| is_truthy(value))
        .map(py_str)
        .unwrap_or_else(|| query.to_string());
    let repo_filter = casefold(py_strip(&repo_filter));
    let repositories: Vec<Value> = snapshot
        .repositories
        .iter()
        .filter_map(|(node_id, node)| {
            let repo_name = match node.get("name") {
                Some(name) if is_truthy(name) => py_str(name),
                _ => String::new(),
            };
            if !repo_filter.is_empty() && !casefold(&repo_name).contains(&repo_filter) {
                return None;
            }
            Some(json!({
                "id": node_id,
                "repo": repo_name,
                "last_snapshot_id": node.get("last_snapshot_id").cloned().unwrap_or(Value::Null),
                "delta": node.get("last_delta").cloned().unwrap_or(Value::Null),
                "receipt": normalize_value(node.get("last_receipt").unwrap_or(&Value::Null)),
            }))
        })
        .collect();
    json!({"operation": "delta", "repositories": repositories})
}

/// `_capabilities`: what this build can do and what the dataset's code graph
/// holds. Never fails, also not for a dataset without a code graph (the
/// counts are then zero). `operations` is all fifteen, sorted
/// ([`CodeOperation::ALL`], Python's `sorted(_OPERATIONS)`), and `limits`
/// the defaults and caps of the navigation operations ([`limits_json`]).
pub fn capabilities(snapshot: &CodeGraphSnapshot) -> Value {
    let empty = HashSet::new();
    let symbols = snapshot.by_type.get("CodeSymbol").unwrap_or(&empty);
    let mut versions: Vec<String> = snapshot
        .repositories
        .values()
        .filter_map(|repository| {
            let receipt = normalize_value(repository.get("last_receipt").unwrap_or(&Value::Null));
            match receipt.get("enola_version") {
                Some(Value::String(version)) if !version.is_empty() => Some(version.clone()),
                _ => None,
            }
        })
        .collect();
    versions.sort();
    versions.dedup();
    let operations: Vec<&str> = CodeOperation::ALL
        .iter()
        .map(|operation| operation.as_str())
        .collect();
    json!({
        "operation": "capabilities",
        "version": CAPABILITIES_VERSION,
        "operations": operations,
        "features": {
            "docs": !snapshot.documented.is_empty(),
            "flows": !snapshot.flows.is_empty(),
        },
        "counts": {
            "nodes": snapshot.nodes.len(),
            "edges": snapshot.edges.len(),
            "symbols": symbols.len(),
            "symbols_with_docs": snapshot.documented.intersection(symbols).count(),
            "flows": snapshot.flows.len(),
            "repositories": snapshot.repositories.len(),
        },
        "enola_versions": versions,
        "limits": limits_json(),
    })
}

/// `_query_facts`: the facts matching every given filter, in node order and
/// paged.
///
/// Filters: `kinds`/`kind`, `files`/`file` (or `file_prefix`), `names` (or
/// a casefolded substring `name`), `repo`, `relation_types` (a fact with an
/// outgoing edge of one of them), and `property`/`prop` with an optional
/// `property_value`/`prop_value` (dotted names reach into objects). The query
/// text is the `name` substring only for a call that configures nothing but
/// the operation. `limit` (100, max 500) and `offset` page the result.
///
/// # Errors
///
/// `invalid_argument` for a malformed filter or page.
pub fn query_facts(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let mut kinds = as_strings(config.get("kinds"), "kinds")?;
    if let Some(kind) = truthy_text(config, "kind") {
        kinds.push(kind);
    }
    let type_filter: Option<HashSet<String>> = if kinds.is_empty() {
        None
    } else {
        Some(
            kinds
                .iter()
                .map(|kind| node_type(kind, "kinds"))
                .collect::<CodeQueryResult<_>>()?,
        )
    };
    let mut files: HashSet<String> = as_strings(config.get("files"), "files")?
        .into_iter()
        .collect();
    if let Some(file) = truthy_text(config, "file") {
        files.insert(file);
    }
    let names: HashSet<String> = as_strings(config.get("names"), "names")?
        .into_iter()
        .collect();
    // The query text is a shorthand only for an otherwise unconfigured call:
    // API and tool callers often must send a generic non-empty query even
    // when their structured filters are complete.
    let query_name = if config.keys().all(|key| key == "operation") {
        query
    } else {
        ""
    };
    let substring = casefold(py_strip(
        &truthy_text(config, "name").unwrap_or_else(|| query_name.to_string()),
    ));
    let file_prefix = truthy_text(config, "file_prefix").unwrap_or_default();
    let repo = config.get("repo").filter(|repo| !repo.is_null());
    let relation_types = relation_types(config)?;
    let property_name = ["property", "prop"]
        .iter()
        .filter_map(|field| config.get(*field))
        .find(|value| is_truthy(value))
        .map(py_str);
    let expected_property = if config.contains_key("property_value") {
        config.get("property_value")
    } else {
        config.get("prop_value")
    };

    let empty = HashSet::new();
    let mut indexed_sets: Vec<HashSet<&String>> = Vec::new();
    if let Some(type_filter) = &type_filter {
        indexed_sets.push(
            type_filter
                .iter()
                .flat_map(|node_type| snapshot.by_type.get(node_type).unwrap_or(&empty))
                .collect(),
        );
    }
    if !files.is_empty() && file_prefix.is_empty() {
        // With file_prefix the file condition is a union (exact file OR
        // prefix), which the exact-file index cannot express.
        indexed_sets.push(
            files
                .iter()
                .flat_map(|file| snapshot.by_file.get(file).unwrap_or(&empty))
                .collect(),
        );
    }
    if !names.is_empty() && substring.is_empty() {
        indexed_sets.push(
            names
                .iter()
                .flat_map(|name| snapshot.by_name.get(name).unwrap_or(&empty))
                .collect(),
        );
    }
    if let Some(repo) = repo {
        indexed_sets.push(
            snapshot
                .by_repo
                .get(&py_str(repo))
                .unwrap_or(&empty)
                .iter()
                .collect(),
        );
    }
    if let Some(property_name) = &property_name {
        let buckets = snapshot.by_property.get(property_name);
        indexed_sets.push(match expected_property {
            None => buckets
                .into_iter()
                .flat_map(|buckets| buckets.values().flatten())
                .collect(),
            Some(expected) => buckets
                .and_then(|buckets| buckets.get(&scalar_text(expected)))
                .unwrap_or(&empty)
                .iter()
                .collect(),
        });
    }
    let candidates: HashSet<&String> = match indexed_sets
        .into_iter()
        .reduce(|left, right| left.intersection(&right).copied().collect())
    {
        Some(candidates) => candidates,
        None => snapshot.nodes.keys().collect(),
    };
    let mut candidates: Vec<&String> = candidates.into_iter().collect();
    candidates.sort_by(|left, right| snapshot.node_order(left, right));

    let mut matched = Vec::new();
    for node_id in candidates {
        let node = &snapshot.nodes[node_id];
        let node_type = node.get("type").map(py_str).unwrap_or_default();
        if type_filter
            .as_ref()
            .is_some_and(|types| !types.contains(&node_type))
        {
            continue;
        }
        let file_name = file_of(node);
        let file_filtered = !files.is_empty() || !file_prefix.is_empty();
        let file_matches = files.contains(&file_name)
            || (!file_prefix.is_empty() && file_name.starts_with(&file_prefix));
        if file_filtered && !file_matches {
            continue;
        }
        let node_name = match node.get("name") {
            Some(name) if is_truthy(name) => py_str(name),
            _ => String::new(),
        };
        let name_filtered = !names.is_empty() || !substring.is_empty();
        let name_matches = names.contains(&node_name)
            || (!substring.is_empty() && casefold(&node_name).contains(&substring));
        if name_filtered && !name_matches {
            continue;
        }
        if let Some(repo) = repo
            && node.get("repo") != Some(repo)
        {
            continue;
        }
        if let Some(relation_types) = &relation_types
            && !snapshot
                .forward(node_id)
                .any(|edge| relation_types.contains(&edge.relation))
        {
            continue;
        }
        if let Some(property_name) = &property_name
            && !matches_property(node, property_name, expected_property)
        {
            continue;
        }
        matched.push(node_id);
    }

    let total = matched.len();
    let offset = offset(config.get("offset"))?;
    let limit = bounded_int(config.get("limit"), "limit", 100, 500)?;
    let page: Vec<&&String> = matched.iter().skip(offset).take(limit).collect();
    let facts: Vec<Value> = page
        .iter()
        .map(|node_id| snapshot.fact(node_id, None, true))
        .collect();
    Ok(json!({
        "operation": "query_facts",
        "facts": facts,
        "total": total,
        "offset": offset,
        "limit": limit,
        "has_more": offset.saturating_add(page.len()) < total,
    }))
}

/// `_matches_property`: whether the node (or its `fact_properties`) holds
/// `property_name` (dotted into objects), with `expected` as its scalar text
/// when given.
fn matches_property(node: &NodeProperties, property_name: &str, expected: Option<&Value>) -> bool {
    [node, properties_of(node)].into_iter().any(|source| {
        let mut value: Option<&Value> = None;
        let mut current: &NodeProperties = source;
        for (index, part) in property_name.split('.').enumerate() {
            if index > 0 {
                match value {
                    Some(Value::Object(object)) => current = object,
                    _ => return false,
                }
            }
            match current.get(part) {
                Some(found) => value = Some(found),
                None => return false,
            }
        }
        match (value, expected) {
            (Some(_), None) => true,
            (Some(found), Some(expected)) => scalar_text(found) == scalar_text(expected),
            (None, _) => false,
        }
    })
}

/// `_neighbors`: every edge of one node, as (relation, direction, neighbour)
/// entries.
///
/// The anchor comes from `name`, `id`/`node_id` or the query text.
/// `direction` (both; forward lists `out` edges, reverse `in` edges),
/// `relation_types` and `kinds` (neighbour kinds) filter; entries are ordered
/// by relation, direction and the stable node order, and paged by `limit`
/// (50, max 200) and `offset`. `flows` names the flows the anchor is the
/// entry, a stage or a route of.
///
/// # Errors
///
/// `invalid_argument` for a malformed argument (checked before the anchor is
/// resolved), `not_found` / `ambiguous` for an anchor that does not resolve.
pub fn neighbors(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    // Arguments first: a bad one is a usage error whether or not the anchor
    // resolves.
    let direction = direction(config.get("direction"), "both")?;
    let relation_types = relation_types(config)?;
    let kinds = node_types(config.get("kinds"), "kinds")?;
    let limit = count(
        config.get("limit"),
        "limit",
        NEIGHBORS_LIMIT.0,
        NEIGHBORS_LIMIT.1,
    )?;
    let offset = offset(config.get("offset"))?;
    let name = identifier_arg(config, &["name"])?;
    let node_id = identifier_arg(config, &["id", "node_id"])?;
    let repo = identifier_arg(config, &["repo"])?.map(Value::String);
    let anchor = snapshot.resolve(
        Some(name.as_deref().unwrap_or(query)),
        node_id.as_deref(),
        repo.as_ref(),
        "neighbors anchor",
    )?;

    let mut entries: Vec<(&str, &str, &String)> = Vec::new();
    let mut passes: Vec<(&str, Vec<&SnapshotEdge>, bool)> = Vec::new();
    if direction.forward() {
        passes.push(("out", snapshot.forward(&anchor).collect(), true));
    }
    if direction.reverse() {
        passes.push(("in", snapshot.reverse(&anchor).collect(), false));
    }
    for (edge_direction, edges, outgoing) in &passes {
        for edge in edges {
            let neighbor_id = if *outgoing {
                &edge.target_id
            } else {
                &edge.source_id
            };
            if relation_types
                .as_ref()
                .is_some_and(|types| !types.contains(&edge.relation))
            {
                continue;
            }
            if let Some(kinds) = &kinds {
                let neighbor_type = snapshot.nodes[neighbor_id]
                    .get("type")
                    .map(py_str)
                    .unwrap_or_default();
                if !kinds.contains(&neighbor_type) {
                    continue;
                }
            }
            entries.push((edge.relation.as_str(), edge_direction, neighbor_id));
        }
    }
    entries.sort_by(|left, right| {
        (left.0, left.1)
            .cmp(&(right.0, right.1))
            .then_with(|| snapshot.node_order(left.2, right.2))
    });
    let total = entries.len();
    let page: Vec<Value> = entries
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(relation, edge_direction, neighbor_id)| {
            json!({
                "relation": relation,
                "direction": edge_direction,
                "node": snapshot.fact(neighbor_id, None, false),
            })
        })
        .collect();
    let has_more = offset.saturating_add(page.len()) < total;
    let mut result = Map::new();
    result.insert("operation".into(), json!("neighbors"));
    result.insert("anchor".into(), snapshot.fact(&anchor, None, false));
    result.insert("neighbors".into(), Value::Array(page));
    let flows: Vec<Value> = snapshot
        .flows_by_node
        .get(&anchor)
        .into_iter()
        .flatten()
        .map(|(flow_id, role)| {
            json!({
                "id": flow_id,
                "name": snapshot.flows[flow_id].get("name").cloned().unwrap_or(Value::Null),
                "as": role.as_str(),
            })
        })
        .collect();
    if !flows.is_empty() {
        result.insert("flows".into(), Value::Array(flows));
    }
    result.insert("total".into(), json!(total));
    result.insert("offset".into(), json!(offset));
    result.insert("limit".into(), json!(limit));
    result.insert("has_more".into(), json!(has_more));
    result.insert("truncated".into(), json!(has_more));
    Ok(Value::Object(result))
}

/// A requested subgraph seed: a name or an id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SeedKind {
    Name,
    Id,
}

/// `_subgraph`: the graph around up to 20 seeds, with every cut reported.
///
/// Seeds come from `seeds`/`names` (names), `seed_ids`/`node_ids`/`ids`
/// (cognee or enola ids), else the query text. `depth` (1, max 3;
/// `max_depth` is an alias), `direction` (both), `relation_types`, `kinds`
/// (node kinds to include; other nodes are walked through but not
/// returned), `max_nodes` (100, max 500; the seeds count towards it),
/// `max_edges` (300, max 1000) and `repo` shape it. Seeds that do not
/// resolve go to `seeds_missing` with candidates; only when none resolves
/// does the call fail. `truncation_reasons` lists every cut, in order:
/// `max_seeds`, `max_depth`, `max_nodes`, `max_edges`.
///
/// # Errors
///
/// `invalid_argument` for a malformed argument or no seed at all;
/// `not_found` (or `ambiguous`, when every seed was ambiguous) when no seed
/// resolves.
pub fn subgraph(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let mut requested: Vec<(SeedKind, String)> = Vec::new();
    for field in ["seeds", "names"] {
        requested.extend(
            as_strings(config.get(field), field)?
                .into_iter()
                .map(|value| (SeedKind::Name, value)),
        );
    }
    for field in ["seed_ids", "node_ids", "ids"] {
        requested.extend(
            as_strings(config.get(field), field)?
                .into_iter()
                .map(|value| (SeedKind::Id, value)),
        );
    }
    let query_seed = py_strip(query);
    if requested.is_empty() && !query_seed.is_empty() {
        requested.push((SeedKind::Name, query_seed.to_string()));
    }
    let mut seen = HashSet::new();
    requested.retain(|seed| seen.insert(seed.clone()));
    if requested.is_empty() {
        return Err(CodeQueryError::invalid(
            "CODE subgraph requires seeds, seed_ids, or query text.",
            "seeds",
        ));
    }
    let mut reasons: Vec<&str> = Vec::new();
    if requested.len() > SUBGRAPH_SEEDS {
        requested.truncate(SUBGRAPH_SEEDS);
        reasons.push("max_seeds");
    }
    let depth_field = if config.get("depth").is_some_and(|value| !value.is_null()) {
        "depth"
    } else {
        "max_depth"
    };
    let raw_depth = config.get(depth_field);
    let depth = count(raw_depth, depth_field, SUBGRAPH_DEPTH.0, SUBGRAPH_DEPTH.1)?;
    if int_or_none(raw_depth).unwrap_or(0) > SUBGRAPH_DEPTH.1 as i64
        || raw_depth.is_some_and(|value| value.is_u64() && value.as_i64().is_none())
    {
        reasons.push("max_depth");
    }
    let max_nodes = count(
        config.get("max_nodes"),
        "max_nodes",
        SUBGRAPH_MAX_NODES.0,
        SUBGRAPH_MAX_NODES.1,
    )?;
    let max_edges = count(
        config.get("max_edges"),
        "max_edges",
        SUBGRAPH_MAX_EDGES.0,
        SUBGRAPH_MAX_EDGES.1,
    )?;
    let direction = direction(config.get("direction"), "both")?;
    let relation_types = relation_types(config)?;
    let kinds = match node_types(config.get("kinds"), "kinds")? {
        Some(kinds) => Some(kinds),
        None => node_types(config.get("node_types"), "node_types")?,
    };
    let repo = config.get("repo").filter(|repo| is_truthy(repo));

    let mut seeds: Vec<String> = Vec::new();
    let mut missing: Vec<(String, SeedKind, CodeQueryError)> = Vec::new();
    for (seed_kind, value) in requested {
        let resolved = match seed_kind {
            SeedKind::Name => snapshot.resolve(Some(&value), None, repo, "subgraph seed"),
            SeedKind::Id => snapshot.resolve(None, Some(&value), repo, "subgraph seed"),
        };
        match resolved {
            Ok(node_id) => {
                if !seeds.contains(&node_id) {
                    seeds.push(node_id);
                }
            }
            Err(error) if error.is_seed_miss() => missing.push((value, seed_kind, error)),
            Err(error) => return Err(error),
        }
    }
    if seeds.is_empty() {
        let all_ambiguous = missing
            .iter()
            .all(|(_, _, error)| error.code == CodeErrorCode::Ambiguous);
        let message = missing
            .iter()
            .map(|(_, _, error)| error.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(CodeQueryError::seed_miss(
            message,
            if all_ambiguous && !missing.is_empty() {
                CodeErrorCode::Ambiguous
            } else {
                CodeErrorCode::NotFound
            },
            "seeds",
        ));
    }

    // max_nodes counts the seeds too: past it, the later seeds are cut.
    let mut node_limit_reached = seeds.len() > max_nodes;
    seeds.truncate(max_nodes);
    let mut included: HashMap<String, usize> = seeds.iter().map(|seed| (seed.clone(), 0)).collect();
    let mut visited: HashSet<String> = seeds.iter().cloned().collect();
    let mut queue: VecDeque<(String, usize)> = seeds.iter().map(|seed| (seed.clone(), 0)).collect();
    let mut traversed: Vec<&SnapshotEdge> = Vec::new();
    let mut traversed_keys: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut edges_considered = 0usize;
    let mut depth_reached = 0usize;
    while let Some((node_id, node_depth)) = queue.pop_front() {
        if node_depth >= depth {
            continue;
        }
        let mut incident: Vec<(&String, &SnapshotEdge)> = Vec::new();
        if direction.forward() {
            incident.extend(
                snapshot
                    .forward(&node_id)
                    .map(|edge| (&edge.target_id, edge)),
            );
        }
        if direction.reverse() {
            incident.extend(
                snapshot
                    .reverse(&node_id)
                    .map(|edge| (&edge.source_id, edge)),
            );
        }
        for (neighbor_id, edge) in incident {
            if relation_types
                .as_ref()
                .is_some_and(|types| !types.contains(&edge.relation))
            {
                continue;
            }
            edges_considered += 1;
            if traversed_keys.insert((&edge.source_id, &edge.target_id, &edge.relation)) {
                traversed.push(edge);
            }
            if !visited.insert(neighbor_id.clone()) {
                continue;
            }
            depth_reached = depth_reached.max(node_depth + 1);
            if let Some(kinds) = &kinds {
                let neighbor_type = snapshot.nodes[neighbor_id]
                    .get("type")
                    .map(py_str)
                    .unwrap_or_default();
                if !kinds.contains(&neighbor_type) {
                    queue.push_back((neighbor_id.clone(), node_depth + 1));
                    continue;
                }
            }
            if included.len() >= max_nodes {
                node_limit_reached = true;
                continue;
            }
            included.insert(neighbor_id.clone(), node_depth + 1);
            queue.push_back((neighbor_id.clone(), node_depth + 1));
        }
    }
    if node_limit_reached {
        reasons.push("max_nodes");
    }

    // Under the edge limit, keep the edges nearest the seeds.
    let mut edges: Vec<&SnapshotEdge> = traversed
        .into_iter()
        .filter(|edge| {
            included.contains_key(&edge.source_id) && included.contains_key(&edge.target_id)
        })
        .collect();
    let edge_depth = |edge: &SnapshotEdge| included[&edge.source_id].max(included[&edge.target_id]);
    edges.sort_by(|left, right| {
        edge_depth(left)
            .cmp(&edge_depth(right))
            .then_with(|| snapshot.edge_order(left, right))
    });
    if edges.len() > max_edges {
        edges.truncate(max_edges);
        reasons.push("max_edges");
    }
    edges.sort_by(|left, right| snapshot.edge_order(left, right));

    let mut nodes: Vec<&String> = included.keys().collect();
    nodes.sort_by(|left, right| {
        included[*left]
            .cmp(&included[*right])
            .then_with(|| snapshot.node_order(left, right))
    });
    let seeds_missing: Vec<Value> = missing
        .iter()
        .map(|(value, seed_kind, error)| missing_seed(snapshot, value, seed_kind, error, repo))
        .collect();
    Ok(json!({
        "operation": "subgraph",
        "seeds": seeds.iter().map(|seed| snapshot.fact(seed, None, false)).collect::<Vec<_>>(),
        "seeds_missing": seeds_missing,
        "nodes": nodes
            .iter()
            .map(|node_id| snapshot.fact(node_id, Some(included[*node_id]), false))
            .collect::<Vec<_>>(),
        "edges": edges.iter().map(|edge| snapshot.edge_payload(edge)).collect::<Vec<_>>(),
        "limits": {
            "seeds": SUBGRAPH_SEEDS,
            "depth": depth,
            "max_nodes": max_nodes,
            "max_edges": max_edges,
        },
        "truncated": !reasons.is_empty(),
        "truncation_reason": reasons.first(),
        "truncation_reasons": reasons,
        "stats": {
            "nodes_visited": visited.len(),
            "edges_considered": edges_considered,
            "depth_reached": depth_reached,
        },
    }))
}

/// `_missing_seed`: a seed that did not resolve, with up to five nodes it
/// may have meant — the equally good name matches of an ambiguous name, the
/// text-search hits of a name that matched nothing, none for an id.
fn missing_seed(
    snapshot: &CodeGraphSnapshot,
    value: &str,
    seed_kind: &SeedKind,
    error: &CodeQueryError,
    repo: Option<&Value>,
) -> Value {
    let candidates: Vec<String> = if error.code == CodeErrorCode::Ambiguous {
        snapshot.name_matches(value, repo)
    } else if *seed_kind == SeedKind::Name {
        let fact_types: HashSet<String> = FACT_TYPE_BY_KIND
            .iter()
            .map(|(_, node_type)| (*node_type).to_string())
            .collect();
        let (_terms, ranked) =
            snapshot.text_matches(value, Some(&fact_types), repo, "", true, Some(5));
        ranked.into_iter().map(|(key, _, _)| key).collect()
    } else {
        Vec::new()
    };
    let candidates: Vec<Value> = candidates
        .iter()
        .take(5)
        .filter_map(|node_id| {
            let node = snapshot.nodes.get(node_id)?;
            let file = file_of(node);
            Some(json!({
                "id": node_id,
                "name": node.get("name").cloned().unwrap_or(Value::Null),
                "kind": node.get("type").and_then(Value::as_str).and_then(kind_of_type),
                "file": if file.is_empty() { Value::Null } else { Value::String(file) },
            }))
        })
        .collect();
    json!({
        "seed": value,
        "reason": error.code.as_str(),
        "candidates": candidates,
    })
}
