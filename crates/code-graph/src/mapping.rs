//! Map enola facts to code graph nodes.
//!
//! Port of the node half of Python's
//! `cognee/tasks/code_graph/extract_code_graph.py`: `fact_node_id`,
//! `map_facts_to_data_points` and `_fact_content_hash`. Node ids, `fact_hash`
//! values and descriptions are byte-identical to Python's, so either SDK can
//! re-ingest a graph the other wrote without rewriting a single node.

use std::collections::{BTreeMap, HashSet};

use cognee_models::DataPoint;
use cognee_utils::generate_node_id;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::{CodeGraphEntity, CodeNode, CodeNodeType, CodeRepository, code_metadata};
use crate::pyjson::{dumps_sorted, py_str};
use crate::snapshot::{Fact, is_enola_id};

/// Version of how cognee maps an enola snapshot onto graph nodes and edges.
/// Bumped whenever that mapping changes, so an unchanged snapshot is still
/// loaded again by the new mapping. Python's `CODE_GRAPH_MAPPING_VERSION`.
pub const CODE_GRAPH_MAPPING_VERSION: i64 = 1;

/// Deterministic node id of the fact `(repo, kind, name)`. Port of Python's
/// `fact_node_id`: `uuid5(NAMESPACE_OID, "enola:" + ":".join(f"{len(part)}:{part}"))`,
/// where `len` counts code points.
pub fn fact_node_id(repo: &str, kind: &str, name: &str) -> Uuid {
    let key = [repo, kind, name]
        .iter()
        .map(|part| format!("{}:{part}", part.chars().count()))
        .collect::<Vec<_>>()
        .join(":");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("enola:{key}").as_bytes())
}

/// Whether a fact becomes a graph node: a kind cognee has a model for and a
/// non-empty string name. Python's `_is_mappable_fact`.
pub(crate) fn is_mappable_fact(fact: &Fact) -> bool {
    matches!(fact.get("kind"), Some(Value::String(kind)) if CodeNodeType::for_fact_kind(kind).is_some())
        && matches!(fact.get("name"), Some(Value::String(name)) if !name.is_empty())
}

/// The fact's repository label, or `fallback_repo`. Python's `_fact_repo`.
pub(crate) fn fact_repo<'a>(fact: &'a Fact, fallback_repo: &'a str) -> &'a str {
    match fact.get("repo") {
        Some(Value::String(repo)) if !repo.is_empty() => repo,
        _ => fallback_repo,
    }
}

/// `pathlib.Path(path).name`: the last component, `""` for a root or `.`.
fn path_name(path: &str) -> String {
    path.split(std::path::is_separator)
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// Fallback repository for facts without a `repo` label: the first labelled
/// fact's repo, else the directory name of `repo_path`, else `unknown`.
/// Python's `_resolve_fallback_repo`; mapping and edges must agree on it.
pub(crate) fn resolve_fallback_repo(facts: &[Fact], repo_path: Option<&str>) -> String {
    facts
        .iter()
        .find_map(|fact| match fact.get("repo") {
            Some(Value::String(repo)) if !repo.is_empty() => Some(repo.clone()),
            _ => None,
        })
        .unwrap_or_else(|| match repo_path {
            Some(path) if !path.is_empty() => path_name(path),
            _ => "unknown".to_string(),
        })
}

/// The generic `"kind: k=v, ..."` description of a fact: its scalar props
/// sorted by key, printed as Python's `str()`. Python's `_describe_fact`.
pub(crate) fn describe_fact(kind: &str, props: &Map<String, Value>) -> Option<String> {
    let scalar: BTreeMap<&String, &Value> = props
        .iter()
        .filter(|(_, value)| matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)))
        .collect();
    if scalar.is_empty() {
        return None;
    }
    let summary = scalar
        .into_iter()
        .map(|(key, value)| format!("{key}={}", py_str(value)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("{kind}: {summary}"))
}

/// Fingerprint of a fact's derived fields: `sha256:` over Python's
/// `json.dumps(fields, sort_keys=True, default=str)`, without `id`, `part_of`
/// and `fact_hash`. Python's `_fact_content_hash`.
pub(crate) fn fact_content_hash(fields: &Map<String, Value>) -> String {
    let hashed: Map<String, Value> = fields
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "part_of" | "fact_hash"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let canonical = dumps_sorted(&Value::Object(hashed));
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    )
}

/// A `NodeSet` as a `belongs_to_set` entry, in the shape the Rust document
/// pipeline writes (`{"id", "name", "type": "NodeSet"}`), with Python's
/// `NodeSet.id_for(name)` id: `generate_node_id("NodeSet:<name>")`.
fn node_set_entry(name: &str) -> Value {
    serde_json::json!({
        "id": generate_node_id(&format!("NodeSet:{name}")).to_string(),
        "name": name,
        "type": "NodeSet",
    })
}

/// An integer field: a JSON integer, never a bool or float.
fn int_field(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) if !number.is_f64() => number.as_i64(),
        _ => None,
    }
}

