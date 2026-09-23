//! Chunk↔summary pairing and merge.
//!
//! Port of `cognee/modules/retrieval/hybrid/pairs.py`. Python's ad-hoc dict is
//! replaced by the [`ChunkSummaryPair`] struct; the per-query candidate list is
//! tiny (`2 * chunks_top_k` at most), so the linear-scan matching stays O(n²)
//! with a negligible `n`.

use std::collections::HashMap;

use uuid::Uuid;

use crate::retrievers::hybrid::results::{display_value, payload_matches_node_filter, result_id};
use crate::types::SearchItem;

/// One candidate chunk together with the summary paired to it (if any) and the
/// per-lane ranks that fed the fusion.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChunkSummaryPair {
    pub chunk_id: Option<String>,
    pub chunk_text: Option<String>,
    pub summary_id: Option<String>,
    pub summary_text: Option<String>,
    pub chunk: Option<SearchItem>,
    pub vector_rank: Option<usize>,
    pub summary_rank: Option<usize>,
}

/// Build chunk↔summary pairs from the two vector lanes.
///
/// Port of `chunk_summary_pairs` (`pairs.py:15-57`): a loop over the vector
/// chunks (first rank wins; chunks merge by id, else by text), then a summary
/// loop keyed strictly by `source_chunk_id` (node-filtered; hits with no
/// `source_chunk_id` are dropped with a warning).
pub(crate) fn chunk_summary_pairs(
    vector_chunks: &[SearchItem],
    summary_hits: &[SearchItem],
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Vec<ChunkSummaryPair> {
    let mut pairs: Vec<ChunkSummaryPair> = Vec::new();

    for (rank, chunk) in vector_chunks.iter().enumerate() {
        let chunk_id = result_id(chunk);
        let chunk_text = chunk.payload.get("text").and_then(display_value);
        if chunk_id.is_none() && chunk_text.is_none() {
            continue;
        }

        let index =
            match find_chunk_summary_pair(&pairs, chunk_id.as_deref(), chunk_text.as_deref()) {
                Some(index) => index,
                None => {
                    pairs.push(new_chunk_summary_pair(chunk_id.clone(), chunk_text.clone()));
                    pairs.len() - 1
                }
            };

        let pair = &mut pairs[index];
        if pair.chunk.is_none() {
            set_pair_chunk(pair, chunk);
        }
        if pair.vector_rank.is_none() {
            pair.vector_rank = Some(rank);
        }
    }

    for (rank, summary) in summary_hits.iter().enumerate() {
        if !payload_matches_node_filter(&summary.payload, node_name, node_name_filter_operator) {
            continue;
        }

        let Some(chunk_id) = summary
            .payload
            .get("source_chunk_id")
            .and_then(display_value)
        else {
            tracing::warn!(
                summary_id = ?result_id(summary),
                "TextSummary_text hit has no source_chunk_id"
            );
            continue;
        };

        let index = match find_chunk_summary_pair(&pairs, Some(chunk_id.as_str()), None) {
            Some(index) => index,
            None => {
                pairs.push(new_chunk_summary_pair(Some(chunk_id.clone()), None));
                pairs.len() - 1
            }
        };

        let pair = &mut pairs[index];
        if pair.summary_rank.is_none() {
            pair.summary_rank = Some(rank);
            pair.summary_id = result_id(summary);
            pair.summary_text = summary.payload.get("text").and_then(display_value);
        }
    }

    pairs
}

/// Chunk ids that need a source-chunk backfill: pairs with a `summary_rank` but
/// no `chunk` yet and a non-empty `chunk_id`.
///
/// Port of `source_chunk_ids_to_load` (`pairs.py:68-73`).
pub(crate) fn source_chunk_ids_to_load(pairs: &[ChunkSummaryPair]) -> Vec<String> {
    pairs
        .iter()
        .filter(|pair| {
            pair.summary_rank.is_some()
                && pair.chunk.is_none()
                && pair.chunk_id.as_deref().is_some_and(|id| !id.is_empty())
        })
        .filter_map(|pair| pair.chunk_id.clone())
        .collect()
}

/// Attach backfilled source chunks onto their matching pairs by `result_id`.
///
/// Port of `attach_source_chunks` (`pairs.py:76-80`).
pub(crate) fn attach_source_chunks(pairs: &mut [ChunkSummaryPair], chunks: &[SearchItem]) {
    for chunk in chunks {
        let chunk_id = result_id(chunk);
        if let Some(index) = find_chunk_summary_pair(pairs, chunk_id.as_deref(), None) {
            set_pair_chunk(&mut pairs[index], chunk);
        }
    }
}

/// Map of `chunk_id -> summary_text` over the pairs that carry both.
///
/// Port of `summary_text_by_chunk_id` (`pairs.py:83-88`).
pub(crate) fn summary_text_by_chunk_id(pairs: &[ChunkSummaryPair]) -> HashMap<String, String> {
    let mut summaries = HashMap::new();
    for pair in pairs {
        if let (Some(chunk_id), Some(text)) = (pair.chunk_id.as_ref(), pair.summary_text.as_ref())
            && !chunk_id.is_empty()
            && !text.is_empty()
        {
            summaries.insert(chunk_id.clone(), text.clone());
        }
    }
    summaries
}

/// Deterministic `TextSummary` id for a `DocumentChunk` id.
///
/// Port of `summary_id_for_chunk` (`pairs.py:91-96`): parse `chunk_id` as a
/// UUID (`None` on failure), then `uuid5(chunk_uuid, "TextSummary")`. This
/// mirrors the scheme in `cognee-cognify`'s `TextSummary::new`
/// (`summarization/models.rs:73`) inline — no cross-crate dependency — so the
/// two never drift.
pub(crate) fn summary_id_for_chunk(chunk_id: &str) -> Option<String> {
    let chunk_uuid = Uuid::parse_str(chunk_id).ok()?;
    Some(Uuid::new_v5(&chunk_uuid, b"TextSummary").to_string())
}

/// Set a pair's chunk, adopting its id/text when present (keeping existing
/// values otherwise). Port of `set_pair_chunk` (`pairs.py:99-102`).
fn set_pair_chunk(pair: &mut ChunkSummaryPair, chunk: &SearchItem) {
    pair.chunk = Some(chunk.clone());
    if let Some(id) = result_id(chunk) {
        pair.chunk_id = Some(id);
    }
    if let Some(text) = chunk.payload.get("text").and_then(display_value) {
        pair.chunk_text = Some(text);
    }
}

/// Find the index of the pair matching `chunk_id` (exact) or, failing that, an
/// id-less pair with the same `chunk_text`. Port of `_find_chunk_summary_pair`
/// (`pairs.py:105-115`).
fn find_chunk_summary_pair(
    pairs: &[ChunkSummaryPair],
    chunk_id: Option<&str>,
    chunk_text: Option<&str>,
) -> Option<usize> {
    for (index, pair) in pairs.iter().enumerate() {
        if let Some(id) = chunk_id
            && pair.chunk_id.as_deref() == Some(id)
        {
            return Some(index);
        }
        if let Some(text) = chunk_text
            && pair.chunk_id.is_none()
            && pair.chunk_text.as_deref() == Some(text)
        {
            return Some(index);
        }
    }
    None
}

/// Fresh pair seeded with a chunk id/text. Port of `_new_chunk_summary_pair`
/// (`pairs.py:118-131`).
fn new_chunk_summary_pair(
    chunk_id: Option<String>,
    chunk_text: Option<String>,
) -> ChunkSummaryPair {
    ChunkSummaryPair {
        chunk_id,
        chunk_text,
        ..Default::default()
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

    fn item(payload: serde_json::Value) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    #[test]
    fn vector_hit_pairs_with_its_summary_by_id() {
        let id = Uuid::new_v4().to_string();
        let vector = vec![item(json!({"id": id, "text": "hello world"}))];
        let summaries = vec![item(json!({
            "id": "s1",
            "text": "a summary",
            "source_chunk_id": id,
        }))];
        let pairs = chunk_summary_pairs(&vector, &summaries, None, "OR");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].vector_rank, Some(0));
        assert_eq!(pairs[0].summary_rank, Some(0));
        assert_eq!(pairs[0].chunk_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn idless_vector_hits_merge_by_text() {
        let vector = vec![
            item(json!({"text": "shared text"})),
            item(json!({"text": "shared text"})),
        ];
        let pairs = chunk_summary_pairs(&vector, &[], None, "OR");
        assert_eq!(pairs.len(), 1, "text-merge collapses into one pair");
        assert_eq!(pairs[0].vector_rank, Some(0));
        assert!(pairs[0].chunk_id.is_none());
    }

    #[test]
    fn summary_without_source_chunk_id_is_dropped() {
        let summaries = vec![item(json!({"id": "s1", "text": "a summary"}))];
        let pairs = chunk_summary_pairs(&[], &summaries, None, "OR");
        assert!(pairs.is_empty());
    }

    #[test]
    fn summary_keyed_by_source_chunk_id() {
        let chunk_id = Uuid::new_v4().to_string();
        let summaries = vec![item(json!({
            "id": "summary-id",
            "text": "the summary",
            "source_chunk_id": chunk_id,
        }))];
        let pairs = chunk_summary_pairs(&[], &summaries, None, "OR");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].chunk_id.as_deref(), Some(chunk_id.as_str()));
        assert_eq!(pairs[0].summary_rank, Some(0));
        assert_eq!(pairs[0].summary_text.as_deref(), Some("the summary"));
        assert!(pairs[0].chunk.is_none());
    }

    #[test]
    fn summary_id_for_chunk_matches_uuid5_scheme() {
        let chunk_id = Uuid::new_v4();
        let expected = Uuid::new_v5(&chunk_id, b"TextSummary").to_string();
        assert_eq!(summary_id_for_chunk(&chunk_id.to_string()), Some(expected));
        // Non-UUID input -> None.
        assert_eq!(summary_id_for_chunk("not-a-uuid"), None);
    }

    #[test]
    fn summary_id_for_chunk_known_vector() {
        // Fixed chunk id -> stable uuid5(chunk, "TextSummary").
        let chunk_id = "12345678-1234-5678-1234-567812345678";
        let parsed = Uuid::parse_str(chunk_id).unwrap();
        let expected = Uuid::new_v5(&parsed, b"TextSummary").to_string();
        assert_eq!(summary_id_for_chunk(chunk_id), Some(expected));
    }

    #[test]
    fn source_chunk_ids_only_for_summary_only_pairs() {
        let chunk_id = Uuid::new_v4().to_string();
        let summaries = vec![item(json!({
            "id": "s",
            "text": "sum",
            "source_chunk_id": chunk_id,
        }))];
        let pairs = chunk_summary_pairs(&[], &summaries, None, "OR");
        let to_load = source_chunk_ids_to_load(&pairs);
        assert_eq!(to_load, vec![chunk_id]);
    }

    #[test]
    fn attach_source_chunks_fills_summary_only_pair() {
        let chunk_id = Uuid::new_v4().to_string();
        // Summary-only pair: has a source_chunk_id but no chunk attached yet.
        let summaries = vec![item(json!({
            "id": "summary-id",
            "text": "the summary",
            "source_chunk_id": chunk_id,
        }))];
        let mut pairs = chunk_summary_pairs(&[], &summaries, None, "OR");
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].chunk.is_none(), "no chunk before backfill");

        // Backfill the source chunk by matching id.
        attach_source_chunks(&mut pairs, &[item(json!({"id": chunk_id, "text": "body"}))]);
        assert!(pairs[0].chunk.is_some());
        assert_eq!(pairs[0].chunk_text.as_deref(), Some("body"));

        // A second call carrying a non-matching id is a no-op: the existing
        // chunk stays, and no new pair is appended.
        attach_source_chunks(
            &mut pairs,
            &[item(
                json!({"id": Uuid::new_v4().to_string(), "text": "other"}),
            )],
        );
        assert_eq!(pairs.len(), 1, "non-matching id adds no pair");
        assert_eq!(
            pairs[0].chunk_text.as_deref(),
            Some("body"),
            "existing chunk untouched by the non-matching backfill"
        );
    }

    #[test]
    fn summary_text_by_chunk_id_only_includes_complete_pairs() {
        let pairs = vec![
            // (a) both chunk_id and summary_text present -> included.
            ChunkSummaryPair {
                chunk_id: Some("x".to_string()),
                summary_text: Some("s".to_string()),
                ..Default::default()
            },
            // (b) chunk_id present but no summary_text -> excluded.
            ChunkSummaryPair {
                chunk_id: Some("y".to_string()),
                summary_text: None,
                ..Default::default()
            },
            // (c) empty chunk_id (falsy) even with summary_text -> excluded.
            ChunkSummaryPair {
                chunk_id: Some(String::new()),
                summary_text: Some("z".to_string()),
                ..Default::default()
            },
        ];
        let map = summary_text_by_chunk_id(&pairs);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("x").map(String::as_str), Some("s"));
        assert!(!map.contains_key("y"));
        assert!(!map.contains_key(""));
    }

    #[test]
    fn summary_lane_respects_node_filter() {
        let keep_chunk = Uuid::new_v4().to_string();
        let drop_chunk = Uuid::new_v4().to_string();
        let summaries = vec![
            item(json!({
                "id": "keep-summary",
                "text": "keep me",
                "source_chunk_id": keep_chunk,
                "belongs_to_set": ["keep"],
            })),
            item(json!({
                "id": "drop-summary",
                "text": "drop me",
                "source_chunk_id": drop_chunk,
                "belongs_to_set": ["drop"],
            })),
        ];
        let keep = vec!["keep".to_string()];
        let pairs = chunk_summary_pairs(&[], &summaries, Some(&keep), "OR");
        assert_eq!(pairs.len(), 1, "only the 'keep' summary passes the filter");
        assert_eq!(pairs[0].chunk_id.as_deref(), Some(keep_chunk.as_str()));

        // Operator threading: a summary in ["keep"] with a request of
        // ["keep","extra"] passes OR (non-empty intersection) but fails AND
        // (request not a subset). If the operator were hard-coded to OR, the
        // AND call would still produce a pair.
        let single = vec![item(json!({
            "id": "keep-summary",
            "text": "keep me",
            "source_chunk_id": keep_chunk,
            "belongs_to_set": ["keep"],
        }))];
        let request = vec!["keep".to_string(), "extra".to_string()];
        let or_pairs = chunk_summary_pairs(&[], &single, Some(&request), "OR");
        assert_eq!(or_pairs.len(), 1, "OR matches on the shared 'keep' entry");
        let and_pairs = chunk_summary_pairs(&[], &single, Some(&request), "AND");
        assert!(
            and_pairs.is_empty(),
            "AND requires the full request to be a subset -> no pair"
        );
    }

    #[test]
    fn duplicate_hit_in_lane_keeps_first_rank() {
        // Vector lane: the same id appears twice; first-rank-wins collapses to
        // one pair whose vector_rank stays at the first occurrence.
        let id = Uuid::new_v4().to_string();
        let vector = vec![
            item(json!({"id": id, "text": "hello"})),
            item(json!({"id": id, "text": "hello"})),
        ];
        let pairs = chunk_summary_pairs(&vector, &[], None, "OR");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].vector_rank, Some(0));

        // Summary lane: two summaries share one source_chunk_id; summary_rank and
        // the recorded summary_id/text stay at the first hit.
        let chunk_id = Uuid::new_v4().to_string();
        let summaries = vec![
            item(json!({
                "id": "first-summary",
                "text": "first",
                "source_chunk_id": chunk_id,
            })),
            item(json!({
                "id": "second-summary",
                "text": "second",
                "source_chunk_id": chunk_id,
            })),
        ];
        let pairs = chunk_summary_pairs(&[], &summaries, None, "OR");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].summary_rank, Some(0));
        assert_eq!(pairs[0].summary_id.as_deref(), Some("first-summary"));
        assert_eq!(pairs[0].summary_text.as_deref(), Some("first"));
    }
}
