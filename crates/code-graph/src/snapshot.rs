//! Read an enola snapshot directory.
//!
//! Port of the parsing half of Python's `cognee/tasks/code_graph/enola.py`.
//! `enola --generate <repo>` writes `.enola/`, whose contract artifacts
//! (versioned by the receipt's `format_version`) are `facts.jsonl` (one fact
//! per line), `insights.json` (explainer findings with evidence), `flows.json`
//! (the ways into the program, ranked by what each reaches) and
//! `receipt.json` (provenance, counts and extraction quality).
//!
//! Facts stay plain JSON objects ([`Fact`]), exactly as Python keeps them as
//! dicts: the mapping reads them defensively, key by key, the way Python does,
//! so a fact of an unexpected shape degrades the same way in both SDKs.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{CodeGraphError, CodeGraphResult};
use crate::pyjson::{is_truthy, py_repr, py_str};

/// One enola fact: a JSON object from `facts.jsonl`, or one synthesized from
/// `insights.json` / `flows.json`.
pub type Fact = Map<String, Value>;

/// A snapshot's `receipt.json`.
pub type Receipt = Map<String, Value>;

/// Snapshot artifact format generations this reader understands
/// (`receipt.json` `format_version`, written since enola 0.4.10). A receipt
/// without the field is a historical writer and reads as version 1.
pub const SUPPORTED_FORMAT_VERSIONS: &[i64] = &[1];

/// Length of a writer fact identity (enola >= 0.4.10): sha256(repo, kind,
/// name, file) truncated to 128 bits, as lowercase hex.
const ENOLA_ID_LENGTH: usize = 32;

/// Relation keys probed for the relation type, in order.
const RELATION_TYPE_KEYS: &[&str] = &["kind", "type", "relation", "rel"];
/// Relation keys probed for the target name, in order.
const RELATION_TARGET_KEYS: &[&str] = &["target", "name", "to", "target_name"];

/// Props an insight carries over from `insights.json`, in this order.
const INSIGHT_PROP_KEYS: &[&str] = &[
    "source",
    "confidence",
    "description",
    "suggested_actions",
    "metrics",
    "informational",
];

/// Props a flow carries over from `flows.json`, in this order.
const FLOW_PROP_KEYS: &[&str] = &[
    "mechanism",
    "rank",
    "summary",
    "metrics",
    "modules",
    "side",
    "routes",
    "entry",
    "stages",
    "same_reach_as",
    "nested_in",
];

/// A snapshot read once: its directory, facts and receipt.
///
/// Mirrors Python's `ParsedSnapshot`, so a load that covers several
/// repositories parses `facts.jsonl` once.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSnapshot {
    /// The `.enola/` directory the snapshot was read from.
    pub snapshot_dir: PathBuf,
    /// Every fact, synthesized insight and flow facts included.
    pub facts: Vec<Fact>,
    /// `receipt.json`, when present and an object.
    pub receipt: Option<Receipt>,
}

impl ParsedSnapshot {
    /// Parse `snapshot_dir` ([`parse_enola_snapshot`]).
    pub fn read(snapshot_dir: impl AsRef<Path>) -> CodeGraphResult<Self> {
        let snapshot_dir = snapshot_dir.as_ref();
        let (facts, receipt) = parse_enola_snapshot(snapshot_dir)?;
        Ok(Self {
            snapshot_dir: snapshot_dir.to_path_buf(),
            facts,
            receipt,
        })
    }
}

