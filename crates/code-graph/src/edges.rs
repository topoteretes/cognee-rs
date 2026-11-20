//! Resolve enola relations into typed graph edges.
//!
//! Port of the edge half of Python's
//! `cognee/tasks/code_graph/extract_code_graph.py`: `build_code_graph_edges`,
//! `_add_client_route_edges`, `_without_declaration_views` and
//! `_short_target_names`. A relation carrying the writer's `target_id`
//! resolves to that fact; otherwise the target is matched by name within the
//! snapshot, preferring the same repository, and an ambiguous or missing
//! target is skipped rather than bound to an arbitrary fact.

use std::collections::{HashMap, HashSet};

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::mapping::{fact_node_id, fact_repo, is_mappable_fact, resolve_fallback_repo};
use crate::pyjson::py_iter;
use crate::snapshot::{Fact, is_enola_id, normalize_relation, relation_target_id};

/// A fact's node identity: `(repo, kind, name)`.
type FactKey = (String, String, String);

/// One typed edge between two code nodes: Python's
/// `(source_id, target_id, relationship_name, properties)` tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeGraphEdge {
    /// Source node id.
    pub source_id: Uuid,
    /// Target node id.
    pub target_id: Uuid,
    /// The relation type (`calls`, `imports`, `has_method`, ...).
    pub relationship_name: String,
    /// Edge properties: `source_node_id`, `target_node_id`,
    /// `relationship_name`, `edge_text`, `updated_at` (ISO 8601, UTC), plus
    /// the relation's own (a `reaches_route` edge's `confidence`).
    pub properties: Map<String, Value>,
}

/// Fact kinds a framework extractor derives from a declaration and names
/// after it: an ORM model's storage, a relation method's association.
const DECLARATION_VIEW_KINDS: &[&str] = &["storage", "association"];

/// Symbol kinds that own methods.
const OWNER_SYMBOL_KINDS: &[&str] = &["struct", "interface", "class", "type"];

/// Unqualified forms of a fact name: `app/db.Database` gives `Database` and
/// `db.Database`. Python's `_short_target_names`.
fn short_target_names(name: &str) -> HashSet<String> {
    let after = |separator: char| name.rsplit(separator).next().unwrap_or(name);
    [after('.'), after('/')]
        .into_iter()
        .filter(|form| !form.is_empty() && *form != name)
        .map(str::to_string)
        .collect()
}

/// Python's `posixpath.dirname`.
fn posix_dirname(path: &str) -> String {
    let split = path.rfind('/').map_or(0, |index| index + 1);
    let head = &path[..split];
    if !head.is_empty() && !head.chars().all(|character| character == '/') {
        head.trim_end_matches('/').to_string()
    } else {
        head.to_string()
    }
}

/// Name candidates minus the views of a symbol that is itself a candidate: a
/// storage or association fact with the symbol's own name, repo and file.
/// Python's `_without_declaration_views`.
fn without_declaration_views(
    candidates: HashSet<FactKey>,
    fact_index: &HashMap<FactKey, &Fact>,
) -> HashSet<FactKey> {
    let is_view = |candidate: &FactKey| -> bool {
        let (repo, kind, name) = candidate;
        let symbol: FactKey = (repo.clone(), "symbol".to_string(), name.clone());
        if !DECLARATION_VIEW_KINDS.contains(&kind.as_str()) || !candidates.contains(&symbol) {
            return false;
        }
        let symbol_file = fact_index.get(&symbol).and_then(|fact| fact.get("file"));
        match symbol_file {
            Some(Value::String(file)) if !file.is_empty() => {
                fact_index.get(candidate).and_then(|fact| fact.get("file"))
                    == Some(&Value::String(file.clone()))
            }
            _ => false,
        }
    };
    candidates
        .iter()
        .filter(|candidate| !is_view(candidate))
        .cloned()
        .collect()
}

