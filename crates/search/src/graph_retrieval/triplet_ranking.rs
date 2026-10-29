//! Triplet scoring — a port of Python's
//! `CogneeGraph._calculate_query_top_triplet_importances` (`score` and its
//! `_effective_distance` helper, `CogneeGraph.py` @ dev `c91cdff`).
//!
//! A triplet's score is the sum over its three elements (source node, target
//! node, edge) of each element's cosine distance to the query, first scaled by
//! the element's `importance_weight`, then blended with its `feedback_weight`
//! and finally scaled by the requesting user's `prefers` weight (Python's
//! `_personal_distance`). Lower is better.

use serde_json::Value;

/// The `importance_weight` / `feedback_weight` Python uses when an element has
/// none, or one `float()` rejects (`DataPoint`'s own default for both).
pub const DEFAULT_ELEMENT_WEIGHT: f32 = 0.5;

/// One of a triplet's three elements — source node, target node or edge — as
/// the ranking sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TripletElement {
    /// Cosine distance to the query (lower = better), or the triplet distance
    /// penalty when the element had no vector match.
    pub distance: f32,
    /// `importance_weight`: the distance is multiplied by `2 - w` before
    /// feedback blending, so `1.0` keeps it, the `0.5` default scales it by
    /// 1.5 and `0.0` doubles it. Edges never carry one in Python's
    /// projection, so theirs is always the default.
    ///
    /// Not clamped, exactly as in Python, which has two consequences for a
    /// weight outside `[0, 1]`: above about 1.69 the *scaled* penalty drops
    /// into `[0, 2]` and is feedback-blended like a real match (see
    /// [`rank_edge_score`]), and above 2 the factor turns negative, so every
    /// distance — the penalty included — becomes a large negative score and
    /// the element's triplets rank first whatever the query. Cognify only
    /// writes weights in `[0, 1]`; anything else comes from a hand-edited or
    /// foreign graph.
    pub importance_weight: f32,
    /// `feedback_weight`, clamped to `[0, 1]` when blended. Ignored when the
    /// feedback influence is not positive.
    pub feedback_weight: f32,
    /// The requesting user's effective `prefers` weight on this node (Python's
    /// `personal_weight` attribute); `None` for an edge or an unrated node.
    pub personal_weight: Option<f32>,
}

impl TripletElement {
    /// An element at `distance` with the default importance and feedback.
    pub fn new(distance: f32) -> Self {
        Self {
            distance,
            importance_weight: DEFAULT_ELEMENT_WEIGHT,
            feedback_weight: DEFAULT_ELEMENT_WEIGHT,
            personal_weight: None,
        }
    }
}

/// The score of a triplet (lower = better match): the sum of its three
/// elements' importance-scaled, feedback-blended distances.
///
/// `distance_penalty` is the configured triplet distance penalty. Like Python,
/// the blend's guard tests the *importance-scaled* distance: a scaled distance
/// at or above the penalty, or outside `[0, 2]`, is not blended. At the
/// default importance an unmatched element scales to `1.5 × penalty` and so is
/// never blended, keeping missing elements below real matches; an importance
/// weight above about 1.69 scales the default 6.5 penalty to 2 or less, and
/// that element *is* blended (see [`TripletElement::importance_weight`]).
///
/// `personal_influence` scales an element with a
/// [`TripletElement::personal_weight`] (see [`personalize`]).
pub fn rank_edge_score(
    source: TripletElement,
    target: TripletElement,
    edge: TripletElement,
    feedback_influence: f32,
    personal_influence: f32,
    distance_penalty: f32,
) -> f32 {
    [source, target, edge]
        .into_iter()
        .map(|element| {
            element_distance(
                element,
                feedback_influence,
                personal_influence,
                distance_penalty,
            )
        })
        .sum()
}