/// Parse `facts.jsonl` (streamed line by line) and `receipt.json` from a
/// snapshot directory. Port of Python's `parse_enola_snapshot`.
///
/// Blank and corrupt lines are skipped with a warning. A missing or
/// unparseable `receipt.json` is not fatal. Findings from `insights.json` and
/// flows from `flows.json` follow as synthesized facts.
///
/// # Errors
///
/// [`CodeGraphError::EnolaSnapshot`] when `facts.jsonl` is missing or cannot
/// be read as UTF-8 text, or when the receipt names an unsupported
/// `format_version`.
pub fn parse_enola_snapshot(
    snapshot_dir: impl AsRef<Path>,
) -> CodeGraphResult<(Vec<Fact>, Option<Receipt>)> {
    let snapshot_dir = snapshot_dir.as_ref();
    let facts_path = snapshot_dir.join("facts.jsonl");
    if !facts_path.is_file() {
        return Err(CodeGraphError::EnolaSnapshot(format!(
            "No facts.jsonl found in '{}'.",
            snapshot_dir.display()
        )));
    }

    let facts = read_facts(&facts_path)?;

    let receipt = read_json_file(&snapshot_dir.join("receipt.json"))
        .map_err(|()| {
            tracing::warn!(
                "Could not parse receipt.json in {}; ignoring it.",
                snapshot_dir.display()
            );
        })
        .ok()
        .flatten()
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        });

    validate_receipt(receipt.as_ref(), snapshot_dir, Some(facts.len()))?;

    let repos: HashSet<String> = facts
        .iter()
        .filter_map(|fact| match fact.get("repo") {
            Some(Value::String(repo)) if !repo.is_empty() => Some(repo.clone()),
            _ => None,
        })
        .collect();
    let insight_facts = synthesize_insight_facts(snapshot_dir);
    let flow_facts = synthesize_flow_facts(snapshot_dir, &repos, receipt.as_ref());
    let (insight_count, flow_count) = (insight_facts.len(), flow_facts.len());
    let mut facts = facts;
    facts.extend(insight_facts);
    facts.extend(flow_facts);

    tracing::info!(
        "Parsed {} fact(s) ({} from insights.json, {} from flows.json) from {}",
        facts.len(),
        insight_count,
        flow_count,
        facts_path.display()
    );
    Ok((facts, receipt))
}

/// Read `facts.jsonl`: one JSON object per line, Python's universal newlines
/// (`\n`, `\r\n` and `\r` all end a line), blank lines skipped, corrupt and
/// non-object lines counted and skipped.
fn read_facts(facts_path: &Path) -> CodeGraphResult<Vec<Fact>> {
    let read_error = |error: std::io::Error| {
        CodeGraphError::EnolaSnapshot(format!(
            "Could not read '{}': {error}",
            facts_path.display()
        ))
    };
    let mut reader = BufReader::new(File::open(facts_path).map_err(read_error)?);
    let mut facts = Vec::new();
    let mut corrupt_lines = 0usize;
    let mut line_number = 0usize;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer).map_err(read_error)?;
        if read == 0 {
            break;
        }
        let chunk = std::str::from_utf8(&buffer).map_err(|error| {
            CodeGraphError::EnolaSnapshot(format!(
                "'{}' is not valid UTF-8: {error}",
                facts_path.display()
            ))
        })?;
        for line in chunk.split('\r') {
            if line.is_empty() {
                continue;
            }
            line_number += 1;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(Value::Object(fact)) => facts.push(fact),
                Ok(_) => {
                    corrupt_lines += 1;
                    tracing::warn!(
                        "Skipping non-object fact on line {} of {}",
                        line_number,
                        facts_path.display()
                    );
                }
                Err(_) => {
                    corrupt_lines += 1;
                    tracing::warn!(
                        "Skipping corrupt JSON on line {} of {}",
                        line_number,
                        facts_path.display()
                    );
                }
            }
        }
    }
    if corrupt_lines > 0 {
        tracing::warn!(
            "Skipped {} corrupt line(s) in {}",
            corrupt_lines,
            facts_path.display()
        );
    }
    Ok(facts)
}

/// Read a JSON file: `Ok(None)` when it does not exist, `Err(())` when it
/// exists but cannot be read or parsed.
fn read_json_file(path: &Path) -> Result<Option<Value>, ()> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
    serde_json::from_str(text).map(Some).map_err(|_| ())
}

