use crate::types::SearchRequest;

/// The first set option that only graph completion honours, which hybrid rejects.
pub(crate) fn passed_graph_only_knob(request: &SearchRequest) -> Option<&'static str> {
    if request.wide_search_top_k.is_some() {
        return Some("wide_search_top_k");
    }
    if request.triplet_distance_penalty.is_some() {
        return Some("triplet_distance_penalty");
    }
    None
}

/// The first set filter that only graph completion supports, which makes hybrid fall back.
pub(crate) fn requested_graph_only_filter(request: &SearchRequest) -> Option<&'static str> {
    if request.node_type.as_deref().is_some_and(|t| t != "NodeSet") {
        return Some("node_type");
    }
    if request.neighborhood_depth.is_some() {
        return Some("neighborhood_depth");
    }
    if request.feedback_influence_or_default() > 0.0 {
        return Some("feedback_influence");
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn request(extra: &str) -> SearchRequest {
        serde_json::from_str(&format!(r#"{{"query_text": "q"{extra}}}"#)).unwrap()
    }

    #[test]
    fn passed_graph_only_knob_names_the_set_knob() {
        assert_eq!(passed_graph_only_knob(&request("")), None);
        let wide = request(r#", "wide_search_top_k": 100"#);
        assert_eq!(passed_graph_only_knob(&wide), Some("wide_search_top_k"));
        let penalty = request(r#", "triplet_distance_penalty": 3.5"#);
        assert_eq!(
            passed_graph_only_knob(&penalty),
            Some("triplet_distance_penalty")
        );
    }

    #[test]
    fn requested_graph_only_filter_names_the_fallback_trigger() {
        let cases = [
            ("", None),
            (r#", "node_type": "NodeSet""#, None),
            (r#", "feedback_influence": 0.0"#, None),
            (r#", "node_type": "Entity""#, Some("node_type")),
            (r#", "neighborhood_depth": 2"#, Some("neighborhood_depth")),
            (r#", "feedback_influence": 0.5"#, Some("feedback_influence")),
        ];
        for (extra, expected) in cases {
            assert_eq!(
                requested_graph_only_filter(&request(extra)),
                expected,
                "{extra}"
            );
        }
    }
}
