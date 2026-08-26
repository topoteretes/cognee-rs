#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
// Each integration binary (brute-force, pgvector, …) pulls in this shared
// module but exercises a different subset of the contract helpers, so the
// unused ones would otherwise trip dead-code warnings per test crate.
#![allow(dead_code)]
//! Shared VectorDB contract tests.
//!
//! Each function exercises one aspect of the [`VectorDB`] trait and can be
//! called with *any* backend (Qdrant, PgVector, Mock, …). Backend-specific
//! integration tests just need to construct their adapter and call these
//! helpers.

use cognee_vector::{VectorDB, VectorDBError, VectorPoint};
use serde_json::json;
use uuid::Uuid;

// -- collection lifecycle ---------------------------------------------------

pub async fn test_create_and_has_collection(db: &dyn VectorDB) {
    db.create_collection("DocChunk", "text", 3).await.unwrap();
    assert!(db.has_collection("DocChunk", "text").await.unwrap());
    assert!(!db.has_collection("DocChunk", "other").await.unwrap());
}

pub async fn test_create_duplicate_errors(db: &dyn VectorDB) {
    db.create_collection("Entity", "name", 3).await.unwrap();
    let err = db.create_collection("Entity", "name", 3).await;
    assert!(
        matches!(err, Err(VectorDBError::CollectionExists(_))),
        "duplicate create should return CollectionExists, got {err:?}"
    );
}

pub async fn test_delete_collection(db: &dyn VectorDB) {
    db.create_collection("Del", "field", 2).await.unwrap();
    assert!(db.has_collection("Del", "field").await.unwrap());

    db.delete_collection("Del", "field").await.unwrap();
    assert!(!db.has_collection("Del", "field").await.unwrap());
}

pub async fn test_list_collections(db: &dyn VectorDB) {
    db.create_collection("Alpha", "text", 3).await.unwrap();
    db.create_collection("Beta", "name", 3).await.unwrap();

    let mut cols = db.list_collections().await.unwrap();
    // Filter to only the ones we created (shared DB may have others).
    cols.retain(|(dt, _)| dt == "Alpha" || dt == "Beta");
    cols.sort();

    assert_eq!(cols.len(), 2);
    assert!(cols.contains(&("Alpha".into(), "text".into())));
    assert!(cols.contains(&("Beta".into(), "name".into())));
}

// -- indexing & size --------------------------------------------------------

pub async fn test_index_and_collection_size(db: &dyn VectorDB) {
    db.create_collection("Size", "f", 2).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]),
    ];
    db.index_points("Size", "f", &points).await.unwrap();

    assert_eq!(db.collection_size("Size", "f").await.unwrap(), 2);
}

pub async fn test_empty_points_index(db: &dyn VectorDB) {
    db.create_collection("Empty", "f", 2).await.unwrap();
    let empty: Vec<VectorPoint> = vec![];
    db.index_points("Empty", "f", &empty).await.unwrap();
    assert_eq!(db.collection_size("Empty", "f").await.unwrap(), 0);
}

pub async fn test_dimension_validation(db: &dyn VectorDB) {
    db.create_collection("Dim", "f", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]), // wrong dim
    ];

    let err = db.index_points("Dim", "f", &points).await;
    assert!(
        matches!(err, Err(VectorDBError::DimensionMismatch { .. })),
        "mismatched dimensions should error, got {err:?}"
    );
}

pub async fn test_upsert_overwrites(db: &dyn VectorDB) {
    db.create_collection("Upsert", "f", 2).await.unwrap();

    let id = Uuid::new_v4();
    let original = vec![VectorPoint::new(id, vec![1.0, 0.0]).with_metadata("v", json!(1))];
    db.index_points("Upsert", "f", &original).await.unwrap();
    assert_eq!(db.collection_size("Upsert", "f").await.unwrap(), 1);

    // Re-index same ID with different vector/metadata — should upsert, not
    // create a second row.
    let updated = vec![VectorPoint::new(id, vec![0.0, 1.0]).with_metadata("v", json!(2))];
    db.index_points("Upsert", "f", &updated).await.unwrap();
    assert_eq!(db.collection_size("Upsert", "f").await.unwrap(), 1);

    // Verify the updated metadata is returned.
    let results = db
        .search_similar("Upsert", "f", &[0.0, 1.0], 1)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, id);
    assert_eq!(results[0].metadata.get("v"), Some(&json!(2)));
}

// -- search -----------------------------------------------------------------

