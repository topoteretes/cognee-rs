use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::types::{SearchContext, SearchItem};

pub fn scope_context_by_datasets(
    context: &SearchContext,
    dataset_ids: &[Uuid],
) -> HashMap<String, SearchContext> {
    let mut scoped_contexts: HashMap<String, SearchContext> = dataset_ids
        .iter()
        .map(|dataset_id| (dataset_id.to_string(), Vec::new()))
        .collect();

    for item in context {
        // A content-addressed point can belong to several datasets; its full
        // membership lives in the `dataset_ids` array (unioned on upsert). When
        // present it is authoritative — assign the item to every requested
        // dataset it belongs to. Fall back to the scalar `dataset_id` only for
        // points written before the array existed (back-compat).
        let member_ids: Option<Vec<&str>> = item
            .payload
            .get("dataset_ids")
            .and_then(|value| value.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect());

        match member_ids {
            Some(ids) => {
                for dataset_id in ids {
                    if let Some(scoped_items) = scoped_contexts.get_mut(dataset_id) {
                        scoped_items.push(item.clone());
                    }
                }
            }
            None => {
                if let Some(dataset_id) = item
                    .payload
                    .get("dataset_id")
                    .and_then(|value| value.as_str())
                    && let Some(scoped_items) = scoped_contexts.get_mut(dataset_id)
                {
                    scoped_items.push(item.clone());
                }
            }
        }
    }

    scoped_contexts
}

pub fn merge_scoped_contexts(scoped_contexts: &HashMap<String, SearchContext>) -> SearchContext {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for context in scoped_contexts.values() {
        for item in context {
            let dedup_key = dedup_key(item);
            if seen.insert(dedup_key) {
                merged.push(item.clone());
            }
        }
    }

    merged
}

fn dedup_key(item: &SearchItem) -> String {
    match item.id {
        Some(id) => id.to_string(),
        None => item.payload.to_string(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::types::SearchItem;

    #[test]
    fn scopes_context_by_dataset_id_payload() {
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();

        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.8),
                payload: json!({"dataset_id": dataset_a.to_string(), "text": "A1"}),
            },
            SearchItem {
                id: None,
                score: Some(0.7),
                payload: json!({"dataset_id": dataset_b.to_string(), "text": "B1"}),
            },
        ];

        let scoped = super::scope_context_by_datasets(&context, &[dataset_a, dataset_b]);

        assert_eq!(scoped.get(&dataset_a.to_string()).unwrap().len(), 1);
        assert_eq!(scoped.get(&dataset_b.to_string()).unwrap().len(), 1);
        assert_eq!(scoped[&dataset_a.to_string()][0].payload["text"], "A1");
        assert_eq!(scoped[&dataset_b.to_string()][0].payload["text"], "B1");
    }

    #[test]
    fn scopes_shared_point_to_all_member_datasets() {
        // A content-addressed point that belongs to two datasets (its
        // `dataset_ids` union array lists both) must be assigned to BOTH scopes,
        // not just one. Regression test for the cross-dataset dedup bug.
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();

        let context = vec![SearchItem {
            id: Some(Uuid::new_v4()),
            score: Some(0.9),
            payload: json!({
                "dataset_ids": [dataset_a.to_string(), dataset_b.to_string()],
                "dataset_id": dataset_b.to_string(),
                "text": "shared across A and B",
            }),
        }];

        let scoped = super::scope_context_by_datasets(&context, &[dataset_a, dataset_b]);

        assert_eq!(
            scoped.get(&dataset_a.to_string()).unwrap().len(),
            1,
            "shared point must be retrievable when scoped to dataset A"
        );
        assert_eq!(
            scoped.get(&dataset_b.to_string()).unwrap().len(),
            1,
            "shared point must be retrievable when scoped to dataset B"
        );
    }

    #[test]
    fn merges_scoped_contexts_without_duplicates() {
        let dataset_a = Uuid::new_v4().to_string();
        let dataset_b = Uuid::new_v4().to_string();
        let duplicate_id = Uuid::new_v4();

        let item_shared = SearchItem {
            id: Some(duplicate_id),
            score: Some(0.9),
            payload: json!({"dataset_id": dataset_a, "text": "shared"}),
        };

        let item_unique = SearchItem {
            id: None,
            score: Some(0.7),
            payload: json!({"dataset_id": dataset_b, "text": "unique"}),
        };

        let merged = super::merge_scoped_contexts(&std::collections::HashMap::from([
            ("a".to_string(), vec![item_shared.clone()]),
            ("b".to_string(), vec![item_shared, item_unique]),
        ]));

        assert_eq!(merged.len(), 2);
    }
}
