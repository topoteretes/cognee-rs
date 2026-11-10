//! Normalization of recall/search retriever output into one wire shape.
//!
//! Mirrors Python `cognee/modules/recall/methods/normalize_search_payload.py`
//! and `cognee/modules/recall/types/SearchResultItem.py`. Retrievers produce
//! heterogeneous payloads (strings, chunk dicts, graph rows, edge lists);
//! [`build_graph_entry`] flattens one retriever output element into a
//! [`NormalizedResult`] with a semantic [`RecallResultKind`], renderable
//! `text`, optional `score`, provenance `metadata` and the verbatim `raw`
//! payload. Both the HTTP server and the SDK bindings call it, so every
//! surface reports the same shape.
//!
//! Deviation from Python (validation note V5): Python's `_score_from` accepts
//! `isinstance(score, (int, float))`, which is true for `bool`, so a
//! `{"score": True}` payload scores `1.0`. `score_from` deliberately
//! excludes booleans.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{SearchResponse, SearchType};

/// Semantic kind of a recall result item.
///
/// Mirrors Python's `SearchResultKind`
/// (`cognee/modules/recall/types/SearchResultItem.py`). `SearchResultItem`
/// carries `model_config = ConfigDict(use_enum_values=True)`, so the enum
/// serializes as its lowercase string value, not its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallResultKind {
    GraphCompletion,
    RagCompletion,
    Cypher,
    Chunk,
    Summary,
    CodingRule,
    Code,
    /// `SearchType.SKILLS` in Python; no Rust search type maps here yet.
    Skill,
    NaturalLanguage,
    Temporal,
    /// Set when a `response_model` was supplied and parsing succeeded.
    Structured,
    Unknown,
}

impl RecallResultKind {
    /// Mirrors Python's `_KIND_BY_SEARCH_TYPE`
    /// (`normalize_search_payload.py:21-39`). A search type absent from
    /// Python's map falls through its `dict.get(...)` default to `UNKNOWN`;
    /// the Rust-only `Feedback` and the unmapped `FeelingLucky` and `GraphReport`
    /// do the same.
    pub fn for_search_type(search_type: SearchType) -> Self {
        use SearchType as S;
        match search_type {
            S::GraphCompletion
            | S::GraphCompletionCot
            | S::GraphCompletionDecomposition
            | S::GraphCompletionContextExtension
            | S::GraphSummaryCompletion
            | S::HybridCompletion => Self::GraphCompletion,
            S::RagCompletion => Self::RagCompletion,
            S::Cypher => Self::Cypher,
            S::NaturalLanguage => Self::NaturalLanguage,
            S::Temporal => Self::Temporal,
            S::CodingRules => Self::CodingRule,
            S::Chunks | S::ChunksLexical => Self::Chunk,
            S::Summaries => Self::Summary,
            S::FeelingLucky | S::GraphReport | S::Feedback => Self::Unknown,
        }
    }
}

/// One normalized graph result: a `source: "graph"` entry of recall's
/// response (the HTTP server's `/api/v1/recall` and the SDK bindings'
/// `normalizedResults`).
///
/// Mirrors Python's `ResponseGraphEntry(SearchResultItem)` — the
/// `SearchResultItem` fields in declaration order, then the `source`
/// discriminator the subclass adds
/// (`cognee/modules/recall/types/RecallResponse.py:25-26`,
/// `SearchResultItem.py:49-82`).
///
/// Every key is snake_case: both classes are bare `BaseModel`s with no
/// `alias_generator`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedResult {
    pub kind: RecallResultKind,
    /// Effective search type, SCREAMING_SNAKE_CASE on the wire (the core
    /// `SearchType` enum already serializes that way).
    ///
    /// Typed as the core enum, not a request-wire enum, because the value
    /// is whatever `run_graph` actually resolved — which for an auto-routed
    /// query can be a variant a request wire does not accept.
    pub search_type: SearchType,
    /// Always populated and always renderable. For an `onlyContext` recall
    /// on a completion type, the user prompt the LLM would have received.
    pub text: String,
    /// The system prompt that goes with `text` on an `onlyContext` recall
    /// whose retrieval found something; `null` otherwise. Python's
    /// `SearchResultItem.system_prompt` (set in `normalize_search_payload`).
    pub system_prompt: Option<String>,
    pub score: Option<f64>,
    pub dataset_id: Option<String>,
    pub dataset_name: Option<String>,
    pub metadata: Map<String, Value>,
    /// The original payload, preserved verbatim.
    pub raw: Map<String, Value>,
    pub structured: Option<Value>,
    /// Discriminator — always `"graph"` for this variant.
    pub source: &'static str,
}

