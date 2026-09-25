//! Reciprocal Rank Fusion (RRF) + importance-weight scoring.
//!
//! Port of `cognee/modules/retrieval/hybrid/ranking.py`, **Phase-1 subset
//! only**. The Python function also threads `use_truth_weight` / `q_coords` /
//! `truth_state_by_id` / `current_truth_epoch` for the Phase-2 truth-subspace
//! boost; per the locked Phase-2 deferral those parameters are intentionally
//! absent here and will be added by the Phase-2 task without touching this
//! code path.

use std::collections::HashMap;

use serde_json::Value;

use cognee_graph::NodeTruthState;
use cognee_truth_subspace::align::truth_factor;

use crate::retrievers::hybrid::pairs::ChunkSummaryPair;
use crate::retrievers::hybrid::results::{payload, result_id};

/// RRF constant `k` derived from the requested chunk count.
///
/// Port of `_rrf_k` (`ranking.py:51-52`): `clamp(20 + 2*chunks_top_k, 30, 60)`.
/// Python's `max(30, min(60, ...))` and Rust's `.clamp(30, 60)` are equivalent
/// for non-negative inputs.
pub(crate) fn rrf_k(chunks_top_k: usize) -> usize {
    2usize
        .saturating_mul(chunks_top_k)
        .saturating_add(20)
        .clamp(30, 60)
}

/// Multiplicative importance boost read from a chunk payload.
///
/// Port of `_importance_factor` (`ranking.py:55-59`): reads
/// `payload["importance_weight"]` as a JSON number (default `0.5` if missing or
/// non-numeric — this rejects strings/bools/arrays, an intentional, documented
/// divergence from CPython's `isinstance(True, int)` quirk), clamps to
/// `[0.0, 1.0]`, and returns `0.75 + 0.5 * importance`.
pub(crate) fn importance_factor(chunk_payload: &Value) -> f64 {
    let importance = chunk_payload
        .get("importance_weight")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    0.75 + 0.5 * importance
}

