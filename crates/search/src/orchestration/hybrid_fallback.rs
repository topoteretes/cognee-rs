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
///
/// `default_feedback_influence` is the orchestrator's (Python's
/// `default_feedback_influence`), which an unset `feedback_influence` takes:
/// a positive default defers hybrid to graph completion as an explicit one
/// does.
pub(crate) fn requested_graph_only_filter(
    request: &SearchRequest,
    default_feedback_influence: f32,
) -> Option<&'static str> {
    if request.node_type.as_deref().is_some_and(|t| t != "NodeSet") {
        return Some("node_type");
    }
    if request.neighborhood_depth.is_some() {
        return Some("neighborhood_depth");
    }
    if request
        .feedback_influence
        .unwrap_or(default_feedback_influence)
        > 0.0
    {
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
                requested_graph_only_filter(&request(extra), 0.0),
                expected,
                "{extra}"
            );
        }
    }

    // A positive orchestrator default is the influence of a request that sets
    // none, so hybrid defers to graph completion; an explicit 0 opts out.
    #[test]
    fn a_default_feedback_influence_defers_hybrid() {
        assert_eq!(
            requested_graph_only_filter(&request(""), 0.4),
            Some("feedback_influence")
        );
        assert_eq!(
            requested_graph_only_filter(&request(r#", "feedback_influence": 0.0"#), 0.4),
            None
        );
    }
}
