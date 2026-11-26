//! The text and flow operations of CODE: `search`, `flows`, `hubs` and
//! `ask`, Python-shaped (same keys, order, defaults, bounds and messages).
//!
//! Ports of `CodeRetriever._text_arg`, `_search_kinds`, `_hit`, `_search`,
//! `_flows`, `_resolve_flow`, `_flows_through`, `_hubs`, `_ask`,
//! `_ask_flows`, `_seed_links`, `_ask_neighborhood`, `_ask_citations` and
//! `_ask_next` (`code_retriever.py:2181-2236, 2308-2406, 2671-3261`). The
//! ranking itself is [`CodeGraphSnapshot::text_matches`] over the BM25 index
//! of [`super::text_index`]. Nothing here is written by a model.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::args::{
    CodeConfig, as_strings, count, direction, identifier_arg, node_type, node_types, offset,
    optional_bool, relation_types,
};
use super::error::{CodeErrorCode, CodeQueryError, CodeQueryResult};
use super::flows::FlowReach;
use super::snapshot::{
    CodeGraphSnapshot, FACT_TYPE_BY_KIND, FLOW_TYPE, NodeProperties, SnapshotEdge, TextMatch,
    doc_of, file_of, int_or_none, is_none_or_empty, kind_of_type, properties_of,
};
use crate::pyjson::{casefold, is_truthy, py_repr_str, py_str, py_str_truthy_or, py_strip};

/// `_LIMITS["search"]["limit"]`.
const SEARCH_LIMIT: (usize, usize) = (10, 50);
/// `_LIMITS["flows"]["limit"]`.
const FLOWS_LIMIT: (usize, usize) = (20, 50);
/// `_LIMITS["hubs"]["limit"]`.
const HUBS_LIMIT: (usize, usize) = (20, 100);
/// `_LIMITS["ask"]`: max_seeds, max_nodes and max_citations.
const ASK_MAX_SEEDS: (usize, usize) = (5, 10);
const ASK_MAX_NODES: (usize, usize) = (60, 200);
const ASK_MAX_CITATIONS: (usize, usize) = (25, 60);
/// `_ASK_MAX_FLOWS`: the flows an answer follows.
const ASK_MAX_FLOWS: usize = 3;
/// `_SEARCH_DEFAULT_KINDS`: what a text search covers unless `kinds` says.
const SEARCH_DEFAULT_KINDS: [&str; 6] = ["symbol", "route", "flow", "module", "storage", "service"];
/// `_CONTAINMENT_EDGE_TYPES`: edges that say where a fact lives rather than
/// what it does. `hubs` and `ask`'s neighbourhood skip them unless
/// `relation_types` names them (`has_method` stays: a type's methods are its
/// behaviour).
const CONTAINMENT_EDGE_TYPES: [&str; 3] = ["part_of", "declares", "evidences"];
/// `_ROUTE_EDGE_TYPES`: the edges that link a symbol to a route.
const ROUTE_EDGE_TYPES: [&str; 3] = ["handled_by", "makes_request", "reaches_route"];

/// `_NO_CODE_GRAPH_MESSAGE`.
pub const NO_CODE_GRAPH_MESSAGE: &str = "CODE found no code graph in this dataset. Index a \
     repository into it first (add or remember the repository, then cognify), or search the \
     dataset that holds it.";
/// `_NO_FLOWS_MESSAGE`.
pub const NO_FLOWS_MESSAGE: &str = "CODE flows are not available: this dataset's code graph has \
     no flows. Flows come from Enola's flows.json, which an Enola release with flows support \
     writes; re-index the repository with such a release to add them.";

fn is_containment(relation: &str) -> bool {
    CONTAINMENT_EDGE_TYPES.contains(&relation)
}

/// `_text_arg`: the first non-blank string among `fields`, stripped, else the
/// stripped query text.
///
/// # Errors
///
/// `invalid_argument` for a field that is set to anything but a string.
fn text_arg(config: &CodeConfig, query: &str, fields: &[&str]) -> CodeQueryResult<String> {
    for field in fields {
        let Some(value) = config.get(*field).filter(|value| !value.is_null()) else {
            continue;
        };
        let Value::String(text) = value else {
            return Err(CodeQueryError::invalid(
                format!("CODE {field} must be a string."),
                field,
            ));
        };
        let stripped = py_strip(text);
        if !stripped.is_empty() {
            return Ok(stripped.to_string());
        }
    }
    Ok(py_strip(query).to_string())
}

/// `_include_tests`: `include_tests` as a boolean, false when missing.
fn include_tests(config: &CodeConfig) -> CodeQueryResult<bool> {
    Ok(optional_bool(config.get("include_tests"), "include_tests")?.unwrap_or(false))
}

/// `config.get(field) or None`: a truthy configured value, as is.
fn truthy<'a>(config: &'a CodeConfig, field: &str) -> Option<&'a Value> {
    config.get(field).filter(|value| is_truthy(value))
}

/// `str(config.get("file_prefix") or "")`.
fn file_prefix(config: &CodeConfig) -> String {
    py_str_truthy_or(config.get("file_prefix"), "")
}