impl NormalizedResult {
    /// Surface the response's evidence references as `metadata.evidence`
    /// (Python `_build_item`, `normalize_search_payload.py:129-130`): only
    /// when there is any, and as the snake_case `model_dump(mode="json")`
    /// shape, one list shared by every entry of the dataset's payload.
    pub fn attach_evidence(&mut self, evidence: &[crate::types::EvidenceReference]) {
        if evidence.is_empty() {
            return;
        }
        // Serializing plain strings, numbers and options cannot fail; an
        // unreachable failure leaves the key off rather than panic.
        if let Ok(value) = serde_json::to_value(evidence) {
            self.metadata.insert("evidence".to_string(), value);
        }
    }
}

/// Build one entry from one retriever output element.
///
/// Mirrors Python `_build_item` (`normalize_search_payload.py:102-142`)
/// composed with `SearchResultItem`'s field assignment. `entry` is one
/// retriever output element as JSON (one graph `RecallItem`'s content).
/// `system_prompt` is left `None`; the caller sets it from
/// [`only_context_system_prompt`].
pub fn build_graph_entry(
    entry: Value,
    search_type: SearchType,
    dataset_id: Option<String>,
    dataset_name: Option<String>,
) -> NormalizedResult {
    let kind = RecallResultKind::for_search_type(search_type);
    let score = score_from(&entry);

    // A structured answer goes through `build_structured_entry` instead.
    let (text, raw) = match entry {
        Value::String(s) => {
            let mut raw = Map::new();
            raw.insert("value".to_string(), Value::String(s.clone()));
            (s, raw)
        }
        Value::Object(map) => (text_from_dict(&map), map),
        Value::Array(items) => {
            let coerced: Vec<Value> = items.iter().map(coerce_to_dict_value).collect();
            let listed = Value::Array(coerced);
            let text = python_json_dumps(&listed);
            let mut raw = Map::new();
            raw.insert("value".to_string(), listed);
            (text, raw)
        }
        other => {
            // Python's `_coerce_to_dict` fallback for a scalar:
            // `{"value": <scalar>}`, then `_text_from_dict` on it — which
            // finds no readable key and dumps the dict.
            let mut raw = Map::new();
            raw.insert("value".to_string(), other);
            (text_from_dict(&raw), raw)
        }
    };

    NormalizedResult {
        metadata: provenance_metadata(&raw),
        kind,
        search_type,
        text,
        system_prompt: None,
        score,
        dataset_id,
        dataset_name,
        raw,
        structured: None,
        source: "graph",
    }
}

/// Build the entry for a structured answer (`SearchOutput::Structured`):
/// kind `STRUCTURED`, the answer in `structured` and `raw`, `text` drawn from
/// it as from any object, and no score.
pub fn build_structured_entry(
    answer: Value,
    search_type: SearchType,
    dataset_id: Option<String>,
    dataset_name: Option<String>,
) -> NormalizedResult {
    let mut entry = build_graph_entry(answer.clone(), search_type, dataset_id, dataset_name);
    entry.kind = RecallResultKind::Structured;
    entry.score = None;
    entry.structured = Some(answer);
    entry
}

