//! Zero-norm (all-zero) embedding vector detection.
//!
//! A zero vector has no direction, so cosine similarity against it is
//! mathematically undefined — and every backend here scores by cosine. The
//! four backends then disagree about what happens, none of them loudly:
//!
//! | backend       | zero-norm row                                   |
//! |---------------|-------------------------------------------------|
//! | LanceDB       | distance is `NaN`; `KNNVectorDistanceExec` filters `NaN` rows out, so the row is read from storage and silently discarded |
//! | pgvector      | `vector <=> $1` yields `NaN` for a zero operand, so the row sorts and scores unusably |
//! | brute-force   | `cosine_similarity` clamps the denominator with `max(f32::EPSILON)`, so the row survives with score `0.0` and ranks last |
//! | MockVectorDB  | returns `0.0` when either magnitude is zero — same last-place ranking as brute-force |
//!
//! The mock is included deliberately despite being `cfg(feature = "testing")`:
//! it is the backend most suites actually run against, and it is where the
//! `MOCK_EMBEDDING` zero-vector problem hid in the first place. A diagnostic
//! that goes quiet in tests is quiet exactly where it would be read.
//!
//! In every case the point is not retrievable as the caller intended, and in
//! the LanceDB case — the OSS default — it is not retrievable at all. Nothing
//! errors, so a zero-norm write is indistinguishable from a successful one.
//!
//! Two things produce zero vectors in practice:
//!
//! 1. `MOCK_EMBEDDING=zero`, which asks for them deliberately.
//! 2. Empty or whitespace-only input text with a real provider. The cloud
//!    embedding adapters substitute `"."` before the API call and then
//!    overwrite that slot with a zero vector (see `handle_embedding_response`
//!    in `cognee-embedding`), so a blank chunk, entity name, or summary is
//!    stored as an unsearchable row.
//!
//! These helpers do not reject anything — they only make the condition visible
//! in logs, which is what was missing when both cases went unnoticed.

use crate::models::VectorPoint;

/// Returns `true` if every component is exactly zero.
///
/// Testing components rather than a computed norm is deliberate: it is exact,
/// needs no square root, and it catches the two cases that actually occur —
/// `MOCK_EMBEDDING=zero`, and the deliberate zeroing of empty-text slots in
/// `handle_embedding_response`. Both write literal zeros.
///
/// This is **not** equivalent to "the computed `f32` norm is zero", and the
/// difference is a known blind spot rather than an invariant. Squares of very
/// small components underflow: for `[f32::MIN_POSITIVE, 0.0]` the sum of
/// squares rounds to `0.0`, so the norm is `0.0` and cosine is every bit as
/// undefined as for an all-zero vector — yet this returns `false` and no
/// warning fires. Accepted because no embedding model emits components near
/// `1e-38`, and because an epsilon on the norm would start warning about
/// legitimate small-magnitude vectors. If that assumption ever breaks, the fix
/// is an explicit norm-underflow check here, not a tolerance.
pub(crate) fn is_zero_norm(vector: &[f32]) -> bool {
    !vector.is_empty() && vector.iter().all(|v| *v == 0.0)
}

/// Warn once per batch if any point carries a zero-norm vector.
///
/// Emits a single record with a count rather than one per point, so a corpus
/// with many blank fields cannot flood the log.
pub(crate) fn warn_zero_norm_points(backend: &str, collection: &str, points: &[VectorPoint]) {
    let zero_norm = points
        .iter()
        .filter(|p| is_zero_norm(&p.vector))
        .collect::<Vec<_>>();
    if zero_norm.is_empty() {
        return;
    }
    // A few ids are enough to chase the source data; the full list would be
    // unbounded.
    let sample: Vec<String> = zero_norm.iter().take(3).map(|p| p.id.to_string()).collect();
    tracing::warn!(
        backend,
        collection,
        zero_norm_points = zero_norm.len(),
        total_points = points.len(),
        sample_ids = %sample.join(", "),
        "indexing zero-norm embedding vectors — cosine similarity is undefined for these, so \
         they are dropped from KNN results on LanceDB/pgvector and ranked last on brute-force; \
         usual cause is empty or whitespace-only input text, or MOCK_EMBEDDING=zero"
    );
}