/// `_search_kinds`: the node types a text search covers, flows included;
/// `None` means every kind. `kinds` defaults to symbol, route, flow,
/// module, storage and service; `all` among them means every kind.
///
/// # Errors
///
/// `invalid_argument` (param `kinds`) for a malformed list or an unknown
/// kind, naming `flow` and `all` among the choices.
fn search_kinds(config: &CodeConfig) -> CodeQueryResult<Option<HashSet<String>>> {
    let mut kinds = as_strings(config.get("kinds"), "kinds")?;
    if kinds.is_empty() {
        kinds = SEARCH_DEFAULT_KINDS
            .iter()
            .map(ToString::to_string)
            .collect();
    }
    if kinds
        .iter()
        .any(|kind| py_strip(kind).to_lowercase() == "all")
    {
        return Ok(None);
    }
    let mut types = HashSet::new();
    for kind in &kinds {
        if matches!(py_strip(kind).to_lowercase().as_str(), "flow" | "codeflow") {
            types.insert(FLOW_TYPE.to_string());
            continue;
        }
        match node_type(kind, "kinds") {
            Ok(node_type) => {
                types.insert(node_type);
            }
            Err(_) => {
                // The fact kinds' own message leaves out what only a text
                // search takes.
                let mut choices: Vec<&str> = FACT_TYPE_BY_KIND
                    .iter()
                    .map(|(kind, _)| *kind)
                    .chain(["flow"])
                    .collect();
                choices.sort_unstable();
                return Err(CodeQueryError::invalid(
                    format!(
                        "Unsupported CODE node kind/type {}; expected one of {}, or all.",
                        py_repr_str(kind),
                        choices.join(", ")
                    ),
                    "kinds",
                ));
            }
        }
    }
    Ok(Some(types))
}

/// `_hit`: one ranked fact or flow as `search` and `ask` list it.
fn hit(snapshot: &CodeGraphSnapshot, (key, score, fields): &TextMatch) -> Value {
    let empty = NodeProperties::new();
    let entity = snapshot.entity(key).unwrap_or(&empty);
    let field = |name: &str| entity.get(name).cloned().unwrap_or(Value::Null);
    let node_type = entity.get("type").map(py_str).unwrap_or_default();
    let file = file_of(entity);
    let mut result = Map::new();
    result.insert("id".into(), json!(key));
    result.insert("kind".into(), json!(kind_of_type(&node_type)));
    result.insert("type".into(), json!(node_type));
    result.insert("name".into(), field("name"));
    result.insert(
        "file".into(),
        if file.is_empty() {
            Value::Null
        } else {
            Value::String(file)
        },
    );
    result.insert("line".into(), field("line"));
    result.insert("repo".into(), field("repo"));
    for (name, value) in [
        ("end_line", entity.get("end_line").cloned()),
        ("symbol_kind", entity.get("symbol_kind").cloned()),
        ("doc", doc_of(entity).map(Value::String)),
    ] {
        if let Some(value) = value
            && !is_none_or_empty(&value)
        {
            result.insert(name.into(), value);
        }
    }
    result.insert("score".into(), json!(score));
    result.insert("matched".into(), json!(fields));
    if snapshot.flows.contains_key(key) {
        let properties = properties_of(entity);
        let property = |name: &str| properties.get(name).cloned().unwrap_or(Value::Null);
        result.insert("mechanism".into(), property("mechanism"));
        result.insert("rank".into(), property("rank"));
        result.insert(
            "summary".into(),
            snapshot
                .flow_summary_text(key)
                .map_or(Value::Null, Value::String),
        );
        result.insert("enola_id".into(), field("enola_id"));
    }
    Value::Object(result)
}

/// `_search`: rank facts and flows against free text, without embeddings.
///
/// The text is `text`, `query` or `q`, else the query text. BM25 over each
/// fact's qualified name (weight 3), docstring (2) and file path (1), and a
/// flow's summary (1), plus a bonus for an exact or short name match.
/// `kinds` (default symbol, route, flow, module, storage, service; `all`
/// for every kind), `repo`, `file_prefix` and `include_tests` (default
/// false) narrow it; `limit` (10, max 50) and `offset` page it. No match is
/// an empty `hits` list.
///
/// # Errors
///
/// `invalid_argument` for no text or a malformed argument.
pub fn search(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let text = text_arg(config, query, &["text", "query", "q"])?;
    if text.is_empty() {
        return Err(CodeQueryError::invalid(
            "CODE search requires text or query text.",
            "text",
        ));
    }
    let kinds = search_kinds(config)?;
    let include_tests = include_tests(config)?;
    let limit = count(config.get("limit"), "limit", SEARCH_LIMIT.0, SEARCH_LIMIT.1)?;
    let offset = offset(config.get("offset"))?;
    let (terms, ranked) = snapshot.text_matches(
        &text,
        kinds.as_ref(),
        truthy(config, "repo"),
        &file_prefix(config),
        include_tests,
        None,
    );
    let page: Vec<Value> = ranked
        .iter()
        .skip(offset)
        .take(limit)
        .map(|item| hit(snapshot, item))
        .collect();
    let has_more = offset.saturating_add(page.len()) < ranked.len();
    Ok(json!({
        "operation": "search",
        "query": text,
        "terms": terms,
        "hits": page,
        "total": ranked.len(),
        "offset": offset,
        "limit": limit,
        "has_more": has_more,
    }))
}

/// What `flows` returns: a ranked list, one flow, or the flows through a
/// symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowsMode {
    List,
    Show,
    Through,
}

