//! Chunk-lane fusion + importance-weight scoring.
//!
//! Port of `cognee/modules/retrieval/hybrid/ranking.py`, **Phase-1 subset
//! only**. The Python function also threads `use_truth_weight` / `q_coords` /
//! `truth_state_by_id` / `current_truth_epoch` for the Phase-2 truth-subspace
//! boost; per the locked Phase-2 deferral those parameters are intentionally
//! absent here and will be added by the Phase-2 task without touching this
//! code path.
//!
//! # Divergence: the default fusion is no longer rank-only RRF
//!
//! Python fuses the chunk lane and the `TextSummary` lane with Reciprocal Rank
//! Fusion over **ranks only**; the similarities both lanes computed are thrown
//! away. RRF's constant `k` is calibrated for candidate lists of thousands, and
//! these lists are 20 and 5 long: with `k = 40` the whole chunk lane spans
//! `1/41 … 1/60`, so the difference between its best and worst hit is smaller
//! than the bonus for merely appearing in the other lane. The ranking that
//! results is "present in both lanes" first and "actually similar to the query"
//! second.
//!
//! Measured on Project Gutenberg's *Alice in Wonderland*, 15 probe questions
//! with the gold passage located by literal string match, scored on the passage
//! set the model is actually shown after the context budget fills — two
//! chunkings (76 × ~2 kB chunks with extractor-generated digests as summaries;
//! 138 × ~1 kB chunks with LLM summaries) × two summary-lane widths (5, and the
//! `chunks_top_k` default):
//!
//! | corpus / summary lane | MRR RRF → relative | gold-in-context RRF → relative |
//! |---|---|---|
//! | 76 chunks, digests, k=5  | 0.537 → 0.557 | 0.80 → 0.80 |
//! | 76 chunks, digests, k=10 | 0.488 → 0.528 | 0.67 → 0.80 |
//! | 138 chunks, summaries, k=5  | 0.422 → 0.478 | 0.80 → 0.87 |
//! | 138 chunks, summaries, k=10 | 0.350 → 0.471 | 0.80 → 0.80 |
//!
//! The concrete failure that started this: asked "Why does Alice follow the
//! White Rabbit", the chunk lane ranked the opening paragraph — the one that
//! says "burning with curiosity, she ran across the field after it" — second of
//! 76, and rank-only fusion pushed it to fifth, past the context budget, behind
//! three chunks the summary lane liked and the chunk lane had ranked 3rd, 8th
//! and 14th. No model ever answered from it. Under relative-score fusion it is
//! shown.
//!
//! Set `chunk_lane_fusion = "reciprocal_rank"` in `retriever_specific_config`
//! to get Python's ranking back verbatim.

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

/// Default weight of the summary lane in [`ChunkLaneFusion::RelativeScore`].
///
/// The two lanes are not equally informative. The chunk lane searches the
/// prose the answer has to be quoted from; the summary lane searches a
/// *derivative* of that same prose — an LLM summary, or on the extractor-only
/// path an entity-and-relation digest — and so contributes no evidence the
/// chunk lane could not also have found. Its vote is a second opinion on the
/// same documents, not an independent channel, and is weighted accordingly.
///
/// `0.75` is the centre of the plateau measured on the Alice corpus over two
/// chunkings (76 × ~2 kB with extractor digests, 138 × ~1 kB with LLM
/// summaries) and both summary-lane widths (5 and `chunks_top_k`): every
/// weight in `[0.6, 0.9]` behaves the same on the three probe questions, and
/// `0.75` is the only value that raised MRR in all four configurations without
/// lowering gold-passage-in-context in any of them. See the module docs.
pub(crate) const DEFAULT_SUMMARY_LANE_WEIGHT: f64 = 0.75;

