/// Blend a similarity score with feedback weight.
///
/// Rust uses cosine similarity (higher is better), unlike Python which uses cosine distance.
/// Blending formula adapted for similarity space [-1, 1]:
///   normalized_sim = (score + 1.0) / 2.0     -- map [-1,1] to [0,1]
///   blended = (1 - fi) * normalized_sim + fi * feedback_weight
///   result  = blended * 2.0 - 1.0             -- map back to [-1,1]
fn effective_score(score: f32, feedback_weight: f32, feedback_influence: f32) -> f32 {
    if feedback_influence <= 0.0 {
        return score;
    }
    let fw = feedback_weight.clamp(0.0, 1.0);
    let normalized_sim = (score + 1.0) / 2.0;
    let blended = (1.0 - feedback_influence) * normalized_sim + feedback_influence * fw;
    blended * 2.0 - 1.0
}

/// Computes the total distance for a triplet (source_node, edge, target_node).
///
/// Each component is a cosine distance (lower = better). The total distance is
/// the sum of all three, matching Python's `_calculate_query_top_triplet_importances`.
///
/// When `feedback_influence` is non-zero, each node's similarity score is blended
/// with its `feedback_weight` before summing.
pub fn rank_edge_score(
    source_distance: f32,
    target_distance: f32,
    edge_distance: f32,
    feedback_influence: f32,
    source_feedback_weight: f32,
    target_feedback_weight: f32,
) -> f32 {
    let s = effective_score(source_distance, source_feedback_weight, feedback_influence);
    let t = effective_score(target_distance, target_feedback_weight, feedback_influence);
    s + t + edge_distance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_three_distance_components() {
        let score = rank_edge_score(0.1, 0.2, 0.3, 0.0, 0.5, 0.5);
        assert!((score - 0.6).abs() < 1e-6, "expected 0.6, got {score}");
    }

    #[test]
    fn unmatched_components_use_penalty() {
        // When all three components equal the default penalty (3.5), total is 10.5
        let score = rank_edge_score(3.5, 3.5, 3.5, 0.0, 0.5, 0.5);
        assert!((score - 10.5).abs() < 1e-6, "expected 10.5, got {score}");
    }

    #[test]
    fn effective_score_zero_influence_returns_input() {
        // with feedback_influence = 0.0, output == input regardless of feedback_weight
        assert_eq!(effective_score(0.5, 0.8, 0.0), 0.5);
        assert_eq!(effective_score(-0.3, 0.2, 0.0), -0.3);
    }

    #[test]
    fn effective_score_full_influence_returns_feedback_based() {
        // with feedback_influence = 1.0, output is purely based on feedback_weight
        // normalized_sim irrelevant, blended = fw, result = fw * 2 - 1
        let result = effective_score(0.0, 1.0, 1.0); // fw=1.0 -> blended=1.0 -> result=1.0
        assert!((result - 1.0).abs() < 1e-6);
        let result2 = effective_score(0.0, 0.0, 1.0); // fw=0.0 -> blended=0.0 -> result=-1.0
        assert!((result2 - (-1.0)).abs() < 1e-6);
    }
}