fn element_distance(
    element: TripletElement,
    feedback_influence: f32,
    personal_influence: f32,
    distance_penalty: f32,
) -> f32 {
    let distance = (2.0 - element.importance_weight) * element.distance;
    let blended = blend_feedback(
        distance,
        element.feedback_weight,
        feedback_influence,
        distance_penalty,
    );
    personalize(
        distance,
        blended,
        element.personal_weight,
        personal_influence,
        distance_penalty,
    )
}

/// Python's `_personal_distance`: scale the feedback-blended distance by the
/// user's `personal_factor` (distance space: a preferred element's distance
/// shrinks). The *raw* importance-scaled distance decides eligibility, the
/// same `[0, 2]` / penalty test the feedback blend applies, so a fallback
/// penalty is never scaled like a real match.
fn personalize(
    raw: f32,
    blended: f32,
    personal_weight: Option<f32>,
    personal_influence: f32,
    distance_penalty: f32,
) -> f32 {
    if personal_influence <= 0.0 || raw >= distance_penalty || !(0.0..=2.0).contains(&raw) {
        return blended;
    }
    match personal_weight {
        Some(weight) => blended * (1.0 - personal_influence * (2.0 * weight - 1.0)),
        None => blended,
    }
}

/// Python's `_effective_distance`: blend a cosine distance with a feedback
/// weight in normalised space (`[0, 2]` → `[0, 1]`), then project back.
///
/// Only real cosine distances in `[0, 2]` are blended; the penalty and
/// out-of-range values — including an importance-scaled distance pushed past
/// 2 — pass through unchanged. A high feedback weight lowers the distance.
fn blend_feedback(
    distance: f32,
    feedback_weight: f32,
    feedback_influence: f32,
    distance_penalty: f32,
) -> f32 {
    if feedback_influence <= 0.0 {
        return distance;
    }
    if distance >= distance_penalty || !(0.0..=2.0).contains(&distance) {
        return distance;
    }
    // `max(0.0, min(1.0, w))`: Python's `min` keeps 1.0 against a NaN.
    let feedback_weight = if feedback_weight.is_nan() {
        1.0
    } else {
        feedback_weight.clamp(0.0, 1.0)
    };
    let normalized_distance = distance / 2.0;
    let blended = (1.0 - feedback_influence) * normalized_distance
        + feedback_influence * (1.0 - feedback_weight);
    blended * 2.0
}

/// A node or edge weight property read the way Python's `float(value)` with
/// an `except (TypeError, ValueError): 0.5` fallback reads it: a number, a
/// boolean (`True` is 1.0), or a numeric string (see [`parse_python_float`]);
/// anything else — including a missing or null property — is
/// [`DEFAULT_ELEMENT_WEIGHT`].
pub(crate) fn element_weight(value: Option<&Value>) -> f32 {
    let parsed = match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::Bool(flag)) => Some(if *flag { 1.0 } else { 0.0 }),
        Some(Value::String(text)) => parse_python_float(text),
        _ => None,
    };
    parsed.map_or(DEFAULT_ELEMENT_WEIGHT, |weight| weight as f32)
}