/// How the chunk lane and the summary lane are fused into one ranking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ChunkLaneFusion {
    /// Reciprocal Rank Fusion over ranks only — Python's
    /// `rank_chunk_summary_pairs` verbatim. Kept as an opt-in so a deployment
    /// can reproduce the Python ranking byte for byte.
    ReciprocalRank,
    /// Per-lane min–max normalised similarity, summed with a lane weight.
    ///
    /// The default, because RRF's rank-only score is miscalibrated for lanes
    /// this short. With `k = rrf_k(10) = 40` and a 20-long chunk lane, the
    /// whole lane spans `1/41 … 1/60` — a 1.46× spread — while being present
    /// in the second lane at all is worth up to another 1.0×. Presence in both
    /// lanes therefore outranks similarity in either: on the Alice corpus a
    /// chunk the summary lane ranked first and the chunk lane ranked
    /// *fourteenth* beat the chunk lane's own second-best hit, which was the
    /// paragraph that introduces the protagonist. Classic RRF is calibrated
    /// for candidate lists of thousands, where the intra-lane spread dwarfs
    /// the cross-lane bonus; it degenerates at these lengths.
    ///
    /// Normalising each lane's own similarities to `[0, 1]` over its own
    /// candidates restores the missing information — *how much* better a hit
    /// is than its neighbours — without comparing raw scores across lanes,
    /// whose scales differ. (This is why it is not the max-of-raw-cosines
    /// fusion that was tried first and measured a dead zero against RRF: that
    /// variant compares two lanes' absolute similarities directly, so an
    /// offset between the lanes' score distributions decides the ranking. Here
    /// each lane is compared only with itself.)
    RelativeScore {
        /// Weight of the summary lane's normalised score in the sum. The chunk
        /// lane always weighs `1.0`; see [`DEFAULT_SUMMARY_LANE_WEIGHT`].
        summary_lane_weight: f64,
    },
}

impl Default for ChunkLaneFusion {
    fn default() -> Self {
        Self::RelativeScore {
            summary_lane_weight: DEFAULT_SUMMARY_LANE_WEIGHT,
        }
    }
}

/// Lowest and highest similarity a lane assigned across its own candidates.
type LaneBounds = Option<(f64, f64)>;

/// Min–max bounds of one lane's scores over the pairs it both ranked and
/// scored. `None` when the lane ranked nothing.
fn lane_bounds(
    pairs: &[ChunkSummaryPair],
    rank_of: impl Fn(&ChunkSummaryPair) -> Option<usize>,
    score_of: impl Fn(&ChunkSummaryPair) -> Option<f32>,
) -> LaneBounds {
    pairs
        .iter()
        .filter(|pair| rank_of(pair).is_some())
        .filter_map(score_of)
        .fold(None, |bounds, score| {
            let score = f64::from(score);
            Some(match bounds {
                None => (score, score),
                Some((low, high)) => (low.min(score), high.max(score)),
            })
        })
}

/// Whether every pair a lane ranked also carries that lane's score.
///
/// A vector adapter that does not report similarities must fall back to
/// rank-only fusion rather than silently rank its hits as the lane's worst.
fn lanes_are_scored(pairs: &[ChunkSummaryPair]) -> bool {
    pairs.iter().all(|pair| {
        (pair.vector_rank.is_none() || pair.vector_score.is_some())
            && (pair.summary_rank.is_none() || pair.summary_score.is_some())
    })
}

/// A lane score mapped onto `[0, 1]` against that lane's own spread.
///
/// A lane whose candidates are all equally similar (one hit, or an exact tie)
/// has no spread to normalise against; every candidate is then the lane's best
/// hit and scores `1.0`, which keeps a single-hit lane from being silently
/// discarded.
fn normalized(score: f64, (low, high): (f64, f64)) -> f64 {
    let span = high - low;
    if span <= f64::EPSILON {
        1.0
    } else {
        (score - low) / span
    }
}