fn text_or_null(value: Option<&str>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

/// Map parsed enola facts to code nodes, one [`CodeRepository`] per repo
/// first. Port of Python's `map_facts_to_data_points`.
///
/// * `repo_path` is the repository's directory: its name is the fallback
///   repository label, and it becomes the primary repository's `path`.
/// * `repo_scope` narrows a multi-repo snapshot to one member.
/// * `node_set` tags every node, the repository included, with
///   `belongs_to_set`/`source_node_set`. It enters `fact_hash` only when the
///   call is tagged (a non-empty list), sorted, so an untagged repository's
///   hashes are those of a call that never had the parameter.
///
/// Facts of an unknown kind are dropped, facts without a name skipped, and
/// same-named facts of one kind in one repo collapse into the first one's
/// node. A `symbol` fact whose `symbol_kind` is not a string is skipped, as
/// Python's model validation skips it.
pub fn map_facts_to_data_points(
    facts: &[Fact],
    repo_path: Option<&str>,
    repo_scope: Option<&str>,
    node_set: Option<&[String]>,
) -> Vec<CodeNode> {
    let fallback_repo = resolve_fallback_repo(facts, repo_path);
    let primary_repo = repo_scope.unwrap_or(&fallback_repo).to_string();
    let node_set = node_set.filter(|names| !names.is_empty());

    let belongs_to_set: Option<Vec<Value>> =
        node_set.map(|names| names.iter().map(|name| node_set_entry(name)).collect());
    let source_node_set = node_set.map(|names| names.join(", "));
    let node_set_hash_value = node_set.map(|names| {
        let mut sorted = names.to_vec();
        sorted.sort();
        Value::from(sorted)
    });

    let new_base = |data_type: CodeNodeType, id: Uuid| {
        let mut base = DataPoint::with_metadata(data_type.class_name(), None, code_metadata());
        base.id = id;
        base.belongs_to_set = belongs_to_set.clone();
        base.source_node_set = source_node_set.clone();
        base
    };

    let mut repositories: Vec<CodeRepository> = Vec::new();
    let mut repository_for = |repo: &str| -> Uuid {
        let id = fact_node_id(repo, "repository", repo);
        if !repositories
            .iter()
            .any(|repository| repository.name == repo)
        {
            let path = match repo_path {
                Some(path) if !path.is_empty() && repo == primary_repo => path.to_string(),
                _ => repo.to_string(),
            };
            repositories.push(CodeRepository {
                base: new_base(CodeNodeType::CodeRepository, id),
                name: repo.to_string(),
                path,
                last_snapshot_id: None,
                last_mapping_version: None,
                last_delta: None,
                last_receipt: None,
            });
        }
        id
    };

    // Always create the primary repository node, even for an empty snapshot.
    repository_for(&primary_repo);

    let mut entities: Vec<CodeGraphEntity> = Vec::new();
    let mut skipped_facts = 0usize;
    let mut duplicate_facts = 0usize;
    let mut unmapped_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen_ids: HashSet<Uuid> = HashSet::new();
    let empty_props = Map::new();

    for fact in facts {
        let model = match fact.get("kind") {
            Some(Value::String(kind)) => {
                CodeNodeType::for_fact_kind(kind).map(|model| (kind, model))
            }
            _ => None,
        };
        let Some((kind, model)) = model else {
            let label = match fact.get("kind") {
                Some(Value::String(kind)) => kind.clone(),
                Some(other) => crate::pyjson::py_repr(other),
                None => "None".to_string(),
            };
            *unmapped_kinds.entry(label).or_default() += 1;
            continue;
        };
        let name = match fact.get("name") {
            Some(Value::String(name)) if !name.is_empty() => name,
            _ => {
                skipped_facts += 1;
                tracing::warn!(
                    "Skipping fact with missing name: {}",
                    serde_json::Value::Object(fact.clone())
                );
                continue;
            }
        };

        let repo = fact_repo(fact, &fallback_repo);
        if repo_scope.is_some_and(|scope| scope != repo) {
            continue;
        }
        let node_id = fact_node_id(repo, kind, name);
        if !seen_ids.insert(node_id) {
            // Same-named facts of the same kind collapse into one node; the
            // first occurrence is kept so fact_hash is deterministic.
            duplicate_facts += 1;
            continue;
        }
        let props = fact
            .get("props")
            .and_then(Value::as_object)
            .unwrap_or(&empty_props);
        let file_path = fact.get("file").and_then(Value::as_str);
        let line = int_field(fact.get("line"));
        let end_line = int_field(fact.get("end_line"));
        let enola_id = fact
            .get("id")
            .filter(|id| is_enola_id(id))
            .and_then(Value::as_str);
        let doc = match props.get("doc") {
            Some(Value::String(doc)) if kind.as_str() != "insight" && !doc.trim().is_empty() => {
                Some(doc.as_str())
            }
            _ => None,
        };
        // Insights carry their explainer's prose, flows their summary,
        // documented facts the author's doc comment.
        let prose: Option<&Value> = match kind.as_str() {
            "insight" => props.get("description"),
            "flow" => props.get("summary"),
            _ => None,
        };
        let description = match (kind.as_str(), prose, doc) {
            ("insight" | "flow", Some(Value::String(prose)), _) => Some(prose.clone()),
            ("insight" | "flow", _, _) => describe_fact(kind, props),
            (_, _, Some(doc)) => Some(doc.to_string()),
            _ => describe_fact(kind, props),
        };
        let symbol_kind = props.get("symbol_kind");

        // The fields Python hands the model, in the hash's terms.
        let mut fields = Map::new();
        fields.insert("name".into(), Value::from(name.clone()));
        fields.insert("kind".into(), Value::from(kind.clone()));
        fields.insert("file_path".into(), text_or_null(file_path));
        fields.insert("line".into(), line.map(Value::from).unwrap_or(Value::Null));
        fields.insert(
            "end_line".into(),
            end_line.map(Value::from).unwrap_or(Value::Null),
        );
        fields.insert("repo".into(), Value::from(repo));
        fields.insert("enola_id".into(), text_or_null(enola_id));
        fields.insert("description".into(), text_or_null(description.as_deref()));
        fields.insert("fact_properties".into(), Value::Object(props.clone()));
        if model == CodeNodeType::CodeSymbol {
            fields.insert(
                "symbol_kind".into(),
                symbol_kind.cloned().unwrap_or(Value::Null),
            );
        }
        if let Some(doc) = doc {
            // Only a documented fact gains the key, so an undocumented fact's
            // hash stays what it was before docs were read.
            fields.insert("doc".into(), Value::from(doc));
        }
        if let Some(tag) = &node_set_hash_value {
            fields.insert("node_set".into(), tag.clone());
        }
        let fact_hash = fact_content_hash(&fields);
        // Python builds the repository node while building the fields, so a
        // fact the model then rejects still gets its repository.
        let part_of = repository_for(repo);

        // Python's model validation: CodeSymbol.symbol_kind is `str | None`.
        let symbol_kind = match (model, symbol_kind) {
            (CodeNodeType::CodeSymbol, None | Some(Value::Null)) => None,
            (CodeNodeType::CodeSymbol, Some(Value::String(symbol_kind))) => {
                Some(symbol_kind.clone())
            }
            (CodeNodeType::CodeSymbol, Some(_)) => {
                skipped_facts += 1;
                tracing::warn!(
                    "Skipping fact with invalid field types: {}",
                    serde_json::Value::Object(fact.clone())
                );
                continue;
            }
            _ => None,
        };

        entities.push(CodeGraphEntity {
            base: new_base(model, node_id),
            name: name.clone(),
            kind: kind.clone(),
            file_path: file_path.map(str::to_string),
            line,
            end_line,
            repo: Some(repo.to_string()),
            enola_id: enola_id.map(str::to_string),
            description,
            doc: doc.map(str::to_string),
            fact_properties: props.clone(),
            fact_hash: Some(fact_hash),
            symbol_kind,
            part_of: Some(part_of),
        });
    }

    if skipped_facts > 0 {
        tracing::warn!(
            "Skipped {} fact(s) that could not be mapped to DataPoints.",
            skipped_facts
        );
    }
    if !unmapped_kinds.is_empty() {
        tracing::warn!(
            "Dropped {} fact(s) of kind(s) cognee has no graph model for: {:?}",
            unmapped_kinds.values().sum::<usize>(),
            unmapped_kinds
        );
    }
    if duplicate_facts > 0 {
        tracing::info!(
            "Collapsed {} duplicate fact(s) into existing node ids.",
            duplicate_facts
        );
    }

    repositories
        .into_iter()
        .map(CodeNode::Repository)
        .chain(entities.into_iter().map(CodeNode::Entity))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fact_node_id_length_prefixes_code_points() {
        // uuid5(NAMESPACE_OID, "enola:9:acme/shop:6:symbol:4:main")
        assert_eq!(
            fact_node_id("acme/shop", "symbol", "main"),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"enola:9:acme/shop:6:symbol:4:main")
        );
        assert_eq!(
            fact_node_id("r", "symbol", "é"),
            Uuid::new_v5(&Uuid::NAMESPACE_OID, "enola:1:r:6:symbol:1:é".as_bytes())
        );
    }

    #[test]
    fn path_name_follows_pathlib() {
        assert_eq!(path_name("/tmp/shop"), "shop");
        assert_eq!(path_name("/tmp/shop/"), "shop");
        assert_eq!(path_name("/"), "");
        assert_eq!(path_name("."), "");
        assert_eq!(path_name("a/."), "a");
    }

    #[test]
    fn describe_fact_sorts_scalars_and_prints_python_values() {
        let props = json!({"port": 8080, "b": true, "a": 0.5, "list": [1], "z": "x"});
        assert_eq!(
            describe_fact("service", props.as_object().expect("object")),
            Some("service: a=0.5, b=True, port=8080, z=x".to_string())
        );
        assert_eq!(describe_fact("service", &Map::new()), None);
    }
}