/// Indexes over the mappable facts of a snapshot.
struct FactIndexes<'a> {
    valid_facts: Vec<(&'a Fact, String)>,
    name_index: HashMap<String, HashSet<(String, String)>>,
    short_name_index: HashMap<String, HashSet<FactKey>>,
    enola_id_index: HashMap<String, FactKey>,
    fact_index: HashMap<FactKey, &'a Fact>,
    /// `(repo, module name)` → module fact, in first-seen order.
    module_index: Vec<((String, String), &'a Fact)>,
    /// `(repo, module name)` → position in `module_index`, for lookups.
    module_positions: HashMap<(String, String), usize>,
    module_path_repos: HashMap<String, HashSet<String>>,
}

impl<'a> FactIndexes<'a> {
    fn build(facts: &'a [Fact], fallback_repo: &str) -> Self {
        let mut indexes = Self {
            valid_facts: Vec::new(),
            name_index: HashMap::new(),
            short_name_index: HashMap::new(),
            enola_id_index: HashMap::new(),
            fact_index: HashMap::new(),
            module_index: Vec::new(),
            module_positions: HashMap::new(),
            module_path_repos: HashMap::new(),
        };
        for fact in facts {
            if !is_mappable_fact(fact) {
                continue;
            }
            let kind = str_field(fact, "kind").to_string();
            let name = str_field(fact, "name").to_string();
            let repo = fact_repo(fact, fallback_repo).to_string();
            indexes.valid_facts.push((fact, repo.clone()));
            if kind != "flow" {
                // No relation targets a flow, and a flow is named after its
                // entry symbol or route: indexing it would make those
                // names ambiguous.
                indexes
                    .name_index
                    .entry(name.clone())
                    .or_default()
                    .insert((repo.clone(), kind.clone()));
                for short_form in short_target_names(&name) {
                    indexes
                        .short_name_index
                        .entry(short_form)
                        .or_default()
                        .insert((repo.clone(), kind.clone(), name.clone()));
                }
            }
            if let Some(Value::String(fact_id)) = fact.get("id").filter(|id| is_enola_id(id)) {
                indexes
                    .enola_id_index
                    .entry(fact_id.clone())
                    .or_insert_with(|| (repo.clone(), kind.clone(), name.clone()));
            }
            indexes
                .fact_index
                .entry((repo.clone(), kind.clone(), name.clone()))
                .or_insert(fact);
            if kind == "module" {
                let key = (repo.clone(), name.clone());
                if !indexes.module_positions.contains_key(&key) {
                    indexes
                        .module_positions
                        .insert(key.clone(), indexes.module_index.len());
                    indexes.module_index.push((key, fact));
                }
                let module_path = fact
                    .get("props")
                    .and_then(Value::as_object)
                    .and_then(|props| props.get("modulePath"));
                if let Some(Value::String(module_path)) = module_path
                    && !module_path.is_empty()
                {
                    indexes
                        .module_path_repos
                        .entry(module_path.clone())
                        .or_default()
                        .insert(repo.clone());
                }
            }
        }
        indexes
    }

    fn module(&self, repo: &str, name: &str) -> Option<&'a Fact> {
        self.module_positions
            .get(&(repo.to_string(), name.to_string()))
            .and_then(|position| self.module_index.get(*position))
            .map(|(_, fact)| *fact)
    }

    /// Python's `_resolve_target`.
    fn resolve_target(
        &self,
        target_name: &str,
        source_repo: &str,
        allowed_repos: Option<&HashSet<String>>,
    ) -> Option<FactKey> {
        let mut candidates: HashSet<FactKey> = self
            .name_index
            .get(target_name)
            .map(|entries| {
                entries
                    .iter()
                    .map(|(repo, kind)| (repo.clone(), kind.clone(), target_name.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if candidates.is_empty() {
            // No fact carries this exact name; fall back to unambiguous
            // suffix matches ('Database' for 'app/db.Database').
            candidates = self
                .short_name_index
                .get(target_name)
                .cloned()
                .unwrap_or_default();
        }
        if let Some(allowed) = allowed_repos {
            candidates.retain(|candidate| allowed.contains(&candidate.0));
        }
        let candidates = without_declaration_views(candidates, &self.fact_index);
        let same_repo: Vec<&FactKey> = candidates
            .iter()
            .filter(|candidate| candidate.0 == source_repo)
            .collect();
        if same_repo.len() == 1 {
            return Some(same_repo[0].clone());
        }
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
        None
    }
}

fn str_field<'a>(fact: &'a Fact, key: &str) -> &'a str {
    fact.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Collects edges, deduplicated by `(source, target, relationship)`.
struct EdgeSink {
    edges: Vec<CodeGraphEdge>,
    seen: HashSet<(Uuid, Uuid, String)>,
}

impl EdgeSink {
    fn add(
        &mut self,
        source: &FactKey,
        target: &FactKey,
        relationship_name: &str,
        properties: Option<Map<String, Value>>,
    ) {
        let source_id = fact_node_id(&source.0, &source.1, &source.2);
        let target_id = fact_node_id(&target.0, &target.1, &target.2);
        if !self
            .seen
            .insert((source_id, target_id, relationship_name.to_string()))
        {
            return;
        }
        let mut properties = properties.unwrap_or_default();
        properties.insert("source_node_id".into(), Value::from(source_id.to_string()));
        properties.insert("target_node_id".into(), Value::from(target_id.to_string()));
        properties.insert("relationship_name".into(), Value::from(relationship_name));
        properties.insert(
            "edge_text".into(),
            Value::from(relationship_name.replace('_', " ")),
        );
        properties.insert(
            "updated_at".into(),
            Value::from(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)),
        );
        self.edges.push(CodeGraphEdge {
            source_id,
            target_id,
            relationship_name: relationship_name.to_string(),
            properties,
        });
    }
}

/// Edges from a client route's `caller` and `matched_routes` props; returns
/// the number of refs that did not resolve. Python's `_add_client_route_edges`.
fn add_client_route_edges(indexes: &FactIndexes<'_>, sink: &mut EdgeSink) -> usize {
    let resolve_ref = |fact_id: Option<&Value>,
                       repo: Option<&Value>,
                       kind: &str,
                       ref_name: Option<&Value>|
     -> Option<FactKey> {
        if let Some(Value::String(fact_id)) = fact_id.filter(|id| is_enola_id(id))
            && let Some(key) = indexes.enola_id_index.get(fact_id)
        {
            return Some(key.clone());
        }
        if let (Some(Value::String(repo)), Some(Value::String(ref_name))) = (repo, ref_name) {
            let key: FactKey = (repo.clone(), kind.to_string(), ref_name.clone());
            if indexes.fact_index.contains_key(&key) {
                return Some(key);
            }
        }
        None
    };

    let mut skipped = 0;
    for (fact, source_repo) in &indexes.valid_facts {
        if str_field(fact, "kind") != "route" {
            continue;
        }
        let Some(Value::Object(props)) = fact.get("props") else {
            continue;
        };
        if props.get("role") != Some(&Value::from("client")) {
            continue;
        }
        let route: FactKey = (
            source_repo.clone(),
            "route".to_string(),
            str_field(fact, "name").to_string(),
        );

        let caller = props.get("caller").filter(|value| !value.is_null());
        let caller_id = props.get("caller_id").filter(|value| !value.is_null());
        if caller.is_some() || caller_id.is_some() {
            let source_repo_value = Value::from(source_repo.clone());
            match resolve_ref(caller_id, Some(&source_repo_value), "symbol", caller) {
                Some(resolved) => sink.add(&resolved, &route, "makes_request", None),
                None => skipped += 1,
            }
        }

        let Some(Value::Array(matches)) = props.get("matched_routes") else {
            continue;
        };
        for entry in matches {
            let Value::Object(entry) = entry else {
                skipped += 1;
                continue;
            };
            let Some(resolved) = resolve_ref(
                entry.get("id"),
                entry.get("repo"),
                "route",
                entry.get("name"),
            ) else {
                skipped += 1;
                continue;
            };
            if resolved == route {
                // A call its own repo serves under the same path collapses
                // onto the served route's node; a self-loop says nothing.
                continue;
            }
            let properties = match entry.get("confidence") {
                Some(Value::String(confidence)) => {
                    let mut properties = Map::new();
                    properties.insert("confidence".into(), Value::from(confidence.clone()));
                    Some(properties)
                }
                _ => None,
            };
            sink.add(&route, &resolved, "reaches_route", properties);
        }
    }
    skipped
}

/// Resolve typed relations between facts into explicit graph edges. Port of
/// Python's `build_code_graph_edges`; returns `(edges, skipped_count)`.
///
/// `repo_path` must be the value given to
/// [`map_facts_to_data_points`](crate::map_facts_to_data_points), so edge
/// endpoint ids match node ids. Besides each fact's explicit relations it
/// adds a client route's `makes_request`/`reaches_route` edges, the
/// module-to-module `imports` bridge of a dependency import, and the
/// `has_method` edge from a type to each of its methods.
pub fn build_code_graph_edges(
    facts: &[Fact],
    repo_path: Option<&str>,
) -> (Vec<CodeGraphEdge>, usize) {
    let fallback_repo = resolve_fallback_repo(facts, repo_path);
    let indexes = FactIndexes::build(facts, &fallback_repo);
    let mut sink = EdgeSink {
        edges: Vec::new(),
        seen: HashSet::new(),
    };
    let mut skipped = 0usize;

    // Module paths, longest first, then alphabetically.
    let mut module_paths: Vec<&String> = indexes.module_path_repos.keys().collect();
    module_paths.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });

    // Every explicit relation. For an unresolved Go call, mirror enola's
    // module-path normalisation before giving up on the target.
    for (fact, source_repo) in &indexes.valid_facts {
        let kind = str_field(fact, "kind");
        let name = str_field(fact, "name");
        for relation in py_iter(fact.get("relations")) {
            let Some((relationship_name, target_name)) = normalize_relation(&relation) else {
                skipped += 1;
                tracing::warn!("Skipping relation that could not be normalized: {relation}");
                continue;
            };
            let mut target = relation_target_id(&relation)
                .and_then(|target_id| indexes.enola_id_index.get(target_id))
                .cloned();
            if target.is_none() {
                target = indexes.resolve_target(&target_name, source_repo, None);
            }
            if target.is_none()
                && relationship_name == "calls"
                && !indexes.name_index.contains_key(&target_name)
            {
                for module_path in &module_paths {
                    let Some(suffix) = target_name.strip_prefix(module_path.as_str()) else {
                        continue;
                    };
                    let provider_repos = &indexes.module_path_repos[*module_path];
                    if provider_repos.len() != 1 {
                        continue;
                    }
                    let normalized_target = if let Some(rest) = suffix.strip_prefix('/') {
                        rest.to_string()
                    } else if suffix.starts_with('.') {
                        format!(".{suffix}")
                    } else {
                        continue;
                    };
                    if !normalized_target.is_empty() {
                        target = indexes.resolve_target(
                            &normalized_target,
                            source_repo,
                            Some(provider_repos),
                        );
                    }
                    if target.is_some() {
                        break;
                    }
                }
            }

            let Some(target) = target else {
                skipped += 1;
                tracing::warn!(
                    "Skipping relation '{}' from '{}': target '{}' is {}.",
                    relationship_name,
                    name,
                    target_name,
                    if indexes.name_index.contains_key(&target_name) {
                        "ambiguous"
                    } else {
                        "unresolved"
                    }
                );
                continue;
            };
            let source: FactKey = (source_repo.clone(), kind.to_string(), name.to_string());
            sink.add(&source, &target, &relationship_name, None);
        }
    }

    skipped += add_client_route_edges(&indexes, &mut sink);

    // enola's query graph connects a dependency import to the modules that
    // contain each side; materialise the same bridge.
    let mut modules_by_name: HashMap<&str, Vec<(&str, &Fact)>> = HashMap::new();
    for ((repo, module_name), module_fact) in &indexes.module_index {
        modules_by_name
            .entry(module_name.as_str())
            .or_default()
            .push((repo.as_str(), module_fact));
    }

    for (fact, source_repo) in &indexes.valid_facts {
        if str_field(fact, "kind") != "dependency" {
            continue;
        }
        let Some(Value::String(file_path)) = fact.get("file") else {
            continue;
        };
        let or_dot = |directory: String| {
            if directory.is_empty() {
                ".".to_string()
            } else {
                directory
            }
        };
        let mut source_directories = vec![or_dot(posix_dirname(file_path))];
        let repo_prefix = format!("{}/", source_repo.trim_end_matches('/'));
        if let Some(relative) = file_path.strip_prefix(&repo_prefix) {
            source_directories.push(or_dot(posix_dirname(relative)));
        }
        let Some(source_module) = source_directories
            .iter()
            .find_map(|directory| indexes.module(source_repo, directory))
        else {
            continue;
        };
        let source_module_name = str_field(source_module, "name");

        for relation in py_iter(fact.get("relations")) {
            let Some((relationship_name, target_name)) = normalize_relation(&relation) else {
                continue;
            };
            if relationship_name != "imports" {
                continue;
            }
            let mut target_module: Option<(&str, &Fact)> = None;
            let mut candidate_name = target_name;
            while !candidate_name.is_empty() {
                if let Some(local) = indexes.module(source_repo, &candidate_name) {
                    target_module = Some((source_repo.as_str(), local));
                    break;
                }
                if let Some(global) = modules_by_name.get(candidate_name.as_str())
                    && global.len() == 1
                {
                    target_module = Some(global[0]);
                    break;
                }
                let parent = posix_dirname(&candidate_name);
                if parent == candidate_name || parent == "." {
                    break;
                }
                candidate_name = parent;
            }

            let Some((target_repo, target_fact)) = target_module else {
                continue;
            };
            let target_module_name = str_field(target_fact, "name");
            if source_repo == target_repo && source_module_name == target_module_name {
                continue;
            }
            sink.add(
                &(
                    source_repo.clone(),
                    "module".to_string(),
                    source_module_name.to_string(),
                ),
                &(
                    target_repo.to_string(),
                    "module".to_string(),
                    target_module_name.to_string(),
                ),
                "imports",
                None,
            );
        }
    }

    // enola derives type membership from qualified symbol names; persisting
    // the edge makes type-level impact analysis include method callers.
    let symbol_kind_of = |fact: &Fact| -> Option<Value> {
        fact.get("props")
            .and_then(Value::as_object)
            .and_then(|props| props.get("symbol_kind"))
            .cloned()
    };
    for (fact, source_repo) in &indexes.valid_facts {
        let name = str_field(fact, "name");
        if str_field(fact, "kind") != "symbol" || !name.contains('.') {
            continue;
        }
        let is_member = matches!(
            symbol_kind_of(fact),
            Some(Value::String(kind)) if kind == "method" || kind == "function"
        );
        if !is_member {
            continue;
        }
        let owner_name = name.rsplit_once('.').map_or(name, |(owner, _)| owner);
        let owner_key: FactKey = (
            source_repo.clone(),
            "symbol".to_string(),
            owner_name.to_string(),
        );
        let owner_is_type = indexes.fact_index.get(&owner_key).is_some_and(|owner| {
            matches!(
                symbol_kind_of(owner),
                Some(Value::String(kind)) if OWNER_SYMBOL_KINDS.contains(&kind.as_str())
            )
        });
        if !owner_is_type {
            continue;
        }
        sink.add(
            &owner_key,
            &(source_repo.clone(), "symbol".to_string(), name.to_string()),
            "has_method",
            None,
        );
    }

    (sink.edges, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_drop_the_qualifier() {
        let forms = short_target_names("app/db.Database");
        assert_eq!(
            forms,
            HashSet::from(["Database".to_string(), "db.Database".to_string()])
        );
        assert!(short_target_names("plain").is_empty());
    }

    #[test]
    fn dirname_follows_posixpath() {
        assert_eq!(
            posix_dirname("r/internal/server/server.go"),
            "r/internal/server"
        );
        assert_eq!(posix_dirname("main.go"), "");
        assert_eq!(posix_dirname("/main.go"), "/");
        assert_eq!(posix_dirname("a//b"), "a");
        assert_eq!(posix_dirname("internal/config"), "internal");
    }
}