/// Rank chunk↔summary pairs and truncate to `limit`.
///
/// Port of `rank_chunk_summary_pairs` (`ranking.py:7-48`), with the fusion
/// itself made a parameter — see [`ChunkLaneFusion`] for why the default is no
/// longer Python's rank-only RRF. For each pair carrying a `chunk`, collect the
/// present ranks from `(vector_rank, summary_rank)` (skip if none), compute the
/// fusion score, multiply by `importance_factor` when `use_importance_weight`,
/// then multiply by `truth_factor` when the truth-weight gate holds, and sort
/// by `(-final, -fusion, min_rank, chunk_id)` (float legs via `f64::total_cmp`,
/// per the locked total-ordering decision).
///
/// [`ChunkLaneFusion::RelativeScore`] needs a similarity on every ranked pair;
/// where the vector adapter reported none, the call falls back to
/// [`ChunkLaneFusion::ReciprocalRank`] for the whole ranking so the two lanes
/// are never mixed under different rules.
///
/// The truth-subspace boost (`ranking.py:39-44`) is applied strictly AFTER the
/// importance factor and only when `use_truth_weight`, `q_coords` is non-empty,
/// an epoch is known, and the chunk's stored `truth_epoch` matches that current
/// epoch. `NodeTruthState.truth_epoch` is a bare `i64` (with `-1` as the
/// "never scored" sentinel), so a stale/sentinel epoch or a chunk id missing
/// from the map both fall through to no multiplier — identical to Python's
/// `None`-vs-int comparison. When the gate is false the `final_score` is exactly
/// the unboosted value, so default-off (`use_truth_weight == false`) ranking is
/// byte-identical to a call with no truth context.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rank_chunk_summary_pairs(
    pairs: Vec<ChunkSummaryPair>,
    limit: usize,
    fusion: ChunkLaneFusion,
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
    // `Some(..)` selects relative-score fusion and carries what it needs: each
    // lane's own spread, plus the summary lane's weight. `None` is rank-only
    // RRF — either because it was asked for, or because a lane reported hits
    // with no similarity attached.
    let relative: Option<(LaneBounds, LaneBounds, f64)> = match fusion {
        ChunkLaneFusion::ReciprocalRank => None,
        ChunkLaneFusion::RelativeScore {
            summary_lane_weight,
        } if lanes_are_scored(&pairs) => Some((
            lane_bounds(&pairs, |pair| pair.vector_rank, |pair| pair.vector_score),
            lane_bounds(&pairs, |pair| pair.summary_rank, |pair| pair.summary_score),
            summary_lane_weight,
        )),
        ChunkLaneFusion::RelativeScore { .. } => {
            tracing::debug!(
                "a hybrid chunk lane reported hits with no similarity score; \
                 falling back to reciprocal-rank fusion"
            );
            None
        }
    };
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

        let fusion_score: f64 = match relative {
            None => ranks.iter().map(|rank| 1.0 / (k + rank + 1) as f64).sum(),
            Some((chunk_bounds, summary_bounds, summary_lane_weight)) => {
                // A lane the pair is absent from contributes nothing; a lane's
                // own worst hit contributes nothing either, which is what makes
                // this a *relative* score.
                let chunk_part = match (pair.vector_score, chunk_bounds) {
                    (Some(score), Some(bounds)) => normalized(f64::from(score), bounds),
                    _ => 0.0,
                };
                let summary_part = match (pair.summary_score, summary_bounds) {
                    (Some(score), Some(bounds)) => normalized(f64::from(score), bounds),
                    _ => 0.0,
                };
                chunk_part + summary_lane_weight * summary_part
            }
        };
        let mut final_score = if use_importance_weight {
            fusion_score * importance_factor(payload(chunk))
        } else {
            fusion_score
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

        ranked.push((final_score, fusion_score, min_rank, chunk_id, pair));
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
            vector_score: None,
            summary_rank: summary,
            summary_score: None,
        }
    }

    /// Like [`pair`] but with the lane similarities relative-score fusion needs.
    fn scored_pair(
        id: &str,
        vector: Option<(usize, f32)>,
        summary: Option<(usize, f32)>,
    ) -> ChunkSummaryPair {
        ChunkSummaryPair {
            vector_rank: vector.map(|(rank, _)| rank),
            vector_score: vector.map(|(_, score)| score),
            summary_rank: summary.map(|(rank, _)| rank),
            summary_score: summary.map(|(_, score)| score),
            ..pair(id, None, None, None)
        }
    }

    /// Relative-score fusion at the default weight.
    fn relative(pairs: Vec<ChunkSummaryPair>, limit: usize) -> Vec<ChunkSummaryPair> {
        rank_chunk_summary_pairs(
            pairs,
            limit,
            ChunkLaneFusion::default(),
            false,
            false,
            None,
            None,
            None,
        )
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
        rank_chunk_summary_pairs(
            pairs,
            limit,
            ChunkLaneFusion::ReciprocalRank,
            use_importance_weight,
            false,
            None,
            None,
            None,
        )
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
            ChunkLaneFusion::ReciprocalRank,
            true,
            false, // use_truth_weight OFF
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        // No truth context at all.
        let no_ctx = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
            true,
            false,
            None,
            None,
            None,
        );

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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
        let base = rank_chunk_summary_pairs(
            vec![b.clone(), a.clone()],
            5,
            ChunkLaneFusion::ReciprocalRank,
            true,
            false,
            None,
            None,
            None,
        );
        assert_eq!(base[0].chunk_id.as_deref(), Some("a"));
        assert_eq!(base[1].chunk_id.as_deref(), Some("b"));

        // With truth on, "a"'s 1.25 factor beats "b"'s 1.0 factor.
        let ranked = rank_chunk_summary_pairs(
            vec![b, a],
            5,
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
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
            ChunkLaneFusion::ReciprocalRank,
            true,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(ids(&both), boost_first);

        // Importance only (truth off): 1.25 alone is not enough -> plain first.
        let imp_only = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
            true,
            false,
            None,
            None,
            None,
        );
        assert_eq!(ids(&imp_only), plain_first);

        // Truth only (importance off): 1.25 alone is not enough -> plain first.
        let truth_only = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
            false,
            true,
            Some(&q_coords),
            Some(&states),
            Some(epoch),
        );
        assert_eq!(ids(&truth_only), plain_first);

        // Neither factor: pure rrf -> plain first.
        let neither = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
            false,
            false,
            None,
            None,
            None,
        );
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
        let base = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
            false,
            false,
            None,
            None,
            None,
        );
        assert_eq!(
            ids(&base),
            vec![Some("stale".to_string()), Some("current".to_string())]
        );

        // Truth ON: only `current` (matching epoch) is boosted, overtaking stale.
        let ranked = rank_chunk_summary_pairs(
            mk(),
            5,
            ChunkLaneFusion::ReciprocalRank,
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

    /// The regression this fusion exists for, modelled on the measured lanes
    /// of "Why does Alice follow the White Rabbit": the chunk lane's top hits
    /// are near-tied, the summary lane likes the third of them, and it also
    /// likes a chunk the chunk lane put fifth.
    ///
    /// Rank-only RRF makes presence in both lanes worth more than similarity in
    /// either, so `weak-chunk` — the chunk lane's fifth — lands ahead of its
    /// first and second, and the gold passage falls out of the top three.
    /// Relative-score fusion scales each lane's vote by how much better its hit
    /// is than that lane's own other hits, so `weak-chunk` is still lifted (it
    /// ends ahead of the equally-ranked `d`, which has no summary hit) but no
    /// longer displaces the head of the chunk lane.
    #[test]
    fn a_weak_chunk_no_longer_rides_the_summary_lane_past_the_best_chunks() {
        let pairs = || {
            vec![
                scored_pair("best-chunk", Some((0, 0.712)), None),
                scored_pair("gold", Some((1, 0.709)), None),
                scored_pair("also-summarised", Some((2, 0.697)), Some((0, 0.708))),
                scored_pair("d", Some((3, 0.690)), None),
                scored_pair("weak-chunk", Some((4, 0.640)), Some((1, 0.698))),
                scored_pair("f", Some((5, 0.600)), None),
                scored_pair("x", None, Some((2, 0.683))),
                scored_pair("y", None, Some((3, 0.671))),
                scored_pair("z", None, Some((4, 0.662))),
            ]
        };
        let names = |ranked: &[ChunkSummaryPair]| {
            ranked
                .iter()
                .map(|pair| pair.chunk_id.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        };

        let by_rank = rank_chunk_summary_pairs(
            pairs(),
            9,
            ChunkLaneFusion::ReciprocalRank,
            false,
            false,
            None,
            None,
            None,
        );
        assert_eq!(
            names(&by_rank),
            [
                "also-summarised",
                "weak-chunk",
                "best-chunk",
                "gold",
                "x",
                "d",
                "y",
                "z",
                "f"
            ]
        );

        assert_eq!(
            names(&relative(pairs(), 9)),
            [
                "also-summarised",
                "best-chunk",
                "gold",
                "weak-chunk",
                "d",
                "x",
                "y",
                "z",
                "f"
            ]
        );
    }

    #[test]
    fn the_summary_lane_still_breaks_a_chunk_lane_near_tie() {
        // Two chunks the chunk lane cannot separate against the spread of its
        // own candidates; only one of them is summarised.
        let pairs = vec![
            scored_pair("unsummarised", Some((0, 0.700)), None),
            scored_pair("summarised", Some((1, 0.699)), Some((0, 0.9))),
            scored_pair("c", Some((2, 0.650)), Some((1, 0.5))),
            scored_pair("d", Some((3, 0.620)), None),
            scored_pair("e", Some((4, 0.600)), None),
        ];
        assert_eq!(
            ids(&relative(pairs, 5))[0],
            Some("summarised".to_string()),
            "a lane that adds information must still be able to reorder a tie"
        );
    }

    #[test]
    fn a_lane_weight_of_zero_is_the_chunk_lane_alone() {
        let pairs = vec![
            scored_pair("chunk-best", Some((0, 0.70)), None),
            scored_pair("summary-best", Some((5, 0.60)), Some((0, 0.99))),
        ];
        let ranked = rank_chunk_summary_pairs(
            pairs,
            2,
            ChunkLaneFusion::RelativeScore {
                summary_lane_weight: 0.0,
            },
            false,
            false,
            None,
            None,
            None,
        );
        assert_eq!(ids(&ranked)[0], Some("chunk-best".to_string()));
    }

    /// A lane whose hits carry no similarity cannot be normalised, and ranking
    /// them all as that lane's worst hit would silently bury them. The whole
    /// call falls back to rank-only fusion instead.
    #[test]
    fn unscored_lanes_fall_back_to_reciprocal_rank() {
        let unscored = || {
            vec![
                pair("two", Some(1), Some(1), None),
                pair("one", Some(0), None, None),
            ]
        };
        assert_eq!(
            ids(&relative(unscored(), 5)),
            ids(&baseline(unscored(), 5, false))
        );
    }

    /// A single-hit lane has no spread to normalise against; its one hit is
    /// that lane's best and must score as such, not as its worst.
    #[test]
    fn a_single_hit_lane_scores_full_marks() {
        assert_eq!(normalized(0.4, (0.4, 0.4)), 1.0);
        let pairs = vec![
            scored_pair("only-chunk", Some((0, 0.5)), None),
            scored_pair("only-summary", None, Some((0, 0.5))),
        ];
        // Both lanes hold exactly one hit, so both normalise to 1.0 and the
        // summary lane's weight decides — it is the weaker lane, so it loses.
        assert_eq!(ids(&relative(pairs, 2))[0], Some("only-chunk".to_string()));
    }

    #[test]
    fn the_default_fusion_is_relative_score_at_the_documented_weight() {
        assert_eq!(
            ChunkLaneFusion::default(),
            ChunkLaneFusion::RelativeScore {
                summary_lane_weight: DEFAULT_SUMMARY_LANE_WEIGHT
            }
        );
        assert_eq!(DEFAULT_SUMMARY_LANE_WEIGHT, 0.75);
    }
}