/// `_flows`: Enola's flows — the ways into the program and what each one
/// reaches.
///
/// `mode` `list` (the default) ranks them as Enola does (`repo`,
/// `mechanism`, `include_side` (false), `limit` (20, max 50), `offset`);
/// `show` (the default when `id`, `name` or `flow` is given) returns one
/// flow with its stages; `through` (the default when `symbol` or `through`
/// is given) lists the flows whose calls reach a symbol.
///
/// # Errors
///
/// `invalid_argument` for a malformed argument, checked before anything
/// else; `no_code_graph` for a dataset without a code graph and
/// `feature_unavailable` (param `operation`) for one without flows (they
/// come from Enola's `flows.json`); `not_found` / `ambiguous` for a flow or
/// symbol that does not resolve.
pub fn flows(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let mode = match config.get("mode").filter(|mode| !mode.is_null()) {
        Some(mode) => match py_strip(&py_str(mode)).to_lowercase().as_str() {
            "list" => FlowsMode::List,
            "show" => FlowsMode::Show,
            "through" => FlowsMode::Through,
            _ => {
                return Err(CodeQueryError::invalid(
                    "CODE flows mode must be 'list', 'show', or 'through'.",
                    "mode",
                ));
            }
        },
        None if truthy(config, "symbol").is_some() || truthy(config, "through").is_some() => {
            FlowsMode::Through
        }
        None if ["id", "name", "flow"]
            .iter()
            .any(|field| truthy(config, field).is_some()) =>
        {
            FlowsMode::Show
        }
        None => FlowsMode::List,
    };
    // Arguments first, like neighbors: a bad one is a usage error whether or
    // not this graph has flows. The "no flows" miss below is softened into a
    // per-dataset miss, which would otherwise hide the caller's mistake.
    let repo = identifier_arg(config, &["repo"])?;
    let mut list_arguments = None;
    match mode {
        FlowsMode::List => {
            let mechanisms: HashSet<String> = as_strings(config.get("mechanism"), "mechanism")?
                .into_iter()
                .collect();
            let include_side =
                optional_bool(config.get("include_side"), "include_side")?.unwrap_or(false);
            let limit = count(config.get("limit"), "limit", FLOWS_LIMIT.0, FLOWS_LIMIT.1)?;
            let offset = offset(config.get("offset"))?;
            list_arguments = Some((mechanisms, include_side, limit, offset));
        }
        FlowsMode::Show => {
            identifier_arg(config, &["id"])?;
            identifier_arg(config, &["name", "flow"])?;
        }
        FlowsMode::Through => {
            identifier_arg(config, &["symbol", "through", "name"])?;
            identifier_arg(config, &["id"])?;
        }
    }
    if snapshot.flows.is_empty() {
        if snapshot.nodes.is_empty() {
            return Err(CodeQueryError::unavailable(
                NO_CODE_GRAPH_MESSAGE,
                CodeErrorCode::NoCodeGraph,
            ));
        }
        return Err(CodeQueryError::unavailable_for(
            NO_FLOWS_MESSAGE,
            CodeErrorCode::FeatureUnavailable,
            "operation",
        ));
    }
    let Some((mechanisms, include_side, limit, offset)) = list_arguments else {
        if mode == FlowsMode::Show {
            let flow_id = resolve_flow(snapshot, config, query)?;
            return Ok(json!({
                "operation": "flows",
                "mode": "show",
                "flow": snapshot.flow_detail(&flow_id),
            }));
        }
        return flows_through(snapshot, config, query);
    };

    let matched: Vec<&String> = snapshot
        .flow_order
        .iter()
        .filter(|flow_id| {
            let flow = &snapshot.flows[*flow_id];
            let properties = properties_of(flow);
            if let Some(repo) = &repo
                && flow.get("repo").and_then(Value::as_str) != Some(repo.as_str())
            {
                return false;
            }
            if !mechanisms.is_empty() {
                let Some(Value::String(mechanism)) = properties.get("mechanism") else {
                    return false;
                };
                if !mechanisms.contains(mechanism) {
                    return false;
                }
            }
            include_side || properties.get("side") != Some(&Value::Bool(true))
        })
        .collect();
    let page: Vec<Value> = matched
        .iter()
        .skip(offset)
        .take(limit)
        .map(|flow_id| Value::Object(snapshot.flow_summary(flow_id)))
        .collect();
    let has_more = offset.saturating_add(page.len()) < matched.len();
    Ok(json!({
        "operation": "flows",
        "mode": "list",
        "flows": page,
        "total": matched.len(),
        "offset": offset,
        "limit": limit,
        "has_more": has_more,
    }))
}

/// `_resolve_flow`: a flow by cognee or Enola id, else by name (`name`,
/// `flow`, else the query text): exact, then suffix, then substring, each
/// casefolded; a tier with several flows is ambiguous.
fn resolve_flow(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<String> {
    let repo = identifier_arg(config, &["repo"])?;
    let in_repo = |flow_id: &str| match &repo {
        Some(repo) => {
            snapshot.flows[flow_id].get("repo").and_then(Value::as_str) == Some(repo.as_str())
        }
        None => true,
    };
    if let Some(raw_id) = identifier_arg(config, &["id"])? {
        let mut flow_id = raw_id.clone();
        if !snapshot.flows.contains_key(&flow_id)
            && let Some(mapped) = snapshot.flow_by_enola_id.get(&flow_id)
        {
            flow_id = mapped.clone();
        }
        if !snapshot.flows.contains_key(&flow_id) || !in_repo(&flow_id) {
            return Err(CodeQueryError::not_found(
                format!("CODE could not resolve flow id {}.", py_repr_str(&raw_id)),
                "id",
            ));
        }
        return Ok(flow_id);
    }

    let name = identifier_arg(config, &["name", "flow"])?;
    let term = py_strip(name.as_deref().unwrap_or(query)).to_string();
    if term.is_empty() {
        return Err(CodeQueryError::invalid(
            "CODE flows show requires a flow id or name.",
            "name",
        ));
    }
    let lowered = casefold(&term);
    let mut tiers: [Vec<&String>; 3] = Default::default();
    for flow_id in &snapshot.flow_order {
        if !in_repo(flow_id) {
            continue;
        }
        let name = casefold(&py_str_truthy_or(snapshot.flows[flow_id].get("name"), ""));
        if name == lowered {
            tiers[0].push(flow_id);
        } else if name.ends_with(&lowered) {
            tiers[1].push(flow_id);
        } else if name.contains(&lowered) {
            tiers[2].push(flow_id);
        }
    }
    for matches in tiers {
        match matches.as_slice() {
            [] => continue,
            [only] => return Ok((*only).clone()),
            _ => {
                let choices: Vec<String> = matches
                    .iter()
                    .take(10)
                    .map(|flow_id| {
                        let flow = &snapshot.flows[*flow_id];
                        format!(
                            "{} [repo={}, id={flow_id}]",
                            flow.get("name").map_or_else(|| "None".to_string(), py_str),
                            py_str_truthy_or(flow.get("repo"), "-"),
                        )
                    })
                    .collect();
                return Err(CodeQueryError::ambiguous(
                    format!(
                        "CODE flow name {} is ambiguous; use an exact id or repo. Candidates: {}",
                        py_repr_str(&term),
                        choices.join(", ")
                    ),
                    "name",
                ));
            }
        }
    }
    Err(CodeQueryError::not_found(
        format!("CODE could not resolve flow name {}.", py_repr_str(&term)),
        "name",
    ))
}

/// `_flows_through`: the flows whose calls reach a symbol (`symbol`,
/// `through`, `name`, `id`, else the query text), nearest first, then in
/// Enola's ranking.
fn flows_through(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let name = identifier_arg(config, &["symbol", "through", "name"])?;
    let node_id = identifier_arg(config, &["id"])?;
    let repo = identifier_arg(config, &["repo"])?.map(Value::String);
    let symbol = snapshot.resolve(
        Some(name.as_deref().unwrap_or(query)),
        node_id.as_deref(),
        repo.as_ref(),
        "flows symbol",
    )?;
    let reached = snapshot.calls_reach(&symbol);
    let mut found: Vec<FlowReach> = snapshot.flows_reaching(&symbol, &reached);
    found.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| snapshot.flow_rank_order(&left.flow_id, &right.flow_id))
    });
    let flows: Vec<Value> = found
        .iter()
        .map(|reach| {
            let mut summary = snapshot.flow_summary(&reach.flow_id);
            summary.insert("via".into(), json!(reach.via.as_str()));
            summary.insert("distance".into(), json!(reach.distance));
            Value::Object(summary)
        })
        .collect();
    let total = flows.len();
    Ok(json!({
        "operation": "flows",
        "mode": "through",
        "symbol": snapshot.fact(&symbol, None, false),
        "flows": flows,
        "total": total,
        "truncated": reached.truncated,
    }))
}

