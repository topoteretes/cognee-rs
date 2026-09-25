//! Two-lane orchestration for the hybrid chunk retriever.
//!
//! Port of `cognee/modules/retrieval/hybrid/chunks.py`. Fans a query out to the
//! vector `DocumentChunk_text` / `TextSummary_text` lanes, merges them into
//! chunk↔summary pairs, ranks with RRF, backfills source chunks and summary
//! text, and returns the top chunks plus a `chunk_id -> summary_text` map.
//!
//! This task **receives** an already-computed `query_vector` (the future
//! retriever task, P1-09, embeds the query once and passes it down), so there
//! is no embedding call here; the Phase-2 truth-subspace parameters are
//! intentionally absent (locked deferral).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_graph::NodeTruthState;
use cognee_vector::VectorDB;
use uuid::Uuid;

use crate::retrievers::context_items::search_results_to_context;
use crate::retrievers::hybrid::pairs::{
    ChunkSummaryPair, attach_source_chunks, chunk_summary_pairs, source_chunk_ids_to_load,
    summary_id_for_chunk, summary_text_by_chunk_id,
};
use crate::retrievers::hybrid::ranking::rank_chunk_summary_pairs;
use crate::retrievers::hybrid::results::{display_value, payload_matches_node_filter, result_id};
use crate::types::{SearchError, SearchItem};

const DOCUMENT_CHUNK_TYPE: &str = "DocumentChunk";
const TEXT_SUMMARY_TYPE: &str = "TextSummary";
const TEXT_FIELD: &str = "text";

/// Result of a hybrid chunk retrieval: the ranked chunks and their paired
/// summaries keyed by chunk id.
pub(crate) struct HybridChunksResult {
    pub chunks: Vec<SearchItem>,
    pub chunk_summaries: HashMap<String, String>,
}

/// Candidate limit for the `TextSummary_text` lane.
///
/// Port of `summary_candidate_limit` (`chunks.py:106-109`). Python's `None`
/// branch clamps to `max(0, chunks_top_k)`; Rust's `usize` makes that a no-op,
/// so this is just `text_summaries_top_k.unwrap_or(chunks_top_k)`.
pub(crate) fn summary_candidate_limit(
    chunks_top_k: usize,
    text_summaries_top_k: Option<usize>,
) -> usize {
    text_summaries_top_k.unwrap_or(chunks_top_k)
}

/// Similarity-search a vector collection, guarding a missing collection and
/// applying the node-set filter through the vector engine (finding F9).
///
/// Port of `search_collection` (`chunks.py:143-175`). Python threads `node_name`
/// into `vector_engine.search(...)`, which filters **server-side then limits**
/// (`filter-then-limit`): the engine only counts in-set rows toward `limit`, so
/// every returned row is in-set and no valid in-set row is ever crowded out by
/// higher-similarity out-of-set rows. Rust now mirrors this by threading
/// `node_name`/`operator` into [`VectorDB::search_similar_filtered`], so the
/// filter is applied inside the adapter rather than after an over-fetch here.
///
/// A missing collection is an empty channel, never an error: Python catches
/// `CollectionNotFoundError` and returns `[]` for every lane (`chunks.py:140-142`).
///
/// # Recall parity
///
/// The in-memory adapters (`BruteForceVectorDB`, `MockVectorDB`) and the
/// pgvector adapter override `search_similar_filtered` with an **exact**
/// server-side filter-then-limit at any collection size. The default trait
/// fallback (used by the LanceDB adapter, whose `metadata` is an opaque JSON
/// string it cannot predicate on) keeps a bounded limit-then-filter that is
/// exact for collections at or below `NODE_FILTER_RECALL_FETCH_CAP` and only
/// approximate above it. See [`VectorDB::search_similar_filtered`] for the full
/// per-backend recall discussion.
///
/// When `node_name` is `None`/empty the call is byte-identical to the previous
/// plain `search_similar` + `search_results_to_context` path.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn search_collection(
    vector_db: &Arc<dyn VectorDB>,
    data_type: &str,
    field: &str,
    query_vector: &[f32],
    limit: usize,
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Result<Vec<SearchItem>, SearchError> {
    if limit == 0 {
        return Ok(vec![]);
    }

    if !vector_db.has_collection(data_type, field).await? {
        tracing::debug!("{data_type}_{field} collection not found; using empty channel");
        return Ok(vec![]);
    }

    match node_name {
        Some(names) if !names.is_empty() => {
            // Server-side filter-then-limit: the adapter drops out-of-set rows
            // before applying `limit`, so in-set rows are never crowded out.
            let results = vector_db
                .search_similar_filtered(
                    data_type,
                    field,
                    query_vector,
                    limit,
                    Some(names),
                    node_name_filter_operator,
                )
                .await?;
            search_results_to_context(results)
        }
        _ => {
            // No filter — byte-identical to the pre-F9 plain-search path.
            let results = vector_db
                .search_similar(data_type, field, query_vector, limit)
                .await?;
            search_results_to_context(results)
        }
    }
}

