//! Deterministic, LLM-free helpers for the Evidence text block appended to RAG
//! answers when `include_references` is on.
//!
//! Port of `cognee/modules/retrieval/utils/references.py`
//! (`format_chunk_references`, `append_chunk_evidence`). The block is grounded
//! in the generated answer, not in whatever happened to be retrieved: only
//! chunks sharing significant terms with the answer are cited, ranked by that
//! overlap. The text format is byte-identical to Python's.
//!
//! Not ported: `build_answer_grounded_chunk_references` /
//! `append_answer_grounded_evidence` (Python keeps them as compatibility
//! helpers that nothing in the completion path calls any more).

use std::collections::HashSet;

use serde_json::Value;

use crate::types::{SearchItem, SearchOutput};

/// Header on its own line above the bullets (Python `EVIDENCE_HEADER`).
pub const EVIDENCE_HEADER: &str = "Evidence:";

/// Maximum length of a rendered snippet in characters before truncation.
const SNIPPET_MAX_CHARS: usize = 160;
/// Hard upper bound on bullets regardless of the requested limit.
const MAX_BULLETS: usize = 5;
const MIN_LIMIT: usize = 3;

/// Common English words excluded from answer/chunk term-overlap scoring
/// (Python `_STOPWORDS`).
const STOPWORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "all", "also", "an", "and", "any", "are", "as", "at",
    "be", "because", "been", "before", "being", "below", "between", "both", "but", "by", "can",
    "did", "do", "does", "doing", "down", "during", "each", "few", "for", "from", "further", "had",
    "has", "have", "having", "he", "her", "here", "hers", "him", "his", "how", "i", "if", "in",
    "into", "is", "it", "its", "just", "me", "more", "most", "my", "no", "nor", "not", "of", "off",
    "on", "once", "only", "or", "other", "our", "ours", "out", "over", "own", "same", "she",
    "should", "so", "some", "such", "than", "that", "the", "their", "theirs", "them", "then",
    "there", "these", "they", "this", "those", "through", "to", "too", "under", "until", "up",
    "very", "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom", "why",
    "will", "with", "you", "your", "yours",
];

fn clamp_limit(limit: usize) -> usize {
    limit.clamp(MIN_LIMIT, MAX_BULLETS)
}

/// A stripped string, or `None` for a missing, null, non-string or blank value
/// (Python `_clean_str`: numbers are rejected on purpose).
fn clean_str(value: Option<&Value>) -> Option<String> {
    let stripped = value?.as_str()?.trim();
    (!stripped.is_empty()).then(|| stripped.to_string())
}