pub async fn test_index_and_search(db: &dyn VectorDB) {
    db.create_collection("Search", "name", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0])
            .with_metadata("name", json!("Cognee")),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0])
            .with_metadata("name", json!("Knowledge")),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 0.0, 1.0]).with_metadata("name", json!("Rust")),
    ];
    db.index_points("Search", "name", &points).await.unwrap();

    let results = db
        .search_similar("Search", "name", &[1.0, 0.0, 0.0], 2)
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    // First result is the exact-match vector — should have the highest score.
    assert!(
        results[0].score >= results[1].score,
        "results should be ordered by similarity desc"
    );
}

pub async fn test_search_returns_top_k(db: &dyn VectorDB) {
    db.create_collection("TopK", "f", 2).await.unwrap();

    let points: Vec<VectorPoint> = (0..10)
        .map(|i| {
            VectorPoint::new(
                Uuid::new_v4(),
                vec![i as f32 / 10.0, 1.0 - (i as f32 / 10.0)],
            )
        })
        .collect();
    db.index_points("TopK", "f", &points).await.unwrap();

    let results = db
        .search_similar("TopK", "f", &[0.5, 0.5], 3)
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
}

pub async fn test_metadata_preserved(db: &dyn VectorDB) {
    db.create_collection("Meta", "f", 2).await.unwrap();

    let id = Uuid::new_v4();
    let points = vec![
        VectorPoint::new(id, vec![1.0, 0.0])
            .with_metadata("type", json!("DocumentChunk"))
            .with_metadata("document_id", json!("doc-123"))
            .with_metadata("chunk_index", json!(42)),
    ];
    db.index_points("Meta", "f", &points).await.unwrap();

    let results = db
        .search_similar("Meta", "f", &[1.0, 0.0], 1)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].metadata.get("type"),
        Some(&json!("DocumentChunk"))
    );
    assert_eq!(
        results[0].metadata.get("document_id"),
        Some(&json!("doc-123"))
    );
    assert_eq!(results[0].metadata.get("chunk_index"), Some(&json!(42)));
}

pub async fn test_uuid_round_trip(db: &dyn VectorDB) {
    db.create_collection("UUID", "f", 2).await.unwrap();

    let stored_id = Uuid::parse_str("f7ab8d87-553f-4509-b595-463cedc998be").unwrap();
    let points = vec![VectorPoint::new(stored_id, vec![1.0, 0.0])];
    db.index_points("UUID", "f", &points).await.unwrap();

    let results = db
        .search_similar("UUID", "f", &[1.0, 0.0], 1)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].id, stored_id,
        "UUID round-trip must preserve all 128 bits"
    );
}

// -- filtered search (node-set filter-then-limit, finding F9) ----------------

/// The scenario the old client-side over-fetch cap could silently drop: many
/// out-of-set points, all maximally aligned with the query, outrank a few
/// slightly off-axis in-set points. A limit-then-filter over a small window
/// returns zero in-set rows; a correct server-side filter-then-limit keeps them.
pub async fn test_search_similar_filtered_filter_then_limit(db: &dyn VectorDB) {
    db.create_collection("Filt", "f", 2).await.unwrap();

    let mut points = Vec::new();
    for _ in 0..64 {
        points.push(
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0])
                .with_metadata("belongs_to_set", json!(["drop"])),
        );
    }
    let keep1 = Uuid::new_v4();
    let keep2 = Uuid::new_v4();
    points.push(
        VectorPoint::new(keep1, vec![0.8, 0.6]).with_metadata("belongs_to_set", json!(["keep"])),
    );
    points.push(
        VectorPoint::new(keep2, vec![0.8, 0.6]).with_metadata("belongs_to_set", json!(["keep"])),
    );
    db.index_points("Filt", "f", &points).await.unwrap();

    let names = vec!["keep".to_string()];
    let results = db
        .search_similar_filtered("Filt", "f", &[1.0, 0.0], 2, Some(&names), "OR")
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = results.iter().map(|r| r.id).collect();
    let want: std::collections::HashSet<Uuid> = [keep1, keep2].into_iter().collect();
    assert_eq!(
        got, want,
        "both in-set points must survive server-side filter-then-limit"
    );
}

