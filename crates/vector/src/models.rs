use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use uuid::Uuid;

/// Vector point to be indexed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPoint {
    /// Data point ID
    pub id: Uuid,

    /// Embedding vector
    pub vector: Vec<f32>,

    /// Metadata (type, field, original data)
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Result from similarity search
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Data point ID
    pub id: Uuid,

    /// Similarity score (higher = more similar)
    pub score: f32,

    /// Metadata from the indexed point
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Configuration for vector collection
#[derive(Debug, Clone)]
pub struct CollectionConfig {
    /// Collection name (e.g., "DocumentChunk_text")
    pub name: String,

    /// Vector dimension
    pub dimension: usize,

    /// Distance metric (Cosine, Euclidean, Dot)
    pub distance: DistanceMetric,
}

/// Distance metric used for vector similarity comparisons.
#[derive(Debug, Clone, Copy)]
pub enum DistanceMetric {
    /// Cosine similarity (angle-based, ignores magnitude).
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Dot-product similarity.
    Dot,
}

impl VectorPoint {
    /// Create a new vector point
    pub fn new(id: Uuid, vector: Vec<f32>) -> Self {
        Self {
            id,
            vector,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata field
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Accumulate the dataset membership recorded on a `previous` point (an
    /// existing point with the same id) into `self`'s [`DATASET_IDS_KEY`] array.
    ///
    /// Point IDs are content-addressed (UUID v5 of the content), so the *same*
    /// point is indexed once per dataset that contains that content. Vector
    /// adapters upsert by id with full replacement, so a plain replace keeps
    /// only the last dataset's scalar `dataset_id` and silently drops the
    /// earlier datasets' membership — making the content unretrievable when a
    /// search is scoped to one of those earlier datasets (the cross-dataset
    /// dedup bug). Calling this in `index_points` upsert paths before replacing
    /// an existing point keeps `dataset_ids` as the union of every dataset the
    /// content belongs to, mirroring Python's `belongs_to_set` union semantics.
    pub fn merge_dataset_membership(&mut self, previous: &VectorPoint) {
        let mut ids: Vec<String> = Vec::new();
        // `previous` first so membership order is stable (oldest dataset first).
        collect_dataset_ids(previous, &mut ids);
        collect_dataset_ids(self, &mut ids);
        if !ids.is_empty() {
            self.metadata.insert(
                DATASET_IDS_KEY.to_string(),
                serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect()),
            );
        }
    }
}

/// Fold repeated ids in `points` down to one point per distinct id, preserving
/// the order in which each id first appears.
///
/// Point IDs are content-addressed (UUID v5 of the content) and callers emit
/// one point per *occurrence* — one per edge for the `EdgeType_relationship_name`
/// index, for instance — so a corpus with thousands of edges over a small
/// relationship vocabulary repeats the same id many times inside a single
/// `index_points` call. Adapters that write a batch as one statement must not
/// see those repeats:
///
/// * Postgres aborts any statement whose `ON CONFLICT` target is touched twice
///   in one command with `ON CONFLICT DO UPDATE command cannot affect row a
///   second time`, failing the whole upsert (and, upstream, rolling the
///   pipeline's graph writes back).
/// * LanceDB's delete-then-add upsert has no primary key, so repeats land as
///   several physical rows sharing one id.
///
/// Folding rule, mirroring Python's `PGVectorAdapter.create_data_points`
/// (`deduped_by_id` + the `belongs_to_set` union): the **last** occurrence wins
/// for the vector and for every ordinary metadata field — the id is
/// content-addressed, so every occurrence carries the same content and the same
/// embedding — while dataset membership is the **union** over all occurrences,
/// oldest first. A naive last-one-wins would drop the membership recorded on the
/// earlier duplicates, re-introducing the cross-dataset dedup bug that
/// [`VectorPoint::merge_dataset_membership`] exists to prevent, one batch at a
/// time instead of one upsert at a time.
///
/// The union against whatever is already stored in the database is a separate,
/// later step (`fetch_metadata` + `merge_dataset_membership` in the pgvector
/// adapter); this only folds the *incoming* list.
pub fn dedup_points_by_id(points: &[VectorPoint]) -> Vec<VectorPoint> {
    fold_by_id(points, true)
}

/// Fold repeated ids in `points` down to one point per distinct id, keeping the
/// **last** occurrence verbatim — including its metadata, with no dataset
/// membership union.
///
/// The `upsert_raw_vectors` counterpart to [`dedup_points_by_id`]. Raw upsert
/// writes system-owned collections (`TruthCentroid_vector` and friends) whose
/// metadata is stored exactly as handed in; unioning `dataset_ids` there would
/// invent membership the in-memory and LanceDB raw paths do not record, so the
/// three adapters would stop agreeing on the same input. The repeats still have
/// to go: the multi-row `INSERT … ON CONFLICT DO UPDATE` this feeds is the same
/// statement shape that Postgres aborts when one id is touched twice.
pub fn dedup_points_by_id_last_wins(points: &[VectorPoint]) -> Vec<VectorPoint> {
    fold_by_id(points, false)
}

/// Shared implementation of the two folds above: one point per distinct id, in
/// first-appearance order, last occurrence winning. With `union_membership`,
/// each later occurrence first absorbs the membership accumulated so far.
fn fold_by_id(points: &[VectorPoint], union_membership: bool) -> Vec<VectorPoint> {
    // `folded` holds the output in first-appearance order; `slot_of` maps an id
    // to the index it already occupies there. The index is recorded as the
    // point is pushed and `folded` only ever grows, so every stored index stays
    // in bounds — no lookup here can fail or panic.
    let mut folded: Vec<VectorPoint> = Vec::with_capacity(points.len());
    let mut slot_of: HashMap<Uuid, usize> = HashMap::with_capacity(points.len());

    for point in points {
        match slot_of.entry(point.id) {
            Entry::Vacant(slot) => {
                slot.insert(folded.len());
                folded.push(point.clone());
            }
            Entry::Occupied(slot) => {
                let existing = &mut folded[*slot.get()];
                let mut latest = point.clone();
                if union_membership {
                    // Arg order puts the older dataset ids first, matching the
                    // DB-union call sites.
                    latest.merge_dataset_membership(existing);
                }
                *existing = latest;
            }
        }
    }

    folded
}

/// Metadata key holding the array of dataset-ID strings a point belongs to.
/// This is the union accumulated across every dataset the content-addressed
/// point has been indexed under (see [`VectorPoint::merge_dataset_membership`]).
pub const DATASET_IDS_KEY: &str = "dataset_ids";

/// Scalar metadata key written by the cognify indexer for the single dataset
/// currently being indexed. Retained for back-compat; the authoritative
/// membership is the union in [`DATASET_IDS_KEY`].
pub const DATASET_ID_KEY: &str = "dataset_id";

/// Append every dataset-ID string recorded on `point` (from both the
/// [`DATASET_IDS_KEY`] array and the scalar [`DATASET_ID_KEY`]) into `out`,
/// skipping empties and duplicates.
fn collect_dataset_ids(point: &VectorPoint, out: &mut Vec<String>) {
    if let Some(arr) = point
        .metadata
        .get(DATASET_IDS_KEY)
        .and_then(|v| v.as_array())
    {
        for v in arr {
            if let Some(s) = v.as_str()
                && !s.is_empty()
                && !out.iter().any(|x| x == s)
            {
                out.push(s.to_string());
            }
        }
    }
    if let Some(s) = point.metadata.get(DATASET_ID_KEY).and_then(|v| v.as_str())
        && !s.is_empty()
        && !out.iter().any(|x| x == s)
    {
        out.push(s.to_string());
    }
}

#[cfg(test)]
mod dedup_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable failures"
    )]

    use super::*;
    use serde_json::json;

    fn point(id: u128, dataset: &str, text: &str) -> VectorPoint {
        VectorPoint::new(Uuid::from_u128(id), vec![0.1, 0.2, 0.3])
            .with_metadata("text", json!(text))
            .with_metadata(DATASET_ID_KEY, json!(dataset))
    }

    fn dataset_ids(p: &VectorPoint) -> Vec<String> {
        p.metadata
            .get(DATASET_IDS_KEY)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_list_without_repeats_is_returned_unchanged() {
        let input = vec![point(1, "ds-a", "one"), point(2, "ds-b", "two")];
        let out = dedup_points_by_id(&input);
        assert_eq!(
            out.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(1), Uuid::from_u128(2)]
        );
    }

    #[test]
    fn repeats_collapse_to_one_point_per_id_in_first_appearance_order() {
        let input = vec![
            point(7, "ds-a", "seven"),
            point(3, "ds-a", "three"),
            point(7, "ds-b", "seven"),
            point(3, "ds-b", "three"),
            point(7, "ds-c", "seven"),
        ];
        let out = dedup_points_by_id(&input);
        assert_eq!(
            out.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(7), Uuid::from_u128(3)],
            "id 7 appears first in the input, so it stays first in the output"
        );
    }

    #[test]
    fn folding_unions_dataset_membership_instead_of_dropping_it() {
        // The naive last-one-wins this replaces would leave only "ds-c".
        let input = vec![
            point(7, "ds-a", "seven"),
            point(7, "ds-b", "seven"),
            point(7, "ds-c", "seven"),
        ];
        let out = dedup_points_by_id(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(
            dataset_ids(&out[0]),
            vec!["ds-a".to_string(), "ds-b".to_string(), "ds-c".to_string()],
            "membership is the union over every duplicate, oldest dataset first"
        );
    }

    #[test]
    fn an_already_unioned_dataset_ids_array_is_carried_through() {
        let first = point(7, "ds-a", "seven").with_metadata(
            DATASET_IDS_KEY,
            json!(["ds-earlier".to_string(), "ds-a".to_string()]),
        );
        let out = dedup_points_by_id(&[first, point(7, "ds-b", "seven")]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            dataset_ids(&out[0]),
            vec![
                "ds-earlier".to_string(),
                "ds-a".to_string(),
                "ds-b".to_string()
            ]
        );
    }

    #[test]
    fn the_last_occurrence_wins_for_ordinary_metadata_and_the_vector() {
        let mut second = point(7, "ds-b", "rewritten");
        second.vector = vec![9.0, 9.0, 9.0];
        let out = dedup_points_by_id(&[point(7, "ds-a", "original"), second]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].metadata.get("text").unwrap(), &json!("rewritten"));
        assert_eq!(out[0].vector, vec![9.0, 9.0, 9.0]);
        assert_eq!(
            out[0].metadata.get(DATASET_ID_KEY).unwrap(),
            &json!("ds-b"),
            "the scalar tag is the newest one; the union lives in dataset_ids"
        );
    }

    #[test]
    fn an_empty_list_folds_to_an_empty_list() {
        assert!(dedup_points_by_id(&[]).is_empty());
    }

    #[test]
    fn points_carrying_no_dataset_tag_at_all_gain_no_membership_key() {
        let bare = VectorPoint::new(Uuid::from_u128(11), vec![0.1, 0.2, 0.3]);
        let out = dedup_points_by_id(&[bare.clone(), bare]);
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].metadata.contains_key(DATASET_IDS_KEY),
            "folding must not invent an empty membership array"
        );
    }
}