/// Collapse whitespace and truncate to a short snippet (Python `_snippet`).
fn snippet(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SNIPPET_MAX_CHARS {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(SNIPPET_MAX_CHARS - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// The 1-based display number: an explicit positive `chunk_number`, else the
/// 0-based `chunk_index + 1` (Python `_chunk_number`). JSON booleans are not
/// integers here, as Python's bool guard intends.
fn chunk_number(payload: &Value) -> Option<i64> {
    if let Some(number) = payload.get("chunk_number").and_then(Value::as_i64)
        && number > 0
    {
        return Some(number);
    }
    payload
        .get("chunk_index")
        .and_then(Value::as_i64)
        .filter(|index| *index >= 0)
        .map(|index| index + 1)
}

/// `" (data_id: ..., chunk_id: ...)"` for whichever ids exist (Python
/// `_provenance_suffix`).
fn provenance_suffix(data_id: Option<&str>, chunk_id: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(data_id) = data_id {
        parts.push(format!("data_id: {data_id}"));
    }
    if let Some(chunk_id) = chunk_id {
        parts.push(format!("chunk_id: {chunk_id}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// A stable chunk id for dedup: the item id, else `payload["id"]` (Python
/// `_chunk_id`). An empty id is falsy in Python: the dedup key falls back to
/// `document#number` and the provenance suffix omits it, so it reads as `None`.
fn chunk_id(item: &SearchItem) -> Option<String> {
    if let Some(id) = item.id {
        return Some(id.to_string());
    }
    match item.payload.get("id") {
        Some(Value::String(id)) => (!id.is_empty()).then(|| id.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn ascii_terms(text: &str) -> impl Iterator<Item = String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Lowercased alphanumeric terms of an answer, minus stopwords and stubs
/// (Python `_significant_terms`).
fn significant_terms(text: &str) -> HashSet<String> {
    ascii_terms(text)
        .filter(|term| term.len() >= 3 && !STOPWORDS.contains(&term.as_str()))
        .collect()
}

struct Candidate {
    overlap: usize,
    document_name: String,
    number: i64,
    text: String,
    data_id: Option<String>,
    chunk_id: Option<String>,
}

/// Build an Evidence block from retrieved vector payloads, grounded in the
/// answer (Python `format_chunk_references`).
///
/// Reads `document_name`, `chunk_number` (falling back to `chunk_index + 1`)
/// and `text` from each item's payload; items missing any of them are skipped.
/// Results are deduplicated by chunk id and capped at 3-5 bullets. With an
/// `answer`, candidates sharing no significant term with it are dropped and
/// the rest ranked by overlap; an answer with no significant terms yields
/// `""` (it cannot be grounded). Returns `""` when nothing is usable.
pub fn format_chunk_references(
    retrieved: &[SearchItem],
    answer: Option<&str>,
    limit: usize,
) -> String {
    if retrieved.is_empty() {
        return String::new();
    }

    let answer_terms = match answer {
        Some(answer) => {
            let terms = significant_terms(answer);
            if terms.is_empty() {
                // Nothing to ground the citation in (e.g. "Yes."): omit Evidence
                // rather than present retrieval order as provenance.
                return String::new();
            }
            Some(terms)
        }
        None => None,
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for item in retrieved {
        let payload = &item.payload;
        if !payload.is_object() {
            continue;
        }
        let (Some(document_name), Some(number), Some(text)) = (
            clean_str(payload.get("document_name")),
            chunk_number(payload),
            clean_str(payload.get("text")),
        ) else {
            continue;
        };

        let chunk_id = chunk_id(item);
        // `document_id` is the ingested Data item's id (cognify sets
        // Document.id = data.id): the id a caller maps a citation back with.
        let data_id = clean_str(payload.get("document_id"));
        let dedup_key = chunk_id
            .clone()
            .unwrap_or_else(|| format!("{document_name}#{number}"));
        if !seen.insert(dedup_key) {
            continue;
        }

        let mut overlap = 0;
        if let Some(answer_terms) = &answer_terms {
            let chunk_terms: HashSet<String> = ascii_terms(&text).collect();
            overlap = answer_terms.intersection(&chunk_terms).count();
            if overlap == 0 {
                // No answer term appears in this chunk: almost certainly not
                // a source of the answer.
                continue;
            }
        }
        candidates.push(Candidate {
            overlap,
            document_name,
            number,
            text,
            data_id,
            chunk_id,
        });
    }

    if candidates.is_empty() {
        return String::new();
    }
    if answer_terms.is_some() {
        // Stable sort: highest answer overlap first, retrieval order breaks ties.
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.overlap));
    }

    let bullets: Vec<String> = candidates
        .iter()
        .take(clamp_limit(limit))
        .map(|candidate| {
            format!(
                "- chunk {} of document {}{}: \"{}\"",
                candidate.number,
                candidate.document_name,
                provenance_suffix(candidate.data_id.as_deref(), candidate.chunk_id.as_deref()),
                snippet(&candidate.text),
            )
        })
        .collect();
    format!("{EVIDENCE_HEADER}\n{}", bullets.join("\n"))
}

/// Append an answer-grounded chunk Evidence block to string answers (Python
/// `append_chunk_evidence`).
///
/// Each string answer gets its own block, filtered and ranked by that answer's
/// text against `context`. Anything that is not a string answer (structured
/// output, items) is returned untouched, and an empty block leaves the answer
/// unchanged.
pub fn append_chunk_evidence(output: SearchOutput, context: &[SearchItem]) -> SearchOutput {
    let append = |answer: String| {
        let evidence = format_chunk_references(context, Some(&answer), MAX_BULLETS);
        if evidence.is_empty() {
            answer
        } else {
            format!("{answer}\n\n{evidence}")
        }
    };
    match output {
        SearchOutput::Text(answer) => SearchOutput::Text(append(answer)),
        SearchOutput::Texts(answers) => {
            SearchOutput::Texts(answers.into_iter().map(append).collect())
        }
        other => other,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn item(payload: Value) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    fn doc(name: &str, index: i64, text: &str) -> SearchItem {
        item(json!({"document_name": name, "chunk_index": index, "text": text}))
    }

    #[test]
    fn renders_one_based_number_from_chunk_index() {
        let block = format_chunk_references(&[doc("a.pdf", 0, "Hello world")], None, 5);
        assert_eq!(
            block,
            "Evidence:\n- chunk 1 of document a.pdf: \"Hello world\""
        );
    }

    #[test]
    fn prefers_explicit_chunk_number() {
        let entry = item(json!({
            "document_name": "a.pdf", "chunk_number": 7, "chunk_index": 0, "text": "Hi there"
        }));
        let block = format_chunk_references(&[entry], None, 5);
        assert!(block.contains("- chunk 7 of document a.pdf"), "{block}");
    }

    #[test]
    fn includes_data_id_and_chunk_id() {
        let id = Uuid::new_v4();
        let entry = SearchItem {
            id: Some(id),
            score: None,
            payload: json!({
                "document_name": "a.pdf", "chunk_index": 1,
                "text": "Body", "document_id": "doc-1"
            }),
        };
        let block = format_chunk_references(&[entry], None, 5);
        assert_eq!(
            block,
            format!(
                "Evidence:\n- chunk 2 of document a.pdf (data_id: doc-1, chunk_id: {id}): \"Body\""
            )
        );
    }

    #[test]
    fn empty_when_a_required_field_is_missing_or_null() {
        for payload in [
            json!({"chunk_index": 0, "text": "t"}),
            json!({"document_name": null, "chunk_index": 0, "text": "t"}),
            json!({"document_name": "a", "text": "t"}),
            json!({"document_name": "a", "chunk_index": 0}),
            json!({"document_name": 5, "chunk_index": 0, "text": "t"}),
        ] {
            assert_eq!(format_chunk_references(&[item(payload)], None, 5), "");
        }
        assert_eq!(format_chunk_references(&[], None, 5), "");
    }

    #[test]
    fn dedups_by_id_and_by_document_chunk_signature() {
        let id = Uuid::new_v4();
        let with_id = |text: &str| SearchItem {
            id: Some(id),
            score: None,
            payload: json!({"document_name": "a", "chunk_index": 0, "text": text}),
        };
        let block = format_chunk_references(&[with_id("first"), with_id("second")], None, 5);
        assert_eq!(block.matches("- chunk").count(), 1);
        let block =
            format_chunk_references(&[doc("a", 0, "first"), doc("a", 0, "second")], None, 5);
        assert_eq!(block.matches("- chunk").count(), 1);
    }

    /// An empty `payload.id` is falsy in Python: it neither dedups nor shows
    /// in the provenance suffix.
    #[test]
    fn empty_payload_id_is_treated_as_missing() {
        let blank = |text: &str| {
            item(json!({"id": "", "document_name": "d", "chunk_index": 0, "text": text}))
        };
        let other = item(json!({"id": "", "document_name": "d", "chunk_index": 1, "text": "y"}));
        assert_eq!(
            format_chunk_references(&[blank("x"), blank("again"), other], None, 5),
            "Evidence:\n- chunk 1 of document d: \"x\"\n- chunk 2 of document d: \"y\""
        );
    }

    #[test]
    fn caps_and_clamps_limit() {
        let chunks: Vec<_> = (0..8).map(|i| doc("a", i, "text body")).collect();
        let count = |limit| {
            format_chunk_references(&chunks, None, limit)
                .matches("- chunk")
                .count()
        };
        assert_eq!(count(1), 3);
        assert_eq!(count(4), 4);
        assert_eq!(count(99), 5);
    }

    #[test]
    fn snippet_collapses_whitespace_and_truncates() {
        let long = "word ".repeat(100);
        let block = format_chunk_references(&[doc("a", 0, &long)], None, 5);
        let rendered = block.split('"').nth(1).unwrap();
        assert!(rendered.ends_with('\u{2026}'));
        assert!(rendered.chars().count() <= 160);
        let block = format_chunk_references(&[doc("a", 0, "a\n  b\t c")], None, 5);
        assert!(block.ends_with("\"a b c\""), "{block}");
    }

    #[test]
    fn answer_filtering_drops_chunks_without_overlap_and_ranks_by_overlap() {
        let chunks = [
            doc("low", 0, "Revenue was mentioned once"),
            doc("none", 0, "Penguins live in Antarctica"),
            doc("high", 0, "Revenue grew twelve percent last year"),
        ];
        let block = format_chunk_references(&chunks, Some("Revenue grew twelve percent."), 5);
        let docs: Vec<_> = block
            .lines()
            .skip(1)
            .map(|line| line.split(' ').nth(5).unwrap().to_string())
            .collect();
        assert_eq!(docs, ["high:", "low:"]);
        assert_eq!(
            format_chunk_references(&chunks, Some("Unrelated zebra answer"), 5),
            ""
        );
    }

    #[test]
    fn answer_with_no_significant_terms_yields_no_evidence() {
        let chunks = [doc("a", 0, "Yes indeed")];
        assert_eq!(format_chunk_references(&chunks, Some("No."), 5), "");
        assert!(!format_chunk_references(&chunks, None, 5).is_empty());
    }

    #[test]
    fn append_only_touches_string_answers() {
        let chunks = [doc(
            "report.pdf",
            0,
            "Revenue grew 12 percent year over year.",
        )];
        let answer = "Revenue grew 12 percent.";

        let SearchOutput::Text(text) =
            append_chunk_evidence(SearchOutput::Text(answer.into()), &chunks)
        else {
            panic!("text stays text");
        };
        assert!(
            text.starts_with(&format!("{answer}\n\nEvidence:\n")),
            "{text}"
        );
        assert!(text.contains("- chunk 1 of document report.pdf:"));

        let structured = SearchOutput::Structured(json!({"answer": "structured"}));
        assert!(matches!(
            append_chunk_evidence(structured, &chunks),
            SearchOutput::Structured(value) if value == json!({"answer": "structured"})
        ));
        assert!(matches!(
            append_chunk_evidence(SearchOutput::Items(vec![]), &chunks),
            SearchOutput::Items(items) if items.is_empty()
        ));

        let SearchOutput::Texts(texts) = append_chunk_evidence(
            SearchOutput::Texts(vec![answer.into(), "Penguins swim.".into()]),
            &chunks,
        ) else {
            panic!("texts stay texts");
        };
        assert!(texts[0].contains("Evidence:"));
        assert_eq!(texts[1], "Penguins swim.");
    }
}
