use crate::types::{SearchRequest, SearchType};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

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

type TurnKey = (String, String);
static TURN_LOCKS: OnceLock<Mutex<HashMap<TurnKey, Arc<Mutex<()>>>>> = OnceLock::new();

/// Serialize turns for one cache identity while allowing unrelated sessions
/// to run independently. This has the same single-process scope as Python's
/// `session_turn_lock`.
pub(crate) async fn lock(request: &SearchRequest) -> OwnedMutexGuard<()> {
    let key = (
        request
            .user_id
            .expect("eligibility requires a user")
            .to_string(),
        request
            .session_id
            .clone()
            .expect("eligibility requires a session"),
    );
    let mutex = {
        let mut locks = TURN_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .await;
        Arc::clone(locks.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
    };
    mutex.lock_owned().await
}

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
            summarize_context: None,
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
            &request(SearchType::TripletCompletion),
            SearchType::TripletCompletion,
            false
        ));
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

    #[tokio::test]
    async fn same_identity_is_serialized_but_different_sessions_are_not() {
        use std::time::Duration;

        let first = request(SearchType::RagCompletion);
        let same = first.clone();
        let mut other = first.clone();
        other.session_id = Some("other".into());

        let held = lock(&first).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lock(&same))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lock(&other))
                .await
                .is_ok()
        );
        drop(held);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lock(&same))
                .await
                .is_ok()
        );
    }
}