/// Reject unsupported artifact formats and surface extraction-quality
/// signals. Port of Python's `validate_receipt`.
///
/// An unsupported `format_version` (including `0`, "unknown", and any
/// non-integer) is a hard error; a missing one reads as version 1. A
/// `fact_count` mismatch and the `quality` block are logged, never raised. A
/// missing receipt validates trivially.
///
/// # Errors
///
/// [`CodeGraphError::EnolaSnapshot`] for an unsupported `format_version`.
pub fn validate_receipt(
    receipt: Option<&Receipt>,
    snapshot_dir: &Path,
    fact_count: Option<usize>,
) -> CodeGraphResult<()> {
    let Some(receipt) = receipt else {
        return Ok(());
    };

    if let Some(format_version) = receipt.get("format_version")
        && !format_version.is_null()
    {
        let supported = format_version
            .as_i64()
            .filter(|_| !format_version.is_f64())
            .is_some_and(|version| SUPPORTED_FORMAT_VERSIONS.contains(&version));
        if !supported {
            let enola_version = receipt
                .get("enola_version")
                .map(py_str)
                .unwrap_or_else(|| "?".to_string());
            return Err(CodeGraphError::EnolaSnapshot(format!(
                "Unsupported enola snapshot format_version {} in '{}' (enola {}); this cognee \
                 reads format version(s) {:?}. Upgrade cognee, or pin an enola release that \
                 writes a supported format.",
                py_repr(format_version),
                snapshot_dir.display(),
                enola_version,
                SUPPORTED_FORMAT_VERSIONS,
            )));
        }
    }

    if let (Some(fact_count), Some(declared)) =
        (fact_count, receipt.get("fact_count").and_then(json_int))
        && declared != fact_count as i64
    {
        tracing::warn!(
            "receipt.json declares {} fact(s) but facts.jsonl in {} yielded {}; the snapshot \
             may be truncated or from a different run.",
            declared,
            snapshot_dir.display(),
            fact_count
        );
    }

    let Some(Value::Object(quality)) = receipt.get("quality") else {
        return Ok(());
    };
    if let Some(parse_errors) = quality.get("parse_errors").and_then(json_int)
        && parse_errors > 0
    {
        tracing::warn!(
            "enola reported {} parse error(s) for {}; the code graph may be incomplete. Sample: {}",
            parse_errors,
            snapshot_dir.display(),
            quality
                .get("parse_error_sample")
                .map(py_repr)
                .unwrap_or_else(|| "None".to_string())
        );
    }
    if let (Some(files_seen), Some(files_parsed)) = (
        quality.get("files_seen").and_then(json_int),
        quality.get("files_parsed").and_then(json_int),
    ) && files_parsed < files_seen
    {
        let top_skip_causes = match quality.get("census") {
            Some(Value::Object(census)) => census.get("top_skip_causes").cloned(),
            _ => None,
        };
        let causes = [top_skip_causes, quality.get("skipped_sample").cloned()]
            .into_iter()
            .flatten()
            .find(is_truthy)
            .map(|value| py_repr(&value))
            .unwrap_or_else(|| "n/a".to_string());
        tracing::info!(
            "enola parsed {} of {} source file(s) in {} (top skip causes: {}).",
            files_parsed,
            files_seen,
            snapshot_dir.display(),
            causes
        );
    }
    Ok(())
}

/// A JSON integer (Python `int`, never `bool` or `float`).
fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) if !number.is_f64() => number.as_i64(),
        _ => None,
    }
}

/// Whether `value` is a writer fact identity: 32 lowercase hex characters
/// (enola >= 0.4.10). Port of Python's `is_enola_id`.
pub fn is_enola_id(value: &Value) -> bool {
    value.as_str().is_some_and(is_enola_id_str)
}