/// `_hubs`: the most connected nodes — a starting point when no name is
/// known.
///
/// `kinds` (default symbol), `direction` (both; forward counts out-degree,
/// reverse in-degree), `relation_types` (default every relation except
/// part_of, declares and evidences), `repo`, `file_prefix`, `include_tests`
/// (false: test code is neither listed nor counted in a degree, so a
/// function called from many tests is not a hub for it) and `limit` (20,
/// max 100). Ordered by degree, then the stable node order.
///
/// # Errors
///
/// `invalid_argument` for a malformed argument.
pub fn hubs(snapshot: &CodeGraphSnapshot, config: &CodeConfig) -> CodeQueryResult<Value> {
    let kinds = node_types(config.get("kinds"), "kinds")?
        .unwrap_or_else(|| HashSet::from(["CodeSymbol".to_string()]));
    let direction = direction(config.get("direction"), "both")?;
    let relation_types = relation_types(config)?;
    let repo = truthy(config, "repo");
    let file_prefix = file_prefix(config);
    let include_tests = include_tests(config)?;
    let limit = count(config.get("limit"), "limit", HUBS_LIMIT.0, HUBS_LIMIT.1)?;

    let counted = |edges: &mut dyn Iterator<Item = &SnapshotEdge>, outgoing: bool| -> usize {
        edges
            .filter(|edge| match &relation_types {
                Some(types) => types.contains(&edge.relation),
                None => !is_containment(&edge.relation),
            })
            .filter(|edge| {
                let other = if outgoing {
                    &edge.target_id
                } else {
                    &edge.source_id
                };
                include_tests || !snapshot.test_nodes.contains(other)
            })
            .count()
    };
    let mut ranked: Vec<(usize, usize, usize, &String)> = Vec::new();
    for node_type in &kinds {
        let Some(node_ids) = snapshot.by_type.get(node_type) else {
            continue;
        };
        for node_id in node_ids {
            let node = &snapshot.nodes[node_id];
            if let Some(repo) = repo
                && node.get("repo") != Some(repo)
            {
                continue;
            }
            if !file_prefix.is_empty() && !file_of(node).starts_with(&file_prefix) {
                continue;
            }
            if !include_tests && snapshot.is_test(node_id) {
                continue;
            }
            let in_degree = counted(&mut snapshot.reverse(node_id), false);
            let out_degree = counted(&mut snapshot.forward(node_id), true);
            let degree = match (direction.forward(), direction.reverse()) {
                (true, true) => in_degree + out_degree,
                (true, false) => out_degree,
                _ => in_degree,
            };
            if degree > 0 {
                ranked.push((degree, in_degree, out_degree, node_id));
            }
        }
    }
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| snapshot.node_order(left.3, right.3))
    });
    let hubs: Vec<Value> = ranked
        .iter()
        .take(limit)
        .map(|(degree, in_degree, out_degree, node_id)| {
            let mut fact = snapshot.fact(node_id, None, false);
            if let Value::Object(fields) = &mut fact {
                fields.insert("degree".into(), json!(degree));
                fields.insert("in_degree".into(), json!(in_degree));
                fields.insert("out_degree".into(), json!(out_degree));
            }
            fact
        })
        .collect();
    Ok(json!({
        "operation": "hubs",
        "hubs": hubs,
        "total": ranked.len(),
        "limit": limit,
    }))
}

/// The part a node plays in `ask`'s neighbourhood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkRole {
    Seed,
    Caller,
    Callee,
    Neighbor,
}

impl LinkRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Caller => "caller",
            Self::Callee => "callee",
            Self::Neighbor => "neighbor",
        }
    }
}

/// How a neighbour came into `ask`'s neighbourhood: the seed, its role and
/// the relation that brought it in.
type Link<'a> = (&'a String, LinkRole, &'a str);