/// Membership semantics must mirror `payload_matches_node_filter`: object
/// entries match by `name`; bare-string dataset-id entries do NOT match a
/// NodeSet-name request (only their literal string).
pub async fn test_search_similar_filtered_semantics(db: &dyn VectorDB) {
    db.create_collection("FiltSem", "f", 2).await.unwrap();
    let obj_alpha = Uuid::new_v4();
    let bare_dataset = Uuid::new_v4();
    let obj_beta = Uuid::new_v4();
    db.index_points(
        "FiltSem",
        "f",
        &[
            VectorPoint::new(obj_alpha, vec![1.0, 0.0]).with_metadata(
                "belongs_to_set",
                json!([{"id": "1", "name": "alpha", "type": "NodeSet"}]),
            ),
            VectorPoint::new(bare_dataset, vec![1.0, 0.0])
                .with_metadata("belongs_to_set", json!(["dataset-xyz"])),
            VectorPoint::new(obj_beta, vec![1.0, 0.0]).with_metadata(
                "belongs_to_set",
                json!([{"id": "2", "name": "beta", "type": "NodeSet"}]),
            ),
        ],
    )
    .await
    .unwrap();

    // Object-shape entry matches by its `name`.
    let r = db
        .search_similar_filtered(
            "FiltSem",
            "f",
            &[1.0, 0.0],
            10,
            Some(&["alpha".to_string()]),
            "OR",
        )
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
    assert_eq!(got, [obj_alpha].into_iter().collect());

    // Requesting NodeSet names pulls in ONLY the object entries, never the
    // bare-string dataset-id row.
    let r = db
        .search_similar_filtered(
            "FiltSem",
            "f",
            &[1.0, 0.0],
            10,
            Some(&["alpha".to_string(), "beta".to_string()]),
            "OR",
        )
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
    assert_eq!(
        got,
        [obj_alpha, obj_beta].into_iter().collect(),
        "bare dataset-id entry must not match NodeSet names"
    );

    // The bare-string entry matches its own literal.
    let r = db
        .search_similar_filtered(
            "FiltSem",
            "f",
            &[1.0, 0.0],
            10,
            Some(&["dataset-xyz".to_string()]),
            "OR",
        )
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
    assert_eq!(got, [bare_dataset].into_iter().collect());
}

/// `"AND"` requires the requested names to be a subset of the row's set;
/// anything else is `"OR"` (non-empty intersection).
pub async fn test_search_similar_filtered_and_vs_or(db: &dyn VectorDB) {
    db.create_collection("FiltAndOr", "f", 2).await.unwrap();
    let both = Uuid::new_v4();
    let only_a = Uuid::new_v4();
    db.index_points(
        "FiltAndOr",
        "f",
        &[
            VectorPoint::new(both, vec![1.0, 0.0])
                .with_metadata("belongs_to_set", json!(["a", "b"])),
            VectorPoint::new(only_a, vec![1.0, 0.0]).with_metadata("belongs_to_set", json!(["a"])),
        ],
    )
    .await
    .unwrap();

    let req = vec!["a".to_string(), "b".to_string()];
    // OR: both rows intersect {a, b}.
    let r = db
        .search_similar_filtered("FiltAndOr", "f", &[1.0, 0.0], 10, Some(&req), "OR")
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
    assert_eq!(got, [both, only_a].into_iter().collect());

    // AND: only the row containing BOTH a and b.
    let r = db
        .search_similar_filtered("FiltAndOr", "f", &[1.0, 0.0], 10, Some(&req), "AND")
        .await
        .unwrap();
    let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
    assert_eq!(got, [both].into_iter().collect());
}

/// A `None`/empty filter returns all rows — including those with no
/// `belongs_to_set` at all — identical to plain `search_similar`.
pub async fn test_search_similar_filtered_none_matches_all(db: &dyn VectorDB) {
    db.create_collection("FiltNone", "f", 2).await.unwrap();
    let tagged = Uuid::new_v4();
    let untagged = Uuid::new_v4();
    db.index_points(
        "FiltNone",
        "f",
        &[
            VectorPoint::new(tagged, vec![1.0, 0.0]).with_metadata("belongs_to_set", json!(["x"])),
            VectorPoint::new(untagged, vec![1.0, 0.0]),
        ],
    )
    .await
    .unwrap();

    let r = db
        .search_similar_filtered("FiltNone", "f", &[1.0, 0.0], 10, None, "OR")
        .await
        .unwrap();
    assert_eq!(
        r.len(),
        2,
        "None filter returns every row, including untagged ones"
    );

    // An empty name slice behaves as no filter too.
    let empty: Vec<String> = vec![];
    let r = db
        .search_similar_filtered("FiltNone", "f", &[1.0, 0.0], 10, Some(&empty), "AND")
        .await
        .unwrap();
    assert_eq!(r.len(), 2, "empty node_name behaves as no filter");
}