/// [`is_enola_id`] for a string.
pub fn is_enola_id_str(value: &str) -> bool {
    value.len() == ENOLA_ID_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The writer-resolved target identity of a relation (`target_id`), or
/// `None`. Port of Python's `relation_target_id`.
pub fn relation_target_id(relation: &Value) -> Option<&str> {
    relation
        .as_object()?
        .get("target_id")
        .and_then(Value::as_str)
        .filter(|value| is_enola_id_str(value))
}

/// Extract `(relation_type, target_name)` from a relation object, probing the
/// alternate key spellings historical writers used. Port of Python's
/// `normalize_relation`.
pub fn normalize_relation(relation: &Value) -> Option<(String, String)> {
    let relation = relation.as_object()?;
    let first_text = |keys: &[&str]| {
        keys.iter().find_map(|key| match relation.get(*key) {
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => None,
        })
    };
    Some((
        first_text(RELATION_TYPE_KEYS)?,
        first_text(RELATION_TARGET_KEYS)?,
    ))
}

/// Stable identity of a snapshot's content, used for incremental skip. Port
/// of Python's `snapshot_identity`.
///
/// Prefers the receipt's `snapshot_id`; falls back to hashing `facts.jsonl`
/// (`sha256:<hex>`). `None` when neither is available.
pub fn snapshot_identity(
    snapshot_dir: impl AsRef<Path>,
    receipt: Option<&Receipt>,
) -> Option<String> {
    if let Some(receipt) = receipt.filter(|receipt| !receipt.is_empty())
        && let Some(Value::String(snapshot_id)) = receipt.get("snapshot_id")
        && !snapshot_id.is_empty()
    {
        return Some(snapshot_id.clone());
    }
    let bytes = std::fs::read(snapshot_dir.as_ref().join("facts.jsonl")).ok()?;
    Some(sha256_tag(&bytes))
}

fn sha256_tag(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Convert `insights.json` explainer findings into facts of kind `insight`.
/// Port of Python's `_synthesize_insight_facts`.
fn synthesize_insight_facts(snapshot_dir: &Path) -> Vec<Fact> {
    let insights = match read_json_file(&snapshot_dir.join("insights.json")) {
        Ok(Some(Value::Array(insights))) => insights,
        Ok(_) => return Vec::new(),
        Err(()) => {
            tracing::warn!(
                "Could not parse insights.json in {}; ignoring it.",
                snapshot_dir.display()
            );
            return Vec::new();
        }
    };

    let mut facts = Vec::new();
    for insight in insights {
        let Value::Object(insight) = insight else {
            continue;
        };
        let title = match insight.get("title") {
            Some(Value::String(title)) if !title.is_empty() => title.clone(),
            _ => continue,
        };
        let props: Map<String, Value> = INSIGHT_PROP_KEYS
            .iter()
            .filter_map(|key| match insight.get(*key) {
                Some(value) if !value.is_null() => Some(((*key).to_string(), value.clone())),
                _ => None,
            })
            .collect();
        let mut relations = Vec::new();
        if let Some(Value::Array(evidence)) = insight.get("evidence") {
            for entry in evidence {
                let Value::Object(entry) = entry else {
                    continue;
                };
                // Python: entry.get("symbol") or entry.get("fact") or entry.get("file")
                let target = ["symbol", "fact", "file"]
                    .iter()
                    .filter_map(|key| entry.get(*key))
                    .find(|value| is_truthy(value));
                if let Some(Value::String(target)) = target {
                    let mut relation = Map::new();
                    relation.insert("kind".into(), Value::from("evidences"));
                    relation.insert("target".into(), Value::from(target.clone()));
                    if let Some(fact_id) = entry.get("fact_id").filter(|id| is_enola_id(id)) {
                        relation.insert("target_id".into(), fact_id.clone());
                    }
                    relations.push(Value::Object(relation));
                }
            }
        }
        let mut fact = Map::new();
        fact.insert("kind".into(), Value::from("insight"));
        fact.insert("name".into(), Value::from(title));
        fact.insert("props".into(), Value::Object(props));
        fact.insert("relations".into(), Value::Array(relations));
        facts.push(fact);
    }
    facts
}

/// Convert `flows.json` entry-point flows into facts of kind `flow`. Port of
/// Python's `_synthesize_flow_facts`.
///
/// Only flows of `repos` (the repo labels `facts.jsonl` carries) are kept;
/// when it carries none, every flow is kept. Only a `flows.json` the receipt
/// vouches for is read ([`receipt_vouches_for_flows`]).
fn synthesize_flow_facts(
    snapshot_dir: &Path,
    repos: &HashSet<String>,
    receipt: Option<&Receipt>,
) -> Vec<Fact> {
    let flows_path = snapshot_dir.join("flows.json");
    if !flows_path.is_file() {
        return Vec::new();
    }
    let Ok(content) = std::fs::read(&flows_path) else {
        tracing::warn!(
            "Could not parse flows.json in {}; ignoring it.",
            snapshot_dir.display()
        );
        return Vec::new();
    };
    if !receipt_vouches_for_flows(receipt, &content) {
        tracing::info!(
            "Ignoring flows.json in {}: receipt.json does not list these bytes among this run's \
             outputs, so another enola run wrote them and their flows may not match these facts.",
            snapshot_dir.display()
        );
        return Vec::new();
    }
    let document = std::str::from_utf8(&content)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let Some(document) = document else {
        tracing::warn!(
            "Could not parse flows.json in {}; ignoring it.",
            snapshot_dir.display()
        );
        return Vec::new();
    };
    let Some(Value::Array(flows)) = document.as_object().and_then(|doc| doc.get("flows")) else {
        return Vec::new();
    };

    let empty_entry = Map::new();
    let mut facts = Vec::new();
    for flow in flows {
        let Value::Object(flow) = flow else {
            continue;
        };
        let name = match flow.get("name") {
            Some(Value::String(name)) if !name.is_empty() => name.clone(),
            _ => continue,
        };
        let repo = flow.get("repo").and_then(Value::as_str);
        if !repos.is_empty() && !repo.is_some_and(|repo| repos.contains(repo)) {
            continue;
        }
        let entry = flow
            .get("entry")
            .and_then(Value::as_object)
            .unwrap_or(&empty_entry);
        let props: Map<String, Value> = FLOW_PROP_KEYS
            .iter()
            .filter_map(|key| match flow.get(*key) {
                Some(value)
                    if !value.is_null() && (*key != "side" || value == &Value::Bool(true)) =>
                {
                    Some(((*key).to_string(), value.clone()))
                }
                _ => None,
            })
            .collect();

        let mut relations = Vec::new();
        let entry_refs = vec![Value::Object(entry.clone())];
        let ref_groups: [(&str, Option<&Vec<Value>>); 3] = [
            ("flow_entry", Some(&entry_refs)),
            ("flow_stage", flow.get("stages").and_then(Value::as_array)),
            ("flow_route", flow.get("routes").and_then(Value::as_array)),
        ];
        for (relation_kind, refs) in ref_groups {
            for reference in refs.into_iter().flatten() {
                let Value::Object(reference) = reference else {
                    continue;
                };
                let Some(Value::String(target)) = reference.get("name") else {
                    continue;
                };
                if target.is_empty() {
                    continue;
                }
                let mut relation = Map::new();
                relation.insert("kind".into(), Value::from(relation_kind));
                relation.insert("target".into(), Value::from(target.clone()));
                if let Some(id) = reference.get("id").filter(|id| is_enola_id(id)) {
                    relation.insert("target_id".into(), id.clone());
                }
                relations.push(Value::Object(relation));
            }
        }

        let mut fact = Map::new();
        fact.insert("kind".into(), Value::from("flow"));
        fact.insert("name".into(), Value::from(name));
        fact.insert("props".into(), Value::Object(props));
        fact.insert("relations".into(), Value::Array(relations));
        if !repos.is_empty() {
            // Always one of the facts' own labels here.
            fact.insert("repo".into(), repo.map(Value::from).unwrap_or(Value::Null));
        }
        if let Some(file) = entry.get("file").filter(|value| !value.is_null()) {
            fact.insert("file".into(), file.clone());
        }
        if let Some(line) = entry.get("line").filter(|value| !value.is_null()) {
            fact.insert("line".into(), line.clone());
        }
        if let Some(id) = flow.get("id").filter(|id| is_enola_id(id)) {
            fact.insert("id".into(), id.clone());
        }
        facts.push(fact);
    }
    disambiguate_flow_names(&mut facts);
    facts
}

/// Group key of a flow: its `repo` value (any JSON, usually a string or
/// absent) and its current name.
fn flow_groups(facts: &[Fact]) -> Vec<((String, String), Vec<usize>)> {
    let mut order: Vec<((String, String), Vec<usize>)> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    for (position, fact) in facts.iter().enumerate() {
        let repo = fact
            .get("repo")
            .map(Value::to_string)
            .unwrap_or_else(|| "null".to_string());
        let name = fact
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let key = (repo, name);
        match index.get(&key) {
            Some(slot) => order[*slot].1.push(position),
            None => {
                index.insert(key.clone(), order.len());
                order.push((key, vec![position]));
            }
        }
    }
    order
}

/// A flow's entry symbol name, the first qualifier of a shared flow name.
fn flow_entry_name(fact: &Fact) -> Option<String> {
    fact.get("props")
        .and_then(|props| props.get("entry"))
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// A flow's enola id, the second qualifier of a shared flow name.
fn flow_enola_id(fact: &Fact) -> Option<String> {
    fact.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Give flows that share a name within a repo distinct names, in place. Port
/// of Python's `_disambiguate_flow_names`: qualify by the entry symbol's
/// name, then by the enola id, then number what still collides in
/// `flows.json` order.
fn disambiguate_flow_names(facts: &mut [Fact]) {
    let qualifiers: [fn(&Fact) -> Option<String>; 2] = [flow_entry_name, flow_enola_id];
    for qualifier in qualifiers {
        for ((_repo, name), group) in flow_groups(facts) {
            if group.len() < 2 {
                continue;
            }
            for position in group {
                let fact = &mut facts[position];
                if let Some(tag) = qualifier(fact) {
                    let current = fact.get("name").and_then(Value::as_str).unwrap_or_default();
                    if !current.ends_with(&format!("({tag})")) {
                        fact.insert("name".into(), Value::from(format!("{name} ({tag})")));
                    }
                }
            }
        }
    }
    for ((_repo, name), group) in flow_groups(facts) {
        if group.len() > 1 {
            for (index, position) in group.into_iter().enumerate() {
                facts[position]
                    .insert("name".into(), Value::from(format!("{name} #{}", index + 1)));
            }
        }
    }
}

/// Whether `receipt.json` says this run wrote `flows.json` with these bytes.
/// Port of Python's `_receipt_vouches_for_flows`.
///
/// With `output_hashes`, the recorded hash must match; without it, the
/// `renderers` list must name `flows`. Without a receipt the file is read.
pub fn receipt_vouches_for_flows(receipt: Option<&Receipt>, content: &[u8]) -> bool {
    let Some(receipt) = receipt else {
        return true;
    };
    if let Some(Value::Object(output_hashes)) = receipt.get("output_hashes") {
        return output_hashes.get("flows.json").and_then(Value::as_str)
            == Some(sha256_tag(content).as_str());
    }
    matches!(receipt.get("renderers"), Some(Value::Array(renderers)) if renderers.iter().any(|renderer| renderer == "flows"))
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
    fn enola_ids_are_32_lowercase_hex_characters() {
        let cases = [
            (json!("a".repeat(32)), true),
            (json!("b40cc8199deadc4199623e0a6a8c64b1"), true),
            (json!("B40CC8199DEADC4199623E0A6A8C64B1"), false),
            (json!("a".repeat(31)), false),
            (json!("a".repeat(33)), false),
            (json!("g".repeat(32)), false),
            (json!(""), false),
            (Value::Null, false),
            (json!(12_345_678_901_234_567_890_u64), false),
        ];
        for (value, expected) in cases {
            assert_eq!(is_enola_id(&value), expected, "{value}");
        }
    }

    #[test]
    fn relation_target_id_needs_an_enola_id() {
        let id = "a".repeat(32);
        assert_eq!(
            relation_target_id(&json!({"kind": "calls", "target": "x", "target_id": id})),
            Some(id.as_str())
        );
        assert_eq!(
            relation_target_id(&json!({"kind": "calls", "target": "x"})),
            None
        );
        assert_eq!(
            relation_target_id(&json!({"kind": "calls", "target": "x", "target_id": "nope"})),
            None
        );
        assert_eq!(relation_target_id(&json!("not a dict")), None);
    }

    #[test]
    fn normalize_relation_tolerates_alternate_key_spellings() {
        let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        let cases = [
            (
                json!({"type": "calls", "target": "helper"}),
                pair("calls", "helper"),
            ),
            (
                json!({"kind": "imports", "to": "utils"}),
                pair("imports", "utils"),
            ),
            (
                json!({"rel": "calls", "target_name": "helper"}),
                pair("calls", "helper"),
            ),
            (
                json!({"relation": "implements", "name": "Storer"}),
                pair("implements", "Storer"),
            ),
            (json!({"foo": "bar"}), None),
            (json!({"type": "calls"}), None),
            (json!({"target": "helper"}), None),
            (json!("not a dict"), None),
        ];
        for (relation, expected) in cases {
            assert_eq!(normalize_relation(&relation), expected, "{relation}");
        }
    }

    #[test]
    fn vouching_follows_output_hashes_then_renderers() {
        let content = b"{\"flows\": []}";
        let hash = sha256_tag(content);
        let receipt = |value: Value| value.as_object().cloned().unwrap();
        assert!(receipt_vouches_for_flows(None, content));
        assert!(receipt_vouches_for_flows(
            Some(&receipt(json!({"output_hashes": {"flows.json": hash}}))),
            content
        ));
        assert!(!receipt_vouches_for_flows(
            Some(&receipt(
                json!({"renderers": ["flows"], "output_hashes": {"facts.jsonl": "x"}})
            )),
            content
        ));
        assert!(receipt_vouches_for_flows(
            Some(&receipt(json!({"renderers": ["flows"]}))),
            content
        ));
        assert!(!receipt_vouches_for_flows(
            Some(&receipt(json!({}))),
            content
        ));
    }
}
