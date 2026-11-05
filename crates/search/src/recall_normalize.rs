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
//! `{"score": True}` payload scores `1.0`. [`score_from`] deliberately
//! excludes booleans.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::SearchType;

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

/// One `source: "graph"` entry of the `/api/v1/recall` response.
///
/// Mirrors Python's `ResponseGraphEntry(SearchResultItem)` — the
/// `SearchResultItem` fields in declaration order, then the `source`
/// discriminator the subclass adds
/// (`cognee/modules/recall/types/RecallResponse.py:25-26`,
/// `SearchResultItem.py:47-81`).
///
/// Every key is snake_case: both classes are bare `BaseModel`s with no
/// `alias_generator`, exactly like `SearchResult` (see `crate::dto`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedResult {
    pub kind: RecallResultKind,
    /// Effective search type, SCREAMING_SNAKE_CASE on the wire (the core
    /// `SearchType` enum already serializes that way).
    ///
    /// Typed as the **core** enum, not [`WireSearchType`], because the value
    /// is whatever `run_graph` actually resolved — which for an auto-routed
    /// query can be a variant the request wire does not accept. Documented
    /// to OpenAPI as a string for that reason.
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

/// Build one entry from one retriever output element.
///
/// Mirrors Python `_build_item` (`normalize_search_payload.py:102-137`)
/// composed with `SearchResultItem`'s field assignment. `entry` is one
/// element of a `RecallItem`'s content as `run_graph` produced it.
pub fn build_graph_entry(
    entry: Value,
    search_type: SearchType,
    dataset_id: Option<String>,
    dataset_name: Option<String>,
) -> NormalizedResult {
    let kind = RecallResultKind::for_search_type(search_type);
    let score = score_from(&entry);

    // Python branches on the *Python* type of the entry. A `BaseModel` entry
    // (its `structured` / `kind = STRUCTURED` branch) has no analogue here:
    // `response_schema` is not on recall's wire, so `SearchOutput::Structured`
    // is unreachable from this router and a JSON object lands in the dict
    // branch below, exactly as an already-dumped model would.
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use SearchType as CoreSearchType;
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