/// The `system_prompt` every entry of one search response carries.
///
/// Python `normalize_search_payload` (`normalize_search_payload.py:149-157`):
/// an `only_context` search whose retrieval found something answers one item
/// whose `text` is the user prompt and whose `system_prompt` is the system
/// message that goes with it. The orchestrator only sets
/// [`SearchResponse::user_prompt`] in exactly that case (and then makes it the
/// `result`), so the system prompt applies iff a user prompt is present;
/// every other entry gets `None`.
pub fn only_context_system_prompt(response: &SearchResponse) -> Option<String> {
    response
        .user_prompt
        .as_ref()
        .and(response.system_prompt.clone())
}

/// Python `_score_from` (`normalize_search_payload.py:70-75`): a score only
/// comes from a dict entry carrying a numeric `score`.
///
/// Deliberate deviation (validation note V5): Python's
/// `isinstance(score, (int, float))` is also true for `bool`, so
/// `{"score": True}` becomes `1.0` there. A JSON boolean is not a number
/// here, so it yields `None`.
pub fn score_from(entry: &Value) -> Option<f64> {
    entry
        .as_object()
        .and_then(|map| map.get("score"))
        .and_then(|v| v.as_f64())
}

/// Python `_text_from_dict` (`normalize_search_payload.py:58-66`): the first
/// non-empty string among a fixed key preference list, else the dict dumped
/// as JSON.
pub fn text_from_dict(payload: &Map<String, Value>) -> String {
    for key in ["text", "completion", "summary", "name", "content", "answer"] {
        if let Some(Value::String(s)) = payload.get(key)
            && !s.is_empty()
        {
            return s.clone();
        }
    }
    python_json_dumps(&Value::Object(payload.clone()))
}

/// Python `_coerce_to_dict` (`normalize_search_payload.py:42-55`) for one
/// element of a list entry. A JSON value is already "plain", so only the
/// scalar wrapping applies.
fn coerce_to_dict_value(item: &Value) -> Value {
    match item {
        Value::Object(_) => item.clone(),
        other => {
            let mut map = Map::new();
            map.insert("value".to_string(), other.clone());
            Value::Object(map)
        }
    }
}

/// Python `_provenance_metadata` (`normalize_search_payload.py:78-99`):
/// surface stable source identifiers, including only the keys present.
pub fn provenance_metadata(raw: &Map<String, Value>) -> Map<String, Value> {
    let mut metadata = Map::new();
    if let Some(v) = raw.get("document_id").filter(|v| !v.is_null()) {
        metadata.insert("data_id".to_string(), Value::String(stringify(v)));
    }
    if let Some(v) = raw.get("id").filter(|v| !v.is_null()) {
        metadata.insert("chunk_id".to_string(), Value::String(stringify(v)));
    }
    if let Some(v) = raw.get("chunk_index")
        && !v.is_boolean()
        && let Some(i) = v.as_i64()
    {
        metadata.insert("chunk_index".to_string(), Value::from(i));
    }
    if let Some(v) = raw.get("document_name").filter(|v| !v.is_null()) {
        metadata.insert("document_name".to_string(), Value::String(stringify(v)));
    }
    metadata
}

/// Python `str(value)` for the scalars these provenance fields carry: a JSON
/// string yields its contents, anything else its JSON rendering.
pub fn stringify(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Serialize like Python's `json.dumps(..., ensure_ascii=False)`.
///
/// `serde_json::to_string` is byte-identical except for separators: Python
/// defaults to `", "` and `": "`, serde to `","` and `":"`. This only ever
/// reaches the wire through the `text` fallback for a payload with no
/// readable key, but that fallback is exactly where a diff would otherwise
/// show a spurious mismatch against Python.
pub fn python_json_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_python_json(value, &mut out);
    out
}