/// `_ask`: evidence for a natural-language question about the code, with
/// citations.
///
/// The question is `question` or `text`, else the query text. It is
/// searched like `search` (`kinds`, `repo`, `file_prefix`, `include_tests`)
/// for `max_seeds` x 4 hits; the first `max_seeds` (5, max 10) fact hits are
/// the seeds and flow hits are matched flows. Up to three flows follow
/// (matched ones and those whose calls reach the seeds, ranked by seeds
/// reached, best seed score, then rank), then the seeds' one-hop
/// neighbourhood (callers, callees, route links, the rest; test code only
/// with `include_tests`; `max_nodes` (60, max 200) caps it, seeds
/// included), then file:line citations (`max_citations`, 25, max 60) and a
/// `coverage` block naming what the graph could not see. Its `seeds` counts
/// the seeds that can carry a docstring (Enola writes one on symbols; a seed
/// of another kind counts when it has one) and `seeds_with_docs` those that
/// do.
///
/// # Errors
///
/// `invalid_argument` for no question or a malformed argument;
/// `no_code_graph` for a dataset without a code graph.
pub fn ask(
    snapshot: &CodeGraphSnapshot,
    config: &CodeConfig,
    query: &str,
) -> CodeQueryResult<Value> {
    let question = text_arg(config, query, &["question", "text"])?;
    if question.is_empty() {
        return Err(CodeQueryError::invalid(
            "CODE ask requires a question or query text.",
            "question",
        ));
    }
    let max_seeds = count(
        config.get("max_seeds"),
        "max_seeds",
        ASK_MAX_SEEDS.0,
        ASK_MAX_SEEDS.1,
    )?;
    let max_nodes = count(
        config.get("max_nodes"),
        "max_nodes",
        ASK_MAX_NODES.0,
        ASK_MAX_NODES.1,
    )?;
    let max_citations = count(
        config.get("max_citations"),
        "max_citations",
        ASK_MAX_CITATIONS.0,
        ASK_MAX_CITATIONS.1,
    )?;
    let repo = truthy(config, "repo");
    let kinds = search_kinds(config)?;
    let include_tests = include_tests(config)?;
    if snapshot.nodes.is_empty() {
        return Err(CodeQueryError::unavailable(
            NO_CODE_GRAPH_MESSAGE,
            CodeErrorCode::NoCodeGraph,
        ));
    }

    let (terms, ranked) = snapshot.text_matches(
        &question,
        kinds.as_ref(),
        repo,
        &file_prefix(config),
        include_tests,
        Some(max_seeds * 4),
    );
    let seed_hits: Vec<&TextMatch> = ranked
        .iter()
        .filter(|(key, _, _)| snapshot.nodes.contains_key(key))
        .take(max_seeds)
        .collect();
    let flow_hits: Vec<&TextMatch> = ranked
        .iter()
        .filter(|(key, _, _)| snapshot.flows.contains_key(key))
        .collect();
    let seeds: Vec<&String> = seed_hits.iter().map(|(key, _, _)| key).collect();
    let documentable: Vec<&&String> = seeds
        .iter()
        .filter(|seed| {
            snapshot.documented.contains(**seed)
                || snapshot.nodes[**seed].get("type").and_then(Value::as_str) == Some("CodeSymbol")
        })
        .collect();

    let flows = ask_flows(snapshot, &seed_hits, &flow_hits);
    let neighborhood = ask_neighborhood(snapshot, &seeds, max_nodes, include_tests);
    let (citations, citations_cut) =
        ask_citations(snapshot, &seeds, &flows, &neighborhood.links, max_citations);

    let unresolved: Vec<i64> = flows
        .iter()
        .filter_map(|flow| int_or_none(flow_metric(flow, "unresolved_calls")))
        .collect();
    let unresolved_calls = (!unresolved.is_empty()).then(|| unresolved.iter().sum::<i64>());
    let flow_cut = flows
        .iter()
        .any(|flow| flow_metric(flow, "truncated") == Some(&Value::Bool(true)));
    let truncated = neighborhood.cut || citations_cut || flow_cut;

    let mut notes: Vec<String> = Vec::new();
    if ranked.is_empty() {
        let terms_text = if terms.is_empty() {
            "(none)".to_string()
        } else {
            terms.join(", ")
        };
        notes.push(format!(
            "No symbol, route or flow matched these terms: {terms_text}. Try an exact \
             identifier with neighbors, or grep."
        ));
    }
    if snapshot.documented.is_empty() {
        notes.push(
            "This snapshot has no docstrings, so matching used names and paths only.".to_string(),
        );
    }
    if snapshot.flows.is_empty() {
        notes.push("This snapshot has no flows, so entry points were not traced.".to_string());
    }
    if let Some(unresolved_calls) = unresolved_calls.filter(|calls| *calls != 0) {
        notes.push(format!(
            "{unresolved_calls} calls in these flows go to code outside the graph (libraries \
             or callees the extractor could not resolve)."
        ));
    }
    if neighborhood.cut {
        notes.push(format!(
            "The neighbourhood was cut at {max_nodes} nodes; use subgraph or neighbors to see \
             more."
        ));
    }
    if citations_cut {
        notes.push(format!("Citations were cut at {max_citations}."));
    }

    let next = ask_next(snapshot, &question, &seeds, &flows, repo);
    let seeds_with_docs = documentable
        .iter()
        .filter(|seed| snapshot.documented.contains(***seed))
        .count();
    Ok(json!({
        "operation": "ask",
        "question": question,
        "terms": terms,
        "seeds": seed_hits.iter().map(|item| hit(snapshot, item)).collect::<Vec<_>>(),
        "flows": flows,
        "neighborhood": {"nodes": neighborhood.nodes, "edges": neighborhood.edges},
        "citations": citations,
        "coverage": {
            "code_graph": true,
            "docs_present": !snapshot.documented.is_empty(),
            "seeds_with_docs": seeds_with_docs,
            "seeds": documentable.len(),
            "flows_present": !snapshot.flows.is_empty(),
            "unresolved_calls": unresolved_calls,
            "truncated": truncated,
            "notes": notes,
        },
        "next": next,
        "truncated": truncated,
    }))
}