// -- deletion ---------------------------------------------------------------

pub async fn test_delete_points(db: &dyn VectorDB) {
    db.create_collection("DelPts", "f", 2).await.unwrap();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let points = vec![
        VectorPoint::new(id1, vec![1.0, 0.0]),
        VectorPoint::new(id2, vec![0.0, 1.0]),
    ];
    db.index_points("DelPts", "f", &points).await.unwrap();
    assert_eq!(db.collection_size("DelPts", "f").await.unwrap(), 2);

    db.delete_points("DelPts", "f", &[id1]).await.unwrap();
    assert_eq!(db.collection_size("DelPts", "f").await.unwrap(), 1);
}

// -- retrieve ---------------------------------------------------------------

pub async fn test_retrieve_round_trip(db: &dyn VectorDB) {
    db.create_collection("Retr", "f", 2).await.unwrap();
    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();
    let points = vec![
        VectorPoint::new(id1, vec![1.0, 0.0]).with_metadata("k", json!("v1")),
        VectorPoint::new(id2, vec![0.0, 1.0]).with_metadata("k", json!("v2")),
        VectorPoint::new(id3, vec![1.0, 1.0]).with_metadata("k", json!("v3")),
    ];
    db.index_points("Retr", "f", &points).await.unwrap();

    let unknown = Uuid::new_v4();
    // Subset + a duplicated id + a nonexistent id, all in one call.
    let results = db
        .retrieve("Retr", "f", &[id1, id1, id2, unknown])
        .await
        .unwrap();

    // Order-independent set equality against the requested-known ids.
    let got: std::collections::HashSet<Uuid> = results.iter().map(|r| r.id).collect();
    let want: std::collections::HashSet<Uuid> = [id1, id2].into_iter().collect();
    assert_eq!(
        got, want,
        "retrieve should return exactly the known requested ids"
    );
    for r in &results {
        assert_eq!(r.score, 0.0, "retrieve always sets score to 0.0");
    }
    let r1 = results.iter().find(|r| r.id == id1).unwrap();
    assert_eq!(r1.metadata.get("k"), Some(&json!("v1")));
}

pub async fn test_retrieve_missing_collection(db: &dyn VectorDB) {
    // Never created — must be Ok([]), not Err.
    let results = db
        .retrieve("NoSuchRetr", "nope", &[Uuid::new_v4()])
        .await
        .unwrap();
    assert!(results.is_empty(), "missing collection must retrieve to []");
}

pub async fn test_retrieve_empty_ids(db: &dyn VectorDB) {
    db.create_collection("RetrEmpty", "f", 2).await.unwrap();
    db.index_points(
        "RetrEmpty",
        "f",
        &[VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0])],
    )
    .await
    .unwrap();
    let results = db.retrieve("RetrEmpty", "f", &[]).await.unwrap();
    assert!(results.is_empty(), "empty ids must retrieve to []");
}

pub async fn test_retrieve_chunking(db: &dyn VectorDB) {
    db.create_collection("RetrChunk", "f", 2).await.unwrap();
    let ids: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();
    let points: Vec<VectorPoint> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| VectorPoint::new(*id, vec![i as f32, 1.0]))
        .collect();
    db.index_points("RetrChunk", "f", &points).await.unwrap();

    // 101 ids exceeds pgvector's BATCH_SIZE (100), exercising the chunk loop.
    let results = db.retrieve("RetrChunk", "f", &ids).await.unwrap();
    let got: std::collections::HashSet<Uuid> = results.iter().map(|r| r.id).collect();
    let want: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
    assert_eq!(got, want, "all 101 ids should round-trip across batches");
}

// -- upsert_raw_vectors ------------------------------------------------------

