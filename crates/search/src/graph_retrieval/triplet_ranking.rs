/// Computes the total distance for a triplet (source_node, edge, target_node).
///
/// Each component is a cosine distance (lower = better). The total distance is
/// the sum of all three, matching Python's `_calculate_query_top_triplet_importances`.
pub fn rank_edge_score(source_distance: f32, target_distance: f32, edge_distance: f32) -> f32 {
    source_distance + target_distance + edge_distance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_three_distance_components() {
        let score = rank_edge_score(0.1, 0.2, 0.3);
        assert!((score - 0.6).abs() < 1e-6, "expected 0.6, got {score}");
    }

    #[test]
    fn unmatched_components_use_penalty() {
        // When all three components equal the default penalty (3.5), total is 10.5
        let score = rank_edge_score(3.5, 3.5, 3.5);
        assert!((score - 10.5).abs() < 1e-6, "expected 10.5, got {score}");
    }
}