fn write_python_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                write_python_json(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_python_json(v, out);
            }
            out.push(']');
        }
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        other => out.push_str(&other.to_string()),
    }
}

/// Recover the dataset a retriever output element came from, from the
/// membership its own payload carries (`dataset_id`, else the first parseable
/// `dataset_ids` entry; looked up under a `payload` wrapper too). Completion
/// texts carry none and return `None`; a caller aligns those with
/// [`SearchResponse::datasets`] instead (see [`entry_dataset_ids`]).
pub fn item_dataset_id(value: &Value) -> Option<uuid::Uuid> {
    let object = value.as_object()?;
    let payload = object
        .get("payload")
        .and_then(Value::as_object)
        .unwrap_or(object);
    payload
        .get("dataset_id")
        .and_then(Value::as_str)
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .or_else(|| {
            payload
                .get("dataset_ids")?
                .as_array()?
                .iter()
                .find_map(|id| uuid::Uuid::parse_str(id.as_str()?).ok())
        })
}

/// The dataset of each of a response's `entries` (its graph output elements,
/// in order): the element's own membership ([`item_dataset_id`]), else — when
/// the response lists exactly one dataset per element, as a per-dataset
/// fan-out of completion answers does — the dataset at the same position,
/// else the response's dataset when it searched exactly one (every element
/// then came from it). `None` where none of these recovers it.
pub fn entry_dataset_ids(entries: &[&Value], response: &SearchResponse) -> Vec<Option<uuid::Uuid>> {
    let datasets = response.datasets.as_deref().unwrap_or_default();
    let aligned = datasets.len() == entries.len();
    let only = match datasets {
        [only] => Some(*only),
        _ => None,
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            item_dataset_id(entry)
                .or_else(|| aligned.then(|| datasets[index]))
                .or(only)
        })
        .collect()
}