/// `flow["metrics"].get(name)` of a rendered flow.
fn flow_metric<'a>(flow: &'a Value, name: &str) -> Option<&'a Value> {
    flow.get("metrics").and_then(|metrics| metrics.get(name))
}

/// `_ask_flows`: the matched flows plus the flows through the seeds, the
/// best three in `show` shape, each with the names of the seeds it reaches.
fn ask_flows(
    snapshot: &CodeGraphSnapshot,
    seed_hits: &[&TextMatch],
    flow_hits: &[&TextMatch],
) -> Vec<Value> {
    if snapshot.flows.is_empty() {
        return Vec::new();
    }
    // Insertion-ordered, as Python's dicts: the matched flows first.
    let mut order: Vec<&String> = Vec::new();
    let mut reaches: HashMap<&String, Vec<&String>> = HashMap::new();
    let mut best: HashMap<&String, f64> = HashMap::new();
    for (flow_id, score, _) in flow_hits {
        if reaches.insert(flow_id, Vec::new()).is_none() {
            order.push(flow_id);
        }
        best.insert(flow_id, *score);
    }
    for (seed, score, _) in seed_hits {
        let reached = snapshot.calls_reach(seed);
        for reach in snapshot.flows_reaching(seed, &reached) {
            let Some((flow_id, _)) = snapshot.flows.get_key_value(&reach.flow_id) else {
                continue;
            };
            reaches
                .entry(flow_id)
                .or_insert_with(|| {
                    order.push(flow_id);
                    Vec::new()
                })
                .push(seed);
            let current = best.entry(flow_id).or_insert(0.0);
            // Python's max(best.get(flow_id, 0.0), score).
            if *score > *current {
                *current = *score;
            }
        }
    }
    order.sort_by(|left, right| {
        reaches[right]
            .len()
            .cmp(&reaches[left].len())
            .then_with(|| best[right].total_cmp(&best[left]))
            .then_with(|| snapshot.flow_rank_order(left, right))
    });
    order
        .into_iter()
        .take(ASK_MAX_FLOWS)
        .map(|flow_id| {
            let mut flow = snapshot.flow_detail(flow_id);
            let names: Vec<Value> = reaches[flow_id]
                .iter()
                .map(|seed| {
                    snapshot.nodes[*seed]
                        .get("name")
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect();
            flow.insert("seeds_reached".into(), Value::Array(names));
            Value::Object(flow)
        })
        .collect()
}

/// `_seed_links`: a seed's neighbours as (node, role, relation), most
/// telling first — callers, then callees, then route links (`handled_by`
/// and the client route edges, or any edge to a route), then every other
/// relation except containment. Test code is left out unless
/// `include_tests`.
fn seed_links<'a>(
    snapshot: &'a CodeGraphSnapshot,
    seed: &str,
    include_tests: bool,
) -> Vec<(&'a String, LinkRole, &'a str)> {
    let mut callers = Vec::new();
    let mut callees = Vec::new();
    let mut routes = Vec::new();
    let mut others = Vec::new();
    let passes: [(LinkRole, Vec<&'a SnapshotEdge>); 2] = [
        (LinkRole::Caller, snapshot.reverse(seed).collect()),
        (LinkRole::Callee, snapshot.forward(seed).collect()),
    ];
    for (role, edges) in passes {
        for edge in edges {
            if is_containment(&edge.relation) {
                continue;
            }
            let neighbor_id = if role == LinkRole::Caller {
                &edge.source_id
            } else {
                &edge.target_id
            };
            if !include_tests && snapshot.test_nodes.contains(neighbor_id) {
                continue;
            }
            let relation = edge.relation.as_str();
            if relation == "calls" {
                if role == LinkRole::Caller {
                    callers.push((neighbor_id, role, relation));
                } else {
                    callees.push((neighbor_id, role, relation));
                }
            } else if ROUTE_EDGE_TYPES.contains(&relation)
                || snapshot.nodes[neighbor_id]
                    .get("type")
                    .and_then(Value::as_str)
                    == Some("ApiEndpoint")
            {
                routes.push((neighbor_id, LinkRole::Neighbor, relation));
            } else {
                others.push((neighbor_id, LinkRole::Neighbor, relation));
            }
        }
    }
    callers.extend(callees);
    callers.extend(routes);
    callers.extend(others);
    callers
}

/// What `_ask_neighborhood` returns.
struct Neighborhood<'a> {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    /// Every added neighbour, in the order it was added, with the link that
    /// brought it in.
    links: Vec<(&'a String, Link<'a>)>,
    cut: bool,
}

/// `_ask_neighborhood`: one hop around the seeds.
///
/// Seeds take turns adding their next neighbour, so `max_nodes` is shared
/// fairly; it counts the seeds too, and past it the later seeds are cut.
fn ask_neighborhood<'a>(
    snapshot: &'a CodeGraphSnapshot,
    seeds: &[&'a String],
    max_nodes: usize,
    include_tests: bool,
) -> Neighborhood<'a> {
    let mut cut = seeds.len() > max_nodes;
    let seeds = &seeds[..seeds.len().min(max_nodes)];
    let mut included: Vec<(&'a String, LinkRole)> =
        seeds.iter().map(|seed| (*seed, LinkRole::Seed)).collect();
    let mut included_set: HashSet<&String> = seeds.iter().copied().collect();
    let mut links: Vec<(&'a String, Link<'a>)> = Vec::new();
    let queues: Vec<Vec<(&'a String, LinkRole, &'a str)>> = seeds
        .iter()
        .map(|seed| seed_links(snapshot, seed, include_tests))
        .collect();
    let mut cursors = vec![0_usize; seeds.len()];
    let mut progressed = true;
    while progressed && !cut {
        progressed = false;
        for (position, seed) in seeds.iter().enumerate() {
            let queue = &queues[position];
            while cursors[position] < queue.len()
                && included_set.contains(queue[cursors[position]].0)
            {
                cursors[position] += 1;
            }
            if cursors[position] == queue.len() {
                continue;
            }
            if included.len() >= max_nodes {
                cut = true;
                break;
            }
            let (neighbor_id, role, relation) = queue[cursors[position]];
            cursors[position] += 1;
            included.push((neighbor_id, role));
            included_set.insert(neighbor_id);
            links.push((neighbor_id, (*seed, role, relation)));
            progressed = true;
        }
    }

    let nodes: Vec<Value> = included
        .iter()
        .map(|(node_id, role)| {
            let depth = if *role == LinkRole::Seed { 0 } else { 1 };
            let mut fact = snapshot.fact(node_id, Some(depth), false);
            if let Value::Object(fields) = &mut fact {
                fields.insert("role".into(), json!(role.as_str()));
            }
            fact
        })
        .collect();
    let mut seen: HashSet<(&String, &String, &String)> = HashSet::new();
    let mut edges: Vec<&SnapshotEdge> = Vec::new();
    for seed in seeds {
        let incident = snapshot
            .forward(seed)
            .map(|edge| (&edge.target_id, edge))
            .chain(snapshot.reverse(seed).map(|edge| (&edge.source_id, edge)));
        for (neighbor_id, edge) in incident {
            if !is_containment(&edge.relation)
                && included_set.contains(neighbor_id)
                && seen.insert((&edge.source_id, &edge.target_id, &edge.relation))
            {
                edges.push(edge);
            }
        }
    }
    edges.sort_by(|left, right| snapshot.edge_order(left, right));
    Neighborhood {
        nodes,
        edges: edges
            .into_iter()
            .map(|edge| snapshot.edge_payload(edge))
            .collect(),
        links,
        cut,
    }
}

/// What one citation points at: a fact node, or an Enola ref the graph does
/// not hold (by its file, line and name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CitationKey {
    Node(String),
    Ref(String),
}

