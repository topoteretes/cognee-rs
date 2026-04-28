use cognee_database::IngestDb;
use uuid::Uuid;

use crate::types::{SearchContext, SearchError};

/// Update `last_accessed` timestamps on the source `Data` records for
/// the given search context items.
///
/// Extracts source data IDs from each item's payload, then calls
/// `IngestDb::update_last_accessed` in bulk. Chunk vectors currently expose
/// this as `document_id`, while other retrievers may expose it as `data_id`.
/// Errors from the database are returned to the caller; the caller decides
/// whether to log-and-swallow or propagate them.
pub async fn update_node_access_timestamps(
    database: &dyn IngestDb,
    context: &SearchContext,
) -> Result<(), SearchError> {
    let now = chrono::Utc::now();

    let data_ids: Vec<Uuid> = context
        .iter()
        .filter_map(|item| {
            item.payload
                .get("data_id")
                .or_else(|| item.payload.get("document_id"))
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .collect();

    if data_ids.is_empty() {
        return Ok(());
    }

    database
        .update_last_accessed(&data_ids, now)
        .await
        .map_err(|e| SearchError::DatabaseError(e.to_string()))
}
