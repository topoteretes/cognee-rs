use cognee_vector::SearchResult;

use crate::types::{SearchContext, SearchError, SearchItem};

pub(crate) fn search_results_to_context(
    results: Vec<SearchResult>,
) -> Result<SearchContext, SearchError> {
    results
        .into_iter()
        .map(|result| {
            let payload = serde_json::to_value(result.metadata)?;
            Ok(SearchItem {
                id: Some(result.id),
                score: Some(result.score),
                payload,
            })
        })
        .collect()
}