/// The fields of one citation before it is written.
struct CitationFields {
    node_id: Value,
    kind: Value,
    name: Value,
    file: Value,
    line: Value,
    end_line: Value,
    doc: Value,
}

/// `_ask_citations`'s accumulator: one citation per key (the first reason
/// wins), only for what has a file, at most `max`.
struct Citations {
    items: Vec<Value>,
    seen: HashSet<CitationKey>,
    max: usize,
    cut: bool,
}

impl Citations {
    fn add(&mut self, key: CitationKey, why: String, fields: CitationFields) {
        if !is_truthy(&fields.file) || self.seen.contains(&key) {
            return;
        }
        if self.items.len() >= self.max {
            self.cut = true;
            return;
        }
        self.seen.insert(key);
        let mut citation = Map::new();
        citation.insert("node_id".into(), fields.node_id);
        citation.insert("kind".into(), fields.kind);
        citation.insert("name".into(), fields.name);
        citation.insert("file".into(), fields.file.clone());
        for (name, value) in [("line", &fields.line), ("end_line", &fields.end_line)] {
            if !value.is_null() {
                citation.insert(name.into(), value.clone());
            }
        }
        let file = py_str(&fields.file);
        let cite = if fields.line.is_null() {
            file
        } else {
            format!("{file}:{}", py_str(&fields.line))
        };
        citation.insert("cite".into(), Value::String(cite));
        if is_truthy(&fields.doc) {
            citation.insert("doc".into(), fields.doc);
        }
        citation.insert("why".into(), Value::String(why));
        self.items.push(Value::Object(citation));
    }

    fn add_node(&mut self, snapshot: &CodeGraphSnapshot, node_id: &str, why: String) {
        let Some(node) = snapshot.nodes.get(node_id) else {
            return;
        };
        let field = |name: &str| node.get(name).cloned().unwrap_or(Value::Null);
        let kind = node
            .get("type")
            .and_then(Value::as_str)
            .and_then(kind_of_type);
        self.add(
            CitationKey::Node(node_id.to_string()),
            why,
            CitationFields {
                node_id: json!(node_id),
                kind: json!(kind),
                name: field("name"),
                file: Value::String(file_of(node)),
                line: field("line"),
                end_line: field("end_line"),
                doc: doc_of(node).map_or(Value::Null, Value::String),
            },
        );
    }

    fn add_ref(&mut self, snapshot: &CodeGraphSnapshot, reference: &Value, why: String) {
        if let Some(Value::String(node_id)) = reference.get("node_id")
            && snapshot.nodes.contains_key(node_id)
        {
            self.add_node(snapshot, node_id, why);
            return;
        }
        // Enola names a symbol the graph does not hold; its file and line
        // still say where to read.
        let field = |name: &str| reference.get(name).cloned().unwrap_or(Value::Null);
        let key = CitationKey::Ref(crate::pyjson::dumps_compact(&json!([
            field("file"),
            field("line"),
            field("name"),
        ])));
        self.add(
            key,
            why,
            CitationFields {
                node_id: Value::Null,
                kind: json!("symbol"),
                name: field("name"),
                file: field("file"),
                line: field("line"),
                end_line: Value::Null,
                doc: field("doc"),
            },
        );
    }
}