/// Rank chunk↔summary pairs by RRF (optionally importance-weighted) and
/// truncate to `limit`.
///
/// Port of `rank_chunk_summary_pairs` (`ranking.py:7-48`). For each pair
/// carrying a `chunk`, collect the present ranks from
/// `(vector_rank, summary_rank)` (skip if none), compute
/// `rrf_score = Σ 1/(k + rank + 1)`, multiply by `importance_factor` when
/// `use_importance_weight`, then multiply by `truth_factor` when the truth-weight
/// gate holds, and sort by `(-final, -rrf, min_rank, chunk_id)` (float legs via
/// `f64::total_cmp`, per the locked total-ordering decision).
///
/// The truth-subspace boost (`ranking.py:39-44`) is applied strictly AFTER the
/// importance factor and only when `use_truth_weight`, `q_coords` is non-empty,
/// an epoch is known, and the chunk's stored `truth_epoch` matches that current
/// epoch. `NodeTruthState.truth_epoch` is a bare `i64` (with `-1` as the
/// "never scored" sentinel), so a stale/sentinel epoch or a chunk id missing
/// from the map both fall through to no multiplier — identical to Python's
/// `None`-vs-int comparison. When the gate is false the `final_score` is exactly
/// the Phase-1 value, so default-off (`use_truth_weight == false`) ranking is
/// byte-identical to a call with no truth context.
pub(crate) fn rank_chunk_summary_pairs(
    pairs: Vec<ChunkSummaryPair>,
    limit: usize,
    use_importance_weight: bool,
    use_truth_weight: bool,
    q_coords: Option<&[f64]>,
    truth_state_by_id: Option<&HashMap<String, NodeTruthState>>,
    current_truth_epoch: Option<i64>,
) -> Vec<ChunkSummaryPair> {
    if limit == 0 {
        return vec![];
    }

    let k = rrf_k(limit);
    let mut ranked: Vec<(f64, f64, usize, String, ChunkSummaryPair)> = Vec::new();

    for pair in pairs {
        let Some(chunk) = pair.chunk.as_ref() else {
            continue;
        };

        let ranks: Vec<usize> = [pair.vector_rank, pair.summary_rank]
            .into_iter()
            .flatten()
            .collect();
        if ranks.is_empty() {
            continue;
        }

        let rrf_score: f64 = ranks.iter().map(|rank| 1.0 / (k + rank + 1) as f64).sum();
        let mut final_score = if use_importance_weight {
            rrf_score * importance_factor(payload(chunk))
        } else {
            rrf_score
        };
        let min_rank = ranks.iter().copied().min().unwrap_or(0);
        let chunk_id = pair
            .chunk_id
            .clone()
            .or_else(|| result_id(chunk))
            .unwrap_or_default();

        // Truth-subspace boost, applied MULTIPLICATIVELY after the importance
        // factor (`ranking.py:39-44`). `q_coords` is `Copy` (`Option<&[f64]>`), so
        // the `let Some(coords)` bind does not move it. A stale/sentinel epoch or
        // a chunk id absent from the map both leave `final_score` unchanged.
        if use_truth_weight
            && let Some(coords) = q_coords
            && !coords.is_empty()
            && let Some(current_epoch) = current_truth_epoch
            && let Some(truth_state) = truth_state_by_id.and_then(|map| map.get(&chunk_id))
            && truth_state.truth_epoch == current_epoch
        {
            final_score *= truth_factor(&truth_state.truth_alignment, coords);
        }

        ranked.push((final_score, rrf_score, min_rank, chunk_id, pair));
    }

    ranked.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });

    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, _, _, pair)| pair)
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::retrievers::hybrid::pairs::ChunkSummaryPair;
    use crate::types::SearchItem;

    fn chunk_item(id: &str, importance: Option<f64>) -> SearchItem {
        let mut payload = json!({"id": id, "text": format!("text-{id}")});
        if let Some(weight) = importance {
            payload["importance_weight"] = json!(weight);
        }
        SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    fn pair(
        id: &str,
        vector: Option<usize>,
        summary: Option<usize>,
        importance: Option<f64>,
    ) -> ChunkSummaryPair {
        ChunkSummaryPair {
            chunk_id: Some(id.to_string()),
            chunk_text: Some(format!("text-{id}")),
            summary_id: None,
            summary_text: None,
            chunk: Some(chunk_item(id, importance)),
            vector_rank: vector,
            summary_rank: summary,
        }
    }

    #[test]
    fn rrf_k_boundaries() {
        assert_eq!(rrf_k(0), 30);
        assert_eq!(rrf_k(5), 30); // 20 + 10 = 30
        assert_eq!(rrf_k(10), 40); // 20 + 20 = 40 (mid-range)
        assert_eq!(rrf_k(20), 60); // 20 + 40 = 60
        assert_eq!(rrf_k(100), 60); // clamped
    }

    #[test]
    fn importance_factor_bounds() {
        assert_eq!(importance_factor(&json!({})), 1.0); // default 0.5 -> 0.75 + 0.25
        assert_eq!(importance_factor(&json!({"importance_weight": "x"})), 1.0);
        assert_eq!(importance_factor(&json!({"importance_weight": 0.0})), 0.75);
        assert_eq!(importance_factor(&json!({"importance_weight": 1.0})), 1.25);
        assert_eq!(importance_factor(&json!({"importance_weight": 0.5})), 1.0);
        // Out of range clamps.
        assert_eq!(importance_factor(&json!({"importance_weight": 1.5})), 1.25);
        assert_eq!(importance_factor(&json!({"importance_weight": -1.0})), 0.75);
    }

    /// Phase-1 baseline ranking call: truth weighting fully off, no truth
    /// context. Keeps the existing tests reading exactly as they did before
    /// P2-07 while pinning the "default-off" argument shape in one place.
    fn baseline(
        pairs: Vec<ChunkSummaryPair>,
        limit: usize,
        use_importance_weight: bool,
    ) -> Vec<ChunkSummaryPair> {
        rank_chunk_summary_pairs(pairs, limit, use_importance_weight, false, None, None, None)
    }

    #[test]
    fn limit_zero_returns_empty() {
        let pairs = vec![pair("a", Some(0), Some(0), None)];
        assert!(baseline(pairs, 0, false).is_empty());
    }

    #[test]
    fn pair_without_ranks_is_skipped() {
        let pairs = vec![pair("a", None, None, None)];
        assert!(baseline(pairs, 5, false).is_empty());
    }

    #[test]
    fn both_lanes_outrank_single_lane() {
        // Same rank slot value but different lane counts.
        let two = pair("two", Some(1), Some(1), None);
        let one = pair("one", Some(0), None, None);
        let ranked = baseline(vec![one, two], 5, false);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("two"));
    }

    #[test]
    fn importance_weight_can_reorder() {
        // Identical ranks; the higher importance weight wins when enabled.
        let low = pair("low", Some(0), None, Some(0.0));
        let high = pair("high", Some(0), None, Some(1.0));
        let ranked = baseline(vec![low, high], 5, true);
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("high"));

        // With importance off, tie-break falls to chunk_id string order.
        let low = pair("low", Some(0), None, Some(0.0));
        let high = pair("high", Some(0), None, Some(1.0));
        let ranked = baseline(vec![low, high], 5, false);
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("high")); // "high" < "low"
    }

    #[test]
    fn tie_break_by_chunk_id() {
        // Identical scores/ranks -> ascending chunk_id string.
        let b = pair("bbb", Some(0), None, None);
        let a = pair("aaa", Some(0), None, None);
        let ranked = baseline(vec![b, a], 5, false);
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("aaa"));
        assert_eq!(ranked[1].chunk_id.as_deref(), Some("bbb"));
    }

    #[test]
    fn hand_computed_rrf_score_orders_by_min_rank_on_tie() {
        // limit=5 -> k=30. Pair X: ranks {vector:0} -> 1/(30+0+1) = 1/31.
        // Pair Y: ranks {vector:0, summary:2} -> 1/31 + 1/33.
        // Y has a higher rrf sum, so ranks first.
        let x = pair("x", Some(0), None, None);
        let y = pair("y", Some(0), Some(2), None);
        let ranked = baseline(vec![x, y], 5, false);
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("y"));
    }

    #[test]
    fn truncates_to_limit() {
        let pairs = vec![
            pair("a", Some(0), None, None),
            pair("b", Some(1), None, None),
            pair("c", Some(2), None, None),
        ];
        let ranked = baseline(pairs, 2, false);
        assert_eq!(ranked.len(), 2);
    }

    // ---- Truth-subspace multiplier (P2-07) ----

    /// Chunk ids for the order-flip fixture below. `HI_ID` sorts AFTER
    /// `BOOST_ID` on purpose, so a correct (score-driven) baseline places `hi`
    /// first *despite* the id order — proving the ordering is not accidentally
    /// decided by the chunk_id tie-break.
    const HI_ID: &str = "bbb-hi";
    const BOOST_ID: &str = "aaa-boost";

    /// Ordered chunk ids of a ranking result, for full-order comparisons.
    fn ids(ranked: &[ChunkSummaryPair]) -> Vec<Option<String>> {
        ranked.iter().map(|p| p.chunk_id.clone()).collect()
    }

    /// Two chunks engineered so the truth multiplier — if it were (wrongly)
    /// applied to the aligned `boost` chunk — would FLIP the baseline order.
    ///
    /// Both carry importance 0.5 (factor 1.0), so the only Phase-1 differentiator
    /// is the RRF rank: `hi` sits at rank 0 (rrf `1/31 = 0.032258`) and `boost`
    /// at rank 1 (rrf `1/32 = 0.031250`), so the no-multiplier order is
    /// `[hi, boost]`. If the 1.25 factor leaked onto `boost`, its score becomes
    /// `0.031250 * 1.25 = 0.039063 > 0.032258`, flipping the order to
    /// `[boost, hi]`. Any spurious factor above ~1.032 flips it.
    fn flip_pairs() -> Vec<ChunkSummaryPair> {
        vec![
            pair(HI_ID, Some(0), None, Some(0.5)),
            pair(BOOST_ID, Some(1), None, Some(0.5)),
        ]
    }

    /// Truth state that boosts only `BOOST_ID` (aligned with the `[1.0, 0.0]`
    /// query direction -> factor 1.25) at the given epoch.
    fn boost_state(epoch: i64) -> HashMap<String, NodeTruthState> {
        let mut states = HashMap::new();
        states.insert(
            BOOST_ID.to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0, 0.0],
                truth_epoch: epoch,
            },
        );
        states
    }

    #[test]
    fn truth_weight_off_is_byte_identical_to_no_truth_context() {
        // Multi-chunk set where the LAST chunk ("lo") carries an aligned truth
        // state. If the off-switch leaked, "lo"'s 1.25 boost would reorder the
        // result, so an identical full ordering across the two calls genuinely
        // proves byte-identity rather than accidental single-chunk agreement.
        let q_coords = vec![1.0, 0.0];
        let epoch = 3;
        let mut states = HashMap::new();
        states.insert(
            "lo".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0, 0.0], // aligned -> would boost 1.25
                truth_epoch: epoch,
            },
        );
        // rrf: hi 1/31 = 0.032258, mid 1/32 = 0.031250, lo 1/33 = 0.030303.
        let mk = || {
            vec![
                pair("hi", Some(0), None, Some(0.5)),
                pair("mid", Some(1), None, Some(0.5)),
                pair("lo", Some(2), None, Some(0.5)),
            ]
        };

        // Truth OFF but full context supplied.
        let with_ctx_off = rank_chunk_summary_pairs(
            mk(),
            5,
            true,
            false, // use_truth_weight OFF
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        // No truth context at all.
        let no_ctx = rank_chunk_summary_pairs(mk(), 5, true, false, None, None, None);

        // Full ordering identical, and equal to the pure rrf x importance
        // baseline [hi, mid, lo].
        let expected = vec![
            Some("hi".to_string()),
            Some("mid".to_string()),
            Some("lo".to_string()),
        ];
        assert_eq!(ids(&with_ctx_off), expected);
        assert_eq!(ids(&no_ctx), expected);
        assert_eq!(ids(&with_ctx_off), ids(&no_ctx));

        // Positive control: the SAME fixture with truth ON reorders ("lo"'s
        // 1/33 * 1.25 = 0.037879 beats "hi"'s 1/31 = 0.032258), proving the
        // off-assertions above are non-vacuous — a leaked multiplier is
        // observable here.
        let with_ctx_on = rank_chunk_summary_pairs(
            mk(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(with_ctx_on[0].chunk_id.as_deref(), Some("lo"));
        assert_ne!(ids(&with_ctx_on), expected);
    }

    /// Each gate condition, negated in turn, must leave ranking equal to the
    /// no-truth baseline. Uses the [`flip_pairs`] order-flip fixture: the
    /// aligned `boost` chunk sits one RRF rank behind `hi`, so any spuriously
    /// applied 1.25 factor would flip the order to `[boost, hi]`. A positive
    /// control below shows the fixture DOES flip when every gate is satisfied,
    /// which makes the negative-case assertions non-vacuous.
    #[test]
    fn truth_gate_negative_cases_match_baseline() {
        let q_coords = vec![1.0, 0.0];
        let states = boost_state(3);
        let epoch = 3;

        // Reference: truth weighting off, no context. Score-driven order is
        // [hi, boost] (hi's id sorts AFTER boost's, so this also confirms the
        // order comes from the score, not the chunk_id tie-break).
        let base = baseline(flip_pairs(), 5, true);
        assert_eq!(
            ids(&base),
            vec![Some(HI_ID.to_string()), Some(BOOST_ID.to_string())]
        );

        // Positive control: with EVERY gate satisfied the same fixture flips to
        // [boost, hi]. This proves an errantly-applied multiplier is observable,
        // so the negative assertions genuinely guard the gate.
        let applied = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(
            ids(&applied),
            vec![Some(BOOST_ID.to_string()), Some(HI_ID.to_string())]
        );

        // 1. use_truth_weight = false, but full context present. A gate that
        //    ignored the flag would boost `boost` and flip the order.
        let c1 = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            false,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        // 2. q_coords empty. (truth_factor is the neutral 1.0 for empty query
        //    coords, so this alone cannot flip the order; the assertion still
        //    pins the baseline and guards against a mutant that fabricated
        //    coords.)
        let empty: Vec<f64> = vec![];
        let c2 = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&empty),
            Some(&states),
            Some(epoch),
        );
        // 3. q_coords None — structurally impossible to apply (no coords to
        //    pass through).
        let c3 = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            None,
            Some(&states),
            Some(epoch),
        );
        // 4. current_truth_epoch None. A gate treating an unknown epoch as
        //    "matches" would boost and flip.
        let c4 = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            None,
        );
        // 5. chunk id missing from the truth-state map (empty map). A gate that
        //    defaulted a missing chunk to aligned would boost and flip.
        let other: HashMap<String, NodeTruthState> = HashMap::new();
        let c5 = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&other),
            Some(epoch),
        );

        for c in [c1, c2, c3, c4, c5] {
            assert_eq!(ids(&c), ids(&base));
        }
    }

    #[test]
    fn truth_multiplier_applies_and_matches_truth_factor() {
        // Two chunks with identical RRF and identical importance (0.5 -> factor
        // 1.0). Chunk "a" is strongly aligned (truth_factor 1.25); chunk "b" is
        // orthogonal to the query direction (truth_factor 1.0). With the boost
        // applied, "a" must outrank "b"; the ratio of their final scores equals
        // the ratio of their truth factors.
        let q_coords = vec![1.0, 0.0];
        let mut states = HashMap::new();
        states.insert(
            "a".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0, 0.0], // aligned -> factor 1.25
                truth_epoch: 3,
            },
        );
        states.insert(
            "b".to_string(),
            NodeTruthState {
                truth_alignment: vec![0.0, 1.0], // orthogonal -> factor 1.0
                truth_epoch: 3,
            },
        );

        // Identical importance (0.5) and identical single-lane rank 0 so the only
        // differentiator is the truth factor.
        let a = pair("a", Some(0), None, Some(0.5));
        let b = pair("b", Some(0), None, Some(0.5));

        // Baseline (truth off): tie resolves to chunk_id order -> "a" first, "b"
        // second (they carry equal scores).
        let base =
            rank_chunk_summary_pairs(vec![b.clone(), a.clone()], 5, true, false, None, None, None);
        assert_eq!(base[0].chunk_id.as_deref(), Some("a"));
        assert_eq!(base[1].chunk_id.as_deref(), Some("b"));

        // With truth on, "a"'s 1.25 factor beats "b"'s 1.0 factor.
        let ranked = rank_chunk_summary_pairs(
            vec![b, a],
            5,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            Some(3),
        );
        assert_eq!(ranked[0].chunk_id.as_deref(), Some("a"));
        assert_eq!(ranked[1].chunk_id.as_deref(), Some("b"));

        // The factor "a" would have received is the independently-computed
        // truth_factor over its alignment and the query coords (proves we call
        // the real function, not a hand-copied literal).
        let expected_factor = truth_factor(&[1.0, 0.0], &q_coords);
        assert!((expected_factor - 1.25).abs() < 1e-9);
    }

    #[test]
    fn truth_stale_epoch_and_missing_id_get_no_multiplier() {
        let q_coords = vec![1.0, 0.0];

        // No-truth baseline on the order-flip fixture: [hi, boost].
        let base = baseline(flip_pairs(), 5, true);
        let base_ids = ids(&base);
        assert_eq!(
            base_ids,
            vec![Some(HI_ID.to_string()), Some(BOOST_ID.to_string())]
        );

        // Positive control: at the CURRENT epoch (3) the fixture flips to
        // [boost, hi], so a wrongly-applied multiplier below would be observable.
        let fresh = boost_state(3);
        let applied = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&fresh),
            Some(3),
        );
        assert_eq!(
            ids(&applied),
            vec![Some(BOOST_ID.to_string()), Some(HI_ID.to_string())]
        );

        // Stale-epoch case (epoch - 1): `boost` is present and aligned but at
        // epoch 2 while the current epoch is 3, so the multiplier must be
        // skipped and the order stays the baseline [hi, boost]. Were it applied
        // the order would flip.
        let stale = boost_state(2);
        let stale_ranked = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&stale),
            Some(3),
        );
        assert_eq!(ids(&stale_ranked), base_ids);

        // Missing-from-map case, handled separately: an empty map means `boost`
        // is absent, so no multiplier applies and the order stays the baseline.
        let missing: HashMap<String, NodeTruthState> = HashMap::new();
        let missing_ranked = rank_chunk_summary_pairs(
            flip_pairs(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&missing),
            Some(3),
        );
        assert_eq!(ids(&missing_ranked), base_ids);
    }

    #[test]
    fn final_score_composes_importance_then_truth() {
        // Composition test: the full `rrf * importance_factor * truth_factor`
        // product (1.25 * 1.25 = 1.5625) must be applied — and only the FULL
        // product, not either factor alone, reproduces the winning order.
        //
        // limit=5 -> k=30, single-lane rrf = 1/(31 + rank).
        //   plain: rank 0  -> rrf 1/31 = 0.0322581, importance 0.5 (factor 1.0),
        //          absent from the truth map (no truth factor)  -> 0.0322581
        //   boost: rank 12 -> rrf 1/43 = 0.0232558, importance 1.0 (factor 1.25),
        //          aligned truth at the current epoch (factor 1.25).
        //     full product : 0.0232558 * 1.5625 = 0.0363372 > 0.0322581  => boost wins
        //     importance only: 0.0232558 * 1.25 = 0.0290698 < 0.0322581  => plain wins
        //     truth only     : 0.0232558 * 1.25 = 0.0290698 < 0.0322581  => plain wins
        //     neither        : 0.0232558          < 0.0322581            => plain wins
        // So [boost, plain] is only reachable when BOTH factors compose.
        let q_coords = vec![1.0, 0.0];
        let epoch = 3;
        let mut states = HashMap::new();
        states.insert(
            "boost".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0, 0.0], // aligned with q_coords -> factor 1.25
                truth_epoch: epoch,
            },
        );

        // Pin the two factors that must multiply to 1.5625 (proves the constants
        // and that both are the real functions, not hand-copied literals).
        let boost_payload = json!({"id": "boost", "importance_weight": 1.0});
        assert!((importance_factor(&boost_payload) - 1.25).abs() < 1e-12);
        assert!((truth_factor(&[1.0, 0.0], &q_coords) - 1.25).abs() < 1e-12);

        let mk = || {
            vec![
                pair("plain", Some(0), None, Some(0.5)),
                pair("boost", Some(12), None, Some(1.0)),
            ]
        };
        let boost_first = vec![Some("boost".to_string()), Some("plain".to_string())];
        let plain_first = vec![Some("plain".to_string()), Some("boost".to_string())];

        // Full composition: importance ON + truth ON -> boost overtakes.
        let both = rank_chunk_summary_pairs(
            mk(),
            5,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(ids(&both), boost_first);

        // Importance only (truth off): 1.25 alone is not enough -> plain first.
        let imp_only = rank_chunk_summary_pairs(mk(), 5, true, false, None, None, None);
        assert_eq!(ids(&imp_only), plain_first);

        // Truth only (importance off): 1.25 alone is not enough -> plain first.
        let truth_only = rank_chunk_summary_pairs(
            mk(),
            5,
            false,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(ids(&truth_only), plain_first);

        // Neither factor: pure rrf -> plain first.
        let neither = rank_chunk_summary_pairs(mk(), 5, false, false, None, None, None);
        assert_eq!(ids(&neither), plain_first);
    }

    #[test]
    fn current_epoch_vector_overtakes_stale_vector() {
        // Selective per-chunk epoch matching: only the chunk whose stored
        // truth_epoch equals the current epoch receives the multiplier, even
        // though BOTH chunks are equally aligned.
        //
        // limit=5 -> k=30. importance weighting OFF, so scores are pure rrf x
        // (optional) truth factor.
        //   stale:   rank 0 -> rrf 1/31 = 0.0322581; truth_epoch 1 != 2 -> no boost.
        //   current: rank 1 -> rrf 1/32 = 0.0312500; truth_epoch 2 == 2 ->
        //            * truth_factor([1.0],[1.0]) = 1.25 -> 0.0390625.
        //   0.0390625 > 0.0322581  => order flips to [current, stale].
        let q_coords = vec![1.0];
        let current_epoch = 2;
        let mut states = HashMap::new();
        states.insert(
            "stale".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0],
                truth_epoch: 1, // stale
            },
        );
        states.insert(
            "current".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0],
                truth_epoch: 2, // current
            },
        );

        let mk = || {
            vec![
                pair("stale", Some(0), None, Some(0.5)),
                pair("current", Some(1), None, Some(0.5)),
            ]
        };

        // Positive control: with truth OFF the higher-rrf stale chunk wins.
        let base = rank_chunk_summary_pairs(mk(), 5, false, false, None, None, None);
        assert_eq!(
            ids(&base),
            vec![Some("stale".to_string()), Some("current".to_string())]
        );

        // Truth ON: only `current` (matching epoch) is boosted, overtaking stale.
        let ranked = rank_chunk_summary_pairs(
            mk(),
            5,
            false, // use_importance_weight
            true,  // use_truth_weight
            Some(&q_coords),
            Some(&states),
            Some(current_epoch),
        );
        assert_eq!(
            ids(&ranked),
            vec![Some("current".to_string()), Some("stale".to_string())]
        );
    }
}
