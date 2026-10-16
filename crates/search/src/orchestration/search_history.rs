use cognee_database::{DatabaseError, SearchHistoryDb};
use uuid::Uuid;

use crate::types::{SearchOutput, SearchRequest, SearchResponse, SearchType};

/// One query and result pair to record in search history.
pub(crate) struct HistoryRow {
    /// The dataset searched, or `None` for an unscoped search.
    pub dataset_id: Option<Uuid>,
    /// The text stored as the result.
    pub value: String,
}

/// The type name stored in history, e.g. `GRAPH_COMPLETION`.
pub(crate) fn history_query_type(search_type: SearchType) -> String {
    match serde_json::to_value(search_type) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{search_type:?}"),
    }
}

/// The row for a search, with its dataset only if exactly one was searched.
///
/// Matches Python with access control off, which also runs one search over
/// all datasets. With it on, Python searches and records each dataset apart.
pub(crate) fn history_row(request: &SearchRequest, response: &SearchResponse) -> HistoryRow {
    let dataset_id = match request.dataset_ids.as_deref() {
        Some([id]) => Some(*id),
        _ => None,
    };
    HistoryRow {
        dataset_id,
        value: history_value(response, dataset_id),
    }
}

/// The answer, else the dataset's context, else the raw result.
fn history_value(response: &SearchResponse, dataset_id: Option<Uuid>) -> String {
    if let SearchOutput::Text(answer) = &response.result {
        return answer.clone();
    }
    let dataset_context = dataset_id.and_then(|id| response.context.as_ref()?.get(&id.to_string()));
    match dataset_context {
        Some(context) => serde_json::to_string(context),
        None => serde_json::to_string(&response.result),
    }
    .unwrap_or_default()
}

/// Write one query and its result to the history tables.
pub(crate) async fn log_history_row(
    database: &dyn SearchHistoryDb,
    request: &SearchRequest,
    query_type: &str,
    row: &HistoryRow,
) -> Result<(), DatabaseError> {
    let query_id = database
        .log_query(
            &request.query_text,
            query_type,
            request.user_id,
            row.dataset_id,
        )
        .await?;
    database
        .log_result(query_id, &row.value, request.user_id, row.dataset_id)
        .await?;
    Ok(())
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

    fn answered(output: SearchOutput) -> SearchResponse {
        SearchResponse::from_output(SearchType::GraphCompletion, output)
    }

    #[test]
    fn history_query_type_is_screaming_snake() {
        assert_eq!(
            history_query_type(SearchType::GraphCompletion),
            "GRAPH_COMPLETION"
        );
        assert_eq!(
            history_query_type(SearchType::HybridCompletion),
            "HYBRID_COMPLETION"
        );
    }

    #[test]
    fn unscoped_search_gives_one_row_without_dataset() {
        let response = answered(SearchOutput::Text("the answer".to_string()));

        let row = history_row(&request(""), &response);

        assert_eq!(row.dataset_id, None);
        assert_eq!(row.value, "the answer");
    }

    #[test]
    fn single_dataset_search_records_its_dataset() {
        let dataset = Uuid::new_v4();
        let request = request(&format!(r#", "dataset_ids": ["{dataset}"]"#));
        let response = answered(SearchOutput::Text("the answer".to_string()));

        let row = history_row(&request, &response);

        assert_eq!(row.dataset_id, Some(dataset));
        assert_eq!(row.value, "the answer");
    }

    #[test]
    fn multi_dataset_search_records_no_dataset() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let request = request(&format!(r#", "dataset_ids": ["{a}", "{b}"]"#));
        let response = answered(SearchOutput::Text("the answer".to_string()));

        assert_eq!(history_row(&request, &response).dataset_id, None);
    }

    #[test]
    fn without_an_answer_a_row_holds_its_dataset_context() {
        let (with_context, without_context) = (Uuid::new_v4(), Uuid::new_v4());
        let mut response = answered(SearchOutput::Texts(vec!["raw".to_string()]));
        response.context = Some([(with_context.to_string(), vec![])].into());
        let raw_result = serde_json::to_string(&response.result).unwrap();

        assert_eq!(history_value(&response, Some(with_context)), "[]");
        assert_eq!(history_value(&response, Some(without_context)), raw_result);
    }
}