/// `_ask_citations`: what to read before answering — the seeds, the flow
/// entries and stages, then the neighbours (callers and callees first).
/// Returns the citations and whether `max_citations` cut them.
fn ask_citations(
    snapshot: &CodeGraphSnapshot,
    seeds: &[&String],
    flows: &[Value],
    links: &[(&String, Link<'_>)],
    max_citations: usize,
) -> (Vec<Value>, bool) {
    let mut citations = Citations {
        items: Vec::new(),
        seen: HashSet::new(),
        max: max_citations,
        cut: false,
    };
    for seed in seeds {
        citations.add_node(snapshot, seed, "seed".to_string());
    }
    let flow_name = |flow: &Value| flow.get("name").map_or_else(|| "None".to_string(), py_str);
    for flow in flows {
        if let Some(entry) = flow.get("entry") {
            citations.add_ref(
                snapshot,
                entry,
                format!("entry of flow {}", flow_name(flow)),
            );
        }
    }
    for flow in flows {
        let stages = flow.get("stages").and_then(Value::as_array);
        for (position, stage) in stages.into_iter().flatten().enumerate() {
            let order = match stage.get("order") {
                Some(order) if is_truthy(order) => py_str(order),
                _ => (position + 1).to_string(),
            };
            citations.add_ref(
                snapshot,
                stage,
                format!("stage {order} of flow {}", flow_name(flow)),
            );
        }
    }
    let seed_name = |seed: &String| {
        snapshot.nodes[seed]
            .get("name")
            .map_or_else(|| "None".to_string(), py_str)
    };
    for (node_id, (seed, role, _relation)) in links {
        match role {
            LinkRole::Caller => {
                citations.add_node(snapshot, node_id, format!("calls {}", seed_name(seed)));
            }
            LinkRole::Callee => {
                citations.add_node(snapshot, node_id, format!("called by {}", seed_name(seed)));
            }
            _ => {}
        }
    }
    for (node_id, (seed, role, relation)) in links {
        if *role == LinkRole::Neighbor {
            citations.add_node(
                snapshot,
                node_id,
                format!("linked to {} by {relation}", seed_name(seed)),
            );
        }
    }
    (citations.items, citations.cut)
}

/// `_ask_next`: two to four follow-up `code_query` calls that widen what
/// `ask` returned.
fn ask_next(
    snapshot: &CodeGraphSnapshot,
    question: &str,
    seeds: &[&String],
    flows: &[Value],
    repo: Option<&Value>,
) -> Vec<Value> {
    let mut steps = Vec::new();
    if let Some(flow) = flows.first() {
        steps.push(json!({
            "code_query": {"operation": "flows", "mode": "show", "id": flow.get("id")},
            "why": format!(
                "Every stage of flow {}.",
                flow.get("name").map_or_else(|| "None".to_string(), py_str)
            ),
        }));
    }
    if let Some(first) = seeds.first() {
        let name = snapshot.nodes[*first]
            .get("name")
            .map_or_else(|| "None".to_string(), py_str);
        steps.push(json!({
            "code_query": {"operation": "neighbors", "id": first},
            "why": format!("Every caller, callee and other link of {name}."),
        }));
        let top: Vec<&&String> = seeds.iter().take(3).collect();
        steps.push(json!({
            "code_query": {"operation": "subgraph", "seed_ids": top, "depth": 2},
            "why": "The graph two hops around the top seeds.",
        }));
        return steps;
    }
    let scoped = |mut query: Map<String, Value>| -> Value {
        if let Some(repo) = repo {
            query.insert("repo".into(), repo.clone());
        }
        Value::Object(query)
    };
    let operation = |name: &str| {
        let mut query = Map::new();
        query.insert("operation".into(), json!(name));
        query
    };
    if !snapshot.flows.is_empty() && flows.is_empty() {
        steps.push(json!({
            "code_query": scoped(operation("flows")),
            "why": "The entry points this snapshot traced, ranked by what they reach.",
        }));
    }
    steps.push(json!({
        "code_query": scoped(operation("hubs")),
        "why": "The most connected symbols, a place to start when no name matched.",
    }));
    if steps.len() < 2 {
        let mut search = operation("search");
        search.insert("text".into(), json!(question));
        search.insert("kinds".into(), json!("all"));
        search.insert("include_tests".into(), json!(true));
        steps.push(json!({
            "code_query": scoped(search),
            "why": "The same terms over every kind, test code included.",
        }));
    }
    steps
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn config(value: Value) -> CodeConfig {
        value.as_object().expect("an object").clone()
    }

    #[test]
    fn text_arg_takes_the_first_non_blank_field_else_the_query() {
        let fields = ["text", "query", "q"];
        assert_eq!(
            text_arg(&config(json!({"text": "  ", "q": " b "})), "c", &fields).unwrap(),
            "b"
        );
        assert_eq!(
            text_arg(&config(json!({"text": null})), " c ", &fields).unwrap(),
            "c"
        );
        let error = text_arg(&config(json!({"query": 5})), "c", &fields).unwrap_err();
        assert_eq!(error.message, "CODE query must be a string.");
        assert_eq!(error.param.as_deref(), Some("query"));
    }

    #[test]
    fn search_kinds_default_all_and_flow() {
        let defaults = search_kinds(&config(json!({}))).unwrap().unwrap();
        let mut defaults: Vec<String> = defaults.into_iter().collect();
        defaults.sort();
        assert_eq!(
            defaults,
            [
                "ApiEndpoint",
                "CodeFlow",
                "CodeModule",
                "CodeService",
                "CodeSymbol",
                "StorageResource"
            ]
        );
        assert_eq!(
            search_kinds(&config(json!({"kinds": ["symbol", " ALL "]}))).unwrap(),
            None
        );
        assert_eq!(
            search_kinds(&config(json!({"kinds": "CodeFlow"}))).unwrap(),
            Some(HashSet::from([FLOW_TYPE.to_string()]))
        );
        let error = search_kinds(&config(json!({"kinds": ["widget"]}))).unwrap_err();
        assert_eq!(
            error.message,
            "Unsupported CODE node kind/type 'widget'; expected one of association, \
             dependency, extraction, file_ref, flow, insight, intent, lint, module, route, \
             service, storage, symbol, test_ref, or all."
        );
    }
}
