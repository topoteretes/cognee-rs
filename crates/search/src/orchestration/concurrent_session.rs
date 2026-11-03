use crate::types::{SearchRequest, SearchType};

/// A spawned analysis task that cannot outlive its owning request future.
pub(crate) struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    pub(crate) fn abort(mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }

    pub(crate) async fn join(mut self) -> Option<Result<T, tokio::task::JoinError>> {
        let handle = self.0.take()?;
        Some(handle.await)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// Execution order for a session-aware completion turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSearchMode {
    Sequential,
    #[default]
    Concurrent,
}

/// Python deliberately opts concrete retrievers in. Rust's registry erases the
/// concrete type, so the equivalent boundary is a built-in (non-community)
/// request whose resolved type is one of the three SDK-1045 retrievers.
pub(crate) fn eligible(request: &SearchRequest, resolved_type: SearchType, is_batch: bool) -> bool {
    request.custom_search_type.is_none()
        && request.search_type != SearchType::FeelingLucky
        && request.session_id.as_ref().is_some_and(|id| !id.is_empty())
        && request.user_id.is_some()
        && !is_batch
        && !request.only_context()
        && matches!(
            resolved_type,
            SearchType::RagCompletion | SearchType::GraphCompletion | SearchType::HybridCompletion
        )
}

/// How many prior Q&A turns a concurrent turn reads: the previous turn for the
/// analysis and the context of the conversational retrieval lane (Python
/// `get_session(last_n=2)`).
pub(crate) const RECENT_QA_TURNS: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn request(search_type: SearchType) -> SearchRequest {
        SearchRequest {
            query_text: "q".into(),
            search_type,
            top_k: None,
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(false),
            use_combined_context: None,
            session_id: Some("s".into()),
            node_type: None,
            node_name: None,
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
        }
    }

    #[test]
    fn exact_eligibility_matrix() {
        for ty in [
            SearchType::RagCompletion,
            SearchType::GraphCompletion,
            SearchType::HybridCompletion,
        ] {
            assert!(eligible(&request(ty), ty, false));
        }
        assert!(!eligible(
            &request(SearchType::FeelingLucky),
            SearchType::GraphCompletion,
            false
        ));
        assert!(!eligible(
            &request(SearchType::RagCompletion),
            SearchType::RagCompletion,
            true
        ));
        let mut r = request(SearchType::RagCompletion);
        r.only_context = Some(true);
        assert!(!eligible(&r, SearchType::RagCompletion, false));
        let mut r = request(SearchType::RagCompletion);
        r.custom_search_type = Some("subclass".into());
        assert!(!eligible(&r, SearchType::RagCompletion, false));
    }
}