/// Python's `float(str)`: surrounding whitespace is ignored and an underscore
/// may group digits (`"1_0"` is 10.0), but only *between* two digits — a
/// leading, trailing or doubled underscore, or one next to the point or
/// exponent, is rejected. The rest (sign, `inf` / `nan` in any case, exponent)
/// is what Rust's own float parser accepts.
fn parse_python_float(text: &str) -> Option<f64> {
    let text = text.trim();
    if !text.contains('_') {
        return text.parse().ok();
    }
    let bytes = text.as_bytes();
    let grouped_digits = bytes.iter().enumerate().all(|(i, &b)| {
        b != b'_'
            || (i > 0
                && bytes[i - 1].is_ascii_digit()
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
    });
    if !grouped_digits {
        return None;
    }
    text.replace('_', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const PENALTY: f32 = 6.5;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "expected {expected}, got {actual}"
        );
    }

    fn element(distance: f32, importance_weight: f32, feedback_weight: f32) -> TripletElement {
        TripletElement {
            distance,
            importance_weight,
            feedback_weight,
            personal_weight: None,
        }
    }

    #[test]
    fn default_weights_scale_every_distance_by_one_and_a_half() {
        let score = rank_edge_score(
            TripletElement::new(0.1),
            TripletElement::new(0.2),
            TripletElement::new(0.3),
            0.0,
            0.0,
            PENALTY,
        );
        assert_close(score, 1.5 * 0.6);
    }

    #[test]
    fn unmatched_elements_take_the_scaled_penalty() {
        let unmatched = TripletElement::new(PENALTY);
        let score = rank_edge_score(unmatched, unmatched, unmatched, 0.0, 0.0, PENALTY);
        assert_close(score, 3.0 * 1.5 * PENALTY);
    }

    /// Equal distances: the more important triplet ranks first.
    #[test]
    fn higher_importance_ranks_first() {
        let important = rank_edge_score(
            element(0.4, 1.0, 0.5),
            element(0.4, 1.0, 0.5),
            TripletElement::new(PENALTY),
            0.0,
            0.0,
            PENALTY,
        );
        let unimportant = rank_edge_score(
            element(0.4, 0.0, 0.5),
            element(0.4, 0.0, 0.5),
            TripletElement::new(PENALTY),
            0.0,
            0.0,
            PENALTY,
        );
        assert!(important < unimportant, "{important} vs {unimportant}");
        assert_close(important, 0.4 + 0.4 + 1.5 * PENALTY);
        assert_close(unimportant, 0.8 + 0.8 + 1.5 * PENALTY);
    }

    /// The regression this port fixes: positive feedback must *lower* the
    /// distance. The old similarity-space blend raised it.
    #[test]
    fn positive_feedback_lowers_the_distance() {
        let liked = element_distance(element(0.4, 1.0, 1.0), 0.5, 0.0, PENALTY);
        let neutral = element_distance(element(0.4, 1.0, 0.5), 0.5, 0.0, PENALTY);
        let disliked = element_distance(element(0.4, 1.0, 0.0), 0.5, 0.0, PENALTY);
        // ((1 - 0.5) * 0.2 + 0.5 * (1 - w)) * 2
        assert_close(liked, 0.2);
        assert_close(neutral, 0.7);
        assert_close(disliked, 1.2);
    }

    #[test]
    fn full_influence_is_pure_feedback() {
        assert_close(blend_feedback(0.3, 1.0, 1.0, PENALTY), 0.0);
        assert_close(blend_feedback(0.3, 0.0, 1.0, PENALTY), 2.0);
    }

    #[test]
    fn zero_influence_ignores_feedback() {
        assert_eq!(blend_feedback(0.3, 1.0, 0.0, PENALTY), 0.3);
    }

    /// The penalty and anything outside `[0, 2]` — including a real distance
    /// pushed past 2 by importance scaling — are not blended.
    #[test]
    fn out_of_range_distances_are_not_blended() {
        assert_eq!(blend_feedback(PENALTY, 1.0, 0.5, PENALTY), PENALTY);
        assert_eq!(blend_feedback(-0.1, 1.0, 0.5, PENALTY), -0.1);
        // 1.5 * 1.5 = 2.25 > 2: Python leaves it as is.
        assert_close(
            element_distance(element(1.5, 0.5, 1.0), 0.5, 0.0, PENALTY),
            2.25,
        );
    }

    #[test]
    fn feedback_weight_is_clamped_and_nan_reads_as_one() {
        assert_close(
            blend_feedback(0.4, 3.0, 0.5, PENALTY),
            blend_feedback(0.4, 1.0, 0.5, PENALTY),
        );
        assert_close(
            blend_feedback(0.4, -2.0, 0.5, PENALTY),
            blend_feedback(0.4, 0.0, 0.5, PENALTY),
        );
        assert_close(
            blend_feedback(0.4, f32::NAN, 0.5, PENALTY),
            blend_feedback(0.4, 1.0, 0.5, PENALTY),
        );
    }

    /// Underscores group digits only between two digits, as in Python.
    #[test]
    fn python_float_underscore_rules() {
        for (text, expected) in [
            ("1_000", Some(1000.0)),
            ("1_0.2_5", Some(10.25)),
            ("1e1_0", Some(1e10)),
            ("_1", None),
            ("1_", None),
            ("1__0", None),
            ("1_.5", None),
            ("1._5", None),
            ("1_e5", None),
            ("-inf", Some(f64::NEG_INFINITY)),
            ("  .5  ", Some(0.5)),
        ] {
            assert_eq!(parse_python_float(text), expected, "{text:?}");
        }
        assert!(parse_python_float("NaN").is_some_and(f64::is_nan));
    }

    /// An importance weight above ~1.69 scales the penalty into `[0, 2]`, so —
    /// as in Python — it is feedback-blended like a real match.
    #[test]
    fn high_importance_lets_the_penalty_be_blended() {
        let unmatched = element(PENALTY, 1.9, 1.0);
        // (2 - 1.9) * 6.5 = 0.65, fully blended towards w = 1.0 -> 0.0.
        assert_close(element_distance(unmatched, 1.0, 0.0, PENALTY), 0.0);
        // Above 2 the factor is negative: the penalty becomes a reward.
        assert_close(
            element_distance(element(PENALTY, 5.0, 0.5), 0.0, 0.0, PENALTY),
            -3.0 * PENALTY,
        );
    }

    // Python `_personal_distance`: a liked node's distance shrinks, a
    // disliked one's grows, a neutral one's is untouched, and the penalty is
    // never scaled.
    #[test]
    fn a_personal_weight_scales_real_distances_only() {
        let with = |distance: f32, weight: Option<f32>| TripletElement {
            personal_weight: weight,
            ..element(distance, 1.0, 0.5)
        };
        assert_close(
            element_distance(with(0.4, Some(1.0)), 0.0, 0.3, PENALTY),
            0.4 * 0.7,
        );
        assert_close(
            element_distance(with(0.4, Some(0.0)), 0.0, 0.3, PENALTY),
            0.4 * 1.3,
        );
        assert_close(
            element_distance(with(0.4, Some(0.5)), 0.0, 0.3, PENALTY),
            0.4,
        );
        assert_close(element_distance(with(0.4, None), 0.0, 0.3, PENALTY), 0.4);
        assert_close(
            element_distance(with(0.4, Some(1.0)), 0.0, 0.0, PENALTY),
            0.4,
        );
        assert_close(
            element_distance(with(PENALTY, Some(1.0)), 0.0, 0.3, PENALTY),
            PENALTY,
        );
        // Applied on top of the feedback blend: ((0.5 * 0.2 + 0.5 * 0) * 2) * 0.7.
        let blended = TripletElement {
            personal_weight: Some(1.0),
            ..element(0.4, 1.0, 1.0)
        };
        assert_close(element_distance(blended, 0.5, 0.3, PENALTY), 0.2 * 0.7);
    }

    #[test]
    fn element_weight_reads_like_python_float() {
        assert_eq!(element_weight(Some(&json!(0.75))), 0.75);
        assert_eq!(element_weight(Some(&json!(1))), 1.0);
        assert_eq!(element_weight(Some(&json!(true))), 1.0);
        assert_eq!(element_weight(Some(&json!(" 0.25 "))), 0.25);
        assert_eq!(element_weight(Some(&json!("1_0"))), 10.0);
        assert_eq!(element_weight(Some(&json!("0.2_5"))), 0.25);
        assert_eq!(element_weight(Some(&json!("heavy"))), 0.5);
        assert_eq!(element_weight(Some(&json!(null))), 0.5);
        assert_eq!(element_weight(Some(&json!([1.0]))), 0.5);
        assert_eq!(element_weight(None), 0.5);
    }
}