/// Warn if a query vector is zero-norm.
///
/// A zero-norm query cannot rank anything, so the search returns nothing
/// useful regardless of what the collection holds — the single most confusing
/// version of this failure, because the data is fine and the query is at fault.
pub(crate) fn warn_zero_norm_query(backend: &str, collection: &str, query_vector: &[f32]) {
    if !is_zero_norm(query_vector) {
        return;
    }
    tracing::warn!(
        backend,
        collection,
        "zero-norm query vector — cosine similarity is undefined, so this search cannot rank \
         any row and will return nothing (LanceDB/pgvector) or an arbitrary order (brute-force); \
         usual cause is an empty or whitespace-only query string, or MOCK_EMBEDDING=zero"
    );
}

/// Warn once if any query vector in a batch is zero-norm.
///
/// `batch_search_similar` overrides exist that never route through
/// `search_similar` (pgvector builds one `unnest ... LATERAL` round-trip), so
/// the single-query helper never runs for them. Counts rather than warning per
/// vector, so a large batch of blanks cannot flood the log.
///
/// Gated because pgvector is currently the only backend that overrides
/// `batch_search_similar`; every other backend inherits the default, which
/// loops `search_similar` and is therefore covered by the single-query helper.
/// Without the gate this is dead code in any build without `pgvector` — a
/// `-D warnings` failure that only appears in feature combinations the lint
/// lanes do not happen to cover. Widen the gate if another backend overrides.
#[cfg(feature = "pgvector")]
pub(crate) fn warn_zero_norm_query_batch(
    backend: &str,
    collection: &str,
    query_vectors: &[Vec<f32>],
) {
    let zero_norm = query_vectors.iter().filter(|v| is_zero_norm(v)).count();
    if zero_norm == 0 {
        return;
    }
    tracing::warn!(
        backend,
        collection,
        zero_norm_queries = zero_norm,
        total_queries = query_vectors.len(),
        "zero-norm query vectors in a batch search — cosine similarity is undefined, so these \
         queries cannot rank any row; usual cause is empty or whitespace-only query text, or \
         MOCK_EMBEDDING=zero"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn all_zero_is_zero_norm() {
        assert!(is_zero_norm(&[0.0, 0.0, 0.0]));
        assert!(is_zero_norm(&[0.0]));
    }

    #[test]
    fn negative_zero_is_still_zero_norm() {
        // -0.0 == 0.0 in IEEE 754, and its square is +0.0, so the norm is zero.
        assert!(is_zero_norm(&[-0.0, 0.0, -0.0]));
    }

    #[test]
    fn any_nonzero_component_is_not_zero_norm() {
        assert!(!is_zero_norm(&[0.0, 0.0, 1.0]));
        assert!(!is_zero_norm(&[-1.0, 0.0]));
    }

    /// Documents the blind spot named in `is_zero_norm`'s doc comment, so the
    /// gap is pinned behaviour rather than a surprise. This vector's *computed*
    /// f32 norm underflows to zero, making it as unsearchable as an all-zero
    /// one, but the component test does not flag it.
    #[test]
    fn underflowing_norm_is_a_known_blind_spot() {
        let tiny = [f32::MIN_POSITIVE, 0.0];
        // The square underflows, so the norm really is 0.0 ...
        let sum_sq: f32 = tiny.iter().map(|v| v * v).sum();
        assert_eq!(
            sum_sq, 0.0,
            "expected the square of MIN_POSITIVE to underflow"
        );
        // ... yet no component is zero, so this returns false and stays silent.
        assert!(!is_zero_norm(&tiny));
    }

    #[test]
    fn empty_vector_is_not_reported() {
        // An empty vector is a dimension error, caught elsewhere with a precise
        // message. Reporting it as zero-norm would misattribute the cause.
        assert!(!is_zero_norm(&[]));
    }

    #[test]
    fn warn_helpers_tolerate_empty_and_clean_input() {
        // No panics and no work when there is nothing to report.
        warn_zero_norm_points("test", "T_f", &[]);
        warn_zero_norm_query("test", "T_f", &[1.0, 0.0]);
        let clean = vec![VectorPoint::new(Uuid::new_v4(), vec![1.0, 2.0])];
        warn_zero_norm_points("test", "T_f", &clean);
    }
}