/// The evidence that belongs to one entry of a response (Python attaches each
/// dataset's payload evidence to that payload's entries only).
///
/// A response over at most one dataset has one evidence list, which every
/// entry shares. One merged over several datasets stamps each reference with
/// its dataset id, so an entry takes only its own dataset's references. The
/// whole list is kept when the entry's dataset is unknown, or when no
/// reference carries a dataset id at all (a shared-store search over several
/// datasets, which cannot be split) — never none.
pub fn entry_evidence(
    evidence: &[crate::types::EvidenceReference],
    response_dataset_count: usize,
    entry_dataset_id: Option<&str>,
) -> Vec<crate::types::EvidenceReference> {
    let splittable = response_dataset_count >= 2
        && evidence
            .iter()
            .any(|reference| reference.dataset_id.is_some());
    evidence
        .iter()
        .filter(|reference| match entry_dataset_id {
            Some(dataset) if splittable => reference.dataset_id.as_deref() == Some(dataset),
            _ => true,
        })
        .cloned()
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
    use SearchType as CoreSearchType;

    mod per_dataset_evidence {
        use serde_json::json;

        use super::super::{entry_dataset_ids, entry_evidence, item_dataset_id};
        use crate::types::SearchType;
        use crate::types::{EvidenceKind, EvidenceReference, SearchOutput, SearchResponse};

        const DS_A: &str = "11111111-1111-4111-8111-111111111111";
        const DS_B: &str = "22222222-2222-4222-8222-222222222222";

        fn stamped(id: &str, dataset: Option<&str>) -> EvidenceReference {
            let mut reference = EvidenceReference::new(EvidenceKind::GraphNode, id);
            reference.dataset_id = dataset.map(str::to_string);
            reference
        }

        fn ids(evidence: &[EvidenceReference]) -> Vec<&str> {
            evidence.iter().map(|r| r.artifact_id.as_str()).collect()
        }

        #[test]
        fn entry_evidence_splits_a_multi_dataset_response_by_dataset() {
            let evidence = [stamped("a", Some(DS_A)), stamped("b", Some(DS_B))];
            assert_eq!(ids(&entry_evidence(&evidence, 1, Some(DS_A))), ["a", "b"]);
            assert_eq!(ids(&entry_evidence(&evidence, 2, Some(DS_A))), ["a"]);
            assert_eq!(ids(&entry_evidence(&evidence, 2, Some(DS_B))), ["b"]);
            assert_eq!(ids(&entry_evidence(&evidence, 2, None)), ["a", "b"]);
        }

        /// A shared-store search over several datasets stamps no dataset on
        /// its references; the list cannot be split, so every entry keeps it.
        #[test]
        fn unstamped_evidence_is_never_split_away() {
            let shared = [stamped("s", None)];
            assert_eq!(ids(&entry_evidence(&shared, 3, None)), ["s"]);
            assert_eq!(ids(&entry_evidence(&shared, 3, Some(DS_A))), ["s"]);
        }

        #[test]
        fn item_dataset_id_reads_payload_membership() {
            assert_eq!(
                item_dataset_id(&json!({"dataset_id": DS_A}))
                    .unwrap()
                    .to_string(),
                DS_A
            );
            assert_eq!(
                item_dataset_id(&json!({"payload": {"dataset_ids": ["junk", DS_B]}}))
                    .unwrap()
                    .to_string(),
                DS_B
            );
            assert_eq!(item_dataset_id(&json!("answer text")), None);
        }

        #[test]
        fn entry_dataset_ids_align_answers_with_the_response_datasets() {
            let mut response = SearchResponse::from_output(
                SearchType::GraphCompletion,
                SearchOutput::Texts(vec!["a".into(), "b".into()]),
            );
            let a = uuid::Uuid::parse_str(DS_A).unwrap();
            let b = uuid::Uuid::parse_str(DS_B).unwrap();
            response.datasets = Some(vec![a, b]);
            let (first, second) = (json!("a"), json!("b"));
            assert_eq!(
                entry_dataset_ids(&[&first, &second], &response),
                [Some(a), Some(b)]
            );
            // Misaligned: answers without membership stay unknown.
            assert_eq!(entry_dataset_ids(&[&first], &response), [None]);
            // Own membership wins over position.
            let item = json!({"dataset_id": DS_B});
            assert_eq!(entry_dataset_ids(&[&item], &response), [Some(b)]);

            // A response over one dataset owns every element it returned.
            response.datasets = Some(vec![a]);
            assert_eq!(
                entry_dataset_ids(&[&first, &second], &response),
                [Some(a), Some(a)]
            );
            // No datasets on the response: only membership recovers one.
            response.datasets = None;
            assert_eq!(
                entry_dataset_ids(&[&first, &item], &response),
                [None, Some(b)]
            );
        }
    }
    use serde_json::json;
    // ── RecallResponse / ResponseGraphEntry shape ────────────────────────

    /// The ten keys Python's `ResponseGraphEntry` puts on the wire, for the
    /// common case: a completion string from a graph retriever.
    ///
    /// Rust used to answer `{"text": ..., "_source": "graph"}`, which is
    /// what the `test_recall_parity` diff reported as
    /// `keys only in py: [dataset_id, dataset_name, kind, metadata, raw,
    /// score, search_type, source, structured]`.
    #[test]
    fn graph_entry_from_string_matches_python_shape() {
        let entry = build_graph_entry(
            Value::String("Guido created Python.".into()),
            CoreSearchType::HybridCompletion,
            Some("11111111-1111-1111-1111-111111111111".into()),
            Some("notes".into()),
        );
        let v = serde_json::to_value(&entry).expect("to_value");
        let obj = v.as_object().expect("object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "dataset_id",
                "dataset_name",
                "kind",
                "metadata",
                "raw",
                "score",
                "search_type",
                "source",
                "structured",
                "system_prompt",
                "text",
            ]
        );

        assert_eq!(obj["kind"], "graph_completion");
        assert!(obj["system_prompt"].is_null());
        assert_eq!(obj["search_type"], "HYBRID_COMPLETION");
        assert_eq!(obj["text"], "Guido created Python.");
        assert!(obj["score"].is_null());
        assert_eq!(obj["dataset_id"], "11111111-1111-1111-1111-111111111111");
        assert_eq!(obj["dataset_name"], "notes");
        // `_provenance_metadata({"value": ...})` finds no provenance key.
        assert_eq!(obj["metadata"], json!({}));
        assert_eq!(obj["raw"], json!({"value": "Guido created Python."}));
        assert!(obj["structured"].is_null());
        assert_eq!(obj["source"], "graph");
        // The legacy key must be gone.
        assert!(!obj.contains_key("_source"));
    }

    /// `_KIND_BY_SEARCH_TYPE` (`normalize_search_payload.py:21-39`): every
    /// graph-ish completion collapses to `graph_completion`, and a type
    /// absent from Python's map falls through to `unknown`.
    #[test]
    fn graph_entry_kind_follows_python_map() {
        use CoreSearchType as S;
        let cases = [
            (S::GraphCompletion, "graph_completion"),
            (S::GraphCompletionCot, "graph_completion"),
            (S::GraphCompletionDecomposition, "graph_completion"),
            (S::GraphCompletionContextExtension, "graph_completion"),
            (S::GraphSummaryCompletion, "graph_completion"),
            (S::HybridCompletion, "graph_completion"),
            (S::RagCompletion, "rag_completion"),
            (S::Cypher, "cypher"),
            (S::NaturalLanguage, "natural_language"),
            (S::Temporal, "temporal"),
            (S::CodingRules, "coding_rule"),
            (S::Chunks, "chunk"),
            (S::ChunksLexical, "chunk"),
            (S::Summaries, "summary"),
            // Not in Python's map -> dict.get(...) default.
            (S::FeelingLucky, "unknown"),
            (S::GraphReport, "unknown"),
            (S::Feedback, "unknown"),
        ];
        for (search_type, expected) in cases {
            let kind = RecallResultKind::for_search_type(search_type);
            let wire = serde_json::to_value(kind).expect("to_value");
            assert_eq!(wire, expected, "{search_type:?}");
        }
    }

    /// A structured answer is kind `STRUCTURED`, carries the answer in
    /// `structured` and `raw`, draws `text` from it, and has no score even
    /// when the answer has a `score` field.
    #[test]
    fn structured_entry_carries_the_answer_without_a_score() {
        let answer = json!({ "text": "Alice is the CTO", "score": 0.9 });
        let entry = build_structured_entry(
            answer.clone(),
            CoreSearchType::GraphCompletion,
            Some("ds".into()),
            None,
        );
        assert_eq!(entry.kind, RecallResultKind::Structured);
        assert_eq!(entry.structured, Some(answer.clone()));
        assert_eq!(Value::Object(entry.raw), answer);
        assert_eq!(entry.text, "Alice is the CTO");
        assert_eq!(entry.score, None);
        assert_eq!(entry.dataset_id.as_deref(), Some("ds"));
    }

    /// A dict entry keeps its payload verbatim under `raw`, picks `text` by
    /// Python's key preference (`_text_from_dict`), lifts a numeric `score`
    /// (`_score_from`), and surfaces provenance (`_provenance_metadata`).
    #[test]
    fn graph_entry_from_object_lifts_text_score_and_provenance() {
        let entry = build_graph_entry(
            json!({
                "text": "chunk body",
                "score": 0.75,
                "id": "chunk-1",
                "document_id": "doc-9",
                "document_name": "notes.md",
                "chunk_index": 3,
            }),
            CoreSearchType::Chunks,
            None,
            None,
        );
        assert_eq!(entry.kind, RecallResultKind::Chunk);
        assert_eq!(entry.text, "chunk body");
        assert_eq!(entry.score, Some(0.75));
        assert_eq!(
            serde_json::to_value(&entry.metadata).expect("metadata"),
            json!({
                "data_id": "doc-9",
                "chunk_id": "chunk-1",
                "chunk_index": 3,
                "document_name": "notes.md",
            })
        );
        // `raw` is the payload itself, not a `{"value": ...}` wrapper.
        assert_eq!(entry.raw.get("text"), Some(&json!("chunk body")));
        assert!(entry.dataset_id.is_none());
    }

    /// `_text_from_dict` walks `text, completion, summary, name, content,
    /// answer` in order and skips empty strings.
    #[test]
    fn graph_entry_text_key_preference_matches_python() {
        let entry = build_graph_entry(
            json!({"text": "", "completion": "from completion", "name": "n"}),
            CoreSearchType::GraphCompletion,
            None,
            None,
        );
        assert_eq!(entry.text, "from completion");
    }

    /// `_score_from` only reads a score off a **dict**; a bare string entry
    /// has none, and a non-numeric one is ignored.
    #[test]
    fn graph_entry_score_only_comes_from_a_numeric_dict_field() {
        let from_string = build_graph_entry(
            Value::String("s".into()),
            CoreSearchType::GraphCompletion,
            None,
            None,
        );
        assert!(from_string.score.is_none());

        let non_numeric = build_graph_entry(
            json!({"text": "t", "score": "high"}),
            CoreSearchType::GraphCompletion,
            None,
            None,
        );
        assert!(non_numeric.score.is_none());
    }

    /// A list entry becomes `raw = {"value": [...]}` with each element
    /// coerced to a dict, and `text` is that list dumped as JSON
    /// (`_build_item`'s list branch).
    #[test]
    fn graph_entry_from_array_wraps_and_dumps() {
        let entry = build_graph_entry(
            json!(["a", {"k": 1}]),
            CoreSearchType::GraphCompletion,
            None,
            None,
        );
        assert_eq!(
            entry.raw.get("value"),
            Some(&json!([{"value": "a"}, {"k": 1}]))
        );
        assert_eq!(entry.text, r#"[{"value": "a"}, {"k": 1}]"#);
    }

    /// The JSON fallback for `text` must use Python's `json.dumps`
    /// separators (`", "` / `": "`), not serde's tighter ones, or the two
    /// SDKs disagree on a string neither of them treats as prose.
    #[test]
    fn python_json_dumps_uses_python_separators() {
        assert_eq!(
            python_json_dumps(&json!({"a": 1, "b": [1, 2]})),
            r#"{"a": 1, "b": [1, 2]}"#
        );
        assert_eq!(python_json_dumps(&json!(null)), "null");
        assert_eq!(python_json_dumps(&json!(true)), "true");
    }

    /// Python `test_normalize_search_payload.py::test_chunk_result_exposes_provenance_metadata`.
    #[test]
    fn chunk_result_exposes_provenance_metadata() {
        let entry = build_graph_entry(
            json!({"id": "chunk-9", "document_id": "data-123",
                   "document_name": "report.pdf", "chunk_index": 4,
                   "text": "Revenue grew 12 percent."}),
            SearchType::Chunks,
            None,
            None,
        );
        assert_eq!(entry.metadata["data_id"], "data-123");
        assert_eq!(entry.metadata["chunk_id"], "chunk-9");
        assert_eq!(entry.metadata["chunk_index"], 4);
        assert_eq!(entry.metadata["document_name"], "report.pdf");
        assert_eq!(entry.raw["document_id"], "data-123");
    }

    /// Python `test_completion_result_has_no_provenance_metadata`.
    #[test]
    fn completion_string_has_empty_metadata() {
        let entry = build_graph_entry(
            Value::String("Alexander is the Russian Emperor.".into()),
            SearchType::GraphCompletion,
            None,
            None,
        );
        assert!(entry.metadata.is_empty());
    }

    /// `_provenance_metadata` includes only the keys that are present, and
    /// stringifies non-string ids.
    #[test]
    fn provenance_includes_only_present_keys() {
        let raw = json!({"id": 7, "document_name": null})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            Value::Object(provenance_metadata(&raw)),
            json!({"chunk_id": "7"})
        );
    }

    /// `isinstance(chunk_index, int) and not isinstance(chunk_index, bool)`.
    #[test]
    fn chunk_index_excludes_bool_and_non_integers() {
        for bad in [json!(true), json!("4"), json!(1.5), json!(null)] {
            let raw = json!({"chunk_index": bad}).as_object().unwrap().clone();
            assert!(provenance_metadata(&raw).is_empty(), "{bad}");
        }
    }

    /// Python `test_code_result_preserves_structured_operation_payload`
    /// (Rust has no CODE search type, so the dict branch is checked on a
    /// completion kind) and `test_feeling_lucky_resolved_type_*`.
    #[test]
    fn dict_payload_is_preserved_verbatim_in_raw() {
        let payload = json!({"operation": "find_path", "found": true, "path": [{"name": "main"}]});
        let entry = build_graph_entry(payload.clone(), SearchType::GraphCompletion, None, None);
        assert_eq!(Value::Object(entry.raw), payload);

        let chunk = build_graph_entry(
            json!({"id": "chunk-1", "text": "snippet", "document_id": "doc-1"}),
            SearchType::Chunks,
            None,
            None,
        );
        assert_eq!(chunk.kind, RecallResultKind::Chunk);
        assert_eq!(chunk.search_type, SearchType::Chunks);
    }

    /// Python `test_chunk_result_surfaces_retriever_score` /
    /// `test_summary_result_surfaces_retriever_score` /
    /// `test_chunk_result_without_score_has_none_score`.
    #[test]
    fn scores_surface_per_entry_and_ignore_non_numeric() {
        let scores: Vec<Option<f64>> = [
            json!({"text": "closest", "score": 0.12}),
            json!({"text": "farther", "score": 0.34}),
            json!({"text": "no score"}),
            json!({"text": "bad", "score": "0.5"}),
        ]
        .into_iter()
        .map(|v| build_graph_entry(v, SearchType::Chunks, None, None).score)
        .collect();
        assert_eq!(scores, [Some(0.12), Some(0.34), None, None]);

        let summary = build_graph_entry(
            json!({"text": "A summary.", "made_from": "chunk-1", "score": 0.2}),
            SearchType::Summaries,
            None,
            None,
        );
        assert_eq!(summary.kind, RecallResultKind::Summary);
        assert_eq!(summary.score, Some(0.2));
    }

    /// Deliberate deviation from Python V5: a boolean score is not a score.
    #[test]
    fn boolean_score_is_excluded() {
        assert_eq!(score_from(&json!({"score": true})), None);
        assert_eq!(score_from(&json!({"score": 1})), Some(1.0));
    }

    /// Python `normalize_search_payload.py:149-157`: the system prompt rides
    /// along only when the response carries an `only_context` user prompt.
    #[test]
    fn system_prompt_only_with_user_prompt() {
        use crate::types::SearchOutput;
        let mut response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Text("user prompt".into()),
        );
        response.system_prompt = Some("sys".into());
        assert_eq!(only_context_system_prompt(&response), None);
        response.user_prompt = Some("user prompt".into());
        assert_eq!(
            only_context_system_prompt(&response).as_deref(),
            Some("sys")
        );
        response.system_prompt = None;
        assert_eq!(only_context_system_prompt(&response), None);
    }

    #[test]
    fn skill_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(RecallResultKind::Skill).unwrap(),
            "skill"
        );
        let back: RecallResultKind = serde_json::from_value(json!("skill")).unwrap();
        assert_eq!(back, RecallResultKind::Skill);
    }
}