/// Fetch `DocumentChunk_text` rows by id for summary-only pairs, dropping any
/// that fail the node filter.
///
/// Port of `load_source_chunks_for_summaries` (`chunks.py:178-209`) using the
/// batched retrieve-by-ids call.
async fn load_source_chunks_for_summaries(
    vector_db: &Arc<dyn VectorDB>,
    chunk_ids: &[String],
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Result<Vec<SearchItem>, SearchError> {
    let uuids: Vec<Uuid> = chunk_ids
        .iter()
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();
    let results = vector_db
        .retrieve(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, &uuids)
        .await?;
    let items = search_results_to_context(results)?;

    let found: HashSet<String> = items.iter().filter_map(result_id).collect();
    let missing: Vec<&String> = chunk_ids.iter().filter(|id| !found.contains(*id)).collect();
    if !missing.is_empty() {
        tracing::warn!(
            ?missing,
            "TextSummary_text hit referenced missing DocumentChunk_text row(s)"
        );
    }

    let mut source_chunks = Vec::new();
    let mut filtered_ids = Vec::new();
    for item in items {
        if payload_matches_node_filter(&item.payload, node_name, node_name_filter_operator) {
            source_chunks.push(item);
        } else if let Some(id) = result_id(&item) {
            filtered_ids.push(id);
        }
    }
    if !filtered_ids.is_empty() {
        tracing::warn!(
            ?filtered_ids,
            "TextSummary_text source chunk failed node filter"
        );
    }
    Ok(source_chunks)
}

/// Backfill `summary_text` onto the ranked pairs by batch-fetching
/// `TextSummary_text` rows.
///
/// Port of `load_summary_text_for_ranked_pairs` (`chunks.py:212-280`). A missing
/// `TextSummary_text` collection is tolerated (`has_collection` guard, mirroring
/// Python's `CollectionNotFoundError` early-return).
async fn load_summary_text_for_ranked_pairs(
    vector_db: &Arc<dyn VectorDB>,
    ranked_pairs: &mut [ChunkSummaryPair],
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> Result<(), SearchError> {
    let mut summary_uuids: Vec<Uuid> = Vec::new();
    for pair in ranked_pairs.iter_mut() {
        if pair.summary_text.is_some() {
            continue;
        }
        let Some(chunk_id) = pair.chunk_id.clone().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(summary_id) = pair
            .summary_id
            .clone()
            .or_else(|| summary_id_for_chunk(&chunk_id))
        else {
            tracing::debug!(%chunk_id, "Cannot fetch paired TextSummary for non-UUID chunk id");
            continue;
        };
        if let Ok(uuid) = Uuid::parse_str(&summary_id) {
            summary_uuids.push(uuid);
        }
        pair.summary_id = Some(summary_id);
    }

    if summary_uuids.is_empty() {
        return Ok(());
    }

    if !vector_db
        .has_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD)
        .await?
    {
        tracing::warn!("TextSummary_text collection missing while loading chunk summaries");
        return Ok(());
    }

    let results = vector_db
        .retrieve(TEXT_SUMMARY_TYPE, TEXT_FIELD, &summary_uuids)
        .await?;
    let items = search_results_to_context(results)?;
    let mut summaries_by_id: HashMap<String, SearchItem> = HashMap::new();
    for item in items {
        if let Some(id) = result_id(&item) {
            summaries_by_id.insert(id, item);
        }
    }

    for pair in ranked_pairs.iter_mut() {
        let (Some(chunk_id), Some(summary_id)) = (pair.chunk_id.clone(), pair.summary_id.clone())
        else {
            continue;
        };
        if chunk_id.is_empty() || summary_id.is_empty() {
            continue;
        }

        let Some(summary) = summaries_by_id.get(&summary_id) else {
            tracing::warn!(%chunk_id, "DocumentChunk_text row has no paired TextSummary_text row");
            continue;
        };

        let Some(summary_text) = summary.payload.get("text").and_then(display_value) else {
            tracing::warn!(%chunk_id, %summary_id, "Paired TextSummary_text row has no text");
            continue;
        };

        if !payload_matches_node_filter(&summary.payload, node_name, node_name_filter_operator) {
            tracing::warn!(%chunk_id, %summary_id, "Paired TextSummary_text row failed node filter");
            continue;
        }

        pair.summary_text = Some(summary_text);
    }

    Ok(())
}

/// Retrieve, merge, and rank hybrid chunks for `query`.
///
/// Port of `retrieve_hybrid_chunks` (`chunks.py:23-98`). Runs the two vector
/// lanes concurrently (a missing collection is an empty channel, as in
/// Python), builds pairs, backfills source chunks for summary-only pairs,
/// ranks, backfills summary text on the **ranked** pairs, and returns the top
/// chunks plus their summaries.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn retrieve_hybrid_chunks(
    vector_db: &Arc<dyn VectorDB>,
    chunks_top_k: usize,
    text_summaries_top_k: Option<usize>,
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
    use_importance_weight: bool,
    query_vector: &[f32],
    use_truth_weight: bool,
    q_coords: Option<&[f64]>,
    truth_state_by_id: Option<&HashMap<String, NodeTruthState>>,
    current_truth_epoch: Option<i64>,
) -> Result<HybridChunksResult, SearchError> {
    let candidate_limit = chunks_top_k.saturating_mul(2);
    let summary_limit = summary_candidate_limit(chunks_top_k, text_summaries_top_k);

    let vector_future = search_collection(
        vector_db,
        DOCUMENT_CHUNK_TYPE,
        TEXT_FIELD,
        query_vector,
        candidate_limit,
        node_name,
        node_name_filter_operator,
    );
    let summary_future = search_collection(
        vector_db,
        TEXT_SUMMARY_TYPE,
        TEXT_FIELD,
        query_vector,
        summary_limit,
        node_name,
        node_name_filter_operator,
    );

    let (vector_result, summary_result) = tokio::join!(vector_future, summary_future);
    let vector_chunks = vector_result?;
    let summary_hits = summary_result?;

    let mut pairs = chunk_summary_pairs(
        &vector_chunks,
        &summary_hits,
        node_name,
        node_name_filter_operator,
    );

    let missing_source_chunk_ids = source_chunk_ids_to_load(&pairs);
    if !missing_source_chunk_ids.is_empty() {
        let source_chunks = load_source_chunks_for_summaries(
            vector_db,
            &missing_source_chunk_ids,
            node_name,
            node_name_filter_operator,
        )
        .await?;
        attach_source_chunks(&mut pairs, &source_chunks);
    }

    let mut ranked_pairs = rank_chunk_summary_pairs(
        pairs,
        chunks_top_k,
        use_importance_weight,
        use_truth_weight,
        q_coords,
        truth_state_by_id,
        current_truth_epoch,
    );
    if summary_limit > 0 {
        load_summary_text_for_ranked_pairs(
            vector_db,
            &mut ranked_pairs,
            node_name,
            node_name_filter_operator,
        )
        .await?;
    }

    let chunk_summaries = summary_text_by_chunk_id(&ranked_pairs);
    let chunks = ranked_pairs
        .into_iter()
        .filter_map(|pair| pair.chunk)
        .collect();

    Ok(HybridChunksResult {
        chunks,
        chunk_summaries,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn dyn_vector(db: MockVectorDB) -> Arc<dyn VectorDB> {
        Arc::new(db)
    }

    /// Index a DocumentChunk_text vector point.
    async fn index_chunk(
        db: &MockVectorDB,
        id: Uuid,
        text: &str,
        belongs_to_set: Option<Vec<&str>>,
        vector: Vec<f32>,
    ) {
        let mut point = VectorPoint::new(id, vector)
            .with_metadata("id", json!(id.to_string()))
            .with_metadata("text", json!(text));
        if let Some(sets) = belongs_to_set {
            point = point.with_metadata("belongs_to_set", json!(sets));
        }
        db.index_points(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, &[point])
            .await
            .unwrap();
    }

    /// Index a TextSummary_text vector point.
    async fn index_summary(
        db: &MockVectorDB,
        id: Uuid,
        text: &str,
        source_chunk_id: Uuid,
        vector: Vec<f32>,
    ) {
        let point = VectorPoint::new(id, vector)
            .with_metadata("id", json!(id.to_string()))
            .with_metadata("text", json!(text))
            .with_metadata("source_chunk_id", json!(source_chunk_id.to_string()));
        db.index_points(TEXT_SUMMARY_TYPE, TEXT_FIELD, &[point])
            .await
            .unwrap();
    }

    /// Index a TextSummary_text vector point carrying a `belongs_to_set` tag.
    async fn index_summary_with_set(
        db: &MockVectorDB,
        id: Uuid,
        text: &str,
        source_chunk_id: Uuid,
        belongs_to_set: Vec<&str>,
        vector: Vec<f32>,
    ) {
        let point = VectorPoint::new(id, vector)
            .with_metadata("id", json!(id.to_string()))
            .with_metadata("text", json!(text))
            .with_metadata("source_chunk_id", json!(source_chunk_id.to_string()))
            .with_metadata("belongs_to_set", json!(belongs_to_set));
        db.index_points(TEXT_SUMMARY_TYPE, TEXT_FIELD, &[point])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn missing_document_chunk_collection_is_an_empty_channel() {
        let vector_db = dyn_vector(MockVectorDB::new());

        let result = retrieve_hybrid_chunks(
            &vector_db,
            3,
            None,
            None,
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Python catches `CollectionNotFoundError` on every lane (`chunks.py:140-142`).
        assert!(result.chunks.is_empty());
        assert!(result.chunk_summaries.is_empty());
    }

    #[tokio::test]
    async fn missing_text_summary_collection_is_tolerated() {
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        let chunk_id = Uuid::new_v4();
        index_chunk(&db, chunk_id, "rust ownership model", None, vec![1.0, 0.0]).await;
        let vector_db = dyn_vector(db);

        let result = retrieve_hybrid_chunks(
            &vector_db,
            3,
            None,
            None,
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 1);
        assert!(result.chunk_summaries.is_empty());
    }

    #[tokio::test]
    async fn node_name_filter_drops_non_matching_vector_hits() {
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        let keep_a = Uuid::new_v4();
        let keep_b = Uuid::new_v4();
        let drop_c = Uuid::new_v4();
        index_chunk(&db, keep_a, "text a", Some(vec!["keep"]), vec![1.0, 0.0]).await;
        index_chunk(&db, keep_b, "text b", Some(vec!["keep"]), vec![1.0, 0.0]).await;
        index_chunk(&db, drop_c, "text c", Some(vec!["drop"]), vec![1.0, 0.0]).await;
        let vector_db = dyn_vector(db);

        let node_name = vec!["keep".to_string()];
        let result = retrieve_hybrid_chunks(
            &vector_db,
            3,
            None,
            Some(&node_name),
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 2);
        for chunk in &result.chunks {
            let sets = chunk.payload.get("belongs_to_set").unwrap();
            assert_eq!(sets, &json!(["keep"]));
        }
    }

    #[tokio::test]
    async fn node_filter_keeps_in_set_chunks_outranked_by_many_out_of_set() {
        // Regression for finding F9: 300 out-of-set chunks outrank the 2 in-set
        // chunks by pure similarity. With chunks_top_k = 2 the candidate limit
        // is 4, so the old limit-then-filter (fetch a small window, then drop
        // out-of-set rows) would exhaust its window entirely on out-of-set rows
        // and return zero in-set chunks. MockVectorDB now filters server-side
        // (filter-then-limit) via `search_similar_filtered`, so both in-set
        // chunks survive regardless of how many out-of-set rows outrank them —
        // deliberately far more than any client-side over-fetch cap would cover.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        // 300 out-of-set chunks, maximally aligned with the query [1, 0].
        for i in 0..300 {
            index_chunk(
                &db,
                Uuid::new_v4(),
                &format!("out of set {i}"),
                Some(vec!["drop"]),
                vec![1.0, 0.0],
            )
            .await;
        }
        // 2 in-set chunks, slightly less aligned so they rank strictly below
        // every out-of-set chunk.
        let keep_a = Uuid::new_v4();
        let keep_b = Uuid::new_v4();
        index_chunk(&db, keep_a, "in set a", Some(vec!["keep"]), vec![0.8, 0.6]).await;
        index_chunk(&db, keep_b, "in set b", Some(vec!["keep"]), vec![0.8, 0.6]).await;

        let vector_db = dyn_vector(db);

        let node_name = vec!["keep".to_string()];
        let result = retrieve_hybrid_chunks(
            &vector_db,
            2,
            None,
            Some(&node_name),
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 2, "both in-set chunks must survive");
        for chunk in &result.chunks {
            let sets = chunk.payload.get("belongs_to_set").unwrap();
            assert_eq!(sets, &json!(["keep"]));
        }
    }

    #[tokio::test]
    async fn summary_only_hit_triggers_source_chunk_backfill() {
        // chunks_top_k = 2 -> candidate_limit = 4. Five filler chunks aligned
        // with the query push the orthogonal source chunk C out of the vector
        // lane's top-4, so C only reaches the pipeline via the summary hit's
        // source_chunk_id and must be backfilled by retrieve-by-id.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        db.create_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        for i in 0..5 {
            index_chunk(
                &db,
                Uuid::new_v4(),
                &format!("filler {i}"),
                None,
                vec![1.0, 0.0],
            )
            .await;
        }
        // Source chunk C: orthogonal to the query so it is not a direct hit.
        let chunk_c = Uuid::new_v4();
        index_chunk(
            &db,
            chunk_c,
            "backfilled source chunk",
            None,
            vec![0.0, 1.0],
        )
        .await;
        // Summary references C and is aligned with the query.
        let summary_id = Uuid::new_v5(&chunk_c, b"TextSummary");
        index_summary(
            &db,
            summary_id,
            "the paired summary",
            chunk_c,
            vec![1.0, 0.0],
        )
        .await;

        let vector_db = dyn_vector(db);

        let result = retrieve_hybrid_chunks(
            &vector_db,
            2,
            None,
            None,
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // C was backfilled and survived ranking; its summary is attached.
        let chunk_c_str = chunk_c.to_string();
        assert!(
            result
                .chunks
                .iter()
                .any(|c| c.payload.get("id") == Some(&json!(chunk_c_str))),
            "backfilled source chunk should appear in ranked chunks"
        );
        assert_eq!(
            result.chunk_summaries.get(&chunk_c_str).map(String::as_str),
            Some("the paired summary")
        );
    }

    #[tokio::test]
    async fn chunk_summaries_reflect_ranked_pairs_only() {
        // chunks_top_k = 1 -> limit = 1: only the single top-ranked pair's
        // summary is reported, not every candidate summary.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        db.create_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        // A strong direct chunk hit that will win the single slot.
        let winner = Uuid::new_v4();
        index_chunk(&db, winner, "winning chunk", None, vec![1.0, 0.0]).await;
        let winner_summary = Uuid::new_v5(&winner, b"TextSummary");
        index_summary(
            &db,
            winner_summary,
            "winner summary",
            winner,
            vec![1.0, 0.0],
        )
        .await;

        // A weaker summary-only candidate that should be truncated away.
        let loser = Uuid::new_v4();
        index_chunk(&db, loser, "loser chunk", None, vec![0.0, 1.0]).await;
        let loser_summary = Uuid::new_v5(&loser, b"TextSummary");
        index_summary(&db, loser_summary, "loser summary", loser, vec![0.2, 1.0]).await;

        let vector_db = dyn_vector(db);

        let result = retrieve_hybrid_chunks(
            &vector_db,
            1,
            None,
            None,
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunk_summaries.len(), 1);
        assert_eq!(
            result
                .chunk_summaries
                .get(&winner.to_string())
                .map(String::as_str),
            Some("winner summary")
        );
        assert!(!result.chunk_summaries.contains_key(&loser.to_string()));
    }

    #[test]
    fn summary_candidate_limit_honors_explicit_and_default() {
        // `None` defaults to `chunks_top_k`; an explicit value (including 0) is
        // used verbatim. Port of `summary_candidate_limit` (chunks.py:106-109).
        assert_eq!(summary_candidate_limit(3, None), 3);
        assert_eq!(summary_candidate_limit(3, Some(0)), 0);
        assert_eq!(summary_candidate_limit(3, Some(5)), 5);
        assert_eq!(summary_candidate_limit(3, Some(1)), 1);
    }

    #[tokio::test]
    async fn text_summaries_top_k_zero_disables_summary_lane() {
        // An explicit text_summaries_top_k = 0 sets summary_limit = 0, which
        // both short-circuits the summary vector lane AND skips the
        // `load_summary_text_for_ranked_pairs` backfill (the `summary_limit > 0`
        // gate). Even with a perfectly valid, query-aligned TextSummary indexed,
        // no summaries are reported.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        db.create_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        let chunk_id = Uuid::new_v4();
        index_chunk(&db, chunk_id, "rust ownership", None, vec![1.0, 0.0]).await;
        // A valid, aligned summary keyed to the chunk — would be backfilled if
        // the summary lane were enabled.
        let summary_id = Uuid::new_v5(&chunk_id, b"TextSummary");
        index_summary(&db, summary_id, "the summary", chunk_id, vec![1.0, 0.0]).await;

        let vector_db = dyn_vector(db);

        let result = retrieve_hybrid_chunks(
            &vector_db,
            3,
            Some(0),
            None,
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // The chunk still comes through the DocumentChunk lane...
        assert_eq!(result.chunks.len(), 1);
        // ...but the summary lane is fully disabled.
        assert!(result.chunk_summaries.is_empty());
    }

    #[tokio::test]
    async fn summary_failing_node_filter_is_not_backfilled() {
        // The winning in-set chunk has a deterministically-paired TextSummary
        // tagged with a DIFFERENT set. The summary lane filters it out, so the
        // ranked pair reaches `load_summary_text_for_ranked_pairs`, which
        // fetches the summary by its deterministic id — and must drop it on the
        // node-filter check (chunks.py: `payload_matches_node_filter`) rather
        // than backfilling its text.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        db.create_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        let keep_chunk = Uuid::new_v4();
        index_chunk(
            &db,
            keep_chunk,
            "kept chunk",
            Some(vec!["keep"]),
            vec![1.0, 0.0],
        )
        .await;
        // Deterministic paired summary id, tagged "drop" so it fails ["keep"].
        let summary_id = Uuid::new_v5(&keep_chunk, b"TextSummary");
        index_summary_with_set(
            &db,
            summary_id,
            "the drop-tagged summary",
            keep_chunk,
            vec!["drop"],
            vec![1.0, 0.0],
        )
        .await;

        let vector_db = dyn_vector(db);

        let node_name = vec!["keep".to_string()];
        let result = retrieve_hybrid_chunks(
            &vector_db,
            3,
            None,
            Some(&node_name),
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // The in-set chunk survives...
        let keep_str = keep_chunk.to_string();
        assert!(
            result
                .chunks
                .iter()
                .any(|c| c.payload.get("id") == Some(&json!(keep_str))),
            "in-set chunk should survive ranking"
        );
        // ...but its out-of-set summary is never backfilled.
        assert!(
            !result.chunk_summaries.contains_key(&keep_str),
            "summary failing the node filter must not be backfilled"
        );
        assert!(result.chunk_summaries.is_empty());
    }

    #[tokio::test]
    async fn backfilled_source_chunk_failing_node_filter_is_dropped() {
        // A summary-only hit references source chunk C. C is out-of-set
        // (["drop"]) while the request is ["keep"], so the summary reaches the
        // pipeline (its own tag passes) but the retrieve-by-id backfill of C in
        // `load_source_chunks_for_summaries` must drop C on the node filter. A
        // pair whose chunk never lands is skipped in ranking, so C appears in
        // neither the chunks nor the summaries.
        let db = MockVectorDB::new();
        db.create_collection(DOCUMENT_CHUNK_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();
        db.create_collection(TEXT_SUMMARY_TYPE, TEXT_FIELD, 2)
            .await
            .unwrap();

        // Source chunk C: out-of-set and orthogonal to the query, so it only
        // reaches the pipeline via the summary's source_chunk_id backfill.
        let chunk_c = Uuid::new_v4();
        index_chunk(
            &db,
            chunk_c,
            "drop-tagged source chunk",
            Some(vec!["drop"]),
            vec![0.0, 1.0],
        )
        .await;
        // Summary S references C, is in-set (["keep"]) and query-aligned, so it
        // survives the summary lane's node filter and triggers the backfill.
        let summary_id = Uuid::new_v5(&chunk_c, b"TextSummary");
        index_summary_with_set(
            &db,
            summary_id,
            "the summary",
            chunk_c,
            vec!["keep"],
            vec![1.0, 0.0],
        )
        .await;

        let vector_db = dyn_vector(db);

        let node_name = vec!["keep".to_string()];
        let result = retrieve_hybrid_chunks(
            &vector_db,
            2,
            None,
            Some(&node_name),
            "OR",
            false,
            &[1.0, 0.0],
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let chunk_c_str = chunk_c.to_string();
        assert!(
            !result
                .chunks
                .iter()
                .any(|c| c.payload.get("id") == Some(&json!(chunk_c_str))),
            "backfilled source chunk failing the node filter must not appear in chunks"
        );
        assert!(
            !result.chunk_summaries.contains_key(&chunk_c_str),
            "its summary must not survive either (the pair has no chunk)"
        );
    }
}