pub async fn test_upsert_raw_vectors_round_trip(db: &dyn VectorDB) {
    use std::collections::HashMap;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();

    // Create-on-absent: no prior `create_collection` call — the raw upsert must
    // self-create the collection sized from the first vector.
    let points = vec![
        VectorPoint::new(id1, vec![1.0, 0.0])
            .with_metadata("k", json!("a"))
            .with_metadata("n", json!(1)),
        VectorPoint::new(id2, vec![0.0, 1.0]).with_metadata("k", json!("b")),
    ];
    db.upsert_raw_vectors("RawUp", "vec", &points)
        .await
        .unwrap();
    assert!(
        db.has_collection("RawUp", "vec").await.unwrap(),
        "upsert_raw_vectors must self-create the collection"
    );

    let got = db.retrieve("RawUp", "vec", &[id1, id2]).await.unwrap();
    let by_id: HashMap<Uuid, _> = got.into_iter().map(|r| (r.id, r.metadata)).collect();
    assert_eq!(by_id.len(), 2, "both raw points must round-trip");
    assert_eq!(by_id[&id1].get("k"), Some(&json!("a")));
    assert_eq!(by_id[&id1].get("n"), Some(&json!(1)));
    assert_eq!(by_id[&id2].get("k"), Some(&json!("b")));

    // Full-metadata replace: re-upsert id1 with entirely different metadata (no
    // dataset-membership union — the old `n` field must be gone).
    let replace = vec![VectorPoint::new(id1, vec![0.5, 0.5]).with_metadata("k", json!("z"))];
    db.upsert_raw_vectors("RawUp", "vec", &replace)
        .await
        .unwrap();
    let got = db.retrieve("RawUp", "vec", &[id1]).await.unwrap();
    assert_eq!(got.len(), 1, "replace must not create a second row");
    assert_eq!(
        got[0].metadata.get("k"),
        Some(&json!("z")),
        "metadata must be fully replaced"
    );
    assert!(
        !got[0].metadata.contains_key("n"),
        "old metadata field must be dropped by full replace"
    );
}

pub async fn test_upsert_raw_vectors_empty_noop(db: &dyn VectorDB) {
    let empty: Vec<VectorPoint> = vec![];
    db.upsert_raw_vectors("RawEmpty", "vec", &empty)
        .await
        .unwrap();
    assert!(
        !db.has_collection("RawEmpty", "vec").await.unwrap(),
        "empty upsert_raw_vectors must be a no-op and not create a collection"
    );
}

// -- batch search -----------------------------------------------------------

pub async fn test_batch_search(db: &dyn VectorDB) {
    db.create_collection("Batch", "f", 3).await.unwrap();

    let points = vec![
        VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]),
        VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0]),
    ];
    db.index_points("Batch", "f", &points).await.unwrap();

    let queries = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
    let results = db
        .batch_search_similar("Batch", "f", &queries, 5)
        .await
        .unwrap();

    assert_eq!(results.len(), 2, "one result set per query");
    assert!(!results[0].is_empty());
    assert!(!results[1].is_empty());
    // Each query's results must map back to that query (ordinality routing) and be
    // ranked, so the nearest hit is that query's exactly-matching point.
    assert_eq!(
        results[0][0].id, points[0].id,
        "query 0 should rank its exact-match point first"
    );
    assert_eq!(
        results[1][0].id, points[1].id,
        "query 1 should rank its exact-match point first"
    );
}

// -- NUL bytes ---------------------------------------------------------------

/// A NUL byte in point metadata must never break persistence.
///
/// Cognify injects the full chunk and summary text into point metadata, so the
/// `::jsonb` cast in the pgvector upsert has the same exposure to PDF-extracted
/// `0x00` as the graph tables do. See `cognee_utils::sanitize`.
///
/// Backends differ in what they store — pgvector strips the NUL, the in-memory
/// and LanceDB adapters keep it — so this asserts the shared contract: the
/// upsert succeeds, the point comes back, and every non-NUL character survives.
pub async fn test_nul_bytes_in_metadata_are_persistable(db: &dyn VectorDB) {
    db.create_collection("NulChunk", "text", 3).await.unwrap();

    let dirty = "page 1\0page 2";
    let id = Uuid::new_v4();
    let point = VectorPoint::new(id, vec![1.0, 0.0, 0.0])
        .with_metadata("text", json!(dirty))
        .with_metadata("nested", json!({"inner": ["a\0b"]}));

    db.index_points("NulChunk", "text", &[point]).await.unwrap();

    let fetched = db.retrieve("NulChunk", "text", &[id]).await.unwrap();
    assert_eq!(fetched.len(), 1, "the point must be retrievable");

    let strip = |s: &str| s.replace('\0', "");
    let text = fetched[0]
        .metadata
        .get("text")
        .and_then(|v| v.as_str())
        .expect("the `text` metadata key must survive the round trip");
    assert_eq!(
        strip(text),
        strip(dirty),
        "every non-NUL character of the chunk text must survive"
    );
}
